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
    // login1 はシステムバス上にある
    let sys_conn = match zbus::Connection::system().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("session watcher: system bus unavailable ({})", e);
            return;
        }
    };

    let session_conn = match zbus::Connection::session().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("session watcher: session bus unavailable ({})", e);
            return;
        }
    };

    let login1 = match Login1ManagerProxy::new(&sys_conn).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("session watcher: login1 proxy unavailable ({})", e);
            return;
        }
    };

    let shutdown_stream = match login1.receive_prepare_for_shutdown().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("session watcher: PrepareForShutdown signal unavailable ({})", e);
            return;
        }
    };

    let dbus_proxy = match FreedesktopDBusProxy::new(&session_conn).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("session watcher: DBus proxy unavailable ({})", e);
            return;
        }
    };

    let name_changed_stream = match dbus_proxy.receive_name_owner_changed().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("session watcher: NameOwnerChanged signal unavailable ({})", e);
            return;
        }
    };

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
}
