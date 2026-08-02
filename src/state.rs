//! デーモンのランタイム状態の永続化。
//!
//! `~/.config/kabekami/state.toml` に保存する。ユーザー設定（config.toml）とは
//! 分離し、一時停止状態など再起動をまたいで維持したい動的状態のみを管理する。
//!
//! フィールドが `paused` 1 つだけのため、toml クレートを使わず自前でパースする。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

#[derive(Debug, Default)]
pub struct DaemonState {
    pub paused: bool,
}

impl DaemonState {
    pub fn load(config_dir: &Path) -> Self {
        let path = Self::path(config_dir);
        match std::fs::read_to_string(&path) {
            Ok(s) => Self { paused: Self::parse_paused(&s) },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                tracing::warn!("failed to read {}: {}, using defaults", path.display(), e);
                Self::default()
            }
        }
    }

    pub fn save(&self, config_dir: &Path) -> Result<()> {
        let path = Self::path(config_dir);
        let text = format!("paused = {}\n", self.paused);
        kabekami_common::atomic_write::atomic_write(&path, text.as_bytes())
            .with_context(|| format!("failed to write state: {}", path.display()))
    }

    fn path(config_dir: &Path) -> PathBuf {
        config_dir.join("state.toml")
    }

    fn parse_paused(s: &str) -> bool {
        for line in s.lines() {
            let line = line.trim();
            if let Some(value) = line.strip_prefix("paused") {
                let value = value.trim_start().strip_prefix('=').map(|v| v.trim());
                if value == Some("true") {
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_paused_true() {
        assert!(DaemonState::parse_paused("paused = true\n"));
        assert!(DaemonState::parse_paused("paused=true"));
        assert!(DaemonState::parse_paused("  paused  =  true  \n"));
    }

    #[test]
    fn parse_paused_false() {
        assert!(!DaemonState::parse_paused("paused = false\n"));
        assert!(!DaemonState::parse_paused(""));
        assert!(!DaemonState::parse_paused("something_else = true\n"));
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let st = DaemonState { paused: true };
        st.save(dir.path()).unwrap();

        let loaded = DaemonState::load(dir.path());
        assert!(loaded.paused);

        let st = DaemonState { paused: false };
        st.save(dir.path()).unwrap();

        let loaded = DaemonState::load(dir.path());
        assert!(!loaded.paused);
    }
}
