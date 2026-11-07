# Module SDK

Read when implementing or reviewing lifecycle ownership, threads, events or errors in `stache-sdk`.

`stache-sdk` is the contract every module uses. It contains no Tauri types, no
native adapters and no global state: a host creates one `Registry`, injects its
logger, event bus, main-thread executor, permission provider and window backend,
and registers modules. Descriptors are plain `'static` data, so the host can
inspect a module before it starts.

## Lifecycle

Registration erases the concrete configuration type once inside the registry;
the returned `ModuleHandle<M::Config>` keeps it at the call site. `reload` accepts
only that configuration type, so a wrong-type candidate is a compile error. It
returns `ReloadReport { status, start_error }`; `status` is
`ReloadStatus::Applied { revision }` or `Unchanged`, so callers never infer the
outcome from snapshots. A start failure after an accepted reload is reported in
`start_error` while the configuration stays committed. `register_rejected`
registers a module with no accepted configuration plus a diagnostic; a later
valid reload commits revision `1` and, once `start_all` has run, starts it. The internal downcast is an invariant
and reports a `Lifecycle` error rather than panicking. `configure` prepares state, `start` acquires owned resources,
`reconfigure` replaces them transactionally and `stop` releases them. A module
cleans up its own resources when any call fails; the registry cancels, stops and
joins the workers it owns.

| Event                               | Enabled | Paused  | Resulting status                                   |
| ----------------------------------- | ------- | ------- | -------------------------------------------------- |
| Register, config valid              | `true`  | no      | `Stopped`                                          |
| Register, config valid              | `false` | no      | `Disabled`                                         |
| Register, config invalid            | either  | no      | `Stopped` plus `ConfigDiagnostic`, revision `0`    |
| Start with every prerequisite       | `true`  | no      | `Running`                                          |
| Start, permission absent            | `true`  | no      | `WaitingForPermission(permission)`                 |
| Start, service or executable absent | `true`  | no      | `Unavailable(reason)`                              |
| `set_enabled(false)`                | `false` | any     | `Disabled`, resources stopped                      |
| `set_enabled(true)` from disabled   | `true`  | cleared | Start is attempted                                 |
| `pause`                             | any     | `true`  | `Paused` (resources stopped) or `Disabled`         |
| `resume`                            | any     | `false` | Start is attempted, or stays `Disabled`            |
| Reload while running, still enabled | any     | no      | `reconfigure`; rejection keeps `Running`           |
| Reload while running, now disabled  | `false` | no      | Stops directly to `Disabled`                       |
| Reload while paused                 | any     | `true`  | Snapshot/revision only; status keeps `Paused`      |
| Reload enabling before `start_all`  | `true`  | no      | Snapshot only; status `Stopped`                    |
| Reload enabling after `start_all`   | `true`  | no      | Start attempted from `Stopped`/`Disabled`/`Failed` |
| Reload while blocked                | `true`  | no      | Keeps `WaitingForPermission`/`Unavailable`         |
| Reload newly needing a missing perm | `true`  | no      | Accepted; stops, then `WaitingForPermission`       |
| Reload dropping the waited perm     | `true`  | no      | Leaves the wait and starts                         |
| Permission granted                  | `true`  | no      | Waiting modules start                              |
| Permission denied                   | `true`  | no      | Declaring modules stop, then wait                  |
| `service_failed`                    | `true`  | no      | Transitive dependents become `Unavailable`         |
| `service_recovered`                 | `true`  | no      | Unavailable dependents are retried                 |
| `shutdown`                          | any     | any     | Resources stopped; later starts are refused        |

### Permission requirements and prompt policy

`ModuleDescriptor::permissions` stays the static list. A module may also override
`Module::required_permissions(&self, &Config) -> Vec<PermissionRequirement>` to
compute its needs from a typed configuration; the default maps the descriptor list
to `PermissionRequirement::automatic`, so existing modules are unchanged. The
registry stores the result for the accepted configuration (at registration and on
every committed reload), uses it for the fresh nonprompting `status` check on
start, for revocation (`on_permission_change` stops only running modules whose
accepted requirements include the permission) and exposes the merged set of
enabled, non-paused modules through `Registry::permission_requirements()`. The
function must be pure and cheap: it runs under the module lock and never touches
the platform.

