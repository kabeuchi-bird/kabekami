//! Wallhaven 壁紙プロバイダー。
//!
//! Wallhaven API でトップリストを取得する（デフォルト: SFW のみ）。
//! API キーは任意（NSFW コンテンツを取得する場合のみ必要）。
//!
//! ドキュメント: https://wallhaven.cc/help/api

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use kabekami_common::config::OnlineSourceConfig;

use super::{ensure_downloaded, safe_ext};

const API_URL: &str = "https://wallhaven.cc/api/v1/search";

#[derive(Deserialize)]
struct WallhavenResponse {
    data: Vec<WallhavenImage>,
}

#[derive(Deserialize)]
struct WallhavenImage {
    id: String,
    /// ダウンロード URL（例: `https://w.wallhaven.cc/full/ab/wallhaven-ab1234.jpg`）
    path: String,
}

pub async fn fetch(
    cfg: &OnlineSourceConfig,
    dir: &Path,
    client: &reqwest::Client,
) -> Result<Vec<PathBuf>> {
    let query = cfg.query.as_deref().unwrap_or("nature");

    let mut params: Vec<(&str, String)> = vec![
        ("q", query.to_string()),
        ("sorting", "toplist".to_string()),
        ("purity", "100".to_string()),    // SFW のみ
        ("categories", "111".to_string()), // general + anime + people
        ("atleast", "1920x1080".to_string()),
        ("per_page", cfg.count.clamp(1, 24).to_string()),
    ];
    if let Some(key) = cfg.api_key.as_deref().filter(|k| !k.is_empty()) {
        params.push(("apikey", key.to_string()));
    }

    let resp: WallhavenResponse = client
        .get(API_URL)
        .query(&params)
        .send()
        .await?
        .json()
        .await
        .context("failed to parse Wallhaven API response")?;

    let mut available = Vec::new();

    for img in &resp.data {
        let filename = format!("wallhaven_{}.{}", img.id, safe_ext(&img.path));
        available.extend(ensure_downloaded(client, "wallhaven", &img.path, dir.join(&filename)).await);
    }

    Ok(available)
}
