//! デスクトップ通知の送信。
//!
//! `org.freedesktop.Notifications` D-Bus インターフェースを既存の `zbus` で呼ぶ。
//! 新規クレート不要。
//!
//! ## replaces_id による重複防止
//! 連続してエラーが起きた場合に通知をスタックさせないため、前回の通知 ID を
//! `replaces_id` として渡す。これにより通知センターには常に最新のエラーが
//! 1 件だけ表示される。

use std::collections::HashMap;

/// デスクトップ通知の状態を保持する。
///
/// `main()` で 1 インスタンスを作り、エラー／成功のたびに `error()` / `clear()` を呼ぶ。
pub struct Notifier {
    /// 前回送信した通知の ID（0 = 未送信）。replaces_id として再利用する。
    last_id: u32,
}

impl Notifier {
    pub fn new() -> Self {
        Self { last_id: 0 }
    }

    /// エラー通知を送る。
    ///
    /// 前回の通知を `replaces_id` で置き換えるため、連続エラーでも通知は 1 件に留まる。
    /// D-Bus が使えない環境では `debug` ログを出して無視する（通知はベストエフォート）。
    pub async fn error(&mut self, summary: &str, body: &str) {
        match self.send_dbus(summary, body).await {
            Ok(id) => self.last_id = id,
            Err(e) => tracing::debug!("desktop notification unavailable: {}", e),
        }
    }

    /// エラーが解消したときに呼ぶ。次回は新規通知として表示される。
    pub fn clear(&mut self) {
        self.last_id = 0;
    }

    /// `org.freedesktop.Notifications::Notify` を呼び、付与された通知 ID を返す。
    async fn send_dbus(&self, summary: &str, body: &str) -> zbus::Result<u32> {
        let conn = zbus::Connection::session().await?;

        // Notify シグネチャ:
        //   (app_name, replaces_id, app_icon, summary, body,
        //    actions, hints, expire_timeout) → notification_id
        let actions: Vec<&str> = vec![];
        let hints: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();

        let reply = conn
            .call_method(
                Some("org.freedesktop.Notifications"),
                "/org/freedesktop/Notifications",
                Some("org.freedesktop.Notifications"),
                "Notify",
                &(
                    "kabekami",   // app_name
                    self.last_id, // replaces_id (0 = 新規)
                    "dialog-error", // app_icon
                    summary,
                    body,
                    actions,
                    hints,
                    7000i32, // expire_timeout (ms)
                ),
            )
            .await?;

        reply.body().deserialize::<u32>().map_err(zbus::Error::from)
    }
}
