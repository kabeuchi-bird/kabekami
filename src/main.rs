//! kabekami — KDE Plasma 向け壁紙ローテーションツール
//!
//! ## Phase 3 追加機能（設計書 §13 Phase 3）
//! - 画面解像度の自動取得（`screen.rs`）: `kscreen-doctor --outputs` でプライマリ
//!   モニタの解像度を自動検出。`KABEKAMI_SCREEN=WxH` 環境変数で上書き可能。
//! - D-Bus 壁紙設定（`plasma.rs`）: `org.kde.PlasmaShell::evaluateScript` を
//!   一次手段に、`plasma-apply-wallpaperimage` CLI をフォールバックとして使用。
//! - 全表示モード実装（`display_mode.rs`）: Fill / Fit / Stretch / Smart。
//!   BlurPad フォールバックを廃止し、全モードを正式実装。
//! - ディレクトリ監視（`watcher.rs`）: `notify` で画像の追加・削除をリアルタイム検知
//!   し、スケジューラに反映する。

mod cache;
mod config;
mod daemon_iface;
mod display_mode;
mod i18n;
mod notify;
mod plasma;
mod prefetch;
mod provider;
mod scanner;
mod scheduler;
mod screen;
mod tray;
mod watcher;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::signal;
use tokio::time::{interval_at, Instant, MissedTickBehavior};
use zbus;

use crate::cache::{Cache, CacheKey};
use crate::config::Config;
use crate::prefetch::Prefetcher;
use crate::scheduler::Scheduler;
use crate::tray::TrayCmd;

/// `KABEKAMI_SCREEN` 環境変数が未設定かつ `kscreen-doctor` も使えない場合の
/// フォールバック解像度。Phase 3 で kscreen-doctor 連携が入るため実際には滅多に使われない。
const FALLBACK_SCREEN_W: u32 = 1920;
const FALLBACK_SCREEN_H: u32 = 1080;

/// CLI サブコマンド。デーモンへの 1 回限りの操作を表す。
enum CliCmd {
    Next,
    Prev,
    TogglePause,
    ReloadConfig,
    Quit,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 1)]
