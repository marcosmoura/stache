---
status: draft
---

# 02 — Host gate, status and tray

## Scope

- New `src/crates/app/src/access.rs`: pure `AccessGate` state machine and the injected `SettingsOpener` trait.
- `src/crates/app/src/host.rs` (`Runner::start`, `refresh_permissions`, `permission_tick`, `start_startup_commands`, `RegistryPermissions::waiting`, `HostHandle`, `HostCommand`), `src/crates/app/src/permissions.rs` (`RefreshReport`), `src/crates/app/src/lib.rs` (registry baseline, production opener), `src/crates/app/src/tray.rs`, `src/crates/app/src/bindings.rs` and regenerated `src/ui/src/sdk/bindings.gen.ts`.
- New safe adapter `src/crates/macos/src/settings.rs`: `open_accessibility_settings()`.
- Docs listed for part 02 in the overview.

## Requirements

### Gate state

1. The composition root builds the registry with baseline `[PermissionRequirement::user_only(Permission::Accessibility)]` (part 01). Host fixtures keep the empty default unless a test opts in, so existing host tests are unchanged.
2. `AccessGate` is pure: states `Checking`, `Blocked { revoked: bool }`, `Open`. Input: one fresh Accessibility status. `Granted` → `Open`; `Denied` and `Unknown` → `Blocked` (fail closed). `revoked` is true when the previous state in this launch was `Open`. It returns the transition (`None`, `Blocked`, `Opened { first: bool }`, `Revoked`) so the runner performs effects without re-deriving them.
3. `Runner::start` performs the first nonprompting check through the injected `PermissionProvider` after factory registration and **before** `registry.start_all()`, then publishes. The registry's own per-slot check still guards every start, so a grant or revocation racing this check cannot start a module without trust.
4. `PermissionManager::refresh` reports the fresh status it read for each needed permission (for example `RefreshReport::statuses`), and the runner feeds the Accessibility entry to `AccessGate` after every refresh: startup, tick, activation, explicit, enable, pause, resume, retry and config apply. No second status call is made for the gate.
5. `RegistryPermissions::waiting` is true while the gate is not `Open`, so the ticker uses the 1 s waiting delay even with every module disabled. When open, the shipped backoff applies.
6. `Opened { first: true }` starts `execOnStartup` (moved out of `start`), at most once per launch. Later openings do not rerun it. A shutdown request suppresses it as today.
7. `Revoked` needs no extra stop call: `on_permission_change(Denied | Unknown)` already stopped every running module (part 01). The runner records it and publishes.
8. Each transition writes one info log record: `access: blocked (accessibility denied|unknown)`, `access: open`, `access: revoked`.
9. Teardown is unchanged: the gate owns no thread or native resource; a refresh after a shutdown request is still a no-op.

### Status

10. `HostStatus` gains `access: AccessView { state: AccessState, revoked: bool, appPath: Option<String> }`, with `AccessState` = `checking | blocked | open`. `appPath` is the enclosing `.app` bundle path of `current_exe()` when it has one, else `None` (unbundled binary). Exported through `bindings::builder()`; `bun run generate-bindings` is the only TypeScript change.
11. The `ready` flag keeps its meaning (host setup finished). A gated host is `ready: true` with `access.state = blocked`.

### Settings opener and explicit request

12. `stache_macos::settings::open_accessibility_settings() -> Result<(), SettingsError>` opens `x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility` through `NSWorkspace openURL:` on the main thread via the host main-thread executor. `false` from AppKit is an error. It is the only new unsafe code and documents its block.
13. `HostHandle::open_accessibility_settings()` enqueues `HostCommand::OpenAccessibilitySettings`; the runner calls the injected `SettingsOpener` only while the gate is not `Open`, records a failure as the last action error and publishes.
14. `HostHandle::request_permission()` stays the explicit path (`RefreshTrigger::Explicit`); with the baseline it requests Accessibility. While the gate is `Open` it requests nothing.

