---
status: done
---

# 09 — Focus width grow/shrink when unfocused borders are off

Read when changing how a border appears or disappears on a focus change with `unfocused = false`. Approved by the user on 2026-11-11 after native run 11. Paths are under `src/crates/`.

## Problem

With `unfocused = false` only the focused window has a border. A focus change released the old window's overlay and showed the new one at once, even with a `transition` configured: the cross-fade of [part 03](03-animations.md) only runs between two drawn states, so the border jumped from one window to the other.

## Requirements

- With `unfocused = false`, a window gaining focus grows its border from width 0 to its configured (visible) width; a window losing focus shrinks it from its width back to 0, then the overlay hides.
- Timing is the configured `[borders] transition` duration and easing (OQ-A, [part 08](08-migration-removal.md)). Absent `transition` or 0 ms: instant, as before (show and hide at once).
- GPU only, as in part 03: Core Animation animations added inside the batch transaction, model values set to the final state, no Stache timer, display link or completion callback.
- The shrink's hide needs no callback: the panel stays ordered in, drawing an empty ring once the animation ends, until the engine releases it at the transition's end through an existing engine deadline (`next_deadline`/`tick`). An application hide, a workspace switch, a Mission Control or Space gate `HideAll`, a window leaving the model or an overlay release still hides at once, never after the shrink.
- Interruption (focus flips back mid-animation) starts from the presented width, computed from the running track's timing; the duration scales with the remaining distance.
- A width change on a configuration reload stays instant; a reload during a shrink hides the shrinking border at once.
- Placement below/above and the visible-fraction geometry apply at every width. Solid, gradient and glow are supported; a glow's halo bitmap fades with the width (opacity 1 ↔ 0) instead of being re-rendered per width.
- When `unfocused` is a drawn state (not `false`), today's cross-fade applies and the width does not animate.

## Design

- Model (`modules/tiling/src/borders/model.rs`): when `collapses_on_blur(config)` (`unfocused = false` and a non-instant `transition`), an unfocused window that would be bordered if focused stays in the model as a `collapsed` entry with its focused look. Collapsed entries never pass the show rule.
- Engine (`borders/engine.rs`): a shown entry turning collapsed sends `Show { change: Shrink }` with the look last drawn and sets `collapse_until = now + transition`; `next_deadline` includes it and `tick`/`update` then send `Release` (an unfocused window holds no overlay). A collapsed entry turning focused while shown sends `Show { change: Grow }`; while hidden, its next show-rule `Show` is `Grow`. Every existing hide path is unchanged and immediate.
- Overlay (`macos/src/border_overlay/`): `AppearanceChange::{Grow, Shrink}`. `ring_at_fraction` (geometry) re-plans the ring at a scaled width, aligned to the full plan; `plan_width` (animation) animates the ring layer's and gradient mask's `path` (and the glow's opacity). A shrink sets the model path to the zero ring (`OverlayHost::set_ring_override`) and keeps the panel ordered in; any hide, `HideAll` or release removes the tracks and orders out at once. A resize mid-change snaps to its end.

## Acceptance criteria

- [x] TB-09-01: Pure and plan tests: the ring at fraction 1 equals the drawn ring, at 0 is empty on the target's edge, and the placement and visible fraction hold at intermediate widths for below and above; `plan_width` describes path tracks (and the glow fade) with the transition's timing; the model keeps collapsed entries only with `unfocused = false` and a non-instant transition.
- [x] TB-09-02: Fake-host and engine tests: a grow starts at 0 and a shrink ends at 0 with the panel ordered in; an interruption starts from the presented width; a `Hide`, `HideAll`, release, application hide, Mission Control suspension or a window leaving the model during a shrink orders out at once; no transition (absent or 0 ms) is instant; a reload is instant; and an end-to-end runtime scenario (focus change between two windows, then an application hide mid-shrink).
- [x] TB-09-03: With fakes, no timer, display link or completion callback is scheduled; nothing runs when a shrink ends until the engine's deadline releases the overlay.

## Tests

`macos/src/border_overlay/width_tests.rs` (TB-09-01 geometry and plan, TB-09-02 and TB-09-03 on the fake host), `modules/tiling/src/borders/engine_tests.rs` (part 09 section), `modules/tiling/src/runtime_tests/borders.rs` (`a_focus_change_shrinks_and_grows_and_an_app_hide_mid_shrink_hides_at_once`, `focus_back_mid_shrink_with_the_reorder_leaves_the_border_visible`). Default tests create no real window, layer or animation.

## Review checklist

- [x] [Hard rule checklist](00-overview.md#hard-rule-review-checklist-every-part) passed: no JankyBorders or Hyprland code used; behavior reimplemented from the approved description.

## Notes

- Native-unverified: `path` interpolation between the two rounded-rect rings (both keep the same corner radii, so the paths have the same elements; a `below` ring whose inner radius clamps to 0 at full width may not interpolate smoothly), the CGPath bridging as an animation value, and the glow halo fade.
- While a shrunk panel waits for its release it is ordered in but draws nothing; a Space switch or Mission Control in that interval hides it through the normal gate.
- Colors do not cross-fade on a grow; the grow shows the new look's colors from the start.
- Native run 12 (2026-11-11, approved, build `f6a21b0`): the user confirmed grow and shrink are smooth (`transition = { duration = 250 }`, `unfocused = false`) and that an application hide or workspace switch mid-shrink hides the border at once.
- Implemented. Tests: `macos/src/border_overlay/width_tests.rs` (TB-09-01 ring and plan; TB-09-02/03 on the fake host, including the review fixes `a_grow_during_a_shrink_survives_the_reorder_and_a_later_resize`, `a_grow_keeps_its_track_across_an_order_with_the_same_plan` and `a_shrink_for_an_unknown_handle_creates_no_panel`), `modules/tiling/src/borders/engine_tests.rs` part 09 section (including `a_reload_that_draws_a_shrinking_border_again_cancels_its_release` and `collapsed_entries_are_not_pending`), and `modules/tiling/src/runtime_tests/borders.rs` (`a_focus_change_shrinks_and_grows_and_an_app_hide_mid_shrink_hides_at_once`, `focus_back_mid_shrink_with_the_reorder_leaves_the_border_visible`).
- Review fix (interruption): any non-shrink show clears the overlay's collapsed state, and an `Order` or `Move` whose re-plan equals the previous plan keeps a running width track (only a real geometry change snaps it). A reload that makes a shrinking entry drawn again clears its release deadline and reloads the full ring.
- A reload that makes a collapsed, hidden window bordered again (for example `unfocused` changed from `false` to a drawn state) shows it at full width through the show rule, without a grow: reloads are instant.
