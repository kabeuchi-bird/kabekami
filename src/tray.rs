//! システムトレイアイコンとコンテキストメニュー。
//!
//! `ksni` クレートの SNI (StatusNotifierItem) プロトコルを使用する。
//! KDE Plasma は SNI をネイティブサポートしているため、
//! Qt/cxx-qt なしで軽量に実装できる。
//!
//! ## 通信方式
//! - トレイ → メインループ: `tokio::sync::mpsc::UnboundedSender<TrayCmd>`
//!   メニュー項目のコールバックからチャンネルに送信する。
//! - メインループ → トレイ: `ksni::Handle::update()` で状態を更新する。
//!   壁紙切り替えのたびに `current_name` 等を反映する。

use tokio::sync::mpsc::UnboundedSender;

use crate::config::DisplayMode;
use kabekami_common::i18n::{Lang, UiStrings};

/// メインループに送るトレイコマンド。
#[derive(Debug, Clone)]
pub enum TrayCmd {
    /// 次の壁紙へ切り替え
    Next,
    /// 前の壁紙に戻る
    Prev,
    /// 一時停止 / 再開のトグル
    TogglePause,
    /// 表示モードを変更
    SetMode(DisplayMode),
    /// 切り替え間隔を変更（秒）
    SetInterval(u64),
    /// 現在の壁紙ファイルを xdg-open で開く
    OpenCurrent,
    /// 現在の壁紙ファイルをソースフォルダから削除する
    DeleteCurrent,
    /// 現在の壁紙を二度と表示しないリストに追加する
    BlacklistCurrent,
    /// 現在の壁紙をお気に入りフォルダにコピーする
    CopyToFavorites,
    /// 設定ファイルを再読み込みする
    ReloadConfig,
    /// 設定 GUI を開く
    OpenSettings,
    /// オンライン壁紙を今すぐ取得（インターバル無視）
    FetchNow,
    /// アプリ終了
    Quit,
    /// KDE Plasma が再起動した（壁紙を再適用する）
    PlasmaRestarted,
}

/// 切り替え間隔プリセット（秒）。ラベル表示は `i18n::UiStrings::interval_labels` を使用する。
pub const INTERVAL_PRESETS: &[u64] = &[10, 30, 300, 1800, 3600, 10800];

/// トレイアイコンの表示状態。メインループが `Handle::update()` で書き込み、
/// `menu()` が読み出してメニューを組み立てる。
pub struct KabekamiTray {
    pub notifier: UnboundedSender<TrayCmd>,
    /// 現在一時停止中か
    pub paused: bool,
    /// 現在の表示モード（ラジオボタン選択表示に使う）
    pub mode: DisplayMode,
    /// 現在の切り替え間隔（秒）
    pub interval_secs: u64,
    /// 現在の壁紙のファイル名（トレイのツールチップに使う）
    pub current_name: String,
    /// 直近のエラーメッセージ。`None` = 正常動作中。
    pub last_error: Option<String>,
    /// ソース画像の総数（ツールチップに表示）。
    pub image_count: usize,
    /// UI 文字列テーブル（言語設定に応じて初期化）。
    pub strings: &'static UiStrings,
    /// お気に入りフォルダが設定されているか（メニュー項目の有効/無効に使う）。
    pub has_favorites_dir: bool,
    /// ブラックリスト機能が有効か（config.ui.enable_blacklist）。
    pub blacklist_enabled: bool,
}

impl KabekamiTray {
    fn tray_item(label: &str, icon: &str, enabled: bool, cmd: TrayCmd) -> ksni::MenuItem<Self> {
        use ksni::menu::StandardItem;
        StandardItem {
            label: label.into(),
            icon_name: icon.into(),
            enabled,
            activate: Box::new(move |this: &mut Self| {
                let _ = this.notifier.send(cmd.clone());
            }),
            ..Default::default()
        }
        .into()
    }
}

impl ksni::Tray for KabekamiTray {
    fn id(&self) -> String {
        env!("CARGO_PKG_NAME").into()
    }

    fn icon_name(&self) -> String {
        if self.last_error.is_some() {
            "dialog-error".into()
        } else {
            "preferences-desktop-wallpaper".into()
        }
    }

