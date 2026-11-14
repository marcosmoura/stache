---
status: done
---

# 07 — Tiling: window-server bounds for in-flight echoes and retargets, lighter settle read

## Scope

While Stache itself is moving a window, Tiling stops asking the application where the window is. The echo of an in-flight write and the start frame of a retargeted animation come from the window server; the settle readback asks the application for the frame only. Decision D12 of the [overview](00-overview.md).

Files expected to change: `src/crates/macos/src/windows/` (a new bounds module, `commands.rs`, `core.rs`, `adapter.rs`, `testing.rs`, `mod.rs`), `src/crates/modules/tiling/src/{backend.rs, actor/mod.rs, state/mod.rs, state/transition.rs, state/frames.rs, animation/worker.rs, animation/control.rs, status.rs}`, the Tiling test suites, and the docs below.

## Requirements

- **Bounds seam (`stache-macos`).** A `Send + Sync` reader of a window's bounds by `CGWindowID`: a trait, the system implementation over `SLSMainConnectionID` and `SLSGetWindowBounds` resolved with the existing `dlsym` lookup of `border_overlay::private_skylight`, and a fake. It answers nothing when a symbol is missing, the call fails or the rectangle is empty. The module documentation carries the private API inventory and the assumed signatures in the same form as `live_follow/skylight.rs`, and says which callers may use it (this spec's two).
- **Frame-only read (`stache-macos`).** A new shard command reads `AXPosition` and `AXSize` of one tracked window after the light validation intermediate frames already use (adapter, epoch, application, serial, not suspended; no `AXWindows` listing). The router submits it without waiting, with the routing rules of `submit_snapshot`.
- **Backend seam (Tiling).** `WindowBackend` gains a bounds query by window id (default: none) and a non-blocking frame-only read. `SystemWindowBackend`, `LazySystemBackend`, `FakeWindowBackend` and the test spies implement them.
- **Pure state.** A new effect (working name `ReadGeometry`) and a new completion (working name `Geometry`):
  - A geometry batch, and the direct moved or resized arm, emits `ReadGeometry` for a window whose frame record holds an in-flight attempt and whose window id is known. Every other window gets `Snapshot`, as today.
  - A `Geometry` completion for a still-tracked window sets its observed frame, through the same echo guard a snapshot's frame passes (`absorb_echo`), and then notifies the drag state as a snapshot completion does. It changes nothing else: no title, flag, exclusion, tab or slot logic runs, and it never feeds minimum-size inference (`observe_growth`), because a window-server frame never decides a minimum size (decision D12).
