# UI SDK and headless bindings

Read when changing the typed command/event surface, the generated TypeScript
bindings, the on-demand test window or their drift checks.

The native host, tray, test-window route and headless binding export ship. The
React client consumes that contract through typed SDK wrappers, a validated
`Shell` window router and the on-demand `TestWindow`; the browser suites run in real
WebKit.

## Layout

```text
src/ui/src/
  sdk/                  generated bindings, client wrappers, status feed, tokens
  shell/
    Shell.tsx           window router: maps each `?surface=` value to one window
    components/         reusable UI shared by windows (Surface, Button, Stack, Icon,
                        ScrollingLabel, QueryErrorFallback: legacy Bar primitives)
    hooks/, utils/      useMediaQuery, useDisableRightClick, the laptop media query
    windows/            one directory per window surface
      AccessibilityGuide/
      Bar/              the status bar: Media, Status, Clock, Cpu, Battery, KeepAwake,
                        Wifi, Weather, Spaces, each with .styles.tsx, tests and index.ts
      Placeholder/      development fallback for the `shell` surface
      TestWindow/
```

Each directory holds the component, its `.styles.tsx`, a colocated test and an
`index.ts`. A new window gets its own `windows/<Name>/` directory and a route in
`Shell`; pieces used by more than one window move to `components/`.

## Versions

| Dependency          | Pin            | Why                                                                |
| ------------------- | -------------- | ------------------------------------------------------------------ |
| `tauri`             | `=2.12.5`      | Host pin; `tauri-specta` enables its `specta` feature.             |
| `tauri-build`       | `=2.7.4`       | Build crate; `tauri 2.12.5` requires `tauri-build ^2.7.4`.         |
| `tauri-specta`      | `=2.0.0-rc.26` | Documented Tauri 2 binding tool.                                   |
| `specta`            | `=2.0.0-rc.26` | Exact version required by `tauri-specta` and the exporter.         |
| `specta-typescript` | `=0.0.13`      | TypeScript exporter used by the generator.                         |
| `@tauri-apps/api`   | `2.12.4`       | Runtime imported by generated bindings; must match the core minor. |
| `@tauri-apps/cli`   | `2.12.4`       | Build/dev CLI; it drives the same 2.12 core.                       |

`tauri-specta =2.0.0-rc.26` declares `specta =2.0.0-rc.26` and
`specta-typescript ^0.0.13`, so the resolved versions are
`specta 2.0.0-rc.26` and `specta-typescript 0.0.13`. Do not upgrade one of them
independently: newer `specta` and `specta-typescript` releases do not resolve until
`tauri-specta` moves to them. `tauri` also enables its `specta` feature so `tauri::State`,
`AppHandle` and window arguments are omitted from generated command parameters.

## Shared collection

`stache_app::bindings::builder()` owns the command and event collection. The
desktop host and the generator both use it, so there is no second list to keep in
sync:

```rust
let bindings = stache_app::bindings::builder();
tauri::Builder::default()
    .invoke_handler(bindings.invoke_handler())
    .setup(move |app| {
        bindings.mount_events(app);
        Ok(())
    })
```

Commands are concrete Rust functions (`collect_commands!`); events have concrete
payloads (`collect_events!`). Nothing goes through a generic JSON `Value` facade,
and there is no hand-written event-name file: generated `events.statusChanged` is
the only client-side name source.

The shipped surface is:

```text
commands.appInfo()      -> AppInfo       static product name, version, identifier
commands.hostStatus()   -> HostStatus    last published snapshot, no module locks
events.statusChanged    -> StatusChanged { status }
```

`host_status` reads the host's published snapshot cell, so it never waits for a
module, a worker join or a socket operation.

## Host status DTOs

- `HostStatus { ready, error, access, modules, config }`.
- `AccessView { state, revoked, appPath }` is the Accessibility launch gate: `state` is
  `checking`, `blocked` or `open`, `revoked` is true when trust was lost after being open in this
  launch, and `appPath` is the enclosing `.app` path of the running executable (`null` for an
  unbundled binary). A gated host is `ready: true` with `access.state = "blocked"`.
