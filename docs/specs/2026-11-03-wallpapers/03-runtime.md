---
status: done
---

# 03 — Runtime and host coordination

Read when implementing planned lifecycle and host application. Depends on Parts 01–02.

## Scope

Implement the new module lifecycle/coordinator and typed `wallpapers.set`
handler in `src/crates/modules/wallpapers/src/`. Add its production factory in
`src/crates/app/src/host.rs`, registration through `host::register_module`, and
shared Rust DTO collection in `bindings.rs`. No mutating frontend endpoint,
window or independent socket. CLI command contribution is Part 04.
Generic factory cancellation/teardown plumbing belongs in app `host.rs` and its
host tests; accepted-state watch/wake support belongs in SDK `registry.rs` and
`threading/` only as needed. Update `docs/architecture/{sdk,ipc}.md` when shipped.
If the timing checkpoint below requires it, narrowly extend generic bounded
request/method budgets in `src/crates/sdk/src/ipc/{client,server,router}.rs` and
document them in `docs/architecture/ipc.md`; do not add tool policy to transport.

## Requirements

- Descriptor ID `wallpapers`; no dependency on other runtime tools, executable,
  window surface, Automation or Accessibility requirement. App owns registry,
  config watcher, socket and tray; module owns wallpaper policy and rotation.
  Disabled modules hold no rotation resources. Explicit action capability is
  independent of a live rotation worker, without a second lifecycle.
- Read the Part 01 accepted config/revision source for host commands, including
  disabled/paused staged updates and last-good config after disk rejection or
  deletion. Do not use `ConfigStore::initial`, current disk, defaults after
  malformed initial input, or the last value seen by `configure`. No snapshot
  mutex/registry map lock across decoding, native work or joins; respect the
  SDK's unavoidable lifecycle serialization rather than expanding it.
- Start validates/builds the source pool, establishes owned coordinator/session
  and exactly one wakeable timer if interval > 0, then enqueues initial selection
  as the FIRST job and returns Running without waiting for image processing or
  native effects. Interval 0 has one initial job, no timer or automatic retry.
  Source validation can still fail start; filesystem I/O is not promised instant.
  Lifecycle startup performs no decoder work on the host or main thread.
  Initial processing/native failure logs a typed error and keeps Running, with
  unapplied targets' cursors unchanged. Retry is the next normal tick for a
  positive interval, a later explicit set or an actual pause/resume start, not a busy loop or
  new asynchronous Failed-state SDK API. Running does not guarantee an applied
  image. Debug and release follow the same policy. Failed start cleans all
  resources and supports generic Retry without restart. An enabled empty usable
  pool yields `ModuleStatus::Failed` with an actionable `ModuleError` classified
  as `Operation::Start` / `ErrorCategory::Config`, exposing the existing generic
  Retry. Shipped `UnavailableReason` only covers missing services/executables;
  do not invent a missing-image/source variant. Log/report processing/native
  failure truthfully, distinct from lifecycle readiness. Disabled explicit files remain independent of pool usability;
  empty random and list results are separate from structural config errors.
- Running reconfigure preflights source I/O and scheduling, keeping candidate
  settings/pool/selection/schedule staged through the window before SDK publishes
  accepted config/revision. Operations may use the last accepted coherent state
  or wait boundedly; never combine old accepted config with candidate effects,
  pool, cursor or deadline. Adopt corresponding settings only upon matching
  accepted publication, without ad-hoc globals, disk rereads or invented lifecycle
  commit hooks. Use Part 01's same-cell typed watch/read handle: stage before
  publication, then match typed config equality plus actual published revision,
  never revision + 1. Publication wakes the coordinator/timer after the pair is
  visible; stale/spurious/coalesced wakes reread it. No missed check/wait wake,
  polling or sleeping until the old deadline. This notification is the narrow
  justified SDK exception, not a broader lifecycle framework.
  Rejection preserves last-good accepted config, actual pool, cursor, schedule
  and visible image. Accepted path/list/mode/effects updates
  stage for the next scheduled or explicit change, never apply on save.
  Preserve next deadline unless interval changes; interval change resets the
  deadline without applying an image. Zero removes the timer. No extra timer
  generations sleeping until old deadlines.
