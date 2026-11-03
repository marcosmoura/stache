# Foundation measurement — 2026-11-03T02:02:50.985Z

Read when comparing Stache Next release startup, idle cost and process trees against the legacy application.

- Commit: `1dfa7f03d1d0dc96416f588bd37822f3be7e98f1` on `feat/foundation`
- Uncommitted changes: yes
- Measured at: 2026-11-03T02:02:50.985Z (duration 149597 ms)
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
| Power             | Battery Power, 39%                                                                                                                                                                               |
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

`/private/var/folders/__/2mbmhrk92q75446h4ln2lymr0000gn/T/opencode/e17g/config/stache/global.jsonc`

```json
{
  "logging": {
    "level": "info",
    "debug": false
  },
  "execOnStartup": []
}
```

`/private/var/folders/__/2mbmhrk92q75446h4ln2lymr0000gn/T/opencode/e17g/config/stache/proxyAudio.jsonc`

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
| 1   | cold    | 211 ms                   | 143 ms                | 0        | core.status over the control socket |
| 2   | warm    | 208 ms                   | 143 ms                | 0        | core.status over the control socket |
| 3   | warm    | 193 ms                   | 130 ms                | 0        | core.status over the control socket |
| 4   | warm    | 232 ms                   | 170 ms                | 0        | core.status over the control socket |
| 5   | warm    | 223 ms                   | 161 ms                | 0        | core.status over the control socket |

- External launch -> ready: warm median 215 ms, range 193 ms–232 ms, cold 211 ms.
- Internal main -> tray: warm median 152 ms, range 130 ms–170 ms, cold 143 ms.

### Idle sample

- Window: 60.0 s (13 snapshots)
- Host CPU average: 0.13 %
- Tree CPU average: 0.13 %
- Host RSS: final 76.0 MiB, peak 76.0 MiB
- Tree RSS: final 76.0 MiB, peak 76.0 MiB (1 process(es))
- RSS is the sum of resident sizes; shared pages are counted once per process.

| PID   | PPID  | Role | CPU    | RSS final | RSS peak | Coverage      | Executable                                                                                                                 |
| ----- | ----- | ---- | ------ | --------- | -------- | ------------- | -------------------------------------------------------------------------------------------------------------------------- |
| 52517 | 52447 | host | 0.13 % | 76.0 MiB  | 76.0 MiB | all snapshots | /Users/marcosmoura/Projects/stache/.worktrees/foundation/target/release/bundle/macos/Stache Next.app/Contents/MacOS/stache |

#### WebKit processes outside the measured tree

Observed in the same user session but not attributable through the process tree; they may be shared with other applications and are excluded from tree totals.

