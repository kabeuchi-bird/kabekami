//! 加工済み画像のキャッシュ管理。
//!
//! ## キャッシュキー
//! `DefaultHasher`(元画像の絶対パス | 画面幅 | 画面高 | DisplayMode | blur_sigma | bg_darken)
//! → 16 進数文字列 + `.webp` がキャッシュファイル名となる。
//!
//! ## LRU 退避
//! `store()` の中で `evict_if_needed()` を呼び、総容量が `max_size_bytes` を
//! 超えていれば更新日時の古いファイルから削除する。
//! ヒット時にも mtime を更新するので、順序は最終使用順になる。
//! 直近 `EVICT_GRACE` 以内に使われたファイルは消さない（適用中・先読み中の
//! 画像を消して存在しないパスを Plasma に渡すのを防ぐ）。

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

use crate::config::{Display, DisplayMode};

// ponytail: 時間ベースの保護。上限が極端に小さいと一時的に超過する。厳密にするなら使用中パスを明示的に pin する
const EVICT_GRACE: Duration = Duration::from_secs(60);

/// 加工済み画像のキャッシュ。`Arc<Cache>` で共有して使う。
pub struct Cache {
    /// キャッシュディレクトリ（`~/.cache/kabekami/`）
    pub directory: PathBuf,
    /// LRU 退避の容量上限（バイト）。0 なら無制限。
    max_size_bytes: u64,
}

/// キャッシュのルックアップ・格納に使うキー。
#[derive(Clone, Debug)]
pub struct CacheKey {
    pub src: PathBuf,
    pub screen_w: u32,
    pub screen_h: u32,
    pub mode: DisplayMode,
    pub blur_sigma: f32,
    pub bg_darken: f32,
}

impl CacheKey {
    /// 1 モニター分のキャッシュキーを組む。
    ///
    /// 加工結果を決めるのは「元画像・解像度・表示設定」だけ。適用側と先読み側が
    /// 別々に組み立てると、片方に設定を足したとき先読みが無音でミスし続ける。
    /// 組み立てを型の側に 1 つ置いて、そのズレが起きない形にする。
    pub fn new(src: &Path, screen_w: u32, screen_h: u32, display: &Display) -> Self {
        Self {
            src: src.to_path_buf(),
            screen_w,
            screen_h,
            mode: display.mode,
            blur_sigma: display.blur_sigma,
            bg_darken: display.bg_darken,
        }
    }
}

impl Cache {
    pub fn new(directory: PathBuf, max_size_mb: u64) -> Self {
        Self {
            directory,
            max_size_bytes: max_size_mb.saturating_mul(1024 * 1024),
        }
    }

    /// キャッシュヒットなら該当ファイルのパスを返す。
    ///
    /// TOCTOU 注意: 返した直後に LRU 退避でファイルが消えることがある。
    /// 呼び出し元は IO エラー時にキャッシュミスとして再処理すること。
    pub fn get(&self, key: &CacheKey) -> Option<PathBuf> {
        let path = self.path_for(key);
        touch(&path).then_some(path)
    }

    /// 加工済み画像をキャッシュに保存し、そのパスを返す。
    ///
    /// すでに同じキーのファイルが存在する場合は書き込みをスキップして
    /// 既存のパスを返す（並列で先読みが書いた場合などの重複書き込み防止）。
    ///
    /// 保存後に容量超過なら LRU 退避まで行う。ブロッキング処理なので
    /// `spawn_blocking` から呼ぶこと。
    pub fn store(&self, key: &CacheKey, img: &image::RgbaImage) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.directory)
            .with_context(|| format!("failed to create cache dir: {}", self.directory.display()))?;

        let path = self.path_for(key);
        if touch(&path) {
            return Ok(path);
        }

        // WebP 可逆圧縮（アルファ保持・品質劣化なし）。clone 不要で直接書き出す。
        img.save_with_format(&path, image::ImageFormat::WebP)
            .with_context(|| format!("WebP encode failed: {}", path.display()))?;

        tracing::debug!("cached: {}", path.display());
        if let Err(e) = self.evict_if_needed() {
            tracing::warn!("eviction failed: {}", e);
        }
        Ok(path)
    }

    /// `max_size_bytes` を超えていたら古いキャッシュファイルを LRU 順に削除する。
    fn evict_if_needed(&self) -> Result<()> {
        if self.max_size_bytes == 0 {
            return Ok(());
        }
        let entries = cache_entries_by_mtime(&self.directory)?;
        let total: u64 = entries.iter().map(|(_, size, _)| size).sum();

        let cutoff = SystemTime::now() - EVICT_GRACE;

        let mut remaining = total;
        for (path, size, mtime) in &entries {
            // mtime 昇順なので、猶予内のファイルに達したら以降もすべて猶予内
            if remaining <= self.max_size_bytes || *mtime > cutoff {
                break;
            }
            match std::fs::remove_file(path) {
                Ok(()) => {
                    tracing::debug!("evicted from cache: {}", path.display());
                    remaining -= size;
                }
                Err(e) => {
                    tracing::warn!("eviction failed for {}: {}", path.display(), e);
                }
            }
        }
        Ok(())
    }

    /// キャッシュキーからファイルパスを導出する（ファイルの存在は確認しない）。
    pub fn path_for(&self, key: &CacheKey) -> PathBuf {
        let hash = Self::compute_hash(key);
        self.directory.join(format!("{hash}.webp"))
    }

    /// キャッシュキーのハッシュ値（16 進 16 文字）を計算する。
    ///
    /// `DefaultHasher` のアルゴリズムは Rust のリリース間で変わりうるが、
    /// 変わってもキャッシュが作り直されるだけなので問題ない。
    fn compute_hash(key: &CacheKey) -> String {
        use std::hash::{Hash, Hasher};
        let mut h = std::hash::DefaultHasher::new();
        key.src.hash(&mut h);
        (key.screen_w, key.screen_h, key.mode).hash(&mut h);
        // f32 は Hash を持たないのでビット列で（±0 や NaN も bit-exact に区別）
        (key.blur_sigma.to_bits(), key.bg_darken.to_bits()).hash(&mut h);
        format!("{:016x}", h.finish())
    }
}

