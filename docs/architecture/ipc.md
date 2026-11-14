# Control socket and CLI

Read when changing the standalone commands, socket transport, identity paths or
the single-binary mode selection.

The `stache` binary is both the desktop host and the command line. Mode
selection runs before any Tauri initialization: commands never start the tray,
a webview, the socket server or a module. The desktop host starts the socket
after registry startup and stops accepting new work before shutdown.

## Mode selection

The app validates every invocation with the CLI's composed clap tree before
selecting a mode, including bundled executables and `--desktop`:

- no command (including config-only invocations), or `--desktop` → desktop;
- executable path containing `.app/Contents/MacOS` → desktop, regardless of
  arguments;
- anything else → CLI.

Valid bundled commands retain desktop precedence; malformed arguments exit 2
and help/version exit without starting it. Use a standalone binary for CLI work.
The desktop callback receives the resolved `ConfigPaths`; the host resolves its
own cache/runtime identity paths and instance lock before starting Tauri.

## Composition

`stache-cli` depends on `stache-sdk` only. The application injects two things:

- a `ConfigCatalog`, so `config` and `schema` describe exactly the types the
  host registers;
- a `CommandCatalog` of contributed commands, so module CLIs are added by the
  composition root and the CLI never imports a module.

```rust
catalog.add(CliContribution::new(|| clap::Command::new("audio")
    .subcommand(clap::Command::new("list")), AudioCommands))?;
```

A `CliContribution` pairs a command builder with a `CliExecutor`. Building runs
through a closure because clap commands are consumed when assembled. Executors
receive the parsed `ArgMatches`, the terminal flag and injected stdout/stderr;
they decide whether to work locally or call the running host. Contributed names
may not duplicate each other or the built-ins (`cache`, `completions`, `config`,
`reload`, `schema`). `completions` is generated from the composed tree, so it
exposes exactly the commands that exist.

A hidden, root-only, nonrepeatable `--stache-internal-cli` flag exists only so the
shared command runner can re-enter the running executable for the bare command name
`stache`. It conflicts with `--desktop`, requires a subcommand (otherwise usage exit 2)
and, after the parser validates the invocation, selects CLI mode before the
application-bundle precedence, so the same argv works for a standalone binary and
`Stache.app/Contents/MacOS/stache`. Help and version keep parser precedence.
Without the flag the direct, Finder and `--desktop` rules are unchanged. It is not a
public command and not an environment switch.

Built-ins are `config init|path|migrate`, `cache clear|path`, `reload`,
`schema [id]` and `completions --shell|-s`. The proxyAudio pilot contributes
`audio list`; see [proxy-audio.md](../features/proxy-audio.md).

## Desktop lifecycle

The desktop claims the Next identity's `runtime/instance.lock` before creating
the tray; a second launch exits successfully without a registry, watcher,
listener, webview or `exec_on_startup`. After the tray exists, the background host
starts modules and then `IpcServer` with `ConfigCore` over the one
`ConfigStore`. The server is owned by the host, not by a Tauri plugin.

Shutdown is one idempotent coordinator shared by tray Quit, SIGINT/SIGTERM/SIGHUP,
Tauri's exit event and AppKit's terminate decision. Requests only record a deadline;
the dedicated coordinator thread owns the single outcome and terminal action and runs
the ordered stages itself off the main thread: close factory admission and wake
factory-owned host-service waits, invalidate IPC and configuration work,
reap the watcher, permission poller and owned startup job while observing the host
worker in the same stage budget, stop the registry in reverse start order one module at
a time, drain factory-owned services in reverse registration order, then release the
socket and the registered native owners. Each wait is capped at
`min(2 s, remaining)`, so a hung owner is named unacknowledged and abandoned instead of
joined; a factory drain names `factory:<id>`. Keep Awake's `power-assertion` slot
acknowledges the release its module `stop` already performed over the same ledger;
an active or acquiring generation, a release in flight or an
unresolved ID is never reported clean, and the slot retries nothing. Socket acknowledgement
requires both the accept thread and every handler gate permit to finish under one bound;
acceptance stops without joining the accept thread and
the owned endpoint is removed after that bound, so a connection holding its handler cannot
delay it. A failed removal of the owned inode is reported as `failed socket` instead of
being dropped, while an already-missing endpoint or a foreign inode at that path counts as
success. A request read after invalidation gets the typed `unavailable` error while an
in-flight handler finishes unharmed. The tray item, the activation observer and the tracked
windows are released by their own acknowledged main-thread hops, separate from the final
exit; a single stalled main hop names every remaining main-thread owner without another
wait. `Clean` is reported only when every owner answered successfully. The total is five
seconds from the first request; a later request shortens it to 250 ms from itself. A
non-zero intended host code reaches the CLI as a failure instead of being reported as
success, and exactly one outcome record is attempted. Repeated requests never start a
second run. Module socket handlers are contributed by their factories and registered on
the same `Router` before the server starts; there is no second server. Real Cmd-Q is verified
on this route; logout, restart and shutdown use it too but have not been verified end to end.

