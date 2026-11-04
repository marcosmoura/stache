---
status: draft
---

# 04 — Verification and gates

Read when validating teardown before merging or adding a shutdown check. Depends on
[Parts 01–03](00-overview.md#parts). Each part owns its unit tests; this part owns the
real-thread scenarios, the harness policy and the manual gates.

## Scope

Prove the assembled coordinator on real threads with fake owners, and collect per-run evidence
that real Cmd-Q, owned AppKit quit, logout and SIGTERM take the coordinated path.
AppleScript is supplementary only.

## Requirements

### Default suite

No default test may quit an application, log out, call `terminate:`, send a signal to a real
process, relaunch, create a power assertion, suppress input, or terminate by executable name.

### Harness policy

- `stache-smoke` stays behind the `smoke` feature and its `required-features` binary
  (`src/crates/app/Cargo.toml:17-20,35`), isolated under a temp root as
  [testing.md](../../guides/testing.md#native-host-smoke-opt-in) describes.
- New scenarios act only on the harness's own PID: `terminate:` on its own `NSApp`, the existing
  SIGTERM, and a relaunch. Each scenario is selected explicitly per run and needs approval for that
  run. The default harness run stays as it is.
- `run_return` does not return on the terminate scenario, so `run_smoke`'s result
  (`lib.rs:138-147`) cannot report it. The verdict is the parent's check of the record and
  socket absence. The harness can inspect the in-memory outcome/terminal slot before
  exit independently of the record sink, including blocked-sink tests; absent output
  alone never proves an outcome.

### Manual gates

- One at a time, each approved for that individual run. Never unattended, never bundled with
  another gate.
- Target only `com.marcosmoura.stache.next`, from a `bun run tauri:build` bundle, using a temp-root
  environment with every module disabled.
- Temporary roots isolate files and instance locks, not LaunchServices addressing or
  the login session. Before any identifier-addressed gate, run the collision query
  `findRunningStacheProcesses` in `scripts/measure/process.ts` (LaunchServices plus exact
  executable/bundle process matching), excluding only the verified launched `$PID`.
  Require no other `com.marcosmoura.stache.next` instance. If any exists, or the query
  fails or cannot establish identity, abort without sending the event; never terminate
  the conflicting instance. Record the query and its result. Recheck immediately before
  the event; prevent concurrent launches for that run.
- Launch so that the PID and stderr belong to the run. Bundle execution selects desktop mode
  ([ipc.md](../../architecture/ipc.md#mode-selection)):

  ```sh
  HOME="$ROOT" TMPDIR="$ROOT" XDG_CONFIG_HOME="$ROOT/config" \
    "$BUNDLE/Contents/MacOS/stache" 2>"$LOG" &
  PID=$!
  ```

- Each gate records the command, outcome and terminal evidence (including record delivery
  availability, late reply and extended bound), socket absence, and `wait "$PID"` OS status.
  Keep `recorded_code` separate: ordinary runtime `exit(7)` records 7 but unchanged
  `dispatch` returns OS status 1; AppKit status is also a fact, not the outcome.
- The quit-event gate defaults to the harness invoking `terminate:` on its own `NSApp`.
  `osascript -e 'tell application id "com.marcosmoura.stache.next" to quit'` is optional
  supplementary external-event evidence only, after the collision guard above passes;
  it addresses an identifier, not `$PID`, and cannot replace the owned gate or Cmd-Q.
- Logout, system restart and shutdown require a disposable login session or a spare
  machine, explicitly approved for that run. Collect evidence outside the affected
  session (including logs and the observing parent), not merely outside the app's temp
  root. If neither environment is available, record the gate as **not run**: Part 04
  stays blocked, never passed. Temporary app paths do not make session termination safe.
- Evidence goes in the part's completion notes or the pull request. A failed gate blocks the part;
  it becomes a documented limitation only by your explicit decision.
- A pass covers that run. Re-run after changing terminate observation, budgets or stage order.

## Acceptance criteria

- [ ] `TEARDOWN-04-01` On real threads with fake owners and a queue-draining executor, the host
      passes: all-acknowledge `Clean`; failing owner `Partial`; hung owner `Uncertain` by the
      deadline; blocked `host-worker` despite all other acknowledgements; escalation during
      a hang; concurrent requests. Include a blocked record sink: the slot remains readable,
      the sink adds at most 50 ms before dispatch and force-exit timing stays bounded. Each yields one
      outcome and no undetected surviving thread. Automated test in `src/crates/app/tests/host.rs`.
- [ ] `TEARDOWN-04-02` The opt-in terminate scenario, with a temporary fake-hanging owner variant,
      gives one record each, no owned socket afterwards, and one reply. Gated manual check, approved
      per run.
- [ ] `TEARDOWN-04-03` With the Test Window open and frontmost, Cmd-Q gives one coordinated run, one
      `Clean` record and no owned socket. The window gives the Accessory app
      (`lib.rs:181`) a way to become active. If the key never reaches AppKit, the gate
      fails and requires explicit re-planning with the owner. App-menu or quit-event
      evidence may supplement, never satisfy or substitute for this gate. Gated manual
      check, approved per run.
- [ ] `TEARDOWN-04-04` During a hanging-owner run, a second Cmd-Q and, in its own run, SIGTERM to
      `$PID` end the run within 250 ms of the second request with `escalated=true`, `Uncertain` and
      one reply. If AppKit does not deliver the second quit while a reply is pending, record that;
      the SIGTERM result must still pass. Gated manual check, approved per run.
- [ ] `TEARDOWN-04-05` The harness's own `NSApp` `terminate:` gives one coordinated
      `Clean` outcome, one reply and no owned socket. Optional AppleScript evidence
      uses the guarded identifier-addressed procedure and proves only that supplementary
      external route. Gated manual check, approved per run.
- [ ] `TEARDOWN-04-06` Logout with the app running gives one record within budget, no owned socket and
      a logout that proceeds, and the built `Info.plist` does not enable sudden or automatic termination.
      Run only in a disposable login session or on a spare machine, with evidence
      collected outside the affected session. Without either, record not run and keep
      the part blocked. The same isolation is mandatory for any approved restart or
      shutdown evidence; those routes are not claimed without their own run. Gated
      manual check, approved per run.
- [ ] `TEARDOWN-04-07` SIGTERM to `$PID` gives one `Clean` record, no owned socket and status 0. Gated
      manual check, approved per run.
- [ ] `TEARDOWN-04-08` The opt-in relaunch scenario's successor acquires the instance lock and reaches
      readiness instead of exiting at `lib.rs:160`. Gated manual check, approved per run. It exercises
      the direct-spawn path; the bundled `open -n` path remains blocking for the Reload item's follow-up.
- [ ] `TEARDOWN-04-09` Identifier-addressed supplementary evidence includes a successful
      collision query showing only the owned PID; a foreign or unknown instance or a
      failed query aborts before any event. Automated fake-query tests prove refusal,
      and each approved identifier-addressed run records its preflight evidence. Never
      launch or signal a real conflicting instance to test this guard.

## Tests

Before each gate and before merge: `bun run lint`, `bun run format:check`, `bun run test`,
`cargo nextest run --workspace --features stache-app/dev-tools`. Run the existing failure probe,
`cargo nextest run -p stache-app --features smoke --test smoke_failure`, so setup failure still exits
non-zero.

## Notes

- Tray Quit and SIGINT keep their existing manual status; a real tray click cannot be automated from
  outside the owned process.
- Materialize [Affected docs](00-overview.md#affected-docs) in the part that ships each behavior, and
  state only what the gates proved.
