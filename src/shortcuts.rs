//! KDE グローバルショートカット登録・監視。
//!
//! `org.kde.KGlobalAccel` D-Bus サービスにアクションを登録し、
//! ショートカットが押されたときに `TrayCmd` をメインループへ転送する。
//!
//! ## ユーザー設定
//! デフォルトキーは割り当てない（空）。
//! `システム設定 → ショートカット → kabekami` から各アクションに
//! 好みのキーを割り当てる。

use futures_util::StreamExt as _;
use tokio::sync::mpsc::UnboundedSender;

use crate::tray::TrayCmd;

const COMPONENT: &str = "kabekami";

#[zbus::proxy(
    interface = "org.kde.KGlobalAccel",
    default_service = "org.kde.kglobalaccel",
    default_path = "/kglobalaccel"
)]
trait KGlobalAccel {
    /// アクションを登録し、デフォルトキーを設定する。
    /// `keys` に空配列を渡すとデフォルトなし（ユーザーが手動設定）。
    fn set_shortcut_keys(
        &self,
        action_id: Vec<String>,
        keys: Vec<Vec<u32>>,
        flags: u32,
    ) -> zbus::Result<bool>;
}

#[zbus::proxy(
    interface = "org.kde.kglobalaccel.Component",
    default_service = "org.kde.kglobalaccel",
    default_path = "/component/kabekami"
)]
trait KGlobalAccelComponent {
    #[zbus(signal)]
    fn global_shortcut_pressed(
        &self,
        component: String,
        shortcut: String,
        timestamp: i64,
    ) -> zbus::Result<()>;
}

/// 登録するアクション: (アクション ID, 表示名)
const ACTIONS: &[(&str, &str)] = &[
    ("next_wallpaper",     "Next Wallpaper"),
    ("prev_wallpaper",     "Previous Wallpaper"),
    ("toggle_pause",       "Pause / Resume"),
    ("trash_current",      "Move to Trash"),
    ("blacklist_current",  "Never Show Again"),
];

/// Start a background task that registers and monitors KDE global shortcuts.
///
/// When shortcuts for the application are activated, the corresponding `TrayCmd` variants are sent through `tx`. If the session D-Bus or required KDE proxies/signals are unavailable, the watcher logs a warning and disables itself without panicking.
///
/// # Examples
///
/// ```
/// # use tokio::sync::mpsc::unbounded_channel;
/// # use kabekami::shortcuts::spawn_shortcut_watcher;
/// # use kabekami::tray::TrayCmd;
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let (tx, _rx) = unbounded_channel::<TrayCmd>();
/// // Spawns a background task that will send `TrayCmd` to `tx` when shortcuts are pressed.
/// spawn_shortcut_watcher(tx).await;
/// # });
/// ```
pub async fn spawn_shortcut_watcher(tx: UnboundedSender<TrayCmd>) {
    let conn = match zbus::Connection::session().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("shortcuts: session bus unavailable ({})", e);
            return;
        }
    };

    let accel = match KGlobalAccelProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("shortcuts: kglobalaccel proxy unavailable ({})", e);
            return;
        }
    };

    for (action_id, display_name) in ACTIONS {
        let id = vec![
            COMPONENT.to_string(),
            action_id.to_string(),
            COMPONENT.to_string(),
            display_name.to_string(),
        ];
        if let Err(e) = accel.set_shortcut_keys(id, vec![], 0).await {
            tracing::warn!("shortcuts: failed to register {}: {}", action_id, e);
        }
    }

    let component = match KGlobalAccelComponentProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("shortcuts: component proxy unavailable ({})", e);
            return;
        }
    };

    let mut stream = match component.receive_global_shortcut_pressed().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("shortcuts: signal stream unavailable ({})", e);
            return;
        }
    };

    tracing::info!(
        "global shortcuts registered \
         (configure in System Settings → Shortcuts → kabekami)"
    );

    tokio::spawn(async move {
        while let Some(signal) = stream.next().await {
            let Ok(args) = signal.args() else { continue };
            if args.component() != COMPONENT {
                continue;
            }
            let cmd = match args.shortcut().as_str() {
                "next_wallpaper"    => TrayCmd::Next,
                "prev_wallpaper"    => TrayCmd::Prev,
                "toggle_pause"      => TrayCmd::TogglePause,
                "trash_current"     => TrayCmd::DeleteCurrent,
                "blacklist_current" => TrayCmd::BlacklistCurrent,
                _                   => continue,
            };
            let _ = tx.send(cmd);
        }
    });
}
