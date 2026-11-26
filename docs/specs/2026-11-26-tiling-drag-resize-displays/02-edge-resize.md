---
status: done
---

# 02 — Edge-aware mouse resize in every tiling layout

## Scope

Replace the legacy `ratio_move` mapping with an edge-aware one so a mouse resize keeps the resized window's new size along the dragged edge and the neighbours sharing that edge adjust, in split, split_vertical, split_horizontal, dwindle, master and grid. Depends on 01 (a committed gesture always relayouts).

Files: `drag.rs` (`resized_window`, `ratio_move`, `ResizeContext`), `state/intents.rs` (`apply_user_resize`, `apply_ratio`, `resize_master`), `minimum_size.rs` (workspace ratios), `layout` planners for master and grid, their pure tests (`pure_tests/layouts.rs`, `pure_tests/grid.rs`, `pure_tests/minimum.rs`).

## Requirements

1. **Edge detection.** From the original and final frame, find which edges moved more than the resize threshold (left, right, top, bottom). Each moved edge maps to the boundary on that side; an edge on the usable area's outer border has no boundary and is ignored (the window then snaps back on that side).
2. **Split and split_vertical/horizontal:** a moved edge moves the boundary between the window and its neighbour on that side; only those two tiles change size along the split axis. An edge across the split axis is ignored.
3. **Dwindle:** a moved edge moves the dwindle split whose boundary is that edge, which may be an ancestor split (not only `position - 1`).
4. **Master:** the edge on the master boundary moves the master ratio (as today, either side). An edge between two stack windows (or two master windows) moves a per-window weight boundary along the stack axis, only between those two windows.
5. **Grid:** per-workspace column widths and row heights. A vertical edge moves that column boundary for every window in the two adjacent columns; a horizontal edge moves that row boundary for the windows in the adjacent rows of that column group. Layouts with an uneven last row keep a consistent rule, documented in the feature doc.
6. **Fit.** The resized window gets its new size along the boundary, clamped so no tile drops below its reported/inferred minimum or a floor (the existing ratio clamp), and so the layout still fills the usable area with the configured gaps. The plan after the commit writes the resized window too (its frame may differ slightly after clamping and gap snapping).
7. **Lifetime.** The new weights and grid boundaries follow the existing workspace-ratio reset rules (layout change, tile count change, `balance`); keyboard and CLI resize (`--resize`) and presets keep working.
8. `UserResize`/`DragOutcome::Resized { changed }` stay accurate; Monocle and Floating unchanged.

## Acceptance criteria

- [x] Pure tests per layout: left edge, right edge, top, bottom, both edges of an axis, an outer edge, a clamp at a minimum, and an uneven grid row.
- [x] Scenario (end to end, as part 01): Grid with three windows, user narrows the left tile to 631 wide → the left column is 631 wide and both right tiles fill the rest without overlap.
- [x] Scenario: Master (left) with three windows, user drags the boundary between the two stack windows down → stack heights change, master unchanged; dragging the master's right edge moves the master ratio.
- [x] Scenario: split_horizontal with three windows, the middle window's left edge dragged → only the left and middle tiles change.
- [x] Scenario: dwindle, a resize of an edge belonging to an ancestor split moves that split.
- [x] `--resize` CLI and `balance` tests still pass; a layout or tile count change resets the new ratios.
- [x] `docs/features/tiling.md` describes mouse resize per layout and updates the Grid paragraph.

## Tests

Pure planner tests for the mapping and layouts; end-to-end scenarios with streamed user resize notifications and late echoes.

## Notes

- **Mechanism.** `drag::edge_resize` is one solver for every layout: the workspace ratios (plus the Master share) form a parameter vector; the boundary of a moved edge is the parameter that moves that edge in the planned frames while changing the fewest tiles (found by a small probe), and its value is solved by bisection against the real planner so the edge lands where the user left it. Outer edges are those no parameter moves. No per-layout inversion formulas, so planner and resize cannot disagree.
- **Clamps.** Each parameter stays inside its run of cumulative boundaries with lanes of at least 5% (Dwindle splits and the Master share 10–90%, the existing clamps). Minimums: the moved boundary stops where the total minimum-size deficit would grow by more than `VIOLATION_TOLERANCE` (1 point) over the deficit before the resize, so a layout that already violates a minimum never blocks unrelated resizes.
- **Grid lattice.** Every Grid shape is a lattice of column and row lanes and the runtime ratios are its cumulative boundaries. Regular shapes keep the existing format (columns then rows). Spanning shapes (3, 5, 7, 10) now store `[master share, the other major-axis boundaries, the minor-axis boundaries]`, absolute over the axis; ratios[0] is still the master share, so `--resize`, the minimum solver and older one-value vectors keep working (missing boundaries default to equal stack lanes; out-of-order ones fall back to equal lanes for that axis).
- **Uneven-row rule.** With the shipped shapes only the portrait ten-window shape has an uneven row (its last row holds three cells, the first under the master). Rule: one lattice per shape; a spanning window follows every boundary on its border, cells of an uneven row use the same column and row boundaries as the rest, and a row boundary a spanning window crosses does not affect it.
- **Master.** `MasterConfig` has one master window, so "several masters" does not arise; stack weights are cumulative boundaries relative to the stack, stored in the workspace's split ratios (Master had none).
- **Tile count reset.** `RatioState` gains `tiles`, the count its ratios were written for; Grid and Master ratios for another count are ignored (lattice lengths collide across counts, e.g. 3 and 4). Split and Dwindle keep their existing validity rules.
- **Write tolerance.** After a resize the resized tile's new plan differs from where the user left it only by a rounding error, which the planner's write tolerance does not write; the part 01 gesture helper now compares within 0.5 point.
