---
status: draft
---

# 03 — Bar: native Wi-Fi change signal

## Scope

The Bar's Wi-Fi provider learns about changes from `CoreWLAN` instead of asking every 5 s, and runs `ipconfig getsummary` only when the network may have changed. Decision D6 of the [overview](00-overview.md).

Files expected to change: `src/crates/macos/src/system/wifi.rs` (and `system/mod.rs` exports), `src/crates/modules/bar/src/providers/{mod.rs, wifi.rs}`, `src/crates/modules/bar/src/lib.rs` exports, the Bar status diagnostics where provider notes are listed, and `docs/features/bar.md`, `docs/guides/performance.md`.

## Requirements

- **Signal source.** `stache-macos` gains a `CoreWLAN` event source: its own `CWWiFiClient` instance (never the shared one, whose delegate it does not own), a delegate object, and `startMonitoringEventWithType:error:` for power, SSID, BSSID, link and link quality. Dropping the guard stops all monitoring and clears the delegate. The delegate methods only classify and call the subscriber: no read, no allocation beyond the call, no lock held while calling out. An interrupted or invalidated client connection counts as an identity signal.
- **Location.** The source calls nothing that needs Location Services. The module documentation states which header lines say so, as `wifi.rs` already does for the reads.
- **Signal kinds.** `WifiSignals::watch` delivers a kind: identity (power, SSID, link, connection interrupted or invalidated) or quality (link quality, BSSID). The provider's coalescing wake keeps one pending flag per kind.
- **Sampler.** The Wi-Fi sampler keeps the last `ipconfig` summary per interface. It runs `ipconfig` when there is no summary yet, when an identity signal arrived since the last summary, when the native reading's power or association state differs from the one the summary was taken under, and on a safety sample. Otherwise it publishes the native reading with the kept network name. With the SSID gate off it never runs `ipconfig`, as today.
- **Schedule with signals.** An identity signal samples after `WIFI_MIN_GAP` (500 ms) as today. Quality signals lead to at most one sample per 5 s. A full safety sample, including `ipconfig`, runs every 60 s.
- **Schedule without signals.** When the source cannot be installed the provider runs exactly today's schedule, one full sample every 5 s, and the Bar status carries one deduplicated diagnostic saying that Wi-Fi change notifications are unavailable and the provider polls.
- **Composition.** `ProviderParts::system()` wires the native source. Subscription still happens on the provider worker, never on the main thread, and ends with the monitor.
- **Docs.** `docs/features/bar.md` (the provider table and the fallback), `docs/guides/performance.md` (child process cadence).

## Acceptance criteria

- [ ] PF-03-01: With signals installed and none delivered, ten minutes on the fake clock run `ipconfig` once at start and once per 60 s, and nothing else.
- [ ] PF-03-02: An identity signal runs `ipconfig` once within the minimum gap; a storm of identity signals runs it once.
- [ ] PF-03-03: A storm of quality signals for one minute runs at most one native sample per 5 s and no `ipconfig`.
- [ ] PF-03-04: A native reading that goes from associated to not associated without any signal runs `ipconfig` at the next sample and clears the kept name.
- [ ] PF-03-05: With the source unavailable the cadence equals today's (one `ipconfig` per 5 s) and the diagnostic appears once.
- [ ] PF-03-06: A stopped monitor issues no sample and no child afterwards, and the guard's drop stops monitoring.
- [ ] PF-03-07: `bun run lint`, `bun run format:check` and `bun run test` pass.

## Tests

- Provider tests on the fake clock with a fake `WifiSignals`, a fake `WifiBackend` and a recording process factory for PF-03-01 to PF-03-06.
- A unit test of the delegate's classification (each selector to its kind) without a live client, the way the location delegate is tested.
- A Bar scenario in the existing provider suite: connected to network A, an identity signal arrives, the summary now reports network B, the published reading shows B; then the radio turns off and the reading shows off with no name.

## Notes

- Native behaviour is unverified: whether monitoring installs without an entitlement on macOS 26 and how often link-quality events fire. The fallback keeps today's behaviour if it does not install.
- For the user to check: the Bar's network name changes when switching networks; `stache` status shows no Wi-Fi diagnostic.
