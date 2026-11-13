---
status: draft
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
  - A `Geometry` completion for a still-tracked window sets its observed frame, through the same echo and growth guards a snapshot's frame passes, and then notifies the drag state as a snapshot completion does. It changes nothing else: no title, flag, exclusion, tab or slot logic runs.
- **Runner.** `ReadGeometry` asks the backend for the bounds. With a frame it completes `Geometry`. Without one it runs the `Snapshot` path and completes as a snapshot, so a missing symbol or id behaves exactly as today.
- **Retarget read.** `WriteFrame` and `AnimationJob` carry the window id. `LiveSink::submit_retarget_read` answers at once from the bounds when they yield a frame; otherwise it submits a frame-only read; if that cannot be queued it answers "no frame" as today.
- **Settle read.** `LiveSink::submit_settle` submits a frame-only read. The outcome rules do not change: a complete read decides `applied`, `partial` or `failed`; an incomplete or refused read reports the final write without a readback.
- **Unchanged.** Enumeration, creation, title, focus and drag-commit reads, reads of windows with no in-flight attempt, the direct write's own readback, minimum-size inference and fullscreen detection.
- **Fallback notice.** When the bounds symbols are missing, status `diagnostics` carry one line saying that geometry reads use Accessibility. No other status, query or published output changes.
- **Docs.** `docs/features/tiling.md` (in Animation: Latest plan only, Every frame is gated, Outcomes and Echo suppression; in Tracking actor: what a geometry sample reads), `docs/architecture/overview.md` (the private API inventory and the window adapter's commands), `docs/guides/performance.md` (reads per animation).

## Acceptance criteria

- [ ] PF-07-01: In the animation rig (the real sharded adapter over the fake AX arena, two applications with two windows each, a 120 Hz fake clock, 5–15 ms of write latency serialized per application, move echoes on), between the first frame and the settle of a layout change no snapshot read is issued for an animated window, and each animated window gets exactly one frame-only read.
- [ ] PF-07-02: During that animation the observed frame of an animated window takes at least one value between its start and its target.
- [ ] PF-07-03: A plan reversed in flight (A, B, A) still animates the return. The new job starts from the bounds frame without waiting for the application and ends exactly on A.
- [ ] PF-07-04: With the bounds unavailable, PF-07-02 and PF-07-03 hold with the previous read pattern (a snapshot per echo, a frame-only retarget read) and the status line appears once.
- [ ] PF-07-05: In one batch, a window whose id is unknown gets a snapshot and the others get bounds reads.
- [ ] PF-07-06: A move by the user, and an entry into native fullscreen, of a window with no in-flight attempt still issue a full snapshot and still update the fullscreen flag.
- [ ] PF-07-07: In the rig of PF-07-01 no application enters the instant fallback, and the Accessibility calls made to an application during one animation equal its frame writes plus a fixed number per window, with nothing that grows with the number of echoes.
- [ ] PF-07-08: The drag, minimum-size, tab, borders, visibility and scenario suites pass unchanged in meaning.
- [ ] PF-07-09: `bun run lint`, `bun run format:check` and `bun run test` pass.

## Tests

- `stache-macos`: the bounds reader with injected symbols (present, missing, refused, empty rectangle) and the frame-only command over the fake AX arena (tracked, suspended, destroyed, wrong epoch).
- Pure-state tests: which effect a geometry batch emits for each kind of window, and what a `Geometry` completion changes and does not change.
- Runner tests with `FakeWindowBackend`: bounds present, bounds absent, cancellation.
- Animation rig tests for PF-07-01 to PF-07-04 and PF-07-07. The fake window system answers bounds from the same nodes the fake AX arena moves, so an observed frame is the frame the rig wrote. Counts come from the arena's call log and the spy, not from timing.
- The user's-setup scenario in `runtime_tests/scenarios.rs` runs once with bounds and once without and gives the same final layout.

## Notes

- Measured on 2026-11-13: `SLSGetWindowBounds` 67 µs per call, equal to the public bounds for 8 of 8 on-screen windows; `CGWindowListCopyWindowInfo` for one window 650 µs. Borders' live follow has used the same call in native runs since the borders spec.
- The window-server frame is where the window is on screen. During an application's own animated resize it can differ from what Accessibility reports, which is why it never decides an outcome (decision D12).
- For the user to check: before starting the branch build, note the `animation metrics` lines of `stache tiling query status` for an Electron application and a native one; compare after. Layout changes, a reversed layout change mid-animation and a drag should look the same or smoother.
