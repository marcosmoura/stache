# Foundation validation

Read when running checks or verifying tooling isolation.

Use Bun 1.4.4, Node 26, the root Rust nightly and macOS 26+ with Xcode command-line tools.
Install cargo-sort, cargo-nextest and (for audits) cargo-audit. CI uses macOS 26.

```sh
bun install --frozen-lockfile
node_modules/.bin/playwright install webkit
bun run lint
bun run format:check
bun run test
bun run test:ui:coverage
bun run build
bun run generate-schema
bun run generate-schema -- --check
bun run tauri:build
```

## CLI and socket checks

`cargo nextest run -p stache-cli -p stache-sdk` covers the standalone commands,
the wire protocol, endpoint safety and the socket limits without touching a
device. The app integration tests run the real `target/debug/stache` binary
with an injected `HOME`/`TMPDIR`: a command that initialized Tauri or the tray
would not exit, and the bounded wait fails. They also exercise piped and PTY
output (`script -q /dev/null`) and the exit codes 0/1/2.

`stache audio list` is the only command that reads real `CoreAudio`. It is
read-only: it enumerates devices, never sets a default, never registers a
listener and never starts the desktop. Tests use its fake backend for goldens
and run the real binary only through that read-only path. Never enable
`proxyAudio` for a test, a smoke run or a CLI check.

No automated check may force-terminate Apple Music/iTunes or open a replacement
player. The noTunes opt-in check runs only with approval for that run, from an
isolated temporary config/identity with any legacy noTunes disabled; see
[notunes.md](../features/notunes.md).

`cargo doc -p stache-sdk -p stache-macos -p stache-proxy-audio -p stache-notunes -p stache-cli --no-deps`
builds their public docs; pass `RUSTDOCFLAGS='-D warnings'` in validation.
Socket tests use temp directories and temp sockets only; no test reads or writes
the real cache, runtime, configuration or legacy identity.

## Automated checks

- `lint:ui`: strict typecheck of frontend, prepare script and root configs, then oxlint/stylelint.
  `lint:native`: cargo-sort check and Clippy with warnings as errors. Neither fixes source.
- `format:check`: pinned oxfmt and rustfmt checks, including active specs. `format` explicitly rewrites.
  Root oxfmt disables nested configs so legacy config cannot override archive exclusions. Generated
  artifacts (`schemas/**`, `src/crates/app/gen/**`, `src/ui/src/sdk/bindings.gen.ts`) are excluded:
  their bytes belong to the generators, so `check:generated`, not the formatter, is their gate.
- `test` runs `test:ui`, `test:scripts` and `test:native`. Bun runs commands; Vitest runs TS tests.
- `test:ui`: real Playwright WebKit through Vitest. Suites live under `src/ui/src` next to components;
  `src/ui/tests` holds setup/support only. SDK wrapper and status-feed suites mock the generated Tauri
  command/event boundary; `WindowRouter` and `TestWindow` suites prove route validation, subscription
  cleanup, event-over-snapshot precedence, empty/error states and token styles through the computed
  background. Coverage retains 80/80/65/80 thresholds.
- `test:scripts`: the Vitest Node project in `vitest.scripts.config.ts` covers the root TypeScript
  tooling under `scripts/`. The teardown collision guard uses injected fake queries only; no test
  reads the real process table, signals or launches a process.
- `test:native`: cargo-nextest across only the root workspace with
  `--features stache-app/dev-tools`; checks desktop identity/window policy, the SDK lifecycle, config
  and window suites, app host races/permissions/shutdown, the proxyAudio policy and fake-backend
  lifecycle, the noTunes blocker sweep/launch/reconfigure/stop policy, the macOS audio
  listener-rollback tests and the macOS adapter unit tests. Native tests
  never prompt for Accessibility and never change the system default audio device; the real
  main-thread/window smoke test is the opt-in `stache-smoke` harness described below.
  Generator binaries require `dev-tools` and are excluded from the release bundle.
