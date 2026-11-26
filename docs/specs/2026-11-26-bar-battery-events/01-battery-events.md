---
status: draft
---

# 01 — Power-source notifications wake the battery provider

## Scope

Add a native power-source change monitor in `stache-macos` and wire it into the Bar battery provider so a plug/unplug or percentage change is published within about a second.

Files: `src/crates/macos/src/system/` (new `battery_events.rs` next to `wifi_events.rs`, exported from `system/mod.rs`), `src/crates/modules/bar/src/providers/battery.rs`, `providers/mod.rs` (timing, signal wiring as for Wi-Fi), `providers/tests.rs`, `docs/features/bar.md`.

## Requirements

1. `stache_macos::system::BatteryEventMonitor` (name may follow the Wi-Fi monitor's conventions) installs an IOKit power-source notification (`IOPSNotificationCreateRunLoopSource`, or the `notify_register_dispatch` keys `com.apple.system.powersources.source` and `com.apple.system.powersources.timeremaining` if that is simpler and as fast; pick one and note why) and calls a sink on every change. It is created on the battery worker (or its own owned thread with a run loop), stops and releases every native handle when the provider monitor ends, and never runs on the main thread.
2. A signal wakes the battery provider's schedule through a coalescing flag (as Wi-Fi does): one sample right away, bursts coalesced, at least about 250 ms between two samples.
3. Safety sample every 120 s regardless of state. When the monitor cannot be installed, the provider keeps the current cadences (30 s charging, 120 s otherwise) and status shows a diagnostic.
4. Unchanged samples still publish nothing; failed reads keep the last reading, as today.
5. Fakes: an injectable signal source so tests drive it; the existing fake battery backend stays.

## Acceptance criteria

- [ ] Test: a power-source signal after a plug-in produces a published `Charging` reading within about 300 ms on the fake clock/backend, without waiting for the period.
- [ ] Test: a percentage change signal publishes the new percentage at once.
- [ ] Test: a burst of 20 signals produces at most two samples.
- [ ] Test: with the monitor unavailable the old cadences apply and a diagnostic is recorded.
- [ ] Test: stopping the provider monitor stops the signal source (no sink calls afterwards) and joins.
- [ ] `docs/features/bar.md` Cadences describe the event-driven battery and the fallback.

## Tests

Provider tests in `providers/tests.rs` with a fake signal source and fake clock, as the Wi-Fi signal tests do. A `stache-macos` unit test that the monitor installs and tears down (no real power change needed).

## Notes
