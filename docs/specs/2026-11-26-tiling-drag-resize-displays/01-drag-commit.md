---
status: done
---

# 01 — Drag health and the gesture commit

## Scope

Make passive drag observable, reproduce the live failure (tiled windows stay where the user dropped or resized them, with no snap-back or relayout) in a realistic end-to-end scenario, and fix its root cause. Resize mapping itself is part 02; this part only guarantees that every finished gesture ends with tiles back in a valid layout.

Files: `src/crates/modules/tiling/src/drag.rs`, `state/drag.rs`, `state/frames.rs` (`force_deviating`, `decide`, `observe_growth`, `absorb_echo`), `state/transition.rs` (snapshot/geometry completions), `status.rs`, `ipc.rs`/`dto.rs` and `cli/render.rs` for status output, `runtime_tests/scenarios.rs` (or a new `runtime_tests/drag.rs`).

## Requirements

1. `tiling query status` (table and `-j`) adds a drag section: `available` (bool) with the unavailable reason from `DragFeed::subscribe`/`DragSignal::Unavailable`, `active` (gesture in progress), counts of gestures, swaps, resizes, snap-backs, stale and cancelled commits, ignored downs, and the last outcome with its travel. Additive JSON fields only; existing keys unchanged.
2. A gesture that ends (commit or cancel) always leads to one plan whose writes return every captured tiled window to its planned frame (or the new frame after swap/resize). A window the user moved or resized during the gesture is never skipped by that plan because of a settled attempt, an exhausted external budget, a settle-echo budget or an inferred minimum.
3. Geometry the user produced during a gesture, including notifications and snapshot replies that arrive after the up and after the commit, never creates or raises an inferred minimum. Echo absorption never swallows the user's final frame.
4. The gesture commits even when reads come back through any completion path (snapshot, geometry, rejected) or the read is coalesced with an in-flight read of the same window; a gesture never waits for the 60 s timeout in normal use.
5. The root cause is named in the commit message and in the Notes of this part, with the scenario that failed before the fix.

## Acceptance criteria

- [x] Status shows drag availability, counters and last outcome in table and JSON; `cli_tests`/`ipc_tests` cover both.
- [x] Scenario: Grid workspace with three windows, user drags one window to an empty area and releases (moves streamed during the gesture, late echoes after) → it returns to its cell; counters show one snap-back.
- [x] Scenario: user drags a window onto another tile → they swap, both written to each other's cells.
- [x] Scenario reproducing the live capture: user resizes a Grid tile's width, then late growth echoes arrive → no inferred minimum, all three tiles end in a valid non-overlapping layout (exact geometry from part 02 once it lands; here, at least back in the planned frames).
- [x] Scenario: a gesture whose read is rejected or coalesced still commits within the same turn sequence.
- [x] These scenarios fail on `main` before the fix (worker states which failed and why).
- [x] `docs/features/tiling.md` Passive drag and Status describe the new status fields and the fixed behaviour.

## Tests

End-to-end scenarios over the real sharded adapter with the fake AX arena (as in `runtime_tests/scenarios.rs`): user gestures are modeled as drag signals plus arena-side frame changes and the notifications/snapshot replies macOS sends, in realistic order and timing (moves during the hold, a final notification racing the up, one or two late echoes). Unit tests for the status rendering.

## Notes

Root cause (2026-11-26, reproduced in `runtime_tests/animation/gestures.rs` over the real sharded adapter and the fake AX arena; candidates (c) and (d) were not the cause, the gesture always committed in every scenario):

1. **A late read of the user's own resize inferred a minimum (candidate a).** After the commit the gesture is gone, so `frames::observe_growth` judges the next read of the resized window against the window's last settled attempt, whose target is the cell from before the gesture. The user's wider frame looks like the window re-asserting a size it refused and becomes `minimum.inferred`; from then on `frames::decide` skips every target smaller than it. Scenario `a_grid_tile_grown_by_the_user_never_leaves_an_inferred_minimum_or_an_overlap` failed on unmodified code with `minimum: Some(Size { width: 1160.0, height: 0.0 })` on a Grid tile widened by 200 points.
2. **The user's frames were swallowed as animation echoes (candidate b).** An animated plan leaves `settle_echoes = 2` on each window until its next write. With animations on (the user's setup), the first two reads of a gesture's window, typically the application's one end-of-move notification and the drag's own read, were absorbed by `frames::absorb_echo`, so the commit saw the pre-gesture frame: a resize was decided as a snap-back with nothing to write, and `force_deviating` forced nothing. Scenario `a_resize_right_after_an_animated_layout_change_relays_out` failed on unmodified code with outcome `SnappedBack` instead of `Resized`. The same absorption hides a drop; a drag recovered only when a third, late read spent the one external attempt.

Fix: a down clears the captured windows' settle-echo budget (`frames::clear_settle_echoes`); the commit (and a cancel) records each window's user span, baseline to final frame (`FrameRecord::user_span`), and `observe_growth` never learns from a frame inside it; the commit also resets the external budget of every captured window, so a final read that arrives after the commit (a read already running at the up answered first) still brings the window back once.