async fn main() -> Result<()> {
    // CLI コマンドが指定されていればデーモンへ転送して終了する
    if let Some(cmd) = parse_cli()? {
        return send_to_daemon(cmd).await;
    }

    // Config を先にロード（tracing の初期化に warn_notify フラグが必要なため）
    let mut config = Config::load().context("failed to load config")?;

    // warn_notify に応じた tracing subscriber を初期化
    let mut warn_rx = init_tracing(config.ui.warn_notify);

    tracing::info!(?config, "loaded config");

    // ローカルディレクトリ + オンラインソースのダウンロードディレクトリを統合してスキャン
    let mut scan_dirs = config.sources.directories.clone();
    for oc in &config.online_sources {
        if oc.enabled {
            scan_dirs.push(oc.resolved_download_dir());
        }
    }
    let images = crate::scanner::scan(&scan_dirs, config.sources.recursive)
        .context("failed to scan source directories")?;
    let has_online = config.online_sources.iter().any(|s| s.enabled);
    if images.is_empty() {
        if has_online {
            tracing::info!("no local images yet; waiting for online sources to fetch");
        } else {
            anyhow::bail!(
                "no images found. Configure [sources] directories in {}",
                Config::config_path()?.display()
            );
        }
    } else {
        tracing::info!("discovered {} image(s)", images.len());
    }

    let (screen_w, screen_h) = resolve_screen_size();
    tracing::info!("using screen size {}x{}", screen_w, screen_h);

    // キャッシュ・スケジューラ・先読みを初期化
    let mut cache = Arc::new(Cache::new(
        config.cache.directory.clone(),
        config.cache.max_size_mb,
    ));
    let mut scheduler = Scheduler::new(images, config.rotation.order);
    let mut prefetcher = Prefetcher::new();

    // ディレクトリ監視を起動（環境によっては unavailable のため Option）
    let (mut watch_rx, mut _watcher_handle) =
        match watcher::spawn(&config.sources.directories, config.sources.recursive) {
            Some((w, rx)) => (rx, Some(w)),
            None => {
                // ウォッチャーが使えない場合は即座に閉じるチャンネルを用意する。
                // select! では Some(ev) パターンが一致しないため無害にスキップされる。
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<watcher::WatchEvent>();
                drop(tx);
                (rx, None)
            }
        };

    // 言語設定を解決する（環境変数 → config → デフォルト ja）
    let mut lang = resolve_lang(&config);
    tracing::info!("ui language: {:?}", lang);

    // デスクトップ通知ハンドル
    let mut notifier = notify::Notifier::new(lang).await;

    // トレイを非同期に起動（D-Bus が使えない環境では None になる）
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<TrayCmd>();
    let tray_handle = tray::spawn_tray(
        cmd_tx.clone(),
        config.display.mode,
        config.rotation.interval_secs,
        lang,
    )
    .await;

    // D-Bus デーモンインターフェースを登録（CLI からのリモート操作を受け付ける）
    let _dbus_conn = spawn_dbus_iface(cmd_tx).await;

    // Plasma への壁紙適用ハンドル（D-Bus 接続を保持して再利用）
    let plasma_shell = plasma::PlasmaShell::new().await;

    // オンラインプロバイダーのフェッチ用チャンネルと共有クライアント
    let (online_tx, mut online_rx) =
        tokio::sync::mpsc::unbounded_channel::<provider::FetchResult>();
    let online_client = match provider::make_client() {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!("online sources disabled: HTTP client init failed: {:#}", e);
            None
        }
    };

    // 画面サイズをフェッチコンテキストとして保持
    let fetch_ctx = provider::FetchContext { screen_w, screen_h };

    // 30 分ごとにプロバイダーを確認する（第 1 tick は即時）
    let mut fetch_ticker = tokio::time::interval(Duration::from_secs(1800));
    fetch_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    // オンラインソース設定を共有（ReloadConfig で更新される）
    let online_configs = std::sync::Arc::new(std::sync::Mutex::new(
        config.online_sources.clone(),
    ));

    // 同時フェッチを防ぐフラグ（前のタスクが終わる前に次の tick が来ても無視する）
    let fetch_in_progress = Arc::new(AtomicBool::new(false));

    // 起動時の即時切り替え
    if config.rotation.change_on_start {
        if let Some(path) = scheduler.next() {
            if let Err(e) = apply(&path, screen_w, screen_h, &config, &cache, &plasma_shell).await {
                tracing::error!(error = %e, "initial wallpaper apply failed");
                let msg = e.to_string();
                notifier.error(&msg).await;
                update_tray_error(&tray_handle, msg).await;
            } else {
                notifier.clear();
                update_tray_ok(&tray_handle, &path).await;
                start_prefetch(&mut prefetcher, &scheduler, screen_w, screen_h, &config, &cache);
            }
        }
    }

    // タイマー: 最初の tick は 1 interval 後から。
    let mut ticker = make_ticker(config.rotation.interval_secs);

    tracing::info!("entering main loop (interval={}s)", config.rotation.interval_secs);

    loop {
        tokio::select! {
            // オンラインプロバイダーの定期フェッチ（30 分ごと、初回は即時）
            _ = fetch_ticker.tick() => {
                if let Some(ref client) = online_client {
                    let configs = online_configs.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    if !configs.is_empty()
                        && !fetch_in_progress.load(Ordering::Acquire)
                    {
                        fetch_in_progress.store(true, Ordering::Release);
                        let tx = online_tx.clone();
                        let client = client.clone();
                        let ctx = fetch_ctx;
                        let in_progress = fetch_in_progress.clone();
                        tokio::spawn(async move {
                            let results = provider::fetch_all_due(&configs, &client, ctx, false).await;
                            for r in results {
                                let _ = tx.send(r);
                            }
                            in_progress.store(false, Ordering::Release);
                        });
                    }
                }
            }

            // オンラインフェッチ完了 → スケジューラに追加
            Some(result) = online_rx.recv() => {
                if !result.new_paths.is_empty() {
                    let was_empty = scheduler.current().is_none() && scheduler.peek_next().is_none();
                    let added = result.new_paths.len();
                    for path in result.new_paths {
                        scheduler.add_image(path);
                    }
                    tracing::info!(
                        "provider {}: {} new image(s) added to rotation",
                        result.provider,
                        added
                    );
                    // 壁紙が 1 枚も表示されていなければ即時適用
                    if was_empty {
                        if let Some(path) = scheduler.next() {
                            if let Err(e) = apply(&path, screen_w, screen_h, &config, &cache, &plasma_shell).await {
                                tracing::error!(error = %e, "online: initial apply failed");
                                let msg = e.to_string();
                                notifier.error(&msg).await;
                                update_tray_error(&tray_handle, msg).await;
                            } else {
                                notifier.clear();
                                update_tray_ok(&tray_handle, &path).await;
                                start_prefetch(&mut prefetcher, &scheduler, screen_w, screen_h, &config, &cache);
                            }
                        }
                    }
                }
            }

            _ = ticker.tick() => {
                if scheduler.is_paused() {
                    continue;
                }
                if let Some(path) = scheduler.next() {
                    if let Err(e) = apply(&path, screen_w, screen_h, &config, &cache, &plasma_shell).await {
                        tracing::error!(error = %e, "auto apply failed");
                        let msg = e.to_string();
                        notifier.error(&msg).await;
                        update_tray_error(&tray_handle, msg).await;
                    } else {
                        notifier.clear();
                        update_tray_ok(&tray_handle, &path).await;
                        start_prefetch(&mut prefetcher, &scheduler, screen_w, screen_h, &config, &cache);
                    }
                }
            }

            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    TrayCmd::Next => {
                        prefetcher.abort();
                        if let Some(path) = scheduler.next() {
                            if let Err(e) = apply(&path, screen_w, screen_h, &config, &cache, &plasma_shell).await {
                                tracing::error!(error = %e, "tray Next failed");
                                let msg = e.to_string();
                                notifier.error(&msg).await;
                                update_tray_error(&tray_handle, msg).await;
                            } else {
                                notifier.clear();
                                update_tray_ok(&tray_handle, &path).await;
                                start_prefetch(&mut prefetcher, &scheduler, screen_w, screen_h, &config, &cache);
                            }
                        }
                        ticker = make_ticker(config.rotation.interval_secs);
                    }

                    TrayCmd::Prev => {
                        if let Some(path) = scheduler.prev() {
                            if let Err(e) = apply(&path, screen_w, screen_h, &config, &cache, &plasma_shell).await {
                                tracing::error!(error = %e, "tray Prev failed");
                                let msg = e.to_string();
                                notifier.error(&msg).await;
                                update_tray_error(&tray_handle, msg).await;
                            } else {
                                notifier.clear();
                                update_tray_ok(&tray_handle, &path).await;
                                start_prefetch(&mut prefetcher, &scheduler, screen_w, screen_h, &config, &cache);
                            }
                        }
                        ticker = make_ticker(config.rotation.interval_secs);
                    }

                    TrayCmd::TogglePause => {
                        if scheduler.is_paused() {
                            scheduler.resume();
                            tracing::info!("resumed");
                        } else {
                            scheduler.pause();
                            tracing::info!("paused");
                        }
                        if let Some(ref h) = tray_handle {
                            let paused = scheduler.is_paused();
                            h.update(|t| t.paused = paused).await;
                        }
                    }

                    TrayCmd::SetMode(mode) => {
                        tracing::info!("display mode → {:?}", mode);
                        config.display.mode = mode;
                        if let Some(cur) = scheduler.current().cloned() {
                            if let Err(e) = apply(&cur, screen_w, screen_h, &config, &cache, &plasma_shell).await {
                                tracing::error!(error = %e, "reapply after mode change failed");
                                let msg = e.to_string();
                                notifier.error(&msg).await;
                                update_tray_error(&tray_handle, msg).await;
                            } else {
                                notifier.clear();
                                update_tray_clear_error(&tray_handle).await;
                            }
                            start_prefetch(&mut prefetcher, &scheduler, screen_w, screen_h, &config, &cache);
                        }
                    }

                    TrayCmd::SetInterval(secs) => {
                        let secs = secs.max(crate::config::MIN_INTERVAL_SECS);
                        tracing::info!("interval → {}s", secs);
                        config.rotation.interval_secs = secs;
                        ticker = make_ticker(secs);
                        if let Some(ref h) = tray_handle {
                            h.update(|t| t.interval_secs = secs).await;
                        }
                    }

                    TrayCmd::OpenCurrent => {
                        if let Some(path) = scheduler.current().cloned() {
                            tokio::task::spawn_blocking(move || {
                                let _ = std::process::Command::new("xdg-open")
                                    .arg(&path)
                                    .status();
                            });
                        }
                    }

                    TrayCmd::ReloadConfig => {
                        // Phase 5 でファイル監視 / SIGUSR1 / D-Bus によるリロードを追加する際は、
                        // このブロックを async fn do_reload_config(...) に切り出して再利用する。
                        match Config::load() {
                            Err(e) => {
                                tracing::error!(error = %e, "config reload failed");
                                let msg = e.to_string();
                                notifier.error(&msg).await;
                                update_tray_error(&tray_handle, msg).await;
                            }
                            Ok(new_cfg) => {
                                tracing::info!("reloading config");

                                // 1. 現在の壁紙パスを退避（scheduler 再構築前）
                                let prev_current = scheduler.current().cloned();

                                // 2. 画像再スキャン → scheduler 再構築
                                match scanner::scan(&new_cfg.sources.directories, new_cfg.sources.recursive) {
                                    Ok(images) if !images.is_empty() => {
                                        tracing::info!("reload: {} image(s) found", images.len());
                                        scheduler = Scheduler::new(images, new_cfg.rotation.order);
                                    }
                                    Ok(_) => tracing::warn!("reload: no images found, keeping current list"),
                                    Err(e) => tracing::warn!("reload: scan error: {}", e),
                                }

                                // 3. ウォッチャー再起動
                                (watch_rx, _watcher_handle) =
                                    match watcher::spawn(&new_cfg.sources.directories, new_cfg.sources.recursive) {
                                        Some((w, rx)) => (rx, Some(w)),
                                        None => {
                                            let (tx, rx) =
                                                tokio::sync::mpsc::unbounded_channel::<watcher::WatchEvent>();
                                            drop(tx);
                                            (rx, None)
                                        }
                                    };

                                // 4. キャッシュ再構築（設定変更を反映）
                                prefetcher.abort();
                                cache = Arc::new(Cache::new(
                                    new_cfg.cache.directory.clone(),
                                    new_cfg.cache.max_size_mb,
                                ));

                                // 5. タイマー再起動
                                ticker = make_ticker(new_cfg.rotation.interval_secs);

                                // 6. 言語更新
                                let new_lang = resolve_lang(&new_cfg);
                                if new_lang != lang {
                                    lang = new_lang;
                                    notifier = notify::Notifier::new(lang).await;
                                }

                                // warn_notify の変更は tracing subscriber を再初期化できないため
                                // 再起動後にのみ反映される
                                if new_cfg.ui.warn_notify != config.ui.warn_notify {
                                    tracing::warn!(
                                        "warn_notify setting changed ({} → {}); restart kabekami to apply this change",
                                        config.ui.warn_notify,
                                        new_cfg.ui.warn_notify
                                    );
                                }

                                // 7. 設定更新（オンラインソース設定も共有 Arc を更新）
                                *online_configs.lock().unwrap_or_else(|e| e.into_inner()) = new_cfg.online_sources.clone();
                                config = new_cfg;

                                // 8. 現在の壁紙を新設定で再適用
                                if let Some(cur) = prev_current {
                                    if let Err(e) = apply(&cur, screen_w, screen_h, &config, &cache, &plasma_shell).await {
                                        tracing::error!(error = %e, "reload: reapply failed");
                                        let msg = e.to_string();
                                        notifier.error(&msg).await;
                                        update_tray_error(&tray_handle, msg).await;
                                    } else {
                                        notifier.clear();
                                        update_tray_ok(&tray_handle, &cur).await;
                                        start_prefetch(&mut prefetcher, &scheduler, screen_w, screen_h, &config, &cache);
                                    }
                                }

                                // 9. トレイ状態更新
                                if let Some(ref h) = tray_handle {
                                    let mode = config.display.mode;
                                    let secs = config.rotation.interval_secs;
                                    let strings = crate::i18n::strings(lang);
                                    h.update(|t| {
                                        t.mode = mode;
                                        t.interval_secs = secs;
                                        t.strings = strings;
                                    }).await;
                                }

                                tracing::info!("config reload complete");
                            }
                        }
                    }

                    TrayCmd::OpenSettings => {
                        match std::process::Command::new("kabekami-config").spawn() {
                            Ok(_) => tracing::info!("launched kabekami-config"),
                            Err(e) => tracing::warn!("failed to launch kabekami-config: {}", e),
                        }
                    }

                    TrayCmd::FetchNow => {
                        if let Some(ref client) = online_client {
                            let configs = online_configs.lock().unwrap_or_else(|e| e.into_inner()).clone();
                            if !configs.is_empty()
                                && !fetch_in_progress.load(Ordering::Acquire)
                            {
                                fetch_in_progress.store(true, Ordering::Release);
                                let tx = online_tx.clone();
                                let client = client.clone();
                                let ctx = fetch_ctx;
                                let in_progress = fetch_in_progress.clone();
                                tokio::spawn(async move {
                                    let results = provider::fetch_all_due(&configs, &client, ctx, true).await;
                                    for r in results {
                                        let _ = tx.send(r);
                                    }
                                    in_progress.store(false, Ordering::Release);
                                });
                                tracing::info!("manual fetch triggered");
                            } else {
                                tracing::info!("fetch already in progress, skipping");
                            }
                        }
                    }

                    TrayCmd::Quit => {
                        tracing::info!("quit requested from tray");
                        break;
                    }
                }
            }

            // ディレクトリ変更監視イベント（ウォッチャーが無効な場合は到達しない）
            Some(ev) = watch_rx.recv() => {
                match ev {
                    watcher::WatchEvent::Added(path) => {
                        tracing::info!("new image detected: {}", path.display());
                        scheduler.add_image(path);
                    }
                    watcher::WatchEvent::Removed(path) => {
                        tracing::info!("image removed: {}", path.display());
                        scheduler.remove_image(&path);
                    }
                }
            }

            msg = next_warn(&mut warn_rx) => {
                match msg {
                    Some(msg) => notifier.warn(&msg).await,
                    None => warn_rx = None, // チャンネルが閉じたらブランチを無効化
                }
            }

            _ = signal::ctrl_c() => {
                tracing::info!("received Ctrl-C, shutting down");
                break;
            }
        }
    }

    prefetcher.abort();
    if let Some(h) = tray_handle {
        h.shutdown().await;
    }
    Ok(())
}