- `cargo nextest run -p stache-sdk -p stache-cli -p stache-proxy-audio -p stache-notunes -p stache-macos`
  runs the config/audio/blocker foundation. The proxyAudio lifecycle suite uses a fake backend for
  every listener, worker-spawn, set-default and stop-race failpoint; no test starts a real listener.
  The noTunes suite uses `FakeApplicationBackend` and `ManualExecutor` for every observation, sweep,
  launch, open and stop failpoint; no test observes, terminates or launches a real application.
  `cargo doc -p stache-sdk -p stache-macos -p stache-proxy-audio -p stache-notunes -p stache-cli --no-deps`
  builds their public docs.
- `generate-schema` writes the committed `schemas/*.schema.json` from the registered types.
  `generate-schema -- --check` writes nothing and fails on missing or stale files; config templates are
  generated from `Default` and schemars descriptions, never from a hand-written copy.
  `generate-bindings -- --check` exports to a secure temporary file and compares bytes; it never
  writes the committed bindings or source.

Oxfmt supports Markdown; verify its pinned version with a disposable malformed Markdown file.
Oxlint is JS/TS linting, **not Markdown prose linting**. No semantic/prose replacement is claimed.

## Hooks and scope

Prepare installs standard Husky in a normal checkout, without sidecar setup. CI sets `HUSKY=0`.
Linked-worktree prepare explicitly **skips installation** to preserve shared Git config; it does not
fabricate shims. Existing shared hook configuration is inherited. If `.husky/_` shims are absent,
run `PATH="$PWD/node_modules/.bin:$PATH" sh .husky/pre-commit` and `sh .husky/pre-push` explicitly
from that worktree. No automatic shared configuration change is made.

Pre-commit checks active staged inputs; Rust stages run workspace format/sort/Clippy checks.
`lint-staged.config.ts` uses one function task because lint-staged globs cannot combine the
extension sets with the nested archive/build/generated exclusions, and `tsc -b` takes no filenames.
It passes literal argv to the installed tools, so names with spaces or apostrophes stay safe. Pre-push runs `bun run test`.
Check routing manually in a disposable checkout for TS, TSX styles, Markdown, Rust and ignored-only
stages.
Vite uses its default watcher rooted at `src/ui`; native crates, archive and root build outputs are
outside that root. No custom watcher module or ancestor-matching ignore is needed.
Smoke HMR once with `bun run dev`: edit/restore Shell text and observe an update without navigation;
write an ignored archive/nested-worktree probe and observe no update. Stop the owned server with Ctrl+C.

Correction smoke (2026-11-02): Shell edit/restoration hot-updated with the same page time origin and
window marker; archive/nested-worktree probes stayed quiet for five seconds. The owned server/browser
were stopped. Disposable hook checks passed Markdown, TS with spaces/apostrophes, TSX styles and Rust
workspace routing; malformed Markdown/TS and invalid CSS failed. Legacy/worktree-only stages ran no
tools. Normal prepare installed Husky in the disposable repo; linked prepare preserved shared config.

## Native dev-watch acceptance (manual)

This check is outstanding after the tooling redesign; do not infer it from a bundle build.
Use two terminals in the worktree. In terminal A run `bun run tauri:dev` and wait for the Next tray
and CLI watch messages. Keep this foreground session so Ctrl+C stops the dev session you own.
In terminal B, preserve the current uncommitted source and trigger a harmless recompile:

```sh
backup=$(mktemp "$PWD/target/native-watch-source.XXXXXX")
cp src/crates/app/src/lib.rs "$backup"
printf '\n// Native watch acceptance probe.\n' >> src/crates/app/src/lib.rs
# Observe recompilation and restart in terminal A, then restore:
cp "$backup" src/crates/app/src/lib.rs
# Observe the restoration recompile/restart, then leave source untouched.
legacy_probe=$(mktemp -d "$PWD/_legacy/.native-watch.XXXXXX")
nested_probe=$(mktemp -d "$PWD/.worktrees/native-watch.XXXXXX")
build_probe=$(mktemp "$PWD/target/native-watch.XXXXXX")
touch "$legacy_probe/probe.rs" "$nested_probe/probe.rs" "$build_probe"
# Observe no rebuild/restart for at least five seconds after these ignored writes.
# Stop terminal A with Ctrl+C before cleanup in terminal B:
rm -- "$legacy_probe/probe.rs" "$nested_probe/probe.rs" "$build_probe" "$backup"
rmdir -- "$legacy_probe" "$nested_probe"
lsof -nP -iTCP:1420 -sTCP:LISTEN
```

