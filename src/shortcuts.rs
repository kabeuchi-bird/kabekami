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
use kabekami_common::i18n::Lang;

const COMPONENT: &str = "kabekami";

#[zbus::proxy(
    interface = "org.kde.KGlobalAccel",
    default_service = "org.kde.kglobalaccel",
    default_path = "/kglobalaccel"
)]
trait KGlobalAccel {
    /// アクションを登録する（デフォルトキーは割り当てない）。
    ///
    /// KDE は camelCase の D-Bus メソッド名を使うため `name = "doRegister"` で
    /// オーバーライドする（zbus 既定の PascalCase だと `DoRegister` になり
    /// `UnknownMethod` エラーになる）。
    #[zbus(name = "doRegister")]
    fn do_register(&self, action_id: Vec<String>) -> zbus::Result<()>;

    /// アクションのキーを設定し、現在のキーを返す。`flags` は KGlobalAccel の
    /// `SetShortcutFlag`（`SET_PRESENT` を参照）。
    #[zbus(name = "setShortcut")]
    fn set_shortcut(&self, action_id: Vec<String>, keys: Vec<i32>, flags: u32) -> zbus::Result<Vec<i32>>;

    /// アクションを「提供中でない」に戻し、キーの grab を外す。
    #[zbus(name = "setInactive")]
    fn set_inactive(&self, action_id: Vec<String>) -> zbus::Result<()>;
}

/// `SetShortcutFlag::SetPresent`。アクションを「提供中」にしてキーを grab させる。
/// `NoAutoloading` を付けないので、ユーザーが割り当て済みのキーはそのまま使われ、
/// 渡した空のキーは未割り当ての初回だけ効く（= デフォルトキーなし）。
const SET_PRESENT: u32 = 2;

#[zbus::proxy(
    interface = "org.kde.kglobalaccel.Component",
    default_service = "org.kde.kglobalaccel",
    default_path = "/component/kabekami"
)]
trait KGlobalAccelComponent {
    /// KDE はシグナル名も camelCase（`globalShortcutPressed`）。
    #[zbus(signal, name = "globalShortcutPressed")]
    fn global_shortcut_pressed(
        &self,
        component: String,
        shortcut: String,
        timestamp: i64,
    ) -> zbus::Result<()>;
}

/// present にした登録済みのアクション。終了時に `deactivate` で grab を外す。
///
/// kglobalaccel はクライアントの切断を見ないので、外さないと kabekami の終了後も
/// 割り当てたキーが押さえられたままになる（KDE の KGlobalAccel ライブラリも
/// アクションの破棄時に `setInactive` を呼ぶ）。異常終了時は呼べないが、次の起動で
/// また present にするので食い違いは残らない。
pub struct Shortcuts {
    accel: KGlobalAccelProxy<'static>,
    action_ids: Vec<Vec<String>>,
}

impl Shortcuts {
    pub async fn deactivate(self) {
        for id in self.action_ids {
            if let Err(e) = self.accel.set_inactive(id.clone()).await {
                tracing::warn!("shortcuts: failed to deactivate {}: {}", id[1], e);
            }
        }
    }
}

/// グローバルショートカット監視をバックグラウンドタスクとして起動する。
///
/// kglobalaccel が利用できない環境では警告を出してサイレントに無効化され、`None` を返す。
/// ショートカットが押されると対応する `TrayCmd` を `tx` に送信する。
pub async fn spawn_shortcut_watcher(tx: UnboundedSender<TrayCmd>, lang: Lang) -> Option<Shortcuts> {
    let conn = match zbus::Connection::session().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("shortcuts: session bus unavailable ({})", e);
            return None;
        }
    };

    let accel = match KGlobalAccelProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("shortcuts: kglobalaccel proxy unavailable ({})", e);
            return None;
        }
    };

    // 登録するアクション: (アクション ID, 表示名)。表示名はトレイと同じ文字列。
    // KDE は登録時の名前を覚えるので、言語の変更は次回のデーモン起動時に反映される。
    let s = kabekami_common::i18n::strings(lang);
    let actions = [
        ("next_wallpaper",    s.next_wallpaper),
        ("prev_wallpaper",    s.prev_wallpaper),
        ("toggle_pause",      s.toggle_pause),
        ("trash_current",     s.delete_current),
        ("blacklist_current", s.blacklist_current),
    ];
    let mut shortcuts = Shortcuts { accel, action_ids: Vec::new() };
    for (action_id, display_name) in actions {
        let id = vec![
            COMPONENT.to_string(),
            action_id.to_string(),
            COMPONENT.to_string(),
            display_name.to_string(),
        ];
        // `doRegister` だけではアクションが present にならず、キーを割り当てても
        // grab されない（設定にも保存されない）。`setShortcut` まで呼んで有効にする。
        let accel = &shortcuts.accel;
        let registered = async {
            accel.do_register(id.clone()).await?;
            accel.set_shortcut(id.clone(), Vec::new(), SET_PRESENT).await
        };
        match registered.await {
            Ok(_) => shortcuts.action_ids.push(id),
            Err(e) => tracing::warn!("shortcuts: failed to register {}: {}", action_id, e),
        }
    }

    let component = match KGlobalAccelComponentProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("shortcuts: component proxy unavailable ({})", e);
            shortcuts.deactivate().await;
            return None;
        }
    };

    let mut stream = match component.receive_global_shortcut_pressed().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("shortcuts: signal stream unavailable ({})", e);
            shortcuts.deactivate().await;
            return None;
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
    Some(shortcuts)
}
