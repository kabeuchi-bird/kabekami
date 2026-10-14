//! kabekami-config — GUI 設定ツール。
//!
//! egui (eframe) による設定画面を提供する。
//! - タブ: Sources / Rotation / Display / Cache / Ui
//! - Display タブで BlurPad パラメータをリアルタイムプレビュー
//! - 保存時に `~/.config/kabekami/config.toml` を上書きし、
//!   デーモンが inotify 経由で自動リロードする

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::mpsc;

use eframe::egui;
use kabekami_common::config::{Config, DisplayMode, OnlineSourceConfig, Order, ProviderKind};
use kabekami_common::i18n::{self, ConfigStrings, Lang};

fn main() -> eframe::Result<()> {
    // タイトルバーは英語固定。ViewportBuilder の仕様で実行中に変更できないこと、
    // および OS のタスクバーで識別しやすいよう "Kabekami Configuration" を採用。
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Kabekami Configuration")
            .with_inner_size([720.0, 580.0])
            .with_resizable(true),
        ..Default::default()
    };
    eframe::run_native(
        "kabekami-config",
        options,
        Box::new(|cc| {
            setup_fonts(&cc.egui_ctx);
            Ok(Box::new(KabekamiApp::new()))
        }),
    )
}

// Common installation paths for noto-fonts-cjk on Linux distributions.
// Latin characters use the default egui font; CJK is appended as fallback.
fn setup_fonts(ctx: &egui::Context) {
    const FONT_PATHS: &[&str] = &[
        "/usr/share/fonts/noto-cjk/NotoSansCJKjp-Regular.otf",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/OTF/NotoSansCJKjp-Regular.otf",
        "/usr/share/fonts/noto/NotoSansCJKjp-Regular.otf",
        "/usr/share/fonts/noto/NotoSansCJK-Regular.ttc",
    ];

    let Some(data) = FONT_PATHS.iter().find_map(|p| std::fs::read(p).ok()) else {
        return;
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("NotoSansCJK".to_owned(), egui::FontData::from_owned(data));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("NotoSansCJK".to_owned());
    }
    ctx.set_fonts(fonts);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// `kdialog` が PATH 上にあるかを起動時に 1 回だけ確認する。
/// 見つからない環境ではフォルダ選択ボタンを無効化する。
fn kdialog_available() -> bool {
    std::process::Command::new("kdialog")
        .arg("--version")
        .output()
        .is_ok()
}

/// 開始位置を解決する（`candidates` のうち最初に存在するもの、無ければ `$HOME`、最後は `.`）。
fn pick_start_dir(candidates: Vec<PathBuf>) -> PathBuf {
    candidates
        .into_iter()
        .find(|p| !p.as_os_str().is_empty() && p.exists())
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// kdialog 出力（パス）を `Option<PathBuf>` に変換する。
/// 失敗・キャンセル・空出力はすべて `None`。
fn kdialog_run(args: &[OsString]) -> Option<PathBuf> {
    let out = std::process::Command::new("kdialog").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(PathBuf::from(s)) }
}

/// kdialog `--getexistingdirectory`（フォルダ選択）の引数。
fn folder_dialog(s: &'static ConfigStrings, start: PathBuf) -> Vec<OsString> {
    vec![
        "--title".into(),
        s.dialog_select_folder.into(),
        "--getexistingdirectory".into(),
        start.into(),
    ]
}

/// kdialog `--getopenfilename`（画像ファイル選択）の引数。
fn image_dialog(s: &'static ConfigStrings, start: PathBuf) -> Vec<OsString> {
    let image_filter = format!("{} (*.jpg *.jpeg *.png *.webp *.avif)", s.image_filter_label);
    vec![
        "--title".into(),
        s.dialog_select_image.into(),
        "--getopenfilename".into(),
        start.into(),
        // 拡張子リストは kdialog の構文なので訳さず、名前だけ差し替える
        image_filter.into(),
    ]
}

/// 選んだパスの書き込み先。
#[derive(Clone, Copy)]
enum PickTarget {
    SourceDir,
    PreviewImage,
    CacheDir,
    /// `online_sources` の添字。ダイアログを開いている間は設定画面ごと操作を
    /// 止めるので（`update` 参照）、添字はずれない。
    DownloadDir(usize),
}

/// 開いているダイアログ。kdialog はユーザーが閉じるまで戻らないので
/// 別スレッドで待ち、UI スレッドは結果をチャネルで受け取るだけにする
/// （同期で呼ぶとその間 GUI の再描画が止まる）。
struct PendingPick {
    target: PickTarget,
    rx: mpsc::Receiver<Option<PathBuf>>,
}


fn compute_dir_size(dir: &std::path::Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

fn opt_text_field(
    ui: &mut egui::Ui,
    label: &str,
    hint: &str,
    width: f32,
    password: bool,
    val: &mut Option<String>,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut s = val.clone().unwrap_or_default();
        let edit = egui::TextEdit::singleline(&mut s)
            .hint_text(hint)
            .desired_width(width)
            .password(password);
        if ui.add(edit).changed() {
            *val = if s.is_empty() { None } else { Some(s) };
        }
    });
}