Confirm source restoration before removing its saved copy. Check lsof reports no owned dev server
and the Next process launched by terminal A exited. Never use killall/process-name termination or launch legacy.
Record the actual restart/quiet-period/cleanup observations before marking this acceptance complete.

## Native host smoke (opt-in)

`cargo nextest run --workspace --features stache-app/dev-tools` covers the host
composition with fake native and permission services: early quit, repeated
shutdown, blocked-start/shutdown races, queued main-thread drain,
watcher/enable/invalid/conflict cases, permission polling, ticker rescheduling and
window leases. The teardown coordinator is exercised with an injected clock, fake
terminal and recording record channel: a blocked configuration commit ends
`Uncertain` naming `host-worker` and `module:<id>` and still dispatches, a later
request shortens but never extends the total, repeat and post-outcome requests
change nothing, a blocked record sink is `unavailable` without delaying the
terminal action, two concurrent requests give one run and one outcome, and only a
`Clean` restart relaunches after releasing the instance claim. Bounded stages use
the same injected time over a real registry: a hung stop, blocked start and hung
module worker are each named `module:<id>` and abandoned while the next module
stops, modules unreached at the total are named in reverse start order, and a hung
watcher, permission ticker, startup job or accept thread is detached with the
owned endpoint still removed. Owner fakes acknowledge, fail or hang, so a missing
answer is unacknowledged and can never produce `Clean`; a recording executor
proves a main-thread owner is answered only by its own closure, while one stalled
hop names every main-thread owner and the tray hop stays separate from exit. A
window release answers only after `window_closed` for every label, including one
released by a stopping module, and an already-gone window counts as closed. The
socket suite checks a failed owned-inode removal, a foreign inode as success and a
hung handler as unacknowledged. The forced exit is a fake that records its code and
instant. It never launches Tauri, a tray, a webview, a real system prompt,
`terminate:` or a relaunch.

H10 runs the real run loop through a separate, feature-gated binary. Use the
provided per-user temporary root, not a fixed `/tmp` directory, and check the
socket path before launching:

```sh
bun run build   # embeds the production React bundle
SMOKE_ROOT=$(mktemp -d "${TMPDIR%/}/opencode/s.XXX")
SOCKET="$SMOKE_ROOT/com.marcosmoura.stache.next/ipc"
test "${#SOCKET}" -le 103 || { echo "temporary root too long"; exit 1; }
HOME="$SMOKE_ROOT" TMPDIR="$SMOKE_ROOT" XDG_CONFIG_HOME="$SMOKE_ROOT/config" \
  cargo run -q --release -p stache-app --features smoke --bin stache-smoke
echo "exit=$?"
rm -r -- "$SMOKE_ROOT"
```

This template produced a 103-byte resolved socket in the approved opencode
temporary root. Do not add another `s` or extend the `XXXX`; a 105+ byte socket is
the deliberate failure probe below, not the normal run.

`smoke` implies `custom-protocol`, so the harness loads the embedded production
assets rather than a dev server. It proves zero webviews, `run_on_main` on the
AppKit main thread, the activation observer delivering on main and removing itself
on drop, the React `TestWindow` DOM reporting `Host ready` and every registered
config file, generated `host_status`/`app_info` IPC inside WebKit, a pre-start
`proxyAudio.jsonc` loaded as a valid disabled config, a real socket reload
accepted from a changed valid config that stays disabled, the resulting
`statusChanged` update advancing the exact revision in both the React DOM and the
`host_status` snapshot, window create/reuse/close, the host still answering on
its socket and not shutting down after the last window closed, the second process
rejected by the instance lock, and a real SIGTERM delivered only to the owned
PID. It exits 0 after the shared shutdown path completes. The reload never
enables `proxyAudio`, so smoke never starts the audio runtime or changes the
default device. It never reads real configuration, launches legacy automation or
prompts for permissions.

The teardown terminate observation proof (`TEARDOWN-01-01`, `TEARDOWN-01-02`) is a
separate scenario of the same binary, selected only by `STACHE_SMOKE_TERMINATE`.
It **requires per-run approval**: do not run it as part of the checks above, and
never run the `stache-smoke` binary built below without that approval.

