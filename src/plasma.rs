//! KDE Plasma への壁紙反映。設計書 §6 に準拠。
//!
//! D-Bus の `org.kde.PlasmaShell::evaluateScript` を一次手段とし、
//! 失敗した場合は `plasma-apply-wallpaperimage` CLI にフォールバックする。
//!
//! ## D-Bus スクリプト
//!
//! `evaluateScript` に渡す JavaScript は全デスクトップをイテレートして
//! 壁紙プラグインと画像パスを設定する（設計書 §6 より）:
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

/// 指定された画像ファイルを KDE Plasma の壁紙に設定する。
///
/// 1. D-Bus `evaluateScript` を試みる（高速・確実）
/// 2. 失敗した場合は `plasma-apply-wallpaperimage` CLI にフォールバック
pub async fn set_wallpaper(path: &Path) -> Result<()> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize path: {}", path.display()))?;

    match set_wallpaper_dbus(&canonical).await {
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

    set_wallpaper_cli(&canonical)
}

/// D-Bus `org.kde.PlasmaShell::evaluateScript` 経由で壁紙を設定する。
async fn set_wallpaper_dbus(path: &Path) -> Result<()> {
    let path_str = path.to_string_lossy();

    // JS: 全デスクトップに壁紙を適用（設計書 §6）
    let script = format!(
        r#"for (const desktop of desktops()) {{
    if (desktop.screen === -1) continue;
    desktop.wallpaperPlugin = "org.kde.image";
    desktop.currentConfigGroup = ["Wallpaper", "org.kde.image", "General"];
    desktop.writeConfig("Image", "file://{}");
}}"#,
        path_str
    );

    let conn = zbus::Connection::session()
        .await
        .context("failed to connect to D-Bus session bus")?;

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
