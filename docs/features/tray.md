# Tray

Read when changing the menu bar item, its rows, or the actions it triggers. **Shipped
behavior.** The source of truth is `src/crates/app/src/tray.rs`.

## Purpose

The tray is the only UI surface of the desktop app. It shows host and module status and
carries the actions that do not need a window: configuration-backed Enable, session-only
Pause/Resume, Retry, the explicit permission request, the test window and Quit. There is
no menu bar application menu, no Dock icon and no settings window.

## Creation and updates

- `tray::create` runs during Tauri setup, after `set_activation_policy(Accessory)` and
  before the host starts its slow services. The tray id is `stache-next`; the tooltip is
  `Stache Next`; the icon is the app's `default_window_icon()`.
- `tray::update` rebuilds the whole menu from a fresh status snapshot and replaces it
  with `set_menu`. It must run on the AppKit main thread; `AppUi::status` dispatches it
  through the host main-thread executor after emitting `status-changed`.
- A rebuild that fails leaves the existing menu in place and reports the Tauri error.
- `AppUi::exit` removes the tray by id on the main thread as part of shutdown.

## Rows

`rows(&HostStatus)` is a pure description, so wording and eligibility are testable
without AppKit. It produces, in order:

| Row                        | Kind       | Content                                                       |
| -------------------------- | ---------- | ------------------------------------------------------------- |
| `status`                   | non-action | `Stache Next`, or `Stache Next — starting…` before readiness  |
| `error`                    | non-action | Host error, truncated to 80 characters                        |
| `enable:<id>`              | check item | `<id> enabled`; checked state is the configuration flag       |
| `state:<id>`               | non-action | Two-space indent, `<id>: <state label>`                       |
| `detail:<id>`              | non-action | Runtime detail when present, truncated                        |
| `diagnostic:<id>`          | non-action | `configuration: <diagnostic>`, truncated                      |
| `pause:<id>`/`resume:<id>` | menu item  | Present for an enabled module; label follows the paused state |
| `retry:<id>`               | menu item  | Only when enabled, not paused, and `Failed` or `Unavailable`  |
| `permission`               | menu item  | `Request Accessibility…` when any module waits for permission |
| `config:<file>`            | non-action | File diagnostic, truncated                                    |
| `warning:<file>:<index>`   | non-action | File warning, truncated                                       |
| `test-window`              | menu item  | `Open Test Window`                                            |
| `quit`                     | menu item  | `Quit Stache Next`                                            |

Separators sit between the status block, the test window and Quit.

State labels are `stopped`, `disabled`, `running`, `paused`, `waiting for permission`,
`unavailable` and `failed`. A non-action row is a disabled menu item, so it can never be
clicked and never implies an action it does not have.

## Actions

`handle_event` maps callback ids to one host command each and returns immediately:

- `enable:<id>` reads the current snapshot and requests the opposite of the accepted
  flag. Enable is configuration-backed: plan, validate, prepare and sync the replacement,
  recheck for an external change before the atomic rename, then apply once. A missing
  target is committed with a no-clobber rename; a symlink is refused; an invalid file is
  never overwritten.
- `pause:<id>` / `resume:<id>` are session-only and never write a file.
- `retry:<id>` retries an eligible failed or unavailable module.
- `permission` triggers the explicit permission request.
- `test-window` opens or focuses the development test window.
- `quit` calls `HostHandle::request_shutdown`, the one idempotent teardown coordinator.

## Invariants and pitfalls

- Menu callbacks never join a module, wait for the socket, or read configuration
  inline; they only enqueue work on the host worker.
- All menu construction and mutation runs on the main thread. Building a menu off it is
  a crash, not a warning.
- The menu is rebuilt from a snapshot, not mutated in place. Row ids are stable strings
  derived from the module id and the file name, so renaming a module only changes its
  row ids.
- Configuration diagnostics are displayed separately from runtime state: an invalid file
  never hides which module is running and never silently resets it.
- Paused is runtime-only; a restart returns every module to its configured state.
- There is no Reload item. Configuration reloads per module and `stache reload` exists,
  so restarting the whole app for a configuration change is no longer part of the model;
  see the note in the [roadmap](../roadmap.md). Whether to bring a release-only Reload
  item back is a product decision recorded there, not shipped behavior here.

## Tests

The pure row description is covered in `src/crates/app/src/tray.rs`: truncation on a
character boundary, a running module showing a plain state row with no Retry, Retry
eligibility only for an enabled unpaused failed module, module and file diagnostics as
separate non-action rows, and paused/disabled rows offering no Retry. A real tray click
remains a manual check.
