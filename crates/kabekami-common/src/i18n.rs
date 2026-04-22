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
    Kansai,
    Ohogoe,
}

impl Lang {
    /// 文字列から `Lang` を解析する。未知の値は `En` にフォールバック。
    pub fn from_str(s: &str) -> Self {
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
    /// config.toml / 環境変数で使う識別子（例: `"ja"`, `"kansai"`）
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
    LangEntry { id: "kansai", display_name: "日本語（関西弁）",  gui_visible: true, variant: Lang::Kansai, strings: &KANSAI },
    LangEntry { id: "ohogoe", display_name: "お゛っ♡", gui_visible: false, variant: Lang::Ohogoe, strings: &OHOGOE },
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
    pub next_wallpaper:  &'static str,
    pub prev_wallpaper:  &'static str,
    pub pause:           &'static str,
    pub resume:          &'static str,
    pub display_mode:    &'static str,
    pub interval:        &'static str,
    pub open_current:    &'static str,
    pub quit:            &'static str,
    /// `{}` = ファイル名
    pub tooltip_current: &'static str,
    /// `{}` = エラー本文
    pub tooltip_error:   &'static str,
    pub notify_failed:   &'static str,
    pub notify_warning:  &'static str,
    /// `tray::INTERVAL_PRESETS` と同じ長さ（6 件）であること
    pub interval_labels: &'static [&'static str],
    pub reload_config:   &'static str,
    pub open_settings:   &'static str,
    pub fetch_now:       &'static str,
    /// 画像枚数の単位（`"images"` / `"枚"`）
    pub images:          &'static str,
}

// ── 英語 ──────────────────────────────────────────────────────────────────────

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
    open_settings:   "Open Settings",
    fetch_now:       "Fetch Wallpapers Now",
    images:          "images",
};

// ── 日本語 ────────────────────────────────────────────────────────────────────

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
    open_settings:   "設定を開く",
    fetch_now:       "今すぐ取得",
    images:          "枚",
};

// ── 関西弁 ────────────────────────────────────────────────────────────────────

pub static KANSAI: UiStrings = UiStrings {
    next_wallpaper:  "次の壁紙にしたるわ",
    prev_wallpaper:  "前の壁紙に戻したるで",
    pause:           "ちょっと止めとくわ",
    resume:          "また始めよか",
    display_mode:    "見せ方",
    interval:        "切り替えの間隔",
    open_current:    "今の壁紙、開けたるで",
    quit:            "さいなら",
    tooltip_current: "今はこれや: {}",
    tooltip_error:   "あかんわ: {}",
    notify_failed:   "壁紙の設定、うまいことできへんかったわ",
    notify_warning:  "kabekamiくん何か言うとるで！",
    interval_labels: &["10秒", "30秒", "5分", "30分", "1時間", "3時間"],
    reload_config:   "設定、読み直したるわ",
    open_settings:   "設定開けるで",
    fetch_now:       "今すぐ取ってきたるわ",
    images:          "枚",
};

// ── おほ声（イースターエッグ） ───────────────────────────────────────────────

pub static OHOGOE: UiStrings = UiStrings {
    next_wallpaper:  "イグぅっ♡♡♡",
    prev_wallpaper:  "またイキますっ♡♡♡",
    pause:           "おマンコ壊れて戻らないよぉおっ♡♡♡",
    resume:          "一突き毎にアクメくるぅっ♡♡♡",
    display_mode:    "あへぇぇえっ♡♡♡中出しっ♡♡♡中出し来てりゅううっ♡♡♡",
    interval:        "おマンコ壊れるっ♡♡♡おマンコ溶けるっ♡♡♡おマンコ蒸発するぅうっ♡♡",
    open_current:    "ザーメン欲しくて発情した脳みそピースサイン出してますぅっ♡♡♡",
    quit:            "これからはご主人様だけのおマンコで生涯忠誠を誓いますぅうっ♡♡♡",
    tooltip_current: "おっほっ♡♡♡きたっ♡♡♡: {}",
    tooltip_error:   "子宮口開いてザーメン待ってるっ♡♡♡ {}",
    notify_failed:   "チンポきもちぃっ……♡",
    notify_warning:  "孕むっ♡絶対孕むっ♡♡",
    interval_labels: &["んぐっ♡", "ぢゅるっ♡", "ちゅぱっ♡", "ごきゅんっ♡", "じゅるるぅっ♡", "ぢゅぞぞぞぞぉぉっ♡♡♡"],
    reload_config:   "ご主人様の赤ちゃん産む準備できてますよぉっ♡♡♡",
    open_settings:   "私のことぉっ♡♡♡いつでもどこでもご自由にお使いくださいぃっ♡♡♡",
    fetch_now:       "おチンポ欲しさに尻尾振って媚び媚びオナホメスに躾けてくださぃっ♡♡♡",
    images:          "発",
};

// ── テスト ────────────────────────────────────────────────────────────────────

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
        assert_eq!(Lang::from_str(""), Lang::En);   // 未知 → 英語
        assert_eq!(Lang::from_str("fr"), Lang::En); // 未知 → 英語
    }

    #[test]
    fn from_str_kansai() {
        assert_eq!(Lang::from_str("kansai"), Lang::Kansai);
        assert_eq!(Lang::from_str("KANSAI"), Lang::Kansai);
        assert_eq!(Lang::from_str(" kansai "), Lang::Kansai);
    }

    #[test]
    fn from_str_ohogoe() {
        assert_eq!(Lang::from_str("ohogoe"), Lang::Ohogoe);
        assert_eq!(Lang::from_str("OHOGOE"), Lang::Ohogoe);
        assert_eq!(Lang::from_str(" ohogoe "), Lang::Ohogoe);
    }


    #[test]
    fn strings_returns_correct_table() {
        assert_eq!(strings(Lang::Ja).quit, "終了");
        assert_eq!(strings(Lang::En).quit, "Quit");
        assert_eq!(strings(Lang::Kansai).quit, "さいなら");
        assert_eq!(strings(Lang::Ohogoe).quit, "これからはご主人様だけのおマンコで生涯忠誠を誓いますぅうっ♡♡♡");
    }

    #[test]
    fn interval_labels_length_matches() {
        // tray::INTERVAL_PRESETS は 6 件。全言語で一致していることを確認。
        assert_eq!(EN.interval_labels.len(), 6);
        assert_eq!(JA.interval_labels.len(), 6);
        assert_eq!(KANSAI.interval_labels.len(), 6);
        assert_eq!(OHOGOE.interval_labels.len(), 6);
    }

    #[test]
    fn registry_covers_all_variants() {
        // 全 variant が REGISTRY に登録されていることを確認
        for variant in [Lang::En, Lang::Ja, Lang::Kansai, Lang::Ohogoe] {
            assert!(
                REGISTRY.iter().any(|e| e.variant == variant),
                "{:?} not found in REGISTRY",
                variant
            );
        }
    }
}
