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

use std::ops::ControlFlow;
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
    Daemon::new().await?.run().await
}

// ── デーモン本体 ─────────────────────────────────────────────────────────────

/// 常駐デーモンの状態一式。
///
/// メインループの各分岐は `&mut self` を取るメソッドとして実装する。
/// `tokio::select!` はフューチャの借用をハンドラ本体に持ち越さないため、
/// 受信チャンネルやタイマーもフィールドとして保持できる。
struct Daemon {
    // ── 設定 ──
    config: Config,
    blacklist: blacklist::Blacklist,
    lang: i18n::Lang,

    // ── 壁紙の適用経路 ──
    /// 接続されているモニター。先頭をプライマリとして扱う。
    screens: Vec<screen::Monitor>,
    cache: Arc<Cache>,
    scheduler: Scheduler,
    prefetcher: Prefetcher,
    plasma: plasma::PlasmaShell,
    notifier: notify::Notifier,
    /// 適用のたびに現在の壁紙を記録する（内容に変化がなければ書き込みは省かれる）。
    state_writer: state::StateWriter,
    tray_handle: Option<ksni::Handle<tray::KabekamiTray>>,

    // ── イベント源 ──
    /// 設定ファイル監視から `ReloadConfig` を自分に送るための送信ハンドル。
    cmd_tx: tokio::sync::mpsc::UnboundedSender<TrayCmd>,
    cmd_rx: tokio::sync::mpsc::UnboundedReceiver<TrayCmd>,
    /// 直近にコマンドを受理した時刻。二重実行のスロットリングに使う。
    last_cmd_at: Option<std::time::Instant>,
    watch_rx: tokio::sync::mpsc::Receiver<watcher::WatchEvent>,
    config_change_rx: tokio::sync::mpsc::UnboundedReceiver<()>,
    warn_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    /// 画面構成の再検出要求（ウォッチャー側で 60s スロットル）。
    screen_check_tx: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    /// 自動切り替えタイマー。間隔変更と手動切り替えで張り替える。
    ticker: tokio::time::Interval,

    // ── オンライン取得 ──
    online_tx: tokio::sync::mpsc::UnboundedSender<provider::FetchResult>,
    online_rx: tokio::sync::mpsc::UnboundedReceiver<provider::FetchResult>,
    /// HTTP クライアントの初期化に失敗した環境では `None`（オンライン取得を諦める）。
    online_client: Option<reqwest::Client>,
    /// フェッチタスクと共有するため `Mutex`。`ReloadConfig` で差し替える。
    online_configs: Arc<std::sync::Mutex<Vec<crate::config::OnlineSourceConfig>>>,
    fetch_in_progress: Arc<AtomicBool>,
    fetch_ticker: tokio::time::Interval,

    // ── 生存させるだけのハンドル ──
    /// ドロップすると監視が止まる。`ReloadConfig` で張り替える。
    _watcher_handle: Option<watcher::DirWatcher>,
    _config_watcher_handle: Option<watcher::DirWatcher>,
    /// ドロップするとバス名を手放し、CLI からの操作を受け付けられなくなる。
    _dbus_conn: Option<zbus::Connection>,
}

