# Wallpapers

Read when changing wallpaper loading, image processing, the wallpaper cache, wallpaper CLI commands, or the wallpaper lifecycle/timer.

Rust paths are relative to `app/native/src/`.

## Purpose

Rotates the desktop wallpaper from a directory or an explicit list, optionally applying rounded corners and blur. Every wallpaper is resized to the target screen's pixel size and written as a lossless PNG in the cache so a repeat set is instant.

## Entry points

| Caller                                | Entry                                                                                                   |
| ------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| App startup, before the Tauri builder | `wallpaper::setup()` in `lib.rs` (`run`) — builds the manager from config, sets the global `OnceLock`   |
| App startup, background phase         | `wallpaper::init()` in `lib.rs` (`lazy_load_modules`) — sets the initial wallpaper and starts the timer |
| CLI                                   | `cli/commands/wallpaper.rs` `execute` → `init_wallpaper_manager`, then `set` / `list` / `generate-all`  |
| Tray                                  | `wallpaper::WallpaperLifecycle` (registered in `lib.rs`)                                                |
| Other modules                         | `wallpaper::perform_action(&WallpaperAction)`                                                           |

## Key types

- `config/types/wallpaper.rs`: `WallpaperConfig { enabled, path, list, interval, mode, radius, blur }`, `WallpaperMode::{Random, Sequential}`.
- `modules/wallpaper/manager.rs`: `WallpaperManager` (global `OnceLock<Arc<WallpaperManager>>`), `WallpaperAction::{Random, RandomForScreen(usize), File(String), FileForScreen(usize, String)}`, `WallpaperManagerError`, `WallpaperLifecycle`.
- `modules/wallpaper/processing.rs`: `process_image`, `process_image_for_screen`, `cached_path*`, `is_supported_image`, `list_images_in_directory`, screen-size helpers.
- `modules/wallpaper/macos.rs`: `set_wallpaper`, `set_wallpaper_for_screen`, `screen_count` — `NSWorkspace setDesktopImageURL:forScreen:options:error:` via Objective-C.

## Data flow

1. `WallpaperManager::new` → `load_wallpapers`: `path` wins over `list`. Directory contents are filtered by extension and natural-sorted (`natord`) so sequential mode is stable. `list` entries are silently dropped when they do not exist or are unsupported.
2. A selection index comes from `select_initial_index` / `select_next_index` / a random draw for `WallpaperAction::Random`.
3. `process_image[_for_screen]` returns a cache path; an existing cache file is reused without decoding.
4. `macos::set_wallpaper[_for_screen]` applies it. `change_lock` serialises concurrent changes.

## Cache

`cache::get_cache_subdir("wallpapers")` → `~/Library/Caches/{APP_BUNDLE_ID}/wallpapers` (fallback `/tmp/{APP_BUNDLE_ID}/wallpapers`). Two naming shapes: `{stem}_{hash}_{width}x{height}_r{radius}_b{blur}.png` for the all-screens path and `{stem}_{hash}_s{screen_index}_{width}x{height}_r{radius}_b{blur}.png` for a specific screen. `hash` is an FNV-1a over the canonical path, file length and mtime (seconds + nanos). Changing `radius`, `blur`, screen size or the source file therefore produces a new file instead of reusing a stale one.

`stache cache path` prints the root; `stache cache clear` deletes it and reports bytes freed (`cli/commands/cache.rs`, `cache.rs`). The IPC socket also lives under that directory, so clearing the cache while the app runs breaks CLI connections until restart — see [gotchas](../gotchas.md).

## Config

Root key `wallpapers` (`StacheConfig::wallpapers`). `enabled` defaults to `false`; `interval: 0` means "set once, never rotate". See [config](../architecture/config.md).

## Invariants and pitfalls

- `setup()` does nothing unless `enabled` is true **and** `path`/`list` is non-empty. Every CLI wallpaper command then fails with "Wallpaper manager not initialized" — `enabled: true` is required even for `stache wallpaper set`.
- Supported extensions are exactly `jpg`, `jpeg`, `png`, `webp` (`SUPPORTED_EXTENSIONS`).
- `init()` sets the initial wallpaper only in release builds; in debug the timer starts but the current wallpaper is left alone.
- The cycling thread exits at its next wakeup when `stop_timer` bumps `timer_generation`, so `reset_timer` needs no sleep and a stopped timer can never apply a wallpaper.
- CLI commands run in the CLI process: they read the config, build their **own** manager and set the wallpaper directly. They do not talk to a running app, so the app's `current_index` and timer are untouched. Command dispatch details: [ipc](../architecture/ipc.md).
- `set` accepts either `PATH` or `--random`, never both; `--screen` is `all`, `main` (0-based index 0) or a 1-based number.
- `generate-all` fans out `screens × wallpapers` tasks with rayon, skips cache hits and prints a TTY spinner.

## Permissions and threading

No Accessibility permission. Objective-C calls are made from the calling thread (startup background task, CLI process, or the timer thread). Nothing must run on the main thread.

## Lifecycle and hot reload

`WallpaperLifecycle` id `wallpapers`. `pause` stops the timer (current wallpaper stays), `resume` starts it again; `status` is `ConfiguredOff` when disabled, `Unavailable` when the manager failed to load, `Running` when `interval` is 0 or the timer is live, and `Paused` otherwise. Pausing is runtime-only.

## Tests

`modules/wallpaper/manager.rs` (`start_timer_is_idempotent_and_preserves_generation`, `reset_timer_restarts_immediately_without_arbitrary_sleep`, `wallpaper_status_maps_states`, load/action error cases), `processing.rs` (cache-filename keying, effects, screen fallbacks), `macos.rs` (error display, missing file), `cli/commands/wallpaper.rs` (clap parsing), `cache.rs`.
