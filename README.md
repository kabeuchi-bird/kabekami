> [日本語版 README はこちら / Japanese README](README.ja.md)

# kabekami (壁紙)

A KDE Plasma wallpaper rotation daemon written in Rust.

- Rotates local images on a timer (sequential or random order)
- **BlurPad** mode: overlays the original image centred on a blurred background (equivalent to [Variety](https://github.com/varietywalls/variety)'s blur-pad)
- System tray resident (SNI protocol) with context menu controls
- LRU cache of processed images (SHA256 keyed) for fast switching even at short intervals
- Background prefetch: pre-processes the next image while the current one is displayed
- **GUI settings tool** (`kabekami-config`): five-tab egui interface with real-time BlurPad preview

## Requirements

| Item | Requirement |
|------|------------|
| OS | Linux |
| DE | KDE Plasma 5.7+ or Plasma 6 |
| Rust | 1.75+ (edition 2021) |
| External command | `plasma-apply-wallpaperimage` (bundled with Plasma) |
| D-Bus | Session bus access (required for tray icon) |

> **Note** `plasma-apply-wallpaperimage` is included with KDE packages.
> Arch Linux: `plasma-workspace` · Fedora/Debian: `plasma-workspace` or `kde-plasma-desktop`.

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

### cargo install (after crates.io release)

```bash
cargo install kabekami kabekami-config
```

### AUR (Arch Linux)

```bash
# After release
paru -S kabekami
# or
yay -S kabekami
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
| **Sources** | Add/remove wallpaper directories, toggle recursive scan |
| **Rotation** | Interval, sequential/random order, change-on-start, prefetch |
| **Display** | Mode selector (BlurPad / Fill / Fit / Stretch / Smart), blur sigma and background darkness sliders with **real-time preview** |
| **Cache** | Cache directory path, maximum size (MB) |
| **UI** | Display language (`en` / `ja`), desktop notification for warnings |

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
# Display language: "en" (English, default) or "ja" (Japanese)
# Can be overridden at runtime with the KABEKAMI_LANG environment variable
language = "en"
# Show WARN-level log events as desktop notifications (default: false)
warn_notify = false
```

## Usage

### Environment Variables

| Variable | Description |
|----------|-------------|
| `KABEKAMI_SCREEN=2560x1440` | Override screen resolution (auto-detected via `kscreen-doctor` by default) |
| `KABEKAMI_LANG=en` | Override UI language at runtime (`ja` or `en`) |
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
├── Reload Config           — Reload config.toml without restarting
├── ───────────────────────
├── Open Settings           — Launch kabekami-config GUI
├── ───────────────────────
└── Quit
```

> When `KABEKAMI_LANG=ja` (or `language = "ja"` in config), the menu is shown in Japanese.

### CLI Commands

When the daemon is already running, you can control it from the command line:

```bash
kabekami --next           # Switch to next wallpaper
kabekami --prev           # Switch to previous wallpaper
kabekami --toggle-pause   # Pause / resume automatic rotation
kabekami --reload-config  # Reload config.toml without restarting
kabekami --quit           # Quit the daemon
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
- Screen resolution
- Display mode
- `blur_sigma` and `bg_darken` values

**The cache persists across restarts**, so subsequent switches of the same image with the same settings are nearly instant.

To clear the cache manually:

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

### Slow image processing (4K displays)

BlurPad processing uses a quarter-scale intermediate for the blur step, typically completing in 1–2 s. With `prefetch = true`, the next image is processed in the background so switching is instant at the cost of slightly higher idle CPU/memory usage.

### Settings not applied after saving in kabekami-config

The daemon reloads the config automatically via inotify when `config.toml` changes. If the daemon is not running, changes take effect on the next start.

## License

[MIT License](LICENSE)

---

## Acknowledgments

kabekami is heavily inspired by [Variety](https://github.com/varietywalls/variety). Many thanks to **Peter Levi** and all the contributors who have maintained Variety over the years. 
