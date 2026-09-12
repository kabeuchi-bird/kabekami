//! UI 文字列の国際化（i18n）サポート。
//!
//! 言語ファイルの置き場所・優先順位・書き方は README の「表示言語の追加」を
//! 参照（利用者向けの説明はそちらが正）。ここでは実装側の要点だけ述べる。
//!
//! ## 構成
//!
//! - `UiStrings`: デーモン（トレイ・通知）用 → TOML の `[tray]`
//! - `ConfigStrings`: 設定 GUI 用 → TOML の `[config]`
//!
//! どちらも `string_table!` が「フィールド名 = 英語の文言」の一覧から生成する。
//! UI 文字列を足すときはその一覧に 1 行書けばよい。
//!
//! ## 英語だけが Rust 側にある理由
//!
//! 英語は「必ず完全な」フォールバック先である必要があるため、TOML ではなく
//! `static` として持つ。フィールドを足した時点で英語の文言を書かないと
//! コンパイルが通らないので、基準が実行時に欠けることが構造的に起こらない。
//! 他の言語は未記載のキーが英語で埋まるため、部分的な翻訳でも動作する。
//!
//! ## 読み込みのタイミング
//!
//! `registry()` の `OnceLock` でプロセスごとに 1 回だけ。`config.toml` と違い
//! ホットリロードしないため、言語ファイルの追加・編集には再起動が要る。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use serde::Deserialize;

// ── 文字列テーブルの定義マクロ ────────────────────────────────────────────────

