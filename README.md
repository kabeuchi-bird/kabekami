> [日本語版 README はこちら / Japanese README](README.ja.md)

# kabekami (壁紙)

A KDE Plasma wallpaper rotation daemon written in Rust.

- Rotates local images on a timer (sequential or random) with a system tray menu
- **BlurPad** mode: image centred on a blurred background (like [Variety](https://github.com/varietywalls/variety)'s blur-pad)
- **Multi-monitor**: per-screen resolution-optimised images via `kscreen-doctor`
- **Online sources**: Bing Daily, Unsplash, Wallhaven, Reddit — auto-downloaded on a schedule
- **GUI settings tool** (`kabekami-config`) with real-time BlurPad preview

## Requirements

| Item | Requirement |
|------|------------|
| OS | Linux |
| DE | KDE Plasma 5.7+ or Plasma 6 |
| Rust | 1.75+ (edition 2021) |
| External | `plasma-apply-wallpaperimage` (bundled with `plasma-workspace`) |
| D-Bus | Session bus (required for tray icon) |
| `kscreen-doctor` | Optional — needed for multi-monitor auto-detection (`kscreen` package) |
| `kdialog` | Optional — native KDE file/folder picker dialogs in `kabekami-config` |

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
├── Open Settings           — Launch kabekami-config
└── Quit
```

### CLI

```bash
kabekami --next
kabekami --prev
kabekami --toggle-pause
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

## Configuration

Config file: `~/.config/kabekami/config.toml`

See [`config.toml`](config.toml) in this repository for a fully annotated reference covering every setting and its default value.

### Supported Image Formats

kabekami supports the following image formats: **jpg, jpeg, png, webp, avif**

EXIF Orientation tags are read and applied automatically, so portrait photos and rotated camera shots are displayed in the correct orientation.

Note: bmp, tiff, and gif are not supported (the `image` crate features are limited to jpeg/png/webp/avif to reduce binary size).

## Troubleshooting

**Tray icon not appearing** — Restart kabekami after Plasma has fully started.

**`plasma-apply-wallpaperimage` not found** — Install `plasma-workspace` for your distro.

**Wallpaper not changing (evaluateScript error)** — Unlock the desktop and try again.

**Multi-monitor: same image on all screens** — Install `kscreen` to enable per-monitor detection.

**Wallpaper looks blurry / not at native resolution** — kabekami first tries `kscreen-doctor --json` (Plasma 6) and falls back to text parsing on older versions. If both fail (KScreen daemon unavailable at startup, etc.), it uses 1920×1080 until the next dynamic re-detection (polled every 60s). To diagnose, run `RUST_LOG=kabekami=debug kabekami` and check the `kscreen-doctor output` log line. As a workaround, override the resolution explicitly with `KABEKAMI_SCREEN=2560x1440` (your actual resolution).

**Online sources download 0 images** — Check API key, network, and `RUST_LOG=kabekami=debug` output.

**Settings not applied after saving** — The daemon reloads `config.toml` via inotify automatically; restart if needed.

## License

[MIT License](LICENSE)

---

Inspired by [Variety](https://github.com/varietywalls/variety). Thanks to Peter Levi and all contributors.
