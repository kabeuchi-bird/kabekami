//! UI 文字列の国際化（i18n）サポート。
//!
//! ## 2 つの文字列テーブル
//!
//! - `UiStrings`: デーモン（トレイメニュー・通知）が使う文字列
//! - `ConfigStrings`: 設定 GUI（kabekami-config）が使う文字列
//!
//! どちらも「フィールド名 = キー」の名前付きテーブルで、言語ごとに 1 つの
//! `static` を持つ。使用箇所には翻訳リテラルを埋め込まず、必ずフィールド参照で
//! 取り出すこと。こうしておくと言語を増やしてもコードの変更が不要になる。
//!
//! ## 言語の追加方法
//!
//! 1. `Lang` enum に variant を追加する
//! 2. `UiStrings` と `ConfigStrings` の `static` インスタンスを作成する
//! 3. `REGISTRY` に `LangEntry` を 1 行追加する
//!
//! これだけでトレイ・通知・GUI のすべてに自動反映される。フィールドを 1 つでも
//! 書き忘れればコンパイルエラーになるため、翻訳漏れが実行時まで残ることはない。

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
        let trimmed = s.trim();
        REGISTRY
            .iter()
            .find(|e| e.id.eq_ignore_ascii_case(trimmed))
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
    /// デーモン（トレイ・通知）用の文字列テーブル
    pub strings: &'static UiStrings,
    /// 設定 GUI 用の文字列テーブル
    pub config: &'static ConfigStrings,
}

/// 登録済み言語の全一覧。
///
/// GUI は `gui_visible: true` のエントリのみを表示する。
pub static REGISTRY: &[LangEntry] = &[
    LangEntry { id: "en", display_name: "English", gui_visible: true, variant: Lang::En, strings: &EN, config: &EN_CONFIG },
    LangEntry { id: "ja", display_name: "日本語", gui_visible: true, variant: Lang::Ja, strings: &JA, config: &JA_CONFIG },
];

/// `Lang` から対応する `UiStrings` 参照を返す（デーモン用）。
pub fn strings(lang: Lang) -> &'static UiStrings {
    REGISTRY
        .iter()
        .find(|e| e.variant == lang)
        .map(|e| e.strings)
        .unwrap_or(&EN)
}

/// `Lang` から対応する `ConfigStrings` 参照を返す（設定 GUI 用）。
pub fn config_strings(lang: Lang) -> &'static ConfigStrings {
    REGISTRY
        .iter()
        .find(|e| e.variant == lang)
        .map(|e| e.config)
        .unwrap_or(&EN_CONFIG)
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

// ── 設定 GUI 用の文字列テーブル型 ─────────────────────────────────────────────

/// 設定 GUI（kabekami-config）で使用する UI 文字列の集合。
///
/// フィールドはタブ構成に沿って並べてある。将来 TOML へ切り出す場合、
/// ここのグループがそのままセクションに対応する想定。
pub struct ConfigStrings {
    // ダイアログ・共通ボタン
    pub dialog_select_folder: &'static str,
    pub dialog_select_image:  &'static str,
    pub browse:               &'static str,
    /// kdialog 不在時のツールチップ（`\n` を含む複数行）
    pub kdialog_missing:      &'static str,
    pub save_button:          &'static str,
    pub add:                  &'static str,

    // ステータス表示
    pub saved:                &'static str,
    pub save_failed:          &'static str,
    pub preview_error:        &'static str,

    // タブ見出し
    pub tab_sources:          &'static str,
    pub tab_online:           &'static str,
    pub tab_rotation:         &'static str,
    pub tab_display:          &'static str,
    pub tab_cache:            &'static str,
    pub tab_ui:               &'static str,

    // ソースタブ
    pub sources_heading:      &'static str,
    pub recursive:            &'static str,
    pub favorites_dir:        &'static str,
    pub favorites_hint:       &'static str,
    pub directories:          &'static str,

    // ローテーションタブ
    pub rotation_heading:     &'static str,
    pub interval_secs:        &'static str,
    pub order:                &'static str,
    pub order_random:         &'static str,
    pub order_sequential:     &'static str,
    pub change_on_start:      &'static str,
    pub prefetch:             &'static str,

