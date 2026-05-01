//! 「二度と表示しない」ブラックリスト。
//!
//! `~/.config/kabekami/blacklist.txt` に 1 行 1 パスで保存する。
//! ファイルが存在しない場合は空のブラックリストとして動作する。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;

pub struct Blacklist {
    paths: HashSet<PathBuf>,
    file_path: PathBuf,
}

impl Blacklist {
    /// `kabekami_config_dir`（`~/.config/kabekami/`）から読み込む。
    /// ファイルが存在しなければ空リストで初期化する。
    pub fn load(kabekami_config_dir: &Path) -> Self {
        let file_path = kabekami_config_dir.join("blacklist.txt");
        let paths = std::fs::read_to_string(&file_path)
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect();
        Self { paths, file_path }
    }

    /// パスがブラックリストに含まれるか判定する。
    pub fn contains(&self, path: &Path) -> bool {
        self.paths.contains(path)
    }

    /// パスをブラックリストに追加してファイルに永続化する。
    /// すでに登録済みの場合は何もしない。
    pub fn add(&mut self, path: &Path) -> Result<()> {
        if self.paths.insert(path.to_path_buf()) {
            self.save()?;
        }
        Ok(())
    }

    fn save(&self) -> Result<()> {
        if let Some(dir) = self.file_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let content: String = self
            .paths
            .iter()
            .map(|p| format!("{}\n", p.display()))
            .collect();
        std::fs::write(&self.file_path, content)?;
        Ok(())
    }
}
