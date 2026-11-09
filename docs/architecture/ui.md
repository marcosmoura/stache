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
    components/         reusable UI shared by windows (created with the first shared component)
    windows/            one directory per window surface
      AccessibilityGuide/
      Placeholder/      development fallback for the `shell` surface
      TestWindow/
```

Each directory holds the component, its `.styles.tsx`, a colocated test and an
`index.ts`. A new window (for example the Bar) gets its own `windows/<Name>/`
directory and a route in `Shell`; pieces used by more than one window move to
`components/`.

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
`BordersConfig`, `BorderStyle`, `BorderStateConfig`, `GradientConfig` and
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
- `WindowKind::Launcher` and `WindowKind::NonActivatingPanel` return
  `WindowError::UnsupportedKind` until a tool spec defines their native
  semantics. A module may only request a surface declared in its descriptor.

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
`test` and `access`. An unknown or empty `surface` value renders a local fallback
and calls no native command, so only `index.html?surface=test` and
`?surface=access` read host data.
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
