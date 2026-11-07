---
status: done
---

# 05 — Keep the HUD readable after an early release

Read when implementing the release linger. Requested by the user on 2026-11-07 after the first manual run: on a tap or early release the HUD disappeared before "Hold ⌘Q to quit `<App>`" could be read. The user then asked for the delay to be configurable. Implemented on `feat/hold-to-quit` after parts 01–04.

## Scope

- Config: `src/crates/modules/hold-to-quit/src/config.rs`, `schemas/commandQuit.schema.json` (regenerated), generated bindings if they change, `src/crates/cli/src/migrate.rs` tests (legacy has no such key; a migrated file has no `dismissDelay` and decodes to the default `1500`).
- HUD adapter: `src/crates/macos/src/hold_hud.rs` (`HudBackend`, `HudControl`, `SystemHudBackend`, `NativePanel`), `hold_hud/testing.rs` (`FakeHudBackend`), `hold_hud/tests.rs`.
- Runtime: `src/crates/modules/hold-to-quit/src/runtime.rs` (worker), `tests.rs`, `test_support.rs` if a helper is needed.
- Docs: `docs/features/hold-to-quit.md`, `docs/architecture/config.md` (the `commandQuit` paragraph), this spec.

## Requirements

1. **Config key.** `CommandQuitConfig` gains `dismiss_delay: u64` (`dismissDelay` in JSON), milliseconds, default `1500`, documented as "How long the HUD stays visible after Cmd+Q is released early, in milliseconds. 0 hides it immediately." It mirrors `holdDuration` exactly: any `u64` is accepted (wrong type or overflow is a type error naming `dismissDelay`), `#[specta(skip)]` with the same reason, `validate` stays `Ok`. The schema is regenerated; a migrated legacy file has no `dismissDelay` and decodes to the default `1500`. The runtime reads the armed config's value per press (a reload applies to the next lingering end; an armed-revision change already hides a pending linger, requirement 5). `0` means no linger and no `freeze`: the immediate hide of today. An unrepresentable `now + dismissDelay` lingers until the next claimed down, stop, revision change or degrade, with no timeout (no polling).
2. **When it applies.** Only when the live press ends because the user ended it: the worker's `step` sees the press cancelled by the callback (key or Command release) or by the frontmost activation listener, or `fire` finds it cancelled at the deadline. Two runtime-driven cancels also linger because they leave `accepting` and the revision unchanged: a tap disable (`on_cancel` cancels the current press) and the older press cancelled when a newer claim's push fails. This is harmless, since no authorization survives any cancel. Every other ending hides immediately as today: a successful fire (hide then terminate), the runtime stopping, an armed-revision change, `accepting` cleared, an epoch mismatch detected by the worker, a HUD `Stale`/`Unavailable`/`Failed`, degraded mode and supersession by a newer press.
3. **Freeze.** On a lingering end the worker calls a new non-waiting `HudControl::freeze(handle, press)`. It enqueues a main-thread job that, if the panel still displays `press`, stops the progress at its current on-screen width (the width is computed from the animation `apply` recorded: start time from `CACurrentMediaTime`, `from` and duration, via a pure function; it is set on the model under `CATransaction::setDisableActions(true)` and the `hold-progress` animation is removed; a full or static fill keeps its model width). A frozen panel stays ordered front and its label is unchanged. `freeze` never emits a `HudEvent`, never changes `current` or `pending`, and is a no-op for another handle, a successor press, or a phase other than `Ready`. `HudBackend` gains `fn freeze(&self, handle: HudHandle, press: PressId)`; `FakeHudBackend` records it.
4. **Linger state.** The worker keeps at most one `Lingering { press, until }` beside `Phase::Idle` (an `Option` field or a fourth phase; the press is no longer live). The cancel, authorization cancel and `clear_authorization` steps of `abandon` happen immediately; only the `hide` is deferred. `timeout()` returns the remaining linger while idle with a linger pending, so the idle runtime still has no polling once the linger ends.
5. **Ending the linger.** When `until` passes, the worker calls `hide(handle, press)` and clears the linger. A new claimed down that reaches `show` hides the old linger press right after enqueuing the new show (FIFO main queue: a no-op if the new press is displayed, an order-out if its show was refused). Stop, an armed-revision change and degrade hide a pending linger immediately.
6. **Late `Shown`.** A `Shown` for the lingering press (show acknowledged after the release) is neither tracked nor hidden; the worker re-sends `freeze` for it so the panel shown late is also frozen. A `Stale`/`Unavailable`/`Failed` for the lingering press clears the linger without a second hide. `Unavailable`/`Failed` still degrade only for a live press, as today.
7. **No quit during a linger.** A lingering press holds no authorization and can never issue `terminate_gracefully`.
8. **Gate unchanged.** The show gate still refuses a press already cancelled when main runs the show, so a release that beats the show leaves nothing on screen. Document this.

## Acceptance criteria

- [x] HQ-05-00 — Config: default `dismissDelay` is `1500`; `{}` keeps it; `0` and `u64::MAX` are accepted; a string or negative value is a type error naming `dismissDelay`; the schema and generated bindings are up to date (`bun run check:generated`); a migrated legacy file decodes to the default (`dismissDelay` is absent from the file).
- [x] HQ-05-01 — Fake clock: a release at 400 ms of a 1500 ms hold with `dismissDelay` 1500 calls `freeze` once for that press, no `hide` until 400 + 1500 ms, then exactly one `hide`; no terminate. With `dismissDelay` 300 the hide comes at 400 + 300 ms. With `dismissDelay` 0 the hide is immediate and there is no `freeze`.
- [x] HQ-05-02 — A new press during the linger shows the new press and never hides it; the old press is hidden once, right after the new show (a no-op on the displayed panel; assert the order of `FakeHudBackend` calls).
- [x] HQ-05-03 — A successful fire, stop, revision change, `accepting` cleared, a worker-detected epoch mismatch, degrade and supersession of a live press each hide immediately with no `freeze` (tested with a non-zero `dismissDelay`); stop and a revision change during a pending linger hide it immediately; a successor whose show is refused (stale gate or `NoScreen`/failed) leaves the old panel ordered out, never frozen on screen. A tap-disable cancel lingers and never quits.
- [x] HQ-05-04 — A `Shown` arriving after the release is not tracked, triggers a `freeze`, and the linger still ends with one `hide`; a `Stale` for the lingering press clears it with no extra `hide`.
- [x] HQ-05-05 — `HudControl::freeze` emits no event, leaves `current`/`pending` unchanged, and is a no-op for a successor press, another handle and a released control (`hold_hud` tests).
- [x] HQ-05-06 — The idle worker with no linger still parks with no timeout (existing no-polling test still passes); with a linger it parks for at most the remaining linger.
- [x] HQ-05-07 — `bun run lint`, `bun run format:check`, the `stache-hold-to-quit`, `stache-macos` and `stache-app` nextest suites and rustdoc pass; `docs/features/hold-to-quit.md` describes the linger and the gate in requirement 8.

## Manual check (per-run approval, disposable app)

Tap ⌘Q and release at about half the hold in TextEdit: the HUD stays about 1.5 s with the bar frozen, then hides, and TextEdit stays open. Tap again during the linger: a fresh hold starts. Set `dismissDelay` to `0`, then to `4000`, and repeat.