// ── ヘルパー関数 ─────────────────────────────────────────────────────────────

fn init_tracing(warn_notify: bool) -> Option<tokio::sync::mpsc::UnboundedReceiver<String>> {
    use tracing_subscriber::{fmt, EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("kabekami=info,warn"));

    if warn_notify {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer())
            .with(WarnNotifyLayer { tx })
            .init();
        Some(rx)
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer())
            .init();
        None
    }
}

/// CLI 引数を解析して `CliCmd` を返す。引数がなければ `None`（デーモンモード）。
fn parse_cli() -> Result<Option<CliCmd>> {
    let mut args = std::env::args().skip(1).peekable();
    let Some(arg) = args.next() else { return Ok(None) };

    let cmd = match arg.as_str() {
        "--next"          => CliCmd::Next,
        "--prev"          => CliCmd::Prev,
        "--toggle-pause"  => CliCmd::TogglePause,
        "--reload-config" => CliCmd::ReloadConfig,
        "--quit"          => CliCmd::Quit,
        "--help" | "-h" => {
            println!("kabekami — KDE Plasma wallpaper rotation daemon\n");
            println!("USAGE:");
            println!("  kabekami                start the daemon");
            println!("  kabekami --next         switch to next wallpaper");
            println!("  kabekami --prev         switch to previous wallpaper");
            println!("  kabekami --toggle-pause pause / resume rotation");
            println!("  kabekami --reload-config reload config.toml");
            println!("  kabekami --quit         quit the daemon");
            std::process::exit(0);
        }
        other => anyhow::bail!("unknown option '{}'. Try --help.", other),
    };
    Ok(Some(cmd))
}

