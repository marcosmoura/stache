# Architecture overview

Read when you are new to the repo, or before changing the entry point, startup or
shutdown order, module lifecycle, or the thread layout.

## One binary, two modes

`app/native/src/main.rs` picks the mode before anything else runs:

- **Desktop** when there are no arguments after stripping `--config`, when the first
  argument is `--desktop`, or when the executable path contains `.app/Contents/MacOS`
  (`is_running_from_app_bundle`).
- **CLI** otherwise: `stache_lib::cli::run()` → `Cli::execute`.

`--config <path>`, `-c <path>` and `--config=<path>` are extracted by
`extract_config_path` and applied with `config::set_custom_config_path`. Desktop mode
exits 1 when that file is missing; the CLI returns `StacheError::ConfigError`.
`lib.rs::run()` is the desktop entry, the library crate is `stache_lib`, the binary is
`stache`. The crate compiles only on macOS (`compile_error!` in `main.rs`).

## Desktop startup order

`lib.rs::run`:

1. `logging::init()`, then `config::init()`.
2. `is_accessibility_granted()` — checks once, caches in a `OnceLock`, prompts if missing.
3. `wallpaper::setup()` — builds the global wallpaper manager from config (loads the file
   list) without setting a wallpaper or starting the timer.
4. Tauri builder: plugins `single-instance`, `zustand`, `shell`, the hotkey plugin;
   `KeepAwakeController` as managed state; every command in `generate_handler!`.
5. `.setup`: register configured hotkeys, set activation policy `Prohibited` (no Dock
   icon), build the `LifecycleRegistry` and register the six toggleable modules, then
   `load_base_modules` and `lazy_load_modules`.
6. `app_shutdown::install_signal_handler`, then `app.run`; its `RunEvent::Exit` handler
   calls `establish_exit_precedence()` and `cleanup_once()`.

`load_base_modules` is blocking and ordered: config watcher → IPC socket server → tray →
bar → widgets. Bar and widgets must exist before the frontend can load.

`lazy_load_modules` spawns one task on `tauri::async_runtime` that runs the
`execOnStartup` commands, initializes wallpaper, audio, notunes, command-quit and
menu-anywhere in parallel (`tokio::join!` over `spawn_blocking`), then starts tiling
through `platform::thread::dispatch_on_main_sync` (its AppKit/AX stages need the main
thread), then appends the tray Modules submenu so the initial check states are accurate.

## Shutdown

`app_shutdown.rs` owns the single cleanup routine. `cleanup_once` runs at most once and
in this order:

1. restore applications hidden by Stache (`tiling::restore_stache_hidden_apps`) and
   `hotkey::shutdown`
2. `tiling::shutdown`
3. `platform::ipc_socket::stop_server`

The first SIGINT/SIGTERM/SIGHUP schedules an orderly exit; the second calls
`std::process::exit(1)`. `exit` and `restart` both go through a CAS arbiter
(`TERMINAL_REQUEST`) so Exit beats Restart and at most one terminal action commits.
`RunEvent::Exit` sets `NATURAL_EXIT`, which suppresses any queued action.

## Threads

| Where                          | What                                                                                                             |
| ------------------------------ | ---------------------------------------------------------------------------------------------------------------- |
| main thread                    | Tauri event loop, all AppKit/AX work                                                                             |
| `ipc-server`                   | Unix socket server started by `ipc_socket::init`                                                                 |
| config watcher                 | `std::thread::spawn` in `config/watcher.rs`                                                                      |
| `stache-<name>`                | `platform::thread::spawn_named_thread`: `media`, `lock-watcher`, `screen-watcher`, `notunes-init`, `tray-toggle` |
| `tauri::async_runtime` (tokio) | tiling `StateActor` loop, `EffectSubscriber`, background startup                                                 |
| plain std threads              | tiling animation worker, border animation runner, screen and mouse monitors                                      |

Anything touching AppKit, the Accessibility API or an `NSWindow` from a background thread
must go through `platform::thread::dispatch_on_main_sync` (runs inline when already on the
main thread) or `dispatch_on_main` (fire and forget).

## Module lifecycle

- Contract: `modules/services/lifecycle.rs` — `LifecycleModule` (`name`, `id`, `start`,
  `pause`, `resume`, `status`) and `ModuleStatus` with four states: `ConfiguredOff`,
  `Running`, `Paused`, `Unavailable(reason)`.
- Registry: `modules/lifecycle_registry.rs`, filled once in `.setup`, never mutated.
- Registered ids: `wallpapers`, `commandQuit`, `notunes`, `proxyAudio`, `menuAnywhere`,
  `tiling`.
- Registration starts nothing. Startup calls each module's own `init()`; the registry only
  reports status and performs pause/resume for the tray.
- The tray (`modules/tray/mod.rs`) creates one check item per module in
  `install_modules_submenu` and drives pause/resume itself.
  `LifecycleRegistry::toggle` enforces the same rules but is currently only exercised by
  its unit tests.
- `services/traits.rs` (`Module`, `BackgroundService`, `ModuleError`) is legacy and has no
  implementors.

## Directory map

`app/native/src/`:

- `cli/` — clap tree (`commands/mod.rs`), one handler per domain, table/JSON output
  (`output.rs`).
- `config/` — `StacheConfig` and domain types (`types/`), loader (`mod.rs`), `.env`
  parsing (`env.rs`), template (`template.rs`), file watcher (`watcher.rs`).
- `modules/` — one directory per feature, plus `lifecycle_registry.rs`, `tray/`,
  `widgets/`.
- `platform/` — macOS layer: `ipc.rs`, `ipc_socket.rs`, `thread.rs`, `objc.rs`, `path.rs`,
  `accessibility.rs`, `window.rs`, `display.rs`, `command.rs`.
- `services/` — legacy traits only.
- root files: `main.rs`, `lib.rs`, `app_shutdown.rs`, `events.rs`, `error.rs`, `cache.rs`,
  `constants.rs`, `logging.rs`, `schema.rs`.

`app/ui/`:

- `components/` — shared primitives (Button, Icon, Stack, Surface, ScrollingLabel,
  QueryErrorFallback).
- `design-system/` — `colors` (Catppuccin Mocha) and `motion` tokens.
- `hooks/` — Tauri/React Query hooks and cross-window sync.
- `renderer/` — window roots; `Renderer.tsx` dispatches on the window label to `bar/` or
  `widgets/`.
- `stores/` — data hooks per feature (Battery, Weather).
- `tests/` — Vitest setup and helpers.
- `types/` — `tauri-events.ts` event constants.
- `utils/` — store factory, event emit helper, query defaults, `resolveModule`.
- `main.tsx`, `main.css`, `index.html` — entry files.

## Build and sidecar notes

- `app/native/build.rs` links the private `SkyLight` framework and `CoreLocation`.
- `scripts/prepare-sidecar.sh` (run by `pnpm prepare`) copies the Homebrew `media-control`
  binary to `app/native/binaries/media-control-<host-triple>` and its support files to
  `app/native/resources/lib|Frameworks`. `modules/bar/components/media.rs` spawns that
  sidecar through the shell plugin; without it the media widget has no data.
- `app/native/tauri.conf.json` declares the two windows (`bar`, `widgets`: hidden,
  transparent, non-focusable, all workspaces), the CSP allowlist (Open-Meteo, Visual
  Crossing, ipapi, ipinfo) and macOS 14.0 as the minimum.
- `beforeBuildCommand` runs `./scripts/generate-schema.sh && pnpm format && pnpm build`.
