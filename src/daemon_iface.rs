//! D-Bus デーモンインターフェース（サーバー側）。
//!
//! `org.kabekami.Daemon` インターフェースを `zbus` で実装する。
//! デーモン起動時に `/org/kabekami/Daemon` オブジェクトとして登録される。
//! CLI から `kabekami --next` 等のコマンドが送られると、対応するメソッドが呼ばれ
//! メインループへ `TrayCmd` を転送する。

use tokio::sync::mpsc::UnboundedSender;

use crate::tray::TrayCmd;

/// D-Bus バス名。CLI とデーモンが共有する識別子。
pub const BUS_NAME: &str = "org.kabekami.Daemon";
/// D-Bus オブジェクトパス。
pub const OBJECT_PATH: &str = "/org/kabekami/Daemon";

/// D-Bus インターフェース実装。各メソッドが TrayCmd をメインループへ転送する。
pub struct DaemonIface {
    pub tx: UnboundedSender<TrayCmd>,
}

/// `TrayCmd` を 1 つ転送するだけの D-Bus メソッド群を生成する。
macro_rules! forward_methods {
    ($( $(#[$doc:meta])* $name:ident => $cmd:ident ),* $(,)?) => {
        #[zbus::interface(name = "org.kabekami.Daemon")]
        impl DaemonIface {
            $(
                $(#[$doc])*
                async fn $name(&self) {
                    let _ = self.tx.send(TrayCmd::$cmd);
                }
            )*
        }
    };
}

forward_methods! {
    /// 次の壁紙へ切り替える。
    next => Next,
    /// 前の壁紙に戻る。
    prev => Prev,
    /// 自動切り替えを一時停止 / 再開する。
    toggle_pause => TogglePause,
    /// デーモンを終了する。
    quit => Quit,
    /// 現在の壁紙をゴミ箱に移動して次の壁紙へ進む。
    trash_current => DeleteCurrent,
    /// 現在の壁紙をお気に入りフォルダにコピーする。
    copy_to_favorites => CopyToFavorites,
    /// 現在の壁紙をブラックリストに追加して次へ進む。
    blacklist_current => BlacklistCurrent,
}
