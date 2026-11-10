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
| Start, baseline permission absent   | `true`  | no      | `WaitingForPermission(permission)`, no module code |
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
| Baseline permission revoked         | `true`  | no      | Every running module stops (reverse), then waits   |
| `service_failed`                    | `true`  | no      | Transitive dependents become `Unavailable`         |
| `service_recovered`                 | `true`  | no      | Unavailable dependents are retried                 |
| `shutdown`                          | any     | any     | Resources stopped; later starts are refused        |

### Tiling module lifecycle (parts 05-06, registered default disabled)

`stache_tiling::TilingModule` (built by the host-lifetime `TilingRuntime` and
registered by `TilingFactory::production`, default disabled) shows the
intended use of the lifecycle contract for a module with native resources:

- **Start** acquires, in order, the backend epoch, the effect worker, the actor,
  the reservation watcher, the native display feed (observer plus worker; its
  initial layout reaches the actor first), the accepted-config coordinator worker, queues the
  initial enumeration plus the accepted configuration and last publishes the
  action authority for the accepted revision. Any failure unwinds every earlier
  stage exactly; an unwind the backend cannot finish leaves a quarantine instead
  of a silent drop. `start` returns without waiting for any window to move.
  Accessibility is declared `PromptPolicy::UserOnly`; a missing grant is the
  registry's `WaitingForPermission` and nothing is acquired or prompted.
- **Reconfigure** only stages pure, fallible work. A coordinator worker waits on
  the `AcceptedWatch`, and only after it observes a revision does it submit that
  coherent `(revision, config)` pair to the actor and the issuance gate. There is
  no module-local config counter, no `revision + 1`, and no native work under the
  publication lock; a rejected candidate changes nothing.
- **Stop** seals the authority with one atomic store (also from the registry's
  invalidation listener), publishes the stopping status, wakes every wait, then
  runs fresh finite 2 s per-hop stages: frame drain, the restore of runtime-hidden
  applications, reservation watcher, display feed, backend epoch, actor and effect worker. A stage that does
  not finish stays owed in a quarantine holding the exact resources. The registry
  retries a failed stop before any restart (`start_slot` stops first), so the next
  stop or enable re-runs only the owed stages; a blocked join is never detached.

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

The registry also accepts a host-level **baseline**, set once with
`Registry::with_baseline_requirements(Vec<PermissionRequirement>)` before the first
`register` (afterwards it returns `RegistryError::BaselineAfterRegistration`; the
default is empty). The baseline is merged into the requirements of every slot,
whatever its configuration and even when it declares none, at registration,
`register_rejected` and every committed reload, with the same user-only-wins rule.
The existing start check, `WaitingForPermission` status and `on_permission_change`
paths therefore apply unchanged: no module code (`configure`, `start`) runs while a
baseline permission is missing, a reload then is accepted and waits, a grant starts
waiting modules in dependency order (a failing start is `Failed` and does not block
the others) and a denial stops running modules in reverse start order. Disabled and
paused modules keep their status. `permission_requirements()` returns the baseline
unconditionally, even with zero, disabled or paused modules. The baseline is
immutable, performs no platform call and caches no grant.

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
The host's Accessibility baseline is user-only. `PermissionRequirement::shared_input`
(user-only) stays the declaration for future non-baseline permissions and for
shared-input consumers such as Keybindings, Hold-to-Quit and MenuAnywhere (the MenuAnywhere runtime consumes it
unchanged, with no module-specific prompt hook); for
Accessibility it is redundant with the baseline but harmless. When two requirements
name the same permission, a user-only requirement wins the merge.

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

## Published state and observation

`stache_sdk::published_state` lets one module publish a typed latest-state value
that other modules and the app read. It is a generic contract: the SDK imports no
module crate. Keep Awake is the first production provider (schema `keepAwakeStatus`
v1 over its own `KeepAwakeRuntimeStatus`, registered by `app::published_state::state_schemas`);
`keepAwake.status` and `KeepAwakeSnapshot` are unchanged. Bar observes it optionally
(`observes`) and the app projects it into the Bar view; no module imports another
module's crate.

