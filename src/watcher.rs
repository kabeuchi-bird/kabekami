//! ソースディレクトリの変更監視。
//!
//! `notify` クレートでソースディレクトリを監視し、画像ファイルの
//! 追加・削除イベントを tokio チャンネル経由でメインループに通知する。
//!
//! ## 使い方
//!
//! ```rust,ignore
//! let (watcher, mut rx) = watcher::spawn(&config.sources.directories, config.sources.recursive)?;
//! // watcher を drop するまで監視が続く。
//!
//! // メインループで select! に組み込む:
//! tokio::select! {
//!     Some(event) = rx.recv() => { ... }
//! }
//! ```

use std::path::PathBuf;

use notify::{
    event::{ModifyKind, RenameMode},
    EventKind, RecursiveMode, Watcher,
};
use tokio::sync::mpsc::{self, UnboundedReceiver};

/// スケジューラに送信するディレクトリ変更イベント。
#[derive(Debug)]
pub enum WatchEvent {
    /// 画像ファイルが追加された（または名前変更で出現した）
    Added(PathBuf),
    /// 画像ファイルが削除された（または名前変更で消えた）
    Removed(PathBuf),
}

/// ディレクトリ監視ハンドル。`Drop` するまで監視が継続する。
pub struct DirWatcher {
    /// 内部の `notify` ウォッチャー。フィールドとして保持することで
    /// `DirWatcher` がドロップされるまで監視が続く。
    _inner: notify::RecommendedWatcher,
}

/// ディレクトリ監視を開始する。
///
/// `dirs` 内の各ディレクトリを `recursive` に応じた深さで監視する。
/// エラー時（`notify` 初期化失敗、ディレクトリ追加失敗）は警告を出して `None` を返し、
/// アプリはウォッチャーなしで動作を継続する。
pub fn spawn(
    dirs: &[PathBuf],
    recursive: bool,
) -> Option<(DirWatcher, UnboundedReceiver<WatchEvent>)> {
    let (tx, rx) = mpsc::unbounded_channel::<WatchEvent>();

    let mut watcher =
        match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            let (kind, paths) = (event.kind, event.paths);

            for path in paths {
                if !crate::scanner::is_image(&path) {
                    continue;
                }
                let msg = match kind {
                    EventKind::Create(_) => WatchEvent::Added(path),
                    EventKind::Remove(_) => WatchEvent::Removed(path),
                    EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
                        WatchEvent::Added(path)
                    }
                    EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                        WatchEvent::Removed(path)
                    }
                    _ => continue,
                };
                let _ = tx.send(msg);
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!("failed to create file watcher: {}", e);
                return None;
            }
        };

    let mode = if recursive {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    };

    let mut any_ok = false;
    for dir in dirs {
        match watcher.watch(dir, mode) {
            Ok(()) => {
                tracing::info!("watching {} for changes", dir.display());
                any_ok = true;
            }
            Err(e) => {
                tracing::warn!("failed to watch {}: {}", dir.display(), e);
            }
        }
    }

    if !any_ok {
        tracing::warn!("no directories could be watched; running without file watcher");
        return None;
    }

    Some((DirWatcher { _inner: watcher }, rx))
}