- **Runner.** `ReadGeometry` asks the backend for the bounds. With a frame it completes `Geometry`. Without one it runs the `Snapshot` path and completes as a snapshot, so a missing symbol or id behaves exactly as today. The same holds for a frame the in-flight write does not explain: the effect carries the attempt's start and target frames, and a window-server frame with a component outside the range those two span (beyond the frame tolerance) takes the `Snapshot` path. Stache's own frames always lie inside that range, so its echoes stay cheap, while a native-fullscreen growth, a user drag or an application moving the window by itself is read in full at once, as before.
- **Retarget read.** `WriteFrame` and `AnimationJob` carry the window id. `LiveSink::submit_retarget_read` answers at once from the bounds when they yield a frame; otherwise it submits a frame-only read; if that cannot be queued it answers "no frame" as today.
- **Settle read.** `LiveSink::submit_settle` submits a frame-only read. The outcome rules do not change: a complete read decides `applied`, `partial` or `failed`; an incomplete or refused read reports the final write without a readback.
- **Unchanged.** Enumeration, creation, title, focus and drag-commit reads, reads of windows with no in-flight attempt, the direct write's own readback, minimum-size inference and fullscreen detection.
- **Fallback notice.** When the bounds symbols are missing, status `diagnostics` carry one line saying that geometry reads use Accessibility. No other status, query or published output changes.
- **Docs.** `docs/features/tiling.md` (in Animation: Latest plan only, Every frame is gated, Outcomes and Echo suppression; in Tracking actor: what a geometry sample reads), `docs/architecture/overview.md` (the private API inventory and the window adapter's commands), `docs/guides/performance.md` (reads per animation).

## Acceptance criteria

- [x] PF-07-01: In the animation rig (the real sharded adapter over the fake AX arena, two applications with two windows each, a 120 Hz fake clock, 5–15 ms of write latency serialized per application, move echoes on), between the first frame and the settle of a layout change no snapshot read is issued for an animated window, and each animated window gets exactly one frame-only read.
- [x] PF-07-02: During that animation the observed frame of an animated window takes at least one value between its start and its target.
- [x] PF-07-03: A plan reversed in flight (A, B, A) still animates the return. The new job starts from the bounds frame without waiting for the application and ends exactly on A.
- [x] PF-07-04: With the bounds unavailable, PF-07-02 and PF-07-03 hold with the previous read pattern (a snapshot per echo, a frame-only retarget read) and the status line appears once.
- [x] PF-07-05: In one batch, a window whose id is unknown gets a snapshot and the others get bounds reads.
- [x] PF-07-06: A move by the user, and an entry into native fullscreen, of a window with no in-flight attempt still issue a full snapshot and still update the fullscreen flag.
- [x] PF-07-07: In the rig of PF-07-01 no application enters the instant fallback, and the Accessibility calls made to an application during one animation equal its frame writes plus a fixed number per window, with nothing that grows with the number of echoes.
- [x] PF-07-08: The drag, minimum-size, tab, borders, visibility and scenario suites pass unchanged in meaning.
- [x] PF-07-10: While a write for a window is in flight, a window-server frame outside the range spanned by the attempt's start and target (a native-fullscreen growth) issues a full snapshot at once and the fullscreen flag is updated; a frame inside the range does not.
- [x] PF-07-09: `bun run lint`, `bun run format:check` and `bun run test` pass.

## Tests

- `stache-macos`: the bounds reader with injected symbols (present, missing, refused, empty rectangle) and the frame-only command over the fake AX arena (tracked, suspended, destroyed, wrong epoch).
- Pure-state tests: which effect a geometry batch emits for each kind of window, and what a `Geometry` completion changes and does not change.
- Runner tests with `FakeWindowBackend`: bounds present, bounds absent, cancellation.
- Animation rig tests for PF-07-01 to PF-07-04 and PF-07-07. The fake window system answers bounds from the same nodes the fake AX arena moves, so an observed frame is the frame the rig wrote. Counts come from the arena's call log and the spy, not from timing.
- The user's-setup scenario in `runtime_tests/scenarios.rs` runs once with bounds and once without and gives the same final layout.

## Notes

- Measured on 2026-11-13: `SLSGetWindowBounds` 67 µs per call, equal to the public bounds for 8 of 8 on-screen windows; `CGWindowListCopyWindowInfo` for one window 650 µs. Borders' live follow has used the same call in native runs since the borders spec.
- Measured on the branch (2026-11-14) from the fake Accessibility arena's call log in the rig (two applications with two windows each, 120 Hz, 5–15 ms serialized writes, move echoes on), per application and layout change: 264 Accessibility calls before (22 writes, 8 full snapshots per window, 20 `AXWindows` listings) and 46 after (the same 22 writes, 16 timeouts, 4 listings, 2 position and 2 size reads; no snapshot of an animated window). The before figure comes from the same counting test run on a copy of the tree at the previous commit.
- Before the event-order fix below, in about one rig run in ten to twenty the actor re-listed every application during the animation. The trigger was found during the review fixes: `Runtime::emit` in `stache-macos` `windows/core.rs` took an event's sequence number and pushed the event in two steps, and every shard shares the counter, so two applications emitting at the same moment could reach the actor out of order. `events/pipeline.rs` read that as a sequence gap. It was the real core, not the fake arena, and it predates this spec.
- Fixed on this branch at the user's request (2026-11-14, decision D15 of the overview): `EventQueue::push_with` builds the event under the queue lock and `emit` takes the sequence number there. The rig asserts zero sequence gaps and no refresh in every run, and the allowances the two rig tests carried for a re-listing are gone.
- `the_users_setup_ends_on_the_same_layout_with_and_without_window_server_bounds` relied on thread timing for its one window-server read and failed about once in eighteen runs. It now holds the application's sends, waits until the animation worker took the jobs and changes to a layout that must retarget them (Monocle, Dwindle, Monocle, Master), so the read happens in every run.
- A fullscreen entry during an in-flight write is tested on the deterministic engine, because a shard stalled inside a write cannot deliver a scripted notification in the real-adapter rig.
- Added in review (2026-11-14): PF-07-10 and the unexplained-frame rule. Without it a native-fullscreen entry during an in-flight write would have gone unread until the next snapshot, where each echo read `AXFullScreen` before. The rule also makes the window-server path check itself: an application whose window-server bounds do not agree with the frames Stache writes simply keeps today's reads.
- PF-07-04 is met in the sense the criterion states: without bounds the read pattern is today's (a snapshot per echo), and in the rig that pattern lets at least one window's observed frame pass through an intermediate value; the last window's echo snapshots queue behind its writes on the single effects thread, as before this part.
- Clarified in review (2026-11-14): a snapshot's frame also passes `observe_growth`, which can learn a minimum size. A `Geometry` completion skips it. Nothing is lost: growth is only learned from a settled attempt, and a window without an in-flight attempt still gets a full snapshot.
- The window-server frame is where the window is on screen. During an application's own animated resize it can differ from what Accessibility reports, which is why it never decides an outcome (decision D12).
- For the user to check: before starting the branch build, note the `animation metrics` lines of `stache tiling query status` for an Electron application and a native one; compare after. Layout changes, a reversed layout change mid-animation and a drag should look the same or smoother.
