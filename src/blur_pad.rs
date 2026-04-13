//! BlurPad 画像処理パイプライン。設計書 §5 に準拠。
//!
//! 設計書の擬似コードに従い、以下の処理を行う:
//!
//! 1. 背景レイヤー: 元画像を画面サイズに cover リサイズしてクロップし、
//!    ガウスぼかしを掛ける。オプションで明度を落とす。
//! 2. 前景レイヤー: 元画像を画面内に contain するようリサイズする。
//! 3. 合成: 背景の中央に前景をオーバーレイする。
//!
//! パフォーマンス最適化（設計書 §5 "パフォーマンス考慮"）:
//! 4K 画像の大きな sigma でのぼかしは重いので、背景は 1/4 サイズに
//! 縮小してからぼかし、最後に画面サイズに拡大する。視覚差はほぼない。

use image::{
    imageops::{self, FilterType},
    DynamicImage, RgbaImage,
};

/// 背景ぼかしを行う際の縮小倍率。1/4 にしてから sigma も 1/4 に調整する。
const DOWNSCALE: u32 = 4;

/// BlurPad 画像を生成する。
///
/// # 引数
/// - `src`: 元画像
/// - `screen_w`, `screen_h`: 出力解像度（画面解像度）
/// - `blur_sigma`: フルサイズ換算でのぼかし sigma（15〜30 程度）
/// - `bg_darken`: 背景を暗くする量（0.0〜1.0）。0.1 で 10% 暗くなる。
pub fn generate_blur_pad(
    src: &DynamicImage,
    screen_w: u32,
    screen_h: u32,
    blur_sigma: f32,
    bg_darken: f32,
) -> RgbaImage {
    assert!(screen_w > 0 && screen_h > 0, "invalid screen dimensions");

    // ---- 1. 背景: 縮小 → cover → blur → 拡大 ----------------------------
    let small_w = (screen_w / DOWNSCALE).max(1);
    let small_h = (screen_h / DOWNSCALE).max(1);

    // `resize_to_fill` がアスペクト比維持の cover リサイズ＋中央クロップを
    // 一括でやってくれる（設計書の cover resize → crop_center に相当）。
    let bg_small_rgba: RgbaImage = src
        .resize_to_fill(small_w, small_h, FilterType::Triangle)
        .to_rgba8();

    // 縮小済みなので sigma もスケール。視覚的な広がりを揃える。
    let scaled_sigma = (blur_sigma / DOWNSCALE as f32).max(0.1);
    let mut bg_blurred: RgbaImage = imageops::blur(&bg_small_rgba, scaled_sigma);

    if bg_darken > 0.0 {
        darken(&mut bg_blurred, bg_darken);
    }

    // フルサイズに戻す。ぼかし済みなので Triangle で十分。
    let mut canvas: RgbaImage = DynamicImage::ImageRgba8(bg_blurred)
        .resize_exact(screen_w, screen_h, FilterType::Triangle)
        .to_rgba8();

    // ---- 2. 前景: contain リサイズ ---------------------------------------
    // `resize` はアスペクト比維持で「画面に収まる」最大サイズにする。
    let fg: RgbaImage = src
        .resize(screen_w, screen_h, FilterType::Lanczos3)
        .to_rgba8();

    // ---- 3. 合成: 中央にオーバーレイ ------------------------------------
    let (fg_w, fg_h) = fg.dimensions();
    let offset_x = ((screen_w.saturating_sub(fg_w)) / 2) as i64;
    let offset_y = ((screen_h.saturating_sub(fg_h)) / 2) as i64;
    imageops::overlay(&mut canvas, &fg, offset_x, offset_y);

    canvas
}

/// 画像の RGB 成分を `amount` の割合で暗くする（アルファは維持）。
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
