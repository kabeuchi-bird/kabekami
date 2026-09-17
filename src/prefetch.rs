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

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use image::ImageDecoder;
use tokio::task::JoinHandle;

use crate::cache::{Cache, CacheKey};

/// 先読みタスクの管理。
///
/// - `start()` で新しい先読みを開始する。前の先読みが走っていれば abort する。
/// - `abort()` で明示的にキャンセルできる（「次へ」連打時など）。
pub struct Prefetcher {
    /// 走行中の先読みタスク。解像度ごとに 1 本走るため複数持つ。
    pending: Vec<JoinHandle<()>>,
    /// 加工中のキャッシュ出力パス（= キーの同一性）。
    ///
    /// `abort()` は外側のタスクしか止められず `spawn_blocking` の中は走り切るため、
    /// これが無いと abort 直後の `start()` が同じ画像を二重にデコードする。
    /// `Cache::store` が弾けるのは書き込みだけで、そこに至る加工は弾けない。
    inflight: Arc<Mutex<HashSet<PathBuf>>>,
}

impl Prefetcher {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            inflight: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// 指定したキャッシュキー群に対応する画像の先読み加工をバックグラウンドで開始する。
    ///
    /// すでに先読み中のタスクは abort してから起動する。キャッシュにあるキーと
    /// 加工中のキーは飛ばす。`CacheKey` が解像度を含むのでキーは複数受け取る
    /// （1 つだけ温めても解像度の違うモニターは切り替えの瞬間にミスする）。
    pub fn start(&mut self, keys: impl IntoIterator<Item = CacheKey>, cache: Arc<Cache>) {
        self.abort();

        for key in keys {
            // キャッシュにすでにある場合はタスク不要
            if cache.get(&key).is_some() {
                tracing::debug!(
                    "prefetch: cache hit, skipping {} ({}x{})",
                    key.src.display(), key.screen_w, key.screen_h,
                );
                continue;
            }

            // 加工中のキーは投げ直さない（abort しても加工自体は止まらないため）
            let out = cache.path_for(&key);
            if !lock(&self.inflight).insert(out.clone()) {
                tracing::debug!(
                    "prefetch: already in flight, skipping {} ({}x{})",
                    key.src.display(), key.screen_w, key.screen_h,
                );
                continue;
            }
            // ガードは `spawn_blocking` のクロージャに持たせる。外側のタスクを
            // abort されても、加工が終わった時点で必ずキーが外れる。
            let guard = InflightGuard { set: Arc::clone(&self.inflight), out };

            tracing::debug!(
                "prefetch: starting for {} ({}x{})",
                key.src.display(), key.screen_w, key.screen_h,
            );
            let cache = cache.clone();
            self.pending.push(tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    let _guard = guard;
                    process_for_cache(&key, &cache)
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
    }

    /// 先読み中のタスクをすべてキャンセルする。
    pub fn abort(&mut self) {
        for handle in self.pending.drain(..) {
            handle.abort();
        }
    }
}

/// 加工中キーの集合からパスを外す RAII ガード。
///
/// `spawn_blocking` のクロージャに持たせる。走り出せば必ず終わり、走り出す前なら
/// クロージャごと捨てられるので、外側を abort されてもキーが取り残されない。
struct InflightGuard {
    set: Arc<Mutex<HashSet<PathBuf>>>,
    out: PathBuf,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        lock(&self.set).remove(&self.out);
    }
}

/// 毒された Mutex から中身を回収する。臨界区間は `insert` / `remove` だけなので
/// 毒されていても集合は壊れていない。先読みの重複排除でデーモンを落とさない。
fn lock(set: &Arc<Mutex<HashSet<PathBuf>>>) -> std::sync::MutexGuard<'_, HashSet<PathBuf>> {
    set.lock().unwrap_or_else(|e| e.into_inner())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DisplayMode;

    fn key(src: &str) -> CacheKey {
        CacheKey {
            src: PathBuf::from(src),
            screen_w: 1920,
            screen_h: 1080,
            mode: DisplayMode::Fill,
            blur_sigma: 0.0,
            bg_darken: 0.0,
        }
    }

    /// 加工中のキーが外れないと、そのキーは二度と先読みされなくなる。
    /// `spawn_blocking` が走り切ったあと必ず外れることが前提。
    #[test]
    fn inflight_guard_releases_the_key_on_drop() {
        let set: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
        let out = PathBuf::from("/cache/out.webp");
        lock(&set).insert(out.clone());
        {
            let _guard = InflightGuard { set: Arc::clone(&set), out: out.clone() };
            assert!(lock(&set).contains(&out), "前提: ガード生存中は保持される");
        }
        assert!(!lock(&set).contains(&out), "ドロップで必ず外れる");
    }

    /// 同じキーに 2 本のタスクを立てない。`start` は同期なので、`await` する前に
    /// 立ったタスクの本数を数えられる（加工そのものはまだ走っていない）。
    #[tokio::test]
    async fn start_submits_one_task_per_key() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(Cache::new(dir.path().to_path_buf(), 0));
        let mut prefetcher = Prefetcher::new();

        prefetcher.start([key("/nonexistent/a.jpg"), key("/nonexistent/a.jpg")], cache);

        assert_eq!(
            prefetcher.pending.len(),
            1,
            "同じキーを渡されても加工は 1 本だけ"
        );
    }
}
