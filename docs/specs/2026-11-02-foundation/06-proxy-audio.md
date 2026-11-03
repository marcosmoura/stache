---
status: done
---

# 06 — proxyAudio pilot and measurements

Read when porting audio selection or measuring foundation cost against legacy.

## Scope and files

Complete src/crates/modules/proxy-audio/src/{lib,config,device,priority,watcher,list,cli,ipc}.rs;
create src/crates/macos/src/{audio,display}.rs, host registration/typed list DTO exports,
a reusable TypeScript tool scripts/measure-foundation.ts (run with Bun) and tests. Commit report/raw samples under this spec's measurements/.
Pilot DTO/client generation targets `src/ui/src/sdk/bindings.gen.ts`; no pilot UI module is added.

## Key APIs and requirements

- ProxyAudioModule implements Module<Config = ProxyAudioConfig>, ID proxyAudio, no permissions,
  cancellable named audio worker, no external dependencies/UI. Safe AudioBackend offers enumerate,
  default/set-default, transport checks and RAII change subscription; fake backend tests policy.
- Worker reads `_legacy/app/native/src/modules/audio/`, config/types/audio.rs and
  `_legacy/docs/features/audio.md`, ports actual code/tests, not just summary. Preserve ordered
  priorities; case-insensitive exact/contains/startsWith, case-sensitive regex, same-direction dependsOn.
  Validate malformed regex before config application so invalid reload preserves working state.
- Output order: mirrored AirPlay, sticky current AirPlay, HDMI when current is built-in, first eligible
  configured device, MacBook Pro fallback. Input omits HDMI, uses microphone fallback then current.
  Preserve no-candidate behavior, type/transport detection/HDMI fallback. IDs are ephemeral; match names.
- All audio/display unsafe code is in macos crate. Listen to Devices/DefaultInputDevice/
  DefaultOutputDevice with partial-registration rollback. Callback only sends bounded/coalesced wakeup;
  worker enumerates and sets target only when changed. Port software-not-hardware mirroring detection
  from platform/display.rs; no new polling priority algorithm.
- Start applies once, owns one listener generation/worker. Stop/pause/disable unregister exact
  address/callback/data, quiesce callbacks, cancel/join safely. Reconfigure prepares/swaps snapshot,
  reevaluates once, never duplicates listeners. Invalid candidate preserves old worker/config;
  paused reload updates config without starting.
- Contribute `audio list [-j/--json] [-i/--input] [-o/--output]`; none/both directions select all.
  Standalone enumeration works with desktop absent/disabled/paused, never starts listeners. Preserve
  AudioDeviceInfo keys (including type), name sort, null/empty behavior, rounded table/columns/truncation
  from list.rs/CLI output. JSON obeys TTY policy. Typed proxyAudio.list IPC uses same read-only service.
  No device-mutation command or proxyAudio webview.

## Performance protocol

Script drives release binaries with explicit isolated config directories/identities. Save machine
model/chip/RAM, macOS, commit/toolchain/profile, power/display/audio state, configs and raw samples.
Disable all other configurable legacy tools; record unavoidable legacy tray/webviews/services as
baseline overhead rather than pretending they were disabled. Never run both device-changing watchers:
refuse active other app, stop/wait for entire process tree between runs, restore defaults/configs on
success/error. Manual real-device tests use the same exclusivity guard; never run them concurrently.

Approved adjustment: the daily legacy app may remain running with its proxyAudio disabled for
Next-only runs. Measuring legacy still requires stopping other legacy instances. A07 uses the
user-approved observational comparison below instead of the isolated legacy protocol.

At least five launches: instrument monotonic host main-entry→tray-created, separately external process
launch/exec→tray-ready acknowledgement. Report median/range and warm/cold context, not one mixed timing.
After warmup, sample idle CPU and RSS over 60 seconds for host and all attributable children, including
WebKit/network/GPU; save process trees, flag shared/unattributable processes, explain RSS shared-page
double counting, report parent and tree totals. Instrument webview count as well as observing processes;
process names alone do not prove count. Run pilot enabled and disabled without requesting any window.

