---
branch: feat/tiling-borders
---

# Tiling native borders

Read when planning or implementing the Tiling-owned border engine that replaces the JankyBorders integration. **DRAFT — NOT SHIPPED.** Every part needs separate approval, and every native gate needs its own per-run approval under [validation](../2026-11-03-tiling/validation.md#per-run-native-approval). This document authorizes no native run.

## Goal

Draw `tiling.borders` with Stache's own overlay windows instead of the external JankyBorders service. Tiling knows exactly when it hides, shows, moves, resizes and focuses each managed window, so it can hide a border before its application is hidden and show it only after the window is visible at its frame. That removes the border blink seen on some applications (Ghostty yes, Spotify no) when they are unhidden or activated. The blink also happens with Tiling paused on a plain ⌘H and Cmd-Tab, and Stache sends no border command then, so it comes from JankyBorders itself; its cause is not verified.

## Problem (current code)

Cited from `main` at `ff84e3b` (2026-11-10; `fix/tiling-fixes` landed as `ba51f20`, the tiling-animation spec as `29348fc`). Paths are under `src/crates/`.

- Part 09 drives an external service: `macos/src/borders/` (Mach `mach.rs`, CLI `cli.rs`, `transport.rs`) and the runner `modules/tiling/src/borders/runtime.rs` (thread `stache-tiling-borders`, 8 fps cap, epochs). The host composes it in `app/src/host.rs:753–766`.
- Policy is pure and reusable: `modules/tiling/src/borders/policy.rs` (`parse_color`, `state_settings`, `focused_state`, `blacklist`, `AnimationPlan`/`PingPong`, gradient angle buckets `gradient_to_janky`). Config: `config/borders.rs` (`BordersConfig`, `BorderStateConfig`, `BorderStyle`, `BorderAnimationConfig`) and `config/color.rs`.
- The actor feeds borders only the focused window's layout and floating flag (`publish_if_changed`, `actor/mod.rs:1044–1063`; `border_focus` at `actor/mod.rs:1079`). JankyBorders then tracks every window itself through private SkyLight notifications, independently of Tiling's hide/unhide (`visibility.rs`, effects `HideApp`/`UnhideApp` at `actor/mod.rs:314–321`) and frame writes (direct `FrameControl::submit_direct`, `actor/frames.rs:225`; animated per-tick batches `Animator::send` → `AnimationSink::submit_batch`, `animation/worker.rs:653–700`, `:165`; both through the gated `FrameControl::request`, `actor/frames.rs:171`).
- Today's effective rendering: one width for every border (the active state's width, `focus_command`), the per-layout and floating override colors only the focused window, unfocused windows use `unfocused`, gradients use two diagonal directions only, and gradient animation sends up to 8 color frames per second over Mach.

## Non-goals