A reload of a running module whose candidate needs a permission that is not
granted does not call `reconfigure`: the candidate is accepted (revision
published), the whole runtime stops and the module reports
`WaitingForPermission`. A grant starts it with the accepted configuration. A
reload of a waiting module that no longer requires the waited permission releases
the wait and starts. `Module::reconfigure` keeps its rejection guarantee because
it is only called when its requirements are satisfied. A trusted start that fails
is `Failed`, never `WaitingForPermission`; nothing caches a grant and there is no
retry loop beyond the host's poll.

`PromptPolicy` is `Automatic` (the host may request once per session on startup,
enable, reload, tick or activation) or `UserOnly` (status checks and polling only;
`request` happens solely on the explicit tray path, `HostHandle::request_permission`).
**Every shared-input consumer must declare `PermissionRequirement::shared_input`**
(user-only), including Keybindings, Hold-to-Quit and MenuAnywhere (the MenuAnywhere
runtime consumes it unchanged, with no module-specific prompt hook). When two modules
need the same permission, a user-only requirement wins the merge.

`start_all` marks the registry started. Before that call, reload only stages a
candidate so bootstrap can register every module without starting early. After
it, an accepted reload starts enabled, non-paused modules that are `Stopped`,
`Disabled` or `Failed`; `WaitingForPermission` and `Unavailable` stay under
permission/service recovery and running resources are left alone.

Disabled configuration stops resources without editing anything. Pause is
temporary and independent of the desired enabled flag; enabling from disabled
clears it. Permission and dependency recovery never overrides disabled or
paused. An unchanged configuration is a no-op. A rejected configuration records
a diagnostic and keeps the last accepted revision and runtime; it never changes
status to `Failed`. A genuine start or stop failure becomes `Failed(failure)`
with the operation and category. A failed configure/start calls `stop`
immediately, then records the original failure; a cleanup `stop` failure does not
mask it.

Start order is topological over `requires`/`provides`, with registration order
among independent modules. Shutdown walks reverse successful-start order.
Repeated `start_all`, disable and `shutdown` are safe. A failed start does not
block independent modules and is retried only by an explicit resume/reload or a
prerequisite recovery event.

Provider changes are reconciled automatically. After disable, pause, resume,
reload, a failed start/stop or a permission change of a module that provides
services, the registry stops running dependents whose services are no longer
ready (`Unavailable(MissingService)`) and then restarts enabled, non-paused
dependents whose services are ready, in dependency order. The pass holds the
lifecycle lock and no module lock. `service_failed` and `service_recovered`
record the external fact and run the same pass.

Descriptors may declare command names. The registry resolves them on `PATH` and
requires an executable file with an execute bit before starting the module.
GUI-launched apps get a minimal `PATH`, so a module must declare only executables
the host can resolve; lookup policy beyond `PATH` belongs to part 6.

## Accepted configuration

Every slot owns one accepted-configuration cell, separate from its lifecycle
lock and published snapshot. `Registry::accepted_watch(handle)` returns a typed
`AcceptedWatch<C>`; the configuration type comes from the handle and is checked
against the registered module's type, so a same-identifier handle for another
registry cannot read the wrong type (`RegistryError::ConfigTypeMismatch`).

- `read()` returns `AcceptedConfig { revision, config }` or `None`. Both fields
  come from one lock acquisition, so they always match. The accessor takes no
  module, lifecycle or reload lock and never reads disk.
- `wait_for_change(seen_revision, timeout, token)` returns `Changed`, `TimedOut`,
  `Cancelled` or `Closed`, treating an absent configuration as revision zero.
  The check and sleep use the same cell lock, so publication between them cannot
  be missed; spurious wakes reread the authoritative pair. Precedence is
  cancellation, close, change, timeout. A timeout that overflows `Instant`
  becomes an unbounded wait rather than a panic or a busy loop.
- A listener on the runtime's token takes the cell lock and wakes waiters, so
  cancellation cannot be lost between the flag check and sleeping. The listener
  guard deregisters on return, so repeated waits do not accumulate listeners.
- `on_change(seen_revision, listener)` returns an `AcceptedSubscription` and
  runs the wake-only listener after a later publication, or once immediately
  when the cell already moved past `seen_revision` or closed. The check and the
  weak listener registration share the cell lock, so a publication between a
  caller's read and subscription cannot be missed. Callbacks run after the
  publication state and listener locks are released, may read the new pair, and
  are retired when the subscription drops. A callback must only wake a wait; it
  must not read disk or run module code.
