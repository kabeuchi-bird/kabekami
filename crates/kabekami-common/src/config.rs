//! 設定ファイル（`~/.config/kabekami/config.toml`）の読み書き。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// 壁紙切り替え間隔の下限（秒）。
pub const MIN_INTERVAL_SECS: u64 = 5;

#[derive(Debug, Clone, Deserialize, Serialize)]
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
    /// オンライン壁紙プロバイダー設定（`[[online_sources]]` 配列）。
    #[serde(default)]
    pub online_sources: Vec<OnlineSourceConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sources: Sources::default(),
            rotation: Rotation::default(),
            display: Display::default(),
            cache: Cache::default(),
            ui: Ui::default(),
            online_sources: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Sources {
    #[serde(default)]
    pub directories: Vec<PathBuf>,
    #[serde(default = "default_true")]
    pub recursive: bool,
    /// お気に入り壁紙のコピー先ディレクトリ。`None` の場合は機能無効。
    #[serde(default)]
    pub favorites_dir: Option<PathBuf>,
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            directories: Vec::new(),
            recursive: true,
            favorites_dir: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Order {
    Sequential,
    #[default]
    Random,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayMode {
    Fill,
    Fit,
    Stretch,
    #[default]
    BlurPad,
    Smart,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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

/// UI 表示言語の設定。
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Ui {
    /// `"ja"` または `"en"`。空文字列はデフォルト（英語）として扱う。
    #[serde(default)]
    pub language: String,
    /// WARN レベルのログをデスクトップ通知として表示する（デフォルト: false）。
    #[serde(default)]
    pub warn_notify: bool,
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
    xdg_cache_dir()
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
        let dir = xdg_config_dir().context("failed to determine config directory")?;
        Ok(dir.join("kabekami").join("config.toml"))
    }

    /// 設定を TOML として `~/.config/kabekami/config.toml` に書き出す。
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        self.save_to(&path)
    }

    /// 設定を TOML として指定パスに書き出す。
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create config dir: {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self)
            .context("failed to serialize config")?;
        std::fs::write(path, text)
            .with_context(|| format!("failed to write config: {}", path.display()))?;
        Ok(())
    }

    /// 設定値を正規化する。
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
        if let Some(dir) = &self.sources.favorites_dir {
            self.sources.favorites_dir = Some(expand_tilde(dir));
        }
        self.cache.directory = expand_tilde(&self.cache.directory);
        for oc in &mut self.online_sources {
            if let Some(dir) = &oc.download_dir {
                oc.download_dir = Some(expand_tilde(dir));
            }
        }
    }
}

// ── オンラインソース ──────────────────────────────────────────────────────────

/// オンラインプロバイダー種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Bing,
    Unsplash,
    Wallhaven,
    Reddit,
}

impl ProviderKind {
    /// プロバイダーの識別名（ディレクトリ名にも使用）。
    pub fn name(self) -> &'static str {
        match self {
            Self::Bing => "bing",
            Self::Unsplash => "unsplash",
            Self::Wallhaven => "wallhaven",
            Self::Reddit => "reddit",
        }
    }

    /// デフォルトの再取得間隔（時間）。
    pub fn default_interval_hours(self) -> u64 {
        match self {
            Self::Bing => 24,
            Self::Unsplash => 24,
            Self::Wallhaven => 24,
            Self::Reddit => 1,
        }
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// オンライン壁紙ソース 1 件の設定。TOML では `[[online_sources]]` 配列。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OnlineSourceConfig {
    /// プロバイダー種別。
    pub provider: ProviderKind,
    /// 有効/無効（デフォルト: true）。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// ダウンロード先ディレクトリ。
    /// `None` の場合は `~/.local/share/kabekami/<provider>` を使用。
    #[serde(default)]
    pub download_dir: Option<PathBuf>,
    /// API キー（Unsplash: 必須、Wallhaven: NSFW 閲覧時のみ必要）。
    #[serde(default)]
    pub api_key: Option<String>,
    /// 検索クエリ（Unsplash / Wallhaven / Reddit subreddit 以外で使用）。
    #[serde(default)]
    pub query: Option<String>,
    /// 保持する画像枚数（デフォルト: 10）。
    #[serde(default = "default_online_count")]
    pub count: u32,
    /// Reddit プロバイダーで使用するサブレディット名（例: `"wallpapers"`）。
    #[serde(default)]
    pub subreddit: Option<String>,
    /// 再取得間隔の上書き（時間）。`None` の場合はプロバイダーのデフォルトを使用。
    #[serde(default)]
    pub interval_hours: Option<u64>,
    /// ロケール（Bing で使用。例: `"ja-JP"`, `"en-US"`）。デフォルト: `"en-US"`。
    #[serde(default)]
    pub locale: Option<String>,
    /// 画像品質（Unsplash で使用: `"regular"` または `"full"`）。デフォルト: `"regular"`。
    #[serde(default)]
    pub quality: Option<String>,
}

