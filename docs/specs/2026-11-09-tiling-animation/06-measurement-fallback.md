---
status: done
---

# 06 — Measurement, instant fallback and lasting docs

Read when implementing after parts 01–04 (and 05, unless skipped) and separate approval. The native measurement needs its own per-run approval under [validation](../2026-11-03-tiling/validation.md#per-run-native-approval). Paths are under `src/crates/`.

## Scope

`modules/tiling/src/animation/` (per-application metrics and the fallback policy), `modules/tiling/src/{status.rs,actor/diag.rs,state/frames.rs}` (status diagnostics; the planner consults the fallback set when it sets `animate`), and the lasting docs. An optional gated measurement binary may be added under `app/src/bin/` behind its own feature, following `menu-native-check`; it is never built by default.

## Requirements

- **Metrics.** Per exact `ApplicationId`, per layout change (the windows of one application in one plan fold into one record: slowest rate, largest latency and end lag, summed frames), record from the coordinator and shard acknowledgements: frames scheduled, frames written (acknowledged `Applied`), frames dropped (replaced in the mailbox), effective rate (written frames ÷ wall time from first tick to final acknowledgement), per-write latency (dispatch to acknowledgement, p50/p95), and end lag (final acknowledgement minus scheduled end). Keep a bounded rolling window (proposed last 8 records per application, bounded application count) and expose a summary in status diagnostics. Metrics cost no AX call and no allocation per frame beyond fixed buffers.
- **Instant fallback policy.** An application instance moves instantly (one final write per plan, `animate: false`) with a status diagnostic `animation: <name> (<bundle id>) moves without animation: <reason>` when any of these holds:
  - its effective rate is below 15 written frames per second, or its end lag exceeds 250 ms, in 2 of its last 3 layout changes (one record per application per layout change; thresholds absolute, per the 2026-11-10 decision in the overview);
  - part 04 reports it re-enabling Enhanced UI in two consecutive batches, or the lease cannot be taken while the attribute is on;
  - its shard is the overflow shard and the overflow shard cannot meet the rate criterion;
  - the smooth path is unavailable for it (no shard, coordinator refused, fallback pacing failing the rate criterion).
- **Mid-animation switch.** When an application crosses into fallback during an animation, each of its running windows receives one final exact write at the next tick; no further intermediate frames.
- **Scope of fallback.** Sticky for the exact application instance for the runtime epoch. A relaunch (new instance), a configuration reload that turns animations off and on, or a Tiling restart clears it. It never affects other applications, never changes configuration and never disables animations globally.
- **No fighting.** After the final write and settle readback of a plan, no further write for that plan. A readback different from the target is reported as today and never retried.
- Workspace switches, reveals and drag stay non-animated (decision 4); the planner's `revealed` rule (`state/frames.rs:619–622`) is unchanged.

## Native measurement (approved run)

Fixed scenario, recorded on the user's machine under per-run approval naming the exact application instances, the temporary Next config, the display and cleanup:

- Display: one named display; record its refresh rate (and ProMotion), macOS and hardware build.
- Config: `tiling.toml` with `[animations]` `enabled = true`, `duration = 200`, `easing = "ease_out"`; one workspace in `master` layout with Ghostty, Zed, Safari (or Aside) and Spotify.
- Action: 10 toggles master → monocle and 10 monocle → master, each after the previous animation settled; repeat the set three times with warmup.
- Record per application: effective rate, dropped frames, per-write latency p50/p95, end lag, whether Enhanced UI was on and leased, fallback triggered and why, SkyLight capability, thread count, CPU and wakeups during the run against animations off.
- **Pass criterion:** every scenario application either (a) has median effective rate ≥ refresh ÷ 2, p95 end lag ≤ one frame period and zero post-settle writes (no fighting), or (b) is in instant fallback with its diagnostic; and all four applications meet (a) (OQ-4: a fallback for any of the four fails the run). The user also judges visual smoothness; effective rate is measured at AX acknowledgement, **not** at on-screen presentation, which cannot be observed without Screen Recording.
- Cleanup: restore the user's config, leave no lease open, report exact owned cleanup.

## Acceptance criteria

- [x] TA-06-01: Metrics are recorded per application and animation with bounded memory and no per-frame AX or allocation, and appear in status diagnostics.
- [x] TA-06-02: The fallback policy triggers on each listed condition with the exact diagnostic, switches a running animation to one final write, stays scoped to the exact application instance and clears on relaunch, animation re-enable or restart.
- [ ] TA-06-03: The approved native measurement is recorded with raw samples and method in [guides/performance.md](../../guides/performance.md) and meets the pass criterion; any unmet or unexecuted item is listed as unverified, not passed. **Unverified (2026-11-10): no approved native run yet; method and scenario are in performance.md.**
- [x] TA-06-04: Lasting docs describe only shipped behavior: the Animation and native adapter sections of [features/tiling.md](../../features/tiling.md#animation) (shards, latest frame, pacing, size envelope, Enhanced UI lease and its limits, SkyLight status, fallback and diagnostics), threads and measurement in [guides/performance.md](../../guides/performance.md), owner map in [architecture/overview.md](../../architecture/overview.md), fakes and native gates in [guides/testing.md](../../guides/testing.md); the planned line added with this spec is removed.

## Tests

Fake clock and fake shard backend: slow application below half rate, end lag over one period, recovery not triggered by one bad animation (2 of 3), Enhanced UI re-enable signal, overflow shard, coordinator refusal, mid-animation switch to one final write, relaunch and reload clearing, other applications unaffected, A–B–A while in fallback. Status diagnostic snapshot tests. Default tests never touch real AX.

## Notes

- Rollback: remove the policy; animations behave as after part 05.
- Unverified until the approved run: every number above, on-screen smoothness, and each application's Enhanced UI default.
