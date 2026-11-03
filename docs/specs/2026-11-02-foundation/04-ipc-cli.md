---
status: done
---

# 04 — Socket control and CLI composition

Read when implementing transport safety and compatibility commands.

## Scope and files

Create src/crates/sdk/src/ipc/{protocol,router,client,server}.rs, src/crates/sdk/src/{cli,paths}.rs,
src/crates/cli/src/{lib,commands,output,cache,mode}.rs and integration/golden tests. App supplies pure
contribution descriptors; fake handler/server tests precede real host integration in part 5.

## Key APIs and dependency direction

```rust
struct Request { module: String, method: String, params: serde_json::Value }
enum Response { Ok { data: Value }, Error { error: WireError } }
trait IpcHandler { fn handle(&self, method: &str, params: Value) -> Result<Value>; }
struct CliContribution { /* clap::Command + standalone/remote executor */ }
fn run(args: impl IntoIterator<Item = OsString>, catalog: &CommandCatalog) -> ExitCode;
```

Adapters deserialize typed params once/serialize typed responses, never build JSON strings or unchecked
Value indexing. SDK may depend on clap; CLI imports SDK, never app/modules. Host injects contributions
and standalone service factories without starting lifecycle. Reserve core namespace for reload/status.

## Requirements

- Preserve mode logic from `_legacy/app/native/src/main.rs`: no args after config stripping,
  --desktop, or executable inside .app/Contents/MacOS selects desktop. Test bundle-path precedence;
  document standalone binary for CLI. Clap validates malformed flags before any desktop startup.
- Newline-delimited JSON, one request per connection; 64 KiB request ceiling, 8 concurrent connections,
  5-second read/write timeouts. Typed malformed/unknown module/method/disabled/unavailable/handler errors.
  Client retries at most 3 times with legacy 100 ms spacing only for AppNotRunning, not arbitrary errors.
- Cache: ~/Library/Caches/com.marcosmoura.stache.next. Runtime: per-user macOS temporary directory/
  com.marcosmoura.stache.next, never beneath cache. Directory 0700, socket 0600; validate owner,
  reject symlinks/path substitution and overlong Unix paths. Do not unlink another live server;
  remove stale endpoint only after confirming no listener, cleanup only owned endpoint. Shared AppPaths
  resolves client/server consistently; fallback stays per-user/identity-isolated or fails clearly.
- Cache clear touches only cache, never runtime/config/legacy. Preserve legacy byte formatting and
  exact strings: `Cache cleared successfully. Freed {bytes}.` and
  `Cache directory does not exist. Nothing to clear.`; cache path is bare path.
- Reload rereads all files independently: silent success 0, rejected-file diagnostics/no running app
  exit 1. Never launch/restart app. Wire core.status to config path diagnostics; offline path succeeds.
- Keep schema/config commands and `completions --shell/-s` for clap-supported shells. Preserve names,
  flags/output/exit codes except agreed directory/multi-schema changes. Clap errors 2, application errors
  1 with `stache: {error}` stderr, success 0. Tiling/wallpaper absent; audio arrives through part 6.
- Serialize pretty JSON; ANSI only when stdout is TTY, never piped (including schema output).
  Port formatting helpers, not approximate table/bytes behavior. Standalone cache/config/schema/
  completions never start tray/webviews/socket server/modules. No hand-built JSON fallbacks to `{}`.

## Acceptance criteria

- [x] I01: Generic routing/core methods work on temp socket; malformed/unknown input fails safely.
- [x] I02: Ownership/mode/live-vs-stale socket protection, size/concurrency/timeouts/retries are tested;
      cache clear leaves live server reachable.
- [x] I03: Identity paths and mode selection match requirements and cannot clear legacy data.
- [x] I04: Cache/reload/config/schema/completions contracts pass parsing/output/exit tests; partial
      reload preserves last-good state and reports rejected files.
- [x] I05: Fake contributed CLI executes without reverse dependencies or desktop/lifecycle startup;
      built-ins pass with a deliberately failing desktop-start sentinel.
- [x] I06: Piped JSON parses without ANSI; PTY behavior is tested and serialization errors are reported.
- [x] I07: Applicable legacy parsing tests are ported and completions expose only registered commands.

## Tests and validation

Worker reads `_legacy/app/native/src/cli/commands/{mod,cache,config_cmd,audio}.rs`, cli/output.rs,
cache.rs and platform/ipc_socket.rs for exact tests/strings. Port applicable mod.rs tests now; audio
tests belong solely to A04. Add subprocess/PTY tests, slow/incomplete/oversize clients, concurrency,
hostile filesystem fixtures. Run `bun run lint`, `bun run test`, `bun run format:check`,
`cargo nextest run -p stache-cli -p stache-sdk`. No device mutation.

## Docs

Write docs/architecture/ipc.md; update overview/config/new-module/testing/index with core routing,
standalone-vs-remote behavior and cache/runtime invariants.