    // 表示タブ
    pub display_heading:      &'static str,
    pub mode_blurpad:         &'static str,
    pub mode_smart:           &'static str,
    pub mode_fill:            &'static str,
    pub mode_fit:             &'static str,
    pub mode_stretch:         &'static str,
    pub blur_sigma:           &'static str,
    pub bg_darken:            &'static str,
    pub preview_image:        &'static str,
    pub preview_button:       &'static str,

    // キャッシュタブ
    pub cache_heading:        &'static str,
    pub cache_directory:      &'static str,
    pub max_size_mb:          &'static str,
    pub unlimited_hint:       &'static str,
    pub refresh:              &'static str,
    pub clear_cache:          &'static str,
    pub current_size_unknown: &'static str,
    pub current_size:         &'static str,
    pub unlimited:            &'static str,

    // オンラインタブ
    pub online_heading:       &'static str,
    pub online_desc:          &'static str,
    pub remove:               &'static str,
    pub count:                &'static str,
    pub interval_hours:       &'static str,
    pub download_dir:         &'static str,
    pub add_provider:         &'static str,
    pub add_provider_button:  &'static str,
    pub download_dir_hint:    &'static str,

    // UI タブ
    pub ui_heading:           &'static str,
    pub language:             &'static str,
    pub warn_notify:          &'static str,
    pub notify_fetch:         &'static str,
    pub enable_blacklist:     &'static str,
}

pub static EN_CONFIG: ConfigStrings = ConfigStrings {
    dialog_select_folder: "Select folder",
    dialog_select_image:  "Select image",
    browse:               "📁 Browse…",
    kdialog_missing:      "kdialog is not installed.\nType the path manually.",
    save_button:          "💾  Save",
    add:                  "Add",

    saved:                "Config saved.",
    save_failed:          "Save failed",
    preview_error:        "Preview error",

    tab_sources:          "Sources",
    tab_online:           "Online",
    tab_rotation:         "Rotation",
    tab_display:          "Display",
    tab_cache:            "Cache",
    tab_ui:               "UI",

    sources_heading:      "Sources",
    recursive:            "Recursive",
    favorites_dir:        "Favorites directory:",
    favorites_hint:       "~/Pictures/Favorites (empty=disabled)",
    directories:          "Directories:",

    rotation_heading:     "Rotation",
    interval_secs:        "Interval (sec):",
    order:                "Order:",
    order_random:         "Random",
    order_sequential:     "Sequential",
    change_on_start:      "Change on start",
    prefetch:             "Prefetch next wallpaper",

    display_heading:      "Display",
    mode_blurpad:         "BlurPad (blur background + foreground)",
    mode_smart:           "Smart (auto by aspect ratio)",
    mode_fill:            "Fill (crop)",
    mode_fit:             "Fit (letterbox)",
    mode_stretch:         "Stretch",
    blur_sigma:           "Blur sigma:",
    bg_darken:            "BG darken:",
    preview_image:        "Preview image:",
    preview_button:       "▶ Preview",

    cache_heading:        "Cache",
    cache_directory:      "Directory:",
    max_size_mb:          "Max size (MB):",
    unlimited_hint:       "0 = unlimited",
    refresh:              "Refresh",
    clear_cache:          "Clear Cache",
    current_size_unknown: "Current size: (click Refresh)",
    current_size:         "Current size",
    unlimited:            "unlimited",

    online_heading:       "Online Sources",
    online_desc:          "Automatically fetch wallpapers from the internet.",
    remove:               "✖  Remove",
    count:                "Count:",
    interval_hours:       "Interval (h, 0=default):",
    download_dir:         "Download dir:",
    add_provider:         "Add provider:",
    add_provider_button:  "Add",
    download_dir_hint:    "💡 Download dir: ~/.local/share/kabekami/<provider>/",

    ui_heading:           "UI Settings",
    language:             "Language:",
    warn_notify:          "Show warnings as desktop notifications",
    notify_fetch:         "Notify when online sources finish fetching",
    enable_blacklist:     "Enable \"Never Show Again\" blacklist",
};

