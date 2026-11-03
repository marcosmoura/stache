# Release performance measurement

Read when building release binaries for comparison, running the foundation
measurement, or interpreting its report.

The tool is `scripts/measure-foundation.ts`, run with Bun. It drives release
binaries only and rejects paths that look like debug builds; it never starts a
dev server.

```sh
bun run measure -- --help
bun run measure -- \
  --new-app "target/release/bundle/macos/Stache Next.app" \
  --legacy-app "/path/to/legacy/Stache.app"
```

## What one run does

Scenarios run sequentially, with no two device-changing watchers alive at once:

1. Stache Next with `proxyAudio` disabled.
2. Stache Next with `proxyAudio` enabled and priorities that resolve to the
   current default devices, so the pilot does not switch anything.
3. Legacy Stache baseline, only when `--legacy-app` is passed.

Each scenario gets its own short temporary root under
`$TMPDIR/opencode/XXXX`, used as `HOME`, `TMPDIR` and `XDG_CONFIG_HOME`.
Configuration files are written inside that root. The real
`~/.config/stache` and `~/Library/Caches` are never read or written.

Every scenario is launched at least five times. The last launch stays alive for
an idle sample after a 10-second warmup. There are no requested windows.

## Flags

| Flag                   | Meaning                                                                                 |
| ---------------------- | --------------------------------------------------------------------------------------- |
| `--new-app <path>`     | Release `.app` bundle or executable. Defaults to the release bundle.                    |
| `--legacy-app <path>`  | Optional legacy release `.app` bundle or executable.                                    |
| `--launches <count>`   | Timed launches per scenario; default and minimum are 5.                                 |
| `--idle-seconds <sec>` | Idle sampling window; default is 60. Use a small value only for a dry check.            |
| `--help`, `-h`         | Prints flags, prerequisites, safety rules and the guarded command without side effects. |

Relative paths resolve against the repository root. The new target must exist;
build it with `bun run tauri:build`. Legacy builds follow `_legacy/README.md`.

## Prerequisites and safety

- Quit every Stache Next instance. The tool refuses to start while one is
  running. A running legacy Stache, such as the daily `/Applications/Stache.app`,
  only blocks a run that passes `--legacy-app`; otherwise keep its proxyAudio
  disabled so only one device-changing watcher runs. The tool never signals or
  terminates another instance; quit it from its own tray or menu.
- The report records the actual guard scope for every launch. Without
  `--legacy-app`, a legacy Stache is detected and noted as observed but does not
  block the run; with `--legacy-app`, both identities block.
- If Launch Services cannot be read, the guard fails closed: the tool refuses to
  start instead of assuming no other instance is running.
- Do not run a manual audio-device test at the same time. Run one measurement at
  a time; two tool instances are not mutually excluded.
- Prefer AC power and a quiet machine. The report records the actual power
  source and every default audio device.
- The tool sends `SIGTERM` only to the process it spawned. If descendants
  outlive it, it verifies the recorded PID still runs the same executable
  before signalling; it never uses `killall` or name matching.
- After each launch it waits for the whole recorded process tree to exit before
  starting the next one.
- Temporary roots and launched children are cleaned up on success, errors and
  SIGINT. `SIGTERM`/`SIGINT` are forwarded only to the owned child. If a measured
  process tree survives the stop attempts, its temporary root is kept, the run
  fails, and the safety record lists the surviving processes.
- A second SIGINT or SIGTERM during cleanup is absorbed so cleanup is not
  abandoned halfway.
- The pilot-enabled scenario uses exact-name priorities for the current default
  input and output. The tool refuses that scenario when the pilot's own order
  would still override the priorities (built-in output with an HDMI output
  connected, or active screen mirroring with an AirPlay device), or when the
  recorded default cannot be located in the device list.
