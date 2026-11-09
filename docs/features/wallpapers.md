# Wallpapers (configuration, sources, pipeline, runtime, host IPC and CLI shipped)

Read when changing `wallpapers.toml`, wallpaper source resolution, the
processing/cache/apply library, the rotation coordinator, the typed set handler
or its production factory, migration of the legacy wallpaper section or the
accepted-configuration SDK watch, or the `stache wallpaper` commands. The
runtime, production factory, typed `wallpapers.set` socket route, generated
bindings and the `wallpaper set|list|generate-all` CLI ship. Real-desktop
behavior of the CLI is unverified until an approved manual run (see
[Desktop verification and limits](#desktop-verification-and-limits)).

The [spec](../specs/2026-11-03-wallpapers/00-overview.md) splits the port into
four approval-gated parts. This page materializes the shipped configuration,
sources, processing, runtime, host IPC and CLI behavior.

## Shipped

- `stache-wallpapers` at `src/crates/modules/wallpapers` owns the authoritative
  `WallpapersConfig` type (catalog ID and file `wallpapers` / `wallpapers.toml`),
  `WallpaperMode`, the schema, the commented template and the migration hook.
- The app catalog, generated schema and Specta bindings include the type and the
  `wallpapers.set` request, selection, screen, outcome, cause and per-target
  DTOs.
- `SourceContext`, `SourcePool`, `resolve_pool` and `resolve_explicit_file`
  resolve configured or explicit sources without decoding an image.
- The SDK ships a typed accepted-configuration watch with coherent reads and a
  bounded, cancellable revision wait. The production Wallpapers factory is its
  consumer; the watch is installed during registration and before start.
- A pure target selector, bounded source reader, image pipeline, validated
  cache and two-step apply library exist. `prepare_plan` prepares a
  caller-resolved mixed-source plan, and `apply_guarded` applies prepared
  outputs through the cancellable guarded native boundary.
- The runtime exists in `stache-wallpapers`: a host-lifetime
  `WallpapersService`, a `WallpapersModule` with one wakeable rotation timer,
  and a typed `wallpapers.set` handler with operation-outcome DTOs. The app's
  `WallpapersFactory` registers the module, installs the accepted watch and
  contributes the `wallpapers` route to the one host router.
- The `wallpaper set|list|generate-all` commands are contributed from
  `stache-wallpapers` (`src/cli/`) through the app command catalog; see
  [CLI](#cli).

## Runtime and host integration

`WallpapersService` is constructed once by `WallpapersFactory` and stays alive
across module stop/start. Construction is inert: no coordinator thread, session,
timer or native call. The factory receives the identity `AppPaths`, the
configuration directory, the injected home and the host main-thread executor,
builds `NativeDesktop` over that executor and creates the service with
`SourceContext`. The service receives a guarded desktop backend, an injected
processor plus clock and random sources. Production passes `RealProcessor`;
tests inject a processor that can block, fail or advance fake time while still
running the real pipeline. `install_accepted_watch` registers a pure wake-only
SDK subscription; a publication wakes queued admission and the live rotation
timer without polling.

The composition root in `src/crates/app/src/lib.rs` adds the factory to the
production list after proxyAudio, noTunes and Keep Awake. `WallpapersFactory`
registers through the shared `host::register_module_observed` helper, which
keeps the normal initial/default/rejected and bind path and installs the typed
watch from the returned `ModuleHandle<WallpapersConfig>` before the registry
starts modules. `ipc_handlers` contributes the `wallpapers` route; the router
merges it with every other factory handler before the socket starts. The
factory's shutdown hooks delegate to the service: `begin_shutdown` closes
admission and cancels every host wait, and `finish_shutdown` drains the
coordinator and reports whether it and any issued native call returned. A
blocked native call or coordinator therefore makes host teardown name
`factory:wallpapers` as unacknowledged rather than claiming completion.

- **Coordinator.** One lazy thread serializes manual and rotation jobs. Admission
  allows one executing job plus at most one queued job; a manual caller waits for
  the queue slot only until its absolute deadline. A full queue with no deadline
  returns the typed `queueFull` cause. All metadata locks are released before
  filesystem, processing or native work. A rotation tick never waits for a queue
  slot: it skips to the next bounded interval instead.
- **Manual requests.** `WallpapersIpc` implements the SDK `IpcHandler` with the
  `set` method, decodes `WallpapersSetParams` with `decode_params` and returns a
  `WallpapersSetOutcome` through `encode_response` inside the unchanged
  `Response::Ok` envelope. Outcomes carry a typed cause and each target carries
  a typed `WallpapersTargetFailure` plus display identity, so partial and
  unknown results are data rather than strings parsed as control flow. The server canonicalizes the requested configuration
  directory and refuses a different reachable identity before any work. The
  accepted watch is the only configuration source, so a disabled or paused
  module still applies an explicit file, collection entry or random draw using
  the last accepted pool and effects. A rejected initial file means no accepted
  configuration; defaults are never invented.
- **Selection.** `WallpaperSelection::File` is one literal absolute path,
  `Collection { index }` is a zero-based accepted-pool position, and `Random`
  draws independently for every addressed display. `WallpaperSelection::Name { name }` looks a token up on the accepted pool with
  one shared function (`find_pool_entry`) and then behaves exactly like
  `Collection` for that position; a miss is a typed `InvalidSelection`.
  `WallpaperScreen` mirrors the
  existing selector (`all`, `main` and one-based index with `0` as primary).
  Sequential state is keyed by the native display identity and the stable pool
  identity `(path, duplicate occurrence)`, so reorder keeps the cursor, removal
  starts at zero, and an explicit file resets the next sequential choice to
  zero. Random and random-all may repeat. Disconnected display state is pruned
  after each snapshot.
- **Rotation.** `WallpapersModule::start` reads the accepted pair, validates the
  pool without decoding, creates one session, enqueues the initial job first and
  returns without waiting for processing or native effects. The initial job is
  latched until every startup resource exists, so a positive-interval timer
  spawn failure rolls back without any native call. If the one bounded queue slot
  is already taken, start fails actionably with `Operation::Start` instead of
  silently dropping the initial job; the generic Retry starts again. Interval
  zero has one initial job and no timer; a positive interval creates exactly one
  wakeable timer in the module worker group. `Module.stop` invalidates only the
  session, its timer and queued rotation work; the coordinator and explicit
  action path stay available. An empty enabled pool fails start or a running
  reconfigure with `ErrorCategory::Config` and the generic Retry; an initial
  processing or native failure logs a typed failure and keeps the module Running
  with unchanged cursors.
- **Publication coherence.** A running reconfigure preflights the candidate pool
  and stages it, then waits for the registry to publish. A running enabled
  candidate with an empty usable pool is rejected before publication, preserving
  the old accepted config, pool, cursor and deadline. The timer adopts only a
  config value equal to the staged candidate and the actual published revision;
  it never assumes revision plus one. Rejection preserves the last-good config,
  pool, cursor, deadline and image. An interval change resets the deadline
  without applying an image; an unchanged interval preserves it; zero removes
  the timer without a sleeping generation, even while a noninterruptible
  processing job is still running. A zero-to-positive reconfigure starts the
  timer before acceptance but adopts nothing until the matching publication.
  A-B-A and same-config/new-revision publications are handled by reading the
  current cell. Stale in-flight work is rechecked before each native call; the
  cursor/deadline commit is tagged with the frozen accepted revision and a
  monotonic session epoch under the state lock, so a reload or stop between the
  completion check and the write cannot mutate successor state. Partial or
  unknown outcomes never count as a full-success timer reset.
- **Native serialization.** One `NativeOperationTracker` is shared by explicit
  and rotation application. A guarded call registers an account when admitted;
  it stays unresolved until the native step returns or the waiter abandons it
  before issuance, and stays owned until its closure returns. Before every
  native call the apply loop waits for `unresolved == 0` with the job's
  cancellation and deadline. A successor may be prepared while an earlier
  `Unknown` call is still running, but it cannot issue until that call finishes.
  A job abandoned before issuance releases the admission wait immediately while
  still owning its closure. The final admission check inside the issued job is
  the linearization point for native issuance; it does not claim atomic
  cancellation of an OS call that already began.
- **Budgets and shutdown.** `WALLPAPERS_CLIENT_BUDGET` is 120 seconds and
  `WALLPAPERS_SERVER_BUDGET` is 115 seconds inclusive of admission, queue wait,
  processing and native work, leaving five seconds for delivery. They are
  used by the CLI's `IpcClient::with_options` call; the budget values were not
  changed by the measurement below. `begin_shutdown` closes admission, cancels
  every host wait and queued job, wakes the coordinator and invalidates the
  session; `finish_shutdown` waits boundedly and reports unresolved
  noninterruptible work or an unacknowledged issued native closure instead of
  pretending it finished. The coordinator thread and service stay owned through
  `Module.stop` cleanup. Manual cancellation and deadlines are linked into the
  guarded acknowledgement wait, so shutdown wakes it without waiting for a poll
  interval.

### Cold-processing measurement

The manual release-mode measurement on November 5, 2026 on an Apple M1 Pro
(10 cores, 32 GB, macOS 27.0 build 26A428) prepared one deterministic
6000 × 4000 JPEG (coordinate gradient, quality 92) for a 2560 × 1440 RGB8 PNG
with `radius = 24` and `blur = 12`, using the shipped `prepare_plan` pipeline.
Every run used a fresh temporary identity root, so the cache started empty and
no native setter was called. The median of three runs was **724.36 ms** (runs:
723.82 ms, 724.36 ms, 742.98 ms). The harness is
`apply::tests::measure_cold_processing_6000x4000_to_2560x1440`; rerun it with
`cargo test -p stache-wallpapers --release measure_cold_processing -- --ignored
--nocapture`.

A sibling manual harness, `apply::tests::measure_cold_processing_multi_display`,
measures three displays (2560 × 1440 @1x, 3024 × 1964 pixels @2x and
5120 × 2880 pixels @2x), each with a distinct deterministic 6000 × 4000 JPEG
(the random-all worst case), `radius = 24`, `blur = 12`, a fresh cache per run
and no setter. Rerun it with
`cargo test -p stache-wallpapers --release measure_cold_processing_multi_display
-- --ignored --nocapture`.

The release run on November 6, 2026 used the same macOS 27.0 build 26A428
machine. The three-display median was **4,382 ms** (runs: 4,371 ms, 4,383 ms,
4,472 ms), about 3.8 % of the 115-second server budget. Rerunning the
single-display harness in the same session gave a median of 767 ms (runs:
760 ms, 767 ms, 769 ms). Outputs are prepared one after another, matching the
coordinator's `prepare_plan` order, so the cost grows roughly with total target
pixels. The budgets were not changed.

This evidence covers three synthetic target geometries with a fake desktop, one
local SSD and page-cache-warm source files. It does not measure physically
attached multi-display hardware, 8K targets, cold storage reads, macOS AppKit
latency or socket overhead. It supports the
orchestration budget as feasible for these normal workloads; it does not promise
a universal latency bound, and work outside the documented pixel and byte limits
is rejected rather than estimated.

Automated runtime tests use `FakeDesktop`, an injected processor, a manual
clock, a scripted random source and barrier backends. They cover interval-zero
and positive timers, sequential/random-all selection, duplicate and reordered
pools, explicit-file cursor reset, disabled/paused accepted updates, publication
barriers and A-B-A, interval/deadline changes, partial/unknown outcomes, queue
admission, stale publication, a late native call that serializes its successor,
startup latch rollback, blocked pause/disable and shutdown ownership, and a fake
cold job beyond five seconds under the 115-second server budget. No test calls a
real setter or sleeps for a real interval.

App integration tests use the same fakes and disposable paths. They prove the
factory registers a disabled module without a coordinator thread, exposes the
`wallpapers` route through the real host socket and held diagnostics, installs
the accepted watch so a reload is observed, applies one initial job with no
timer when an enabled interval-zero file starts, composes Wallpapers into the
production factory list, and reports `factory:wallpapers` unacknowledged while
a native call is blocked, then acknowledges after release.

## CLI

```text
stache wallpaper set [PATH] [--random|-r] [--screen|-s all|main|N]
stache wallpaper list
stache wallpaper generate-all
```

Help text and examples are the legacy ones. Grammar errors (including a bad
`--screen`) exit 2 with the legacy `Invalid screen value '…'. Expected 'all',
'main', or a positive integer.` text. `all` and `main` ignore case; `main`, `1`
and the legacy `0` all address `NSScreen.screens[0]`; other numbers are
one-based and a value that does not fit `u32` is a grammar error. An
out-of-range number is checked against the one native snapshot before any decode
or cache write.

Before any configuration, source, manager or socket work, `set` validates its
arguments: both PATH and `--random` print
`stache: Cannot specify both <path> and --random. Use one or the other.`; neither
prints `stache: Either <path> or --random must be specified.`; both exit 1. A
complete known success prints `Wallpaper set successfully.` and exits 0; any
other typed outcome exits 1 and never prints it.

### PATH resolution (client side, before sending)

The argument is a literal filesystem token: it is **never trimmed**, so a file
name with leading or trailing spaces works. Only `~` and `~/…` expand, from the
injected home. Absolute paths stay absolute; relative ones resolve from the
invocation working directory and are sent to the host absolute, never reinterpreted
by the host's CWD.

1. A _path-shaped_ token (absolute, `~`-anchored or containing `/`) must be an
   existing regular `jpg`/`jpeg`/`png`/`webp` file. Otherwise it is an argument
   error (`stache: Wallpaper error: Invalid wallpaper path "…": …`, exit 1)
   and there is no collection-name fallback.
2. A _bare_ token that is an existing supported regular file in the working
   directory is that file. An actual file wins over a collection entry with the
   same name (this intentionally widens the legacy basename-only lookup).
3. Any other bare token is a collection name, looked up case-insensitively
   against each accepted pool entry's file name or file stem **in pool order;
   the first matching entry wins** (the legacy per-entry precedence). Duplicate
   or ambiguous names are therefore resolved deterministically by pool order;
   there is no selector syntax to pick another. No match is a typed
   `invalid-selection` failure.

An explicit file never resolves the configured pool, so a bad or empty pool does
not block it. Name lookup and `--random` need a usable pool. Configured `path`
and `list` strings keep trim-before-tilde expansion and resolve against the
configuration directory.

### Routing

`set` sends one `wallpapers.set` (no separate `core.status` probe) to the host
for the selected configuration directory, using
`IpcClient::with_options(socket, RetryPolicy::legacy(), WALLPAPERS_CLIENT_BUDGET)`.
The server enforces the same-canonical-directory rule again on every request, so
a host restart between requests cannot be applied to the wrong configuration. The
host uses its accepted state, including last-good state after a rejected or
deleted file, and applies even while the module is disabled or paused without
changing lifecycle or timers.

- Only `ClientError::NotRunning` (proven absence, nothing sent) allows exactly one
  standalone application.
- Every other client error (timeout, broken pipe, reset, malformed or incomplete
  response, serialization after connect), an undecodable `Ok` payload and a
  `handler` wire error print the _response uncertainty_ message, exit 1, make
  zero local calls and never retry.
- The other wire error kinds (`malformedRequest`, `invalidParams`, `unknownModule`,
  `unknownMethod`, `disabled`, `unavailable`) are typed refusals before any
  native change.
- A live different-config host is refused with an actionable message and no
  fallback.

**Documented race.** While a desktop instance is starting and no socket exists
yet, `NotRunning` still permits one local set, and that host's normal initial
job may later override it. There is no startup coordination or job protocol. An
automated test models this ordering with fakes.

### Standalone application

The standalone path does not use `WallpapersService`: its coordinator dispatches
native work to the main queue, which nothing drains in a CLI process. It runs on
the process main thread: read the current `wallpapers.toml` (missing file means
defaults, a malformed one is an error), resolve the source, take one guarded
snapshot, range-check the screen, `prepare_plan` (processing workers join on
main) and `Prepared::apply_guarded` through `NativeDesktop` over `MainThread`,
which runs inline there. It uses the same processor, cache and effects as the
host and draws `--random` independently per display. It starts no registry,
module, tray, window, watcher, timer, socket listener or startup command.

### `list` and `generate-all`

Both are local, read the current configuration irrespective of `enabled`, never
touch the socket and never call a setter. `list` reads metadata only (no decode,
no native geometry) and prints a pretty JSON array, styled only on a terminal, or
`No wallpapers found.` for a valid empty pool; malformed configuration or
directory I/O fails with `stache: Wallpaper error: …` and exit 1.

`generate-all` prints exactly `No wallpapers to generate.` for a valid empty pool
without any native or processing work. Otherwise it takes one guarded native
snapshot and prepares every display × image through the bounded batch pipeline,
then prints the legacy header, per-screen groups (zero-based `Screen N:`
headings), `●` cache hit / `✓` generated / `✗` error rows and the
`Done! Generated/Cached/Errors (N.NNs)` summary. The spinner, cursor hiding and
every ANSI sequence are written only when stdout is a terminal. A completed batch
with per-image failures exits **0** and reports them. Top-level configuration,
setup, native-enumeration, cancellation and output-write failures exit 1; the
spinner stops, workers are joined and the cursor is restored. The production
cancellation scope reuses the existing `SignalBridge` (installed only while the
command runs), so the first Ctrl-C/SIGTERM/SIGHUP cancels cooperatively; codec
work in flight finishes first. A second signal force-exits with status 130 after
showing a hidden cursor on a terminal. The default handlers are not restored
afterwards because the process exits right after the command.

### Error adapters

New domain errors print `stache: Wallpaper error: ` and one body below, then a
newline, on stderr with exit 1. Values in braces are serde JSON strings (control
characters, including ESC, become `\uXXXX`); target lists are compact JSON arrays in
operation order. The adapters render typed data only; nothing parses them.

| Condition                                       | Body                                                                                                                                                             |
| ----------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Different config                                | `Running host uses config directory {host}; requested {selected}. Use the same config directory or stop that host.`                                              |
| No accepted config                              | `No accepted Wallpapers configuration is available. Fix wallpapers.toml and reload, then retry.`                                                                 |
| Queue full                                      | `Wallpaper queue is full; no native change was issued. Retry after current operations finish.`                                                                   |
| Deadline before native issuance                 | `Wallpaper deadline expired before native application; no native change was issued. Retry when ready.`                                                           |
| Cancelled before native issuance                | `Wallpaper operation cancelled before native application; no native change was issued.`                                                                          |
| Response/transport uncertainty                  | `Wallpaper outcome is unknown after a send or response failure. Check the desktop before retrying; no automatic retry was attempted.`                            |
| Partial/failure/cancelled/uncertain after issue | `Wallpaper application {outcome}: applied={…}; failed={…}; not-attempted={…}; unknown={…}. Check the desktop before retrying; no automatic retry was attempted.` |
| Other typed pre-native failure                  | `Wallpaper operation failed before native application ({category}): {detail}. No native change was issued.`                                                      |

`{category}` is the outcome cause tag (`source`, `invalid-selection`,
`processing`, `native`, `unavailable`) or, for a wire error, its kind tag. The
application body is used when any target applied, failed or has an unknown result;
an `uncertain` outcome that names no target uses the uncertainty body rather than
inventing empty identity lists. The DTO has no optional OS-cause field, so no
` Detail:` suffix is ever produced. A timeout after a send is always the
uncertainty body, never the zero-effect deadline body.

## Processing and cache contract

`prepare(targets, source, effects, deps, cancel)` snapshots the displays once,
resolves the selector and prepares every output; the returned `Prepared` owns
the lease and applies the outputs. `prepare_plan(plan, deps, cancel)` takes a
caller-snapshotted plan of one `SelectedTarget` and one source path per display,
with per-target effects. It validates every display, target and effect before
any read or cache write, refuses an empty plan and duplicate display
identities, and prepares every output before returning. Adjacent targets that
share a source reuse that source's bounded bytes and decode; a different source
releases the previous decode before it is read. `prepare_batch` does the same
preparation for many sources with bounded workers. All are ordinary blocking
functions; none spawns a native hop or a timer.

- **Selection.** `ScreenTarget` is `all`, `main` or a 1-based numeric index;
  `0` keeps the legacy primary alias. `main` is entry 0 of the snapshot, never
  the focus-dependent `mainScreen`. An invalid selector, an empty screen list,
  an out-of-range index or non-finite/non-positive/overflowing geometry fails
  with `SelectionError` before any decode, read or cache write. No fallback
  display or resolution is invented.
- **Target pixels.** The point frame is multiplied by the backing scale,
  truncated like the legacy adapter, and checked against per-display limits, so
  a 1× and a 2× display with the same points get different exact outputs.
- **Read once.** The canonical source is checked without following its final
  link, opened read-only, read into one bounded buffer, hashed and decoded from
  that same buffer. Device, inode, length and modification time are compared on
  the descriptor and the path before and after the read; a replacement is a
  typed `SourceChanged` error. Sources are never written, renamed or removed.
- **Order.** Decode, cover resize with Lanczos3, center crop to the exact
  pixels, blur, black rounded corners, then RGB8 PNG with Best compression and
  adaptive filtering. Zero effects skip their stages. JPEG, PNG and WebP decode
  from content; a JPEG under a `.jpg` name is accepted, and WebP decoding is an
  intentional improvement over the legacy processor.
- **Effects.** Blur at most 5 uses a true Gaussian; above 5 uses the
  native-resolution fast-blur approximation. Corners cap the radius at half the
  smaller dimension and use 8×8 supersampling. The blur is bounded before any
  work.
- **Cache key.** SHA-256 over a length-prefixed encoding of the canonical source
  path, the digest of the exact source bytes, the target width and height, the
  radius, the blur and `PROCESSING_VERSION`. The file is
  `<AppPaths cache>/wallpapers/<hex>.png`. A different directory with the same
  basename gets a different key; changed bytes invalidate even with an unchanged
  length and modification time.
- **Validation.** A hit is used only when it is an owned regular file (no
  symlink) that fully decodes as RGB8 PNG with the expected dimensions. An
  owned corrupt or truncated file is rebuilt. A symlinked or foreign entry, a
  symlinked `wallpapers` directory or an unsafe cache root is refused.
- **Publication.** A completed output is written to a uniquely named
  `create_new` temporary in the same directory, flushed, synced, closed,
  reopened and validated, then renamed over the final name atomically. A final
  commit check revalidates cancellation and the held lease after validation and
  immediately before the rename, so an encoder that outlived cancellation or a
  replaced lease cannot publish obsolete output. Failed or cancelled work
  removes only its own temporary. Concurrent equivalent producers may each
  publish; the atomic rename leaves one reusable winner. Batch publication uses
  the same check.
- **Lease scope.** One shared `CacheLease` is held from before the first cache
  read through native acknowledgement of the last set call; `Prepared` owns it.
  The lease is verified immediately before cache publication and before the
  first native call. It is never upgraded, and no native or registry lock is
  held while waiting. A five-second bounded, cancellable wait reports typed
  `Busy` or `Cancelled`.
- **Apply.** Every output is prepared and validated before the first
  `set_image`. Apply is sequential and stops after the first failure or unknown
  outcome. `ApplyOutcome` reports `applied`, `failed` (known errors), `unknown`
  (`DesktopError::Unknown`) and `not_attempted` identities; `is_complete()` is
  true only when everything applied. There is no partial-success flag and no
  rollback promise. Cancellation and the `still_current` closure can only
  suppress future calls. Already-issued calls may finish but cannot start
  another change.
- **Guarded apply.** `Prepared::apply_guarded` builds one `DesktopOperation` per
  target from the caller's cancellation token, absolute monotonic deadline,
  injected `DesktopClock` and pure current-generation/revision predicate, and
  attaches the prepared cache lease as its keepalive. A cancelled, expired or
  stale refusal is not-issued and becomes `not_attempted`; an issued call that
  races cancellation may report `unknown`. The guard is checked before the job
  is enqueued and again on the main thread just before the native call, after
  the display geometry recheck. Cancellation and expiry wake the
  acknowledgement wait through the token listener and a condition variable.
  Cancelling never interrupts an issued OS call; the job retains the keepalive
  lease until that call finishes. The Part 02 `apply` path remains for callers
  that do not need the guarded boundary.

Internal limits are not configuration:

| Bound          | Value                           | Rationale                                                       |
| -------------- | ------------------------------- | --------------------------------------------------------------- |
| Source bytes   | 128 MiB                         | Large camera exports and panoramas; one bounded buffer per job. |
| Decoded source | 100 MP, 65,536 px edge          | 8K is 33 MP; a 20,000 × 4,000 panorama (80 MP) fits.            |
| Target         | 100 MP, 16,384 px edge          | Above 8K on either axis with headroom.                          |
| Cover canvas   | 100 MP, 65,536 px edge          | Rejects an exploding intermediate before allocating it.         |
| Blur           | 4,096 px                        | Beyond this no supported panel distinguishes the effect.        |
| Workers        | `min(available_parallelism, 4)` | Bounded batch concurrency.                                      |

`prepare_batch` pulls work from the caller's iterator through a bounded channel,
so `screens × images` is never materialized as a queue. The first failure stops
the producer from pulling more work; `generate-all` therefore reruns the
unresolved items after each failure, so every display and image ends as an
output or a typed failure. Each `BatchOutput` reports whether it was a cache hit. Cancellation is observed at queue, decode,
resize, effect, encode and publication boundaries. Codec calls themselves are
noninterruptible; the code and this page say so rather than claiming otherwise.

## Desktop verification and limits

Stache does not support macOS Spaces. Native Space coverage is not functionality,
future work or a verification gate. Tiling's app-level virtual workspaces are
unrelated to Wallpapers.

An explicitly approved standalone main-thread probe on November 5, 2026 called
`NativeDesktop::set_image` for the one attached display (id `3`, 2560×1440
points/pixels, scale `1`) with a generated RGB8 magenta PNG in a disposable
temporary directory. It returned `Ok` in about 1.75 ms; the user visually
confirmed solid magenta. Immediate `NSWorkspace.desktopImageURLForScreen`
readback still showed the old URL, so it is not reliable evidence of visual
completion. After deletion of only the probe image, the user confirmed magenta
remained visible. This simulated cache-file loss; it was not `cache clear` and
touched no real cache. Restoration using the recorded original URL and exact
AppKit options returned `Ok`; immediate readback still showed the probe URL,
but the user visually confirmed the original restored. The probe exited `0`.

This observation does not prove actual cache-clear integration, long-term
persistence, behavior across logout/restart, physical hotplug or multi-display
behavior, or perfect restoration of dynamic wallpaper. Assigning an OS wallpaper
URL does not make its file permanent; OS persistence after removal remains
system-dependent. The library makes no automatic restore or recache promise.
Further real desktop changes or restoration require explicit each-run approval.

## Configuration

```toml
#:schema https://raw.githubusercontent.com/marcosmoura/stache/main/schemas/wallpapers.schema.json
enabled = false
path = ""
list = []
interval = 0
mode = "random"
radius = 0
blur = 0
```

Defaults stay disabled with no sources. `interval` is seconds (0 to 9223372036854775807, the
largest TOML integer) and `radius`/`blur` are `u32` pixels; `mode` is `random` or `sequential`. Unknown keys are warnings,
never rejections. An empty source list is valid configuration.

`validate` is structural only: it rejects a NUL byte in `path` or a list entry
and never reads the filesystem. Structural failures from parsing and typing stay
deserialization diagnostics; a missing directory, unreadable directory or empty
usable pool is a source-resolution failure instead.

## Source resolution

The shared resolver implements these rules; runtime and CLI parts reuse it.

- A nonempty `path` replaces `list` completely. An invalid directory, including
  a whitespace-only `path`, is an error and never falls back to the list.
- Configuration strings for `path` and each `list` entry are trimmed. `~` and
  `~/…` expand from the injected home directory; absolute paths stay absolute;
  any other path resolves under the directory containing `wallpapers.toml`,
  never the process working directory. No shell or environment expansion runs.
- A directory listing is nonrecursive, regular files only, with
  case-insensitive `jpg`, `jpeg`, `png` and `webp` extensions. It is naturally
  ordered (`2` before `10`) with a deterministic path tie-break.
- A list keeps its order and duplicates. Missing, non-regular and unsupported
  entries, and `~` entries with no injected home, are filtered; a mixed or
  entirely invalid list is not a hard failure.
  An empty resulting pool is reported as `EmptyPool` when a caller needs at
  least one image, while `list` can report the empty result successfully.
- `resolve_explicit_file` is independent of the configured pool. Its argument is
  literal: it is not trimmed or tilde-expanded, a relative argument resolves from
  the invocation CWD, and the file must exist, be a regular file and have a
  supported extension. This lets a later `list`, `generate-all` or `set` use an
  actual explicit file even while the configured pool is empty or unusable.

Errors distinguish `Directory`/`MissingHome` source failures, `InvalidArgument`
for an empty explicit argument, `File` for an unusable explicit file and
`EmptyPool`.

## Migration

`config migrate` decodes the legacy `wallpapers` section through the catalog
type rather than copying it verbatim. The section is validated before any file
is written and the disabled default template is emitted when it is absent; it is
never enabled implicitly.

Absent, absolute and home-anchored sources keep their meaning. A bare relative
`path` or list entry is preserved as text with a warning: the new file resolves
it under the destination configuration directory, which may differ from the
legacy working directory. Migration does not claim the old meaning can be
inferred.

## Accepted configuration watch

`Registry::accepted_watch(handle)` returns `AcceptedWatch<C>` for the module's
registered configuration type. `read()` returns the last accepted
`AcceptedConfig { revision, config }` pair or `None`; the pair comes from one
lock, so a revision and its configuration are always coherent. It takes no
module, lifecycle or reload lock and never reads disk.

`wait_for_change(seen_revision, timeout, &CancellationToken)` returns `Changed`,
`TimedOut`, `Cancelled` or `Closed`. An absent configuration is revision zero.
The check and the sleep share the cell lock, so publication cannot be missed
between them, and spurious wakes reread the authoritative pair. Precedence is
cancellation, close, change, timeout. A timeout whose deadline overflows
`Instant` waits without a deadline instead of panicking or spinning. Registry
invalidation, including shutdown and teardown, closes the cell and wakes every
waiter; the last pair stays readable.

Publication happens only on acceptance: registration with a valid configuration
(revision `1`), and a change committed by a reload. Rejected validation, a
failed reconfigure, a failed pre-commit quiesce, deleted files and unchanged
reloads publish nothing. A configuration committed before a later start or stop
failure is visible, and the old pair stays readable throughout `reconfigure`.
See [config.md](../architecture/config.md#accepted-configuration) and
[sdk.md](../architecture/sdk.md#accepted-configuration).

## Verification

Automated CLI tests use fakes (`FakeDesktop`, a fake transport, an injected clock
and random source) and disposable roots; they cover the command tree and help,
grammar versus both/neither exits, token precedence, routing and every adapter
body, static and terminal goldens for `generate-all`, and standalone behavior.
Real-socket tests run the CLI executor against a real `IpcServer` and
`WallpapersService` with `FakeDesktop`: disabled and paused hosts, a different
directory and a symlink alias, no accepted configuration fixed without a
restart, a Name resolved against the last-good accepted pool, partial and
uncertain outcomes, a host restart on the same socket, a raw fake peer that drops,
truncates or corrupts the response (one connection, zero local apply), and a
request that expires while queued under a manual clock (zero native issuance).
`QueueFull` is not reachable over the socket (the handler always passes a finite
deadline, so a full queue ends as the deadline cause) and a deadline expiring
after native issuance is covered by the Part 03 service tests, not re-run through
the socket. The wire path itself never sleeps for real time.

No real wallpaper setter, desktop session or macOS multi-display behavior is
exercised by the CLI tests. That remains the authorized manual verification gap.

Automated tests decode real in-test JPEG, PNG and WebP fixtures and use
temporary configuration/home/cache paths. They use `FakeDesktop` except for the
harness-less main-thread proof, which reads the real `NSScreen` snapshot and,
under the `testing` feature, runs the real resolve/recheck half of `set_image`
against absent identities and simulated expected width/height/scale mismatches.
No automated test calls the real setter, changes the desktop or runs the legacy
application. Checks measure a cap of four workers and cross-process busy
results during write, sync, publish and batch-processing pauses, as well as
lease contention through `Prepared` and cache-hit reads. Guarded native tests
use an injected clock, a manual main-thread executor and a native-step double;
they prove zero native sends for queued cancellation, expiry and staleness,
`Unknown` plus keepalive retention for an already-issued call, and that a
cancel or replaced lease observed at the final publication point leaves no
output and no temporary.
