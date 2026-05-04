//! オンライン壁紙プロバイダー。
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
use std::time::{Duration, SystemTime};

use anyhow::Result;
use kabekami_common::config::OnlineSourceConfig;

/// 画面サイズ（ピクセル）。プロバイダーが解像度を選択するために使用する。
#[derive(Debug, Clone, Copy)]
pub struct FetchContext {
    pub screen_w: u32,
    pub screen_h: u32,
}

/// プロバイダーが新たにダウンロードした画像のパスと件数。
pub struct FetchResult {
    pub provider: String,
    pub new_paths: Vec<PathBuf>,
}

/// 各プロバイダーを並列確認し、再取得が必要なものだけフェッチして結果を返す。
///
/// `force = true` のときは `.last_fetch` タイムスタンプを無視して全プロバイダーを取得する。
/// ネットワークエラーは warning としてログに記録するだけで、他のプロバイダーの処理は継続する。
pub async fn fetch_all_due(
    configs: &[OnlineSourceConfig],
    client: &reqwest::Client,
    ctx: FetchContext,
    force: bool,
) -> Vec<FetchResult> {
    let mut set = tokio::task::JoinSet::new();

    for cfg in configs.iter().filter(|c| c.enabled) {
        let cfg = cfg.clone();
        let client = client.clone();
        set.spawn(async move { fetch_if_due(&cfg, &client, ctx, force).await });
    }

    let mut results = Vec::new();
    while let Some(res) = set.join_next().await {
        match res {
            Ok(Some(r)) => results.push(r),
            Ok(None) => {}
            Err(e) => tracing::warn!("provider task panicked: {}", e),
        }
    }
    results
}

/// 1 プロバイダーのフェッチが必要かどうかを判定し、必要なら実行する。
async fn fetch_if_due(
    cfg: &OnlineSourceConfig,
    client: &reqwest::Client,
    ctx: FetchContext,
    force: bool,
) -> Option<FetchResult> {
    if !force && !is_fetch_due(cfg).await {
        tracing::debug!(
            "provider {}: not due yet (interval={}h)",
            cfg.provider,
            cfg.effective_interval_hours()
        );
        return None;
    }

    let dir = cfg.resolved_download_dir();
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        tracing::warn!("provider {}: cannot create dir {}: {}", cfg.provider, dir.display(), e);
        return None;
    }

    let provider_name = cfg.provider.to_string();
    tracing::info!("provider {}: fetching…", provider_name);

    match fetch_one(cfg, &dir, client, ctx).await {
        Ok(paths) if !paths.is_empty() => {
            prune_dir(&dir, &paths).await;
            mark_fetch_done(cfg).await;
            tracing::info!("provider {}: {} image(s) available", provider_name, paths.len());
            Some(FetchResult { provider: provider_name, new_paths: paths })
        }
        Ok(_) => {
            tracing::warn!(
                "provider {}: fetch returned 0 images (skipping timestamp update)",
                provider_name
            );
            None
        }
        Err(e) => {
            tracing::warn!("provider {}: fetch failed: {:#}", provider_name, e);
            None
        }
    }
}

async fn fetch_one(
    cfg: &OnlineSourceConfig,
    dir: &Path,
    client: &reqwest::Client,
    ctx: FetchContext,
) -> Result<Vec<PathBuf>> {
    use kabekami_common::config::ProviderKind;
    match cfg.provider {
        ProviderKind::Bing => bing::fetch(cfg, dir, client, ctx).await,
        ProviderKind::Unsplash => unsplash::fetch(cfg, dir, client).await,
        ProviderKind::Wallhaven => wallhaven::fetch(cfg, dir, client).await,
        ProviderKind::Reddit => reddit::fetch(cfg, dir, client).await,
    }
}

