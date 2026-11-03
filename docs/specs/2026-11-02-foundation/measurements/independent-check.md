# Independent release-process check

Read when checking the two 2026-11-03 captures without relying on the TypeScript collector.

The orchestrator launched the existing Tauri 2.11.5 release bundle directly, using separate
temporary config directories and exact-name priorities for the current audio defaults. After
10 seconds of warmup, `ps` CPU time was sampled twice, 60 seconds apart. `footprint` and `top`
were run afterward, outside that CPU interval. Legacy remained running with proxyAudio disabled.
This was not a fresh build or an isolated legacy comparison.

| Configured mode     | PID   | `ps` CPU time before → after | RSS      | `footprint` | `top` CPU / memory | Children |
| ------------------- | ----- | ---------------------------- | -------- | ----------- | ------------------ | -------- |
| proxyAudio disabled | 54381 | `0:00.13` → `0:00.21`        | 76.1 MiB | 14 MB       | `0.1` / `14M`      | 0        |
| proxyAudio enabled  | 54800 | `0:00.14` → `0:00.22`        | 77.0 MiB | 15 MB       | `0.1` / `15M`      | 0        |

Both deltas are 0.08 seconds of CPU over 60 seconds, approximately 0.13% of one core.
The startup logs reported 140 ms and 142 ms respectively, with `webviews=0` in both cases.
Both owned PIDs exited after SIGTERM; their temporary directories were removed afterward.
The two configured modes were tested sequentially, not concurrently.

Commands used for each owned PID:

```sh
pgrep -P "$PID"
ps -o time= -p "$PID"  # before and after a 60-second wait
ps -o rss= -p "$PID"   # KiB, converted to MiB
footprint "$PID"
top -l 2 -s 5 -pid "$PID" -stats pid,cpu,mem,threads
```

RSS and physical footprint are not interchangeable. These readings do not prove how much of
the RSS consists of shared framework pages. The `top` memory column agrees with footprint,
not RSS, on this machine.

## Corrections to earlier summaries

- Run 1's disabled external warm median is **197 ms**, not the 203 ms quoted in an earlier
  conversational summary. The committed report contains the correct value.
- The first two collector captures recorded a misleading guard message claiming no legacy
  process existed. The collector intentionally allowed the daily legacy app to remain running
  when no legacy baseline was requested. Its guard text was wrong, not evidence that legacy
  had been stopped. Preserve the raw samples as captured.
- The two collector captures used the same pre-upgrade release executable. Their agreement
  establishes repeatability for that binary, not performance of subsequent dependency updates.
- CLI commands issued through the executable inside `.app/Contents/MacOS/` select desktop
  mode by contract; their empty output was not proof of an active config. Collector readiness
  uses the actual control socket instead.
