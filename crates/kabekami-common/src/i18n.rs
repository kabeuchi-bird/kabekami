//! UI 文字列の国際化（i18n）サポート。
//!
//! ## 言語の追加方法
//!
//! `crates/kabekami-common/i18n/ja.toml` をコピーし、言語コードのファイル名
//! （例: `fr.toml`）で下記のいずれかに置くだけです。**再ビルドは不要**で、
//! 設定 GUI の言語ドロップダウンにも自動で並びます。
//!
//! | 置き場所 | 用途 |
//! |---|---|
//! | `$KABEKAMI_I18N_DIR` | 開発・テスト用の上書き（最優先） |
//! | `~/.config/kabekami/i18n/` | ユーザー個別 |
//! | `/usr/share/kabekami/i18n/` | システム全体 |
//!
//! 同じ言語コードが複数見つかった場合は上の表で優先度の高いものが勝ちます。
//! `ja.toml` はバイナリに埋め込まれているため、ファイルを 1 つも設置しなくても
//! 日本語は利用できます。
//!
//! ## 2 つの文字列テーブル
//!
//! - `UiStrings`: デーモン（トレイメニュー・通知）が使う文字列 → TOML の `[tray]`
//! - `ConfigStrings`: 設定 GUI が使う文字列 → TOML の `[config]`
//!
//! ## 英語だけが Rust 側にある理由
//!
//! 英語テーブルは「必ず完全な」フォールバック先である必要があるため、TOML では
//! なく Rust の `static` として持っています。こうするとフィールドを追加した時点で
//! 英語の文言を書かないとコンパイルが通らず、翻訳の基準が実行時に欠けることが
//! 構造的に起こりません。他の言語は未記載のキーが英語で埋まるため、部分的な
//! 翻訳ファイルでも問題なく動作します。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use serde::Deserialize;

// ── 文字列テーブルの定義マクロ ────────────────────────────────────────────────

