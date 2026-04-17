//! Bing Daily 壁紙プロバイダー。
//!
//! `https://www.bing.com/HPImageArchive.aspx?format=js&idx=0&n=8&mkt=en-US`
//! から最大 8 枚の壁紙 URL を取得し、1920×1080 版をダウンロードする。
//! API キー不要。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use kabekami_common::config::OnlineSourceConfig;

use super::download_image;

const API_URL: &str =
    "https://www.bing.com/HPImageArchive.aspx?format=js&idx=0&n=8&mkt=en-US";
const BASE_URL: &str = "https://www.bing.com";

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

pub async fn fetch(
    cfg: &OnlineSourceConfig,
    dir: &Path,
    client: &reqwest::Client,
) -> Result<Vec<PathBuf>> {
    let resp: BingResponse = client
        .get(API_URL)
        .send()
        .await?
        .json()
        .await
        .context("failed to parse Bing API response")?;

    let count = cfg.count as usize;
    let mut available = Vec::new();

    for img in resp.images.iter().take(count) {
        // 1920×1080 版 URL: https://www.bing.com{urlbase}_1920x1080.jpg
        let url = format!("{}{}_1920x1080.jpg", BASE_URL, img.urlbase);
        let filename = format!("bing_{}.jpg", img.startdate);
        let dest = dir.join(&filename);

        if dest.exists() {
            available.push(dest);
            continue;
        }

        match download_image(client, &url, &dest).await {
            Ok(()) => {
                tracing::debug!("bing: downloaded {}", dest.display());
                available.push(dest);
            }
            Err(e) => tracing::warn!("bing: failed to download {}: {:#}", url, e),
        }
    }

    Ok(available)
}
