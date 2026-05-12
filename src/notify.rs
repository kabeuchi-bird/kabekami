//! デスクトップ通知の送信。
//!
//! `org.freedesktop.Notifications` D-Bus インターフェースを既存の `zbus` で呼ぶ。
//! 新規クレート不要。
//!
//! ## replaces_id による重複防止
//! 連続してエラーが起きた場合に通知をスタックさせないため、前回の通知 ID を
//! `replaces_id` として渡す。これにより通知センターには常に最新のエラーが
//! 1 件だけ表示される。
//!
//! ## KDE 通知ヒント
//! エラー通知には以下の KDE 拡張ヒントを付与する:
//! - `resident = true` : 自動消去せず通知センターに残す
//! - `category = "device.error"` : 通知カテゴリー（KDE 通知フィルターで活用可能）
//! - `x-kde-origin-url` : 問題のあったファイルの URL（ファイルマネージャー連携）

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use zbus::zvariant::{OwnedValue, Value};

use kabekami_common::i18n::Lang;

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
    /// `origin_path` が指定された場合、`x-kde-origin-url` ヒントを付与する。
    /// `resident = true` により通知は自動消去されない。
    /// 前回の通知を `replaces_id` で置き換えるため、連続エラーでも通知は 1 件に留まる。
    pub async fn error(&mut self, body: &str, origin_path: Option<&Path>) {
        let url = origin_path.map(|p| format!("file://{}", p.display()));
        let mut hints: HashMap<String, OwnedValue> = HashMap::new();
        insert_hint(&mut hints, "resident", Value::Bool(true));
        insert_hint(&mut hints, "category", Value::Str("device.error".into()));
        if let Some(ref u) = url {
            insert_hint(&mut hints, "x-kde-origin-url", Value::Str(u.as_str().into()));
        }
        match self.send_dbus("dialog-error", self.summary, body, -1, self.last_id, hints).await {
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
        let mut hints: HashMap<String, OwnedValue> = HashMap::new();
        insert_hint(&mut hints, "category", Value::Str("device.warning".into()));
        match self.send_dbus("dialog-warning", self.warn_summary, body, 5000, self.warn_last_id, hints).await {
            Ok(id) => self.warn_last_id = id,
            Err(e) => tracing::debug!("desktop notification unavailable: {}", e),
        }
    }

    /// 情報通知を送る（5 秒で自動消去・連続通知は集約しない）。
    /// オンライン取得完了サマリーなどの軽い通知に使う。
    pub async fn info(&mut self, summary: &str, body: &str) {
        // `replaces_id = 0` で常に新規通知。ID は捨てる（連続通知を集約しないため）。
        let hints: HashMap<String, OwnedValue> = HashMap::new();
        if let Err(e) = self
            .send_dbus("preferences-desktop-wallpaper", summary, body, 5000, 0, hints)
            .await
        {
            tracing::debug!("desktop notification unavailable: {}", e);
        }
    }

    /// `org.freedesktop.Notifications::Notify` を呼び、付与された通知 ID を返す。
    async fn send_dbus(
        &mut self,
        icon: &str,
        summary: &str,
        body: &str,
        expire_ms: i32,
        replaces_id: u32,
        hints: HashMap<String, OwnedValue>,
    ) -> Result<u32> {
        if self.conn.is_none() {
            self.conn = zbus::Connection::session().await.ok();
        }
        let conn = self.conn.as_ref()
            .ok_or_else(|| anyhow::anyhow!("D-Bus session unavailable"))?;

        let actions: Vec<&str> = vec![];

        let reply = conn
            .call_method(
                Some("org.freedesktop.Notifications"),
                "/org/freedesktop/Notifications",
                Some("org.freedesktop.Notifications"),
                "Notify",
                &(
                    "kabekami",
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

fn insert_hint(hints: &mut HashMap<String, OwnedValue>, key: &str, value: Value<'_>) {
    if let Ok(owned) = OwnedValue::try_from(value) {
        hints.insert(key.to_string(), owned);
    }
}
