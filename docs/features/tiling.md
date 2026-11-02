# Tiling window manager

Read when changing window tracking, workspace assignment, layouts, gaps, borders, tabs, pause/resume, or `stache tiling`.

Rust paths are relative to `app/native/src/modules/tiling/`.

## Purpose

Tiling arranges every managed window of a screen into virtual workspaces using a layout algorithm, and makes workspace switching/hiding invisible to the app. It is optional: nothing starts unless `tiling.enabled` is true and Accessibility is granted.

## Entry points

| Caller                        | Entry                                                                                                                |
| ----------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| App startup, background phase | `lib.rs::lazy_load_modules` → `dispatch_on_main_sync` → `tiling::init`                                               |
| `stache tiling …`             | `cli/commands/tiling.rs` → `platform::ipc_socket` (`0600` socket)                                                    |
| Modules menu / tray           | `init::TilingLifecycle` (id `tiling`)                                                                                |
| Bar frontend                  | `bar/components/tiling.rs` Tauri commands                                                                            |
| Local clients (CLI)           | `init::handle_ipc_query` (queries) and `bar/ipc_listener.rs::handle_command` (commands), both over the `0600` socket |

`init::init` returns `false` and logs when `tiling.enabled` is false or Accessibility is not granted; on success it emits `stache://tiling/initialized`. `is_tiling_enabled()` = initialized **and** enabled.

## Module map

| Path            | Role                                                                                                    |
| --------------- | ------------------------------------------------------------------------------------------------------- |
| `init.rs`       | Staged runtime factory, lifecycle state, teardown, event emitters, IPC query handlers                   |
| `actor/`        | `StateActor` + `StateActorHandle` + `StateMessage`/`StateQuery` + per-domain handlers                   |
| `state/`        | `TilingState` (observable screens/workspaces/windows/focus) and the `Screen`/`Workspace`/`Window` types |
| `events/`       | `EventProcessor`, `AXObserverAdapter`, `AppMonitorAdapter`, `ScreenMonitorAdapter`, drag/mouse monitors |
| `effects/`      | `EffectSubscriber` → `TilingEffect` → `EffectExecutor`, `window_ops`, `window_cache`, `animation/`      |
| `layout/`       | `calculate_layout[_full]`, dwindle/split/master/grid/monocle/floating, `Gaps`                           |
| `rules/`        | Window-rule matching and the "never tile these" lists                                                   |
| `identity.rs`   | `AppIdentity` (pid + launch date) and `WindowTarget`                                                    |
| `visibility.rs` | `VisibilityRegistry` for apps Stache hid, and restore on shutdown                                       |
| `borders.rs`    | `JankyBorders` configuration, focus colors, border animation                                            |
| `tabs.rs`       | Native tab detection (`AXTabGroup`/`AXTabs`) and the tab registry                                       |
| `window.rs`     | AX-first window/app enumeration                                                                         |
| `ffi/`          | Safe wrappers for Accessibility, SkyLight, transactions and fast window queries                         |
| `commands.rs`   | Helpers for other modules (`is_tiling_enabled`, `layout_to_string_pub`)                                 |

## Data flow

macOS sources → adapters → `EventProcessor` → `mpsc::Sender<StateMessage>` (buffer 1024, with a semantic overflow mailbox) → `StateActor` → observable `TilingState` → `EffectSubscriber` → `Vec<TilingEffect>` → `EffectExecutor` (AX writes, borders, Tauri events).

- The processor dispatches focus/create/destroy immediately and batches move/resize per screen with that screen's refresh rate (clamped 30–360 Hz).
- The actor is the **only** writer of state and of the visibility registry. It processes messages sequentially and catches panics per message so one bad event cannot kill tiling.
- Queries (`StateQuery` → `QueryResult`) never mutate. The command dispatcher (`bar/ipc_listener.rs`) uses a 5 s query timeout when it must resolve focus or a workspace first.
- Effects are derived from state, not called imperatively: subscribers compute deltas, the executor applies them.

## Lifecycle and generations

`init.rs` keeps `LIFECYCLE: Mutex<LifecycleState>` (`Stopped`/`Starting`/`Running`/`Stopping`) and `RUNTIME: Mutex<RuntimeSlot>` (`Empty`, `Running(TilingRuntime)`, `Quarantined(PartialRuntime)`). Each start bumps `NEXT_GENERATION`; adapters carry their generation and ignore events from an older one.

