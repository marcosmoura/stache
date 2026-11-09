---
status: draft
---

# 02 — Asynchronous frame pipeline

Read when implementing after part 01 and separate approval. Paths are under `src/crates/`.

## Scope

`modules/tiling/src/animation/{worker.rs,control.rs,mod.rs,tests.rs}`, `modules/tiling/src/actor/{frames.rs,mod.rs}`, `modules/tiling/src/backend.rs` (new submit-style frame method on `WindowBackend` and its fake), `macos/src/windows/{commands.rs,core.rs,adapter.rs,issuance.rs}`. Pure planning in `state/frames.rs` is unchanged except where completion handling needs the new outcome fields.

Today the stepper writes each window serially and waits for each reply (`animation/worker.rs:198–231`, `actor/frames.rs:167–212`), and each reply includes a readback (`macos/src/windows/core.rs:1079–1090`) that the stepper keeps as `confirmed` (`worker.rs:262–271`).

## Requirements

- **Animation coordinator.** The `stache-tiling-animation` worker stays the one coordinator, spawned only while `animations.enabled`. On each tick it computes the next frame of every running window and sends **one batch per application** to that application's shard. It never waits for a write to finish; acknowledgements come back as messages.
- **Latest-frame-wins mailbox.** Each shard holds at most one pending frame per window. A newer frame for the same window and plan replaces the pending one (counted as a dropped frame, not acknowledged separately); a newer plan supersedes the older plan's pending frame, which is acknowledged once as `NotIssued(Superseded)` for its own plan sequence. The mailbox is bounded by the shard's tracked windows. A slow application skips frames instead of queueing them.
- **Write-only intermediate frames.** An intermediate frame is a gated write with **no readback**. Its acknowledgement carries the per-component write status and the shard-side send time. Only an `Applied` status keeps the window animating; refused, partial, unresizable, uncertain, stale, expired or unauthorized ends it with no final snap and no retry, as today.
- **Final settle readback.** The final frame is the exact target. After `SETTLE` (50 ms, `animation/mod.rs:46`) the shard takes the one bounded readback; that readback alone decides the reported `Applied`/`Partial`/failed outcome and the confirmed frame, as today.
- **Retarget.** A new plan for a moving window starts from one bounded readback taken on the shard at retarget time (falling back to the last acknowledged written frame when the read fails). It never starts from the old destination or the planner's stale belief. A–B–A must still bring the window back.
- **Membership (OQ-1).** Until OQ-1 is approved, every write keeps today's full `AXWindows` revalidation. If approved: revalidate at the first and final write of an animation; intermediate writes check the retained element, the destroy-notification invalidation and the issuance gate, and an invalid-element error ends the animation as `Stale`.
- **Instant writes.** Direct (non-animated) frame writes also go through the shard mailbox asynchronously; the effects runner no longer blocks on a native frame write. Their outcome and readback stay as today (one write, one readback). `FrameControl` keeps its latest-plan ledger, outstanding counter and `drain`.
- **Authority.** Every write, intermediate or final, carries a `WindowIssuance` (generation, accepted revision, exact token, plan sequence, deadline) checked by `admit`/`commit` on the shard immediately before the native send. A sealed or superseded write is inert at the shard. Acknowledgements route to the actor as `Input::Completion` with the exact plan sequence; an interrupted job can never mark a successor applied.
- **Bounds.** Keep `MAX_ANIMATED` and `MAX_IN_FLIGHT_FRAMES` (16). Refusal by a full or closed coordinator still falls back to one direct write, never a silent drop. No lock is held across a native call, a shard send or a completion submit.
- **Stop.** Seal, then close the coordinator, then drain: every shard mailbox empties (sealed frames acknowledge `NotIssued(Sealed)`) and every issued write acknowledges before the owned restore runs. A shard stuck in a send leaves `frame drain` owed and the module quarantined, as today.

## Acceptance criteria

- [ ] TA-02-01: With fakes, a held write in one application does not delay frames for other applications; the held application's newer frames replace the pending one and are counted as dropped.
- [ ] TA-02-02: Intermediate frames perform no readback; the final frame performs exactly one settle readback; retarget performs at most one readback; outcomes stay truthful (Applied/Partial/Uncertain/Stale/NotIssued) per exact plan sequence, including A–B–A.
- [ ] TA-02-03: Direct writes are asynchronous; seal during held writes leaves queued frames inert; stop drains every shard before restore, or quarantines with a typed owed stage.
- [ ] TA-02-04: Membership revalidation follows the approved OQ-1 answer, with tests for a window destroyed mid-animation either way.

## Tests

Extend `animation/tests.rs` and `actor` tests with a fake shard backend that records per-application batches and can hold, refuse, partially apply or delay writes: multi-application tick, mailbox replacement and drop counts, plan supersede ack, retarget readback and its failure, final settle, seal and stop during held writes, A–B–A, coordinator full fallback. Extend `macos/src/windows/tests.rs` for write-only frames, mailbox replacement on the shard and gate checks at send. Default tests never touch real AX.

## Notes

- Rollback: restore the synchronous sink; the shards from part 01 stay.
- Without readback the echo suppression relies on the in-flight attempt (`state/frames.rs`, `absorb_echo`); keep the existing bounded settle-echo budget.
- Docs: update the Animation section of [features/tiling.md](../../features/tiling.md#animation) (latest frame, no per-frame readback) and [guides/performance.md](../../guides/performance.md).
