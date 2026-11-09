---
branch: feat/tiling-borders
---

# Tiling native borders

Read when planning or implementing the Tiling-owned border engine that replaces the JankyBorders integration. **DRAFT — NOT SHIPPED.** Every part needs separate approval, and every native gate needs its own per-run approval under [validation](../2026-11-03-tiling/validation.md#per-run-native-approval). This document authorizes no native run.

## Goal

Draw `tiling.borders` with Stache's own overlay windows instead of the external JankyBorders service. Tiling knows exactly when it hides, shows, moves, resizes and focuses each managed window, so it can hide a border before its application is hidden and show it only after the window is visible at its frame. That removes the border blink seen on some applications (Ghostty yes, Spotify no) when they are unhidden or activated. The blink also happens with Tiling paused on a plain ⌘H and Cmd-Tab, and Stache sends no border command then, so it comes from JankyBorders itself; its cause is not verified.

## Problem (current code)

Cited from `fix/tiling-fixes` (worktree `.worktrees/tiling-fixes`), which merges before this spec starts. Paths are under `src/crates/`.

- Part 09 drives an external service: `macos/src/borders/` (Mach `mach.rs`, CLI `cli.rs`, `transport.rs`) and the runner `modules/tiling/src/borders/runtime.rs` (thread `stache-tiling-borders`, 8 fps cap, epochs). The host composes it in `app/src/host.rs:749–762`.
- Policy is pure and reusable: `modules/tiling/src/borders/policy.rs` (`parse_color`, `state_settings`, `focused_state`, `blacklist`, `AnimationPlan`/`PingPong`, gradient angle buckets `gradient_to_janky`). Config: `config/borders.rs` (`BordersConfig`, `BorderStateConfig`, `BorderStyle`, `BorderAnimationConfig`) and `config/color.rs`.
- The actor feeds borders only the focused window's layout and floating flag (`actor/mod.rs:967–1013`, `border_focus`). JankyBorders then tracks every window itself through private SkyLight notifications, independently of Tiling's hide/unhide (`visibility.rs`, effects `HideApp`/`UnhideApp` at `actor/mod.rs:298–305`) and frame writes (`actor/frames.rs:167`, `animation/worker.rs:60–72`).
- Today's effective rendering: one width for every border (the active state's width, `focus_command`), the per-layout and floating override colors only the focused window, unfocused windows use `unfocused`, gradients use two diagonal directions only, and gradient animation sends up to 8 color frames per second over Mach.

## Non-goals

