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

    /// パスをブラックリストに追加してファイルに永続化する。
    /// すでに登録済みの場合は何もしない。保存に失敗した場合はメモリ上の集合も
    /// ロールバックし、`false` を返す（失敗の詳細は `save_offloaded` が warn に出す）。
    ///
    /// 書き込みは `atomic_write`（一意な tmp 名 + fsync + 親ディレクトリ fsync）で
    /// 電源断・並列書き込みに耐える。その fsync 2 回は `state::save_offloaded` で
    /// `spawn_blocking` に逃がす（単一ワーカー上で同期実行すると D-Bus・トレイ・
    /// タイマーごと待たせ、キー操作がその場で止まって見える）。
    pub async fn add(&mut self, path: &Path) -> bool {
        let path_buf = path.to_path_buf();
        if !self.paths.insert(path_buf.clone()) {
            return true;
        }
        let content = self.serialize();
        let file_path = self.file_path.clone();
        let saved = crate::state::save_offloaded(
            move || {
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

    /// ファイルに書き出す内容（1 行 1 パス）。
    fn serialize(&self) -> String {
        self.paths
            .iter()
            .map(|p| format!("{}\n", p.display()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 書き込みを `spawn_blocking` に逃がしても、ファイルには確実に残る。
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

    /// 登録済みのパスは書き込みを起こさない（同じキーの連打で fsync を繰り返さない）。
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
