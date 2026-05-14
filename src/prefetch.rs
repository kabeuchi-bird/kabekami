//! 次の壁紙の先読み（バックグラウンド加工）。
//!
//! 壁紙切り替えの直後に「次の画像」の加工をバックグラウンドで開始しておくことで、
//! 次の切り替え時にはキャッシュがヒットし即座に反映できる。
//!
//! ```text
//! 時刻  0s: 画像 A を壁紙に設定
//!           └─ 画像 B の加工を非同期開始（tokio::spawn）
//! 時刻 ~1.5s: 画像 B の加工完了 → キャッシュに保存
//! 時刻 10s: 画像 B をキャッシュから即座に反映
//!           └─ 画像 C の加工を非同期開始
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use image::ImageDecoder;
use tokio::task::JoinHandle;

use crate::cache::{Cache, CacheKey};

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

    /// 指定したキャッシュキーに対応する画像の先読み加工をバックグラウンドで開始する。
    ///
    /// すでに先読み中のタスクがある場合は abort してから新しいタスクを起動する。
    /// キャッシュにすでにある場合はタスクを起動せずに即座に返る。
    pub fn start(&mut self, key: CacheKey, cache: Arc<Cache>) {
        self.abort();

        // キャッシュにすでにある場合はタスク不要
        if cache.get(&key).is_some() {
            tracing::debug!("prefetch: cache hit, skipping {}", key.src.display());
            return;
        }

        tracing::debug!("prefetch: starting for {}", key.src.display());
        self.pending = Some(tokio::spawn(async move {
            let result =
                tokio::task::spawn_blocking(move || process_for_cache(&key, &cache)).await;

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

/// `CacheKey` で指定された画像を読み込み・加工してキャッシュに保存する（ブロッキング処理）。
///
/// この関数は `spawn_blocking` から呼ばれることを想定している。
/// キャッシュにすでにある場合は二重書き込みを避けるためスキップする。
pub fn process_for_cache(key: &CacheKey, cache: &Arc<Cache>) -> anyhow::Result<PathBuf> {
    let src = key.src.as_path();

    // 二重チェック（並列 prefetch が先に書いた可能性）
    if let Some(cached) = cache.get(key) {
        return Ok(cached);
    }

    tracing::debug!("prefetch: processing {}", src.display());

    // マジックバイトによるフォーマット検出（拡張子に依存しない）
    let reader = image::ImageReader::open(src)
        .map_err(|e| anyhow::anyhow!("failed to open {}: {}", src.display(), e))?;
    let ext_fmt = reader.format(); // 拡張子から推定したフォーマット
    let reader = reader
        .with_guessed_format()
        .map_err(|e| anyhow::anyhow!("failed to read {}: {}", src.display(), e))?;
    let content_fmt = reader.format(); // マジックバイトから検出したフォーマット

    // 拡張子と実際のフォーマットが異なる場合は警告
    if let (Some(ef), Some(cf)) = (ext_fmt, content_fmt) {
        if ef != cf {
            tracing::warn!(
                "extension/format mismatch: {} (extension → {:?}, content → {:?}); decoding as {:?}",
                src.display(), ef, cf, cf,
            );
        }
    }

    // EXIF Orientation を読み取りつつデコード。decode() は orientation を取り出す前に
    // reader を消費するので into_decoder() で分解する必要がある。
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| anyhow::anyhow!("failed to create decoder for {}: {}", src.display(), e))?;
    let orientation = decoder.orientation().unwrap_or_else(|e| {
        tracing::debug!("orientation read failed for {}: {}", src.display(), e);
        image::metadata::Orientation::NoTransforms
    });
    let mut img = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| anyhow::anyhow!("failed to decode {}: {}", src.display(), e))?;
    if !matches!(orientation, image::metadata::Orientation::NoTransforms) {
        tracing::debug!("applying EXIF orientation {:?} to {}", orientation, src.display());
        img.apply_orientation(orientation);
    }

    let processed = crate::display_mode::process(
        &img,
        key.screen_w,
        key.screen_h,
        key.mode,
        key.blur_sigma,
        key.bg_darken,
    );

    cache.store(key, &processed)
}