/// `Option<PathBuf>` 用の単一行入力欄。空（前後の空白除く）なら `None`。
fn opt_path_field(ui: &mut egui::Ui, hint: &str, width: f32, val: &mut Option<PathBuf>) {
    let mut s = val.as_deref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let edit = egui::TextEdit::singleline(&mut s).hint_text(hint).desired_width(width);
    if ui.add(edit).changed() {
        *val = Some(s.trim()).filter(|t| !t.is_empty()).map(PathBuf::from);
    }
}

// ---------------------------------------------------------------------------
// Preview background thread
// ---------------------------------------------------------------------------

struct PreviewRequest {
    path: PathBuf,
    mode: DisplayMode,
    blur_sigma: f32,
    bg_darken: f32,
}

/// ワーカーからの結果。エラーはスレッドを越えるので文字列にしておく。
type PreviewResult = Result<egui::ColorImage, String>;

fn spawn_preview_worker(
    req_rx: mpsc::Receiver<PreviewRequest>,
    res_tx: mpsc::SyncSender<PreviewResult>,
) {
    std::thread::spawn(move || {
        for req in req_rx {
            // ignore send error (UI closed)
            let _ = res_tx.try_send(render_preview(&req).map_err(|e| e.to_string()));
        }
    });
}

fn render_preview(req: &PreviewRequest) -> anyhow::Result<egui::ColorImage> {
    const PREV_W: u32 = 480;
    const PREV_H: u32 = 270; // 16:9

    let src = kabekami_common::display_mode::load_oriented(&req.path)?;

    let rgba = kabekami_common::display_mode::process(
        &src,
        PREV_W,
        PREV_H,
        req.mode,
        req.blur_sigma,
        req.bg_darken,
    );

    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [PREV_W as usize, PREV_H as usize],
        rgba.as_raw(),
    ))
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Sources,
    Online,
    Rotation,
    Display,
    Cache,
    Ui,
}

struct KabekamiApp {
    config: Config,
    tab: Tab,
    status: String,
    status_is_error: bool,

    // Preview state
    preview_image_path: String,
    preview_texture: Option<egui::TextureHandle>,
    preview_req_tx: mpsc::SyncSender<PreviewRequest>,
    preview_res_rx: mpsc::Receiver<PreviewResult>,
    preview_rendering: bool,
    /// Track last-sent params to avoid redundant renders
    preview_last: Option<(String, DisplayMode, f32, f32)>,

    // Sources tab: editing
    new_dir_input: String,

    // Online sources tab: editing
    new_online_provider: ProviderKind,

    // Cache tab: computed size (None = not yet measured)
    cache_size_bytes: Option<u64>,

