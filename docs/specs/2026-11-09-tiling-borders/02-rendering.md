---
status: done
---

# 02 — Static rendering styles

Read when implementing after part 01 and separate approval. Paths are under `src/crates/`. Still behind `native-borders`; production unchanged. Animations are [part 03](03-animations.md).

## Scope

`macos/src/border_overlay/` (layer tree and `BorderAppearance`), and a new pure mapping in `modules/tiling/src/borders/appearance.rs` from `BorderStateConfig` to `BorderAppearance`, reusing `parse_color`, `is_enabled` and `focused_state` from `borders/policy.rs` (moved, not duplicated, so part 08 can delete the JankyBorders argv code alone). Studied for approach (decision 5): JankyBorders `src/border.c` (`border_draw`: outset frame, inner clip path), `src/misc/drawing.h` (stroke, gradient direction, glow). Stache draws with Core Animation layers rather than a Core Graphics bitmap.

## Requirements

- **Appearance (plain data).** `BorderAppearance { width, style, hidpi, paint, corner_radius }` with `paint` one of `Solid(Rgba)`, `Gradient { from, to, angle }`, `Glow(Rgba)`; part 03 adds the motion fields. A disabled state maps to "no overlay" (the overlay is hidden, not drawn transparent). An unparsable color keeps today's behavior: transparent paint with its width (`policy.rs` `state_settings`), so nothing visible is drawn.
- **Colors.** Hex and `rgb()`/`rgba()` parse exactly as `parse_color` today (sRGB, channel values identical to the current `0xAARRGGBB` rounding).
- **Width (resolved OQ-1).** Every border uses the active (focused) state's width, as `focus_command` sends today; `unfocused.width` and override widths of non-active states are not used. Width 0 hides the overlay.
- **Style.** `round` strokes a rounded rectangle whose inner edge follows the corner radius from part 01; `square` strokes a plain rectangle. The stroke lies entirely outside the target frame.
- **Solid.** A `CAShapeLayer` stroke (no fill) of the path.
- **Gradient.** A `CAGradientLayer` (axial, two stops) masked by the stroke shape, drawn in the exact configured direction (below).
- **Glow.** A stroke of the glow color plus a layer shadow of the same color (offset 0, radius derived from width, opacity 1), clipped to stay outside the target frame. Exact visual parity with JankyBorders glow is not required; the user judges it in the native check.
- **Updates.** Changing appearance updates layer properties in place (no overlay recreation); recoloring on focus change is a property change only. Without a configured transition (part 03) every change is instant: implicit actions stay disabled.

### Gradient direction

Resolved OQ-2: any direction, drawn exactly. The field keeps its documented meaning (`config/borders.rs`, `GradientConfig::angle`: "0 is left to right, 90 bottom to top"); no field is added because one angle already expresses every direction.

- **Convention.** Normalize `angle` to `[0, 360)`. The gradient runs along the screen direction `(cos a, sin a)` with x to the right and y **up**: 0° left → right, 90° bottom → top, 180° right → left, 270° top → bottom, 45° bottom-left → top-right, 135° bottom-right → top-left. Angles grow counterclockwise. `from` is drawn at the start, `to` at the end.
- **Extent.** Measured in points on the overlay's outer rectangle (not the unit square, so a wide window keeps the true angle): `from` is exact at the corner that lies furthest against the direction, `to` at the opposite corner, linear in between, and isolines are perpendicular to the direction (the CSS `linear-gradient` corner rule). A pure function `gradient_points(size, angle) -> (start, end)` returns the `CAGradientLayer` `startPoint`/`endPoint` in its unit space (accounting for the layer's y-up geometry); it is recomputed whenever the overlay size changes.
- **Visual change from today (document in part 08).** JankyBorders drew two buckets: normalized `[0, 90)` and `[180, 270)` as `from` top-right → `to` bottom-left, the rest as `from` top-left → `to` bottom-right. Exact drawing matches that only at 225° and 315°. Axis angles (0, 90, 180, 270) now draw horizontal or vertical gradients; 45° and 135° are the reverse of today. The default angle 90 changes from a top-left → bottom-right diagonal to a bottom → top gradient. Users wanting the old look set 225 or 315. Existing configurations keep parsing unchanged.

## Acceptance criteria

- [x] TB-02-01: Pure mapping tests cover every `BorderStateConfig` variant, disabled and unparsable colors, `rgb()`/`rgba()`, width 0 and both styles, with color, precedence and width results identical to the current `policy.rs` tests; every border's width is the active state's width.
- [x] TB-02-02: `gradient_points` is tested for 0, 45, 90, 135, 180, 225, 270, 315, 359.99, -90, 360 and 720 on a square, a 3:1 and a 1:3 size: the direction in points equals the angle within 0.01°, `from` lands on the documented corner or edge, and 225/315 match today's bucket corners.
- [x] TB-02-03: Recolor, angle and width changes mutate the fake layer tree in place without creating a new overlay or an implicit animation; disabled states and width 0 hide the overlay.

## Tests

Pure tests in `modules/tiling/src/borders/appearance.rs` (port the color, precedence and width cases from `borders/tests.rs`; the angle bucket cases are replaced by the exact-direction table above), fake layer tree tests in `macos/src/border_overlay/tests.rs`. Default tests create no real layer.

## Native gate (per-run approval)

Combined with part 01's gate when approved together: render each style (solid; gradient at 0, 45, 90, 180, 270 and 315 on a wide and a tall window; glow; round; square; hidpi on/off) on an owned test window on a 2× and a 1× display. Pass: the user confirms each gradient runs in the documented direction.

## Review checklist

- [x] [Hard rule checklist](00-overview.md#hard-rule-review-checklist-every-part) passed: drawing approach inspired by, not derived from, `border.c`/`drawing.h`; no copied, translated or closely paraphrased code or constants.

## Notes

- Rollback: revert to part 01's solid stroke.
- Unverified: glow look, color space match with JankyBorders, `CAGradientLayer` unit-space orientation on macOS (covered by the gate).
- Implemented (2026-11-10): `gradient_points` assumes an unflipped layer (unit y up) and that `CAGradientLayer` interpolates in unit space, so the unit vector is the point-space direction scaled by the layer size; points may fall outside `[0, 1]`. Glow is the ring plus a same-color shadow (radius = width) inside a container masked to exclude the target frame; the panel gains a glow margin. An unparsable color maps to a transparent solid with the state's width. `BorderAppearance::corner_radius` is an override (`None` keeps part 01's resolution); Tiling passes `None`.
- Native gate: not run (needs per-run approval; combined with parts 01 and 03).
- Native run 1 (2026-11-10, build `f8754c5`, one 2× display): gradient directions 0, 45, 90 and 270 confirmed by the user on wide and grid windows (180 and 315 not separately checked). Glow was too subtle: the blur radius is now three times the width (`GLOW_RADIUS_FACTOR`, unverified). Square style showed gaps at the windows' round corners with above placement; below placement is expected to fill them (re-check). Borders are absent from a ⌘⇧3 screenshot (`sharingType = none` works).
- Docs: none lasting until part 08; appearance rules are recorded in this spec.
