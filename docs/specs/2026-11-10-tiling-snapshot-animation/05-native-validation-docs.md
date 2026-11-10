---
status: draft
---

# 05 — Native validation and lasting docs

Read when validating snapshot animation on a real desktop and writing the lasting docs, after parts 01-04. **DRAFT — NOT SHIPPED.** This part needs separate approval, and **every gate below needs its own per-run approval** under [validation](../2026-11-03-tiling/validation.md#per-run-native-approval). This document authorizes no native run. Paths are under `src/crates/` unless noted.

## Scope

- Native gates N1-N9, recorded in [guides/performance.md](../../guides/performance.md) (N7 and the latencies of N2 and N8) and in this part's Notes (pass or fail per gate, with raw evidence).
- An optional gated check binary under `app/src/bin/`, behind its own feature that is never built by default (precedent `menu-native-check`). It is used only if a gate cannot be run through `bun run tauri:dev`.
- Lasting docs: `docs/features/tiling.md`, `docs/guides/performance.md`, `docs/architecture/overview.md`, `docs/guides/testing.md`, `docs/roadmap.md`.

## Requirements

### Every gate

Each per-run approval request names:

- the exact applications and windows (owned test windows where possible);
- the display, with its refresh rate and scale;
- the temporary Next config: `tiling.toml` with `[animations]` `enabled = true`, `duration = 200`, `easing = "ease_out"`, the `style` under test, and one workspace in `master` layout; and `global.toml` `[logging] level = "debug"`;
- the actions;
- the permissions involved;
- the exact cleanup: restore the user's config files, close owned test windows, leave no overlay on screen and no window parked in a corner, and revoke a grant only if the approval said so.

A refused or unrun gate stays **unverified**, never passed. Never launch, quit or kill an application by shared executable name, and never touch user windows outside the approved list.

### Gates

- **N1 — foreign ordering.** Does `orderWindow(.above, relativeTo:)` with another application's `CGWindowID` place the overlay directly above that window, below a higher window of a third application? Record the result and whether the level fallback was needed. Also record whether the overlay stays directly above its window after that window is parked (decision 8).
- **N2 — capture.** Single-window ScreenCaptureKit capture of a window that is occluded, one that is moving (during a live animation) and one on another Space. Record per-window latency p50/p95 for 4 applications and 8 windows, and the share within the 50 ms budget. The target-image capture of a parked window is N8.
- **N3 — permission.**
  - The first activation prompts exactly once per session.
  - A denied grant gives all-live with the diagnostic.
  - Record whether a grant takes effect without a relaunch.
  - Record whether an ad-hoc rebuild (`bun run tauri:build` or `tauri:dev`, `signingIdentity: "-"`) loses the grant.
  - Record which process TCC attributes the grant to under `tauri:dev`.
  - Record any periodic re-confirmation macOS shows.
- **N4 — self-exclusion.** Overlays with `sharingType = none` never appear in Stache's own captures, either in a single-window capture of the window under an overlay or in a display capture taken by the check.
- **N5 — Spaces.** Switch Space, open Mission Control and enter or leave full-screen during an animation. An overlay never shows on another Space, in Mission Control, or over a full-screen window, and none outlives the hard ceiling. No window stays parked after the switch or after full-screen is entered. Record the cross-display case (OQ-3).
- **N6 — visual judgment.** On the user's 120 Hz display, with Ghostty, Zed, Safari or Aside, and Spotify: 10 master → monocle, 10 monocle → master and 10 master → grid toggles, with `style = "snapshot"` and then `style = "live"`.
  - The user judges smoothness, the stretch, the cross-fade, the reveal and the missing border during the motion (decision 14).
  - The user confirms that no window ever appears twice (no visible duplicate of a window and its overlay) and that there is no visible flash when parking or revealing.
  - **Pass** = the user accepts snapshot as smooth for all four applications. Record any proposed constant tuning (`SHOW_BUDGET`, `REVEAL_FADE`) or park-corner change as a decision in the overview.
- **N7 — cost.** CPU, memory (peak while animating and after it), wakeups and thread count over the N6 run, for snapshot, live and animations off. Raw samples and method go into `guides/performance.md`.
- **N8 — capture of a parked window.** Single-window capture of a window parked in the hide corner (mostly off-screen) at its final size, for the N2 applications. Record whether ScreenCaptureKit returns the full window content or an error, empty or cropped image, whether it shows the new size's content, and the latency p50/p95 against half of `duration`.
- **N9 — park corner.** On the built-in display and on one external display, park owned test windows and the N6 applications. Record:
  - how much of each parked window stays visible (in points), and whether that is acceptable to the user;
  - whether macOS clamps the requested position, moves the window elsewhere (for example back on screen or onto another display), or refuses the write;
  - the neighbor-display corner rule with a display to the right of or below the parked display;
  - any visible flash when parking (between the show and the park) or revealing (between the reveal and the fade).

### Lasting docs (describe only what ships)

- **`docs/features/tiling.md` Animation:**
  - describe snapshot style: capture, overlay, park in the hide corner at the final size, keyframes, target image and cross-fade, position-only reveal, fade;
  - describe the Screen Recording behavior (non-gating, request once, ad-hoc rebuild and relaunch limits), the live fallback and its reasons, the stop order with `snapshot overlay` and `parked windows`, the metrics line and the known limits (missing border during the motion, the sliver left by the park per N9, clicks during the motion, a crash leaving a window parked until the next start);
  - correct line 180, which says on-screen presentation "cannot be observed without Screen Recording";
  - extend the "Unverified natively" bullet (line 181) with the gate results;
  - update the Stop bullet (line 169) with the new stages.

  Line 171 (no SkyLight) stays true. Remove the "Planned, not shipped: snapshot animation" line added with this spec.

- **`docs/guides/performance.md`:** the threads (no new Tiling thread; main-thread overlay work; ScreenCaptureKit's own queues), the N2/N7/N8 method and results, and the line at 269 about Screen Recording, revised if it is no longer accurate.
- **`docs/architecture/overview.md`:** the owner map gains the snapshot overlay owner on main and the capture adapter in `src/crates/macos`. The HUD sentence (lines 18-21) and the statement that the applications adapter never uses Screen Recording (line 30) stay true.
- **`docs/guides/testing.md`:** `FakeScreenCapture`, `FakeSnapshotOverlay`, the end-to-end scenarios and the native gates with their status.
- **`docs/roadmap.md`:** the row moves to Finished when merged, with any unverified gates named.

## Acceptance criteria

- [ ] SA-05-01: N1 is run under its own approval and recorded, or listed as unverified.
- [ ] SA-05-02: N2 is run under its own approval with raw latency samples recorded, or listed as unverified.
- [ ] SA-05-03: N3 is run under its own approval and recorded, or listed as unverified.
- [ ] SA-05-04: N4 is run under its own approval and recorded, or listed as unverified.
- [ ] SA-05-05: N5 is run under its own approval and recorded, or listed as unverified.
- [ ] SA-05-06: N6 is run under its own approval with the user's verdict per application, or listed as unverified.
- [ ] SA-05-07: N7 is run under its own approval with raw samples and method in `guides/performance.md`, or listed as unverified.
- [ ] SA-05-08: The lasting docs above describe only shipped behavior, the planned line is removed, and the roadmap row is updated.
- [ ] SA-05-09: N8 is run under its own approval with raw latency samples and the image result recorded, or listed as unverified.
- [ ] SA-05-10: N9 is run under its own approval with the visible sliver, the clamp behavior and the flash verdict recorded, or listed as unverified.

## Tests

- No new automated test is required. If the check binary is added, it builds only behind its feature, and `bun run lint` and `bun run test` stay green without it.
- `bun run lint`, `bun run format:check` and `bun run test` pass after the doc changes.

## Notes

- Gate results go here, one entry per gate: the date, the approval reference, the environment (macOS build, hardware, display), the result and the cleanup performed.
- A gate failure that needs a code change is handled by the orchestrator as a fix in the owning part's area, with a recorded decision. It never weakens a pass criterion here.