- Slot invalidation, including `Registry::shutdown` and the teardown stages,
  closes the cell and wakes every waiter and change subscription; the last pair
  stays readable.

Publication happens only on acceptance. Registration with an accepted
configuration publishes revision `1`; `register_rejected`, an invalid initial
configuration, rejected validation, a failed reconfigure, a failed pre-commit
quiesce, deleted files and unchanged reloads publish nothing. A changed reload
publishes immediately after the registry assigns the candidate and increments
the revision, before a disabling quiesce or a start attempt, so a configuration
committed despite a later start or stop error is visible. The old pair stays
readable for the whole `Module::reconfigure` call. `FactoryContext` exposes the
same watch to factory socket handlers.

## Cache leases

`CacheLease` coordinates cooperating Stache Next processes that touch the
identity cache. It locks `runtime/cache.lock` (see [ipc.md](ipc.md)), a private
`0600` owned regular file outside the cache root, so deleting and recreating the
cache keeps one lock identity. `shared` covers processing, publication, reads
and apply; `exclusive` covers whole-root `cache clear`. A held shared lease is
never upgraded in place: drop it and acquire again before critical work.

`LeaseWait` carries a budget, a `CancellationToken` and an injected `LeaseClock`.
Acquisition polls one open file description with `try_lock`/`try_lock_shared` in
private ten-millisecond slices; cancellation wakes the sleep immediately and no
other lock is held while waiting. A bounded wait returns `Busy` at expiry and
`Cancelled` when the token fires, both without mutation. `verify` rechecks the
path and runtime directory against the held `dev`/`ino` and returns `Replaced`
or `Unsafe` instead of continuing under a split identity. Dropping a lease
releases the kernel lock and removes nothing; no participant unlinks the lock,
including clear and shutdown.

This is advisory coordination among cooperating processes, not protection
against OS or temporary-directory cleaners, or malicious same-UID mutation.

## Durable storage

`stache_sdk::durable` is the identity's durable counterpart of the runtime lock, added for the
Keybindings Caps journal. `AppPaths::application_support_directory()` resolves
`~/Library/Application Support/com.marcosmoura.stache.next` (a different root from the cache,
the runtime directory, the user-selected configuration directory and the legacy identity;
resolving it creates nothing, and tests inject `HOME`). `prepare_directory` creates a named `0700`
subdirectory (`keybindings`); `existing_directory` inspects without creating. `DurableLease` is an
exclusive advisory lock on a private lock file there, built on the same `lockfile` open/verify and
`LeaseWait`/`LeaseClock` as cache leases (ten-millisecond slices, `Busy`/`Cancelled` without
mutation, `verify` against the held inode and directory, never unlinked). `write_atomic` writes a
temporary `0600` sibling with no-follow exclusive create, syncs it, renames it over the target and
syncs the directory; `read_file` refuses links, loose modes, foreign owners, extra hard links and
oversize files; `remove_file` syncs the directory. The SDK knows no module: callers pick the
subdirectory and names.

## Threads

- Every module has its own lock. The registry's map lock is never held while a
  module or native service runs; multi-module operations take a separate
  lifecycle lock. The only nesting is a consumer's module lock while
  `service_ready` reads the provider's published metadata; it never takes the
  provider's module lock, calls provider code or joins a worker. That published
  cell is separately locked, so a consumer cannot wait on a provider transition.
- Each slot has a separately locked published snapshot. `snapshot`, `snapshots`
  and `service_ready` read only that cell, so they never take a module lock,
  call module code or wait for a worker join. A concurrent read sees the last
  published metadata: the last completed transition while configure, start,
  reconfigure or stop is in flight. Before a stop or cleanup begins, the slot
  publishes its non-running target status, so dependents never observe a
  service as ready while its provider is shutting down. Publishing runs under
  the module lock and holds the small published lock only to replace metadata;
  no module or native code runs while it is held.
- Each runtime owns a `WorkerGroup` and a `CancellationToken`. Modules receive
  only a `WorkerHandle`, which spawns named workers and cannot stop or join them.
  A token listener registered with `on_cancel` runs once after the token's own
  lock is released; the returned `CancelListener` deregisters on drop, and a
  listener registered after cancellation runs immediately.
