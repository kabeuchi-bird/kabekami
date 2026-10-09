//! ファイルのアトミック書き込みヘルパー。
//!
//! 電源断・並列書き込み耐性を両立するパターン:
//! 1. 一意な tmp 名 (PID + nanos + monotonic counter) を生成して衝突回避
//! 2. tmp に書き込み後 `sync_all()` で fsync
//! 3. `rename()` で本ファイル名に差し替え（POSIX rename はアトミック）
//! 4. 親ディレクトリも fsync して rename を永続化
//!
//! 5 番目の親ディレクトリ fsync は ext4/xfs などのジャーナリング FS で
//! rename をディスクに確実に反映させるために必要（電源断耐性）。

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// プロセス内の tmp 名衝突を確実に避けるための単調増加カウンタ。
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// `path` にバイト列をアトミックに書き込む。
///
/// - 一意な tmp 名 (`<basename>.<pid>.<nanos>.<counter>.tmp`) で別プロセスの
///   並列書き込みでも衝突しない
/// - tmp 書き込み後 `sync_all()` で永続化、`rename()` で差し替え、最後に親ディレクトリも fsync
/// - 権限: `path` が既にあればその権限を引き継ぎ、無ければ `0600` にする
///   （umask には左右されない）
///
/// 失敗時は tmp ファイルを掃除してからエラーを返す。
pub fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory")
    })?;
    fs::create_dir_all(parent)?;

    let tmp = unique_tmp_path(path);

    // 権限: 新規ファイルは 0600、既存ファイルはその権限を引き継ぐ。umask 任せに
    // すると、利用者が 0600 にした config.toml（API キー入り）が保存のたびに
    // 0644 へ戻る（#57）。作成時の `mode` は umask で削られる（umask 0777 なら
    // 000 になる）ので、作成後に `set_permissions` で明示し直す。
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;

    if let Err(e) = (|| -> io::Result<()> {
        let perms = fs::metadata(path)
            .map(|m| m.permissions())
            .unwrap_or_else(|_| fs::Permissions::from_mode(0o600));
        file.set_permissions(perms)?;
        file.write_all(contents)?;
        file.sync_all()?;
        Ok(())
    })() {
        drop(file);
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    drop(file);

    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    // 親ディレクトリの fsync。ext4/xfs では rename の永続化に必要。
    // 失敗してもデータ自体は書き込み済みなのでログ程度に留め、エラーは返さない。
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }

    Ok(())
}

fn unique_tmp_path(path: &Path) -> std::path::PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let suffix = format!(".{}.{}.{}.tmp", pid, nanos, counter);

    // 拡張子に suffix を追加（例: `config.toml` → `config.toml.<pid>.<nanos>.<counter>.tmp`）
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(&suffix);
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn writes_atomically_and_overwrites() {
        let dir = std::env::temp_dir().join(format!("kabekami-atomic-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("file.txt");

        atomic_write(&path, b"first").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"first");

        atomic_write(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");

        // tmp ファイルが残っていないこと
        let leftover: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftover.is_empty(), "no tmp files should remain");

        let _ = fs::remove_dir_all(&dir);
    }

    /// 新規は 0600、既存はその権限を保つ。API キー入りの config.toml を
    /// 他ユーザーから読めるようにしない（#57）。
    #[test]
    fn new_files_are_private_and_existing_permissions_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;

        atomic_write(&path, b"a").unwrap();
        assert_eq!(mode(&path), 0o600, "新規は 0600");

        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        atomic_write(&path, b"b").unwrap();
        assert_eq!(mode(&path), 0o640, "既存の権限を引き継ぐ");
    }

    #[test]
    fn unique_tmp_paths_differ() {
        let p = Path::new("/tmp/kabekami-test-unique/file.toml");
        let a = unique_tmp_path(p);
        let b = unique_tmp_path(p);
        assert_ne!(a, b, "consecutive tmp paths must be unique");
    }
}
