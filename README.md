> [日本語版 README はこちら / Japanese README](README.ja.md)

# kabekami (壁紙)

A KDE Plasma wallpaper rotation daemon written in Rust.

- Rotates local images on a timer (sequential or random order)
- **BlurPad** mode: original image centred on a blurred background (like [Variety](https://github.com/varietywalls/variety)'s blur-pad)
- System tray icon (SNI) with context menu, multi-language UI
- LRU cache + background prefetch for instant switching at any interval
- **Multi-monitor**: per-screen resolution-optimised images via `kscreen-doctor`
- **Online sources**: Bing Daily, Unsplash, Wallhaven, Reddit — auto-downloaded on a schedule
- **Never Show Again**: permanently blacklist a wallpaper (`~/.config/kabekami/blacklist.txt`)
- **Global shortcuts**: configurable via System Settings → Shortcuts → kabekami
- **Session management**: graceful shutdown via `logind`, auto-reapply on Plasma restart
- **GUI settings tool** (`kabekami-config`): 6-tab egui interface with real-time BlurPad preview

## Requirements

| Item | Requirement |
|------|------------|
| OS | Linux |
| DE | KDE Plasma 5.7+ or Plasma 6 |
| Rust | 1.75+ (edition 2021) |
| External | `plasma-apply-wallpaperimage` (bundled with `plasma-workspace`) |
| D-Bus | Session bus (required for tray icon) |
| `kscreen-doctor` | Optional — needed for multi-monitor auto-detection (`kscreen` package) |

## Installation

### Build from source

```bash
git clone https://github.com/kabeuchi-bird/kabekami.git
cd kabekami
cargo build --release
sudo install -m755 target/release/kabekami        /usr/local/bin/
sudo install -m755 target/release/kabekami-config /usr/local/bin/
```

### AUR (Arch Linux)

```bash
paru -S kabekami-git
```

## Quick Start

1. Create `~/.config/kabekami/config.toml`:

   ```toml
   [sources]
   directories = ["~/Pictures/Wallpapers"]

   [rotation]
   interval_secs = 1800
   order = "random"

   [display]
   mode = "blur_pad"

   [ui]
   language = "en"   # "en" or "ja"
   ```

   Or launch `kabekami-config` for a GUI editor.

2. Run `kabekami` — a tray icon appears in your system tray.

3. **Autostart** (optional) — place a `.desktop` file:

   ```bash
   cat > ~/.config/autostart/kabekami.desktop <<'EOF'
   [Desktop Entry]
   Name=kabekami
   Exec=kabekami
   Type=Application
   X-KDE-autostart-phase=2
   EOF
   ```

   > `X-KDE-autostart-phase=2` ensures kabekami starts after Plasma has fully initialised.

   Or use a **systemd user unit** for automatic restart on crash:

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
   journalctl --user -u kabekami.service -f   # view logs
   ```

## Usage

### Tray Menu

```
├── Next Wallpaper          — Switch immediately (timer resets)
├── Previous Wallpaper      — Go back (up to 50 history)
├── Pause / Resume
├── Display Mode ▶          — Fill / Fit / Stretch / BlurPad / Smart
├── Rotation Interval ▶     — 10s / 30s / 5m / 30m / 1h / 3h
├── Open Current Wallpaper
├── Copy to Favorites       — (disabled if favorites_dir not set)
├── Move to Trash           — Delete and advance
├── Never Show Again        — Blacklist permanently
├── Reload Config
├── Open Settings           — Launch kabekami-config
├── Fetch Wallpapers Now    — Trigger online fetch immediately
└── Quit
```

### CLI

```bash
kabekami --next
kabekami --prev
kabekami --toggle-pause
kabekami --reload-config
kabekami --fetch-now
kabekami --trash-current
kabekami --blacklist-current
kabekami --copy-to-favorites
kabekami --quit
```

Commands are forwarded via D-Bus (`org.kabekami.Daemon`).

### Global Shortcuts

Register shortcuts in **System Settings → Shortcuts → kabekami** (no defaults assigned):

| Action | Description |
|--------|-------------|
| Next Wallpaper | Switch to the next image |
| Previous Wallpaper | Go back to the previous image |
| Pause / Resume | Toggle automatic rotation |
| Move to Trash | Trash current image and advance |
| Never Show Again | Blacklist current image permanently |

### Environment Variables

| Variable | Description |
|----------|-------------|
| `KABEKAMI_SCREEN=2560x1440` | Override screen resolution |
| `KABEKAMI_LANG=en` | Override UI language (`en` / `ja`) |
| `RUST_LOG=kabekami=debug` | Enable debug logging |

## Configuration Reference

Config file: `~/.config/kabekami/config.toml` (all values optional, defaults shown)

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

| Mode | Behaviour |
|------|-----------|
| `blur_pad` | Blurred background with original overlaid (**recommended**) |
| `fill` | Fill screen, crop edges |
| `fit` | Fit within screen, black bars |
| `stretch` | Stretch, ignoring aspect ratio |
| `smart` | Auto-selects `fill` or `blur_pad` by aspect ratio |

### `[cache]` — Cache Settings

```toml
[cache]
# Directory for processed image cache (default: ~/.cache/kabekami)
directory = "~/.cache/kabekami"
# Maximum cache size in MB. Oldest files are evicted when exceeded (default: 500)
max_size_mb = 500
```

Clear cache: **kabekami-config → Cache → Clear Cache**, or `rm -rf ~/.cache/kabekami/`.

### `[ui]` — UI Language

```toml
[ui]
# Display language: "en" (English, default) or "ja" (Japanese)
# Can be overridden at runtime with the KABEKAMI_LANG environment variable
language = "en"
# Show WARN-level log events as desktop notifications (default: false)
warn_notify = false
```

### `[[online_sources]]`

```toml
# Bing Daily (no API key required)
[[online_sources]]
provider = "bing"
enabled  = true
count    = 8
locale   = "en-US"

# Unsplash (API key required)
[[online_sources]]
provider = "unsplash"
enabled  = true
api_key  = "YOUR_KEY"
query    = "nature landscape"
count    = 10

# Wallhaven (API key optional)
[[online_sources]]
provider = "wallhaven"
enabled  = true
query    = "landscape"
count    = 10

# Reddit (no API key required)
[[online_sources]]
provider       = "reddit"
enabled        = true
subreddit      = "wallpapers"
count          = 10
interval_hours = 1
```

Use **Fetch Wallpapers Now** in the tray to trigger a fetch immediately.

## Troubleshooting

**Tray icon not appearing** — Restart kabekami after Plasma has fully started.

**`plasma-apply-wallpaperimage` not found** — Install `plasma-workspace` for your distro.

**Wallpaper not changing (evaluateScript error)** — Unlock the desktop and try again.

**Multi-monitor: same image on all screens** — Install `kscreen` to enable per-monitor detection.

**Online sources download 0 images** — Check API key, network, and `RUST_LOG=kabekami=debug` output.

**Settings not applied after saving** — The daemon reloads `config.toml` via inotify automatically; restart if needed.

## License

[MIT License](LICENSE)

---

Inspired by [Variety](https://github.com/varietywalls/variety). Thanks to Peter Levi and all contributors.
