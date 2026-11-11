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
- `src/crates/macos`: safe Accessibility trust, System Settings opener, application-activation, main-thread, `CoreAudio`,
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
  and its native behavior is verified only by the opt-in `menu-native-check` harness. The shared AX
  FFI (`AxApi`, `AxElement`, `AxError`, `SystemAx`, the 0.5 s `ELEMENT_TIMEOUT` and 1 s
  `PRESS_TIMEOUT`) lives in the neutral `ax` module; `menu` re-exports the same names so
  MenuAnywhere is unchanged. Tiling slice 03a adds (consumed by the shipped Tiling runtime; native behavior
  unverified), a `WindowAx` capability trait (separate `AXPosition`/`AXSize` read and write through
  `AXValue`, settable-ness, minimized/fullscreen/hidden flags, title/role/subrole, windows, focused
  and main window, explicit unknown/unsupported readings instead of zero frames), an `AxObserver`
  wrapper (create, add/remove notification, run-loop source; its callback records only a pure
  notification enum and an opaque owner-local element key into a bounded mailbox), a
  `_AXUIElementGetWindow` resolver by `dlsym` only (missing symbol is an explicit `Unsupported`
  capability, never linked), and compile-proofed `AXTabGroup`/`AXTabs` extraction reads (members read with their ids and
  selected flags by `read_tab_group`) that do not claim tab support. All AX objects stay `!Send` on one owner thread. Slice 03b adds (consumed by the shipped Tiling runtime; **native behavior unverified**), the `windows` ownership adapter (see below). The applications adapter additionally gains
  exact-instance `hide_exact`/`unhide_exact` (the `activate_exact` issuance, authorization and
  outcome model; `NSRunningApplication hide`/`unhide`) and `watch_lifecycle` (launch, terminate,
  hide, unhide as pure `LifecycleEvent`s with the exact `ApplicationId`, deferred to a later main
  turn; a terminate that cannot be resolved to an id after pruning is delivered as the synthesized
  `TerminatedUnresolved`, never dropped). Part 06 slice B adds the `AXHidden` fallback beside them,
  on the tracked application's Tiling AX shard (`stache-tiling-ax-<n>`, never main and never
  `MenuAnywhere`'s `stache-ax-owner`): `WindowAx::set_hidden` plus the `windows` command `application_hidden`
  (`HiddenRequest`/`HiddenOutcome`) read or write `AXHidden` on one tracked exact `ApplicationId`
  only. It is used solely when the public `hide_exact`/`unhide_exact` returned `false`; a write is
  authorized by its own one-shot `ApplicationIssuance` (not the frame `GenerationCell`, which stop
  seals before the restore that may need it) and answers with a typed outcome (issued with a
  read-back, `Unobserved` after a lost acknowledgement, never-sent reasons, stale). Native behavior
  stays unverified. A `displays` adapter reads per-display id, full frame with origin, visible frame, backing
  scale, optional refresh rate, localized name and primary/main flags, converts between the
  `AppKit` and AX coordinate spaces in pure code, and observes
  `NSApplicationDidChangeScreenParametersNotification` as a pure topology-changed hint. It is the
  only crate containing project-authored unsafe code; public functions are safe.
- `src/crates/modules/proxy-audio`: the proxyAudio pilot — authoritative configuration, priority
  policy, `CoreAudio` watcher, `audio list` command and read-only `proxyAudio.list` handler. See
  [proxy-audio.md](../features/proxy-audio.md).
- `src/crates/modules/notunes`: the noTunes blocker — authoritative `notunes.toml`
  configuration, an optional user-chosen replacement target (path or bundle
  identifier) and the launch policy that terminates Apple Music/iTunes through the
  safe application boundary. See [notunes.md](../features/notunes.md).
- `src/crates/modules/hold-to-quit`: the Hold-to-Quit module — authoritative `commandQuit.toml`
  (`enabled`, `hold_duration`, `dismiss_delay`, `position`, `animation`), schema, template and typed migration, plus `HoldToQuitRuntime`: an
  allocation-free input callback over the shared service, one worker, the HUD owner (`HudHost`)
  and an injected host frontmost lease. The app's `hold_to_quit.rs` is its factory and teardown
  owners. See [hold-to-quit.md](../features/hold-to-quit.md).