- No borders on windows Tiling does not manage (ignored applications, system windows, Stache's own windows).
- No breaking configuration change: every existing `tiling.borders` configuration keeps parsing, validating and resolving with the same precedence (decision 3). The only schema changes are the additive animation fields of [part 03](03-animations.md) (OQ-A, approved 2026-11-09) and the documented meaning of `enabled`.
- No GPL code in Stache (decision 5). No Screen Recording permission, no SIP-requiring API, no scripting addition.
- No managing, launching, stopping or reconfiguring a running JankyBorders after part 08.
- No change to layouts, CLI or socket contracts beyond additive status diagnostics. No legacy port; `_legacy/` is read-only reference.

## Decisions

Approved by the user on 2026-11-09. They supersede the [Tiling part 09](../2026-11-03-tiling/09-jankyborders.md) note "not silently replaced with a custom border engine": this is the explicit, approved replacement.

1. **Native borders replace JankyBorders.** The part 09 Mach/CLI transport and runner are removed only in part 08, after native borders pass their approved native check. Until then production keeps the JankyBorders path and the native engine is built only behind a compile-time feature; there is never a build without a working border path.
2. **Managed windows only.** A border is drawn on a tracked tiled or floating window that is on a visible workspace and is not ignored, a system window, an inactive tab or suspended slot member, minimized, fullscreen or hidden.
3. **Configuration kept, extended additively.** `enabled`, `focused`/`unfocused`, the per-layout and `floating` overrides with legacy `focused_state_config` precedence, solid/gradient(angle)/glow colors including `rgb()`/`rgba()`, `width`, `style` round|square, `hidpi`, gradient animation `duration`/`easing` and the ignore/blacklist semantics keep their meaning. `gradient.angle` keeps its documented meaning ("0 is left to right, 90 bottom to top") and is now drawn exactly (decision 7). `enabled: true` no longer means consent to overwrite an external service; the schema description and docs change accordingly. Hyprland-style animation fields are added without changing any existing field ([part 03](03-animations.md), OQ-A).
4. **Live follow.** Borders follow user drags and resizes at display refresh. Minimal private SkyLight APIs are allowed behind compile gates and approved native checks with a public fallback, under the same policy as `_AXUIElementGetWindow` (`macos/src/ax/window_id.rs:1–19`) and the [animation spec](../2026-11-09-tiling-animation/00-overview.md#decisions) decision 2. Without live tracking a border hides while its window moves and shows at the final frame.
5. **Inspired by, not derived from.** Stache is MIT; JankyBorders is GPL-3.0. Its source **may be studied** to learn its approach (which macOS and SkyLight APIs it uses, how it tracks, orders, renders and hides border windows, how it handles events), and each part cites the files it learned from. **Hard rule:** no JankyBorders code is copied, translated line by line or closely paraphrased into Stache (names, structure, constants or comments included). Stache writes its own implementation from Apple documentation, the public behavior and the ideas. Every part's review checks this.
6. **Degrade borders only.** Any unavailable capability (overlay creation, ordering, live tracking, corner radius, rendering, Space or Mission Control detection) degrades borders only, with a status diagnostic. Layout, commands, focus and other modules are never affected.
7. **Appearance answers (2026-11-09).** One width for every border, taken from the active (focused) state, as today (OQ-1). Gradients run in exactly the configured direction at any angle, not two diagonal buckets (OQ-2; convention and visual changes in [part 02](02-rendering.md#gradient-direction)). Per-layout and `floating` overrides color only the focused window, as today (OQ-3). `round` uses the private `SLSWindowIteratorGetCornerRadii` with a measured per-OS fallback; `square` is 0 (OQ-4). Ignore rules match the application's bundle id and name, not JankyBorders' process name (OQ-6).
8. **Animations are Hyprland-style and GPU-only.** Gradient angle rotation (`once` or `loop`) and a focus color cross-fade, modeled on Hyprland's `borderangle` and `border` animations (behavioral reference only: <https://wiki.hypr.land/Configuring/Animations/>), run as Core Animation animations on the render server: no per-frame IPC, timer or process wakeup, zero work while idle; a running loop costs only compositor time. The legacy ping-pong keeps working ([part 03](03-animations.md)).
9. **Never visible in Mission Control or on another Space (OQ-5).** No border may be visible in Mission Control, App Exposé, Show Desktop, a Space switch animation, a fullscreen transition or on an inactive Space. Any visible border there is a defect, not a limit; when the state cannot be determined, every border hides ([part 05](05-spaces-mission-control.md)).
10. **Focus width animation (2026-11-11).** With `unfocused = false` and a `transition`, a focus change grows the new border from width 0 and shrinks the old one to 0 before it hides, GPU only and never delaying a hide; approved by the user 2026-11-11 after native run 11 ([part 09](09-focus-width-animation.md)).

## Hard rule review checklist (every part)

- [ ] No JankyBorders source was copied, translated line by line or closely paraphrased; ideas are reimplemented from Apple documentation and observed behavior; the part cites the JankyBorders files it studied.
- [ ] No border on an unmanaged, hidden, minimized, fullscreen, suspended or inactive-tab window, on an inactive Space or in Mission Control.
- [ ] No main-thread wait, no AX call on main, every queue bounded, no lock held across a native call or a main-thread dispatch, no detached thread.
- [ ] Every degradation has one status diagnostic; core Tiling behavior is unchanged when borders fail.
- [ ] Default tests create no real window, overlay, display link or SkyLight connection.

## Sources

Studied by the spec writer on 2026-11-09 for approach only (decision 5). JankyBorders at commit `a7297ca7` (<https://github.com/FelixKratz/JankyBorders>, [man page](https://github.com/FelixKratz/JankyBorders/wiki/Man-Page)):

- `src/events.c`: registers private `SLSRegisterNotifyProc` handlers for window move, resize, reorder, level, hide, unhide, close, create, destroy, Space and front-application changes; focus is re-evaluated after short delays (10–50 ms).
- `src/windows.c`: `SLSRequestNotificationsForWindows` subscribes per window; `SLSWindowIteratorGetCornerRadii` (resolved by `dlsym`, macOS 26 only, `src/main.c`) gives the target's corner radius, else a fixed fallback; per-application allow/deny by process name.
- `src/border.c`: one private SkyLight window per target; the border is drawn **outside** the target frame (outset by width plus padding) with a clipped inner rounded path; moves and orders the border in one `SLSTransaction` relative to the target id, copying its level and sub-level; hide orders out; unhide orders back in immediately when the target is unhidden. Our reading: the unhide path re-shows the border as soon as the window server reports the target unhidden, possibly before the application has drawn, which is consistent with the observed blink (unverified).
- `src/misc/window.h`, `src/misc/drawing.h`, `src/animation.c`: window creation tags/shadow, `hidpi` as window resolution 1× or 2×, Core Graphics stroke, gradient and glow drawing, `CVDisplayLink` pacing for its proxy animation.
- `src/events.c` registers only window, Space-change and front-application notifications; no Mission Control or Exposé detection was found. `src/border.c`/`src/misc/window.h` place each border on its target's Space (`SLSMoveWindowsToManagedSpace`), set private window tags and skip drawing when the Space is not visible; Mission Control behavior is left to the window server (our reading; unverified).
- yabai `src/mission_control.c` (<https://github.com/koekeishiya/yabai>, studied for approach only): observes the Dock process with Accessibility notifications for Mission Control, App Exposé, Show Desktop and their exit.
- Hyprland animation config (<https://wiki.hypr.land/Configuring/Animations/>): `border` (color change) and `borderangle` (gradient rotation, styles `once` and `loop`) are the behavioral reference for part 03; no Hyprland code is used.
- yabai issue [#2835](https://github.com/koekeishiya/yabai/issues/2835): leaked proxy/overlay windows remaining on screen; a known hazard part 07 guards against.
- In tree: the Hold-to-Quit HUD (`macos/src/hold_hud.rs`) is the existing pattern for main-thread-owned `NSPanel` + `CALayer` overlays keyed by opaque handles and dispatched through `MainThreadExecutor` (`sdk/src/threading.rs:321`).

## Parts

| Part                               | Summary                                                                                | Status |
| ---------------------------------- | -------------------------------------------------------------------------------------- | ------ |
| [01](01-overlay-engine.md)         | Main-thread overlay window pool, ordering, collection behavior, solid stroke           | done   |
| [02](02-rendering.md)              | Static Core Animation styles: solid, exact-angle gradient, glow, width, style, hidpi   | done   |
| [03](03-animations.md)             | Hyprland-style GPU animations: angle rotation once/loop, focus cross-fade, ping-pong   | done   |
| [04](04-tiling-coupling.md)        | Border model from Tiling state; frame, focus, hide-before/show-after, tabs             | done   |
| [05](05-spaces-mission-control.md) | Spaces and Mission Control: hidden in Exposé, Space switches, fullscreen; fail closed  | done   |
| [06](06-live-follow.md)            | Live drag/resize follow: gated SkyLight notifications, display pacing, fallback        | done   |
| [07](07-lifecycle.md)              | Start/stop/reload stages, no leaked overlays, quarantine, performance budget           | done   |
| [08](08-migration-removal.md)      | Approved native check, config extension, production switch, JankyBorders removal, docs | done   |
| [09](09-focus-width-animation.md)  | Focus width grow/shrink when unfocused borders are off                                 | done   |

## Sequencing

`fix/tiling-fixes` has merged (`ba51f20`) and the cited lines were re-checked against `main` at `ff84e3b`. Order: 01 → 02 → 03 → 04 → 05 → 06 → 07 → 08. Parts 01–07 build the engine behind the compile-time feature `native-borders` on `stache-macos`, `stache-tiling` and `stache-app` (off by default; the Tiling and app features enable the macOS one; used by tests and approved native checks), so production keeps JankyBorders. Part 08 adds the approved config fields (no inert field ships earlier), flips production and removes part 09 code.

Coordinate with the [animation spec](../2026-11-09-tiling-animation/00-overview.md): borders consume the same frame events. Animation part 02 has shipped, so part 04's `BorderFrames` seam is fed from its frame pipeline: direct writes through `FrameControl::submit_direct` (`actor/frames.rs:225`) and animated per-tick batches from `Animator::send` (`animation/worker.rs:653–700`) with their acknowledgements (`on_ack`/`frame_answered`, `animation/worker.rs:705`, `:750`). Animation part 03 has shipped the shared display-link adapter (`macos/src/display_link.rs`, `FrameClock`/`FrameClockFactory`); part 06 reuses it and adds no second adapter.

The [merge gate](../../workflow.md#merge-gate) blocks merging while any part is not `done`. Acceptance IDs `TB-NN-MM` have exactly one owning part.

## Open questions

- **OQ-A animation schema — approved 2026-11-09** as proposed, with the recommended defaults (a)–(e). The gradient angle keeps its documented convention (0 = left→right, 90 = bottom→top) drawn exactly; the visual change for 45/90/135 is accepted. Approved shape (additive; every existing configuration keeps its meaning), shown in `tiling.toml` with the snake_case spelling every configuration key and enum value uses ([config](../../architecture/config.md#key-spelling)); the values were approved as `pingPong` and `easeOut` before the TOML switch:

  ```toml
  [borders]
  # New, optional. Focus color cross-fade (Hyprland `border`). Absent = instant, as today.
  transition = { duration = 200, easing = "ease_out" }
  
  [borders.focused]
  width = 4
  gradient = { from = "#b4befe", to = "#cba6f7", angle = 45 }
  # Existing table, one new optional field `style`.
  # "ping_pong" (default, legacy): colors swap, `duration` per leg.
  # "once" | "loop" (Hyprland `borderangle`): the angle turns 360°, `duration` per turn.
  animation = { style = "loop", duration = 4000, easing = "linear" }
  ```

  Recommended defaults: (a) `style` absent = `ping_pong`, so legacy configs animate exactly as today (focused window only); (b) `once`/`loop` apply to every bordered window whose active state carries them (Hyprland-like, so an animated `unfocused` gradient rotates too), not only the focused one; (c) rotation increases the angle (counterclockwise under the angle convention), no `direction` field; (d) `transition` absent = instant, `easing` default `ease_out`, `duration = 0` = instant; (e) border animations ignore `tiling.animations.enabled` and the system Reduce Motion setting, consistent with the [animation spec](../2026-11-09-tiling-animation/00-overview.md) OQ-5.

- **OQ-B unattainable Space-switch guarantee — resolved 2026-11-10: structural guarantee accepted.** Native runs 3 and 4 found that on macOS 26 no available signal precedes a Space switch or Mission Control: the Dock posts none of the Exposé notifications (all registered without error) and the private Space-change event arrives about 1 ms before the public completion. The user approved the structural guarantee as the pass criterion: no border is visible in Mission Control, App Exposé, Show Desktop, a fullscreen transition or on an inactive Space, and during a Space slide a border may only travel with its own window (each overlay belongs to its target's Space, `transient`, never on all Spaces). Detection stays as a best-effort, fail-closed layer. The formal native check (20 repeats per scenario, 60 fps recording) verifies this criterion; a border shown apart from its window, or in Mission Control, Exposé, Show Desktop or a fullscreen transition, still fails.

### Resolved (2026-11-09)

- **OQ-1 width per state → single width.** One width from the active (focused) state for every border, as today (decision 7, part 02).
- **OQ-2 gradient angle → exact, any direction.** The configured angle is drawn exactly; the field's documented meaning is kept; non-bucket angles render differently than under JankyBorders (part 02).
- **OQ-3 overrides → focused only.** Per-layout and `floating` overrides color only the focused window (part 04).
- **OQ-4 corner radius → private with fallback.** `SLSWindowIteratorGetCornerRadii` (gated, `dlsym`) with a measured per-OS fallback; `square` = 0 (part 01).
- **OQ-5 Mission Control and Spaces → must be guaranteed.** Borders are never visible in Mission Control, App Exposé, Show Desktop, Space switches, fullscreen transitions or on inactive Spaces; a dedicated part with its own native check (part 05).
- **OQ-6 ignore matching → bundle id and name.** Matched against the bundle id and localized application name from Tiling's tracking; JankyBorders matched the process (executable) name, so an entry naming only an executable that differs from the application name no longer matches (documented in part 08).

## Affected docs

Each part updates only its shipped slice: the Borders section of [features/tiling.md](../../features/tiling.md#borders), [architecture/overview.md](../../architecture/overview.md) (owner map), [guides/testing.md](../../guides/testing.md) (fakes and native gates), [guides/performance.md](../../guides/performance.md) (threads, overlays, budget) and the `enabled` schema description plus the added animation fields (part 08). Until part 08, the feature doc carries one "planned" line pointing here.
