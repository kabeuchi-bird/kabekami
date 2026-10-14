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

use super::{ensure_downloaded, get_json, image_path, safe_ext};

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

/// `X-API-Key` に載せる値。sensitive にしておくと、リクエストやヘッダーを
/// Debug 出力してもキーが伏せられる。
fn api_key_header(key: &str) -> Result<reqwest::header::HeaderValue> {
    let mut value = reqwest::header::HeaderValue::from_str(key)
        .context("wallhaven: api_key contains characters not allowed in a header")?;
    value.set_sensitive(true);
    Ok(value)
}

/// `purity` 設定を API の値（SFW / Sketchy / NSFW のビット列）にする。指定した
/// 段階までを含める。NSFW はキーが無いと API が返さないので、黙って SFW だけに
/// ならないようエラーにする。
fn purity_bits(purity: Option<&str>, has_key: bool) -> Result<&'static str> {
    match purity.unwrap_or("sfw") {
        "sfw" => Ok("100"),
        "sketchy" => Ok("110"),
        "nsfw" if has_key => Ok("111"),
        "nsfw" => anyhow::bail!("wallhaven: purity = \"nsfw\" requires api_key"),
        other => anyhow::bail!(
            "wallhaven: unknown purity {other:?} (expected \"sfw\", \"sketchy\" or \"nsfw\")"
        ),
    }
}

pub async fn fetch(
    cfg: &OnlineSourceConfig,
    dir: &Path,
    client: &reqwest::Client,
) -> Result<Vec<PathBuf>> {
    let query = cfg.query.as_deref().unwrap_or("nature");
    let api_key = cfg.api_key.as_deref().filter(|k| !k.is_empty());
    let purity = purity_bits(cfg.purity.as_deref(), api_key.is_some())?;

    let params: Vec<(&str, String)> = vec![
        ("q", query.to_string()),
        ("sorting", "toplist".to_string()),
        ("purity", purity.to_string()),
        ("categories", "111".to_string()), // general + anime + people
        ("atleast", "1920x1080".to_string()),
        ("per_page", cfg.count.clamp(1, 24).to_string()),
    ];
    // キーはクエリ (`apikey`) ではなくヘッダで送る。reqwest のエラー表示は URL を
    // 含むため、クエリに載せると通信失敗の warn（と WARN 通知）にキーが出る（#57）。
    // 独自ヘッダはリダイレクト先にも転送されるので、キーを付けるときは
    // リダイレクトを追わないクライアントで送る。
    let req = match api_key {
        Some(key) => super::make_no_redirect_client()?
            .get(API_URL)
            .query(&params)
            .header("X-API-Key", api_key_header(key)?),
        None => client.get(API_URL).query(&params),
    };

    let resp: WallhavenResponse = get_json(req, "wallhaven").await?;

    let mut available = Vec::new();

    for img in &resp.data {
        let Some(dest) = image_path(dir, "wallhaven", &img.id, &safe_ext(&img.path)) else { continue };
        available.extend(ensure_downloaded(client, "wallhaven", &img.path, dest).await);
    }

    Ok(available)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purity_defaults_to_sfw_and_nsfw_needs_a_key() {
        assert_eq!(purity_bits(None, false).unwrap(), "100");
        assert_eq!(purity_bits(Some("sketchy"), false).unwrap(), "110");
        assert_eq!(purity_bits(Some("nsfw"), true).unwrap(), "111");
        assert!(purity_bits(Some("nsfw"), false).is_err());
        assert!(purity_bits(Some("all"), true).is_err());
    }

    /// Debug 出力（診断ログ等）にキーが出ないこと。
    #[test]
    fn api_key_header_is_hidden_from_debug_output() {
        let value = api_key_header("secret-key-123").unwrap();
        assert!(value.is_sensitive());
        assert!(!format!("{value:?}").contains("secret-key-123"));
    }
}
