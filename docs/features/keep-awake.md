# Keep Awake

Read when changing the keep-awake toggle, the session-lock handling, or the coffee-cup button in the status bar.

Rust paths are relative to `app/native/src/`; UI paths relative to `app/ui/`.

## Purpose

Holds a macOS power assertion so the machine stays awake (display, idle and system sleep all prevented), and releases it while the session is locked. The status bar shows a coffee-cup button whose state reflects the controller.

## Entry points

| Caller                    | Entry                                                                               |
| ------------------------- | ----------------------------------------------------------------------------------- |
| Bar startup               | `modules/bar/mod.rs::init` → `components::init(window)` → `keepawake::init(window)` |
| Frontend                  | Tauri commands `is_system_awake`, `toggle_system_awake` (registered in `lib.rs`)    |
| Lock/unlock notifications | `handle_lock_state_notification` → `signal_lock_refresh`                            |

There is no `LifecycleModule`: keep-awake is not in the tray's Modules submenu and cannot be paused at runtime.

## Key types

- `modules/bar/components/keepawake.rs`: `KeepAwakeController` (managed state, `Mutex<KeepAwakeState>`), `KeepAwakeState { desired_awake, handle: Option<KeepAwake> }`, `KeepAwakeChangedPayload { locked, desired_awake }`.
- `keepawake` crate: `Builder::default().display(true).idle(true).sleep(true).reason(…).app_name(…).app_reverse_domain(…)`; dropping the `KeepAwake` value releases the assertion.
- UI: `app/ui/renderer/bar/Status/KeepAwake/{KeepAwake.tsx,KeepAwake.state.ts}`.

## Data flow

1. `KeepAwakeController` is created with `app.manage` in `lib.rs`; `keepawake::init` calls `enable_awake()`, so Stache acquires an assertion on startup (`desired_awake = true`).
2. `toggle_system_awake` flips the desired state: on → acquire the handle if missing, off → set `desired_awake = false` and drop the handle.
3. A `lock-watcher` thread registers for `com.apple.screenIsLocked` / `com.apple.screenIsUnlocked` on `NSDistributedNotificationCenter` and, as a fallback, polls `CGSessionCopyCurrentDictionary` (`CGSSessionScreenIsLocked`) every 2 s through `start_best_effort_refresh_watcher`.
4. On lock: drop the handle (assertion released) but keep `desired_awake`. On unlock: re-acquire when `desired_awake` is still true. Both paths emit `stache://keepawake/state-changed` with `{ locked, desired_awake }` (`events::keepawake::STATE_CHANGED`).
5. The frontend queries `is_system_awake` on mount/focus (`['keep-awake']`) and invokes `toggle_system_awake` on click; the icon turns green while awake, and the button exposes `data-test-state="awake" | "sleep"`.

`is_awake()` reports `handle.is_some()`, i.e. it reflects the real assertion, not the desired flag.

## Config

None. Keep-awake has no config key and is active whenever the bar is enabled (`bar.enabled`). See [config](../architecture/config.md).

## Permissions and threading

- No Accessibility or other TCC permission; the assertion comes from the `keepawake` crate (IOPMAssertion).
- `NSDistributedNotificationCenter` observer registration and `CGSessionCopyCurrentDictionary` are called from the watcher thread; `init` itself runs on the main thread as part of bar setup. Emitting the Tauri event happens from the watcher thread.
- Repeated `init` is guarded by `LOCK_WATCHER_ONCE`, so only one observer and one poll loop exist.

## Invariants and pitfalls

- The state struct holds the real `KeepAwake` handle; dropping it is the only way to release the assertion. `ensure_awake_handle` only creates when the slot is empty, so a toggle cycle must clear the old handle.
- Locking does not change `desired_awake`: after unlock the assertion is restored without user action.
- The backend payload is an object (`{ locked, desired_awake }`), and `app/ui/types/tauri-events.ts` documents it that way. `KeepAwake.state.ts` subscribes with `useTauriEvent<boolean>` and writes the payload into the `['keep-awake']` query cache as-is, while `KeepAwake.tsx` treats it as a boolean (`isSystemAwake ? …`). The click path writes a real boolean. Keep this mismatch in mind when changing the event payload or the hook.
- `is_session_locked()` returns `false` when the dictionary has no `CGSSessionScreenIsLocked` key; the poll loop only emits when the value changes, so a flapping session does not spam events.
- Nothing releases the assertion on app exit explicitly; the process exit does.

## Lifecycle and hot reload

No lifecycle registration and no pause/resume. Config hot reload restarts the app, which drops the old assertion and re-acquires it on bar init — see `config/watcher.rs` and [config](../architecture/config.md).

## Tests

`modules/bar/components/keepawake.rs` (controller defaults, toggle transitions, lock/unlock payloads, `KeepAwakeChangedPayload` serialization, poll interval, constant sanity checks), `app/ui/renderer/bar/Status/KeepAwake/KeepAwake.test.tsx` (renders in sleep/awake state, icon present, clickable button).
