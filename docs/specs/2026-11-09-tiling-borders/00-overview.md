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
- No configuration change: `tiling.borders` keeps its schema, defaults, validation and precedence exactly (decision 3). Only the documented meaning of `enabled` changes.
- No GPL code in Stache (decision 5). No Screen Recording permission, no SIP-requiring API, no scripting addition.
- No managing, launching, stopping or reconfiguring a running JankyBorders after part 06.
- No change to layouts, CLI or socket contracts beyond additive status diagnostics. No legacy port; `_legacy/` is read-only reference.

## Decisions

Approved by the user on 2026-11-09. They supersede the [Tiling part 09](../2026-11-03-tiling/09-jankyborders.md) note "not silently replaced with a custom border engine": this is the explicit, approved replacement.

1. **Native borders replace JankyBorders.** The part 09 Mach/CLI transport and runner are removed only in part 06, after native borders pass their approved native check. Until then production keeps the JankyBorders path and the native engine is built only behind a compile-time feature; there is never a build without a working border path.
2. **Managed windows only.** A border is drawn on a tracked tiled or floating window that is on a visible workspace and is not ignored, a system window, an inactive tab or suspended slot member, minimized, fullscreen or hidden.
3. **Configuration unchanged.** `enabled`, `focused`/`unfocused`, the per-layout and `floating` overrides with legacy `focused_state_config` precedence, solid/gradient(angle)/glow colors including `rgb()`/`rgba()`, `width`, `style` round|square, `hidpi`, gradient animation `duration`/`easing` and the ignore/blacklist semantics keep their meaning. `enabled: true` no longer means consent to overwrite an external service; the schema description and docs change accordingly.
4. **Live follow.** Borders follow user drags and resizes at display refresh. Minimal private SkyLight APIs are allowed behind compile gates and approved native checks with a public fallback, under the same policy as `_AXUIElementGetWindow` (`macos/src/ax/window_id.rs:1–19`) and the [animation spec](../2026-11-09-tiling-animation/00-overview.md#decisions) decision 2. Without live tracking a border hides while its window moves and shows at the final frame.
5. **Inspired by, not derived from.** Stache is MIT; JankyBorders is GPL-3.0. Its source **may be studied** to learn its approach (which macOS and SkyLight APIs it uses, how it tracks, orders, renders and hides border windows, how it handles events), and each part cites the files it learned from. **Hard rule:** no JankyBorders code is copied, translated line by line or closely paraphrased into Stache (names, structure, constants or comments included). Stache writes its own implementation from Apple documentation, the public behavior and the ideas. Every part's review checks this.
6. **Degrade borders only.** Any unavailable capability (overlay creation, ordering, live tracking, corner radius, rendering) degrades borders only, with a status diagnostic. Layout, commands, focus and other modules are never affected.

## Hard rule review checklist (every part)

- [ ] No JankyBorders source was copied, translated line by line or closely paraphrased; ideas are reimplemented from Apple documentation and observed behavior; the part cites the JankyBorders files it studied.
- [ ] No border on an unmanaged, hidden, minimized, fullscreen, suspended or inactive-tab window.
- [ ] No main-thread wait, no AX call on main, every queue bounded, no lock held across a native call or a main-thread dispatch, no detached thread.
- [ ] Every degradation has one status diagnostic; core Tiling behavior is unchanged when borders fail.
- [ ] Default tests create no real window, overlay, display link or SkyLight connection.

## Sources

Studied by the spec writer on 2026-11-09 for approach only (decision 5). JankyBorders at commit `a7297ca7` (<https://github.com/FelixKratz/JankyBorders>, [man page](https://github.com/FelixKratz/JankyBorders/wiki/Man-Page)):

- `src/events.c`: registers private `SLSRegisterNotifyProc` handlers for window move, resize, reorder, level, hide, unhide, close, create, destroy, Space and front-application changes; focus is re-evaluated after short delays (10–50 ms).
- `src/windows.c`: `SLSRequestNotificationsForWindows` subscribes per window; `SLSWindowIteratorGetCornerRadii` (resolved by `dlsym`, macOS 26 only, `src/main.c`) gives the target's corner radius, else a fixed fallback; per-application allow/deny by process name.
- `src/border.c`: one private SkyLight window per target; the border is drawn **outside** the target frame (outset by width plus padding) with a clipped inner rounded path; moves and orders the border in one `SLSTransaction` relative to the target id, copying its level and sub-level; hide orders out; unhide orders back in immediately when the target is unhidden. Our reading: the unhide path re-shows the border as soon as the window server reports the target unhidden, possibly before the application has drawn, which is consistent with the observed blink (unverified).
- `src/misc/window.h`, `src/misc/drawing.h`, `src/animation.c`: window creation tags/shadow, `hidpi` as window resolution 1× or 2×, Core Graphics stroke, gradient and glow drawing, `CVDisplayLink` pacing for its proxy animation.
- yabai issue [#2835](https://github.com/koekeishiya/yabai/issues/2835): leaked proxy/overlay windows remaining on screen; a known hazard part 05 guards against.
- In tree: the Hold-to-Quit HUD (`macos/src/hold_hud.rs`) is the existing pattern for main-thread-owned `NSPanel` + `CALayer` overlays keyed by opaque handles and dispatched through `MainThreadExecutor` (`sdk/src/threading.rs:321`).

## Parts

| Part                          | Summary                                                                           | Status |
| ----------------------------- | --------------------------------------------------------------------------------- | ------ |
| [01](01-overlay-engine.md)    | Main-thread overlay window pool, ordering, Space/level behavior, solid stroke     | draft  |
| [02](02-rendering.md)         | Core Animation styles: solid, gradient, glow, width, style, hidpi, GPU animation  | draft  |
| [03](03-tiling-coupling.md)   | Border model from Tiling state; frame, focus, hide-before/show-after, tabs        | draft  |
| [04](04-live-follow.md)       | Live drag/resize follow: gated SkyLight notifications, display pacing, fallback   | draft  |
| [05](05-lifecycle.md)         | Start/stop/reload stages, no leaked overlays, quarantine, performance budget      | draft  |
| [06](06-migration-removal.md) | Approved native check, production switch, JankyBorders code removal, lasting docs | draft  |

## Sequencing

Starts only after `fix/tiling-fixes` merges into `main`; rebase and re-check the cited lines first. Order: 01 → 02 → 03 → 04 → 05 → 06. Parts 01–05 build the engine behind the compile-time feature `native-borders` on `stache-tiling` and `stache-app` (off by default; used by tests and approved native checks), so production keeps JankyBorders. Part 06 flips production and removes part 09 code.

Coordinate with the [animation spec](../2026-11-09-tiling-animation/00-overview.md): borders consume the same frame events. Part 03 defines a `BorderFrames` seam fed from the frame write path that exists when it lands (synchronous `AnimationSink::write` today; animation part 02's per-tick batches later); whichever spec lands second adapts the seam. Part 04 and animation part 03 share one display-link adapter (`macos/src/display_link.rs`): whichever lands first owns it.

The [merge gate](../../workflow.md#merge-gate) blocks merging while any part is not `done`. Acceptance IDs `TB-NN-MM` have exactly one owning part.

## Open questions

- **OQ-1 width per state.** Today every border shares the active state's width. Keep that (default in this spec) or honor each state's own `width` (`unfocused.width` would start to matter)?
- **OQ-2 gradient angle.** Keep the two diagonal directions of `policy.rs` (default in this spec, "identical") or honor the exact configured angle, which Core Animation can draw?
- **OQ-3 per-layout overrides on unfocused windows.** Today they color only the focused window. Keep (default) or apply the layout/floating state to each window's own workspace?
- **OQ-4 corner radius source.** Private `SLSWindowIteratorGetCornerRadii` (gated, part 01) with a measured per-OS fallback constant, or a fixed constant only? `square` always uses 0.
- **OQ-5 Spaces and fullscreen.** Borders hide while a window is fullscreen and while its macOS Space is not active (default). Should they also be hidden in Mission Control (best effort, part 01 gate) or is that acceptable as observed?
- **OQ-6 blacklist matching.** JankyBorders matched entries against the process name; native borders match the application's bundle id and localized name from Tiling's tracking. Accept this as the "same semantics" (default), or also match the executable name?

## Affected docs

Each part updates only its shipped slice: the Borders section of [features/tiling.md](../../features/tiling.md#borders), [architecture/overview.md](../../architecture/overview.md) (owner map), [guides/testing.md](../../guides/testing.md) (fakes and native gates), [guides/performance.md](../../guides/performance.md) (threads, overlays, budget) and the `enabled` schema description (part 06). Until part 06, the feature doc carries one "planned" line pointing here.
