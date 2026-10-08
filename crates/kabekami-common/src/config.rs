//! 設定ファイル（`~/.config/kabekami/config.toml`）の読み書き。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// 壁紙切り替え間隔の下限（秒）。
pub const MIN_INTERVAL_SECS: u64 = 5;

// 各セクションは `#[serde(default)]` で、欠けたキーを `Default` 実装の値で埋める。
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub sources: Sources,
    pub rotation: Rotation,
    pub display: Display,
    pub cache: Cache,
    pub ui: Ui,
    /// オンライン壁紙プロバイダー設定（`[[online_sources]]` 配列）。
    pub online_sources: Vec<OnlineSourceConfig>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Sources {
    pub directories: Vec<PathBuf>,
    pub recursive: bool,
    /// お気に入り壁紙のコピー先ディレクトリ。`None` の場合は機能無効。
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

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Rotation {
    pub interval_secs: u64,
    pub order: Order,
    pub change_on_start: bool,
    pub prefetch: bool,
}

impl Default for Rotation {
    fn default() -> Self {
        Self {
            interval_secs: 1800,
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

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Display {
    pub mode: DisplayMode,
    pub blur_sigma: f32,
    pub bg_darken: f32,
}

impl Default for Display {
    fn default() -> Self {
        Self {
            mode: DisplayMode::default(),
            blur_sigma: 25.0,
            bg_darken: 0.1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayMode {
    Fill,
    Fit,
    Stretch,
    #[default]
    BlurPad,
    Smart,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Cache {
    pub directory: PathBuf,
    pub max_size_mb: u64,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            directory: xdg_dir("XDG_CACHE_HOME", ".cache")
                .unwrap_or_else(|| PathBuf::from(".cache"))
                .join("kabekami"),
            max_size_mb: 500,
        }
    }
}

/// UI 表示言語の設定。
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Ui {
    /// `"ja"` または `"en"`。空文字列はデフォルト（英語）として扱う。
    pub language: String,
    /// WARN レベルのログをデスクトップ通知として表示する（デフォルト: false）。
    pub warn_notify: bool,
    /// オンラインソースが新しい画像を取得したときにデスクトップ通知を出す
    /// （デフォルト: true）。
    pub notify_fetch: bool,
    /// 「二度と表示しない」ブラックリスト機能を有効にする（デフォルト: true）。
    /// false にするとトレイメニュー項目・CLI・D-Bus メソッドが無効になる。
    pub enable_blacklist: bool,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            language: String::new(),
            warn_notify: false,
            notify_fetch: true,
            enable_blacklist: true,
        }
    }
}

fn default_true() -> bool {
    true
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

    /// `base`（読み込んだ時点の設定）から変えたキーだけを
    /// `~/.config/kabekami/config.toml` に書き込む。詳細は `save_changes_to`。
    pub fn save_changes(&self, base: &Config) -> Result<()> {
        let path = Self::config_path()?;
        self.save_changes_to(base, &path)
    }

    /// `base` から変えたキーだけを、既存のファイルに上書きする（#60）。
    ///
    /// 全体を書き直さないのは、次のものを失わないため:
    /// - ユーザーのコメントや書式（同梱の雛形はコメント付き）
    /// - 触っていないキーの書き方（`~/Pictures` が `normalize()` 後の絶対パスにならない）
    /// - 他のプロセスが保存したキー（GUI を開いている間にトレイで変えた値など）
    ///
    /// 書き込みは `atomic_write` 経由（電源断時に途中状態のファイルを残さない）。
    pub fn save_changes_to(&self, base: &Config, path: &Path) -> Result<()> {
        let mut doc: toml_edit::DocumentMut = match std::fs::read_to_string(path) {
            Ok(text) => text
                .parse()
                .with_context(|| format!("failed to parse config file: {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("failed to read config file: {}", path.display()))
            }
        };
        let old = toml::Table::try_from(base).context("failed to serialize config")?;
        let new = toml::Table::try_from(self).context("failed to serialize config")?;
        merge_changes(doc.as_table_mut(), &old, &new)?;
        crate::atomic_write::atomic_write(path, doc.to_string().as_bytes())
            .with_context(|| format!("failed to write config: {}", path.display()))
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

        // f32 フィールドは TOML 直編集で `nan` / `inf` が入りうる。
        // 画像処理側の前提を壊さないよう、非有限値はデフォルトに戻して clamp する。
        let defaults = Display::default();
        sanitize_f32(
            &mut self.display.blur_sigma,
            BLUR_SIGMA_RANGE,
            defaults.blur_sigma,
            "blur_sigma",
        );
        sanitize_f32(
            &mut self.display.bg_darken,
            BG_DARKEN_RANGE,
            defaults.bg_darken,
            "bg_darken",
        );

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

/// `display.blur_sigma` の有効範囲（kabekami-config GUI のスライダーと同じ値）。
const BLUR_SIGMA_RANGE: std::ops::RangeInclusive<f32> = 1.0..=50.0;
/// `display.bg_darken` の有効範囲。
const BG_DARKEN_RANGE: std::ops::RangeInclusive<f32> = 0.0..=1.0;

/// f32 フィールドの正規化:
/// 1. 非有限値 (`NaN` / `±inf`) なら `default_value` に戻す
/// 2. それ以外は `range` にクランプ
fn sanitize_f32(value: &mut f32, range: std::ops::RangeInclusive<f32>, default_value: f32, name: &str) {
    if !value.is_finite() {
        tracing::warn!("{} is not finite ({}), resetting to default {}", name, value, default_value);
        *value = default_value;
        return;
    }
    if !range.contains(value) {
        let clamped = value.clamp(*range.start(), *range.end());
        tracing::warn!(
            "{} {} is out of range {}..={}, clamping to {}",
            name, value, range.start(), range.end(), clamped
        );
        *value = clamped;
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
            Self::Reddit => 1,
            _ => 24,
        }
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// オンライン壁紙ソース 1 件の設定。TOML では `[[online_sources]]` 配列。
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
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
        xdg_dir("XDG_DATA_HOME", ".local/share")
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

/// `old` → `new` で変わったキーだけを `doc` に反映する。
///
/// 両方がテーブルで、`doc` 側も通常のテーブルなら中へ降りてキー単位で比べる
/// （同じセクションの他のキーとコメントを残すため）。それ以外は値ごと置き換える
/// （`[[online_sources]]` は 1 件でも変われば配列ごと書き直す）。
fn merge_changes(doc: &mut toml_edit::Table, old: &toml::Table, new: &toml::Table) -> Result<()> {
    for (key, nv) in new {
        let ov = old.get(key);
        if ov == Some(nv) {
            continue;
        }
        if let (Some(toml::Value::Table(ot)), toml::Value::Table(nt)) = (ov, nv) {
            let item = doc
                .entry(key)
                .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
            if let Some(t) = item.as_table_mut() {
                merge_changes(t, ot, nt)?;
                continue;
            }
        }
        doc.insert(key, to_item(key, nv)?);
    }
    for key in old.keys().filter(|k| !new.contains_key(*k)) {
        doc.remove(key);
    }
    Ok(())
}

/// `toml::Value` を `toml_edit::Item` に変換する。`[[...]]` やセクションとして
/// 書かれるよう、`key = value` の 1 項目の文書として出力してから取り出す。
fn to_item(key: &str, value: &toml::Value) -> Result<toml_edit::Item> {
    let mut single = toml::Table::new();
    single.insert(key.to_owned(), value.clone());
    let mut doc: toml_edit::DocumentMut = toml::to_string(&single)
        .context("failed to serialize config")?
        .parse()
        .context("failed to re-parse serialized config")?;
    doc.remove(key).context("serialized config lost its key")
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

/// `$<var>` が空でなければそれを、無ければ `$HOME/<home_rel>` を返す。
fn xdg_dir(var: &str, home_rel: &str) -> Option<PathBuf> {
    match std::env::var(var) {
        Ok(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => home_dir().map(|h| h.join(home_rel)),
    }
}

pub(crate) fn xdg_config_dir() -> Option<PathBuf> {
    xdg_dir("XDG_CONFIG_HOME", ".config")
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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let base = Config::default();
        let mut cfg = base.clone();
        cfg.rotation.interval_secs = 300;
        cfg.display.mode = DisplayMode::Fill;
        cfg.display.blur_sigma = 15.0;
        cfg.save_changes_to(&base, &path).unwrap();

        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.rotation.interval_secs, 300);
        assert_eq!(loaded.display.mode, DisplayMode::Fill);
        assert!((loaded.display.blur_sigma - 15.0).abs() < f32::EPSILON);
    }

    /// 保存は変えたキーだけを書き換える。コメント・`~` のパス・他から保存された
    /// キーは残る（#60）。
    #[test]
    fn save_changes_keeps_comments_untouched_keys_and_concurrent_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# 手書きのコメント\n[sources]\ndirectories = [\"~/Pictures\"] # 壁紙\n\n[rotation]\ninterval_secs = 60\n",
        )
        .unwrap();

        // GUI が読み込んだ時点の設定
        let base = Config::load_from(&path).unwrap();
        // GUI を開いている間に、トレイが表示モードを保存した
        let mut tray = base.clone();
        tray.display.mode = DisplayMode::Fill;
        tray.save_changes_to(&base, &path).unwrap();
        // GUI は間隔だけを変えて保存する（表示モードは古い値のまま持っている）
        let mut gui = base.clone();
        gui.rotation.interval_secs = 120;
        gui.save_changes_to(&base, &path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# 手書きのコメント"), "{text}");
        assert!(text.contains("\"~/Pictures\"] # 壁紙"), "{text}");
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.rotation.interval_secs, 120);
        assert_eq!(loaded.display.mode, DisplayMode::Fill, "トレイの変更が消えた");
    }

    /// 値を消した（`Some` → `None`）キーはファイルからも消える。
    #[test]
    fn save_changes_removes_cleared_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[sources]\nfavorites_dir = \"/fav\"\n").unwrap();

        let base = Config::load_from(&path).unwrap();
        let mut cfg = base.clone();
        cfg.sources.favorites_dir = None;
        cfg.save_changes_to(&base, &path).unwrap();

        assert_eq!(Config::load_from(&path).unwrap().sources.favorites_dir, None);
    }

    #[test]
    fn ui_defaults() {
        let ui = Ui::default();
        assert!(ui.language.is_empty());
        assert!(!ui.warn_notify);
        assert!(ui.notify_fetch, "notify_fetch must default to true");
        assert!(ui.enable_blacklist, "enable_blacklist must default to true");

        // TOML omit-all should produce the same defaults
        let parsed: Ui = toml::from_str("").unwrap();
        assert!(parsed.enable_blacklist);
        assert!(parsed.notify_fetch);
        assert!(!parsed.warn_notify);
    }

    #[test]
    fn home_dir_empty_string_returns_none() {
        let _guard = EnvGuard::set("HOME", "");
        assert_eq!(home_dir(), None);
    }

    #[test]
    fn normalize_resets_non_finite_floats() {
        let mut cfg = Config::default();
        cfg.display.blur_sigma = f32::NAN;
        cfg.display.bg_darken = f32::INFINITY;
        cfg.normalize();
        assert_eq!(cfg.display.blur_sigma, Display::default().blur_sigma);
        assert_eq!(cfg.display.bg_darken, Display::default().bg_darken);

        cfg.display.blur_sigma = f32::NEG_INFINITY;
        cfg.normalize();
        assert_eq!(cfg.display.blur_sigma, Display::default().blur_sigma);
    }

    #[test]
    fn normalize_clamps_out_of_range_floats() {
        let mut cfg = Config::default();
        cfg.display.blur_sigma = 1000.0;
        cfg.display.bg_darken = -5.0;
        cfg.normalize();
        assert_eq!(cfg.display.blur_sigma, 50.0);
        assert_eq!(cfg.display.bg_darken, 0.0);

        cfg.display.blur_sigma = 0.0; // below 1.0
        cfg.normalize();
        assert_eq!(cfg.display.blur_sigma, 1.0);
    }

    #[test]
    fn normalize_preserves_in_range_floats() {
        let mut cfg = Config::default();
        cfg.display.blur_sigma = 12.5;
        cfg.display.bg_darken = 0.7;
        cfg.normalize();
        assert!((cfg.display.blur_sigma - 12.5).abs() < f32::EPSILON);
        assert!((cfg.display.bg_darken - 0.7).abs() < f32::EPSILON);
    }
}
