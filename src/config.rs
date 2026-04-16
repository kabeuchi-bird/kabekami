//! 設定ファイル（`~/.config/kabekami/config.toml`）の読み込み。
//!
//! 設計書 §8 に準拠。Phase 1 では cache / display / rotation / sources のみ
//! 必要。Phase 2 以降で参照されるフィールドもここで定義しておく。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// 壁紙切り替え間隔の下限（秒）。Plasma のフェードアニメーションとの
/// 干渉を避けるため設計書 §5a で定められた値。
pub const MIN_INTERVAL_SECS: u64 = 5;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub sources: Sources,
    #[serde(default)]
    pub rotation: Rotation,
    #[serde(default)]
    pub display: Display,
    #[serde(default)]
    pub cache: Cache,
    #[serde(default)]
    pub ui: Ui,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sources: Sources::default(),
            rotation: Rotation::default(),
            display: Display::default(),
            cache: Cache::default(),
            ui: Ui::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Sources {
    #[serde(default)]
    pub directories: Vec<PathBuf>,
    #[serde(default = "default_true")]
    pub recursive: bool,
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            directories: Vec::new(),
            recursive: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rotation {
    #[serde(default = "default_interval_secs")]
    pub interval_secs: u64,
    #[serde(default)]
    pub order: Order,
    #[serde(default = "default_true")]
    pub change_on_start: bool,
    #[serde(default = "default_true")]
    pub prefetch: bool,
}

impl Default for Rotation {
    fn default() -> Self {
        Self {
            interval_secs: default_interval_secs(),
            order: Order::default(),
            change_on_start: true,
            prefetch: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Order {
    Sequential,
    #[default]
    Random,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Display {
    #[serde(default)]
    pub mode: DisplayMode,
    #[serde(default = "default_blur_sigma")]
    pub blur_sigma: f32,
    #[serde(default = "default_bg_darken")]
    pub bg_darken: f32,
}

impl Default for Display {
    fn default() -> Self {
        Self {
            mode: DisplayMode::default(),
            blur_sigma: default_blur_sigma(),
            bg_darken: default_bg_darken(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayMode {
    Fill,
    Fit,
    Stretch,
    #[default]
    BlurPad,
    Smart,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Cache {
    #[serde(default = "default_cache_dir")]
    pub directory: PathBuf,
    #[serde(default = "default_max_size_mb")]
    pub max_size_mb: u64,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            directory: default_cache_dir(),
            max_size_mb: default_max_size_mb(),
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_interval_secs() -> u64 {
    1800
}
fn default_blur_sigma() -> f32 {
    25.0
}
fn default_bg_darken() -> f32 {
    0.1
}
fn default_max_size_mb() -> u64 {
    500
}
fn default_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from(".cache"))
        .join("kabekami")
}

impl Config {
    /// `~/.config/kabekami/config.toml` を読み込む。
    /// ファイルが存在しない場合はデフォルト値を返す。
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            tracing::info!(
                "config file not found, using defaults: {}",
                path.display()
            );
            let mut cfg = Self::default();
            cfg.normalize();
            return Ok(cfg);
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file: {}", path.display()))?;
        let mut cfg: Self = toml::from_str(&text)
            .with_context(|| format!("failed to parse config file: {}", path.display()))?;
        cfg.normalize();
        Ok(cfg)
    }

    pub fn config_path() -> Result<PathBuf> {
        let dir = dirs::config_dir().context("failed to determine config directory")?;
        Ok(dir.join("kabekami").join("config.toml"))
    }

    /// 設定値を正規化する。
    /// - `interval_secs` が下限未満なら補正
    /// - `~` で始まるパスをホームディレクトリに展開
    pub fn normalize(&mut self) {
        if self.rotation.interval_secs < MIN_INTERVAL_SECS {
            tracing::warn!(
                "interval_secs {} is below minimum {}, clamping",
                self.rotation.interval_secs,
                MIN_INTERVAL_SECS
            );
            self.rotation.interval_secs = MIN_INTERVAL_SECS;
        }
        self.sources.directories = self
            .sources
            .directories
            .iter()
            .map(|p| expand_tilde(p))
            .collect();
        self.cache.directory = expand_tilde(&self.cache.directory);
    }
}

/// UI 表示言語の設定。
///
/// 言語解決の優先順位（`main.rs` の `resolve_lang()` で実施）:
/// 1. 環境変数 `KABEKAMI_LANG`
/// 2. このフィールド（`config.toml` の `[ui] language`）
/// 3. デフォルト: `"ja"`（日本語）
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Ui {
    /// `"ja"` または `"en"`。空文字列はデフォルト（英語）として扱う。
    #[serde(default)]
    pub language: String,
    /// WARN レベルのログをデスクトップ通知として表示する（デフォルト: false）。
    #[serde(default)]
    pub warn_notify: bool,
}

fn expand_tilde(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    } else if s == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let toml_text = r#"
[sources]
directories = ["/tmp/a", "/tmp/b"]
recursive = false

[rotation]
interval_secs = 60
order = "sequential"
change_on_start = false
prefetch = false

[display]
mode = "smart"
blur_sigma = 10.0
bg_darken = 0.2

[cache]
directory = "/tmp/cache"
max_size_mb = 123
"#;
        let cfg: Config = toml::from_str(toml_text).unwrap();
        assert_eq!(cfg.sources.directories.len(), 2);
        assert!(!cfg.sources.recursive);
        assert_eq!(cfg.rotation.interval_secs, 60);
        assert_eq!(cfg.rotation.order, Order::Sequential);
        assert!(!cfg.rotation.change_on_start);
        assert_eq!(cfg.display.mode, DisplayMode::Smart);
        assert!((cfg.display.blur_sigma - 10.0).abs() < f32::EPSILON);
        assert!((cfg.display.bg_darken - 0.2).abs() < f32::EPSILON);
        assert_eq!(cfg.cache.max_size_mb, 123);
    }

    #[test]
    fn normalizes_low_interval() {
        let mut cfg = Config::default();
        cfg.rotation.interval_secs = 1;
        cfg.normalize();
        assert_eq!(cfg.rotation.interval_secs, MIN_INTERVAL_SECS);
    }

    #[test]
    fn defaults_match_design_doc() {
        let cfg = Config::default();
        assert_eq!(cfg.rotation.interval_secs, 1800);
        assert_eq!(cfg.rotation.order, Order::Random);
        assert!(cfg.rotation.change_on_start);
        assert!(cfg.rotation.prefetch);
        assert_eq!(cfg.display.mode, DisplayMode::BlurPad);
        assert!((cfg.display.blur_sigma - 25.0).abs() < f32::EPSILON);
        assert!((cfg.display.bg_darken - 0.1).abs() < f32::EPSILON);
        assert_eq!(cfg.cache.max_size_mb, 500);
    }
}
