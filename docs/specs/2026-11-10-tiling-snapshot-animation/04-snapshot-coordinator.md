---
status: draft
---

# 04 — Tiling snapshot coordinator

Read when wiring snapshot animation into Tiling, after parts 01-03. **DRAFT — NOT SHIPPED.** This part needs separate approval. It authorizes no native run. Paths are under `src/crates/` unless noted.

## Scope

- New `modules/tiling/src/animation/snapshot/`:
  - a deterministic engine over a fake clock, fake capture (part 01 trait), fake overlay (part 02 trait) and the frame sink;
  - keyframe sampling;
  - the park-corner helper;
  - the parked-window ledger;
  - per-application snapshot metrics.
- `modules/tiling/src/state/mod.rs:342` (`Effect::WriteFrame` gains `cg_window_id: Option<u32>`) and `state/frames.rs:641-653` (fill it from the tracked window, `state/transition.rs:625`).
- `modules/tiling/src/state/frames.rs` and `state/transition.rs:607-610`: observations of a parked window keep the last confirmed frame (see Echoes and observations).
- `modules/tiling/src/actor/mod.rs:253-313`: the branch point that routes an `animate` job to snapshot or live, and `CancelFrame` at `:313`. Before `Effect::HideApp` (`state/mod.rs:377`) or `Effect::UnhideApp` (`:384`) runs for an application with a parked window, that window is revealed.
- `modules/tiling/src/animation/control.rs`: `AnimationControl` owns the snapshot engine next to the live worker. `configure` (`:320`) applies the style and reveals parked windows before it retires the worker or switches to `live`. `report` (`:403`) adds the snapshot report.
- `modules/tiling/src/module.rs`:
  - `Teardown` (`:75-100`) gains `SnapshotOverlay` and `ParkedWindows`;
  - `Owed` (`:374-427`) gains `snapshot_overlay` (described as `snapshot overlay`) and `parked` (described as `parked windows`);
  - `teardown` (`:518-705`) gains both stages;
  - `tracking.rs` gains `release_snapshot_overlays(hop)` and `return_parked_windows(hop)` next to `stop_frame_clocks` (`tracking.rs:603`) and `restore_enhanced_ui` (`tracking.rs:639`).
- `modules/tiling/src/status.rs:109-131`: diagnostics, plus removal of part 03's interim line.
- Injection of the real capture and overlay adapters through `TrackingOptions` (precedent: `TrackingOptions::frame_clocks`), with fakes in tests.
- Tests: `animation/snapshot/tests.rs`, `runtime_tests/animation.rs` (or a new `runtime_tests/snapshot.rs`), `module_tests.rs`, `status.rs` tests, `state/animation_tests.rs`.
- Docs: `docs/features/tiling.md` Animation, `docs/guides/testing.md`.

## Requirements

### Routing

- An `Effect::WriteFrame` with `animate: true` goes to the snapshot engine when **all** of these hold. Otherwise it goes to the existing live path (`AnimationControl::submit`), and to a direct write if that path refuses it, exactly as today:
  - animations are enabled and `style = "snapshot"`;
  - the capability is `Granted`;
  - `cg_window_id` is `Some`;
  - the job is not `cross_display` (OQ-3);
  - the overlay engine is available;
  - the engine holds fewer than `MAX_SNAPSHOT_OVERLAYS` windows.
- When a window goes live for a reason, that reason is counted (see Metrics).
- **Instant-fallback set (OQ-2).** Under `style = "snapshot"`, the planner's instant-fallback check (`state/frames.rs:622`) no longer turns `animate` off. The set is applied only when a window would take the live path, where it sends that window directly as today.
- `animate: false` (reveals, workspace switches, hide/unhide) never captures and never shows an overlay.
- **Permission.** When snapshot style first becomes active with animations enabled (an accepted configuration), the engine preflights. If the result is `NotGranted`, it calls `request_once` (part 01) once per session. While not granted, every window takes the live path, and status shows `animation: Screen Recording is not granted; windows animate live`.
- **Threading.** No new Tiling thread. The engine runs on the existing animation worker thread (`stache-tiling-animation`), which exists only while animations are enabled. Capture and overlay replies post into its bounded mailbox. No lock is held across a capture, overlay or frame submission. The effect runner never waits on capture or overlay.
- **Parked ledger.** The set of parked windows (window, park frame, last planned target position, plan sequence) lives in `AnimationControl`'s shared state, not only on the worker thread. It survives a worker join, so the stop stage and a reload can return the windows. It is bounded by `MAX_SNAPSHOT_OVERLAYS`.