- Stop order is: cancel the token, call `Module::stop` (unregister native
  callbacks), then join workers. A stopped group rejects new spawns. On a failed
  configure/start the same cleanup runs immediately; `needs_stop` stays set only
  when that cleanup stop itself failed.
- Each runtime also carries a `ModuleInvalidation` handle, returned by
  `ctx.invalidation()`. The registry fires it during the first teardown stage
  without taking the registry, slot or lifecycle lock, including while a start or
  stop holds one of those locks. It is logical invalidation only: queued native
  work that was never issued becomes inert, already-issued work finishes inert,
  and a listener registered after invalidation runs immediately. It is not a
  cleanup path; `Module::stop` still performs the acknowledged native removal.
- During coordinated teardown the registry stops modules in reverse start order,
  one at a time, each on an abandonable carrier bounded by `min(2 s, remaining)`.
  A hung module is named `module:<id>` and abandoned with the locks it took, and
  the next module still stops.
- Module factories belong to the app composition root, not the SDK. The app
  teardown stages retain every supplied factory, run its non-blocking
  `begin_shutdown` before registry invalidation, and run its bounded
  `finish_shutdown` drain in reverse registration order only after every module
  stop attempt. A factory-owned service therefore stays strongly owned through
  `Module::stop`, and a stalled drain keeps its factory alive while being named
  `factory:<id>`.
- `cancel_and_join` must not be called from one of that group's own workers.
  Registry operations that join run on the caller's thread, so the host calls
  disable/pause/reload/shutdown off the main thread.
- `MainThreadExecutor::execute` runs inline when already on the main thread and
  otherwise enqueues without waiting. No worker may wait synchronously for the
  main thread while the main thread waits for that worker.

`stache-macos::MainThread` implements the executor with `pthread_main_np` and
`dispatch_async_f`. The boxed job is reclaimed exactly once by the trampoline.

## Windows

Windows are a narrow module capability, not a raw native handle. A descriptor
declares `surfaces: &'static [SurfaceId]`; `ctx.windows()` returns a
`WindowCapability` bound to the module, those surfaces and the runtime's
cancellation token. `request` accepts a `WindowRequest` with an owner-local key, a
declared surface, validated logical dimensions (1..=8192) and a kind. Surface
spellings must be ASCII letters, digits, `-` or `_`, so a surface cannot add a
query parameter. Only `WindowKind::Standard` has native semantics; `Launcher` and
`NonActivatingPanel` return `WindowError::UnsupportedKind`.

The host supplies a `WindowBackend` at `Registry::new`. It opens or focuses the
window for an owner/key pair and returns a `WindowId`. Each runtime owns one
window session; every context created for that runtime (`configure`, `start`,
`reconfigure`, `stop`) clones the same capability, so all requests share one
lease map and one cancellation state. Every request for that identity shares one
lease: dropping or explicitly closing one `OwnedWindow` releases only that lease,
and the native window closes with the last one. The session gate stays held across
the backend's open/close/release calls; a backend queues native work and must not
block or reenter the capability, so this serializes bookkeeping without waiting
for AppKit.

A capability retained after its runtime stopped rejects every request with
`WindowError::OwnerStopped`; a request that races the stop releases its owner's
windows rather than resurrecting one. Stop invalidates the old session before
owner cleanup and any successor runtime, so an obsolete capability cannot open or
release a new runtime's windows. When a module stops, including the cleanup after
a failed configure or start, the registry releases the exact runtime's owner so no
window leaks. The lease cache holds live leases only; the last release removes the
entry instead of retaining a dead weak handle.

`stache_app` implements the backend over Tauri: it validates kind, size and
surface again at that boundary, then dispatches creation, focus and close to the
main thread without waiting. The launch path creates no window; only an explicit
developer action requests the `test` surface. A queued creation rechecks its live
reservation before touching AppKit, so a concurrent close or owner release wins
and a failed native creation removes the reservation for a later retry.

## Shared input (inert contract)

