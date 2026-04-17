//! DisplayMode 別の画像加工。設計書 §4 に準拠。

use image::{
    imageops::{self, FilterType},
    DynamicImage, Rgba, RgbaImage,
};

use crate::config::DisplayMode;

const SMART_THRESHOLD: f32 = 0.15;

/// `mode` に応じて画像を加工し、`screen_w × screen_h` の `RgbaImage` を返す。
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

fn fill(src: &DynamicImage, screen_w: u32, screen_h: u32) -> RgbaImage {
    src.resize_to_fill(screen_w, screen_h, FilterType::Lanczos3)
        .to_rgba8()
}

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
        let out = fit(&solid(100, 200), 200, 200);
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
