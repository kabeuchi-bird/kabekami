//! 「二度と表示しない」ブラックリスト。
//!
//! `~/.config/kabekami/blacklist.txt` に 1 行 1 パスで保存する。
//! ファイルが存在しない場合は空のブラックリストとして動作する。

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Result;

pub struct Blacklist {
    paths: HashSet<PathBuf>,
    file_path: PathBuf,
}

impl Blacklist {
    /// `kabekami_config_dir/blacklist.txt` からブラックリストを読み込む。
    /// ファイルが存在しない場合は空リストで初期化する。
    /// `NotFound` 以外の IO エラーは `Err` として返す。
    pub fn load(kabekami_config_dir: &Path) -> Result<Self, io::Error> {
        let file_path = kabekami_config_dir.join("blacklist.txt");
        let content = match std::fs::read_to_string(&file_path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                tracing::warn!("failed to read blacklist at {}: {}", file_path.display(), e);
                return Err(e);
            }
        };
        let paths = content
            .lines()
            .map(|line| line.trim_end_matches('\r'))
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect();
        Ok(Self { paths, file_path })
    }

    /// パスがブラックリストに含まれるか判定する（O(1)）。
    pub fn contains(&self, path: &Path) -> bool {
        self.paths.contains(path)
    }

    /// パスをブラックリストに追加してファイルに永続化する。登録済みなら何もしない。
    /// 保存に失敗したらメモリ上の集合もロールバックして `false` を返す
    /// （詳細は `save_offloaded` が warn に出す）。
    ///
    /// 書き込みは `atomic_write`（一意な tmp 名 + fsync 2 回）で電源断に耐える。
    /// その fsync は `state::save_offloaded` で `spawn_blocking` へ逃がす
    /// （単一ワーカー上で同期実行するとキー操作がその場で止まって見える）。
    pub async fn add(&mut self, path: &Path) -> bool {
        let path_buf = path.to_path_buf();
        if !self.paths.insert(path_buf.clone()) {
            return true;
        }
        let line = format!("{}\n", path.display());
        let file_path = self.file_path.clone();
        let saved = crate::state::save_offloaded(
            move || {
                // メモリ上の集合を書き出すのではなく、ファイルを読み直して 1 行足す。
                // 動作中に手で編集（解除）された内容を、起動時の内容で潰さないため。
                // 手での編集がデーモンに反映されるのは次の起動から。
                let mut content = match std::fs::read_to_string(&file_path) {
                    Ok(s) => s,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
                    Err(e) => return Err(e.into()),
                };
                if !content.is_empty() && !content.ends_with('\n') {
                    content.push('\n');
                }
                content.push_str(&line);
                kabekami_common::atomic_write::atomic_write(&file_path, content.as_bytes())?;
                Ok(())
            },
            "blacklist",
        )
        .await;
        if !saved {
            self.paths.remove(&path_buf);
        }
        saved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ここが壊れると再起動でブラックリストが消え、除外した画像が戻ってくる。
    #[tokio::test]
    async fn add_persists_so_a_reload_still_excludes_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut bl = Blacklist::load(dir.path()).unwrap();
        let path = Path::new("/pics/nope.jpg");

        assert!(bl.add(path).await, "保存は成功する");
        assert!(bl.contains(path));

        let reloaded = Blacklist::load(dir.path()).unwrap();
        assert!(reloaded.contains(path), "読み直しても除外され続ける");
    }

    /// 動作中に手で消した行を、次の追加で書き戻さない。
    #[tokio::test]
    async fn add_keeps_manual_edits_to_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut bl = Blacklist::load(dir.path()).unwrap();
        assert!(bl.add(Path::new("/pics/a.jpg")).await);

        // 手で a を解除し、c を足した（末尾の改行なし）
        std::fs::write(dir.path().join("blacklist.txt"), "/pics/c.jpg").unwrap();
        assert!(bl.add(Path::new("/pics/b.jpg")).await);

        let reloaded = Blacklist::load(dir.path()).unwrap();
        assert!(!reloaded.contains(Path::new("/pics/a.jpg")), "解除した行が戻っている");
        assert!(reloaded.contains(Path::new("/pics/b.jpg")));
        assert!(reloaded.contains(Path::new("/pics/c.jpg")));
    }

    /// 登録済みのパスは書き込みを起こさない（連打で fsync を繰り返さない）。
    #[tokio::test]
    async fn adding_a_known_path_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let mut bl = Blacklist::load(dir.path()).unwrap();
        let path = Path::new("/pics/nope.jpg");
        assert!(bl.add(path).await);

        let file = dir.path().join("blacklist.txt");
        let before = std::fs::metadata(&file).unwrap().modified().unwrap();
        assert!(bl.add(path).await, "二度目も成功扱い");
        assert_eq!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            before,
            "二度目は書き込まない"
        );
    }
}