`stache_sdk::input` is the platform-independent half of the shared input service
(Keybindings part 02). It ships **idle**. `Registry::new` attaches
`InputService::unavailable()`, so fixtures get an `InputCapability` whose
subscriptions fail with `InputError::Unavailable` without calling any backend.
The production host attaches one service over the `stache-macos` owner with
`Registry::with_input_service` (through `HostDriver::with_input_service`), with the Keybindings
dispatcher's `WorkReservations` as the work capacity (`NoWork` in fixtures and the default), so a tap reservation and
dispatcher admission are one bounded decision. The native owner thread and every tap are lazy:
they exist only while an enabled module (today, Keybindings with Caps bindings, a running
Hold-to-Quit and a running MenuAnywhere) holds a subscription, or while a deferred removal still swallows a claimed press. No live tap or hotkey exists in this crate.

### Accepted configuration and post-commit work

The registry calls `Module::reconfigure(candidate)` **before** it publishes the candidate through
the accepted cell, and publishes only when `reconfigure` succeeded; a rejected candidate publishes
nothing. A module with resources that cannot be flipped inertly (a global Caps remap, native
registrations) therefore follows the canonical contract: stage inert resources in `reconfigure`
(failure rejects the candidate), observe the published `(revision, config)` through its
`AcceptedWatch`, and only then flip. Everything after publication is an accepted configuration with a
module-published degraded or cleanup outcome, never a rejection. Keybindings (`runtime.rs`) is the
reference: its coordinator worker waits on the watch, and every press compares its route's revision
with the cell's current one, so nothing executes between publication and observation. No generic
post-commit API was needed; if a future module needs a typed registry-level post-commit outcome,
extend the registry contract narrowly instead of weakening `reconfigure`.

- **Values.** `KeyCode`, exact `MouseButton` numbers (button 2 never matches 3+),
  a named `ModifierFlags` mask, `InputEventKind` (key down/up, `FlagsChanged`,
  mouse down/up) and an immutable `InputEvent` (kind, stream, flags, repeat,
  timestamp, optional `location`, `PressId`, context snapshot). There is no native
  pointer, mutation or injection API. `location` is an `InputPoint` (`f64` x and y in
  Quartz global coordinates) that the tap callback reads from the tapped event as a
  plain field; it is `None` for keyboard events. The macOS adapter maps left, right and
  other mouse events to one mouse kind with the native button number and drops any
  event whose type contradicts its button (left 0, right 1, other 2 or more), so a
  consumer can match on the button number alone. A consumer that needs the click
  point (MenuAnywhere) copies the event's `location` in its callback instead of
  sampling the pointer later.
- **Filters.** `InputFilter` has a kind set, key/button `Selector`s and a
  `ModifierMatch` (`Any`, `Exact`, `AllOf`; Caps Lock is ignored by `Exact`). A
  `FlagsChanged` event for a key the filter names always passes the modifier
  comparison, so a modifier release still arrives.
- **Context.** The host publishes versioned immutable `ContextSnapshot`s through
  a `ContextPublisher`; frontmost state is `Known` (bundle id plus opaque
  exemption bits), `Unknown` or `Stale`. The tap reads the snapshot with
  `try_read`; a busy read fails open as `Unknown` and is counted.
  The host publishes it from `frontmost_context.rs`, one reference-counted lease
  over its own `ApplicationControl`: the first lease installs the observation and
  an observed frontmost watch, the last release stops them with acknowledgement,
  and with no lessee nothing runs. An activation marks `Stale` at once; a
  confirmed refresh with a bundle id publishes `Known` with the
  `host::input_context` bits (`FINDER = 1 << 0`, `CURRENT_PROCESS = 1 << 1`, the
  rest reserved; an exempt app is still `Known`); anything else and the final
  stop mark `Unknown`. A lease failure is a typed error, unacknowledged cleanup
  stays owned (it blocks a duplicate install and is retried first), and a
  teardown owner under `NativeSlot::ApplicationObservers` acknowledges it.
  Hold-to-Quit is the first lessee: its start leases the publisher through an
  injected `FrontmostLeaser` (the module crate never depends on the app crate)
  and releases it at stop, and it reads the context only as the secondary,
  bundle-only gate beside its own exact-instance snapshot. Its HUD and
  application-control cleanup that is not acknowledged is kept by the factory
  (`HudHost`, `ApplicationControl`) and acknowledged under `NativeSlot::Hud` and
  `NativeSlot::ApplicationObservers`.
