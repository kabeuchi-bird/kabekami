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

    /// 一時ファイル → `rename` の atomic-write でブラックリストを保存する。
    /// 電源断や並列書き込みで `blacklist.txt` が中途半端な状態で残らないようにする。
    fn save(&self) -> Result<()> {
        if let Some(dir) = self.file_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let content: String = self
            .paths
            .iter()
            .map(|p| format!("{}\n", p.display()))
            .collect();

        let tmp = self.file_path.with_extension("txt.tmp");
        std::fs::write(&tmp, &content)?;
        if let Err(e) = std::fs::rename(&tmp, &self.file_path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        Ok(())
    }
}
