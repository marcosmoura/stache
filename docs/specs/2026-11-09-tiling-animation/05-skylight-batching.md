---
status: skipped
---

# 05 — Optional SkyLight update batching

Read when implementing after part 03 and separate approval. Optional: skipping it needs a user decision recorded in the overview. Paths are under `src/crates/`.

## Scope

New `macos/src/skylight.rs` (or `macos/src/ax/`-adjacent module) with the symbol resolver and a fake; `macos/Cargo.toml` (a new feature, proposed `private-skylight-updates`, off by default); `modules/tiling/src/animation/worker.rs` (bracket a tick); `modules/tiling/src/status.rs` (capability diagnostic). Legacy reference: `_legacy/app/native/src/modules/tiling/ffi/skylight.rs` (`UpdateGuard`), read-only.

## Requirements

- **Inventory first.** Document in the module header, like `macos/src/ax/window_id.rs:1–19`: symbols `SLSMainConnectionID`, `SLSDisableUpdate`, `SLSReenableUpdate`; the missing public equivalent (`NSDisableScreenUpdates` is deprecated and unverified on macOS 26); the minimum need (show one tick's frames across applications in the same screen update).
- **Compile gate.** Code is compiled only with the feature; without it the capability is `Unsupported` and nothing else changes. The feature is enabled in `stache-app` only after an approved native check passes.
- **Runtime gate.** Symbols resolve only by `dlopen`/`dlsym` of the SkyLight framework image at construction, never linked. A missing symbol disables only this capability and shows a status diagnostic.
- **Use.** For one tick, the coordinator disables updates, dispatches that tick's batches, waits for their acknowledgements for at most one frame period (cancellation-wakeable), then re-enables. Re-enable always runs: a guard re-enables on every exit path, including panic and seal, and the guard never outlives the tick. A late acknowledgement lands after re-enable; it is never waited for longer.
- **Health.** Disable or re-enable failure turns the capability off for the runtime epoch with a diagnostic; there is no retry loop. Disable is never held across a sleep, a stop or a config reload.
- No other SkyLight call, no connection to other processes' windows, no Screen Recording, no SIP requirement.

## Acceptance criteria

- [ ] TA-05-01: Without the feature, or with a missing symbol, the capability reports `Unsupported` and animations run exactly as after part 03.
- [ ] TA-05-02: With a fake resolver, each tick is bracketed by exactly one disable/re-enable pair; re-enable runs on success, timeout, seal, cancel and panic; the bracket never exceeds one frame period plus the send of one batch.
- [ ] TA-05-03: The approved native check records the effect on per-tick visual consistency and on unrelated screen updates before the feature is turned on; until then the feature stays off in `stache-app`.

## Tests

Fake resolver with call log: absent symbol, disable failure, re-enable failure, held acknowledgement past the frame period, panic inside the bracket, seal mid-tick. Compile both with and without the feature in CI (`cargo check` with `--features stache-macos/private-skylight-updates`).

## Notes

- Rollback: leave the feature off; nothing else depends on it.
- Unverified natively: whether these symbols exist and behave on macOS 26, which thread may call them, whether the WindowServer re-enables updates by itself after a timeout, and whether disabling updates for one frame is visible in other applications (video, cursor).
- Docs: native adapter notes in [features/tiling.md](../../features/tiling.md) and the private API inventory, only once shipped enabled.