- **Subscriptions.** `InputHandler` (suppressing) returns `Disposition` and gets a
  `CallbackScope` that can only take the work slot reserved for the press and
  change one per-press word. `InputObserver` (passive) returns nothing and runs on
  the separate listen-only tap, so it cannot suppress or reorder. `InputPriority`
  is a generic `(level, tie_break)`; the SDK knows no module IDs. Higher priority
  runs first, ties fall back to registration order, the first suppressor wins and
  lower handlers never see a suppressed event. Overlaps (including equal-priority
  ties) are recorded as `InputDiagnostic::Overlap`. `SubscriptionId` is scoped by
  service identity, tap generation and revision; foreign and stale IDs are
  rejected.
- **Press ledger.** A service-owned ledger bounded at 1,024 entries records the
  first-down disposition and suppressing owner until the up. Logical
  unsubscribe, reload or a priority change make the owner inert but leave a
  tombstone that still consumes repeat and up. A passed down never acquires a
  suppressor mid-press and orphan releases pass. Exhaustion fails open with a
  counter; nothing is evicted. A first down that cannot reserve required work
  (`WorkRequirement::Reserve`) fails open for the whole press; empty-command
  capture uses `WorkRequirement::None`, needing a ledger entry but no slot.
- **Work reservation.** `WorkReservations::try_reserve`/`release` is the
  non-blocking contract the Keybindings dispatcher implements. Overflow is a
  counter, never deferred execution. There is no second queue.
- **Tap lifecycle.** `InputBackend::submit` queues `ControlRequest`s
  (`Install`, `Enable`, `Remove` per `TapKind::{Suppressing, ListenOnly}`) for one
  native owner; `ControlQueue` is the 256-request reject-on-full queue the owner
  drains. The suppressing tap installs on the first suppressing subscription and
  is removed after the last unsubscribe is acknowledged (see _Deferred tap
  removal_ below when a suppressed press is still in flight); the listen-only tap
  exists only for passive subscribers, and passive-only use never creates the
  suppressing tap. Readiness is published only after the `Enable` acknowledgement.
  Acknowledgements wait up to `ACKNOWLEDGEMENT_BUDGET` (2 s) in
  `CANCELLATION_QUANTUM` (10 ms) slices on an injectable `InputClock`; both equal
  the `stache-macos::applications` constants. A cancelled or timed-out queued
  `Install`/`Enable` is revoked so the owner never acts on it late; a timed-out
  `Remove` stays queued. A failed or timed-out action leaves the tap `Retained`
  with the reason, never a false success; `retry_cleanup` and
  `retry_reenable` (bounded at `MAX_RETRY_ATTEMPTS`) are explicit.
- **Deferred tap removal.** When the last suppressing handler unsubscribes while
  the ledger still holds a suppressed tombstone, logical unsubscribe and
  reservation removal (which restores Carbon yields) stay immediate but the
  suppressing tap stays installed, so the rest of that press (repeats and up) is
  still swallowed. `UnsubscribeReport.native` is `NativeCleanup::Deferred`. New
  first downs pass meanwhile (no handler) and tombstoned repeats/ups stay
  suppressed. Removal becomes due when the last tombstone is consumed by its up,
  when a tap-disable drains the ledger (which then skips the re-enable, but only
  while the deferral is still pending and the tap is `Ready` or `Disabled`; if the
  helper already finished, or a subscription is reviving the tap, or a revive
  failed, the normal re-enable runs, which is harmless), or at
  owner shutdown (`InputService::begin_owner_shutdown` queues the `Remove`
  without waiting; `MacInputBackend::shutdown` calls it first). The tap
  callback only flips a flag and notifies; a short-lived helper thread then
  performs the ordinary acknowledged `Remove` through the control path, never on
  the tap thread, with no timer. A failure leaves the tap `Retained` for
  `retry_cleanup`. A new suppressing subscription during the deferral cancels it
  and reuses the installed tap; if that tap was disabled meanwhile, the
  subscription first re-enables it with an acknowledged `Enable`, and if that
  fails the subscribe fails while the deferral still removes the tap. A disable
  that lands during that revive `Enable` submits its own re-enable, so the
  subscribe may fail with `TapNotReady`. Without tombstones removal is immediate as before.
- **Tap disabled.** `InputService::tap_disabled` cancels armed per-press state
  (`InputHandler::on_cancel`), clears the ledger, advances the `Epoch`, never
  synthesizes releases, records a `Discontinuity` and submits one re-enable whose
  result shows in `tap_status` (`Disabled`, `Ready` or `ReenableFailed`).