    /// `kdialog` が利用可能か（起動時に 1 回判定し、参照ボタンの活性／非活性に使う）。
    has_kdialog: bool,
    /// 開いているファイル選択ダイアログ（同時に 1 つまで）。
    pending_pick: Option<PendingPick>,
}

impl KabekamiApp {
    fn new() -> Self {
        let config = Config::load().unwrap_or_default();

        // channel: UI → worker (unbounded so UI never blocks)
        let (req_tx, req_rx) = mpsc::sync_channel::<PreviewRequest>(1);
        // channel: worker → UI (sync, capacity 1: drop stale results)
        let (res_tx, res_rx) = mpsc::sync_channel::<PreviewResult>(1);

        spawn_preview_worker(req_rx, res_tx);

        Self {
            config,
            tab: Tab::Sources,
            status: String::new(),
            status_is_error: false,
            preview_image_path: String::new(),
            preview_texture: None,
            preview_req_tx: req_tx,
            preview_res_rx: res_rx,
            preview_rendering: false,
            preview_last: None,
            new_dir_input: String::new(),
            new_online_provider: ProviderKind::Bing,
            cache_size_bytes: None,
            has_kdialog: kdialog_available(),
            pending_pick: None,
        }
    }

    /// 現在の言語の文字列テーブル。`config.ui.language` から毎回引くので、
    /// UI タブの言語ドロップダウンを変えた瞬間に GUI 全体の表示が切り替わる。
    ///
    /// 返り値は `&'static` なので `self` の借用を持ち越さない。
    /// 各 `ui_*` の先頭で `let s = self.s();` と束縛しておけば、
    /// 以降 `&mut self.config` と同時に使っても借用が衝突しない。
    fn s(&self) -> &'static ConfigStrings {
        i18n::config_strings(Lang::from_code(&self.config.ui.language))
    }

    /// 「📁 参照」ボタンを描画する（kdialog 不在時は無効化＆ツールチップ）。
    /// クリックされたら `true` を返す。ラベルはここで一元管理し、呼び出し側での
    /// 翻訳漏れを構造的に防ぐ。
    fn browse_button(&self, ui: &mut egui::Ui) -> bool {
        let s = self.s();
        let resp = ui.add_enabled(self.has_kdialog, egui::Button::new(s.browse));
        let clicked = resp.clicked();
        if !self.has_kdialog {
            resp.on_hover_text(s.kdialog_missing);
        }
        clicked
    }

    fn set_status(&mut self, msg: impl Into<String>, is_error: bool) {
        self.status = msg.into();
        self.status_is_error = is_error;
    }

    fn save_config(&mut self) {
        match self.config.save() {
            Ok(()) => self.set_status(self.s().saved, false),
            Err(e) => self.set_status(format!("{}: {e}", self.s().save_failed), true),
        }
    }

    fn request_preview(&mut self) {
        let path_str = self.preview_image_path.trim().to_string();
        if path_str.is_empty() {
            return;
        }
        let mode = self.config.display.mode;
        let sigma = self.config.display.blur_sigma;
        let darken = self.config.display.bg_darken;

        let key = (path_str.clone(), mode, sigma, darken);
        if self.preview_last.as_ref() == Some(&key) {
            return; // no change
        }
        self.preview_last = Some(key);
        self.preview_rendering = true;

        let _ = self.preview_req_tx.try_send(PreviewRequest {
            path: PathBuf::from(path_str),
            mode,
            blur_sigma: sigma,
            bg_darken: darken,
        });
    }

    /// ファイル選択ダイアログを開く。画像を選ぶのはプレビューだけで、
    /// 他はフォルダ。開始位置の存在確認（ネットワークマウントでは遅い）も
    /// 別スレッドで行う。
    fn start_pick(&mut self, ctx: &egui::Context, target: PickTarget, start: Vec<PathBuf>) {
        let (tx, rx) = mpsc::channel();
        let (ctx, s) = (ctx.clone(), self.s());
        std::thread::spawn(move || {
            let start = pick_start_dir(start);
            let args = match target {
                PickTarget::PreviewImage => image_dialog(s, start),
                _ => folder_dialog(s, start),
            };
            let _ = tx.send(kdialog_run(&args));
            // 入力が無くても結果を拾えるよう、UI を起こす
            ctx.request_repaint();
        });
        self.pending_pick = Some(PendingPick { target, rx });
    }

    /// ダイアログの結果が届いていれば書き込み先に反映する。
    fn poll_pick(&mut self) {
        let Some(pending) = &self.pending_pick else { return };
        let picked = match pending.rx.try_recv() {
            Ok(picked) => picked,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        let target = pending.target;
        self.pending_pick = None;
        let Some(path) = picked else { return };
        match target {
            PickTarget::SourceDir => {
                self.config.sources.directories.push(path);
                self.new_dir_input.clear();
            }
            PickTarget::PreviewImage => {
                self.preview_image_path = path.to_string_lossy().into_owned();
                self.request_preview();
            }
            PickTarget::CacheDir => {
                self.config.cache.directory = path;
                self.cache_size_bytes = None;
            }
            PickTarget::DownloadDir(i) => {
                if let Some(oc) = self.config.online_sources.get_mut(i) {
                    oc.download_dir = Some(path);
                }
            }
        }
    }

    fn poll_preview(&mut self, ctx: &egui::Context) {
        if let Ok(result) = self.preview_res_rx.try_recv() {
            self.preview_rendering = false;
            match result {
                Ok(img) => {
                    self.preview_texture = Some(ctx.load_texture(
                        "preview",
                        img,
                        egui::TextureOptions::LINEAR,
                    ));
                }
                Err(e) => {
                    self.set_status(format!("{}: {e}", self.s().preview_error), true);
                    self.preview_texture = None;
                }
            }
        }
    }
}