## Wire protocol

One newline-delimited JSON request per Unix connection, one response back:

```json
{"module":"proxyAudio","method":"list","params":{"kind":"output"}}
{"status":"ok","data":[]}
{"status":"error","error":{"kind":"disabled","message":"proxyAudio is disabled"}}
```

`status` is `ok` or `error`; error kinds are `malformedRequest`, `invalidParams`,
`unknownModule`, `unknownMethod`, `disabled`, `unavailable` and `handler`.
Adapters deserialize `params` exactly once with `decode_params` and serialize
typed responses once with `encode_response`; there is no hand-built JSON and no
`{}` fallback. An unknown module, unknown method or malformed body is reported
as a typed error and the server keeps serving.

`proxyAudio.list` is read-only. `kind` is `all` (the default when params are
omitted or `null`), `input` or `output`; the response is the same
`AudioDeviceInfo` shape as `stache audio list --json`. The handler shares the
listing service with the command and never starts a listener.

### Keep Awake status

`keepAwake.status` is the other module method and is also read-only. Omitted or
`null` params and `{}` are the same zero-argument request; any non-empty object,
string, number, boolean or array (including `[]`) is `invalidParams`, and an
unknown method is `unknownMethod`. The response is the app-owned
`KeepAwakeStatus` envelope:

```json
{
  "module": {
    "id": "keepAwake",
    "state": "running",
    "enabled": true,
    "paused": false,
    "revision": "3",
    "detail": null,
    "diagnostic": null
  },
  "runtime": {
    "assertionState": "active",
    "lockState": "unlocked",
    "observation": "notificationsAndPolling",
    "conditions": []
  }
}
```

`module` is the `ModuleStatusView` projection: lifecycle, enabled, paused, the
exact revision as a decimal string and diagnostics. The handler prefers the held
`ConfigStore` file diagnostic over the registry diagnostic, so a rejected parse,
type, I/O or deleted file stays visible while the last accepted configuration,
revision and lease remain live. `runtime` is the module's single published
observation, copied from `KeepAwakeSnapshot::read()`; condition kinds keep a
stable order. The envelope and runtime DTOs are exported through the same
`bindings::builder()` collection as the other config types, so
`generate-bindings` is the only TypeScript update; there is no Tauri command, CLI
command or hand-written TypeScript mirror.

The method answers in every module state, including disabled, paused, failed,
locked, acquiring and `releasePending`. It copies published and held data only:
no native probe or assertion, worker start, observer install, filesystem read or
write, configuration apply or window effect. Runtime fields are coherent with
each other, but lifecycle, diagnostics and runtime are sampled separately, so a
response is an observation, not a globally atomic snapshot. `active` proves
owned prevention, not immunity to lid close, forced sleep or low-battery policy.
See [keep-awake.md](../features/keep-awake.md).

### Wallpapers set

