---
status: done
---

# 02 — Bounded ordered teardown

Read when changing teardown order, budgets or what happens when a stage misses its
deadline. Depends on [Part 01](01-terminate-observation.md).

## Scope

Replace `Runner::teardown` (`host.rs:1008-1031`) with explicit stages that cannot exceed
the coordinator's deadline, and gate exit on a recorded outcome.

## Requirements

### Stage order

1. **Invalidate, without joining or taking a contended lock.**
   - Set a registry shutdown latch that does not take `lifecycle`. Today the flag is set
     after that lock (`sdk/src/registry.rs:958-959`), so a blocked holder delays it.
   - Stop accepting socket connections without joining. `IpcServer::shutdown` couples
     `gate.stop()` to the join and endpoint removal (`sdk/src/ipc/server.rs:246-253`).
   - Refuse router dispatch; a late request gets a typed `unavailable` wire error.
   - Discard queued non-teardown `HostCommand`s; they must not start work.
   - Cancel the watcher, ticker and startup tokens, and invalidate module windows.
   - Fire every module-native invalidation handle, including when a start or stop holds
     a slot lock. The registry retains these handles outside slot state and `lifecycle`;
     neither lock is taken to fire them. Each module registers a `Send + Sync` handle
     at start, before native work can issue. Registration racing shutdown immediately
     invalidates the handle and permits no native issuance. Queued native actions recheck
     it at issuance and become inert. noTunes must connect this to its
     `ApplicationControl` generation: on today's registry teardown path, native
     invalidation is only reached from module `stop` (`modules/notunes/src/runtime.rs:378-384`,
     `macos/src/applications.rs:681-698`). This is logical invalidation only, not another
     native cleanup path. No module API beyond this handle and no blanket
     `unsafe impl Send` or `Sync`.
2. **Reap monitors within budget:** watcher, ticker, startup job.
3. **Stop the registry** in reverse start order, one module at a time.
4. **Native cleanup,** in this order, acknowledged per
   [Part 03](03-owner-acknowledgements.md): power assertion release, input-tap removal,
   application observers, HUD and window release, socket close, tray removal, webview
   and window release. Slots with no owner today are empty.
5. **Store the outcome,** attempt bounded record output, publish one terminal status,
   then close the signal bridge. Output failure never blocks the exit gate.
6. **Open the exit gate** and run the terminal action.

### Bounds

- Stages run from the coordinator thread. A blocked stage cannot stop later stages from
  being attempted, so owners needed there must be reachable when `Runner` is stuck.