impl OnlineSourceConfig {
    /// 実際のダウンロードディレクトリを返す（`download_dir` が未設定の場合はデフォルト値）。
    pub fn resolved_download_dir(&self) -> PathBuf {
        if let Some(dir) = &self.download_dir {
            return dir.clone();
        }
        xdg_data_local_dir()
            .unwrap_or_else(|| PathBuf::from(".local/share"))
            .join("kabekami")
            .join(self.provider.name())
    }

    /// 実効的な再取得間隔（時間）。
    pub fn effective_interval_hours(&self) -> u64 {
        self.interval_hours
            .unwrap_or_else(|| self.provider.default_interval_hours())
    }
}

fn default_online_count() -> u32 {
    10
}

fn expand_tilde(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    } else if s == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    }
    path.to_path_buf()
}

fn home_dir() -> Option<PathBuf> {
    let v = std::env::var("HOME").ok()?;
    if v.is_empty() { return None; }
    Some(PathBuf::from(v))
}

fn xdg_config_dir() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("XDG_CONFIG_HOME") {
        if !v.is_empty() { return Some(PathBuf::from(v)); }
    }
    home_dir().map(|h| h.join(".config"))
}

fn xdg_cache_dir() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("XDG_CACHE_HOME") {
        if !v.is_empty() { return Some(PathBuf::from(v)); }
    }
    home_dir().map(|h| h.join(".cache"))
}

fn xdg_data_local_dir() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("XDG_DATA_HOME") {
        if !v.is_empty() { return Some(PathBuf::from(v)); }
    }
    home_dir().map(|h| h.join(".local/share"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    // Serialises all tests that mutate environment variables.
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    /// RAII guard: acquires ENV_LOCK, sets `key` to `value`, restores original on drop.
    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let mutex = ENV_LOCK.get_or_init(|| Mutex::new(()));
            let lock = mutex.lock().unwrap_or_else(|e| e.into_inner());
            let original = std::env::var(key).ok();
            // SAFETY: single-threaded thanks to the lock above.
            unsafe { std::env::set_var(key, value) };
            Self { _lock: lock, key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                // SAFETY: single-threaded thanks to the lock held in _lock.
                Some(v) => unsafe { std::env::set_var(self.key, v) },
                None    => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

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

    #[test]
    fn save_and_reload_roundtrip() {
        let dir = std::env::temp_dir().join("kabekami-config-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");

        let mut cfg = Config::default();
        cfg.rotation.interval_secs = 300;
        cfg.display.mode = DisplayMode::Fill;
        cfg.display.blur_sigma = 15.0;
        cfg.save_to(&path).unwrap();

        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.rotation.interval_secs, 300);
        assert_eq!(loaded.display.mode, DisplayMode::Fill);
        assert!((loaded.display.blur_sigma - 15.0).abs() < f32::EPSILON);
    }

    #[test]
    fn home_dir_empty_string_returns_none() {
        let _guard = EnvGuard::set("HOME", "");
        assert_eq!(home_dir(), None);
    }
}
