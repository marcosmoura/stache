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
  display, application-observation/control and per-display desktop-image adapters, plus the owned
  Keep Awake assertion, lock-probe and observation adapters; constructing them acquires no
  assertion. The shipped `hold_hud` adapter is a click-through, nonactivating `NSPanel` HUD with
  Core Animation progress, pure placement/layout and fake-tested ownership; it is main-thread
  owned, hidden until a show request, and has no event tap, tray, IPC or application observer of
  its own; the Hold-to-Quit runtime owns when it is installed, shown and released. The application
  adapter additionally exposes workspace-activation observation with a nonblocking published
  frontmost snapshot (and an additive listener, `watch_frontmost_observed`, told about every
  activation and publication), a main exact-instance frontmost preflight, a separately named
  graceful `terminate` request (the force-terminating `terminate` that noTunes uses is unchanged)
  under a typed per-press authorization, a separately named exact-instance activation
  (`activate_exact`: `NSRunningApplication activateWithOptions:` on the retained instance, under a
  typed cancellable authorization with an optional deadline; never by path, frontmost or retry),
  and a best-effort display hint from public window geometry; the applications adapter itself
  never uses Accessibility or Screen Recording. Hold-to-Quit and the host frontmost publisher
  consume it. The `menu` adapter is a dedicated AX owner thread that reads a target's menu bar
  into a pure `Send` snapshot (0.5 s per-element timeout, 1.5 s deadline, 4096 nodes, depth 16,
  all-or-nothing) and keeps actionable leaves retained per presentation. On top of it sit a
  main-thread `NSMenu` presenter (main-local native state keyed by presentation, tokens only), pure
  Quartz-to-AppKit geometry anchored on the primary display, a run-loop dispatcher
  (`CFRunLoopPerformBlock` in the common modes) so `cancelTracking` runs during menu tracking,
  exact-instance invocation (activation, then `AXPress` under a one-shot gate with leased
  elements) and a `MenuControl` with acknowledged cancellation and cleanup that reports
  `cleanupPending` honestly. It has no module runtime, input subscription or enabled behavior yet,
  and its native behavior is verified only by the opt-in `menu-native-check` harness. It is the only crate containing project-authored unsafe code; public
  functions are safe.
- `src/crates/modules/proxy-audio`: the proxyAudio pilot — authoritative configuration, priority
  policy, `CoreAudio` watcher, `audio list` command and read-only `proxyAudio.list` handler. See
  [proxy-audio.md](../features/proxy-audio.md).
- `src/crates/modules/notunes`: the noTunes blocker — authoritative `notunes.jsonc`
  configuration, an optional user-chosen replacement target (path or bundle
  identifier) and the launch policy that terminates Apple Music/iTunes through the
  safe application boundary. See [notunes.md](../features/notunes.md).
- `src/crates/modules/hold-to-quit`: the Hold-to-Quit module — authoritative `commandQuit.jsonc`
  (`enabled`, `holdDuration`, `dismissDelay`, `position`, `animation`), schema, template and typed migration, plus `HoldToQuitRuntime`: an
  allocation-free input callback over the shared service, one worker, the HUD owner (`HudHost`)
  and an injected host frontmost lease. The app's `hold_to_quit.rs` is its factory and teardown
  owners. See [hold-to-quit.md](../features/hold-to-quit.md).
- `src/crates/modules/menu-anywhere`: the MenuAnywhere module — authoritative `menuAnywhere.jsonc`
  (`enabled`, `modifiers`, `mouseButton`), schema, template and migration, plus `MenuAnywhereRuntime`: a
  pure-decision input callback over the shared service (one published tuple, one busy reservation, a
  one-slot queue), a trigger worker that captures the exact frontmost instance and drives the
  `stache-macos` `menu` adapter, and a config worker that publishes the accepted trigger after the
  registry's accepted revision. Native objects stay behind the `MenuHost` seam. The app's
  `MenuAnywhereFactory` (in `host.rs`, appended to `ModuleFactories::production`) registers it.
  See [menu-anywhere.md](../features/menu-anywhere.md).