Provisional targets: ≈0% average idle CPU, lower tree RSS than legacy, host tray-ready <300 ms release,
zero webviews. State CPU noise/precision in report; do not invent a numeric budget before measuring.
Report actual misses/causes for explicit orchestrator/user decision before A07 is done, never quietly
relax targets. External process-launch timing is reported separately, not promised <300 ms.

## Acceptance criteria

- [x] A01: Pilot registers solely through SDK, no globals/unsafe/cross-module imports/audio SDK switch;
      legacy priority/matching cases pass on fake backend.
- [x] A02: Listener rollback/callback ownership/worker teardown pass failpoint/race tests; lifecycle
      never leaves duplicate watcher generations.
- [x] A03: Real hot reload changes priorities; invalid reload retains operation/diagnostics, tray and
      paused reload obey shared lifecycle rules.
- [x] A04: Port audio tests from legacy mod.rs/audio.rs; flags/table/JSON goldens and standalone
      no-desktop/no-listener execution match contract.
- [x] A05: Manual macOS checks cover connect/disconnect/defaults/AirPlay/mirroring/HDMI where available;
      hardware gaps are recorded, not claimed tested.
- [x] A06: Repeatable script captures named-machine release/raw timing/60-second CPU/tree RSS/webview
      evidence, forbids dual watchers and restores environment even on failure.
- [x] A07: Committed comparison distinguishes timing and memory scopes, records targets met or an
      explicit approved resolution for misses; zero-webview invariant holds with pilot enabled/disabled.

## Manual hardware verification (A05)

Run on 2026-11-03 on the user's MacBook Pro (`MacBookPro18,3`, macOS 27.0) with the debug build, isolated config
directories and legacy proxyAudio disabled, so only one device-changing watcher ran.

- Automated by the orchestrator: start-up switch to the configured output; hot reload to a
  `startsWith` entry switches without a new listener generation; an invalid regex keeps the device
  and reports the diagnostic in `config path`; disabling stops without switching; re-enabling starts
  generation 2 and switches; SIGTERM exits cleanly and removes the socket.
- Run by the user: manual override in Control Center switches back; wired headphones
  connect/disconnect; Bluetooth connect/disconnect; sticky current AirPlay; AirPlay preferred while
  screen mirroring; tray Enable/Disable, Pause/Resume and Open Test Window.
- **Not tested:** HDMI promotion over built-in speakers (no HDMI audio display used). Covered only by
  the ported fake-backend priority tests.
- Found and fixed during this run: desktop mode held the stdout/stderr locks for the whole session,
  so the first background log line deadlocked the host (`96aa826`).

## Measurement results (A06/A07)

Measured on 2026-11-03 on the same machine with the release bundle and `bun run measure`; the
user's legacy Stache stayed running with its proxyAudio disabled. Reports and raw samples:
`measurements/2026-11-03T01-55-04-092Z/` and `measurements/2026-11-03T02-02-50-985Z/`.

| Metric                                  | proxyAudio off (run 1 / run 2) | proxyAudio on (run 1 / run 2) |
| --------------------------------------- | ------------------------------ | ----------------------------- |
| Host main entry → tray, warm median     | 133 / 152 ms                   | 127 / 125 ms                  |
| Launch → `core.status` ack, warm median | 197 / 215 ms                   | 257 / 257 ms                  |
| Idle tree CPU over 60 s                 | 0.10 / 0.13 %                  | 0.07 / 0.13 %                 |
| Idle tree RSS (one process)             | 72.8 / 76.0 MiB                | 69.3 / 76.8 MiB               |
| Webviews (instrumented and observed)    | 0 / 0                          | 0 / 0                         |

Independent check without the tool (`ps`, `footprint`, `top` on a manually launched release
bundle): 0.08 s CPU over 60 s (≈0.13 %) in both modes, RSS 76–77 MiB, physical footprint 14–15 MB,
no child processes. RSS and physical footprint are different accounting measures; these readings
do not establish that the RSS difference consists solely of shared framework pages.