- Paused/disabled reloads use SDK accepted staging, not private module caching.
  Resume/enable is an actual start using current accepted settings and asynchronous initial
  policy. Pause/disable/stop cancels rotation, wakes/joins session-owned workers,
  clears queued rotation work and leaves the current desktop unchanged. The
  factory-owned explicit-action service remains available. No restore.
- One factory-created host-lifetime apply/processing/native service, separate
  from the rotation runtime WorkerGroup, owns the bounded coordinator used even
  by disabled/paused explicit requests. It is not borrowed from a stopped
  ModuleContext. Module.stop cancels the rotation/session epoch and queued
  rotation jobs, not future explicit requests; session invalidation prevents
  obsolete jobs from updating successor state. Host cancellation covers ALL
  manual jobs, including disabled/paused ones. Constructor/default-disabled
  factory registration creates only inert coordinator state. Processing workers
  and capacity are lazy, on request or active rotation; default-disabled
  registration creates no permanent pool or timer.
- Choose narrow generic app `ModuleFactory` begin-shutdown cancellation and
  drain/teardown hooks, default no-op for existing factories; reuse/extend the
  previously landed generic context instead of Wallpapers-specific host logic.
  Use this deterministic two-phase host shutdown order, including partial-startup
  cleanup: begin-shutdown closes admission and cancels/wakes ALL host-service and
  request waits (accepted-config watch, coordinator queue and main-dispatch
  acknowledgment included), BEFORE `IpcServer::shutdown` closes ingress and joins
  connection handlers. Then registry reverse-order shutdown calls Module.stop to
  cancel/join rotation contexts and timers; only afterward factory finish/drain/
  join completes and services drop. The factory service/coordinator/native owner
  stays alive THROUGH Module.stop cleanup; never join/drop it first while a
  module could wait on it. Config-watcher and other startup cleanup must respect
  this order, with cancellation/wakes before any dependent join and no cyclic
  joins. Keep teardown idempotent. The current handler has no cancellation input;
  waiting for Module.stop alone would deadlock a disabled manual request. Retain
  safe ownership through noninterruptible codec/FS work, never abandon it; do not
  promise forceful interruption or a universal bounded join. No new job protocol,
  unbounded pool or tool-specific host branch.
- The one coordinator serializes manual and scheduled requests with bounded
  admission/backpressure. Freeze a coherent accepted config/revision plus its
  prepared-pool identity, selection and display snapshot for each operation.
  Source-pool validation, processing and all-output
  preparation precede native calls. Revalidate ownership/generation/revision
  before side effects and before state publication; accepted reload/stop wins
  over stale queued work. A cancelled/obsolete request cannot change successor
  cursors/deadlines or trigger more native calls, even if an issued OS call
  finishes. No main-thread wait on worker that waits for main.
- Track successful selection per stable native display identity, not positional
  screen index or an unrelated random draw. Sequential starts at pool position
  zero and advances in pool order per display, wrapping. Random may repeat;
  random-all independently draws for each display. Initial random selection
  uses the same per-display policy. Successful explicit collection selection
  updates only applied displays' state; an external explicit file is represented
  honestly, with next sequential choice starting at zero. Preserve selected pool
  entries across reorder/reload where identifiable (including duplicate
  occurrence); if removed, next sequential choice deterministically starts at
  zero. New display starts at zero/random; remove disconnected display state.
- Successful explicit apply resets the next interval from completion only when
  rotation is running. Paused/disabled explicit set never enables/resumes or
  creates a timer. Partial failure records only actual applied targets; no
  full-success timer reset. Timer failures are logged with typed categories and
  continue at a bounded normal next interval, without busy retries.

### IPC boundary

- Register only the needed typed `wallpapers.set` surface on the existing router.
  Params distinguish absolute explicit file, collection selection and random,
  screen selector, plus selected canonical config-directory precondition. Share
  DTOs with CLI/Rust binding generation; do not hand-build JSON or TypeScript.