- No unbounded wait remains in the stage path: the joins and waits listed in
  [Shipped state](00-overview.md#shipped-state) gain bounded equivalents inside their
  owning types, not in a new thread that is itself joined. `JoinHandle::is_finished` polled
  on a wakeable wait is acceptable.
- Every wait uses `min(2 s, remaining)` and wakes on escalation within one 10 ms quantum.
  This bounds the coordinator's observation, not an owner's own latency: the watcher
  checks cancellation only between 250 ms polls (`host.rs:767`).
- A module stop is one owner wait through an abandonable carrier, bounded by
  `min(2 s, remaining)`. A hung module is named and abandoned; the next module still
  stops. A module's own 2 s hop can tie this budget, which must be reported honestly.
  The registry changes needed to make abandonment safe are below; wrapping today's
  locked loop in a thread is insufficient.
- Socket endpoint removal must not depend on handler join. A connection may legitimately
  hold its handler for the five-second deadline ([ipc.md](../../architecture/ipc.md#endpoint-safety)),
  longer than a hop. Remove the owned inode after the bounded wait; keep the inode check.
- Stages not reached by the deadline are named as unacknowledged. Non-blocking steps, such
  as endpoint removal off main, still run.

### Registry isolation and lifecycle refusal

- Today `Registry::shutdown` holds `lifecycle` across its whole loop, takes each
  `slot.state` lock while finding targets, then calls `quiesce` under the slot lock
  (`registry.rs:957-984,1077-1112`). An abandoned callback would retain those locks.
- Snapshot target metadata (stable owner name, reverse-start sequence and stop-needed
  state) under a short metadata lock, released before any module code runs. Metadata
  must remain available even during a blocked start; do not obtain it by waiting on
  that start's slot state or `lifecycle`.
- No `lifecycle` lock spans a stop, and no shared slot lock spans abandonable module
  code. Isolate callback ownership from registry bookkeeping, including blocked starts,
  so abandonment retains only that carrier's owned resources, not locks needed by the
  coordinator or another module. An already-busy module is bounded and named, never
  stopped concurrently or waited on without a bound. Late completion cannot publish
  running state, restart work or change the outcome.
- Stage 1 refuses pause, resume, enable **and disable**, reload, start and registration
  with `RegistryError::ShuttingDown`, before acquiring a possibly held transition lock
  or mutating state. Recheck admission at transition commitment so shutdown races cannot
  admit fresh work. Host callers keep their signatures; discarded queued commands
  do not replace the registry's own refusal checks.
- Existing checks: `register_module` checks before the slots lock (`:409`);
  `start_all`, `service_failed`, `service_recovered` and `on_permission_change` check
  only after `lifecycle` (`:453-455,758-760,774-776,892-894`); `start_slot` checks only
  after the slot lock (`:992-995`). Move/add early admission checks to the latter
  paths. `pause`, `resume`, `set_enabled` and `reload` lack entry checks
  (`:484-590`); add them, including no-op, disable and next-start-config branches.
  `reconcile_locked` currently returns `Ok` on shutdown (`:791-792`), not a typed
  refusal; it cannot stand in for entry-point checks.

### Abandonment and failure

- An abandoned thread stays detached and inert, keeps what it owns and is named in the
  outcome. No fallback to an unbounded join, no forced release, and no kill of another
  process. An abandoned thread never triggers an exit by itself.
- A failing owner records its name and later stages continue.
- Nothing on the teardown path panics; locks tolerate poisoning, as `host.rs:436` does.

### Queued versus issued work

Queued work can be dropped by the latch and generation checks. An OS action already issued,
such as a noTunes termination or application open
([Part 01 of noTunes](../2026-11-03-notunes/01-config-native.md#enforceable-native-ownership)),
is not retried or called cancelled; its late completion stays inert.

### Exit gate

- `ExitGate` opens only after the outcome is stored in the slot, whether or not output
  was delivered. Today `AppUi::exit` opens it before
  the tray closure runs (`lib.rs:79-87`).
- Exit requests before that stay prevented; afterwards they are allowed
  (`shutdown.rs:78-84`).

### Final exit on a stalled main thread

The reply and `app.exit` both need main, so a wedged main thread would otherwise leave the
stop unbounded. The forced self-exit below is the only `std::process::exit` on the shutdown
path, and it only ever targets the current process.

- **Dispatch first.** After storing the outcome and its bounded output attempt, the
  coordinator dispatches the terminal action to main: the reply on the AppKit path,
  `app.exit` otherwise. It decides
  about a forced exit only after that dispatch, on both paths.
- **Accepted means started.** Main accepted the action when the terminal closure starts on
  main; enqueueing is not acceptance ([Part 03](03-owner-acknowledgements.md#no-invented-acknowledgements)).
  The closure and the forced exit share one claim, the Part 01 reply guard. Once the closure
  claims it, delivery completion is tracked separately; acceptance alone cannot suppress
  forced exit forever. A closure that loses the claim is inert and never replies.
- **Unclaimed at the deadline.** If the closure has not claimed by the current deadline,
  the coordinator claims forced exit and self-exits at that deadline, not 2 s later.
  This holds on AppKit and runtime paths, even after early teardown completion or a
  shortened deadline. A queue that first starts the closure after the deadline loses
  the claim and cannot reply. Serialize this race; enqueueing never buys extra time.
- **Claimed reply only.** If the AppKit closure claimed before the deadline but reply
  delivery has not completed, mark `reply_late=true` and `bound_extended=true` in terminal
  evidence. Allow at most one full 2 s hop from the deadline for delivery, not from the
  claim or a later queue resumption. The `min(2 s, remaining)` rule does not apply to
  this sole exception: at most 7 s from the first request, or 2 s after the shortened
  deadline. A reply completing during this hop prevents forced exit, and is recorded
  late. Otherwise self-exit occurs at the hop's end. A runtime action gets no reply
  exception: a claimed but incomplete runtime action still self-exits at the deadline.
  Requests after the outcome do not move these checks.
- **Store, attempt output, then exit.** Terminal evidence shares Part 01's in-memory
  slot and bounded non-blocking channel. It records
  `teardown: terminal=true forced_exit=… reply_late=… bound_extended=… outcome=… cause=… elapsed_ms=… recorded_code=…`.
  Store it even when the sink is unavailable, and never claim successful output unless
  the write completed. One output attempt costs at most 50 ms within the applicable
  deadline, never after it; with no time left, mark output unavailable without waiting.
  No sink lock spans the terminal action. Forced exit then calls
  `std::process::exit(recorded_code)` for this process. The intended code is Part 01's
  mapping, not an assertion about an observed OS status; a direct forced exit bypasses
  `dispatch`'s ordinary error-to-1 conversion.
- **What it skips.** No destructors and no `RunEvent::Exit` run, so a forced exit never
  relaunches, even after `Clean`. The kernel releases the instance lock
  (`sdk/src/instance.rs:9-10`). Tauri's own fallback makes the same call
  (`tauri/src/app.rs:585`).
- **Limit.** Only the pre-deadline claimed AppKit reply can extend the overall bound.
  Main-thread delivery may still fail; the extended bound and late/missing completion
  stay visible in the slot rather than being reported as timely delivery.

## Acceptance criteria

- [ ] `TEARDOWN-02-01` All stage-1 effects happen before any join, including while another
      thread holds `lifecycle` or a slot lock. A queued fake native action checks its
      registered invalidation handle and never issues after stage 1, including a
      registration/start race. Automated recording-fake and real SDK registry tests.
- [ ] `TEARDOWN-02-02` After invalidation a new connection is refused, a request already
      read gets typed `unavailable`, and an in-flight handler finishes unharmed. Automated
      SDK test on a temp-root socket.
- [ ] `TEARDOWN-02-03` Stage order is exactly as listed, with registry before native cleanup
      and native slots in order. Automated recording-fake test.
- [ ] `TEARDOWN-02-04` Modules stop in reverse start order and a failing module does not stop
      the next. Automated fake-registry test.
- [ ] `TEARDOWN-02-05` A hung module is abandoned at 2 s, named, and the next module stops; at
      the total, unreached owners are named. Real-registry tests with injected time cover
      blocked start and blocked stop separately, another module stopping successfully,
      and no shared lock retained across abandoned module code.
- [ ] `TEARDOWN-02-06` A hung watcher, ticker, startup job, IPC worker and module worker are
      each abandoned at budget, never joined unboundedly. Automated tests per owning type.
- [ ] `TEARDOWN-02-07` A held connection does not delay endpoint removal past the hop, and a
      foreign inode at that path survives. Automated SDK test.
- [ ] `TEARDOWN-02-08` Escalation wakes every wait within one quantum. Automated injected-clock
      test.
- [ ] `TEARDOWN-02-09` With an off-main fake executor that never drains, teardown still reaches
      its outcome and never waits on main from main. Automated host test.
- [ ] `TEARDOWN-02-10` Queued fake work is dropped, while an issued fake action is counted once
      and reported issued, with no retry. Automated unit test.
- [ ] `TEARDOWN-02-11` The gate stays closed until the outcome exists, and an earlier exit
      request stays prevented. Automated unit test.
- [ ] `TEARDOWN-02-12` A poisoned lock and a failing owner in every stage cause no panic and
      still give one outcome. Automated unit test.
- [ ] `TEARDOWN-02-13` With a queue-only executor that never starts the terminal closure, on both
      the AppKit and runtime paths, the fake exit fires once at exactly the deadline,
      after terminal dispatch and outcome storage, and not one tick earlier, including
      after an early finish and after a shortened deadline. A closure first started just
      after that deadline is inert. With an AppKit closure claimed before the deadline,
      suspend delivery, resume it just after the deadline and prove one delivered reply,
      late/extended evidence and no forced exit before or after successful delivery.
      If delivery never completes, force exit exactly at deadline + 2 s, not before.
      A claimed but incomplete runtime action exits at the deadline without the extra
      hop. Automated fake-clock tests with a fake exit, including claim/deadline races.
- [ ] `TEARDOWN-02-14` A forced exit stores one outcome then terminal evidence with
      `forced_exit=true`, the same outcome and recorded code, and calls the fake exit
      last for this process only. It starts no relaunch, even after a `Clean` restart.
      A delivered reply records `forced_exit=false`; a timely reply records no extension.
      Automated test through a recording teardown channel and slot.
- [ ] `TEARDOWN-02-15` A blocked/undrained or closed record sink cannot delay terminal
      dispatch by more than 50 ms or extend either terminal deadline. Both outcome and
      forced-exit evidence remain inspectable in the slot, with output unavailable,
      and the fake exit still fires at the applicable deadline without a held sink lock.
      Automated injected-clock and blocked-sink tests; no synchronous `Logger` fallback.
- [ ] `TEARDOWN-02-16` After stage 1, pause/resume, enable/disable (including no-ops), reload,
      start and registration return typed `ShuttingDown` without waiting on held slot or
      lifecycle locks or mutating state. Real-registry tests also cover service/permission
      entry points and an admission race; late callback completion stays inert.

## Tests

Recording fakes per stage, real-registry lock/contention tests, a fake worker group,
a blocked record sink and a queue-only executor.
Timing uses an injected clock, not `sleep`. Socket tests use temp roots.
`cargo nextest run -p stache-app -p stache-sdk --features stache-app/dev-tools`.

## Notes

- Keep reverse-start ordering, and keep `stop` receiving a valid context after cancellation
  (`registry.rs:1097-1102`).
- Startup ownership is unchanged: only the directly spawned child is killed and reaped
  (`startup.rs:94-97`). Its descendants are not tracked.
- Module callback ownership and invalidation metadata must be isolated before bounded
  registry stops are claimed; a fake registry alone cannot prove that isolation.