- `src/crates/modules/menu-anywhere`: the MenuAnywhere module — authoritative `menuAnywhere.toml`
  (`enabled`, `modifiers`, `mouse_button`), schema, template and migration, plus `MenuAnywhereRuntime`: a
  pure-decision input callback over the shared service (one published tuple, one busy reservation, a
  one-slot queue), a trigger worker that captures the exact frontmost instance and drives the
  `stache-macos` `menu` adapter, and a config worker that publishes the accepted trigger after the
  registry's accepted revision. Native objects stay behind the `MenuHost` seam. The app's
  `MenuAnywhereFactory` (in `host.rs`, appended to `ModuleFactories::production`) registers it.
  See [menu-anywhere.md](../features/menu-anywhere.md).
- `src/crates/modules/keybindings`: the Keybindings module — `keybindings.toml`
  configuration with legacy-map migration, shortcut planning, the bounded command
  dispatcher, Caps Lock ownership, and the `KeybindingsRuntime` lifecycle with its
  published runtime status. The app composes it in `app/src/keybindings.rs` and
  registers it through `KeybindingsFactory`. See [keybindings.md](../features/keybindings.md).
- `src/crates/modules/keep-awake`: the Keep Awake module — authoritative
  `keepAwake.toml` configuration, lock-aware one-lease power policy and the
  module's published runtime status. It owns no socket handler: the app composes
  the read-only `keepAwake.status` envelope from the module snapshot reader. See
  [keep-awake.md](../features/keep-awake.md).
- `src/crates/modules/wallpapers`: authoritative `wallpapers.toml`
  configuration, schema, template, migration, shared pool/explicit-file source
  resolution, the processing/cache/apply library, a host-lifetime coordinator
  and one wakeable rotation timer. The app's `WallpapersFactory` mounts it and
  contributes the typed `wallpapers.set` handler; the CLI command remains
  planned. See [wallpapers.md](../features/wallpapers.md).
- `src/crates/cli`: built-in `config`/`cache`/`reload`/`schema`/`completions` implementation and
  command composition; it imports the SDK, never the app or a module crate.