/// D-Bus 経由でデーモンにコマンドを送信する。
async fn send_to_daemon(cmd: CliCmd) -> Result<()> {
    use daemon_iface::{BUS_NAME, OBJECT_PATH};

    let method = match cmd {
        CliCmd::Next         => "Next",
        CliCmd::Prev         => "Prev",
        CliCmd::TogglePause  => "TogglePause",
        CliCmd::ReloadConfig => "ReloadConfig",
        CliCmd::Quit         => "Quit",
    };

    let conn = zbus::Connection::session()
        .await
        .context("failed to connect to D-Bus session bus")?;

    conn.call_method(
        Some(BUS_NAME),
        OBJECT_PATH,
        Some(BUS_NAME),
        method,
        &(),
    )
    .await
    .with_context(|| {
        format!("failed to send '{method}' to kabekami daemon — is it running?")
    })?;

    Ok(())
}

/// D-Bus デーモンインターフェースを起動する。
///
/// 登録に失敗した場合（D-Bus 未使用環境など）は警告ログを出して `None` を返す。
/// 返値の `Connection` を main() スコープで保持し続けることでサービスが維持される。
async fn spawn_dbus_iface(
    tx: tokio::sync::mpsc::UnboundedSender<TrayCmd>,
) -> Option<zbus::Connection> {
    use daemon_iface::{BUS_NAME, OBJECT_PATH, DaemonIface};

    let result = zbus::conn::Builder::session()
        .and_then(|b| b.name(BUS_NAME))
        .and_then(|b| b.serve_at(OBJECT_PATH, DaemonIface { tx }))
        .map(|b| async move { b.build().await });

    match result {
        Err(e) => {
            tracing::warn!("D-Bus daemon interface unavailable: {}", e);
            None
        }
        Ok(fut) => match fut.await {
            Ok(conn) => {
                tracing::info!("D-Bus daemon interface active ({})", BUS_NAME);
                Some(conn)
            }
            Err(e) => {
                tracing::warn!("D-Bus daemon interface unavailable: {}", e);
                None
            }
        },
    }
}