- **Schema contract.** A provider type implements `PublishedState`
  (`SCHEMA = StateSchema { name, version }`). The app registers one contract per
  provider before any module registers, with `Registry::with_state_schema::<T>(provider)`
  or a `StateSchemas` set (`HostDriver::with_state_schemas` forwards it). Identity
  is provider `ModuleId` + schema name + version. A second schema for a provider
  (`DuplicateStateSchema`) or a registration after the first module
  (`StateSchemaAfterRegistration`) is a `RegistryError`.
- **Publishing.** `ctx.state_publisher::<T>()` returns a `StatePublisher<T>` bound
  to the runtime generation, or a deterministic `StateError`: `NotRegistered`,
  `SchemaMismatch` (the module's registered type is another type or version) or
  `Unauthorized` (`T` is registered for another provider). `publish(revision, &T)` serializes
  to JSON inside the SDK; a `StatePublisher<A>` cannot publish a `B` (compile-fail
  doctest).
- **Observing.** A consumer lists providers in `ModuleDescriptor::observes` (new,
  distinct from `requires`; `requires`, ordering, readiness and `service_failed`
  are unchanged) and calls `ctx.observe_state(provider)`; an undeclared provider is
  `StateError::NotObserved`. The app uses `Registry::observe_state` or
  `FactoryContext::observe_state` for any provider. A `StateObserver` is read-only:
  `read()` and `wait_for_change(seen_version, timeout, token)`. An unknown provider
  or one without a schema reads `StateReading::Absent`; a registered provider that
  is not running reads `Unavailable` with a reason (`NotStarted`, `Starting`,
  `NotPublished` or `Ended(RuntimeEnd)`), never stale active data. Observing never
  changes the consumer's lifecycle.
- **Envelope.** `StateEnvelope` carries provider, runtime `generation` (new per
  start, unique per registry), the accepted configuration `revision` the provider
  labelled the payload with, a per-generation `sequence` (restarts at one) and the schema
  identity, plus the serialized payload. Consumers outside the provider crate read
  the payload and identity; the app decodes with the provider's own type through
  `StateEnvelope::decode::<T>(provider)`, which checks the provider and schema
  identity first, so two providers with the same schema name and version cannot
  decode each other's payloads. Payload
  fields are one atomic snapshot; host diagnostics sampled separately are not
  claimed atomic with it. `u64` values become exact decimal strings only in the
  app projection (`PublishedStateView`), like `ModuleSnapshot::revision`.
- **Accepted revisions.** The provider labels every payload with the accepted
  revision it was derived from (`AcceptedConfig::revision` from the worker's
  `AcceptedWatch`), so a worker still computing with config N publishes
  N-labelled envelopes while N+1 is accepted and labels N+1 only after reading
  it from the watch. Per generation the cell tracks `start_revision` and the
  latest `accepted` revision: a revision below `start_revision` or below the last
  committed one fails with `RevisionStale` (an older payload never overwrites a
  newer one), and one above `accepted` fails with `RevisionNotAccepted`. The
  registry advances the cell's `accepted` BEFORE the accepted-configuration cell
  shows the new revision, so no envelope can carry a revision that has not been
  accepted. A provider that only labels payloads with revisions it read from the
  watch never runs ahead of it; in the short window between the cell learning a
  revision and the watch showing it, a publish at that revision can succeed. There is
  no reconfigure hold: a failed reconfigure changes nothing and an unrelated
  publication made during it commits at once. The SDK cannot tell a payload's
  source, so never publish candidate-derived state from `Module::reconfigure`;
  publish it after the worker reads the new revision from the watch. Only publications made during
  `Module::start` are held (latest wins) and released just before the module
  reads as running (`PublishOutcome::Held`); a failed start drops them and
  exposes no available state. A provider that never started reads the registry
  status (`Ended(Disabled | Paused | PermissionBlocked | DependencyUnavailable |
  Failed)`, else `NotStarted`) with no generation. After a generation has ended the
  tombstone keeps that last generation, and its reason follows later registry
  status changes (for example a failed provider that is then disabled reads
  `Ended(Disabled)`); it never becomes available again except through a new
  generation, and a repeated identical status does not change the cell.
- **Concurrency.** One cell per provider; `publish` updates it under its lock
  before waking waiters. `wait_for_change` mirrors `AcceptedWatch::wait_for_change`
  (same-lock check and sleep, cancellation wake, precedence cancel, close, change,
  timeout, overflow becomes unbounded) and returns the latest value, so a slow
  consumer sees coalesced state rather than a per-sample queue. `Registry::invalidate`
  closes every cell and wakes waiters; cells created after that start closed, so
  a late `observe_state` never blocks until timeout. Readers take no registry,
  module or lifecycle lock and never run provider code.
- **Generation scope.** The registry creates a crate-internal `GenerationScope`
  per runtime (`generation.rs`) and ends it with a `RuntimeEnd` (`Stopped`,
  `Disabled`, `Paused`, `Failed`, `PermissionBlocked`, `DependencyUnavailable`) as
  the first step of every stop, before cancellation, `Module::stop` or a worker
  join. The cell then holds a tombstone and wakes readers; writes from the ended
  generation's handle fail with `StateError::Stale`, and an old handle can never
  update or clear the successor's state. This is distinct from the per-slot
  `ModuleInvalidation`, which fires only at shutdown.

## Screen reservations and display identity

A module that sets `ModuleDescriptor::reserves_screen` (default `false`, like
`observes` it is a declaration, not a dependency; `requires` is unchanged) obtains
`ctx.screen_reservations()`; without the flag it gets
`ScreenReservationError::NotDeclared`. The registry owns one `ReservationHub`
(`screen_reservations.rs`, distinct from the keyboard-chord registry in
`input/reservations.rs`).

- **Display identity.** `DisplayId` is a numeric `CGDirectDisplayID`-compatible
  `u32` (no native pointer; `0`, the null display, is never present).
  `TopologyGeneration` is a strictly increasing host-chosen number (initial
  `0`). The SDK never enumerates displays: the host reports the live set with
  `Registry::apply_screen_topology(generation, &[DisplayId])`, which rejects a
  non-increasing generation. An ID is meaningful only with the topology
  generation it was observed in. A display is present from the generation that
  first listed it (`DisplayReservations::since`) until a topology omits it; applying
  a topology drops every reservation of an omitted display, and a later return of
  the same numeric ID is a new presence (later `since`) that is not resurrected.
  The SDK cannot see a reuse inside one topology step, so the host omits such an
  ID from the topology it applies first. Native enumeration and hotplug are not
  shipped.
- **Staging.** `ScreenReservations::replace(topology, &[(DisplayId, EdgeInsets)])`
  atomically replaces the owner's whole set (empty removes it) and returns the
  snapshot revision; `remove()` is idempotent. `EdgeInsets::new(top, left, bottom,
  right)` accepts finite, nonnegative logical points up to
  `MAX_RESERVATION_INSET` (negative zero is normalized); NaN, infinite, negative
  or larger values fail with `InvalidInset`/`InsetTooLarge`. `replace` also fails
  with `StaleTopology` (staged against a generation other than the current one),
  `UnknownDisplay`, `DuplicateDisplay`, `TooManyDisplays` or `StaleGeneration`;
  every failure leaves state unchanged.
- **Snapshot.** `Registry::observe_screen_reservations()`,
  `FactoryContext::observe_screen_reservations()` and
  `ModuleContext::observe_screen_reservations()` (any module, no declaration)
  return a read-only
  `ScreenReservationObserver`. `read()` gives an immutable
  `ScreenReservationSnapshot { revision, topology, displays }`: every present
  display, ordered by `DisplayId`, with the per-edge MAXIMUM over all owners
  (never a sum) and each owner's contribution (owner, exact runtime generation,
  insets) ordered by owner. `revision` is monotonic and advances only on an
  observable change (an identical replacement or a repeated remove does not).
  Insets are measured inward from the full logical display bounds.
