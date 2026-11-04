---
status: done
---

# 03 — Owner acknowledgements

Read when a native owner gains a resource that must be released on exit, or when changing
how cleanup completion is reported. Depends on [Part 02](02-bounded-teardown.md).

## Scope

Define what "acknowledged" means for each owner that exists today, implement it, and fix
the contract future owners must meet so `Clean` is reported only when it is proven.

## Requirements

### Today's owners

| Owner               | Thread | Acknowledged when                                        | Today                                                                             |
| ------------------- | ------ | -------------------------------------------------------- | --------------------------------------------------------------------------------- |
| `host-worker`       | Worker | In-flight command quiesced; no command can start         | Coordinator waits for current command, `host.rs:650-709`; config commit at `:846` |
| `watcher`           | Worker | Thread finished                                          | Unbounded join, `host.rs:603`                                                     |
| `permission-ticker` | Worker | Thread finished                                          | Unbounded join, `permissions.rs:285`                                              |
| `startup-job`       | Worker | Thread finished, after killing and reaping its own child | Blocking wait, `startup.rs:96`                                                    |
| `module:<id>`       | Worker | `stop` returned `Ok` and its workers finished            | Error logged only, `host.rs:1025`                                                 |
| `socket`            | Worker | Acceptor and handlers finished; owned inode gone         | Removal result dropped, `sdk/src/ipc/server.rs:320`                               |
| `activation`        | Main   | Observer removed on main                                 | `Drop` skipped on the AppKit path, `lib.rs:276`                                   |
| `tray`              | Main   | Removal closure ran and the tray is gone                 | Unacknowledged closure, `lib.rs:85`                                               |
| `windows`           | Main   | `Destroyed` observed for every label closed              | Result dropped and bookkeeping cleared, `windows.rs:127,413`                      |

Rules for these owners:

- `host-worker`: Part 01 tracks command admission and quiescence outside the worker.
  Stage 1 closes admission and discards queued commands; an already-running command
  gets `min(2 s, remaining)` to quiesce, observed alongside monitor reaping without
  preventing registry invalidation or stops. Abandon it at that bound and name
  `host-worker` unacknowledged. `Clean` is prohibited while any command is in flight,
  including a configuration commit outside a module. Late completion cannot publish
  host state, issue follow-on native work, restart a module or relaunch, and cannot
  change the outcome. A configuration write already committed is not rolled back
  or called cancelled. No second teardown begins when the abandoned worker returns.
