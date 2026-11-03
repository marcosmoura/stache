# Hold-to-Quit (commandQuit)

Read when changing the ⌘Q interception, the hold timer, the quit/alert behaviour, or the commandQuit lifecycle.

Rust paths are relative to `app/native/src/`.

## Purpose

Replaces the instant ⌘Q with "hold ⌘Q for `holdDuration`". A tap shows a hint instead of quitting; a hold terminates the frontmost application. Inspired by the Hammerspoon HoldToQuit Spoon.

## Entry points

| Caller                                     | Entry                                                   |
| ------------------------------------------ | ------------------------------------------------------- |
| App startup (background, `spawn_blocking`) | `cmd_q::init(handle, &config.command_quit)` in `lib.rs` |
| Tray                                       | `cmd_q::CmdQLifecycle` (registered in `lib.rs`)         |

`init` returns early when `commandQuit.enabled` is false; otherwise it stores the duration and app handle, then spawns the event-tap thread and the timer thread.

## Key types

- `config/types/command_quit.rs`: `CommandQuitConfig { enabled, hold_duration }` (milliseconds, default 1500), `hold_duration_secs()`.
- `modules/cmd_q/mod.rs`: `HoldState { press_start, quit_triggered }`, `HOLD_STATE`, `CHECK_QUIT`, `EVENT_TAP`, `CmdQLifecycle`.

## Data flow

1. A HID `CGEventTap` (key down + key up) filters for keycode `12` (`Q`) and requires Command **only** — Shift, Control or Option present means the event passes through untouched.
2. Matching events are swallowed (callback returns null) — the normal macOS quit never runs. Key down starts `press_start` (repeat presses are ignored) and sets `CHECK_QUIT`; key up clears the state and, if no quit was triggered, emits the hint.
3. `timer_loop` polls: 100 ms while idle, 16 ms while a press is tracked. Once elapsed ≥ duration and `quit_triggered` is false, it calls `kill_frontmost_app` and clears the flag.
4. `kill_frontmost_app` sends `terminate` to `NSWorkspace.frontmostApplication`, falling back to `forceTerminate`, and logs which one worked.

## Config and event

Root key `commandQuit` (`#[serde(rename = "commandQuit")]`). `enabled` defaults to `false`; `holdDuration` defaults to `1500`. The doc comment on `StacheConfig::command_quit` says "enabled by default" — the struct default and tests say otherwise. See [config](../architecture/config.md).

Hint event: `stache://cmd-q/alert` (`events::cmd_q::ALERT`) with a `String` payload ("Hold ⌘Q to quit <app>"). It is defined in `app/ui/types/tauri-events.ts` as `CmdQEvents.ALERT`, but nothing in `app/ui` subscribes to it today; the only visible fallback is the `tracing::debug!` line.

## Permissions and threading

- Accessibility is required: `CGEventTapCreate` fails without it and logs "check accessibility permissions"; the tray then reports `Unavailable`.
- The tap owns a dedicated thread and CFRunLoop; the timer is another thread. The AppKit calls in `kill_frontmost_app`/`get_frontmost_app_name` run on the timer thread — `NSWorkspace` is used read-only there, and nothing is dispatched to the main thread.
- The Accessibility check is cached at startup, so granting the permission later needs a restart.

## Invariants and pitfalls

- Only `Command+Q` is intercepted. `Command+Shift+Q`, `Command+Option+Q` and friends keep their normal behaviour.
- Because both key down and key up are suppressed, apps never see ⌘Q while the tap is enabled; pausing via the tray disables the tap and restores stock behaviour immediately, while a config-disabled module never creates the tap at all.
- `quit_triggered` is per press: holding through the threshold quits once, and the key-up afterwards does not emit a hint.
- State is only rebuilt on key up. If the tap is disabled mid-hold the release can be missed; `on_key_down` starts timing only when `press_start` is `None`, so a later press does not reset the old start time.
- Emitting the alert cannot fail the operation; a missing frontend listener is silent.

## Lifecycle and hot reload

`CmdQLifecycle` id `commandQuit`. `pause`/`resume` call `CGEventTapEnable` on the retained tap and error when no tap exists; nothing is torn down. `status`: `ConfiguredOff` when disabled, `Running` when the tap is enabled, `Paused` when it exists but is off, `Unavailable` when `IS_RUNNING` is set but no tap exists.

## Tests

`modules/cmd_q/mod.rs` (`cmd_q_status_maps_states`, `HoldState` defaults/flag, `RetainedEventTap` `Send + Sync`), `config/types/command_quit.rs` (defaults, `holdDuration` deserialization, seconds conversion).
