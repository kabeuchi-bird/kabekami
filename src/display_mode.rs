//! DisplayMode 別の画像加工。設計書 §4 に準拠。
//!
//! 全モードで画面サイズ (`screen_w × screen_h`) ぴったりの `RgbaImage` を生成して返す。
//! KDE Plasma には常に「画面サイズの加工済み画像」を渡すため、KDE 側の
//! スケーリング設定には依存しない。
//!
//! ## 各モードの動作
//!
//! | モード   | 動作                                             |
//! |---------|--------------------------------------------------|
//! | Fill    | アスペクト比維持・画面を完全に覆う（はみ出し中央クロップ） |
//! | Fit     | アスペクト比維持・画面に収める（余白を黒で埋める）        |
//! | Stretch | アスペクト比無視で画面いっぱいに引き伸ばす               |
//! | BlurPad | ぼかし背景＋前景オーバーレイ（`blur_pad.rs` に委譲）    |
//! | Smart   | アスペクト比の差 ≤ 0.15 → Fill、それ以外 → BlurPad    |

use image::{
    imageops::{self, FilterType},
    DynamicImage, Rgba, RgbaImage,
};

use crate::config::DisplayMode;

/// Smart モードでアスペクト比の差がこの閾値以内なら Fill を選択する。
const SMART_THRESHOLD: f32 = 0.15;

/// `mode` に応じて画像を加工し、`screen_w × screen_h` の `RgbaImage` を返す。
///
/// # 引数
/// - `src`: 元画像
/// - `screen_w`, `screen_h`: 出力解像度
/// - `mode`: 表示モード
/// - `blur_sigma`: BlurPad / Smart(→ BlurPad) 時のぼかし強度
/// - `bg_darken`: BlurPad 背景の暗さ（0.0〜1.0）
pub fn process(
    src: &DynamicImage,
    screen_w: u32,
    screen_h: u32,
    mode: DisplayMode,
    blur_sigma: f32,
    bg_darken: f32,
) -> RgbaImage {
    match mode {
        DisplayMode::Fill => fill(src, screen_w, screen_h),
        DisplayMode::Fit => fit(src, screen_w, screen_h),
        DisplayMode::Stretch => stretch(src, screen_w, screen_h),
        DisplayMode::BlurPad => {
            crate::blur_pad::generate_blur_pad(src, screen_w, screen_h, blur_sigma, bg_darken)
        }
        DisplayMode::Smart => {
            let src_ratio = src.width() as f32 / src.height() as f32;
            let scr_ratio = screen_w as f32 / screen_h as f32;
            if (src_ratio - scr_ratio).abs() <= SMART_THRESHOLD {
                fill(src, screen_w, screen_h)
            } else {
                crate::blur_pad::generate_blur_pad(
                    src, screen_w, screen_h, blur_sigma, bg_darken,
                )
            }
        }
    }
}

/// Fill: アスペクト比を維持しつつ画面を完全に覆う（中央クロップ）。
fn fill(src: &DynamicImage, screen_w: u32, screen_h: u32) -> RgbaImage {
    src.resize_to_fill(screen_w, screen_h, FilterType::Lanczos3)
        .to_rgba8()
}

/// Fit: アスペクト比を維持しつつ画面に収め、余白を黒で埋める（レターボックス）。
fn fit(src: &DynamicImage, screen_w: u32, screen_h: u32) -> RgbaImage {
    let resized = src
        .resize(screen_w, screen_h, FilterType::Lanczos3)
        .to_rgba8();
    let (rw, rh) = resized.dimensions();

    let mut canvas = RgbaImage::from_pixel(screen_w, screen_h, Rgba([0, 0, 0, 255]));
    let offset_x = ((screen_w.saturating_sub(rw)) / 2) as i64;
    let offset_y = ((screen_h.saturating_sub(rh)) / 2) as i64;
    imageops::overlay(&mut canvas, &resized, offset_x, offset_y);
    canvas
}

/// Stretch: アスペクト比を無視して画面いっぱいに引き伸ばす。
fn stretch(src: &DynamicImage, screen_w: u32, screen_h: u32) -> RgbaImage {
    src.resize_exact(screen_w, screen_h, FilterType::Lanczos3)
        .to_rgba8()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32) -> DynamicImage {
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(w, h, Rgba([100, 150, 200, 255])))
    }

    #[test]
    fn fill_produces_screen_dimensions() {
        let out = fill(&solid(800, 600), 1920, 1080);
        assert_eq!(out.dimensions(), (1920, 1080));
    }

    #[test]
    fn fit_produces_screen_dimensions() {
        let out = fit(&solid(800, 600), 1920, 1080);
        assert_eq!(out.dimensions(), (1920, 1080));
    }

    #[test]
    fn fit_fills_with_black_letterbox() {
        // Portrait source on landscape screen → black bars top/bottom
        let out = fit(&solid(100, 200), 200, 200);
        // Top row should be black (letterbox)
        let top_pixel = out.get_pixel(0, 0);
        assert_eq!(top_pixel[0], 0, "letterbox should be black");
    }

    #[test]
    fn stretch_produces_screen_dimensions() {
        let out = stretch(&solid(800, 600), 1920, 1080);
        assert_eq!(out.dimensions(), (1920, 1080));
    }

    #[test]
    fn smart_close_ratio_uses_fill() {
        // 16:9 source on 16:9 screen → diff = 0, use Fill
        let src = solid(1920, 1080);
        let out = process(&src, 1920, 1080, DisplayMode::Smart, 25.0, 0.1);
        assert_eq!(out.dimensions(), (1920, 1080));
    }

    #[test]
    fn all_modes_produce_correct_dimensions() {
        let src = solid(800, 600);
        for mode in [
            DisplayMode::Fill,
            DisplayMode::Fit,
            DisplayMode::Stretch,
            DisplayMode::BlurPad,
            DisplayMode::Smart,
        ] {
            let out = process(&src, 1920, 1080, mode, 10.0, 0.1);
            assert_eq!(
                out.dimensions(),
                (1920, 1080),
                "mode {:?} produced wrong dimensions",
                mode
            );
        }
    }
}