```sh
SMOKE_ROOT=$(mktemp -d "${TMPDIR%/}/opencode/s.XXX")
SOCKET="$SMOKE_ROOT/com.marcosmoura.stache.next/ipc"
test "${#SOCKET}" -le 103 || { echo "temporary root too long"; exit 1; }
HOME="$SMOKE_ROOT" TMPDIR="$SMOKE_ROOT" XDG_CONFIG_HOME="$SMOKE_ROOT/config" \
  STACHE_SMOKE_TERMINATE=1 \
  cargo run -q --release -p stache-app --features smoke --bin stache-smoke
echo "exit=$?"
rm -r -- "$SMOKE_ROOT"
```

Without `STACHE_SMOKE_TERMINATE` the binary runs the checks described above
unchanged. With it, the scenario uses the shipped observation and coordinator —
it installs neither — calls the real `terminate:` on its own process only, and
prints this evidence while `AppKit` waits for the coordinator's single reply:

```text
smoke: ok: applicationShouldTerminate: reached the shipped teardown coordinator
smoke: ok: a main-queue job ran while the terminate reply was pending
smoke: ok: a tao proxy event ran while the terminate reply was pending
smoke: ok: the coordinated teardown owns the reply; waiting for termination
```

Each step is bounded to 5 s and the process has 10 s after the last probe to
terminate; a missing step prints `smoke: FAIL: …` and exits non-zero. The
coordinator prints its one `teardown: outcome=…` record before the reply. The
scenario never signals or terminates another process, never terminates by
executable name and does not enable a module. Logout, restart and shutdown remain
unverified and need the disposable-session procedure in the spec.

The opt-in real-process failure probe deliberately makes the socket too long and
must exit non-zero even though `run_return` reports zero on macOS:

```sh
BASE=$(mktemp -d "${TMPDIR%/}/opencode/s.XXXX")
LONG="$BASE/$(printf '%080d' 0)"
mkdir -p "$LONG"
HOME="$LONG" TMPDIR="$LONG" XDG_CONFIG_HOME="$LONG/config" \
  ./target/release/stache-smoke
test $? -eq 1
rm -r -- "$BASE"
```

It prints `host setup failed: socket path … is longer than 103 bytes` and
`desktop host exited with code 1`, and returns **1**. The same assertion runs as
`cargo nextest run -p stache-app --features smoke --test smoke_failure`.

Observed 2026-11-03 in this worktree: every `smoke: ok:` line printed and the
normal process exited 0; the deliberate long-socket process exited 1. A second
manual run is required on any machine that changes the Tauri/window stack; do not
infer H10 from the bundle build alone. Tray-click and SIGINT delivery remain
manual because automating a menu-bar click is outside the owned process.

## Teardown verification (Part 04)

Read before validating the coordinated teardown. Parts 01–03 are committed. No
test or scenario here quits, logs out, terminates, signals or relaunches a real
process by executable name.

### Real-thread host scenarios

`src/crates/app/tests/host.rs` runs the assembled coordinator with the real
system clock, real threads and fake owners registered through
`Teardown::register_owner`. A real thread drains `ManualExecutor`, so a
main-hop owner is answered only by the closure that thread runs. Each owner
scenario yields exactly one outcome and one outcome record attempt, and leaves
the coordinator and host runner finished; the blocked-sink scenario additionally
attempts its forced-exit terminal evidence after the outcome slot is readable:

| Result                                         | Test                                                                          |
| ---------------------------------------------- | ----------------------------------------------------------------------------- |
| `Clean`                                        | `real_threads_all_owners_acknowledge_clean`                                   |
| `Partial`                                      | `real_threads_a_failing_owner_is_partial`                                     |
| `Uncertain` at the owner bound                 | `real_threads_a_hung_owner_is_uncertain_by_the_deadline`                      |
| blocked `host-worker` despite acknowledgements | `real_threads_a_blocked_host_worker_is_uncertain_despite_acknowledged_owners` |
| escalation ends a hang                         | `real_threads_escalation_ends_a_hung_owner_within_the_escalation_bound`       |
| concurrent requests                            | `real_threads_concurrent_requests_produce_one_run_and_one_outcome`            |
| blocked record sink                            | `a_blocked_record_sink_keeps_the_slot_readable_and_bounds_the_forced_exit`    |