- `src/crates/modules/keybindings`: the Keybindings module — `keybindings.jsonc`
  configuration with legacy-map migration, shortcut planning, the bounded command
  dispatcher, Caps Lock ownership, and the `KeybindingsRuntime` lifecycle with its
  published runtime status. The app composes it in `app/src/keybindings.rs` and
  registers it through `KeybindingsFactory`. See [keybindings.md](../features/keybindings.md).
- `src/crates/modules/keep-awake`: the Keep Awake module — authoritative
  `keepAwake.jsonc` configuration, lock-aware one-lease power policy and the
  module's published runtime status. It owns no socket handler: the app composes
  the read-only `keepAwake.status` envelope from the module snapshot reader. See
  [keep-awake.md](../features/keep-awake.md).
- `src/crates/modules/wallpapers`: authoritative `wallpapers.jsonc`
  configuration, schema, template, migration, shared pool/explicit-file source
  resolution, the processing/cache/apply library, a host-lifetime coordinator
  and one wakeable rotation timer. The app's `WallpapersFactory` mounts it and
  contributes the typed `wallpapers.set` handler; the CLI command remains
  planned. See [wallpapers.md](../features/wallpapers.md).
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
executor's lock-observation installer and the composition root's one `AssertionOwner`; the
composition root then adds `WallpapersFactory`, because only it holds the identity paths,
configuration directory and home. That owner is also registered as the teardown
`power-assertion` acknowledgement, so the module and the slot share one ledger. The pilot's factory also contributes the read-only `proxyAudio.list`
handler to the same socket router, so the CLI and socket enumerate through the same listing
service; the blocker contributes no command or socket handler, and Keep Awake contributes
`keepAwake.status`. That handler receives a clone of the app's `FactoryContext`, whose private
registry and store handles expose only the published `module_snapshot(id)` and held
`diagnostic(id)`. It copies the runtime cell from `KeepAwakeSnapshot::read()` and composes the
app-owned envelope without mutable registry/store or disk access. `WallpapersFactory`
contributes `wallpapers.set` and installs the module's typed accepted watch through
`host::register_module_observed` before registry start. Registration creates no
listener or assertion and installs no observer; each module starts only from an accepted
`enabled: true` configuration. Every factory keeps the default acknowledged no-op
shutdown hooks except Wallpapers, which overrides `begin_shutdown` to close its
host service admission and wake waits without joining, and `finish_shutdown` to
drain after module stop; both hooks are generic, and the host retains the factory
handles independently of the runner. See
[proxy-audio.md](../features/proxy-audio.md), [notunes.md](../features/notunes.md),
[keep-awake.md](../features/keep-awake.md) and
[wallpapers.md](../features/wallpapers.md).

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
ordered stages itself rather than the host worker: close factory admission and wake
factory-owned host-service waits, invalidate every module and native handle
without waiting on the lifecycle or a slot lock, reap the config watcher, permission ticker,
startup job and socket acceptance while observing the host worker inside the same stage budget,
stop the registry in reverse start order one module at a time, then drain factory-owned
services in reverse registration order, run native cleanup in the approved
order, store the outcome with its bounded record and one terminal status, close the signal
bridge, open the exit gate and run the terminal action. Every wait is capped at
`min(2 s, remaining)` and wakes on escalation within 10 ms, so a hung owner is named and
abandoned at its bound while the next stage still runs.

Each owner has a stable record name: `host-worker`, `watcher`, `permission-ticker`,
`startup-job`, `module:<id>`, `factory:<id>`, `power-assertion`, `input-tap`, `socket`, `activation`, `tray` and `windows`.
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

### Shared input owner

