# Foundation measurement — 2026-11-03T02:35:11.032Z

Read when comparing Stache Next release startup, idle cost and process trees against the legacy application.

- Commit: `1a91331bdb3e68f7393325585059406d190fd8c4` on `feat/foundation`
- Uncommitted changes: yes
- Measured at: 2026-11-03T02:35:11.032Z (duration 149387 ms)
- Command: `/Users/marcosmoura/.bun/bin/bun /Users/marcosmoura/Projects/stache/.worktrees/foundation/scripts/measure-foundation.ts`
- Status: COMPLETED

## Environment

| Item              | Value                                                                                                                                                                                            |
| ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Machine           | MacBookPro18,3                                                                                                                                                                                   |
| Chip              | Apple M1 Pro                                                                                                                                                                                     |
| Memory            | 32 GiB                                                                                                                                                                                           |
| macOS             | 27.0 (26A428)                                                                                                                                                                                    |
| Rust              | rustc 1.100.0-nightly (34baba539 2026-08-16)                                                                                                                                                     |
| Cargo             | cargo 1.100.0-nightly (8a0d8afba 2026-08-15)                                                                                                                                                     |
| Stache Next build | `/Users/marcosmoura/Projects/stache/.worktrees/foundation/target/release/bundle/macos/Stache Next.app/Contents/MacOS/stache`, 4.7 MiB, modified 2026-11-03T02:32:59.535Z; /release/ in path: yes |
| Power             | Battery Power, 25%                                                                                                                                                                               |
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
- Build: 4.7 MiB executable, modified 2026-11-03T02:32:59.535Z; /release/ in path: yes
- 5 timed launches; idle sample uses the last launch after 10 s warmup.
- No window is requested; the host aborts setup if any webview exists.

### Configuration used

`/private/var/folders/__/2mbmhrk92q75446h4ln2lymr0000gn/T/opencode/1Yz7/config/stache/global.jsonc`

```json
{
  "logging": {
    "level": "info",
    "debug": false
  },
  "execOnStartup": []
}
```

`/private/var/folders/__/2mbmhrk92q75446h4ln2lymr0000gn/T/opencode/1Yz7/config/stache/proxyAudio.jsonc`

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
| 1   | cold    | 220 ms                   | 146 ms                | 0        | core.status over the control socket |
| 2   | warm    | 206 ms                   | 134 ms                | 0        | core.status over the control socket |
| 3   | warm    | 204 ms                   | 135 ms                | 0        | core.status over the control socket |
| 4   | warm    | 195 ms                   | 127 ms                | 0        | core.status over the control socket |
| 5   | warm    | 188 ms                   | 122 ms                | 0        | core.status over the control socket |

- External launch -> ready: warm median 200 ms, range 188 ms–206 ms, cold 220 ms.
- Internal main -> tray: warm median 131 ms, range 122 ms–135 ms, cold 146 ms.

### Idle sample

- Window: 60.0 s (13 snapshots)
- Host CPU average: 0.13 %
- Tree CPU average: 0.13 %
- Host RSS: final 75.6 MiB, peak 75.8 MiB
- Tree RSS: final 75.6 MiB, peak 75.8 MiB (1 process(es))
- RSS is the sum of resident sizes; shared pages are counted once per process.

| PID   | PPID  | Role | CPU    | RSS final | RSS peak | Coverage      | Executable                                                                                                                 |
| ----- | ----- | ---- | ------ | --------- | -------- | ------------- | -------------------------------------------------------------------------------------------------------------------------- |
| 74049 | 73951 | host | 0.13 % | 75.6 MiB  | 75.8 MiB | all snapshots | /Users/marcosmoura/Projects/stache/.worktrees/foundation/target/release/bundle/macos/Stache Next.app/Contents/MacOS/stache |

#### WebKit processes outside the measured tree

Observed in the same user session but not attributable through the process tree; they may be shared with other applications and are excluded from tree totals.

