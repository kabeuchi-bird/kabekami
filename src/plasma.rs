//! KDE Plasma への壁紙反映。設計書 §6 に準拠。
//!
//! Phase 1 では CLI の `plasma-apply-wallpaperimage` を呼ぶシンプルな実装。
//! Phase 3 で D-Bus の `evaluateScript` 経路を追加する予定。

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

/// 指定された画像ファイルを KDE Plasma の壁紙に設定する。
///
/// `path` は加工済みで画面サイズに合った画像であること（BlurPad では
/// kabekami 側で画面サイズに揃えた画像を生成する）。
pub fn set_wallpaper(path: &Path) -> Result<()> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize path: {}", path.display()))?;

    tracing::debug!("plasma-apply-wallpaperimage {}", canonical.display());

    let status = Command::new("plasma-apply-wallpaperimage")
        .arg(&canonical)
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

    tracing::info!("wallpaper applied: {}", canonical.display());
    Ok(())
}
