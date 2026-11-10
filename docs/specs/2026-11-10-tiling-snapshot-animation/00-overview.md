---
branch: feat/tiling-snapshot-animation
---

# Tiling snapshot animation

Read when planning or implementing snapshot-based Tiling animation. **DRAFT — NOT SHIPPED.** Every part needs separate approval, and every native gate needs its own per-run approval under [validation](../2026-11-03-tiling/validation.md#per-run-native-approval). This document authorizes no native run.

## Goal

Make opt-in Tiling animations smooth on real applications at the native refresh rate. Stache captures an image of each moving window and shows it in its own overlay panel exactly over the window. It then **parks** the real window in a hidden corner of its display at its final size, animates the image with Core Animation, and finally **reveals** the real window by moving it to its final position. Each window re-renders once, out of sight, and is never visible twice. When a snapshot cannot be used, the window falls back to today's live Accessibility pipeline, never to an instant move.

## Problem

Cited from `main` at `29348fc` (2026-11-10). Paths are under `src/crates/`.

- The shipped pipeline ([tiling-animation spec](../2026-11-09-tiling-animation/00-overview.md), [feature doc](../../features/tiling.md#animation)) animates by writing each window's position and size through Accessibility on every frame:
  - per-application AX shards, latest-frame-wins mailboxes and per-display `CVDisplayLink` pacing in `modules/tiling/src/animation/` (`worker.rs` `Animator`, `control.rs` `LiveSink`, `pacing.rs`);
  - size interpolated on every frame, with an exact final frame (`animation/size.rs`);
  - an `AXEnhancedUserInterface` lease around every frame write;
  - a last-resort per-application instant fallback: under 15 written frames per second or more than 250 ms late in 2 of the last 3 layout changes (`animation/fallback.rs:18-31`).
- On the user's 120 Hz desktop (2026-11-10), animations ran but were **choppy**. Every AX size write makes the application re-lay out and redraw, and most applications need longer than one frame for that.
- Position-only frames with a size snap at the end (an earlier variant) looked wrong to the user.
- No public macOS API animates the content of another application's window smoothly.
- The user chose **snapshot animation** on 2026-11-10.
- Writing the real window to its final frame at the start (the first draft of this spec) shows it twice: the real window at its destination and its own image sliding toward it. The **park model** (Decision 1) keeps the real window out of sight until the image arrives.

Branch points today:

- `Effect::WriteFrame` dispatch in `modules/tiling/src/actor/mod.rs:253-312`: `animate` jobs go to `AnimationControl::submit` (`animation/control.rs:420`), everything else to `FrameControl::submit_direct` (`actor/frames.rs:225`).
- The planner decides `animate` in `state/frames.rs:619-625`: the `revealed` rule and the instant-fallback set (`state.animation_fallback`).
- `Effect::WriteFrame` (`state/mod.rs:342`) carries no `CGWindowID`. The tracked window state holds one (`state/transition.rs:625`, filled from `WindowSnapshot::cg_window_id`, `macos/src/windows/events.rs:90`).

## Non-goals

- No SIP-requiring API and no private SkyLight call. `_AXUIElementGetWindow` stays the only private symbol, under its existing policy (`macos/src/ax/window_id.rs:1-19`).
- No change to the app-wide Accessibility gate. Screen Recording is never a gating permission (`docs/specs/2026-11-07-accessibility-gate/00-overview.md:17`).
- No change to `enabled` (default `false`), `duration` or `easing`, and none to the curves or per-window duration scaling.
- No animation of workspace switches, reveals, hide/unhide or focus.
- No removal of the live AX path. It stays the fallback and the `style = "live"` behavior.
- No image on disk, in logs or sent anywhere.
- No dependency on the unimplemented borders overlay engine (`macos/src/border_overlay/`, [borders draft part 01](../2026-11-09-tiling-borders/01-overlay-engine.md)).
- No legacy port. `_legacy/` is read-only.

## Decisions

Approved by the user on 2026-11-10 unless marked (orchestrator).

1. **Model: park and reveal (orchestrator, 2026-11-10; replaces the first draft, see OQ-1 under Resolved questions).** For each animated window:
   1. capture its start image with ScreenCaptureKit;
   2. show it in a Stache overlay exactly over the real window;
   3. **park** the real window with one gated direct write (`FrameControl::submit_direct`, `actor/frames.rs:225`, so the Enhanced UI lease applies): its position goes to a hide corner of its own display and its size to its **final size**.
      - The corner follows AeroSpace: the window's top-left goes to the bottom-right corner of the display's visible frame, so at most a minimal sliver stays on screen.
      - macOS may refuse a fully off-screen position. The park reads back and accepts the clamped position.
      - This is the window's only re-render, and it happens out of sight;
   4. animate the overlay layer from start to target with Core Animation;
   5. while the layer animates, once the parked window has settled at its final size, capture the **target image** (decision 7). If it arrives before about half the duration, cross-fade the layer's contents to it. Otherwise keep the stretched start image;
   6. at the end, **reveal**: one gated direct write of the final **position only** (no re-render), then the settle readback, then fade the overlay out (about 100 ms).

   Each animated window gets exactly two writes: the park (position and size) and the reveal (position only). The reported outcome is the reveal write and its readback, combined with the park write's size result. A refused or partial size is reported truthfully (Partial or Failed), as today.

2. **Screen Recording is required for snapshot style.** This **supersedes** the tiling-animation spec's non-goal "No GPU proxy, window snapshot, Screen Recording permission or SIP-requiring API" (`../2026-11-09-tiling-animation/00-overview.md:26`). It also supersedes its Goal ("without Screen Recording, snapshot proxies", line 11) and its Sources (lines 61-62), but only for Screen Recording and window snapshots. That archived spec is not edited. SIP-requiring APIs and SkyLight stay excluded.
3. **Fallback.** A window takes the existing **live AX path** (today's per-frame pipeline, including its instant fallback) when:
   - the permission is missing;
   - a capture fails or misses its budget;
   - the window has no `CGWindowID`;
   - an overlay cannot be shown.

   Status shows a diagnostic. A window never moves instantly just because snapshot is unavailable.

4. **Configuration.** New key `animations.style = "snapshot" | "live"`, default `"snapshot"`. `enabled` (default `false`), `duration` and `easing` are unchanged. `style` is ignored while animations are disabled. This supersedes the tiling-animation non-goal "No new configuration key" (`../2026-11-09-tiling-animation/00-overview.md:27`).
5. **Permission behavior (orchestrator).** Screen Recording is a **non-gating** capability.
   - Preflight with `CGPreflightScreenCaptureAccess`, which never prompts.
   - Request with `CGRequestScreenCaptureAccess` at most once per app session, and only when snapshot style becomes active with animations enabled.
   - The bundle carries `NSScreenCaptureUsageDescription` (Tauri config or `Info.plist`).
   - Document two limits. Ad-hoc signed dev builds (`app/tauri.conf.json:25`, `signingIdentity: "-"`) may lose the grant after a rebuild. macOS may require a relaunch after granting.
6. **Capture (orchestrator).**
   - Use a ScreenCaptureKit single-window screenshot: `SCScreenshotManager` with an `SCContentFilter` for one `SCWindow` matched by `CGWindowID`.
   - Capture the windows of one plan concurrently with a **50 ms budget** from job admission. A window that misses it takes the live path for that plan.
   - Images live only in memory for the animation and are released at its end.
   - Only windows about to animate are captured.
   - Overlay panels set `sharingType = none`.
   - The binding is a new dependency, `objc2-screen-capture-kit`, in the 0.3.x family that matches the existing `objc2-*` 0.3.2 crates (`macos/Cargo.toml:21-29`). It is absent from `Cargo.lock` today. Verify the version and features before adding it.
7. **Content during resize (orchestrator).**
   - The start image is stretched to the layer's animated frame.
   - The **target image** is captured from the **parked** window once it has settled at its final size. If it arrives before about half the duration, cross-fade the layer's contents from the start image to the target image during the remaining motion. Otherwise keep the stretched start image.
   - Whether ScreenCaptureKit captures a window that is mostly off-screen, and how fast, is native gate N8. Until N8 passes, a failed or empty target capture only means the start image is kept.
   - Always finish with the fade-out.
8. **Overlay windows (orchestrator).**
   - One borderless, non-activating, transparent, click-through (`ignoresMouseEvents`) `NSPanel` per animated window, from a bounded pool (`MAX_SNAPSHOT_OVERLAYS` = 16, matching `MAX_ANIMATED`, `animation/mod.rs:66`).
   - Each panel is ordered directly above its own (parked) window with `orderWindow(_:relativeTo:)` and that window's `CGWindowID`, so the overlay keeps the window's z-order. If AppKit refuses a foreign window number (native gate N1), the fallback is the normal window level above the workspace's tiled windows.
   - Under the park model no real window is at its target before its reveal, so a real window can no longer show in the gaps between overlays or cover another window's overlay from its final frame.
   - Collection behavior is `transient | ignoresCycle`, without `canJoinAllSpaces`, `stationary` or `fullScreenAuxiliary`. This aligns with borders draft part 05 (`../2026-11-09-tiling-borders/05-spaces-mission-control.md:16`) and is the opposite of the HUD (`macos/src/hold_hud.rs:1854-1859`).
   - All AppKit and Core Animation work runs on main and is owned like the HUD:
     - `thread_local` panels (`hold_hud.rs:1783`) and handles off main;
     - jobs through `run_on_main` (`macos/src/main_thread.rs:44`);
     - release with a bounded acknowledgement (`hold_hud.rs:1547` `release`, `1594` `wait_released`).
   - Never wait on main from main. Pending overlay work is bounded.
   - **Hard ceiling:** every overlay removes itself on main after duration + 1 s, even if Tiling never asks. The ceiling moves no window: if the coordinator dies, parked windows are returned by the `parked windows` stop stage (decision 13) or by the next start's layout.
   - The engine may later be shared with the borders overlay engine. It does not depend on it.
9. **Timing curves (orchestrator).**
   - Geometry uses a `CAKeyframeAnimation` whose values are sampled from the existing curves: `apply_easing` (`animation/easing.rs:55`) and `SpringState` (`animation/spring.rs:36`), including the clamp that keeps a spring from passing its target.
   - Per-window duration scaling stays `AnimationSettings::calculate_duration` (`animation/mod.rs:103`).
10. **Retarget and A–B–A (orchestrator).**
    - A new plan for a window that is animating starts its layer from the layer's current presentation frame.
    - The real window is still parked. If the new final size differs, one gated direct write sets it at the park position; that is the only extra re-render. Otherwise no write happens until the reveal.
    - The start image is kept, and a new target image is captured.
    - The reveal writes the new target position.
    - A–B–A ends at A, with each plan's outcome reported truthfully for its own sequence.
11. **Truthfulness and authority (orchestrator).** The overlay is cosmetic.
    - The reported outcome is the reveal write and its readback combined with the park write's size result (Applied, Partial, Uncertain, Stale or NotIssued), per exact plan sequence (decision 1).
    - The park and reveal writes both pass the issuance gate.
    - [Tiling decisions](../2026-11-03-tiling/00-overview.md#decisions) 5-9 and tiling-animation Decision 6 apply unchanged:
      - authority is checked at the shard send;
      - stop seals, and drains before restore;
      - no lock is held across a native call;
      - queues are bounded and no thread is detached;
      - A–B–A never marks a successor applied.
    - An overlay failure never changes an outcome.
12. **Unchanged behavior.**
    - Workspace switches, reveals and hide/unhide never animate (the `revealed` rule, `state/frames.rs:619-621`). They keep their own paths: runtime-hidden applications and the `revealed` rule.
    - A parked window is **not** hidden in the visibility ledger's sense. The park never interacts with application hide or unhide.
    - If an application is hidden, or a workspace switches, while one of its windows is parked, that window is revealed first (cancelled to its target: reveal write, overlay removed), then the hide or switch proceeds.
    - A drag that starts during an animation (`Effect::CancelFrame`, `actor/mod.rs:313`) reveals at once: the target position is written, then the overlay is removed without a fade.
13. **Stop (orchestrator).** Parked windows are never left in the corner.
    - Returning them is **owned cleanup allowed after the seal**, like the Enhanced UI lease restore and the owned unhide: each parked window gets one write of its last planned target position.
    - The shipped order (`module.rs:518-705`, `Owed` at `module.rs:374-427`) becomes: sealed → drag → borders → frame drain → animation worker (coordinator closed) → frame clock → **snapshot overlay** release → **parked windows** return → Enhanced UI lease restore → runtime-hidden restore → watcher → displays → epoch → actor → runner.
    - The snapshot overlay stage releases every panel on main with a bounded acknowledgement. If the release is not acknowledged, the stage stays owed as `snapshot overlay`. Images are dropped.
    - If the return cannot complete, the stage stays owed as `parked windows`, the module is quarantined, and a later stop retries exactly the windows still parked.
    - A configuration reload that turns animations off, or changes `style` from `snapshot` to `live`, reveals every parked window first.
    - **Known limit:** a crash or SIGKILL during an animation can leave a window parked in the corner until the user moves it or Tiling lays it out again after a restart. Tiling's startup layout already writes every window to its target, so the next start fixes it. There is no journal (compare OQ-3 of the [tiling-animation spec](../2026-11-09-tiling-animation/00-overview.md)).
14. **Borders (orchestrator).** JankyBorders follows the real window: it goes to the corner at the park and appears at the target at the reveal, so the border is missing during the motion. This is a documented known limit.
15. **Metrics.** Per application, in the existing metrics book style (`animation/metrics.rs:154-287`):
    - capture latency p50 and p95;
    - start latency (job admission to animation start);
    - park latency (park submission to its readback) and reveal latency (reveal submission to its readback);
    - reveal misses (a reveal whose readback is not at the target position, or that was refused);
    - capture misses and failures;
    - overlay failures;
    - live fallbacks by reason.

    Status diagnostics follow the existing format (`status.rs:109-131`). The live-path metrics stay.

## Resolved questions

- **OQ-1 resolved: park model (orchestrator, 2026-11-10; parts 02, 04, 05).** The first draft of decision 1 wrote each real window to its final frame at the start. The real window then stayed visible at its destination while its own snapshot slid toward it, so every window appeared twice, and no ordering of the overlays could fix that. Decision 1 now parks the real window in a hide corner of its display at its final size and reveals it with a position-only write at the end. Decisions 7, 8, 10, 12, 13 and 14 follow from it, and gates N8 and N9 were added. The z-order question reduces to decision 8: each overlay sits above its own parked window, and nothing real is at a target before its reveal.
- **OQ-2 resolved with the proposed default (orchestrator, 2026-11-10; part 04): the instant-fallback set under snapshot style.** The planner sets `animate: false` for applications in the live instant-fallback set (`state/frames.rs:622`), so an application that once failed live would never get a snapshot. Under `style = "snapshot"`, the set applies only to windows that would take the live path. Its windows still try snapshot first.
- **OQ-3 resolved with the proposed default (orchestrator, 2026-11-10; part 04): cross-display moves.** A panel spanning two displays with different scales or Spaces is unproven, and the park corner is per display. Cross-display jobs take the live path with reason `cross display` until N5 proves otherwise.
- **OQ-4 resolved with the proposed default (orchestrator, 2026-11-10; part 01): usage string.** Whether macOS 26 reads `NSScreenCaptureUsageDescription` is unverified. Part 01 adds the key anyway (decision 5) and records what Apple documents. N3 verifies it.

## Open questions

None. New questions are recorded here and answered before the affected part starts.

## Sources

Orchestrator research, 2026-11-10. Not re-verified while writing this spec.

- ScreenCaptureKit `SCScreenshotManager` single-image capture with a single-window `SCContentFilter` (macOS 14+). A desktop-independent window filter captures only that window, so Stache's overlays above it cannot appear in a capture. `sharingType = none` is defense in depth (N4).
- `CGPreflightScreenCaptureAccess` and `CGRequestScreenCaptureAccess` (CoreGraphics, macOS 10.15+).
- `NSWindow.orderWindow(_:relativeTo:)` takes a window number. Whether a foreign window number is honored is unverified (N1).
- yabai animates with GPU proxies through private SkyLight and partially disabled SIP. Its window-image idea is reused through public ScreenCaptureKit only; nothing is copied.
- AeroSpace (`github.com/nikitabobko/AeroSpace`) hides windows by moving them into a corner of their monitor so that only a sliver stays visible, its documented hide-in-corner technique. It is precedent for the park corner (decision 1); nothing is copied. Not re-verified.

## Parts

| Part                                | Summary                                                                                   | Status |
| ----------------------------------- | ----------------------------------------------------------------------------------------- | ------ |
| [01](01-screen-capture-adapter.md)  | Screen Recording capability, request-once, usage string, single-window capture adapter    | draft  |
| [02](02-snapshot-overlay-engine.md) | Main-thread overlay panel pool, keyframe geometry, cross-fade, fade-out, ceiling, release | draft  |
| [03](03-animation-style-config.md)  | `animations.style` (`snapshot` default, `live`), schema, bindings; snapshot behaves live  | draft  |
| [04](04-snapshot-coordinator.md)    | Tiling snapshot coordinator, park and reveal, live fallback, retarget, stop, metrics, E2E | draft  |
| [05](05-native-validation-docs.md)  | Native gates N1-N9 (each per-run approved) and lasting docs                               | draft  |

## Sequencing

- Order: 01 → 02 → 03 → 04 → 05.
- Parts 01, 02 and 03 are independent and may run in parallel if approved.
- Part 04 needs 01-03. Part 05 runs last.
- Each part compiles, tests and commits alone, and leaves animations working (live).
- The [merge gate](../../workflow.md#merge-gate) blocks merging while any part is not `done`.
- Acceptance IDs `SA-NN-MM` each have exactly one owning part.

## Affected docs

- Now (with this spec): [roadmap](../../roadmap.md) row, [index](../../index.md) route, and one "planned" line in [features/tiling.md](../../features/tiling.md#animation).
- Part 03: [features/tiling.md](../../features/tiling.md#animation) (the `style` key and that `snapshot` currently behaves as `live`) and [architecture/config.md](../../architecture/config.md) if it lists animation keys.
- Part 04: [features/tiling.md](../../features/tiling.md#animation) (snapshot behavior with park and reveal, fallback, stop order, the crash limit, metrics) and [guides/testing.md](../../guides/testing.md) (fakes and scenarios).
- Part 05: [features/tiling.md](../../features/tiling.md#animation), [guides/performance.md](../../guides/performance.md), [architecture/overview.md](../../architecture/overview.md), [guides/testing.md](../../guides/testing.md) and [roadmap](../../roadmap.md). Part 05 also removes the "planned" line.
