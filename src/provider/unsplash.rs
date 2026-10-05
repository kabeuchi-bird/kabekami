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

use super::{ensure_downloaded, get_json};

const API_URL: &str = "https://api.unsplash.com/photos/random";

#[derive(Deserialize)]
struct UnsplashPhoto {
    id: String,
    urls: UnsplashUrls,
}

#[derive(Deserialize)]
struct UnsplashUrls {
    /// 1080p 相当の圧縮 URL（デフォルト）
    regular: String,
    /// フルサイズ（非圧縮）URL。`quality = "full"` 設定時に使用。
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

    // API キーは URL クエリ (`client_id`) ではなく Authorization ヘッダで送る。
    // クエリだと `RUST_LOG=reqwest=debug` 等で URL がログ出力された際に
    // キーが平文で漏洩しうるため、Unsplash 公式が推奨するヘッダ送信を採用。
    let photos: Vec<UnsplashPhoto> = get_json(
        client
            .get(API_URL)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Client-ID {}", api_key),
            )
            .query(&[
                ("count", count.to_string()),
                ("query", query.to_string()),
                ("orientation", "landscape".to_string()),
            ]),
        "unsplash",
    )
    .await?;

    // quality = "full" のみフルサイズ。デフォルトは regular（1080p 相当、容量が 1/10 程度）。
    let use_full = cfg.quality.as_deref() == Some("full");

    let mut available = Vec::new();

    for photo in &photos {
        let filename = format!("unsplash_{}.jpg", photo.id);
        let url = if use_full { &photo.urls.full } else { &photo.urls.regular };
        available.extend(ensure_downloaded(client, "unsplash", url, dir.join(&filename)).await);
    }

    Ok(available)
}
