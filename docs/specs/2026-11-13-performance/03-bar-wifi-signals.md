---
status: done
---

# 03 — Bar: native Wi-Fi change signal

## Scope

The Bar's Wi-Fi provider learns about changes from `CoreWLAN` instead of asking every 5 s, and runs `ipconfig getsummary` only when the network may have changed. Decision D6 of the [overview](00-overview.md).

Files expected to change: `src/crates/macos/src/system/wifi.rs` (and `system/mod.rs` exports), `src/crates/modules/bar/src/providers/{mod.rs, wifi.rs}`, `src/crates/modules/bar/src/lib.rs` exports, the Bar status diagnostics where provider notes are listed, and `docs/features/bar.md`, `docs/guides/performance.md`.

## Requirements

- **Signal source.** `stache-macos` gains a `CoreWLAN` event source: its own `CWWiFiClient` instance (never the shared one, whose delegate it does not own), a delegate object, and `startMonitoringEventWithType:error:` for power, SSID, BSSID, link and link quality. Dropping the guard stops all monitoring and clears the delegate. The delegate methods only classify and call the subscriber: no read, no allocation beyond the call, no lock held while calling out. An interrupted client connection counts as an identity signal (`CoreWLAN` re-registers the events itself). An invalidated connection is permanent, so it is reported as the loss of the source.
- **Location.** The source calls nothing that needs Location Services. The module documentation states which header lines say so, as `wifi.rs` already does for the reads.
- **Signal kinds.** `WifiSignals::watch` delivers a kind: identity (power, SSID, link, connection interrupted), quality (link quality, BSSID) or lost (connection invalidated). The provider's coalescing wake keeps one pending flag per kind.
- **Sampler.** The Wi-Fi sampler keeps the last `ipconfig` summary per interface. It runs `ipconfig` when there is no summary yet, when an identity signal arrived since the last summary, when the native reading's power or association state differs from the one the summary was taken under, and on a safety sample. Otherwise it publishes the native reading with the kept network name. With the SSID gate off it never runs `ipconfig`, as today.
- **Schedule with signals.** An identity signal samples after `WIFI_MIN_GAP` (500 ms) as today. Quality signals lead to at most one sample per 5 s. A full safety sample, including `ipconfig`, runs every 60 s.
- **Schedule without signals.** When the source cannot be installed the provider runs exactly today's schedule, one full sample every 5 s, and the Bar status carries one deduplicated diagnostic saying that Wi-Fi change notifications are unavailable and the provider polls. A lost source leads to the same state: one full sample at once, then today's schedule and the diagnostic for the rest of the run.
- **Composition.** `ProviderParts::system()` wires the native source. Subscription still happens on the provider worker, never on the main thread, and ends with the monitor.
- **Docs.** `docs/features/bar.md` (the provider table and the fallback), `docs/guides/performance.md` (child process cadence).

## Acceptance criteria

- [x] PF-03-01: With signals installed and none delivered, ten minutes on the fake clock run `ipconfig` once at start and once per 60 s, and nothing else.
- [x] PF-03-02: An identity signal runs `ipconfig` once within the minimum gap; a storm of identity signals runs it once.
- [x] PF-03-03: A storm of quality signals for one minute runs at most one native sample per 5 s and no `ipconfig`.
- [x] PF-03-04: A native reading that goes from associated to not associated without any signal runs `ipconfig` at the next sample and clears the kept name.
- [x] PF-03-05: With the source unavailable the cadence equals today's (one `ipconfig` per 5 s) and the diagnostic appears once. After a lost signal the provider samples at once and continues the same way.
- [x] PF-03-06: A stopped monitor issues no sample and no child afterwards, and the guard's drop stops monitoring.
- [x] PF-03-07: `bun run lint`, `bun run format:check` and `bun run test` pass.

## Tests

- Provider tests on the fake clock with a fake `WifiSignals`, a fake `WifiBackend` and a recording process factory for PF-03-01 to PF-03-06.
- A unit test of the delegate's classification (each selector to its kind) without a live client, the way the location delegate is tested.
- A Bar scenario in the existing provider suite: connected to network A, an identity signal arrives, the summary now reports network B, the published reading shows B; then the radio turns off and the reading shows off with no name.

## Notes

- Measured on the branch (2026-11-13) with the provider worker on the fake clock and a recording process factory, ten idle minutes while connected: 120 `ipconfig` children without a signal source (12 per minute, today's cadence) and 10 with signals installed (one at start, then one per 60 s). A one-minute storm of quality signals runs no child and at most one native sample per 5 s.
- Found in review (2026-11-13): `CWWiFiClient.h` documents that an interrupted connection re-registers its events and that an invalidated one is permanent. Treating invalidation as a one-off identity signal would have left the provider on the 60 s safety schedule with no signal source, so it ends the signal mode instead.
- Probe (2026-11-13, macOS 27.3, a standalone command-line binary outside the repo, not the app): an owned `CWWiFiClient` installed all five event types without an entitlement; six link-quality callbacks arrived in 40 s on `CoreWLAN`'s own queue (`com.apple.wifi.xpcclient.event.…`) with no run loop on the creating thread or the main thread; stopping succeeded. With the daemon lookup blocked (a sandboxed run) every install failed with an error and `clientConnectionInvalidated` was delivered, which is the fallback path.
- Still unverified: the same calls inside the app, and the power, SSID and link callbacks on a real network change (the probe only listened). The fallback keeps today's behaviour if monitoring does not install.
- For the user to check: the Bar's network name changes when switching networks; `stache` status shows no Wi-Fi diagnostic.
