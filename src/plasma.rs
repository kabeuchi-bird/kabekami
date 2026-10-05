//! KDE Plasma への壁紙反映。
//!
//! D-Bus の `org.kde.PlasmaShell::evaluateScript` を一次手段とし、
//! 失敗した場合は `plasma-apply-wallpaperimage` CLI にフォールバックする。
//!
//! ## D-Bus スクリプト
//!
//! `evaluateScript` に渡す JavaScript は全デスクトップをイテレートして
//! 壁紙プラグインと画像パスを設定する:
//!
//! ```js
//! const wallpapers = {"0": "file:///a.webp", "1": "file:///b.webp"}; // 1 枚なら {"*": ...}
//! for (const desktop of desktops()) {
//!     if (desktop.screen === -1) continue;
//!     const p = wallpapers[String(desktop.screen)] || wallpapers["*"];
//!     if (!p) continue;
//!     desktop.wallpaperPlugin = "org.kde.image";
//!     desktop.currentConfigGroup = ["Wallpaper", "org.kde.image", "General"];
//!     desktop.writeConfig("Image", p);
//! }
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

/// KDE Plasma への壁紙適用ハンドル。
///
/// D-Bus セッション接続を保持して再利用することで、壁紙を設定するたびに
/// 接続を張り直すオーバーヘッドを排除する。
pub struct PlasmaShell {
    /// セッションバス接続。D-Bus が利用不可の場合は `None`（CLI フォールバックを使用）。
    conn: Option<zbus::Connection>,
}

impl PlasmaShell {
    /// セッションバスへの接続を試みて初期化する。
    ///
    /// D-Bus が利用できない場合はログを出して `conn = None` で初期化する。
    /// その場合 `set_wallpaper_multi` は CLI フォールバックを使用する。
    pub async fn new() -> Self {
        match zbus::Connection::session().await {
            Ok(conn) => {
                tracing::debug!("PlasmaShell: D-Bus session connected");
                Self { conn: Some(conn) }
            }
            Err(e) => {
                tracing::warn!(
                    "PlasmaShell: D-Bus session unavailable ({}); will use CLI fallback",
                    e
                );
                Self { conn: None }
            }
        }
    }

    /// モニターごとに壁紙を設定する。
    ///
    /// `entries` は `(screen_index, image_path)` のスライス。1 件だけなら
    /// 全スクリーンに同じ画像を設定する。
    ///
    /// 1. D-Bus `evaluateScript` を試みる（高速・確実）
    /// 2. 失敗した場合は `plasma-apply-wallpaperimage` CLI で最初のエントリを全スクリーンに適用
    pub async fn set_wallpaper_multi(&self, entries: &[(usize, &Path)]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let canonical: Vec<(usize, PathBuf)> = entries
            .iter()
            .map(|(idx, p)| {
                let c = if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    p.canonicalize()
                        .with_context(|| format!("failed to canonicalize path: {}", p.display()))?
                };
                Ok((*idx, c))
            })
            .collect::<Result<_>>()?;

        if let Some(ref conn) = self.conn {
            match set_wallpaper_dbus(&canonical, conn).await {
                Ok(()) => {
                    tracing::info!("wallpaper set on {} screen(s) via D-Bus", canonical.len());
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!("D-Bus evaluateScript failed ({}), falling back to CLI", e);
                }
            }
        }

        // 外部コマンドの終了待ちは単一ワーカーを止めるので逃がす
        // （D-Bus が失敗している状況でトレイまで固まらないように）
        let path = canonical[0].1.clone();
        tokio::task::spawn_blocking(move || set_wallpaper_cli(&path))
            .await
            .context("plasma-apply-wallpaperimage task panicked")?
    }
}

/// パスを JS 文字列リテラル内で安全に使えるようエスケープする。
fn escape_js_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// D-Bus `org.kde.PlasmaShell::evaluateScript` 経由でスクリーンごとに壁紙を設定する。
///
/// エントリが 1 件ならキー `"*"` に置き、全スクリーンに適用する。
async fn set_wallpaper_dbus(entries: &[(usize, PathBuf)], conn: &zbus::Connection) -> Result<()> {
    let single = entries.len() == 1;
    let map_entries: String = entries
        .iter()
        .map(|(idx, path)| {
            let escaped = escape_js_string(&path.to_string_lossy());
            let key = if single { "*".to_string() } else { idx.to_string() };
            format!("\"{key}\": \"file://{escaped}\"")
        })
        .collect::<Vec<_>>()
        .join(", ");

    let script = format!(
        r#"const wallpapers = {{{map_entries}}};
for (const desktop of desktops()) {{
    if (desktop.screen === -1) continue;
    const p = wallpapers[String(desktop.screen)] || wallpapers["*"];
    if (!p) continue;
    desktop.wallpaperPlugin = "org.kde.image";
    desktop.currentConfigGroup = ["Wallpaper", "org.kde.image", "General"];
    desktop.writeConfig("Image", p);
}}"#
    );

    conn.call_method(
        Some("org.kde.plasmashell"),
        "/PlasmaShell",
        Some("org.kde.PlasmaShell"),
        "evaluateScript",
        &(script.as_str(),),
    )
    .await
    .context("evaluateScript D-Bus call failed")?;

    Ok(())
}

/// `plasma-apply-wallpaperimage` CLI 経由で壁紙を設定する（フォールバック）。
fn set_wallpaper_cli(path: &Path) -> Result<()> {
    tracing::debug!("plasma-apply-wallpaperimage {}", path.display());

    let status = Command::new("plasma-apply-wallpaperimage")
        .arg(path)
        .status()
        .context(
            "failed to invoke `plasma-apply-wallpaperimage`. \
             Is KDE Plasma installed and in PATH?",
        )?;

    if !status.success() {
        anyhow::bail!(
            "plasma-apply-wallpaperimage exited with non-zero status: {}",
            status
        );
    }

    tracing::info!("wallpaper applied via CLI: {}", path.display());
    Ok(())
}
