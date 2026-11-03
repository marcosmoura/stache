# Skeleton architecture

Read when changing the shipped foundation boundaries.

- `src/crates/app`: the only desktop binary (`stache`), Tauri composition root, native tray,
  configuration-backed enable/pause actions, permission policy, shutdown coordinator, test-window
  backend, configuration catalog in `catalog.rs`, contributed CLI commands in `commands.rs`, and the
  developer-only `generate-schema`/`generate-bindings` binaries. The same `stache` binary selects
  desktop or CLI before any Tauri startup.
- `src/crates/sdk`: owned module SDK — registry, lifecycle, threading, typed events/errors,
  injected logging, permissions and window capabilities, file-backed configuration (catalog, store,
  watcher, comment-preserving edits, schemas), identity paths/instance lock and the socket
  protocol/router/client/server; no Tauri dependency. See [sdk.md](sdk.md), [config.md](config.md)
  and [ipc.md](ipc.md).
- `src/crates/macos`: safe Accessibility trust, application-activation, main-thread, `CoreAudio`,
  display and application-observation/control adapters. It is the only crate containing
  project-authored unsafe code; public functions are safe.
- `src/crates/modules/proxy-audio`: the proxyAudio pilot — authoritative configuration, priority
  policy, `CoreAudio` watcher, `audio list` command and read-only `proxyAudio.list` handler. See
  [proxy-audio.md](../features/proxy-audio.md).
- `src/crates/modules/notunes`: the noTunes blocker — authoritative `notunes.jsonc`
  configuration, an optional user-chosen replacement target (path or bundle
  identifier) and the launch policy that terminates Apple Music/iTunes through the
  safe application boundary. See [notunes.md](../features/notunes.md).
- `src/crates/cli`: built-in `config`/`cache`/`reload`/`schema`/`completions` implementation and
  command composition; it imports the SDK, never the app or a module crate.
- `src/ui/src/shell/components/Shell`: development/test-only React surface with colocated test,
  Linaria `.styles.tsx` file and index export. React Compiler/Vite process the frontend.
- `src/ui/src/sdk`: generated command/event bindings plus the React Query status feed, client wrappers and tokens used by the test window.
- `src/ui/main.tsx` bootstraps the app exported by `src/ui/index.tsx`; `main.css` owns global CSS.
- `src/ui/tests` contains setup/support only; real tests are colocated under `src/ui/src`.
- `_legacy`: independent Cargo/pnpm application, excluded from root workspace and quality tools.

## Desktop host

The desktop path claims the Next identity instance lock, then creates the tray on the AppKit main
thread before any slow service. A background host thread owns one registry, configuration store and
watcher, permission policy, socket server and module control handles; tray/Tauri callbacks enqueue
commands and never wait for module code, a worker join or the socket. Status is published to the tray
and as the `status-changed` event; `host_status` reads that snapshot without taking module locks.
After creating the tray the host writes one info record, `startup: tray ready after N ms,
webviews=N`, which the release measurement reads; see
[performance.md](../guides/performance.md).

`ModuleFactories::production(main_thread)` registers the proxyAudio pilot over one `CoreAudio`
backend and the noTunes blocker over one `stache_macos::ApplicationControl` built from the same
host main-thread executor. The pilot's factory also contributes the read-only `proxyAudio.list`
handler to the same socket router, so the CLI and socket enumerate through the same listing
service; the blocker contributes no command or socket handler. Registration creates no listener
and installs no observer; each module starts only from an accepted `enabled: true` configuration.
See [proxy-audio.md](../features/proxy-audio.md) and [notunes.md](../features/notunes.md).

Tray **Enable** is a config-backed edit: plan, validate, prepare and sync the replacement, recheck
for an external change immediately before the atomic rename, then apply once. A missing target is
committed with a no-clobber rename and a symlink is refused. **Pause/Resume** are session-only. An
invalid file is never overwritten and its diagnostic is displayed separately from runtime state.
Failed and unavailable modules expose a **Retry** action; waiting modules expose the explicit
permission request. Permissions poll every second while a module waits and back off otherwise; a
schedule change wakes the pending wait, and at most one periodic tick is queued. Only enabled
declared requirements may prompt, at most once per session unless the user explicitly retries.
macOS application activation is observed through `NSApplicationDidBecomeActiveNotification` in
`stache-macos`, not Tauri's loop `Resumed` event. `MainThreadMarker` enforces main-thread
registration; the non-`Send` guard lives as a local around the Tauri event loop and removes its
observer on drop. The pilot declares none, so normal operation never prompts for Accessibility.

Launch creates no webview and no Dock icon. The explicit **Open Test Window** action opens the app
URL with `?surface=test` as `stache-test-<n>`, reuses it for the same owner/key and closes it on
request; stop releases module windows. `src/crates/app/tauri.conf.json` has no fixed windows and
lists only the least-privilege `test-window` capability. The app crate defines the
`custom-protocol` feature that `bun run tauri:build` enables; without it, Tauri stays in dev mode and a
packaged window has no embedded document. Generate/schema binaries require `dev-tools` and are not
packaged. See [ui.md](ui.md).

Quit, SIGINT/SIGTERM/SIGHUP and Tauri's exit event converge on one idempotent off-main teardown:
stop accepting IPC/config work, cancel monitors, cancel and reap the owned startup job, stop the
registry in reverse order, release windows, tray and socket, then let Tauri exit. The signal thread
is closed and joined by that teardown, on the host worker rather than the main thread. Only an
explicit quit request starts that teardown: Tauri's last-window `ExitRequested` (`code: None`, from
`tauri-runtime-wry`'s `Destroyed` handler) just drops the window registry entry, while the
`Some(code)` form from `app.exit` prevents that exit and requests the shutdown.
`execOnStartup` runs once on its own owned thread after first setup, never for CLI, reload or a
second launch; a long-lived command cannot delay readiness or shutdown. See [ipc.md](ipc.md) and
[config.md](config.md).

## Identity

New bundle: **Stache Next**, `com.marcosmoura.stache.next`, macOS 26 minimum. Old bundle remains
**Stache**, `com.marcosmoura.stache`. Both executables are called `stache`; build/dev never use
`killall`. Root build output and legacy build output live in separate `target/` directories.
The CLI resolves cache and runtime directories for the new identity only; the desktop host uses
them for the single-instance lock, socket server and published status. The runtime lock is
`runtime/instance.lock`, a private `0600` regular file held for the process lifetime. See
[ipc.md](ipc.md#identity-paths).

## Planned work

Part 05 ships its native integration and the typed React client/router: static status surfaces,
capabilities, tray, permission policy, shutdown, test-window backend, generated bindings and the
browser suites. [ui.md](ui.md) records the contract. Remaining manual coverage is a real tray click
and SIGINT delivery; the opt-in harness covers SIGTERM and the WebKit surface. The proxyAudio
runtime ships in [Foundation part 06](../specs/2026-11-02-foundation/06-proxy-audio.md); only its
comparative measurements and manual hardware checks remain open.

`stache-sdk`, `stache-cli`, `stache-proxy-audio`, `stache-notunes` and `stache-app` forbid unsafe
code; `stache-macos` documents every unsafe block.
