# Foundation measurement — 2026-11-03T01:55:04.092Z

Read when comparing Stache Next release startup, idle cost and process trees against the legacy application.

- Commit: `96aa8263b16631dc94128b113d2d2771ac3ad0f7` on `feat/foundation`
- Uncommitted changes: yes
- Measured at: 2026-11-03T01:55:04.092Z (duration 149911 ms)
- Command: `/Users/marcosmoura/.bun/bin/bun /Users/marcosmoura/Projects/stache/.worktrees/foundation/scripts/measure-foundation.ts`

## Environment

| Item              | Value                                                                                                                                                                                            |
| ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Machine           | MacBookPro18,3                                                                                                                                                                                   |
| Chip              | Apple M1 Pro                                                                                                                                                                                     |
| Memory            | 32 GiB                                                                                                                                                                                           |
| macOS             | 27.0 (26A428)                                                                                                                                                                                    |
| Rust              | rustc 1.100.0-nightly (34baba539 2026-08-16)                                                                                                                                                     |
| Cargo             | cargo 1.100.0-nightly (8a0d8afba 2026-08-15)                                                                                                                                                     |
| Stache Next build | `/Users/marcosmoura/Projects/stache/.worktrees/foundation/target/release/bundle/macos/Stache Next.app/Contents/MacOS/stache`, 4.6 MiB, modified 2026-11-03T01:55:01.804Z; /release/ in path: yes |
| Power             | Battery Power, 43%                                                                                                                                                                               |
| Displays          | Color LCD (1920 x 1200 @ 120.00Hz; main, online)                                                                                                                                                 |
| Default input     | MacBook Pro Microphone                                                                                                                                                                           |
| Default output    | MacBook Pro Speakers                                                                                                                                                                             |

### Audio devices

| Device                 | Transport                     | Input | Output | Default input | Default output | Class    |
| ---------------------- | ----------------------------- | ----- | ------ | ------------- | -------------- | -------- |
| MacBook Pro Microphone | coreaudio_device_type_builtin | 1     |        | yes           |                | built-in |
| MacBook Pro Speakers   | coreaudio_device_type_builtin |       | 2      |               | yes            | built-in |
| External Speakers      | coreaudio_device_type_virtual |       | 2      |               |                |          |

## Method

- Tray-ready (`external`): new host until the control socket answers `core.status`; legacy until its own `setup complete (background tasks spawned)` stderr line.
- External readiness is polled every 25 ms; the value includes process creation.
- Tray-created (`internal`): one host log line `startup: tray ready after N ms, webviews=N`, timed from process main entry with `Instant`.
- Idle sampling: 5000 ms process-tree snapshots of the host and every transitive child.
- CPU: average from `ps` cumulative CPU time deltas; instantaneous `%cpu` is not used.
- The sampler runs `ps` outside the measured tree; its own CPU is not attributed.
- CPU precision: `ps` time resolution is 10 ms, which is about 0.017 percentage points over the 60-second window for one process.
- RSS: sum of resident sizes. Shared pages are counted once per process, so tree RSS is an upper bound for distinct physical memory.
- `cold` is the first launch in a scenario with an empty isolated cache; `warm` is a later launch.
- External launch-to-ready timing includes process creation and is reported separately; the <300 ms target applies only to the internal host measurement.

## Stache Next - proxyAudio disabled

- Target: `/Users/marcosmoura/Projects/stache/.worktrees/foundation/target/release/bundle/macos/Stache Next.app/Contents/MacOS/stache`
- Bundle identifier: com.marcosmoura.stache.next
- Build: 4.6 MiB executable, modified 2026-11-03T01:55:01.804Z; /release/ in path: yes
- 5 timed launches; idle sample uses the last launch after 10 s warmup.
- No window is requested; the host aborts setup if any webview exists.

### Configuration used

`/private/var/folders/__/2mbmhrk92q75446h4ln2lymr0000gn/T/opencode/5rm5/config/stache/global.jsonc`

```json
{
  "logging": {
    "level": "info",
    "debug": false
  },
  "execOnStartup": []
}
```

`/private/var/folders/__/2mbmhrk92q75446h4ln2lymr0000gn/T/opencode/5rm5/config/stache/proxyAudio.jsonc`

```json
{
  "enabled": false,
  "input": [],
  "output": []
}
```

### Launch timings

| #   | Context | External launch -> ready | Internal main -> tray | Webviews | Ready source                        |
| --- | ------- | ------------------------ | --------------------- | -------- | ----------------------------------- |
| 1   | cold    | 235 ms                   | 172 ms                | 0        | core.status over the control socket |
| 2   | warm    | 192 ms                   | 128 ms                | 0        | core.status over the control socket |
| 3   | warm    | 203 ms                   | 138 ms                | 0        | core.status over the control socket |
| 4   | warm    | 189 ms                   | 125 ms                | 0        | core.status over the control socket |
| 5   | warm    | 218 ms                   | 151 ms                | 0        | core.status over the control socket |

