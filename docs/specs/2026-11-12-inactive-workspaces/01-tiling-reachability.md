---
status: draft
---

# 01 — Tiling: reachability, refusal and published data

## Scope

The Rust side of inactive workspaces: the reachability rule in the Tiling pure state, the refused focus intent, the published state, the Bar strip data and the CLI answer. The Bar UI does not change in this part, except that test fixtures and fake-host data gain the new fields so type checks pass.

Files expected to change: `src/crates/modules/tiling/src/{workspaces.rs, intent.rs, state/intents.rs, state/mod.rs, published.rs, ipc.rs}`, `src/crates/app/src/bar_tiling.rs`, the generated `src/ui/src/ipc/bindings.gen.ts`, UI fixtures under `src/ui/tests/` and `src/ui/e2e/`, and the docs below.

## Requirements

- **Rule.** Implement decision D1 of the [overview](00-overview.md) as one pure function over `TilingState` (working name `unreachable`, returning the workspace ids). "No live window" uses the same notion as the published window list (a departing window does not count; a minimized or hidden one does).
- **Shared host selection.** Rules 2 and 3 call the same code the visibility planner uses to decide that Finder hosts and to pick a windowless host. Factor that selection so that "a host exists for this state" can be asked without the planner's "a hide is wanted now" preconditions. No second copy of the candidate rules.
- **Startup.** Until startup has tried every listed application (`completed_once`), a workspace with no windows that is not visible is inactive unless rule 1 holds.
- **Focus intent.** `Intent::FocusWorkspace` on an inactive workspace returns a new rejection (working name `IntentRejection::InactiveWorkspace`). The state is unchanged apart from the refusal record: no focus change, no workspace history entry, no effect. The existing "already visible and focused" no-op is checked first and is unchanged.
- **Refusal record.** The state keeps the last refused focus: the workspace id and a counter that increases by one on every refusal (wrapping `u32`). A new Tiling generation starts with no record.
- **Cycle.** `Intent::CycleWorkspace` steps over inactive workspaces. With fewer than two workspaces left on the display it is the existing no-op.
- **Other intents.** `SendWindowToWorkspace` to an inactive workspace, rule assignment of a new window and every other intent behave as today.
- **Snapshot and publication.** The snapshot carries the result of the rule and the refusal record; the projection only copies them. `PublishedWorkspace` gains `inactive: bool`. The published state gains the last refused focus as an optional value with the workspace name and the counter; it is absent when there is none or when its workspace no longer exists. A new counter value is a publication; nothing else about a refusal is.
- **Schema.** Decision D8.
- **Strip.** `StripWorkspace` gains `inactive`. `TilingStripView::Available` gains the refused focus (working name `refusedFocus { workspace, serial }`). Regenerate the bindings (`bun run generate-bindings`); `bun run check:generated` passes.
- **Bar click.** `bar_tiling_action` with `focusWorkspace` on an inactive workspace answers `refused` and produces the same refusal record. No new outcome kind.
- **CLI.** `stache tiling workspace --focus <name>` on an inactive workspace fails with the fixed handler error `Workspace is inactive`, with the same exit status and the same multi-operation behaviour as the existing `Workspace not found`.
- **Log.** The refusal is visible in the existing intent log line. No new warning per press.
- **UI fixtures.** Add the new fields to the UI test fixtures and fake-host data (`inactive: false`, no refused focus) so `bun run lint` and `bun run test` pass. No UI behaviour changes.
- **Docs.** `docs/features/tiling.md`: an "Inactive workspaces" entry with the definition, what is refused, what still works (D4, D5) and the startup rule; the CLI error in the CLI section. `docs/features/bar.md`, "Tiling strip data": the two new fields. `docs/architecture/ipc.md` where it lists them.

## Acceptance criteria

- [ ] IW-01-01: The rule matches D1 for each of these states: no windows anywhere; Finder without a window; Finder with a window on a hidden workspace and no windowless application; the same with a windowless application; two displays with and without a window on the other display's visible workspace; the target visible; the target holding only a minimized window; startup not complete.
- [ ] IW-01-02: Focusing an inactive workspace is rejected, changes nothing but the refusal record, sends no native request and adds no history entry. The counter increases on every refusal.
- [ ] IW-01-03: For every state of IW-01-01, `inactive` in the published state is true exactly when `FocusWorkspace` is rejected as inactive.
- [ ] IW-01-04: With the scenario desktop that keeps the last visible application, focusing an empty workspace that is not inactive ends with only the host on screen, and never logs `cannot show an empty desktop` or `hide abandoned`.
- [ ] IW-01-05: Sending a window to an inactive workspace works and makes it active; a rule-assigned new window does the same; the cycle skips inactive workspaces.
- [ ] IW-01-06: The published state and the strip carry `inactive` and the refused focus; a refusal publishes once; a removed workspace clears the refused focus; a new generation starts without one.
- [ ] IW-01-07: The CLI and `bar_tiling_action` answer as specified.
- [ ] IW-01-08: Bindings are regenerated, UI fixtures compile, and the docs describe the shipped behaviour.
- [ ] IW-01-09: `bun run lint`, `bun run format:check`, `bun run check:generated` and `bun run test` pass.

## Tests

- Pure-state tests for the rule matrix (IW-01-01) and for the intent, the counter and the cycle.
- A table test for IW-01-03 that builds each state once and checks both the projection and the intent outcome.
- Published-state tests for the projection and the publish gate.
- IPC and CLI tests for the error text, the exit status, a multi-operation request and the exact-focus outcome.
- A realistic scenario in the Tiling scenario suite (`runtime_tests/scenarios.rs`, with the `last_app_kept` desktop), modelled on the user's setup: nine workspaces on one display, Ghostty on `terminal`, a Finder window on `files`, every other application with windows. Pressing the key for `tasks` is refused and nothing native happens. The Finder window closes, `tasks` becomes active, the key works and only the desktop is left. Coming back focuses Ghostty. A second run with two displays shows rule 1.
- The same session through the app's strip path in `src/crates/app/src/bar_tiling_e2e.rs`: the strip's `inactive` flags, a refused click with its refused focus, and the flags flipping when the Finder window closes.
- No real sleeps; time comes from the test clocks.

## Notes

- The planner already handles the state this part prevents users from entering by a switch (`kept_last`, the abandoned-hide retry). It stays for the routes the non-goals name.
- The windowless host rule has not been exercised on a real desktop yet. If it is removed before this part starts, rule 3 goes with it and IW-01-01 loses that case.