/// warn_rx が None のときは永遠に pending にする（select! ブランチを無効化）。
async fn next_warn(
    rx: &mut Option<tokio::sync::mpsc::UnboundedReceiver<String>>,
) -> Option<String> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// WARN イベントを tokio チャンネル経由で非同期に転送する tracing レイヤー。
struct WarnNotifyLayer {
    tx: tokio::sync::mpsc::UnboundedSender<String>,
}

struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_string();
        }
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{:?}", value);
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for WarnNotifyLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        // kabekami クレートの WARN のみを対象にする（外部クレートの WARN は除外）
        if *event.metadata().level() == tracing::Level::WARN
            && event.metadata().target().starts_with("kabekami")
        {
            let mut v = MessageVisitor(String::new());
            event.record(&mut v);
            if !v.0.is_empty() {
                let _ = self.tx.send(v.0);
            }
        }
    }
}

/// 表示言語を解決する。優先順位:
///
/// 1. 環境変数 `KABEKAMI_LANG`（`"en"` / `"ja"`）
/// 2. `config.toml` の `[ui] language`
/// 3. デフォルト: 英語
fn resolve_lang(config: &Config) -> i18n::Lang {
    if let Ok(val) = std::env::var("KABEKAMI_LANG") {
        return i18n::Lang::from_str(val.trim());
    }
    if !config.ui.language.is_empty() {
        return i18n::Lang::from_str(&config.ui.language);
    }
    i18n::Lang::default()
}