### Park corner

- A pure, tested function picks the park position from the display's visible frame (AX global coordinates) and the window's final size:
  - default: the window's top-left at the bottom-right corner of the visible frame, so at most a minimal sliver stays on screen (AeroSpace precedent);
  - if another display touches that corner so the parked window would appear on it, use the bottom-left corner instead (the window's top-right at the visible frame's bottom-left); if both corners touch another display, pick the one whose parked rectangle overlaps other displays least.
- The display is the job's display. Start and target are on the same display because cross-display jobs go live.
- The park reads the position back and accepts whatever position macOS clamps it to. N9 records how much stays visible.

### Sequence for one plan

Jobs with the same `plan_sequence` form one group.

1. **Admit.** Record the job's admission time, its `from`, `target`, settings and `calculate_duration`.
2. **Capture.** Capture every window of the group in one call with a deadline of admission + **50 ms** (`CAPTURE_BUDGET`). A window whose reply is `Missed`, `Failed`, `NotFound`, `NotGranted` or `Busy` takes the live path for this plan with that reason. The rest of the group continues.
3. **Show.** Show all captured windows' overlays at their `from` frames, ordered above their own `CGWindowID`. If a show is not acknowledged `Shown` within 2 frame periods of the target display (`SHOW_BUDGET`, proposed), that window's overlay is removed and the window takes the live path (`overlay`).
4. **Park.** For each shown window, submit exactly one `FrameControl::submit_direct` (`actor/frames.rs:225`) with the park position and the target **size**, with the job's `stamp`, `revision`, `plan_sequence` and `components`. The issuance gate and the Enhanced UI lease apply as to any direct write. The park reply is **not** reported to the actor; the engine keeps its size result and records the window in the parked ledger.
5. **Animate.** Right after submitting the parks, start every shown overlay's keyframe animation in one main-thread job with one shared begin time.
6. **Target image.** When a park's readback shows the final size settled, capture the window's target image from the parked window (same adapter, deadline = remaining duration). If the image arrives before half the window's duration, cross-fade to it over the remaining motion. Otherwise, or on any capture failure, keep the stretched start image.
7. **Reveal.** When the geometry animation has ended and the park reply has arrived, submit exactly one `FrameControl::submit_direct` with the target **position only** (no size component). Its readback is combined with the park's size result into the outcome reported to the actor for that exact plan sequence as `CompletionResult::Frame`, followed by `FrameControl::finish`. The window leaves the parked ledger when the reveal readback arrives.
8. **Fade.** After the reveal readback, fade the overlay out (about 100 ms, `REVEAL_FADE`). The images are dropped when it is released.

- Every snapshot window gets exactly two writes per plan: the park (position and size) and the reveal (position only). The reveal never writes a size.
- The real window is never at its target position before the reveal write.
- **Size refused or partial.** A park whose size is refused or only partly applied still continues. The outcome is reported truthfully as `Partial` or `Failed` with the actual frame, as today.
- **Park refused or not issued.** If the park is `Stale` or `NotIssued` (the window never moved), the overlay is removed at once with no fade, the window leaves the ledger, and that outcome is reported unchanged. If the park was issued but its readback shows the window did not leave its start position, the engine reveals at once (target position write), removes the overlay without a fade and counts a reveal miss; the outcome is the truthful combination.
- **Reveal refused.** A reveal refused at the gate (`Stale` or `NotIssued`) means a newer plan owns the window; that plan's write or retarget moves it. If no newer write for that window is admitted before the overlay's hard ceiling (duration + 1 s), the engine writes its last planned target position as owned cleanup and counts a reveal miss. A parked window is never left in the corner while Tiling runs.

### Echoes and observations