/// `.last_fetch` タイムスタンプを確認して再取得が必要かどうかを返す。
async fn is_fetch_due(cfg: &OnlineSourceConfig) -> bool {
    let stamp = cfg.resolved_download_dir().join(".last_fetch");
    if !tokio::fs::try_exists(&stamp).await.unwrap_or(false) {
        return true;
    }
    let interval_secs = cfg.effective_interval_hours() * 3600;
    let modified = tokio::fs::metadata(&stamp)
        .await
        .ok()
        .and_then(|m| m.modified().ok())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let elapsed = SystemTime::now().duration_since(modified).unwrap_or_default();
    elapsed.as_secs() >= interval_secs
}

/// ダウンロードディレクトリから `keep` に含まれないファイルを削除する。
///
/// `.last_fetch` タイムスタンプと `.tmp` 一時ファイルは `keep` にないが残す。
/// フェッチ成功後に呼び出すことで、`count` を超えた古い画像を自動的に除去する。
async fn prune_dir(dir: &Path, keep: &[PathBuf]) {
    let keep_set: std::collections::HashSet<&PathBuf> = keep.iter().collect();
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else { return };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == ".last_fetch" || name.ends_with(".tmp") {
            continue;
        }
        if !keep_set.contains(&path) {
            if let Err(e) = tokio::fs::remove_file(&path).await {
                tracing::warn!("provider: failed to prune {}: {}", path.display(), e);
            } else {
                tracing::debug!("provider: pruned old image {}", path.display());
            }
        }
    }
}

/// `.last_fetch` タイムスタンプを更新する（画像を 1 枚以上取得できたときのみ呼ぶこと）。
async fn mark_fetch_done(cfg: &OnlineSourceConfig) {
    let stamp = cfg.resolved_download_dir().join(".last_fetch");
    if let Err(e) = tokio::fs::write(&stamp, b"").await {
        tracing::warn!("provider {}: failed to write last_fetch stamp: {}", cfg.provider, e);
    }
}

/// HTTP GET で画像をダウンロードして `dest` に書き出す。
///
/// - Content-Type が `text/html` / `application/json` の場合はエラー（HTML エラーページ対策）
/// - 一時ファイル経由のアトミック書き込み
/// - 最大 3 回の指数バックオフリトライ（2s → 4s）
pub async fn download_image(client: &reqwest::Client, url: &str, dest: &Path) -> Result<()> {
    let mut last_err = anyhow::anyhow!("download not attempted");
    let mut delay = Duration::from_secs(2);

    for attempt in 0..3u32 {
        match try_download(client, url, dest).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = e;
                if attempt < 2 {
                    tracing::debug!(
                        "download attempt {}/3 failed for {}: {:#}",
                        attempt + 1,
                        url,
                        last_err
                    );
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
            }
        }
    }
    Err(last_err)
}

async fn try_download(client: &reqwest::Client, url: &str, dest: &Path) -> Result<()> {
    let resp = client.get(url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {} downloading {}", status, url);
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if content_type.starts_with("text/html") || content_type.starts_with("application/json") {
        anyhow::bail!(
            "unexpected Content-Type '{}' downloading {} (HTML/JSON error page?)",
            content_type,
            url
        );
    }

    const MAX_BYTES: u64 = 50 * 1024 * 1024; // 50 MB
    if resp.content_length().unwrap_or(0) > MAX_BYTES {
        anyhow::bail!("response too large for {}", url);
    }
    let bytes = resp.bytes().await?;
    if bytes.len() as u64 > MAX_BYTES {
        anyhow::bail!("response too large ({} bytes) for {}", bytes.len(), url);
    }
    let tmp = dest.with_extension("tmp");
    tokio::fs::write(&tmp, &bytes).await?;
    tokio::fs::rename(&tmp, dest).await?;
    Ok(())
}

/// `reqwest::Client` を生成する。`reqwest::Client` は内部で `Arc` を使っているため
/// `.clone()` は安価で、外側の `Arc` ラップは不要。
pub fn make_client() -> Result<reqwest::Client> {
    let client = reqwest::Client::builder()
        .user_agent(concat!(
            "kabekami/",
            env!("CARGO_PKG_VERSION"),
            " (Linux wallpaper tool)"
        ))
        .timeout(Duration::from_secs(30))
        .build()?;
    Ok(client)
}
