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

Keep Awake validation is fake-backed. No automated check creates a real power
assertion, locks or unlocks the screen, wakes the machine, installs a real
distributed observer or launches Stache Next. Module tests drive fake assertion
and observation backends, in-memory lock results, barriers and races, and a fake
clock; app host tests write malformed JSON, a wrong boolean and a deleted file,
run the normal watcher/reload path, query the actual `keepAwake.status` socket,
restore identical last-good bytes and verify both held store and snapshot
diagnostics clear without lease churn. They also cover tray Enable/Disable JSONC
edits, session Pause/Resume without disk writes and shutdown cancellation. The
Keep Awake envelope and runtime DTOs use the shared bindings collector, so the
generated DTO check is their gate; there is no Keep Awake Tauri command or CLI
command. Real three-assertion behavior, notification delivery, manual
lock/unlock, resume and forced-sleep checks remain **unverified**; they require
approval for each run and the
[spec's power protocol](../specs/2026-11-03-keep-awake/00-overview.md#validation-and-safety).

`cargo doc -p stache-sdk -p stache-macos -p stache-keep-awake -p stache-proxy-audio -p stache-notunes -p stache-cli --no-deps`
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
- `test` runs `test:ui` and `test:native`. Bun runs commands; Vitest runs TS tests.
- `test:ui`: real Playwright WebKit through Vitest. Suites live under `src/ui/src` next to components;
  `src/ui/tests` holds setup/support only. SDK wrapper and status-feed suites mock the generated Tauri
  command/event boundary; `WindowRouter` and `TestWindow` suites prove route validation, subscription
  cleanup, event-over-snapshot precedence, empty/error states and token styles through the computed
  background. Coverage retains 80/80/65/80 thresholds.
- `test:native`: cargo-nextest across only the root workspace with
  `--features stache-app/dev-tools`; checks desktop identity/window policy, the SDK lifecycle, config
  and window suites, app host races/permissions/shutdown, the proxyAudio policy and fake-backend
  lifecycle, the noTunes blocker sweep/launch/reconfigure/stop policy, the Keep Awake fake
  lock/assertion/observation matrix and its host status/socket suites, the macOS audio
  listener-rollback tests and the macOS adapter unit tests. Native tests
  never prompt for Accessibility and never change the system default audio device; the real
  main-thread/window smoke test is the opt-in `stache-smoke` harness described below.
  Generator binaries require `dev-tools` and are excluded from the release bundle.
- `cargo nextest run -p stache-sdk -p stache-cli -p stache-proxy-audio -p stache-notunes -p stache-keep-awake -p stache-macos`
  runs the config/audio/blocker/keep-awake foundation. The proxyAudio lifecycle suite uses a fake backend for
  every listener, worker-spawn, set-default and stop-race failpoint; no test starts a real listener.
  The noTunes suite uses `FakeApplicationBackend` and `ManualExecutor` for every observation, sweep,
  launch, open and stop failpoint; no test observes, terminates or launches a real application.
  The Keep Awake suite uses fake assertion, lock-probe, observation and clock inputs; no test
  acquires real power prevention or interacts with the real screen lock.
  `cargo doc -p stache-sdk -p stache-macos -p stache-keep-awake -p stache-proxy-audio -p stache-notunes -p stache-cli --no-deps`
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
window leases. It never launches Tauri, a tray, a webview or a real system
prompt.

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