/// 文字列テーブルを「フィールド名 = 英語の文言」の一覧 1 つから生成する。
///
/// 生成物は 4 つ: テーブル構造体、英語の `static`、TOML 読み込み用の中間表現、
/// そして重ね合わせ／マージ処理。UI 文字列を追加するときはこの一覧に 1 行
/// 足すだけでよく、英語の文言を書き忘れればコンパイルエラーになる。
macro_rules! string_table {
    // リスト型のフィールドを持たないテーブル用。
    (
        $(#[$smeta:meta])*
        $name:ident / $raw:ident / $en:ident {
            $( $(#[$fmeta:meta])* $field:ident = $fdefault:expr ),* $(,)?
        }
    ) => {
        string_table! {
            $(#[$smeta])*
            $name / $raw / $en { $( $(#[$fmeta])* $field = $fdefault ),* }
            lists { }
        }
    };
    (
        $(#[$smeta:meta])*
        $name:ident / $raw:ident / $en:ident {
            $( $(#[$fmeta:meta])* $field:ident = $fdefault:expr ),* $(,)?
        }
        lists {
            $( $(#[$lmeta:meta])* $lfield:ident = $ldefault:expr ),* $(,)?
        }
    ) => {
        $(#[$smeta])*
        pub struct $name {
            $( $(#[$fmeta])* pub $field: &'static str, )*
            $( $(#[$lmeta])* pub $lfield: &'static [&'static str], )*
        }

        /// 英語テーブル。フォールバックの基準であり、必ず全フィールドが埋まる。
        pub static $en: $name = $name {
            $( $field: $fdefault, )*
            $( $lfield: $ldefault, )*
        };

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

            /// 優先度の高い層の内容を重ねる（指定されたキーだけ上書き）。
            ///
            /// 置き換えではなく重ね合わせにすることで、ユーザーは気に入らない
            /// 訳語だけを数行のファイルで差し替えられ、残りは同梱の翻訳が
            /// そのまま使われる（更新にも追従する）。
            fn overlay(&mut self, other: Self) {
                $( if other.$field.is_some() { self.$field = other.$field; } )*
                $( if other.$lfield.is_some() { self.$lfield = other.$lfield; } )*
            }

            /// 未指定（= 英語にフォールバックする）キーの名前を全て返す。
            ///
            /// 同梱翻訳の網羅性テストで使う。フィールド一覧はこのマクロが
            /// 持っているため、フィールドを増やしても検査対象が自動で増える。
            #[cfg(test)]
            fn missing_keys(&self) -> Vec<&'static str> {
                let mut missing = Vec::new();
                $( if self.$field.is_none() { missing.push(stringify!($field)); } )*
                $( if self.$lfield.is_none() { missing.push(stringify!($lfield)); } )*
                missing
            }
        }
    };
}

string_table! {
    /// トレイメニュー・通知で使用する UI 文字列の集合（TOML の `[tray]`）。
    UiStrings / RawUiStrings / EN {
        next_wallpaper = "Next Wallpaper",
        prev_wallpaper = "Previous Wallpaper",
        pause = "Pause",
        resume = "Resume",
        display_mode = "Display Mode",
        interval = "Rotation Interval",
        open_current = "Open Current Wallpaper",
        delete_current = "Move to Trash",
        blacklist_current = "Never Show Again",
        copy_to_favorites = "Copy to Favorites",
        quit = "Quit",
        open_settings = "Open Settings",
        /// 画像枚数の単位（`"images"` / `"枚"`）
        images = "images",
        /// `{}` = ファイル名
        tooltip_current = "Current: {}",
        /// `{}` = エラー本文
        tooltip_error = "Error: {}",
        notify_failed = "Wallpaper apply failed",
        notify_warning = "kabekami Warning",
        /// オンライン取得サマリー通知のヘッダー
        notify_fetch_title = "Online sources",
        /// 置換トークン: `{provider}` = プロバイダー名, `{count}` = 取得枚数
        notify_fetch_body = "Downloaded {count} image(s) from {provider}",
    }
    lists {
        /// `tray::INTERVAL_PRESETS` と同じ順序・件数（6 件）であること
        interval_labels = &["10s", "30s", "5m", "30m", "1h", "3h"],
    }
}

string_table! {
    /// 設定 GUI（kabekami-config）で使用する UI 文字列の集合（TOML の `[config]`）。
    ///
    /// フィールドはタブ構成に沿って並べてあり、TOML 側のコメント区切りと対応する。
    ConfigStrings / RawConfigStrings / EN_CONFIG {
        // ダイアログ・共通ボタン
        dialog_select_folder = "Select folder",
        dialog_select_image = "Select image",
        browse = "📁 Browse…",
        /// kdialog 不在時のツールチップ（`\n` を含む複数行）
        kdialog_missing = "kdialog is not installed.\nType the path manually.",
        save_button = "💾  Save",
        add = "Add",

        // ステータス表示
        saved = "Config saved.",
        save_failed = "Save failed",
        preview_error = "Preview error",

        // タブ見出し
        tab_sources = "Sources",
        tab_online = "Online",
        tab_rotation = "Rotation",
        tab_display = "Display",
        tab_cache = "Cache",
        tab_ui = "UI",

        // ソースタブ
        sources_heading = "Sources",
        recursive = "Recursive",
        favorites_dir = "Favorites directory:",
        favorites_hint = "~/Pictures/Favorites (empty=disabled)",
        directories = "Directories:",

        // ローテーションタブ
        rotation_heading = "Rotation",
        interval_secs = "Interval (sec):",
        order = "Order:",
        order_random = "Random",
        order_sequential = "Sequential",
        change_on_start = "Change on start",
        prefetch = "Prefetch next wallpaper",

        // 表示タブ
        display_heading = "Display",
        mode_blurpad = "BlurPad (blur background + foreground)",
        mode_smart = "Smart (auto by aspect ratio)",
        mode_fill = "Fill (crop)",
        mode_fit = "Fit (letterbox)",
        mode_stretch = "Stretch",
        blur_sigma = "Blur sigma:",
        bg_darken = "BG darken:",
        preview_image = "Preview image:",
        preview_button = "▶ Preview",

        // キャッシュタブ
        cache_heading = "Cache",
        cache_directory = "Directory:",
        max_size_mb = "Max size (MB):",
        unlimited_hint = "0 = unlimited",
        refresh = "Refresh",
        clear_cache = "Clear Cache",
        current_size_unknown = "Current size: (click Refresh)",
        current_size = "Current size",
        unlimited = "unlimited",

        // オンラインタブ
        online_heading = "Online Sources",
        online_desc = "Automatically fetch wallpapers from the internet.",
        remove = "✖  Remove",
        count = "Count:",
        interval_hours = "Interval (h, 0=default):",
        download_dir = "Download dir:",
        add_provider = "Add provider:",
        add_provider_button = "Add",
        download_dir_hint = "💡 Download dir: ~/.local/share/kabekami/<provider>/",
        api_key = "API Key:",
        query = "Query:",
        subreddit = "Subreddit:",
        locale = "Locale:",
        quality = "Quality:",
        /// `{}` = プロバイダー既定の再取得間隔（時間）
        interval_default_hint = "(default: {}h)",
        /// 設定値が未指定のときに選択欄へ出す表示。既定値を示す注記にも流用する
        default_option = "(default)",
        /// `ui.language` が登録済みのどの言語にも一致しないときの表示
        unknown_language = "(unknown)",
        /// 画像ファイル選択ダイアログのフィルター名。拡張子リストはコード側が付ける
        image_filter_label = "Images",

        // UI タブ
        ui_heading = "UI Settings",
        language = "Language:",
        warn_notify = "Show warnings as desktop notifications",
        notify_fetch = "Notify when online sources finish fetching",
        enable_blacklist = "Enable \"Never Show Again\" blacklist",
    }
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

/// 文字列リストを `'static` に昇格させる。未指定なら `fallback` をそのまま返す。
/// `leak_str` と同じ理由でリークさせている（言語テーブルはプロセスと同寿命）。
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
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Lang(usize);

/// ログに出るのが `Lang(0)` ではなく言語コードになるようにする。
/// （`tracing::info!("ui language: {:?}", lang)` の可読性のため）
impl std::fmt::Debug for Lang {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

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
        Lang(resolve_code(registry(), s))
    }

    /// 対応する言語コードを返す。
    pub fn code(self) -> &'static str {
        self.entry().id
    }

    /// 対応する登録エントリ。範囲外なら英語（`registry()` の先頭）に倒す。
    fn entry(self) -> &'static LangEntry {
        // インデックスは registry() から得たものしか存在しないが、
        // 念のため範囲外は英語に倒す。
        let reg = registry();
        reg.get(self.0).unwrap_or(&reg[0])
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
    /// デーモン（トレイ・通知）用の文字列テーブル
    pub strings: &'static UiStrings,
    /// 設定 GUI 用の文字列テーブル
    pub config: &'static ConfigStrings,
}

/// 言語コードからレジストリ内の位置を引く。未知の値は英語（先頭）に倒す。
///
/// `registry()` ではなくスライスを受け取るのは、テストがプロセス共有の
/// グローバル（＝ホスト上の言語ファイルに左右される）を経由せずに
/// 解決ロジックを検証できるようにするため。
fn resolve_code(reg: &[LangEntry], code: &str) -> usize {
    reg.iter().position(|e| code_matches(e, code)).unwrap_or(0)
}

/// 言語コードの照合規則。前後の空白を無視し、大文字小文字を区別しない。
/// `Lang::from_code` と `lookup_code` で同じ規則を使うため 1 箇所に置く。
fn code_matches(entry: &LangEntry, code: &str) -> bool {
    entry.id.eq_ignore_ascii_case(code.trim())
}

/// 言語コードから登録エントリを引く。`Lang::from_code` と同じ照合規則だが、
/// 一致するものが無ければ英語に倒さず `None` を返す。
///
/// `Lang::from_code` は未知のコードを英語として扱うため、設定画面で
/// 「設定値が認識されているか」を区別するにはこちらを使う。
pub fn lookup_code(code: &str) -> Option<&'static LangEntry> {
    registry().iter().find(|e| code_matches(e, code))
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
///
/// 全フィールドが `Option`（未指定を表現できる形）なので、探索パスをまたいだ
/// 重ね合わせと、英語へのフォールバックが同じ仕組みで扱える。
#[derive(Default, Deserialize)]
struct LangFile {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    gui_visible: Option<bool>,
    #[serde(default)]
    tray: RawUiStrings,
    #[serde(default)]
    config: RawConfigStrings,
}

impl LangFile {
    /// 優先度の高いファイルの内容を重ねる。
    fn overlay(&mut self, other: Self) {
        if other.display_name.is_some() {
            self.display_name = other.display_name;
        }
        if other.gui_visible.is_some() {
            self.gui_visible = other.gui_visible;
        }
        self.tray.overlay(other.tray);
        self.config.overlay(other.config);
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

/// 既定の探索パスでレジストリを構築する。`registry()` から一度だけ呼ばれる。
fn build_registry() -> Vec<LangEntry> {
    build_registry_from(&search_dirs())
}

/// 探索パスを指定してレジストリを構築する（`build_registry` の本体）。
///
/// `registry()` はプロセスに 1 つしか作れないため、優先順位の検証が
/// できるようディレクトリを引数に取る形へ分離してある。
fn build_registry_from(dirs: &[PathBuf]) -> Vec<LangEntry> {
    // id → 内容。後から挿入したものが前のものを置き換える。
    let mut files: BTreeMap<String, LangFile> = BTreeMap::new();

    for (id, text) in BUNDLED {
        match toml::from_str::<LangFile>(text) {
            Ok(f) => {
                files.entry((*id).to_string()).or_default().overlay(f);
            }
            // 埋め込みファイルはテストで検証済みなので通常ここには来ない
            Err(e) => tracing::warn!("i18n: bundled {}.toml is malformed: {}", id, e),
        }
    }

    for dir in dirs {
        load_dir(dir, &mut files);
    }

    // 英語は常に先頭。ディスク上に en.toml があればその内容を上書き適用する。
    let mut en_file = files.remove("en").unwrap_or_default();
    en_file.display_name.get_or_insert_with(|| "English".to_string());
    let mut entries = vec![make_entry("en", en_file, &EN, &EN_CONFIG)];

    for (id, file) in files {
        entries.push(make_entry(&id, file, &EN, &EN_CONFIG));
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
        let Some(f) = crate::toml_file::load_lenient::<LangFile>(&path, "language file") else {
            continue;
        };
        tracing::debug!("i18n: loaded {} from {}", id, path.display());
        // 置き換えではなく重ね合わせ。優先度の低い層で訳されたキーは
        // 上書きされない限りそのまま残る。
        out.entry(id.to_ascii_lowercase()).or_default().overlay(f);
    }
}

/// 読み込んだ言語ファイルを 1 つの登録エントリに変換する。
///
/// 書かれていないキーは `base_ui` / `base_cfg` から埋める（部分翻訳を許すため）。
/// `interval_labels` は件数が `tray::INTERVAL_PRESETS` と一致しないと
/// メニュー描画で添字が溢れるため、ここで検証して英語に倒す。
fn make_entry(
    id: &str,
    file: LangFile,
    base_ui: &'static UiStrings,
    base_cfg: &'static ConfigStrings,
) -> LangEntry {
    let id: &'static str = Box::leak(id.to_string().into_boxed_str());
    let mut ui = file.tray.merge(base_ui);

    // `interval_labels` はトレイの切り替え間隔メニューに 1:1 で対応し、
    // 選択された添字がそのまま `tray::INTERVAL_PRESETS` の添字として使われる。
    // 件数がずれた翻訳を受け入れると、余分な項目を選んだ瞬間に範囲外アクセスに
    // なる（少なすぎる場合はプリセットが黙って消える）。件数違いは翻訳ミスと
    // みなし、このキーだけ英語に戻す。
    if ui.interval_labels.len() != base_ui.interval_labels.len() {
        tracing::warn!(
            "i18n: {}: interval_labels must have exactly {} entries (got {}), using English",
            id,
            base_ui.interval_labels.len(),
            ui.interval_labels.len()
        );
        ui.interval_labels = base_ui.interval_labels;
    }

    LangEntry {
        id,
        // display_name 未指定なら言語コードをそのまま表示名にする
        display_name: leak_str(file.display_name, id),
        gui_visible: file.gui_visible.unwrap_or(true),
        strings: Box::leak(Box::new(ui)),
        config: Box::leak(Box::new(file.config.merge(base_cfg))),
    }
}

// ── テスト ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 埋め込み `ja.toml` が **全キー** を網羅しているかを検証する。
    ///
    /// 英語は Rust 側にあるためフィールド追加時にコンパイラが漏れを止めるが、
    /// 同梱の日本語には同じ保護が無い。フィールド一覧はマクロが持っているので、
    /// `missing_keys()` で機械的に全件検査する（フィールドを増やせば
    /// 検査対象も自動で増える）。
    #[test]
    fn bundled_ja_covers_every_key() {
        let (_, text) = BUNDLED.iter().find(|(id, _)| *id == "ja").unwrap();
        let f: LangFile = toml::from_str(text).expect("ja.toml should parse");

        assert!(f.display_name.is_some(), "ja.toml: display_name is missing");
        assert_eq!(
            f.tray.missing_keys(),
            Vec::<&str>::new(),
            "ja.toml: [tray] にキーが不足している"
        );
        assert_eq!(
            f.config.missing_keys(),
            Vec::<&str>::new(),
            "ja.toml: [config] にキーが不足している"
        );

        // 複数行リテラルが意図どおり 2 行になっているか
        let cfg = f.config.merge(&EN_CONFIG);
        assert_eq!(cfg.kdialog_missing.lines().count(), 2);
    }

    /// 同梱言語の `interval_labels` は `tray::INTERVAL_PRESETS` と同じ 6 件。
    #[test]
    fn interval_labels_length_matches() {
        for entry in bundled_only() {
            assert_eq!(
                entry.strings.interval_labels.len(),
                6,
                "{}: interval_labels must have exactly 6 entries",
                entry.id
            );
        }
    }

    /// 件数の違う `interval_labels` は翻訳ミスとして英語に戻す。
    /// 受け入れるとトレイの選択時に範囲外アクセスになる。
    #[test]
    fn wrong_length_interval_labels_falls_back_to_english() {
        let user = tempfile::tempdir().unwrap();
        write_lang(
            user.path(),
            "fr",
            "[tray]\ninterval_labels = [\"a\", \"b\", \"c\", \"d\", \"e\", \"f\", \"g\"]\n",
        );
        let reg = build_registry_from(&[user.path().to_path_buf()]);
        assert_eq!(find(&reg, "fr").strings.interval_labels, EN.interval_labels);
    }

    #[test]
    fn english_is_always_first_and_default() {
        assert_eq!(bundled_only()[0].id, "en");
        assert_eq!(bundled_only()[0].strings.quit, "Quit");
        // Lang::default() はレジストリの中身に依らず先頭を指す
        assert_eq!(Lang::default(), Lang(0));
    }

    /// 全ての登録言語が言語ドロップダウンに出ること。
    ///
    /// en.toml が無い場合の英語エントリは `LangFile::default()` から作られる。
    /// derive した `Default` だと `gui_visible` が `false` になり、英語だけが
    /// 選択肢から消えるという不具合が実際に起きたのでテストで固定する。
    #[test]
    fn all_languages_are_gui_visible_by_default() {
        for entry in bundled_only() {
            assert!(
                entry.gui_visible,
                "{}: gui_visible should default to true",
                entry.id
            );
        }
    }

    #[test]
    fn from_code_resolves_and_falls_back() {
        let reg = bundled_only();
        let code = |s: &str| reg[resolve_code(&reg, s)].id;
        assert_eq!(code("en"), "en");
        assert_eq!(code("EN"), "en");
        assert_eq!(code(" ja "), "ja");
        assert_eq!(code("JA"), "ja");
        // 未知・空文字は英語（先頭）へ
        assert_eq!(code(""), "en");
        assert_eq!(code("xx"), "en");
    }

    /// 設定画面は選択中の言語名を `lookup_code` で引く。`Lang::from_code` が
    /// 正規化して受け付けるコードはこちらでも同じ言語に解決されないといけない。
    /// ずれると `ui.language = "JA"` のとき GUI は日本語なのに言語欄だけ
    /// 「不明」になる（実際にそうなっていた）。
    #[test]
    fn lookup_code_agrees_with_from_code_normalization() {
        // GUI が実際に呼ぶ公開 API を通す。日本語は埋め込み済みなので、
        // 利用者が言語ファイルを追加していてもこの判定は変わらない。
        for code in ["ja", "JA", "Ja", " ja ", "\tja\n"] {
            let hit = lookup_code(code)
                .unwrap_or_else(|| panic!("lookup_code({code:?}) が解決できていない"));
            assert_eq!(hit.id, "ja", "code {code:?}");
            assert_eq!(hit.id, Lang::from_code(code).code(), "code {code:?}");
        }
    }

    /// 未登録のコードは英語に倒さず該当なしを返す。`from_code` は英語へ倒すので、
    /// 設定画面が「認識できない設定値」を区別できるのはこの差による。
    #[test]
    fn lookup_code_has_no_match_for_unregistered_code() {
        let reg = bundled_only();
        assert!(reg.iter().all(|e| !code_matches(e, "xx")));
        assert_eq!(reg[resolve_code(&reg, "xx")].id, "en");
    }

    #[test]
    fn bundled_ja_is_registered() {
        let reg = bundled_only();
        let ja = find(&reg, "ja");
        assert_eq!(ja.strings.quit, "終了");
        assert_eq!(ja.config.saved, "設定を保存しました");
        assert_eq!(ja.display_name, "日本語");
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
        let e = make_entry("de", f, &EN, &EN_CONFIG);
        assert_eq!(e.display_name, "de");
        assert!(e.gui_visible, "gui_visible の既定は true");
    }

    /// 同梱分だけのレジストリ（ホスト上の言語ファイルを見ない）。
    ///
    /// `registry()` はグローバルかつ探索パス（`$HOME` や
    /// `/usr/share/kabekami/i18n`）を読むため、そのままテストで内容を
    /// 断定すると実行環境に左右される。内容の検証はこちらを使う。
    fn bundled_only() -> Vec<LangEntry> {
        build_registry_from(&[])
    }

    /// テスト用に言語ファイルを書き出す。
    fn write_lang(dir: &std::path::Path, id: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(format!("{id}.toml")), body).unwrap();
    }

    fn find<'a>(reg: &'a [LangEntry], id: &str) -> &'a LangEntry {
        reg.iter().find(|e| e.id == id).expect("language not found")
    }

    /// ユーザーが自分のディレクトリに置いたファイルで言語を追加できる。
    #[test]
    fn user_directory_can_add_a_language() {
        let user = tempfile::tempdir().unwrap();
        write_lang(
            user.path(),
            "fr",
            "display_name = \"Français\"\n[tray]\nquit = \"Quitter\"\n",
        );

        let reg = build_registry_from(&[user.path().to_path_buf()]);
        let fr = find(&reg, "fr");
        assert_eq!(fr.display_name, "Français");
        assert_eq!(fr.strings.quit, "Quitter");
        // 未記載のキーは英語のまま
        assert_eq!(fr.strings.pause, EN.pause);
    }

    /// ユーザーのファイルは同梱の翻訳（ja）を上書きできる。
    /// 訳語が気に入らない場合に再ビルドせず差し替えられる。
    #[test]
    fn user_directory_overrides_bundled_language() {
        let user = tempfile::tempdir().unwrap();
        write_lang(user.path(), "ja", "[tray]\nquit = \"おわり\"\n");

        let reg = build_registry_from(&[user.path().to_path_buf()]);
        let ja = find(&reg, "ja");
        assert_eq!(ja.strings.quit, "おわり", "ユーザーのファイルが勝つべき");
        // 上書きしなかったキーは同梱の日本語のまま（英語に戻らない）
        assert_eq!(ja.strings.pause, "一時停止");
    }

    /// 探索パスは後にあるものほど優先される（システム < ユーザー < 環境変数）。
    #[test]
    fn later_search_dirs_win() {
        let system = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        let env = tempfile::tempdir().unwrap();

        write_lang(system.path(), "fr", "display_name = \"System\"\n");
        write_lang(user.path(), "fr", "display_name = \"User\"\n");
        let reg = build_registry_from(&[
            system.path().to_path_buf(),
            user.path().to_path_buf(),
        ]);
        assert_eq!(find(&reg, "fr").display_name, "User");

        write_lang(env.path(), "fr", "display_name = \"Env\"\n");
        let reg = build_registry_from(&[
            system.path().to_path_buf(),
            user.path().to_path_buf(),
            env.path().to_path_buf(),
        ]);
        assert_eq!(find(&reg, "fr").display_name, "Env");
    }

    /// 英語もユーザーのファイルで上書きでき、かつ常に先頭に居続ける。
    #[test]
    fn english_can_be_overridden_but_stays_first() {
        let user = tempfile::tempdir().unwrap();
        write_lang(user.path(), "en", "[tray]\nquit = \"Exit\"\n");

        let reg = build_registry_from(&[user.path().to_path_buf()]);
        assert_eq!(reg[0].id, "en", "英語は常に先頭");
        assert_eq!(reg[0].strings.quit, "Exit");
        assert_eq!(reg[0].strings.pause, EN.pause);
    }

    /// 壊れたファイルは無視され、他の言語や英語の動作を巻き込まない。
    #[test]
    fn malformed_file_is_ignored() {
        let user = tempfile::tempdir().unwrap();
        write_lang(user.path(), "broken", "this is not = valid = toml\n");
        write_lang(user.path(), "fr", "display_name = \"Français\"\n");

        let reg = build_registry_from(&[user.path().to_path_buf()]);
        assert!(reg.iter().all(|e| e.id != "broken"), "壊れた言語は登録しない");
        assert_eq!(find(&reg, "fr").display_name, "Français");
        assert_eq!(reg[0].strings.quit, "Quit");
    }

    /// 存在しないディレクトリを指定しても失敗しない（設置は任意）。
    #[test]
    fn missing_directory_is_not_an_error() {
        let reg = build_registry_from(&[PathBuf::from("/nonexistent/kabekami/i18n")]);
        assert_eq!(reg[0].id, "en");
        assert!(reg.iter().any(|e| e.id == "ja"), "同梱の日本語は残る");
    }

    /// `resolve_code` が各エントリを自分自身の位置に解決すること。
    #[test]
    fn lookup_by_code_returns_that_entry() {
        let reg = bundled_only();
        for (i, entry) in reg.iter().enumerate() {
            assert_eq!(resolve_code(&reg, entry.id), i, "{}", entry.id);
        }
    }

    /// グローバルの `registry()` に対する最小限の健全性確認。
    ///
    /// ホスト上の言語ファイルを拾うため、内容に踏み込んだ断定はしない
    /// （AUR の check() は利用者の環境で cargo test を走らせるので、
    /// ここでホスト状態に依存すると他人のパッケージ更新を壊す）。
    #[test]
    fn global_registry_is_usable() {
        assert_eq!(registry()[0].id, "en", "先頭は必ず英語");
        assert_eq!(Lang::default().code(), "en");
        assert!(registry().iter().any(|e| e.id == "ja"), "同梱の日本語が居る");
    }
}