- From the park submission until the reveal readback, AX readings of a parked window (moved or resized notifications, enumeration) keep the last confirmed frame, like `absorb_echo` (`state/frames.rs:862-882`, called from `state/transition.rs:607-610`). A parked reading never changes the window's display, workspace or layout and never starts a drift correction, replan or drag.
- After the reveal settles, the attempt is marked animated so the bounded settle-echo budget (`MAX_SETTLE_ECHOES`, `state/frames.rs:46`, set at `:840`) absorbs a late park echo.
- The drag gesture's rebase (`state/drag.rs:19`) treats both writes as own echoes.

### Keyframes

- One sample per frame period of the target display (`events/geometry.rs:33` `flush_period`, so 30-360 Hz), with at least 2 and at most 360 samples.
- Values come from `apply_easing` (`animation/easing.rs:55`) or `SpringState` (`animation/spring.rs:36`), using the same progress clamp as the live path, so a spring never passes its target.
- The last sample is exactly `target`. A pure, tested function produces them.

### Retarget and A–B–A

- A new plan for a window the engine holds:
  - reads the overlay's presentation frame (part 02 `retarget`);
  - animates from there to the new target with a newly computed duration;
  - keeps the window parked: if the new target size differs from the parked size, submits one direct write of the new size at the park position (the only extra re-render); otherwise writes nothing until the reveal;
  - keeps the start image and captures a new target image after a size change settles;
  - reveals at the new target position.
- The superseded plan's outcome is reported for its own plan sequence only: `NotIssued` (superseded before its reveal) or what its writes answered. A–B–A ends with the layer and the real window at A, and B is never marked applied for A.
- A non-animated write (`animate: false`) for a parked window cancels its snapshot: the overlay is removed without a fade and that direct write, which moves the window out of the corner, is its reveal.

### Drag, hide, workspace switch, reload and failure

- **Drag.** `Effect::CancelFrame` for a held window reveals at once: the reveal write of the target position, then `remove` (no fade). The outcome is reported as usual.
- **Hide and workspace switch.** Before an `Effect::HideApp` or `Effect::UnhideApp` for an application, or the effects of a workspace switch, touch a window that is parked, the engine reveals it (cancel to target: reveal write, overlay removed). The hide, unhide or switch then proceeds on its own path. A parked window is never recorded in the visibility ledger, and the park never hides or unhides an application.
- **Disable or style change.** A reload that disables animations or switches `snapshot` → `live` first reveals every parked window (reveal write, then the overlay fades as at a normal reveal), then retires the worker or switches the style. New plans follow the new configuration.
- **Overlay failure.** An overlay failure after the park (animation, cross-fade or fade-out failing) removes that overlay and counts an overlay failure. The reveal still happens on schedule. It never changes the outcome.
- **Window destroyed.** A parked window that is destroyed or whose application terminates leaves the ledger and its overlay is removed. Nothing is written.

### Truthfulness

- The overlay is cosmetic. Tiling decisions 5-9 and tiling-animation Decision 6 hold unchanged: the issuance gate checks generation, revision, exact token, plan sequence, deadline and seal at the shard send, for the park and the reveal.
- The attempt stays in flight from the park until the reveal readback.

### Stop

- The order becomes: sealed → drag → borders → frame drain → animation worker (coordinator closed) → frame clock → **snapshot overlay** → **parked windows** → Enhanced UI restore → runtime-hidden restore → watcher → displays → epoch → actor → runner.
- The snapshot overlay stage runs only after the animation worker has joined, so no new show can follow. It calls the overlay `release_all` with the stop `hop` and drops every image. An unacknowledged release leaves `snapshot overlay` owed. The parked-window return and both restores **do not wait** for it, because an overlay moves no window and its hard ceiling removes it.
- The parked windows stage writes each ledger entry's last planned target position, as owned cleanup allowed after the seal (precedents: `restore_enhanced_ui`, `tracking.rs:634-641`, and `restore_visibility` with its fresh cancellation token). It runs after the drain and the worker join, and before the Enhanced UI restore, so the lease it may take is restored next.
- A return that fails, times out or is not acknowledged leaves `parked windows` owed with the windows still parked. The module is quarantined, and a later stop retries exactly those windows. The Enhanced UI and runtime-hidden restores still run.
- No frame other than the owned return is written after the seal.
- **Crash limit.** A crash or SIGKILL mid-animation can leave a window parked. The next start's layout writes every tracked window to its target, which returns it. No journal is written. The feature doc records this.