impl Daemon {
    /// 設定の読み込みから各ウォッチャーの起動までを行う。
    /// 画像が 1 枚も無く、オンラインソースも無効なら失敗する。
    async fn new() -> Result<Self> {
        // Config を先にロード（tracing の warn_notify 初期値を取得するため）
        let config = Config::load().context("failed to load config")?;

        // tracing subscriber を初期化（warn_notify は実行時に動的切り替え可能）
        let warn_rx = init_tracing(config.ui.warn_notify);

        tracing::info!(?config, "loaded config");

        // ブラックリストを起動時に読み込む
        let config_dir = Config::config_path()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let blacklist = blacklist::Blacklist::load(&config_dir)
            .context("failed to load blacklist")?;

        let images = build_filtered_images_list(
            &collect_source_dirs(&config),
            config.sources.recursive,
            &blacklist,
        )
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

        // キャッシュ・スケジューラ・先読みを初期化
        let cache = Arc::new(Cache::new(
            config.cache.directory.clone(),
            config.cache.max_size_mb,
        ));
        let mut scheduler = Scheduler::new(images, config.rotation.order);
        let daemon_state = state::DaemonState::load(&config_dir);
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
        let state_writer = state::StateWriter::new(config_dir, daemon_state);

        // ディレクトリ監視を起動（環境によっては unavailable のため Option）
        let (watch_rx, watcher_handle) = spawn_dir_watcher(&config);

        // 言語設定を解決する（環境変数 → config → デフォルト ja）
        // 初回呼び出しで言語ファイルの探索（同期 I/O）が走るが、この時点では
        // トレイも D-Bus もまだ起動しておらず待たせる相手が居ないため、
        // spawn_blocking へ逃がす意味は無い（直前の画像スキャンや Config::load も
        // 同様に同期のままである）。
        let lang = resolve_lang(&config);
        tracing::info!("ui language: {:?}", lang);

        // デスクトップ通知ハンドル
        let notifier = notify::Notifier::new(lang).await;

        // トレイを非同期に起動（D-Bus が使えない環境では None になる）
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<TrayCmd>();
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

        // D-Bus デーモンインターフェースを登録（CLI からのリモート操作を受け付ける）
        let dbus_conn = spawn_dbus_iface(cmd_tx.clone()).await;

        // セッション管理ウォッチャーを起動（ログアウト検知・Plasma 再起動検知）
        session::spawn_session_watcher(cmd_tx.clone()).await;

        // 画面構成変更の監視（壁紙更新を契機に再検出、60s スロットル）
        let screen_check_tx = screen_watcher::spawn(screens.clone(), cmd_tx.clone());

        // KDE グローバルショートカットを登録・監視する
        shortcuts::spawn_shortcut_watcher(cmd_tx.clone()).await;

        // 設定ファイル監視を起動。失敗時は閉じたチャンネルにフォールバック
        // （`Some(()) = ...` パターンが一致せず select! で無害にスキップされる）。
        let (config_change_rx, config_watcher_handle) = match Config::config_path()
            .ok()
            .and_then(|p| watcher::spawn_config(&p))
        {
            Some((w, rx)) => (rx, Some(w)),
            None => {
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<()>();
                drop(tx);
                (rx, None)
            }
        };

        // Plasma への壁紙適用ハンドル（D-Bus 接続を保持して再利用）
        let plasma = plasma::PlasmaShell::new().await;

        // オンラインプロバイダーのフェッチ用チャンネルと共有クライアント
        let (online_tx, online_rx) =
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
        let mut fetch_ticker =
            interval_at(Instant::now() + FIRST_FETCH_DELAY, Duration::from_secs(1800));
        fetch_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let online_configs = Arc::new(std::sync::Mutex::new(config.online_sources.clone()));

        Ok(Self {
            ticker: make_ticker(config.rotation.interval_secs),
            config,
            blacklist,
            lang,
            screens,
            cache,
            scheduler,
            prefetcher: Prefetcher::new(),
            plasma,
            notifier,
            state_writer,
            tray_handle,
            cmd_tx,
            cmd_rx,
            last_cmd_at: None,
            watch_rx,
            config_change_rx,
            warn_rx,
            screen_check_tx,
            online_tx,
            online_rx,
            online_client,
            online_configs,
            fetch_in_progress: Arc::new(AtomicBool::new(false)),
            fetch_ticker,
            _watcher_handle: watcher_handle,
            _config_watcher_handle: config_watcher_handle,
            _dbus_conn: dbus_conn,
        })
    }

