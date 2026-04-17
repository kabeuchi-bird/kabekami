//! オンライン壁紙プロバイダー。設計書 §14 Phase 4 に準拠。
//!
//! ## 対応プロバイダー
//! | プロバイダー | API キー | 説明 |
//! |------------|---------|------|
//! | Bing Daily | 不要 | 毎日更新の Bing 壁紙（最大 8 枚） |
//! | Unsplash   | 必須 | 高品質フリー写真（クエリ指定可） |
//! | Wallhaven  | 任意 | アニメ・自然・都市など豊富なカテゴリ |
//! | Reddit     | 不要 | 指定サブレディットの上位投稿画像 |

pub mod bing;
pub mod reddit;
pub mod unsplash;
pub mod wallhaven;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::Result;
use kabekami_common::config::OnlineSourceConfig;

/// プロバイダーが新たにダウンロードした画像のパスと件数。
pub struct FetchResult {
    pub provider: String,
    pub new_paths: Vec<PathBuf>,
}

/// 各プロバイダーを確認し、再取得が必要なものだけフェッチして結果を返す。
///
/// ネットワークエラーは warning としてログに記録するだけで、他のプロバイダーの
/// 処理は継続する。
pub async fn fetch_all_due(
    configs: &[OnlineSourceConfig],
    client: &reqwest::Client,
) -> Vec<FetchResult> {
    let mut results = Vec::new();
    for cfg in configs {
        if !cfg.enabled {
            continue;
        }
        if !is_fetch_due(cfg) {
            tracing::debug!(
                "provider {}: not due yet (interval={}h)",
                cfg.provider,
                cfg.effective_interval_hours()
            );
            continue;
        }

        let dir = cfg.resolved_download_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!("provider {}: cannot create dir {}: {}", cfg.provider, dir.display(), e);
            continue;
        }

        let provider_name = cfg.provider.to_string();
        tracing::info!("provider {}: fetching…", provider_name);

        match fetch_one(cfg, &dir, client).await {
            Ok(paths) => {
                mark_fetch_done(cfg);
                tracing::info!(
                    "provider {}: {} image(s) available",
                    provider_name,
                    paths.len()
                );
                results.push(FetchResult {
                    provider: provider_name,
                    new_paths: paths,
                });
            }
            Err(e) => {
                tracing::warn!("provider {}: fetch failed: {:#}", provider_name, e);
            }
        }
    }
    results
}

async fn fetch_one(
    cfg: &OnlineSourceConfig,
    dir: &Path,
    client: &reqwest::Client,
) -> Result<Vec<PathBuf>> {
    use kabekami_common::config::ProviderKind;
    match cfg.provider {
        ProviderKind::Bing => bing::fetch(cfg, dir, client).await,
        ProviderKind::Unsplash => unsplash::fetch(cfg, dir, client).await,
        ProviderKind::Wallhaven => wallhaven::fetch(cfg, dir, client).await,
        ProviderKind::Reddit => reddit::fetch(cfg, dir, client).await,
    }
}

/// `.last_fetch` タイムスタンプを確認して再取得が必要かどうかを返す。
fn is_fetch_due(cfg: &OnlineSourceConfig) -> bool {
    let stamp = cfg.resolved_download_dir().join(".last_fetch");
    if !stamp.exists() {
        return true;
    }
    let interval_secs = cfg.effective_interval_hours() * 3600;
    let modified = std::fs::metadata(&stamp)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let elapsed = SystemTime::now()
        .duration_since(modified)
        .unwrap_or_default();
    elapsed.as_secs() >= interval_secs
}

/// `.last_fetch` タイムスタンプを更新する。
fn mark_fetch_done(cfg: &OnlineSourceConfig) {
    let stamp = cfg.resolved_download_dir().join(".last_fetch");
    if let Err(e) = std::fs::write(&stamp, b"") {
        tracing::warn!("provider {}: failed to write last_fetch stamp: {}", cfg.provider, e);
    }
}

/// HTTP GET で画像をダウンロードして `dest` に書き出す。
///
/// アトミックな書き込みのため一時ファイル経由で rename する。
pub async fn download_image(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
) -> Result<()> {
    let resp = client.get(url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {} downloading {}", status, url);
    }
    let bytes = resp.bytes().await?;

    let tmp = dest.with_extension("tmp");
    tokio::fs::write(&tmp, &bytes).await?;
    tokio::fs::rename(&tmp, dest).await?;
    Ok(())
}

/// 共有 `reqwest::Client` を生成する。
pub fn make_client() -> Result<Arc<reqwest::Client>> {
    let client = reqwest::Client::builder()
        .user_agent(concat!(
            "kabekami/",
            env!("CARGO_PKG_VERSION"),
            " (Linux wallpaper tool)"
        ))
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    Ok(Arc::new(client))
}
