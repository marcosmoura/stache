---
branch: feat/accessibility-gate
---

# Accessibility launch gate

Read when checking the app-wide Accessibility launch gate's implementation and decisions. All five parts are done on `feat/accessibility-gate`; this record does not authorize further implementation.

## Goal

Stache Next must not run without macOS Accessibility trust. On every launch the host checks trust before any module starts. Without it the app enters a **gated** state: no module runs, the tray, socket and CLI say why, and a guidance surface explains how to grant it. The grant is detected without a restart; modules then start normally. Revocation while running stops every module and returns to the gate.

Motivation: in a manual test Hold-to-Quit silently did nothing until the user found and granted Accessibility for "Stache Next" by hand. A missing grant must never be silent again. Ask for approval before implementing each part.

## Non-goals

- No Input Monitoring, Screen Recording or other permission in the gate (see Resolved questions).
- No automatic `tccutil` call, no TCC database access, no private permission API. (Tauri's `macos-private-api` feature for the guidance window's transparency is allowed by Decision 9; it authorizes no other private API.)
- No Settings window, onboarding beyond this one permission, or change to module configuration files.
- No change to the legacy app, its identity or its privacy records.
- No change to the coordinated teardown order beyond the gate's own owners.

## Decisions

Decisions 1–4 and 6–8 restate the user's request. Decision 5 and the resolved questions below were confirmed by the user on 2026-11-07 (all recommended answers).

1. **App-wide requirement.** Accessibility (`Permission::Accessibility`) is required by the app, not by individual modules. Every registered module, whatever its configuration, waits while trust is missing, including modules that need no Accessibility themselves (proxyAudio, noTunes, Keep Awake, Wallpapers, ordinary Keybindings shortcuts).
2. **Subsume, do not replace, the per-module machinery.** The registry gains one host-level baseline requirement merged into every module's accepted requirements (part 01). The existing `missing_permission` start check, `WaitingForPermission(accessibility)` status, `on_permission_change` revocation stop and grant restart in dependency order are reused unchanged. `Module::required_permissions` and `PermissionRequirement::shared_input` stay for future, non-baseline permissions; for Accessibility they become redundant but harmless. Nothing caches a grant.
3. **What still runs while gated.** The tray, instance lock, config store and watcher (changes are accepted and staged; modules stay waiting), the permission ticker and activation observer, the socket server (part 03), the Caps Lock startup recovery (it restores user state and needs no Accessibility), the test window and Quit. `execOnStartup` is deferred until the gate first opens in that launch and runs at most once per launch.
4. **Prompt policy.** The baseline requirement is user-only. The host never calls `PermissionProvider::request` on its own; only an explicit action in the guidance surface or tray does. Showing the guidance surface itself is automatic.
5. **Surface: a dedicated webview window** (`?surface=access`, label `stache-access-<n>`), opened automatically when the gate blocks, not a native `NSAlert`. Reasons: it reuses the shipped window backend, router, typed bindings and status feed, so it closes itself on the `status-changed` event that reports the grant; it runs in the WebKit browser suites with fakes; it is non-modal and never blocks the AppKit main thread that the tray, activation observer and teardown hops share. `NSAlert.runModal` would block that thread, need new unsafe AppKit code, cannot dismiss itself on grant without extra native plumbing and is only manually testable. Cost: a gated launch creates one webview after setup (logged as `access: guidance window requested`; the `startup: ... webviews=N` record is written before the first check and stays `0`); a trusted launch still creates none. **Confirmed by the user on 2026-11-07.**
6. **Grant detection.** The existing `PermissionManager`/`PermissionTicker` poll at the 1 s waiting delay while the gate blocks, plus the `NSApplicationDidBecomeActiveNotification` refresh. Once open, the shipped backoff (5 s to 30 s) and activation refresh detect revocation.
7. **Revocation.** A fresh `Denied`/`Unknown` while open stops every running module in reverse start order through `on_permission_change`, returns to the gate and opens the guidance surface again.
8. **Identity.** The check is for the running process's own identity (`com.marcosmoura.stache.next`, "Stache Next"). The guidance shows the name, bundle identifier and on-disk path being checked, so the user does not toggle the legacy "Stache" entry. Nothing terminates or signals processes by executable name.
9. **Guidance window design (part 05).** On 2026-11-08 the user asked for a modern guidance window that matches the bar's look and has no title bar ("The UI completely sucks. Create a nice modern UI that matches the bar aesthetics, but with no title bar."). The user confirmed these choices:
   1. Only the access surface is frameless and transparent: `decorations(false)`, `transparent(true)`, not resizable, native shadow on. This enables Tauri's `macos-private-api` cargo feature and `app.macOSPrivateApi: true`, which the Bar port needs anyway and the legacy app used. The test window keeps its decorated, opaque chrome. There are no traffic lights. A custom close button and Escape close the window, and the header is a drag region. Only `access-window.json` gains `core:window:allow-start-dragging`. The size may change from 520×420 within the backend bounds; the final size is recorded.
   2. The typeface is the bar's root stack `'Maple Mono NF', 'SF Mono', monospace` at 13px, weight 500, with tabular numbers and line-height 1.6em. These fonts are system-installed and never bundled.
   3. Icons are Hugeicons: `@hugeicons/react` 1.1.10 and `@hugeicons/core-free-icons` 4.4.0, exact pins, using the `HugeiconsIcon` component with free icons.
   4. The redesign is recorded as part 05. Part 04 stays `done` as the record of the window's behavior, and its copy is unchanged.

## Intentional compatibility changes