    /// メインループ。トレイ・D-Bus・タイマー・ファイル監視のイベントを捌く。
    async fn run(mut self) -> Result<()> {
        // トレイに初期画像枚数と復元した現在画像名を反映
        if let Some(ref h) = self.tray_handle {
            let count = self.scheduler.image_count();
            let name = tray_display_name(self.scheduler.current().map(|p| p.as_path()));
            h.update(|t| {
                t.image_count = count;
                t.current_name = name;
            })
            .await;
        }

        // 起動時の即時切り替え。
        // 一時停止状態は再起動をまたいで復元されるため、停止中なら切り替えない
        // （停止したまま再起動したのに壁紙が変わる、という挙動を避ける）。
        if self.config.rotation.change_on_start {
            if let Some(path) = self.scheduler.auto_next() {
                self.apply_and_notify(&path, "initial apply failed").await;
            }
        }
        // 起動時適用に時間がかかっても、最初の自動切り替えまでの間隔が縮まないようにする。
        self.reset_ticker();

        tracing::info!("entering main loop (interval={}s)", self.config.rotation.interval_secs);

        loop {
            tokio::select! {
                _ = self.fetch_ticker.tick() => self.spawn_fetch(),

                Some(result) = self.online_rx.recv() => self.on_fetch_result(result).await,

                _ = self.ticker.tick() => {
                    if let Some(path) = self.scheduler.auto_next() {
                        self.apply_and_notify(&path, "auto apply failed").await;
                    }
                }

                Some(cmd) = self.cmd_rx.recv() => {
                    if self.on_command(cmd).await.is_break() {
                        break;
                    }
                }

                Some(ev) = self.watch_rx.recv() => self.on_watch_event(ev).await,

                Some(()) = self.config_change_rx.recv() => self.on_config_file_changed().await,

                msg = self.warn_rx.recv() => {
                    if let Some(msg) = msg {
                        self.notifier.warn(&msg).await;
                    }
                }

                _ = signal::ctrl_c() => {
                    tracing::info!("received Ctrl-C, shutting down");
                    break;
                }
            }
        }

        self.prefetcher.abort();
        if let Some(h) = self.tray_handle {
            h.shutdown().await;
        }
        Ok(())
    }

    // ── コマンド処理 ─────────────────────────────────────────────────────────

