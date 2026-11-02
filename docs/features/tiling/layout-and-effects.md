# Tiling: layout, effects and borders

Read when changing a layout algorithm, gaps, minimum sizes, window writes/animations, or border rendering.

Paths are relative to `app/native/src/modules/tiling/`.

## Layout entry

`layout/mod.rs`: `calculate_layout` → `calculate_layout_with_gaps` → `calculate_layout_full(layout, window_ids, screen_frame, master_ratio, gaps, split_ratios, master_position)`.

Returns `LayoutResult = SmallVec<[(u32, Rect); 16]>` (window id → frame). `LayoutType::Floating` returns an empty result: floating means "write nothing", not "write the current frames". Outer gaps are applied once up front; each algorithm receives the usable frame.

| `LayoutType`                                | File                | Shape                                                                                                   |
| ------------------------------------------- | ------------------- | ------------------------------------------------------------------------------------------------------- |
| `Dwindle`                                   | `layout/dwindle.rs` | Binary space partition spiral; per-node ratios                                                          |
| `Split`, `SplitVertical`, `SplitHorizontal` | `layout/split.rs`   | Even splits with cumulative ratios (`layout_auto` picks axis from the frame)                            |
| `Master`                                    | `layout/master.rs`  | One master at `master_ratio`, rest stacked; `MasterPosition::Auto` = left on landscape, top on portrait |
| `Grid`                                      | `layout/grid.rs`    | Balanced grid; **caps at `MAX_GRID_WINDOWS = 12`**, extra windows are not positioned                    |
| `Monocle`                                   | `layout/monocle.rs` | Every window fills the usable frame; `is_stacking()`                                                    |
| `Floating`                                  | —                   | No positioning                                                                                          |

`layout/floating.rs` provides preset helpers (`calculate_preset_frame`, `find_preset`, `list_preset_names`) used by `actor/handlers/preset.rs`; preset dimensions may be pixels or `"50%"` (`DimensionValue::resolve`), and `center: true` ignores x/y.

## Gaps

`layout/gaps.rs::Gaps { inner_h, inner_v, outer_top, outer_right, outer_bottom, outer_left }` is built by `Gaps::from_config(value, screen_name, is_main, bar_offset)`:

- `GapsConfigValue::Global` applies everywhere; `PerScreen` matches the screen name, then `main`/`primary`/`secondary`, then falls back to the first entry, then to zero.
- `bar_offset` (= `bar.height + bar.padding` when the bar is enabled) is added to `outer_top` on the main screen only.
- `GapValue::PerSide` collapses to `(left+right)/2` / `(top+bottom)/2` for inner gaps.

## Ratios and minimum sizes

- Master ratio per workspace: `workspace.master_ratio` (set by a user resize) overrides `tiling.master.ratio / 100`; balance and layout changes clear the override.
- Split ratios are cumulative boundaries. `has_valid_cumulative_ratios` guards them: after a window is added or removed the stale boundaries are dropped and the layout falls back to an equal split.
- `actor/minimum_size.rs` enforces `Window::effective_minimum_size()` in two phases — detect frames below minimum, then redistribute ratios (`enforce_minimum_sizes_for_split`, dwindle and grid equivalents). It can raise `inferred_minimum_size`, which balance clears.

## Effects

`effects/subscriber.rs::EffectSubscriber` subscribes to the observable state and derives effects:

- keeps `desired_layout_positions` vs `applied_layout_positions` per workspace, plus a monotonic `layout_revisions` counter, because a frame can recur (A → B → A);
- caches focused window/workspace, visible workspaces, layouts and the floating set from notifications instead of querying the actor on every focus change;
- prunes per-window state via `notify_window_destroyed`, so a reused window ID never inherits old flags;
- entry points: `notify_layout_changed`, `notify_focus_changed_with_layout`, `notify_visibility_changed`, `notify_floating_changed`, `notify_workspace_layout_changed`, `notify_window_destroyed`, `shutdown`.

`effects/executor.rs::EffectExecutor` groups `TilingEffect`s (window frames, borders, frontend events), applies frames through `effects/window_ops.rs` (AX, with `window_cache` memoising `AXUIElement`s), and emits `stache://tiling/*`.

`effects/animation/` runs one worker thread: `submit_animation`, `pause_worker`/`resume_worker`, `reset_transient_state`, `should_ignore_geometry_events` (drop the AX echoes our own writes produce), `get_interrupted_position` (retarget mid-flight), spring/easing in `spring.rs`/`easing.rs`. Animation is opt-in via `tiling.animations.enabled`.

## Borders

`borders.rs` drives `JankyBorders` over Mach IPC (service `git.felix.borders`), falling back to the CLI:

- `init()` sends one batched command (width, style `round`/`square`, hidpi, active/inactive colors, blacklist built from the ignore rules + `SKIP_TILING_*` lists) and starts the animation runner;
- `on_focus_changed(layout, is_window_floating)` picks the state via `BordersConfig::focused_state_config` — floating → `floating`, layout-specific override (`monocle`, `dwindle`, `grid`, `split*`, …) if enabled, else `focused`;
- commands are deduplicated by comparing with the last sent string; a send failure clears the cache so the next call retries;
- animation runs at **8 fps** and every base command bumps a monotonic epoch, so a frame that passed its check cannot overwrite a newer focus;
- `pause()` sends `width=0` and transparent colors; `resume()` re-sends and refreshes against fresh state.

## Pitfalls

- Do not implement "apply floating layout" by writing frames; floating windows are excluded (`Window::is_layoutable`) and the layout returns nothing.
- Grid silently leaves window 13+ unpositioned; shorten the workspace or use another layout.
- Border config is resolved at event time, so a state config that is `false`/absent falls back to `focused` — a "missing" border usually means "not enabled for this state".
- Border animation is deliberately low-rate; raising the fps floods JankyBorders' FIFO Mach queue.
- Focus-only changes emit no visibility event, which is why focus notifications carry the workspace layout along.
- `notify_layout_changed(ws, user_triggered)` must be told when the user dragged/resized: the subscriber then trusts the live frames over its own tracked positions.

## Tests

`layout/{dwindle,split,master,grid,monocle,floating,gaps,helpers,mod}.rs`, `actor/minimum_size.rs`, `actor/handlers/{layout,resize,preset,window_move}.rs`, `effects/{subscriber,executor,window_cache,window_ops}.rs`, `effects/animation/*` (easing, spring, transitions, worker), `borders.rs` (dedupe, epoch, state selection).
