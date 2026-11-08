---
status: done
---

# 03 — Socket and CLI while gated

## Scope

- `src/crates/sdk/src/ipc/protocol.rs` (`CoreStatus`, `CoreReload`, new `CoreAccess`), `src/crates/sdk/src/ipc/router.rs` docs.
- `src/crates/app/src/host.rs`: `HostCore` and a wrapper around every factory-contributed `IpcHandler`.
- `src/crates/cli` (`reload`, `config path` output) and its tests.
- [ipc.md](../../architecture/ipc.md).

Depends on part 02 for the gate state. Applies the confirmed answers to Resolved questions 3 and 4.

## Requirements

1. The host shares its gate state with the socket through a lock-free cell (for example an `Arc<AtomicU8>` written by the runner on each transition). Handlers read it without taking the host worker, a module lock or the registry lifecycle lock.
2. Every factory-contributed module handler is wrapped before router registration. While the state is not `open` (including `checking`) the wrapper answers the typed error kind `unavailable` with the message `Stache Next is waiting for Accessibility permission. Grant it in System Settings > Privacy & Security > Accessibility.` and never calls the inner handler. When open it delegates unchanged.
3. `CoreStatus` and `CoreReload` gain `access: Option<CoreAccess>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`; `CoreAccess { state: "checking" | "blocked" | "open", revoked: bool }`. `ConfigCore` returns `None`; `HostCore` fills it. Older clients ignore the field; a newer client treats `None` as "not reported".
4. `core.reload` and `core.status` keep working while gated: reload applies and stages files exactly as today (modules keep waiting) and still asks the host to publish once.
5. `stache reload`: unchanged output and exit code, plus, when `access.state` is `blocked` or `checking`, one stderr line `stache: modules are waiting for Accessibility permission` after any diagnostics. Exit 0 if no file was rejected.
6. `stache config path` prints the same notice with the other host diagnostics on stderr.
7. Contributed CLI commands that call a module method print the `unavailable` message through their existing typed-refusal path. `stache wallpaper set` stays a refusal, never a standalone fallback: only `NotRunning` permits that, unchanged.
8. Commands that never contact the app (`config init|migrate`, `cache`, `schema`, `completions`, `audio list` and any other contributed command that works locally) are unaffected.

## Acceptance criteria

- [ ] AG-03-01 Router test: a wrapped handler with the cell at `blocked` and at `checking` returns `unavailable` with the exact message and a call counter of zero; at `open` it delegates.
- [ ] AG-03-02 `keepAwake.status`, `proxyAudio.list` and `wallpapers.set` all answer `unavailable` through a real router over a blocked cell.
- [ ] AG-03-03 `core.status` and `core.reload` include `access` from `HostCore`, omit it from `ConfigCore`, and round-trip through serde; JSON without `access` still decodes.
- [ ] AG-03-04 CLI `reload` against a fake server reporting `blocked` prints the notice on stderr and exits 0; with a rejected file it exits 1 and prints both.
- [ ] AG-03-05 CLI `wallpaper set` against a fake server answering `unavailable` prints the gate message, applies nothing locally and exits nonzero.
- [ ] AG-03-06 The wrapper adds no lock: a test holds the registry lifecycle lock and the host worker busy while a gated request still answers within the handler's timeout.

## Tests

SDK protocol serde tests; router tests in `host.rs` with the factory handlers over fake backends; CLI tests with the existing fake socket server fixtures. No test starts the desktop.

## Manual verification (approval needed for each run)

- AG-03-M01 With the gate blocked (as in AG-02-M01), run `stache reload` and `stache wallpaper set --random` from a standalone CLI build: the notice and refusal appear, no wallpaper changes.
