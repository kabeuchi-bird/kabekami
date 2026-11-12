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
/// - シャットダウン・再起動の開始 → `TrayCmd::Quit`（`PrepareForShutdown` は
///   ログアウトでは来ない）
/// - Plasma 再起動検知 → `TrayCmd::PlasmaRestarted`
///
/// 2 つは別のバス（システム／セッション）なので独立に起動する。片方のバスが
/// 使えなくても、もう片方の検知は動く。使えない側は警告を出して無効になる。
pub async fn spawn_session_watcher(tx: UnboundedSender<TrayCmd>) {
    if let Err(e) = spawn_shutdown_watcher(tx.clone()).await {
        tracing::warn!("session watcher: shutdown detection unavailable ({})", e);
    }
    if let Err(e) = spawn_plasma_watcher(tx).await {
        tracing::warn!("session watcher: Plasma restart detection unavailable ({})", e);
    }
}

async fn spawn_shutdown_watcher(tx: UnboundedSender<TrayCmd>) -> zbus::Result<()> {
    // login1 はシステムバス上にある
    let conn = zbus::Connection::system().await?;
    let mut stream = Login1ManagerProxy::new(&conn).await?.receive_prepare_for_shutdown().await?;
    tokio::spawn(async move {
        while let Some(signal) = stream.next().await {
            if signal.args().is_ok_and(|args| *args.start()) {
                tracing::info!("session watcher: PrepareForShutdown(true)");
                let _ = tx.send(TrayCmd::Quit);
                break;
            }
        }
    });
    Ok(())
}

async fn spawn_plasma_watcher(tx: UnboundedSender<TrayCmd>) -> zbus::Result<()> {
    let conn = zbus::Connection::session().await?;
    let mut stream = FreedesktopDBusProxy::new(&conn).await?.receive_name_owner_changed().await?;
    tokio::spawn(async move {
        while let Some(signal) = stream.next().await {
            let Ok(args) = signal.args() else { continue };
            if args.name() == "org.kde.plasmashell" && !args.new_owner().is_empty() {
                tracing::info!("session watcher: plasmashell restarted");
                let _ = tx.send(TrayCmd::PlasmaRestarted);
            }
        }
    });
    Ok(())
}
