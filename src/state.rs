//! デーモンのランタイム状態の永続化。
//!
//! `~/.config/kabekami/state.toml` に保存する。ユーザー設定（config.toml）とは
//! 分離し、一時停止状態・現在の壁紙など再起動をまたいで維持したい動的状態のみを
//! 管理する。
//!
//! config.toml と別ファイルにしているのは、設定ファイル監視
//! （`watcher::spawn_config`）が config.toml のパス一致でフィルタしているため、
//! ここへの書き込みが設定リロードを誘発しないという利点もある。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct DaemonState {
    /// 自動切り替えが一時停止中か。
    #[serde(default)]
    pub paused: bool,
    /// 最後に適用した壁紙のパス。再起動時に `Scheduler::restore_current` に渡す。
    #[serde(default)]
    pub current: Option<PathBuf>,
}

impl DaemonState {
    /// 状態を読み込む。ファイル未作成・破損時はデフォルト（未停止・現在画像なし）。
    pub fn load(config_dir: &Path) -> Self {
        let path = Self::path(config_dir);
        let text = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                tracing::warn!("failed to read {}: {}, using defaults", path.display(), e);
                return Self::default();
            }
        };
        toml::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!("malformed state file {}: {}, using defaults", path.display(), e);
            Self::default()
        })
    }

    /// 状態を `atomic_write` で永続化する（電源断時に壊れた state が残らない）。
    pub fn save(&self, config_dir: &Path) -> Result<()> {
        let path = Self::path(config_dir);
        let text = toml::to_string_pretty(self).context("failed to serialize state")?;
        kabekami_common::atomic_write::atomic_write(&path, text.as_bytes())
            .with_context(|| format!("failed to write state: {}", path.display()))
    }

    fn path(config_dir: &Path) -> PathBuf {
        config_dir.join("state.toml")
    }
}

/// 現在の状態を保存し、失敗しても警告のみで処理を続行する。
///
/// 壁紙適用のたびに呼ばれるため、保存失敗で壁紙切り替え自体を止めたくない。
pub fn persist(config_dir: &Path, paused: bool, current: Option<&Path>) {
    let st = DaemonState {
        paused,
        current: current.map(|p| p.to_path_buf()),
    };
    if let Err(e) = st.save(config_dir) {
        tracing::warn!("failed to persist daemon state: {:#}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let st = DaemonState::load(dir.path());
        assert!(!st.paused);
        assert_eq!(st.current, None);
    }

    #[test]
    fn malformed_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("state.toml"), b"this is not = valid = toml").unwrap();
        let st = DaemonState::load(dir.path());
        assert!(!st.paused);
        assert_eq!(st.current, None);
    }

    #[test]
    fn roundtrip_preserves_fields() {
        let dir = tempfile::tempdir().unwrap();
        let st = DaemonState {
            paused: true,
            current: Some(PathBuf::from("/home/u/Pictures/a b.jpg")),
        };
        st.save(dir.path()).unwrap();

        let loaded = DaemonState::load(dir.path());
        assert!(loaded.paused);
        assert_eq!(loaded.current, Some(PathBuf::from("/home/u/Pictures/a b.jpg")));
    }

    #[test]
    fn roundtrip_without_current() {
        let dir = tempfile::tempdir().unwrap();
        persist(dir.path(), false, None);

        let loaded = DaemonState::load(dir.path());
        assert!(!loaded.paused);
        assert_eq!(loaded.current, None);
    }
}