The composition root creates exactly one `InputService` over `stache_macos::MacInputBackend`
and attaches it to the registry through `HostDriver::with_input_service`; host fixtures keep
the inert default. The Keybindings dispatcher's reservations are the service's work budget,
and the host reserves generic priorities (`input_priority`: Hold-to-Quit > MenuAnywhere >
Keybindings), all three of which subscribe. The backend owns one lazily started thread,
`stache-input-owner`, with its own run loop: the event-tap ports, run-loop sources and the
run loop itself live in a `!Send` struct created on that thread and never leave it. Only the
256-request control queue, a signal-only wake handle and the `Arc<InputService>` cross
threads. The owner never dispatches to the main thread. The Keybindings module subscribes only
while it is enabled with Caps Lock bindings, and Hold-to-Quit only while it is enabled and running with a healthy HUD, so no thread starts and no tap exists otherwise;
real native behavior remains unverified. Install creates a disabled HID tap
(suppressing: head-insert; passive: listen-only), a missing Input Monitoring or
Accessibility grant is acknowledged as an error and never prompts, and each request is
checked for cancellation just before its native mutation. The `input-tap` teardown slot
(`InputTapOwner`, off main) asks the owner to stop: it removes any tap, destroys the native
objects on the owner thread and only then acknowledges, so success proves the release.

**Permission contract for shared-input consumers.** Every module that subscribes to the shared
input service (Keybindings, Hold-to-Quit, MenuAnywhere) declares its Accessibility need through
`PermissionRequirement::shared_input` from `Module::required_permissions`, computed from its accepted
configuration (nothing when the configuration needs no tap). The requirement is user-prompt-only: the
host checks and polls the trust status on startup, enable, reload, tick and activation but calls
`PermissionProvider::request` only for the explicit tray request. A configuration that newly needs
trust is accepted and waits; it never prompts. A trusted but failed tap is a module failure, not a
missing permission. Later consumers must not opt into automatic prompting.

### Caps Lock remap ownership and startup recovery