- `ModuleStatusView { id, state, enabled, paused, revision, detail, diagnostic }`.
  `state`/`detail` are runtime facts; `diagnostic` is a rejected configuration.
  `revision` is an exact decimal **string**: JavaScript numbers cannot hold every
  SDK `u64`. Treat it as opaque text and never call `Number()` on it.
- `ConfigFileView { file, path, state, active, diagnostic, warnings }` separates
  file validity (`missing`, `valid`, `invalid`) from active runtime bytes.
- `ModuleState` is `stopped`, `disabled`, `running`, `paused`,
  `waitingForPermission`, `unavailable`, `failed`.

A rejected reload keeps the running module's `state` and records `diagnostic`; it
does not become `failed`.

## Config types

`GlobalConfig` and `ProxyAudioConfig` are exported with explicit `.typ::<T>()`
calls from their authoritative crates. Specta derives live on those types only;
the generated file is never hand-mirrored.

The pilot's read-only listing DTOs — `AudioDeviceInfo`, `AudioListKind` and
`AudioListParams` — are exported from the same builder. They describe the
`proxyAudio.list` socket request and response, but the pilot has no Tauri
command and no UI module: launch still creates no window and no webview.

`WallpapersSetParams`, `WallpaperSelection`, `WallpaperScreen`,
`WallpapersSetOutcome`, `WallpapersOutcomeStatus`, `WallpapersOutcomeCause`,
`WallpapersTargetOutcome`, `WallpapersTargetResult` and
`WallpapersTargetFailure` are exported the same way. They describe the
`wallpapers.set` socket request and its typed domain outcome; no Tauri command
or UI surface consumes them yet.

`TilingConfig` and its nested wire types (`LayoutType`, `EasingType`,
`AnimationConfig`, `MasterConfig`, `MasterPosition`, `WorkspaceConfig`,
`WindowRule`, `GapsConfigValue`, `GapsConfig`, `ScreenGapsConfig`, `GapValue`,
`FloatingConfig`, `FloatingPosition`, `FloatingPreset`, `DimensionValue`,
`BordersConfig`, `BorderStateConfig`, `GradientConfig` and
`BorderAnimationConfig`) are exported the same way. They are catalog types for
the Tiling configuration; the shipped Tiling runtime consumes them when `enabled = true`.

Serde phases make `ProxyAudioConfig` and `AudioDevicePriority` unions of
`_Serialize` and `_Deserialize` shapes. Unified serde mode is rejected because
`skip_serializing_if` cannot be represented there. The JSON Schemas remain the
validation authority; the TypeScript types describe the wire shape.

## Generate and check

```sh
bun run generate-bindings
bun run generate-bindings -- --check
```

The generator is the Rust binary
`cargo run -q -p stache-app --features dev-tools --bin generate-bindings`. It writes
`src/ui/src/sdk/bindings.gen.ts`. `--check` exports to a secure
`tempfile::NamedTempFile`, compares bytes, prints the stale path and exits
nonzero without modifying the committed file or source. `bun run check:generated`
runs both the schema and binding checks; `bun run lint` and `bun run build` call
it, so CI fails on drift.

The generated file is excluded from oxfmt, oxlint and lint-staged: specta emits
tabs and its own layout. The generator only trims trailing line whitespace before
writing or comparing. Never hand-format or edit it; regenerate it instead.

## Test window

Launch creates no windows and no Dock icon (`ActivationPolicy::Accessory`). The
tray's **Open Test Window** action is the only launch path that creates a webview:

- Rust opens one `Standard` window through `NativeWindows`, labelled
  `stache-test-<n>` where `<n>` is a monotonic host id.
- The URL is the app URL plus `?surface=test`; `src/ui/index.html` remains the
  Vite entry, so the router reads `surface` from the query string. There is no
  arbitrary URL, JavaScript URL or native flag input.
- The `stache-app` crate defines `custom-protocol = ["tauri/custom-protocol"]`
  and `bun run tauri:build` passes it. Without that feature Tauri stays in dev
  mode, loads `devUrl`, and a packaged window has no document; a release build
  fails in `build.rs` instead of shipping that state. `smoke` implies it so the
  opt-in harness exercises the embedded bundle.
