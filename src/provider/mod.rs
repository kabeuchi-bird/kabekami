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

use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use futures_util::StreamExt as _;
use reqwest::redirect;
use tokio::io::AsyncWriteExt as _;

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

/// ダウンロード 1 ファイルあたりの最大バイト数。
/// `Content-Length` ヘッダを信用せず、実受信バイト数で強制する。
const MAX_BYTES: u64 = 50 * 1024 * 1024;

async fn try_download(client: &reqwest::Client, url: &str, dest: &Path) -> Result<()> {
    // pre-request: 接続を確立する前に URL のホストを検証する。
    // 内部ネットワーク向け URL がプロバイダー応答に紛れ込んでも、TCP コネクション自体を張らない。
    let parsed = url::Url::parse(url).with_context(|| format!("invalid URL: {}", url))?;
    if let Some(host) = parsed.host_str() {
        if is_private_host(host) {
            anyhow::bail!("refusing to connect to private host {}", host);
        }
    }

    let resp = client.get(url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {} downloading {}", status, url);
    }

    // post-request: リダイレクト後の最終 URL のホストも再検証。
    // make_client() の redirect ポリシーで既に拒否しているが、二重防衛。
    if let Some(host) = resp.url().host_str() {
        if is_private_host(host) {
            anyhow::bail!("refusing to read response from private host {}", host);
        }
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

    // Content-Length が信用できる場合の早期拒否（無駄な接続維持を避ける）
    if let Some(len) = resp.content_length() {
        if len > MAX_BYTES {
            anyhow::bail!("response too large ({} bytes, claimed) for {}", len, url);
        }
    }

    // ストリーミング受信: チャンクごとに上限チェックして直接 tmp に書き出す。
    // 全体を一度にメモリへ載せないので、悪意のあるサーバが Content-Length を偽って
    // 巨大なボディを送ってきても OOM にならない。
    let tmp = dest.with_extension("tmp");
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut received: u64 = 0;
    let mut stream = resp.bytes_stream();
    let result: Result<()> = async {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            received = received.saturating_add(chunk.len() as u64);
            if received > MAX_BYTES {
                anyhow::bail!("response too large (> {} bytes) for {}", MAX_BYTES, url);
            }
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        Ok(())
    }
    .await;

    drop(file);
    if let Err(e) = result {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    tokio::fs::rename(&tmp, dest).await?;
    Ok(())
}

/// `reqwest::Client` を生成する。`reqwest::Client` は内部で `Arc` を使っているため
/// `.clone()` は安価で、外側の `Arc` ラップは不要。
///
/// SSRF 対策:
/// - リダイレクトは最大 5 回まで（古典的な無限リダイレクトループ防止）
/// - 各リダイレクト先のホストが loopback / private / link-local / multicast
///   等の内部ネットワークアドレスならエラー
pub fn make_client() -> Result<reqwest::Client> {
    let policy = redirect::Policy::custom(|attempt| {
        const MAX_REDIRECTS: usize = 5;
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error("too many redirects");
        }
        let host = attempt.url().host_str().map(|h| h.to_string());
        if let Some(host) = host {
            if is_private_host(&host) {
                return attempt.error(format!(
                    "refusing to follow redirect to private host {}",
                    host
                ));
            }
        }
        attempt.follow()
    });

    let client = reqwest::Client::builder()
        .user_agent(concat!(
            "kabekami/",
            env!("CARGO_PKG_VERSION"),
            " (Linux wallpaper tool)"
        ))
        .timeout(Duration::from_secs(30))
        .redirect(policy)
        .build()?;
    Ok(client)
}

/// ホスト名 / IP リテラルが「内部ネットワーク向け」かを判定する。
///
/// 真を返す場合: loopback (127.0.0.0/8, ::1), private (RFC1918, ULA),
/// link-local, unspecified (0.0.0.0, ::), broadcast, multicast, `localhost`。
/// この判定はリダイレクトに対する SSRF 防御で使う。
fn is_private_host(host: &str) -> bool {
    // IPv6 リテラルの `[::1]` を除いた素のアドレスを取り出す
    let bare = host.trim_start_matches('[').trim_end_matches(']');

    if bare.eq_ignore_ascii_case("localhost") {
        return true;
    }

    if let Ok(ip) = bare.parse::<Ipv4Addr>() {
        return is_private_ipv4(ip);
    }

    if let Ok(ip) = bare.parse::<Ipv6Addr>() {
        // IPv4-mapped IPv6 (`::ffff:127.0.0.1` 等) は埋め込み IPv4 で判定する。
        // これを忘れると `::ffff:127.0.0.1` のようなアドレスで loopback への
        // 接続を許してしまう。`Ipv6Addr::to_ipv4_mapped` は MSRV 1.75 に
        // 含まれていないため手動で展開する。
        if let Some(v4) = ipv4_mapped(&ip) {
            return is_private_ipv4(v4);
        }
        if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
            return true;
        }
        let s = ip.segments();
        // ULA fc00::/7
        if (s[0] & 0xfe00) == 0xfc00 {
            return true;
        }
        // Link-local fe80::/10
        if (s[0] & 0xffc0) == 0xfe80 {
            return true;
        }
        return false;
    }

    false
}