- External launch -> ready: warm median 197 ms, range 189 ms–218 ms, cold 235 ms.
- Internal main -> tray: warm median 133 ms, range 125 ms–151 ms, cold 172 ms.

### Idle sample

- Window: 60.0 s (13 snapshots)
- Host CPU average: 0.10 %
- Tree CPU average: 0.10 %
- Host RSS: final 72.8 MiB, peak 72.9 MiB
- Tree RSS: final 72.8 MiB, peak 72.9 MiB (1 process(es))
- RSS is the sum of resident sizes; shared pages are counted once per process.

| PID   | PPID  | Role | CPU    | RSS final | RSS peak | Coverage      | Executable                                                                                                                 |
| ----- | ----- | ---- | ------ | --------- | -------- | ------------- | -------------------------------------------------------------------------------------------------------------------------- |
| 41170 | 41058 | host | 0.10 % | 72.8 MiB  | 72.9 MiB | all snapshots | /Users/marcosmoura/Projects/stache/.worktrees/foundation/target/release/bundle/macos/Stache Next.app/Contents/MacOS/stache |

#### WebKit processes outside the measured tree

Observed in the same user session but not attributable through the process tree; they may be shared with other applications and are excluded from tree totals.

- pid 19112 (ppid 1), RSS 28.3 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.GPU.xpc/Contents/MacOS/com.apple.WebKit.GPU`
- pid 19113 (ppid 1), RSS 81.1 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent`
- pid 19114 (ppid 1), RSS 9.8 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.Networking.xpc/Contents/MacOS/com.apple.WebKit.Networking`
- pid 19115 (ppid 1), RSS 14.4 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent`
- pid 35712 (ppid 1), RSS 88.9 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent`
- pid 87612 (ppid 1), RSS 27.2 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.GPU.xpc/Contents/MacOS/com.apple.WebKit.GPU`
- pid 87615 (ppid 1), RSS 10.4 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.Networking.xpc/Contents/MacOS/com.apple.WebKit.Networking`

## Stache Next - proxyAudio enabled with inert priorities

- Target: `/Users/marcosmoura/Projects/stache/.worktrees/foundation/target/release/bundle/macos/Stache Next.app/Contents/MacOS/stache`
- Bundle identifier: com.marcosmoura.stache.next
- Build: 4.6 MiB executable, modified 2026-11-03T01:55:01.804Z; /release/ in path: yes
- 5 timed launches; idle sample uses the last launch after 10 s warmup.
- No window is requested; the host aborts setup if any webview exists.

### Configuration used

`/private/var/folders/__/2mbmhrk92q75446h4ln2lymr0000gn/T/opencode/PSlD/config/stache/global.jsonc`

```json
{
  "logging": {
    "level": "info",
    "debug": false
  },
  "execOnStartup": []
}
```

`/private/var/folders/__/2mbmhrk92q75446h4ln2lymr0000gn/T/opencode/PSlD/config/stache/proxyAudio.jsonc`

```json
{
  "enabled": true,
  "input": [
    {
      "name": "MacBook Pro Microphone",
      "strategy": "exact"
    }
  ],
  "output": [
    {
      "name": "MacBook Pro Speakers",
      "strategy": "exact"
    }
  ]
}
```

### Launch timings

| #   | Context | External launch -> ready | Internal main -> tray | Webviews | Ready source                        |
| --- | ------- | ------------------------ | --------------------- | -------- | ----------------------------------- |
| 1   | cold    | 287 ms                   | 135 ms                | 0        | core.status over the control socket |
| 2   | warm    | 264 ms                   | 123 ms                | 0        | core.status over the control socket |
| 3   | warm    | 252 ms                   | 131 ms                | 0        | core.status over the control socket |
| 4   | warm    | 262 ms                   | 131 ms                | 0        | core.status over the control socket |
| 5   | warm    | 252 ms                   | 117 ms                | 0        | core.status over the control socket |

- External launch -> ready: warm median 257 ms, range 252 ms–264 ms, cold 287 ms.
- Internal main -> tray: warm median 127 ms, range 117 ms–131 ms, cold 135 ms.

### Idle sample

- Window: 60.0 s (13 snapshots)
- Host CPU average: 0.07 %
- Tree CPU average: 0.07 %
- Host RSS: final 69.3 MiB, peak 76.7 MiB
- Tree RSS: final 69.3 MiB, peak 76.7 MiB (1 process(es))
- RSS is the sum of resident sizes; shared pages are counted once per process.

| PID   | PPID  | Role | CPU    | RSS final | RSS peak | Coverage      | Executable                                                                                                                 |
| ----- | ----- | ---- | ------ | --------- | -------- | ------------- | -------------------------------------------------------------------------------------------------------------------------- |
| 43069 | 41058 | host | 0.07 % | 69.3 MiB  | 76.7 MiB | all snapshots | /Users/marcosmoura/Projects/stache/.worktrees/foundation/target/release/bundle/macos/Stache Next.app/Contents/MacOS/stache |

#### WebKit processes outside the measured tree

Observed in the same user session but not attributable through the process tree; they may be shared with other applications and are excluded from tree totals.

- pid 19112 (ppid 1), RSS 24.3 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.GPU.xpc/Contents/MacOS/com.apple.WebKit.GPU`
- pid 19113 (ppid 1), RSS 75.1 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent`
- pid 19114 (ppid 1), RSS 7.4 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.Networking.xpc/Contents/MacOS/com.apple.WebKit.Networking`
- pid 19115 (ppid 1), RSS 13.5 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent`
- pid 35712 (ppid 1), RSS 79.5 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent`
- pid 87612 (ppid 1), RSS 23.8 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.GPU.xpc/Contents/MacOS/com.apple.WebKit.GPU`
- pid 87615 (ppid 1), RSS 9.6 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.Networking.xpc/Contents/MacOS/com.apple.WebKit.Networking`