- **Consumer facts.** The host applies topology (`Registry::apply_screen_topology`),
  not modules. The app's Bar-scoped topology feed (below) is the only caller, and
  only while Bar runs: with Bar off the snapshot lists no displays and consumers
  treat every display as zero-reserved; with Bar running it lists every attached
  display. Reservations have no reconfigure hold, so an owner stages reservations
  only after observing its accepted revision. Reservations staged during
  `Module::start` are visible immediately (there is no start hold, unlike
  published state) and are removed if the start fails, so consumers must not
  treat a start-time reservation as a confirmed running owner.
- **Bar-scoped topology feed (Bar part 03, `stache-app` `bar.rs`).** The feed is a
  `stache_bar::TopologyLease` that the Bar module obtains from the
  `stache_bar::TopologySource` seam (so `stache-bar` never depends on the app) in
  `Module::start` and ends in `stop` or on any failed start; dropping it ends it
  too. Beginning it observes `DisplayControl::observe_topology` (the callback only
  records a coalesced `TopologyHint` and wakes the `stache-bar-displays` worker),
  queries the layout once (a bounded main-thread job) and applies the first
  topology. The worker re-queries per hint, applies a NEW monotonic generation only
  when the set of display IDs changed (the generation counter lives in the source,
  so it stays monotonic across Bar runs) and publishes a `TopologyReading` (layout,
  primary, applied generation, error) that Bar follows. `end()` applies the EMPTY
  topology FIRST, then removes the observer and joins the worker within
  `FEED_JOIN`; a stuck removal or join returns "still owed" and the lease stays
  owned for an exact retry (the module's `stop` returns an error so the registry
  retries it). A failed display query publishes an explicit "unavailable" reading
  (Bar clears its panel and reservation) and retries every `QUERY_RETRY`. The
  registry reaches the feed through `ModuleFactory::bind_registry` (a `Weak`, so a
  module never owns the registry). A numeric display ID that disappears and returns
  inside one coalesced notification looks unchanged; a real unplug/replug produces
  a notification per step, so an intermediate topology omits the ID.