fn is_private_ipv4(ip: Ipv4Addr) -> bool {
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
}

/// IPv6 が IPv4-mapped 形式 (`::ffff:a.b.c.d`) なら埋め込み IPv4 を返す。
/// `Ipv6Addr::to_ipv4_mapped()` は Rust 1.80 で安定化されたため、MSRV 1.75 互換で手書き。
fn ipv4_mapped(ip: &Ipv6Addr) -> Option<Ipv4Addr> {
    let s = ip.segments();
    if s[0] == 0 && s[1] == 0 && s[2] == 0 && s[3] == 0 && s[4] == 0 && s[5] == 0xffff {
        Some(Ipv4Addr::new(
            (s[6] >> 8) as u8,
            (s[6] & 0xff) as u8,
            (s[7] >> 8) as u8,
            (s[7] & 0xff) as u8,
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod download_tests {
    use super::is_private_host;

    #[test]
    fn rejects_loopback_and_localhost() {
        assert!(is_private_host("localhost"));
        assert!(is_private_host("LOCALHOST"));
        assert!(is_private_host("127.0.0.1"));
        assert!(is_private_host("127.1.2.3"));
        assert!(is_private_host("::1"));
        assert!(is_private_host("[::1]"));
    }

    #[test]
    fn rejects_private_ipv4() {
        assert!(is_private_host("10.0.0.1"));
        assert!(is_private_host("172.16.0.1"));
        assert!(is_private_host("172.31.255.255"));
        assert!(is_private_host("192.168.1.1"));
        assert!(is_private_host("169.254.1.1")); // link-local
        assert!(is_private_host("0.0.0.0"));
        assert!(is_private_host("255.255.255.255")); // broadcast
        assert!(is_private_host("224.0.0.1")); // multicast
    }

    #[test]
    fn rejects_private_ipv6() {
        assert!(is_private_host("fc00::1")); // ULA
        assert!(is_private_host("fd12:3456::1")); // ULA
        assert!(is_private_host("fe80::1")); // link-local
        assert!(is_private_host("::")); // unspecified
        assert!(is_private_host("ff02::1")); // multicast
    }

    #[test]
    fn accepts_public_hosts() {
        assert!(!is_private_host("www.bing.com"));
        assert!(!is_private_host("api.unsplash.com"));
        assert!(!is_private_host("8.8.8.8"));
        assert!(!is_private_host("1.1.1.1"));
        assert!(!is_private_host("2606:4700:4700::1111")); // Cloudflare IPv6
    }

    #[test]
    fn rejects_ipv4_mapped_ipv6_private() {
        // IPv4-mapped 形式で SSRF を試みるパターンの拒否
        assert!(is_private_host("::ffff:127.0.0.1"));
        assert!(is_private_host("::ffff:192.168.1.1"));
        assert!(is_private_host("[::ffff:10.0.0.1]"));
        assert!(is_private_host("::ffff:169.254.169.254")); // AWS metadata service
    }

    #[test]
    fn accepts_ipv4_mapped_ipv6_public() {
        assert!(!is_private_host("::ffff:8.8.8.8"));
        assert!(!is_private_host("::ffff:1.1.1.1"));
    }
}