/// 既存ファイルの mtime を現在時刻にする（LRU の「使用」扱い）。読めなければ false。
///
/// mtime の更新失敗（読み取り専用ファイル・FS など）はヒットのまま扱う。
/// miss にすると同じパスへの再保存も失敗し、壁紙の適用自体が止まる。
fn touch(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let _ = file.set_modified(SystemTime::now());
    true
}

/// kabekami がこれまでに書き出したことがある拡張子をすべて列挙する。
/// フォーマット変更後も旧形式のファイルが LRU 退避対象から漏れないようにする。
const CACHE_EXTS: &[&str] = &["jpg", "webp", "png"];

/// キャッシュディレクトリ内の画像ファイルを mtime 昇順（古い順）で返す。
/// `CACHE_EXTS` に含まれる拡張子のみを対象とする。
fn cache_entries_by_mtime(dir: &Path) -> Result<Vec<(PathBuf, u64, SystemTime)>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir).context("failed to read cache directory")? {
        let entry = entry?;
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !CACHE_EXTS.contains(&ext) {
            continue;
        }
        let meta = entry.metadata()?;
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        entries.push((path, meta.len(), mtime));
    }
    entries.sort_by_key(|(_, _, t)| *t);
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn tmp_cache(name: &str) -> Cache {
        let dir = std::env::temp_dir().join(format!("kabekami-cache-test-{}", name));
        let _ = std::fs::remove_dir_all(&dir);
        Cache::new(dir, 10)
    }

    fn solid_rgba(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba([100, 150, 200, 255]))
    }

    fn key(src: &str) -> CacheKey {
        CacheKey {
            src: PathBuf::from(src),
            screen_w: 1920,
            screen_h: 1080,
            mode: DisplayMode::BlurPad,
            blur_sigma: 25.0,
            bg_darken: 0.1,
        }
    }

    #[test]
    fn store_and_get_roundtrip() {
        let cache = tmp_cache("roundtrip");
        let k = key("/tmp/foo.jpg");
        assert!(cache.get(&k).is_none(), "cache should be empty initially");

        let img = solid_rgba(100, 100);
        let stored = cache.store(&k, &img).unwrap();
        assert!(stored.exists());

        let got = cache.get(&k).expect("should hit after store");
        assert_eq!(got, stored);
    }

    #[test]
    fn different_keys_produce_different_paths() {
        let cache = tmp_cache("keys");
        let k1 = key("/tmp/a.jpg");
        let k2 = key("/tmp/b.jpg");
        assert_ne!(cache.path_for(&k1), cache.path_for(&k2));
    }

    #[test]
    fn mode_and_sigma_affect_hash() {
        let cache = tmp_cache("hash");
        let mut k1 = key("/tmp/x.jpg");
        let mut k2 = k1.clone();
        k2.mode = DisplayMode::Fill;
        assert_ne!(cache.path_for(&k1), cache.path_for(&k2));

        k1.blur_sigma = 10.0;
        k2.blur_sigma = 20.0;
        k2.mode = k1.mode;
        assert_ne!(cache.path_for(&k1), cache.path_for(&k2));
    }

    #[test]
    fn eviction_removes_old_files_but_keeps_recent_ones() {
        // max 1 MB。新しいファイル単体で上限を超えても、猶予内なので消さない
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf(), 1);

        let old_path = dir.path().join("0000old.jpg");
        let new_path = dir.path().join("zzzznew.jpg");
        std::fs::write(&new_path, vec![0u8; 1200 * 1024]).unwrap();
        let old = std::fs::File::create(&old_path).unwrap();
        old.set_len(600 * 1024).unwrap();
        old.set_modified(SystemTime::now() - EVICT_GRACE * 2)
            .unwrap();

        cache.evict_if_needed().unwrap();

        assert!(!old_path.exists(), "old file should be evicted");
        assert!(
            new_path.exists(),
            "recent file must survive even over the limit"
        );
    }

    #[test]
    fn cache_hit_protects_file_from_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf(), 1);
        let k = key("/tmp/hit.jpg");
        let path = cache.path_for(&k);
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(1200 * 1024).unwrap();
        f.set_modified(SystemTime::now() - EVICT_GRACE * 2).unwrap();

        assert!(cache.get(&k).is_some());
        cache.evict_if_needed().unwrap();

        assert!(path.exists(), "a cache hit must refresh mtime");
    }

    /// 書き込み用に開けないファイルでもヒット扱いにする。
    #[test]
    fn read_only_file_is_still_a_hit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf(), 0);
        let k = key("/tmp/ro.jpg");
        let path = cache.path_for(&k);
        std::fs::write(&path, b"x").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();

        assert_eq!(cache.get(&k), Some(path));
    }
}