`wallpapers.set` carries `WallpapersSetParams` (`configDirectory`,
`selection`, `screen`) and answers with one `WallpapersSetOutcome` in
`Response::Ok` data. Transport success means the request was handled, never that
a wallpaper was applied: `status` is `fullSuccess`, `partial`, `failure`,
`cancelled` or `uncertain`, with a typed `cause` and per-display
`WallpapersTargetOutcome` entries using `applied`, `failed`, `notAttempted` or
`unknown`. `selection` is `file` (absolute literal path), `collection` (zero-based
accepted-pool index), `name` (a file name or stem looked up on the accepted pool,
case-insensitive, first pool entry wins; a miss is a typed `invalidSelection`) or
`random`. The server canonicalizes the requested configuration directory and
its own accepted identity before any work and refuses a different reachable
directory. The handler is synchronous; it enforces `WALLPAPERS_SERVER_BUDGET`
(115 s) internally for admission, processing and native work. The facade is
registered even while the module is disabled or paused. The contributed
`stache wallpaper set` is the caller of the 120-second client budget: it builds
`IpcClient::with_options(socket, RetryPolicy::legacy(), WALLPAPERS_CLIENT_BUDGET)`
and sends exactly one `wallpapers.set` with no separate `core.status` probe.
Only `NotRunning` (nothing was sent) allows one standalone application. Every
other client error, a malformed `Ok` payload or a `handler` wire error is an
uncertain outcome with no local application and no retry; other wire error kinds
are reported as typed refusals before any native change. Built-in client and
server transport defaults, wire limits, connection cap and retry policy are
unchanged. See [wallpapers.md](../features/wallpapers.md#cli). The request,
selection, screen, outcome, cause and target DTOs are generated from the same
Rust types through `bindings::builder()`.

`core` is reserved. `core.reload` rereads every file and returns per-file
`applied { revision }`, `unchanged` or `rejected` outcomes with diagnostics;
`core.status` returns the configuration directory, each file's state
(`missing`, `valid`, `invalid`), whether its current bytes were accepted for a
loaded global or bound module, diagnostics and warnings. Held diagnostics are
independent of disk validity; `config path` sends host diagnostics to stderr.
`ConfigCore` implements both over the
existing `ConfigStore`; the store still owns last-good state, so one rejected
file never blocks the others. The desktop `core.reload` wrapper asks the host
worker to publish the resulting module status once, so a CLI reload updates the
tray and popover without a restart.

### Tiling methods

`stache-tiling` serves module `tiling` through `TilingIpc` (registered by
`TilingFactory::ipc_handlers`, wrapped in the host gate). The methods are
`pause`, `resume`, `query`, `window` and `workspace`. Every request type denies
unknown fields and is decoded once; every `Response::Ok.data` is encoded once.

- `query` takes `{"kind":"screens|workspaces|windows|apps|status"}` with optional
  `screen`, `workspace`, `focusedScreen`, `focusedWorkspace` filters (conflicting
  filters are `InvalidParams`) and answers a bare array of rows in actor snapshot
  order, never filtered by name sort. Row fields are fixed and camelCase:
  screens `id,name,isMain,isBuiltin,scaleFactor,frame,visibleFrame`, workspaces
  `id` (string)`,name,screenName,layout,isVisible,isFocused,windowCount`, windows
  `id,pid,appId,appName,title,workspace,frame,isMinimized,isFullscreen,isFloating,isFocused`
  and apps `pid,name,bundleId,isHidden`. `status` takes no filter and answers one object
  (`phase,healthy,faults,layoutActive,configRevision,planSequence,apps,trackedApps,windows,failedApps,truncatedApps,unconfirmedApps,inflightApps,noFrame,hiddenApps,lastIntent,trackedWindows,excludedWindows,frameAttempts,enumerationFailures,animationFallbacks`),
  also while the runtime is initializing. That declaration order is the frozen JSON
  key order the CLI renders. A disabled, paused, pending-cleanup or not yet
  initialized module answers a typed error, never an empty list.
- `window` takes `{focus?, swap?, toggleFloating?, preset?, resize?: [{dimension,
  amount}], sendToScreen?, sendToWorkspace?}`; `workspace` takes `{focus?,
  layout?, balance?, sendToScreen?}`. Actions always run in the fixed order focus,
  swap, toggle-floating, preset, resize pairs (as supplied), send-to-screen,
  send-to-workspace (workspace: focus, layout, balance, send-to-screen), whatever
  the field order. Each action waits for its actor record before the next
  selector resolves and the first reported failure stops the list. Earlier
  admitted actions are not rolled back; resize dimension/amount are validated when
  that pair executes, so earlier actions may already have run. Focus accepts
  directions case-insensitively (`prev` aliases `previous`) or a numeric window id
  resolved once to the exact current window; swap and `layout` are exact.
  Focusing an inactive workspace (no window and no way to show it empty, see
  [Tiling](../features/tiling.md)) fails with `Workspace is inactive`.
- `pause`/`resume` take `{}` and wait for the registry acknowledgement.
  They are single-flight: while another pause or resume is still running (including
  one abandoned after its deadline) the request is not started and answers a typed
  `uncertain` outcome with `notAttempted` action status and
  `unknown: [{subject: "stage:lifecycle", reason: "inProgress"}]`, which the CLI prints
  as the uncertain error (exit 1); it is not reported as pending cleanup.

Mutations answer `Response::Ok` with `OperationResponse`, tagged by `outcome`:
`enqueued` (the actor admitted the request), `applied` (the stated scope is
confirmed, for example an acknowledged pause), `partial` or `uncertain`, each with
`request.actions[]`, `targets`, `failed`, `notAttempted` and `unknown` evidence.
`enqueued` is **admission**, not proof that a window moved. Known immediate
failures (a refusal, a proven no-send, a dead target, a missing workspace) are
typed wire errors with fixed bodies (`ipc::body`), not `Ok` bodies. Existing
`WireErrorKind`s are used (`Unavailable`, `Disabled`, `Handler`, `InvalidParams`,
`UnknownMethod`); no wire variant was added. One request has a finite budget
(4 s, hops capped at 2 s) inside the client's 5 s total; host begin-shutdown
cancels waits, and no request is retried after a possible send.

### Accessibility gate

The host publishes its Accessibility gate state (`checking`, `blocked`,
`open`; see [overview.md](overview.md)) through a lock-free atomic cell that
the runner writes on every gate transition. An ungated host (empty baseline,
test fixtures) reads `open`; a gated host starts at `checking`. Socket handlers
read the cell without the host worker, a module lock or the registry lifecycle
lock, so a refusal never waits on a busy worker or a stuck module start.

`build_router` wraps every factory-contributed handler before registration.
While the cell is not `open` (including `checking`) the wrapper answers the
typed error `unavailable` with `Waiting for Accessibility
permission. Grant it in System Settings > Privacy & Security > Accessibility.`
and never calls the module; read-only methods such as `keepAwake.status` and
`proxyAudio.list` are refused too. While `open` it delegates unchanged. The
cell also closes at the start of a revocation, before the registry stops
modules.

`core.reload` and `core.status` keep working while gated: reload applies and
stages files as usual and still asks the host to publish once. Both results
carry `access: { state: "checking" | "blocked" | "open", revoked }`, written
by the host core and omitted (`None`, not serialized) by `ConfigCore` and by
older hosts. Older JSON without `access` still decodes, and a client treats a
missing field as "not reported".

CLI: when `access.state` is `blocked` or `checking`, `stache reload` prints
`stache: modules are waiting for Accessibility permission` once on stderr after
any file diagnostics (exit 0 unless a file was rejected, then 1 with both
printed), and `stache config path` prints the same line after its host
diagnostics. A contributed command that calls a module method prints the
`unavailable` message through its normal typed-refusal path;
`stache wallpaper set` stays a refusal and never falls back to a standalone
apply, because only `NotRunning` permits that. Commands that never contact the
app are unaffected.

## Bar data bridge (Tauri IPC)

Not the control socket: the Bar webview talks to the host through one generated
command and one generated event (`src/ui/src/ipc/bindings.gen.ts`).

- `bar_snapshot()` returns `BarSnapshotOutcome`, tagged by `kind`: `available`
  (`view`: `generation`, `sequence`, `configRevision` as decimal strings, `panel`,
  `menu`, `frame`, `height`, `padding`, `detail`), `unavailable` (`reason`,
  `detail`, `generation`, `sequence`) or `unauthorized`. It takes no argument:
  the caller is the actual webview window, never a supplied label.
- `BarChanged` (`generation`, `sequence`, `resnapshot`, `outcome`) is emitted to
  the live Bar window only. Ordering: subscribe, then snapshot, then apply only a
  strictly greater numeric `sequence` (use `BigInt`); `resnapshot: true` means call
  `bar_snapshot` again. Delivery is coalesced and bounded and never blocks the
  command, which reads published state only.

- `bar_action(action)` runs one fixed Bar click. `action` is the closed enum
  `cpu | clock | battery | wifi | weather`; there is no name, path, URL or argv. It returns
  `BarActionOutcome` (command result data, never an error string), tagged by `kind`:
  `completed`, `stale`, `notIssued`, `refused` (`detail`), `notInstalled`,
  `timedOutBeforeIssuance`, `uncertainAfterIssuance`, `busy`, `unauthorized` or
  `unavailable` (`reason`, `detail`). `completed` means macOS reported the issued
  request done, not that a window is visible; `uncertainAfterIssuance` is never
  retried. The caller is the actual webview window, authorized like `bar_snapshot`.
  It is an async command: the bounded native work runs on the blocking pool and one
  action is in flight at a time (`busy` otherwise). See
  [Bar fixed clicks](../features/bar.md#click-actions).
- `bar_weather()` returns `BarWeatherOutcome` (tagged by `kind`: `ready`
  with `weather`, `unavailable`, `unauthorized`, `stale`) for one weather refresh.
  `weather` carries the configured provider, the trimmed Visual Crossing key and
  the location (`coreLocation`, `defaultLocation` or `none`). The key and
  coordinates exist only in this response, which goes to the authorized Bar window
  only; they are never in `bar_snapshot`, `BarChanged`, `host_status` or
  `StatusChanged`. Async, off the main thread and bounded. See
  [Bar weather data path](../features/bar.md#weather-data-path).

- `bar_session_action(module, action)` pauses or resumes one module for this
  session and returns `SessionActionOutcome` (tagged by `kind`: `applied`,
  `notApplied` (`reason`), `stale`, `disabled`, `unavailable` (`reason`), `busy`,
  `uncertain`, `unauthorized`). `action` is the closed enum `pause | resume`;
  `module` is validated strictly and the Bar capability holds only `keepAwake`.
  The actual calling window must be the live Bar panel; the host runner executes
  the request in command order through the module's existing pause/resume control
  and returns the actual result (never a queue acknowledgement); a request whose
  action no longer matches the current session state is `stale`. ACL-gated by
  `allow-bar-session-action`. Async, bounded, one in flight (`busy`). No CLI
  grammar or socket method is added. See
  [Keep Awake session click](../features/bar.md#keep-awake-session-click).
- `bar_tiling_action(request)` focuses one Tiling workspace or window for the
  Bar strip and returns `BarTilingOutcome` (tagged by `kind`: `applied`, `noOp`,
  `stale`, `unavailable` (`reason`: `barNotRunning`, `notRunning`, `disabled`,
  `paused`, `cleanupPending`, `shuttingDown`, `unhealthy`), `busy`, `uncertain`,
  `refused` (also for an inactive workspace), `failed`, `invalid`, `unauthorized`). `request` is
  `{ generation, sequence, action }` with unknown fields rejected; `action` is the
  closed enum `focusWorkspace { name } | focusWindow { window }` (an opaque
  decimal key from `BarView.tiling`). The actual calling window must be the live Bar
  panel; malformed decimals are `invalid`; a rendered generation that is not the
  current one or a sequence ahead of it is `stale`; Tiling's own typed focus path
  resolves the key against its live snapshot and revalidates at issuance. It never
  enables, resumes or reconfigures Tiling. ACL-gated by `allow-bar-tiling-action`.
  Async, bounded, one Tiling action in flight (`busy`, never queued). No CLI grammar
  or socket method is added. See [Tiling strip data](../features/bar.md#tiling-strip-data).
- `bar_media_artwork(id)` returns `BarArtworkOutcome` (tagged by `kind`: `ready`
  with `artwork` (base64 PNG), `missing`, `unauthorized`, `unavailable`). `id` is the
  `artworkId` of `BarView.media`; a malformed id is `missing` without touching the
  filesystem. Authorized like `bar_weather`, ACL-gated by `allow-bar-media-artwork`.
  `BarAction` also has `media` (no identifier; the host maps the published player)
  and `BarActionOutcome` has `notAttempted`. See
  [Bar media data path](../features/bar.md#media-data-path).

## Client behavior

`IpcClient` sets a five-second total response deadline and write timeout. A
response requires a newline and may contain at most 1 MiB before it. It retries only
`NotRunning` (absent socket or refused connect, before any bytes are sent): three
attempts, 100 ms apart. Once connected, broken pipes and resets are I/O errors,
never retries: the peer may already have executed the action. Timeouts, invalid
responses and structured remote errors are not retried either. Requests
are serialized with serde; a serialization failure is a typed client error.
`config path` uses a single-attempt client so it stays instant while offline.

`reload` is silent on success (exit 0) apart from the Accessibility notice
described above. Rejected files print
`stache: {diagnostic}` on stderr and exit 1; no running host prints
`stache: IPC error: Failed to send reload command to Stache app: Stache app is not running`
and exits 1. Application errors use `stache: {error}` on stderr. Clap usage
errors exit 2 and success exits 0. `reload` never launches or restarts the app.

## Endpoint safety

The server owns the runtime directory and its socket:

- the runtime directory is created `0700`; an existing directory must be a real
  directory, owned by the resolved user, and is re-tightened to `0700`;
- the socket is created `0600`, must be a socket, and must be owned by the same
  user; validation precedes chmod, and symlinks/regular files are not unlinked;
- a private `0600` regular `server.lock` is opened without following symlinks
  and locked across the server lifetime. Directory and lock identities are
  rechecked after locking, serializing cooperating stale-probe/unlink/bind calls;
- an existing socket is probed. A successful connect means another server is
  live and the endpoint is left alone. `ConnectionRefused`/`NotFound` means it
  is stale and may be removed; other probe errors are reported, not guessed;
- shutdown removes only the socket inode the server created (device and inode
  are captured after bind), so a foreign endpoint is preserved. A missing or
  foreign inode is a successful removal; a real removal error on the owned inode
  is reported as a failed `socket` acknowledgement instead of being dropped.

The accept loop is non-blocking and shares a stop flag with a slot gate. When no
connection is pending it waits in `poll(2)` on the listener and on the read end
of a close-on-exec wake pipe the gate owns, with no timeout, so a connection is
accepted as soon as it arrives and an idle server makes no timed wakeup. A stop
sets the flag and writes one byte to the non-blocking write end, which ends the
wait at once; a full pipe already holds a wake byte, so that write cannot fail
the stop. Another `accept` error, or a failing `poll`, waits 50 ms or until stop
before retrying, so a persistent error cannot spin. At most
8 connections are served at once; excess connections wait in the kernel backlog.
Each connection has a five-second total read deadline and write timeout, and a
request may not exceed 64 KiB before its newline. Over-long, empty, incomplete
and timed-out requests are closed or answered with a typed error; none panic.
Finished handlers publish their thread ID to a bounded completion channel before
releasing capacity. Reaping joins only those IDs, never an arbitrary active
handler; retained handles remain within the connection limit. The loop reaps on
every pass, and an idle loop makes none, so finished handles within that limit
are joined when the next connection or the stop arrives.
In unwinding (debug and test) builds a drop guard releases capacity on a panic and
a handler panic becomes a `Handler` wire error; the release build aborts the
process instead, and the client sees the connection close
([Release panics](sdk.md#release-panics)).
Stop updates its predicate under the same mutex used by waiting acquisitions.
Bounded shutdown joins the accept thread only when it and every handler permit
finished inside the bound; otherwise it detaches them and still removes the owned
endpoint. The unbounded drop path joins the accept thread; handlers must terminate
themselves within their read and write timeouts.
Zero connection/request bounds clamp to one; zero timeout is rejected before
endpoint creation. Failed socket-mode or timeout setup closes that connection.
These checks protect against foreign users and cooperating startup races, not
arbitrary malicious same-UID path mutation between filesystem checks and use.

Any process of the same user can call every socket method, including
`tiling query windows` (which returns window titles) and the window and
workspace mutations. There is deliberately no peer-credential check: the `0600`
socket in a `0700` directory already excludes other users, and a same-user
process could edit the configuration, which is code, anyway. See the
[trust model](config.md#trust-model).

## Identity paths

`AppPaths` is shared by client and server and derived from an injected
`AppEnvironment` and the build's compile-time `IDENTITY` (`com.marcosmoura.stache` in release
builds, `com.marcosmoura.stache.next` in debug builds; see
[overview.md](overview.md#identity)). `AppPaths::resolve_for` takes an explicit identity only as a
test seam; every production caller uses `resolve`:

- cache: `~/Library/Caches/<identity>`. For a release build this is the path the removed legacy app
  used, which may still hold its files (`stache.sock`, `wallpapers`, `media_artwork`); they are never
  read, and `stache cache clear` would remove them with the rest of the tree. A development build's
  cache is `~/Library/Caches/com.marcosmoura.stache.next`;
- runtime: `$TMPDIR/<identity>`, never beneath the cache;
- socket: `runtime/ipc`, rejected above 103 bytes because that is the
  macOS `sun_path` limit;
- instance: `runtime/instance.lock`, a private `0600` regular file locked
  exclusively for the desktop process lifetime;
- cache lease: `runtime/cache.lock`, a private `0600` regular file locked shared
  or exclusively for cache work, never unlinked by a participant.

Missing `HOME` or `TMPDIR` fails with a typed error rather than falling back to
a shared directory. Relative anchors are refused; existing directory anchors
are canonicalized before overlap checks (including parent symlinks and `..`).
Expected ownership is the process effective UID via safe rustix, never HOME
metadata. The resolved temporary root must be owned by that UID. Tests pass temp
roots and never touch the real home, cache or runtime of either identity.

## Cache command

`cache path` prints the bare path. `cache clear` refuses a symlinked,
non-directory or foreign-owned cache path, counts bytes without following
symlinks (links and sockets contribute none), then removes the directory tree
under one exclusive cache lease. It waits at most five seconds for that lease;
on expiry it mutates nothing, prints
`stache: Failed to clear cache: cache is busy; retry after current operations finish.`
and exits 1, while a cancelled wait prints
`stache: Failed to clear cache: operation cancelled.` and exits 1. The root is
revalidated under the lease and the held lock identity is checked again just
before removal.

Clear may create the runtime directory `0700` for the lease but never creates a
socket, and it never removes the lock. The legacy success and empty messages
are preserved byte-for-byte; byte formatting matches
`0 bytes`, `1.00 KB`, `1.50 MB`, `1.00 GB`. Clearing can never reach the
runtime directory, the configuration directory or the other identity's cache: a release CLI
clears only `com.marcosmoura.stache` and a development CLI only `com.marcosmoura.stache.next`. The lease is coordination among cooperating
processes, not protection against OS or temporary-directory cleaners or
malicious same-UID mutation.

## Audio command

`audio list [-j|--json] [-i|--input] [-o|--output]` runs locally in the CLI
process. It enumerates through `CoreAudio`, never connects to the socket, never
starts the desktop and never registers a change listener. Neither flag, or both,
selects every device; results are sorted by name. JSON preserves the legacy
`name` / `type` / `input` / `output` keys and is styled only on a terminal. See
[proxy-audio.md](../features/proxy-audio.md).

## Wallpaper command

`wallpaper set|list|generate-all` is contributed by `stache-wallpapers`
(`src/cli/`), added in `src/crates/app/src/commands.rs`; `stache-cli` still never
imports a module. The composition root injects the config and identity
environment, invocation CWD, the control-socket transport, the native desktop
(`NativeDesktop` over `MainThread`, which runs inline on the CLI process main
thread), processor, clock, random source and a cancellation scope. The scope
installs the existing `SignalBridge` only while a wallpaper command runs, so
the first SIGINT/SIGTERM/SIGHUP cancels a `generate-all` or standalone `set`
cooperatively while the command runs. A second signal force-exits with status 130. The default handlers are not restored afterwards; the process exits right
after the command.
`list` and `generate-all` never touch the socket. The global `-c/--config`
value propagates into the subcommand matches and resolves exactly like the
built-ins. See [wallpapers.md](../features/wallpapers.md#cli).

Contributed executors receive the same `ansi` flag as built-ins
(`CliExecutor::execute`), so a contributed command follows one terminal policy.

## Tiling command

`tiling pause|resume|query|window|workspace` is contributed by `stache-tiling`
(`src/cli.rs`, `src/cli/render.rs`), added in `src/crates/app/src/commands.rs`.
The composition root injects the config and identity environment and the JSON
styling; the module crate owns the clap tree, renderers and error adapters, so
`stache-cli` stays SDK-only. The command never starts the desktop, the
Accessibility owner or the module: it needs a running host with the **same
configuration directory** and otherwise fails. `-c/--config` is a directory
(before or after any subcommand); before each request the CLI reads `core.status`
and compares the canonical directory with the host's, and a mismatch is refused
(`Running host uses config directory "..."; requested "...". Use the same config
directory or stop that host.`) before any Tiling request is sent.

Grammar (see [tiling.md](../features/tiling.md#cli) for examples):

```text
stache tiling pause|resume
stache tiling query [-j|--json] [-d|--detailed] [screens|apps|status]
stache tiling query [-j|--json] [-d|--detailed] workspaces [--focused-screen|--screen NAME]
stache tiling query [-j|--json] [-d|--detailed] windows [--focused-screen|--focused-workspace|--screen NAME] [--workspace NAME]
stache tiling window [--focus DIRECTION|WINDOW_ID] [--swap DIRECTION] [--toggle-floating] [--preset NAME] [--resize DIMENSION AMOUNT]... [--send-to-screen NAME] [--send-to-workspace NAME]
stache tiling workspace [--focus NAME] [--layout LAYOUT] [--balance] [--send-to-screen NAME]
```

`-j`/`-d` are global within `query`. `query` alone prints the query help and a
blank line and exits 0. Swap and `--layout` are exact enums (`--layout` is snake_case, with
`split-vertical`/`split-horizontal` accepted as CLI-only aliases; the socket
accepts snake_case only), so an invalid value exits 2; focus is a free string and resize dimension/amount are
validated semantically by the host (exit 1).

| Situation                                                 | Stream | Exit | Body                                                                                          |
| --------------------------------------------------------- | ------ | ---- | --------------------------------------------------------------------------------------------- |
| Parse, arity, value-enum or flag-conflict error           | stderr | 2    | clap text                                                                                     |
| Mutation `enqueued` or `applied`                          | none   | 0    | silent (admission, not native success)                                                        |
| Mutation `partial`                                        | stderr | 1    | `stache: IPC error: Tiling operation partially applied.`                                      |
| Mutation `uncertain` (or a non-`applied` pause)           | stderr | 1    | `stache: IPC error: Tiling operation outcome is uncertain; it may already have taken effect.` |
| Action/semantic error or host refusal                     | stderr | 1    | `stache: IPC error: {body}`                                                                   |
| No window/workspace flags; bad resize dimension or amount | stderr | 1    | `stache: {body}` (legacy argument errors; no host contact for the empty request)              |
| Query success                                             | stdout | 0    | table, or the whole pretty JSON array plus a newline                                          |
| Query failure, table mode                                 | stdout | 1    | `Stache app is not running.` when offline, else `Error: {body}`                               |
| Query failure, `--json`                                   | stdout | 1    | `{"error":"Stache app is not running"}` or `{"error":"{body}"}` (serde-escaped)               |

Typed SDK client errors map through the module adapter, never the SDK's
lowercase `Display`: not running becomes `Stache app is not running`, timeout
`Connection timed out`, I/O `IO error: {source}`, an invalid envelope `Invalid
response: {detail}`, a request that cannot be serialized `Cannot serialize tiling
request` and a remote error its frozen body. Data that does not decode into the
typed rows is `Invalid response: the Tiling response could not be decoded`; the
payload is never echoed and a failed decode is never an empty success. Control
characters in bodies and in table cells print as visible escapes (`\u001b`).

Tables are rounded (`╭ ─ ┬ ╮ │ ├ ┼ ┤ ╰ ┴ ╯`), one space of padding, widths measured in
terminal columns, `✓`/`✗` glyphs, headings `Screens (N)`, `Workspaces (N)`,
`Windows (N)` and `Running Apps (N)` and empty bodies `No screens detected.`, `No
workspaces found.`, `No windows found.` and `No running apps found.`. ANSI
(bold headings, green/red glyphs, dimmed empty bodies, red `Error:`) is emitted
only when stdout is a terminal; stripped of escapes it equals the piped text.
Contributed executors receive the same `ansi` flag as built-ins.

## Output

`schema` prints deterministic pretty JSON. ANSI styling is added only when
stdout is a terminal; piped output, including redirected schema files, contains
no escape sequences. The styling pass inserts escapes only, so stripping them
restores the exact serde output. Serialization failures are reported as
application errors rather than replaced by a placeholder. `audio list --json`
uses the same injected styling function for the same policy.
