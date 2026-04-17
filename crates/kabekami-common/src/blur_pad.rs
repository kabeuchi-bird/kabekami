//! BlurPad 画像処理パイプライン。設計書 §5 に準拠。

use image::{
    imageops::{self, FilterType},
    DynamicImage, RgbaImage,
};

const DOWNSCALE: u32 = 4;

/// BlurPad 画像を生成する。
pub fn generate_blur_pad(
    src: &DynamicImage,
    screen_w: u32,
    screen_h: u32,
    blur_sigma: f32,
    bg_darken: f32,
) -> RgbaImage {
    assert!(screen_w > 0 && screen_h > 0, "invalid screen dimensions");

    let small_w = (screen_w / DOWNSCALE).max(1);
    let small_h = (screen_h / DOWNSCALE).max(1);

    let bg_small_rgba: RgbaImage = src
        .resize_to_fill(small_w, small_h, FilterType::Triangle)
        .to_rgba8();

    let scaled_sigma = (blur_sigma / DOWNSCALE as f32).max(0.1);
    let mut bg_blurred: RgbaImage = imageops::blur(&bg_small_rgba, scaled_sigma);

    if bg_darken > 0.0 {
        darken(&mut bg_blurred, bg_darken);
    }

    let mut canvas: RgbaImage = DynamicImage::ImageRgba8(bg_blurred)
        .resize_exact(screen_w, screen_h, FilterType::Triangle)
        .to_rgba8();

    let fg: RgbaImage = src
        .resize(screen_w, screen_h, FilterType::Lanczos3)
        .to_rgba8();

    let (fg_w, fg_h) = fg.dimensions();
    let offset_x = ((screen_w.saturating_sub(fg_w)) / 2) as i64;
    let offset_y = ((screen_h.saturating_sub(fg_h)) / 2) as i64;
    imageops::overlay(&mut canvas, &fg, offset_x, offset_y);

    canvas
}

fn darken(img: &mut RgbaImage, amount: f32) {
    let factor = (1.0 - amount.clamp(0.0, 1.0)).max(0.0);
    for pixel in img.pixels_mut() {
        pixel[0] = (pixel[0] as f32 * factor) as u8;
        pixel[1] = (pixel[1] as f32 * factor) as u8;
        pixel[2] = (pixel[2] as f32 * factor) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn solid(w: u32, h: u32, color: [u8; 4]) -> DynamicImage {
        let mut img = RgbaImage::new(w, h);
        for p in img.pixels_mut() {
            *p = Rgba(color);
        }
        DynamicImage::ImageRgba8(img)
    }

    #[test]
    fn output_dimensions_match_screen() {
        let src = solid(800, 600, [200, 100, 50, 255]);
        let out = generate_blur_pad(&src, 1920, 1080, 10.0, 0.0);
        assert_eq!(out.dimensions(), (1920, 1080));
    }

    #[test]
    fn handles_portrait_source_on_landscape_screen() {
        let src = solid(600, 1200, [50, 150, 200, 255]);
        let out = generate_blur_pad(&src, 1920, 1080, 10.0, 0.1);
        assert_eq!(out.dimensions(), (1920, 1080));
    }

    #[test]
    fn handles_landscape_source_on_portrait_screen() {
        let src = solid(1200, 600, [150, 50, 200, 255]);
        let out = generate_blur_pad(&src, 1080, 1920, 10.0, 0.0);
        assert_eq!(out.dimensions(), (1080, 1920));
    }

    #[test]
    fn darken_reduces_rgb_values() {
        let mut img = RgbaImage::from_pixel(2, 2, Rgba([100, 100, 100, 255]));
        darken(&mut img, 0.5);
        let px = img.get_pixel(0, 0);
        assert_eq!(px[0], 50);
        assert_eq!(px[3], 255, "alpha must be preserved");
    }
}
