---
status: draft
---

# 05 — Clock, CPU, battery, Wi-Fi and fixed actions

Read when connecting system data after 04 and separate approval. Visual ownership remains part 04; extend its fixtures, do not change styles.

## Scope

Add `modules/bar/src/providers/{cpu,battery,wifi}.rs`, safe adapters/fakes in `macos/src/`, app bridge/bindings/action projection, UI provider hooks and colocated tests. Clock may stay a local deterministic time-derived React Query value as legacy, not a needless native roundtrip. Reuse the landed `CommandExecutor::run_captured`/`run_captured_prepared` (`sdk/src/commands.rs:847,864`; caller deadline, separate bounded stdout/stderr, cancellation, typed overflow/timeout) for system probes; do not build a Bar-local capture wrapper. Reuse `ApplicationControl` from main, not a Bar shell framework.

## Requirements

- Clock preserves `Intl.DateTimeFormat('en-US')`, 24-hour weekday/month/two-digit day/time with seconds assembled as `Clock.state.ts`, 1s cadence and system timezone. Freeze timezone/time in tests. Timers stop with surface/runtime, not global forever.
- CPU preserves rounded usage/temperature, temperature nullable, valid 0<temp<150°C, SMC CPU sensor average first (TC/Tp/Te/Tf fallback keys), then optional `ismc temp -o json`, then `smctemp -c`; unknown temperature remains unknown. Preserve 2s cadence and >=85°C icon/red threshold. Native SMC feasibility on target hardware is gated; missing optional binaries must not make Bar unavailable. Shared runner supports fixed executable/argument vectors, executable resolution (including legacy Homebrew prefixes), bounded stdout/stderr/deadline/cancellation and kill/reap of only the exact owned child. No shell interpolation, install or arbitrary command input.
- Battery via `starship-battery` or safe IOKit projection preserves legacy percentage/state/absence and icon map. 30s charging, 120s otherwise; no battery hides component, Full=100%, compact percentage at <=2036px, otherwise legacy state label/color. Read providers off-main and publish complete DTOs; failure never invents charge/temperature/SSID.
- Wi-Fi safe native power/interface/RSSI first where prompt-free; SSID uses the audited `ipconfig getsummary <interface>` compatibility path only after an explicitly approved macOS 26 feasibility run. No Location permission request/entitlement workaround or private Apple80211 fallback. Use typed interface identity, bounded runner and legacy summary parsing; sanitize/bound untrusted output. Unavailable/redacted SSID stays null; do not infer disconnected merely from missing SSID. Preserve Unknown/Off/Disconnected/Connected, labels and RSSI thresholds -60/-70/-80, 5s maximum sampling cadence, native change signals/coalescing where feasible. Gate failure is documented degradation, not a new prompt.
- Providers start only for accepted enabled/unpaused Bar; one owned sample schedule, bounded in-flight work/queues, no sample overlapping itself and no storm catchup. Slow/hung read cannot block host/main/other providers; use cancellable bounded adapter/runner and identity checks, retained owner for unresolved native work. Stop cancels, wakes and drains exact-generation workers/children; late completion cannot republish. Stage reconfigure until accepted revision. Error/unavailable projection follows legacy visual fixture branches, with typed diagnostics outside Bar visuals; no new warning UI.
- Fixed typed action enum: CPU→Activity Monitor (`com.apple.ActivityMonitor`), Clock→Calendar (`com.apple.iCal`), Battery→`x-apple.systempreferences:com.apple.Battery-Settings.extension`, Wi-Fi→`x-apple.systempreferences:com.apple.wifi-settings-extension`. Weather/Media enum cases arrive with 06/07. Host determines target, accepts no arbitrary name/path/URL/argv and validates real Bar caller identity. No general `open_app` command or Settings/widget surface. Preserve click/hover styles exactly.
- Reuse main's exact application resolution/open/issuance-permit adapter (`ApplicationControl`, `macos/src/applications.rs:588`). Its `open` takes only a file `Path` (`:1337-1341`), so add a narrow safe fixed-settings-URL operation for the two settings targets; do not pretend arbitrary URL support ships. Bound acks and distinguish stale/not issued, issued/completed, refused/not installed, timeout-before-issuance and uncertain-after-issuance; no automatic retry after uncertainty. Return concrete Rust serde/Specta outcomes in generated command result data (`Response::Ok` data if routed through IPC), never JSON-string errors or claims that issued means launched. Cancel stale clicks after runtime shutdown; current action revalidates authorization/generation at native send.

## Acceptance criteria

- [ ] BAR-05-01: Clock/CPU/battery DTOs, formatting/fallbacks/cadences and null states pass fake-time/source fixtures with no visual delta.
- [ ] BAR-05-02: Wi-Fi native/summary adapters preserve status/icon semantics without requesting Location; live feasibility is explicitly recorded or degraded/unverified.
- [ ] BAR-05-03: Fixed authorized clicks and typed uncertain outcomes round-trip generated commands; no arbitrary launcher or unsafe retry is exposed.
- [ ] BAR-05-04: Bounded generation-owned sampling/runner shutdown passes stall/race tests and part 00 performance method/budget is recorded honestly; affected behavior/testing docs are updated.

## Tests

Colocated Rust/provider and WebKit hooks tests: fake CPU warmup/threshold/invalid SMC/all fallback orders, missing tools, malformed/nonfinite/oversized output, stalled child, cancellation/reap, percentage bounds/state transitions/no battery, Wi-Fi no interfaces/multiple interfaces/power unknown/redacted SSID/quoted SSID/RSSI thresholds and storm coalescing. Fake-clock windows assert exactly bounded 1s/2s/5s/30s/120s schedules without real sleeping and zero disabled samples/workers. Action adapter fakes cover missing app, rejected URL, stale permit, completion inline after issuance, ack timeout before/after issuance, stop during preflight and no successor effects. Real bridge serialization and unauthorized test/shell/forged labels are tested, not merely UI mocks. Run parity matrix for populated/empty/error data and fixed-action hover behavior. All native/runner operations fake by default; SMC/SSID/launch/performance manual runs each need separate approval.

## Notes

Materialize provider/action/limitations sections in feature and UI docs when implemented. No mandatory executable requirement for optional temperature tools; no live power/audio/input mutations for validation.
