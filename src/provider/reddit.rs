//! Reddit 壁紙プロバイダー。
//!
//! 指定サブレディット（デフォルト: `wallpapers`）の週間トップ投稿から
//! 直接リンク画像だけをフィルタしてダウンロードする。
//! API キー不要。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use kabekami_common::config::OnlineSourceConfig;

use super::download_image;

const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp"];

#[derive(Deserialize)]
struct RedditListing {
    data: RedditListingData,
}

#[derive(Deserialize)]
struct RedditListingData {
    children: Vec<RedditChild>,
}

#[derive(Deserialize)]
struct RedditChild {
    data: RedditPost,
}

#[derive(Deserialize)]
struct RedditPost {
    /// 投稿 ID（例: `t3_abc123`）
    name: String,
    url: String,
    #[serde(default)]
    post_hint: String,
    #[serde(default)]
    is_self: bool,
}

pub async fn fetch(
    cfg: &OnlineSourceConfig,
    dir: &Path,
    client: &reqwest::Client,
) -> Result<Vec<PathBuf>> {
    let subreddit = cfg.subreddit.as_deref().unwrap_or("wallpapers");
    let fetch_limit = (cfg.count * 3).min(100); // 非画像投稿を除外するために多めに取得

    let api_url = format!(
        "https://www.reddit.com/r/{}/top.json?t=week&limit={}",
        subreddit, fetch_limit,
    );

    let listing: RedditListing = client
        .get(&api_url)
        .header("Accept", "application/json")
        .send()
        .await?
        .json()
        .await
        .context("failed to parse Reddit API response")?;

    let target = cfg.count as usize;
    let mut available = Vec::new();

    for child in &listing.data.children {
        if available.len() >= target {
            break;
        }
        let post = &child.data;
        if post.is_self {
            continue;
        }

        // 直接画像 URL のみ対象（`post_hint == "image"` か URL 末尾で判定）
        let url_lower = post.url.to_lowercase();
        let is_image = (post.post_hint == "image" || post.post_hint.is_empty())
            && IMAGE_EXTS.iter().any(|ext| url_lower.ends_with(ext));
        if !is_image {
            continue;
        }

        let ext = post.url.rsplit('.').next().unwrap_or("jpg");
        let id = post.name.trim_start_matches("t3_");
        let filename = format!("reddit_{}.{}", id, ext);
        let dest = dir.join(&filename);

        if dest.exists() {
            available.push(dest);
            continue;
        }

        match download_image(client, &post.url, &dest).await {
            Ok(()) => {
                tracing::debug!("reddit: downloaded {}", dest.display());
                available.push(dest);
            }
            Err(e) => tracing::warn!("reddit: failed {}: {:#}", post.url, e),
        }
    }

    Ok(available)
}
