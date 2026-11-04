# UI SDK and headless bindings

Read when changing the typed command/event surface, the generated TypeScript
bindings, the on-demand test window or their drift checks.

The native host, tray, test-window route and headless binding export ship. The
React client consumes that contract through typed SDK wrappers, a validated
`WindowRouter` and the on-demand `TestWindow`; the browser suites run in real
WebKit.

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

- `HostStatus { ready, error, modules, config }`.
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
  host-owned requests. Only the shipped `test` surface is implemented; surface
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

`WindowRouter` accepts only `shell` (the default when `surface` is absent) and
`test`. An unknown or empty `surface` value renders a local fallback and calls
no native command, so only `index.html?surface=test` reads host data.
`TestWindow` shows loading, unavailable, host-ready and empty module/config
states; module runtime detail and rejected-configuration diagnostics use
separate labels, as do file diagnostics and warnings. It is a read-only status
surface and adds no configuration persistence or action endpoint.

Suites sit beside their sources: `client.test.ts`, `statusFeed.test.ts`,
`TestWindow.test.tsx` and `WindowRouter.test.tsx`. Fixtures and the
QueryClient render helper live under `src/ui/tests/`. The generated wrappers are
exercised with module mocks at the `@tauri-apps/api/core` and
`@tauri-apps/api/event` boundary; the router and UI suites run against the real
WebKit browser.
