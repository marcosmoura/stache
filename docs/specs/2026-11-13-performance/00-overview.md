---
branch: feat/performance
---

# Performance: input latency, polling, wallpapers and Tiling reads

Read when planning or implementing the 2026-11 performance pass. Drafted 2026-11-13 from an audit of the Rust code on `main` at `8c70e93d`.

## Goal

Window operations and wallpaper switches feel instant, and an idle Stache does not wake on a timer. No feature changes what it does: every part keeps the observable behaviour of the code it touches and names the one place where a documented contract is reworded.

## Background

Measured on 2026-11-13 on the user's machine (Apple M1 Pro, macOS 27.3) with the user's configuration: two displays (2560×1440 and 1920×1200), fourteen 6–7K PNG wallpapers, `blur = 10`, `radius = 20`, Tiling with animations and borders on, every key binding a `stache …` command.

| Path                                                           | Today                                  | After                                 |
| -------------------------------------------------------------- | -------------------------------------- | ------------------------------------- |
| `stache tiling …` command (status probe, then the action)      | median 55–68 ms, p90 102–109 ms        | median 1.1 ms, p90 1.9 ms (prototype) |
| One request per connection (`stache wallpaper set`)            | median 31 ms                           | median 0.7 ms (prototype)             |
| Cold wallpaper, one display (6016×3900 PNG to 2560×1440)       | about 1.6 s                            | about 0.47 s (04), about 0.26 s (05)  |
| Cold wallpaper, both displays, different images                | about 2.9 s, one display after another | about 0.5 s (04), about 0.3 s (05)    |
| Cached wallpaper switch, both displays                         | 25–65 ms                               | about 1 ms (06)                       |
| Accessibility round trips to an application per animated frame | 1–2 writes plus about 10 for the echo  | the 1–2 writes (07)                   |
| Timed wakeups while idle, Tiling on                            | about 58 per second                    | none from these loops (01, 02)        |
| `ipconfig` child processes while the Bar runs                  | 12 per minute                          | about 1 per minute, plus changes (03) |

Where the time goes today:

- The control socket's accept loop polls a non-blocking listener every 50 ms (`sdk/src/ipc/server.rs`). A connection waits up to one period, and the Tiling CLI makes two connections per command. A prototype that waits in `poll(2)` passed all 63 SDK IPC tests.
- A cold wallpaper spends its time in PNG `Best` compression (0.4–1.0 s), the `image` crate's Lanczos3 resize (0.4–0.6 s at `opt-level = "s"`) and the source decode; displays are prepared one after another. A cache hit re-reads and re-hashes the source and fully decodes the cached PNG.
- Every move or resize notification, including the echo of Stache's own animation frames, triggers a full window snapshot: an `AXWindows` listing plus about nine attribute reads on the same per-application thread that writes the frames. A window-server bounds read (`SLSGetWindowBounds`, already bound for borders) measured 67 µs and matched the public bounds for 8 of 8 windows.
- All read effects, hides, unhides and focus run one after another on one blocking effects thread, so one slow application delays the next workspace switch.
- Five loops wake on a fixed period while nothing happens, and the Bar's Wi-Fi provider spawns `ipconfig` every 5 s.

The Tiling state machine itself is not a cost: one transition is about 5 µs and a full snapshot 17 µs with 40 applications and 49 windows.

## Non-goals

- No in-process dispatch of `stache …` key bindings, no change to the CLI's two connections, to the 2 ms intent acknowledgement poll, to the Enhanced UI lease, to frame-clock lifetime or to logging. Each saves a few milliseconds at most (user decision 2026-11-13: no micro-shaving).
- No change to how windows are hidden or shown: no minimizing, no off-screen parking, no macOS Spaces.
- No SkyLight window moves, transforms, transactions or screen-update batching, and no private front-process switching. The only private call this spec adds a use of is `SLSGetWindowBounds`.
- No new configuration key and no change to CLI output, JSON shapes or published state.
- No background pre-generation of the wallpaper pool.
- The Bar CPU and battery schedules and the permission ticker stay as they are.

## Decisions

