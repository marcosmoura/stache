---
status: draft
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

- [ ] Pure tests per layout: left edge, right edge, top, bottom, both edges of an axis, an outer edge, a clamp at a minimum, and an uneven grid row.
- [ ] Scenario (end to end, as part 01): Grid with three windows, user narrows the left tile to 631 wide → the left column is 631 wide and both right tiles fill the rest without overlap.
- [ ] Scenario: Master (left) with three windows, user drags the boundary between the two stack windows down → stack heights change, master unchanged; dragging the master's right edge moves the master ratio.
- [ ] Scenario: split_horizontal with three windows, the middle window's left edge dragged → only the left and middle tiles change.
- [ ] Scenario: dwindle, a resize of an edge belonging to an ancestor split moves that split.
- [ ] `--resize` CLI and `balance` tests still pass; a layout or tile count change resets the new ratios.
- [ ] `docs/features/tiling.md` describes mouse resize per layout and updates the Grid paragraph.

## Tests

Pure planner tests for the mapping and layouts; end-to-end scenarios with streamed user resize notifications and late echoes.

## Notes
