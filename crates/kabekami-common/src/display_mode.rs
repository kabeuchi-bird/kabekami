//! DisplayMode 別の画像加工。

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

    /// 左右で色が違う画像。単色だと fill と BlurPad の出力差が
    /// レターボックス部分だけになり、経路の判別が弱くなる。
    fn two_tone(w: u32, h: u32) -> DynamicImage {
        let mut img = RgbaImage::new(w, h);
        for (x, _y, px) in img.enumerate_pixels_mut() {
            *px = if x < w / 2 {
                Rgba([200, 40, 40, 255])
            } else {
                Rgba([40, 40, 200, 255])
            };
        }
        DynamicImage::ImageRgba8(img)
    }

    #[test]
    fn fit_fills_with_black_letterbox() {
        let out = fit(&solid(100, 200), 200, 200);
        let top_pixel = out.get_pixel(0, 0);
        assert_eq!(top_pixel[0], 0, "letterbox should be black");
    }

    /// 画面 384x216 は 16:9 (1.778)。ここに比 1.700 の画像を渡すと
    /// 差 0.078 <= SMART_THRESHOLD なので fill 経路に入るべき。
    #[test]
    fn smart_uses_fill_when_ratio_is_close() {
        let src = two_tone(340, 200);
        let out = process(&src, 384, 216, DisplayMode::Smart, 25.0, 0.1);
        // 寸法はどのモードでも同じになるため、出力そのものを突き合わせる
        assert!(out == fill(&src, 384, 216), "比が近いときは fill と一致すべき");
        assert!(
            out != crate::blur_pad::generate_blur_pad(&src, 384, 216, 25.0, 0.1),
            "BlurPad と一致してしまうと、この判定を検証できていない"
        );
    }

    /// 比 1.000 の画像は差 0.778 > SMART_THRESHOLD なので BlurPad 経路に入るべき。
    #[test]
    fn smart_uses_blur_pad_when_ratio_is_far() {
        let src = two_tone(200, 200);
        let out = process(&src, 384, 216, DisplayMode::Smart, 25.0, 0.1);
        assert!(
            out == crate::blur_pad::generate_blur_pad(&src, 384, 216, 25.0, 0.1),
            "比が離れているときは BlurPad と一致すべき"
        );
        assert!(out != fill(&src, 384, 216), "fill と一致してしまうと判定を検証できていない");
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
