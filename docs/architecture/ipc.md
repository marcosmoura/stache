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

Built-ins are `config init|path|migrate`, `cache clear|path`, `reload`,
`schema [id]` and `completions --shell|-s`. The proxyAudio pilot contributes
`audio list`; see [proxy-audio.md](../features/proxy-audio.md).

## Desktop lifecycle

The desktop claims the Next identity's `runtime/instance.lock` before creating
the tray; a second launch exits successfully without a registry, watcher,
listener, webview or `execOnStartup`. After the tray exists, the background host
starts modules and then `IpcServer` with `ConfigCore` over the one
`ConfigStore`. The server is owned by the host, not by a Tauri plugin.

Shutdown is one idempotent coordinator shared by tray Quit, SIGINT/SIGTERM/SIGHUP,
Tauri's exit event and AppKit's terminate decision. Requests only record a deadline;
the dedicated coordinator thread owns the single outcome and terminal action and runs
the ordered stages itself off the main thread: invalidate IPC and configuration work,
reap the watcher, permission poller and owned startup job while observing the host
worker in the same stage budget, stop the registry in reverse start order one module at
a time, then release the socket and the registered native owners. Each wait is capped at
`min(2 s, remaining)`, so a hung owner is named unacknowledged and abandoned instead of
joined. Socket acknowledgement requires both the accept thread and every handler gate
permit to finish under one bound; acceptance stops without joining the accept thread and
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
tray and test window without a restart.

## Client behavior

`IpcClient` sets a five-second total response deadline and write timeout. A
response requires a newline and may contain at most 1 MiB before it. It retries only
`NotRunning` (absent socket or refused connect, before any bytes are sent): three
attempts, 100 ms apart. Once connected, broken pipes and resets are I/O errors,
never retries: the peer may already have executed the action. Timeouts, invalid
responses and structured remote errors are not retried either. Requests
are serialized with serde; a serialization failure is a typed client error.
`config path` uses a single-attempt client so it stays instant while offline.

`reload` is silent on success (exit 0). Rejected files print
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

The accept loop is non-blocking and shares a stop flag with a slot gate. At most
8 connections are served at once; excess connections wait in the kernel backlog.
Each connection has a five-second total read deadline and write timeout, and a
request may not exceed 64 KiB before its newline. Over-long, empty, incomplete
and timed-out requests are closed or answered with a typed error; none panic.
Finished handlers publish their thread ID to a bounded completion channel before
releasing capacity. Reaping joins only those IDs, never an arbitrary active
handler; retained handles remain within the connection limit.
A drop guard releases capacity even on panic; handler panics become wire errors.
Stop updates its predicate under the same mutex used by waiting acquisitions.
Bounded shutdown joins the accept thread only when it and every handler permit
finished inside the bound; otherwise it detaches them and still removes the owned
endpoint. The unbounded drop path joins the accept thread; handlers must terminate
themselves within their read and write timeouts.
Zero connection/request bounds clamp to one; zero timeout is rejected before
endpoint creation. Failed socket-mode or timeout setup closes that connection.
These checks protect against foreign users and cooperating startup races, not
arbitrary malicious same-UID path mutation between filesystem checks and use.

## Identity paths

`AppPaths` is shared by client and server and derived from an injected
`AppEnvironment`:

- cache: `~/Library/Caches/com.marcosmoura.stache.next`;
- runtime: `$TMPDIR/com.marcosmoura.stache.next`, never beneath the cache;
- socket: `runtime/ipc`, rejected above 103 bytes because that is the
  macOS `sun_path` limit;
- instance: `runtime/instance.lock`, a private `0600` regular file locked
  exclusively for the desktop process lifetime.

Missing `HOME` or `TMPDIR` fails with a typed error rather than falling back to
a shared directory. Relative anchors are refused; existing directory anchors
are canonicalized before overlap checks (including parent symlinks and `..`).
Expected ownership is the process effective UID via safe rustix, never HOME
metadata. The resolved temporary root must be owned by that UID. Tests pass temp
roots and never touch the real home, cache, runtime or legacy identity.

## Cache command

`cache path` prints the bare path. `cache clear` refuses a symlinked,
non-directory or foreign-owned cache path, counts bytes without following
symlinks (links and sockets contribute none), then removes the directory tree.
The two legacy messages are preserved byte-for-byte; byte formatting matches
`0 bytes`, `1.00 KB`, `1.50 MB`, `1.00 GB`. Clearing can never reach the
runtime directory, the configuration directory or the legacy
`com.marcosmoura.stache` cache.

## Audio command

`audio list [-j|--json] [-i|--input] [-o|--output]` runs locally in the CLI
process. It enumerates through `CoreAudio`, never connects to the socket, never
starts the desktop and never registers a change listener. Neither flag, or both,
selects every device; results are sorted by name. JSON preserves the legacy
`name` / `type` / `input` / `output` keys and is styled only on a terminal. See
[proxy-audio.md](../features/proxy-audio.md).

Contributed executors receive the same `ansi` flag as built-ins
(`CliExecutor::execute`), so a contributed command follows one terminal policy.

## Output

`schema` prints deterministic pretty JSON. ANSI styling is added only when
stdout is a terminal; piped output, including redirected schema files, contains
no escape sequences. The styling pass inserts escapes only, so stripping them
restores the exact serde output. Serialization failures are reported as
application errors rather than replaced by a placeholder. `audio list --json`
uses the same injected styling function for the same policy.
