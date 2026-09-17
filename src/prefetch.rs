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

use crate::cache::{Cache, CacheKey, Claim, ClaimGuard};

/// 先読みタスクの管理。
///
/// - `start()` で新しい先読みを開始する。前の先読みが走っていれば abort する。
/// - `abort()` で明示的にキャンセルできる（「次へ」連打時など）。
pub struct Prefetcher {
    /// 走行中の先読みタスク。解像度ごとに 1 本走る。
    pending: Vec<JoinHandle<()>>,
}

impl Prefetcher {
    pub fn new() -> Self {
        Self { pending: Vec::new() }
    }

    /// 指定したキャッシュキー群に対応する画像の先読み加工をバックグラウンドで開始する。
    ///
    /// 先読み中のタスクは abort してから起動し、キャッシュにあるキーは飛ばす。
    /// 加工中のキーは `process_single_flight` が待ち側に回すので、ここでは見ない。
    /// キーが複数なのは `CacheKey` が解像度を含むため
    /// （1 つだけ温めても解像度の違うモニターはミスする）。
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

            tracing::debug!(
                "prefetch: starting for {} ({}x{})",
                key.src.display(), key.screen_w, key.screen_h,
            );
            let cache = cache.clone();
            self.pending.push(tokio::spawn(async move {
                match process_single_flight(&key, &cache).await {
                    Ok(path) => tracing::debug!("prefetch: done → {}", path.display()),
                    Err(e) => tracing::warn!("prefetch: processing error: {:#}", e),
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

/// 1 つのキャッシュキーを加工してキャッシュパスを返す。同じキーの加工は
/// 同時に 1 回しか走らない（shared single-flight）。
///
/// 前景の壁紙適用と先読みは同じ `Arc<Cache>` を共有するので、受付を `Cache` に
/// 置くことで両方が同じ門を見る。`Prefetcher` 側だけで重複排除しても、
/// 前景が `abort()` 直後に同じキーを投げる経路（Next 連打）は防げない。
///
/// 後から来た側は先行の完了を待ってキャッシュを引き直す。先行が失敗・panic・
/// キャンセルされた場合は待ちが解放されてキャッシュミスになるので、そのときだけ
/// 自分で加工し直す。ループが回るのは「加工するか、加工している誰かを待つか」の
/// どちらかなので、同時に居る要求者の数で止まる。
pub async fn process_single_flight(key: &CacheKey, cache: &Arc<Cache>) -> anyhow::Result<PathBuf> {
    let out = cache.path_for(key);
    loop {
        if let Some(cached) = cache.get(key) {
            tracing::debug!("cache hit: {}", key.src.display());
            return Ok(cached);
        }
        match cache.claim(out.clone()) {
            Claim::Owned(guard) => {
                return run_blocking(key.clone(), Arc::clone(cache), guard).await;
            }
            Claim::Waiting(gate) => {
                // 門が閉じるまで待つ。閉じた `Semaphore` では即戻るので、
                // 待ち始めが完了より後でも取りこぼさない。
                let _ = gate.acquire().await;
            }
        }
    }
}

/// 加工を `spawn_blocking` に投げる。加工権はクロージャに持たせる。
///
/// 外側の future がキャンセルされてもクロージャは走り切るので、加工が終わるまで
/// 加工権が解放されない（待ち側が二重に走り出さない）。走り出す前に捨てられた
/// 場合はクロージャごと落ちるので、やはり取り残されない。
async fn run_blocking(
    key: CacheKey,
    cache: Arc<Cache>,
    guard: ClaimGuard,
) -> anyhow::Result<PathBuf> {
    use anyhow::Context;
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        process_for_cache(&key, &cache)
    })
    .await
    .context("image processing task panicked")?
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
    use image::{Rgba, RgbaImage};

    fn key(src: &str) -> CacheKey {
        CacheKey {
            src: PathBuf::from(src),
            screen_w: 64,
            screen_h: 64,
            mode: DisplayMode::Fill,
            blur_sigma: 0.0,
            bg_darken: 0.0,
        }
    }

    fn owned(claim: Claim) -> ClaimGuard {
        match claim {
            Claim::Owned(g) => g,
            Claim::Waiting(_) => panic!("加工権を取れるはずの場面で待ち側になった"),
        }
    }

    fn waiting(claim: Claim) -> Arc<tokio::sync::Semaphore> {
        match claim {
            Claim::Waiting(gate) => gate,
            Claim::Owned(_) => panic!("待ち側になるはずの場面で加工権を取った"),
        }
    }

    fn cache(dir: &tempfile::TempDir) -> Arc<Cache> {
        Arc::new(Cache::new(dir.path().to_path_buf(), 0))
    }

    /// 同じ出力パスの加工権は 1 つだけ。2 人目以降は待ち側になる。
    #[tokio::test]
    async fn only_one_claimant_owns_a_given_output() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(&dir);
        let out = PathBuf::from("/cache/a.webp");

        let guard = owned(cache.claim(out.clone()));
        let gate = waiting(cache.claim(out.clone()));
        assert!(
            gate.try_acquire().is_err(),
            "加工権を持っている間は待ち側が通れてはいけない"
        );
        drop(guard);
    }

    /// 加工権が外れると待ち側が解放され、次の要求者が加工権を取れる。
    #[tokio::test]
    async fn releasing_the_claim_wakes_waiters_and_frees_the_slot() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(&dir);
        let out = PathBuf::from("/cache/a.webp");

        let guard = owned(cache.claim(out.clone()));
        let gate = waiting(cache.claim(out.clone()));

        drop(guard);

        // 閉じた門なので即戻る（待ち始めが解放より後でも取りこぼさない）
        assert!(gate.acquire().await.is_err(), "門が閉じたので待ちは解ける");
        owned(cache.claim(out));
    }

    /// 加工権を持ったまま panic しても、巻き戻しで `Drop` が走って待ちが解ける。
    /// ここが漏れると、そのキーの加工を待つ側が永久に止まる。
    #[tokio::test]
    async fn a_panicking_holder_releases_waiters() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(&dir);
        let out = PathBuf::from("/cache/a.webp");

        let gate = {
            let guard = owned(cache.claim(out.clone()));
            let gate = waiting(cache.claim(out.clone()));
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                let _guard = guard;
                panic!("加工中の panic");
            }));
            assert!(result.is_err(), "前提: panic している");
            gate
        };

        assert!(gate.acquire().await.is_err(), "panic でも待ちは解ける");
        owned(cache.claim(out));
    }

    /// 加工権を持った future が走り出す前に捨てられても待ちが解ける
    /// （`Prefetcher::abort()` でタスクがキャンセルされる経路）。
    #[tokio::test]
    async fn a_cancelled_holder_releases_waiters() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(&dir);
        let out = PathBuf::from("/cache/a.webp");

        let guard = owned(cache.claim(out.clone()));
        let gate = waiting(cache.claim(out.clone()));

        // 加工権をクロージャへ移し、そのクロージャごと捨てる
        let never_run = move || {
            let _guard = guard;
        };
        drop(never_run);

        assert!(gate.acquire().await.is_err(), "キャンセルでも待ちは解ける");
        owned(cache.claim(out));
    }

    /// 同じキーを同時に投げても、全員が同じ出力パスを受け取る。
    /// 加工に入るのは `Claim::Owned` を取った 1 本だけで、残りは待って引き直す。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_requests_agree_on_one_output() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.png");
        RgbaImage::from_pixel(32, 32, Rgba([10, 20, 30, 255]))
            .save(&src)
            .expect("テスト画像を書けるべき");
        let cache = Arc::new(Cache::new(dir.path().join("cache"), 0));
        let k = key(src.to_str().unwrap());

        let results = futures_util::future::join_all((0..4).map(|_| {
            let cache = Arc::clone(&cache);
            let k = k.clone();
            async move { process_single_flight(&k, &cache).await }
        }))
        .await;

        let paths: Vec<PathBuf> = results
            .into_iter()
            .map(|r| r.expect("加工は成功するべき"))
            .collect();
        assert_eq!(paths.len(), 4);
        assert!(
            paths.windows(2).all(|w| w[0] == w[1]),
            "全員が同じ出力パスを受け取るべき: {:?}",
            paths
        );
        assert!(paths[0].exists(), "出力が書かれているべき");
    }

    /// 先行が失敗したら待ち側はキャッシュミスに戻り、自分で試して失敗を受け取る。
    /// 待ちっぱなしにならないことがここの主眼。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failing_source_does_not_leave_waiters_stuck() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(&dir);
        let k = key("/nonexistent/missing.png");

        let results = futures_util::future::join_all((0..3).map(|_| {
            let cache = Arc::clone(&cache);
            let k = k.clone();
            async move { process_single_flight(&k, &cache).await }
        }))
        .await;

        assert!(results.iter().all(|r| r.is_err()), "全員がエラーを受け取る");
        // 受付が空に戻っているので、次の要求者は加工権を取れる
        owned(cache.claim(cache.path_for(&k)));
    }

    /// `start` はキャッシュにあるキーを飛ばす。
    #[tokio::test]
    async fn start_skips_keys_already_cached() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(&dir);
        let k = key("/nonexistent/a.jpg");
        cache.store(&k, &RgbaImage::from_pixel(4, 4, Rgba([0, 0, 0, 255]))).unwrap();

        let mut prefetcher = Prefetcher::new();
        prefetcher.start([k], Arc::clone(&cache));

        assert!(prefetcher.pending.is_empty(), "キャッシュ済みなら起動しない");
    }
}