### Metrics and diagnostics

- **Snapshot record.** For each finished snapshot group, record per exact `ApplicationId`:
  - capture latency (admission to image, p50 and p95);
  - start latency (admission to animation start, p95);
  - park latency (park submission to readback, p95);
  - reveal latency (reveal submission to readback, p95);
  - reveal misses (a reveal not at its target in the readback, a park that did not leave the start, or a reveal done as owned cleanup after the ceiling);
  - capture misses and failures;
  - overlay failures;
  - live fallbacks by reason (`not granted`, `capture missed`, `capture failed`, `no window id`, `overlay`, `pool full`, `cross display`).

  Use the same bounds as the live book (last 8 records, 64 applications, least recently recorded evicted, terminated dropped), with no per-frame allocation. A disable clears it, as with the live book (`control.rs:345-348`).

- **Status line.** One line per application with data: `animation snapshot: <name> (<bundle id>): <n> animations, capture p50 <ms> ms, p95 <ms> ms, start p95 <ms> ms, park p95 <ms> ms, reveal p95 <ms> ms, reveal misses <r>, misses <m>, failures <f>, overlay failures <o>, live <l>`. The live-path `animation metrics:` lines and instant-fallback lines are unchanged.
- **Engine-wide lines.** `animation: Screen Recording is not granted; windows animate live` and `animation: snapshot overlays are unavailable (<reason>); windows animate live`.
- Remove part 03's interim line `animation: snapshot animation is not available yet`.

## Acceptance criteria

- [ ] SA-04-01: Routing selects snapshot only when every condition holds, and otherwise uses the unchanged live path with a counted reason. `animate: false` never captures or shows. `Effect::WriteFrame` carries `cg_window_id`. The instant-fallback set applies only to windows going live (OQ-2).
- [ ] SA-04-02: Permission: preflight at activation, `request_once` at most once per session, all-live with the exact diagnostic while not granted.
- [ ] SA-04-03: The sequence capture → show → park (one write: corner position and target size) → shared-begin animation → target capture from the parked window and cross-fade (before half the duration only) → reveal (one position-only write) after both the animation end and the park reply → fade after the reveal readback. The outcome combines the reveal readback with the park's size result. Park refusals, size refusals and reveal refusals behave as specified.
- [ ] SA-04-04: A capture miss or failure, a missing window id, a cross-display job, a show failure or a full pool sends **only that window** live, in the same plan.
- [ ] SA-04-05: Keyframes are sampled from the shared curves with the spring clamp and an exact final sample, at one sample per frame period within 2..=360.
- [ ] SA-04-06: The park-corner function picks the bottom-right corner, switches when a neighboring display touches it, and is pure and tested. The clamped readback is accepted.
- [ ] SA-04-07: Parked readings never change the window's display, workspace or layout, and never start a replan or drag. Late echoes after the reveal are absorbed within the bounded budget.
- [ ] SA-04-08: Retarget starts from the presentation frame, writes a size at the park position only when the size changed, and reveals at the new target. A–B–A ends at A with each plan's outcome reported for its own sequence only.
- [ ] SA-04-09: A drag reveals at once and removes the overlay without a fade. A hide, unhide or workspace switch reveals the application's parked windows first. A disable or `snapshot` → `live` reload reveals every parked window first and then routes new plans by the new configuration.
- [ ] SA-04-10: Stop runs `snapshot overlay` then `parked windows` after the frame clock and before the Enhanced UI restore. A stuck release or a stuck return stays owed and quarantines, and a later stop retries it. Images are dropped.
- [ ] SA-04-11: Snapshot metrics (including park latency, reveal latency and reveal misses) and the exact status lines exist with bounded memory, and the interim part 03 line is gone.
- [ ] SA-04-12: The end-to-end scenarios below pass, and `docs/features/tiling.md` and `docs/guides/testing.md` describe the shipped behavior, including the crash limit.

## Tests

