---
status: draft
---

# 03 — Hyprland-style border animations

Read when implementing after part 02, separate approval (the OQ-A schema was approved 2026-11-09, [overview](00-overview.md#open-questions)). Paths are under `src/crates/`. Still behind `native-borders`; production unchanged. The config fields themselves land in [part 08](08-migration-removal.md) so no inert field ships while JankyBorders is production; this part works from a plain motion model.

## Scope

`modules/tiling/src/borders/appearance.rs` (motion model, pure timeline evaluation), `macos/src/border_overlay/` (Core Animation descriptions installed on the layer tree; fake layer recording them), `animation/easing.rs` only to share the easing set. Behavioral reference only (decision 8): Hyprland's `border` and `borderangle` animations (<https://wiki.hypr.land/Configuring/Animations/>; `borderangle` supports styles `once` and `loop`). Studied for approach (decision 5): JankyBorders `src/animation.c` (its display-link driven proxy animation, which Stache does not reproduce).

## Configuration (OQ-A, approved 2026-11-09)

Additive to `[borders]` in `tiling.toml` (`config/borders.rs`); recommended defaults from the overview's OQ-A. Keys and values are snake_case like every configuration spelling ([config](../../architecture/config.md#key-spelling)); the new `ping_pong` value was approved as `pingPong`.

| Field                                   | Type and default                                                   | Meaning                                                                                                                          |
| --------------------------------------- | ------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------- |
| `transition`                            | optional `{ duration, easing }`; absent = instant (today)          | Focus color cross-fade (Hyprland `border`). `duration` ms (`u32`, 0 = instant), `easing` `EasingType`, default `ease_out`.       |
| `<state>.animation.style`               | optional `"ping_pong"`, `"once"` or `"loop"`; absent = `ping_pong` | New field on the existing `BorderAnimationConfig` of a gradient state.                                                           |
| `<state>.animation.duration` (existing) | `u32` ms, required                                                 | `ping_pong`: one color-swap leg (legacy). `once`/`loop`: one full 360° turn (Hyprland `borderangle`). Clamped to at least 16 ms. |
| `<state>.animation.easing` (existing)   | `EasingType`, default `linear`                                     | Per leg (`ping_pong`) or per turn (`once`/`loop`).                                                                               |

The legacy ping-pong is **kept**, not remapped: an existing `animation` table with only `duration` and `easing` keeps swapping colors exactly as today. `animation` stays valid only on gradient states (unchanged schema shape).

## Requirements

- **Motion model (plain data).** `BorderAppearance` gains `motion: Option<BorderMotion>` with `PingPong { leg, easing }`, `Rotate { turn, easing, repeat: Once | Loop }`, and the engine-wide `transition: Option<Transition { duration, easing }>`. `duration` below 16 ms is clamped as `MIN_LEG_MS` today.
- **Easing.** Every `EasingType` maps to a `CAMediaTimingFunction` (`linear`, `ease_in`, `ease_out`, `ease_in_out`, `ease_out_expo` as control points); `spring` is linear, as in `apply_easing`. The mapping is total.
- **Angle rotation (`once`, `loop`).** The gradient direction turns from the configured angle `a` to `a + 360°` (counterclockwise under part 02's convention) over `turn`, eased per turn, keeping part 02's exact-direction and extent rules at every instant. It is one render-server animation per border (for example a keyframe track of the gradient's start/end points with at most 1° between keyframes, built for the current overlay size); a size change rebuilds the track and keeps the phase. `loop` repeats forever while the state is the window's active appearance; `once` plays one turn each time the state becomes the window's active appearance (focus gained or lost, first show) and ends exactly at `a`. Recolor without a state change and `Move` never restart it.
- **Which borders animate.** `ping_pong` animates only the focused window's border, as today; a focus change removes it from the old border. `once`/`loop` animate every bordered window whose active state carries them (OQ-A default b). A hidden border carries no animation; it restarts from phase 0 when shown again.
- **Focus cross-fade (`transition`).** When a window's active appearance changes because focus or its active state changed, its colors animate from the currently displayed (presentation) colors to the new ones over `duration` with `easing`; solid is treated as a two-stop gradient of one color, so solid ↔ gradient fades color by color. A change to or from glow cross-fades the opacity of the glow and stroke layers. The angle switches to the new state's angle at the start (a running rotation of the new state continues from there). Width changes are instant. A new change during a fade starts from the current presentation values, never jumps. Configuration reloads, show and hide are instant: hide is never delayed by a fade (part 04's hide-before rule).
- **GPU only.** Every animation is a Core Animation animation added on main inside the batch's transaction; no timer, display link, completion callback or per-frame wakeup in Stache. Model values are set to the final state when an animation is added, so nothing must run when it ends. While idle, or when only finite animations have finished, Stache does no border work; a running `loop` costs only render-server time.
- **Failure.** If an animation cannot be described (for example a zero-size overlay), the border draws statically with a diagnostic; borders keep working.

## Acceptance criteria

- [ ] TB-03-01: A pure evaluator `sample(motion, transition, t)` over a fake clock gives the displayed angle and colors at time `t`; tests cover `once` ending at `a`, `loop` periodicity across 3 turns, every easing, the 16 ms clamp, and `ping_pong` frames identical to today's `PingPong` at the same instants.
- [ ] TB-03-02: The fake layer records one animation description per running motion; `ping_pong` only on the focused border; `once`/`loop` on every border whose state carries them; removal on hide, focus loss (`ping_pong`), state change and release; a resize rebuilds the rotation track with unchanged phase.
- [ ] TB-03-03: Cross-fades start from the recorded presentation values, cover solid ↔ gradient ↔ glow, restart cleanly when interrupted, never delay a hide, and are absent when `transition` is absent or 0 ms.
- [ ] TB-03-04: With fakes, Stache schedules no timer, display link or completion callback for any animation, and an idle engine with finished finite animations does no work.

## Tests

Pure evaluator and easing-mapping tests in `borders/appearance.rs` on a fake clock; fake layer tests in `macos/src/border_overlay/tests.rs` for install, remove, resize-rebuild and interruption. Default tests create no real layer or animation.

## Native gate (per-run approval)

On owned test windows on a 2× and a 1× display: `loop` at 4 s per turn on 3 bordered windows, `once` on focus change, a 200 ms cross-fade between solid, gradient and glow states, and the legacy ping-pong. Record Stache process CPU and wakeups for 60 s with each running against borders static (pass: within noise of static), WindowServer CPU with `loop` (recorded, judged by the user), and visual smoothness (user judgment).

## Review checklist

- [ ] [Hard rule checklist](00-overview.md#hard-rule-review-checklist-every-part) passed: no JankyBorders or Hyprland code used; behavior reimplemented from the documented configuration.

## Notes

- Rollback: drop the motion fields; part 02 draws statically.
- Unverified: render-server cost of several looping gradients, keyframe smoothness on ProMotion displays.
- Docs: none lasting until part 08 (config reference and feature doc).
