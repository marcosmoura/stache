---
branch: feat/inactive-workspaces
---

# Inactive workspaces

Read when planning or implementing the rule that a workspace Stache cannot show as an empty desktop is inactive. Drafted 2026-11-12.

## Goal

A workspace with no windows that Stache cannot show as an empty desktop is **inactive**. The Bar dims it, and a request to focus it (key binding, CLI or Bar click) is refused and answered with a flash of its Bar item, instead of switching the state while the previous application stays on screen.

## Background

macOS never hides the last visible regular application. A switch to an empty workspace therefore works only while some regular application can stay visible with nothing on screen: Finder without a window, or another application without one (see [Tiling](../../features/tiling.md), "Empty workspaces and Finder"). When neither exists, the previous application cannot be hidden. Measured on the user's machine on 2026-11-12: `-[NSRunningApplication hide]` and an `AXHidden` write do nothing in that state, an active accessory application does not count, Hide Others still leaves one regular application, and Finder refuses to make a window invisible.

## Prerequisites

`fix/tiling-empty-workspace-switch` is merged into `main` first (user decision 2026-11-12). It provides the desktop host rules (`visibility_requirement`, the Finder host entry, `windowless_host`), the kept-last fallback and the abandoned-hide retry this spec builds on.

## Non-goals

- No native HUD. The refusal feedback is a flash in the Bar only (user decision 2026-11-12).
- No new way of taking windows off screen. Stache never minimizes or un-minimizes windows, never moves them off screen, never closes or covers them, never becomes a regular (Dock) application and never uses extra macOS Spaces (user decisions 2026-11-12).
- An empty workspace that can be shown stays switchable. "Every empty workspace is inactive" was considered and rejected (user decision 2026-11-12).
- No change to what happens when the focused workspace becomes empty by another route (its last window closes, is sent away or is reassigned by a reload). The existing behaviour, including the kept-last fallback, stays.
- An application that only has ignored or excluded windows does not count as "an application that stays visible". The rule stays conservative there.
- No new configuration key, and no change to `stache tiling query` output.

## Decisions

- **D1. Definition.** A workspace is inactive when all of these hold: no live window is assigned to it; it is not visible on its display; and it is not reachable. It is reachable when startup has tried every listed application and at least one of these holds:
  1. a workspace that is visible on another display has at least one window;
  2. Finder is running and has no window Stache tracks (Finder hosts the desktop);
  3. the planner's windowless host selection finds a host (another running application with no window Stache has listed; only while Finder is running, because the planner uses such a host only while Finder is held hidden).
- **D2. One source of truth.** One pure function in the Tiling state decides. The focus intent, the workspace cycle and the published state all use it, so the Bar never shows a workspace as active that a key press would refuse, or the reverse. Rules 2 and 3 use the same host selection as the visibility planner; if the planner's rule changes, the definition changes with it.
- **D3. Refusal.** `FocusWorkspace` on an inactive workspace is rejected. Nothing changes and nothing native is sent. The state records the refusal (the workspace and a counter) and publishes it, which is what the Bar flashes on. The CLI answers with a typed error; a Bar click answers `refused`.
- **D4. Other ways in stay open.** Sending a window to an inactive workspace and a rule that assigns a new window to it work as today. The workspace then has a window and is no longer inactive.
- **D5. The focused workspace is never inactive.** A workspace that is visible on its display, with or without windows, is not inactive. It is drawn as today.
- **D6. Bar.** An inactive workspace is dimmed and marked `aria-disabled`. A click is still sent to Tiling, which decides; there is no second rule in the UI. A refusal flashes the item once. Under reduced motion the flash does not animate.
- **D7. Names.** The user-facing word is "inactive" (published field `inactive`). `workspaces.rs` already uses `inactive` for windows on hidden workspaces, so the pure-state notion is named `unreachable` there.
- **D8. Published schema.** The new published fields are additive with defaults. `tilingState` keeps its version unless the SDK's compatibility rule requires a bump; the worker follows `docs/architecture/sdk.md` and reports which applied.

## Parts

| Part                               | Summary                                                                             | Status |
| ---------------------------------- | ----------------------------------------------------------------------------------- | ------ |
| [01](01-tiling-reachability.md)    | Tiling: the reachability rule, the refused focus, published and strip data, the CLI | done   |
| [02](02-bar-inactive-workspace.md) | Bar: the dimmed workspace item and the refusal flash                                | done   |

After part 02 the user tries it in the running app; the steps are in part 02's notes. All native behaviour stays unverified until then.

## Affected docs

`docs/features/tiling.md` (a new "Inactive workspaces" entry, the CLI error), `docs/features/bar.md` (Tiling strip data and Tiling strip UI), `docs/architecture/ipc.md` if it lists the Tiling handler errors or the strip fields, `docs/guides/testing.md` if it lists the suites touched, and the rows in `docs/index.md` and `docs/roadmap.md`.