Timing assertions are bounds, not exact instants; the escalation bound carries a
scheduling allowance for parallel suite runs. The blocked-sink scenario waits for
the normal total deadline before the forced exit, so it is the one host test that
costs about five seconds. The deliberately hung owner in the `Uncertain`
scenarios is named in the record instead of being silently left behind.

### Opt-in harness scenarios

Each scenario is selected explicitly for one run and needs approval for that run.
The binary remains behind the `smoke` feature and is built from this worktree.
Use a per-user temporary root and check the socket path first:

```sh
SMOKE_ROOT=$(mktemp -d "${TMPDIR%/}/opencode/s.XXX")
SOCKET="$SMOKE_ROOT/com.marcosmoura.stache.next/ipc"
test "${#SOCKET}" -le 103 || { echo "temporary root too long"; exit 1; }
HOME="$SMOKE_ROOT" TMPDIR="$SMOKE_ROOT" XDG_CONFIG_HOME="$SMOKE_ROOT/config" \
  cargo run -q --release -p stache-app --features smoke --bin stache-smoke
echo "exit=$?"
rm -r -- "$SMOKE_ROOT"
```

`STACHE_SMOKE_TERMINATE=hang` keeps the terminate observation scenario and also
registers a temporary hanging HUD owner, `smoke-hang`, that is never answered.
Its `release` prints exactly:

```text
smoke: ok: hanging owner entered
```

The owner's bound is the normal 2 s owner wait, so a parent has that window to
send a second request. Expected evidence is one `teardown: outcome=uncertain`
record whose `unacknowledged` list names `smoke-hang`, one `AppKit` reply with
its terminal record, and the exit status from `AppKit`. Any other non-empty
`STACHE_SMOKE_TERMINATE` value keeps the previous behaviour.

`STACHE_SMOKE_HOLD=1` starts the normal app with that hanging owner registered,
schedules no `terminate:`, opens the explicit test window the smoke checks use,
and waits for an external quit. It prints:

```text
smoke: ok: test window stache-test-N visible
smoke: ok: holding for an external quit
```

Bring the window frontmost and press Cmd-Q (twice for the repeat case). With
nothing arriving it prints `smoke: FAIL: no external quit arrived within 120 s`
and exits non-zero. `STACHE_SMOKE_HOLD=clean` is the same without the hanging
owner. A successful run ends inside `AppKit`, so the verdict is the stderr log
plus `wait "$PID"`.

`STACHE_SMOKE_RELAUNCH=1` sends the first process through the shipped
`request_restart` path after readiness. Before requesting, it writes a marker
under its own `AppPaths` runtime root; a process that finds the marker is the
successor. The successor must hold the instance lock, reach `ready == true`, and
print:

```text
smoke: ok: first process ready; requesting a coordinated restart
smoke: ok: relaunched successor reached readiness
```

The successor then requests an ordinary shutdown and exits by itself. Every wait
is bounded with `smoke: FAIL`. The first process is replaced by the restart, so
`run_return` does not return: the verdict is the parent's reading of the log, not
the process result.

These scenarios never touch a module's real resources; the smoke binary still
writes the disabled `proxyAudio` baseline before the host starts. Run
`bun run build` first so the embedded production assets exist, then build the
harness binary:

```sh
bun run build
cargo build --release -p stache-app --features smoke --bin stache-smoke
```

### Manual gates

One gate at a time, approved for that individual run. Never unattended, never
bundled with another gate. Target only `com.marcosmoura.stache.next`. The
env-selected scenarios live in the feature-gated `stache-smoke` binary; the
`bun run tauri:build` bundle is the shipped binary and is used where a gate calls
for the bundle itself. Both run with a temp-root environment and every module
disabled (`stache-smoke` writes the disabled `proxyAudio` baseline; the bundle
reads the isolated `HOME`/`XDG_CONFIG_HOME`):

```sh
ROOT=$(mktemp -d "${TMPDIR%/}/opencode/g.XXX")
SOCKET="$ROOT/com.marcosmoura.stache.next/ipc"
test "${#SOCKET}" -le 103 || { echo "temporary root too long"; exit 1; }
BUNDLE="$PWD/target/release/bundle/macos/Stache Next.app"
HOME="$ROOT" TMPDIR="$ROOT" XDG_CONFIG_HOME="$ROOT/config" \
  "$BUNDLE/Contents/MacOS/stache" 2>"$ROOT/stderr.log" &
PID=$!
```