- Compare selected directory against host identity on the server before any
  work/mutation. Canonicalize existing ancestors plus absent suffixes and symlink
  aliases consistently; a client probe alone is not sufficient across host
  restart. A reachable different-config host refuses with an actionable error;
  no local fallback. Keep policy out of generic host transport code.
- Malformed params/routing errors use existing wire errors. Handled domain
  outcomes use a module-owned typed operation-outcome DTO inside `Response::Ok`
  data: full success, partial, failure, cancelled or uncertain, with typed cause
  and known applied/failed/not-attempted/unknown target identities. Transport ok
  means handled, NOT successful wallpaper application. Do not extend SDK
  WireError/HandlerError payloads (currently kind + message), encode JSON in an
  error string or parse strings as control flow. Different config, unavailable
  accepted config, source/selector/processing failures remain distinguishable;
  disabled itself is not an error. Design/compile-prove precise schema here and
  share serde/Rust DTOs. CLI prints success only for full known success; other
  domain outcomes exit 1 via Part 04's frozen adapters. Unknown already-issued
  effects are never reported as not attempted, nor automatically retried.

### Finite request-budget checkpoint

- Before Part 04 uses host routing, compile-prove the complete handler/client
  path and demonstrate with fake time that a legitimate cold multi-display job
  taking longer than five seconds can complete. The client currently defaults
  to five-second response/write timeouts; server five-second socket I/O limits
  do not impose a handler-execution deadline (`handle_connection` calls the
  router synchronously). Merely bounding completion “relative to transport” is
  not a feasible cold-cache contract.
- Choose and document a finite processing-plus-response budget from Part 02's
  concrete source-byte/pixel/allocation bounds, target-display count, maximum
  admitted/queued work and measured cold resize/effects/Best-PNG evidence. State
  reference hardware/workloads and supported normal resource envelope; document
  rejection/timeout outside it, not an unsupported universal numeric guarantee.
  Fake timing proves orchestration, not codec performance; retain implementation
  measurements as feasibility evidence before enabling the Part 04 route.
- Prefer shipped `IpcClient::with_options` when sufficient. If five seconds is
  insufficient, allow only a narrow generic bounded per-request/method budget
  extension, including server plumbing only as necessary. Preserve all built-in
  defaults, 64 KiB request / 1 MiB response bounds, eight-connection cap and
  absent-only retries. No infinite/global blanket increase, background-job
  protocol, new CLI flags/commands or service framework.
- Define coherently related client response wait and server/coordinator deadline:
  include queue/admission time and reserve bounded response delivery time before
  the client wait expires. Document budget origins and transport overhead so a
  server-local deadline does not silently outlive the client's corresponding
  wait. Expiration invalidates queued dispatch and is checked again before native
  commit: expired work cannot begin a native mutation or issue another one.
- Timeout/cancellation must retain safe ownership until work is joined and owned
  temporary artifacts/leases are cleaned. Noninterruptible codec/filesystem/OS
  work cannot be promised instantaneous cancellation or universal bounded join;
  document residual cleanup latency and keep it owned, never detach it and claim
  completion. Already-issued OS side effects may remain uncertain after timeout;
  report applied/unknown outcomes honestly, suppress successor-state updates and
  never permit standalone fallback. Shutdown must not deadlock or abandon workers.

## Acceptance criteria

- [x] WP03-01: Production registration/start/stop uses the SDK path and requires no other
      tool; defaults start no rotation or native application.
- [x] WP03-02: Start/enable/resume, publication-barrier staging, interval-only changes
      and rejected source reloads have the exact image/resource behavior above;
      empty enabled rotation has the shipped typed failed-start/Retry mapping.
- [x] WP03-03: Exactly one wakeable timer and one serialized apply path survive repeated
      reload/pause/resume/stop; stale jobs are inert at native/state boundaries.
- [x] WP03-04: Host set works disabled/paused using accepted staged config, including
      malformed initial file fixed later and rejected/deleted reloads.