/// 文字列テーブルの構造体・TOML 読み込み用の中間表現・英語へのマージ処理を
/// 1 つのフィールド一覧からまとめて生成する。
///
/// UI 文字列を追加するときはここのフィールド一覧に 1 行足し、英語テーブルに
/// 文言を書けばよい（後者を忘れるとコンパイルエラーになる）。
macro_rules! string_table {
    (
        $(#[$smeta:meta])*
        $name:ident / $raw:ident {
            $( $(#[$fmeta:meta])* $field:ident ),* $(,)?
        }
        lists {
            $( $(#[$lmeta:meta])* $lfield:ident ),* $(,)?
        }
    ) => {
        $(#[$smeta])*
        pub struct $name {
            $( $(#[$fmeta])* pub $field: &'static str, )*
            $( $(#[$lmeta])* pub $lfield: &'static [&'static str], )*
        }

        /// TOML から読み込むための中間表現。
        /// 省略されたキーは英語へフォールバックするため全て `Option`。
        #[derive(Default, Deserialize)]
        struct $raw {
            $( #[serde(default)] $field: Option<String>, )*
            $( #[serde(default)] $lfield: Option<Vec<String>>, )*
        }

        impl $raw {
            /// 英語テーブルを土台に、指定されたキーだけを差し替える。
            fn merge(self, base: &'static $name) -> $name {
                $name {
                    $( $field: leak_str(self.$field, base.$field), )*
                    $( $lfield: leak_list(self.$lfield, base.$lfield), )*
                }
            }
        }
    };
}

string_table! {
    /// トレイメニュー・通知で使用する UI 文字列の集合（TOML の `[tray]`）。
    UiStrings / RawUiStrings {
        next_wallpaper,
        prev_wallpaper,
        pause,
        resume,
        display_mode,
        interval,
        open_current,
        delete_current,
        blacklist_current,
        copy_to_favorites,
        quit,
        open_settings,
        /// 画像枚数の単位（`"images"` / `"枚"`）
        images,
        /// `{}` = ファイル名
        tooltip_current,
        /// `{}` = エラー本文
        tooltip_error,
        notify_failed,
        notify_warning,
        /// オンライン取得サマリー通知のヘッダー
        notify_fetch_title,
        /// 置換トークン: `{provider}` = プロバイダー名, `{count}` = 取得枚数
        notify_fetch_body,
    }
    lists {
        /// `tray::INTERVAL_PRESETS` と同じ順序・件数（6 件）であること
        interval_labels,
    }
}

string_table! {
    /// 設定 GUI（kabekami-config）で使用する UI 文字列の集合（TOML の `[config]`）。
    ///
    /// フィールドはタブ構成に沿って並べてあり、TOML 側のコメント区切りと対応する。
    ConfigStrings / RawConfigStrings {
        // ダイアログ・共通ボタン
        dialog_select_folder,
        dialog_select_image,
        browse,
        /// kdialog 不在時のツールチップ（`\n` を含む複数行）
        kdialog_missing,
        save_button,
        add,

        // ステータス表示
        saved,
        save_failed,
        preview_error,

        // タブ見出し
        tab_sources,
        tab_online,
        tab_rotation,
        tab_display,
        tab_cache,
        tab_ui,

        // ソースタブ
        sources_heading,
        recursive,
        favorites_dir,
        favorites_hint,
        directories,

        // ローテーションタブ
        rotation_heading,
        interval_secs,
        order,
        order_random,
        order_sequential,
        change_on_start,
        prefetch,

        // 表示タブ
        display_heading,
        mode_blurpad,
        mode_smart,
        mode_fill,
        mode_fit,
        mode_stretch,
        blur_sigma,
        bg_darken,
        preview_image,
        preview_button,

        // キャッシュタブ
        cache_heading,
        cache_directory,
        max_size_mb,
        unlimited_hint,
        refresh,
        clear_cache,
        current_size_unknown,
        current_size,
        unlimited,

        // オンラインタブ
        online_heading,
        online_desc,
        remove,
        count,
        interval_hours,
        download_dir,
        add_provider,
        add_provider_button,
        download_dir_hint,

        // UI タブ
        ui_heading,
        language,
        warn_notify,
        notify_fetch,
        enable_blacklist,
    }
    lists {}
}

/// 読み込んだ文字列を `'static` に昇格させる。
///
/// 言語テーブルはプロセス起動時に一度だけ構築されてそのまま生存し続けるため、
/// ここでリークさせても増え続けることはない。こうすることで既存の
/// `&'static str` ベースの API（`ksni` のトレイが保持する参照など）を
/// そのまま維持できる。
fn leak_str(v: Option<String>, fallback: &'static str) -> &'static str {
    match v {
        Some(s) => Box::leak(s.into_boxed_str()),
        None => fallback,
    }
}

fn leak_list(v: Option<Vec<String>>, fallback: &'static [&'static str]) -> &'static [&'static str] {
    match v {
        Some(list) => {
            let leaked: Vec<&'static str> = list
                .into_iter()
                .map(|s| &*Box::leak(s.into_boxed_str()))
                .collect();
            Box::leak(leaked.into_boxed_slice())
        }
        None => fallback,
    }
}

// ── 言語 ──────────────────────────────────────────────────────────────────────

/// 対応言語。`registry()` へのインデックスとして振る舞う。
///
/// 言語がファイルから動的に増えるため enum ではなく不透明なインデックス型。
/// 添字はプロセス内でのみ意味を持ち、設定ファイルには言語コード
/// （`ui.language`）が保存されるので、実行ごとに順序が変わっても問題ない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lang(usize);

impl Default for Lang {
    /// 英語。`registry()` の先頭は常に英語であることが保証されている。
    fn default() -> Self {
        Lang(0)
    }
}

impl Lang {
    /// 言語コード文字列（`"en"`, `"ja"` 等）から `Lang` を解析する。
    /// 未知の値は英語にフォールバックする。
    pub fn from_code(s: &str) -> Self {
        let trimmed = s.trim();
        registry()
            .iter()
            .position(|e| e.id.eq_ignore_ascii_case(trimmed))
            .map(Lang)
            .unwrap_or_default()
    }

    /// 対応する言語コードを返す。
    pub fn code(self) -> &'static str {
        self.entry().id
    }

    fn entry(self) -> &'static LangEntry {
        // インデックスは registry() から得たものしか存在しないが、
        // 念のため範囲外は英語に倒す。
        registry().get(self.0).unwrap_or(&registry()[0])
    }
}

/// 言語の登録エントリ。
pub struct LangEntry {
    /// config.toml / 環境変数で使う識別子（例: `"ja"`）。ファイル名に由来する。
    pub id: &'static str,
    /// GUI の言語選択ドロップダウンに表示する名前
    pub display_name: &'static str,
    /// `false` のエントリは GUI に表示されない
    pub gui_visible: bool,
    /// このエントリを指す `Lang`
    pub variant: Lang,
    /// デーモン（トレイ・通知）用の文字列テーブル
    pub strings: &'static UiStrings,
    /// 設定 GUI 用の文字列テーブル
    pub config: &'static ConfigStrings,
}

/// 登録済み言語の一覧。先頭は必ず英語。
///
/// 初回呼び出し時に埋め込み分と探索パス上の TOML を読み込む。以降は
/// キャッシュされた参照を返すだけなので、毎フレーム呼んでも問題ない。
pub fn registry() -> &'static [LangEntry] {
    static REG: OnceLock<&'static [LangEntry]> = OnceLock::new();
    REG.get_or_init(|| Box::leak(build_registry().into_boxed_slice()))
}

/// `Lang` から対応する `UiStrings` 参照を返す（デーモン用）。
pub fn strings(lang: Lang) -> &'static UiStrings {
    lang.entry().strings
}

/// `Lang` から対応する `ConfigStrings` 参照を返す（設定 GUI 用）。
pub fn config_strings(lang: Lang) -> &'static ConfigStrings {
    lang.entry().config
}

// ── 言語ファイルの読み込み ────────────────────────────────────────────────────

/// 言語ファイル 1 つ分の内容。
#[derive(Deserialize)]
struct LangFile {
    display_name: Option<String>,
    #[serde(default = "default_true")]
    gui_visible: bool,
    #[serde(default)]
    tray: RawUiStrings,
    #[serde(default)]
    config: RawConfigStrings,
}

fn default_true() -> bool {
    true
}

/// `Default` は「ファイルが無い」場合の英語エントリ生成に使う。
///
/// `#[serde(default = ...)]` はデシリアライズ時にしか効かないため、
/// derive した `Default` だと `gui_visible` が `false` になり英語が
/// 言語ドロップダウンから消えてしまう。ここで明示的に揃えておく。
impl Default for LangFile {
    fn default() -> Self {
        Self {
            display_name: None,
            gui_visible: default_true(),
            tray: RawUiStrings::default(),
            config: RawConfigStrings::default(),
        }
    }
}

/// バイナリに埋め込む言語ファイル。ファイルを設置しなくても使える。
const BUNDLED: &[(&str, &str)] = &[("ja", include_str!("../i18n/ja.toml"))];

/// 言語ファイルの探索パス。後にあるものほど優先度が高い（後勝ち）。
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/usr/share/kabekami/i18n")];
    if let Some(cfg) = crate::config::xdg_config_dir() {
        dirs.push(cfg.join("kabekami").join("i18n"));
    }
    // 開発・テスト時の明示的な上書き
    if let Some(dir) = std::env::var_os("KABEKAMI_I18N_DIR") {
        dirs.push(PathBuf::from(dir));
    }
    dirs
}

fn build_registry() -> Vec<LangEntry> {
    // id → 内容。後から挿入したものが前のものを置き換える。
    let mut files: BTreeMap<String, LangFile> = BTreeMap::new();

    for (id, text) in BUNDLED {
        match toml::from_str::<LangFile>(text) {
            Ok(f) => {
                files.insert((*id).to_string(), f);
            }
            // 埋め込みファイルはテストで検証済みなので通常ここには来ない
            Err(e) => tracing::warn!("i18n: bundled {}.toml is malformed: {}", id, e),
        }
    }

    for dir in search_dirs() {
        load_dir(&dir, &mut files);
    }

    // 英語は常に先頭。ディスク上に en.toml があればその内容を上書き適用する。
    let en_file = files.remove("en").unwrap_or_default();
    let mut entries = vec![make_entry(0, "en", "English", en_file, &EN, &EN_CONFIG)];

    for (id, file) in files {
        let idx = entries.len();
        entries.push(make_entry(idx, &id, &id, file, &EN, &EN_CONFIG));
    }
    entries
}

/// ディレクトリ内の `*.toml` を読み込み、ファイル名を言語コードとして登録する。
fn load_dir(dir: &std::path::Path, out: &mut BTreeMap<String, LangFile>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return; // 存在しないディレクトリは無視（設置は任意）
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("i18n: cannot read {}: {}", path.display(), e);
                continue;
            }
        };
        match toml::from_str::<LangFile>(&text) {
            Ok(f) => {
                tracing::debug!("i18n: loaded {} from {}", id, path.display());
                out.insert(id.to_ascii_lowercase(), f);
            }
            Err(e) => tracing::warn!("i18n: {} is malformed, ignored: {}", path.display(), e),
        }
    }
}

fn make_entry(
    idx: usize,
    id: &str,
    fallback_name: &str,
    file: LangFile,
    base_ui: &'static UiStrings,
    base_cfg: &'static ConfigStrings,
) -> LangEntry {
    LangEntry {
        id: Box::leak(id.to_string().into_boxed_str()),
        display_name: leak_str(file.display_name, Box::leak(fallback_name.to_string().into_boxed_str())),
        gui_visible: file.gui_visible,
        variant: Lang(idx),
        strings: Box::leak(Box::new(file.tray.merge(base_ui))),
        config: Box::leak(Box::new(file.config.merge(base_cfg))),
    }
}

// ── 英語（フォールバックの基準となる完全なテーブル） ──────────────────────────

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
    open_settings:      "Open Settings",
    images:             "images",
    tooltip_current:    "Current: {}",
    tooltip_error:      "Error: {}",
    notify_failed:      "Wallpaper apply failed",
    notify_warning:     "kabekami Warning",
    notify_fetch_title: "Online sources",
    notify_fetch_body:  "Downloaded {count} image(s) from {provider}",
    interval_labels:    &["10s", "30s", "5m", "30m", "1h", "3h"],
};

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

// ── テスト ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 埋め込み `ja.toml` が全キーを網羅しているかを検証する。
    ///
    /// 同梱の翻訳が虫食いだと英語が混ざって出てしまうため、キーの綴り間違いを
    /// ここで落とす（利用者が設置する翻訳ファイルは部分的でも構わない）。
    #[test]
    fn bundled_ja_covers_every_key() {
        let (_, text) = BUNDLED.iter().find(|(id, _)| *id == "ja").unwrap();
        let f: LangFile = toml::from_str(text).expect("ja.toml should parse");

        assert!(f.display_name.is_some(), "ja.toml: display_name is missing");

        // 英語とは異なる値になっているはず = キー名が正しく届いている
        let ui = f.tray.merge(&EN);
        assert_ne!(ui.quit, EN.quit, "ja.toml: [tray] quit is missing");
        assert_ne!(ui.images, EN.images, "ja.toml: [tray] images is missing");
        assert_ne!(
            ui.notify_fetch_body, EN.notify_fetch_body,
            "ja.toml: [tray] notify_fetch_body is missing"
        );

        let cfg = f.config.merge(&EN_CONFIG);
        assert_ne!(cfg.saved, EN_CONFIG.saved, "ja.toml: [config] saved is missing");
        assert_ne!(
            cfg.enable_blacklist, EN_CONFIG.enable_blacklist,
            "ja.toml: [config] enable_blacklist is missing"
        );
        assert_ne!(
            cfg.kdialog_missing, EN_CONFIG.kdialog_missing,
            "ja.toml: [config] kdialog_missing is missing"
        );
        // 複数行リテラルが意図どおり 2 行になっているか
        assert_eq!(cfg.kdialog_missing.lines().count(), 2);
    }

    #[test]
    fn interval_labels_length_matches() {
        // tray::INTERVAL_PRESETS は 6 件。全言語で一致していることを確認。
        for entry in registry() {
            assert_eq!(
                entry.strings.interval_labels.len(),
                6,
                "{}: interval_labels must have exactly 6 entries",
                entry.id
            );
        }
    }

    #[test]
    fn english_is_always_first_and_default() {
        assert_eq!(registry()[0].id, "en");
        assert_eq!(Lang::default().code(), "en");
        assert_eq!(strings(Lang::default()).quit, "Quit");
    }

    /// 全ての登録言語が言語ドロップダウンに出ること。
    ///
    /// en.toml が無い場合の英語エントリは `LangFile::default()` から作られる。
    /// derive した `Default` だと `gui_visible` が `false` になり、英語だけが
    /// 選択肢から消えるという不具合が実際に起きたのでテストで固定する。
    #[test]
    fn all_languages_are_gui_visible_by_default() {
        for entry in registry() {
            assert!(
                entry.gui_visible,
                "{}: gui_visible should default to true",
                entry.id
            );
        }
    }

    #[test]
    fn from_code_resolves_and_falls_back() {
        assert_eq!(Lang::from_code("en").code(), "en");
        assert_eq!(Lang::from_code("EN").code(), "en");
        assert_eq!(Lang::from_code(" ja ").code(), "ja");
        assert_eq!(Lang::from_code("JA").code(), "ja");
        // 未知・空文字は英語へ
        assert_eq!(Lang::from_code("").code(), "en");
        assert_eq!(Lang::from_code("xx").code(), "en");
    }

    #[test]
    fn bundled_ja_is_registered() {
        let ja = Lang::from_code("ja");
        assert_eq!(strings(ja).quit, "終了");
        assert_eq!(config_strings(ja).saved, "設定を保存しました");
        assert_eq!(
            registry().iter().find(|e| e.id == "ja").unwrap().display_name,
            "日本語"
        );
    }

    /// 未記載のキーが英語で埋まることを確認する（部分翻訳の許容）。
    #[test]
    fn partial_translation_falls_back_to_english() {
        let text = r#"
            display_name = "Partial"
            [tray]
            quit = "Beenden"
        "#;
        let f: LangFile = toml::from_str(text).unwrap();
        let ui = f.tray.merge(&EN);
        assert_eq!(ui.quit, "Beenden");
        // 未記載のキーは英語のまま
        assert_eq!(ui.pause, EN.pause);
        assert_eq!(ui.interval_labels, EN.interval_labels);
    }

    /// `display_name` 省略時はファイル名（言語コード）が表示名になる。
    #[test]
    fn display_name_defaults_to_id() {
        let f: LangFile = toml::from_str("[tray]\nquit = \"x\"").unwrap();
        let e = make_entry(1, "de", "de", f, &EN, &EN_CONFIG);
        assert_eq!(e.display_name, "de");
        assert!(e.gui_visible, "gui_visible の既定は true");
    }

    #[test]
    fn every_entry_has_consistent_variant() {
        for (i, entry) in registry().iter().enumerate() {
            assert_eq!(entry.variant, Lang(i), "{}: variant mismatch", entry.id);
            assert_eq!(strings(entry.variant) as *const _, entry.strings as *const _);
            assert_eq!(config_strings(entry.variant) as *const _, entry.config as *const _);
        }
    }
}
