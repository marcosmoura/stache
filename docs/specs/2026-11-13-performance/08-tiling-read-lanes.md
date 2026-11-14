---
status: done
---

# 08 — Tiling: read effects on per-application lanes

## Scope

A slow or hung application no longer delays another application's reads, the next workspace switch or startup tracking. Read effects leave the single effects thread for a small pool with one queue per application. Decision D13 of the [overview](00-overview.md). Builds on part 07.

Files expected to change: `src/crates/modules/tiling/src/actor/mod.rs` (and a new sibling module for the lanes), `src/crates/modules/tiling/src/tracking.rs`, the stop path where it closes the runner, the Tiling test suites, and the docs below. `stache-macos` does not change.

## Requirements

- **Routing.** `ThreadRunner` sends `Track`, `Refresh`, `Snapshot`, `ReadGeometry`, `ReadFocus` and `Untrack` to a read pool of four threads (`stache-tiling-reads-<n>`). Every other effect goes to the existing `stache-tiling-effects` thread in submission order, as today.
- **Per-application order.** Read effects of one application run one at a time in submission order. Read effects of different applications run at the same time, up to the pool size.
- **Focus reads wait for Stache's own focus.** A `ReadFocus` of an application does not start before every `Focus` of a window of that application that was submitted before it has finished on the effects thread (run or discarded). `Focus` activates the application and then raises the window; the activation makes the system report the application frontmost, which queues the `ReadFocus`. On the single thread that read always ran after the raise. A read between the two would report the window focused before the raise, and its completion could switch the workspace back. No other read depends on the effects thread.
- **Coalescing.** A `Snapshot` or `ReadGeometry` for a window that already has one of the same kind queued and not started is not queued again; the queued one answers for both.
- **Capacity.** Queued reads are bounded by `RUNNER_CAPACITY`. Beyond it `submit` answers `RunnerError::Full` and the actor's existing rejection path runs. The effects thread keeps its own bound.
- **Same calls.** The pool makes the same blocking backend calls with the same deadlines, so every outcome, including `Unacknowledged`, is unchanged.
- **Cancellation and stop.** After cancellation queued reads are discarded through `EffectEnv::discard`. `close` and `close_within` close admission and join the effects thread and every read worker under the one bound. A thread that does not finish keeps its handle, and the stop stage that owns the runner stays owed exactly as today.
- **Idle.** No runner thread wakes while nothing is queued.
- **Docs.** `docs/guides/performance.md` (the thread list and the new bound), `docs/features/tiling.md` and `docs/architecture/overview.md` where they describe the effect worker.

## Acceptance criteria

- [x] PF-08-01: With every read of application A held, a workspace switch that hides A and focuses a window of application B completes: the hide, the unhide, the activation and the raise are issued and B's reads answer.
- [x] PF-08-02: For one application, a track, a refresh, three snapshots and an untrack submitted in that order reach the backend in that order while reads of three other applications run.
- [x] PF-08-03: With every backend call held and eight applications to track, four tracks are in progress at once and never more.
- [x] PF-08-04: A hundred geometry batches for one window of a held application lead to at most two backend reads for that window.
- [x] PF-08-05: With the read queue full the next read is rejected and recorded as today, while a frame write and a hide are still accepted.
- [x] PF-08-06: A stop joins every runner thread within its bound. With one read worker held in a backend call the stop reports the runner's stage owed, and a later stop joins it.
- [x] PF-08-07: The scenario, animation, borders, tabs, visibility and drag suites pass unchanged in meaning.
- [x] PF-08-09: With a `Focus` of a window of application B held between its activation and its raise, a `ReadFocus` of B submitted meanwhile does not reach the backend; it runs after the `Focus` finished and reports the raised window, and the workspace is not switched back. A `ReadFocus` of B with no `Focus` of B pending runs at once, also while the effects thread is busy with another application.
- [x] PF-08-08: `bun run lint`, `bun run format:check` and `bun run test` pass.

## Tests

- Lane unit tests for order, concurrency, coalescing, capacity and discard, driven by `FakeWindowBackend` holds. No real sleeps.
- Runner tests for PF-08-01 and PF-08-06 over the real `ThreadRunner`.
- A startup scenario modelled on the user's desktop: forty applications, one of which never answers. Every other application is tracked and laid out, startup reaches `complete`, the stuck application is listed as failed and is retried on the existing backoff.

## Notes

- Measured on the branch (2026-11-14) with the same tests on the unchanged runner and on the lanes: with every read of application A held, a workspace switch to application B issued nothing within 10 s before and completes in about 30 ms after; at a startup of forty applications with one that never answers, 1 of the other 39 was tracked while it was held before and all 39 after.
- Coalescing compares the registry stamp as well as the window, so a read queued for an earlier registry (whose completion is dropped as stale) never answers for the current one.
- `mailbox_saturation_is_a_visible_failure_not_success` now allows up to four tracks in the backend at once instead of three, since four read threads exist; what it protects is unchanged.
- Found before implementation (2026-11-14): moving `ReadFocus` to a lane without a rule would let it run between the activation and the raise of Stache's own `Focus` (`VisibilityControl::execute_focus`), read the previously focused window and, through `on_focused`, make that window's workspace visible again. Keeping `ReadFocus` on the effects thread would keep the stall this part removes: its backend call can block for the 5 s backend budget when a hung application is activated. The read therefore runs on its lane behind a per-application barrier, and PF-08-09 pins it.
- This part is about the worst case. With every application responsive it changes little that can be felt; with one slow application it removes a stall that today can last as long as that application's Accessibility timeouts.
- The pool exists only while Tiling runs, like the effects thread.
- For the user to check: launch Stache with many applications open and see windows laid out as before; switch workspaces while a heavy application is busy.
