//! kabekami-config — GUI 設定ツール。設計書 §13 に準拠。
//!
//! egui (eframe) による設定画面を提供する。
//! - タブ: Sources / Rotation / Display / Cache / Ui
//! - Display タブで BlurPad パラメータをリアルタイムプレビュー
//! - 保存時に `~/.config/kabekami/config.toml` を上書きし、
//!   デーモンが inotify 経由で自動リロードする

use std::path::PathBuf;
use std::sync::mpsc;

use eframe::egui;
use kabekami_common::config::{Config, DisplayMode, Order};

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("kabekami 設定 / Settings")
            .with_inner_size([720.0, 580.0])
            .with_resizable(true),
        ..Default::default()
    };
    eframe::run_native(
        "kabekami-config",
        options,
        Box::new(|_cc| Ok(Box::new(KabekamiApp::new()))),
    )
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

enum PreviewResult {
    Ready(egui::ColorImage),
    Error(String),
}

fn spawn_preview_worker(
    req_rx: mpsc::Receiver<PreviewRequest>,
    res_tx: mpsc::SyncSender<PreviewResult>,
) {
    std::thread::spawn(move || {
        for req in req_rx {
            let result = render_preview(&req);
            let msg = match result {
                Ok(img) => PreviewResult::Ready(img),
                Err(e) => PreviewResult::Error(e.to_string()),
            };
            // ignore send error (UI closed)
            let _ = res_tx.try_send(msg);
        }
    });
}

fn render_preview(req: &PreviewRequest) -> anyhow::Result<egui::ColorImage> {
    const PREV_W: u32 = 480;
    const PREV_H: u32 = 270; // 16:9

    let reader = image::ImageReader::open(&req.path)?
        .with_guessed_format()?;
    let src = reader.decode()?;

    let rgba = kabekami_common::display_mode::process(
        &src,
        PREV_W,
        PREV_H,
        req.mode,
        req.blur_sigma,
        req.bg_darken,
    );

    let pixels: Vec<egui::Color32> = rgba
        .pixels()
        .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
        .collect();

    Ok(egui::ColorImage {
        size: [PREV_W as usize, PREV_H as usize],
        pixels,
    })
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Sources,
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
        }
    }

    fn set_status(&mut self, msg: impl Into<String>, is_error: bool) {
        self.status = msg.into();
        self.status_is_error = is_error;
    }

    fn save_config(&mut self) {
        match self.config.save() {
            Ok(()) => self.set_status("設定を保存しました / Config saved.", false),
            Err(e) => self.set_status(format!("保存失敗 / Save failed: {e}"), true),
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

    fn poll_preview(&mut self, ctx: &egui::Context) {
        if let Ok(result) = self.preview_res_rx.try_recv() {
            self.preview_rendering = false;
            match result {
                PreviewResult::Ready(img) => {
                    self.preview_texture = Some(ctx.load_texture(
                        "preview",
                        img,
                        egui::TextureOptions::LINEAR,
                    ));
                }
                PreviewResult::Error(e) => {
                    self.set_status(format!("プレビューエラー / Preview error: {e}"), true);
                    self.preview_texture = None;
                }
            }
        }
    }
}

impl eframe::App for KabekamiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_preview(ctx);

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                for (label, tab) in [
                    ("Sources", Tab::Sources),
                    ("Rotation", Tab::Rotation),
                    ("Display", Tab::Display),
                    ("Cache", Tab::Cache),
                    ("UI", Tab::Ui),
                ] {
                    ui.selectable_value(&mut self.tab, tab, label);
                }
            });
        });

        egui::TopBottomPanel::bottom("actions").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("💾  保存 / Save").clicked() {
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
            egui::ScrollArea::vertical().show(ui, |ui| {
                match self.tab {
                    Tab::Sources => self.ui_sources(ui),
                    Tab::Rotation => self.ui_rotation(ui),
                    Tab::Display => self.ui_display(ui, ctx),
                    Tab::Cache => self.ui_cache(ui),
                    Tab::Ui => self.ui_ui_tab(ui),
                }
            });
        });
    }
}

// ---------------------------------------------------------------------------
// Tab implementations
// ---------------------------------------------------------------------------

