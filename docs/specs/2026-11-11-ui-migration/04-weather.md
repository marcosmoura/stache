---
status: done
---

# 04 — Weather

## Scope

The Weather widget in `windows/Bar/widgets/Weather/`: the inputs command, the two providers fetched from the webview, refresh rules, icon tables and click. Depends on part 02. No Rust change.

## Requirements

- **Query.** One `queryOptions` factory keyed by the accepted `configRevision` and the snapshot's `weatherEpoch`. Its `queryFn` calls `bar_weather`, then the selected provider, using the query `signal` so a superseded or unmounted request is aborted. It runs only while the Bar snapshot is `available`. The API key and coordinates stay inside the `queryFn` closure: never in a query key, store, log, error message or the DOM.
- **Refresh.** `staleTime: Infinity`, no retry, refetch every configured `refresh_interval` minutes (from the last ready answer; 20 until one is known; none when unconfigured), in the background too, and on reconnect. A new revision or epoch is a new query; an older answer can never replace a newer one.
- **Providers.** `auto` uses Visual Crossing when a non-empty key is present, otherwise Open Meteo. `visual_crossing` without a key is unconfigured, which hides the widget. `open_meteo` never uses a key. A failure never switches provider. Endpoints, parameters and the location rules (native coordinates; a `lat,lon` default parsed as numbers; a place name sent as typed to Visual Crossing and geocoded for Open Meteo; no location means no request) are those documented in `docs/features/bar.md` on `main`.
- **Failure.** A non-OK status, malformed JSON, a wrong shape or a 15 second timeout leaves the widget in its "no conditions" state with a fixed message that contains no URL, key or coordinate.
- **Display.** Condition icon and `N°C`, where N is the feels-like temperature rounded up and a missing value shows `0°C`. `Loading...` while configured without conditions. Hidden while unconfigured. Icons: the Visual Crossing `icons2` names and the WMO codes (night variants when it is not day) map to Hugeicons free icons, with a default for unknown names.
- **Click.** `bar_action("weather")`.
- Response parsing validates the shape of what it reads and nothing more.
- Register `weather` in the production registry.

## Acceptance criteria

- [x] MIG-04-01: Provider selection, the unconfigured case and every location rule are covered, including no request without a location.
- [x] MIG-04-02: Requests carry exactly the documented parameters; the key and coordinates appear in no query key, log, thrown message or rendered text.
- [x] MIG-04-03: Refresh follows the configured interval, a new revision or epoch refetches and aborts the previous request, and reconnect refetches.
- [x] MIG-04-04: Every failure mode renders `Loading...` with no secret in the error; success renders the icon and rounded-up temperature; `null` feels-like renders `0°C`.
- [x] MIG-04-05: Every icon name and WMO code in the tables maps to an existing icon; unknown values use the default.
- [x] MIG-04-06: The click sends `bar_action("weather")`.
- [x] MIG-04-07: `docs/features/bar.md` restores the Weather UI section as shipped.
- [x] MIG-04-08: `bun run lint`, `bun run format:check` and `bun run test` pass.

## Tests

- Provider and location unit tests with a stubbed `fetch`: URLs and parameters, parsing, each failure, timeout with fake timers, abort.
- Query tests: key contents, interval from the answer, revision and epoch change, stale answer discarded, not running while the Bar is unavailable.
- Icon table tests enumerating every key.
- Widget tests for each display state.
- End-to-end `weather.spec.ts`: the fake host answers `bar_weather` and Playwright routes the provider URLs; cover Visual Crossing success, Open Meteo with geocoding, a provider error, a config reload that changes the provider, and the click. Assert no request leaves for an unrouted host.
- Screenshot baselines for ready and loading in dark and light.

## Notes

- Behavior reference on `main`: `docs/features/bar.md` (Weather sections) and `Weather.api.ts`, `Weather.query.ts`, `Weather.codes.ts`, `Weather.icons.ts`.
- Outcome: a location fix with missing or non-finite coordinates counts as no location, so no request is made (`main` threw). Nothing was run against the real provider hosts.
