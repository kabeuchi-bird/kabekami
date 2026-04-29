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

#[zbus::interface(name = "org.kabekami.Daemon")]
impl DaemonIface {
    /// 次の壁紙へ切り替える。
    async fn next(&self) {
        let _ = self.tx.send(TrayCmd::Next);
    }

    /// 前の壁紙に戻る。
    async fn prev(&self) {
        let _ = self.tx.send(TrayCmd::Prev);
    }

    /// 自動切り替えを一時停止 / 再開する。
    async fn toggle_pause(&self) {
        let _ = self.tx.send(TrayCmd::TogglePause);
    }

    /// 設定ファイルを再読み込みする。
    async fn reload_config(&self) {
        let _ = self.tx.send(TrayCmd::ReloadConfig);
    }

    /// デーモンを終了する。
    async fn quit(&self) {
        let _ = self.tx.send(TrayCmd::Quit);
    }

    /// オンライン壁紙を今すぐ取得する（インターバル無視）。
    async fn fetch_now(&self) {
        let _ = self.tx.send(TrayCmd::FetchNow);
    }

    /// 現在の壁紙をゴミ箱に移動して次の壁紙へ進む。
    async fn trash_current(&self) {
        let _ = self.tx.send(TrayCmd::DeleteCurrent);
    }
}
