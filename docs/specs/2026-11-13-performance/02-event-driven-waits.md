---
status: draft
---

# 02 — Host, config watcher, Tiling reservations and Keep Awake wait instead of poll

## Scope

Four loops that wake on a fixed period only to notice a stop, and one safety probe, become waits. Decisions D3, D4 and D5 of the [overview](00-overview.md).

Files expected to change: `src/crates/sdk/src/config/watcher.rs`, `src/crates/sdk/Cargo.toml`, `Cargo.lock`, `src/crates/app/src/host.rs` (the host worker loop and `start_watcher`), `src/crates/modules/tiling/src/reservations.rs`, `src/crates/modules/keep-awake/src/runtime.rs`, and the docs below.

## Requirements

- **Config watcher backend.** `ConfigWatcher` uses `notify`'s recommended watcher directly; `notify-debouncer-full` leaves the workspace manifests and the lockfile. The watcher still observes the directory non-recursively, still re-anchors to the nearest existing ancestor and still reports the same `ChangeSet` for the same file activity.
- **Debounce.** A batch is reported once the watched directory has been quiet for `DEBOUNCE` (200 ms) after the last event. Events that keep arriving cannot postpone a report by more than 2 s. No thread ticks while nothing changes.
- **Blocking wait.** `ConfigWatcher` gains a wait that returns on a change, on a stop request and on a backend failure, and otherwise sleeps without a deadline. `next(timeout)` keeps its contract for existing callers and tests. The stop request is a handle the owner can call from another thread; it performs no native call.
- **Config thread.** The `stache-config` thread in `Runner::start_watcher` uses the blocking wait. Stopping the `WatcherJob` wakes it at once; the teardown stage that reaps the watcher keeps its budget and its record name.
- **Host worker loop.** `Runner::run` blocks on its command channel. Every way a teardown can be requested ends the loop without a timed poll: check each caller of the teardown coordinator's request (the `HostHandle` methods, the signal bridge, the AppKit terminate path, the Tauri exit event) and make sure each delivers a wake. Queued non-teardown work is still dropped once a shutdown is requested.
- **Tiling reservation watcher.** `ReservationWatcher` waits for a change with no deadline. The call already takes the cancellation token, so `stop` wakes it. `wait_primed` keeps its bounded wait.
- **Keep Awake.** The worker's scheduled pass runs every 30 s while a `NativeObservation` is installed (`notificationsAndPolling`) and every 2 s without one (`pollingFallback`). Hints, the probe, the published `observation` values and every transition are unchanged.
- **Docs.** `docs/architecture/config.md` (watcher backend, debounce, the blocking wait), `docs/architecture/overview.md` (the host worker blocks), `docs/features/keep-awake.md` (the two intervals and what a dropped notification now costs), `docs/guides/performance.md` (the idle wakeups this removes).

## Acceptance criteria

- [ ] PF-02-01: A burst of writes to one config file within 200 ms yields one batch; a file saved through a temporary file and a rename yields a batch naming that file; a directory that disappears and returns yields `rescan`. The existing watcher tests pass unchanged in meaning.
- [ ] PF-02-02: Events arriving every 50 ms for 5 s yield a batch no later than 2 s after the first event, and further batches after that.
- [ ] PF-02-03: With no file activity for 500 ms the watcher completes no wait pass, and a stop request ends a blocked wait within 100 ms.
- [ ] PF-02-04: The host worker loop completes no pass while no command arrives for 500 ms, and a teardown requested through each entry point ends the loop within 100 ms.
- [ ] PF-02-05: The reservation watcher makes one `wait_for_change` call that stays outstanding while nothing changes; a change is delivered; `stop` joins it within its hop.
- [ ] PF-02-06: On the fake clock, Keep Awake with notifications installed runs no scheduled pass before 30 s and one at 30 s; without notifications it runs one every 2 s; a hint runs a pass at once in both modes.
- [ ] PF-02-07: `notify-debouncer-full` is absent from every manifest and from `Cargo.lock`.
- [ ] PF-02-08: `bun run lint`, `bun run format:check` and `bun run test` pass.

## Tests

- Watcher tests on a temporary directory for PF-02-01 to PF-02-03. The debounce bound uses real file events, so it asserts ordering and an upper bound with slack, not exact times.
- Host tests for PF-02-04 using the existing host fixtures and a test-only pass counter.
- A reservation watcher test with a fake `ReservationSource` that counts calls.
- Keep Awake tests on the manual clock for both modes.
- One host scenario: start the host with a config directory, edit `tiling.toml`, see the reload applied, request teardown, see a `Clean` outcome. This is the end-to-end proof that the blocking waits still deliver changes and still stop.

## Notes

- The `notify` crate's macOS backend runs its own FSEvents thread, which blocks in a run loop. That thread is not a timed wake.
- The 30 s Keep Awake interval is decision D5. If lock notifications prove unreliable on the user's machine the interval is one constant.
- For the user to check: editing a config file still reloads within about a quarter of a second; quitting from the tray still ends cleanly; Keep Awake still releases on lock and returns on unlock.
