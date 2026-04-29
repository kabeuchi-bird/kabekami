> [日本語版 README はこちら / Japanese README](README.ja.md)

# kabekami (壁紙)

A KDE Plasma wallpaper rotation daemon written in Rust.

- Rotates local images on a timer (sequential or random order)
- **BlurPad** mode: overlays the original image centred on a blurred background (equivalent to [Variety](https://github.com/varietywalls/variety)'s blur-pad)
- System tray resident (SNI protocol) with context menu controls
- LRU cache of processed images (SHA256 keyed) for fast switching even at short intervals
- Background prefetch: pre-processes the next image while the current one is displayed
- **Multi-monitor support**: detects all connected monitors via `kscreen-doctor` and applies a resolution-optimised image to each screen independently
- **Online wallpaper sources**: automatically download fresh wallpapers from Bing Daily, Unsplash, Wallhaven, and Reddit at configurable intervals
- **Favorites folder**: copy the current wallpaper to a configured directory with one click
- **Move to Trash**: send the current wallpaper to the system trash and advance to the next image
- **Session management**: listens to `logind` for graceful shutdown and automatically re-applies the wallpaper when Plasma restarts
- **GUI settings tool** (`kabekami-config`): six-tab egui interface with real-time BlurPad preview

## Requirements

| Item | Requirement |
|------|------------|
| OS | Linux |
| DE | KDE Plasma 5.7+ or Plasma 6 |
| Rust | 1.75+ (edition 2021) |
| External command | `plasma-apply-wallpaperimage` (bundled with Plasma) |
| D-Bus | Session bus access (required for tray icon) |
| `kscreen-doctor` | Optional — required for multi-monitor auto-detection |

> **Note** `plasma-apply-wallpaperimage` is included with KDE packages.
> Arch Linux: `plasma-workspace` · Fedora/Debian: `plasma-workspace` or `kde-plasma-desktop`.
>
> `kscreen-doctor` is part of `kscreen`. If it is absent, kabekami falls back to 1920×1080
> or the value set in `KABEKAMI_SCREEN`.

## Installation

### cargo build (recommended)

```bash
git clone https://github.com/kabeuchi-bird/kabekami.git
cd kabekami
cargo build --release
# Install both binaries
sudo install -m755 target/release/kabekami        /usr/local/bin/
sudo install -m755 target/release/kabekami-config /usr/local/bin/
```

### AUR (Arch Linux)

```bash
paru -S kabekami-git
# or
yay -S kabekami-git
```

## Quick Start

1. **Create a configuration file**

   ```bash
   mkdir -p ~/.config/kabekami
   ```

   Create `~/.config/kabekami/config.toml`:

   ```toml
   [sources]
   directories = ["~/Pictures/Wallpapers"]
   recursive = true

   [rotation]
   interval_secs = 1800   # rotate every 30 minutes
   order = "random"
   change_on_start = true

   [display]
   mode = "blur_pad"      # BlurPad mode (recommended)
   blur_sigma = 25.0
   bg_darken = 0.1

   [cache]
   directory = "~/.cache/kabekami"
   max_size_mb = 500
   ```

   Alternatively, open the GUI settings tool to configure without editing TOML manually:

   ```bash
   kabekami-config
   ```

2. **Launch**

   ```bash
   kabekami
   ```

   A tray icon will appear in your system tray.

3. **Set up autostart (optional)**

   **Method A — KDE System Settings (recommended)**

   1. Open **System Settings**
   2. Go to **Startup and Shutdown** → **Autostart**
   3. Click **Add Application…**
   4. Type `kabekami`, select it, and click **OK**

   Or place a `.desktop` file directly:

   ```bash
   cat > ~/.config/autostart/kabekami.desktop <<'EOF'
   [Desktop Entry]
   Name=kabekami
   GenericName=Wallpaper Rotator
   Comment=KDE Plasma wallpaper rotation tool
   Exec=kabekami
   Type=Application
   Categories=Utility;
   X-KDE-autostart-phase=2
   EOF
   ```

   > `X-KDE-autostart-phase=2` ensures kabekami starts after the Plasma shell has fully initialised, so the tray icon is reliably shown.

   **Method B — systemd user unit**

   ```ini
   # ~/.config/systemd/user/kabekami.service
   [Unit]
   Description=kabekami wallpaper rotator
   After=graphical-session.target plasma-plasmashell.service

   [Service]
   ExecStart=%h/.local/bin/kabekami
   Restart=on-failure
   RestartSec=5

   [Install]
   WantedBy=graphical-session.target
   ```

   ```bash
   systemctl --user enable --now kabekami.service
   # View logs
   journalctl --user -u kabekami.service -f
   ```

   > The systemd approach provides automatic restart on crash (`Restart=on-failure`) and integrated log management. Specifying `plasma-plasmashell.service` in `After=` ensures the tray is ready before kabekami starts.

## Settings GUI (`kabekami-config`)

`kabekami-config` is a graphical settings editor bundled with kabekami.

**Launch from the system tray:**

Right-click the tray icon → **Open Settings**

**Launch directly:**

```bash
kabekami-config
```

### Tabs

| Tab | Contents |
|-----|----------|
| **Sources** | Add/remove wallpaper directories, toggle recursive scan, set favorites folder |
| **Rotation** | Interval, sequential/random order, change-on-start, prefetch |
| **Display** | Mode selector (BlurPad / Fill / Fit / Stretch / Smart), blur sigma and background darkness sliders with **real-time preview** |
| **Cache** | Cache directory path, maximum size (MB), Clear Cache button |
| **UI** | Display language (`en` / `ja` / `kansai`), desktop notification for warnings |
| **Online** | Add/remove online providers (Bing / Unsplash / Wallhaven / Reddit), API keys, fetch interval, download directory |

Changes are saved to `~/.config/kabekami/config.toml` when you click **Save**. The running daemon detects the file change automatically via inotify and reloads without a restart.

> **Note** The real-time preview in the Display tab renders at 480×270 (16:9). Processing runs in a background thread so the UI stays responsive.

## Configuration Reference

Config file: `~/.config/kabekami/config.toml`

If the file does not exist, all default values are used.

### `[sources]` — Image Sources

```toml
[sources]
# Directories containing wallpaper images (multiple entries allowed)
directories = [
    "~/Pictures/Wallpapers",
    "~/Pictures/Photos",
]
# Recursively scan subdirectories (default: true)
recursive = true

# Favorites folder — copy current wallpaper here via tray menu or --copy-to-favorites
# If unset, the "Copy to Favorites" menu item is disabled.
# favorites_dir = "~/Pictures/Favorites"
```

Supported extensions: `jpg` / `jpeg` / `png` / `webp` / `bmp` / `tiff` / `gif`

### `[rotation]` — Rotation Settings

```toml
[rotation]
# Rotation interval in seconds. Minimum is 5 s (lower values are clamped).
interval_secs = 1800

# Rotation order
#   "random"     — Fisher-Yates shuffle (visits every image once before reshuffling)
#   "sequential" — in directory scan order
order = "random"

# Change wallpaper immediately on startup (default: true)
change_on_start = true

# Pre-process the next wallpaper in the background (default: true)
# Recommended when using short intervals
prefetch = true
```

### `[display]` — Display Mode

```toml
[display]
# Display mode (see table below)
mode = "blur_pad"

# BlurPad parameters
blur_sigma = 25.0   # Blur intensity (higher = more blur, recommended: 15–30)
bg_darken  = 0.1    # Darken background by this fraction (0.0–1.0, 0.1 = 10% darker)
```

#### Display Modes

| Mode | Behaviour |
|------|-----------|
| `blur_pad` | Blurred background with the original image overlaid (**recommended**) |
| `fill` | Fill screen, cropping edges to maintain aspect ratio |
| `fit` | Fit within screen, black letterbox/pillarbox |
| `stretch` | Stretch to fill, ignoring aspect ratio |
| `smart` | Auto-selects `fill` or `blur_pad` based on aspect ratio difference |

### `[cache]` — Cache Settings

```toml
[cache]
# Directory for processed image cache (default: ~/.cache/kabekami)
directory = "~/.cache/kabekami"
# Maximum cache size in MB. Oldest files are evicted when exceeded (default: 500)
max_size_mb = 500
```

### `[ui]` — UI Language

```toml
[ui]
# Display language: "en" (English, default), "ja" (Japanese), or "kansai" (Kansai dialect)
# Can be overridden at runtime with the KABEKAMI_LANG environment variable
language = "en"
# Show WARN-level log events as desktop notifications (default: false)
warn_notify = false
```

### `[[online_sources]]` — Online Wallpaper Sources

Each entry in the `[[online_sources]]` array configures one online provider.
Downloaded images are stored in `~/.local/share/kabekami/<provider>/` by default.

```toml
# Bing Daily — no API key required, up to 8 images per day
[[online_sources]]
provider = "bing"
enabled  = true
count    = 8            # 1–8 (Bing API limit)
locale   = "en-US"      # optional — e.g. "ja-JP", "de-DE" (default: "en-US")
# download_dir = "~/.local/share/kabekami/bing"   # override download path

# Unsplash — API key required (free tier: 50 req/hour)
[[online_sources]]
provider = "unsplash"
enabled  = true
api_key  = "YOUR_ACCESS_KEY"
query    = "nature landscape"   # search terms (default: "wallpaper")
count    = 10                   # 1–30
# quality = "regular"           # "regular" (default, ~1080p) or "full" (raw, very large)

# Wallhaven — API key optional (required only for NSFW content)
[[online_sources]]
provider = "wallhaven"
enabled  = true
# api_key = "YOUR_API_KEY"
query    = "anime landscape"
count    = 10                   # 1–24

# Reddit — no API key required
[[online_sources]]
provider       = "reddit"
enabled        = true
subreddit      = "wallpapers"   # subreddit name (alphanumeric + underscore only)
count          = 10
interval_hours = 1              # override fetch interval (default: 1h for Reddit)
```

#### Provider defaults

| Provider | Default interval | Max count | Notes |
|----------|-----------------|-----------|-------|
| `bing` | 24 h | 8 | Downloads FHD (1920×1080) or UHD (3840×2160) based on screen size |
| `unsplash` | 24 h | 30 | `quality = "regular"` is recommended over `"full"` |
| `wallhaven` | 24 h | 24 | SFW only by default (`purity = 100`) |
| `reddit` | 1 h | 100 | Direct-link images only; `post_hint = "image"` or URL extension |

The fetch interval timer resets only after at least one image is successfully downloaded.
To trigger a fetch immediately, use **Fetch Wallpapers Now** in the tray menu.

## Usage

### Environment Variables

| Variable | Description |
|----------|-------------|
| `KABEKAMI_SCREEN=2560x1440` | Override screen resolution (auto-detected via `kscreen-doctor` by default) |
| `KABEKAMI_LANG=en` | Override UI language at runtime (`en`, `ja`, or `kansai`) |
| `RUST_LOG=kabekami=debug` | Enable debug logging |

**Examples:**

```bash
# Specify resolution for a 4K monitor
KABEKAMI_SCREEN=3840x2160 kabekami

# Run with English tray menu and notifications
KABEKAMI_LANG=en kabekami

# Enable debug logging
RUST_LOG=kabekami=debug kabekami
```

### System Tray Menu

After launch, right-click the tray icon to open the context menu:

```
kabekami
├── Next Wallpaper          — Switch immediately (timer resets)
├── Previous Wallpaper      — Go back to the previous image (up to 50 history)
├── ───────────────────────
├── Pause / Resume          — Stop or resume automatic rotation
├── ───────────────────────
├── Display Mode ▶          — Fill / Fit / Stretch / BlurPad / Smart
├── Rotation Interval ▶     — 10s / 30s / 5m / 30m / 1h / 3h
├── ───────────────────────
├── Open Current Wallpaper  — Open the current file with xdg-open
├── Copy to Favorites       — Copy the current wallpaper to favorites_dir (disabled if unset)
├── Move to Trash           — Send the current wallpaper to the system trash and advance
├── Reload Config           — Reload config.toml without restarting
├── Open Settings           — Launch kabekami-config GUI
├── Fetch Wallpapers Now    — Trigger online provider fetch immediately (ignores interval)
├── ───────────────────────
└── Quit
```

> When `KABEKAMI_LANG=ja` (or `language = "ja"` in config), the menu is shown in Japanese.

### CLI Commands

When the daemon is already running, you can control it from the command line:

```bash
kabekami --next               # Switch to next wallpaper
kabekami --prev               # Switch to previous wallpaper
kabekami --toggle-pause       # Pause / resume automatic rotation
kabekami --reload-config      # Reload config.toml without restarting
kabekami --fetch-now          # Trigger online wallpaper fetch immediately
kabekami --trash-current      # Move current wallpaper to trash and advance
kabekami --copy-to-favorites  # Copy current wallpaper to favorites folder
kabekami --quit               # Quit the daemon
```

Commands communicate with the daemon via D-Bus (`org.kabekami.Daemon`).
If the daemon is not running, the command exits with an error.

### Stopping

```bash
# From the command line
kabekami --quit

# If running in the foreground
Ctrl-C
```

## Multi-Monitor Support

kabekami automatically detects all connected and enabled monitors via `kscreen-doctor --outputs` and applies a resolution-optimised processed image to each screen independently. The cache key includes the screen resolution, so each monitor's image is cached separately.

If `kscreen-doctor` is not available or fails, kabekami falls back to the primary resolution (or `KABEKAMI_SCREEN` if set).

To verify which monitors are detected at startup, check the log:

```bash
RUST_LOG=kabekami=info kabekami 2>&1 | grep "monitor detected"
# monitor detected: DP-1 2560x1440
# monitor detected: HDMI-1 1920x1080
```

## Session Management

kabekami integrates with the system session via two D-Bus signals:

| Signal | Action |
|--------|--------|
| `org.freedesktop.login1.Manager::PrepareForShutdown(true)` | Graceful shutdown — saves state and exits cleanly before the session ends |
| `org.freedesktop.DBus::NameOwnerChanged` for `org.kde.plasmashell` | Plasma restart detection — re-applies the current wallpaper automatically when Plasma restarts |

This means the wallpaper is correctly restored after Plasma crashes or the user runs `plasmashell --replace`.

## Logging

kabekami uses the `tracing` crate. By default `INFO` and above are printed to `stderr`.

```bash
# Change log level (trace / debug / info / warn / error)
RUST_LOG=kabekami=debug kabekami

# Log all crates
RUST_LOG=debug kabekami
```

## Cache

Processed images are stored as WebP (lossless) under `~/.cache/kabekami/`. The cache key is a SHA256 hash of:

- Absolute source image path
- Screen resolution (per monitor in multi-monitor setups)
- Display mode
- `blur_sigma` and `bg_darken` values

**The cache persists across restarts**, so subsequent switches of the same image with the same settings are nearly instant.

To clear the cache, use the **Clear Cache** button in the Cache tab of `kabekami-config`, or manually:

```bash
rm -rf ~/.cache/kabekami/
```

## Repository Structure

```
kabekami/
├── src/                     # kabekami daemon
│   ├── main.rs
│   ├── config.rs            # re-exports kabekami-common::config
│   ├── display_mode.rs      # re-exports kabekami-common::display_mode
│   ├── plasma.rs            # KDE Plasma D-Bus / CLI integration
│   ├── screen.rs            # monitor detection (kscreen-doctor)
│   ├── session.rs           # logind + NameOwnerChanged watchers
│   └── ...
├── crates/
│   ├── kabekami-common/     # Shared library (config types, image processing)
│   └── kabekami-config/     # GUI settings tool (egui / eframe)
└── Cargo.toml               # Cargo workspace root
```

## Troubleshooting

### Tray icon not appearing

- Verify `org.kde.StatusNotifierWatcher` is running. It starts automatically with KDE Plasma.
- If kabekami started before Plasma was fully initialised, restart kabekami.
- On non-SNI desktops (GNOME, etc.) a KStatusNotifierItem compatibility plugin is required.

### `plasma-apply-wallpaperimage` not found

```bash
which plasma-apply-wallpaperimage
```

If not found, install the package:

```bash
# Arch Linux
sudo pacman -S plasma-workspace

# Fedora
sudo dnf install plasma-workspace

# Debian / Ubuntu
sudo apt install plasma-workspace
```

### Wallpaper not changing (evaluateScript error)

Plasma's `evaluateScript` can fail when desktop widgets are locked. Unlock the desktop and try again.

### Multi-monitor: same image on all screens

If `kscreen-doctor` is not in `PATH`, kabekami cannot detect individual monitors and applies one image to all screens. Install `kscreen`:

```bash
# Arch Linux
sudo pacman -S kscreen

# Fedora
sudo dnf install kscreen

# Debian / Ubuntu
sudo apt install kscreen
```

### Slow image processing (4K displays)

BlurPad processing uses a quarter-scale intermediate for the blur step, typically completing in 1–2 s. With `prefetch = true`, the next image is processed in the background so switching is instant at the cost of slightly higher idle CPU/memory usage.

### Settings not applied after saving in kabekami-config

The daemon reloads the config automatically via inotify when `config.toml` changes. If the daemon is not running, changes take effect on the next start.

### Online sources download 0 images

- **Unsplash**: check that `api_key` is set and the 50 req/hour free-tier limit has not been exceeded.
- **Reddit**: the subreddit must exist and contain posts with direct image links (`.jpg`, `.png`, `.webp`). Gallery/album links are not supported.
- **Wallhaven / Bing**: verify network connectivity and check `RUST_LOG=kabekami=debug` output for detailed error messages.
- The `.last_fetch` timestamp is only updated when at least one image is downloaded. If a fetch returns 0 images, the next attempt happens at the next interval tick (not after a full interval delay).

### Online images not appearing in rotation after download

Use **Fetch Wallpapers Now** in the tray menu to trigger an immediate fetch and confirm the images are added. Then use **Reload Config** to re-scan the download directories.

## License

[MIT License](LICENSE)

---

## Acknowledgments

kabekami is heavily inspired by [Variety](https://github.com/varietywalls/variety). Many thanks to **Peter Levi** and all the contributors who have maintained Variety over the years.
