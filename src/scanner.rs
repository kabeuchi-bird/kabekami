//! 画像ファイルの走査・リスト構築。
//!
//! 設定された `directories` を走査し、拡張子で画像ファイルをフィルタして
//! `Vec<PathBuf>` を返す。`recursive = true` ならサブディレクトリも辿る。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// 画像として扱う拡張子（大文字小文字を無視して比較する）。
/// `image` クレートで有効化している feature と揃える（jpeg/png/webp/avif）。
pub(crate) const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "webp", "avif",
];

/// 指定されたディレクトリを走査して画像ファイルのパス一覧を返す。
///
/// - 見つからないディレクトリは警告を出して無視する（壊れた設定でも起動できるように）。
/// - 読み取りエラーが起きたサブディレクトリはスキップする。
/// - 返値は決定性のためにソートされる。
pub fn scan(directories: &[PathBuf], recursive: bool) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    for dir in directories {
        if !dir.exists() {
            tracing::warn!("source directory not found: {}", dir.display());
            continue;
        }
        if !dir.is_dir() {
            tracing::warn!("source is not a directory: {}", dir.display());
            continue;
        }
        scan_dir(dir, recursive, &mut out, &mut visited)
            .with_context(|| format!("failed to scan directory: {}", dir.display()))?;
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// `visited` は走査済みディレクトリの実体（canonicalize 後）。シンボリックリンクで
/// 同じディレクトリに戻ってくる循環を止め、重なったソース指定の二重走査も避ける。
fn scan_dir(
    dir: &Path,
    recursive: bool,
    out: &mut Vec<PathBuf>,
    visited: &mut HashSet<PathBuf>,
) -> Result<()> {
    if let Ok(real) = dir.canonicalize() {
        if !visited.insert(real) {
            return Ok(());
        }
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) => {
            tracing::warn!("cannot read dir {}: {}", dir.display(), err);
            return Ok(());
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!("dir entry error in {}: {}", dir.display(), err);
                continue;
            }
        };
        let path = entry.path();
        // `DirEntry::file_type` はリンクを辿らないので、リンクはリンク先で判定する
        // （辿らないと is_file も is_dir も偽になり、黙って読み飛ばす。#61）。
        let file_type = match entry.file_type() {
            Ok(ft) if ft.is_symlink() => match std::fs::metadata(&path) {
                Ok(m) => m.file_type(),
                Err(_) => continue, // リンク切れ
            },
            Ok(ft) => ft,
            Err(err) => {
                tracing::warn!("cannot stat {}: {}", path.display(), err);
                continue;
            }
        };
        if file_type.is_dir() {
            if recursive {
                let _ = scan_dir(&path, recursive, out, visited);
            }
        } else if file_type.is_file() && is_image(&path) {
            out.push(path);
        }
    }
    Ok(())
}

pub(crate) fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| {
            IMAGE_EXTENSIONS
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(ext))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kabekami-scanner-test-{}", name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(p: &Path) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, b"").unwrap();
    }

    #[test]
    fn picks_up_images_by_extension() {
        let root = tmp_dir("ext");
        touch(&root.join("a.jpg"));
        touch(&root.join("b.PNG"));
        touch(&root.join("ignore.txt"));
        touch(&root.join("c.webp"));

        let found = scan(std::slice::from_ref(&root), false).unwrap();
        assert_eq!(found.len(), 3);
        assert!(found.iter().all(|p| is_image(p)));
    }

    #[test]
    fn recursive_vs_flat() {
        let root = tmp_dir("recursive");
        touch(&root.join("top.jpg"));
        touch(&root.join("sub/nested.jpg"));

        let flat = scan(std::slice::from_ref(&root), false).unwrap();
        assert_eq!(flat.len(), 1);

        let recursive = scan(std::slice::from_ref(&root), true).unwrap();
        assert_eq!(recursive.len(), 2);
    }

    #[test]
    fn missing_directory_is_warning_not_error() {
        let res = scan(&[PathBuf::from("/nonexistent/kabekami/xyz")], false).unwrap();
        assert!(res.is_empty());
    }

    /// リンクの画像・ディレクトリも拾い、自分を指す循環リンクでは止まる（#61）。
    #[cfg(unix)]
    #[test]
    fn follows_symlinks_without_looping() {
        use std::os::unix::fs::symlink;
        let root = tmp_dir("symlink");
        let other = tmp_dir("symlink-target");
        touch(&other.join("linked.jpg"));
        touch(&other.join("sub/deep.jpg"));
        symlink(other.join("linked.jpg"), root.join("file-link.jpg")).unwrap();
        symlink(other.join("sub"), root.join("dir-link")).unwrap();
        symlink(&root, root.join("loop")).unwrap();
        symlink(root.join("missing.jpg"), root.join("dangling.jpg")).unwrap();

        let found = scan(std::slice::from_ref(&root), true).unwrap();
        assert_eq!(found, vec![root.join("dir-link/deep.jpg"), root.join("file-link.jpg")]);
    }

}