- **Bar producer (part 03, `stache-bar` `runtime.rs`).** `BarModule` declares
  `reserves_screen: true` and the `bar` surface. It stages ONE reservation, a
  constant top inset `height + padding`, on the primary display (`NSScreen.screens[0]`,
  never `mainScreen`) while a panel exists, named against the topology generation
  of its latest reading; a stale generation or unknown display re-reads the feed and
  retries once per topology change, never in a loop. Zero size (`height: 0` or a
  computed width `<= 0`), no primary and every end of the generation (stop, pause,
  disable, failure) clear it. `start` and `reconfigure` never wait for the page:
  they queue the open, stage the reservation and return, so a page that never
  loads cannot hold the registry's global lifecycle gate. Bar's own
  `stache-bar-panel` worker observes the load off-main with no lock held, bounded
  by `PANEL_LOAD_DEADLINE` (10 s), woken by cancellation or any new convergence,
  and rechecks the exact run and window before it commits. A load failure,
  timeout or create failure after `start` returned closes the exact window,
  clears the reservation and publishes `panel: failed` with a typed detail; Bar
  stays `Running` and the topology feed stays applied (every display listed, zero
  host inset). The worker retries with a fresh exact window after 2 s, 10 s and
  30 s (`PANEL_RETRY_BACKOFF`), then stays failed until the next convergence
  trigger (accepted config change, topology or primary change) or a pause/resume,
  each of which retries immediately. Only synchronous failures (no reservations
  capability, feed begin, converge error, worker spawn) fail `start`; a
  `reconfigure` that enters the panel state is accepted and follows the same path
  (an inexpressible frame is still rejected at preflight). The production factory
  is NOT chained into `lib.rs` yet (part 04).