- **D1. Evidence.** Each part records a before and an after with the harness it names, on the same machine in the same session. A part whose after does not beat its before stops and reports instead of landing.
- **D2. Accept loop.** The listener stays non-blocking; the loop waits in `poll(2)` on the listener and on the read end of a wake pipe, with no timeout. A stop writes one byte to the pipe. Both pipe ends are close-on-exec and the write end is non-blocking.
- **D3. Polls become waits.** A loop that only polls to notice a stop blocks instead, and the stop wakes it. Bounded waits that exist only during an operation (child exit, cancellation slices, teardown joins, tab and fullscreen re-lists) are out of scope.
- **D4. Config watcher.** `notify-debouncer-full` is replaced by `notify` (already in the lockfile) with a quiet-period wait inside the watcher. The 200 ms debounce stays; a continuous stream of events cannot postpone a report by more than 2 s.
- **D5. Keep Awake.** With lock notifications installed the safety probe runs every 30 s instead of every 2 s; in polling fallback it stays at 2 s. A dropped lock notification is therefore corrected within 30 s instead of 2 s. This is the one deliberate change to a documented bound: a pass that leaves something to retry (lock state unreadable, assertion not created, release pending) is still followed by the next pass 2 s later in both modes, so failure recovery keeps its cadence.
- **D6. Wi-Fi.** `CoreWLAN` event monitoring on a client instance Stache owns provides the change signal. Identity signals (power, SSID, link) refresh the SSID through `ipconfig`; quality signals (RSSI, BSSID) read the native values only. With signals installed a full safety sample runs every 60 s. When the signal cannot be installed, or its connection is permanently invalidated later, today's 5 s schedule runs unchanged and the status says so.
- **D7. PNG.** Outputs are written with `CompressionType::Fast` and adaptive filtering. Pixels are identical, files are about 1.8 times larger. `PROCESSING_VERSION` does not change, so existing cache entries stay valid.
- **D8. Parallel prepare.** One plan is prepared by at most `MAX_CONCURRENT_WORKERS` (4) scoped threads. Each distinct source is read and decoded once; each distinct output is produced once. At most four decoded sources are resident, the bound the batch path already has (the previous bound for a plan was one).
- **D9. Optimisation level.** The crates that do pixel and codec work on the wallpaper path, and `stache-wallpapers`, build at `opt-level = 3` in release. Everything else stays at `"s"`.
- **D10. Resize.** `fast_image_resize` 5 (default features off) replaces the `image` crate's Lanczos3 in the cover resize. Geometry is unchanged. On content away from black and white pixel values differ by at most one level per channel; near pure black or white single channels can differ by more, within the bounds of part 05 that the user accepted on 2026-11-14. `PROCESSING_VERSION` becomes 2. The dependency adds three crates to the lockfile (`fast_image_resize`, `document-features`, `litrs`).
- **D11. Cache memo.** The host remembers, per process, which cache files it fully validated and which source files it hashed, keyed by device, inode, length, modification time and change time. A matching entry skips the decode or the read. Standalone CLI paths have no memo.
- **D12. Window-server bounds.** `SLSGetWindowBounds` answers two questions that only need a frame: where a window is while a Stache write for it is in flight, and where a retargeted animation starts. It never decides an outcome, a minimum size or a fullscreen state. When the symbol or the window id is missing the Accessibility path runs as today.
- **D13. Read lanes.** Read effects run on four worker threads with one FIFO per application. Hide, unhide, activation, focus and frame dispatch stay on the effects thread in order. A focus read waits for a focus of the same application that was submitted before it, as it did on the single thread. `stache-macos` does not change for this.
- **D14. Native behaviour.** Everything here is proven with fakes and harnesses. Agents never launch the app. After the branch is ready the user checks the points listed under each part's notes.
- **D15. Event order (added 2026-11-14 at the user's request).** Not one of the eight parts. On `main` a window event took its sequence number and was queued in two steps on a counter shared by every per-application thread, so two applications emitting at once could reach the Tiling actor out of order; the actor read that as a lost event and re-listed every application. Events now take their number under the event queue's lock, so a sequence gap means a dropped event. Found while testing part 07.

## Parts

| Part                              | Summary                                                                         | Status |
| --------------------------------- | ------------------------------------------------------------------------------- | ------ |
| [01](01-ipc-accept.md)            | SDK: event-driven control-socket accept loop                                    | done   |
| [02](02-event-driven-waits.md)    | Host, config watcher, Tiling reservations and Keep Awake wait instead of poll   | done   |
| [03](03-bar-wifi-signals.md)      | Bar: native Wi-Fi change signal, `ipconfig` only when the network may differ    | done   |
| [04](04-wallpaper-cold-path.md)   | Wallpapers: Fast PNG, parallel prepare, `opt-level = 3` for the image stack     | done   |
| [05](05-wallpaper-simd-resize.md) | Wallpapers: SIMD Lanczos3 cover resize, `PROCESSING_VERSION` 2                  | done   |
| [06](06-wallpaper-memo.md)        | Wallpapers: host memo of validated outputs and source digests                   | done   |
| [07](07-tiling-inflight-reads.md) | Tiling: window-server bounds for in-flight echoes and retargets, lighter settle | done   |
| [08](08-tiling-read-lanes.md)     | Tiling: read effects on per-application lanes                                   | done   |

Parts 01 to 03 are independent of 04 to 06 and of 07 and 08. Part 05 builds on 04, part 06 on 04, part 08 on 07.

## Affected docs

`docs/architecture/ipc.md` (accept loop), `docs/architecture/config.md` (watcher), `docs/architecture/overview.md` (host loop, private API inventory, Tiling threads), `docs/features/keep-awake.md` (probe interval), `docs/features/bar.md` (Wi-Fi provider), `docs/features/wallpapers.md` (pipeline, limits, cache contract, measurements), `docs/features/tiling.md` (animation readbacks, echoes, read effects), `docs/guides/performance.md` (bounds, threads, wakeups), `docs/guides/testing.md` where it lists suites, and the rows in `docs/index.md` and `docs/roadmap.md`.
