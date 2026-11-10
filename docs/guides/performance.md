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

A gated launch (Accessibility missing) creates one webview, the guidance window,
after the first permission check; the startup record above is written earlier and
still reads `webviews=0`. The host then logs `access: guidance window requested`.
A trusted launch creates no webview.

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

## Tiling runtime bounds

These are deterministic bounds, not measurements (live figures need a per-run
approved measurement). Disabled Tiling owns no worker, timer or tap. Geometry
flushes run no faster than the configured display rate, clamped to 30–360 Hz
(60 Hz fallback). Animation is off by default and spawns its single coordinator
thread (`stache-tiling-animation`) only when `animations.enabled`. The
coordinator never makes an AX call and never waits for a write: each tick it
sends one batch per application to that application's AX thread, whose mailbox
holds at most one pending frame per window, so a slow application skips frames
and never delays another. It keeps one latest plan per window in a mailbox of at
most 16 windows, paces frames with one display clock per display that has
running animations (below), never retries a refused or partial frame, and is capped at
1000 ms per move. Intermediate frames are write-only with no `AXWindows` listing
and no readback; the first and final frames list membership, and each animation
takes one settle readback (plus one at a retarget). Non-animated frame writes are
submitted the same way, so the effect worker never blocks on a native write. The
coordinator and its pacing are **unverified** on real hardware.

Animation pacing owns one `CVDisplayLink` per display with running animations,
created on its first job and stopped when it has none: idle and disabled Tiling
own zero clocks. The link's callback thread belongs to CoreVideo and does one
atomic store and one short mailbox lock plus a condition-variable notify per
refresh (no AX call, no allocation, no lock held while calling out); the
coordinator thread wakes once per refresh of a display that animates, takes only
the latest tick, and computes one frame per window for it, so a late wake skips
frames instead of bursting. Stopping a clock waits at most 100 ms for a callback
in flight; a clock that will not stop is kept and retried. A display without a
clock falls back to the timer at the clamped rate and reports it in status. The
display-link availability, cadence and ProMotion behavior on macOS 26 are
**unverified**. While the size changes, every frame writes the interpolated size with the
position (the final frame writes the exact target), so content may relayout on
each written frame; how each application copes with that is **unverified**.

Tiling's AX work runs on one thread per tracked application
(`stache-tiling-ax-<n>`, at most 32; applications beyond that share
`stache-tiling-ax-overflow`), started on the first `track` and ended on
`untrack` or stop. A disabled Tiling, and a running one before its first
tracked application, own none. Every shard queue holds at most 64 commands and
one slow application never delays another's actions or snapshots. The cost of
one run loop and observer per thread is **unverified**.