    /// トレイ・D-Bus・グローバルショートカット由来のコマンドを処理する。
    /// `Break` を返したらメインループを抜ける。
    async fn on_command(&mut self, cmd: TrayCmd) -> ControlFlow<()> {
        let now = std::time::Instant::now();
        if should_throttle(&cmd, self.last_cmd_at, now) {
            tracing::debug!("command throttled (< 500ms): {:?}", cmd);
            return ControlFlow::Continue(());
        }
        self.last_cmd_at = Some(now);

        match cmd {
            TrayCmd::Next => self.cmd_next().await,
            TrayCmd::Prev => self.cmd_prev().await,
            TrayCmd::TogglePause => self.cmd_toggle_pause().await,
            TrayCmd::SetMode(mode) => self.cmd_set_mode(mode).await,
            TrayCmd::SetInterval(secs) => self.cmd_set_interval(secs).await,
            TrayCmd::OpenCurrent => self.cmd_open_current(),
            TrayCmd::DeleteCurrent => self.cmd_delete_current().await,
            TrayCmd::BlacklistCurrent => self.cmd_blacklist_current().await,
            TrayCmd::CopyToFavorites => self.cmd_copy_to_favorites().await,
            TrayCmd::ReloadConfig => self.cmd_reload_config().await,
            TrayCmd::OpenSettings => open_settings(),
            TrayCmd::PlasmaRestarted => self.cmd_plasma_restarted().await,
            TrayCmd::ScreensChanged(screens) => self.cmd_screens_changed(screens).await,
            TrayCmd::Quit => {
                tracing::info!("quit requested from tray");
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    }

    async fn cmd_next(&mut self) {
        self.prefetcher.abort();
        if let Some(path) = self.scheduler.next() {
            self.apply_and_notify(&path, "tray Next failed").await;
        }
        self.reset_ticker();
    }

    async fn cmd_prev(&mut self) {
        if let Some(path) = self.scheduler.prev() {
            self.apply_and_notify(&path, "tray Prev failed").await;
        }
        self.reset_ticker();
    }

    async fn cmd_toggle_pause(&mut self) {
        if self.scheduler.is_paused() {
            self.scheduler.resume();
            tracing::info!("resumed");
        } else {
            self.scheduler.pause();
            tracing::info!("paused");
        }
        let paused = self.scheduler.is_paused();
        let current = self.scheduler.current().map(|p| p.as_path());
        self.state_writer.persist(paused, current).await;
        if let Some(ref h) = self.tray_handle {
            h.update(|t| t.paused = paused).await;
        }
    }

    async fn cmd_set_mode(&mut self, mode: crate::config::DisplayMode) {
        tracing::info!("display mode → {:?}", mode);
        self.config.display.mode = mode;
        // トレイでの変更を再起動後も保つ。保存で発生する監視イベントは
        // ReloadConfig 側の同値スキップで吸収される。
        persist_config(&self.config, "display mode").await;
        let Some(cur) = self.scheduler.current().cloned() else { return };
        // 壁紙自体は変わらないので、現在名の更新と state への記録は不要
        // （`apply_and_notify` を通さず、エラー表示だけ面倒を見る）。
        match apply(&cur, &self.screens, &self.config, &self.cache, &self.plasma).await {
            Err(e) => {
                tracing::error!(error = %e, "reapply after mode change failed");
                let msg = e.to_string();
                self.notifier.error(&msg, Some(&cur)).await;
                self.tray_error(msg).await;
            }
            Ok(()) => {
                self.notifier.clear();
                self.tray_clear_error().await;
            }
        }
        self.start_prefetch();
    }

    async fn cmd_set_interval(&mut self, secs: u64) {
        let secs = secs.max(crate::config::MIN_INTERVAL_SECS);
        tracing::info!("interval → {}s", secs);
        self.config.rotation.interval_secs = secs;
        persist_config(&self.config, "interval").await;
        self.reset_ticker();
        if let Some(ref h) = self.tray_handle {
            h.update(|t| t.interval_secs = secs).await;
        }
    }

    /// 現在の壁紙ファイルを既定のアプリで開く。
    fn cmd_open_current(&self) {
        if let Some(path) = self.scheduler.current().cloned() {
            tokio::task::spawn_blocking(move || {
                let _ = std::process::Command::new("xdg-open").arg(&path).status();
            });
        }
    }

    async fn cmd_delete_current(&mut self) {
        let Some(path) = self.scheduler.current().cloned() else { return };
        let result = tokio::task::spawn_blocking({
            let path = path.clone();
            move || trash::delete(&path)
        }).await;
        match result {
            Ok(Err(e)) => tracing::error!("failed to trash wallpaper {}: {}", path.display(), e),
            Err(e) => tracing::error!("trash task panicked: {}", e),
            Ok(Ok(())) => {
                tracing::info!("moved to trash: {}", path.display());
                self.scheduler.remove_image(&path);
                self.prefetcher.abort();
                if let Some(next) = self.scheduler.next() {
                    self.apply_and_notify(&next, "apply after trash failed").await;
                }
                self.update_tray_count().await;
                self.reset_ticker();
            }
        }
    }

    async fn cmd_blacklist_current(&mut self) {
        if !self.config.ui.enable_blacklist {
            tracing::debug!("blacklist disabled in config, ignoring");
            return;
        }
        let Some(path) = self.scheduler.current().cloned() else { return };
        if let Err(e) = self.blacklist.add(&path) {
            tracing::error!("blacklist: failed to save {}: {}", path.display(), e);
            return;
        }
        tracing::info!("blacklisted: {}", path.display());
        self.scheduler.remove_image(&path);
        self.prefetcher.abort();
        match self.scheduler.next() {
            Some(next) => {
                self.apply_and_notify(&next, "apply after blacklist failed").await;
                self.update_tray_count().await;
            }
            // 候補が尽きた場合は apply_and_notify を通らないので、
            // トレイに残る壁紙名をここで消す。
            None => {
                if let Some(ref h) = self.tray_handle {
                    let count = self.scheduler.image_count();
                    h.update(|t| {
                        t.current_name = String::new();
                        t.image_count = count;
                    })
                    .await;
                }
            }
        }
        self.reset_ticker();
    }

    async fn cmd_copy_to_favorites(&self) {
        let Some(path) = self.scheduler.current().cloned() else { return };
        let Some(fav_dir) = self.config.sources.favorites_dir.clone() else {
            tracing::warn!("copy_to_favorites: favorites_dir not configured");
            return;
        };
        let Some(filename) = path.file_name() else { return };
        let dest = fav_dir.join(filename);
        if let Err(e) = tokio::fs::create_dir_all(&fav_dir).await {
            tracing::error!("favorites: failed to create dir {}: {}", fav_dir.display(), e);
            return;
        }
        match tokio::fs::copy(&path, &dest).await {
            Ok(_) => tracing::info!("copied to favorites: {}", dest.display()),
            Err(e) => tracing::error!("favorites: failed to copy {}: {}", path.display(), e),
        }
    }

    async fn cmd_reload_config(&mut self) {
        let new_cfg = match Config::load() {
            Err(e) => {
                tracing::error!(error = %e, "config reload failed");
                let msg = e.to_string();
                self.notifier.error(&msg, None).await;
                self.tray_error(msg).await;
                return;
            }
            // 内容が同一なら何もしない。トレイからのモード／間隔変更で
            // デーモン自身が config.toml を保存した場合もここで弾かれ、
            // 不要な再スキャンとスケジューラ再構築を避けられる。
            Ok(new_cfg) if new_cfg == self.config => {
                tracing::debug!("config unchanged, skipping reload");
                return;
            }
            Ok(new_cfg) => new_cfg,
        };

        tracing::info!("reloading config");

        match build_filtered_images_list(
            &collect_source_dirs(&new_cfg),
            new_cfg.sources.recursive,
            &self.blacklist,
        ) {
            Ok(images) if !images.is_empty() => {
                tracing::info!("reload: {} image(s) found", images.len());
                // 一時停止状態と現在画像は rebuild が引き継ぐ
                self.scheduler.rebuild(images, new_cfg.rotation.order);
            }
            Ok(_) => tracing::warn!("reload: no images found, keeping current list"),
            Err(e) => tracing::warn!("reload: scan error: {}", e),
        }

        (self.watch_rx, self._watcher_handle) = spawn_dir_watcher(&new_cfg);

        self.prefetcher.abort();
        self.cache = Arc::new(Cache::new(
            new_cfg.cache.directory.clone(),
            new_cfg.cache.max_size_mb,
        ));

        self.ticker = make_ticker(new_cfg.rotation.interval_secs);

        let new_lang = resolve_lang(&new_cfg);
        if new_lang != self.lang {
            self.lang = new_lang;
            self.notifier = notify::Notifier::new(new_lang).await;
        }

        if new_cfg.ui.warn_notify != self.config.ui.warn_notify {
            WARN_NOTIFY_ENABLED.store(new_cfg.ui.warn_notify, Ordering::Relaxed);
            tracing::info!(
                "warn_notify toggled: {} → {}",
                self.config.ui.warn_notify,
                new_cfg.ui.warn_notify
            );
        }

        *self.online_configs.lock().unwrap_or_else(|e| e.into_inner()) =
            new_cfg.online_sources.clone();
        self.config = new_cfg;

        // rebuild 後の current を使う。新しいソースから外れた画像や
        // ブラックリスト入りした画像は rebuild で current から落ちるため、
        // ここで拾わないことで「除外したはずの画像が再適用される」のを防ぐ。
        match self.scheduler.current().cloned() {
            // 再適用が成功した場合だけ apply_and_notify 内で
            // state に記録される。ここで先に persist すると、
            // 適用に失敗した壁紙を「現在の壁紙」として
            // 保存してしまい、再起動後にトレイやゴミ箱操作が
            // 画面に出ていない画像を指す（分岐を畳まないこと）。
            Some(cur) => {
                self.apply_and_notify(&cur, "reload: reapply failed").await;
            }
            // current が落ちた場合は apply_and_notify を通らないので、
            // state に残る旧画像を明示的に消す。
            None => {
                self.state_writer.persist(self.scheduler.is_paused(), None).await;
            }
        }

        if let Some(ref h) = self.tray_handle {
            let mode = self.config.display.mode;
            let secs = self.config.rotation.interval_secs;
            let strings = i18n::strings(self.lang);
            let count = self.scheduler.image_count();
            let has_fav = self.config.sources.favorites_dir.is_some();
            let bl_enabled = self.config.ui.enable_blacklist;
            let name = tray_display_name(self.scheduler.current().map(|p| p.as_path()));
            h.update(|t| {
                t.mode = mode;
                t.interval_secs = secs;
                t.strings = strings;
                t.image_count = count;
                t.has_favorites_dir = has_fav;
                t.blacklist_enabled = bl_enabled;
                t.current_name = name;
            })
            .await;
        }

        tracing::info!("config reload complete");
    }

    async fn cmd_plasma_restarted(&mut self) {
        tracing::info!("Plasma restarted, re-applying wallpaper");
        if let Some(path) = self.scheduler.current().cloned() {
            self.apply_and_notify(&path, "reapply after Plasma restart failed").await;
        }
    }

    async fn cmd_screens_changed(&mut self, new_screens: Vec<screen::Monitor>) {
        let (w, h) = primary_size(&new_screens);
        tracing::info!(
            "screens updated: {} monitor(s), primary {}x{}",
            new_screens.len(), w, h
        );
        self.screens = new_screens;
        // 解像度が変わるとキャッシュキーも変わるため、現在の壁紙を
        // 新しい解像度で再加工して適用する。
        if let Some(path) = self.scheduler.current().cloned() {
            self.apply_and_notify(&path, "reapply after screen change failed").await;
        }
    }

    // ── その他のイベント ─────────────────────────────────────────────────────

    /// オンラインプロバイダーの取得結果をローテーションに取り込む。
    async fn on_fetch_result(&mut self, result: provider::FetchResult) {
        let provider::FetchResult { provider, new_paths } = result;
        if new_paths.is_empty() {
            return;
        }
        let was_empty = self.scheduler.current().is_none() && self.scheduler.peek_next().is_none();
        let new_paths: Vec<_> = new_paths
            .into_iter()
            .filter(|p| !self.blacklist.contains(p))
            .collect();
        let added = new_paths.len();
        for path in new_paths {
            self.scheduler.add_image(path);
        }
        tracing::info!("provider {}: {} new image(s) added to rotation", provider, added);
        self.update_tray_count().await;
        if added > 0 && self.config.ui.notify_fetch {
            let strings = i18n::strings(self.lang);
            let body = strings.notify_fetch_body
                .replace("{provider}", &provider)
                .replace("{count}", &added.to_string());
            self.notifier.info(strings.notify_fetch_title, &body).await;
        }
        // 取り込む前が空だった場合は自動切り替えを待たずに最初の 1 枚を出す。
        if was_empty {
            if let Some(path) = self.scheduler.auto_next() {
                self.apply_and_notify(&path, "online: initial apply failed").await;
            }
        }
    }

    async fn on_watch_event(&mut self, ev: watcher::WatchEvent) {
        match ev {
            watcher::WatchEvent::Added(path) => {
                if self.blacklist.contains(&path) {
                    tracing::debug!("ignoring blacklisted image: {}", path.display());
                } else {
                    tracing::info!("new image detected: {}", path.display());
                    self.scheduler.add_image(path);
                }
            }
            watcher::WatchEvent::Removed(path) => {
                tracing::info!("image removed: {}", path.display());
                self.scheduler.remove_image(&path);
            }
        }
        self.update_tray_count().await;
    }

    /// `config.toml` の変更を検知したら、連続イベントを集約してから `ReloadConfig` を送信。
    /// 保留中のイベントをドレインし、100ms 待ってからもう一度ドレインすることで、
    /// エディタの保存が生むバーストを 1 回のリロードにまとめる。
    async fn on_config_file_changed(&mut self) {
        while self.config_change_rx.try_recv().is_ok() {}
        tracing::info!("config file changed; waiting 100ms to coalesce events");
        tokio::time::sleep(Duration::from_millis(100)).await;
        // Drain again in case more events arrived during sleep
        while self.config_change_rx.try_recv().is_ok() {}
        tracing::info!("queueing single ReloadConfig after debounce");
        let _ = self.cmd_tx.send(TrayCmd::ReloadConfig);
    }

    // ── 壁紙の適用と周辺の更新 ───────────────────────────────────────────────

    /// apply + 通知 + トレイ更新 + 先読み開始 + 画面構成再検出トリガーをまとめて行う。
    async fn apply_and_notify(&mut self, path: &Path, log_ctx: &str) {
        if let Err(e) = apply(path, &self.screens, &self.config, &self.cache, &self.plasma).await {
            tracing::error!(error = %e, "{}", log_ctx);
            let msg = e.to_string();
            self.notifier.error(&msg, Some(path)).await;
            self.tray_error(msg).await;
            return;
        }
        self.notifier.clear();
        if let Some(ref h) = self.tray_handle {
            let name = tray_display_name(Some(path));
            h.update(|t| {
                t.last_error = None;
                t.current_name = name;
            })
            .await;
        }
        // 現在の壁紙を記録しておき、再起動後も同じ画像を「現在」として扱えるようにする
        self.state_writer.persist(self.scheduler.is_paused(), Some(path)).await;
        self.start_prefetch();
        // 画面構成の再検出を要求（ウォッチャー側で 60s スロットル）
        if let Some(ref tx) = self.screen_check_tx {
            let _ = tx.send(());
        }
    }

    /// 次の壁紙をバックグラウンドで加工してキャッシュに載せる。
    fn start_prefetch(&mut self) {
        if !self.config.rotation.prefetch {
            return;
        }
        let Some(next) = self.scheduler.peek_next() else { return };
        let (screen_w, screen_h) = primary_size(&self.screens);
        let key = CacheKey {
            src: next.clone(),
            screen_w,
            screen_h,
            mode: self.config.display.mode,
            blur_sigma: self.config.display.blur_sigma,
            bg_darken: self.config.display.bg_darken,
        };
        self.prefetcher.start(key, self.cache.clone());
    }

    /// 期限の来たオンラインプロバイダーの取得をバックグラウンドで起動する。
    /// 走行中のフェッチがあれば何もしない。
    fn spawn_fetch(&self) {
        let Some(client) = self.online_client.as_ref() else { return };
        let configs = self
            .online_configs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if configs.is_empty() {
            return;
        }
        // 確認＋セットを単一の atomic 操作で行う。`true` が返れば既に走行中。
        if self.fetch_in_progress.swap(true, Ordering::AcqRel) {
            return;
        }
        let client = client.clone();
        let in_progress = self.fetch_in_progress.clone();
        let tx = self.online_tx.clone();
        let (screen_w, screen_h) = primary_size(&self.screens);
        let ctx = provider::FetchContext { screen_w, screen_h };
        tokio::spawn(async move {
            // パニックしてもスタック巻き戻し中に Drop が走り、フラグが false に戻る。
            // これによりタスクが死んでも以降のフェッチが永久にブロックされなくなる。
            let _guard = FlagGuard(in_progress);
            // force = false: 前回取得から interval_hours 経過したプロバイダーだけ叩く。
            for r in provider::fetch_all_due(&configs, &client, ctx, false).await {
                let _ = tx.send(r);
            }
        });
    }

    /// 自動切り替えタイマーを現在の間隔で張り直す。
    /// 手動切り替えの直後に自動切り替えが続けて走るのを防ぐ。
    fn reset_ticker(&mut self) {
        self.ticker = make_ticker(self.config.rotation.interval_secs);
    }

    /// トレイの画像枚数表示を現在値に合わせる。
    async fn update_tray_count(&self) {
        if let Some(ref h) = self.tray_handle {
            let count = self.scheduler.image_count();
            h.update(|t| t.image_count = count).await;
        }
    }

    async fn tray_error(&self, msg: String) {
        if let Some(ref h) = self.tray_handle {
            h.update(|t| t.last_error = Some(msg)).await;
        }
    }

    async fn tray_clear_error(&self) {
        if let Some(ref h) = self.tray_handle {
            h.update(|t| t.last_error = None).await;
        }
    }
}

/// 設定 GUI を別プロセスで起動する。
fn open_settings() {
    match std::process::Command::new("kabekami-config").spawn() {
        Ok(_) => tracing::info!("launched kabekami-config"),
        Err(e) => tracing::warn!("failed to launch kabekami-config: {}", e),
    }
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
/// ソースのダウンロード先を足したもの。
fn collect_source_dirs(config: &Config) -> Vec<std::path::PathBuf> {
    let mut dirs = config.sources.directories.clone();
    for oc in &config.online_sources {
        if oc.enabled {
            dirs.push(oc.resolved_download_dir());
        }
    }
    dirs
}

/// ディレクトリ監視を起動する。監視が使えない環境では閉じたチャンネルを返し、
/// `Some(ev) = watch_rx.recv()` が一致しなくなることで select! が無害にスキップする。
fn spawn_dir_watcher(
    config: &Config,
) -> (
    tokio::sync::mpsc::Receiver<watcher::WatchEvent>,
    Option<watcher::DirWatcher>,
) {
    match watcher::spawn(&collect_source_dirs(config), config.sources.recursive) {
        Some((w, rx)) => (rx, Some(w)),
        None => {
            let (tx, rx) = tokio::sync::mpsc::channel::<watcher::WatchEvent>(1);
            drop(tx);
            (rx, None)
        }
    }
}

/// プライマリモニターの解像度。検出できていなければフォールバック値を返す。
fn primary_size(screens: &[screen::Monitor]) -> (u32, u32) {
    screens
        .first()
        .map(|m| (m.width, m.height))
        .unwrap_or((FALLBACK_SCREEN_W, FALLBACK_SCREEN_H))
}

/// 連続したコマンドの 2 発目以降を捨てるか判定する。
///
/// 500ms という長さの理由: KRunner で `kabekami --next` を実行すると
/// CLI バイナリの起動 + D-Bus 接続 (50-200ms) を 2 回経由して daemon に
/// 届くケースがあり、短いスロットル (100ms 等) だと二重実行を吸収しきれない。
///
/// システムイベント系（Quit / PlasmaRestarted / ReloadConfig / ScreensChanged）は
/// ユーザー操作ではなく取りこぼすと状態がずれるため、スロットリングから除外する。
fn should_throttle(cmd: &TrayCmd, last_cmd_at: Option<std::time::Instant>, now: std::time::Instant) -> bool {
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
    let cache_owned = Arc::clone(cache);
    let key_owned = key;
    tokio::task::spawn_blocking(move || prefetch::process_for_cache(&key_owned, &cache_owned))
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
        let (w, h) = primary_size(screens);
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

/// トレイに表示する壁紙名（拡張子付きファイル名）。
/// 取得できない場合は空文字列を返す。
fn tray_display_name(path: Option<&Path>) -> String {
    path.and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}
/// 設定を保存し、失敗しても警告に留めて処理を続行する。
/// `what` は失敗ログに出す変更内容（例: `"display mode"`）。
async fn persist_config(config: &Config, what: &str) {
    let owned = config.clone();
    state::save_offloaded(move || owned.save(), what).await;
}

/// `Arc<AtomicBool>` を `Drop` で `false` に戻す RAII ガード。
/// 非同期タスクのパニックでも確実にフラグが解放されるようにするために使う。
struct FlagGuard(Arc<AtomicBool>);
impl Drop for FlagGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DisplayMode;

    fn monitor(name: &str, width: u32, height: u32) -> screen::Monitor {
        screen::Monitor { name: name.into(), width, height }
    }

    #[test]
    fn primary_size_uses_first_monitor() {
        let screens = vec![monitor("DP-1", 2560, 1440), monitor("HDMI-1", 1920, 1080)];
        assert_eq!(primary_size(&screens), (2560, 1440));
    }

    #[test]
    fn primary_size_falls_back_without_monitors() {
        assert_eq!(primary_size(&[]), (FALLBACK_SCREEN_W, FALLBACK_SCREEN_H));
    }

    #[test]
    fn collect_source_dirs_includes_only_enabled_online_sources() {
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

        let dirs = collect_source_dirs(&config);
        assert_eq!(
            dirs,
            vec![
                std::path::PathBuf::from("/pics/a"),
                std::path::PathBuf::from("/pics/b"),
                std::path::PathBuf::from("/dl/bing"),
            ],
            "無効なオンラインソースのダウンロード先はスキャン対象に入れない"
        );
    }

    #[test]
    fn collect_source_dirs_without_online_sources() {
        let mut config = Config::default();
        config.sources.directories = vec![std::path::PathBuf::from("/pics")];
        assert_eq!(collect_source_dirs(&config), vec![std::path::PathBuf::from("/pics")]);
    }

    #[test]
    fn tray_display_name_is_the_file_name() {
        assert_eq!(tray_display_name(Some(Path::new("/pics/sunset.jpg"))), "sunset.jpg");
        assert_eq!(tray_display_name(None), "");
    }

    #[test]
    fn first_command_is_never_throttled() {
        let now = std::time::Instant::now();
        assert!(!should_throttle(&TrayCmd::Next, None, now));
    }

    #[test]
    fn repeated_user_command_within_500ms_is_throttled() {
        let first = std::time::Instant::now();
        // KRunner 経由の二重配信を想定した間隔
        let second = first + Duration::from_millis(120);
        assert!(should_throttle(&TrayCmd::Next, Some(first), second));
    }

    #[test]
    fn user_command_after_500ms_passes() {
        let first = std::time::Instant::now();
        let second = first + Duration::from_millis(500);
        assert!(!should_throttle(&TrayCmd::Next, Some(first), second));
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
            TrayCmd::ScreensChanged(vec![monitor("DP-1", 1920, 1080)]),
        ] {
            assert!(
                !should_throttle(&cmd, Some(first), immediately_after),
                "{cmd:?} はスロットリングの対象外であるべき"
            );
        }
    }

    #[test]
    fn throttled_commands_cover_every_user_facing_variant() {
        let first = std::time::Instant::now();
        let immediately_after = first + Duration::from_millis(1);
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
            assert!(
                should_throttle(&cmd, Some(first), immediately_after),
                "{cmd:?} は連打を吸収すべき"
            );
        }
    }
}