impl KabekamiApp {
    fn ui_sources(&mut self, ui: &mut egui::Ui) {
        ui.heading("壁紙ソース / Sources");
        ui.separator();

        ui.checkbox(&mut self.config.sources.recursive, "サブフォルダも含める / Recursive");
        ui.add_space(8.0);

        ui.label("ディレクトリ / Directories:");
        let dirs = self.config.sources.directories.clone();
        let mut remove_idx = None;
        for (i, dir) in dirs.iter().enumerate() {
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
            let add = ui.button("追加 / Add").clicked()
                || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            if add {
                let p = self.new_dir_input.trim().to_string();
                if !p.is_empty() {
                    self.config.sources.directories.push(PathBuf::from(p));
                    self.new_dir_input.clear();
                }
            }
        });
    }

    fn ui_rotation(&mut self, ui: &mut egui::Ui) {
        ui.heading("ローテーション / Rotation");
        ui.separator();

        ui.horizontal(|ui| {
            ui.label("切り替え間隔 / Interval (秒/sec):");
            ui.add(egui::DragValue::new(&mut self.config.rotation.interval_secs).range(5..=86400));
        });
        ui.add_space(4.0);

        ui.label("順序 / Order:");
        ui.radio_value(&mut self.config.rotation.order, Order::Random, "ランダム / Random");
        ui.radio_value(
            &mut self.config.rotation.order,
            Order::Sequential,
            "順番 / Sequential",
        );
        ui.add_space(4.0);

        ui.checkbox(
            &mut self.config.rotation.change_on_start,
            "起動時に即切り替え / Change on start",
        );
        ui.checkbox(
            &mut self.config.rotation.prefetch,
            "次の壁紙を先読み / Prefetch next wallpaper",
        );
    }

    fn ui_display(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.heading("表示モード / Display");
        ui.separator();

        let mut mode_changed = false;
        for (mode, label) in [
            (DisplayMode::BlurPad, "BlurPad (ぼかし背景＋前景)"),
            (DisplayMode::Smart, "Smart (アスペクト比で自動選択)"),
            (DisplayMode::Fill, "Fill (クロップ)"),
            (DisplayMode::Fit, "Fit (レターボックス)"),
            (DisplayMode::Stretch, "Stretch (引き伸ばし)"),
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
                ui.label("ぼかし強度 / Blur sigma:");
                let resp = ui.add(
                    egui::Slider::new(&mut self.config.display.blur_sigma, 1.0..=50.0)
                        .step_by(0.5),
                );
                if resp.changed() {
                    mode_changed = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label("背景暗さ / BG darken:");
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
        ui.label("プレビュー画像 / Preview image:");
        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.preview_image_path)
                    .hint_text("/path/to/image.jpg")
                    .desired_width(500.0),
            );
            let preview_clicked = ui.button("▶ プレビュー").clicked();
            if preview_clicked || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) || mode_changed {
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
        ui.heading("キャッシュ / Cache");
        ui.separator();

        ui.horizontal(|ui| {
            ui.label("ディレクトリ / Directory:");
            let mut dir_str = self.config.cache.directory.to_string_lossy().into_owned();
            if ui.text_edit_singleline(&mut dir_str).changed() {
                self.config.cache.directory = PathBuf::from(dir_str);
            }
        });

        ui.horizontal(|ui| {
            ui.label("最大サイズ / Max size (MB):");
            ui.add(egui::DragValue::new(&mut self.config.cache.max_size_mb).range(0..=100_000));
        });
        ui.label("0 = 無制限 / unlimited");
    }

    fn ui_ui_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("UI 設定 / UI Settings");
        ui.separator();

        ui.label("表示言語 / Language:");
        ui.radio_value(&mut self.config.ui.language, String::new(), "English (default)");
        ui.radio_value(&mut self.config.ui.language, "en".to_string(), "English (explicit)");
        ui.radio_value(&mut self.config.ui.language, "ja".to_string(), "日本語");
        ui.add_space(8.0);

        ui.checkbox(
            &mut self.config.ui.warn_notify,
            "警告をデスクトップ通知で表示 / Show warnings as desktop notifications",
        );
    }
}