- **Engine (fake clock, fake capture, fake overlay, recording sink):**
  - each sequence step and its order in the overlay and write logs;
  - the 50 ms budget at the exact boundary;
  - the show budget;
  - a target image arriving before and after half the duration, and failing;
  - the reveal waiting for both the animation end and the park reply;
  - the park's size `Partial` and `Failed` reported truthfully;
  - a park `Stale` or `NotIssued` removing the overlay without a fade;
  - a park that did not move the window (immediate reveal, reveal miss);
  - a reveal refused with and without a newer plan (ceiling cleanup write, reveal miss);
  - every live reason;
  - pool exhaustion at 17 windows;
  - keyframe sampling for every easing and the spring (monotonic, never past the target, exact last sample);
  - the park-corner function: single display, a display to the right, a display below, both corners touching, different visible-frame insets;
  - A–B–A and retarget, with and without a size change;
  - drag, hide, unhide and workspace switch mid-animation;
  - disable and style switch mid-animation;
  - metrics bounds and percentiles.
- **State (`state/animation_tests.rs`):** parked readings keep the confirmed frame and change no display or workspace; late echoes after the reveal are absorbed within `MAX_SETTLE_ECHOES`.
- **End-to-end scenarios.** Run the real actor and tracking runtime with the real sharded `WindowAdapter` over `FakeWindowSystem::factory`, `FakeScreenCapture` and `FakeSnapshotOverlay` (precedent `modules/tiling/src/runtime_tests/animation.rs`). The desktop has:
  - at least 3 applications with 2-3 windows each, on a 120 Hz display;
  - fake AX write latency of 5-10 ms per write (`FakeWindowAx::on_node_write`);
  - capture latency of 10-40 ms per window, with scripted misses.

  Scenarios:
  1. A layout change (master → monocle → grid) animates every window. Each window gets **exactly** one park write (position and size) and one reveal write (position only), never a size write at the reveal. Every overlay is revealed, and every window ends at its exact target.
  2. In the same run, the fake AX arena's recorded positions show that no window is at its target position before its reveal write.
  3. An interrupted and retargeted animation, and A–B–A ending at A, with at most one extra size write at the park position per size change and correct per-sequence outcomes.
  4. One window's capture misses the budget: that window alone animates live (multiple frames written) and the others use snapshot.
  5. Permission missing: every window animates live, the not-granted diagnostic shows, and the request is made once.
  6. A workspace switch: direct writes only, no capture call and no overlay call. A workspace switch or application hide during an animation reveals the parked windows first, then proceeds.
  7. A drag during an animation: the reveal write happens at once, the overlay is removed without a fade, and no further write happens for that plan.
  8. Stop mid-animation: the overlays are released, then the parked windows are returned to their targets, then the Enhanced UI restore, then the runtime-hidden restore, in the traced order. With `release_all` stuck, `snapshot overlay` stays owed and the later stages still run. With the return stuck (a held AX write), `parked windows` stays owed, the module is quarantined, and a later stop returns the window.
  9. A configuration reload to `style = "live"` mid-animation reveals every parked window before any live frame is written.
  10. Crash and restart: a new runtime starts over a fake desktop that has a window parked in a corner (no ledger), and its startup layout writes that window to its target.

- **`module_tests.rs`:** stop order with `Teardown::SnapshotOverlay` and `Teardown::ParkedWindows`, owed and retry for both, and reload of the style.
- **Status tests:** the exact snapshot metrics line and the engine-wide lines.
- **Healthy-application test:** every "give up" rule (the capture budget, the show budget, the reveal-miss ceiling, the live instant fallback reached from snapshot fallbacks) has a test showing that a healthy application under the realistic latencies above keeps snapshot animation.

## Notes

- Known limit (decision 14): JankyBorders follows the real window, so the border goes to the corner at the park and appears at the target at the reveal.
- Known limit: a click during the motion reaches whatever is under the click-through overlay, not the parked window.
- The requirements use the resolved defaults of OQ-2 (instant-fallback set only for live windows) and OQ-3 (cross-display goes live).
- `SHOW_BUDGET` (2 frame periods) and `REVEAL_FADE` (about 100 ms) are proposed constants. N6 may tune them with a recorded decision. N9 may change the park corner rule with a recorded decision.
- Rollback: route every `animate` job live. Parts 01-03 stay.