- pid 19112 (ppid 1), RSS 26.8 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.GPU.xpc/Contents/MacOS/com.apple.WebKit.GPU`
- pid 19113 (ppid 1), RSS 93.0 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent`
- pid 19114 (ppid 1), RSS 11.0 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.Networking.xpc/Contents/MacOS/com.apple.WebKit.Networking`
- pid 19115 (ppid 1), RSS 13.4 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent`
- pid 73935 (ppid 1), RSS 68.9 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent`
- pid 87612 (ppid 1), RSS 28.8 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.GPU.xpc/Contents/MacOS/com.apple.WebKit.GPU`
- pid 87615 (ppid 1), RSS 21.8 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.Networking.xpc/Contents/MacOS/com.apple.WebKit.Networking`

## Stache Next - proxyAudio enabled with inert priorities

- Target: `/Users/marcosmoura/Projects/stache/.worktrees/foundation/target/release/bundle/macos/Stache Next.app/Contents/MacOS/stache`
- Bundle identifier: com.marcosmoura.stache.next
- Build: 4.7 MiB executable, modified 2026-11-03T02:32:59.535Z; /release/ in path: yes
- 5 timed launches; idle sample uses the last launch after 10 s warmup.
- No window is requested; the host aborts setup if any webview exists.

### Configuration used

`/private/var/folders/__/2mbmhrk92q75446h4ln2lymr0000gn/T/opencode/9eOP/config/stache/global.jsonc`

```json
{
  "logging": {
    "level": "info",
    "debug": false
  },
  "execOnStartup": []
}
```

`/private/var/folders/__/2mbmhrk92q75446h4ln2lymr0000gn/T/opencode/9eOP/config/stache/proxyAudio.jsonc`

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
| 1   | cold    | 271 ms                   | 139 ms                | 0        | core.status over the control socket |
| 2   | warm    | 262 ms                   | 133 ms                | 0        | core.status over the control socket |
| 3   | warm    | 267 ms                   | 137 ms                | 0        | core.status over the control socket |
| 4   | warm    | 263 ms                   | 127 ms                | 0        | core.status over the control socket |
| 5   | warm    | 244 ms                   | 118 ms                | 0        | core.status over the control socket |

- External launch -> ready: warm median 263 ms, range 244 ms–267 ms, cold 271 ms.
- Internal main -> tray: warm median 130 ms, range 118 ms–137 ms, cold 139 ms.

### Idle sample

- Window: 60.0 s (13 snapshots)
- Host CPU average: 0.12 %
- Tree CPU average: 0.12 %
- Host RSS: final 76.5 MiB, peak 76.6 MiB
- Tree RSS: final 76.5 MiB, peak 76.6 MiB (1 process(es))
- RSS is the sum of resident sizes; shared pages are counted once per process.

| PID   | PPID  | Role | CPU    | RSS final | RSS peak | Coverage      | Executable                                                                                                                 |
| ----- | ----- | ---- | ------ | --------- | -------- | ------------- | -------------------------------------------------------------------------------------------------------------------------- |
| 74360 | 73951 | host | 0.12 % | 76.5 MiB  | 76.6 MiB | all snapshots | /Users/marcosmoura/Projects/stache/.worktrees/foundation/target/release/bundle/macos/Stache Next.app/Contents/MacOS/stache |

#### WebKit processes outside the measured tree

Observed in the same user session but not attributable through the process tree; they may be shared with other applications and are excluded from tree totals.

- pid 19112 (ppid 1), RSS 26.8 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.GPU.xpc/Contents/MacOS/com.apple.WebKit.GPU`
- pid 19113 (ppid 1), RSS 93.3 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent`
- pid 19114 (ppid 1), RSS 11.0 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.Networking.xpc/Contents/MacOS/com.apple.WebKit.Networking`
- pid 19115 (ppid 1), RSS 13.4 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent`
- pid 73935 (ppid 1), RSS 72.8 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent`
- pid 87612 (ppid 1), RSS 29.4 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.GPU.xpc/Contents/MacOS/com.apple.WebKit.GPU`
- pid 87615 (ppid 1), RSS 21.8 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.Networking.xpc/Contents/MacOS/com.apple.WebKit.Networking`

## Provisional targets