- `socket`: a foreign inode at the path is success, because the owned endpoint is gone
  ([endpoint rules](../../architecture/ipc.md#endpoint-safety)).
- `activation`: the guard stays `!Send` and main-owned. Hold it where a main-thread closure
  can remove it (`macos/src/activation.rs:66-74`); never move it to a worker.
- `windows`: `close_all` clears its maps before anything closes (`windows.rs:410-416`). Keep a
  pending label set that `window_closed` (`lib.rs:268-272`) drains. A window already gone
  counts as closed.
- `module:<id>`: native state owned by a module, such as the noTunes observer, is reported
  through its `stop` result (`modules/notunes/src/runtime.rs:384-399`). The coordinator
  adds no second cleanup path into it; Part 02's registered handle provides only the
  earlier lock-independent logical invalidation.

### Not owners

- **Instance lock.** Dropping a `File` has no failure to report, and the kernel releases it at
  exit (`sdk/src/instance.rs:9-10,74-76`). It gets no acknowledgement; only relaunch ordering
  applies to it (Part 01).
- **Signal bridge.** It stays open until the outcome is recorded so a repeat signal can
  escalate; it closes after that (Part 02) and cannot appear in the record.
- **Terminal action.** The reply or `app.exit` is not an owner (Part 01). A forced exit has its
  own terminal evidence, including late delivery and any extended bound
  ([Part 02](02-bounded-teardown.md#final-exit-on-a-stalled-main-thread)), and adds no owner entry.

### No invented acknowledgements

- A dispatch is not an acknowledgement. Off main, `MainThread::execute` only enqueues
  (`macos/src/main_thread.rs:77-84`), and an owner counts only when its own closure reports
  completion.
- The tray closure splits from the final `app.exit` (`lib.rs:84-87`).
- After one main-thread hop misses its budget, remaining main-thread owners are marked
  unacknowledged without new 2 s waits, because the same stalled queue would spend the total.
  Their closures stay queued.
- A closure that runs after the outcome is inert: it cannot change the outcome, and its late
  answer is ignored. It may still release what it owns, on main.
- When an acknowledgement is missing, ownership stays where it is. Nothing main-owned is dropped
  from a worker, and nothing is force-released to improve the outcome.
- No blanket `unsafe impl Send` or `Sync` moves an owner across threads.

### Future owners

An owner added by a later spec must:

- register, in the same change, into its native-cleanup slot: power assertion, input tap,
  application observer, or HUD/window ([order](02-bounded-teardown.md#stage-order));
- provide a stable diagnostic name, a release that starts without blocking its caller, and one
  completion answer: success, failure or none;
- be logically inert after invalidation, before physical cleanup is acknowledged;
- keep native work in `stache-macos`, and on main when the API requires it;
- add no second shutdown path, latch, `process::exit` or terminate observer.

Keep Awake (power assertion), Keybindings (input tap) and Hold-to-Quit (HUD) adopt this; none
exists on this branch.

## Acceptance criteria

- [ ] `TEARDOWN-03-01` Watcher, ticker and startup job each report success when finished and
      appear as unacknowledged when hung. The startup child is killed only by its own job.
      Automated fake and `startup.rs` tests.
- [ ] `TEARDOWN-03-02` Every module that fails or hangs is named in stable order, and one failure
      does not mask another. Automated fake-registry test.
- [ ] `TEARDOWN-03-03` A failed owned-inode removal is `failed=[socket]`, a foreign inode is success,
      and a hung handler is unacknowledged. Automated SDK test on a temp root.
- [ ] `TEARDOWN-03-04` Activation removal runs on main through the acknowledged hop. Automated
      recording-executor test; `ActivationObserver: !Send` is proven by a `compile_fail` doctest
      under `cargo test -p stache-macos --doc`.
- [ ] `TEARDOWN-03-05` Tray removal is a separate acknowledged hop; a never-run closure gives
      `Uncertain` naming `tray`. Automated recording-executor test.
- [ ] `TEARDOWN-03-06` The window ack waits for `window_closed` on every label; a missing event names
      `windows`, and an already-gone window counts as closed. Automated `windows.rs` test.
- [ ] `TEARDOWN-03-07` A stalled queue costs one hop budget and names all main owners. A closure run
      after the outcome neither changes it nor writes a second record. Automated fake-clock test.
- [ ] `TEARDOWN-03-08` A registered owner without an answer can never produce `Clean`, and a new owner
      in any slot runs in slot order. Automated fake-owner test.
- [ ] `TEARDOWN-03-09` A command blocked in a configuration commit is abandoned at its
      bound and named `host-worker`, even if every other owner succeeds. The outcome is
      `Uncertain`, never `Clean`, and a restart cannot relaunch. Releasing the command
      after the outcome causes no publication, native follow-on action, new teardown
      or outcome change. An idle/quiesced worker acknowledges success. Automated host
      tests extend Part 01's blocked-command check.

## Tests

Each owner fake has three behaviors: acknowledges, fails, hangs past its budget. A recording executor
proves dispatch is asynchronous and completion observed. Default tests touch no real tray, window,
user socket path or logged-in session.
`cargo nextest run -p stache-app -p stache-sdk -p stache-macos --features stache-app/dev-tools`.

## Notes

A missing acknowledgement is a normal outcome. A session that is logging out cannot always prove a
window closed, and the record says so.
