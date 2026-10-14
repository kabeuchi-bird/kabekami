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
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// プロセス内の tmp 名衝突を確実に避けるための単調増加カウンタ。
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// `path` にバイト列をアトミックに書き込む。
///
/// - 一意な tmp 名 (`<basename>.<pid>.<nanos>.<counter>.tmp`) で別プロセスの
///   並列書き込みでも衝突しない
/// - tmp 書き込み後 `sync_all()` で永続化、`rename()` で差し替え、最後に親ディレクトリも fsync
///
/// 失敗時は tmp ファイルを掃除してからエラーを返す。
pub fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    // シンボリックリンク（ドットファイル管理など）はリンク先に書く。そのまま rename
    // するとリンクが実ファイルに置き換わる（#60）。
    let path = &write_target(path)?;
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory")
    })?;
    fs::create_dir_all(parent)?;

    let tmp = unique_tmp_path(path);

    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)?;

    if let Err(e) = (|| -> io::Result<()> {
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

/// 実際に書き込む先。シンボリックリンクならリンク先をたどる。
///
/// リンク先がまだ無い（リンク切れ）と `canonicalize` は失敗するので、そのときは
/// `read_link` で 1 段ずつたどる（相対パスのリンク先はリンクの親ディレクトリ基準）。
/// たどり着いた先がまだ無ければ、親ディレクトリだけ実体に揃える（別名のパスから
/// 書いても同じ先になるように）。循環していれば、リンクを実ファイルで潰さないよう
/// エラーにする。
pub fn write_target(path: &Path) -> io::Result<std::path::PathBuf> {
    if let Ok(real) = fs::canonicalize(path) {
        return Ok(real);
    }
    let mut cur = path.to_path_buf();
    // Linux の SYMLOOP_MAX と同じ 40 段で打ち切る
    for _ in 0..40 {
        match fs::read_link(&cur) {
            Ok(target) => {
                cur = match cur.parent() {
                    Some(parent) if target.is_relative() => parent.join(target),
                    _ => target,
                };
            }
            // リンクではない（まだ無いファイル）。ここが書き込み先
            Err(_) => {
                let real_parent = cur.parent().and_then(|p| fs::canonicalize(p).ok());
                return Ok(match (real_parent, cur.file_name()) {
                    (Some(parent), Some(name)) => parent.join(name),
                    _ => cur,
                });
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("too many levels of symbolic links: {}", path.display()),
    ))
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

    #[test]
    fn writes_atomically_and_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let path = dir.join("file.txt");

        atomic_write(&path, b"first").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"first");

        atomic_write(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");

        // tmp ファイルが残っていないこと
        let leftover: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftover.is_empty(), "no tmp files should remain");
    }

    /// シンボリックリンクはリンクのまま残し、リンク先を書き換える。
    #[test]
    fn writes_through_symlinks_without_replacing_them() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.toml");
        let link = dir.path().join("config.toml");
        fs::write(&target, b"old").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        atomic_write(&link, b"new").unwrap();

        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(&target).unwrap(), b"new");
    }

    /// リンク先がまだ無いシンボリックリンクでも、リンクを実ファイルに置き換えない。
    #[cfg(unix)]
    #[test]
    fn dangling_symlink_is_kept_and_its_target_is_created() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("dotfiles")).unwrap();
        let link = dir.path().join("config.toml");
        // 相対パスのリンク先（リンクの親ディレクトリ基準）
        std::os::unix::fs::symlink("dotfiles/config.toml", &link).unwrap();

        atomic_write(&link, b"x").unwrap();

        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "リンクのまま");
        assert_eq!(fs::read(dir.path().join("dotfiles/config.toml")).unwrap(), b"x");
    }

    /// 循環したリンクは書き込まずにエラーにし、リンクのまま残す。
    #[cfg(unix)]
    #[test]
    fn symlink_loop_is_an_error_and_keeps_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::os::unix::fs::symlink(&b, &a).unwrap();
        std::os::unix::fs::symlink(&a, &b).unwrap();

        assert!(atomic_write(&a, b"x").is_err());
        assert!(fs::symlink_metadata(&a).unwrap().file_type().is_symlink(), "リンクのまま");
    }

    #[test]
    fn unique_tmp_paths_differ() {
        let p = Path::new("/tmp/kabekami-test-unique/file.toml");
        let a = unique_tmp_path(p);
        let b = unique_tmp_path(p);
        assert_ne!(a, b, "consecutive tmp paths must be unique");
    }
}
