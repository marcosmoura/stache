---
status: done
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

- [x] Test: a power-source signal after a plug-in produces a published `Charging` reading within about 300 ms on the fake clock/backend, without waiting for the period.
- [x] Test: a percentage change signal publishes the new percentage at once.
- [x] Test: a burst of 20 signals produces at most two samples.
- [x] Test: with the monitor unavailable the old cadences apply and a diagnostic is recorded.
- [x] Test: stopping the provider monitor stops the signal source (no sink calls afterwards) and joins.
- [x] `docs/features/bar.md` Cadences describe the event-driven battery and the fallback.

## Tests

Provider tests in `providers/tests.rs` with a fake signal source and fake clock, as the Wi-Fi signal tests do. A `stache-macos` unit test that the monitor installs and tears down (no real power change needed).

## Notes

- Native source: `notify_register_dispatch` on a global utility queue for `com.apple.system.powersources.source` (plug/unplug), `.percent` (added so percentage moves fire directly), `.timeremaining` and `com.apple.system.powersources` (`kIOPSNotifyAnyPowerSource`, any attribute change, so a charging-state change that posts none of the others, such as charging starting seconds after plug-in or reaching a charge limit, still wakes the provider). `.source`, `.timeremaining` and the any-power-source key are public in `IOPowerSources.h`; `.percent` is a private key `powerd` posts, not in the public SDK. Chosen over `IOPSNotificationCreateRunLoopSource` because it needs no `CFRunLoop` and no owned thread; the libnotify/libdispatch symbols are in libSystem, declared with `extern "C"`, and the handler uses the existing `block2` dependency. No new dependency.
- Teardown: dropping `BatteryEventMonitor` clears an armed flag (an in-flight callback calls nothing) and `notify_cancel`s every token.
- Only root may post `com.apple.system.*` keys, so the `stache-macos` test checks install/teardown on the real keys and delivery plus no-call-after-drop on a private test key. A real plug/unplug inside the app is unverified on hardware.
- The battery signal reuses the Wi-Fi `Wakeup` as an identity signal and the Wi-Fi `SignalState`; `ProviderTiming.battery_min_gap` (250 ms) is the gap.
