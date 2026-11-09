---
status: draft
---

# 07 — Lifecycle, robustness and performance budget

Read when implementing after parts 01–06 and separate approval. Paths are under `src/crates/`. Still behind `native-borders`.

## Scope

`modules/tiling/src/module.rs` (start/stop/quarantine stages, `Teardown`), `tracking.rs` (engine composition and `configure`), `status.rs` (borders diagnostics, `borders_failure` at `status.rs:222`), `borders/engine.rs`, `macos/src/border_overlay/` (`release_all`, leak accounting). Studied for approach (decision 5): JankyBorders `src/main.c` and `src/windows.c` (`windows_remove_all`, `windows_recreate_all_borders`); yabai issue [#2835](https://github.com/koekeishiya/yabai/issues/2835) (leaked proxy windows) as the hazard to prevent.

## Requirements

- **Start.** Borders stay outside the numbered start stages, as today (`module.rs:23–26`): the engine starts only when the accepted configuration has `borders.enabled`, after Tiling is Running, and a failure to start degrades borders only. Disabled, the engine owns no overlay, thread, display link, subscription or timer.
- **Reload.** `enabled` false → true starts the engine; true → false releases every overlay; appearance-only changes update in place (parts 02–03; running animations are replaced, a `transition` change applies to the next focus change); ignore changes recompute the model.
- **Stop order.** Stop closes the borders engine right after the drag observer and before the frame drain (the current borders slot, `module.rs:528–546`): stop the live-follow owner, the Spaces owner (part 05) and the display link, then `release_all` on main and wait for its acknowledgement within `BORDERS_RELEASE_WAIT` (proposed 500 ms). Pause and disabling Tiling do the same.
- **No leak, no orphan.** Every overlay handle is accounted for in the engine ledger; after `release_all` the acknowledged count must equal the ledger count. A missing acknowledgement, a mismatch or a stuck live-follow or Spaces owner leaves `borders overlays` owed: the module is quarantined, start is refused, the exact handles are retained, and a later stop retries exactly that stage. No thread is detached. A periodic check is not added; leaks are prevented by ownership, not swept.
- **Crash safety.** Overlays are windows of the Stache process, so the window server removes them when the process exits or crashes; no persisted state or journal is kept. A panic in the engine (unwind builds) releases overlays on the next main-thread turn and reports a borders diagnostic; Tiling keeps running.
- **Main-thread health.** If main does not acknowledge an overlay batch within `BORDER_ACK_WAIT` (proposed 250 ms), later batches merge latest-wins (part 01) and status reports `borders: main thread busy`; nothing waits longer.
- **Budget (deterministic bounds).** At most `MAX_OVERLAYS` (64) overlays and one pending batch; at most one display link per display while something moves; zero Stache wakeups while idle, including while a `loop` rotation runs (no timers, every animation on the render server, part 03); memory per overlay bounded by its layers (no bitmap backing store).

## Acceptance criteria

- [ ] TB-07-01: Start, reload (enable, disable, appearance, ignore) and disabled states own exactly the specified resources, proven with fakes and resource counters.
- [ ] TB-07-02: Stop releases every overlay before the frame drain; a missing or mismatched acknowledgement or a stuck owner leaves `borders overlays` owed, quarantines, retains exact handles and is retried only by a later stop; no thread is detached.
- [ ] TB-07-03: A main thread held past `BORDER_ACK_WAIT` produces the diagnostic and latest-wins merging without blocking the actor, the effect runner or Tiling commands; borders failures never change Tiling health.

## Tests

Module tests in `module_tests.rs` with fake overlay and event backends: start enabled/disabled, reload transitions, stop with held main acknowledgement, mismatched release count, stuck live owner then retry, pause, engine panic (unwind builds). Status snapshot tests for each diagnostic.

## Native gate (per-run approval)

Release build, 3 owned test windows bordered: record idle CPU, wakeups and memory with borders off, on (static), on (legacy ping-pong) and on (`loop` rotation on all 3 plus a cross-fade every 5 s) for 60 s each with warmup; 100 start/stop and reload cycles with a window-list check after each that no Stache overlay remains; quit through the supported tray path and confirm no overlay remains. Results go to [guides/performance.md](../../guides/performance.md) in part 08.

## Review checklist

- [ ] [Hard rule checklist](00-overview.md#hard-rule-review-checklist-every-part) passed: lifecycle inspired by, not derived from, JankyBorders; no code copied, translated or closely paraphrased.

## Notes

- Rollback: return to part 06's engine without staged teardown (feature still off in production).
- Unverified: window-server cleanup timing after a crash, main-thread contention with the Bar webview.
- Docs: none lasting until part 08.
