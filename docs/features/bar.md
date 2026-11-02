# Status bar (bar)

Read when changing the bar window, its placement or stacking, menu-bar auto-hide handling, the Spaces/Media/Status trees, the bar's Tauri commands, or anything under `bar` in the config.

Rust paths are relative to `app/native/src/`; UI paths are relative to `app/ui/renderer/`.

## Purpose

The bar is one non-activating, always-present overlay window (`label: "bar"`, `#/bar` in `tauri.conf.json`) that floats above every Space and holds three subtrees: `Spaces` (workspaces and windows from the tiling state), `Media`, and `Status` (weather, CPU, battery, keep-awake, Wi-Fi, clock).

## Entry points

| Caller                                   | Entry                                                                 |
| ---------------------------------------- | --------------------------------------------------------------------- |
| App startup, UI-critical phase           | `lib.rs::load_base_modules` → `modules::bar::init(app)`               |
| CLI control commands                     | `platform::ipc_socket` → `modules::bar::ipc_listener::handle_command` |
| `NSDistributedNotificationCenter` events | `ipc_listener::init` → `handle_notification` (benign refreshes only)  |
| Frontend                                 | Tauri commands registered in `lib.rs` `generate_handler!`             |

`bar::init` returns early when `bar.enabled` is false; the window is created hidden (`visible: false`) and shown at the end of init.

## Key types

- `bar/window.rs`: `WindowFrame { x, y, width, height }` (camelCase; returned by `get_bar_window_frame`), `calculate_window_frame`.
- `config/types/bar.rs`: `BarConfig { enabled, height (28), padding (12), weather }`, `WeatherConfig { provider, api_keys, default_location }`, `WeatherProvider::{Auto, VisualCrossing, OpenMeteo}`.
- `bar/components/tiling.rs`: `WorkspaceInfo`, `WindowInfo` (frontend-shaped, camelCase).
- `bar/components/weather.rs`: `WeatherConfigInfo` (carries the resolved API key).
- `bar/components/keepawake.rs`: `KeepAwakeController` (managed Tauri state).

## Rust modules

| File                          | Role                                                                                                                                                                 |
| ----------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `bar/mod.rs`                  | `init`: gate on config, sticky + below-menu flags, position, watchers, components, IPC, show                                                                         |
| `bar/window.rs`               | `set_window_position`, `calculate_window_frame`, `get_bar_window_frame`                                                                                              |
| `bar/screen.rs`               | `init_screen_watcher`: `CGDisplayRegisterReconfigurationCallback` on a named thread                                                                                  |
| `bar/menubar.rs`              | Hacky menu-bar visibility detection (see pitfalls)                                                                                                                   |
| `bar/watcher.rs`              | `start_best_effort_refresh_watcher`: observers + fallback poll loop                                                                                                  |
| `bar/ipc_listener.rs`         | CLI command/notification dispatch; tiling commands reach the actor here                                                                                              |
| `bar/components/apps.rs`      | `open_app(name)` — fixed allowlist of app names and `x-apple.systempreferences:` URLs, launched with `open`                                                          |
| `bar/components/battery.rs`   | `get_battery_info`                                                                                                                                                   |
| `bar/components/cpu.rs`       | `get_cpu_info` (sysinfo + SMC temperature)                                                                                                                           |
| `bar/components/keepawake.rs` | `is_system_awake`, `toggle_system_awake`, watcher — see [keep-awake](keep-awake.md)                                                                                  |
| `bar/components/media.rs`     | `media-control` sidecar stream, artwork processing, `get_current_media_info`                                                                                         |
| `bar/components/tiling.rs`    | `get_tiling_workspaces/_windows/_focused_workspace/_focused_window/_current_workspace_windows`, `focus_tiling_workspace`, `focus_tiling_window`, `is_tiling_enabled` |
| `bar/components/weather.rs`   | `get_weather_config` — exposes config plus the API key read from the env file                                                                                        |
| `bar/components/wifi.rs`      | `get_wifi_info`                                                                                                                                                      |

