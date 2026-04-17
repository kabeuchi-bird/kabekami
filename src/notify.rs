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

use anyhow::Result;

use crate::i18n::Lang;

/// デスクトップ通知の状態を保持する。
///
/// `main()` で 1 インスタンスを作り、エラー／成功のたびに `error()` / `clear()` を呼ぶ。
pub struct Notifier {
    /// 前回送信したエラー通知の ID（0 = 未送信）。replaces_id として再利用する。
    last_id: u32,
    /// エラー通知のサマリー文字列。
    summary: &'static str,
    /// 前回送信した警告通知の ID（0 = 未送信）。
    warn_last_id: u32,
    /// 警告通知のサマリー文字列。
    warn_summary: &'static str,
    /// D-Bus セッション接続。確立済みの接続を保持して再利用する。
    /// 利用不可の場合は `None`（通知はベストエフォートで送信を試みる）。
    conn: Option<zbus::Connection>,
}

impl Notifier {
    /// 言語設定に応じたサマリー文字列でノーティファイアを初期化する。
    pub async fn new(lang: Lang) -> Self {
        let strings = crate::i18n::strings(lang);
        let conn = zbus::Connection::session().await.ok();
        Self {
            last_id: 0,
            summary: strings.notify_failed,
            warn_last_id: 0,
            warn_summary: strings.notify_warning,
            conn,
        }
    }

    /// エラー通知を送る。
    ///
    /// 前回の通知を `replaces_id` で置き換えるため、連続エラーでも通知は 1 件に留まる。
    /// D-Bus が使えない環境では `debug` ログを出して無視する（通知はベストエフォート）。
    pub async fn error(&mut self, body: &str) {
        match self.send_dbus("dialog-error", self.summary, body, 7000, self.last_id).await {
            Ok(id) => self.last_id = id,
            Err(e) => tracing::debug!("desktop notification unavailable: {}", e),
        }
    }

    /// エラーが解消したときに呼ぶ。次回は新規通知として表示される。
    pub fn clear(&mut self) {
        self.last_id = 0;
    }

    /// WARN レベルのログを通知として表示する。連続警告は 1 件に集約される。
    pub async fn warn(&mut self, body: &str) {
        match self.send_dbus("dialog-warning", self.warn_summary, body, 5000, self.warn_last_id).await {
            Ok(id) => self.warn_last_id = id,
            Err(e) => tracing::debug!("desktop notification unavailable: {}", e),
        }
    }

    /// `org.freedesktop.Notifications::Notify` を呼び、付与された通知 ID を返す。
    ///
    /// 接続が未確立の場合は再接続を試みる。それでも失敗した場合はエラーを返す。
    async fn send_dbus(
        &mut self,
        icon: &str,
        summary: &str,
        body: &str,
        expire_ms: i32,
        replaces_id: u32,
    ) -> Result<u32> {
        // 接続がなければ再接続を試みる
        if self.conn.is_none() {
            self.conn = zbus::Connection::session().await.ok();
        }
        let conn = self.conn.as_ref()
            .ok_or_else(|| anyhow::anyhow!("D-Bus session unavailable"))?;

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
                    "kabekami", // app_name
                    replaces_id,
                    icon,
                    summary,
                    body,
                    actions,
                    hints,
                    expire_ms,
                ),
            )
            .await?;

        Ok(reply.body().deserialize::<u32>()?)
    }
}
