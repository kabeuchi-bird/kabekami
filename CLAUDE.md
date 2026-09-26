# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

kabekami is a KDE Plasma wallpaper rotation daemon (Rust, tray icon, D-Bus). Source comments and the primary docs are in Japanese; `README.md` / `README.ja.md` and `config.toml` / `config.ja.toml` are parallel English/Japanese pairs — keep both in sync when editing one.

## Commands

```bash
cargo build --release                 # builds `kabekami` (daemon+CLI) and `kabekami-config` (GUI)
cargo test --workspace                # default-members is only root + kabekami-config, so plain `cargo test` skips kabekami-common
cargo test -p kabekami-common         # shared crate only
cargo test <name>                     # single test by name filter
RUST_LOG=kabekami=debug cargo run     # run daemon with debug logs
```

Tests are inline `#[cfg(test)]` modules (no `tests/` dir); `tempfile` is the only dev-dependency. There is no lint config beyond default `cargo clippy`/`cargo fmt`.

Runtime env overrides: `KABEKAMI_SCREEN=WxH`, `KABEKAMI_LANG=en|ja`, `KABEKAMI_I18N_DIR`.

## Architecture

Cargo workspace with three crates:

- **root `kabekami`** (`src/`) — the daemon. `main.rs` (~1.2k lines) owns the event loop (config hot-reload lives in `reload.rs`, which borrows the loop's state via `ReloadCtx`); it doubles as the CLI client: `--next/--prev/--quit/...` are parsed in `parse_cli()` and forwarded over D-Bus (`org.kabekami.Daemon`, see `daemon_iface.rs`) instead of starting a daemon.
- **`crates/kabekami-common`** — code shared with the GUI: `config.rs` (TOML schema), `display_mode.rs`, `blur_pad.rs` (image compositing), `i18n.rs`, `atomic_write.rs`/`toml_file.rs` (safe config persistence). The root crate has no copies: `main.rs` does `use kabekami_common::{config, display_mode, i18n};`, so `crate::config::…` etc. resolve to the shared crate.
- **`crates/kabekami-config`** — the settings GUI with live BlurPad preview.

### Daemon event loop

User/system commands funnel into one `mpsc` channel of `TrayCmd` consumed by the main `select!` loop. Producers: tray menu (`tray.rs`, ksni), D-Bus CLI (`daemon_iface.rs`), KDE global shortcuts (`shortcuts.rs`), session/Plasma-restart watcher (`session.rs`), screen-change watcher (`screen_watcher.rs`, throttled 60s). The file watchers (`watcher.rs`) have their own `select!` arms: source-dir add/remove events go straight to the scheduler, and `config.toml` changes are debounced 100ms and then call `reload::reload_config` directly. Other arms: rotation timer, a 30-minute online-provider check (at most one fetch task in flight, tracked by its `JoinHandle`), online `FetchResult`s, and WARN-log notifications.

Wallpaper pipeline: `scheduler.rs` picks the next image (index-based queue + 50-entry history, shuffle-per-cycle for random) → `prefetch.rs` processes ahead of time → `cache.rs` stores processed WebP files keyed by an FNV-1a hash of (path, screen size, mode, blur params) (`Cache::store` runs mtime-based LRU eviction inline, so call it from `spawn_blocking`) → `plasma.rs` applies via one D-Bus `evaluateScript` covering all monitors (a single entry means "every screen"), falling back to `plasma-apply-wallpaperimage`. `screen.rs` resolves per-monitor resolution (`kscreen-doctor`, with fallback 1920x1080).

Online sources live in `src/provider/` (bing, unsplash, wallhaven, reddit) sharing a client and `FetchResult` channel from `provider/mod.rs`; downloads land in a local dir and are picked up by the directory watcher.

Persistence under `~/.config/kabekami/`: `config.toml` (user settings), `state.toml` (pause + current wallpaper, read once at startup, managed by `state.rs`), `blacklist.txt` (`blacklist.rs`). Processed images cache in `~/.cache/kabekami/`.

### i18n

UI strings are TOML files (`crates/kabekami-common/i18n/ja.toml`), layered at runtime: `/usr/share/kabekami/i18n/` < `~/.config/kabekami/i18n/` < `$KABEKAMI_I18N_DIR`, so new languages need no rebuild. Any new UI string must be added through this system, not hardcoded.

## Conventions

- Shared dependency versions/features are centralized in root `[workspace.dependencies]`; add shared deps there so feature sets don't diverge between crates.
- Deliberate design rationale is documented in Japanese comments (e.g. why startup uses sync I/O, why `scanned_dirs` is tracked separately from `config`); read them before "fixing" such code.
- `packaging/aur/` holds the AUR PKGBUILD; `.coderabbit.yaml` configures review (Japanese, one auto-review per PR).
