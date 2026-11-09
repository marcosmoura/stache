---
status: draft
---

# 01 — Baseline measurements

Read when implementing after separate approval, only when the owner explicitly selects this part and every other roadmap workstream is finished or has no actionable part ([priority](00-overview.md#priority)). Each measurement run needs its own explicit owner approval.

## Scope

A new, repeatable measurement of the current Tauri UI surface in a release build, plus its first recorded results: `scripts/measure-ui.ts` wired as `bun run measure:ui`, with its own modules under `scripts/measure/ui/`. The measured surface is the Bar window if Bar has shipped, otherwise the test window (OQ-1). No shipped surface or behavior changes; any `stache-app` change beyond OQ-1/OQ-2 is out of scope.

### Reused unchanged from `scripts/measure/`

Imported as they are; no existing export changes signature or behavior:

- `process.ts`: `runCommand`, `listProcesses`, `parseCpuTime`, `readProcessTree`, `processTree`, `readRunningApplications`, `findRunningStacheProcesses`, `assertNoOtherNextInstance`, `stopOwnedProcessTree`, `waitForExit`.
- `idle.ts`: `sampleIdle` at most for the host-tree portion (per-process CPU from cumulative `ps` time and RSS of the host and its descendants). Its `unattributableWebKit` field is one end-of-window, RSS-only snapshot of every WebKit helper on the system and is not used as a metric; WebKit helpers are sampled by the new UI sampler ([WebKit helper attribution](#webkit-helper-attribution)).
- `interrupt.ts`: `InterruptedError`, `throwIfInterrupted`, `sleepUnlessInterrupted`.
- `config.ts`: `createTempRoot`, `removeTempRoot`, `writeNextConfigs(root, false, { devices: [], defaultInput: null, defaultOutput: null })`. With `enabled = false` the `AudioState` argument is never read (both priority lists render as `[]`), so the tool does not call `readAudioState` for configuration and needs no audio restore.
- `system.ts`: `collectEnvironment` (machine, OS, toolchain, repository, power, displays).
- `paths.ts` constants that are target-agnostic (`REPOSITORY_ROOT`, `NEXT_BUNDLE_IDENTIFIER`, `LEGACY_BUNDLE_IDENTIFIER`, `DEFAULT_NEXT_APP`, `IDLE_SAMPLE_INTERVAL_MS`, `SHUTDOWN_TIMEOUT_MS`).
- `launch.ts`: only the pure `controlSocketPath`.
- The debug-path rule of `targets.ts` (`assertReleaseBuild`, not exported today): either export it additively, or apply the same rule (`/debug/` in the lower-cased path is rejected) in the new module.

### New code in `scripts/measure/ui/`

The foundation modules below are hard-wired to the foundation run and are **not** reused:

- `launch.ts` `startTarget` always passes `--config`/`--desktop` and `waitForReady` knows only the Next socket and the legacy log line → new target launch and readiness ([Readiness rules](#readiness-rules)).
- `types.ts` `TargetKind` is `'next' | 'legacy'` → new UI target and sample types.
- `paths.ts` `MEASUREMENTS_DIRECTORY` points at the foundation spec → new output directory constant.
- `report.ts` renders the foundation report and its targets → new report rendering.
- `run.ts` samples idle only on the last launch and owns the SIGINT handling, exit codes (0/1/130) and cleanup → new runner that keeps the same exit-code and cleanup contract, with an idle sample on every trial.
- WebKit helper attribution and a UI sampler `scripts/measure/ui/sampler.ts`, built on the reused `listProcesses`, `processTree` and `parseCpuTime`, that samples the host tree plus the attributed helper PIDs on every interval (no foundation equivalent; [WebKit helper attribution](#webkit-helper-attribution)).
- Wakeup sampling from `top -l` (no foundation equivalent).
- The WebKit helper pattern: the foundation's `WEBKIT_PROCESS` (`/WebKit\.(WebContent|Networking|GPU)|com\.apple\.WebKit/` in `idle.ts`) is not exported; either export it additively or use the same pattern in the new module.

If implementation finds that a shared module must change, foundation regression tests for the affected behavior are added in a separate commit first, and the change is additive.

The target is an input (`--app <path>` plus a readiness rule) so part 02 can point the tool at the spike.

## Requirements

### Targets and safety

- Every rule of [guides/performance.md](../../guides/performance.md#prerequisites-and-safety) applies: release builds only (reject a path containing `/debug/`), no dev server, isolated temporary root used as `HOME`/`TMPDIR`/`XDG_CONFIG_HOME`, fail closed when Launch Services or the process table cannot be read, signal only the owned child, verify PID and executable before signalling descendants, no `killall`/name matching, cleanup on success, error and SIGINT. No `sudo` and no `powermetrics`.
- **No legacy.** `measure:ui` has no legacy flag and rejects a target whose bundle identifier is `com.marcosmoura.stache` before anything launches.
- **Target kinds** (closed set): `next` (bundle identifier `com.marcosmoura.stache.next`) and `spike` (bundle identifier `com.marcosmoura.stache.spike.gpui`, part 02). Any other bundle identifier, or a bare executable whose identity cannot be read, is rejected.
- **Collision refusal, before each launch.** The tool refuses to start, for either target kind, when reused `findRunningStacheProcesses(next, null)` returns any `next` or `legacy` entry (as foundation `run.ts` `assertNoOtherStache` does for `next`; with `legacy = null` it still reports legacy through the Launch Services bundle identifier and the `/Stache.app/Contents/MacOS/` path), or when any spike instance runs. The `next` argument is the `--app` target for a `next` run; for a `spike` run it is a `next` `Target` built from `DEFAULT_NEXT_APP` resolved against `REPOSITORY_ROOT` (or an optional `--next-app <path>` override), the same way `scripts/check-next-instance.ts` builds it (`bundleTarget`). Other Stache Next copies at any path are still caught by the bundle identifier and by the `/Stache Next.app/Contents/MacOS/` path match in `process.ts` `classifyExecutable`. Spike detection uses the Launch Services bundle identifier `com.marcosmoura.stache.spike.gpui` plus the exact resolved executable path and PID from the process table; never a process name. A failed query refuses (fail closed).
- **Ownership check, after launch.** For `next`, `assertNoOtherNextInstance(query, ownedPid)` once the child has a PID; it throws `unknown-identity` while the owned PID is not yet reported, so it runs after readiness, not before launch. For `spike`, the owned PID must be the only spike process and must run the target's exact executable path.
- **Launch arguments.** `next`: `--config <isolated config dir> --desktop`. `spike`: `--cadence <none|1s> --window <none|surface>` only (`none` for the `no-window` scenario, `surface` for `surface-visible`). The child environment is the same isolated `HOME`/`TMPDIR`/`XDG_CONFIG_HOME` set for both.

### Readiness rules

Readiness is a closed enum recorded per trial in `samples.json`; anything else is a usage error:

- `next-core-status`: the isolated control socket (`controlSocketPath(root)`) answers `core.status` with `status: "ok"`.
- `stderr-line:<exact text>`: the child writes a stderr line equal to `<exact text>` after trimming. The spike uses `stderr-line:spike: ready`. Exact matching fits because the spike prints these bare lines itself, without a logger prefix.

Each rule has a timeout (default 30 s); on timeout the trial is cleaned up and excluded with the stderr tail ([exclude and replace](#method)). `spike: first frame presented` is parsed as a separate optional timing, never as readiness.

### Accessibility gate (Stache Next)

- The measured release bundle must already be trusted for Accessibility. Each `next` trial records `access.state` from the readiness `core.status` response (it may still be `checking`) and again from a second `core.status` query at the end of warmup. Both values are recorded per trial in `samples.json`.
- Host log lines reach stderr as `stache: <level> host.<op>: <message>` (`StderrLogger` in `src/crates/app/src/logging.rs`). The tool matches the message after that prefix: `access: open` and `access: guidance window requested` exactly, `access: blocked (` as a prefix. Unlike the exact `stderr-line:` readiness rule, these are suffix matches on prefixed log lines. Matched lines and their times are recorded per trial.
- The run fails (`FAILED`, cleanup as usual; the trial is not replaced) if the guidance window is requested, or if the end-of-warmup `access.state` is not `open`. A gated launch would otherwise measure the guidance webview while the startup line reads `webviews=0`.
- `--help` states that a rebuilt or re-signed bundle may need its Accessibility trust granted again before a run.

### Configuration

The isolated configuration enables no module that adds work unrelated to the surface: reused `writeNextConfigs` with proxyAudio disabled, plus new files that explicitly set `enabled = false` for every other module with an `enabled` key (Tiling, Keybindings, Keep Awake, Wallpapers, noTunes, Hold-to-Quit, MenuAnywhere and any added later). All files and the `core.status` file states are recorded in `samples.json`.

### Surface and visibility

- With Bar shipped: Bar on the primary display at its configured size and cadence; the `no-window` scenario sets Bar's `enabled = false`.
- Without Bar: the test window. For **each** `surface-visible` trial the operator is prompted to click **Open Test Window** in the tray once; the tool times out (default 60 s) and cleans up and excludes the trial ([exclude and replace](#method)) if the surface is not detected.
- **Surface detection is per target kind** and recorded per trial with its rule:
  - `next`: the attributed WebContent helper appears ([WebKit helper attribution](#webkit-helper-attribution)). Its PID, parent PID and appearance time are recorded.
  - `spike` (`--window surface`): the `spike: ready` stderr line, which the spike writes only after the window is created and shown (part 02). The spike spawns no WebKit helper.
  - The two definitions measure different events, so surface detection and startup times are not directly comparable across target kinds.

### WebKit helper attribution

WKWebView's WebContent, GPU and Networking helpers are XPC services parented by `launchd` (PPID 1), so they never appear in the host's process tree. They are attributed by a quiet-system snapshot diff; no `sudo` and no private API.

- **Quiet system.** `--help` and the per-run operator prompt require quitting every other WebKit client (Safari, Mail and any other WKWebView app) before the run. The tool refuses while legacy Stache or a spike runs ([collision refusal](#targets-and-safety)).
- **Pre-launch set.** Immediately before each launch the tool records the set of WebKit helper PIDs (the `WEBKIT_PROCESS` pattern) with their cumulative CPU time.
- **Attribution window.** It starts at launch for `next` with Bar and for `spike`, and at the operator prompt for the test window; it ends at the end of the idle window. Helpers absent from the pre-launch set that appear inside it are attributed to the target.
- **WebContent calibration.** The first owner-approved run records how many new WebContent helpers one surface creates on this WebKit version (prewarmed or cached helpers included), together with the WebKit and macOS versions, in `samples.json` and `report.md`: without a calibration input, the count from the first `surface-visible` trial that passes every other guard becomes the run's calibrated value, and the calibration is recorded only if every valid `surface-visible` trial of that run has the same count (a polluted first trial therefore excludes the clean trials that follow, and the run ends `FAILED` instead of silently contaminated). Later runs take it as `--webcontent-per-surface <n> --webcontent-calibration <path to the calibrating run's samples.json>`; the tool exits with a usage error when that file's WebKit version, macOS version or surface (test window versus Bar) differs from the current run, so an OS update or a switch to Bar requires recalibrating. Where the optional corroboration below is available, the calibrating trial is checked against it. Prewarmed or cached helpers that appear inside the attribution window are attributed to the target; they are its cost.
- **Churn guard.** A trial is excluded as `excluded (helper attribution ambiguous: <reason>)` ([exclude and replace](#method)) when:
  - a new helper appears between launch and the start of the attribution window (test window only);
  - any new helper appears between launch and the end of the idle window in a `no-window` trial, or in any `spike` trial (GPUI uses no WebKit);
  - the number of new WebContent helpers differs from the calibrated count per surface, in either direction (`WebContent count <n> ≠ calibrated <c>`) (Bar counts one surface, on the primary display), or more than one new GPU or Networking helper appears.
- **Pre-existing helper CPU warning.** A pre-existing helper whose cumulative CPU time grows by more than a threshold (default 0.1 s over the trial, `--helper-cpu-warning-seconds`) is recorded as a per-trial warning in `samples.json` and `report.md`, never a failure or exclusion. XPC helpers are per client app, so a pre-existing helper cannot serve the target and its CPU is never counted; its growth is system noise that the envelope already captures.
- **Optional corroboration.** During implementation, check whether `launchctl print pid/<host pid>` (no `sudo`) lists the host's XPC helpers, and whether `lsappinfo` reports the helper with a host-derived display name (`<Host> Web Content`) or the host's ASN as its parent. For each source that does, record its output per trial as no-`sudo` corroboration; neither is required and neither overrides the snapshot diff.
- **Sampler.** `scripts/measure/ui/sampler.ts` samples, on every interval, the host tree plus every attributed helper PID: cumulative CPU-time deltas and RSS. An attributed helper that exits during the trial is recorded with its observed interval.
- **Wakeups.** The metric is `top`'s per-process `idlew` column (`power` optionally recorded alongside). Because helpers appear mid-trial, a fixed `-pid` list cannot be used: the tool runs one all-process `top -l <k+1> -s <interval> -stats pid,idlew` per idle window, keeps only rows for the host-tree and attributed PIDs, and discards the first sample, which has no interval delta. The exact command is recorded.
- The window's actual frame, display, scale and content update cadence are recorded. A static window is recorded as cadence `none`. Part 02 must use this recorded cadence.
- **Open-to-content is unverified** for Stache Next release builds: the only page-loaded hook is `smoke`-only (overview context). Without OQ-2 approval, the report states "window open to content: unverified (no release signal)" and records only the time from the prompt to WebContent appearance, labelled as a detection time, not a render time. First rendered frame is unverified.

### Metrics per trial

- **Headline: app total** = host tree + attributed WebKit helpers, for RSS (final and peak), CPU and wakeups, with a per-process breakdown (host, each descendant, each attributed helper by role).
- CPU over the idle window from cumulative `ps` CPU time deltas, per process and summed.
- Wakeups per second per process from `top`'s `idlew` column ([Wakeups](#webkit-helper-attribution)), with the exact command and sampling interval recorded.
- Attribution record: the pre-launch helper set, attribution window bounds, attributed helpers, the WebContent calibration value, any exclusion reason and any pre-existing helper CPU warning.
- Startup: `startup: tray ready after N ms` (`next` only) and external launch-to-ready per the readiness rule.
- Gate state (`next`), readiness rule, cadence, surface detection time and its per-target rule.

### Method

- Scenarios: `no-window` and `surface-visible`, so the surface's own cost (app total, helpers included) is a delta. Part 02 runs the same two scenarios with `--window none|surface` and may add separately labelled extra scenarios.
- **N ≥ 5 idle trials per scenario** (default 5). Every trial is a fresh launch in a fresh temporary root, followed by the fixed warmup (default 10 s; for `surface-visible` it starts at the surface-detection event of the target kind: the attributed WebContent helper for `next`, the `spike: ready` line for `spike`) and the full idle window (default 60 s). No trial reuses another trial's process.
- **Exclude and replace.** A trial excluded by the [churn guard](#webkit-helper-attribution), a readiness timeout or a surface-detection timeout is cleaned up, kept in `samples.json` and `report.md` with its reason, and replaced by a fresh trial, up to an attempt cap of 2N trials per scenario. Exclusion is never based on metric values. The run is `FAILED` only when a scenario ends with fewer than N valid trials. Safety refusals (collision refusal, ownership check, unreadable Launch Services or process table), Accessibility gate violations and incomplete cleanup are not replaced: they fail the run immediately, as do interruptions (`INTERRUPTED`). Only valid trials enter medians and envelopes.
- **Per-metric noise envelope:** for each scenario and each metric, the max − min across that scenario's trials. Report median, min, max and envelope per metric; there is no single global noise number.
- Recorded once per run: power source, hardware model, macOS build, display list, commit and dirty state (reused `collectEnvironment`).

### Output

- `report.md` and `samples.json` under `docs/specs/2026-11-10-gpui-evaluation/measurements/<UTC timestamp>/`, formatted by the repository formatter.
- Partial runs keep their samples and are marked `FAILED` or `INTERRUPTED`; exit codes match the foundation tool (0 completed and clean, 1 failure or incomplete cleanup, 2 usage error, 130 interrupted).
- The Evidence section below links the accepted run and copies its summary table. No number is written that is not in `samples.json`.

## Acceptance criteria

- [ ] GE-01-01: `bun run measure:ui -- --help` prints flags, readiness rules, prerequisites (trusted bundle, release build, every other WebKit client quit), safety rules and the guarded command, with no side effects.
- [ ] GE-01-02: Script unit tests cover: argument validation (debug path rejected, missing target, unknown readiness rule, no legacy flag, bundle identifier `com.marcosmoura.stache` rejected, unknown bundle identifier rejected); readiness parsing for both rules and the timeout; spike collision refusal by exact path and PID (a same-named process at another path does not match); gate handling (prefixed `stache: <level> host.<op>: access: …` lines matched, `checking` at readiness then `open` at end of warmup passes, guidance line or non-`open` end-of-warmup state fails); pre-launch collision refusal on any `next`, `legacy` or spike entry and post-launch ownership; helper snapshot diff (a `launchd`-parented helper that appears in the window is attributed; one present before launch is not; window start at launch versus at the prompt); each churn-guard condition excluding the trial as `helper attribution ambiguous`; WebContent calibration (first valid `surface-visible` count recorded, prewarmed helpers inside the window attributed, counts above and below the calibrated value both excluded, a polluted first trial leading to a `FAILED` run, a `--webcontent-calibration` file with a different WebKit version, macOS version or surface rejected as a usage error); the pre-existing helper CPU growth recorded as a warning only, never an exclusion; exclude and replace (excluded trials kept with reasons and replaced, a scenario reaching the 2N attempt cap with fewer than N valid trials fails the run, safety refusals and gate violations fail the run without replacement); surface detection per target kind and warmup starting at it; app totals and per-process breakdown; wakeup parsing of the `idlew` column from a recorded all-process `top -l` fixture (first sample discarded, rows filtered to host-tree and attributed PIDs) and CPU parsing from recorded `ps` fixtures; the `next` guard target built from `DEFAULT_NEXT_APP` or `--next-app` for spike runs; per-metric envelope and summary; partial-run report writing and exit codes.
- [ ] GE-01-03: No existing export in `scripts/measure/*` changes signature or behavior; the foundation `bun run measure -- --help` output, its tests and `scripts/check-next-instance.ts` (which imports `paths.ts` and `process.ts`) are unaffected. Any shared-module change was preceded by its own foundation regression tests.
- [ ] GE-01-04: One owner-approved run on a trusted release bundle produced `report.md` and `samples.json` with ≥ 5 valid trials per scenario, every excluded trial with its reason, the WebContent calibration value with the WebKit and macOS versions, gate state per trial, all metrics above (or an explicit **unverified** entry with its reason), and the Evidence section links it.
- [ ] GE-01-05: `bun run lint`, `bun run format:check` and `bun run test` pass.
- [ ] GE-01-06: [guides/performance.md](../../guides/performance.md) documents the UI measurement, its flags, readiness rules, the gate requirement and the quiet-system precondition and its limitations (attribution by quiet-system snapshot diff; WindowServer compositing is not attributed on either side; unverified open-to-content and first frame).

## Tests

Vitest script tests next to the new modules (`scripts/measure/ui/*.test.ts`, run by `bun run test:scripts`) with recorded fixtures and injected queries; no test launches an app, opens a window or runs `top`/`ps` for real. Existing `scripts/measure/*.test.ts` stay unchanged. The real run is manual, owner-approved and recorded in `measurements/`.

## Evidence

None yet. Unverified until the approved run.

## Notes

- Rollback: delete `scripts/measure-ui.ts`, `scripts/measure/ui/` and the `measure:ui` script entry; the foundation tool is untouched.
- If Bar ships during this part, re-run against Bar; a test-window baseline alone is then insufficient for part 03.
