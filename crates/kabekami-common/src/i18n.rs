//! UI 文字列の国際化（i18n）サポート。
//!
//! ## 言語の追加方法
//!
//! 1. `Lang` enum に variant を追加する
//! 2. `UiStrings` の `static` インスタンスを作成する
//! 3. `REGISTRY` に `LangEntry` を 1 行追加する
//!
//! これだけでトレイ・通知・GUI のすべてに自動反映される。

/// 対応言語。デフォルトは英語。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    En,
    Ja,
}

impl Lang {
    /// 言語コード文字列（"en", "ja" 等）から `Lang` を解析する。
    /// 未知の値は `En` にフォールバック。
    ///
    /// `FromStr` トレイトとは別で lossy な独自パーサ（`Result` を返さない）。
    pub fn from_code(s: &str) -> Self {
        let key = s.trim().to_ascii_lowercase();
        REGISTRY
            .iter()
            .find(|e| e.id == key.as_str())
            .map(|e| e.variant)
            .unwrap_or_default()
    }
}

/// 言語の登録エントリ。`REGISTRY` スライスの要素。
pub struct LangEntry {
    /// config.toml / 環境変数で使う識別子（例: `"ja"`）
    pub id: &'static str,
    /// GUI の言語選択ドロップダウンに表示する名前
    pub display_name: &'static str,
    /// `false` のエントリは GUI に表示されない（イースターエッグ扱い）
    pub gui_visible: bool,
    /// 対応する enum variant
    pub variant: Lang,
    /// 対応する文字列テーブル
    pub strings: &'static UiStrings,
}

/// 登録済み言語の全一覧。
///
/// GUI は `gui_visible: true` のエントリのみを表示する。
pub static REGISTRY: &[LangEntry] = &[
    LangEntry { id: "en", display_name: "English", gui_visible: true, variant: Lang::En, strings: &EN },
    LangEntry { id: "ja", display_name: "日本語", gui_visible: true, variant: Lang::Ja, strings: &JA },
];

/// `Lang` から対応する `UiStrings` 参照を返す。
pub fn strings(lang: Lang) -> &'static UiStrings {
    REGISTRY
        .iter()
        .find(|e| e.variant == lang)
        .map(|e| e.strings)
        .unwrap_or(&EN)
}

// ── 文字列テーブル型 ──────────────────────────────────────────────────────────

/// トレイメニュー・通知で使用する UI 文字列の集合。
///
/// すべてのフィールドは `'static` 参照なのでゼロコストで渡せる。
/// フォーマット文字列（`tooltip_current` 等）は `{}` を 1 個含む。
pub struct UiStrings {
    pub next_wallpaper:     &'static str,
    pub prev_wallpaper:     &'static str,
    pub pause:              &'static str,
    pub resume:             &'static str,
    pub display_mode:       &'static str,
    pub interval:           &'static str,
    pub open_current:       &'static str,
    pub delete_current:     &'static str,
    pub blacklist_current:  &'static str,
    pub copy_to_favorites:  &'static str,
    pub quit:               &'static str,
    /// `{}` = ファイル名
    pub tooltip_current:    &'static str,
    /// `{}` = エラー本文
    pub tooltip_error:      &'static str,
    pub notify_failed:      &'static str,
    pub notify_warning:     &'static str,
    /// オンライン取得サマリー通知のヘッダー
    pub notify_fetch_title: &'static str,
    /// オンライン取得サマリー通知の本文。
    /// 置換トークン: `{provider}` = プロバイダー名, `{count}` = 取得枚数
    pub notify_fetch_body:  &'static str,
    /// `tray::INTERVAL_PRESETS` と同じ長さ（6 件）であること
    pub interval_labels:    &'static [&'static str],
    pub open_settings:      &'static str,
    /// 画像枚数の単位（`"images"` / `"枚"`）
    pub images:             &'static str,
}

// ── 英語 ──────────────────────────────────────────────────────────────────────

pub static EN: UiStrings = UiStrings {
    next_wallpaper:     "Next Wallpaper",
    prev_wallpaper:     "Previous Wallpaper",
    pause:              "Pause",
    resume:             "Resume",
    display_mode:       "Display Mode",
    interval:           "Rotation Interval",
    open_current:       "Open Current Wallpaper",
    delete_current:     "Move to Trash",
    blacklist_current:  "Never Show Again",
    copy_to_favorites:  "Copy to Favorites",
    quit:               "Quit",
    tooltip_current:    "Current: {}",
    tooltip_error:      "Error: {}",
    notify_failed:      "Wallpaper apply failed",
    notify_warning:     "kabekami Warning",
    notify_fetch_title: "Online sources",
    notify_fetch_body:  "Downloaded {count} image(s) from {provider}",
    interval_labels:    &["10s", "30s", "5m", "30m", "1h", "3h"],
    open_settings:      "Open Settings",
    images:             "images",
};

// ── 日本語 ────────────────────────────────────────────────────────────────────

pub static JA: UiStrings = UiStrings {
    next_wallpaper:     "次の壁紙",
    prev_wallpaper:     "前の壁紙",
    pause:              "一時停止",
    resume:             "再開",
    display_mode:       "表示モード",
    interval:           "切り替え間隔",
    open_current:       "現在の壁紙を開く",
    delete_current:     "ゴミ箱に移動",
    blacklist_current:  "二度と表示しない",
    copy_to_favorites:  "お気に入りに追加",
    quit:               "終了",
    tooltip_current:    "現在: {}",
    tooltip_error:      "エラー: {}",
    notify_failed:      "壁紙の設定に失敗しました",
    notify_warning:     "kabekami 警告",
    notify_fetch_title: "オンラインソース",
    notify_fetch_body:  "{provider} から {count} 枚の画像をダウンロードしました",
    interval_labels:    &["10秒", "30秒", "5分", "30分", "1時間", "3時間"],
    open_settings:      "設定を開く",
    images:             "枚",
};

// ── テスト ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_en() {
        assert_eq!(Lang::from_code("en"), Lang::En);
        assert_eq!(Lang::from_code("EN"), Lang::En);
        assert_eq!(Lang::from_code(" en "), Lang::En);
    }

    #[test]
    fn from_str_ja_and_fallback() {
        assert_eq!(Lang::from_code("ja"), Lang::Ja);
        assert_eq!(Lang::from_code("JA"), Lang::Ja);
        assert_eq!(Lang::from_code(" ja "), Lang::Ja);
        assert_eq!(Lang::from_code(""), Lang::En);   // 未知 → 英語
        assert_eq!(Lang::from_code("fr"), Lang::En); // 未知 → 英語
    }

    #[test]
    fn strings_returns_correct_table() {
        assert_eq!(strings(Lang::Ja).quit, "終了");
        assert_eq!(strings(Lang::En).quit, "Quit");
    }

    #[test]
    fn interval_labels_length_matches() {
        // tray::INTERVAL_PRESETS は 6 件。全言語で一致していることを確認。
        assert_eq!(EN.interval_labels.len(), 6);
        assert_eq!(JA.interval_labels.len(), 6);
    }

    #[test]
    fn registry_covers_all_variants() {
        // 全 variant が REGISTRY に登録されていることを確認
        for variant in [Lang::En, Lang::Ja] {
            assert!(
                REGISTRY.iter().any(|e| e.variant == variant),
                "{:?} not found in REGISTRY",
                variant
            );
        }
    }
}
