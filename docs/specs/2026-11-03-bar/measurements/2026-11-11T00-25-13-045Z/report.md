# Bar performance measurement - 2026-11-11T00:25:13.045Z

Read when judging the Bar-on cost against the Bar-off noise envelope.

- Status: **failed** - fewer valid launches than requested in at least one arm
- Series: default (SSID path on)
- App: `/Users/marcosmoura/Projects/stache/.worktrees/tiling-bar/target/release/bundle/macos/Stache Next.app` (`/Users/marcosmoura/Projects/stache/.worktrees/tiling-bar/target/release/bundle/macos/Stache Next.app/Contents/MacOS/stache`)
- Plan: 5 valid launches per arm, warmup 10 s, sample 60 s, Bar height 28 / padding 14
- Arm order: off on on off off on on off off on
- Command: `/Users/marcosmoura/.bun/bin/bun /Users/marcosmoura/Projects/stache/.worktrees/tiling-bar/scripts/measure-bar.ts --padding 14`
- Machine: MacBookPro18,3, Apple M1 Pro; macOS 27.0 (26A428); power AC Power 85%
- Repository: fdd90b41bf25441a9774b2a12a94f1cbb25ce28f on feat/tiling-bar

## Pre-flight

Whole-machine CPU 89.2% over 10 s (limit 150%): idle enough.

## Result

Valid launches: off 4, on 5 (requested 5 per arm); invalid trials: 1.

**Insufficient valid launches.** The statistics below are incomplete and must not be used to claim the budget; the budget stays unverified.

| Metric            | Arm     | Mean | Stdev | Min  | Max  | N   |
| ----------------- | ------- | ---- | ----- | ---- | ---- | --- |
| Host CPU avg (%)  | Bar off | 0.10 | 0.00  | 0.10 | 0.10 | 4   |
| Host CPU avg (%)  | Bar on  | 0.68 | 0.24  | 0.45 | 0.97 | 5   |
| Host CPU peak (%) | Bar off | 0.21 | 0.01  | 0.20 | 0.22 | 4   |
| Host CPU peak (%) | Bar on  | 1.66 | 1.24  | 0.65 | 3.37 | 5   |
| Tree CPU avg (%)  | Bar off | 0.10 | 0.00  | 0.10 | 0.10 | 4   |
| Tree CPU avg (%)  | Bar on  | 0.69 | 0.24  | 0.45 | 0.97 | 5   |
| Idle wakeups (/s) | Bar off | 0.25 | 0.10  | 0.12 | 0.35 | 4   |
| Idle wakeups (/s) | Bar on  | 0.70 | 0.30  | 0.37 | 0.95 | 5   |

Bar-off noise envelope = the Bar-off min..max and stdev above.

- Host CPU avg: on - off = 0.58 % (spread: standard error 0.11 %); the Bar-on mean is ABOVE the largest Bar-off value (outside the recorded noise envelope).
- Host CPU peak: on - off = 1.45 % (spread: standard error 0.56 %); the Bar-on mean is ABOVE the largest Bar-off value (outside the recorded noise envelope).
- Tree CPU avg: on - off = 0.59 % (spread: standard error 0.11 %); the Bar-on mean is ABOVE the largest Bar-off value (outside the recorded noise envelope).
- Idle wakeups: on - off = 0.44 /s (spread: standard error 0.14 /s); the Bar-on mean is ABOVE the largest Bar-off value (outside the recorded noise envelope).

The budget is "no statistically distinguishable regression beyond the recorded noise envelope". This report records the evidence; it does not declare the budget met.

## Trials