`stache_macos::caps_remap` is the safe, library-only owner of the global Caps Lock to F18
`/usr/bin/hidutil` mapping used by the Keybindings Caps adapter. The Keybindings runtime applies
the remap only after a confirmed tap, and its first apply waits (bounded) for the host's startup
recovery of a crash-left remap; real native behavior remains unverified. An apply runs under an identity-wide durable lease
(`Application Support/<identity>/keybindings/caps-remap.lock`, five-second wait): strictly parse
the whole current `UserKeyMapping` (unknown, truncated, quoted, null, hexadecimal or duplicate-Caps
output is refused, and any Caps mapping without Next's journal is foreign, even Caps to F18), write
the versioned journal atomically (`caps-remap.journal`, phases `apply_pending`, `applied`,
`restore_pending`, `ownership_lost`), run `hidutil` through the bounded shared executor (5 s child
lifetime, 1 MiB stdout, 64 KiB stderr, 2 s cleanup then the bounded reaper), reread and verify.
A restore re-reads, edits only the owned entry of the latest mapping and deletes the journal only
after verified absence with no unconfirmed earlier child. Outcomes are `Restored`, `NoJournal`,
`OwnershipLost` (a foreign Caps mapping is never overwritten and the record stays),
`CleanupPending` (known pending, nothing provably changed), `Uncertain` (a native write may have
happened) or `Blocked` (corrupt journal or unsafe path). `hidutil` has no compare-and-swap; the
lease serializes only cooperating Stache Next processes and the journal proves only intent.

The host runs one startup step per desktop start, `HostDriver::with_startup_recovery` with
`KeybindingsCapsRecovery` (module crate `stache_keybindings::caps::recover_stale_caps_remap`),
before the factories register and regardless of module enabled state, configuration validity or
the tray. It runs on its own cancellable `stache-caps-recovery` thread held in a monitor cell,
so a slow step never blocks readiness and teardown cancels and reaps it. It acts only when Next's
own journal exists (with none it reads one directory entry: no lease, no `hidutil`, no created
path), is never retried, and surfaces a returned diagnostic once as a persistent host condition.
Recovering a crash needs a relaunch; until then physical Caps Lock may still send F18.
`stache_keybindings::caps` also holds the pure chord state, the suppressing handler (invalidatable,
so it can never enqueue a command again) and `CapsOwner`, which orders tap-before-remap and
invalidate-before-restore; the Keybindings runtime constructs it (see below).

### Keybindings runtime composition

`app/src/keybindings.rs::compose` builds, once per launch and inert, the bounded `Dispatcher`
(16 active, 64 waiting), the shared `InputService` whose work reservations **are** that dispatcher
(one admission decision for the tap and the dispatcher), the `Router` that serves as both the Carbon
and Caps sink, the `HotkeyControl`, the `CapsOwner` and the startup-recovery step over the **same**
`Arc<CapsRemap>`. `KeybindingsFactory` (in `host.rs`) registers the module and installs its typed
accepted-config watch before `start_all`; it contributes no socket method. Its `begin_shutdown`
closes press admission; `finish_shutdown` runs after the registry stopped the module and closes the
dispatcher within its bound; the shared input owner is released after both.

The runtime details travel on the host status: `ModuleStatusView.keybindings` is the module's
separately published `KeybindingsRuntimeStatus`, overlaid by the host at publication time (so it is a
point-in-time observation, not a live stream). The module `diagnostic` stays the rejected-file
channel and is overlaid from the held ConfigStore diagnostic without touching runtime details.

### Carbon hotkey and layout store

`stache_macos::hotkeys` is the main-owned Carbon/TIS adapter for ordinary and `§` shortcuts.
The composition root (`app/src/keybindings.rs`) builds one `HotkeyControl` over the real native
layer, inert: no handler is installed and no hotkey is registered until the enabled Keybindings
module commits a candidate. The runtime prepares candidates inertly and commits them only after it
observes the published revision (see below).
Every `EventHandlerRef`, `EventHotKeyRef`, the layout observer and the route table live in a
main-thread-local store per control. The native layer is a `!Send` `Box<dyn Native>` and the
route table an `Rc<RefCell<_>>` shared only with the Carbon callback, so none of it can leave
the main thread by type; there is no `unsafe impl Send` or `Sync`. Off-main callers hold a
`Send + Sync` `HotkeyControl` and plain values (`BindingToken`, `StagedId`, reports) scoped by
control, generation and revision. Native hotkey IDs carry the Stache signature `stnx` and come
from a process-monotonic counter that is never reused, so a queued event for a retired
registration is forwarded, never read as a successor's.

The first commit that registers a chord installs the single handler lazily; a failed install is
its own outcome and each chord registers and reports individually (an occupied chord is
`Occupied`, never "all active"). `prepare` stages inert candidates; `commit` transfers
unchanged registrations without re-registering them, registers new chords, publishes central
Carbon reservations and flips routes in one non-native swap, and only then unregisters removed
chords. Stop and drop inhibit dispatch with an atomic before any main-thread hop; native cleanup
then runs under the shared 2 s acknowledgement and 10 ms cancellation quantum, rechecks
cancellation before mutating and keeps failures as inert guards reported as pending cleanup. The
Carbon callback forwards foreign, malformed and unknown events through `CallNextEventHandler`,
ignores releases, skips removed or yielded (`dispatchable == false`) routes and otherwise calls a
non-blocking `HotkeySink`; it does no native query, wait or process spawn.

`stache_macos::keyboard_layout` resolves `§` with a pure resolver over a layout table (TIS and
`UCKeyTranslate` copy the layout bytes on main). An input-source observer only calls a coalescing
refresh hint; the refresh re-resolves through the staging path and rechecks generation, revision
and layout identity before any native mutation or publication. A hint for an unchanged layout
does nothing. Default tests use a fake Carbon/TIS layer; the real layer is compile-checked only.

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
`execOnStartup` runs once, through the shared SDK command runner, on its own owned thread after first setup, never for CLI, reload or a
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

`stache-sdk`, `stache-cli`, `stache-hold-to-quit`, `stache-keep-awake`, `stache-keybindings`, `stache-menu-anywhere`,
`stache-proxy-audio`, `stache-notunes`, `stache-wallpapers` and `stache-app` forbid unsafe code;
`stache-macos` documents every unsafe block.
