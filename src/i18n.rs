//! UI 文字列の国際化（i18n）サポート。設計書 §13 Phase 3.1 に準拠。
//!
//! 新クレート不要。静的文字列テーブルによるシンプルな実装。
//!
//! ## 言語解決の優先順位
//!
//! 1. 環境変数 `KABEKAMI_LANG`（`"en"` / `"ja"`）
//! 2. `config.toml` の `[ui] language`
//! 3. デフォルト: `"en"`（英語）

/// 対応言語。デフォルトは英語。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    Ja,
    #[default]
    En,
}

impl Lang {
    /// 文字列から `Lang` を解析する。未知の値は `En` にフォールバック。
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "ja" => Self::Ja,
            _ => Self::En,
        }
    }
}

/// トレイメニュー・通知で使用する UI 文字列の集合。
///
/// すべてのフィールドは `'static` 参照なのでゼロコストで渡せる。
/// フォーマット文字列（`tooltip_current` 等）は `{}` を 1 個含む。
pub struct UiStrings {
    /// トレイメニュー: 次の壁紙
    pub next_wallpaper: &'static str,
    /// トレイメニュー: 前の壁紙
    pub prev_wallpaper: &'static str,
    /// トレイメニュー: 一時停止
    pub pause: &'static str,
    /// トレイメニュー: 再開
    pub resume: &'static str,
    /// トレイメニュー: 表示モード（サブメニュー見出し）
    pub display_mode: &'static str,
    /// トレイメニュー: 切り替え間隔（サブメニュー見出し）
    pub interval: &'static str,
    /// トレイメニュー: 現在の壁紙を開く
    pub open_current: &'static str,
    /// トレイメニュー: 終了
    pub quit: &'static str,
    /// ツールチップ: 現在の壁紙名を表示（`{}` = ファイル名）
    pub tooltip_current: &'static str,
    /// ツールチップ: エラーメッセージを表示（`{}` = エラー本文）
    pub tooltip_error: &'static str,
    /// デスクトップ通知のサマリー（壁紙設定失敗時）
    pub notify_failed: &'static str,
    /// デスクトップ通知のサマリー（WARN 通知時）
    pub notify_warning: &'static str,
    /// 切り替え間隔プリセットのラベル一覧。`tray::INTERVAL_PRESETS` と並列。
    pub interval_labels: &'static [&'static str],
    /// トレイメニュー: 設定を再読み込み
    pub reload_config: &'static str,
}

/// 日本語文字列テーブル。
pub static JA: UiStrings = UiStrings {
    next_wallpaper:  "次の壁紙",
    prev_wallpaper:  "前の壁紙",
    pause:           "一時停止",
    resume:          "再開",
    display_mode:    "表示モード",
    interval:        "切り替え間隔",
    open_current:    "現在の壁紙を開く",
    quit:            "終了",
    tooltip_current: "現在: {}",
    tooltip_error:   "エラー: {}",
    notify_failed:   "壁紙の設定に失敗しました",
    notify_warning:  "kabekami 警告",
    interval_labels: &["10秒", "30秒", "5分", "30分", "1時間", "3時間"],
    reload_config:   "設定を再読み込み",
};

/// 英語文字列テーブル。
pub static EN: UiStrings = UiStrings {
    next_wallpaper:  "Next Wallpaper",
    prev_wallpaper:  "Previous Wallpaper",
    pause:           "Pause",
    resume:          "Resume",
    display_mode:    "Display Mode",
    interval:        "Rotation Interval",
    open_current:    "Open Current Wallpaper",
    quit:            "Quit",
    tooltip_current: "Current: {}",
    tooltip_error:   "Error: {}",
    notify_failed:   "Wallpaper apply failed",
    notify_warning:  "kabekami Warning",
    interval_labels: &["10s", "30s", "5m", "30m", "1h", "3h"],
    reload_config:   "Reload Config",
};

/// `Lang` から対応する `UiStrings` 参照を返す。
pub fn strings(lang: Lang) -> &'static UiStrings {
    match lang {
        Lang::Ja => &JA,
        Lang::En => &EN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_en() {
        assert_eq!(Lang::from_str("en"), Lang::En);
        assert_eq!(Lang::from_str("EN"), Lang::En);
        assert_eq!(Lang::from_str(" en "), Lang::En);
    }

    #[test]
    fn from_str_ja_and_fallback() {
        assert_eq!(Lang::from_str("ja"), Lang::Ja);
        assert_eq!(Lang::from_str("JA"), Lang::Ja);
        assert_eq!(Lang::from_str(" ja "), Lang::Ja);
        assert_eq!(Lang::from_str(""), Lang::En);  // 未知 → 英語
        assert_eq!(Lang::from_str("fr"), Lang::En); // 未知 → 英語
    }

    #[test]
    fn strings_returns_correct_table() {
        assert_eq!(strings(Lang::Ja).quit, "終了");
        assert_eq!(strings(Lang::En).quit, "Quit");
    }

    #[test]
    fn interval_labels_length_matches() {
        // tray::INTERVAL_PRESETS は 6 件。両言語で一致していることを確認。
        assert_eq!(JA.interval_labels.len(), 6);
        assert_eq!(EN.interval_labels.len(), 6);
    }
}
