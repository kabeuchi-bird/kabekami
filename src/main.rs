//! kabekami — KDE Plasma 向け壁紙ローテーションデーモン

mod blacklist;
mod cache;
mod config;
mod daemon_iface;
mod display_mode;
use kabekami_common::i18n;
mod notify;
mod plasma;
mod prefetch;
mod provider;
mod scanner;
mod scheduler;
mod screen;
mod session;
mod shortcuts;
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

/// `KABEKAMI_SCREEN` 環境変数が未設定かつ `kscreen-doctor` も使えない場合のフォールバック解像度。
const FALLBACK_SCREEN_W: u32 = 1920;
const FALLBACK_SCREEN_H: u32 = 1080;

/// CLI サブコマンド。デーモンへの 1 回限りの操作を表す。
enum CliCmd {
    Next,
    Prev,
    TogglePause,
    ReloadConfig,
    FetchNow,
    TrashCurrent,
    BlacklistCurrent,
    CopyToFavorites,
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

    // ブラックリストを起動時に読み込む
    let kabekami_config_dir = Config::config_path()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let mut blacklist = blacklist::Blacklist::load(&kabekami_config_dir)
        .context("failed to load blacklist")?;

    // ローカルディレクトリ + オンラインソースのダウンロードディレクトリを統合してスキャン
    let mut scan_dirs = config.sources.directories.clone();
    for oc in &config.online_sources {
        if oc.enabled {
            scan_dirs.push(oc.resolved_download_dir());
        }
    }
    let images = build_filtered_images_list(&scan_dirs, config.sources.recursive, &blacklist)
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

    // モニター検出（マルチモニター対応）
    let screens = resolve_screens().await;
    // プライマリ解像度: フェッチコンテキスト・プリフェッチに使用
    let (screen_w, screen_h) = screens
        .first()
        .map(|m| (m.width, m.height))
        .unwrap_or((FALLBACK_SCREEN_W, FALLBACK_SCREEN_H));

    // キャッシュ・スケジューラ・先読みを初期化
    let mut cache = Arc::new(Cache::new(
        config.cache.directory.clone(),
        config.cache.max_size_mb,
    ));
    let mut scheduler = Scheduler::new(images, config.rotation.order);
    let mut prefetcher = Prefetcher::new();

