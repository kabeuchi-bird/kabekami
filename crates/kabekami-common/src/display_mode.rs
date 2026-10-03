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
        DisplayMode::BlurPad => blur_pad(src, screen_w, screen_h, blur_sigma, bg_darken),
        DisplayMode::Smart => {
            let src_ratio = src.width() as f32 / src.height() as f32;
            let scr_ratio = screen_w as f32 / screen_h as f32;
            if (src_ratio - scr_ratio).abs() <= SMART_THRESHOLD {
                fill(src, screen_w, screen_h)
            } else {
                blur_pad(
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
    let canvas = RgbaImage::from_pixel(screen_w, screen_h, Rgba([0, 0, 0, 255]));
    center_on(canvas, src)
}

/// `src` を `canvas` に収まる大きさへ縮小し、中央に重ねる（Fit と BlurPad の前景）。
fn center_on(mut canvas: RgbaImage, src: &DynamicImage) -> RgbaImage {
    let (w, h) = canvas.dimensions();
    let fg = src.resize(w, h, FilterType::Lanczos3).to_rgba8();
    let offset_x = (w.saturating_sub(fg.width()) / 2) as i64;
    let offset_y = (h.saturating_sub(fg.height()) / 2) as i64;
    imageops::overlay(&mut canvas, &fg, offset_x, offset_y);
    canvas
}

fn stretch(src: &DynamicImage, screen_w: u32, screen_h: u32) -> RgbaImage {
    src.resize_exact(screen_w, screen_h, FilterType::Lanczos3)
        .to_rgba8()
}

// ── BlurPad ────────────────────────────────────────────────────────────

const DOWNSCALE: u32 = 4;

/// BlurPad 画像を生成する。
fn blur_pad(
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

    let canvas: RgbaImage = DynamicImage::ImageRgba8(bg_blurred)
        .resize_exact(screen_w, screen_h, FilterType::Triangle)
        .to_rgba8();

    center_on(canvas, src)
}

fn darken(img: &mut RgbaImage, amount: f32) {
    let factor = (1.0 - amount.clamp(0.0, 1.0)).max(0.0);
    for pixel in img.pixels_mut() {
        pixel[0] = (pixel[0] as f32 * factor) as u8;
        pixel[1] = (pixel[1] as f32 * factor) as u8;
        pixel[2] = (pixel[2] as f32 * factor) as u8;
    }
}

/// 画像を読み込み、EXIF Orientation を適用して返す。
///
/// フォーマットは拡張子ではなくマジックバイトで判定する（食い違えば警告）。
/// `decode()` は orientation を取り出す前に reader を消費するため、
/// `into_decoder()` で分解して orientation を先に読む。
pub fn load_oriented(src: &std::path::Path) -> anyhow::Result<image::DynamicImage> {
    use image::ImageDecoder;

    let reader = image::ImageReader::open(src)
        .map_err(|e| anyhow::anyhow!("failed to open {}: {}", src.display(), e))?;
    let ext_fmt = reader.format(); // 拡張子から推定したフォーマット
    let reader = reader
        .with_guessed_format()
        .map_err(|e| anyhow::anyhow!("failed to read {}: {}", src.display(), e))?;
    let content_fmt = reader.format(); // マジックバイトから検出したフォーマット

    if let (Some(ef), Some(cf)) = (ext_fmt, content_fmt) {
        if ef != cf {
            tracing::warn!(
                "extension/format mismatch: {} (extension → {:?}, content → {:?}); decoding as {:?}",
                src.display(), ef, cf, cf,
            );
        }
    }

    let mut decoder = reader
        .into_decoder()
        .map_err(|e| anyhow::anyhow!("failed to create decoder for {}: {}", src.display(), e))?;
    let orientation = decoder.orientation().unwrap_or_else(|e| {
        tracing::debug!("orientation read failed for {}: {}", src.display(), e);
        image::metadata::Orientation::NoTransforms
    });
    let mut img = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| anyhow::anyhow!("failed to decode {}: {}", src.display(), e))?;
    if !matches!(orientation, image::metadata::Orientation::NoTransforms) {
        tracing::debug!("applying EXIF orientation {:?} to {}", orientation, src.display());
        img.apply_orientation(orientation);
    }
    Ok(img)
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
            out != blur_pad(&src, 384, 216, 25.0, 0.1),
            "BlurPad と一致してしまうと、この判定を検証できていない"
        );
    }

    /// 比 1.000 の画像は差 0.778 > SMART_THRESHOLD なので BlurPad 経路に入るべき。
    #[test]
    fn smart_uses_blur_pad_when_ratio_is_far() {
        let src = two_tone(200, 200);
        let out = process(&src, 384, 216, DisplayMode::Smart, 25.0, 0.1);
        assert!(
            out == blur_pad(&src, 384, 216, 25.0, 0.1),
            "比が離れているときは BlurPad と一致すべき"
        );
        assert!(out != fill(&src, 384, 216), "fill と一致してしまうと判定を検証できていない");
    }

    /// どのモードでも画面ぴったりの寸法を返すこと。
    ///
    /// 4:3 (1.333) を 16:9 (1.778) に出すので、Smart は差 0.444 で
    /// BlurPad 側に入る。寸法の正しさは解像度に依存しないため、
    /// BlurPad のぼかしが軽く済む小さい画面で検証する。
    #[test]
    fn all_modes_produce_correct_dimensions() {
        const SCREEN: (u32, u32) = (384, 216);
        let src = solid(160, 120);
        for mode in [
            DisplayMode::Fill,
            DisplayMode::Fit,
            DisplayMode::Stretch,
            DisplayMode::BlurPad,
            DisplayMode::Smart,
        ] {
            let out = process(&src, SCREEN.0, SCREEN.1, mode, 10.0, 0.1);
            assert_eq!(
                out.dimensions(),
                SCREEN,
                "mode {:?} produced wrong dimensions",
                mode
            );
        }
    }

    #[test]
    fn output_dimensions_match_screen() {
        let src = solid(800, 600);
        let out = blur_pad(&src, 1920, 1080, 10.0, 0.0);
        assert_eq!(out.dimensions(), (1920, 1080));
    }

    #[test]
    fn handles_portrait_source_on_landscape_screen() {
        let src = solid(600, 1200);
        let out = blur_pad(&src, 1920, 1080, 10.0, 0.1);
        assert_eq!(out.dimensions(), (1920, 1080));
    }

    #[test]
    fn handles_landscape_source_on_portrait_screen() {
        let src = solid(1200, 600);
        let out = blur_pad(&src, 1080, 1920, 10.0, 0.0);
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
