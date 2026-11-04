---
branch: feat/teardown
---

# Coordinated application teardown

Read when closing the Cmd-Q/logout teardown gap recorded in the
[roadmap](../../roadmap.md). **PLANNED, NOT SHIPPED.** Every part is `draft`; every
type or function this spec introduces is a planned name, not a shipped API.
Base: `main` at `ad34b46`.

## Goal

Every way the desktop process can be asked to stop converges on one idempotent,
budgeted coordinator that finishes in bounded time and reports a truthful outcome.

## Non-goals

- No new tray item, menu change, settings surface, alert, window or dialog. The
  release-only tray "Reload Stache" item is a later follow-up; this spec provides only
  the coordinated restart path it will call. That item is responsible for surfacing that
  a restart that does not end `Clean` leaves no app running.
- No new module API beyond a registered invalidation handle; module `stop` keeps its
  semantics and gains a bound.
- No Keep Awake, Keybindings, Hold-to-Quit, Bar or Tiling behavior. Part 03 fixes the
  contract those owners adopt; their internals stay in their own specs.
- No new termination of any other process and no termination by executable name. The one
  forced exit in this spec targets the current process only
  ([Part 02](02-bounded-teardown.md#final-exit-on-a-stalled-main-thread)).
- No change to normal startup, readiness or configuration reload; lifecycle calls during
  shutdown gain a typed refusal.

## Shipped state

Dependency versions are the `Cargo.lock` pins: `tauri 2.12.4`, `tauri-runtime-wry
2.12.3`, `tao 0.37.2`, `muda 0.20.0`.

- `RunEvent::ExitRequested` has two producers: last-window `Destroyed` with
  `code: None` (`tauri-runtime-wry/src/lib.rs:4262`) and `Message::RequestExit` with
  `code: Some` (`:4302`), sent by `AppHandle::exit`, `restart` and `request_restart`
  (`tauri/src/app.rs:582,619,639`).
- tao's delegate class registers `applicationWillTerminate:` and no
  `applicationShouldTerminate:` (`tao/src/platform_impl/macos/app_delegate.rs:47-85`).
  That handler reaches `Event::LoopDestroyed` and `RunEvent::Exit`
  (`app_state.rs:271-273`, `tauri-runtime-wry/src/lib.rs:4131`), and no `ExitRequested`
  is emitted. The app ignores `RunEvent::Exit` (`src/crates/app/src/lib.rs:273`).
- AppKit never returns control to `main()` after
  [`terminate:`](<https://developer.apple.com/documentation/appkit/nsapplication/terminate(_:)>),
  so `run_return` does not return and `finish_result`, `drop(activation)` and
  `drop(instance)` are skipped (`lib.rs:275-278`).
- The app supplies no menu (`lib.rs:177-179`), so Tauri installs its default
  (`tauri/src/app.rs:2410`) with `PredefinedMenuItem::quit` (`menu/menu.rs:194`), whose
  action is `terminate:` (`muda/src/platform_impl/macos/mod.rs:944`).
- Three paths reach the coordinator today: tray Quit (`tray.rs:295`), the signal bridge
  (`lib.rs:233`) and `AppHandle::exit` with a code (`lib.rs:262-265`). The only latch is
  one `AtomicBool` swap (`host.rs:467-471`).
- The coordinator is the host worker itself: `Runner::run` reaches `teardown`
  (`host.rs:1008-1031`) only after its current command returns, so a blocked command
  delays it without limit.
- Every teardown wait is unbounded: `IpcServer::shutdown` (`sdk/src/ipc/server.rs:249`,
  handlers at `:650`), watcher (`host.rs:603`), ticker (`permissions.rs:285`), startup
  job (`startup.rs:66`, child at `:96`), `Registry::shutdown` holding its lifecycle lock
  through every `stop` (`sdk/src/registry.rs:958,1102`), module workers
  (`sdk/src/threading.rs:175`) and the signal thread (`shutdown.rs:116`).
- Failures are logged or discarded, never reported: registry errors become a warning and
  exit 0 (`host.rs:708-709,1025-1027`), and socket removal and window close results are
  dropped (`server.rs:320`, `windows.rs:127`).
- Tray removal and the final `app.exit` share one unacknowledged main-queue closure
  (`lib.rs:82-91`). Its error fallback is unreachable in production: `run_on_main`
  cannot fail (`macos/src/main_thread.rs:41-44`), so a stalled main thread means no exit.
- `Cargo.toml:43` sets `panic = "abort"` for release only. `AppHandle::restart` and
  `request_restart` have no caller in `src/crates`.

## Entry points

| Entry point                                | Route today                    | Route when shipped (owning part)                                      |
| ------------------------------------------ | ------------------------------ | --------------------------------------------------------------------- |
| Tray Quit                                  | Coordinator                    | Coordinator ([01](01-terminate-observation.md))                       |
| Default-menu Quit, Cmd-Q                   | `terminate:`, bypasses         | Terminate observation; menu unchanged (01)                            |
| AppleScript or another app's quit event    | `terminate:` expected, bypass  | Terminate observation; external route requires supplementary evidence |
| `open`                                     | Has no quit action; see note   | No change                                                             |
| Logout, restart, shutdown                  | `terminate:` expected, bypass  | Terminate observation, delayed never bypassed (01)                    |
| SIGINT, SIGTERM, SIGHUP                    | Coordinator after `lib.rs:233` | Coordinator; repeat escalates (01)                                    |
| `AppHandle::exit(code)`                    | Coordinator; code discarded    | Coordinator; code kept (01)                                           |
| Coordinated restart                        | Does not exist                 | Coordinator, then relaunch only after `Clean` (01)                    |
| Last window closed (`code: None`)          | Not a stop request             | No change                                                             |
| Re-entrant or repeated request             | Latch no-op                    | Tightens the deadline only (01)                                       |
| Panic, abort, SIGKILL, Force Quit, power   | Process dies                   | Unrecoverable; documented, not intercepted                            |
| Signal before `lib.rs:233`, or failed hook | Default disposition            | Unchanged limit                                                       |

`open` launches or reactivates; `open -n` starts a second process that stops at the
instance lock (`lib.rs:160`) and leaves the first running.

Under `panic = "abort"` there is no unwinding and no destructor; a panic hook would
still run, but `src/crates` installs none, so nothing cleans up. Dev and test profiles
unwind, where a panicking `stache-host` thread leaves the app running without teardown.
For the unrecoverable and early-signal rows the kernel releases the instance lock
(`sdk/src/instance.rs:9-10`) and the next start removes a stale socket
([ipc.md](../../architecture/ipc.md#endpoint-safety)).

## Decisions

1. **Budgets are approved values.** See [Budgets](#budgets). The total starts at the
   first request and never grows.
2. **A second request shortens.** Repeat Cmd-Q, a signal after Cmd-Q or a Quit click
   during teardown sets the deadline to 250 ms from that request, capped by the total.
   Owners still outstanding then finish `Uncertain`. Nothing is killed.
3. **Logout is delayed, not bypassed.** Answer `NSTerminateLater`, run the same
   coordinator, reply `YES` at most once. Reply delivery, not dispatch, must finish by the
   total deadline except for the claimed-reply case in decision 10. Teardown
   never answers `NO` or `NSTerminateCancel`.
4. **Honest and silent.** A timeout or missing acknowledgement yields one typed outcome
   in a test/harness-readable in-memory slot, with no UI. Output gets one non-blocking
   attempt bounded by 50 ms within the existing deadline; unavailable output never
   blocks termination or counts as delivered ([Part 01](01-terminate-observation.md#typed-outcome)).
5. **Invalidation is not cleanup.** Logical invalidation is immediate and needs no
   owner; physical cleanup is acknowledged by its owner or reported missing. Queued work
   can be dropped; an OS action already issued is recorded, not retried or called
   cancelled.
6. **Fixed order.** Stop accepting work and invalidate dispatch before any join, then
   stop the registry in reverse order, then run acknowledged native cleanup, retaining
   inert ownership and diagnostics when cleanup fails or times out.
7. **The main thread never waits.** No main→worker→main wait and no main-thread wait on
   a worker that needs main. Off main, `MainThread::execute` enqueues and returns; on
   main it runs inline (`macos/src/main_thread.rs:77-84`), so coordinator code never
   runs on main.
8. **The selector is a gate.** If `applicationShouldTerminate:` cannot be observed,
   Part 01 fails loudly and the spec is re-planned. Menu-only routing is not an
   acceptable downgrade.
9. **Owners adopt one contract.** Future native owners implement Part 03; they add no
   shutdown path of their own.
10. **A wedged main thread ends in a forced self-exit.** An unclaimed terminal action
    forces self-exit at the deadline. Only an AppKit reply claimed on main before the
    deadline but not yet completed gets one additional 2 s delivery hop. A stalled main
    thread that already started that reply can therefore extend the overall bound from
    5 s to 7 s; terminal evidence marks the late reply and extended bound. Completion
    within the hop cancels forced exit; otherwise self-exit occurs at its end
    ([rules](02-bounded-teardown.md#final-exit-on-a-stalled-main-thread)).
11. **Relaunch only after `Clean`.** `Partial` and `Uncertain` finish and log without
    relaunching ([rules](01-terminate-observation.md#restart)).

## Budgets

| Stage                         | Budget                                                                      |
| ----------------------------- | --------------------------------------------------------------------------- |
| Total, from the first request | 5 s                                                                         |
| After a later request         | `min(current deadline, request time + 250 ms)`                              |
| One native hop or owner wait  | `min(2 s, remaining)`                                                       |
| Cancellation observation      | 10 ms quantum, for waits that cannot block on a wake                        |
| AppKit reply                  | Delivered by deadline; only a pre-deadline claim permits one extra 2 s hop  |
| Forced self-exit check        | At deadline if unclaimed; at deadline + 2 s for an incomplete claimed reply |
| Teardown record output        | One non-blocking attempt, at most 50 ms within the applicable deadline      |

`stache_macos::ACKNOWLEDGEMENT_BUDGET` and `CANCELLATION_QUANTUM` already hold the 2 s
and 10 ms values (`macos/src/applications.rs:98,106`); they remain per-hop values, and
only this spec adds a total.

## Blocking gates

Nothing here is claimed as working until its check passes.

| Unproven                                                                         | Evidence         |
| -------------------------------------------------------------------------------- | ---------------- |
| AppKit calls a selector added to tao's live delegate class                       | `TEARDOWN-01-01` |
| Main queue and tao proxy run during `NSModalPanelRunLoopMode`                    | `TEARDOWN-01-02` |
| Cmd-Q reaches `terminate:` under the Accessory activation policy                 | `TEARDOWN-04-03` |
| AppKit behavior for a repeat quit while a reply is pending                       | `TEARDOWN-04-04` |
| Owned `NSApp` quit reaches the observation; external event is supplementary only | `TEARDOWN-04-05` |
| Identifier-addressed evidence has no other running instance                      | `TEARDOWN-04-09` |
| Logout is delayed in a disposable session; sudden termination is off             | `TEARDOWN-04-06` |
| Relaunched process wins the instance lock                                        | `TEARDOWN-04-08` |

System restart and shutdown have no required gate. Shipped docs may claim them only if
an approved run of the `TEARDOWN-04-06` procedure covers them. Logout, restart and shutdown
require a disposable login session or spare machine and evidence collected outside that
session. Without one, logout is not run and Part 04 stays blocked. Cmd-Q non-delivery
fails its gate and requires re-planning with the owner; other routes cannot substitute.

## Parts

| Part                               | Summary                                                       | Status      |
| ---------------------------------- | ------------------------------------------------------------- | ----------- |
| [01](01-terminate-observation.md)  | Terminate observation, one coordinator, budget, typed outcome | in-progress |
| [02](02-bounded-teardown.md)       | Ordered stages, per-hop budgets, abandonment, exit gate       | draft       |
| [03](03-owner-acknowledgements.md) | Real acknowledgements for today's owners; future contract     | draft       |
| [04](04-verification-gates.md)     | Real-thread scenarios, harness policy, per-run manual gates   | draft       |

Parts run in order. Each must independently build, pass `bun run lint`,
`bun run format:check` and `bun run test`, and commit cleanly. After Part 01 the existing
teardown still runs unbounded behind a bounded coordinator; Parts 02 and 03 refine it.
Part 01 establishes the bounded record channel and `host-worker` tracking; Part 02
adds lock-independent module invalidation and abandonable registry stops. Part 03
completes the owner acknowledgements; Part 04 verifies their integration.

## Affected docs

No shipped doc changes while this is unshipped. When parts land:

- [overview.md](../../architecture/overview.md): the convergence paragraph under
  "Desktop host".
- [ipc.md](../../architecture/ipc.md): "Desktop lifecycle" and the shutdown wording under
  "Endpoint safety".
- [testing.md](../../guides/testing.md): harness scenarios and manual gates.
- [notunes.md](../../features/notunes.md): remove the Cmd-Q/logout out-of-scope note.
- [roadmap.md](../../roadmap.md): drop the follow-up section when Part 04 is done.
