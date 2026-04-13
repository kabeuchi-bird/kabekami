//! kabekami — KDE Plasma 向け壁紙ローテーションツール
//!
//! ## Phase 2 追加機能（設計書 §13 Phase 2）
//! - キャッシュ（`cache.rs`）: 加工済み画像を SHA256 キーで保存。再起動後もヒット。
//! - 先読み（`prefetch.rs`）: 壁紙設定直後に次の画像をバックグラウンド加工開始。
//! - トレイ（`tray.rs`）: `ksni` + SNI プロトコルでシステムトレイに常駐。
//! - スケジューラ（`scheduler.rs`）: 前へ戻る・一時停止・再開・間隔変更。

mod blur_pad;
mod cache;
mod config;
mod plasma;
mod prefetch;
mod scanner;
mod scheduler;
mod tray;

use std::path::Path;
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

/// Phase 1 と同様のデフォルト解像度。Phase 3 で `kscreen-doctor` 連携に置き換え。
const DEFAULT_SCREEN_W: u32 = 1920;
const DEFAULT_SCREEN_H: u32 = 1080;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    init_tracing();

    let mut config = Config::load().context("failed to load config")?;
    tracing::info!(?config, "loaded config");

    let images = crate::scanner::scan(&config.sources.directories, config.sources.recursive)
        .context("failed to scan source directories")?;
    if images.is_empty() {
        anyhow::bail!(
            "no images found. Configure [sources] directories in {}",
            Config::config_path()?.display()
        );
    }
    tracing::info!("discovered {} image(s)", images.len());

    let (screen_w, screen_h) = resolve_screen_size();
    tracing::info!("using screen size {}x{}", screen_w, screen_h);

    // キャッシュ・スケジューラ・先読みを初期化
    let cache = Arc::new(Cache::new(
        config.cache.directory.clone(),
        config.cache.max_size_mb,
    ));
    let mut scheduler = Scheduler::new(images, config.rotation.order);
    let mut prefetcher = Prefetcher::new();

    // トレイを非同期に起動（D-Bus が使えない環境では None になる）
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<TrayCmd>();
    let tray_handle = tray::spawn_tray(
        cmd_tx,
        config.display.mode,
        config.rotation.interval_secs,
    )
    .await;

    // 起動時の即時切り替え
    if config.rotation.change_on_start {
        if let Some(path) = scheduler.next() {
            if let Err(e) = apply(&path, screen_w, screen_h, &config, &cache).await {
                tracing::error!(error = %e, "initial wallpaper apply failed");
            } else {
                update_tray_current(&tray_handle, &path).await;
                start_prefetch(&mut prefetcher, &scheduler, screen_w, screen_h, &config, &cache);
            }
        }
    }

    // タイマー: 最初の tick は即座ではなく 1 interval 後から。
    let mut ticker = make_ticker(config.rotation.interval_secs);

    tracing::info!("entering main loop (interval={}s)", config.rotation.interval_secs);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if scheduler.is_paused() {
                    continue;
                }
                if let Some(path) = scheduler.next() {
                    if let Err(e) = apply(&path, screen_w, screen_h, &config, &cache).await {
                        tracing::error!(error = %e, "auto apply failed");
                    } else {
                        update_tray_current(&tray_handle, &path).await;
                        start_prefetch(&mut prefetcher, &scheduler, screen_w, screen_h, &config, &cache);
                    }
                }
            }

            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    TrayCmd::Next => {
                        prefetcher.abort();
                        if let Some(path) = scheduler.next() {
                            if let Err(e) = apply(&path, screen_w, screen_h, &config, &cache).await {
                                tracing::error!(error = %e, "tray Next failed");
                            } else {
                                update_tray_current(&tray_handle, &path).await;
                                start_prefetch(&mut prefetcher, &scheduler, screen_w, screen_h, &config, &cache);
                            }
                        }
                        // 「次へ」操作でタイマーをリセット（次の自動切り替えは 1 interval 後）
                        ticker = make_ticker(config.rotation.interval_secs);
                    }

                    TrayCmd::Prev => {
                        if let Some(path) = scheduler.prev() {
                            if let Err(e) = apply(&path, screen_w, screen_h, &config, &cache).await {
                                tracing::error!(error = %e, "tray Prev failed");
                            } else {
                                update_tray_current(&tray_handle, &path).await;
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
                        // 設定を変えたら現在の壁紙をすぐ再処理（キャッシュミス → 再加工）
                        if let Some(cur) = scheduler.current().cloned() {
                            if let Err(e) = apply(&cur, screen_w, screen_h, &config, &cache).await {
                                tracing::error!(error = %e, "reapply after mode change failed");
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

                    TrayCmd::Quit => {
                        tracing::info!("quit requested from tray");
                        break;
                    }
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

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("kabekami=info,warn"));
    fmt().with_env_filter(filter).init();
}

/// `KABEKAMI_SCREEN=WxH` 環境変数、なければデフォルト解像度を使う。
fn resolve_screen_size() -> (u32, u32) {
    if let Ok(val) = std::env::var("KABEKAMI_SCREEN") {
        if let Some((w, h)) = val.split_once('x') {
            if let (Ok(w), Ok(h)) = (w.trim().parse::<u32>(), h.trim().parse::<u32>()) {
                if w > 0 && h > 0 {
                    return (w, h);
                }
            }
        }
        tracing::warn!("invalid KABEKAMI_SCREEN='{}', expected WxH (e.g. 2560x1440)", val);
    }
    (DEFAULT_SCREEN_W, DEFAULT_SCREEN_H)
}

/// 「1 interval 後に最初の tick」が来る Interval を生成する。
///
/// `interval()` と異なり初期 tick が即座に来ない。
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
async fn apply(src: &Path, screen_w: u32, screen_h: u32, config: &Config, cache: &Cache) -> Result<()> {
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
        let cache_owned = Arc::new(Cache::new(cache.directory.clone(), 0)); // no evict here
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

    // 加工後にキャッシュ全体の退避チェック（メインキャッシュで実施）
    if let Err(e) = cache.evict_if_needed() {
        tracing::warn!("cache eviction error: {}", e);
    }

    plasma::set_wallpaper(&output)
}

/// トレイアイコンの `current_name` を更新する。
async fn update_tray_current(
    tray_handle: &Option<ksni::Handle<tray::KabekamiTray>>,
    path: &Path,
) {
    if let Some(ref h) = tray_handle {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        h.update(|t| t.current_name = name).await;
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