- [x] WP03-05: Server-enforced identity, partial DTOs and finite cold-request budget
      pass compile/fake-time proofs with implementation feasibility evidence;
      built-in limits/defaults are unchanged. Factory/types/docs and overview gates pass.

## Tests

Injected clock/random/geometry/native/processor services, not wall-clock sleeps
or real desktop calls. Test initial sequential/random-all, repeats, wrap, subset
selection, external file then sequential, duplicate/reordered/removed pool and
display identities. Assert precise timer/worker counts, interval zero, deadline
reset only after full explicit success, rejected reload retaining old deadline,
and multi-reload not multiplying workers.

Barrier-controlled tests cover pause/stop during decode, all-prepared dispatch,
queued native call invalidation, already-issued native completion, hotplug,
concurrent manual/timer/reload, partial native failure, failed worker startup,
shutdown and Retry. Assert no successor config/cursor/deadline mutation.
Block a cold initial job: unrelated module startup, pause, disable and shutdown
must proceed; cancel before its initial native commit and prove no setter call.
Assert initial failure stays Running, leaves unapplied cursors unchanged, retries
only at the normal tick/action, and interval zero creates no retry loop. Host-main
decoder assertions prove ownership, not a performance guarantee; off-main startup
does not change the literal tray-created startup measurement budget.
Block a fake cold manual job while disabled; separately block a rotation worker
on an old long interval/accepted-config watch. Race each with host shutdown:
begin-shutdown must wake all waits BEFORE connection joins, including queue and
pending main dispatch. Assert the exact order: begin-shutdown, IPC close/join,
registry reverse-stop/context/timer joins, then factory finish/drain/join/drop;
service/coordinator/native ownership survives every Module.stop. Prove no cyclic
joins or abandoned noninterruptible work. Test partial factory startup, watcher
cleanup, repeated shutdown and proxyAudio's default-hook behavior. Assert inert
default-disabled registration creates no permanent processing pool or timer;
workers/capacity appear only on request or active rotation.
SDK-integrated tests cover disabled/paused snapshots, staged effects, current
invalid/deleted disk, initially malformed then fixed, same-dir aliases/absent
directories, different-dir refusal and server restart between probe and set.

Hold a deterministic barrier between successful reconfigure preflight and SDK
accepted publication. Concurrent manual/timer operations use the old coherent
config/revision/pool/cursor/deadline or wait boundedly; after acceptance they adopt
only the matching new state, and rejection preserves all old state. Complete an
old in-flight operation after publication and prove it cannot mutate successor
state. Include typed accepted-snapshot consistency tests from Part 01 and empty
pool failed-start/Retry followed by source repair, with disabled explicit-file
success and separate empty-list/random assertions.
Include publication between read/wait, rapid/coalesced A–B–A, same-config/different-
revision coherence, initial disabled/paused publication and bounded cancel/stop
wake under an old long interval. Roundtrip actual router/client serde for every
operation outcome, including partial and unknown target identities, without
SDK wire-error payload changes.

Use fake time (no long sleeps) for a valid cold job beyond five seconds within
the chosen budget, expiry while queued, before native commit and after native
issuance. Assert no post-expiry native admission, no duplicate fallback, truthful
uncertainty, joined owned cleanup and bounded capacity under noninterruptible
fake work. SDK transport regression tests prove built-in five-second defaults,
wire limits, connection cap and absent-only retry behavior remain unchanged.

## Notes

Implementation adaptations: the factory lives in `src/crates/app/src/wallpapers.rs`
and registers through `host::register_module_observed`, which `register_module`
now delegates to. The shutdown order follows the coordinated teardown that landed
after this part was written: factory begin hooks run in stage 1, modules stop and
factories drain in the registry stage, and the socket slot closes afterward.
Admission is already closed by then, so a late handler returns a cancelled outcome.

Explicit operations may own short-lived processing/native work while disabled;
that does not mean a module runtime was started. Do not weaken generic SDK
disabled/paused rules to make this tool's command path convenient.
