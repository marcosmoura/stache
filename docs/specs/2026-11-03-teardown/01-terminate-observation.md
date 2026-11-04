---
status: done
---

# 01 — Terminate observation and one coordinator

Read when closing the AppKit terminate bypass or changing how stop requests are
received. Depends on nothing; [Part 02](02-bounded-teardown.md) depends on this.
Facts and budgets are in the [overview](00-overview.md#shipped-state).

## Scope

Observe AppKit's terminate decision and move the coordinator off the host worker so
one budget governs every [entry point](00-overview.md#entry-points). Add escalation, a
typed outcome and the restart request. Teardown stages stay as they are until Part 02.

## Requirements

### Terminate observation (`stache-macos`)

- Run the proof first: before other work, `TEARDOWN-01-01` and `TEARDOWN-01-02` must
  pass. If either fails, stop and re-plan with the user; no fallback ships.
- Candidate mechanism, unproven: add `applicationShouldTerminate:` to the live delegate's
  class (`TaoAppDelegateParent`, `tao/.../app_delegate.rs:47-50`) with `class_addMethod`
  (`objc2/src/ffi/class.rs:78`), on main, beside `ActivationObserver::install`
  (`lib.rs:254`), which runs before `run_return`. tao sets the delegate during event-loop
  creation (`tao/.../event_loop.rs:178`). AppKit may cache delegate capabilities at
  `setDelegate:`, so the probe must exercise a real `terminate:`.
- Bindings exist: `NSApplicationTerminateReply` and
  `replyToApplicationShouldTerminate` (`objc2-app-kit/src/generated/NSApplication.rs:783,1059`).
  All unsafe code stays in `stache-macos`; `stache-app` keeps `#![forbid(unsafe_code)]`.
- The callback contains panics, holds no policy, does no teardown and never waits. It
  records a plain request and returns `TerminateLater`.
- Installation failure, including an existing implementation after a tao upgrade, is a
  typed error. The host logs it at error level and keeps it as a persistent status
  condition (`host.rs:586-589`); it never silently continues as if covered.
- `RunEvent::Exit` with no recorded outcome writes one bypass record at error level, so
  an unobserved path stays visible in the log.

### Coordinator (`src/crates/app/src/shutdown.rs`)

- One coordinator state with its own deadline, driven by a dedicated thread that exists
  before any request can arrive. It must reach a terminal outcome by the deadline even
  when the host worker is blocked in a command, in `start` or in teardown.
  Track `host-worker` in-flight command quiescence independently of that worker from
  this part onward; its full contract is in [Part 03](03-owner-acknowledgements.md#todays-owners).
- Requests never block and are safe from main, signal, socket and tray threads. The first
  sets the cause and the 5 s deadline. A later one applies
  `min(current deadline, now + 250 ms)` and wakes every wait, so a third request cannot
  extend the deadline. Requests after the terminal outcome are ignored.
- A request before the host worker starts still produces one outcome through the existing
  early path (`host.rs:651-654`).
- `HostHandle::request_shutdown` keeps its signature and callers: tray, signal bridge and
  `ExitAction::PreventAndShutdown`. Last-window `code: None` remains `PreventOnly`.
- `Some(code)` keeps its first non-zero code, which `PreventAndShutdown` discards today.

### AppKit reply

- At most one reply is pending. A repeat `applicationShouldTerminate:` escalates and
  returns `TerminateLater` without adding a reply. After the terminal outcome it returns
  `TerminateNow`.
- With a reply pending, the terminal action is `reply(true)` on main, never `app.exit`.
  Teardown never replies `false`.
- The reply is guarded so a late or duplicate dispatch cannot reach AppKit twice. Part 02
  extends this guard to the
  [forced self-exit](02-bounded-teardown.md#final-exit-on-a-stalled-main-thread).

### Typed outcome

- Planned type: `TeardownOutcome::{Clean, Partial, Uncertain}` plus the cause, elapsed
  time, whether escalated, and failed and unacknowledged owner names in stable order.
  - `Clean`: every owner acknowledged success.
  - `Partial`: every owner answered and at least one reported failure.
  - `Uncertain`: at least one owner was abandoned at its bound or had not answered at
    the deadline. A late answer never upgrades the outcome.
- An escalated run whose owners all answer before the shortened deadline keeps the result
  those answers prove, with `escalated=true`.
- Exactly one outcome per process, stored before terminal dispatch in an in-memory slot
  readable by tests and the harness. Publication never waits on a reader or on host,
  module or sink locks. Its output form is one line:
  `teardown: outcome=… cause=… elapsed_ms=… escalated=… recorded_code=… failed=[…] unacknowledged=[…]`.
  Only terminal-delivery/forced-exit evidence may follow it
  ([Part 02](02-bounded-teardown.md#final-exit-on-a-stalled-main-thread)); it never creates
  a second owner outcome.
- Use an injected bounded, non-blocking teardown record channel, not a synchronous
  `Logger::log` call: `StderrLogger::log` holds the stderr lock across blocking `writeln!`
  (`logging.rs:25-34`). Each record gets one write attempt with a hard 50 ms bound,
  capped by time remaining before the applicable terminal deadline. Reserve this time
  inside the budget, never add it after the deadline, and hold no output lock across
  dispatch or a terminal action. A blocked or closed sink, including failure to acquire
  its lock, yields `unavailable` delivery in the slot, no retry and no claim of delivery.
  The outcome still reaches the slot and termination still proceeds. Tests and the harness
  can inspect the slot before exit even if no output survives process death.
- Recorded code on runtime paths: `Clean` preserves the requested code (default 0, or 1
  for a host setup failure, `host.rs:655-661`); `Partial` and `Uncertain` record 1.
  `finish_result` reports any non-zero intended code as an error (`lib.rs:298-308`), and
  unchanged `dispatch` maps every desktop error to OS status 1 (`lib.rs:401-405`). Thus
  `exit(7)` records `recorded_code=7` but ordinary runtime return exits 1, not 7.
  This spec does not change `dispatch`; the record carries the intended code, never
  claims that it is the observed OS status.
- On the AppKit path `finish_result` never runs, so the status cannot carry the outcome.
  The stored outcome (or its delivered record) is authoritative there; Part 04 records
  the observed OS status separately.

### Restart

- One planned request, `HostHandle::request_restart`, runs the same coordinator with a
  `Restart` cause.
- Relaunch only after `Clean`. A restart that ends `Partial` or `Uncertain` finishes and
  logs with no relaunch: it writes its outcome record with `cause=restart`, and its
  terminal action is the ordinary exit with status 1. Relaunching would hide a stalled
  owner behind a healthy-looking restart.
- This can leave no app running. This spec adds no UI for it; the future release-only
  Reload item is responsible for surfacing it.
- A pending AppKit reply wins: logout never relaunches.
- Use `AppHandle::request_restart`. `AppHandle::restart` sleeps forever off main and skips
  the exit events on main (`tauri/src/app.rs:606-630`).
- Tauri relaunches inside the `RunEvent::Exit` callback (`app.rs` `RuntimeRunEvent::Exit`)
  through `/usr/bin/open -n` or a direct spawn, then exits (`tauri/src/process.rs:74-154`),
  while `run_with` still holds `InstanceLock` (`lib.rs:277`). A new process that loses
  that race returns `Ok(())` silently (`lib.rs:160`). Release the lock before relaunch.

## Acceptance criteria

- [ ] `TEARDOWN-01-01` A real `terminate:` in the owned harness process invokes the added
      selector before termination. Gated manual check: opt-in `stache-smoke` terminate
      scenario, approved for that run.
- [ ] `TEARDOWN-01-02` With a reply pending, a main-queue job and a tao proxy event both
      run and `reply(true)` completes termination. Gated manual check: same scenario.
- [ ] `TEARDOWN-01-03` Installing on a test-declared class succeeds once; a second install
      or an existing selector returns the typed error. Automated `stache-macos` test,
      without `NSApplication`.
- [ ] `TEARDOWN-01-04` An installation error becomes a persistent status condition, and an
      exit event with no outcome yields one bypass record. Automated host tests.
- [ ] `TEARDOWN-01-05` At most one `true` reply and no `false` across normal finish,
      deadline expiry and repeat terminate; exactly one when delivery is permitted by
      Part 02's claim/deadline rules, none when forced exit wins the claim. A
      post-outcome terminate returns `TerminateNow`. Fake-clock unit tests.
- [ ] `TEARDOWN-01-06` A later request sets the deadline to 250 ms from itself, never past
      the total (including at 4.9 s), and a third request changes nothing. Fake-clock unit
      test with an injected signal call.
- [ ] `TEARDOWN-01-07` With a host command blocked, the outcome is `Uncertain` at the
      deadline naming `host-worker`, never `Clean`, and the terminal action is dispatched
      independently of the command. Fake-clock host test with a blocked configuration commit.
- [ ] `TEARDOWN-01-08` Concurrent requests from tray, signal, terminate, exit and restart
      yield one run and one outcome. Real-thread unit test with fakes.
- [ ] `TEARDOWN-01-09` `exit_request_action` keeps its decision table, last-window close
      starts no teardown, and a non-zero `Some(code)` reaches `finish_result`. Automated
      `shutdown.rs` and host tests; `exit(7)` records 7 while unchanged `dispatch` returns
      OS status 1 on ordinary runtime return.
- [ ] `TEARDOWN-01-10` One outcome record per run, with stable field and owner order and the
      code mapping above, stored before dispatch. Automated test through a recording
      teardown channel and the in-memory slot.
- [ ] `TEARDOWN-01-11` After a `Clean` restart the fake relaunch fires once, after lock
      release. It never fires with a reply pending. A restart ending `Partial` or `Uncertain`
      never relaunches and gives one record with `cause=restart` and status 1. Automated unit
      test.

## Tests

Injected clock, fake runtime driver, recording teardown channel and readable outcome slot.
Default tests never call
`terminate:`, send a real signal or relaunch.
`cargo nextest run -p stache-app -p stache-macos --features stache-app/dev-tools`.

## Notes

- The default menu stays unchanged. Its Quit already sends `terminate:`, so observation
  covers it, and rerouting only the menu would leave AppleScript and logout uncovered.
- `NSTerminateLater` runs the run loop in `NSModalPanelRunLoopMode`
  ([Apple](https://developer.apple.com/documentation/appkit/nsapplication/terminatereply/terminatelater)).
  tao's observers, timer and proxy source are on common modes
  (`tao/.../observer.rs:222,280`, `event_loop.rs:336`), but the combined behavior on
  macOS 26 is what `TEARDOWN-01-02` proves.
