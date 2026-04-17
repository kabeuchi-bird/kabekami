//! システムトレイアイコンとコンテキストメニュー。設計書 §10 に準拠。
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
use crate::i18n::{Lang, UiStrings};

/// メインループに送るトレイコマンド。
#[derive(Debug)]
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
    /// 設定ファイルを再読み込みする
    ReloadConfig,
    /// アプリ終了
    Quit,
}

/// 設計書 §10 のメニューに表示する切り替え間隔プリセット（秒）。
/// ラベル表示は `i18n::UiStrings::interval_labels` を使用する。
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
    /// UI 文字列テーブル（言語設定に応じて初期化）。
    pub strings: &'static UiStrings,
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
                None => self.strings.tooltip_current.replacen("{}", &self.current_name, 1),
            },
            ..Default::default()
        }
    }

    /// 設計書 §10 のメニュー構成を組み立てる。
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
            .unwrap_or(3);

        vec![
            // ▶ 次の壁紙 / Next Wallpaper
            StandardItem {
                label: self.strings.next_wallpaper.into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.notifier.send(TrayCmd::Next);
                }),
                ..Default::default()
            }
            .into(),
            // ◀ 前の壁紙 / Previous Wallpaper
            StandardItem {
                label: self.strings.prev_wallpaper.into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.notifier.send(TrayCmd::Prev);
                }),
                ..Default::default()
            }
            .into(),
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
            // 現在の壁紙を開く / Open Current Wallpaper
            StandardItem {
                label: self.strings.open_current.into(),
                icon_name: "document-open".into(),
                enabled: !self.current_name.is_empty(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.notifier.send(TrayCmd::OpenCurrent);
                }),
                ..Default::default()
            }
            .into(),
            // 設定を再読み込み / Reload Config
            StandardItem {
                label: self.strings.reload_config.into(),
                icon_name: "view-refresh".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.notifier.send(TrayCmd::ReloadConfig);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            // 終了 / Quit
            StandardItem {
                label: self.strings.quit.into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.notifier.send(TrayCmd::Quit);
                }),
                ..Default::default()
            }
            .into(),
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
) -> Option<ksni::Handle<KabekamiTray>> {
    use ksni::TrayMethods;

    let tray = KabekamiTray {
        notifier,
        paused: false,
        mode,
        interval_secs,
        current_name: String::new(),
        last_error: None,
        strings: crate::i18n::strings(lang),
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