- If a default device changes anyway, the tool attempts a restore with
  `SwitchAudioSource` when installed, otherwise with
  `scripts/measure/restore-default-audio-device.swift` through `/usr/bin/swift`,
  and records the result. A changed or unrestored device keeps the run exit
  code non-zero.

## Failures and interrupts

A run that stops early still writes `report.md` and `samples.json`. The status
line records `FAILED` or `INTERRUPTED` with the original error, scenarios that
did not finish are marked incomplete, and every launch and idle sample collected
before the stop is kept. Cleanup issues — a kept temporary root, a stop that
could not be confirmed, or an unverified audio end state — are listed in the
safety record next to the failure.

Exit codes:

- `0`: every scenario completed, all temporary roots were removed, and the
  default audio devices were read back and verified unchanged.
- `1`: the measurement failed or its cleanup was incomplete; the report says
  which.
- `130`: SIGINT or SIGTERM stopped the run.

A signal ends the active readiness wait, warmup or idle sample, stops the owned
tree, and no later launch starts. A second signal during cleanup is absorbed so
cleanup still finishes.

The tool formats `report.md` and `samples.json` with the repository formatter, so
a measurement directory needs no separate `bun run format` before committing.

## Readiness definitions

The two timing columns answer different questions:

- External launch to ready: for Stache Next, the control socket answers
  `core.status` (the socket is created after the tray). For legacy, its own
  `setup complete (background tasks spawned)` stderr line, which it emits after
  creating the tray, bar and widgets.
- Internal main entry to tray created: the host writes one info line,
  `startup: tray ready after N ms, webviews=N`, from
  `src/crates/app/src/lib.rs` using an `Instant` captured in `run_main`. The
  line is emitted after `tray::create` and after the existing zero-webview
  check.

The `< 300 ms` provisional target applies to the internal host measurement only.
External launch timing is reported separately and has no target.

## Webview evidence

For Stache Next the instrumented count in the startup line is authoritative,
and the host aborts setup if any webview exists at that point. The process tree
is still sampled, so an unexpected WebKit helper becomes an observed
inconsistency. For legacy, only observed WebKit processes are reported; process
names alone do not prove a webview count.

## Reading the report

Reports and raw samples are written to
`docs/specs/2026-11-02-foundation/measurements/<UTC timestamp>/`:

- `report.md` — environment, method, per-launch timings, idle trees, target
  comparison, misses, safety record and limitations.
- `samples.json` — every launch, config file content, idle process row and
  process-tree snapshot used to make the report.

`cold` is the first launch in a scenario with an empty isolated app cache.
`warm` launches reuse that cache. The OS page cache is not flushed, so cold is
not necessarily the first launch after the build. Warm medians and ranges are
reported with the cold value separately.

CPU comes from `ps` cumulative CPU time deltas over the sampling window, not
from the decaying `%cpu` column. `ps` time resolution is 10 ms, about 0.017
percentage points over 60 seconds for one process. CPU figures are averages and
remain subject to unrelated system noise.

RSS values are resident sizes summed per process. Shared pages are counted once
per process, so tree RSS is an upper bound for distinct physical memory. Helpers
reparented to `launchd` cannot be attributed by parent PID; the report lists
observed WebKit processes outside the tree separately and excludes them from
tree totals.

The report compares against the provisional targets without inventing a numeric
CPU budget: ≈0% idle CPU is labelled against a 1% noise threshold while the raw
average is always shown. Misses are listed explicitly; targets are not relaxed.

## Known limitations

- The legacy cache socket path under the isolated `HOME` exceeds the 103-byte
  macOS `sun_path` limit, so the legacy IPC server cannot bind during a
  measurement. Legacy readiness uses its setup log line and the baseline lacks
  that idle accept thread.
- The legacy application always starts its tray, bar and widget webviews; the
  report records them as baseline overhead rather than claiming they were
  disabled.
- A bare release binary at a path other than the measured executable is not
  detected by the exclusivity guard; release `.app` bundles are matched by
  Launch Services bundle identifier and path.
