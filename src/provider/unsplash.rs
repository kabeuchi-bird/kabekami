//! Unsplash 壁紙プロバイダー。
//!
//! Unsplash API v1 でランダム写真を取得する。
//! `api_key` (Access Key) が必須。無料プランは 50 リクエスト/時間。
//!
//! ドキュメント: https://unsplash.com/documentation#get-a-random-photo

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use kabekami_common::config::OnlineSourceConfig;

use super::download_image;

const API_URL: &str = "https://api.unsplash.com/photos/random";

#[derive(Deserialize)]
struct UnsplashPhoto {
    id: String,
    urls: UnsplashUrls,
}

#[derive(Deserialize)]
struct UnsplashUrls {
    /// フルサイズ（非圧縮）URL
    full: String,
}

pub async fn fetch(
    cfg: &OnlineSourceConfig,
    dir: &Path,
    client: &reqwest::Client,
) -> Result<Vec<PathBuf>> {
    let api_key = cfg
        .api_key
        .as_deref()
        .filter(|k| !k.is_empty())
        .context("unsplash: api_key is required")?;

    let query = cfg.query.as_deref().unwrap_or("wallpaper");

    // count は 1〜30 の範囲（API 制限）
    let count = cfg.count.clamp(1, 30);

    let photos: Vec<UnsplashPhoto> = client
        .get(API_URL)
        .query(&[
            ("count", count.to_string()),
            ("query", query.to_string()),
            ("orientation", "landscape".to_string()),
            ("client_id", api_key.to_string()),
        ])
        .send()
        .await?
        .json()
        .await
        .context("failed to parse Unsplash API response")?;

    let mut available = Vec::new();

    for photo in &photos {
        let filename = format!("unsplash_{}.jpg", photo.id);
        let dest = dir.join(&filename);

        if dest.exists() {
            available.push(dest);
            continue;
        }

        match download_image(client, &photo.urls.full, &dest).await {
            Ok(()) => {
                tracing::debug!("unsplash: downloaded {}", dest.display());
                available.push(dest);
            }
            Err(e) => tracing::warn!("unsplash: failed {}: {:#}", photo.id, e),
        }
    }

    Ok(available)
}