- **Bar menu-bar observation and data bridge (part 03 slice c, `stache-bar`
  `menubar.rs`, `stache_macos::menu_bar`, `stache-app` `bar_bridge.rs`).** The
  published `menu` (`visible`, `hidden`, `unknown` plus a reason) follows the
  legacy watcher: `visible` means the system menu bar is on screen, which is where
  the legacy UI slid the Bar away (`menuHidden` was true when the menu bar was
  visible). Observation (one `stache-bar-menu` worker and one native observer)
  exists only while Bar runs with a panel; disabled, paused, zero size, no
  primary display and a failed panel own none. The worker subscribes to the
  native notifications first, seeds with one sample, re-samples on every coalesced
  hint, polls every 100 ms **only while auto-hide is on or unknown** (the pointer
  entering the menu-bar row posts nothing and the auto-hide preference has no
  usable notification) and sleeps otherwise, and retries a failed sample or
  observer registration every 2 s (polling meanwhile). Only a changed
  `(state, reason)` is published, so a persistent failure is one diagnostic. Stop
  sets the exact stop flag, removes the observer, joins within 2 s; a stuck join or
  removal reports `cleanupPending` and is retried, and callbacks after stop are
  ignored. The reservation stays `height + padding` in every menu state; the menu
  only changes the published view. Limits: the `CGWindowList` layer-25 fallback
  cannot tell the menu bar from other layer-25 windows and window names need
  Screen Recording; real notification delivery is unverified.
  The host bridge reads the Bar runtime's published status cell (short cell lock
  only; no module, registry or native lock and no native call on a read) and
  projects it to the `BarView` DTO. `generation`, `sequence` and `configRevision`
  are exact `u64`s sent as decimal strings. The `sequence` never resets, also across
  generations, and alone orders views and events: the UI subscribes to the
  `BarChanged` event first, then calls `bar_snapshot`, and applies a snapshot or
  event only if its sequence is numerically greater than the held one
  (`sequence_is_newer`). A worker (`stache-bar-bridge`, alive only while Bar runs
  or a change is pending) turns the cell's change flag into events: bursts
  coalesce into the newest view, a gap of more than 64 sequences or an earlier
  failed emission becomes `resnapshot: true` with no view. Events go to the live
  Bar label only. The bridge is managed detached until part 04 chains the factory.
- **Native projection.** `stache_app::display::sdk_display_id` converts a
  `stache_macos::desktop::DisplayId` into the SDK `DisplayId` (a plain function:
  the orphan rule blocks `From`). It is the one projection Tiling 05 and Bar 03
  must use; applying the topology itself still belongs to Bar 03. The reservation
  hub is a single hub closed by `Registry::invalidate`, so it has no late-created
  cell gap.
- **Tiling consumer (parts 05-06, registered default disabled).** `stache-tiling`
  `module.rs` is the first consumer of this observer and of `AcceptedWatch`. Its
  `start` spawns a `ReservationWatcher` (subscribe-before-fetch) and waits, within
  one finite hop, until the first snapshot reached the actor, and only then
  publishes the action authority, so reservations are consumed before the first
  frame effect. The watcher reads through `ModuleContext::observe_screen_reservations`
  (the app factory injects `stache_app::display::sdk_display_id` as the
  `DisplayProjection`). Tiling still declares no `reserves_screen` and applies no
  topology. The `lib.rs` composition registers the module default disabled.
- **Reading.** Subscribe before fetch: `read()` then
  `wait_for_change(seen_revision, timeout, token)` with the same race-free
  check-and-sleep, cancellation wake, precedence (cancel, close, change,
  timeout) and overflow rule as the other watches. `Registry::invalidate` closes
  waiters. Readers take no registry, module or lifecycle lock and never read a
  producer's configuration.