Only `keepawake` and `media` register background work in `components::init`; every other component is pull-only through a command.

## Data flow

- **Placement**: `set_window_position` reads the screen width once and applies `calculate_window_frame(width, height, padding)` → origin `(padding, padding)`, size `(width - 2*padding, height)`. `screen::init_screen_watcher` re-runs it on display reconfiguration. The window is also put on a private `SLS` space (`platform/window.rs::set_window_sticky`) and at the main-menu window level with `always_on_bottom` (`set_window_below_menu`), so it never steals focus.
- **Menu bar**: `menubar.rs` emits `stache://menubar/visibility-changed`; `Bar.state.ts` turns that into `menuHidden`, and `Bar.styles.ts::barHidden` slides the bar off-screen whenever the macOS menu bar is visible.
- **Events**: `stache://media/playback-changed`, `stache://keepawake/state-changed`, `stache://tiling/*`, `stache://app/reload`, `stache://widgets/toggle`. Names live in `events.rs` and are mirrored in `app/ui/types/tauri-events.ts`.
- **Frontend**: `Bar.tsx` is a three-column grid of `Spaces`, `Media`, `Status`. `Spaces.state.ts` queries `get_tiling_workspaces` + `get_tiling_current_workspace_windows`, invalidates on tiling events (100 ms debounce for focus bursts), and falls back to empty data when tiling is not up yet, then re-fetches on `stache://tiling/initialized`. `Media.state.ts` seeds from `get_current_media_info`, then follows `playback-changed`. `Status` polls: CPU 2 s, Wi-Fi 5 s, clock 1 s, battery 30 s charging / 2 min otherwise.

## Config

Root key `bar`. `height` and `padding` are also read by the tiling layout as a top gap of `height + padding` on the main screen only (see [tiling](tiling.md)). Weather network calls run in the webview; the allowed hosts are pinned in `tauri.conf.json` `security.csp.connect-src`.

## Invariants and pitfalls

- The bar window must exist in `tauri.conf.json`; `bar::init` only logs and returns otherwise.
- `set_window_sticky` calls `set_focusable(true)` even though the window declares `focusable: false`, and installs the `StacheNonActivatingTaoWindow` subclass so clicks are accepted without activating Stache. Don't "fix" either half alone.
- Menu-bar visibility has no public API: `menubar.rs` combines the `NSMenu` query, a pointer-in-menu-bar-row check (covers auto-hide reveal), and a `CGWindowList` layer heuristic, and only reports hidden when both primary signals agree. Treat it as best effort.
- A failed observer registration must not stop refreshing: `start_best_effort_refresh_watcher` always runs the fallback poll loop.
- Media emits only when the state hash changes (`save_artwork_and_emit`), and artwork is capped (16 MB encoded / 12 MB decoded / 8192 px) because payloads are player-controlled.
- The Spaces workspace order is hardcoded in `Spaces.state.ts` (`workspaceOrder`), not taken from config; unknown names sort alphabetically after known ones.
- Weather rendering is decided in the webview by `isProviderAvailable`: `auto` always works (Visual Crossing when a key was loaded, Open Meteo otherwise), and only an explicit `visual-crossing` without a key hides the widget. The Rust helper `WeatherConfig::is_enabled` encodes a stricter rule but is currently only exercised by tests.

## Tests

`bar/window.rs` (frame math for 800/1920/3840 px and custom config), `bar/watcher.rs`, `bar/screen.rs`, `bar/menubar.rs` (`resolve_menu_bar_visible` matrix), `bar/ipc_listener.rs` (event names, command→notification mapping), `bar/components/{media,weather,battery,cpu,wifi,keepawake}.rs`. UI: `bar/Bar.test.tsx`, `bar/Spaces/**`, `bar/Media/Media.test.tsx`, `bar/Status/**`.
