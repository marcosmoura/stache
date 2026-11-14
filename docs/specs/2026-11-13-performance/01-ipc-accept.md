---
status: done
---

# 01 — SDK: event-driven accept loop

## Scope

The control socket server stops polling. A connection is accepted as soon as it arrives and an idle server makes no timed wakeup.

Files expected to change: `src/crates/sdk/src/ipc/server.rs` and `docs/architecture/ipc.md`. Nothing else: the wire protocol, the client, the router, the limits and the endpoint ownership rules are untouched.

## Requirements

- **Wait.** `serve` keeps the listener non-blocking and, when `accept` would block, waits in `poll(2)` (`rustix::event::poll`) on the listener and on the read end of a wake pipe owned by the `Gate`, with no timeout. Decision D2 of the [overview](00-overview.md).
- **Stop.** `Gate::stop` makes the pipe readable after it set the stop flag. `stop_accepting`, `shutdown`, `shutdown_bounded` and drop therefore end the wait at once. `stop_accepting` still joins nothing and waits for nothing.
- **Pipe.** Created when the server starts, with both ends close-on-exec and the write end non-blocking, the way `commands/stream.rs` builds its wake pipe. A write to a full pipe is not an error. A failure to create or configure it fails `start_with` with `ServerError::Io { action: "configure", .. }` and leaves no endpoint behind, like a failed thread spawn.
- **Other accept errors.** An `accept` error that is not `WouldBlock` keeps the existing stop-wakeable 50 ms wait, so a persistent error cannot spin.
- **Workers.** Connection workers are reaped on every pass as today and the retained handle bound keeps holding.
- **Docs.** `docs/architecture/ipc.md` describes the wait and the wake pipe where it describes the accept loop.

## Acceptance criteria

- [x] PF-01-01: Over 40 sequential round trips against a real server, sent at uneven intervals between 3 ms and 120 ms, the median is below 10 ms. The same holds for a `core.status` request followed at once by a second request on a new connection.
- [x] PF-01-02: With no connection for 300 ms the accept loop does not complete a pass (a test-only pass counter stays unchanged).
- [x] PF-01-03: After `stop_accepting` the accept thread finishes within 100 ms without a connection arriving.
- [x] PF-01-04: Both pipe ends carry close-on-exec and the write end is non-blocking.
- [x] PF-01-05: Every existing test in `ipc::server`, `ipc::client` and `ipc::router` passes unchanged in meaning.
- [x] PF-01-06: `bun run lint`, `bun run format:check` and `bun run test` pass.

## Tests

- A latency test for PF-01-01 over a real `IpcServer` and `IpcClient` on a temporary socket. It asserts the median only, so one slow sample on a loaded machine does not fail it.
- A pass-counter test for PF-01-02 and a stop test for PF-01-03.
- A descriptor-flag test for PF-01-04.
- The existing shutdown, permit, stale-endpoint and bounded-handle tests.

## Notes

- Prototype on a scratch copy of the SDK (2026-11-13): single request median 31.4 ms to 0.68 ms, status plus action median 68 ms to 1.1 ms, p90 102 ms to 1.9 ms; 63 of 63 IPC tests passed.
- Measured on the branch (2026-11-13) with `round_trips_at_uneven_gaps_are_answered_promptly`, the same test body on both loops: with the 50 ms poll the single request median was 38.6 ms and status plus action 87.7 ms (the test fails); with the `poll(2)` wait 0.69 ms and 0.38 ms.
- A failing `poll` (anything but `EINTR`) falls back to the stop-wakeable 50 ms wait, for the same reason another `accept` error does.
- An idle loop makes no pass, so finished connection handles (at most the connection limit) are joined when the next connection or the stop arrives instead of within 50 ms.
- The Tiling CLI still opens two connections per command. That is a non-goal: after this part the second connection costs well under a millisecond.
- For the user to check: key bindings for focus, swap and workspace switch respond without the previous lag; `stache tiling query status` still answers.