impl eframe::App for KabekamiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_preview(ctx);
        self.poll_pick();
        let s = self.s();

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                for (label, tab) in [
                    (s.tab_sources, Tab::Sources),
                    (s.tab_online, Tab::Online),
                    (s.tab_rotation, Tab::Rotation),
                    (s.tab_display, Tab::Display),
                    (s.tab_cache, Tab::Cache),
                    (s.tab_ui, Tab::Ui),
                ] {
                    ui.selectable_value(&mut self.tab, tab, label);
                }
            });
        });

        egui::TopBottomPanel::bottom("actions").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button(s.save_button).clicked() {
                    self.save_config();
                }
                ui.separator();
                if self.status_is_error {
                    ui.colored_label(egui::Color32::RED, &self.status);
                } else {
                    ui.label(&self.status);
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            // ダイアログを開いている間は設定を触らせない（書き込み先を添字で持つため）
            ui.add_enabled_ui(self.pending_pick.is_none(), |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
                    Tab::Sources => self.ui_sources(ui),
                    Tab::Online => self.ui_online(ui),
                    Tab::Rotation => self.ui_rotation(ui),
                    Tab::Display => self.ui_display(ui, ctx),
                    Tab::Cache => self.ui_cache(ui),
                    Tab::Ui => self.ui_ui_tab(ui),
                });
            });
        });
    }
}

// ---------------------------------------------------------------------------
// Tab implementations
// ---------------------------------------------------------------------------

