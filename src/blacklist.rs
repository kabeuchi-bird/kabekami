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
    /// すでに登録済みの場合は何もしない。保存失敗時はロールバックして `Err` を返す。
    pub fn add(&mut self, path: &Path) -> Result<()> {
        let path_buf = path.to_path_buf();
        if self.paths.insert(path_buf.clone()) {
            if let Err(e) = self.save() {
                self.paths.remove(&path_buf);
                return Err(e);
            }
        }
        Ok(())
    }

    /// `kabekami_common::atomic_write` でブラックリストを永続化する。
    /// 一意な tmp 名 (PID + nanos) + fsync + 親ディレクトリ fsync で
    /// 電源断・並列書き込み耐性を確保する。
    fn save(&self) -> Result<()> {
        let content: String = self
            .paths
            .iter()
            .map(|p| format!("{}\n", p.display()))
            .collect();
        kabekami_common::atomic_write::atomic_write(&self.file_path, content.as_bytes())?;
        Ok(())
    }
}
