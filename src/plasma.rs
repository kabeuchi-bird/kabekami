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
//! for (const desktop of desktops()) {
//!     if (desktop.screen === -1) continue;
//!     desktop.wallpaperPlugin = "org.kde.image";
//!     desktop.currentConfigGroup = ["Wallpaper", "org.kde.image", "General"];
//!     desktop.writeConfig("Image", "file:///path/to/image.webp");
//! }
//! ```

use std::path::Path;
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
    /// その場合 `set_wallpaper` は CLI フォールバックを使用する。
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

    /// 指定された画像ファイルを KDE Plasma の壁紙に設定する（全スクリーン共通）。
    ///
    /// 1. D-Bus `evaluateScript` を試みる（高速・確実）
    /// 2. 失敗した場合は `plasma-apply-wallpaperimage` CLI にフォールバック
    pub async fn set_wallpaper(&self, path: &Path) -> Result<()> {
        let canonical = path
            .canonicalize()
            .with_context(|| format!("failed to canonicalize path: {}", path.display()))?;

        if let Some(ref conn) = self.conn {
            match set_wallpaper_dbus(&canonical, conn).await {
                Ok(()) => {
                    tracing::info!("wallpaper applied via D-Bus: {}", canonical.display());
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!(
                        "D-Bus evaluateScript failed ({}), falling back to CLI",
                        e
                    );
                }
            }
        }

        set_wallpaper_cli(&canonical)
    }

    /// 複数モニターに個別の壁紙を設定する。
    ///
    /// `entries` は `(screen_index, image_path)` のスライス。
    /// `screen_index` は KDE Plasma の `desktop.screen` 値（0 始まり）に対応する。
    ///
    /// 1 件の場合は `set_wallpaper()` と同等。
    /// D-Bus 失敗時は最初のエントリで CLI フォールバック。
    pub async fn set_wallpaper_multi(&self, entries: &[(usize, &Path)]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        if entries.len() == 1 {
            return self.set_wallpaper(entries[0].1).await;
        }

        let canonical: Vec<(usize, std::path::PathBuf)> = entries
            .iter()
            .map(|(idx, p)| {
                p.canonicalize()
                    .with_context(|| format!("failed to canonicalize path: {}", p.display()))
                    .map(|c| (*idx, c))
            })
            .collect::<Result<_>>()?;

        if let Some(ref conn) = self.conn {
            match set_wallpaper_multi_dbus(&canonical, conn).await {
                Ok(()) => {
                    tracing::info!("wallpaper set on {} screen(s) via D-Bus", canonical.len());
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!("D-Bus multi-wallpaper failed ({}), falling back to CLI", e);
                }
            }
        }

        // CLI フォールバック: 最初のエントリを全スクリーンに適用
        if let Some((_, path)) = canonical.first() {
            set_wallpaper_cli(path)?;
        }
        Ok(())
    }
}

/// D-Bus `org.kde.PlasmaShell::evaluateScript` 経由で壁紙を設定する。
async fn set_wallpaper_dbus(path: &Path, conn: &zbus::Connection) -> Result<()> {
    let path_str = path.to_string_lossy();
    // JS 文字列として安全にエスケープ（`\` `"` `\n` `\r` が対象）
    let escaped = path_str
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r");

    let script = format!(
        r#"for (const desktop of desktops()) {{
    if (desktop.screen === -1) continue;
    desktop.wallpaperPlugin = "org.kde.image";
    desktop.currentConfigGroup = ["Wallpaper", "org.kde.image", "General"];
    desktop.writeConfig("Image", "file://{}");
}}"#,
        escaped
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

/// D-Bus `org.kde.PlasmaShell::evaluateScript` 経由でスクリーンごとに壁紙を設定する。
async fn set_wallpaper_multi_dbus(
    entries: &[(usize, std::path::PathBuf)],
    conn: &zbus::Connection,
) -> Result<()> {
    let map_entries: String = entries
        .iter()
        .map(|(idx, path)| {
            let escaped = path
                .to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r");
            format!("\"{idx}\": \"file://{escaped}\"")
        })
        .collect::<Vec<_>>()
        .join(", ");

    let script = format!(
        r#"const wallpapers = {{{map_entries}}};
for (const desktop of desktops()) {{
    if (desktop.screen === -1) continue;
    const p = wallpapers[String(desktop.screen)];
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