The two captures used the pre-upgrade Tauri 2.11.5 release binary, built from a dirty worktree.
The second ran on battery power. Their historical guard messages incorrectly say no legacy
process existed; legacy was allowed to remain running and was not included in the Next totals.
Raw samples are retained unchanged rather than rewriting that collector output.

### Final dependency-aligned release

The final capture, `measurements/2026-11-03T02-35-11-032Z/`, uses Rust Tauri `=2.12.4`,
`tauri-build =2.7.3`, JavaScript API `2.12.4` and collector commit `1a91331`. It ran on battery
power at 25%. The dirty-worktree flag reflects pending measurement/spec documentation and the
user's editor settings, not uncommitted runtime or collector code.

| Metric                                              | proxyAudio off | proxyAudio on |
| --------------------------------------------------- | -------------- | ------------- |
| Main entry → tray, warm median                      | 131 ms         | 130 ms        |
| Launch → `core.status` acknowledgement, warm median | 200 ms         | 263 ms        |
| Idle CPU over 60 s                                  | 0.13%          | 0.12%         |
| Idle tree RSS (one process)                         | 75.6 MiB       | 76.5 MiB      |
| Webviews / observed WebKit descendants              | 0 / 0          | 0 / 0         |

Both scenarios completed five launches and the full idle window. The collector recorded the
running legacy PID honestly, removed its temporary roots, and verified unchanged audio defaults.
The raw report continues to label the isolated legacy RSS target **NOT MEASURED**; the approval
above resolves that acceptance criterion without changing the collected data into a pass.

### Final validation

- Frozen Bun install, lint, formatting, generated-artifact checks, build and release bundle passed.
- 472 native tests and 24 WebKit tests passed; browser coverage passed all configured thresholds.
- Rustdoc with warnings denied and smoke-feature Clippy passed. The smoke-feature unit/failure
  selection passed 57 tests.
- Real release smoke passed on Tauri 2.12.4: zero launch webviews, AppKit main-thread execution,
  activation observer, actual WebKit invoke/events at disabled config revision 2, window reuse/close,
  last-window-close liveness, second-instance refusal and owned SIGTERM teardown. Audio defaults
  were checked before/after and did not change. Generic smoke now keeps proxyAudio disabled.
- Eight disposable offline collector checks passed, including interruption without a second
  launch, repeated signals, partial failure reports, failed cleanup, retained live-tree roots,
  fail-closed Launch Services inspection and automatic output formatting.
- Release signature verification passed. The bundle is ad-hoc signed, not notarized.
- AFT diagnostics were incomplete for this linked worktree; compiler/linter/test gates above
  provide the validation evidence instead.

HDMI hardware remains untested. The known native Cmd-Q/logout teardown gap remains a separate
follow-up; tray Quit and SIGTERM use the coordinated teardown.

Targets: tray ready < 300 ms, ≈0 % idle CPU and zero webviews are met. **Lower tree RSS than
legacy — approved resolution (user, 2026-11-03):** no isolated legacy baseline was run, because it
requires quitting the daily legacy app. The agreed comparison is the observed daily legacy instance
(its own config, tools enabled): about 90 MiB host RSS plus a 20 MiB perl helper and about 120 MiB
of WebKit processes, against 70–77 MiB for one Stache Next process. This is an observation, not a
like-for-like measurement.

## Tests and validation

Port device/priority/list/config tests; fake every listener-add failure, worker spawn/set-default
failure, callback during stop and repeated reload. No device-changing CI/parallel tests; opt-in real
tests require exclusive guard. Extend binding/schema drift tests for pilot DTOs. Run `bun run lint`,
`bun run test`, `bun run format:check`, `bun run build`, `bun run tauri:build`,
`bun run generate-schema -- --check`, `bun run generate-bindings -- --check`,
`bun run measure -- --help`, then documented guarded release measurement command.

## Docs

Write docs/features/proxy-audio.md and docs/guides/performance.md; finish new-module recipe with pilot
example. Update index/architecture/testing docs to shipped behavior. Commit numbers/commands/hardware
gaps/budget decisions in measurement report, not merely the worker summary.
