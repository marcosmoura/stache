---
status: done
---

# 04 — Enhanced UI suppression lease

Read when implementing after part 02 (part 03 is not required) and separate approval. Paths are under `src/crates/`.

## Scope

`macos/src/ax/` (a typed read and set of the application element's `AXEnhancedUserInterface` boolean on `WindowAx`, with its fake), `macos/src/windows/{core.rs,commands.rs,tests.rs}` (the lease on each application shard), `modules/tiling/src/{status.rs,actor/diag.rs}` (diagnostics and counters). No configuration change.

Nothing in the tree reads or clears `AXEnhancedUserInterface` today. Applications with it on animate every write themselves and lag about 250 ms (see the [overview sources](00-overview.md#sources)).

## Requirements

- **Every frame write path.** The lease wraps every frame write a shard issues: animated intermediate and final frames, direct writes (layout, preset, floating centering, drag snap-back) and any fallback-to-instant write. `Raise`, hide/unhide and reads take no lease.
- **Batch scope.** Before the first write of a batch for an application, the shard reads the application's attribute once. If it is `true`, the shard sets it `false` and records a lease owned by this adapter epoch and exact `ApplicationId`. The lease stays open while that application has queued or in-flight frame writes and is restored (set back to `true`) after the application's last write of the batch is acknowledged, plus a short idle grace (proposed one frame period, at most 50 ms) so back-to-back batches do not toggle it repeatedly (the toggle itself is costly in Chromium-based applications).
- **Restore only what Stache changed.** Restore runs only for a lease this runtime took, only to the value it observed (`true`), and only on the exact application instance. An application that was already `false` is never touched.
- **Re-enable detection, no fighting.** At the final write of a batch the shard re-reads the attribute once. If something else turned it back on during the lease, the shard counts it in diagnostics and does **not** set it off again during that batch; the next batch may take a new lease once. An application that re-enables it in two consecutive batches is reported to part 06's fallback as unable to keep up.
- **Failures.** A read or set failure is a typed outcome: the batch proceeds without a lease and the diagnostic names the application. A restore failure keeps the lease as owned cleanup.
- **Stop and crash safety.** Restore is owned cleanup, allowed after the seal like owned unhide: stop order is seal → coordinator closed → frame drain → **lease restore** → runtime-hidden application restore. A panic on a shard restores its open leases before the shard exits (`catch_unwind` boundary). A failed restore is retained in quarantine with the exact application and retried by `retry_cleanup`; a terminated application drops its lease without a write.
- **VoiceOver (OQ-2, approved).** Take no lease while VoiceOver is running; read that state off the shard without waiting on main (for example a published watch updated by the main-thread owner).
- **Hard kill (OQ-3, approved).** A hard kill or crash during a lease is a documented known limit; no journal is written.
- Lease bookkeeping is per shard, bounded by its tracked applications, and holds no lock across an AX call. A–B–A plans for the same application reuse the open lease rather than stacking.

## Acceptance criteria

- [x] TA-04-01: With a fake AX, every frame write path (animated, direct, fallback) of an application whose attribute is on runs inside one lease that turns it off before the first write and restores it after the last write plus the idle grace; applications already off are never written.
- [x] TA-04-02: Re-enable by a third party is detected once per batch, counted and not fought; read/set failures proceed without a lease with a diagnostic.
- [x] TA-04-03: Stop, disable, panic and application termination restore or drop every open lease in the specified order; a failed restore is quarantined and retried exactly.
- [x] TA-04-04: VoiceOver and hard-kill behavior follow OQ-2 (no lease under VoiceOver) and OQ-3 (documented limit, no journal) and are documented.

## Tests

Fake AX with per-application attribute state and call log: lease open/close around animated and direct batches, idle grace with chained batches (one toggle), already-off application, re-enable mid-lease, read failure, set failure, restore failure then `retry_cleanup`, stop with held write (restore only after drain), shard panic, application terminated mid-lease, VoiceOver running. Default tests never touch real AX.

## Notes

- Rollback: remove the lease; part 06's fallback then treats slow applications as instant.
- Unverified natively: which of the scenario applications ship with the attribute on, the actual latency gain, and whether restore causes a visible jump. Recorded by part 06.
- Docs: a lease paragraph in the Animation and native adapter notes of [features/tiling.md](../../features/tiling.md#animation), including the known limits.
