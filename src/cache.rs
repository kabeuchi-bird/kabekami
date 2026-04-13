//! 加工済み画像のキャッシュ管理。設計書 §12 に準拠。
//!
//! ## キャッシュキー
//! SHA256(元画像の絶対パス | 画面幅 | 画面高 | DisplayMode | blur_sigma | bg_darken)
//! → 16 進数文字列 + `.jpg` がキャッシュファイル名となる。
//!
//! ## LRU 退避
//! `store()` の後に `evict_if_needed()` を呼び、総容量が `max_size_bytes` を
//! 超えていれば更新日時の古いファイルから削除する。

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::config::DisplayMode;

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

impl Cache {
    pub fn new(directory: PathBuf, max_size_mb: u64) -> Self {
        Self {
            directory,
            max_size_bytes: max_size_mb.saturating_mul(1024 * 1024),
        }
    }

    /// キャッシュヒットなら該当ファイルのパスを返す。
    pub fn get(&self, key: &CacheKey) -> Option<PathBuf> {
        let path = self.path_for(key);
        if path.exists() {
            Some(path)
        } else {
            None
        }
    }

    /// 加工済み画像をキャッシュに保存し、そのパスを返す。
    ///
    /// すでに同じキーのファイルが存在する場合は書き込みをスキップして
    /// 既存のパスを返す（並列で先読みが書いた場合などの重複書き込み防止）。
    pub fn store(&self, key: &CacheKey, img: &image::RgbaImage) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.directory)
            .with_context(|| format!("failed to create cache dir: {}", self.directory.display()))?;

        let path = self.path_for(key);
        if path.exists() {
            return Ok(path);
        }

        // JPEG quality 92（設計書 §5 に指定）
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
            std::fs::File::create(&path)
                .with_context(|| format!("cannot create cache file: {}", path.display()))?,
            92,
        );
        image::DynamicImage::ImageRgba8(img.clone())
            .into_rgb8()
            .write_with_encoder(encoder)
            .with_context(|| format!("JPEG encode failed: {}", path.display()))?;

        tracing::debug!("cached: {}", path.display());
        self.evict_if_needed()?;
        Ok(path)
    }

    /// `max_size_bytes` を超えていたら古いキャッシュファイルを LRU 順に削除する。
    pub fn evict_if_needed(&self) -> Result<()> {
        if self.max_size_bytes == 0 {
            return Ok(());
        }
        let entries = cache_entries_by_mtime(&self.directory)?;
        let total: u64 = entries.iter().map(|(_, size, _)| size).sum();
        if total <= self.max_size_bytes {
            return Ok(());
        }

        let mut remaining = total;
        for (path, size, _) in &entries {
            if remaining <= self.max_size_bytes {
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
        drop(entries);
        Ok(())
    }

    /// キャッシュキーからファイルパスを導出する（ファイルの存在は確認しない）。
    pub fn path_for(&self, key: &CacheKey) -> PathBuf {
        let hash = Self::compute_hash(key);
        self.directory.join(format!("{hash}.jpg"))
    }

    /// キャッシュキーのハッシュ値（SHA256 → 16 進 64 文字）を計算する。
    fn compute_hash(key: &CacheKey) -> String {
        let mut h = Sha256::new();
        // パスを正規化して OS 差異を吸収
        h.update(key.src.to_string_lossy().as_bytes());
        h.update(b"\x00");
        h.update(key.screen_w.to_le_bytes());
        h.update(key.screen_h.to_le_bytes());
        h.update(format!("{:?}", key.mode).as_bytes());
        h.update(b"\x00");
        // f32 は bit-exact 比較のため整数化して保存（±0 や NaN の問題を回避）
        h.update(key.blur_sigma.to_bits().to_le_bytes());
        h.update(key.bg_darken.to_bits().to_le_bytes());
        hex::encode(h.finalize())
    }
}

/// キャッシュディレクトリ内の `.jpg` ファイルを mtime 昇順（古い順）で返す。
fn cache_entries_by_mtime(dir: &Path) -> Result<Vec<(PathBuf, u64, SystemTime)>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir).context("failed to read cache directory")? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jpg") {
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
    fn eviction_removes_oldest_files_first() {
        // max 1 MB に設定し、2 MB 相当のファイルを書き込む
        let dir = std::env::temp_dir().join("kabekami-cache-evict");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = Cache::new(dir.clone(), 1);

        // ダミーファイルを 2 つ作成（それぞれ ~600KB）
        let data = vec![0u8; 600 * 1024];
        let old_path = dir.join("0000old.jpg");
        let new_path = dir.join("zzzznew.jpg");
        std::fs::write(&old_path, &data).unwrap();
        // 少し待ってから新しいファイルを書く（mtime が変わるように）
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&new_path, &data).unwrap();

        cache.evict_if_needed().unwrap();

        // 古い方が削除されているはず
        assert!(!old_path.exists(), "oldest file should be evicted");
        assert!(new_path.exists(), "newest file should remain");
    }
}
