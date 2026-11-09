---
status: draft
---

# 02 — Rendering styles

Read when implementing after part 01 and separate approval. Paths are under `src/crates/`. Still behind `native-borders`; production unchanged.

## Scope

`macos/src/border_overlay/` (layer tree and `BorderAppearance`), and a new pure mapping in `modules/tiling/src/borders/appearance.rs` from `BorderStateConfig` to `BorderAppearance`, reusing `parse_color`, `is_enabled` and `focused_state` from `borders/policy.rs` (moved, not duplicated, so part 06 can delete the JankyBorders argv code alone). Studied for approach (decision 5): JankyBorders `src/border.c` (`border_draw`: outset frame, inner clip path), `src/misc/drawing.h` (stroke, gradient direction, glow). Stache draws with Core Animation layers rather than a Core Graphics bitmap.

## Requirements

- **Appearance (plain data).** `BorderAppearance { width, style, hidpi, paint, corner_radius }` with `paint` one of `Solid(Rgba)`, `Gradient { from, to, direction, animation }`, `Glow(Rgba)`. A disabled state maps to "no overlay" (the overlay is hidden, not drawn transparent). An unparsable color keeps today's behavior: transparent paint with its width (`policy.rs` `state_settings`), so nothing visible is drawn.
- **Colors.** Hex and `rgb()`/`rgba()` parse exactly as `parse_color` today (sRGB, channel values identical to the current `0xAARRGGBB` rounding).
- **Width.** Per OQ-1 default, every border uses the active state's width (as `focus_command` sends today). Width 0 hides the overlay.
- **Style.** `round` strokes a rounded rectangle whose inner edge follows the corner radius from part 01; `square` strokes a plain rectangle. The stroke lies entirely outside the target frame.
- **Solid.** A `CAShapeLayer` stroke (no fill) of the path.
- **Gradient.** A `CAGradientLayer` masked by the stroke shape. Direction per OQ-2 default is identical to `gradient_to_janky`: normalize the angle to `[0, 360)`; `[0, 90)` and `[180, 270)` run top-right → bottom-left, the others top-left → bottom-right.
- **Gradient animation.** When the active state is a gradient with `animation`, the colors ping-pong as today (`PingPong`: leg `duration`, minimum 16 ms, eased per leg, direction flips). It runs as one Core Animation animation on the render server (`colors` animated with autoreverse and infinite repeat), with the easing mapped to a `CAMediaTimingFunction` (linear, ease-in, ease-out, ease-in-out, ease-out-expo as control points; `spring` is linear as in `apply_easing`). No per-frame process wakeup, IPC or timer; the 8 fps cap of part 09 (`ANIMATION_FPS`) does not apply. Only the focused window's border animates (today's behavior); a focus change removes the animation from the old border.
- **Glow.** A stroke of the glow color plus a layer shadow of the same color (offset 0, radius derived from width, opacity 1), clipped to stay outside the target frame. Exact visual parity with JankyBorders glow is not required; the user judges it in the native check.
- **Updates.** Changing appearance updates layer properties in place (no overlay recreation); recoloring on focus change is a property change only.

## Acceptance criteria

- [ ] TB-02-01: Pure mapping tests cover every `BorderStateConfig` variant, disabled and unparsable colors, `rgb()`/`rgba()`, width 0, both styles and every angle boundary (0, 89.99, 90, 180, 270, -90, 360, 720) with results identical to the current `policy.rs` tests.
- [ ] TB-02-02: Easing to timing-function mapping is total and tested; the gradient animation is a single CA animation description (fake layer records it), installed only on the focused border and removed on focus loss or reconfigure.
- [ ] TB-02-03: Recolor and width changes mutate the fake layer tree in place without creating a new overlay; disabled states hide the overlay.

## Tests

Pure tests in `modules/tiling/src/borders/appearance.rs` (port the color, precedence and angle cases from `borders/tests.rs`), fake layer tree tests in `macos/src/border_overlay/tests.rs`. Default tests create no real layer.

## Native gate (per-run approval)

Combined with part 01's gate when approved together: render each style (solid, gradient both directions, glow, round, square, hidpi on/off) on an owned test window on a 2× and a 1× display; record CPU and wakeups with an animated gradient running for 60 s against borders off.

## Review checklist

- [ ] [Hard rule checklist](00-overview.md#hard-rule-review-checklist-every-part) passed: drawing approach inspired by, not derived from, `border.c`/`drawing.h`; no copied, translated or closely paraphrased code or constants.

## Notes

- Rollback: revert to part 01's solid stroke.
- Unverified: glow look, CA animation cost on the render server, color space match with JankyBorders.
- Docs: none lasting until part 06; appearance rules are recorded in this spec.
