---
status: draft
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

- [ ] Status shows drag availability, counters and last outcome in table and JSON; `cli_tests`/`ipc_tests` cover both.
- [ ] Scenario: Grid workspace with three windows, user drags one window to an empty area and releases (moves streamed during the gesture, late echoes after) → it returns to its cell; counters show one snap-back.
- [ ] Scenario: user drags a window onto another tile → they swap, both written to each other's cells.
- [ ] Scenario reproducing the live capture: user resizes a Grid tile's width, then late growth echoes arrive → no inferred minimum, all three tiles end in a valid non-overlapping layout (exact geometry from part 02 once it lands; here, at least back in the planned frames).
- [ ] Scenario: a gesture whose read is rejected or coalesced still commits within the same turn sequence.
- [ ] These scenarios fail on `main` before the fix (worker states which failed and why).
- [ ] `docs/features/tiling.md` Passive drag and Status describe the new status fields and the fixed behaviour.

## Tests

End-to-end scenarios over the real sharded adapter with the fake AX arena (as in `runtime_tests/scenarios.rs`): user gestures are modeled as drag signals plus arena-side frame changes and the notifications/snapshot replies macOS sends, in realistic order and timing (moves during the hold, a final notification racing the up, one or two late echoes). Unit tests for the status rendering.

## Notes