Threads an enabled, animating Tiling owns (all others are existing application
threads): one `stache-tiling-ax-<n>` AX shard per tracked application (at most
32), the shared `stache-tiling-ax-overflow` shard for applications beyond that,
the single `stache-tiling-animation` coordinator, and one CoreVideo-owned
callback thread per display clock while that display has a running animation.
The shards and the overflow shard make every native write; the coordinator and
the clock callbacks make none. The animation metrics cost the coordinator one
short lock at the end of each animation (a fixed 64-sample latency array per
running animation, a book of at most 8 animations for each of at most 64
applications) and no AX call; status takes the same lock to read. SkyLight
batching is not used, so no private window-server call or extra thread exists.
Whether those threads, their wakeups and CPU cost are acceptable is
**unverified**; the instant fallback (see [Tiling](../features/tiling.md#animation))
bounds the cost of an application that cannot keep up.

Borders are off by default and then own no thread, port, child or timer. Enabled,
they spawn one runner thread holding the latest pending update; animation frames
are capped at **8 per second**, a Mach send at 0.5 s, a helper CLI child at 2 s
(plus 2 s to reap, 64 KiB per output stream), and the stop join at the sum of
those. Waits are cancellation-wakeable and no lock is held across a send or a
sleep. The achieved rate and JankyBorders' own load are **unverified**.

## Bar system providers (part 05)

Deterministic bounds (tested on a fake clock, no real sleeping): a disabled,
paused, zero-size or panel-less Bar owns **zero** provider threads, timers,
children and reads. An active Bar owns three worker threads (CPU, battery,
Wi-Fi), each with one owned schedule: CPU every 2 s after a 250 ms warm-up,
battery every 30 s while charging and 120 s otherwise, Wi-Fi at most every 5 s.
A sample never overlaps itself and a stall causes one late sample, never a
catch-up burst. The CPU temperature is a native SMC read (no child process);
a sample re-reads an all-invalid key group at most 3 more times and repeats the
last valid value for at most 3 samples. The Wi-Fi `ipconfig` child (1.5 s
deadline, 256 KiB cap) runs once per Wi-Fi sample by default (SSID path on; `STACHE_BAR_WIFI_SSID_SUMMARY=0` turns it off for diagnostics).

**Measurement procedure (script ready, run pending; needs separate approval,
release builds only).** The method is the foundation tool above plus module
toggling, implemented by `scripts/measure-bar.ts`:

1. Build one release `.app`; never a debug build, never the legacy app.
2. Fix the conditions across all trials: same hardware and power state (note
   battery vs AC), same Bar `height`/`padding`, same display widths, no other
   foreground work, identical warmup (at least 10 s) and sample duration (at
   least 60 s).
3. Alternate **Bar off** (`bar.toml` `enabled = false`) and **Bar on**
   (`enabled = true`) in an isolated `HOME`, at least 5 launches each, order
   alternated to cancel drift. Record host CPU % (average and peak) and idle
   wakeups per second for each launch.
4. Report all raw samples, the run-to-run noise envelope of the Bar-off trials
   and the module-on minus module-off delta with its spread. The budget is "no
   statistically distinguishable regression beyond the recorded noise
   envelope"; publish the samples and method before calling it met.
5. The SSID path is on by default, so the series above is the default series.
   Add a separate series labelled "SSID off" (`STACHE_BAR_WIFI_SSID_SUMMARY=0`)
   as a comparison, never mixed with the default.
6. A legacy comparison needs an independently supplied legacy measurement that
   isolates Bar; without one the relative budget stays **unverified**, never
   passed, and legacy automation is not launched to obtain it.

Until the run, the SMC read cost and the CoreWLAN read cost are **unverified**.

### Running the Bar measurement

```sh
bun run measure:bar -- --help        # reads no process state
bun run measure:bar -- --app "target/release/bundle/macos/Stache Next.app"
bun run measure:bar -- --app "target/release/bundle/macos/Stache Next.app" --ssid-off   # "SSID off" series
```

Preconditions the script checks or explains:

- **Accessibility.** The launch gate holds every module, Bar included, unless the
  exact release `.app` being measured is trusted for Accessibility. Grant it for
  that path first (System Settings > Privacy & Security > Accessibility). The
  script reads `core.status` over the isolated control socket; `access.state`
  must be `open` in both arms. A gated launch is reported as an **invalid trial**
  (never a sample) with a message naming the `.app` path, and the run stops.
- **Bar actually started.** The control socket has no Bar status method, so the
  "on" arm also needs a host-owned window of the configured Bar height (read
  with `list-windows.swift`: owner pid, bounds, layer; no Screen Recording
  permission) and the "off" arm must own no window. Module `Running` and panel
  `Loaded` are therefore **inferred**, not read. Either failure marks the trial
  invalid; three invalid trials stop the run.
- A release `.app` only (a debug path or a bare executable is refused), and no
  Stache Next or legacy Stache process running (refused, never terminated).
- An idle machine: a 10 s whole-machine CPU pre-flight (the foundation
  `sampleIdle` over the process table) refuses above `--max-busy-cpu`. CPU is
  summed over processes with 100% per core; the default is 15% of total capacity
  (150 on a 10-core Mac), because a normal idle desktop (WindowServer, audio and
  HID drivers, an editor) already sums to over half a core.

Procedure: `--launches` (default and minimum 5) valid launches per arm, alternating
off/on and on/off pairs, each in its own isolated temporary root with a `bar.toml`
(`enabled = false|true`, the same `height`/`padding`), a 10 s minimum warmup after
readiness and a 60 s minimum sample. Per launch it records host CPU average and
peak (peak over 5 s intervals), tree CPU and idle wakeups per second (`top -l 2`,
summed over the host tree). Signals go only to the child the script spawned, through
the foundation tool's owned-tree shutdown; nothing is stopped by name.

Output: `docs/specs/2026-11-03-bar/measurements/<UTC timestamp>/report.md` and
`samples.json`, written even for a failed or interrupted run with every raw sample,
invalid trial and the process-tree snapshots. The report gives the Bar-off
envelope (mean, stdev, min, max), the Bar-on figures and the on minus off delta with
its standard error. It never declares the budget met. Exit codes: `0` completed and
clean, `1` failed or incomplete, `2` usage, `130` interrupted.

The SSID path is on by default, so the default series is the one above; run
`--ssid-off` for the separate labelled comparison. The run needs a per-run user
approval and is not part of any automated check.

## Animation measurement method

**Status: unverified. No approved native run has been recorded.** Every number
a run would produce is marked "unverified: no approved native run yet" until one
is recorded here with its raw samples. The run needs its own per-run approval
under [validation](../specs/2026-11-03-tiling/validation.md#per-run-native-approval)
naming the exact application instances, the temporary Tiling config, the display
and the cleanup. Nothing in the default build or the test suites runs it, and no
binary for it ships.

**Scenario.**

- Display: one named display. Record its refresh rate (and whether it is
  ProMotion), the macOS version and the hardware build.
- Config: a temporary `tiling.toml` with `[animations]` `enabled = true`,
  `duration = 200`, `easing = "ease_out"`, and one workspace in the `master`
  layout holding Ghostty, Zed, Safari (or Aside) and Spotify.
- Action: 10 toggles `master` to `monocle` and 10 `monocle` to `master`, each
  after the previous animation settled; repeat the set three times with a warmup
  set first.
- Record per application: effective rate, dropped frames, per-write latency p50
  and p95, end lag (the final write's acknowledgement minus the tick or timer
  deadline that produced it), whether Enhanced UI was on and leased, whether the instant
  fallback triggered and why, thread count, and CPU and wakeups during the run
  against the same run with animations off. The first five come from the status
  `animation metrics` and `animation:` diagnostic lines; the rest from the
  system's process tools.
- Cleanup: restore the user's config, leave no lease open and report exactly
  what the run owned.

**Pass criterion.** Every scenario application either (a) has a median effective
rate of at least half the display refresh, a p95 end lag of at most one frame
period and zero writes after its settle readback, or (b) is in instant fallback
with its diagnostic. All four applications must meet (a): a fallback for any of
the four fails the run. Visual smoothness is judged by the user. The effective
rate is measured at the AX acknowledgement, **not** at on-screen presentation,
which cannot be observed without Screen Recording.

The criterion above is the target of the native run. The shipped automatic
fallback is looser and absolute: one record per application per layout change,
bad below 15 written frames per second or more than 250 ms of end lag, and the
fallback triggers when 2 of the last 3 records are bad. Pacing still targets the
display's native refresh.

**Results.**

| Item                                                 | Result                                 |
| ---------------------------------------------------- | -------------------------------------- |
| Display, refresh rate, macOS, hardware               | unverified: no approved native run yet |
| Effective rate per application                       | unverified: no approved native run yet |
| Dropped frames per application                       | unverified: no approved native run yet |
| Write latency p50 / p95 per application              | unverified: no approved native run yet |
| End lag p95 per application                          | unverified: no approved native run yet |
| Enhanced UI on and leased per application            | unverified: no approved native run yet |
| Fallback triggered and why                           | unverified: no approved native run yet |
| Thread count, CPU and wakeups against animations off | unverified: no approved native run yet |
| Zero writes after the settle readback                | unverified: no approved native run yet |
| On-screen smoothness                                 | unverified: no approved native run yet |

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