- pid 19112 (ppid 1), RSS 29.9 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.GPU.xpc/Contents/MacOS/com.apple.WebKit.GPU`
- pid 19113 (ppid 1), RSS 93.3 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent`
- pid 19114 (ppid 1), RSS 9.2 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.Networking.xpc/Contents/MacOS/com.apple.WebKit.Networking`
- pid 19115 (ppid 1), RSS 13.5 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent`
- pid 35712 (ppid 1), RSS 111.4 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent`
- pid 87612 (ppid 1), RSS 27.2 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.GPU.xpc/Contents/MacOS/com.apple.WebKit.GPU`
- pid 87615 (ppid 1), RSS 10.7 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.Networking.xpc/Contents/MacOS/com.apple.WebKit.Networking`

## Stache Next - proxyAudio enabled with inert priorities

- Target: `/Users/marcosmoura/Projects/stache/.worktrees/foundation/target/release/bundle/macos/Stache Next.app/Contents/MacOS/stache`
- Bundle identifier: com.marcosmoura.stache.next
- Build: 4.6 MiB executable, modified 2026-11-03T01:55:01.804Z; /release/ in path: yes
- 5 timed launches; idle sample uses the last launch after 10 s warmup.
- No window is requested; the host aborts setup if any webview exists.

### Configuration used

`/private/var/folders/__/2mbmhrk92q75446h4ln2lymr0000gn/T/opencode/ErlL/config/stache/global.jsonc`

```json
{
  "logging": {
    "level": "info",
    "debug": false
  },
  "execOnStartup": []
}
```

`/private/var/folders/__/2mbmhrk92q75446h4ln2lymr0000gn/T/opencode/ErlL/config/stache/proxyAudio.jsonc`

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
| 1   | cold    | 272 ms                   | 132 ms                | 0        | core.status over the control socket |
| 2   | warm    | 256 ms                   | 126 ms                | 0        | core.status over the control socket |
| 3   | warm    | 258 ms                   | 134 ms                | 0        | core.status over the control socket |
| 4   | warm    | 262 ms                   | 123 ms                | 0        | core.status over the control socket |
| 5   | warm    | 249 ms                   | 121 ms                | 0        | core.status over the control socket |

- External launch -> ready: warm median 257 ms, range 249 ms–262 ms, cold 272 ms.
- Internal main -> tray: warm median 125 ms, range 121 ms–134 ms, cold 132 ms.

### Idle sample

- Window: 60.0 s (13 snapshots)
- Host CPU average: 0.13 %
- Tree CPU average: 0.13 %
- Host RSS: final 76.8 MiB, peak 76.8 MiB
- Tree RSS: final 76.8 MiB, peak 76.8 MiB (1 process(es))
- RSS is the sum of resident sizes; shared pages are counted once per process.

| PID   | PPID  | Role | CPU    | RSS final | RSS peak | Coverage      | Executable                                                                                                                 |
| ----- | ----- | ---- | ------ | --------- | -------- | ------------- | -------------------------------------------------------------------------------------------------------------------------- |
| 53372 | 52447 | host | 0.13 % | 76.8 MiB  | 76.8 MiB | all snapshots | /Users/marcosmoura/Projects/stache/.worktrees/foundation/target/release/bundle/macos/Stache Next.app/Contents/MacOS/stache |

#### WebKit processes outside the measured tree

Observed in the same user session but not attributable through the process tree; they may be shared with other applications and are excluded from tree totals.

- pid 19112 (ppid 1), RSS 30.2 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.GPU.xpc/Contents/MacOS/com.apple.WebKit.GPU`
- pid 19113 (ppid 1), RSS 91.8 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent`
- pid 19114 (ppid 1), RSS 9.2 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.Networking.xpc/Contents/MacOS/com.apple.WebKit.Networking`
- pid 19115 (ppid 1), RSS 13.5 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent`
- pid 35712 (ppid 1), RSS 120.1 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent`
- pid 87612 (ppid 1), RSS 32.6 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.GPU.xpc/Contents/MacOS/com.apple.WebKit.GPU`
- pid 87615 (ppid 1), RSS 10.7 MiB: `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.Networking.xpc/Contents/MacOS/com.apple.WebKit.Networking`

## Provisional targets

| Target                                                          | Status       | Evidence                                                                                                                                                                                                          |
| --------------------------------------------------------------- | ------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Host main-entry -> tray-created < 300 ms (release, warm median) | MET          | Stache Next - proxyAudio disabled: warm median 152 ms (range 130 ms–170 ms, cold 143 ms); Stache Next - proxyAudio enabled with inert priorities: warm median 125 ms (range 121 ms–134 ms, cold 132 ms)           |
| ~0% average idle CPU (noise threshold < 1%)                     | MET          | Stache Next - proxyAudio disabled: tree average 0.13 %, host 0.13 %; Stache Next - proxyAudio enabled with inert priorities: tree average 0.13 %, host 0.13 %                                                     |
| Zero webviews with pilot enabled and disabled                   | MET          | Stache Next - proxyAudio disabled: instrumented 0 webview(s), 0 WebKit descendant(s) observed; Stache Next - proxyAudio enabled with inert priorities: instrumented 0 webview(s), 0 WebKit descendant(s) observed |
| Lower idle tree RSS than legacy                                 | NOT MEASURED | legacy baseline was not requested or produced no idle sample                                                                                                                                                      |

The "≈0% idle CPU" target has no numeric budget in the spec. The tool reports the raw average and uses < 1% only as a noise-level label; it does not relax any target.

## Misses

None.

## Safety record

- Guard checks: no other Stache Next or legacy Stache process at 2026-11-03T02:02:51.759Z; no other Stache Next or legacy Stache process at 2026-11-03T02:02:51.986Z; no other Stache Next or legacy Stache process at 2026-11-03T02:02:52.730Z; no other Stache Next or legacy Stache process at 2026-11-03T02:02:53.450Z; no other Stache Next or legacy Stache process at 2026-11-03T02:02:54.226Z; no other Stache Next or legacy Stache process at 2026-11-03T02:02:55.049Z; no other Stache Next or legacy Stache process at 2026-11-03T02:04:06.358Z; no other Stache Next or legacy Stache process at 2026-11-03T02:04:07.145Z; no other Stache Next or legacy Stache process at 2026-11-03T02:04:07.931Z; no other Stache Next or legacy Stache process at 2026-11-03T02:04:08.704Z; no other Stache Next or legacy Stache process at 2026-11-03T02:04:09.527Z
- Temporary roots: /private/var/folders/**/2mbmhrk92q75446h4ln2lymr0000gn/T/opencode/e17g, /private/var/folders/**/2mbmhrk92q75446h4ln2lymr0000gn/T/opencode/ErlL
- Temporary roots removed: yes
- Default audio devices: start input MacBook Pro Microphone, output MacBook Pro Speakers, end input MacBook Pro Microphone, output MacBook Pro Speakers
- Audio restore: not needed
- Signals: SIGTERM 52473 (stop measured process tree); SIGTERM 52481 (stop measured process tree); SIGTERM 52494 (stop measured process tree); SIGTERM 52502 (stop measured process tree); SIGTERM 52517 (stop measured process tree); SIGTERM 53328 (stop measured process tree); SIGTERM 53336 (stop measured process tree); SIGTERM 53349 (stop measured process tree); SIGTERM 53357 (stop measured process tree); SIGTERM 53372 (stop measured process tree)

## Limitations

- OS page cache is not flushed: "cold" means the first launch in a scenario with an empty isolated app cache, not necessarily the first launch after the build.
- Process-tree attribution follows parent PIDs. Helpers reparented to launchd are listed as observed WebKit processes outside the tree and excluded from tree totals.
- Legacy has no tray-created acknowledgement; its readiness is the setup-complete log line emitted after tray, bar and widget creation.
- RSS sums resident sizes per process, so shared pages are counted more than once.
- `ps` cumulative CPU time has 10 ms resolution; helper processes that start and exit inside one sampling interval may not be captured.
- The measured machine is shared: unrelated system activity can add CPU noise.
- Two measurement tool instances are not mutually excluded; run one at a time.
- Process identity is matched by Launch Services bundle identifier, the measured executable path, and the Stache Next/Stache .app paths. A bare release binary at a different path is not detected by the guard.