/// 画面解像度を解決する。優先順位:
///
/// 1. `KABEKAMI_SCREEN=WxH` 環境変数
/// 2. `kscreen-doctor --outputs` による自動検出
/// 3. フォールバック（1920×1080）
fn resolve_screen_size() -> (u32, u32) {
    // 1. 環境変数による手動指定
    if let Ok(val) = std::env::var("KABEKAMI_SCREEN") {
        if let Some((w, h)) = val.split_once('x') {
            if let (Ok(w), Ok(h)) = (w.trim().parse::<u32>(), h.trim().parse::<u32>()) {
                if w > 0 && h > 0 {
                    tracing::info!("screen size from KABEKAMI_SCREEN: {}x{}", w, h);
                    return (w, h);
                }
            }
        }
        tracing::warn!("invalid KABEKAMI_SCREEN='{}', expected WxH (e.g. 2560x1440)", val);
    }

    // 2. kscreen-doctor による自動検出
    if let Some((w, h)) = screen::detect() {
        tracing::info!("screen size from kscreen-doctor: {}x{}", w, h);
        return (w, h);
    }

    tracing::warn!(
        "could not detect screen size, using fallback {}x{}",
        FALLBACK_SCREEN_W,
        FALLBACK_SCREEN_H
    );
    (FALLBACK_SCREEN_W, FALLBACK_SCREEN_H)
}

