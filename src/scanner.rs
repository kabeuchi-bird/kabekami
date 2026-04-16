//! 画像ファイルの走査・リスト構築。設計書 §9 / §11 に準拠。
//!
//! 設定された `directories` を走査し、拡張子で画像ファイルをフィルタして
//! `Vec<PathBuf>` を返す。`recursive = true` ならサブディレクトリも辿る。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// 画像として扱う拡張子（大文字小文字を無視して比較する）。
pub(crate) const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "webp", "bmp", "tiff", "tif", "gif",
];

/// 指定されたディレクトリを走査して画像ファイルのパス一覧を返す。
///
/// - 見つからないディレクトリは警告を出して無視する（壊れた設定でも起動できるように）。
/// - 読み取りエラーが起きたサブディレクトリはスキップする。
/// - 返値は決定性のためにソートされる。
pub fn scan(directories: &[PathBuf], recursive: bool) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for dir in directories {
        if !dir.exists() {
            tracing::warn!("source directory not found: {}", dir.display());
            continue;
        }
        if !dir.is_dir() {
            tracing::warn!("source is not a directory: {}", dir.display());
            continue;
        }
        scan_dir(dir, recursive, &mut out)
            .with_context(|| format!("failed to scan directory: {}", dir.display()))?;
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn scan_dir(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) -> Result<()> {
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
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(err) => {
                tracing::warn!("cannot stat {}: {}", path.display(), err);
                continue;
            }
        };
        if file_type.is_dir() {
            if recursive {
                let _ = scan_dir(&path, recursive, out);
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

        let found = scan(&[root.clone()], false).unwrap();
        assert_eq!(found.len(), 3);
        assert!(found.iter().all(|p| is_image(p)));
    }

    #[test]
    fn recursive_vs_flat() {
        let root = tmp_dir("recursive");
        touch(&root.join("top.jpg"));
        touch(&root.join("sub/nested.jpg"));

        let flat = scan(&[root.clone()], false).unwrap();
        assert_eq!(flat.len(), 1);

        let recursive = scan(&[root.clone()], true).unwrap();
        assert_eq!(recursive.len(), 2);
    }

    #[test]
    fn missing_directory_is_warning_not_error() {
        let res = scan(&[PathBuf::from("/nonexistent/kabekami/xyz")], false).unwrap();
        assert!(res.is_empty());
    }
}
