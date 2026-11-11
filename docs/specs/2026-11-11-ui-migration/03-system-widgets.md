---
status: draft
---

# 03 — System widgets

## Scope

Clock, CPU, Battery, Wi-Fi and Keep Awake as registered widgets, each in `widgets/<Name>/` with the four-file convention and its own lazy chunk. Depends on part 02.

## Requirements

Each widget renders into `BarItem`, reads its slice of the Bar snapshot with a `select`, and renders nothing while its reading is absent unless stated otherwise.

- **Clock.** UI-local time from one shared one-second ticker (a zustand store in `stores/` that runs its timer only while it has subscribers). Text `Wed Nov 11 13:04:05` in the system time zone (`en-US`, short weekday and month, two-digit day). Options: `show_date = false` drops the date; `show_seconds = false` drops the seconds and lets the ticker consumer re-render once a minute; `hour_cycle = "h12"` shows a 12-hour time with AM/PM. Clock icon. Click: `bar_action("clock")`.
- **CPU.** `usage%`, followed by `temperature°C` when a temperature is present and non-zero. At or above the `temperature_alert` option the icon switches to the "charge" variant and turns the danger color. Click: `bar_action("cpu")`.
- **Battery.** Hidden when the machine reports no battery reading. Label: `Loading...` without percentage or state; `100%` when full; `NN%` when the state is unknown or in compact mode; otherwise `NN% (State)`. Icon by charge: charging icon while charging; otherwise full at 100, then steps at 75, 50 and 25, empty below. Icon color: success while charging, warning while discharging, danger when empty, text otherwise. Click: `bar_action("battery")`.
- **Wi-Fi.** Hidden while the status is unknown. Connected: signal icon by RSSI (full at -60 dBm or better, medium to -70, low to -80, weakest below or when unknown), info color, and the network name in a `ScrollingLabel` when one is known. Disconnected: disconnected icon, warning color, label `Disconnected`. Off: off icon, danger color, label `Wi-Fi Off`. Click: `bar_action("wifi")`.
- **Keep Awake.** Hidden until the first observation. A coffee icon, filled and in the success color while the system is being kept awake, outlined in the text color otherwise. Icon-only, so it has an accessible name that states the current state. Click: `bar_session_action` with the action implied by the `paused` flag of the rendered view.
- Register the five kinds in the production registry.
- Confirm the exact field names and value spellings from the generated bindings before mapping them.

## Acceptance criteria

- [ ] MIG-03-01: Each widget renders every state listed above from snapshot fixtures and renders nothing for its absent state.
- [ ] MIG-03-02: Clock formats correctly for all eight option combinations, ticks on the second, and the ticker has no timer while no Clock is mounted. Two Clock instances share one timer.
- [ ] MIG-03-03: CPU honors `temperature_alert`, including the default and a changed value arriving in a reload.
- [ ] MIG-03-04: Battery's label follows compact mode; Wi-Fi's label scrolls only when the name overflows.
- [ ] MIG-03-05: Each click sends exactly its command; Keep Awake sends `pause` when not paused and `resume` when paused.
- [ ] MIG-03-06: A snapshot change in one reading re-renders only that widget.
- [ ] MIG-03-07: With the default layout, the end zone shows CPU, Battery, Keep Awake, Wi-Fi and Clock in order, and each widget chunk loads only when its kind is in the layout.
- [ ] MIG-03-08: `docs/features/bar.md` restores the sections for these widgets as shipped, including the new options.
- [ ] MIG-03-09: `bun run lint`, `bun run format:check` and `bun run test` pass.

## Tests

- One colocated test file per widget covering its state table, thresholds at their boundaries, absent state, click and accessibility name.
- Ticker store tests with fake timers: subscriber counting, minute alignment for `show_seconds = false`, time zone and DST safety (format from `Date`, never from arithmetic).
- Render-count test across widgets.
- End-to-end `system-widgets.spec.ts` through the fake host: boot with the default layout and realistic readings; change battery to charging, Wi-Fi to off, CPU above the alert; click each widget and assert the recorded commands; reload with a Clock option change and a reordered layout and assert the Clock keeps ticking in its new place.
- Screenshot baselines of the end zone in dark and light with a fixed clock.

## Notes

- Today's thresholds, labels and icon choices are in `main`'s `Battery.icons.ts`, `Battery.state.ts`, `Wifi.state.ts`, `Cpu.state.ts`, `Clock.state.ts` and `KeepAwake.tsx`, and in `docs/features/bar.md`. They are behavior references only.