/// 「1 interval 後に最初の tick」が来る Interval を生成する。
fn make_ticker(interval_secs: u64) -> tokio::time::Interval {
    let period = Duration::from_secs(interval_secs);
    let mut t = interval_at(Instant::now() + period, period);
    t.set_missed_tick_behavior(MissedTickBehavior::Skip);
    t
}

/// 壁紙を加工してキャッシュし、Plasma に反映する。
///
/// 1. キャッシュヒット → キャッシュ済みファイルを直接 Plasma に渡す（高速パス）
/// 2. キャッシュミス  → `spawn_blocking` で加工・保存してから Plasma に渡す
///    （`store()` が LRU 退避も担う）
async fn apply(src: &Path, screen_w: u32, screen_h: u32, config: &Config, cache: &Arc<Cache>, plasma: &plasma::PlasmaShell) -> Result<()> {
    let key = CacheKey {
        src: src.to_path_buf(),
        screen_w,
        screen_h,
        mode: config.display.mode,
        blur_sigma: config.display.blur_sigma,
        bg_darken: config.display.bg_darken,
    };

    let output = if let Some(cached) = cache.get(&key) {
        tracing::debug!("cache hit: {}", src.display());
        cached
    } else {
        let src_owned = src.to_path_buf();
        let cache_owned = Arc::clone(cache);
        let mode = config.display.mode;
        let blur_sigma = config.display.blur_sigma;
        let bg_darken = config.display.bg_darken;
        tokio::task::spawn_blocking(move || {
            prefetch::process_for_cache(
                &src_owned,
                screen_w,
                screen_h,
                mode,
                blur_sigma,
                bg_darken,
                &cache_owned,
            )
        })
        .await
        .context("image processing task panicked")??
    };

    plasma.set_wallpaper(&output).await
}

/// トレイアイコンに `last_error` をセットする。
async fn update_tray_error(
    tray_handle: &Option<ksni::Handle<tray::KabekamiTray>>,
    msg: String,
) {
    if let Some(ref h) = tray_handle {
        h.update(|t| t.last_error = Some(msg)).await;
    }
}

/// トレイアイコンの `last_error` をクリアする。
async fn update_tray_clear_error(tray_handle: &Option<ksni::Handle<tray::KabekamiTray>>) {
    if let Some(ref h) = tray_handle {
        h.update(|t| t.last_error = None).await;
    }
}

/// 壁紙切り替え成功時に `last_error` クリアと `current_name` 更新を 1 回の IPC で行う。
async fn update_tray_ok(tray_handle: &Option<ksni::Handle<tray::KabekamiTray>>, path: &Path) {
    if let Some(ref h) = tray_handle {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        h.update(|t| { t.last_error = None; t.current_name = name; }).await;
    }
}

/// 先読みを開始する。`scheduler.peek_next()` の画像を対象にする。
fn start_prefetch(
    prefetcher: &mut Prefetcher,
    scheduler: &Scheduler,
    screen_w: u32,
    screen_h: u32,
    config: &Config,
    cache: &Arc<Cache>,
) {
    if !config.rotation.prefetch {
        return;
    }
    if let Some(next) = scheduler.peek_next() {
        prefetcher.start(
            next.clone(),
            screen_w,
            screen_h,
            config.display.mode,
            config.display.blur_sigma,
            config.display.bg_darken,
            cache.clone(),
        );
    }
}
