//! ソースディレクトリの変更監視。
//!
//! `notify` クレートでソースディレクトリを監視し、画像ファイルの
//! 追加・削除イベントを tokio チャンネル経由でメインループに通知する。
//!
//! ## 使い方
//!
//! ```rust,ignore
//! let (mut rx, watcher) = watcher::spawn(&dirs, recursive, rescan.clone());
//! // watcher を drop するまで監視が続く。
//!
//! // メインループで select! に組み込む:
//! tokio::select! {
//!     Some(event) = rx.recv() => { ... }
//! }
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use notify::{
    event::{ModifyKind, RenameMode},
    EventKind, RecursiveMode, Watcher,
};
use tokio::sync::mpsc::{self, Receiver, UnboundedReceiver};
use tokio::sync::Notify;

/// ディレクトリ監視のチャンネル容量。大量のファイル操作（コピー等）で
/// 一時的にバーストしてもメインタスクが処理しきれるよう余裕をもたせた値。
/// 溢れた場合はイベントを落とし、`rescan` で全体の再走査を要求する。
const WATCH_QUEUE_CAPACITY: usize = 256;

/// スケジューラに送信するディレクトリ変更イベント。
#[derive(Debug)]
pub enum WatchEvent {
    /// 画像ファイルが追加された（または名前変更で出現した）
    Added(PathBuf),
    /// パスが削除された（または名前変更で消えた）。ディレクトリのこともあるので、
    /// 受け取った側はこのパス自身と配下の画像をすべて外す。
    Removed(PathBuf),
}

/// ディレクトリ監視ハンドル。`Drop` するまで監視が継続する。
pub struct DirWatcher {
    /// 内部の `notify` ウォッチャー。フィールドとして保持することで
    /// `DirWatcher` がドロップされるまで監視が続く。
    _inner: notify::RecommendedWatcher,
    /// 対象ディレクトリすべての登録に成功したか。
    complete: bool,
}

impl DirWatcher {
    /// 対象ディレクトリすべてを監視できているか。
    ///
    /// `false` なら一部の登録に失敗しており、そこでの追加・削除は届かない。
    /// 呼び出し側が再スキャンと登録の再試行を判断するために使う。
    pub fn is_complete(&self) -> bool {
        self.complete
    }
}

/// ディレクトリ監視を開始する。
///
/// `dirs` 内の各ディレクトリを `recursive` に応じた深さで監視する。
///
/// エラー時（`notify` 初期化失敗、ディレクトリ追加失敗）はハンドルが `None` に
/// なるが、受信端は常に返す。その場合は送信端が落ちた「閉じたチャンネル」なので、
/// `Some(ev) = rx.recv()` パターンが一致せず `select!` の該当 arm が無害に
/// 無効化される。呼び出し側は縮退時の受信端を自分で用意しなくてよい。
///
/// イベントを取りこぼしたとき（キューが溢れた、`notify` が再走査を求めた）は
/// `rescan` に通知する。`Notify` は待ち手が居なくても許可を 1 つ溜めるので、
/// メインループがどの時点で待ち始めても要求は失われない。
pub fn spawn(
    dirs: &[PathBuf],
    recursive: bool,
    rescan: Arc<Notify>,
) -> (Receiver<WatchEvent>, Option<DirWatcher>) {
    let (tx, rx) = mpsc::channel::<WatchEvent>(WATCH_QUEUE_CAPACITY);

    let mut watcher =
        match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            if event.need_rescan() {
                rescan.notify_one();
            }
            for path in event.paths {
                for msg in events_for(event.kind, path, recursive) {
                    // 同期コールバックなのでブロックしない try_send。溢れたら
                    // イベントを落とし、代わりに全体の再走査を要求する（#61）。
                    if let Err(mpsc::error::TrySendError::Full(_)) = tx.try_send(msg) {
                        tracing::debug!("watcher channel full; requesting rescan");
                        rescan.notify_one();
                        return;
                    }
                }
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!("failed to create file watcher: {}", e);
                return (rx, None);
            }
        };

    let mode = if recursive {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    };

    let mut ok_count = 0usize;
    for dir in dirs {
        match watcher.watch(dir, mode) {
            Ok(()) => {
                tracing::info!("watching {} for changes", dir.display());
                ok_count += 1;
            }
            Err(e) => {
                tracing::warn!("failed to watch {}: {}", dir.display(), e);
            }
        }
    }

    if ok_count == 0 {
        tracing::warn!("no directories could be watched; running without file watcher");
        return (rx, None);
    }

    // 一部でも失敗していれば「監視できている」と言ってはいけない
    // （そのディレクトリの追加・削除は届かず、呼び出し側が再スキャンで補う）
    let complete = ok_count == dirs.len();
    if !complete {
        tracing::warn!(
            "watching {} of {} directories; the rest rely on rescans",
            ok_count, dirs.len(),
        );
    }

    (rx, Some(DirWatcher { _inner: watcher, complete }))
}

