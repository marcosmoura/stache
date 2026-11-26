---
branch: fix/tiling-drag-resize-displays
---

# Tiling: drag, mouse resize and display changes

Read when planning or implementing the 2026-11-26 fix for Tiling drag, mouse resize and display changes. Drafted from the user's report and live `stache tiling query` captures on `main` at `385f8412`.

## Goal

A tiled window cannot be left where the mouse dropped it. Resizing a tiled window with the mouse keeps the new size and the other tiles adjust to fit, in every tiling layout. A display that is connected, disconnected or reconfigured re-lays out every window on every workspace, hidden ones included.

## Background

Live evidence (2026-11-26, built-in 1920x1200 display only, Input Monitoring granted to Stache, `tiling query status` healthy with no faults):

- `files` workspace, Grid, three Finder windows after a mouse resize: `631x1130@14,56`, `939x558@967,56`, `939x864@967,322`. The resized window kept its new width, its neighbours did not follow, the two right-hand windows overlap, and nothing snapped back. Tiled windows can also be dragged anywhere and stay there.
- `coding` workspace, Grid, hidden, after the external monitor was unplugged: Zed windows keep `1259x1370@14,-184` and `1259x678@1287,-184`, frames from the old two-display arrangement that fall outside the only display left.

Code facts:

- Passive drag (`drag.rs`, `state/drag.rs`) captures a gesture on mouse down, suppresses planning while it lasts, reads every captured window after the up and commits: resize (`resized_window`, then `ratio_move` and `intents::apply_user_resize`), else swap, else snap-back through `frames::force_deviating`. The whole path is fake-proven and was never checked natively. Status (`tiling query status`) shows nothing about drag: whether the tap is installed, how many gestures ran, how they ended.
- Candidates for the live failure, none proven: a gesture that never commits, a commit whose plan writes nothing (`frames::decide` skips a window whose `minimum.inferred` came from late echoes of the user's own resize via `observe_growth` after the gesture ended, or whose attempt is not forced), or no gesture at all.
- `ratio_move` ignores which edge moved; Grid reacts to width only, through one global ratio, and snaps back on height; Master moves only the master boundary, so resizing one stack window against another does nothing.
- `frames::plan_and_issue` plans only visible workspaces (`note_revealed`), so windows on hidden workspaces keep whatever frame they had when their display changed.

## Non-goals

- Dragging a tiled window to another display or workspace, or making a dropped tiled window floating.
- Keyboard and CLI resize (`stache tiling window --resize`) beyond keeping them working on the new ratio model.
- Changing minimum-size inference outside the gesture case, Monocle and Floating behaviour, borders and animation.
- Moving or minimizing windows off-screen to hide them (never, see the inactive-workspaces rules).

## Decisions

- **Drag health is visible** (user, 2026-11-26): `tiling query status` (table and `-j`) reports whether passive drag runs (or why not), gesture counts by outcome and the last outcome.
- **Root cause first:** part 01 reproduces the live failure in an end-to-end scenario over the real sharded adapter and a fake AX arena that replays a user drag and a user resize as macOS delivers them (a stream of move/resize notifications during the gesture and late ones after the up), then fixes it. A user gesture never leaves an inferred minimum behind.
- **Edge-aware resize in every tiling layout** (user: "all of them"): split, split_vertical, split_horizontal, dwindle, master and grid. The edge the user dragged moves the boundary on that side; the resized window keeps its size along that boundary, clamped to the usable area and the neighbours' minimums; the other tiles sharing the boundary follow. A resize of both edges of one axis (corner drags included) moves both boundaries.
- **Master:** the master boundary as today, plus per-window weights along the stack axis, so resizing a stack window against its stack neighbour works. With several master windows the same weights apply inside the master area.
- **Grid row/column boundaries** (user): Grid keeps per-workspace column widths and row heights. Dragging a vertical edge moves that column boundary for every window in those columns; a horizontal edge moves that row boundary in the window's column group (rows of an uneven last row follow the same rule). The documented deviation that an _inferred minimum_ never reshapes the grid stays; only a user resize moves Grid boundaries.
- **Ratio lifetime:** new ratios follow the existing reset rules of the workspace ratios (layout change, tile count change, `balance`). No new configuration key.
- **Display changes re-lay out everything, now** (user): any display topology or geometry change (connect, disconnect, resolution, arrangement, visible-frame change) replans every workspace and writes every eligible window, hidden workspaces included. Writes to windows of runtime-hidden applications never unhide, activate or focus them. Revealing a workspace later finds its windows already in place.
- **Native check:** at the end, with the user's approval right before each launch, one release install run confirms drag snap-back, swap, a resize in Grid, Master and Split, and an unplug/replug.

## Parts

| Part                          | Summary                                                                                    | Status |
| ----------------------------- | ------------------------------------------------------------------------------------------ | ------ |
| [01](01-drag-commit.md)       | Drag health in status; reproduce and fix gestures that leave windows dropped               | done   |
| [02](02-edge-resize.md)       | Edge-aware mouse resize for split, dwindle, master (with stack) and grid                   | done   |
| [03](03-display-relayout.md)  | Display changes replan and write every workspace, hidden ones included                     | done   |
| [04](04-gesture-detection.md) | Start gestures from window geometry while a button is held (legacy model); live tap health | done   |

## Affected docs

- `docs/features/tiling.md`: Passive drag, Status, the Grid minimum-size deviation paragraph, display handling and the inactive-workspaces notes.
- `docs/architecture/ipc.md` if the status JSON shape is documented there.