| #   | Pair | Arm | Verdict | Host CPU avg % | Host CPU peak % | Tree CPU % | Wakeups/s | Note                                       |
| --- | ---- | --- | ------- | -------------- | --------------- | ---------- | --------- | ------------------------------------------ |
| 1   | 1    | off | valid   | 0.10           | 0.20            | 0.10       | 0.35      | ok                                         |
| 2   | 1    | on  | valid   | 0.45           | 0.65            | 0.47       | 0.85      | ok                                         |
| 3   | 2    | on  | valid   | 0.88           | 3.37            | 0.88       | 0.37      | ok                                         |
| 4   | 2    | off | INVALID | -              | -               | -          | -         | host pid 98225 exited during idle sampling |
| 5   | 3    | off | valid   | 0.10           | 0.20            | 0.10       | 0.30      | ok                                         |
| 6   | 3    | on  | valid   | 0.97           | 2.58            | 0.97       | 0.93      | ok                                         |
| 7   | 4    | on  | valid   | 0.67           | 0.87            | 0.67       | 0.95      | ok                                         |
| 8   | 4    | off | valid   | 0.10           | 0.22            | 0.10       | 0.25      | ok                                         |
| 9   | 5    | off | valid   | 0.10           | 0.22            | 0.10       | 0.12      | ok                                         |
| 10  | 5    | on  | valid   | 0.45           | 0.79            | 0.45       | 0.38      | ok                                         |

## Safety

- Temporary roots: 10; removed: true
- Signals sent (owned children only): 9
  - 2026-11-11T00:26:36.457Z pid 66321 SIGTERM: stop measured process tree
  - 2026-11-11T00:27:48.654Z pid 78475 SIGTERM: stop measured process tree
  - 2026-11-11T00:29:03.184Z pid 86737 SIGTERM: stop measured process tree
  - 2026-11-11T00:31:23.827Z pid 6543 SIGTERM: stop measured process tree
  - 2026-11-11T00:32:36.055Z pid 13985 SIGTERM: stop measured process tree
  - 2026-11-11T00:33:50.247Z pid 21123 SIGTERM: stop measured process tree
  - 2026-11-11T00:35:04.358Z pid 27848 SIGTERM: stop measured process tree
  - 2026-11-11T00:36:16.398Z pid 34955 SIGTERM: stop measured process tree
  - 2026-11-11T00:37:28.467Z pid 42071 SIGTERM: stop measured process tree
- Guard: no Stache Next or legacy Stache process at 2026-11-11T00:25:13.214Z
- Guard: no Stache Next or legacy Stache process at 2026-11-11T00:25:24.356Z
- Guard: no Stache Next or legacy Stache process at 2026-11-11T00:26:36.808Z
- Guard: no Stache Next or legacy Stache process at 2026-11-11T00:27:51.317Z
- Guard: no Stache Next or legacy Stache process at 2026-11-11T00:29:05.784Z
- Guard: no Stache Next or legacy Stache process at 2026-11-11T00:30:12.010Z
- Guard: no Stache Next or legacy Stache process at 2026-11-11T00:31:24.297Z
- Guard: no Stache Next or legacy Stache process at 2026-11-11T00:32:38.519Z
- Guard: no Stache Next or legacy Stache process at 2026-11-11T00:33:52.693Z
- Guard: no Stache Next or legacy Stache process at 2026-11-11T00:35:04.718Z
- Guard: no Stache Next or legacy Stache process at 2026-11-11T00:36:16.741Z

## Limitations

- The control socket exposes only the Accessibility gate (`core.status`), not the Bar module state or panel phase. Bar "on" is proven by an open gate plus a host-owned window of the configured Bar height (CGWindowList: owner pid, bounds; no Screen Recording needed). A loaded page is not separately observed.
- Idle wakeups come from the second sample of `top -l 2 -stats pid,idlew` over the sampling window, summed over every pid seen in the host tree; only compare values from this tool.
- Host CPU peak is the highest average over one 5 s snapshot interval, not an instantaneous maximum.
- `ps` cumulative CPU time has 10 ms resolution.
- The pre-flight sums per-process CPU over the whole process table; short-lived processes are not captured.
- Dock, Spaces and display state are not controlled; the machine is shared with unrelated activity.
- Bar-on runs after a Bar-off launch on a cold app cache for neither arm; page cache is not flushed.