/// 設定ファイル（`~/.config/kabekami/config.toml`）の変更を監視する。
///
/// エディタや設定 GUI による上書き保存を検知するために、ファイル単体ではなく
/// 親ディレクトリを監視する（atomic-write 系エディタはファイルを置換するため
/// ファイル直接の watch だと取りこぼすことがある）。
///
/// イベントは内容なし `()` のチャンネルで通知する。バーストはメインループ側の
/// 100ms スロットルおよびリロード処理の冪等性で吸収する。
/// `notify` のイベント 1 件（パス 1 つ分）を `WatchEvent` に変換する。
///
/// - 画像の作成・移入は `Added`。
/// - ディレクトリの作成・移入は、`recursive` なら配下を走査して画像ごとに `Added`
///   （移入したディレクトリは中身のイベントが来ないため。#61）。
/// - 削除・移出はパスの種類を問わず `Removed`（消えた後なのでディレクトリか
///   判別できない。受け取った側が配下ごと外す）。
fn events_for(kind: EventKind, path: PathBuf, recursive: bool) -> Vec<WatchEvent> {
    let appeared = matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(RenameMode::To))
    );
    let vanished = matches!(
        kind,
        EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(RenameMode::From))
    );
    if vanished {
        vec![WatchEvent::Removed(path)]
    } else if appeared && crate::scanner::is_image(&path) {
        vec![WatchEvent::Added(path)]
    } else if appeared && recursive && path.is_dir() {
        crate::scanner::scan(&[path], true)
            .unwrap_or_default()
            .into_iter()
            .map(WatchEvent::Added)
            .collect()
    } else {
        Vec::new()
    }
}

pub fn spawn_config(config_path: &Path) -> (UnboundedReceiver<()>, Option<notify::RecommendedWatcher>) {
    // `spawn` と同じく、失敗時も閉じた受信端を返す。
    let (tx, rx) = mpsc::unbounded_channel::<()>();

    let Some(parent) = config_path.parent().map(|p| p.to_path_buf()) else {
        tracing::warn!("config path has no parent dir: {}", config_path.display());
        return (rx, None);
    };
    if let Err(e) = std::fs::create_dir_all(&parent) {
        tracing::warn!("failed to create config dir {}: {}", parent.display(), e);
        return (rx, None);
    }

    let target = config_path.to_path_buf();

    let mut watcher = match notify::recommended_watcher(
        move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            // 書き込み・置換に関連するイベントのみ
            if !matches!(
                event.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            ) {
                return;
            }
            if event.paths.iter().any(|p| p == &target) {
                let _ = tx.send(());
            }
        },
    ) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("failed to create config watcher: {}", e);
            return (rx, None);
        }
    };

    if let Err(e) = watcher.watch(&parent, RecursiveMode::NonRecursive) {
        tracing::warn!(
            "failed to watch config dir {}: {}",
            parent.display(),
            e
        );
        return (rx, None);
    }

    tracing::info!(
        "watching {} for config changes",
        config_path.display()
    );
    (rx, Some(watcher))
}


#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, RemoveKind};

    fn added(events: Vec<WatchEvent>) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = events
            .into_iter()
            .map(|e| match e {
                WatchEvent::Added(p) => p,
                WatchEvent::Removed(p) => panic!("unexpected Removed({})", p.display()),
            })
            .collect();
        v.sort();
        v
    }

    /// 移入したディレクトリは中身のイベントが来ないので、配下を走査して拾う（#61）。
    #[test]
    fn moved_in_directory_adds_its_images_when_recursive() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("album");
        std::fs::create_dir_all(sub.join("nested")).unwrap();
        std::fs::write(sub.join("a.jpg"), b"").unwrap();
        std::fs::write(sub.join("nested/b.png"), b"").unwrap();
        std::fs::write(sub.join("notes.txt"), b"").unwrap();
        let moved_in = EventKind::Modify(ModifyKind::Name(RenameMode::To));

        assert_eq!(
            added(events_for(moved_in, sub.clone(), true)),
            vec![sub.join("a.jpg"), sub.join("nested/b.png")]
        );
        assert!(events_for(moved_in, sub, false).is_empty(), "非再帰なら配下は対象外");
    }

    /// 消えたパスは種類を問わず `Removed`（ディレクトリかどうかはもう判別できない）。
    #[test]
    fn vanished_paths_are_removed_even_if_not_images() {
        let gone = PathBuf::from("/pics/album");
        let ev = events_for(EventKind::Remove(RemoveKind::Any), gone.clone(), true);
        assert!(matches!(ev.as_slice(), [WatchEvent::Removed(p)] if *p == gone));
    }

    #[test]
    fn non_image_files_are_ignored_on_create() {
        let ev = events_for(EventKind::Create(CreateKind::File), PathBuf::from("/pics/a.txt"), true);
        assert!(ev.is_empty());
    }
}