- Module and host requests are validated twice: the SDK capability checks the
  declared surface, kind and size, and `NativeWindows` repeats the check for
  host-owned requests. Only the shipped `test` and `access` surfaces are implemented; surface
  spellings must be ASCII letters, digits, `-` or `_`, so they cannot add a query
  parameter.
- The same owner/key request reuses and focuses the existing window; closing it
  removes the bookkeeping entry and allows a new window. Closing the last window
  only removes that state: Tauri's `ExitRequested` arrives with `code: None`
  there, so the handler prevents the exit without starting the host teardown.
  Module stop (including a failed start) releases every window owned by that
  module. Repeated requests share one lease; the last released handle closes the
  native window.
- `WindowKind::Launcher` returns `WindowError::UnsupportedKind` until a tool spec
  defines its native semantics. A module may only request a surface declared in
  its descriptor.
- `WindowKind::NonActivatingPanel` is landed for surface `bar` only; `bar` is
  admitted only as a panel, and `test`/`access` stay `Standard`-only. Bar's
  production factory is chained next to Tiling in `lib.rs` (default disabled,
  inert until `enabled = true`) and requests it; the `bar` route and the
  `bar-window` capability exist. A panel request must carry an explicit
  logical `origin` (a `Standard` request must not); size validation is the
  shared one. `stache-bar-<n>` is built undecorated, transparent, shadowless,
  not resizable, hidden at build, unfocused and unfocusable, accept-first-mouse,
  all Spaces, dark theme, not closable/minimizable/maximizable, drag-drop off,
  at the requested position and size. It **never** goes through Standard's
  `show()` + `set_focus()`. Tauri owns the window and the webview and keeps its
  window hidden forever; right after the build the `stache-macos` panel host
  (`panel.rs`, main thread only) creates a native `NSPanel` with
  `Borderless | NonactivatingPanel` **at init**, moves the webview into it, and
  configures dark appearance, `canJoinAllSpaces | stationary` (never
  `fullScreenAuxiliary`), the hover tracking area and `BelowNormalWindowLevel`
  (last), then orders it front without activating the app. No class of a Tauri
  or AppKit object is changed (a class swap broke KVO and crashed on close;
  see the live evidence in [testing](../guides/testing.md)). Hosted state lives
  in a main-thread registry keyed by label. `unhost` (webview back into Tauri's
  window, panel closed) runs before every Tauri close of the window; a
  `Destroyed` event for a still-hosted label only drops the state. Reposition
  updates Tauri's hidden window and then mirrors its frame onto the panel.
  Focus on a panel is a no-op. The hosting was verified live on macOS 27.0
  (2026-11-09, committed host `377512b`: no activation on click, hover while
  another app is active, below normal windows, clean close; see the
  [part 03 evidence](../specs/2026-11-03-bar/03-panel-window.md)). No private SkyLight
  call is used.
- **Bar data bridge authorization (slice c).** `bar_snapshot` takes the actual
  calling `WebviewWindow` injected by Tauri; its label must be the live window held
  by owner `bar` (`stache-bar-<digits>`). The test, access, shell and any forged or
  stale Bar label get the typed `{"kind":"unauthorized"}`. A Bar-shaped label while
  Bar is not running gets `{"kind":"unavailable","reason":...}` (`disabled`,
  `paused`, `waitingForPermission`, `failed`, `starting`, `stopping`, `stopped`,
  `unavailable`) with `generation`/`sequence`, immediately and without stale data.
  The UI renders `menu.state === "visible"` as the Bar slid away (translateY(100%),
  opacity 0, as legacy `menuHidden` true). The `bar-window`
  capability (`capabilities/bar.json`, label `stache-bar-*`) grants only
  `allow-bar-snapshot`, `allow-bar-action`, `allow-bar-weather`, `allow-bar-media-artwork` and `allow-bar-session-action` (all five commands
  are ACL-gated in the app manifest, and each also authorizes the calling label
  itself),
  `core:event:allow-listen` and `core:event:allow-unlisten`. `bar_action` takes a
  closed `BarAction` enum only (part 05 slice b); the CPU, Clock, Battery, Wi-Fi and Weather
  buttons call it through `barClick` (`Bar.actions.ts`) and render nothing for its
  outcome. `bar_weather` returns the weather inputs (including the API key and
  coordinates) to this window only (part 06 slice B); the Weather hook consumes it (slice C). `bar_session_action` pauses/resumes `keepAwake` only (the Keep Awake button calls it through `keepAwakeClick`; the outcome renders nothing). `bar_media_artwork` returns the 128x128 artwork PNG (base64) for an `artworkId` of `BarView.media` (part 07 slice B; `useMediaArtwork` consumes it, slice C). No close,
  drag, filesystem, shell, opener, HTTP or plugin permission is granted, and
  `app.security.capabilities` lists `bar-window` explicitly.
