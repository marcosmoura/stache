---
status: draft
---

# 03 — Display pacing, position-only frames and the size envelope

Read when implementing after part 02 and separate approval. Paths are under `src/crates/`.

## Scope

New `macos/src/display_link.rs` (or `macos/src/displays/`, next to the existing display adapters) with a fake; `modules/tiling/src/animation/{worker.rs,transition.rs,mod.rs,tests.rs}`; `modules/tiling/src/outcome.rs` only if `FrameComponents` needs a per-frame variant. Easing, spring and duration rules (`animation/{easing,spring}.rs`, `AnimationSettings`) are unchanged.

Today pacing is a `Condvar` timeout of `flush_period(refresh_hz)` (`modules/tiling/src/events/geometry.rs:24–35`) and every frame writes the full `components` the plan asked for (`actor/mod.rs:224–247`), so a move with a size change resizes on every frame.

## Requirements

- **Frame clock.** A per-display frame clock drives the coordinator. Preferred source: `CVDisplayLinkCreateWithCGDisplay` (public; deprecated in recent SDKs, so availability on macOS 26 is a native-gate item). Its callback runs on CoreVideo's thread and only posts a tick (display, target timestamp) into a bounded, latest-wins slot and wakes the coordinator: no AX, no allocation beyond the slot, no lock held while calling out. One clock per display with running animations; created on the first job and stopped when the display has none, so idle and disabled own zero clocks.
- **Stop safety.** Stopping a clock must guarantee no callback runs afterwards before its context is freed (stop, then a bounded wait for an in-flight callback, never a detached owner). A clock that fails to stop keeps its context and is reported owed in quarantine.
- **Fallback pacing.** If a display has no clock (creation fails, unknown display), the coordinator paces that display with today's deadline timer at `clamp_refresh_hz` and records the reason in status diagnostics. Smoothness of that path is judged by part 06's metrics, not assumed.
- **Batching across windows.** Frames for all windows of one display are computed for the same tick timestamp and dispatched in that tick (one batch per application, part 02).
- **Position-only interactive frames.** Intermediate frames write position only.
- **Size envelope.** When a move changes size, the window's size is written at most twice: on the first frame, size becomes the per-axis maximum of the start and target sizes (the envelope), and on the final frame the exact target size. A component already equal to the envelope is not written. Effects: pure growth resizes once at the start, pure shrink once at the end, mixed changes never show the window smaller than either endpoint. Rationale: content relayout happens at most twice per animation, and the window never uncovers the desktop mid-move; a short overlap with neighbors is accepted.
- **Cross-display moves.** When the start and target are on different displays, the envelope rule is not used: size is written once on the first frame as the target size, and the move is paced by the target display's clock.
- **Final frame.** Always writes the exact target position and, if the plan wanted size, the exact target size, through the gated path. Unresizable or refused size ends the animation as today.
- Authority, mailboxes, acknowledgements and stop ordering are part 02's, unchanged.

## Acceptance criteria

- [ ] TA-03-01: A fake frame clock drives ticks per display; the coordinator issues one batch per application per tick, and idle/disabled owns no clock; clock creation failure uses the timer fallback with a diagnostic.
- [ ] TA-03-02: Clock stop guarantees no callback after release in fakes (held callback barrier), and a clock that cannot stop is quarantined, not leaked or detached.
- [ ] TA-03-03: Intermediate frames carry position only; size is written per the envelope rule (growth at start, shrink at end, mixed envelope then exact), cross-display moves write target size once, and the final frame is exact.

## Tests

Pure tests for the envelope rule (growth, shrink, mixed axes, equal sizes, cross-display, unresizable final). Coordinator tests with a fake clock per display: two displays at different rates, clock missing for one display, tick coalescing when the coordinator is late (latest tick wins, no burst catch-up), stop with a held callback. Default tests never create a real display link.

## Notes

- Rollback: return to timer pacing and full-component frames; part 02 stays.
- Unverified natively: CVDisplayLink availability and callback cadence on macOS 26, ProMotion variable refresh behavior, and whether position-only frames avoid relayout in each application. Recorded by part 06.
- Docs: Animation section of [features/tiling.md](../../features/tiling.md#animation) (pacing, size policy) and [guides/performance.md](../../guides/performance.md).