1. `docs/architecture/overview.md` says "normal operation never prompts for Accessibility" and that shared-input consumers are user-prompt-only with "later consumers must not opt into automatic prompting". New contract: Accessibility is a launch requirement of the whole app; the host still never calls the system prompt automatically, but it automatically shows guidance and blocks every module until trust exists.
2. Modules that need no Accessibility (proxyAudio, noTunes, Keep Awake, Wallpapers, ordinary shortcuts) no longer run without it. Their status reads `waiting for permission`.
3. The per-module tray row `Request Accessibility…` is replaced by the gate rows (part 02).
4. `execOnStartup` runs after the first grant in a launch, not right after setup.
5. While gated, module socket methods answer `unavailable` (part 03); `stache wallpaper set` therefore refuses instead of applying.
6. A gated launch creates one webview (part 04).
7. The release spec note "No shipped module needs Accessibility" (`docs/specs/2026-11-03-release/04-identity-switch.md`) is no longer true; the identity switch must expect a fresh grant for the renamed bundle.

## Parts

| Part                               | Summary                                                                                         | Status |
| ---------------------------------- | ----------------------------------------------------------------------------------------------- | ------ |
| [01](01-sdk-baseline.md)           | SDK: host-level baseline permission requirement in `Registry`                                   | done   |
| [02](02-host-gate.md)              | Host gate state, status DTO, deferred startup commands, revocation, tray rows, settings URL     | done   |
| [03](03-socket-cli.md)             | Socket and CLI behavior while gated: refused module methods, additive `access` on core calls    | done   |
| [04](04-guidance-window.md)        | Guidance window surface, commands, capability, auto-open/close and copy                         | done   |
| [05](05-guidance-window-design.md) | Guidance window redesign: frameless transparent chrome, bar styling, close button, Escape, drag | done   |

Order: 01 → 02 → (03, 04) → 05. 03 and 04 both need 02 and are independent of each other; 05 needs 04. Each part builds, tests and commits alone. Only the orchestrator changes status; a part that is not `done` blocks merge.

## Ad-hoc signed development builds

Development bundles are signed ad hoc (`Signing with identity "-"`). TCC binds an ad-hoc grant to the code hash, so a rebuild can leave a "Stache Next" entry that is switched on in System Settings but no longer matches; `AXIsProcessTrusted` then returns false. An unbundled binary (`bun run tauri:dev`) may have trust attributed to the launching terminal application instead. Part 04's copy covers both: if Stache Next is already on, remove the entry with the minus button and add the current app again (or toggle it off and on); for an unbundled binary, the terminal may need the grant. Developers can reset only Next's record with `tccutil reset Accessibility com.marcosmoura.stache.next` while Stache Next is quit; the app never runs it.

## Resolved questions

The user accepted every recommended answer on 2026-11-07.

1. **Surface (Decision 5).** A webview window, not an `NSAlert`, for the reasons in Decision 5.
2. **Input Monitoring.** The gate covers Accessibility only. `Permission::InputMonitoring` is added later as a per-module requirement only if the manual tap check (part 02, AG-02-M05) shows a tap failing with Accessibility granted.
3. **Socket while gated.** `core.status` and `core.reload` answer with an additive `access` field; every module method answers `unavailable` with the gate message, including read-only ones (`keepAwake.status`, `proxyAudio.list`). Standalone CLI commands are unaffected because they never contact the app.
4. **`stache reload` exit code while gated.** Exit 0 (files were applied) with one stderr notice that modules wait for Accessibility.
5. **Buttons and copy.** Primary **Request Access** (explicit `request`, which adds the entry and may show the macOS alert), secondary **Open System Settings** (opens the pane directly), tertiary **Quit Stache Next**, with the copy drafted in part 04.
6. **Auto-open on revocation.** Yes: the guidance window opens again at most once per gated episode; closing it does not reopen it until the next episode or a tray click.

## Validation and safety

Automatic tests use fake `PermissionProvider`s with scripted status sequences, a recording settings opener, fake window backends and the WebKit suite with mocked bindings. No test calls `AXIsProcessTrusted*` with a prompt, opens System Settings, edits TCC or launches Stache Next. Each manual check (listed per part) needs explicit user approval for that run and runs only against Stache Next, never the legacy app.

## Affected docs

Lasting docs change only in the part that ships the behavior:

- Part 01: [sdk.md](../../architecture/sdk.md) (state table and "Permission requirements and prompt policy").
- Part 02: [overview.md](../../architecture/overview.md) (Desktop host, permission contract for shared-input consumers), [tray.md](../../features/tray.md), [ui.md](../../architecture/ui.md) (`HostStatus` DTO), [keybindings.md](../../features/keybindings.md) (Accessibility note), [new-module.md](../../guides/new-module.md), [testing.md](../../guides/testing.md).
- Part 03: [ipc.md](../../architecture/ipc.md).
- Part 04: [ui.md](../../architecture/ui.md) (surfaces, capability), [performance.md](../../guides/performance.md) (gated launch webview count), [tray.md](../../features/tray.md).
- Part 05: [ui.md](../../architecture/ui.md) (access surface chrome, final size, capability list, transparent window and `macos-private-api` note), [capabilities/README.md](../../../src/crates/app/capabilities/README.md), and [tray.md](../../features/tray.md) only if its wording about the window changes.
- After merge: [roadmap](../../roadmap.md) and [index](../../index.md) rows; the release spec note in `2026-11-03-release/04-identity-switch.md` and the Hold-to-Quit/MenuAnywhere drafts' `shared_input` wording are aligned by the orchestrator.
- Legacy docs stay untouched.
