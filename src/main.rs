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
mod screen_watcher;
mod session;
mod shortcuts;
mod state;
mod tray;
mod watcher;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::signal;
use tokio::time::{interval_at, Instant, MissedTickBehavior};

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

    // Config を先にロード（tracing の warn_notify 初期値を取得するため）
    let mut config = Config::load().context("failed to load config")?;

    // tracing subscriber を初期化（warn_notify は実行時に動的切り替え可能）
    let mut warn_rx = init_tracing(config.ui.warn_notify);

    tracing::info!(?config, "loaded config");

    // ブラックリストを起動時に読み込む
    let kabekami_config_dir = Config::config_path()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let mut blacklist = blacklist::Blacklist::load(&kabekami_config_dir)
        .context("failed to load blacklist")?;

    // スキャン対象と監視対象は同一。1 つの一覧を両方で使う。
    let source_dirs = collect_source_dirs(&config);
    let images = scan_images(&source_dirs, config.sources.recursive, &blacklist)
        .await
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
    // プライマリ解像度は `primary_size()` で都度導出する（`screens` と二重に
    // 持つと ScreensChanged で同期を取り違える余地が残るため）。
    let mut screens = resolve_screens().await;

    // キャッシュ・スケジューラ・先読みを初期化
    let mut cache = Arc::new(Cache::new(
        config.cache.directory.clone(),
        config.cache.max_size_mb,
    ));
    let mut scheduler = Scheduler::new(images, config.rotation.order);
    let daemon_state = state::DaemonState::load(&kabekami_config_dir);
    if daemon_state.paused {
        scheduler.pause();
        tracing::info!("restored paused state from previous session");
    }
    // 前回の壁紙を復元する。デスクトップには Plasma 側の設定で既に表示されているため
    // 再適用はせず、内部ポインタ（トレイのツールチップ・ゴミ箱/お気に入り操作の対象）
    // だけを合わせる。画像が削除済みの場合は復元をスキップする。
    if let Some(ref cur) = daemon_state.current {
        if scheduler.restore_current(cur) {
            tracing::info!("restored current wallpaper: {}", cur.display());
        } else {
            tracing::info!("saved wallpaper no longer available: {}", cur.display());
        }
    }
    let mut state_writer =
        state::StateWriter::new(kabekami_config_dir.clone(), daemon_state);
    let mut prefetcher = Prefetcher::new();

    // ディレクトリ監視を起動（環境によっては unavailable のため Option）
    // 起動時はトレイも D-Bus もまだ立っていないので、登録の同期待ちは問題ない。
    let (mut watch_rx, mut watcher_handle) =
        watcher::spawn(&source_dirs, config.sources.recursive);
    // 最後に「成功して」スキャンした対象。リロード時の再スキャン要否をこれと比べる。
    // `config` と比べないのは、スキャンが空／失敗して旧一覧を保った場合に
    // 次のリロードで再試行できるようにするため。
    let mut scanned_dirs = source_dirs;
    let mut scanned_recursive = config.sources.recursive;

    // 言語設定を解決する（環境変数 → config → デフォルト ja）
    // 初回呼び出しで言語ファイルの探索（同期 I/O）が走るが、この時点では
    // トレイも D-Bus もまだ起動しておらず待たせる相手が居ないため、
    // spawn_blocking へ逃がす意味は無い（直前の画像スキャンや Config::load も
    // 同様に同期のままである）。
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
        scheduler.is_paused(),
    )
    .await;

    // 設定ファイルの自動リロード用にトレイ／DBus とは別系統の送信ハンドルを保持
    let cmd_tx_for_config = cmd_tx.clone();

    // D-Bus デーモンインターフェースを登録（CLI からのリモート操作を受け付ける）
    let _dbus_conn = spawn_dbus_iface(cmd_tx.clone()).await;

    // セッション管理ウォッチャーを起動（ログアウト検知・Plasma 再起動検知）
    session::spawn_session_watcher(cmd_tx.clone()).await;

    // 画面構成変更の監視（壁紙更新を契機に再検出、60s スロットル）
    let screen_check_tx = screen_watcher::spawn(screens.clone(), cmd_tx.clone());

    // KDE グローバルショートカットを登録・監視する
    shortcuts::spawn_shortcut_watcher(cmd_tx).await;

    // 設定ファイル監視を起動。パスが取れない場合も含め、失敗時は
    // `watcher::spawn_config` 側が閉じた受信端を返す。
    let (mut config_change_rx, _config_watcher_handle) = match Config::config_path() {
        Ok(path) => watcher::spawn_config(&path),
        Err(e) => {
            tracing::warn!("config path unavailable, not watching config: {}", e);
            (watcher::degraded_config(), None)
        }
    };

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

    // 30 分ごとにプロバイダーを確認する。
    //
    // `tokio::time::interval` は既定で第 1 tick が即座に完了する。この直後の
    // select! ループでは SNI トレイ登録・KGlobalAccel・D-Bus インターフェースの
    // セットアップが完了した直後というタイミングになる。ここで複数プロバイダー
    // への TLS ハンドシェイクが同時に走ると、`worker_threads = 1` の唯一の
    // ワーカースレッドを（yield 点のない同期的な暗号演算で）数百 ms〜数秒
    // 占有し、その間 kabekami は D-Bus 応答を返せなくなる。KDE のシステムトレイ
    // はアイコン登録直後にプロパティを同期（ブロッキング）取得することがあり、
    // これが Plasma パネル全体の一時的なフリーズとして観測される。
    // `interval_at` で第 1 tick を数秒後にずらし、D-Bus 周りの初期化が
    // 落ち着いてからフェッチが始まるようにする。
    //
    // これは競合ウィンドウを狭めるだけで、競合そのものは無くならない
    // （初期化が 5 秒を超えれば再発しうる）。競合を構造的に無くすなら
    // `worker_threads = 2` にして D-Bus / トレイのポーリングを別スレッドへ
    // 逃がすのが本筋だが、常駐デーモンとしてアイドル時のスレッド・メモリ
    // コストを増やしたくないため、起動直後の数秒だけの問題に対しては
    // この遅延で足りると判断している。再発するようなら worker_threads を
    // 見直すこと。
    const FIRST_FETCH_DELAY: Duration = Duration::from_secs(5);
    let mut fetch_ticker = interval_at(Instant::now() + FIRST_FETCH_DELAY, Duration::from_secs(1800));
    fetch_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let fetch_in_progress = Arc::new(AtomicBool::new(false));

    // トレイに初期画像枚数と復元した現在画像名を反映
    update_tray_current_and_count(
        &tray_handle,
        scheduler.current().map(|p| p.as_path()),
        scheduler.image_count(),
    )
    .await;

    // `apply_and_notify` に渡す `ApplyCtx` を組み立てる。参照するローカル変数が多く
    // 呼び出しが 10 箇所あるため、借用リストの重複をここ 1 箇所に閉じ込める。
    // マクロにすることで、借用がステートメント単位で完結する（関数に切り出すと
    // `&mut notifier` 等を保持するクロージャが main 全体を借用してしまう）。
    macro_rules! apply_ctx {
        () => {
            &mut ApplyCtx {
                screens: &screens,
                config: &config,
                cache: &cache,
                plasma: &plasma_shell,
                tray_handle: &tray_handle,
                scheduler: &scheduler,
                screen_check_tx: screen_check_tx.as_ref(),
                notifier: &mut notifier,
                prefetcher: &mut prefetcher,
                state_writer: &mut state_writer,
            }
        };
    }

    // 起動時の即時切り替え。
    // 一時停止状態は再起動をまたいで復元されるため、停止中なら切り替えない
    // （停止したまま再起動したのに壁紙が変わる、という挙動を避ける）。
    if config.rotation.change_on_start {
        if let Some(path) = scheduler.auto_next() {
            apply_and_notify(apply_ctx!(), &path, "initial apply failed").await;
        }
    }

    let mut ticker = make_ticker(config.rotation.interval_secs);
    let mut last_cmd_at: Option<std::time::Instant> = None;

    tracing::info!("entering main loop (interval={}s)", config.rotation.interval_secs);

    loop {
        tokio::select! {
            _ = fetch_ticker.tick() => {
                if let Some(ref client) = online_client {
                    try_spawn_fetch(
                        client,
                        &config.online_sources,
                        &online_tx,
                        &fetch_in_progress,
                        &screens,
                    );
                }
            }

            Some(result) = online_rx.recv() => {
                let provider::FetchResult { provider, new_paths } = result;
                if !new_paths.is_empty() {
                    let was_empty = scheduler.current().is_none() && scheduler.peek_next().is_none();
                    let new_paths: Vec<_> = new_paths.into_iter()
                        .filter(|p| !blacklist.contains(p))
                        .collect();
                    let added = new_paths.len();
                    for path in new_paths {
                        scheduler.add_image(path);
                    }
                    tracing::info!(
                        "provider {}: {} new image(s) added to rotation",
                        provider,
                        added
                    );
                    update_tray_count(&tray_handle, scheduler.image_count()).await;
                    if added > 0 && config.ui.notify_fetch {
                        let strings = i18n::strings(lang);
                        let body = strings.notify_fetch_body
                            .replace("{provider}", &provider)
                            .replace("{count}", &added.to_string());
                        notifier.info(strings.notify_fetch_title, &body).await;
                    }
                    if was_empty {
                        if let Some(path) = scheduler.auto_next() {
                            apply_and_notify(apply_ctx!(), &path, "online: initial apply failed").await;
                        }
                    }
                }
            }

            _ = ticker.tick() => {
                if let Some(path) = scheduler.auto_next() {
                    apply_and_notify(apply_ctx!(), &path, "auto apply failed").await;
                }
            }

            Some(cmd) = cmd_rx.recv() => {
                let now = std::time::Instant::now();
                if should_throttle(&cmd, last_cmd_at, now) {
                    tracing::debug!("command throttled (< 500ms): {:?}", cmd);
                    continue;
                }
                last_cmd_at = Some(now);
                match cmd {
                    TrayCmd::Next => {
                        prefetcher.abort();
                        if let Some(path) = scheduler.next() {
                            apply_and_notify(apply_ctx!(), &path, "tray Next failed").await;
                        }
                        ticker = make_ticker(config.rotation.interval_secs);
                    }

                    TrayCmd::Prev => {
                        if let Some(path) = scheduler.prev() {
                            apply_and_notify(apply_ctx!(), &path, "tray Prev failed").await;
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
                        let paused = scheduler.is_paused();
                        state_writer.persist(paused, scheduler.current().map(|p| p.as_path())).await;
                        if let Some(ref h) = tray_handle {
                            h.update(|t| t.paused = paused).await;
                        }
                    }

                    TrayCmd::SetMode(mode) => {
                        tracing::info!("display mode → {:?}", mode);
                        config.display.mode = mode;
                        // トレイでの変更を再起動後も保つ。保存で発生する監視イベントは
                        // ReloadConfig 側の同値スキップで吸収される。
                        persist_config(&config, "display mode").await;
                        // 画像は同じだがモードが変わるとキャッシュキーも変わるので作り直す。
                        // 適用後の通知・トレイ・先読みは apply_and_notify に任せる
                        // （ここで手書きすると再適用経路が 2 系統に分かれる）。
                        if let Some(cur) = scheduler.current().cloned() {
                            apply_and_notify(apply_ctx!(), &cur, "reapply after mode change failed").await;
                        }
                    }

                    TrayCmd::SetInterval(secs) => {
                        let secs = secs.max(crate::config::MIN_INTERVAL_SECS);
                        tracing::info!("interval → {}s", secs);
                        config.rotation.interval_secs = secs;
                        persist_config(&config, "interval").await;
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
                                        apply_and_notify(apply_ctx!(), &next, "apply after trash failed").await;
                                    }
                                    update_tray_count(&tray_handle, scheduler.image_count()).await;
                                    ticker = make_ticker(config.rotation.interval_secs);
                                }
                            }
                        }
                    }

                    TrayCmd::BlacklistCurrent => {
                        if !config.ui.enable_blacklist {
                            tracing::debug!("blacklist disabled in config, ignoring");
                        } else if let Some(path) = scheduler.current().cloned() {
                            if blacklist.add(&path).await {
                                tracing::info!("blacklisted: {}", path.display());
                                scheduler.remove_image(&path);
                                prefetcher.abort();
                                match scheduler.next() {
                                    Some(next) => {
                                        apply_and_notify(apply_ctx!(), &next, "apply after blacklist failed").await;
                                        update_tray_count(&tray_handle, scheduler.image_count()).await;
                                    }
                                    // 最後の 1 枚を除外した場合。`remove_image` が
                                    // `current` を落としているので名前は空になる。
                                    None => {
                                        update_tray_current_and_count(
                                            &tray_handle,
                                            scheduler.current().map(|p| p.as_path()),
                                            scheduler.image_count(),
                                        )
                                        .await;
                                    }
                                }
                                ticker = make_ticker(config.rotation.interval_secs);
                            } else {
                                // 失敗の詳細は `save_offloaded` が warn に出す
                                tracing::error!("blacklist: failed to save {}", path.display());
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
                            Ok(new_cfg) if new_cfg == config => {
                                // 内容が同一なら何もしない。トレイからのモード／間隔変更で
                                // デーモン自身が config.toml を保存した場合もここで弾かれ、
                                // 不要な再スキャンとスケジューラ再構築を避けられる。
                                tracing::debug!("config unchanged, skipping reload");
                            }
                            Ok(new_cfg) => {
                                tracing::info!("reloading config");

                                // 対象ディレクトリが変わっていなければ再スキャンも監視の
                                // 張り替えも不要。単一ワーカーなので、大きなソースでは
                                // `interval_secs` を変えただけの保存でも数百 ms 止まりうる。
                                let source_dirs = collect_source_dirs(&new_cfg);
                                let sources_changed = source_dirs != scanned_dirs
                                    || new_cfg.sources.recursive != scanned_recursive;
                                // 監視が全ディレクトリに張れていなければ、設定保存が唯一の
                                // 再スキャン契機になる。一部失敗も同じ扱い（そのディレクトリの
                                // イベントは届かないので、再スキャンと登録再試行の両方が要る）。
                                let watching_everything =
                                    watcher_handle.as_ref().is_some_and(|w| w.is_complete());
                                let needs_rescan = sources_changed || !watching_everything;

                                // 走行中の先読みが温めている画像。あとで変わっていれば、
                                // その先読みはもう「次の画像」を指していない。
                                let warming = scheduler.peek_next().cloned();
                                if needs_rescan {
                                    match scan_images(&source_dirs, new_cfg.sources.recursive, &blacklist).await {
                                        Ok(images) if !images.is_empty() => {
                                            tracing::info!("reload: {} image(s) found", images.len());
                                            // 一時停止状態と現在画像は rebuild が引き継ぐ
                                            scheduler.rebuild(images, new_cfg.rotation.order);
                                            // 画像一覧を差し替えたときだけ監視対象も差し替える。
                                            // 旧一覧を保ったまま監視だけ新ディレクトリへ移すと、
                                            // 旧画像の削除イベントを取りこぼし、消えた画像が
                                            // ローテーションに残り続ける。
                                            (watch_rx, watcher_handle) = spawn_dir_watcher_offloaded(
                                                &source_dirs,
                                                new_cfg.sources.recursive,
                                            )
                                            .await;
                                            scanned_dirs = source_dirs;
                                            scanned_recursive = new_cfg.sources.recursive;
                                        }
                                        // 空・失敗のときは画像一覧も監視対象も据え置く
                                        // （ネットワークマウントの一時的な不在などで
                                        // ローテーションが空になるのを避ける）。
                                        // `scanned_dirs` を更新しないので次のリロードで再試行される。
                                        Ok(_) => tracing::warn!(
                                            "reload: no images found, keeping current list and watcher"
                                        ),
                                        Err(e) => tracing::warn!(
                                            "reload: scan error, keeping current list and watcher: {}", e
                                        ),
                                    }
                                } else {
                                    tracing::debug!("reload: source dirs unchanged, skipping rescan");
                                }

                                // rebuild を通らなかった場合も並び順は反映する
                                // （通っていれば同値なので何もしない）。
                                scheduler.set_order(new_cfg.rotation.order);

                                // キャッシュキーに効く設定が変わったか。変わっていなければ
                                // 温まったキャッシュも走行中の先読みもそのまま活かす。
                                let cache_changed = new_cfg.cache != config.cache;
                                let display_changed = new_cfg.display != config.display;

                                // 先読みの指す先が変わったときだけ捨てる（据え置きなら
                                // ほぼ終わったデコードを捨てる理由がない）。表示・キャッシュ
                                // 設定はパスが同じでもキーが変わるので別に見る。
                                if scheduler.peek_next() != warming.as_ref()
                                    || cache_changed
                                    || display_changed
                                {
                                    prefetcher.abort();
                                }
                                if cache_changed {
                                    cache = Arc::new(Cache::new(
                                        new_cfg.cache.directory.clone(),
                                        new_cfg.cache.max_size_mb,
                                    ));
                                }

                                ticker = make_ticker(new_cfg.rotation.interval_secs);

                                let new_lang = resolve_lang(&new_cfg);
                                if new_lang != lang {
                                    lang = new_lang;
                                    notifier = notify::Notifier::new(lang).await;
                                }

                                if new_cfg.ui.warn_notify != config.ui.warn_notify {
                                    WARN_NOTIFY_ENABLED.store(new_cfg.ui.warn_notify, Ordering::Relaxed);
                                    tracing::info!(
                                        "warn_notify toggled: {} → {}",
                                        config.ui.warn_notify,
                                        new_cfg.ui.warn_notify
                                    );
                                }

                                config = new_cfg;

                                // rebuild を通れば新しい一覧に残っていた画像、通らなければ
                                // 据え置きの current。どちらも「いま表示しているべき画像」。
                                match scheduler.current().cloned() {
                                    // 記録は `apply_and_notify` 内、成功時のみ。先に persist
                                    // すると、適用に失敗した壁紙を「現在」として保存し、
                                    // 再起動後のトレイやゴミ箱操作が画面に無い画像を指す
                                    // （分岐を畳まないこと）。
                                    Some(cur) => {
                                        apply_and_notify(apply_ctx!(), &cur, "reload: reapply failed").await;
                                    }
                                    // current が落ちた場合は apply_and_notify を通らないので、
                                    // state に残る旧画像を明示的に消す。
                                    None => {
                                        state_writer.persist(scheduler.is_paused(), None).await;
                                    }
                                }

                                if let Some(ref h) = tray_handle {
                                    let mode = config.display.mode;
                                    let secs = config.rotation.interval_secs;
                                    let strings = crate::i18n::strings(lang);
                                    let count = scheduler.image_count();
                                    let has_fav = config.sources.favorites_dir.is_some();
                                    let bl_enabled = config.ui.enable_blacklist;
                                    let name = tray_display_name(scheduler.current().map(|p| p.as_path()));
                                    h.update(|t| {
                                        t.mode = mode;
                                        t.interval_secs = secs;
                                        t.strings = strings;
                                        t.image_count = count;
                                        t.has_favorites_dir = has_fav;
                                        t.blacklist_enabled = bl_enabled;
                                        t.current_name = name;
                                    }).await;
                                }

                                tracing::info!("config reload complete");
                            }
                        }
                    }

                    TrayCmd::OpenSettings => {
                        // fork/exec はデーモンのアドレス空間サイズに比例してブロックする
                        // （加工直後は RSS が大きい）。隣の `OpenCurrent` と同じく
                        // 単一ワーカーの外へ出す。
                        tokio::task::spawn_blocking(|| {
                            match std::process::Command::new("kabekami-config").spawn() {
                                Ok(_) => tracing::info!("launched kabekami-config"),
                                Err(e) => tracing::warn!("failed to launch kabekami-config: {}", e),
                            }
                        });
                    }

                    TrayCmd::PlasmaRestarted => {
                        tracing::info!("Plasma restarted, re-applying wallpaper");
                        if let Some(path) = scheduler.current().cloned() {
                            apply_and_notify(apply_ctx!(), &path, "reapply after Plasma restart failed").await;
                        }
                    }

                    TrayCmd::ScreensChanged(new_screens) => {
                        let (new_w, new_h) = primary_size(&new_screens);
                        tracing::info!(
                            "screens updated: {} monitor(s), primary {}x{}",
                            new_screens.len(), new_w, new_h
                        );
                        screens = new_screens;
                        // 解像度が変わるとキャッシュキーも変わるため、現在の壁紙を
                        // 新しい解像度で再加工して適用する。
                        if let Some(path) = scheduler.current().cloned() {
                            apply_and_notify(apply_ctx!(), &path, "reapply after screen change failed").await;
                        }
                    }

                    TrayCmd::Quit => {
                        tracing::info!("quit requested from tray");
                        break;
                    }
                }
            }

            Some(ev) = watch_rx.recv() => {
                // 大量コピー時、途中の枚数は誰も読めない。イベントはパスを運ぶので捨てず、
                // 処理はして更新だけまとめる。上限を置くのは、この中に await が無く
                // `remove_image` が画像数に比例するため（残りは次のループで拾う）。
                const MAX_DRAIN_PER_PASS: usize = 32;
                let count_before = scheduler.image_count();
                apply_watch_event(ev, &mut scheduler, &blacklist);
                for _ in 1..MAX_DRAIN_PER_PASS {
                    let Ok(ev) = watch_rx.try_recv() else { break };
                    apply_watch_event(ev, &mut scheduler, &blacklist);
                }
                // 枚数が動いていなければ往復も要らない
                if scheduler.image_count() != count_before {
                    update_tray_count(&tray_handle, scheduler.image_count()).await;
                }
            }

            // config.toml の変更を検知したら、連続イベントを集約してから ReloadConfig を送信。
            // まず recv() で待機し、その後 try_recv() で保留中のイベントをドレインし、
            // 100ms 待機後に 1 つの ReloadConfig だけを送信して、バーストを確実に集約する。
            Some(()) = config_change_rx.recv() => {
                // Drain any additional pending config change events
                while config_change_rx.try_recv().is_ok() {}
                tracing::info!("config file changed; waiting 100ms to coalesce events");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                // Drain again in case more events arrived during sleep
                while config_change_rx.try_recv().is_ok() {}
                tracing::info!("queueing single ReloadConfig after debounce");
                let _ = cmd_tx_for_config.send(TrayCmd::ReloadConfig);
            }

            msg = warn_rx.recv() => {
                if let Some(msg) = msg {
                    notifier.warn(&msg).await;
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

/// ソースディレクトリを走査し、ブラックリストを除いた画像一覧を返す。
///
/// `scanner::scan` は同期 I/O。単一ワーカーを占有すると D-Bus・トレイ・監視が
/// 止まるので `spawn_blocking` へ逃がす。絞り込みはメモリ上なので呼び出し側で行う。
async fn scan_images(
    scan_dirs: &[std::path::PathBuf],
    recursive: bool,
    blacklist: &blacklist::Blacklist,
) -> Result<Vec<std::path::PathBuf>> {
    let dirs = scan_dirs.to_vec();
    let scanned = tokio::task::spawn_blocking(move || crate::scanner::scan(&dirs, recursive))
        .await
        .context("scan task panicked")??;
    Ok(scanned.into_iter().filter(|p| !blacklist.contains(p)).collect())
}

/// 監視の登録も同期。`notify` の `watch()` は内部スレッドの応答を待ち、`recursive`
/// ならツリー全体の走査が終わるまで戻らないので、走査と同じく `spawn_blocking` へ。
async fn spawn_dir_watcher_offloaded(
    source_dirs: &[std::path::PathBuf],
    recursive: bool,
) -> (
    tokio::sync::mpsc::Receiver<watcher::WatchEvent>,
    Option<watcher::DirWatcher>,
) {
    let dirs = source_dirs.to_vec();
    match tokio::task::spawn_blocking(move || watcher::spawn(&dirs, recursive)).await {
        Ok(pair) => pair,
        Err(e) => {
            // 監視の起動に失敗したのと同じ縮退状態にする。
            tracing::warn!("watcher task panicked, running without file watcher: {}", e);
            (watcher::degraded(), None)
        }
    }
}

/// tracing subscriber を初期化する。
///
/// `WarnNotifyLayer` は常時インストールし、実行時の有効・無効は
/// `WARN_NOTIFY_ENABLED` フラグで切り替える（`config.toml` 編集で動的反映）。
fn init_tracing(warn_notify: bool) -> tokio::sync::mpsc::UnboundedReceiver<String> {
    use tracing_subscriber::{fmt, EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("kabekami=info,warn"));

    WARN_NOTIFY_ENABLED.store(warn_notify, Ordering::Relaxed);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .with(WarnNotifyLayer { tx })
        .init();
    rx
}

/// CLI 引数を解析して `CliCmd` を返す。引数がなければ `None`（デーモンモード）。
fn parse_cli() -> Result<Option<CliCmd>> {
    let mut args = std::env::args().skip(1).peekable();
    let Some(arg) = args.next() else { return Ok(None) };

    let cmd = match arg.as_str() {
        "--next"               => CliCmd::Next,
        "--prev"               => CliCmd::Prev,
        "--toggle-pause"       => CliCmd::TogglePause,
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

struct WarnNotifyLayer {
    tx: tokio::sync::mpsc::UnboundedSender<String>,
}

/// `warn_notify` の現在状態。`config.toml` 編集で動的に切り替えられる。
/// 起動時に一度だけ `WarnNotifyLayer` をインストールし、`on_event` で
/// このフラグを参照することで再起動なしの ON/OFF を実現する。
static WARN_NOTIFY_ENABLED: AtomicBool = AtomicBool::new(false);

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
        if !WARN_NOTIFY_ENABLED.load(Ordering::Relaxed) {
            return;
        }
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
        return i18n::Lang::from_code(val.trim());
    }
    if !config.ui.language.is_empty() {
        return i18n::Lang::from_code(&config.ui.language);
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

/// スキャンと監視の対象ディレクトリ。ローカル指定分に、有効なオンライン
/// ソースのダウンロード先を足したもの。両者は常に同一集合。
fn collect_source_dirs(config: &Config) -> Vec<std::path::PathBuf> {
    let mut dirs = config.sources.directories.clone();
    for oc in &config.online_sources {
        if oc.enabled {
            dirs.push(oc.resolved_download_dir());
        }
    }
    dirs
}

/// 先頭（プライマリ扱い）モニターの解像度。
///
/// 空スライスは実際には来ない（`resolve_screens` は必ず 1 つ返し、`screen_watcher` は
/// 空の検出を捨てる）。`unwrap_or` は panic 避けの保険。
fn primary_size(screens: &[screen::Monitor]) -> (u32, u32) {
    screens
        .first()
        .map(|m| (m.width, m.height))
        .unwrap_or((FALLBACK_SCREEN_W, FALLBACK_SCREEN_H))
}

/// `screens` に現れる解像度を重複なしで列挙する（元の並び順を保つ）。
///
/// `CacheKey` は解像度を含むので同解像度のモニターは同じキー。加工も先読みも
/// 解像度の数だけで足りる。
fn distinct_sizes(screens: &[screen::Monitor]) -> Vec<(u32, u32)> {
    let mut sizes: Vec<(u32, u32)> = Vec::with_capacity(screens.len());
    for m in screens {
        if !sizes.contains(&(m.width, m.height)) {
            sizes.push((m.width, m.height));
        }
    }
    if sizes.is_empty() {
        // `primary_size` と同じフォールバック（実際には空スライスは来ない）
        sizes.push(primary_size(screens));
    }
    sizes
}

/// 連続して届いたコマンドの 2 発目以降を捨てるか判定する。
///
/// 500ms という長さの理由: KRunner で `kabekami --next` を実行すると
/// CLI バイナリの起動 + D-Bus 接続 (50-200ms) を 2 回経由して daemon に
/// 届くケースがあり、短いスロットル (100ms 等) だと二重実行を吸収しきれない。
///
/// システムイベント系（Quit / PlasmaRestarted / ReloadConfig / ScreensChanged）は
/// ユーザー操作ではなく、取りこぼすと内部状態が画面と食い違うため除外する。
fn should_throttle(
    cmd: &TrayCmd,
    last_cmd_at: Option<std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    const THROTTLE: Duration = Duration::from_millis(500);
    let exempt = matches!(
        cmd,
        TrayCmd::Quit | TrayCmd::PlasmaRestarted | TrayCmd::ReloadConfig | TrayCmd::ScreensChanged(_)
    );
    !exempt && last_cmd_at.is_some_and(|t| now.duration_since(t) < THROTTLE)
}

fn make_ticker(interval_secs: u64) -> tokio::time::Interval {
    let period = Duration::from_secs(interval_secs);
    let mut t = interval_at(Instant::now() + period, period);
    t.set_missed_tick_behavior(MissedTickBehavior::Skip);
    t
}

/// `screens` を表示するのに必要なキャッシュキーの一式（解像度ごとに 1 つ）。
///
/// 適用側と先読み側の唯一の共有点。キーの中身までここで決めるので、表示設定に
/// フィールドが増えてもズレない。ズレても無音で、先読みが無駄に回り続ける。
fn cache_keys(src: &Path, screens: &[screen::Monitor], config: &Config) -> Vec<CacheKey> {
    distinct_sizes(screens)
        .into_iter()
        .map(|(w, h)| CacheKey::new(src, w, h, &config.display))
        .collect()
}

/// 1 つのキャッシュキー向けに壁紙を加工してキャッシュパスを返す。
async fn process_key(key: &CacheKey, cache: &Arc<Cache>) -> Result<std::path::PathBuf> {
    let src = key.src.as_path();
    if let Some(cached) = cache.get(key) {
        tracing::debug!("cache hit: {}", src.display());
        return Ok(cached);
    }
    let cache_owned = Arc::clone(cache);
    let key_owned = key.clone();
    tokio::task::spawn_blocking(move || prefetch::process_for_cache(&key_owned, &cache_owned))
        .await
        .context("image processing task panicked")?
}

/// 壁紙を加工してキャッシュし、Plasma に反映する。
///
/// 解像度ごとに 1 回だけ加工して同解像度のモニターで使い回す。並列に投げると
/// `process_for_cache` の二重チェックをすり抜けて同じ画像を 2 回デコードする。
/// モニター 1 台でも分岐しない（`set_wallpaper_multi` が 1 件なら委譲、0 件なら無処理）。
async fn apply(
    src: &Path,
    screens: &[screen::Monitor],
    config: &Config,
    cache: &Arc<Cache>,
    plasma: &plasma::PlasmaShell,
) -> Result<()> {
    let processed: std::collections::HashMap<(u32, u32), std::path::PathBuf> =
        futures_util::future::try_join_all(cache_keys(src, screens, config).into_iter().map(
            |key| async move {
                let size = (key.screen_w, key.screen_h);
                process_key(&key, cache).await.map(|p| (size, p))
            },
        ))
        .await?
        .into_iter()
        .collect();
    // `processed` のキーは `screens` の解像度そのものなので、取りこぼしは起きない
    let entries: Vec<(usize, &Path)> = screens
        .iter()
        .enumerate()
        .filter_map(|(idx, m)| processed.get(&(m.width, m.height)).map(|p| (idx, p.as_path())))
        .collect();
    plasma.set_wallpaper_multi(&entries).await
}

/// `apply_and_notify` の long-lived な引数束。
/// メインループのスコープ変数を借用してまとめる。
struct ApplyCtx<'a> {
    screens: &'a [screen::Monitor],
    config: &'a Config,
    cache: &'a Arc<Cache>,
    plasma: &'a plasma::PlasmaShell,
    tray_handle: &'a Option<ksni::Handle<tray::KabekamiTray>>,
    scheduler: &'a Scheduler,
    screen_check_tx: Option<&'a tokio::sync::mpsc::UnboundedSender<()>>,
    notifier: &'a mut notify::Notifier,
    prefetcher: &'a mut Prefetcher,
    /// 適用のたびに現在の壁紙を記録する（内容に変化がなければ書き込みは省かれる）。
    state_writer: &'a mut state::StateWriter,
}

/// apply + 通知 + tray 更新 + prefetch 開始 + 画面構成再検出トリガーをまとめて行う。
async fn apply_and_notify(ctx: &mut ApplyCtx<'_>, path: &Path, log_ctx: &str) {
    if let Err(e) = apply(path, ctx.screens, ctx.config, ctx.cache, ctx.plasma).await {
        tracing::error!(error = %e, "{}", log_ctx);
        let msg = e.to_string();
        ctx.notifier.error(&msg, Some(path)).await;
        update_tray_error(ctx.tray_handle, msg).await;
    } else {
        ctx.notifier.clear();
        update_tray_ok(ctx.tray_handle, path).await;
        // 現在の壁紙を記録しておき、再起動後も同じ画像を「現在」として扱えるようにする
        ctx.state_writer.persist(ctx.scheduler.is_paused(), Some(path)).await;
        start_prefetch(ctx.prefetcher, ctx.scheduler, ctx.screens, ctx.config, ctx.cache);
        // 画面構成の再検出を要求（ウォッチャー側で 60s スロットル）
        if let Some(tx) = ctx.screen_check_tx {
            let _ = tx.send(());
        }
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

/// 監視イベント 1 件を画像一覧に反映する。トレイ更新は呼び出し側でまとめる。
fn apply_watch_event(
    ev: watcher::WatchEvent,
    scheduler: &mut Scheduler,
    blacklist: &blacklist::Blacklist,
) {
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
}

/// トレイの壁紙名と枚数を 1 回の `update` で更新する（分けると往復が 2 回になる）。
async fn update_tray_current_and_count(
    tray_handle: &Option<ksni::Handle<tray::KabekamiTray>>,
    current: Option<&Path>,
    count: usize,
) {
    if let Some(ref h) = tray_handle {
        let name = tray_display_name(current);
        h.update(|t| {
            t.current_name = name;
            t.image_count = count;
        })
        .await;
    }
}

/// トレイの画像枚数表示を更新する。
async fn update_tray_count(
    tray_handle: &Option<ksni::Handle<tray::KabekamiTray>>,
    count: usize,
) {
    if let Some(ref h) = tray_handle {
        h.update(|t| t.image_count = count).await;
    }
}

/// トレイに表示する壁紙名（拡張子付きファイル名）。
/// 取得できない場合は空文字列を返す。
fn tray_display_name(path: Option<&Path>) -> String {
    path.and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

async fn update_tray_ok(tray_handle: &Option<ksni::Handle<tray::KabekamiTray>>, path: &Path) {
    if let Some(ref h) = tray_handle {
        let name = tray_display_name(Some(path));
        h.update(|t| { t.last_error = None; t.current_name = name; }).await;
    }
}

/// 設定を保存し、失敗しても警告に留めて処理を続行する。
/// `what` は失敗ログに出す変更内容（例: `"display mode"`）。
async fn persist_config(config: &Config, what: &str) {
    let owned = config.clone();
    state::save_offloaded(move || owned.save(), what).await;
}

/// 期限の来たオンラインプロバイダーの取得をバックグラウンドで起動する。
/// 走行中のフェッチがあれば何もしない。
fn try_spawn_fetch(
    client: &reqwest::Client,
    configs: &[crate::config::OnlineSourceConfig],
    tx: &tokio::sync::mpsc::UnboundedSender<provider::FetchResult>,
    in_progress: &Arc<AtomicBool>,
    screens: &[screen::Monitor],
) {
    if configs.is_empty() {
        return;
    }
    // 確認＋セットを単一の atomic 操作で行う。`true` が返れば既に走行中。
    if in_progress.swap(true, Ordering::AcqRel) {
        return;
    }
    // ここから先は必ず spawn するので、この時点で初めて複製する。
    let configs = configs.to_vec();
    let client = client.clone();
    let tx = tx.clone();
    let in_progress = Arc::clone(in_progress);
    let (screen_w, screen_h) = primary_size(screens);
    let ctx = provider::FetchContext { screen_w, screen_h };
    tokio::spawn(async move {
        // パニックしてもスタック巻き戻し中に Drop が走り、フラグが false に戻る。
        // これによりタスクが死んでも以降のフェッチが永久にブロックされなくなる。
        let _guard = FlagGuard(in_progress);
        for r in provider::fetch_all_due(&configs, &client, ctx).await {
            let _ = tx.send(r);
        }
    });
}

/// `Arc<AtomicBool>` を `Drop` で `false` に戻す RAII ガード。
/// 非同期タスクのパニックでも確実にフラグが解放されるようにするために使う。
struct FlagGuard(Arc<AtomicBool>);
impl Drop for FlagGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// 次に表示する画像の先読みを開始する（`apply` と同じキー一式を温める）。
fn start_prefetch(
    prefetcher: &mut Prefetcher,
    scheduler: &Scheduler,
    screens: &[screen::Monitor],
    config: &Config,
    cache: &Arc<Cache>,
) {
    if !config.rotation.prefetch {
        return;
    }
    if let Some(next) = scheduler.peek_next() {
        prefetcher.start(cache_keys(next, screens, config), cache.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DisplayMode;

    /// スキャン・監視の対象ディレクトリは「ローカル指定 + 有効なオンライン
    /// ソースのダウンロード先」。ここを間違うと壁紙が黙って現れない／消える。
    /// スキャンと監視で同じ関数を使うので、ズレる余地も無くなっている。
    #[test]
    fn source_dirs_include_only_enabled_online_sources() {
        let config: Config = toml::from_str(
            r#"
            [sources]
            directories = ["/pics/a", "/pics/b"]

            [[online_sources]]
            provider = "bing"
            enabled = true
            download_dir = "/dl/bing"

            [[online_sources]]
            provider = "wallhaven"
            enabled = false
            download_dir = "/dl/wallhaven"
            "#,
        )
        .expect("test config should parse");

        assert_eq!(
            collect_source_dirs(&config),
            vec![
                std::path::PathBuf::from("/pics/a"),
                std::path::PathBuf::from("/pics/b"),
                std::path::PathBuf::from("/dl/bing"),
            ],
            "無効なオンラインソースのダウンロード先は対象に入れない"
        );
    }

    #[test]
    fn source_dirs_without_online_sources() {
        let mut config = Config::default();
        config.sources.directories = vec![std::path::PathBuf::from("/pics")];
        assert_eq!(collect_source_dirs(&config), vec![std::path::PathBuf::from("/pics")]);
    }

    /// 解像度が同じモニターは同じ `CacheKey` になるため、加工も先読みも
    /// 1 回で足りる。ここが重複すると同じ画像を並列に 2 回デコードする。
    fn mon(name: &str, width: u32, height: u32) -> screen::Monitor {
        screen::Monitor { name: name.to_string(), width, height }
    }

    /// 解像度の違う 2 台 + 片方と同じ解像度の 3 台目。
    fn mixed_screens() -> Vec<screen::Monitor> {
        vec![
            mon("DP-1", 3840, 2160),
            mon("DP-2", 1920, 1080),
            mon("HDMI-1", 3840, 2160),
        ]
    }

    /// `cache_keys` は適用側 (`apply`) と先読み側 (`start_prefetch`) の唯一の共有点。
    /// 解像度ごとに 1 つで、表示設定がそのまま乗ることを固定する。ここがズレると
    /// エラーにならず、先読みが誰も引かないファイルを温め続ける。
    #[test]
    fn cache_keys_are_one_per_resolution_and_carry_display_settings() {
        let mut config = Config::default();
        config.display.mode = DisplayMode::Fit;
        config.display.blur_sigma = 12.5;
        config.display.bg_darken = 0.25;
        let src = std::path::Path::new("/pics/a.jpg");

        let keys = cache_keys(src, &mixed_screens(), &config);

        assert_eq!(
            keys.iter().map(|k| (k.screen_w, k.screen_h)).collect::<Vec<_>>(),
            vec![(3840, 2160), (1920, 1080)],
            "解像度ごとに 1 つ（同じ解像度は畳む）"
        );
        for k in &keys {
            assert_eq!(k.src, src, "元画像は共通");
            assert_eq!(k.mode, DisplayMode::Fit, "表示モードが落ちている");
            assert_eq!(k.blur_sigma, 12.5, "blur_sigma が落ちている");
            assert_eq!(k.bg_darken, 0.25, "bg_darken が落ちている");
        }
    }

    #[test]
    fn distinct_sizes_dedupes_identical_resolutions() {
        let screens = mixed_screens();
        assert_eq!(
            distinct_sizes(&screens),
            vec![(3840, 2160), (1920, 1080)],
            "重複を除き、最初に現れた順を保つ"
        );
    }

    /// `primary_size` と同じフォールバックに揃える。空でも 0 件を返すと
    /// 先読みも加工も走らなくなる。
    #[test]
    fn distinct_sizes_falls_back_when_no_screens() {
        assert_eq!(
            distinct_sizes(&[]),
            vec![(FALLBACK_SCREEN_W, FALLBACK_SCREEN_H)]
        );
    }

    #[test]
    fn first_command_is_never_throttled() {
        assert!(!should_throttle(&TrayCmd::Next, None, std::time::Instant::now()));
    }

    #[test]
    fn user_command_after_500ms_passes() {
        let first = std::time::Instant::now();
        assert!(!should_throttle(&TrayCmd::Next, Some(first), first + Duration::from_millis(500)));
    }

    /// KRunner 経由だと同じコマンドが 2 回届くことがあるため、
    /// ユーザー操作系はすべて連打を吸収する。
    #[test]
    fn user_commands_within_500ms_are_throttled() {
        let first = std::time::Instant::now();
        let again = first + Duration::from_millis(120);
        for cmd in [
            TrayCmd::Next,
            TrayCmd::Prev,
            TrayCmd::TogglePause,
            TrayCmd::SetMode(DisplayMode::Fill),
            TrayCmd::SetInterval(30),
            TrayCmd::OpenCurrent,
            TrayCmd::DeleteCurrent,
            TrayCmd::BlacklistCurrent,
            TrayCmd::CopyToFavorites,
            TrayCmd::OpenSettings,
        ] {
            assert!(should_throttle(&cmd, Some(first), again), "{cmd:?} は連打を吸収すべき");
        }
    }

    /// 取りこぼすと内部状態が実際のデスクトップとずれるコマンドは、
    /// 直前に別のコマンドを処理していても必ず通す。
    #[test]
    fn system_commands_bypass_the_throttle() {
        let first = std::time::Instant::now();
        let immediately_after = first + Duration::from_millis(1);
        for cmd in [
            TrayCmd::Quit,
            TrayCmd::PlasmaRestarted,
            TrayCmd::ReloadConfig,
            TrayCmd::ScreensChanged(vec![screen::Monitor {
                name: "DP-1".into(),
                width: 1920,
                height: 1080,
            }]),
        ] {
            assert!(
                !should_throttle(&cmd, Some(first), immediately_after),
                "{cmd:?} はスロットリングの対象外であるべき"
            );
        }
    }
}
