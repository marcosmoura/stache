# Bar performance measurement - 2026-11-10T19:33:59.056Z

Read when judging the Bar-on cost against the Bar-off noise envelope.

- Status: **completed**
- Series: default (SSID path on)
- App: `/Users/marcosmoura/Projects/stache/.worktrees/bar/target/release/bundle/macos/Stache Next.app` (`/Users/marcosmoura/Projects/stache/.worktrees/bar/target/release/bundle/macos/Stache Next.app/Contents/MacOS/stache`)
- Plan: 5 valid launches per arm, warmup 10 s, sample 60 s, Bar height 28 / padding 14
- Arm order: off on on off off on on off off on
- Command: `/Users/marcosmoura/.bun/bin/bun /Users/marcosmoura/Projects/stache/.worktrees/bar/scripts/measure-bar.ts --app target/release/bundle/macos/Stache Next.app --height 28 --padding 14`
- Machine: MacBookPro18,3, Apple M1 Pro; macOS 27.0 (26A428); power AC Power 100%
- Repository: afe3646fcecba62e1271f4e3b1d53b7905942011 on feat/bar

## Pre-flight

Whole-machine CPU 60.9% over 10 s (limit 150%): idle enough.

## Result

Valid launches: off 5, on 5 (requested 5 per arm); invalid trials: 0.

| Metric            | Arm     | Mean | Stdev | Min  | Max  | N   |
| ----------------- | ------- | ---- | ----- | ---- | ---- | --- |
| Host CPU avg (%)  | Bar off | 0.14 | 0.01  | 0.13 | 0.15 | 5   |
| Host CPU avg (%)  | Bar on  | 0.73 | 0.06  | 0.65 | 0.80 | 5   |
| Host CPU peak (%) | Bar off | 0.21 | 0.01  | 0.20 | 0.22 | 5   |
| Host CPU peak (%) | Bar on  | 0.91 | 0.11  | 0.79 | 0.99 | 5   |
| Tree CPU avg (%)  | Bar off | 0.14 | 0.01  | 0.13 | 0.15 | 5   |
| Tree CPU avg (%)  | Bar on  | 0.73 | 0.06  | 0.65 | 0.80 | 5   |
| Idle wakeups (/s) | Bar off | 0.60 | 0.15  | 0.48 | 0.80 | 5   |
| Idle wakeups (/s) | Bar on  | 1.00 | 0.31  | 0.52 | 1.38 | 5   |

Bar-off noise envelope = the Bar-off min..max and stdev above.

- Host CPU avg: on - off = 0.59 % (spread: standard error 0.03 %); the Bar-on mean is ABOVE the largest Bar-off value (outside the recorded noise envelope).
- Host CPU peak: on - off = 0.70 % (spread: standard error 0.05 %); the Bar-on mean is ABOVE the largest Bar-off value (outside the recorded noise envelope).
- Tree CPU avg: on - off = 0.59 % (spread: standard error 0.03 %); the Bar-on mean is ABOVE the largest Bar-off value (outside the recorded noise envelope).
- Idle wakeups: on - off = 0.39 /s (spread: standard error 0.16 /s); the Bar-on mean is ABOVE the largest Bar-off value (outside the recorded noise envelope).

The budget is "no statistically distinguishable regression beyond the recorded noise envelope". This report records the evidence; it does not declare the budget met.

## Trials

| #   | Pair | Arm | Verdict | Host CPU avg % | Host CPU peak % | Tree CPU % | Wakeups/s | Note |
| --- | ---- | --- | ------- | -------------- | --------------- | ---------- | --------- | ---- |
| 1   | 1    | off | valid   | 0.13           | 0.20            | 0.13       | 0.80      | ok   |
| 2   | 1    | on  | valid   | 0.80           | 0.99            | 0.80       | 1.38      | ok   |
| 3   | 2    | on  | valid   | 0.78           | 0.99            | 0.78       | 0.97      | ok   |
| 4   | 2    | off | valid   | 0.15           | 0.22            | 0.15       | 0.48      | ok   |
| 5   | 3    | off | valid   | 0.15           | 0.22            | 0.15       | 0.73      | ok   |
| 6   | 3    | on  | valid   | 0.65           | 0.79            | 0.65       | 1.12      | ok   |
| 7   | 4    | on  | valid   | 0.72           | 0.80            | 0.72       | 1.00      | ok   |
| 8   | 4    | off | valid   | 0.13           | 0.22            | 0.13       | 0.52      | ok   |
| 9   | 5    | off | valid   | 0.13           | 0.20            | 0.13       | 0.48      | ok   |
| 10  | 5    | on  | valid   | 0.68           | 0.99            | 0.68       | 0.52      | ok   |

## Safety

- Temporary roots: 10; removed: true
- Signals sent (owned children only): 10
  - 2026-11-10T19:35:23.622Z pid 71172 SIGTERM: stop measured process tree
  - 2026-11-10T19:36:35.258Z pid 77600 SIGTERM: stop measured process tree
  - 2026-11-10T19:37:49.006Z pid 84063 SIGTERM: stop measured process tree
  - 2026-11-10T19:39:02.806Z pid 90472 SIGTERM: stop measured process tree
  - 2026-11-10T19:40:14.440Z pid 96812 SIGTERM: stop measured process tree
  - 2026-11-10T19:41:26.125Z pid 3500 SIGTERM: stop measured process tree
  - 2026-11-10T19:42:40.031Z pid 9883 SIGTERM: stop measured process tree
  - 2026-11-10T19:43:53.755Z pid 16355 SIGTERM: stop measured process tree
  - 2026-11-10T19:45:05.407Z pid 22460 SIGTERM: stop measured process tree
  - 2026-11-10T19:46:17.092Z pid 28801 SIGTERM: stop measured process tree
- Guard: no Stache Next or legacy Stache process at 2026-11-10T19:33:59.180Z
- Guard: no Stache Next or legacy Stache process at 2026-11-10T19:34:10.102Z
- Guard: no Stache Next or legacy Stache process at 2026-11-10T19:35:24.050Z
- Guard: no Stache Next or legacy Stache process at 2026-11-10T19:36:37.825Z
- Guard: no Stache Next or legacy Stache process at 2026-11-10T19:37:51.636Z
- Guard: no Stache Next or legacy Stache process at 2026-11-10T19:39:03.270Z
- Guard: no Stache Next or legacy Stache process at 2026-11-10T19:40:14.953Z
- Guard: no Stache Next or legacy Stache process at 2026-11-10T19:41:28.809Z
- Guard: no Stache Next or legacy Stache process at 2026-11-10T19:42:42.587Z
- Guard: no Stache Next or legacy Stache process at 2026-11-10T19:43:54.247Z
- Guard: no Stache Next or legacy Stache process at 2026-11-10T19:45:05.896Z

## Limitations

- The control socket exposes only the Accessibility gate (`core.status`), not the Bar module state or panel phase. Bar "on" is proven by an open gate plus a host-owned window of the configured Bar height (CGWindowList: owner pid, bounds; no Screen Recording needed). A loaded page is not separately observed.
- Idle wakeups come from the second sample of `top -l 2 -stats pid,idlew` over the sampling window, summed over every pid seen in the host tree; only compare values from this tool.
- Host CPU peak is the highest average over one 5 s snapshot interval, not an instantaneous maximum.
- `ps` cumulative CPU time has 10 ms resolution.
- The pre-flight sums per-process CPU over the whole process table; short-lived processes are not captured.
- Dock, Spaces and display state are not controlled; the machine is shared with unrelated activity.
- Bar-on runs after a Bar-off launch on a cold app cache for neither arm; page cache is not flushed.
