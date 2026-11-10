---
status: draft
---

# 02 — Snapshot overlay engine

Read when implementing the overlay panels that show and animate window images. **DRAFT — NOT SHIPPED.** This part needs separate approval. Native checks belong to part 05 (N1, N4, N5) and each needs [per-run approval](../2026-11-03-tiling/validation.md#per-run-native-approval). Paths are under `src/crates/` unless noted.

## Scope

- New `macos/src/snapshot_overlay/`: a handle off main, a main-thread owner, pure geometry and keyframe helpers, and a `testing` fake.
- `macos/Cargo.toml` adds only the `objc2-app-kit` and `objc2-quartz-core` features this needs, such as `CAKeyframeAnimation`, `CAAnimationGroup` and the `NSWindowSharingType` setter.
- Nothing in Tiling uses this part yet.
- No dependency on the borders draft's `macos/src/border_overlay/` (unimplemented). Notes record what could be shared later.

## Requirements

- **Ownership.** Follow the HUD's model:
  - native panels and layers live in main-thread `thread_local` state (precedent `macos/src/hold_hud.rs:1783`);
  - off main there is a `Send + Sync` handle holding only ids, generations and bounded queues;
  - every native job goes through the injected `MainThreadExecutor` (`run_on_main`, `macos/src/main_thread.rs:44`).
  - No AppKit or Core Animation call happens off main. A call made on main runs inline rather than dispatching to itself and waiting; nothing ever waits on main from main.
- **Bounded pool.** At most `MAX_SNAPSHOT_OVERLAYS` = 16 panels exist. They are created lazily and reused after a fade-out. A `show` with no free panel answers `PoolFull` without creating one. At most 64 overlay jobs are pending toward main; past that, `show` answers `Busy` and later commands for that overlay are coalesced (latest wins).
- **Panel properties.**
  - Borderless, non-activating `NSPanel`, transparent background, no shadow, `ignoresMouseEvents = true`, `animationBehavior = none`.
  - `sharingType = none`.
  - Collection behavior is exactly `transient | ignoresCycle`, a constant asserted by a test. It is never `canJoinAllSpaces`, `stationary` or `fullScreenAuxiliary` (contrast `hold_hud.rs:1854-1859`).
  - The layer `contentsGravity` stretches the image to the layer's bounds (decision 7), with `contentsScale` set to the display's backing scale.
- **Show.** `show(id, image, frame, above: Option<u32>)` places the panel at `frame` (AX top-left global coordinates, converted to AppKit bottom-left by a pure, tested helper) and sets the layer contents.
  - If `above` is `Some`, the panel is ordered with `orderWindow(.above, relativeTo:)` using that `CGWindowID`. If AppKit refuses it or N1 fails, the engine uses the level fallback: normal window level, above the workspace's windows. Which mode is active is a capability the engine reports.
  - Show is acknowledged only after the panel is ordered on screen and its `CATransaction` is committed and flushed, so the caller may park the real window from underneath it (decision 1) without a visible gap.
  - The panel stays ordered relative to the same window number while that window is parked; the engine never re-orders it on its own.
  - The acknowledgement is typed: `Shown`, `PoolFull`, `Busy` or `Failed(reason)`.
- **Geometry animation.** `animate(id, keyframes, duration)` adds a `CAKeyframeAnimation` on the layer's frame (position and bounds, or one transform). Rules:
  - The animation uses `calculationMode = linear`, uniform key times and the supplied sampled values. The engine never computes curves itself; part 04 samples them.
  - The panel spans the union of the start and end frames (or the display), and only the layer moves inside it. The panel never moves on every frame.
  - The model value is set to the final frame, so the layer rests there when the animation ends.
  - The end is reported to the handle off main as a typed event.
- **Contents cross-fade.** `crossfade(id, image, duration)` animates the layer's `contents` from the current image to `image` over `duration`, while any geometry animation keeps running.
- **Fade-out.** `fade_out(id, duration ≈ 100 ms)` animates opacity to 0. Afterwards it orders the panel out, clears its contents (dropping the image), returns it to the pool and reports `Released(id)`.
- **Retarget.** `retarget(id, keyframes, duration)` reads the layer's current **presentation** frame, sets it as the model value, replaces the animation from there and reports the frame it started from. The current contents are kept.
- **Remove now.** `remove(id)` orders the panel out with no fade and returns it to the pool. Part 04 uses it after an immediate reveal (drag, park refused).
- **Hard ceiling.** Every `show` arms a main-thread timer (`CFRunLoopTimer` or `NSTimer`) for `duration + 1 s`. When it fires, it removes that overlay if it is still owned by the same generation and reports `CeilingFired(id)` off main. The timer is cancelled on release. An overlay can never stay on screen past the ceiling, even when no other command ever arrives. The ceiling moves no real window; returning a parked window is part 04's job.
- **Release all.** `release_all(cancel) -> OverlayReleaseOutcome` with a bounded acknowledgement, following `hold_hud.rs:1547` (`release`) and `1594` (`wait_released`).
  - Invalidation is immediate: later commands are inert.
  - Physical cleanup is enqueued on main even when `cancel` is already cancelled: every panel ordered out and closed, contents and images dropped, timers invalidated.
  - The wait gives up on timeout or cancel and reports `Retained` with the owner. A later call reuses the queued cleanup, which runs exactly once.
- **Fake.** `testing::FakeSnapshotOverlay` records an ordered call log (`Show`, `Animate`, `Crossfade`, `FadeOut`, `Retarget`, `Remove`, `ReleaseAll`, `CeilingFired`). It has:
  - scripted acknowledgement results and latency;
  - a stuck `release_all`;
  - a manual clock for the ceiling;
  - a scripted presentation frame for retarget.

  Expose the engine through a trait so Tiling can inject the fake.

- Default tests never create a real `NSPanel` or layer.

## Acceptance criteria

- [ ] SA-02-01: Main-thread ownership. No native call happens off main, and no main-to-main wait exists. The pool is bounded at 16 and the job queue at 64, with typed `PoolFull` and `Busy` answers.
- [ ] SA-02-02: Panel properties, including the `transient | ignoresCycle` constant and `sharingType = none`, are set and asserted by tests. The coordinate conversion helper is pure and tested on several displays.
- [ ] SA-02-03: `show` is ordered relative to a `CGWindowID`, falls back to the level mode with a reported capability, and is acknowledged only after the transaction is committed.
- [ ] SA-02-04: Keyframe geometry, the contents cross-fade, the about 100 ms fade-out and retarget from the presentation frame work as specified, and each end is reported off main.
- [ ] SA-02-05: The hard ceiling removes an overlay after `duration + 1 s` with no other command. `release_all` is bounded, enqueues cleanup even when cancelled, reports `Retained` when unacknowledged and runs exactly once.
- [ ] SA-02-06: `FakeSnapshotOverlay` and the trait are available to Tiling tests.

## Tests

- Pure: AX ↔ AppKit coordinate conversion (primary display, a display above or left of it, different scales), the union panel frame, and the collection-behavior constant.
- Handle and fake state machine:
  - pool exhaustion and reuse after a fade;
  - queue bound and coalescing;
  - a ceiling firing with no further command;
  - a stale generation's ceiling being ignored after reuse;
  - `release_all` acknowledged, timed out (`Retained`), cancelled before the call (cleanup still enqueued) and retried (exactly-once cleanup);
  - commands after `release_all` being inert.
- A main-thread executor fake that runs jobs inline or holds them, proving no wait from main on main (precedent: the HUD tests in `macos/src/hold_hud/tests.rs`).

## Notes

- The borders draft (part 01, `../2026-11-09-tiling-borders/01-overlay-engine.md`) plans a separate overlay engine with different level and ordering needs. Sharing the pool or main-thread owner may be considered after both exist.
- Unverified until N1/N4/N5/N9: foreign `orderWindow(_:relativeTo:)` (including relative to a parked window), `sharingType = none` with ScreenCaptureKit on macOS 26, behavior across Spaces, Mission Control and full-screen, and any flash between the show and the park.
- Rollback: remove the module. Nothing else uses it.
