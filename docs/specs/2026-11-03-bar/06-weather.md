---
status: draft
---

# 06 — Weather compatibility

Read when connecting weather after 05 and separate approval. Keep legacy providers and visuals; geolocation consent is approved behavior, not a test side effect.

## Scope

Add Bar weather config/env reader in Rust, app generated read contract, UI `bar/` weather/location/provider directories and tests; additive pinned CSP in `app/tauri.conf.json` (`csp`/`devCsp`, `:15-16`, admit only `ipc:` and localhost today) and necessary scoped usage descriptions. Preserve legacy `stores/WeatherStore/{location,providers}` mappings and `Weather.state.ts`, without the old store framework or widgets.

## Requirements

- Keep `auto` (Visual Crossing if nonempty key else Open Meteo), forced `visual-crossing` (unconfigured without key), and `open-meteo` (no key). Preserve actual UI `isProviderAvailable` logic: auto works without an env path/key, despite contradictory legacy Rust `is_enabled` comments. Audit provider normalization/weather-code icons/units/feels-like/default-location resolution and HTTP error behavior from actual provider files, not type comments. Label remains `Math.ceil(feelslike || 0)°C`, `Loading...` when no current data, null when unconfigured, legacy icon fallback.
- Location chain unchanged: browser geolocation with 2000ms timeout → `https://ipapi.co/json/` → `https://ipinfo.io/json` → `defaultLocation`, including display-name/coordinate parsing. Permission denial/unavailable/timeout proceeds, no coercion to bogus coordinates. Possible OS/browser consent remains; do not promise one prompt or Wi-Fi permission piggyback. Refresh location/weather at 20 minutes and reconnect as legacy, bounded retries/in-flight requests, cancellation/generation checks on stop/reconfigure.
- Retain source API shapes: Visual Crossing `https://weather.visualcrossing.com/VisualCrossingWebServices/rest/services/timeline/<encoded-location>/next5days` with metric/icons2/current,hours,days; Open Meteo `https://api.open-meteo.com/v1/forecast` with timezone auto/forecast_days5; city resolution `https://geocoding-api.open-meteo.com/v1/search` with count1/language en. Pin CSP to these three hosts plus ipapi.co/ipinfo.io, not wildcard connect/remote script/eval. Preserve audited request fields and normalized current-condition/icon results. Keep API fetches in scoped UI with mocked transports, no general Tauri HTTP permission. Invalid response/status/timeouts enter existing error/data states, never fallback to fake weather or alter provider selection silently.
- Read `VISUAL_CROSSING_API_KEY` from configured env file using injected config directory/home, trim/tilde/absolute/config-relative rules from 01. Actual legacy `config/env.rs` and `platform/path.rs` resolve relative to provided config directory; `weather.rs` falls back to `.` when config path absent. Explicitly document this CWD fallback removal and destination-directory migration ambiguity, not falsely claim all legacy paths were CWD-based. No process environment mutation or secret in logs, errors, query keys, localStorage/zustand, cross-window synchronization or generic host-status events. Narrow generated read contract is Bar-authorized; private query config does not persist or broadcast the key.
- Accepted weather config reload invalidates/refetches only relevant cached data; old location/request/key response cannot replace a newer revision. Env file errors produce absent key plus redacted actionable diagnostics; config/schema listing reads no key. Define env reads on accepted config load/refetch, no undocumented watcher framework. Native I/O off-main and bounded; stale result dropped.
- Weather click opens only Apple Weather (`com.apple.weather`) through 05's fixed action adapter, never a widget; appearance unchanged. Add generated enum/commands/capability access additively, no arbitrary URL input.

## Acceptance criteria

- [ ] BAR-06-01: Provider/availability/code/unit/location chain and 20-minute/reconnect behavior match audited legacy fixtures with deterministic errors/cancellation.
- [ ] BAR-06-02: Env paths, migration ambiguity, redaction and scoped key delivery work without environment mutation/persistence/leakage; approved geolocation behavior is documented honestly.
- [ ] BAR-06-03: CSP/usage descriptions admit only audited requirements, fixed Weather action uses typed adapter, parity and generated drift checks pass.

## Tests

Colocate location/provider/env/store/Weather tests. Fake geolocation success/denial/no API/timeout/late callback; mock every HTTP host and fail unexpected requests. Assert exact fallback order and no later call after success; invalid/missing coordinates, default city/coordinates, empty default, all provider modes with missing/empty key, weather-code maps, unit/ceil behavior and malformed/error responses. Fake 20-minute/reconnect scheduler; old config/key/response races, unmount cancellation and StrictMode disposal. Temp env fixtures quoted values/comments/missing/unreadable/absolute/tilde/relative/config-dir-vs-CWD and malformed entries; never actual home or environment. Secret sentinel absent from serialized public status/errors/query keys/persisted stores/logs. Extend independent parity matrix for every visible weather state. Tests cannot prompt or contact network; any live consent/network gate requires fresh approval.

## Notes

Update feature/config/UI/testing docs for actual providers, secret boundary and consent/CSP limits. No location permission added to the Wi-Fi adapter by this part.
