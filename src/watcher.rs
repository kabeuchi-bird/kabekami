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

use std::path::{Path, PathBuf};

use notify::{
    event::{ModifyKind, RenameMode},
    EventKind, RecursiveMode, Watcher,
};
use tokio::sync::mpsc::{self, Receiver, UnboundedReceiver};

/// ディレクトリ監視のチャンネル容量。大量のファイル操作（コピー等）で
/// 一時的にバーストしてもメインタスクが処理しきれるよう余裕をもたせた値。
/// 溢れた場合は最古のイベントが落ちる（次の取りこぼしは起動時のスキャンで吸収）。
const WATCH_QUEUE_CAPACITY: usize = 256;

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
    /// 対象ディレクトリすべての登録に成功したか。
    complete: bool,
}

impl DirWatcher {
    /// 対象ディレクトリすべてを監視できているか。
    ///
    /// `false` のときは一部のディレクトリの登録に失敗しており、そこでの追加・削除は
    /// 一切届かない。呼び出し側が再スキャンと登録の再試行を判断するために使う。
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
pub fn spawn(
    dirs: &[PathBuf],
    recursive: bool,
) -> (Receiver<WatchEvent>, Option<DirWatcher>) {
    let (tx, rx) = mpsc::channel::<WatchEvent>(WATCH_QUEUE_CAPACITY);

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
                // 同期コールバックなのでブロックしない try_send。
                // 溢れた場合はイベントを落とす（バックプレッシャより取りこぼし優先）。
                if let Err(mpsc::error::TrySendError::Full(_)) = tx.try_send(msg) {
                    tracing::debug!("watcher channel full; dropping event");
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

    // 一部でも登録に失敗していれば「監視できている」と言ってはいけない。
    // そのディレクトリの追加・削除は届かず、呼び出し側が再スキャンで補う必要がある。
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
/// 100ms スロットルおよび `ReloadConfig` ハンドラの冪等性で吸収する。
pub fn spawn_config(config_path: &Path) -> (UnboundedReceiver<()>, Option<DirWatcher>) {
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
    // 対象は 1 ディレクトリだけなので、ここに来た時点で部分失敗はない
    (rx, Some(DirWatcher { _inner: watcher, complete: true }))
}