- No borders on windows Tiling does not manage (ignored applications, system windows, Stache's own windows).
- No breaking configuration change: every existing `tiling.borders` configuration keeps parsing, validating and resolving with the same precedence (decision 3). The only schema changes are the additive animation fields of [part 03](03-animations.md) (shape pending OQ-A) and the documented meaning of `enabled`.
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
| [01](01-overlay-engine.md)         | Main-thread overlay window pool, ordering, collection behavior, solid stroke           | draft  |
| [02](02-rendering.md)              | Static Core Animation styles: solid, exact-angle gradient, glow, width, style, hidpi   | draft  |
| [03](03-animations.md)             | Hyprland-style GPU animations: angle rotation once/loop, focus cross-fade, ping-pong   | draft  |
| [04](04-tiling-coupling.md)        | Border model from Tiling state; frame, focus, hide-before/show-after, tabs             | draft  |
| [05](05-spaces-mission-control.md) | Spaces and Mission Control: hidden in Exposé, Space switches, fullscreen; fail closed  | draft  |
| [06](06-live-follow.md)            | Live drag/resize follow: gated SkyLight notifications, display pacing, fallback        | draft  |
| [07](07-lifecycle.md)              | Start/stop/reload stages, no leaked overlays, quarantine, performance budget           | draft  |
| [08](08-migration-removal.md)      | Approved native check, config extension, production switch, JankyBorders removal, docs | draft  |

## Sequencing

Starts only after `fix/tiling-fixes` merges into `main`; rebase and re-check the cited lines first. Order: 01 → 02 → 03 → 04 → 05 → 06 → 07 → 08. Part 03 starts only after the user approves the OQ-A schema. Parts 01–07 build the engine behind the compile-time feature `native-borders` on `stache-tiling` and `stache-app` (off by default; used by tests and approved native checks), so production keeps JankyBorders. Part 08 adds the approved config fields (no inert field ships earlier), flips production and removes part 09 code.

Coordinate with the [animation spec](../2026-11-09-tiling-animation/00-overview.md): borders consume the same frame events. Part 04 defines a `BorderFrames` seam fed from the frame write path that exists when it lands (synchronous `AnimationSink::write` today; animation part 02's per-tick batches later); whichever spec lands second adapts the seam. Part 06 and animation part 03 share one display-link adapter (`macos/src/display_link.rs`): whichever lands first owns it.

The [merge gate](../../workflow.md#merge-gate) blocks merging while any part is not `done`. Acceptance IDs `TB-NN-MM` have exactly one owning part.

## Open questions

- **OQ-A animation schema (blocks part 03).** Needs the user's approval before part 03 is implemented. Recommended shape (additive; every existing configuration keeps its meaning):

  ```jsonc
  "borders": {
    // New, optional. Focus color cross-fade (Hyprland `border`). Absent = instant, as today.
    "transition": { "duration": 200, "easing": "easeOut" },
    "focused": {
      "width": 4,
      "gradient": { "from": "#b4befe", "to": "#cba6f7", "angle": 45 },
      // Existing object, one new optional field `style`.
      // "pingPong" (default, legacy): colors swap, `duration` per leg.
      // "once" | "loop" (Hyprland `borderangle`): the angle turns 360°, `duration` per turn.
      "animation": { "style": "loop", "duration": 4000, "easing": "linear" }
    }
  }
  ```

  Recommended defaults: (a) `style` absent = `pingPong`, so legacy configs animate exactly as today (focused window only); (b) `once`/`loop` apply to every bordered window whose active state carries them (Hyprland-like, so an animated `unfocused` gradient rotates too), not only the focused one; (c) rotation increases the angle (counterclockwise under the angle convention), no `direction` field; (d) `transition` absent = instant, `easing` default `easeOut`, `duration: 0` = instant; (e) border animations ignore `tiling.animations.enabled` and the system Reduce Motion setting, consistent with the [animation spec](../2026-11-09-tiling-animation/00-overview.md) OQ-5.

- **OQ-B unattainable Space-switch guarantee.** If part 05's native check finds a scenario (most likely a trackpad Space swipe or a fullscreen transition started by the green button) where no available mechanism hides borders before the first transition frame, part 05 fails and stays open. Recommended: the user then chooses between another mechanism found in that run and an explicitly approved documented limit; nothing is accepted silently.

### Resolved (2026-11-09)

- **OQ-1 width per state → single width.** One width from the active (focused) state for every border, as today (decision 7, part 02).
- **OQ-2 gradient angle → exact, any direction.** The configured angle is drawn exactly; the field's documented meaning is kept; non-bucket angles render differently than under JankyBorders (part 02).
- **OQ-3 overrides → focused only.** Per-layout and `floating` overrides color only the focused window (part 04).
- **OQ-4 corner radius → private with fallback.** `SLSWindowIteratorGetCornerRadii` (gated, `dlsym`) with a measured per-OS fallback; `square` = 0 (part 01).
- **OQ-5 Mission Control and Spaces → must be guaranteed.** Borders are never visible in Mission Control, App Exposé, Show Desktop, Space switches, fullscreen transitions or on inactive Spaces; a dedicated part with its own native check (part 05).
- **OQ-6 ignore matching → bundle id and name.** Matched against the bundle id and localized application name from Tiling's tracking; JankyBorders matched the process (executable) name, so an entry naming only an executable that differs from the application name no longer matches (documented in part 08).

## Affected docs

Each part updates only its shipped slice: the Borders section of [features/tiling.md](../../features/tiling.md#borders), [architecture/overview.md](../../architecture/overview.md) (owner map), [guides/testing.md](../../guides/testing.md) (fakes and native gates), [guides/performance.md](../../guides/performance.md) (threads, overlays, budget) and the `enabled` schema description plus the added animation fields (part 08). Until part 08, the feature doc carries one "planned" line pointing here.