- **Reservation registry.** `ReservationRegistry` holds normalized physical
  `Chord`s and declarative HID filters with `Carbon`/`Hid` backend, opaque
  owner/generation/revision and priority. Cross-backend overlaps are recorded as
  conflicts. A HID claim with strictly higher priority makes every overlapping
  Carbon reservation _yield_: `dispatchable(id)` is then false (it also fails
  closed for stale IDs) and `yielded_to(id)` names the claimant. Yields are
  derived, not stored by callers: they are re-derived on every register, update,
  remove, `set_active` (pause) and `release_owner`, so they persist across
  reclaims of either side and are restored when the claimant is removed, paused,
  replaced or moved off the chord. `yield_to(reservation, claimant)` is the
  explicit, idempotent, validated form: it rejects stale/foreign IDs, non-Carbon or
  non-overlapping pairs and inactive claimants (`NotYieldable`), and a claimant
  without strictly higher priority (`UnsupportedOverlap`, with a diagnostic).
  Equal or lower priority overlaps remain recorded conflicts. Yielded and paused
  pairs are not reported by `conflicts()`. The only claim is that a yielded Carbon
  reservation does not dispatch; whether a suppressing HID tap already prevents
  Carbon delivery is an unmeasured gate of Keybindings part 03.
- **Limits.** The macOS HID tap carries no per-keyboard identity, so a press is a
  key or button plus an epoch. The 1 ms callback target is soft and measured after
  the callback returns (the subscriber then becomes inert); arbitrary code cannot
  be preempted. Panics are contained with `catch_unwind` only in unwind-enabled
  builds; the release profile uses `panic = "abort"`, so production callbacks must
  be audited, bounded and non-panicking. Locks on the tap path are `try_lock` only
  and fail open.