- **Lifecycle.** The capability listens on the runtime's `GenerationScope`. When
  the generation ends (stop, pause, disable, failure, failed or partial start)
  the hub removes exactly that owner and generation's set and wakes readers
  before cancellation, `Module::stop` or a worker join; later writes from the
  ended handle fail with `StaleGeneration`. The hub registers one end listener per
  (owner, generation), however often the capability is requested. An old handle
  can never replace or remove a successor's set. Logical reservations are host state and distinct from
  native panel acknowledgments.

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
query parameter. `WindowKind::Standard` and `WindowKind::NonActivatingPanel` have
native semantics; `Launcher` returns `WindowError::UnsupportedKind`. A panel
request must carry `WindowRequest::origin` (logical top-left, `WindowOrigin`) and
a `Standard` request must not (`WindowError::InvalidPlacement`); the size
validation is shared and unchanged. No production module requests a panel yet;
only host surface `bar` can be a panel (see [UI](ui.md)). Natively Tauri keeps
the window and webview (hidden window) and the webview is displayed inside a
`stache-macos` non-activating `NSPanel`; the panel is unhosted before any close.

`OwnedWindow::reposition(WindowFrame)` (backed by `WindowBackend::reposition(owner,
window, frame)`) updates the frame of that exact live window; it is refused for a
released handle, a stopped session or a window of another owner. It only queues
native work, like open.

Queue acceptance is not creation. A backend may expose a `WindowPhases` tracker
(`WindowBackend::phases`); the owner reads `OwnedWindow::phase()` (a
`PhaseSnapshot` of `WindowPhase::{Requested, Created, Loaded, Failed(reason),
Closing, Closed}` plus a revision) and calls `wait_phase_change(after_revision,
timeout, &cancel)` from a worker. The read and wait share one lock and every
cancellation listener takes it before notifying, so a change or cancellation
between read and wait is never lost; session stop or the caller token ends the
wait with `PhaseWait::Cancelled`, and no session gate is held while waiting. Phases
only move forward and `Failed`/`Closed` are terminal, so a late callback cannot
resurrect a closed or failed window. A backend without a tracker never reports
more than `Requested`. `OwnedWindow::watch()` returns a cloneable, lease-free
`WindowWatch` (`id`, `phase`, `wait_change`) that keeps reading the tracker after
the session ended, so an owner can observe the close acknowledgement of a window
it already released (Bar reports unacknowledged closes as `cleanupPending`).

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
  suppressing tap (Tiling's passive drag observer is the first such consumer; the
  tap needs Input Monitoring, which is not a typed `Permission`, so a refused
  install surfaces only as `InputError::Native` and the consumer degrades). A
  passive event that finds the subscriber snapshot busy is dropped and counted as
  contention; only a tap disable is reported to observers as a discontinuity.
  Readiness is published only after the `Enable` acknowledgement.
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
            observes: &[],
            reserves_screen: false,
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
  `StepFailure`. There is no duration deadline. `exec_on_startup` uses it through the
  app's `ProcessCommands` adapter, which keeps the established `startup command N: ...`
  wording and treats cancellation as success.
- **Captured** (`run_captured`): a caller deadline and separate stdout/stderr caps
  (each at most `MAX_CAPTURE_BYTES`, 1 MiB) with nonblocking streamed draining on the
  poll quantum. Overflow, timeout and cancellation are typed outcomes; truncated output
  is never a success, and a timeout never implies the command did not act.

