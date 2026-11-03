# Control channel and CLI

Read when reviewing the planned socket/command boundary; materialize as docs/architecture/ipc.md in part 4.

## Routing

One newline-delimited request per Unix connection: {module, method, params}. Registry adapters decode
typed params and return serialized data or structured errors. core.reload/core.status are reserved;
modules declare methods without adding SDK switches. Distributed notifications are not a control path.

Limits: 8 concurrent connections, 5-second read/write timeouts, 64 KiB request maximum. Preserve bounded
AppNotRunning-only retries; unknown/invalid/oversized requests never panic. Runtime directory 0700,
socket 0600, ownership/symlink checks and live-endpoint protection before stale cleanup.

## Paths

Next cache is ~/Library/Caches/com.marcosmoura.stache.next. Runtime socket uses a separate per-user
macOS temporary directory named for next identity. AppPaths is shared between client/server.
Cache clear cannot touch runtime, config or legacy data. Failure to resolve a safe path is an error,
not fallback to a public shared socket directory.

## Execution

src/crates/cli depends on SDK, never app/modules. Composition root passes descriptors and executors.
Standalone cache/config/schema/completions and pilot audio enumeration do not launch desktop or module
listeners. Reload requires running host. Audio list can also expose read-only IPC using the same service.

No arguments after config stripping, --desktop, or app-bundle executable path selects desktop, retaining
legacy precedence. Use standalone executable for CLI. Clap errors exit 2; application errors exit 1
with stache: prefix on stderr; success exits 0. No tiling/wallpaper commands before their ports.

Cache success/no-directory messages and byte formatting retain legacy output. Cache path is bare path;
reload succeeds silently. JSON uses typed serialization/pretty formatting; ANSI allowed only on TTY.
Schemas/config commands carry the explicitly changed directory/multi-file semantics from config docs.

Compatibility tests read/port legacy CLI parsing/output tests; PTY and piped output are tested separately.
