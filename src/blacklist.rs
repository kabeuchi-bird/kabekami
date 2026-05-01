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
    /// Load the blacklist from `kabekami_config_dir/blacklist.txt`.
    ///
    /// If the file does not exist, initialize an empty blacklist. `kabekami_config_dir`
    /// is the path to the kabekami configuration directory (for example, `~/.config/kabekami`).
    ///
    /// # Examples
    ///
    /// ```
    /// let cfg_dir = std::path::Path::new("/home/user/.config/kabekami");
    /// let _blacklist = crate::blacklist::Blacklist::load(cfg_dir);
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if reading the file fails with an IO error other than `NotFound`.
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

    /// Check whether a path is present in the blacklist.
    ///
    /// # Returns
    ///
    /// `true` if the given `path` is present in the blacklist, `false` otherwise.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    /// # use kabekami::blacklist::Blacklist;
    ///
    /// let mut bl = Blacklist::load(&std::env::temp_dir());
    /// let p = Path::new("/tmp/example-path-for-blacklist");
    /// assert!(!bl.contains(p));
    /// bl.add(p).unwrap();
    /// assert!(bl.contains(p));
    /// ```
    pub fn contains(&self, path: &Path) -> bool {
        self.paths.contains(path)
    }

    /// Adds `path` to the blacklist and persists the updated list to the configured file.
    ///
    /// If `path` was already present, this method does nothing.
    ///
    /// # Errors
    ///
    /// Returns an error if writing the blacklist file or creating its parent directory fails.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    /// // Initialize a blacklist rooted at a temporary directory (example only).
    /// let tmp_dir = std::env::temp_dir().join(format!("kabekami_example_{}", std::process::id()));
    /// let mut bl = Blacklist::load(&tmp_dir);
    /// bl.add(Path::new("some/path")).unwrap();
    /// assert!(bl.contains(Path::new("some/path")));
    /// ```
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

    /// Persist the stored blacklist paths to the configured file on disk.
    ///
    /// This will create the file's parent directory if it does not exist and overwrite the file
    /// with one path per line using the paths' display representation.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::PathBuf;
    /// // create a temporary config dir
    /// let tmp = tempfile::tempdir().unwrap();
    /// let config_dir = tmp.path();
    /// let mut bl = crate::blacklist::Blacklist::load(config_dir);
    /// bl.add(&PathBuf::from("/some/path")).unwrap();
    /// // `add` calls `save`, so the file should now exist
    /// assert!(config_dir.join("blacklist.txt").exists());
    /// ```
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, or an `Err` propagated from underlying file-system operations.
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
