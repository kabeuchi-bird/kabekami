//! kabekami — KDE Plasma 向け壁紙ローテーションツール（Phase 1 MVP）。
//!
//! 設計書 §11 "処理フロー" の Phase 1 部分を実装する:
//! 1. 設定ファイル読み込み → ディレクトリ走査
//! 2. `change_on_start` なら即座に最初の壁紙を反映
//! 3. `interval_secs` ごとにタイマーで次の画像を選び、BlurPad 加工して
//!    `plasma-apply-wallpaperimage` で反映
//!
//! Phase 1 スコープ外（後続フェーズで実装）:
//! - キャッシュ管理 (`cache.rs`)
//! - 先読み (`prefetch.rs`)
//! - トレイアイコン (`tray.rs`)
//! - D-Bus evaluateScript
//! - 画面解像度の自動取得（`CLI --screen-size` か環境変数で上書き可能）

mod blur_pad;
mod config;
mod plasma;
mod scanner;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use rand::Rng;
use tokio::signal;
use tokio::time::MissedTickBehavior;

use crate::config::{Config, DisplayMode, Order};

/// Phase 1 の仮の画面解像度。Phase 3 で `kscreen-doctor` から自動取得する。
/// それまでの間は環境変数 `KABEKAMI_SCREEN` (例: `1920x1080`) で上書き可能。
const DEFAULT_SCREEN_W: u32 = 1920;
const DEFAULT_SCREEN_H: u32 = 1080;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    init_tracing();

    let config = Config::load().context("failed to load config")?;
    tracing::info!(?config, "loaded config");

    let images = scanner::scan(&config.sources.directories, config.sources.recursive)
        .context("failed to scan source directories")?;
    if images.is_empty() {
        anyhow::bail!(
            "no images found. Configure `[sources] directories` in {}",
            Config::config_path()?.display()
        );
    }
    tracing::info!("discovered {} image(s)", images.len());

    let (screen_w, screen_h) = resolve_screen_size();
    tracing::info!("using screen size {}x{}", screen_w, screen_h);

    // 出力ディレクトリ（Phase 1 では単に加工済み画像の置き場として使う）。
    std::fs::create_dir_all(&config.cache.directory).with_context(|| {
        format!(
            "failed to create cache dir: {}",
            config.cache.directory.display()
        )
    })?;

    let mut queue = Queue::new(images, config.rotation.order);

    if config.rotation.change_on_start {
        if let Some(path) = queue.next() {
            if let Err(err) = apply(&path, screen_w, screen_h, &config).await {
                tracing::error!(error = %err, "failed to apply initial wallpaper");
            }
        }
    }

    let interval = Duration::from_secs(config.rotation.interval_secs);
    tracing::info!("rotation interval: {:?}", interval);

    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    // 最初の tick は即座に発火するので一度捨てる。
    ticker.tick().await;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Some(path) = queue.next() {
                    if let Err(err) = apply(&path, screen_w, screen_h, &config).await {
                        tracing::error!(error = %err, "apply failed, continuing");
                    }
                }
            }
            _ = signal::ctrl_c() => {
                tracing::info!("received Ctrl-C, shutting down");
                break;
            }
        }
    }

    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("kabekami=info,warn"));
    fmt().with_env_filter(filter).init();
}

/// `KABEKAMI_SCREEN=WxH` があれば使う。無ければ Phase 1 デフォルトにフォールバック。
fn resolve_screen_size() -> (u32, u32) {
    if let Ok(val) = std::env::var("KABEKAMI_SCREEN") {
        if let Some((w, h)) = val.split_once('x') {
            if let (Ok(w), Ok(h)) = (w.trim().parse::<u32>(), h.trim().parse::<u32>()) {
                if w > 0 && h > 0 {
                    return (w, h);
                }
            }
        }
        tracing::warn!("invalid KABEKAMI_SCREEN='{}', expected WxH", val);
    }
    (DEFAULT_SCREEN_W, DEFAULT_SCREEN_H)
}

/// 画像を DisplayMode に応じて加工し、Plasma に反映する。
///
/// 画像処理はブロッキング（CPU bound）なので `spawn_blocking` で別スレッドへ。
async fn apply(src: &Path, screen_w: u32, screen_h: u32, config: &Config) -> Result<()> {
    let src = src.to_path_buf();
    let mode = config.display.mode;
    let blur_sigma = config.display.blur_sigma;
    let bg_darken = config.display.bg_darken;
    let cache_dir = config.cache.directory.clone();

    let output = tokio::task::spawn_blocking(move || {
        process_image(&src, screen_w, screen_h, mode, blur_sigma, bg_darken, &cache_dir)
    })
    .await
    .context("image processing task panicked")??;

    plasma::set_wallpaper(&output)
}

