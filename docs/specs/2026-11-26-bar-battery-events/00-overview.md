---
branch: fix/bar-battery-events
---

# Bar: battery changes show at once

Read when planning or implementing the 2026-11-26 change that makes the Bar's battery reading event driven.

## Goal

Plugging or unplugging power and every change of the battery percentage reach the Bar within about a second, instead of after up to 30 s (charging) or 120 s (otherwise).

## Background

`stache-bar` `providers/battery.rs` (`BatterySampler`) polls `stache_macos::system::SystemBattery` (`starship-battery`) every 30 s while charging and 120 s otherwise (`BATTERY_CHARGING_PERIOD`, `BATTERY_IDLE_PERIOD` in `providers/mod.rs`). The UI already updates as soon as a changed reading is published (`BarChanged`), so the delay is the cadence alone. The Wi-Fi provider already wakes its schedule from native change signals (`stache_macos::system::wifi_events`, one coalescing flag per kind) with a safety sample; battery should follow the same shape.

macOS posts power-source changes through IOKit: `IOPSNotificationCreateRunLoopSource` (any power-source change, including the time-remaining and capacity updates the OS publishes as the percentage moves) and the `com.apple.system.powersources.source` / `...powersources.timeremaining` notify keys.

## Non-goals

- No change to the reading's shape (`BatteryReading`), states or UI rules.
- No new configuration key.
- CPU and Wi-Fi providers unchanged.

## Decisions

- **Event driven, like Wi-Fi.** A native power-source change signal wakes the battery provider, which samples at once. Signals coalesce (a burst is one extra sample), with at least about 250 ms between samples.
- **Safety poll stays** as a fallback at 120 s in every state (a missed or unavailable notification is caught). When the native signal cannot be installed, the provider falls back to today's cadence and records a diagnostic.
- **Lifecycle:** the signal source lives only while the battery provider runs (same start/stop, cancel and join rules as the Wi-Fi monitor). No thread, timer or native handle without a running Bar with a panel.

## Parts

| Part                       | Summary                                                                 | Status |
| -------------------------- | ----------------------------------------------------------------------- | ------ |
| [01](01-battery-events.md) | Power-source change notifications wake the battery provider immediately | draft  |

## Affected docs

- `docs/features/bar.md` (Cadences and Readings).