- `OwnedWindow::reposition(WindowFrame)` moves and resizes the exact live window
  of that owner and session on the main thread; a stale handle, another owner or
  a closed window is refused and a queued move is dropped if a close wins.
- Lifecycle phases are tracked in `stache_sdk::WindowPhases` and read through
  `OwnedWindow::phase()` / `wait_phase_change()`: `Requested` (queue accepted
  only), `Created` (native build returned), `Loaded` (page finished), `Failed`
  (create error, with reason; a page-load failure is only reported through the
  host `WindowEvents::page_failed` hook, because Tauri exposes no load-failure
  event), `Closing` and `Closed` (the `Destroyed` event). Phases never move
  backwards, so a late created/loaded callback after a close or failure is
  ignored.

`src/crates/app/capabilities/test-window.json` grants the test labels only
`core:event:allow-listen`, `core:event:allow-unlisten` and
`core:window:allow-close`. `app.security.capabilities` lists it explicitly, so a
new capability file is not silently enabled. No filesystem, shell, dialog, HTTP,
media, weather, system or tray permission applies.

## Accessibility guidance window

While the Accessibility launch gate blocks, the host opens one more host-owned
`Standard` window, surface `access` (`?surface=access`), key `access`, 480×640
logical pixels (not resizable), labelled `stache-access-<n>`:

- Chrome comes from `chrome_for(surface)` in `windows.rs`, carried in
  `NativeWindowSpec`: `access` is frameless (`decorations(false)`), transparent,
  not resizable and keeps the native shadow; `test` keeps the decorated, opaque,
  resizable defaults. There is no title bar or traffic lights. Transparency needs
  Tauri's `macos-private-api` cargo feature and `app.macOSPrivateApi: true` in
  `tauri.conf.json` (app-wide; it authorizes no other private API). The page sets
  `html[data-surface='access']`, which makes `html`, `body` and `#root` transparent
  for this surface only.

- The runner opens it once per gated episode on each `Blocked` transition (first
  check or revocation) through `NativeWindows`; `window.set_focus` brings the
  Accessory app and the window forward, exactly like the test window. A user
  close does not reopen it in the same episode. The tray row `access-guide`
  (**Show Accessibility Help…**) opens or focuses it again; the next episode
  opens it automatically. On `Opened` the runner closes it through the release
  path, and the React surface also closes itself when `status-changed` reports
  `open`.
- A trusted launch and an ungated host (empty baseline) never create it, so the
  `webviews=0` startup record is unchanged on those paths. The startup record is
  written before the first permission check; a gated launch logs
  `access: guidance window requested` when the host opens the window.
- `NativeWindows` accepts exactly the `test` and `access` host requests; every
  other surface, kind or invalid size stays refused.
- Commands in `bindings::builder()`, all returning `()` immediately with results
  delivered through `status-changed`: `requestAccessibility()` (explicit request,
  downgraded to a check while open), `openAccessibilitySettings()` and
  `quitApp()` (`HostHandle::request_shutdown`).
- `src/crates/app/capabilities/access-window.json` grants `stache-access-*`
  labels only `core:event:allow-listen`, `core:event:allow-unlisten`,
  `core:window:allow-close`, `core:window:allow-start-dragging` (header drag) and
  `allow-request-accessibility`,
  `allow-open-accessibility-settings`, `allow-quit-app`. `build.rs` declares those
  three commands in the Tauri app manifest, so they are ACL-gated: the test
  window's capability does not include them and cannot call them. Their
  autogenerated permission files live in `src/crates/app/permissions/autogenerated/`.
  `app.security.capabilities` lists both capabilities explicitly.