### Tray

15. While `checking`: status row `Stache Next — checking Accessibility…`. While `blocked`: status row `Stache Next — Accessibility required` (or `Accessibility was turned off` when `revoked`), followed by action rows `access-request` **Request Accessibility…** and `access-settings` **Open Accessibility Settings…**. Module rows stay and show `waiting for permission`; Enable, Pause/Resume, Test Window and Quit keep working.
16. The per-module `permission` row (`Request Accessibility…` when any module waits) is removed; gate rows replace it. When open, no gate row appears.
17. `rows(&HostStatus)` stays pure; callbacks only enqueue.

## Acceptance criteria

- [ ] AG-02-01 Untrusted fake provider at launch: the first published status after setup has `access.state = blocked`, every enabled module is `waitingForPermission`, no module `configure`/`start` ran, and the provider's `request` count is zero.
- [ ] AG-02-02 With every module disabled and an untrusted provider, the gate still blocks and the ticker delay is the waiting delay (1 s).
- [ ] AG-02-03 Scripted `Denied, Denied, Granted` on ticks: the gate opens on the third tick without a restart, modules start in dependency order, one status is published per transition and `execOnStartup` runs exactly once.
- [ ] AG-02-04 Activation refresh with a now-granted provider opens the gate before the next tick.
- [ ] AG-02-05 Open → `Denied`: every running module stops in reverse start order, `access = { blocked, revoked: true }`, and a second grant starts them again without rerunning `execOnStartup`.
- [ ] AG-02-06 `Unknown` behaves as `Denied`.
- [ ] AG-02-07 Explicit request while blocked calls `request` once per action; startup, tick, activation, enable, reload and config apply call it zero times.
- [ ] AG-02-08 `open_accessibility_settings` calls the fake opener once while blocked, zero times while open; an opener error appears in `HostStatus.error` and is cleared by the next successful action.
- [ ] AG-02-09 Tray row tests: checking, blocked, revoked and open snapshots produce the rows and labels above; no `permission` row exists in any snapshot.
- [ ] AG-02-10 `bun run generate-bindings -- --check` passes after regeneration; the WebKit test-window suite still passes with the new field in fixtures.
- [ ] AG-02-11 A shutdown requested while blocked produces the same teardown outcome as today and never calls `request` or the opener afterwards.

## Tests

- `access.rs` unit tests: every state × status transition table, including `revoked`.
- Host tests in `host.rs` and `src/crates/app/tests/host.rs` with a scripted `PermissionProvider` (status queue, request counter), a recording `SettingsOpener`, recording fake modules and a fake startup-command runner. No real `AXIsProcessTrusted*` call and no real URL open.
- `tray.rs` pure row tests.
- `settings.rs`: compile-checked only; no test opens System Settings.

## Manual verification (approval needed for each run)

Run only against a Stache Next bundle; never touch the legacy "Stache" entry. Record the starting grant state and restore it at the end.

- AG-02-M01 Remove the Stache Next Accessibility grant, launch: tray shows "Accessibility required", modules read "waiting for permission", an enabled Keep Awake holds no assertion (`pmset -g assertions` lists none from Stache Next).
- AG-02-M02 Click **Open Accessibility Settings…**: the Accessibility list opens on macOS 26. Turn Stache Next on: modules start within about one second, no restart.
- AG-02-M03 Turn it off while running: modules stop within 30 s or on the next activation; the tray shows "Accessibility was turned off".
- AG-02-M04 Rebuild an ad-hoc signed bundle without touching the existing grant and relaunch: report whether the gate blocks with the stale entry still shown as on.
- AG-02-M05 With Accessibility on and Input Monitoring off, enable a Caps Lock binding: report whether the tap installs (input to Resolved question 2).

## Notes

The `x-apple.systempreferences` URL is undocumented by Apple; AG-02-M02 is its verification. If it fails on macOS 26, fall back to opening `x-apple.systempreferences:com.apple.preference.security` and say so in the copy.
