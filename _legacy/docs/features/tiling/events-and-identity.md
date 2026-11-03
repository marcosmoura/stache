# Tiling: events, identity and app ownership

Read when touching AX/app/screen observers, geometry batching, window identity, tab handling, or the code that hides and restores apps.

Paths are relative to `app/native/src/modules/tiling/`.

## Sources and adapters

| Source                                       | File                       | Produces                                                                                                                     |
| -------------------------------------------- | -------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `AXObserver` per app (main-thread CFRunLoop) | `events/observer.rs`       | `WindowEvent` for created/destroyed/focused/unfocused/moved/resized/minimized/unminimized/title, plus app activate/hide/show |
| AX → processor bridge                        | `events/ax_observer.rs`    | `AXObserverAdapter`; builds `WindowCreatedInfo`, applies ignore rules and PiP filtering                                      |
| `NSWorkspace` notifications                  | `events/app_monitor.rs`    | launch/terminate/activate/deactivate/hide/show; adds and removes AX observers                                                |
| Display configuration                        | `events/screen_monitor.rs` | screen add/remove/geometry snapshot, taken on the main thread                                                                |
| Global mouse buttons (`CGEventTap`)          | `events/mouse_monitor.rs`  | drag start/end detection; `set_active` gates it                                                                              |
| Drag/resize bookkeeping                      | `events/drag_state.rs`     | `DragInfo`; `init.rs::on_mouse_up` resolves swaps and ratio changes                                                          |

Adapters are installed per runtime generation and gated off during teardown (`shutdown`, `set_active(false)`); a callback from an older generation is dropped.

## Batching

`events/processor.rs::EventProcessor` routes each window to a screen (window→screen map, display frames) and then:

- dispatches focus/create/destroy immediately;
- batches move/resize per screen, coalescing by `(AppIdentity, window_id)`, on a timer of `1 / refresh_rate` (clamped 30–360 Hz, 60 Hz fallback);
- reconciles the AX fullscreen attribute before forwarding geometry (`on_window_fullscreen_changed`);
- remaps routes after a display change using each window's last known frame;
- exposes `stop_and_wait(timeout)` so teardown never races a live timer.

The processor is the only producer of `StateMessage`s; it never mutates state and never touches the visibility registry.

## Identity

`identity.rs`:

- `AppIdentity { pid, launch_date: LaunchDateBits }` — `launch_date` is `NSRunningApplication.launchDate` as `f64::to_bits`, falling back to the kernel process start time when AppKit returns null (Finder does). Both must be valid or the app is skipped.
- `WindowTarget { identity, window_id }` — the exact target for any delayed, cached or cross-thread window operation.
- `process_id_for_app` — `processIdentifier` can be `NSNotFound` (`-1`) even for a running app (e.g. `com.apple.dt.Devices`, Device Hub); it resolves the real pid by matching `executableURL` against the kernel process table.
- `is_process_alive`, `take_identities_for_dead_processes` for pruning.

The actor drops any `WindowFocused`/`WindowUnfocused`/`WindowDestroyed` whose `identity` does not match the tracked window (`StateActor::window_event_matches`), so reused window IDs cannot resurrect stale state.

## Which windows are managed

`rules/mod.rs`:

- `matches_window` — a rule with none of `app_id`/`app_name`/`title` is invalid (`is_valid`) and never matches; `app_id` is an exact case-insensitive match, `app_name` and `title` are case-insensitive substrings, and all specified fields must match (AND).
- `should_tile_window` — rejects `SKIP_TILING_BUNDLE_IDS` / `SKIP_TILING_APP_NAMES` (Dock, Control Center, WindowServer-ish components, `borders`, Stache itself).
- `should_tile_window_with_rules` adds the config `tiling.ignore` list.
- `is_pip_window` — `AXFloatingWindow` subrole, never tiled.

Assignment on create: matching workspace rule → focused workspace on the window's screen → default workspace.

## Tabs

macOS tabs are separate `CGWindowID`s sharing one visible window. `tabs.rs` walks `AXTabGroup` → `AXTabs` and reads each tab's window ID via `_AXUIElementGetWindow`, registering it in the global `TabRegistry`. Tab windows are skipped by layout (they are not separate tiles), and tab create/destroy must not trigger relayout. Startup scans once per app identity (`scan_and_register_tabs_for_app`); teardown calls `clear_all_tabs`.

## Hidden-app ownership

`visibility.rs`:

- `VisibilityRegistry` holds a `BTreeSet<AppIdentity>` plus a one-way `sealed` flag, shared between `StateActor` and `StateActorHandle`; the actor is the sole runtime writer and uses the atomic `hide_if_open` / `unhide_if_open` (sealed check → OS op → set mutation under one lock).
- `hide_app_instance_with_outcome` re-resolves `NSRunningApplication`, refuses the hide if the captured identity differs (PID reuse), and falls back to the Accessibility `AXHidden` attribute because **`NSRunningApplication.hide()` is a no-op for some apps**. The unhide path mirrors this.
- `restore_stache_hidden_apps` seals and drains the registry, then unhides only identities that still validate and are actually hidden. It runs before any actor teardown, while `get_handle()` still resolves.
- `relinquish_if_open` drops ownership when the app terminates on its own; termination paths never call `hide`.

## Pitfalls

- Never hide/unhide apps from the event processor or in response to a generic "window closed" signal. Only the actor, through the registry, may.
- A sealed registry stays sealed; a new runtime must build a new one.
- Don't add `prepare()`-dependent rule matching without running `StacheConfig::prepare` (it precomputes the lowercase caches for ignore, workspace and border rules). Note the `app_id` fast path is the only correct one; the `app_id_lower == None` fallback compares the rule's value with itself.
- `proc_listallpids` returns a **pid count**, not bytes; dividing it truncates the scan (see `identity::pid_list_count`).
- Observers are main-thread only. Register and remove them inside `platform::thread::dispatch_on_main_sync`.

## Tests

`events/processor.rs` (routing, refresh-rate batching, timer stop), `ax_observer.rs` (event → message, fullscreen reconcile), `app_monitor.rs`, `screen_monitor.rs`, `mouse_monitor.rs`, `drag_state.rs`, `observer.rs`, `identity.rs` (launch-date capture, NSNotFound path, `i32::MAX`), `tabs.rs`, `visibility.rs` (exact-instance restore, seal/drain), `rules/mod.rs`.