pub static JA_CONFIG: ConfigStrings = ConfigStrings {
    dialog_select_folder: "フォルダを選択",
    dialog_select_image:  "画像を選択",
    browse:               "📁 参照",
    kdialog_missing:      "kdialog がインストールされていません。\nパスを直接入力してください。",
    save_button:          "💾  保存",
    add:                  "追加",

    saved:                "設定を保存しました",
    save_failed:          "保存失敗",
    preview_error:        "プレビューエラー",

    tab_sources:          "ソース",
    tab_online:           "オンライン",
    tab_rotation:         "ローテーション",
    tab_display:          "表示",
    tab_cache:            "キャッシュ",
    tab_ui:               "UI",

    sources_heading:      "壁紙ソース",
    recursive:            "サブフォルダも含める",
    favorites_dir:        "お気に入りフォルダ",
    favorites_hint:       "~/Pictures/Favorites (空欄=無効)",
    directories:          "ディレクトリ",

    rotation_heading:     "ローテーション",
    interval_secs:        "切り替え間隔 (秒):",
    order:                "順序",
    order_random:         "ランダム",
    order_sequential:     "順番",
    change_on_start:      "起動時に即切り替え",
    prefetch:             "次の壁紙を先読み",

    display_heading:      "表示モード",
    mode_blurpad:         "BlurPad (ぼかし背景＋前景)",
    mode_smart:           "Smart (アスペクト比で自動選択)",
    mode_fill:            "Fill (クロップ)",
    mode_fit:             "Fit (レターボックス)",
    mode_stretch:         "Stretch (引き伸ばし)",
    blur_sigma:           "ぼかし強度",
    bg_darken:            "背景暗さ",
    preview_image:        "プレビュー画像",
    preview_button:       "▶ プレビュー",

    cache_heading:        "キャッシュ",
    cache_directory:      "ディレクトリ",
    max_size_mb:          "最大サイズ",
    unlimited_hint:       "0 = 無制限",
    refresh:              "更新",
    clear_cache:          "クリア",
    current_size_unknown: "現在のサイズ: (更新 をクリック)",
    current_size:         "現在のサイズ",
    unlimited:            "無制限",

    online_heading:       "オンラインソース",
    online_desc:          "インターネットから壁紙を自動取得します。",
    remove:               "✖  削除",
    count:                "保持枚数",
    interval_hours:       "再取得間隔",
    download_dir:         "ダウンロード先",
    add_provider:         "プロバイダーを追加",
    add_provider_button:  "＋ 追加",
    download_dir_hint:    "💡 ダウンロード先: ~/.local/share/kabekami/<provider>/",

    ui_heading:           "UI 設定",
    language:             "表示言語",
    warn_notify:          "警告をデスクトップ通知で表示",
    notify_fetch:         "オンライン取得完了時に通知を表示",
    enable_blacklist:     "「二度と表示しない」機能を有効にする",
};

// ── テスト ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_code_en() {
        assert_eq!(Lang::from_code("en"), Lang::En);
        assert_eq!(Lang::from_code("EN"), Lang::En);
        assert_eq!(Lang::from_code(" en "), Lang::En);
    }

    #[test]
    fn from_code_ja_and_fallback() {
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
    fn config_strings_returns_correct_table() {
        assert_eq!(config_strings(Lang::Ja).saved, "設定を保存しました");
        assert_eq!(config_strings(Lang::En).saved, "Config saved.");
    }

    #[test]
    fn every_registered_language_has_both_tables() {
        // REGISTRY 経由で両テーブルが引けることを確認する。
        // 言語を追加したのに config 側のテーブルを忘れる、という漏れを防ぐ。
        for entry in REGISTRY {
            assert_eq!(
                strings(entry.variant) as *const _,
                entry.strings as *const _,
                "{}: UiStrings mismatch",
                entry.id
            );
            assert_eq!(
                config_strings(entry.variant) as *const _,
                entry.config as *const _,
                "{}: ConfigStrings mismatch",
                entry.id
            );
        }
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
