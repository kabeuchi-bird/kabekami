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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use tokio::sync::Semaphore;

use anyhow::{Context, Result};

use crate::config::{Display, DisplayMode};

// ponytail: 時間ベースの保護。上限が極端に小さいと一時的に超過する。厳密にするなら使用中パスを明示的に pin する
const EVICT_GRACE: Duration = Duration::from_secs(60);

/// 加工済み画像のキャッシュ。`Arc<Cache>` で共有して使う。
pub struct Cache {
    /// キャッシュディレクトリ（`~/.cache/kabekami/`）
    pub directory: PathBuf,
    /// LRU 退避の容量上限（バイト）。0 なら無制限。
    ///
    /// 設定リロードで容量だけが変わったときは `Cache` を作り直さずにここだけ
    /// 書き換える。作り直すと加工受付（`inflight`）が空の別物になり、走行中の
    /// 加工と同じキーを新旧の `Cache` が別々に加工してしまう。
    max_size_bytes: AtomicU64,
    /// 加工中の出力パスと、その完了を知らせる門。
    ///
    /// 前景の適用と先読みは同じ `Arc<Cache>` を共有するので、ここに置けば
    /// 両方が同じ受付を見る。`Semaphore` を「完了したら閉じる門」として使う
    /// のは取りこぼしを避けるため。閉じた `Semaphore` の `acquire()` は即
    /// エラーで返るので、待ち始めが完了より後でも待ちっぱなしにならない
    /// （`Notify` は通知前に待ち始めていないと取りこぼす）。
    inflight: Mutex<HashMap<PathBuf, Arc<Semaphore>>>,
}

/// `Cache::claim` の結果。
pub enum Claim {
    /// 加工権を取れた。`ClaimGuard` を落とすと待っている側が解放される。
    Owned(ClaimGuard),
    /// 他の誰かが加工中。この門が閉じるまで待ってからキャッシュを引き直す。
    Waiting(Arc<Semaphore>),
}

/// 加工権。`Drop` で受付から外し、待っている側を解放する。
///
/// 正常終了・エラー・panic の巻き戻し・future の破棄（タスクのキャンセル）の
/// いずれでも走るので、待ち側が取り残されない。
pub struct ClaimGuard {
    cache: Arc<Cache>,
    out: PathBuf,
    gate: Arc<Semaphore>,
}