| Target                                                          | Status       | Evidence                                                                                                                                                                                                          |
| --------------------------------------------------------------- | ------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Host main-entry -> tray-created < 300 ms (release, warm median) | MET          | Stache Next - proxyAudio disabled: warm median 131 ms (range 122 ms–135 ms, cold 146 ms); Stache Next - proxyAudio enabled with inert priorities: warm median 130 ms (range 118 ms–137 ms, cold 139 ms)           |
| ~0% average idle CPU (noise threshold < 1%)                     | MET          | Stache Next - proxyAudio disabled: tree average 0.13 %, host 0.13 %; Stache Next - proxyAudio enabled with inert priorities: tree average 0.12 %, host 0.12 %                                                     |
| Zero webviews with pilot enabled and disabled                   | MET          | Stache Next - proxyAudio disabled: instrumented 0 webview(s), 0 WebKit descendant(s) observed; Stache Next - proxyAudio enabled with inert priorities: instrumented 0 webview(s), 0 WebKit descendant(s) observed |
| Lower idle tree RSS than legacy                                 | NOT MEASURED | legacy baseline was not requested or produced no idle sample                                                                                                                                                      |

The "≈0% idle CPU" target has no numeric budget in the spec. The tool reports the raw average and uses < 1% only as a noise-level label; it does not relax any target.

## Misses

None.

## Safety record

- Guard checks: no other Stache Next process (legacy Stache is not guarded without --legacy-app) at 2026-11-03T02:35:11.662Z; no other Stache Next process (legacy Stache is not guarded without --legacy-app) at 2026-11-03T02:35:11.964Z; no other Stache Next process (legacy Stache is not guarded without --legacy-app) at 2026-11-03T02:35:12.755Z; no other Stache Next process (legacy Stache is not guarded without --legacy-app) at 2026-11-03T02:35:13.464Z; no other Stache Next process (legacy Stache is not guarded without --legacy-app) at 2026-11-03T02:35:14.181Z; no other Stache Next process (legacy Stache is not guarded without --legacy-app) at 2026-11-03T02:35:14.885Z; no other Stache Next process (legacy Stache is not guarded without --legacy-app) at 2026-11-03T02:36:26.053Z; no other Stache Next process (legacy Stache is not guarded without --legacy-app) at 2026-11-03T02:36:26.849Z; no other Stache Next process (legacy Stache is not guarded without --legacy-app) at 2026-11-03T02:36:27.631Z; no other Stache Next process (legacy Stache is not guarded without --legacy-app) at 2026-11-03T02:36:28.419Z; no other Stache Next process (legacy Stache is not guarded without --legacy-app) at 2026-11-03T02:36:29.201Z
- Temporary roots: /private/var/folders/**/2mbmhrk92q75446h4ln2lymr0000gn/T/opencode/1Yz7, /private/var/folders/**/2mbmhrk92q75446h4ln2lymr0000gn/T/opencode/9eOP
- Temporary roots removed: yes
- Default audio devices: start input MacBook Pro Microphone, output MacBook Pro Speakers, end input MacBook Pro Microphone, output MacBook Pro Speakers
- Audio restore: not needed
- Signals forwarded to measured processes: SIGTERM 74001 (stop measured process tree); SIGTERM 74010 (stop measured process tree); SIGTERM 74026 (stop measured process tree); SIGTERM 74039 (stop measured process tree); SIGTERM 74049 (stop measured process tree); SIGTERM 74317 (stop measured process tree); SIGTERM 74330 (stop measured process tree); SIGTERM 74338 (stop measured process tree); SIGTERM 74346 (stop measured process tree); SIGTERM 74360 (stop measured process tree)
- A legacy Stache was running and was not measured (pid 19110 Stache (/Applications/Stache.app/Contents/MacOS/stache)); it does not block a run without --legacy-app, and its proxyAudio must stay disabled so only one device-changing watcher runs.

## Limitations

- OS page cache is not flushed: "cold" means the first launch in a scenario with an empty isolated app cache, not necessarily the first launch after the build.
- Process-tree attribution follows parent PIDs. Helpers reparented to launchd are listed as observed WebKit processes outside the tree and excluded from tree totals.
- Legacy has no tray-created acknowledgement; its readiness is the setup-complete log line emitted after tray, bar and widget creation.
- RSS sums resident sizes per process, so shared pages are counted more than once.
- `ps` cumulative CPU time has 10 ms resolution; helper processes that start and exit inside one sampling interval may not be captured.
- The measured machine is shared: unrelated system activity can add CPU noise.
- Two measurement tool instances are not mutually excluded; run one at a time.
- Process identity is matched by Launch Services bundle identifier, the measured executable path, and the Stache Next/Stache .app paths. A bare release binary at a different path is not detected by the guard.
- A running legacy Stache is recorded in the safety notes but does not block a run without --legacy-app; keep its proxyAudio disabled while measuring.