The harness scenarios use the same environment with the binary instead:

```sh
HOME="$ROOT" TMPDIR="$ROOT" XDG_CONFIG_HOME="$ROOT/config" \
  ./target/release/stache-smoke 2>"$ROOT/stderr.log" &
PID=$!
```

Temporary roots isolate files and instance locks, not LaunchServices addressing
or the login session. Before any identifier-addressed event, record the
collision query and require it to pass:

```sh
bun scripts/check-next-instance.ts "$PID"
```

It prints the `query:` snapshot and the `result:` verdict, exits non-zero on a
foreign instance, an unknown identity or a failed query, and never sends
anything. A running legacy `com.marcosmoura.stache` process is a distinct
identity: it is not a collision and it is not treated as the owned instance.
Recheck immediately before the event and prevent concurrent launches for the
run.

Record for every gate: the command, the outcome and terminal evidence (including
record delivery availability, late reply and extended bound), socket absence, and
the `wait "$PID"` OS status. Keep `recorded_code` separate: `exit(7)` records 7
in the teardown outcome, while an unchanged `dispatch` may still return OS status
1; the AppKit status is a fact, not the outcome. The owned socket is absent when
`test ! -S "$ROOT/com.marcosmoura.stache.next/ipc"` holds.

- `TEARDOWN-04-02` — owned terminate with a temporary hanging owner:

  ```sh
  HOME="$ROOT" TMPDIR="$ROOT" XDG_CONFIG_HOME="$ROOT/config" \
    STACHE_SMOKE_TERMINATE=hang ./target/release/stache-smoke 2>"$ROOT/stderr.log" &
  PID=$!
  wait "$PID"; echo "status=$?"
  grep 'teardown:' "$ROOT/stderr.log"
  test ! -S "$ROOT/com.marcosmoura.stache.next/ipc"
  ```

  Pass: one `Uncertain` record naming `smoke-hang`, one reply with its terminal
  record, no owned socket, and the AppKit exit status recorded.

- `TEARDOWN-04-03` — hold without a hanging owner:

  ```sh
  HOME="$ROOT" TMPDIR="$ROOT" XDG_CONFIG_HOME="$ROOT/config" \
    STACHE_SMOKE_HOLD=clean ./target/release/stache-smoke 2>"$ROOT/stderr.log" &
  PID=$!
  ```

  Bring `smoke: ok: test window stache-test-N visible` frontmost, press Cmd-Q
  once, then `wait "$PID"`. Pass: one coordinated run, one `Clean` record, no
  owned socket. If the key never reaches AppKit, the gate fails and requires
  explicit re-planning with the owner; app-menu or quit-event evidence is
  supplementary only. Turn off legacy `commandQuit` first: while legacy Stache
  runs with it enabled, its event tap swallows Cmd-Q before this process sees it.

- `TEARDOWN-04-04` — hold with the hanging owner:

  ```sh
  HOME="$ROOT" TMPDIR="$ROOT" XDG_CONFIG_HOME="$ROOT/config" \
    STACHE_SMOKE_HOLD=1 ./target/release/stache-smoke 2>"$ROOT/stderr.log" &
  PID=$!
  ```

  First run: bring the window frontmost and press Cmd-Q twice. Second, separate
  run: start `STACHE_SMOKE_TERMINATE=hang` instead, wait for
  `smoke: ok: hanging owner entered`, then send SIGTERM to `$PID` from the parent,
  so the signal is the second request. Pass: within 250 ms of the second request the outcome has
  `escalated=true` and `Uncertain`, with one reply. If AppKit does not deliver
  the second quit while a reply is pending, record that; the SIGTERM result must
  still pass.

