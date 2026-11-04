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
  display and application-observation/control adapters, plus the owned Keep Awake assertion,
  lock-probe and observation adapters; constructing them acquires no assertion. It is the only
  crate containing project-authored unsafe code; public functions are safe.
- `src/crates/modules/proxy-audio`: the proxyAudio pilot — authoritative configuration, priority
  policy, `CoreAudio` watcher, `audio list` command and read-only `proxyAudio.list` handler. See
  [proxy-audio.md](../features/proxy-audio.md).
- `src/crates/modules/notunes`: the noTunes blocker — authoritative `notunes.jsonc`
  configuration, an optional user-chosen replacement target (path or bundle
  identifier) and the launch policy that terminates Apple Music/iTunes through the
  safe application boundary. See [notunes.md](../features/notunes.md).
- `src/crates/modules/keep-awake`: the Keep Awake module — authoritative
  `keepAwake.jsonc` configuration, lock-aware one-lease power policy and the
  module's published runtime status. It owns no socket handler: the app composes
  the read-only `keepAwake.status` envelope from the module snapshot reader. See
  [keep-awake.md](../features/keep-awake.md).
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

`ModuleFactories::production(main_thread, power_owner)` registers the proxyAudio pilot over one
`CoreAudio` backend, the noTunes blocker over one `stache_macos::ApplicationControl` built from
the same host main-thread executor, and the Keep Awake module over the host main-thread
executor's lock-observation installer and the composition root's one `AssertionOwner`. That
owner is also registered as the teardown `power-assertion` acknowledgement, so the module and
the slot share one ledger. The pilot's factory also contributes the read-only `proxyAudio.list`
handler to the same socket router, so the CLI and socket enumerate through the same listing
service; the blocker contributes no command or socket handler, and Keep Awake contributes
`keepAwake.status`. That handler receives a clone of the app's `FactoryContext`, whose private
registry and store handles expose only the published `module_snapshot(id)` and held
`diagnostic(id)`. It copies the runtime cell from `KeepAwakeSnapshot::read()` and composes the
app-owned envelope without mutable registry/store or disk access. Registration creates no
listener or assertion and installs no observer; each module starts only from an accepted
`enabled: true` configuration. See
[proxy-audio.md](../features/proxy-audio.md), [notunes.md](../features/notunes.md) and
[keep-awake.md](../features/keep-awake.md).

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
registration; the non-`Send` guard is held in a main-thread cell that only a main-thread closure
takes and drops, removing its observer. The pilot declares none, so normal operation never prompts
for Accessibility.

Launch creates no webview and no Dock icon. The explicit **Open Test Window** action opens the app
URL with `?surface=test` as `stache-test-<n>`, reuses it for the same owner/key and closes it on
request; stop releases module windows, and each window counts as released only after its
`Destroyed` event or a native "already gone" answer. `src/crates/app/tauri.conf.json` has no fixed
windows and lists only the least-privilege `test-window` capability. The app crate defines the
`custom-protocol` feature that `bun run tauri:build` enables; without it, Tauri stays in dev mode and a
packaged window has no embedded document. Generate/schema binaries require `dev-tools` and are not
packaged. See [ui.md](ui.md).

Quit, SIGINT/SIGTERM/SIGHUP, Tauri's exit event and AppKit's terminate decision converge on one
idempotent off-main coordinator. A request only records a deadline: the first sets a five-second
total and the cause, and a later one shortens it to 250 ms from that request without ever passing
the total. A dedicated `stache-teardown` thread stores exactly one outcome (`Clean`, `Partial` or
`Uncertain`) in memory, makes one bounded record attempt through a non-blocking channel, using
50 ms reserved inside that total, and dispatches one terminal action. The coordinator runs the
ordered stages itself rather than the host worker: invalidate every module and native handle
without waiting on the lifecycle or a slot lock, reap the config watcher, permission ticker,
startup job and socket acceptance while observing the host worker inside the same stage budget,
stop the registry in reverse start order one module at a time, run native cleanup in the approved
order, store the outcome with its bounded record and one terminal status, close the signal
bridge, open the exit gate and run the terminal action. Every wait is capped at
`min(2 s, remaining)` and wakes on escalation within 10 ms, so a hung owner is named and
abandoned at its bound while the next stage still runs.

Each owner has a stable record name: `host-worker`, `watcher`, `permission-ticker`,
`startup-job`, `module:<id>`, `power-assertion`, `socket`, `activation`, `tray` and `windows`.
A registered native owner is released by its own completion answer; a dispatch is never an
acknowledgement, so `Clean` is reported only when every owner proved that it released its
resource. The activation observer, the tray item and the tracked windows are removed by their
own main-thread hops, and the tray hop no longer shares the final `app.exit`. Keep Awake
registers its one assertion owner as `power-assertion`: the slot acknowledges the release its
module `stop` already performed and reports success only when the shared ledger has no active
or acquiring generation, no cleanup pass in flight and no unresolved ID, so it adds no second
native cleanup or retry path. The `!Send` activation guard stays on main in a thread-local cell
that only a main-thread closure takes and drops. After one main-thread hop misses its budget,
every remaining main-thread owner is still released but named unacknowledged without another
wait, because the same stalled queue would spend the total; worker-thread owners keep their own
wait. Socket acknowledgement requires the accept thread and every handler permit to finish, and
a socket inode that cannot be removed is reported instead of dropped. An answer that arrives
after the outcome is inert: it may release what it owns, but it cannot publish, change the
outcome or write a second record.

With an AppKit reply pending the terminal action is `reply(true)` on main; otherwise Tauri exits
with the intended code, separately from the tray hop. A terminal action not accepted by the
deadline exits this process at that deadline; a claimed AppKit reply that never completes gets
one further two-second hop, capped at seven seconds from the first request. A coordinated restart
relaunches only after a `Clean` outcome, after releasing the instance lock. The signal thread is
closed and joined by that terminal action, never on the main thread. Only an explicit quit
request starts teardown: Tauri's last-window `ExitRequested` (`code: None`, from
`tauri-runtime-wry`'s `Destroyed` handler) just drops the window registry entry, while the
`Some(code)` form from `app.exit` prevents that exit and requests the shutdown. Real Cmd-Q
reaches the coordinator through the default menu's Quit item, with the Test Window frontmost, but
a repeat Cmd-Q does not escalate while the reply is pending; a signal does. Logout, restart and
shutdown have not been verified end to end.
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

`stache-sdk`, `stache-cli`, `stache-keep-awake`, `stache-proxy-audio`, `stache-notunes` and
`stache-app` forbid unsafe code; `stache-macos` documents every unsafe block.
