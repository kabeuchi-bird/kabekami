//! 次の壁紙の先読み（バックグラウンド加工）。設計書 §5a に準拠。
//!
//! 壁紙切り替えの直後に「次の画像」の加工をバックグラウンドで開始しておくことで、
//! 次の切り替え時にはキャッシュがヒットし即座に反映できる。
//!
//! ## タイムライン（設計書 §5a より）
//! ```text
//! 時刻  0s: 画像 A を壁紙に設定
//!           └─ 画像 B の加工を非同期開始（tokio::spawn）
//! 時刻 ~1.5s: 画像 B の加工完了 → キャッシュに保存
//! 時刻 10s: 画像 B をキャッシュから即座に反映
//!           └─ 画像 C の加工を非同期開始
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use tokio::task::JoinHandle;

use crate::cache::{Cache, CacheKey};
use crate::config::DisplayMode;

/// 先読みタスクの管理。
///
/// - `start()` で新しい先読みを開始する。前の先読みが走っていれば abort する。
/// - `abort()` で明示的にキャンセルできる（「次へ」連打時など）。
pub struct Prefetcher {
    pending: Option<JoinHandle<()>>,
}

impl Prefetcher {
    pub fn new() -> Self {
        Self { pending: None }
    }

    /// 指定した画像の先読み加工をバックグラウンドで開始する。
    ///
    /// すでに先読み中のタスクがある場合は abort してから新しいタスクを起動する。
    /// キャッシュにすでにある場合はタスクを起動せずに即座に返る。
    pub fn start(
        &mut self,
        image: PathBuf,
        screen_w: u32,
        screen_h: u32,
        mode: DisplayMode,
        blur_sigma: f32,
        bg_darken: f32,
        cache: Arc<Cache>,
    ) {
        self.abort();

        let key = CacheKey {
            src: image.clone(),
            screen_w,
            screen_h,
            mode,
            blur_sigma,
            bg_darken,
        };

        // キャッシュにすでにある場合はタスク不要
        if cache.get(&key).is_some() {
            tracing::debug!("prefetch: cache hit, skipping {}", image.display());
            return;
        }

        tracing::debug!("prefetch: starting for {}", image.display());
        self.pending = Some(tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                process_for_cache(&image, screen_w, screen_h, mode, blur_sigma, bg_darken, &cache)
            })
            .await;

            match result {
                Ok(Ok(path)) => tracing::debug!("prefetch: done → {}", path.display()),
                Ok(Err(e)) => tracing::warn!("prefetch: processing error: {}", e),
                Err(e) if e.is_cancelled() => tracing::debug!("prefetch: cancelled"),
                Err(e) => tracing::warn!("prefetch: task panicked: {}", e),
            }
        }));
    }

    /// 先読み中のタスクをキャンセルする。
    pub fn abort(&mut self) {
        if let Some(handle) = self.pending.take() {
            handle.abort();
        }
    }
}

impl Default for Prefetcher {
    fn default() -> Self {
        Self::new()
    }
}

/// 画像を読み込み、加工してキャッシュに保存する（ブロッキング処理）。
///
/// この関数は `spawn_blocking` から呼ばれることを想定している。
/// キャッシュにすでにある場合は二重書き込みを避けるためスキップする。
pub fn process_for_cache(
    src: &std::path::Path,
    screen_w: u32,
    screen_h: u32,
    mode: DisplayMode,
    blur_sigma: f32,
    bg_darken: f32,
    cache: &Cache,
) -> anyhow::Result<PathBuf> {
    let key = CacheKey {
        src: src.to_path_buf(),
        screen_w,
        screen_h,
        mode,
        blur_sigma,
        bg_darken,
    };

    // 二重チェック（並列 prefetch が先に書いた可能性）
    if let Some(cached) = cache.get(&key) {
        return Ok(cached);
    }

    tracing::info!("prefetch: processing {}", src.display());
    let img = image::open(src)
        .map_err(|e| anyhow::anyhow!("failed to open {}: {}", src.display(), e))?;

    let processed = match mode {
        DisplayMode::BlurPad => {
            crate::blur_pad::generate_blur_pad(&img, screen_w, screen_h, blur_sigma, bg_darken)
        }
        other => {
            tracing::warn!("display mode {:?} not implemented, using BlurPad", other);
            crate::blur_pad::generate_blur_pad(&img, screen_w, screen_h, blur_sigma, bg_darken)
        }
    };

    cache.store(&key, &processed)
}
