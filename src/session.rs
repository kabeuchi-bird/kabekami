//! KDE セッション管理との連携。
//!
//! - `org.freedesktop.login1.Manager::PrepareForShutdown` シグナルで
//!   シャットダウン前に `TrayCmd::Quit` を送信し、グレースフルに終了する。
//! - `org.freedesktop.DBus::NameOwnerChanged` を監視して
//!   `org.kde.plasmashell` の再起動を検知し、`TrayCmd::PlasmaRestarted` を送信する。

use futures_util::StreamExt as _;
use tokio::sync::mpsc::UnboundedSender;

use crate::tray::TrayCmd;

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Login1Manager {
    #[zbus(signal)]
    fn prepare_for_shutdown(&self, start: bool) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.DBus",
    default_service = "org.freedesktop.DBus",
    default_path = "/org/freedesktop/DBus"
)]
trait FreedesktopDBus {
    #[zbus(signal)]
    fn name_owner_changed(
        &self,
        name: String,
        old_owner: String,
        new_owner: String,
    ) -> zbus::Result<()>;
}

/// セッション管理ウォッチャーをバックグラウンドタスクとして起動する。
///
/// - ログアウト/シャットダウン開始 → `TrayCmd::Quit`
/// - Plasma 再起動検知 → `TrayCmd::PlasmaRestarted`
///
/// D-Bus が利用できない環境では警告を出してサイレントに無効化される。
pub async fn spawn_session_watcher(tx: UnboundedSender<TrayCmd>) {
    if let Err(e) = try_spawn(tx).await {
        tracing::warn!("session watcher: unavailable ({})", e);
    }
}

async fn try_spawn(tx: UnboundedSender<TrayCmd>) -> zbus::Result<()> {
    // login1 はシステムバス上にある
    let sys_conn = zbus::Connection::system().await?;
    let session_conn = zbus::Connection::session().await?;
    let shutdown_stream = Login1ManagerProxy::new(&sys_conn)
        .await?
        .receive_prepare_for_shutdown()
        .await?;
    let name_changed_stream = FreedesktopDBusProxy::new(&session_conn)
        .await?
        .receive_name_owner_changed()
        .await?;

    tracing::info!("session watcher active (login1 + NameOwnerChanged)");

    tokio::spawn(async move {
        let mut shutdown_stream = shutdown_stream;
        let mut name_changed_stream = name_changed_stream;

        loop {
            tokio::select! {
                Some(signal) = shutdown_stream.next() => {
                    if let Ok(args) = signal.args() {
                        if *args.start() {
                            tracing::info!("session watcher: PrepareForShutdown(true)");
                            let _ = tx.send(TrayCmd::Quit);
                            break;
                        }
                    }
                }
                Some(signal) = name_changed_stream.next() => {
                    if let Ok(args) = signal.args() {
                        if args.name() == "org.kde.plasmashell" && !args.new_owner().is_empty() {
                            tracing::info!("session watcher: plasmashell restarted");
                            let _ = tx.send(TrayCmd::PlasmaRestarted);
                        }
                    }
                }
                else => break,
            }
        }
    });
    Ok(())
}