- `AccessibilityGuide` (`src/ui/src/shell/windows/AccessibilityGuide/`) reads
  `appInfo()` and the host status through the shared React Query feed (no new
  event). States: checking (loading line), blocked (guidance, identity line with
  name, identifier and `access.appPath`, collapsed troubleshooting with an
  unbundled variant), revoked (alternate title and line) and open ("Access
  granted. Starting Stache Next…", then it closes itself). Buttons disable while
  their command is in flight and `HostStatus.error` is shown when present.
- Look (part 05): one 16px-radius `crust` panel with a 1px inset `surface0` ring in the
  bar's typeface (`fonts.bar`, 13px/500) and motion tokens. A `data-tauri-drag-region`
  header (the Close button is a sibling, never inside it) sits above a scrolling body
  (tinted hero badge with `data-tone`: overlay/mauve/peach/green; numbered steps;
  identity card with Name, Bundle ID and Path; troubleshooting disclosure; red-tinted
  alert) and a fixed action footer. Escape and the Close button call the window's
  `close()`; auto-repeat is ignored. Animations and transitions are off under
  `prefers-reduced-motion: reduce`. Icons are Hugeicons.

## React client

`src/ui/src/sdk/` wraps the generated bindings; it never repeats a command or
event name. `client.ts` re-exports the generated DTOs, `readAppInfo`,
`readHostStatus` and `subscribeToStatus`. `queries.ts` owns the React Query
hooks, `statusFeed.ts` owns the status subscription, and `tokens.ts` holds the
Catppuccin Mocha values mirrored from the legacy design system plus the system
font stack.

The host status lives only in the React Query cache:

- `connectHostStatus` registers `events.statusChanged.listen` before it asks
  `commands.hostStatus()` for the first snapshot.
- A status event received while the snapshot is in flight increments a local
  generation and writes the cache; the older snapshot is discarded instead of
  replacing it. The generation check and cache write happen in one synchronous
  step, so they cannot interleave with an event.
- Disposing the feed unlistens. If `listen` resolves after StrictMode or unmount
  cleanup, the resolved unlisten runs immediately and no snapshot is fetched.
- Subscription and snapshot failures are recorded in the query cache error
  state; they are not swallowed. A successful event clears the error through
  `setQueryData`.
- `ModuleStatusView.revision` is rendered as text. It is never parsed with
  `Number()` or any other numeric conversion.

`Shell` accepts only `shell` (the default when `surface` is absent),
`test`, `access` and `bar`. An unknown or empty `surface` value renders a local fallback
and calls no native command, so only `index.html?surface=test` and
`?surface=access` read host data, and `?surface=bar` reads only `bar_snapshot`, `bar_weather` and `bar_media_artwork` and, on a click, calls `bar_action` or `bar_session_action`.
`TestWindow` shows loading, unavailable, host-ready and empty module/config
states; module runtime detail and rejected-configuration diagnostics use
separate labels, as do file diagnostics and warnings. It is a read-only status
surface and adds no configuration persistence or action endpoint.

Suites sit beside their sources: `client.test.ts`, `statusFeed.test.ts`,
`TestWindow.test.tsx`, `AccessibilityGuide.test.tsx`, `Placeholder.test.tsx` and `Shell.test.tsx`. Fixtures and the
QueryClient render helper live under `src/ui/tests/`. The generated wrappers are
exercised with module mocks at the `@tauri-apps/api/core` and
`@tauri-apps/api/event` boundary; the router and UI suites run against the real
WebKit browser.

## Bar window

`windows/Bar/` is the legacy Bar UI ported value-for-value (see the
[Bar feature](../features/bar.md)); nothing in it is redesigned.

- **Primitives** live in `shell/components/` (Surface, Button, Stack, Icon,
  ScrollingLabel, QueryErrorFallback) with the legacy styles. Surface animates in with
  the `motion` library at the actual `motionRaw.durationSlow` (0.275 s, exported from
  `sdk/tokens.ts`; the CSS `durationSlow` stays `400ms`). Style declaration order is
  kept: the Button stylesheet is evaluated before Surface's (the `QueryErrorFallback`
  import in `Bar.tsx` comes first), so `Surface as={Button}` keeps its crust rest
  background and the Button hover wins; do not reorder those imports.