/// 画像を読み込み、DisplayMode に従って加工して出力ファイルに書き出す。
///
/// Phase 1 では BlurPad のみ正式に実装し、Fill/Fit/Stretch/Smart は BlurPad に
/// フォールバックする。Phase 3 で `display_mode.rs` を追加してそれぞれ実装する。
fn process_image(
    src: &Path,
    screen_w: u32,
    screen_h: u32,
    mode: DisplayMode,
    blur_sigma: f32,
    bg_darken: f32,
    cache_dir: &Path,
) -> Result<PathBuf> {
    tracing::info!("processing {}", src.display());
    let img = image::open(src).with_context(|| format!("failed to open image: {}", src.display()))?;

    let processed = match mode {
        DisplayMode::BlurPad => {
            blur_pad::generate_blur_pad(&img, screen_w, screen_h, blur_sigma, bg_darken)
        }
        // Phase 1: 他モードは BlurPad にフォールバック。Phase 3 で実装予定。
        other => {
            tracing::warn!(
                "display mode {:?} not yet implemented, falling back to BlurPad",
                other
            );
            blur_pad::generate_blur_pad(&img, screen_w, screen_h, blur_sigma, bg_darken)
        }
    };

    std::fs::create_dir_all(cache_dir)?;
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("wallpaper");
    let out_path = cache_dir.join(format!("{}_{}x{}.jpg", stem, screen_w, screen_h));

    // アルファは捨てて JPEG で書き出す（quality は image crate のデフォルト=75 相当だが
    // Phase 1 では十分。Phase 2 のキャッシュ導入時に 92 品質で書き直す）。
    image::DynamicImage::ImageRgba8(processed)
        .into_rgb8()
        .save_with_format(&out_path, image::ImageFormat::Jpeg)
        .with_context(|| format!("failed to save processed image: {}", out_path.display()))?;

    Ok(out_path)
}

/// 画像リストからの「次の画像」取得。順次モードはインデックス、ランダムモードは
/// Fisher-Yates でシャッフルしたキューから消費する（同じ画像の連続を防ぐ）。
struct Queue {
    images: Vec<PathBuf>,
    order: Order,
    /// Sequential 用の現在位置
    seq_pos: usize,
    /// Random 用のシャッフル済みキュー（末尾に達したら再シャッフル）
    shuffled: Vec<PathBuf>,
    shuf_pos: usize,
}

impl Queue {
    fn new(images: Vec<PathBuf>, order: Order) -> Self {
        let mut q = Self {
            shuffled: images.clone(),
            images,
            order,
            seq_pos: 0,
            shuf_pos: 0,
        };
        if order == Order::Random {
            q.reshuffle();
        }
        q
    }

    fn reshuffle(&mut self) {
        self.shuffled = self.images.clone();
        fisher_yates(&mut self.shuffled);
        self.shuf_pos = 0;
    }

    fn next(&mut self) -> Option<PathBuf> {
        if self.images.is_empty() {
            return None;
        }
        match self.order {
            Order::Sequential => {
                let p = self.images[self.seq_pos].clone();
                self.seq_pos = (self.seq_pos + 1) % self.images.len();
                Some(p)
            }
            Order::Random => {
                if self.shuf_pos >= self.shuffled.len() {
                    self.reshuffle();
                }
                let p = self.shuffled[self.shuf_pos].clone();
                self.shuf_pos += 1;
                Some(p)
            }
        }
    }
}

fn fisher_yates<T>(slice: &mut [T]) {
    let mut rng = rand::rng();
    for i in (1..slice.len()).rev() {
        let j = rng.random_range(0..=i);
        slice.swap(i, j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(n: usize) -> Vec<PathBuf> {
        (0..n).map(|i| PathBuf::from(format!("/tmp/{}.jpg", i))).collect()
    }

    #[test]
    fn sequential_cycles_through_all_and_wraps() {
        let mut q = Queue::new(paths(3), Order::Sequential);
        assert_eq!(q.next().unwrap(), PathBuf::from("/tmp/0.jpg"));
        assert_eq!(q.next().unwrap(), PathBuf::from("/tmp/1.jpg"));
        assert_eq!(q.next().unwrap(), PathBuf::from("/tmp/2.jpg"));
        assert_eq!(q.next().unwrap(), PathBuf::from("/tmp/0.jpg"));
    }

    #[test]
    fn random_visits_each_image_exactly_once_per_cycle() {
        let mut q = Queue::new(paths(5), Order::Random);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..5 {
            seen.insert(q.next().unwrap());
        }
        assert_eq!(seen.len(), 5, "random cycle must visit each image once");
    }

    #[test]
    fn empty_queue_returns_none() {
        let mut q = Queue::new(Vec::new(), Order::Random);
        assert!(q.next().is_none());
    }
}
