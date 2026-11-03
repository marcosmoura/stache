# noTunes

Read when changing the Apple Music launch blocking, the replacement-app launch, or the noTunes lifecycle.

Rust paths are relative to `app/native/src/`.

## Purpose

Stops Apple Music (and legacy iTunes) from launching when macOS asks for it — media keys, Bluetooth headphone reconnects, `open`-style activation — and optionally launches a preferred player instead. Inspired by tombonez/noTunes.

## Entry points

| Caller                                     | Entry                                                |
| ------------------------------------------ | ---------------------------------------------------- |
| App startup (background, `spawn_blocking`) | `notunes::init()` in `lib.rs`                        |
| Tray                                       | `notunes::NoTunesLifecycle` (registered in `lib.rs`) |

`init` returns early when `notunes.enabled` is false, then caches `target_app` in the `TARGET_APP` `OnceLock` and sets up the observer on the main thread.

## Key types

- `config/types/notunes.rs`: `NoTunesConfig { enabled, target_app }`, `TargetMusicApp::{Tidal, Spotify, Feishin, None}` with `app_path()`, `bundle_id()`, `display_name()`.
- `modules/notunes/mod.rs`: `init`, `IS_RUNNING`, `OBSERVER`, `TARGET_APP`, `NoTunesLifecycle`.

## Data flow

1. On the main thread: `setup_workspace_observer` registers a dynamically created `NoTunesObserver` (`NSObject` subclass, class name `NoTunesObserver`) with `NSWorkspace.notificationCenter` for `NSWorkspaceWillLaunchApplicationNotification`; then `terminate_music_apps` force-terminates any already-running `com.apple.Music` / `com.apple.iTunes`.
2. On each notification, `handleAppLaunch:` reads `NSWorkspaceApplicationKey`, compares the bundle id against the two hardcoded ids, and if it matches calls `forceTerminate` then `launch_target_app`.
3. `launch_target_app` does nothing for `TargetMusicApp::None`, warns when the app path is missing, returns when the bundle id is already running, and otherwise runs `/usr/bin/open <app_path>`.

## Config

Root key `notunes`. `enabled` defaults to `false`; `targetApp` defaults to `spotify` and also accepts `tidal`, `feishin`, `none`. See [config](../architecture/config.md).

## Permissions and threading

- No Accessibility or other TCC permission; this only uses `NSWorkspace` notifications plus `forceTerminate`.
- All `NSWorkspace` / `NSNotificationCenter` work must happen on the main thread. Use `platform::thread::dispatch_on_main_sync`; pause removes the observer that way.
- The class is registered once (`Class::get("NoTunesObserver")` guard) and the observer instance is retained by the notification center, so `OBSERVER` only keeps a pointer for `removeObserver:`.

## Invariants and pitfalls

- `TARGET_APP` is a `OnceLock`: changing `targetApp` requires an app restart even if the module is paused and resumed.
- `get_target_app()` falls back to `Tidal` when the slot was never set — only reachable if the observer fires without a successful `init`.
- Bundle ids are hardcoded; a renamed or containerised Music app is not blocked.
- `forceTerminate` is a race, not a veto: the app may appear briefly before it disappears.
- Blocking happens only for the notification. Launching Music directly from Stache's own CLI/config commands is not intercepted.
- No status is shown anywhere except the tray; there is no event or Tauri command.

## Lifecycle and hot reload

`NoTunesLifecycle` id `notunes`. `pause` removes the observer and clears `IS_RUNNING`; `resume` re-registers the observer with the cached target app. `status`: `ConfiguredOff` when disabled, `Running` when an observer is retained, `Unavailable("observer registration failed")` when `IS_RUNNING` is set but no observer exists, `Paused` otherwise. Config changes restart the app — see [config](../architecture/config.md).

## Tests

`modules/notunes/mod.rs` (`no_tunes_status_maps_states`, `is_music_app` for both bundle ids, `ObserverPtr` `Send + Sync`), `config/types/notunes.rs` (default target, path/bundle/display tables).
