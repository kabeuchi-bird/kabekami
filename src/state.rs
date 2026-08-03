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

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
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
    fn save(&self, config_dir: &Path) -> Result<()> {
        let path = Self::path(config_dir);
        let text = toml::to_string_pretty(self).context("failed to serialize state")?;
        kabekami_common::atomic_write::atomic_write(&path, text.as_bytes())
            .with_context(|| format!("failed to write state: {}", path.display()))
    }

    fn path(config_dir: &Path) -> PathBuf {
        config_dir.join("state.toml")
    }
}

/// `state.toml` への書き込み担当。「いつ・どこへ書くか」をここ 1 箇所に集約する。
///
/// 直近に書き出した内容を保持し、同じ値なら書き込みを省く。Plasma 再起動・画面構成
/// 変更では同じ壁紙を再適用するため、この抑止がないと内容の変わらない
/// `atomic_write`（fsync 2 回）を繰り返すことになる。
pub struct StateWriter {
    dir: PathBuf,
    last: DaemonState,
}

impl StateWriter {
    /// 起動時に読み込んだ状態を「書き込み済みの内容」として初期化する。
    pub fn new(dir: PathBuf, initial: DaemonState) -> Self {
        Self { dir, last: initial }
    }

    /// 状態が前回と変わっていれば保存する。
    ///
    /// 壁紙適用のたびに呼ばれるため、保存失敗は警告に留めて処理を続行する
    /// （state の書き込み失敗で壁紙切り替え自体を止めたくない）。
    pub fn persist(&mut self, paused: bool, current: Option<&Path>) {
        let next = DaemonState {
            paused,
            current: current.map(|p| p.to_path_buf()),
        };
        if next == self.last {
            return;
        }
        if let Err(e) = next.save(&self.dir) {
            tracing::warn!("failed to persist daemon state: {:#}", e);
            return;
        }
        self.last = next;
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
        let img = PathBuf::from("/home/u/Pictures/a b.jpg");
        let mut w = StateWriter::new(dir.path().to_path_buf(), DaemonState::default());
        w.persist(true, Some(&img));

        let loaded = DaemonState::load(dir.path());
        assert!(loaded.paused);
        assert_eq!(loaded.current, Some(img));
    }

    #[test]
    fn roundtrip_without_current() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = StateWriter::new(
            dir.path().to_path_buf(),
            DaemonState { paused: true, current: None },
        );
        w.persist(false, None);

        let loaded = DaemonState::load(dir.path());
        assert!(!loaded.paused);
        assert_eq!(loaded.current, None);
    }

    #[test]
    fn unchanged_state_is_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let img = PathBuf::from("/home/u/Pictures/a.jpg");
        let mut w = StateWriter::new(dir.path().to_path_buf(), DaemonState::default());

        w.persist(false, Some(&img));
        let first = std::fs::metadata(dir.path().join("state.toml")).unwrap().modified().unwrap();

        // 同じ値での再呼び出しはファイルに触れない（Plasma 再起動・画面構成変更の経路）
        w.persist(false, Some(&img));
        let second = std::fs::metadata(dir.path().join("state.toml")).unwrap().modified().unwrap();
        assert_eq!(first, second, "identical state should not rewrite the file");
    }
}