Startup is a staged factory (`build_runtime`): actor → processor → subscriber → app monitor → screen monitor → AX adapter → standalone observers → optional mouse monitor/borders → initial enumeration, then wait for the subscriber to report ready. A fatal stage failure drops the partial runtime, which closes channels and stops timers.

Teardown (`pause_runtime`) is ordered and idempotent, tracked per stage in `TeardownProgress`:
restore Stache-hidden apps → remove main-thread sources → stop processor → cancel drag, pause animations/mouse tap/borders → stop subscriber → stop actor → clear caches → publish `Empty`/`Stopped`.
A stage that times out puts the whole runtime back as `Quarantined`; only a later `pause` retries the unfinished stages, and `start`/`resume` refuse to run until the slot is empty.

## CLI

`stache tiling pause|resume`, `stache tiling query [--json|-d] {screens|workspaces|windows|apps}`, `stache tiling window --focus|--swap|--toggle-floating|--preset|--resize <dim> <px>|--send-to-screen|--send-to-workspace`, `stache tiling workspace --focus|--layout|--balance|--send-to-screen`. Multiple flags run in a fixed order (window: focus → swap → toggle floating → preset → resize → send). Commands return after the actor accepted the message, not after the layout was applied. See [ipc](../architecture/ipc.md).

## Config

Root key `tiling`: `enabled`, `defaultLayout`, `workspaces[]` (`name`, `layout`, `screen`, `rules[]`, `preset-on-open`), `ignore[]`, `animations`, `gaps` (global or per-screen), `floating` (`defaultPosition`, `presets[]`), `master` (`ratio`, `position`), `borders`. Types live in `config/types/{tiling,workspaces,gaps,borders}.rs`. Editing the config restarts the app in release builds (debug only logs), so tiling always starts from a fresh snapshot; see [config](../architecture/config.md).

Empty `workspaces` creates one default workspace per screen; a configured workspace whose screen is missing falls back to the main screen.

## Invariants and pitfalls

- Two `LayoutType` enums exist: `config::types::tiling::LayoutType` (default `Dwindle`, what users write) and `state::LayoutType` (default `Floating`, what the runtime stores). Convert in one place (`actor/handlers/screen.rs::convert_layout_type`).
- Never mutate state outside the actor, and never key delayed work by window ID alone — use `WindowTarget { identity, window_id }`. Window IDs are recycled by the OS.
- `AppIdentity` is `(pid, launch_date)` on purpose: a pid alone is reused after an app quits. Some apps report `NSNotFound` (`-1`) as their pid; `identity::process_id_for_app` resolves the real pid by executable path.
- `set_layout` is a no-op when the layout is unchanged (it also protects runtime ratios); `balance` is a no-op when no ratios or inferred minimums exist. Do not "always apply" on these paths.
- Layout writes must not fight the user: move/resize events are ignored while an animation is active or settling (`should_ignore_geometry_events`) because they are our own writes, and while a drag is in progress only the frame is updated (relayout waits for mouse-up).
- The bar shifts tiling: `bar.height + bar.padding` is added to the top outer gap on the main screen only, when `bar.enabled`.
- Add-on services (mouse monitor, borders) are optional: a failure logs "degraded mode" and tiling still runs.

## Subdocs

- [events-and-identity](tiling/events-and-identity.md) — AX/app/screen observers, batching, identity, tabs, hidden-app ownership.
- [layout-and-effects](tiling/layout-and-effects.md) — layout algorithms, gaps, minimum sizes, subscriber/executor, animation, borders.

## Tests

Inline `#[cfg(test)]` modules across the module: `init.rs` (latch, quarantine, staged failures, idempotent pause), `actor/*` (handlers, message routing, minimum sizes), `layout/*` (every algorithm and gaps), `effects/*` (subscriber deltas, cache pruning, animation), `identity.rs`, `visibility.rs`, `borders.rs`, `tabs.rs`, `rules/mod.rs`, `config/types/{tiling,workspaces,gaps,borders}.rs`, `cli/commands/tiling.rs` (clap parsing). Run `pnpm test:native`, or narrow it with `cargo nextest run -p stache -E 'test(/tiling/)'`.
