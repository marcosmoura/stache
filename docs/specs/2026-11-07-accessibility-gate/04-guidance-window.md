---
status: draft
---

# 04 — Guidance window

## Scope

- `src/crates/app/src/windows.rs` (`ACCESS_SURFACE`, backend surface check), `src/crates/app/src/host.rs` (auto-open/close), `src/crates/app/src/bindings.rs` (two commands), `src/crates/app/src/tray.rs` (one row), `src/crates/app/tauri.conf.json` and a new `src/crates/app/capabilities/access-window.json`.
- `src/ui/src/shell/components/AccessibilityGuide/` (component, `.styles.tsx`, colocated test, index), `WindowRouter` and generated bindings.
- [ui.md](../../architecture/ui.md), [performance.md](../../guides/performance.md), [tray.md](../../features/tray.md).

Depends on part 02. Implements Decision 5; if the user picks `NSAlert` instead, rewrite this part before implementation.

## Requirements

### Window lifecycle

1. New host-owned surface `access` (`?surface=access`), `WindowKind::Standard`, fixed key `access`, size about 520×420, label `stache-access-<n>`. `NativeWindows` accepts exactly `test` and `access` for host requests; every other surface stays refused.
2. On each `Blocked` transition (first check or revocation) the runner opens the window once for that gated episode and brings the app forward so the window is frontmost despite the Accessory policy. Closing it by the user does not reopen it during the same episode.
3. On `Opened` the runner closes the window through the existing release path. The window also reacts to the `status-changed` event (step 9) so it never shows stale guidance if the close races.
4. The tray, while blocked, gains `access-guide` **Show Accessibility Help…** as its first gate row, which opens or focuses the window (same owner/key reuse as the test window).
5. A trusted launch creates no webview; `startup: tray ready after N ms, webviews=0` is unchanged on that path. A gated launch reports the webview it created.

### Commands and capability

6. Two Tauri commands in `bindings::builder()`: `requestAccessibility()` → `HostHandle::request_permission()` and `openAccessibilitySettings()` → `HostHandle::open_accessibility_settings()`. Both return immediately with `()`; the result arrives through `status-changed`. Both are no-ops while the gate is open (part 02).
7. `access-window.json` grants the `stache-access-*` labels only `core:event:allow-listen`, `core:event:allow-unlisten`, `core:window:allow-close` and the two commands above; it is listed explicitly in `app.security.capabilities`. No filesystem, shell, opener, dialog or HTTP permission.
8. The quit button uses the existing tray-equivalent shutdown request through a third command `quitApp()` → `HostHandle::request_shutdown()`, granted only to the access label. (If the user prefers no quit button, drop this command.)

### React surface

9. `WindowRouter` accepts `access` and renders `AccessibilityGuide`, which reads `appInfo()` and the host status through the existing React Query feed, never through a new event name.
10. Content follows `access`: `checking` shows a short loading line; `blocked` shows the guidance below; `open` shows "Access granted. Starting Stache Next…" and closes itself (the host closes it anyway).
11. Draft copy (confirmed as Resolved question 5):
    - Title: **Stache Next needs Accessibility access**.
    - Why: "Stache Next reads keyboard shortcuts and controls windows and apps. macOS only allows that with Accessibility access. Until you grant it, Stache Next stays idle."
    - Steps: "1. Click **Request Access**, or **Open System Settings**. 2. In Privacy & Security > Accessibility, turn on **Stache Next**. 3. Come back here; Stache Next starts by itself."
    - Identity line: name, `identifier` from `appInfo()` and `access.appPath`, with "Make sure you turn on Stache Next, not Stache."
    - Troubleshooting (collapsed): "Stache Next is already on but this window stays open? The entry belongs to an older build. Select it, remove it with −, then turn on the new entry or add the app at the path above." When `appPath` is null: "This copy is not an app bundle. macOS may assign the permission to the app that launched it, such as your terminal."
    - When `revoked`: title **Accessibility access was turned off** and "Stache Next stopped all features until it is turned on again."
    - Buttons: **Request Access** (primary), **Open System Settings**, **Quit Stache Next**.
12. Buttons disable while their command is in flight and show a host action error from `HostStatus.error` when present. Styling uses the shared Linaria tokens; no new design system.

## Acceptance criteria

- [ ] AG-04-01 Host test: a blocked startup opens exactly one `access` window via the fake window backend; a grant closes it; a revocation opens one again; a user close followed by more blocked ticks opens none.
- [ ] AG-04-02 A trusted startup opens no window and the startup record says `webviews=0`.
- [ ] AG-04-03 `NativeWindows` accepts `access` and `test` host requests and still refuses any other surface, kind or size.
- [ ] AG-04-04 Tray `access-guide` opens or focuses the same window; it is absent when open.
- [ ] AG-04-05 The capability file lists only the permissions in step 7; a config test asserts `app.security.capabilities` lists it explicitly.
- [ ] AG-04-06 WebKit suite: `AccessibilityGuide` renders checking, blocked, revoked, unbundled (`appPath: null`) and open fixtures with the copy above; each button calls its mocked command once; the open fixture shows the granted message.
- [ ] AG-04-07 `WindowRouter` routes `access`, keeps `test` and the fallback for unknown or empty surfaces.
- [ ] AG-04-08 `bun run generate-bindings -- --check`, `bun run lint`, `bun run test` and `bun run test:ui:coverage` pass.

## Tests

Rust host tests with the fake window backend and scripted permission provider from part 02. UI tests colocated with the component, using the existing module mocks at the `@tauri-apps/api` boundary and the QueryClient helper in `src/ui/tests/`. No test opens a real window or calls a real command.

## Manual verification (approval needed for each run)

- AG-04-M01 Without the grant, launch the bundle: the guidance window appears in front within a second of the tray; it shows "Stache Next", `com.marcosmoura.stache.next` and the bundle path.
- AG-04-M02 **Request Access**: report what macOS shows (system alert, entry added to the list or nothing) on macOS 26.
- AG-04-M03 **Open System Settings**, turn Stache Next on: the window closes by itself and modules start, without a restart.
- AG-04-M04 Turn it off: the window reappears with the "turned off" copy; close it; it does not reopen until the next episode; the tray row reopens it.
- AG-04-M05 **Quit Stache Next** from the window: the normal teardown runs.