    // ディレクトリ監視を起動（環境によっては unavailable のため Option）
    let (mut watch_rx, mut _watcher_handle) =
        match watcher::spawn(&collect_watch_dirs(&config), config.sources.recursive) {
            Some((w, rx)) => (rx, Some(w)),
            None => {
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
        config.sources.favorites_dir.is_some(),
        config.ui.enable_blacklist,
        config.online_sources.iter().any(|s| s.enabled),
    )
    .await;

    // D-Bus デーモンインターフェースを登録（CLI からのリモート操作を受け付ける）
    let _dbus_conn = spawn_dbus_iface(cmd_tx.clone()).await;

    // セッション管理ウォッチャーを起動（ログアウト検知・Plasma 再起動検知）
    session::spawn_session_watcher(cmd_tx.clone()).await;

    // KDE グローバルショートカットを登録・監視する
    shortcuts::spawn_shortcut_watcher(cmd_tx).await;

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

    let fetch_ctx = provider::FetchContext { screen_w, screen_h };

    // 30 分ごとにプロバイダーを確認する（第 1 tick は即時）
    let mut fetch_ticker = tokio::time::interval(Duration::from_secs(1800));
    fetch_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let online_configs = std::sync::Arc::new(std::sync::Mutex::new(
        config.online_sources.clone(),
    ));

    let fetch_in_progress = Arc::new(AtomicBool::new(false));

    // トレイに初期画像枚数を反映
    if let Some(ref h) = tray_handle {
        let count = scheduler.image_count();
        h.update(|t| t.image_count = count).await;
    }

    // 起動時の即時切り替え
    if config.rotation.change_on_start {
        if let Some(path) = scheduler.next() {
            apply_and_notify(&path, &screens, &config, &cache, &plasma_shell,
                &mut notifier, &tray_handle, &mut prefetcher, &scheduler, "initial apply failed").await;
        }
    }

    let mut ticker = make_ticker(config.rotation.interval_secs);
    let mut last_cmd_at: Option<std::time::Instant> = None;

    tracing::info!("entering main loop (interval={}s)", config.rotation.interval_secs);

    loop {
        tokio::select! {
            _ = fetch_ticker.tick() => {
                if let Some(ref client) = online_client {
                    let configs = online_configs.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    try_spawn_fetch(client, configs, online_tx.clone(), fetch_in_progress.clone(), fetch_ctx, false);
                }
            }

            Some(result) = online_rx.recv() => {
                if !result.new_paths.is_empty() {
                    let was_empty = scheduler.current().is_none() && scheduler.peek_next().is_none();
                    let new_paths: Vec<_> = result.new_paths.into_iter()
                        .filter(|p| !blacklist.contains(p))
                        .collect();
                    let added = new_paths.len();
                    for path in new_paths {
                        scheduler.add_image(path);
                    }
                    tracing::info!(
                        "provider {}: {} new image(s) added to rotation",
                        result.provider,
                        added
                    );
                    if let Some(ref h) = tray_handle {
                        let count = scheduler.image_count();
                        h.update(|t| t.image_count = count).await;
                    }
                    if was_empty {
                        if let Some(path) = scheduler.next() {
                            apply_and_notify(&path, &screens, &config, &cache, &plasma_shell,
                                &mut notifier, &tray_handle, &mut prefetcher, &scheduler, "online: initial apply failed").await;
                        }
                    }
                }
            }

            _ = ticker.tick() => {
                if scheduler.is_paused() {
                    continue;
                }
                if let Some(path) = scheduler.next() {
                    apply_and_notify(&path, &screens, &config, &cache, &plasma_shell,
                        &mut notifier, &tray_handle, &mut prefetcher, &scheduler, "auto apply failed").await;
                }
            }

            Some(cmd) = cmd_rx.recv() => {
                let now = std::time::Instant::now();
                // Quit と PlasmaRestarted はスロットリングをバイパスする
                let throttle_exempt = matches!(cmd, TrayCmd::Quit | TrayCmd::PlasmaRestarted);
                if !throttle_exempt
                    && last_cmd_at.map_or(false, |t| now.duration_since(t) < Duration::from_millis(100))
                {
                    tracing::debug!("command throttled (< 100ms): {:?}", cmd);
                    continue;
                }
                last_cmd_at = Some(now);
                match cmd {
                    TrayCmd::Next => {
                        prefetcher.abort();
                        if let Some(path) = scheduler.next() {
                            apply_and_notify(&path, &screens, &config, &cache, &plasma_shell,
                                &mut notifier, &tray_handle, &mut prefetcher, &scheduler, "tray Next failed").await;
                        }
                        ticker = make_ticker(config.rotation.interval_secs);
                    }

                    TrayCmd::Prev => {
                        if let Some(path) = scheduler.prev() {
                            apply_and_notify(&path, &screens, &config, &cache, &plasma_shell,
                                &mut notifier, &tray_handle, &mut prefetcher, &scheduler, "tray Prev failed").await;
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
                            if let Err(e) = apply(&cur, &screens, &config, &cache, &plasma_shell).await {
                                tracing::error!(error = %e, "reapply after mode change failed");
                                let msg = e.to_string();
                                notifier.error(&msg, Some(&cur)).await;
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

                    TrayCmd::DeleteCurrent => {
                        if let Some(path) = scheduler.current().cloned() {
                            let result = tokio::task::spawn_blocking({
                                let path = path.clone();
                                move || trash::delete(&path)
                            }).await;
                            match result {
                                Ok(Err(e)) => tracing::error!(
                                    "failed to trash wallpaper {}: {}", path.display(), e
                                ),
                                Err(e) => tracing::error!("trash task panicked: {}", e),
                                Ok(Ok(())) => {
                                    tracing::info!("moved to trash: {}", path.display());
                                    scheduler.remove_image(&path);
                                    prefetcher.abort();
                                    if let Some(next) = scheduler.next() {
                                        apply_and_notify(&next, &screens, &config, &cache,
                                            &plasma_shell, &mut notifier, &tray_handle,
                                            &mut prefetcher, &scheduler, "apply after trash failed").await;
                                    }
                                    if let Some(ref h) = tray_handle {
                                        h.update(|t| t.image_count = scheduler.image_count()).await;
                                    }
                                    ticker = make_ticker(config.rotation.interval_secs);
                                }
                            }
                        }
                    }

                    TrayCmd::BlacklistCurrent => {
                        if !config.ui.enable_blacklist {
                            tracing::debug!("blacklist disabled in config, ignoring");
                        } else if let Some(path) = scheduler.current().cloned() {
                            if let Err(e) = blacklist.add(&path) {
                                tracing::error!("blacklist: failed to save {}: {}", path.display(), e);
                            } else {
                                tracing::info!("blacklisted: {}", path.display());
                                scheduler.remove_image(&path);
                                prefetcher.abort();
                                match scheduler.next() {
                                    Some(next) => {
                                        apply_and_notify(&next, &screens, &config, &cache,
                                            &plasma_shell, &mut notifier, &tray_handle,
                                            &mut prefetcher, &scheduler, "apply after blacklist failed").await;
                                        if let Some(ref h) = tray_handle {
                                            h.update(|t| t.image_count = scheduler.image_count()).await;
                                        }
                                    }
                                    None => {
                                        if let Some(ref h) = tray_handle {
                                            h.update(|t| {
                                                t.current_name = String::new();
                                                t.image_count = scheduler.image_count();
                                            }).await;
                                        }
                                    }
                                }
                                ticker = make_ticker(config.rotation.interval_secs);
                            }
                        }
                    }

                    TrayCmd::CopyToFavorites => 'fav: {
                        let Some(path) = scheduler.current().map(|p| p.to_owned()) else { break 'fav };
                        let Some(fav_dir) = config.sources.favorites_dir.clone() else {
                            tracing::warn!("copy_to_favorites: favorites_dir not configured");
                            break 'fav;
                        };
                        let Some(filename) = path.file_name().map(|n| n.to_owned()) else { break 'fav };
                        let dest = fav_dir.join(&filename);
                        if let Err(e) = tokio::fs::create_dir_all(&fav_dir).await {
                            tracing::error!("favorites: failed to create dir {}: {}", fav_dir.display(), e);
                            break 'fav;
                        }
                        match tokio::fs::copy(&path, &dest).await {
                            Ok(_) => tracing::info!("copied to favorites: {}", dest.display()),
                            Err(e) => tracing::error!("favorites: failed to copy {}: {}", path.display(), e),
                        }
                    }

                    TrayCmd::ReloadConfig => {
                        match Config::load() {
                            Err(e) => {
                                tracing::error!(error = %e, "config reload failed");
                                let msg = e.to_string();
                                notifier.error(&msg, None).await;
                                update_tray_error(&tray_handle, msg).await;
                            }
                            Ok(new_cfg) => {
                                tracing::info!("reloading config");

                                let prev_current = scheduler.current().cloned();

                                let mut reload_scan_dirs = new_cfg.sources.directories.clone();
                                for oc in &new_cfg.online_sources {
                                    if oc.enabled {
                                        reload_scan_dirs.push(oc.resolved_download_dir());
                                    }
                                }
                                match build_filtered_images_list(&reload_scan_dirs, new_cfg.sources.recursive, &blacklist) {
                                    Ok(images) if !images.is_empty() => {
                                        tracing::info!("reload: {} image(s) found", images.len());
                                        scheduler = Scheduler::new(images, new_cfg.rotation.order);
                                    }
                                    Ok(_) => tracing::warn!("reload: no images found, keeping current list"),
                                    Err(e) => tracing::warn!("reload: scan error: {}", e),
                                }

                                (watch_rx, _watcher_handle) =
                                    match watcher::spawn(&collect_watch_dirs(&new_cfg), new_cfg.sources.recursive) {
                                        Some((w, rx)) => (rx, Some(w)),
                                        None => {
                                            let (tx, rx) =
                                                tokio::sync::mpsc::unbounded_channel::<watcher::WatchEvent>();
                                            drop(tx);
                                            (rx, None)
                                        }
                                    };

                                prefetcher.abort();
                                cache = Arc::new(Cache::new(
                                    new_cfg.cache.directory.clone(),
                                    new_cfg.cache.max_size_mb,
                                ));

                                ticker = make_ticker(new_cfg.rotation.interval_secs);

                                let new_lang = resolve_lang(&new_cfg);
                                if new_lang != lang {
                                    lang = new_lang;
                                    notifier = notify::Notifier::new(lang).await;
                                }

                                if new_cfg.ui.warn_notify != config.ui.warn_notify {
                                    tracing::warn!(
                                        "warn_notify setting changed ({} → {}); restart kabekami to apply this change",
                                        config.ui.warn_notify,
                                        new_cfg.ui.warn_notify
                                    );
                                }

                                *online_configs.lock().unwrap_or_else(|e| e.into_inner()) = new_cfg.online_sources.clone();
                                config = new_cfg;

                                if let Some(cur) = prev_current {
                                    apply_and_notify(&cur, &screens, &config, &cache, &plasma_shell,
                                        &mut notifier, &tray_handle, &mut prefetcher, &scheduler, "reload: reapply failed").await;
                                }

                                if let Some(ref h) = tray_handle {
                                    let mode = config.display.mode;
                                    let secs = config.rotation.interval_secs;
                                    let strings = crate::i18n::strings(lang);
                                    let count = scheduler.image_count();
                                    let has_fav = config.sources.favorites_dir.is_some();
                                    let bl_enabled = config.ui.enable_blacklist;
                                    let has_online = config.online_sources.iter().any(|s| s.enabled);
                                    h.update(|t| {
                                        t.mode = mode;
                                        t.interval_secs = secs;
                                        t.strings = strings;
                                        t.image_count = count;
                                        t.has_favorites_dir = has_fav;
                                        t.blacklist_enabled = bl_enabled;
                                        t.has_online_sources = has_online;
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
                            if try_spawn_fetch(client, configs, online_tx.clone(), fetch_in_progress.clone(), fetch_ctx, true) {
                                tracing::info!("manual fetch triggered");
                            } else {
                                tracing::info!("fetch already in progress, skipping");
                            }
                        }
                    }

                    TrayCmd::PlasmaRestarted => {
                        tracing::info!("Plasma restarted, re-applying wallpaper");
                        if let Some(path) = scheduler.current().cloned() {
                            apply_and_notify(&path, &screens, &config, &cache, &plasma_shell,
                                &mut notifier, &tray_handle, &mut prefetcher, &scheduler, "reapply after Plasma restart failed").await;
                        }
                    }

                    TrayCmd::Quit => {
                        tracing::info!("quit requested from tray");
                        break;
                    }
                }
            }

            Some(ev) = watch_rx.recv() => {
                match ev {
                    watcher::WatchEvent::Added(path) => {
                        if blacklist.contains(&path) {
                            tracing::debug!("ignoring blacklisted image: {}", path.display());
                        } else {
                            tracing::info!("new image detected: {}", path.display());
                            scheduler.add_image(path);
                        }
                    }
                    watcher::WatchEvent::Removed(path) => {
                        tracing::info!("image removed: {}", path.display());
                        scheduler.remove_image(&path);
                    }
                }
                if let Some(ref h) = tray_handle {
                    let count = scheduler.image_count();
                    h.update(|t| t.image_count = count).await;
                }
            }

            msg = next_warn(&mut warn_rx) => {
                match msg {
                    Some(msg) => notifier.warn(&msg).await,
                    None => warn_rx = None,
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

fn build_filtered_images_list(
    scan_dirs: &[std::path::PathBuf],
    recursive: bool,
    blacklist: &blacklist::Blacklist,
) -> Result<Vec<std::path::PathBuf>> {
    let images: Vec<_> = crate::scanner::scan(scan_dirs, recursive)?
        .into_iter()
        .filter(|p| !blacklist.contains(p))
        .collect();
    Ok(images)
}

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
        "--next"               => CliCmd::Next,
        "--prev"               => CliCmd::Prev,
        "--toggle-pause"       => CliCmd::TogglePause,
        "--reload-config"      => CliCmd::ReloadConfig,
        "--fetch-now"          => CliCmd::FetchNow,
        "--trash-current"      => CliCmd::TrashCurrent,
        "--blacklist-current"  => CliCmd::BlacklistCurrent,
        "--copy-to-favorites"  => CliCmd::CopyToFavorites,
        "--quit"               => CliCmd::Quit,
        "--help" | "-h" => {
            println!("kabekami — KDE Plasma wallpaper rotation daemon\n");
            println!("USAGE:");
            println!("  kabekami                      start the daemon");
            println!("  kabekami --next               switch to next wallpaper");
            println!("  kabekami --prev               switch to previous wallpaper");
            println!("  kabekami --toggle-pause       pause / resume rotation");
            println!("  kabekami --reload-config      reload config.toml");
            println!("  kabekami --fetch-now          trigger online wallpaper fetch");
            println!("  kabekami --trash-current      move current wallpaper to trash");
            println!("  kabekami --blacklist-current  never show current wallpaper again");
            println!("  kabekami --copy-to-favorites  copy current wallpaper to favorites folder");
            println!("  kabekami --quit               quit the daemon");
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
        CliCmd::Next             => "Next",
        CliCmd::Prev             => "Prev",
        CliCmd::TogglePause      => "TogglePause",
        CliCmd::ReloadConfig     => "ReloadConfig",
        CliCmd::FetchNow         => "FetchNow",
        CliCmd::TrashCurrent     => "TrashCurrent",
        CliCmd::BlacklistCurrent => "BlacklistCurrent",
        CliCmd::CopyToFavorites  => "CopyToFavorites",
        CliCmd::Quit             => "Quit",
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

async fn next_warn(
    rx: &mut Option<tokio::sync::mpsc::UnboundedReceiver<String>>,
) -> Option<String> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

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

fn resolve_lang(config: &Config) -> i18n::Lang {
    if let Ok(val) = std::env::var("KABEKAMI_LANG") {
        return i18n::Lang::from_str(val.trim());
    }
    if !config.ui.language.is_empty() {
        return i18n::Lang::from_str(&config.ui.language);
    }
    i18n::Lang::default()
}

/// モニター一覧を解決する。優先順位:
///
/// 1. `KABEKAMI_SCREEN=WxH` 環境変数（単一モニターとして扱う）
/// 2. `kscreen-doctor --outputs` による自動検出（最大4回、指数バックオフ）
/// 3. フォールバック（1920×1080 の単一モニター）
async fn resolve_screens() -> Vec<screen::Monitor> {
    // 1. 環境変数による手動指定
    if let Ok(val) = std::env::var("KABEKAMI_SCREEN") {
        if let Some((w, h)) = val.split_once('x') {
            if let (Ok(w), Ok(h)) = (w.trim().parse::<u32>(), h.trim().parse::<u32>()) {
                if w > 0 && h > 0 {
                    tracing::info!("screen from KABEKAMI_SCREEN: {}x{}", w, h);
                    return vec![screen::Monitor { name: "env".into(), width: w, height: h }];
                }
            }
        }
        tracing::warn!("invalid KABEKAMI_SCREEN='{}', expected WxH (e.g. 2560x1440)", val);
    }

    // 2. kscreen-doctor による自動検出（起動競合に備えてリトライ）
    let mut delay_secs = 0u64;
    for attempt in 1..=4u32 {
        if delay_secs > 0 {
            tracing::info!(
                "screen detection: retrying in {}s (attempt {}/4)...",
                delay_secs, attempt
            );
            tokio::time::sleep(Duration::from_secs(delay_secs)).await;
        }
        let monitors = screen::detect_all();
        if !monitors.is_empty() {
            for m in &monitors {
                tracing::info!("monitor detected: {} {}x{}", m.name, m.width, m.height);
            }
            return monitors;
        }
        if delay_secs == 0 { delay_secs = 1; } else { delay_secs *= 2; }
    }

    tracing::warn!(
        "could not detect screens after 4 attempts, using fallback {}x{}",
        FALLBACK_SCREEN_W,
        FALLBACK_SCREEN_H
    );
    vec![screen::Monitor { name: "fallback".into(), width: FALLBACK_SCREEN_W, height: FALLBACK_SCREEN_H }]
}

fn collect_watch_dirs(config: &Config) -> Vec<std::path::PathBuf> {
    let mut dirs = config.sources.directories.clone();
    for oc in &config.online_sources {
        if oc.enabled {
            dirs.push(oc.resolved_download_dir());
        }
    }
    dirs
}

fn make_ticker(interval_secs: u64) -> tokio::time::Interval {
    let period = Duration::from_secs(interval_secs);
    let mut t = interval_at(Instant::now() + period, period);
    t.set_missed_tick_behavior(MissedTickBehavior::Skip);
    t
}

/// 1 つのモニター解像度向けに壁紙を加工してキャッシュパスを返す。
async fn process_image(
    src: &Path,
    screen_w: u32,
    screen_h: u32,
    config: &Config,
    cache: &Arc<Cache>,
) -> Result<std::path::PathBuf> {
    let key = CacheKey {
        src: src.to_path_buf(),
        screen_w,
        screen_h,
        mode: config.display.mode,
        blur_sigma: config.display.blur_sigma,
        bg_darken: config.display.bg_darken,
    };
    if let Some(cached) = cache.get(&key) {
        tracing::debug!("cache hit: {}", src.display());
        return Ok(cached);
    }
    let src_owned = src.to_path_buf();
    let cache_owned = Arc::clone(cache);
    let mode = config.display.mode;
    let blur_sigma = config.display.blur_sigma;
    let bg_darken = config.display.bg_darken;
    tokio::task::spawn_blocking(move || {
        prefetch::process_for_cache(&src_owned, screen_w, screen_h, mode, blur_sigma, bg_darken, &cache_owned)
    })
    .await
    .context("image processing task panicked")?
}

/// 壁紙を加工してキャッシュし、Plasma に反映する。
///
/// マルチモニター時は各モニターの解像度で個別に処理して `set_wallpaper_multi` を呼ぶ。
async fn apply(
    src: &Path,
    screens: &[screen::Monitor],
    config: &Config,
    cache: &Arc<Cache>,
    plasma: &plasma::PlasmaShell,
) -> Result<()> {
    if screens.len() <= 1 {
        let (w, h) = screens
            .first()
            .map(|m| (m.width, m.height))
            .unwrap_or((FALLBACK_SCREEN_W, FALLBACK_SCREEN_H));
        let output = process_image(src, w, h, config, cache).await?;
        plasma.set_wallpaper(&output).await
    } else {
        let entries: Vec<(usize, std::path::PathBuf)> =
            futures_util::future::try_join_all(screens.iter().enumerate().map(
                |(idx, monitor)| async move {
                    process_image(src, monitor.width, monitor.height, config, cache)
                        .await
                        .map(|p| (idx, p))
                },
            ))
            .await?;
        let entry_refs: Vec<(usize, &Path)> = entries.iter().map(|(i, p)| (*i, p.as_path())).collect();
        plasma.set_wallpaper_multi(&entry_refs).await
    }
}

/// apply + 通知 + tray 更新 + prefetch 開始をまとめて行う。
async fn apply_and_notify(
    path: &Path,
    screens: &[screen::Monitor],
    config: &Config,
    cache: &Arc<Cache>,
    plasma: &plasma::PlasmaShell,
    notifier: &mut notify::Notifier,
    tray_handle: &Option<ksni::Handle<tray::KabekamiTray>>,
    prefetcher: &mut Prefetcher,
    scheduler: &Scheduler,
    ctx: &str,
) {
    if let Err(e) = apply(path, screens, config, cache, plasma).await {
        tracing::error!(error = %e, "{}", ctx);
        let msg = e.to_string();
        notifier.error(&msg, Some(path)).await;
        update_tray_error(tray_handle, msg).await;
    } else {
        notifier.clear();
        update_tray_ok(tray_handle, path).await;
        let (w, h) = screens
            .first()
            .map(|m| (m.width, m.height))
            .unwrap_or((FALLBACK_SCREEN_W, FALLBACK_SCREEN_H));
        start_prefetch(prefetcher, scheduler, w, h, config, cache);
    }
}

async fn update_tray_error(
    tray_handle: &Option<ksni::Handle<tray::KabekamiTray>>,
    msg: String,
) {
    if let Some(ref h) = tray_handle {
        h.update(|t| t.last_error = Some(msg)).await;
    }
}

async fn update_tray_clear_error(tray_handle: &Option<ksni::Handle<tray::KabekamiTray>>) {
    if let Some(ref h) = tray_handle {
        h.update(|t| t.last_error = None).await;
    }
}

async fn update_tray_ok(tray_handle: &Option<ksni::Handle<tray::KabekamiTray>>, path: &Path) {
    if let Some(ref h) = tray_handle {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        h.update(|t| { t.last_error = None; t.current_name = name; }).await;
    }
}

fn try_spawn_fetch(
    client: &reqwest::Client,
    configs: Vec<crate::config::OnlineSourceConfig>,
    tx: tokio::sync::mpsc::UnboundedSender<provider::FetchResult>,
    in_progress: Arc<AtomicBool>,
    ctx: provider::FetchContext,
    force: bool,
) -> bool {
    if configs.is_empty() || in_progress.load(Ordering::Acquire) {
        return false;
    }
    in_progress.store(true, Ordering::Release);
    let client = client.clone();
    tokio::spawn(async move {
        let results = provider::fetch_all_due(&configs, &client, ctx, force).await;
        for r in results {
            let _ = tx.send(r);
        }
        in_progress.store(false, Ordering::Release);
    });
    true
}

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