- **Root rules.** `Bar.root.css` holds the legacy `main.css` rules scoped to
  `html[data-surface='bar']` with `:where()` so every selector keeps its legacy
  specificity. `Bar` sets and removes the attribute (layout effect); other surfaces
  never carry it. The font stack is `'Maple Mono NF', 'SF Mono', monospace`, system
  installed, never bundled.
- **Menu state** comes from the host bridge. `useBarSnapshot` (`sdk/queries.ts`) keeps
  the canonical `BarSnapshotOutcome` in React Query; `connectBarSnapshot`
  (`sdk/barFeed.ts`) subscribes to the generated `events.barChanged` **before** it calls
  `commands.barSnapshot()`, applies a snapshot or event only if its `sequence` is a
  numerically greater `BigInt` than the held one (a stale initial snapshot after a newer
  event is discarded), refetches on `resnapshot: true`, keeps `unauthorized` only while
  nothing with a sequence is held, records subscription/snapshot failures in the query
  error state and unlistens a late subscription at once under StrictMode. The Bar slides
  away (`translateY(100%)`, opacity 0, 150 ms ease) only for an `available` view with
  `menu.state === "visible"`; every other outcome keeps it shown, like legacy's initial
  `false`. Zustand is not used: the Bar has no UI-local state beyond component state.
- **Data seam.** Provider-backed items read `useBarData()` (`Bar.data.ts`, a React
  context that is `null` when nothing was supplied). In production `Bar` renders
  `barDataFromOutcome(snapshot)`: the CPU, battery and Wi-Fi readings of the generated
  `BarView` (part 05 slice a) mapped value-for-value onto the seam shape, each absent
  until its provider has a reading (so each item renders the legacy unresolved branch)
  and all absent for an `unavailable`/`unauthorized` answer; nothing is fabricated.
  Weather is the exception that is not in the snapshot: `BridgeBarData` (`Bar.tsx`)
  adds `useBarWeather(configRevision)` (`Weather/Weather.query.ts`, a React Query hook
  keyed `['bar','weather',revision]`) to the bridge data, only when no explicit fixture
  data is supplied. It calls `bar_weather`, then fetches Visual Crossing or Open Meteo
  from the webview (the secret key and coordinates stay in the `queryFn` closure, never
  in keys, storage, zustand or logs), refreshes every `weather.refresh_interval` minutes (default 20) and on reconnect
  and aborts on unmount. See [Bar](../features/bar.md#weather-ui).
  Media comes from `BarView.media` (artwork fetched by id with `bar_media_artwork` through
  `useMediaArtwork`, retained like legacy; click is `bar_action('media')`, see
  [Bar](../features/bar.md#media-ui)); Keep Awake comes from `BarView.keepAwake` (part 08;
  click is `bar_session_action`, see [Bar](../features/bar.md#keep-awake-indicator)). Tests
  and the parity gate supply fixtures from `src/ui/tests/barFixtures.ts` through
  `BarDataProvider`, which always wins over the bridge. Parts 05-08 replace a field's
  source and never the styles. The Clock is UI-local and live. No Bar item calls a
  command that does not exist; Media's click is wired (slice C).
- **Spaces.** The ported legacy Tiling strip reads `BarData.spaces` (mapped from
  `BarView.tiling` in `barDataFromOutcome`, no own subscription or timer). Unresolved,
  absent or zero workspaces keep the empty first-slot `<div />` so Media and Status stay in
  place; `loading` shows the legacy `Loading...` fallback (`SpacesFallback`); clicks go
  through `tilingClick` with the rendered generation and sequence
  (see [Bar](../features/bar.md#tiling-strip-ui)).
- **Error boundary.** `Bar` keeps legacy's `ErrorBoundary` with `QueryErrorFallback`
  and `Suspense fallback={null}`; Retry only resets the boundary.
- The packaged CSP is `default-src 'self'` with `connect-src ipc: http://ipc.localhost`
  plus exactly the three weather hosts (`weather.visualcrossing.com`,
  `api.open-meteo.com`, `geocoding-api.open-meteo.com`); the dev CSP adds the Vite
  origins. Both add `img-src 'self' data:` for the media artwork `data:` URLs
  (see [Bar CSP](../features/bar.md#content-security-policy)).