impl KabekamiApp {
    fn ui_sources(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        ui.heading(s.sources_heading);
        ui.separator();

        ui.checkbox(&mut self.config.sources.recursive, s.recursive);
        ui.add_space(8.0);

        // お気に入りフォルダ
        ui.label(s.favorites_dir);
        opt_path_field(ui, s.favorites_hint, 400.0, &mut self.config.sources.favorites_dir);
        ui.add_space(8.0);

        ui.label(s.directories);
        let mut remove_idx = None;
        for (i, dir) in self.config.sources.directories.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(dir.to_string_lossy().as_ref());
                if ui.small_button("✖").clicked() {
                    remove_idx = Some(i);
                }
            });
        }
        if let Some(idx) = remove_idx {
            self.config.sources.directories.remove(idx);
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.new_dir_input)
                    .hint_text("/path/to/wallpapers")
                    .desired_width(400.0),
            );
            let add = ui.button(s.add).clicked()
                || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            if add {
                let p = self.new_dir_input.trim().to_string();
                if !p.is_empty() {
                    self.config.sources.directories.push(PathBuf::from(p));
                    self.new_dir_input.clear();
                }
            }
            if self.browse_button(ui) {
                self.start_pick(ui.ctx(), PickTarget::SourceDir, Vec::new());
            }
        });
    }

    fn ui_rotation(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        ui.heading(s.rotation_heading);
        ui.separator();

        ui.horizontal(|ui| {
            ui.label(s.interval_secs);
            ui.add(egui::DragValue::new(&mut self.config.rotation.interval_secs).range(5..=86400));
        });
        ui.add_space(4.0);

        ui.label(s.order);
        ui.radio_value(&mut self.config.rotation.order, Order::Random, s.order_random);
        ui.radio_value(
            &mut self.config.rotation.order,
            Order::Sequential,
            s.order_sequential,
        );
        ui.add_space(4.0);

        ui.checkbox(
            &mut self.config.rotation.change_on_start,
            s.change_on_start,
        );
        ui.checkbox(
            &mut self.config.rotation.prefetch,
            s.prefetch,
        );
    }

    fn ui_display(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let s = self.s();
        ui.heading(s.display_heading);
        ui.separator();

        let mut mode_changed = false;
        for (mode, label) in [
            (DisplayMode::BlurPad, s.mode_blurpad),
            (DisplayMode::Smart, s.mode_smart),
            (DisplayMode::Fill, s.mode_fill),
            (DisplayMode::Fit, s.mode_fit),
            (DisplayMode::Stretch, s.mode_stretch),
        ] {
            if ui
                .radio_value(&mut self.config.display.mode, mode, label)
                .clicked()
            {
                mode_changed = true;
            }
        }

        let blur_applies =
            matches!(self.config.display.mode, DisplayMode::BlurPad | DisplayMode::Smart);

        ui.add_space(8.0);
        ui.add_enabled_ui(blur_applies, |ui| {
            ui.horizontal(|ui| {
                ui.label(s.blur_sigma);
                let resp = ui.add(
                    egui::Slider::new(&mut self.config.display.blur_sigma, 1.0..=50.0)
                        .step_by(0.5),
                );
                if resp.changed() {
                    mode_changed = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label(s.bg_darken);
                let resp = ui.add(
                    egui::Slider::new(&mut self.config.display.bg_darken, 0.0..=1.0)
                        .step_by(0.05),
                );
                if resp.changed() {
                    mode_changed = true;
                }
            });
        });

        ui.add_space(12.0);
        ui.separator();
        ui.label(s.preview_image);
        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.preview_image_path)
                    .hint_text("/path/to/image.jpg")
                    .desired_width(440.0),
            );
            let should_preview =
                ui.button(s.preview_button).clicked()
                || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                || mode_changed;
            if self.browse_button(ui) {
                let cur_path = std::path::Path::new(self.preview_image_path.trim());
                let start = cur_path
                    .parent()
                    .map(PathBuf::from)
                    .into_iter()
                    .chain(self.config.sources.directories.first().cloned())
                    .collect();
                self.start_pick(ui.ctx(), PickTarget::PreviewImage, start);
            }
            if should_preview {
                self.request_preview();
            }
        });

        if self.preview_rendering {
            ui.spinner();
            ctx.request_repaint();
        }

        if let Some(tex) = &self.preview_texture {
            let avail = ui.available_size();
            let max_w = avail.x.min(480.0);
            let aspect = 270.0 / 480.0;
            let img_size = egui::Vec2::new(max_w, max_w * aspect);
            ui.add(egui::Image::new(tex).fit_to_exact_size(img_size));
        }
    }

    fn ui_cache(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        ui.heading(s.cache_heading);
        ui.separator();

        ui.horizontal(|ui| {
            ui.label(s.cache_directory);
            let mut dir_str = self.config.cache.directory.to_string_lossy().into_owned();
            if ui.text_edit_singleline(&mut dir_str).changed() {
                self.config.cache.directory = PathBuf::from(dir_str);
                self.cache_size_bytes = None; // ディレクトリ変更時はリセット
            }
            if self.browse_button(ui) {
                let start = vec![self.config.cache.directory.clone()];
                self.start_pick(ui.ctx(), PickTarget::CacheDir, start);
            }
        });

        ui.horizontal(|ui| {
            ui.label(s.max_size_mb);
            ui.add(egui::DragValue::new(&mut self.config.cache.max_size_mb).range(0..=100_000));
        });
        ui.label(s.unlimited_hint);

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(s.refresh).clicked() {
                self.cache_size_bytes = Some(compute_dir_size(&self.config.cache.directory));
            }
            if ui.button(s.clear_cache).clicked() {
                if let Ok(entries) = std::fs::read_dir(&self.config.cache.directory) {
                    for entry in entries.flatten() {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
                self.cache_size_bytes = Some(0);
            }
            match self.cache_size_bytes {
                None => {
                    ui.label(s.current_size_unknown);
                }
                Some(bytes) => {
                    let mb = bytes as f64 / (1024.0 * 1024.0);
                    let max = self.config.cache.max_size_mb;
                    let label = s.current_size;
                    if max > 0 {
                        ui.label(format!("{label}: {:.1} MB / {} MB", mb, max));
                    } else {
                        let unlimited = s.unlimited;
                        ui.label(format!("{label}: {:.1} MB ({unlimited})", mb));
                    }
                }
            }
        });
    }

    fn ui_online(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        ui.heading(s.online_heading);
        ui.separator();
        ui.label(s.online_desc);
        ui.add_space(8.0);

        let mut remove_idx: Option<usize> = None;
        // ループ中に self.browse_button()（&self 借用）を呼ぶため、Vec を take して
        // 所有権を手元に移す。deep clone と違い take/戻しは O(1)。
        let mut sources = std::mem::take(&mut self.config.online_sources);
        for (i, oc) in sources.iter_mut().enumerate() {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut oc.enabled, format!("**{}**", oc.provider));
                    if ui.small_button(s.remove).clicked() {
                        remove_idx = Some(i);
                    }
                });

                ui.indent(format!("online_{}", i), |ui| {
                    if matches!(oc.provider, ProviderKind::Unsplash | ProviderKind::Wallhaven) {
                        opt_text_field(ui, s.api_key, "", 300.0, true, &mut oc.api_key);
                        opt_text_field(ui, s.query, "nature", 200.0, false, &mut oc.query);
                    }
                    if oc.provider == ProviderKind::Reddit {
                        opt_text_field(ui, s.subreddit, "wallpapers", 200.0, false, &mut oc.subreddit);
                    }
                    // Bing: ロケール
                    if oc.provider == ProviderKind::Bing {
                        let locale_hint = format!("en-US {}", s.default_option);
                        opt_text_field(ui, s.locale, &locale_hint, 150.0, false, &mut oc.locale);
                    }
                    // Unsplash: 画質
                    if oc.provider == ProviderKind::Unsplash {
                        ui.horizontal(|ui| {
                            ui.label(s.quality);
                            let mut q = oc.quality.clone().unwrap_or_else(|| "regular".to_string());
                            // "regular" / "full" は Unsplash API の値そのものなので訳さない
                            let regular_label = format!("regular {}", s.default_option);
                            ui.radio_value(&mut q, "regular".to_string(), regular_label);
                            ui.radio_value(&mut q, "full".to_string(), "full");
                            oc.quality = if q == "regular" { None } else { Some(q) };
                        });
                    }
                    // 保持枚数
                    ui.horizontal(|ui| {
                        ui.label(s.count);
                        ui.add(egui::DragValue::new(&mut oc.count).range(1..=100));
                    });
                    // 再取得間隔
                    ui.horizontal(|ui| {
                        ui.label(s.interval_hours);
                        let mut hours = oc.interval_hours.unwrap_or(0);
                        let resp = ui.add(egui::DragValue::new(&mut hours).range(0..=8760));
                        ui.label(
                            s.interval_default_hint
                                .replace("{}", &oc.provider.default_interval_hours().to_string()),
                        );
                        if resp.changed() {
                            oc.interval_hours = if hours == 0 { None } else { Some(hours) };
                        }
                    });
                    // ダウンロード先ディレクトリ
                    ui.horizontal(|ui| {
                        ui.label(s.download_dir);
                        let hint = format!(
                            "~/.local/share/kabekami/{} {}",
                            oc.provider, s.default_option
                        );
                        opt_path_field(ui, &hint, 260.0, &mut oc.download_dir);
                        if self.browse_button(ui) {
                            let target = PickTarget::DownloadDir(i);
                            self.start_pick(ui.ctx(), target, oc.download_dir.iter().cloned().collect());
                        }
                    });
                });
            });
            ui.add_space(4.0);
        }
        self.config.online_sources = sources;
        if let Some(idx) = remove_idx {
            self.config.online_sources.remove(idx);
        }

        // 新規追加
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(s.add_provider);
            egui::ComboBox::from_id_salt("new_provider")
                .selected_text(self.new_online_provider.to_string())
                .show_ui(ui, |ui| {
                    for p in [
                        ProviderKind::Bing,
                        ProviderKind::Unsplash,
                        ProviderKind::Wallhaven,
                        ProviderKind::Reddit,
                    ] {
                        ui.selectable_value(&mut self.new_online_provider, p, p.to_string());
                    }
                });
            if ui.button(s.add_provider_button).clicked() {
                self.config.online_sources.push(OnlineSourceConfig {
                    provider: self.new_online_provider,
                    enabled: true,
                    download_dir: None,
                    api_key: None,
                    query: None,
                    count: 10,
                    subreddit: None,
                    interval_hours: None,
                    locale: None,
                    quality: None,
                });
            }
        });
        ui.add_space(4.0);
        ui.label(s.download_dir_hint);
    }

    fn ui_ui_tab(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        ui.heading(s.ui_heading);
        ui.separator();

        ui.label(s.language);
        // `Lang::from_code` と同じ照合規則で引く。完全一致で引くと
        // `ui.language = "JA"` のとき GUI は日本語なのに表示だけ
        // 「不明」になってしまう。
        let selected_label = if self.config.ui.language.is_empty() {
            s.default_option
        } else {
            kabekami_common::i18n::lookup_code(&self.config.ui.language)
                .map(|e| e.display_name)
                .unwrap_or(s.unknown_language)
        };
        egui::ComboBox::from_id_salt("language")
            .selected_text(selected_label)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.config.ui.language, String::new(), s.default_option);
                for entry in kabekami_common::i18n::registry() {
                    ui.selectable_value(
                        &mut self.config.ui.language,
                        entry.id.to_string(),
                        entry.display_name,
                    );
                }
            });
        ui.add_space(8.0);

        ui.checkbox(
            &mut self.config.ui.warn_notify,
            s.warn_notify,
        );
        ui.add_space(4.0);
        ui.checkbox(
            &mut self.config.ui.notify_fetch,
            s.notify_fetch,
        );
        ui.add_space(4.0);
        ui.checkbox(
            &mut self.config.ui.enable_blacklist,
            s.enable_blacklist,
        );
    }
}
