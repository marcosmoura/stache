---
status: done
---

# 06 — Configurable HUD position and show/hide animation

Read when implementing HUD placement and animation options. Requested by the user on 2026-11-07 after the Part 05 manual run: make the on-screen position configurable (top, right, left, bottom, center), animate showing and hiding, and make the animation configurable (fade or slide; a slide also fades and enters from the chosen position; for center it slides down by 100% of the HUD height). Parts 01–05 are done on `feat/hold-to-quit`.

## Compatibility change

Part 02 decided "window animation behavior is `None`, so there is never a decorative fade" and "omit decorative fades" for Reduce Motion. This part replaces it: the HUD always animates its show and hide with a short fade, plus a slide when configured. Reduce Motion turns a slide into a plain fade (cross-fades are the system's reduced-motion substitute); progress is unchanged. The AppKit window animation behavior stays `None`; the animation is ours, not AppKit's.

## Scope

- Config: `src/crates/modules/hold-to-quit/src/config.rs`, regenerated `schemas/commandQuit.schema.json` and generated TypeScript bindings (`bun run generate-bindings`), migration tests (legacy has neither key: a migrated file decodes to the defaults).
- HUD adapter: `src/crates/macos/src/hold_hud.rs` (`HudRequest`, `HudShowCommand`, `compute_layout`, `HudStyle`/`style_for`, `NativePanel`, `SystemHudBackend::{show, hide, freeze, release}`), `hold_hud/testing.rs`, `hold_hud/tests.rs`.
- Runtime: `src/crates/modules/hold-to-quit/src/runtime.rs` passes the armed values on each show; `tests.rs`.
- Docs: `docs/features/hold-to-quit.md`, `docs/architecture/config.md`, `docs/architecture/overview.md` if it lists keys, the Part 02 decision note in `00-overview.md` (record the compatibility change), this spec.

## Requirements

1. **Config keys.** `CommandQuitConfig` gains:
   - `position: HudPosition` (`"top" | "right" | "left" | "bottom" | "center"`, default `"bottom"`).
   - `animation: HudAnimation` (`"fade" | "slide"`, default `"fade"`).
     Both are lowercase string enums in JSON, exported to TypeScript (unlike the `u64` keys), with doc comments that become schema descriptions. Unknown strings are type errors naming the key. `validate` stays `Ok`. The config types live in the module crate; the HUD crate gets its own plain enums (`HudPlacement`-style naming must not collide with the existing `HudPlacement` struct, e.g. `HudAnchor` and `HudMotion`), and the runtime maps one to the other.
2. **Placement.** `compute_layout(visible, app, anchor)` places the panel inside the chosen visible frame with the same margins and size limits as today:
   - `bottom`: horizontally centered, `BOTTOM_INSET` above the lower edge (today's placement, unchanged numbers).
   - `top`: horizontally centered, the same inset below the upper edge.
   - `left` / `right`: vertically centered, the same inset from that edge.
   - `center`: centered both ways.
     The panel never leaves the visible frame for any anchor, including tiny frames. Display selection (`display_for` hint, fallbacks) is unchanged.
3. **Request plumbing.** `HudRequest` gains `with_anchor` and `with_motion` (defaults `Bottom` and `Fade`); `HudShowCommand` carries them. The runtime sets both from the armed configuration on every show, so a reload applies to the next press.
4. **Show animation.** After the gate passes, the panel is ordered front at alpha 0 and animated to alpha 1 over `SHOW_DURATION` (200 ms, ease-out), using `NSAnimationContext` on the window's `animator()` (alpha and frame), on the main thread. `fade`: alpha only, at the final frame. `slide`: alpha plus frame from a start offset to the final frame:
   - `bottom`: starts below by panel height + inset (enters from the lower edge, moving up).
   - `top`: starts above by panel height + inset (moving down).
   - `left` / `right`: starts outside that edge by panel width + inset (moving inward).
   - `center`: starts one panel height above the final frame (moving down by 100% of the HUD height).
     With Reduce Motion, `slide` behaves as `fade`. The start/end frames are computed by a pure function (`motion_frames(layout, anchor, motion, reduce_motion) -> (start, end)`) and unit-tested. `Shown` is still acknowledged when the panel is ordered front (the start of the animation), so the hold deadline and progress are unchanged. The progress animation starts with the show, as today.
5. **Hide animation.** `hide` animates to alpha 0 over `HIDE_DURATION` (150 ms, ease-in) — `fade` in place, `slide` back toward the start frame of requirement 4 — then orders out in the completion handler, but only if the panel is still hiding the same press (a hide token/generation; a show in between wins). The native `displayed` press is cleared when the hide starts, so a later `freeze`/`hide` for that press is a no-op. The Part 05 linger is unchanged: freeze keeps the panel visible and still; the animated hide happens when the linger ends.
6. **Interruption.** A show for a new press while a hide animation is running cancels it: the panel jumps to the new show's start state (no visible flicker beyond the new show) and the stale completion never orders the new press out. A hide while the show animation is running animates out from the current state. A show for a new press while the panel is still visible (for example during a Part 05 linger) does not blink: it keeps the current alpha and frame and animates to the new final frame. `release` (teardown) orders out immediately with no animation and drops everything, as today; a pending completion after release does nothing.
7. **Style.** `HudStyle.fade` is removed or replaced by the effective motion; `HudAppearance.reduce_motion` docs updated. Increase Contrast / Reduce Transparency behavior unchanged.
8. **Fakes.** `FakeHudBackend` records the anchor, motion and effective motion per show, and models an animated hide as an immediate order-out (tests stay deterministic); a hook may expose "hide in flight" for the interruption test if needed.

## Acceptance criteria

- [x] HQ-06-01 — Config: defaults `position: "bottom"`, `animation: "fade"`; `{}` keeps them; every listed value is accepted; an unknown string or wrong type is a type error naming the key; schema and bindings regenerated and `bun run check:generated` passes; a migrated legacy file decodes to the defaults.
- [x] HQ-06-02 — `compute_layout` places the panel per anchor (bottom matches today's frame exactly for the same input) and keeps it inside the visible frame for normal, tiny and offset (secondary display) frames, for every anchor.
- [x] HQ-06-03 — `motion_frames`: fade has start == end frame; slide offsets per anchor exactly as requirement 4 (center = one panel height above, moving down); Reduce Motion turns slide into fade.
- [x] HQ-06-04 — Runtime: a show carries the armed anchor and motion; after a reload changing them, the next press carries the new values (fake backend records).
- [x] HQ-06-05 — Hide/show interruption is safe in the fake model: a hide followed by a successor show never leaves the successor hidden; a stale hide completion is a no-op (pure hide-token logic unit-tested if the native completion cannot be faked).
- [x] HQ-06-06 — Existing hq_02/hq_04/hq_05 tests still pass unchanged in intent (update only fixtures that now need anchor/motion).
- [x] HQ-06-07 — `bun run lint`, `bun run format:check`, `bun run check:generated`, nextest for the touched crates and rustdoc pass; feature and config docs describe both keys, the Reduce Motion fallback and the compatibility change.

## Manual check (per-run approval, disposable app)

For each `position` with `animation: "slide"`, hold ⌘Q briefly in TextEdit: the HUD slides in from that edge (center: slides down) while fading, freezes on release for `dismissDelay`, then slides back out while fading. Repeat once with `"fade"`, once with Reduce Motion on (slide becomes fade), and once pressing ⌘Q again during the hide animation (the HUD comes back cleanly).
