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

mod blur_pad;
mod cache;
mod config;
mod display_mode;
mod plasma;
mod prefetch;
mod scanner;
mod scheduler;
mod screen;
mod tray;
mod watcher;

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

/// `KABEKAMI_SCREEN` 環境変数が未設定かつ `kscreen-doctor` も使えない場合の
/// フォールバック解像度。Phase 3 で kscreen-doctor 連携が入るため実際には滅多に使われない。
const FALLBACK_SCREEN_W: u32 = 1920;
const FALLBACK_SCREEN_H: u32 = 1080;

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

    // ディレクトリ監視を起動（環境によっては unavailable のため Option）
    let (mut watch_rx, _watcher) =
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

    // タイマー: 最初の tick は 1 interval 後から。
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
async fn apply(src: &Path, screen_w: u32, screen_h: u32, config: &Config, cache: &Arc<Cache>) -> Result<()> {
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

    plasma::set_wallpaper(&output).await
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
