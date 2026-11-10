---
status: done
---

# 01 — Per-application AX owner shards

Read when implementing after separate approval (`fix/tiling-fixes` merged as `ba51f20`). Paths are under `src/crates/`. This part changes threading only; window behavior, outcomes and Tiling state are unchanged.

## Scope

`macos/src/windows/` (`adapter.rs`, `core.rs`, `driver.rs`, `commands.rs`, `mod.rs`, `testing.rs`, `tests.rs`), and `modules/tiling/src/backend.rs` only where the owner thread name and adapter options are set. No change to `animation/`, `state/` or the issuance types beyond what routing needs.

Today one `stache-tiling-ax-owner` thread (`backend.rs:31`) owns every application's observer, retained elements, enumeration and writes (`windows/mod.rs:8–19`), so one slow application stalls all others. AX objects are `!Send` (`ax/mod.rs:10–15`), so per-application writers must own their own elements: the owner itself is split per application.

## Requirements

- `WindowAdapter` becomes a router over **application shards**. A shard is one thread with its own run loop (`RunLoopDriver`), `SystemAx`, `AxObserver`, retained window elements, enumeration state and command queue for exactly one tracked `ApplicationId`. All AX for that application runs on its shard; the router itself makes no AX call.
- Shards are created on `track` and joined on `untrack`/`stop` within the existing budgets. A shard that does not exit keeps its join handle and its retained resources in the existing quarantine (`Quarantined`, `CleanupReport`, `retry_cleanup`); no thread is detached and no restart happens while quarantined.
- Bounded: at most `MAX_APPLICATION_SHARDS` (proposed 32) dedicated shards. Further applications are hosted on one shared **overflow shard** that runs today's multi-application core unchanged; the status diagnostic names how many applications share it. Each shard's command queue is bounded (reuse `OWNER_QUEUE_BOUND`).
- Thread names are `stache-tiling-ax-<n>` and `stache-tiling-ax-overflow`, claimed through the existing `NameGuard`. They never equal MenuAnywhere's `stache-ax-owner` (still rejected).
- Identity is unchanged: `WindowInstance` stays `(adapter, epoch, application, serial)`; serials stay unique per adapter epoch (mint them in the router or partition them so a shard can never reuse another's). The `GenerationCell` is shared by all shards; `WindowIssuance::admit`/`commit` keep running on the shard immediately before the native send. Seal and invalidation stay lock-free and reach every shard.
- Events from every shard go into the one bounded `EventQueue` with the existing per-application ordering (create/destroy/focus never reorder within an application). Cross-application order carries no guarantee today and gains none.
- Commands for a token whose application has no shard return the same typed `Stale`/`NotIssued` outcome as today. Hops stay bounded by `hop_wait`; the router never holds its lock across a send to a shard or a wait.
- Main still owns only AppKit applications and `NSScreen`; no shard waits on main.
- Disabled Tiling, and a running Tiling before the first `track`, own zero shards.

## Acceptance criteria

- [x] TA-01-01: Each tracked application's AX observation, enumeration, snapshots and actions run on its own shard thread; fakes prove an application whose fake AX call is held never delays another application's action or snapshot.
- [x] TA-01-02: Shard count is bounded with a working overflow shard; spawn failure, untrack, stop and a stuck shard leave typed outcomes, quarantine with retained handles and no detached thread.
- [x] TA-01-03: Existing `windows` and Tiling suites pass unchanged in meaning: token identity, A–B–A, reused CG IDs/PIDs, generation turnover, seal during a held send, per-application event ordering and drain-before-restore.

## Tests

Extend `macos/src/windows/tests.rs` and `testing.rs` with a multi-application fake system: two shards with one held send (barrier), shard spawn refusal, overflow at the cap, untrack during enumeration, stop with one shard stuck then `retry_cleanup`, serial uniqueness across shards, thread-name rejection of `stache-ax-owner`. Default tests never touch real AX.

## Notes

- Rollback: revert to the single owner; no persisted state or configuration involved.
- Unverified natively: the cost of one run loop and observer per thread, and AX behavior with many concurrent per-application callers. Part 06 records thread counts and wakeups.
- Docs: update the owner map in `macos/src/windows/mod.rs`, [architecture/overview.md](../../architecture/overview.md) and the thread list in [guides/performance.md](../../guides/performance.md).