    fn title(&self) -> String {
        "kabekami".into()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "kabekami".into(),
            description: match &self.last_error {
                Some(e) => self.strings.tooltip_error.replacen("{}", e, 1),
                None if self.current_name.is_empty() => String::new(),
                None if self.image_count > 0 => format!(
                    "{} ({} {})",
                    self.strings.tooltip_current.replacen("{}", &self.current_name, 1),
                    self.image_count,
                    self.strings.images,
                ),
                None => self.strings.tooltip_current.replacen("{}", &self.current_name, 1),
            },
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;

        // 表示モードのラジオボタン選択インデックス
        const MODES: &[(DisplayMode, &str)] = &[
            (DisplayMode::Fill, "Fill"),
            (DisplayMode::Fit, "Fit"),
            (DisplayMode::Stretch, "Stretch"),
            (DisplayMode::BlurPad, "BlurPad"),
            (DisplayMode::Smart, "Smart"),
        ];
        let mode_selected = MODES
            .iter()
            .position(|(m, _)| *m == self.mode)
            .unwrap_or(3);

        // 切り替え間隔のラジオボタン選択インデックス
        let interval_selected = INTERVAL_PRESETS
            .iter()
            .position(|&s| s == self.interval_secs)
            .unwrap_or(usize::MAX); // プリセット外の場合は全ボタン非選択

        vec![
            Self::tray_item(self.strings.next_wallpaper, "", true, TrayCmd::Next),
            Self::tray_item(self.strings.prev_wallpaper, "", true, TrayCmd::Prev),
            MenuItem::Separator,
            // ⏸ 一時停止 / ▶ 再開
            StandardItem {
                label: if self.paused {
                    self.strings.resume.into()
                } else {
                    self.strings.pause.into()
                },
                icon_name: if self.paused {
                    "media-playback-start".into()
                } else {
                    "media-playback-pause".into()
                },
                activate: Box::new(|this: &mut Self| {
                    // 楽観的にローカル状態を更新しておく（Handle::update で正式反映される）
                    this.paused = !this.paused;
                    let _ = this.notifier.send(TrayCmd::TogglePause);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            // 表示モード / Display Mode サブメニュー
            SubMenu {
                label: self.strings.display_mode.into(),
                submenu: vec![RadioGroup {
                    selected: mode_selected,
                    select: Box::new(|this: &mut Self, idx| {
                        let mode = MODES[idx].0;
                        this.mode = mode;
                        let _ = this.notifier.send(TrayCmd::SetMode(mode));
                    }),
                    options: MODES
                        .iter()
                        .map(|(_, label)| RadioItem {
                            label: label.to_string(),
                            ..Default::default()
                        })
                        .collect(),
                }
                .into()],
                ..Default::default()
            }
            .into(),
            // 切り替え間隔 / Rotation Interval サブメニュー
            SubMenu {
                label: self.strings.interval.into(),
                submenu: vec![RadioGroup {
                    selected: interval_selected,
                    select: Box::new(|this: &mut Self, idx| {
                        let secs = INTERVAL_PRESETS[idx];
                        this.interval_secs = secs;
                        let _ = this.notifier.send(TrayCmd::SetInterval(secs));
                    }),
                    options: self.strings.interval_labels
                        .iter()
                        .map(|label| RadioItem {
                            label: label.to_string(),
                            ..Default::default()
                        })
                        .collect(),
                }
                .into()],
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            Self::tray_item(self.strings.open_current,       "document-open",      !self.current_name.is_empty(), TrayCmd::OpenCurrent),
            Self::tray_item(self.strings.copy_to_favorites, "emblem-favorite",     !self.current_name.is_empty() && self.has_favorites_dir, TrayCmd::CopyToFavorites),
            Self::tray_item(self.strings.delete_current,    "edit-delete",         !self.current_name.is_empty(), TrayCmd::DeleteCurrent),
            Self::tray_item(self.strings.blacklist_current, "dialog-cancel",       !self.current_name.is_empty() && self.blacklist_enabled, TrayCmd::BlacklistCurrent),
            Self::tray_item(self.strings.reload_config, "view-refresh", true, TrayCmd::ReloadConfig),
            Self::tray_item(self.strings.open_settings, "preferences-system", true, TrayCmd::OpenSettings),
            Self::tray_item(self.strings.fetch_now, "download", true, TrayCmd::FetchNow),
            MenuItem::Separator,
            Self::tray_item(self.strings.quit, "application-exit", true, TrayCmd::Quit),
        ]
    }
}

/// トレイをバックグラウンドで起動し `Handle` を返す。
///
/// D-Bus / SNI が使えない環境（CI、仮想デスクトップ等）では警告を出して
/// `None` を返す。アプリはトレイなしで動作を継続する。
pub async fn spawn_tray(
    notifier: UnboundedSender<TrayCmd>,
    mode: DisplayMode,
    interval_secs: u64,
    lang: Lang,
    has_favorites_dir: bool,
    blacklist_enabled: bool,
) -> Option<ksni::Handle<KabekamiTray>> {
    use ksni::TrayMethods;

    let tray = KabekamiTray {
        notifier,
        paused: false,
        mode,
        interval_secs,
        current_name: String::new(),
        last_error: None,
        image_count: 0,
        strings: crate::i18n::strings(lang),
        has_favorites_dir,
        blacklist_enabled,
    };

    // `assume_sni_available(true)` にすることで、デスクトップ環境の起動が
    // まだ完了していない場合も起動できる（watcher_offline → watcher_online の流れ）。
    match tray.assume_sni_available(true).spawn().await {
        Ok(handle) => {
            tracing::info!("tray icon active (SNI)");
            Some(handle)
        }
        Err(e) => {
            tracing::warn!("tray unavailable ({}), running without tray", e);
            None
        }
    }
}