## Provisional targets

| Target                                                          | Status       | Evidence                                                                                                                                                                                                          |
| --------------------------------------------------------------- | ------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Host main-entry -> tray-created < 300 ms (release, warm median) | MET          | Stache Next - proxyAudio disabled: warm median 133 ms (range 125 ms–151 ms, cold 172 ms); Stache Next - proxyAudio enabled with inert priorities: warm median 127 ms (range 117 ms–131 ms, cold 135 ms)           |
| ~0% average idle CPU (noise threshold < 1%)                     | MET          | Stache Next - proxyAudio disabled: tree average 0.10 %, host 0.10 %; Stache Next - proxyAudio enabled with inert priorities: tree average 0.07 %, host 0.07 %                                                     |
| Zero webviews with pilot enabled and disabled                   | MET          | Stache Next - proxyAudio disabled: instrumented 0 webview(s), 0 WebKit descendant(s) observed; Stache Next - proxyAudio enabled with inert priorities: instrumented 0 webview(s), 0 WebKit descendant(s) observed |
| Lower idle tree RSS than legacy                                 | NOT MEASURED | legacy baseline was not requested or produced no idle sample                                                                                                                                                      |

The "≈0% idle CPU" target has no numeric budget in the spec. The tool reports the raw average and uses < 1% only as a noise-level label; it does not relax any target.

## Misses

None.

## Safety record

- Guard checks: no other Stache Next or legacy Stache process at 2026-11-03T01:55:04.759Z; no other Stache Next or legacy Stache process at 2026-11-03T01:55:05.015Z; no other Stache Next or legacy Stache process at 2026-11-03T01:55:05.781Z; no other Stache Next or legacy Stache process at 2026-11-03T01:55:06.497Z; no other Stache Next or legacy Stache process at 2026-11-03T01:55:07.213Z; no other Stache Next or legacy Stache process at 2026-11-03T01:55:07.919Z; no other Stache Next or legacy Stache process at 2026-11-03T01:56:19.337Z; no other Stache Next or legacy Stache process at 2026-11-03T01:56:20.138Z; no other Stache Next or legacy Stache process at 2026-11-03T01:56:20.917Z; no other Stache Next or legacy Stache process at 2026-11-03T01:56:21.740Z; no other Stache Next or legacy Stache process at 2026-11-03T01:56:22.561Z
- Temporary roots: /private/var/folders/**/2mbmhrk92q75446h4ln2lymr0000gn/T/opencode/5rm5, /private/var/folders/**/2mbmhrk92q75446h4ln2lymr0000gn/T/opencode/PSlD
- Temporary roots removed: yes
- Default audio devices: start input MacBook Pro Microphone, output MacBook Pro Speakers, end input MacBook Pro Microphone, output MacBook Pro Speakers
- Audio restore: not needed
- Signals: SIGTERM 41110 (stop measured process tree); SIGTERM 41124 (stop measured process tree); SIGTERM 41135 (stop measured process tree); SIGTERM 41148 (stop measured process tree); SIGTERM 41170 (stop measured process tree); SIGTERM 43025 (stop measured process tree); SIGTERM 43038 (stop measured process tree); SIGTERM 43048 (stop measured process tree); SIGTERM 43061 (stop measured process tree); SIGTERM 43069 (stop measured process tree)

## Limitations

- OS page cache is not flushed: "cold" means the first launch in a scenario with an empty isolated app cache, not necessarily the first launch after the build.
- Process-tree attribution follows parent PIDs. Helpers reparented to launchd are listed as observed WebKit processes outside the tree and excluded from tree totals.
- Legacy has no tray-created acknowledgement; its readiness is the setup-complete log line emitted after tray, bar and widget creation.
- RSS sums resident sizes per process, so shared pages are counted more than once.
- `ps` cumulative CPU time has 10 ms resolution; helper processes that start and exit inside one sampling interval may not be captured.
- The measured machine is shared: unrelated system activity can add CPU noise.
- Two measurement tool instances are not mutually excluded; run one at a time.
- Process identity is matched by Launch Services bundle identifier, the measured executable path, and the Stache Next/Stache .app paths. A bare release binary at a different path is not detected by the guard.