- `src/ui/src/shell`: the `Shell` window router (`Shell.tsx`), `windows/` (one directory per surface, including the
  development-only `Placeholder`) and `components/` (reusable UI, created with the first shared component); each directory has a colocated test,
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
`HostStatus.access` reports the Accessibility launch gate described below. After creating the tray the host writes one info record, `startup: tray ready after N ms,
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
`enabled = true` configuration. Every factory keeps the default acknowledged no-op
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
Failed and unavailable modules expose a **Retry** action; the gate rows expose the explicit
Accessibility request. Permissions poll every second while a module waits or the Accessibility gate
is not open and back off otherwise; a schedule change wakes the pending wait, and at most one
periodic tick is queued. Only enabled declared requirements may prompt, at most once per session
unless the user explicitly retries; the baseline Accessibility requirement never prompts on its own.
macOS application activation is observed through `NSApplicationDidBecomeActiveNotification` in
`stache-macos`, not Tauri's loop `Resumed` event. `MainThreadMarker` enforces main-thread
registration; the non-`Send` guard is held in a main-thread cell that only a main-thread closure
takes and drops, removing its observer.

**Accessibility launch gate.** Accessibility is a launch requirement of the whole app. The
composition root builds the registry with the baseline requirement
`PermissionRequirement::user_only(Permission::Accessibility)`, which the registry merges into every
module, so every registered module waits (`waitingForPermission`) without trust, including modules
that need none themselves (proxyAudio, noTunes, Keep Awake, Wallpapers, ordinary shortcuts). The
pure `AccessGate` (`access.rs`) is `Checking`, `Blocked { revoked }` or `Open`: `Granted` opens it,
`Denied` and `Unknown` block (fail closed). The first non-prompting check runs in `Runner::start`
after factory registration and before `registry.start_all()`; every later permission refresh
(tick, activation, explicit, enable, pause, resume, retry, config apply) feeds the gate the
Accessibility status that refresh already read (`RefreshReport::statuses`), so the gate makes no
extra platform call. The registry's own `on_permission_change` stops running modules in reverse
order on revocation and restarts them in dependency order on a grant; the gate adds no lifecycle
path. Each transition writes one info record: `access: blocked (accessibility denied|unknown)`,
`access: open` or `access: revoked`. While gated the tray, instance lock, configuration store and
watcher, permission ticker, activation observer, test window, Quit and Caps Lock startup recovery
keep working. The host never calls `PermissionProvider::request` on its own: only the explicit
tray **Request Accessibility…** (`HostHandle::request_permission`) does, and it requests nothing
while the gate is open. **Open Accessibility Settings…** (`HostHandle::open_accessibility_settings`)
calls the injected `SettingsOpener` only while the gate is not open; its production implementation
hops to the main thread and calls `stache_macos::settings::open_accessibility_settings`
(`x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility`, an undocumented
URL verified manually). A failure is the last action error until the next successful action.
`HostStatus.access` is `{ state: checking|blocked|open, revoked, appPath }`; `appPath` is the
enclosing `.app` of the running executable, or absent for an unbundled binary. `ready` keeps its
meaning: a gated host is `ready: true` with `access.state = blocked`.

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
missing permission. Later consumers must not opt into automatic prompting. Since Accessibility is
also the app-wide baseline (see the launch gate above), this declaration is redundant for
Accessibility but harmless and stays for future, non-baseline permissions. The host still never
calls the system prompt automatically; it does automatically block every module until trust exists.

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
`exec_on_startup` runs once, through the shared SDK command runner, on its own owned thread, never for CLI, reload or a
second launch. It starts when the Accessibility gate first opens in a launch (right after setup when
trust already exists, otherwise after the first grant) and never reruns on a later reopening; a long-lived command cannot delay readiness or shutdown. See [ipc.md](ipc.md) and
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

## Window ownership adapter (`stache_macos::windows`, Tiling slice 03b)

Consumed by the shipped Tiling runtime (part 05 onward); native behavior unverified. Compile- and
fake-proven only; construction starts nothing and sends no AX message.

| Thread                                                                                                  | Owns                                                                                                                                                                                                                                                                                                                                                                            |
| ------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Main                                                                                                    | `AppKit` applications and `NSScreen` only. Never runs an AX query, enumeration chunk or action.                                                                                                                                                                                                                                                                                 |
| Tiling AX shards (`stache-tiling-ax-<n>`, `stache-tiling-ax-overflow`; prefix supplied by the consumer) | One thread per tracked application (up to `MAX_APPLICATION_SHARDS` = 32; further applications share the overflow shard): the AX API (created in-thread from a `Send` factory), that application's `AxObserver`, the `CFRunLoop` that delivers its callbacks, its enumeration and every retained window element. The `WindowAdapter` router owns no thread and makes no AX call. |
| MenuAnywhere AX owner (`stache-ax-owner`)                                                               | Its own state. Never shared: the adapter rejects that name and any name already used by a live adapter.                                                                                                                                                                                                                                                                         |

- **Shards.** `WindowAdapter` is a router (no thread, no AX call) over application shards. A shard
  is the owner core on its own thread with its own driver, system, observer, elements and bounded
  queue. The first `track` of an application starts a dedicated shard (`<prefix>-<lowest free n>`,
  at most `MAX_APPLICATION_SHARDS` = 32); further applications run on one `<prefix>-overflow` shard
  that hosts several applications (`WindowAdapter::shard_stats` reports the count). `untrack` ends a
  dedicated shard. The router mints the runtime epoch and installs it on every shard with shared
  serial and event-sequence counters, so serials are unique per epoch across shards, and all shards
  share the one `GenerationCell` and `EventQueue`. Window and application commands route by
  application; an application without a shard answers the same typed `Stale`/`WrongEpoch` outcome as
  before. `stop` seals the gate, then stops every shard under one shared deadline; a shard that does
  not finish or quarantines keeps its thread handle in a router quarantine for `retry_cleanup`
  (nothing is detached) and a refused thread is reported as `OwnerUnavailable`. One slow application
  never delays another's actions or snapshots.
- **Commands and wakeup.** Callers send pure `Command`s through a bounded channel
  (`OWNER_QUEUE_BOUND`) and a coalescing wakeup that performs a block on the owner's run loop
  (`CFRunLoopPerformBlock` plus `CFRunLoopWakeUp`; a queued block cannot be lost if it races ahead of
  the park, and `core_foundation::CFRunLoop` is already `Send + Sync`, so no unsafe `Send` impl is
  needed). The idle owner parks with no timeout; there is no polling timer. While enumeration or
  queued commands remain it polls with a zero timeout (the re-post). A `Driver` trait abstracts the
  park so tests drive the owner deterministically.
- **Identity.** `WindowInstance = (adapter, runtime epoch, exact ApplicationId, serial)`; the serial is
  never reused. Before every action the owner revalidates adapter, epoch, application instance,
  retained element and current `AXWindows` membership. A destroy notification invalidates the token
  before queued commands are examined. `CGWindowID` is metadata: `resolve_window_id` returns only a
  live, unambiguous token, so a reused id never revives a destroyed one.
- **Observation.** Application-level notifications are registered before the first enumeration read.
  Events leave as pure values (token or application id, kind, epoch, sequence) through a bounded
  `EventQueue` that counts dropped events explicitly. Enumeration is chunked at 64 elements or 50 ms of
  issuing time, then re-posted; cancellation, deadline and sealing are checked between elements.
  A refresh reports every listed window again, with its original token and fresh facts. Only a
  listing that finished untruncated and without error retires owner-tracked windows absent from a
  fresh full list (each reported `Destroyed`; a failed registration removal stays owned for cleanup);
  an aborted, cancelled or capped listing retires nothing, and Tiling's scan repair acts only on an
  untruncated completion. The fresh read and each re-read or retirement of a missed window count
  against the same 64-element and 50 ms chunk and are re-posted; cancellation, deadline and sealing
  are checked between them, and an interrupted listing ends as `EnumerationAborted` (windows already
  reported `Destroyed` were proven absent; nothing else is retired). This is fake-proven, not natively verified. Observer events are processed
  from an ordered backlog: at most 64 `WindowCreated` adoptions within 50 ms per turn, the rest
  re-posted in order; callback-delivered keys are released only after the whole backlog and only when
  no undrained observer event still names them, a focus or main notification for a window created
  later is reported for its token right after that creation (the latest of each kind wins), the
  backlog and deferred entries of an untracked application are dropped with its observer, and an
  unresolvable creation becomes `ObserverOverflow` instead of being dropped. Tiling coalesces
  unresolved terminations that arrive during an in-flight application list into one follow-up list.
- **Gate.** `WindowIssuance` is checked against a `GenerationCell` (revision, live generation, exact
  token, plan sequence, deadline) at the send instant, with a one-shot commit after the messaging
  timeout and immediately before the native call. Invalidation and `cancel` are lock-free and return
  while a send is stalled. Outcomes are `NotIssued`, `ExpiredBeforeSend`, `Stale` or
  `Issued {Applied | Partial | Uncertain | Rejected}`; a timeout after the commit is `Uncertain`.
  Focus is not a second activation path: callers combine `ApplicationObservation::activate_exact`
  with `AXRaise`.
- **Hops.** One `ACKNOWLEDGEMENT_BUDGET` (2 s) per hop capped by the command's remaining deadline,
  cancellation observed every `CANCELLATION_QUANTUM` (10 ms), including the backend calls the
  Tiling effect worker makes (so shutdown does not wait out a hop); budgets are shared, never
  multiplied.
- **Cleanup.** Stop seals issuance, cancels enumeration and removes observer registrations with
  acknowledgement. A failed removal keeps the observer and elements inert in an owner-held
  quarantine; `retry_cleanup` retries each registration exactly once per call, and a quarantined
  adapter refuses a new install until cleanup completes. Old-epoch cleanup cannot touch a successor.
  `close` joins the thread with a bounded wait and refuses while resources are quarantined.
- **Tiling seam.** `stache-tiling` `backend.rs` exposes the safe `WindowBackend` trait,
  `SystemWindowBackend`, `LazySystemBackend` (the production composition: it creates the
  adapter on the first epoch install; the `stache-tiling-ax-<n>` shard threads start on the first
  tracked applications, so a disabled Tiling, and a running one before its first `track`, own no
  thread) and an in-crate `FakeWindowBackend`.
- **Tiling animation owners.** `stache-tiling` `animation/` splits the work by owner. The
  `stache-tiling-animation` coordinator thread (spawned only while `animations.enabled`) owns the
  running jobs, the per-display frame clocks and each job's counters and latency ring; it computes
  frames and never makes an AX call. The application's AX shard (or the shared overflow shard) makes
  every native write, keeps one pending frame per window and answers each frame. The actor owns the
  planner state and sets `animate`; before each transition it copies the instant-fallback set into
  the pure state, and it adds an application to the set when its Enhanced UI lease cannot be held.
  The coordinator shares only an `AnimationShared` with the actor and status: the metrics book
  (written at job end) and the fallback set, each behind a short lock never held across a call-out.
  Every write goes through the `LiveSink` and the existing `FrameControl` gate; there is no second
  write path and no SkyLight call.
- **Tiling read-only actor (slice 04).** `stache-tiling` adds a single-writer actor thread
  (`stache-tiling-actor`) and a bounded read-effect worker thread, both started only by the
  internal `TrackingRuntime` harness, which nothing constructs or registers. The actor owns the pure
  state, drains the backend `EventQueue` and a bounded mailbox, and publishes immutable
  `Arc<TilingSnapshot>` values; native reads run on the effect worker (calling only the backend's
  `track`, `refresh`, `untrack` and `snapshot`) and return as tagged completions. It never waits on the
  main thread, calls `act`, or mutates a window, and it adds no SDK, IPC or CLI surface.
- **Bar module (parts 03-08).** `stache-bar` holds the `BarModule`/`BarRuntime` (`module.rs`,
  `runtime.rs`), the pure primary-display frame math (`window.rs`), the typed `BarRuntimeStatus`
  (`status.rs`), the host seam (`topology.rs`), the menu-bar observation (`menubar.rs`), the
  providers (`providers/`, including media) and the weather resolver (`weather/`). `stache-app`
  `bar.rs` holds `BarFactory` (`project_status`, `begin_shutdown`/`finish_shutdown`) and the
  Bar-scoped topology feed (see [SDK](sdk.md#screen-reservations-and-display-identity)), with the
  data bridge (`bar_bridge.rs`), fixed actions (`bar_actions.rs`), the Keep Awake projection
  (`bar_keep_awake.rs`) and the Tiling strip projection and `bar_tiling_action` click
  (`bar_tiling.rs`, over Tiling's published `tilingState`; no crate or config import between
  Bar and Tiling) beside it. `lib.rs` chains `BarFactory::production_with_media` next to
  Tiling, **default disabled** and inert at construction, with the bridge attached; the `bar` UI
  route and the `bar-window` capability exist. `ModuleStatusView.bar` carries the runtime status.
  See [Bar](../features/bar.md).
- **Tiling module (parts 05-06).** `stache-tiling` `module.rs`/`status.rs` add the `Module`
  implementation (`TilingModule`, `TilingRuntime`) and `TilingFactory` in `stache-app` `host.rs`.
  `lib.rs` appends `TilingFactory::production` to the production composition **default disabled**
  (after Wallpapers, Keybindings and Hold-to-Quit); it uses a Tiling-private `ApplicationControl`
  (the repository pattern is one control per consumer), `LazySystemBackend` over default
  `AdapterOptions` (`CGWindowID`-keyed tab member reads off, so Tiling's `tabs.rs` registry is inert; tab bar identity reads on), a system `DisplayControl` and `sdk_display_id`. Construction is
  inert. Start is staged with exact rollback (backend epoch, effect worker, actor, reservation
  watcher, **display feed**, coordinator, enumeration, authority). The display feed registers a
  `DisplayControl::observe_topology` observer whose callback only records a pure hint; the
  `stache-tiling-displays` worker queries the layout off the callback and calls `submit_displays`,
  and the initial layout reaches the actor before the authority is published. Tiling never calls
  `Registry::apply_screen_topology`; the app's Bar-scoped topology feed owns it, only while Bar runs. Reconfigure flips only after the same revision is
  observed through the accepted watch. Stop seals first and then drains, restores runtime-hidden
  applications (the owned ledger), stops the watcher, the display feed, the backend epoch, the actor
  and the worker with finite waits, retaining a quarantine for any owed stage (a failed display
  observer removal is owed as `display observer`). The status travels on `ModuleStatusView.tiling`:
  lifecycle, owed stages, unsupported-feature and display diagnostics, ambiguous visibility records,
  and counts of partially placed windows and visibility conflicts. The production directory is the
  run's own application observation (`ActionsDirectory`), so every `ApplicationId` shares the
  generation its hide, unhide and activation authorizations accept.

## External borders transport (`stache_macos::borders`, Tiling part 09 slice A)

`stache_macos::borders` is the safe native transport to an externally managed `JankyBorders`
service. It is consumed by the Tiling borders runner (part 09 slice B: color/style/blacklist policy and
the `borders.enabled` consent). The module sends NUL-separated argv batches (the legacy
`encode_mach_args` bytes) as one complex Mach message with a single out-of-line descriptor to
`git.felix.borders`, and offers a bounded `borders` CLI fallback through the SDK `CommandExecutor`
(`resolve_binary`, `run_captured_prepared`, 2 s child deadline, 64 KiB per-stream cap, cancel/reap
only the exact child; no shell).

- **Never launch, leave on stop.** Every attempt starts with `ServiceAttempt::begin`, a fresh
  `bootstrap_look_up`; only the returned attempt can send or run the CLI fallback. An absent service
  yields `MissingService` and sends/spawns nothing. The service can still exit between the lookup and
  a CLI invocation (documented residual race). Stache never starts, restarts or kills a service, and
  releasing the connection sends no mutation (no `width=0`); the service keeps the last settings.
- **Rights.** `BordersConnection` owns one send right from the lookup, is `!Send`/`!Sync`, has no
  statics, and deallocates the right exactly once (replace, `release`, or drop). The temporary
  bootstrap-port right is deallocated after each lookup.
- **Outcomes.** `Sent` (kernel accepted; not an acknowledgement of applied settings), `MissingService`,
  `Rejected` (argv encoding failure or a message-validation code such as `MACH_SEND_TOO_LARGE`,
  `NO_BUFFER`, `INVALID_DATA` or a filter code, proven not enqueued; the only case that allows the CLI
  fallback). `MACH_SEND_INVALID_DEST` and `MACH_SEND_INVALID_RIGHT` prove the service is gone in this
  attempt, so they map to `MissingService` (the right is released, no CLI fallback, because running
  the `borders` CLI could itself become the service) and `Uncertain` (`MACH_SEND_TIMED_OUT`, interrupted, in progress or any unknown code; no
  fallback or autonomous retry after it). Per-message timeout is clamped to 1-500 ms (legacy used 0/100
  ms). Empty and NUL-containing arguments are rejected instead of splitting or truncating argv.
- **What is proven.** `mach_msg_header_t` (24 B), body (4 B) and OOL descriptor (16 B) sizes/offsets
  are compile-time asserted against `<mach/message.h>` (44 B message, 4-byte packing); encoding,
  outcome mapping, right lifecycle and the CLI argv are covered with fakes (`FakeTransport`, a fake
  kernel, a fake process factory). Default tests never look up a service, send a message or spawn a
  process.
- **What is not proven.** Compatibility with any real `JankyBorders` version or protocol, service
  acknowledgement, and real send-timeout behavior against a full queue are unverified until the
  approved manual protocol test in the Tiling validation plan.