impl Drop for ClaimGuard {
    fn drop(&mut self) {
        self.cache.lock_inflight().remove(&self.out);
        self.gate.close();
    }
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
            max_size_bytes: AtomicU64::new(mb_to_bytes(max_size_mb)),
            inflight: Mutex::new(HashMap::new()),
        }
    }

    /// 容量上限だけを差し替える（ディレクトリが同じ設定リロード用）。
    pub fn set_max_size_mb(&self, max_size_mb: u64) {
        self.max_size_bytes
            .store(mb_to_bytes(max_size_mb), Ordering::Relaxed);
    }

    /// 出力パスごとの加工権を取る。すでに誰かが加工中なら待つ側になる。
    pub fn claim(self: &Arc<Self>, out: PathBuf) -> Claim {
        let mut inflight = self.lock_inflight();
        if let Some(gate) = inflight.get(&out) {
            return Claim::Waiting(Arc::clone(gate));
        }
        let gate = Arc::new(Semaphore::new(0));
        inflight.insert(out.clone(), Arc::clone(&gate));
        Claim::Owned(ClaimGuard {
            cache: Arc::clone(self),
            out,
            gate,
        })
    }

    /// 毒された Mutex から中身を回収する。臨界区間は挿入と削除だけなので
    /// 毒されていても表は壊れていない。先読みの調整でデーモンを落とさない。
    fn lock_inflight(&self) -> MutexGuard<'_, HashMap<PathBuf, Arc<Semaphore>>> {
        self.inflight.lock().unwrap_or_else(|e| e.into_inner())
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

        // WebP 可逆圧縮（アルファ保持・品質劣化なし）。最終パスへ直接書くと、
        // エンコード中に別の要求の `get` が書きかけのファイルをヒットとして
        // 受け取ってしまう（`get` は開けるかしか見ない）。一時ファイルに書いて
        // から rename し、最終パスには完成したファイルしか現れないようにする。
        write_atomically(&path, |tmp| {
            img.save_with_format(tmp, image::ImageFormat::WebP)
                .with_context(|| format!("WebP encode failed: {}", path.display()))
        })?;

        tracing::debug!("cached: {}", path.display());
        if let Err(e) = self.evict_if_needed() {
            tracing::warn!("eviction failed: {}", e);
        }
        Ok(path)
    }

    /// `max_size_bytes` を超えていたら古いキャッシュファイルを LRU 順に削除する。
    fn evict_if_needed(&self) -> Result<()> {
        let max_size_bytes = self.max_size_bytes.load(Ordering::Relaxed);
        if max_size_bytes == 0 {
            return Ok(());
        }
        let entries = cache_entries_by_mtime(&self.directory)?;
        let total: u64 = entries.iter().map(|(_, size, _)| size).sum();

        let cutoff = SystemTime::now() - EVICT_GRACE;

        let mut remaining = total;
        for (path, size, mtime) in &entries {
            // mtime 昇順なので、猶予内のファイルに達したら以降もすべて猶予内
            if remaining <= max_size_bytes || *mtime > cutoff {
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

fn mb_to_bytes(mb: u64) -> u64 {
    mb.saturating_mul(1024 * 1024)
}

/// `write` に同じディレクトリの一時パスを渡して書かせ、成功したら `path` へ
/// rename する。同一 FS 内の rename は置き換えが一度に起きるので、`path` には
/// 完成したファイルしか現れない。失敗時は一時ファイルを消す。
///
/// 書いている途中でプロセスが落ちた場合は一時ファイルが残るが、最終パスに
/// 書きかけが残って以後ずっとヒットし続けるよりはよい。
fn write_atomically(path: &Path, write: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(
        ".{name}.{}-{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed),
    ));
    let result = write(&tmp).and_then(|()| {
        std::fs::rename(&tmp, path)
            .with_context(|| format!("failed to move cache file into place: {}", path.display()))
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
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

    /// 書いている最中の最終パスは存在してはいけない。存在すると、別の要求の
    /// `get` が書きかけのファイルをヒットとして受け取る（`get` は開けるかしか見ない）。
    #[test]
    fn store_never_exposes_a_partial_file_at_the_final_path() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf(), 0);
        let k = key("/tmp/foo.jpg");
        let final_path = cache.path_for(&k);

        write_atomically(&final_path, |tmp| {
            std::fs::write(tmp, b"half")?;
            assert!(!final_path.exists(), "書き込み中に最終パスが見えている");
            assert!(cache.get(&k).is_none(), "書き込み中にヒットしてはいけない");
            Ok(())
        })
        .unwrap();

        assert!(cache.get(&k).is_some(), "rename 後はヒットする");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path() != final_path)
            .collect();
        assert!(leftovers.is_empty(), "一時ファイルが残っている: {leftovers:?}");
    }

    /// 書き込みに失敗したら、最終パスにも一時ファイルにも何も残さない。
    #[test]
    fn a_failed_write_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("x.webp");

        let result = write_atomically(&final_path, |tmp| {
            std::fs::write(tmp, b"half")?;
            anyhow::bail!("encode failed")
        });

        assert!(result.is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "何も残さない");
    }

    /// 容量だけの設定変更は `Cache` を作り直さずに反映される（加工受付を保つため）。
    #[test]
    fn set_max_size_mb_takes_effect_on_the_next_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf(), 0);
        let old = dir.path().join("old.webp");
        let f = std::fs::File::create(&old).unwrap();
        f.set_len(2 * 1024 * 1024).unwrap();
        f.set_modified(SystemTime::now() - EVICT_GRACE * 2).unwrap();

        cache.evict_if_needed().unwrap();
        assert!(old.exists(), "前提: 上限 0（無制限）では消えない");

        cache.set_max_size_mb(1);
        cache.evict_if_needed().unwrap();
        assert!(!old.exists(), "新しい上限で退避される");
    }


    /// `store` が `write_atomically` を通っていることの確認。最終パスに「存在しない
    /// 先を指すシンボリックリンク」を置くと、直接書き込みはリンクをたどって先に
    /// ファイルを作り、rename はリンク自体を置き換える。この差で書き方を判別する。
    #[cfg(unix)]
    #[test]
    fn store_replaces_the_final_path_instead_of_writing_through_it() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf(), 0);
        let k = key("/tmp/foo.jpg");
        let final_path = cache.path_for(&k);
        let target = dir.path().join("target.webp");
        std::os::unix::fs::symlink(&target, &final_path).unwrap();

        cache.store(&k, &solid_rgba(4, 4)).unwrap();

        assert!(!target.exists(), "最終パスへ直接書いている（リンク先に書けた）");
        let meta = std::fs::symlink_metadata(&final_path).unwrap();
        assert!(meta.is_file(), "rename で通常ファイルに置き換わるべき");
    }

}