Fakes (`FakeClock`, `FakeBackend`, `CapacityWork`) live in `input::testing`, built
under `cfg(test)` or the `testing` feature. The macOS owner implementing
`InputBackend` is described in [overview.md](overview.md#shared-input-owner).

## Events

`EventBus::publish` is non-blocking and never calls subscriber code. Each
`Subscription<E>` owns a bounded channel; consumers pull with `try_recv` or
`recv_timeout`. When a queue is full the newest event is dropped and that
subscription's `dropped()` counter increments. Dropping the subscription
unregisters it. There are no callbacks in the bus, so a slow or absent consumer
cannot block a native publisher. A zero capacity is clamped to one.

## Errors and logging

`ModuleError` carries the module, `Operation`, `ErrorCategory`, a human message
and an optional source. Branch on the operation and category, never on the
message. `RegistryError` is the separate host-level type for duplicate modules,
unknown handles and invalid service declarations.

`Logger` is injected, not global. A `ScopedLogger` binds the module and operation
to every record. Level filtering and formatting belong to the injected logger;
two registries in one process can use different loggers safely. When a reload
already reports a start error, a failure in the reconciliation pass that follows
is logged at `warn` with the reloading module and `Operation::Reload` instead of
being discarded.

## Fake module example

This compiles against the shipped API. The executor and permission provider are
fakes; the host supplies the real implementations.

```rust
use std::sync::Arc;
use stache_sdk::{
    ConfigError, DiscardLogger, EventBus, MainThreadExecutor, Module, ModuleConfig, ModuleContext,
    ModuleDescriptor, ModuleId, ModuleResult, ModuleStatus, Permission, PermissionError,
    PermissionProvider, PermissionStatus, Registry, ThreadError,
};

#[derive(Clone, Debug, PartialEq)]
struct WatchConfig {
    enabled: bool,
}

impl ModuleConfig for WatchConfig {
    fn enabled(&self) -> bool {
        self.enabled
    }

    fn validate(&self) -> Result<(), ConfigError> {
        Ok(())
    }
}

struct Watch;

impl Module for Watch {
    type Config = WatchConfig;

    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: ModuleId::new("watch"),
            provides: &[],
            requires: &[],
            permissions: &[],
            executables: &[],
            surfaces: &[],
        }
    }

    fn configure(&mut self, config: &WatchConfig, ctx: &ModuleContext) -> ModuleResult<()> {
        ctx.logger().info(format!("configured (enabled: {})", config.enabled));
        Ok(())
    }

    fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        ctx.logger().info("started");
        Ok(())
    }

    fn reconfigure(&mut self, _: &WatchConfig, ctx: &ModuleContext) -> ModuleResult<()> {
        ctx.logger().info("reconfigured");
        Ok(())
    }

    fn stop(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        ctx.logger().info("stopped");
        Ok(())
    }
}

struct InlineMain;

impl MainThreadExecutor for InlineMain {
    fn is_main_thread(&self) -> bool {
        true
    }

    fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
        job();
        Ok(())
    }
}

struct NoPermissions;

impl PermissionProvider for NoPermissions {
    fn status(&self, _: Permission) -> PermissionStatus {
        PermissionStatus::Granted
    }

    fn request(&self, _: Permission) -> Result<PermissionStatus, PermissionError> {
        Ok(PermissionStatus::Granted)
    }
}

fn main() {
    let registry = Registry::new(
        Arc::new(DiscardLogger),
        Arc::new(EventBus::new()),
        Arc::new(InlineMain),
        Arc::new(NoPermissions),
        Arc::new(stache_sdk::UnavailableWindows),
    );
    let handle = registry.register(Watch, WatchConfig { enabled: true }).expect("register");
    registry.start_all().expect("start");
    assert_eq!(registry.snapshot(handle).expect("snapshot").status, ModuleStatus::Running);
    registry.shutdown().expect("shutdown");
}
```

## Commands

`stache_sdk::commands` is the shared direct-process runner. Commands are split with
the legacy quoting rules (single/double quotes, backslash escapes, empty quoted
arguments, errors for an unterminated quote or trailing escape) and spawned directly:
no shell, expansion, pipe or redirect, and the caller's cwd, environment and stdio are
inherited. A name resolves directly when absolute, otherwise through injected
`STACHE_EXTRA_PATHS`, `PATH`, `/usr/local/bin`, `/opt/homebrew/bin`,
`/opt/homebrew/sbin`, `~/.cargo/bin` and `~/.local/bin` in that order (a missing `HOME`
drops the last two; empty `PATH` components keep their `std::env::split_paths` meaning).
The exact name `stache` resolves only to the injected running executable, with no
`PATH` or legacy fallback, and is spawned with `--stache-internal-cli` before its
original arguments (see [ipc.md](ipc.md)).

`CommandExecutor` has two modes over an injected `ProcessFactory` and `Clock`:

- **Inherited** (`run_sequence`): waits for each step in order and stops at the first
  parse, resolution, spawn, wait or nonzero-exit failure, reported as a typed
  `StepFailure`. There is no duration deadline. `execOnStartup` uses it through the
  app's `ProcessCommands` adapter, which keeps the established `startup command N: ...`
  wording and treats cancellation as success.
- **Captured** (`run_captured`): a caller deadline and separate stdout/stderr caps
  (each at most `MAX_CAPTURE_BYTES`, 1 MiB) with nonblocking streamed draining on the
  poll quantum. Overflow, timeout and cancellation are typed outcomes; truncated output
  is never a success, and a timeout never implies the command did not act.

A `SpawnGate` is consulted immediately before process creation so a stale generation,
revision or cancellation cannot issue a later step. Cancellation kills only the directly
owned child, polls every 10 ms (`POLL_QUANTUM`) for 2 s (`CANCEL_CLEANUP`), then moves the
exact handle to the executor's own reaper. The executor owns 64 retained-child slots
(`RETAINED_SLOTS`); a slot is reserved before each spawn, released on confirmed reap, and
exhaustion refuses new children instead of dropping a handle. The executor starts no
thread: retained children are re-signalled and swept on the next use or `sweep()`, and
a `Retained` outcome never proves termination.

## Currently shown vs planned

Shipped here: registry, lifecycle, threading, events, logging, permissions,
errors, descriptors, window capabilities, the safe Accessibility/main-thread
adapters, file-backed configuration (catalog, store, watcher, comment-preserving
edits, templates and schemas), the typed accepted-configuration watch, identity
paths/instance lock, cache leases, durable storage and the socket protocol/router/client/server,
and the shared input contract with its idle macOS owner. The Tauri
composition root, tray, permission polling, shutdown coordinator and test window
are described in [overview.md](overview.md) and [ui.md](ui.md); the CLI
composition is in [ipc.md](ipc.md). The proxyAudio runtime is in
[proxy-audio.md](../features/proxy-audio.md); the typed React client and test
window are in [ui.md](ui.md).
