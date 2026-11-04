# Tray

Read when changing the menu bar item, its rows, or the actions it triggers. **Shipped
behavior.** The source of truth is `src/crates/app/src/tray.rs`.

## Purpose

The tray is the primary, always-present UI surface of the desktop app. It shows host and
module status and carries the actions: configuration-backed Enable, session-only
Pause/Resume, Retry, the explicit permission request, the on-demand test window and Quit.
The test window is the only other surface, and it exists only while it is open.

There is no Dock icon and no settings window. The app supplies no menu, so Tauri installs
its default macOS application menu; under the Accessory activation policy no menu bar is
displayed, but the menu object and its Quit key equivalent exist, which is why Cmd-Q
reaches AppKit.

## Creation and updates

- `tray::create` runs during Tauri setup, after `set_activation_policy(Accessory)` and
  before the host starts its slow services. The tray id is `stache-next`; the tooltip is
  `Stache Next`; the icon is the app's `default_window_icon()`.
- `tray::update` rebuilds the whole menu from a fresh status snapshot and replaces it
  with `set_menu`. It must run on the AppKit main thread; `AppUi::status` dispatches it
  through the host main-thread executor after emitting `status-changed`.
- A rebuild that fails leaves the existing menu in place. The error is not surfaced:
  `AppUi::status` discards the value `tray::update` returns, so a failed rebuild produces
  no log line and no status diagnostic.
- The coordinator's terminal exit removes the tray by id on the main thread; AppKit's
  terminate reply path instead answers once and lets AppKit terminate.

## Rows

`rows(&HostStatus)` is a pure description, so wording and eligibility are testable
without AppKit. It produces, in order:

| Row                        | Kind       | Content                                                       |
| -------------------------- | ---------- | ------------------------------------------------------------- |
| `status`                   | non-action | `Stache Next`, or `Stache Next — starting…` before readiness  |
| `error`                    | non-action | Host error, truncated                                         |
| `enable:<id>`              | check item | `<id> enabled`; checked state is the configuration flag       |
| `state:<id>`               | non-action | Two-space indent, `<id>: <state label>`                       |
| `detail:<id>`              | non-action | Two-space indent, runtime detail when present, truncated      |
| `diagnostic:<id>`          | non-action | Two-space indent, `configuration: <diagnostic>`, truncated    |
| `pause:<id>`/`resume:<id>` | menu item  | Present for an enabled module; `Pause <id>` or `Resume <id>`  |
| `retry:<id>`               | menu item  | `Retry <id>`, only when enabled, not paused, and failed       |
| `permission`               | menu item  | `Request Accessibility…` when any module waits for permission |
| `config:<file>`            | non-action | `<file>: <diagnostic>`, truncated                             |
| `warning:<file>:<index>`   | non-action | `<file>: <warning>`, truncated                                |
| `test-window`              | menu item  | `Open Test Window`                                            |
| `quit`                     | menu item  | `Quit Stache Next`                                            |

Separators sit between the status block, the test window and Quit.

Every truncated payload has a budget of 80 characters, and the trailing `…` is part of
that budget. Truncation applies to the payload before indentation and prefixes are added,
so a rendered line can be longer than 80 characters.

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
  Cmd-Q and other AppKit-initiated quits reach the same coordinator through the observed
  `applicationShouldTerminate:`; their real-session behavior is not verified yet, so the
  [roadmap](../roadmap.md) follow-up stays open.

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
  so restarting the whole app for a configuration change is no longer part of the model.
  A release-only Reload item is planned, and it depends on the coordinated restart path
  from the planned teardown spec on the unmerged `feat/teardown` branch; see the note in
  the [roadmap](../roadmap.md).

## Tests

The pure row description is covered in `src/crates/app/src/tray.rs`: truncation on a
character boundary, a running module showing a plain state row with no Retry, Retry
eligibility only for an enabled unpaused failed module, module and file diagnostics as
separate non-action rows, and paused/disabled rows offering no Retry. A real tray click
remains a manual check.
