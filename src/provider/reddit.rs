//! Reddit 壁紙プロバイダー。
//!
//! 指定サブレディット（デフォルト: `wallpapers`）の週間トップ投稿から
//! 直接リンク画像だけをフィルタしてダウンロードする。
//! API キー不要。

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Deserialize;

use kabekami_common::config::OnlineSourceConfig;

use super::{ensure_downloaded, get_json, safe_ext};

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

/// サブレディット名が安全かどうかを検証する（英数字とアンダースコアのみ許可）。
fn is_valid_subreddit(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 投稿が直接リンクの画像であるかを判定する。
/// `post_hint == "image"` は `i.redd.it` 直リンクで確実。
/// それ以外は URL 末尾の拡張子で補完する。
fn is_image_post(post: &RedditPost) -> bool {
    if post.is_self {
        return false;
    }
    let url_lower = post.url.to_lowercase();
    post.post_hint == "image" || IMAGE_EXTS.iter().any(|ext| url_lower.ends_with(ext))
}

pub async fn fetch(
    cfg: &OnlineSourceConfig,
    dir: &Path,
    client: &reqwest::Client,
) -> Result<Vec<PathBuf>> {
    let subreddit = cfg.subreddit.as_deref().unwrap_or("wallpapers");
    if !is_valid_subreddit(subreddit) {
        anyhow::bail!("reddit: invalid subreddit name: {:?}", subreddit);
    }

    // 非画像投稿を除外するために多めに取得（saturating_mul で u32 オーバーフロー対策）
    let fetch_limit = cfg.count.saturating_mul(3).min(100);

    let api_url = format!("https://www.reddit.com/r/{}/top.json", subreddit);
    let listing: RedditListing = get_json(
        client
            .get(&api_url)
            .query(&[("t", "week"), ("limit", &fetch_limit.to_string())])
            .header("Accept", "application/json"),
        "reddit",
    )
    .await?;

    let target = cfg.count as usize;
    let mut available = Vec::new();

    for child in &listing.data.children {
        if available.len() >= target {
            break;
        }
        let post = &child.data;
        if !is_image_post(post) {
            continue;
        }

        // `trim_start_matches` は文字集合 `"t3_"` を繰り返し剥がしてしまうため
        // (`"tt3_abc"` → `"abc"` のような誤動作になる)、prefix 完全一致の
        // `strip_prefix` を使う。
        let id = post.name.strip_prefix("t3_").unwrap_or(&post.name);
        let filename = format!("reddit_{}.{}", id, safe_ext(&post.url));
        available.extend(ensure_downloaded(client, "reddit", &post.url, dir.join(&filename)).await);
    }

    Ok(available)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subreddit_validation_valid() {
        assert!(is_valid_subreddit("wallpapers"));
        assert!(is_valid_subreddit("Wallpaper_2560x1440"));
        assert!(is_valid_subreddit("EarthPorn"));
    }

    #[test]
    fn subreddit_validation_invalid() {
        assert!(!is_valid_subreddit(""));
        assert!(!is_valid_subreddit("wallpapers?t=all&hack=1"));
        assert!(!is_valid_subreddit("../etc/passwd"));
        assert!(!is_valid_subreddit("wall papers"));
    }

    fn make_post(post_hint: &str, url: &str, is_self: bool) -> RedditPost {
        RedditPost {
            name: "t3_abc".into(),
            url: url.into(),
            post_hint: post_hint.into(),
            is_self,
        }
    }

    #[test]
    fn image_filter_post_hint_image() {
        assert!(is_image_post(&make_post("image", "https://i.redd.it/someid", false)));
    }

    #[test]
    fn image_filter_extension_fallback() {
        assert!(is_image_post(&make_post("", "https://example.com/photo.jpg", false)));
        assert!(is_image_post(&make_post("", "https://example.com/photo.PNG", false)));
    }

    #[test]
    fn image_filter_rejects_self_posts() {
        assert!(!is_image_post(&make_post("image", "https://i.redd.it/x", true)));
    }

    #[test]
    fn image_filter_rejects_non_image_links() {
        assert!(!is_image_post(&make_post("link", "https://imgur.com/gallery/abc", false)));
        assert!(!is_image_post(&make_post("hosted:video", "https://v.redd.it/x.mp4", false)));
        assert!(!is_image_post(&make_post("", "https://example.com/video.mp4", false)));
    }
}