- **Line stream** (`open_line_stream`, `commands/stream.rs`): one owned child with piped
  nonblocking stdout/stderr and no stdin, pulled with `LineStream::next_event`. Stdout is
  split into lines as bytes and decoded once complete (UTF-8 split across reads is safe;
  invalid UTF-8 is `InvalidUtf8`). A line is accepted up to `StreamLimits::line_cap`
  (default 1 MiB, at most 64 MiB, the cap itself is accepted); a longer one is never
  buffered past the cap, yields one `LineTooLong` and is discarded to the next newline;
  an unterminated tail at EOF is `Unterminated`. Stderr keeps only the last
  `STDERR_TAIL_BYTES` (8 KiB) plus a dropped-byte count, is always drained and is drained
  at most 64 KiB per pull so it cannot starve stdout or cancellation.
  **Overflow policy: backpressure, not a queue.** There is no producer thread; stdout is
  read only when the consumer pulls, the OS pipe is the bounded buffer, a slow consumer
  blocks the child's writes and no line is dropped (the media consumer coalesces
  snapshots itself). Cancellation wins over queued lines.
  **Idle wakeups and cancel latency.** A silent real child wakes the stream **zero** times.
  An idle pull blocks in `poll(2)` with no timeout on the child's stdout, its stderr and a
  private self-pipe (`ChildProcess::wait_readable`). The first idle wait registers a
  `CancellationToken::on_cancel` listener that writes one byte to the pipe, so cancelling
  wakes `next_event` at once; output and the child's exit (pipe hang-up) wake it too.
  `LineStream::idle_wakeups` counts returned waits. If the pipe cannot be created, or for
  fakes without descriptors, the wait is bounded and follows the `Clock::wait` schedule
  that starts at `POLL_QUANTUM` (10 ms) and doubles up to `IDLE_CEILING` (250 ms).
  `stop` stays bounded by the clock (`term_grace`, `CANCEL_CLEANUP`).
  `LineStream::stop` sends `SIGTERM` to the exact child pid (`ChildProcess::terminate`,
  never to an already reaped child), waits `term_grace` (default 2 s), sends `SIGKILL`,
  waits `CANCEL_CLEANUP` and reports `Exited`, `Killed` or `ReapTimeout` truthfully;
  `ReapTimeout` hands the handle and slot to the reaper like captured runs. Dropping an
  unstopped stream kills the child and retains it if unreaped. No process group is
  created: the only user, the media adapter, runs in-process under perl with no
  descendants (verified 2026-11-10), so owning the exact child is sufficient and a group
  signal would only add a pid-reuse hazard.

A `SpawnGate` is consulted immediately before process creation so a stale generation,
revision or cancellation cannot issue a later step. Cancellation kills only the directly
owned child, polls every 10 ms (`POLL_QUANTUM`) for 2 s (`CANCEL_CLEANUP`), then moves the
exact handle to the executor's own reaper. The executor owns 64 retained-child slots
(`RETAINED_SLOTS`); a slot is reserved before each spawn, released on confirmed reap, and
exhaustion refuses new children instead of dropping a handle. The executor starts no
thread: retained children are re-signalled and swept on the next use or `sweep()`, and
a `Retained` outcome never proves termination.

The Bar system providers (only the gated Wi-Fi `ipconfig` summary; the CPU temperature is a
native SMC read, not a child) are
the first module consumers of `run_captured_prepared`: they build a `PreparedCommand` from a
fixed executable and a fixed argument vector (no shell, no `split_command`), use the
worker's stop token as both the cancellation and the `SpawnGate`, and share the injected
`Clock` with their fake-time schedules. They add no capture wrapper of their own.

## Currently shown vs planned

Shipped here: registry, lifecycle, threading, events, logging, permissions,
errors, descriptors, window capabilities, the safe Accessibility/main-thread
adapters, file-backed configuration (catalog, store, watcher, comment-preserving
edits, templates and schemas), the typed accepted-configuration watch, generic published state with `observes`, host-owned screen reservations with the SDK display ID, identity
paths/instance lock, cache leases, durable storage and the socket protocol/router/client/server,
and the shared input contract with its idle macOS owner. The Tauri
composition root, tray, permission polling, shutdown coordinator and test window
are described in [overview.md](overview.md) and [ui.md](ui.md); the CLI
composition is in [ipc.md](ipc.md). The proxyAudio runtime is in
[proxy-audio.md](../features/proxy-audio.md); the typed React client and test
window are in [ui.md](ui.md).
