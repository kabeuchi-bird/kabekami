//! Bing Daily 壁紙プロバイダー。
//!
//! `https://www.bing.com/HPImageArchive.aspx` から最大 8 枚の壁紙 URL を取得する。
//! API キー不要。
//!
//! - `locale` 設定（例: `"ja-JP"`）で言語・地域の壁紙を選択できる（デフォルト: `"en-US"`）
//! - 4K 以上の画面では UHD 版（3840×2160）をダウンロードする

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use url::Url;

use kabekami_common::config::OnlineSourceConfig;

use super::{download_image, FetchContext};

const BASE_URL: &str = "https://www.bing.com/";
/// `urlbase` の resolve 後にこのホスト以外を返したら拒否する。
/// Bing API が改ざんされた `urlbase`（例: `//attacker/`）を返しても外部ホストには辿り着かない。
const ALLOWED_HOST: &str = "www.bing.com";

#[derive(Deserialize)]
struct BingResponse {
    images: Vec<BingImage>,
}

#[derive(Deserialize)]
struct BingImage {
    /// `/th?id=OHR.SomeName_EN-US0000000000` 形式のベースパス
    urlbase: String,
    /// `"20240115"` 形式の日付文字列（ファイル名に使用）
    startdate: String,
}

/// 画面サイズから適切な解像度サフィックスを返す。
/// 幅 3840 px 以上、または高さ 2160 px 以上を 4K と判定して UHD 版を選択する。
fn resolution_suffix(screen_w: u32, screen_h: u32) -> &'static str {
    if screen_w >= 3840 || screen_h >= 2160 { "_UHD.jpg" } else { "_1920x1080.jpg" }
}

pub async fn fetch(
    cfg: &OnlineSourceConfig,
    dir: &Path,
    client: &reqwest::Client,
    ctx: FetchContext,
) -> Result<Vec<PathBuf>> {
    let mkt = cfg.locale.as_deref().unwrap_or("en-US");
    let n = cfg.count.clamp(1, 8); // Bing API は最大 8 枚

    let api_url = format!(
        "https://www.bing.com/HPImageArchive.aspx?format=js&idx=0&n={}&mkt={}",
        n, mkt
    );

    let resp: BingResponse = client
        .get(&api_url)
        .send()
        .await?
        .json()
        .await
        .context("failed to parse Bing API response")?;

    let res_suffix = resolution_suffix(ctx.screen_w, ctx.screen_h);
    let base = Url::parse(BASE_URL).context("BASE_URL is invalid (compile-time bug)")?;
    let mut available = Vec::new();

    for img in &resp.images {
        // startdate はサーバー由来のため英数字のみ残してサニタイズ
        let safe_date: String =
            img.startdate.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        let filename = format!("bing_{}.jpg", safe_date);
        let dest = dir.join(&filename);

        if dest.exists() {
            available.push(dest);
            continue;
        }

        // `urlbase + res_suffix` を `base` 相対で resolve し、ホストが Bing 以外なら拒否する。
        // 単純な文字列連結だと `urlbase = "//attacker/foo"` で別ホストへ誘導されうるため。
        let relative = format!("{}{}", img.urlbase, res_suffix);
        let url = match base.join(&relative) {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!("bing: malformed urlbase {:?}: {}", img.urlbase, e);
                continue;
            }
        };
        if url.host_str() != Some(ALLOWED_HOST) {
            tracing::warn!(
                "bing: urlbase resolved to unexpected host {:?}, skipping",
                url.host_str()
            );
            continue;
        }

        match download_image(client, url.as_str(), &dest).await {
            Ok(()) => {
                tracing::debug!("bing: downloaded {}", dest.display());
                available.push(dest);
            }
            Err(e) => tracing::warn!("bing: failed to download {}: {:#}", url, e),
        }
    }

    Ok(available)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_suffix_fhd() {
        assert_eq!(resolution_suffix(1920, 1080), "_1920x1080.jpg");
        assert_eq!(resolution_suffix(2560, 1440), "_1920x1080.jpg");
        assert_eq!(resolution_suffix(3839, 2159), "_1920x1080.jpg");
    }

    #[test]
    fn resolution_suffix_uhd_by_width() {
        assert_eq!(resolution_suffix(3840, 2160), "_UHD.jpg");
        assert_eq!(resolution_suffix(4096, 2160), "_UHD.jpg");
        assert_eq!(resolution_suffix(7680, 4320), "_UHD.jpg");
    }

    #[test]
    fn resolution_suffix_uhd_by_height() {
        // 幅が 4K 未満でも高さが 2160 以上なら UHD（縦長 4K モニタなど）
        assert_eq!(resolution_suffix(2560, 2160), "_UHD.jpg");
    }

    #[test]
    fn startdate_sanitization() {
        let safe: String = "20240115".chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        assert_eq!(safe, "20240115");

        // パス区切り文字が含まれていても除去される
        let safe: String = "2024/01/15".chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        assert_eq!(safe, "20240115");

        let safe: String = "../etc/passwd".chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        assert_eq!(safe, "etcpasswd");
    }
}