- `TEARDOWN-04-05` — the harness's own `NSApp` `terminate:`:

  ```sh
  HOME="$ROOT" TMPDIR="$ROOT" XDG_CONFIG_HOME="$ROOT/config" \
    STACHE_SMOKE_TERMINATE=1 ./target/release/stache-smoke 2>"$ROOT/stderr.log" &
  PID=$!
  wait "$PID"; echo "status=$?"
  ```

  Pass: one coordinated `Clean`, one reply, no owned socket. Optional
  `osascript -e 'tell application id "com.marcosmoura.stache.next" to quit'` is
  allowed only after the collision guard passes; it proves only that
  supplementary external route.

- `TEARDOWN-04-06` — logout with the app running, only in a disposable login
  session or on a spare machine approved for that run, with evidence collected
  outside the affected session. Pass: one record within budget, no owned socket,
  the logout proceeds, and the built `Info.plist` does not enable sudden or
  automatic termination. Without a disposable session or spare machine, record
  **not run**; Part 04 stays blocked. The same isolation is mandatory for any
  approved restart or shutdown evidence.
- `TEARDOWN-04-07` — SIGTERM to `$PID` from the bundle with no prior request.
  Pass: one `Clean` record, no owned socket, and `wait "$PID"` status 0.
- `TEARDOWN-04-08` — the coordinated relaunch:

  ```sh
  HOME="$ROOT" TMPDIR="$ROOT" XDG_CONFIG_HOME="$ROOT/config" \
    STACHE_SMOKE_RELAUNCH=1 ./target/release/stache-smoke 2>"$ROOT/relaunch.log" &
  PID=$!
  for _ in $(seq 1 60); do
    grep -q 'relaunched successor reached readiness' "$ROOT/relaunch.log" && break
    sleep 1
  done
  wait "$PID"; echo "status=$?"
  grep 'smoke: ok:' "$ROOT/relaunch.log"
  ```

  Pass: the first line, then the successor line, with the successor holding the
  instance lock and reaching readiness instead of stopping at the early
  `AlreadyRunning` return; it then exits on its own. The first process's result
  and the log are the evidence because the restart does not return through
  `run_return`. This exercises the direct-spawn path; the bundled `open -n` path
  remains blocking for the Reload item's follow-up.

- `TEARDOWN-04-09` — the fake-query tests above prove refusal. Each approved
  identifier-addressed run records the `bun scripts/check-next-instance.ts "$PID"`
  output before the event. Never launch or signal a real conflicting instance to
  test the guard.

This procedure claims no gate as passed; each run's evidence lives in the
[part's completion notes](../specs/2026-11-03-teardown/04-verification-gates.md#gate-evidence). The orchestrator records the
per-run evidence. A pass covers that run only, and a change to terminate
observation, budgets or stage order requires a rerun.

## Bundles and prerequisites

The Cargo lock pins `tauri =2.12.5`, the same minor as the pinned
`@tauri-apps/api 2.12.4`; the Tauri CLI rejects a Rust/JS minor mismatch
("Found version mismatched Tauri packages…"), so the two move together. The rest
of the lock only carries what that pin requires: `tauri-build =2.7.4`,
`tauri-runtime`, `tauri-runtime-wry`, `tauri-utils`, `tauri-codegen`/`tauri-macros`,
`wry`, `tao`, `muda` and `tray-icon`. Cargo audit currently reports upstream
maintenance/unsoundness warnings. The Bun lock pins patched Vite/Vitest and
tested happy-dom/minimatch/tinypool overrides.
`bun run tauri:build` passes `--features custom-protocol`; a release build without it fails early
because Tauri would otherwise ship a dev-mode window. Generate/schema binaries are feature-gated and
not bundled.

Launch only `target/release/bundle/macos/Stache Next.app`, verify tray/Quit and zero windows, then quit.
The skeleton needs no Accessibility/audio permissions. Local bundles use ad-hoc signing; distribution
signing/notarization needs separate credentials. CI audits new locks, not the archive. Legacy build
verification is opt-in via [_legacy/README.md](../../_legacy/README.md), including resource/signing gaps.
Never run legacy and new device-changing automation together.

## Release performance comparison

`bun scripts/measure-foundation.ts` (also `bun run measure -- --help`) measures
release startup, 60-second idle CPU/RSS and process trees with isolated config
roots. It refuses to start while any Stache Next or legacy Stache process is
running and never signals another instance. Build the bundles and follow
[performance.md](performance.md) before running it; do not run a manual
audio-device test at the same time.
