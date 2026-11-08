//! The Keybindings runtime: lifecycle, accepted-revision flips and routing.
//!
//! [`KeybindingsRuntime`] is the host-lifetime service the composition root
//! builds once over the shared input service, the Carbon adapter, the Caps owner
//! and the bounded [`Dispatcher`]. Building it is inert: no tap, hotkey, worker,
//! remap or process exists until an enabled module starts.
//!
//! # Threads
//!
//! * the registry thread runs `configure`, `start`, `reconfigure` and `stop`;
//! * one **coordinator** worker per run waits on the module's accepted-config
//!   watch with a bounded, cancellation-wakeable wait. It adopts accepted
//!   revisions and polls the Caps tap while Caps is owned;
//! * the dispatcher owns its lazy command tasks; the Carbon callback runs on the
//!   main thread and the Caps callback on the tap thread. Neither waits.
//!
//! No thread waits for a thread that needs main: Carbon calls hop to main with
//! the shared 2 s / 10 ms bounds and the main thread never joins any of them.
//!
//! # Accepted reload
//!
//! The registry calls `reconfigure(candidate)` **before** it publishes the
//! candidate, and only a rejected result keeps the old state:
//!
//! 1. `reconfigure` plans the candidate and stages inert resources only: the
//!    Carbon candidate is *prepared* (nothing registered), the Caps remap is
//!    not touched, and no candidate-derived command can run because no route
//!    exists for it. A failure here rejects the candidate and changes nothing.
//! 2. The registry publishes `(revision, config)` through the accepted cell.
//! 3. The coordinator observes that revision. Between 2 and 3 the router table
//!    still holds the old routes tagged with the old revision, and every press
//!    compares its route's revision to the cell's current one, so **no press
//!    from either binding set executes**. A press whose revision no longer
//!    matches is counted, never run.
//! 4. After observing, the coordinator clears the routes, invalidates the old
//!    dispatcher generation (queued sequences are discarded and directly spawned
//!    children are cancelled), installs the new routes for the observed
//!    revision, commits the staged Carbon candidate and only then applies or
//!    removes the Caps remap for that committed revision.
//!
//! A post-commit failure (Carbon commit, Caps apply or restore) is an accepted
//! configuration with a degraded or cleanup outcome in the runtime snapshot; it
//! is never reported as a rejection. A published revision without matching
//! staged resources (disable or a permission block, where the registry stops the
//! module instead) leaves every route inert.
//!
//! # Stop
//!
//! Stop clears the routes and invalidates the dispatcher first, cancels the
//! run, then releases Carbon and Caps with fresh bounded tokens (the runtime
//! token is already cancelled). Repeated or partial stop is safe. Unresolved
//! cleanup is retained by the host-lifetime Carbon store and Caps owner, shown in
//! the snapshot, returned as a stop error and never blocks unrelated shutdown.

// The live-state lock is held across a whole adoption on purpose: the
// coordinator, `start` and `stop` are serialized by it.
#![allow(clippy::significant_drop_tightening)]

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use stache_macos::caps_remap::CapsRemap;
use stache_macos::hotkeys::{
    BindingOutcome, CommitReport, HandlerStatus, HotkeyControl, HotkeyError, HotkeyRequest,
    HotkeySink, HotkeyTarget, StagedBinding, StagedId, StopReport,
};
use stache_sdk::input::{InputPriority, InputService, TapKind};
use stache_sdk::{
    AcceptedConfig, AcceptedWait, AcceptedWatch, CancellationToken, ErrorCategory, ModuleContext,
    ModuleDescriptor, ModuleError, ModuleId, ModuleResult, Operation, Permission,
    PermissionRequirement,
};

use crate::KeybindingsConfig;
use crate::caps::{CapsOwner, CapsSink, CapsStatus};
use crate::dispatch::Dispatcher;
use crate::plan::{Plan, PlanDiagnostic};
use crate::shortcut::{CapsKey, Route};
use crate::status::{
    BindingRoute, BindingState, BindingStatus, DiagnosticLog, KeybindingsSnapshot,
    MAX_BINDING_STATUSES, Published, RemapPhase, RemapStatus, RuntimeState, TapState,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

/// Press counters the router keeps. Atomics only, so reading never waits.
#[derive(Default)]
pub struct RouterCounters {
    window_drops: AtomicU64,
    unrouted: AtomicU64,
    lost: AtomicU64,
    submitted: AtomicU64,
}

/// A copy of [`RouterCounters`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CounterValues {
    /// Presses refused because their route's revision was not the published one.
    pub window_drops: u64,
    /// Presses whose route is unknown.
    pub unrouted: u64,
    /// Presses the dispatcher refused: lost invocations.
    pub lost: u64,
    /// Presses handed to the dispatcher.
    pub submitted: u64,
}

impl RouterCounters {
    /// Copies the counters.
    #[must_use]
    pub fn read(&self) -> CounterValues {
        CounterValues {
            window_drops: self.window_drops.load(Ordering::Relaxed),
            unrouted: self.unrouted.load(Ordering::Relaxed),
            lost: self.lost.load(Ordering::Relaxed),
            submitted: self.submitted.load(Ordering::Relaxed),
        }
    }
}

struct RouteEntry {
    revision: u64,
    generation: u64,
    commands: Arc<[String]>,
}

/// The immutable route table of one adopted revision.
#[derive(Default)]
struct Table {
    carbon: HashMap<u64, RouteEntry>,
    caps: HashMap<u16, RouteEntry>,
}

impl Table {
    fn build(plan: &Plan, split: &Split, revision: u64, generation: u64) -> Self {
        let entry = |index: usize| RouteEntry {
            revision,
            generation,
            commands: plan.bindings[index].commands.clone().into(),
        };
        let mut table = Self::default();
        for request in &split.requests {
            table.carbon.insert(request.key, entry(index_of(request.key)));
        }
        for &(index, key) in &split.caps {
            table.caps.insert(key.0, entry(index));
        }
        table
    }
}

/// The non-blocking consumer of accepted presses for both routes.
///
/// It is the Carbon [`HotkeySink`] and the Caps [`CapsSink`]. Every press looks
/// its route up in the installed table, compares the route's revision with the
/// accepted cell's current revision and only then submits to the dispatcher in
/// the route's generation. It never blocks, queries native state or runs a
/// command; a refused submission is a counted lost invocation.
pub struct Router {
    dispatcher: Dispatcher,
    watch: OnceLock<AcceptedWatch<KeybindingsConfig>>,
    table: Mutex<Arc<Table>>,
    closed: AtomicBool,
    counters: Arc<RouterCounters>,
}

impl Router {
    /// A router in front of `dispatcher`, with no routes.
    #[must_use]
    pub fn new(dispatcher: Dispatcher) -> Arc<Self> {
        Arc::new(Self {
            dispatcher,
            watch: OnceLock::new(),
            table: Mutex::new(Arc::new(Table::default())),
            closed: AtomicBool::new(false),
            counters: Arc::new(RouterCounters::default()),
        })
    }

    /// The counters shared with the status snapshot.
    #[must_use]
    pub fn counters(&self) -> Arc<RouterCounters> { Arc::clone(&self.counters) }

    /// How many routes are installed.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn route_count(&self) -> usize {
        let table = lock(&self.table);
        table.carbon.len() + table.caps.len()
    }

    fn install(&self, table: Table) { *lock(&self.table) = Arc::new(table); }

    fn clear(&self) { self.install(Table::default()); }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.clear();
    }

    fn route(&self, lookup: impl FnOnce(&Table) -> Option<&RouteEntry>) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        let table = Arc::clone(&lock(&self.table));
        let Some(route) = lookup(&table) else {
            self.counters.unrouted.fetch_add(1, Ordering::Relaxed);
            return;
        };
        // The accepted cell is the single source of truth for the revision: a
        // route is live only while its revision is the published one.
        let published = self.watch.get().and_then(AcceptedWatch::read).map(|pair| pair.revision);
        if published != Some(route.revision) {
            self.counters.window_drops.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.counters.submitted.fetch_add(1, Ordering::Relaxed);
        if self
            .dispatcher
            .submit_tagged(route.generation, route.revision, &route.commands)
            .is_err()
        {
            self.counters.lost.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl HotkeySink for Router {
    fn pressed(&self, token: stache_macos::hotkeys::BindingToken) {
        self.route(|table| table.carbon.get(&token.key()));
    }
}

impl CapsSink for Router {
    fn execute(&self, key: CapsKey) { self.route(|table| table.caps.get(&key.0)); }
}

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

/// A plan split by route, with Carbon request keys unique across the run.
struct Split {
    serial: u64,
    requests: Vec<HotkeyRequest>,
    caps: Vec<(usize, CapsKey)>,
}

const fn index_of(key: u64) -> usize { (key & 0xFFFF_FFFF) as usize }

fn split(plan: &Plan, serial: u64, priority: InputPriority) -> Split {
    let mut requests = Vec::new();
    let mut caps = Vec::new();
    for (index, binding) in plan.bindings.iter().enumerate() {
        let key = (serial << 32) | index as u64;
        match binding.route {
            Route::Ordinary { chord } => requests.push(HotkeyRequest {
                key,
                target: HotkeyTarget::Physical {
                    code: chord.key,
                    modifiers: chord.modifiers,
                },
                priority,
            }),
            Route::Section { modifiers } => requests.push(HotkeyRequest {
                key,
                target: HotkeyTarget::Section { modifiers },
                priority,
            }),
            Route::Caps { key } => caps.push((index, key)),
        }
    }
    Split { serial, requests, caps }
}

fn caps_keys(split: &Split) -> Vec<u16> {
    let keys: BTreeSet<u16> = split.caps.iter().map(|(_, key)| key.0).collect();
    keys.into_iter().collect()
}

/// Whether a configuration has any `CapsLock+<key>` binding. Pure and cheap.
fn has_caps_binding(config: &KeybindingsConfig) -> bool {
    config
        .bindings
        .keys()
        .any(|raw| matches!(crate::shortcut::parse(raw), Ok(Route::Caps { .. })))
}

// ---------------------------------------------------------------------------
// Parts and shared state
// ---------------------------------------------------------------------------

/// Observes an accepted revision the coordinator just saw, before it adopts it.
pub type ObservedHook = Arc<dyn Fn(u64) + Send + Sync>;

/// Bounds and test seams of the coordinator.
#[derive(Clone)]
pub struct RuntimeTiming {
    /// How long the coordinator waits for an accepted revision before it polls
    /// the Caps tap. The wait is woken at once by a publication or a stop.
    pub poll: Duration,
    /// A test seam called with the observed revision before the coordinator
    /// adopts it, so a test can hold the publication-to-observation window
    /// open. Production leaves it `None`.
    pub observed: Option<ObservedHook>,
    /// A test seam called with the observed revision while `flip` holds the
    /// operations lock, after its cancellation check and before it invalidates,
    /// so a test can overlap a stop with an adoption. Production leaves it
    /// `None`.
    pub adopting: Option<ObservedHook>,
}

impl Default for RuntimeTiming {
    fn default() -> Self {
        Self {
            poll: Duration::from_millis(500),
            observed: None,
            adopting: None,
        }
    }
}

impl std::fmt::Debug for RuntimeTiming {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeTiming")
            .field("poll", &self.poll)
            .finish_non_exhaustive()
    }
}

/// The host-lifetime resources the composition root assembles.
///
/// `remap` is the same `Arc<CapsRemap>` the host's startup recovery step uses, so
/// recovery and the runtime see one pending state.
pub struct KeybindingsParts {
    /// The bounded dispatcher; its reservations back the input service.
    pub dispatcher: Dispatcher,
    /// The router that is also the Carbon and Caps sink.
    pub router: Arc<Router>,
    /// The off-main handle to the main-owned Carbon store.
    pub hotkeys: Arc<HotkeyControl>,
    /// The Caps ownership owner.
    pub caps: Arc<CapsOwner>,
    /// The shared Caps remap.
    pub remap: Arc<CapsRemap>,
    /// The shared input service.
    pub input: Arc<InputService>,
    /// Runtime diagnostics, shared with the dispatcher's sink.
    pub log: DiagnosticLog,
    /// The priority published for Carbon reservations.
    pub priority: InputPriority,
    /// Wait bounds.
    pub timing: RuntimeTiming,
}

struct Inner {
    parts: KeybindingsParts,
    snapshot: KeybindingsSnapshot,
}

/// The host-lifetime Keybindings service. Cloning shares it.
#[derive(Clone)]
pub struct KeybindingsRuntime {
    inner: Arc<Inner>,
}

impl KeybindingsRuntime {
    /// The module identifier.
    pub const ID: ModuleId = KeybindingsConfig::ID;

    /// Builds the service. Inert: nothing native is created.
    #[must_use]
    pub fn new(parts: KeybindingsParts) -> Self {
        let snapshot = KeybindingsSnapshot::new(
            parts.dispatcher.clone(),
            parts.router.counters(),
            parts.log.clone(),
        );
        Self {
            inner: Arc::new(Inner { parts, snapshot }),
        }
    }

    /// A new module over this service. The registry owns it.
    #[must_use]
    pub fn module(&self) -> KeybindingsModule {
        KeybindingsModule {
            inner: Arc::clone(&self.inner),
            config: None,
            run: None,
        }
    }

    /// Installs the module's typed accepted-configuration watch. Call it before
    /// the registry starts modules. A second install is ignored.
    pub fn install_accepted_watch(&self, watch: AcceptedWatch<KeybindingsConfig>) {
        let router = &self.inner.parts.router;
        if router.watch.set(watch.clone()).is_ok() {
            // A sequence tagged with an older revision starts no further child
            // once a newer revision is published, even before observation.
            router.dispatcher.set_currency_guard(Arc::new(move |revision| {
                watch.read().is_some_and(|pair| pair.revision == revision)
            }));
        }
    }

    /// The read-only status source.
    #[must_use]
    pub fn snapshot(&self) -> KeybindingsSnapshot { self.inner.snapshot.clone() }

    /// Stage 1 of host shutdown: closes admission. Takes no lock held across
    /// native work, joins nothing and performs no I/O.
    pub fn begin_shutdown(&self) { self.inner.parts.router.close(); }

    /// Stage 3 of host shutdown, after the registry stopped the module: closes
    /// the dispatcher and waits at most its shutdown bound for active tasks.
    ///
    /// Returns whether every task finished. Retained cleanup is not waited for:
    /// it stays in the host-lifetime owners and in the snapshot.
    #[must_use]
    pub fn finish_shutdown(&self) -> bool {
        let report = self.inner.parts.dispatcher.shutdown();
        if report.detached > 0 || report.retained > 0 {
            self.inner.parts.log.push(format!(
                "shutdown left {} detached task(s) and {} retained child(ren)",
                report.detached, report.retained
            ));
        }
        report.detached == 0
    }
}

// ---------------------------------------------------------------------------
// Live state
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
enum Outcome {
    Active,
    Rejected(String),
}

impl Outcome {
    fn from_binding(outcome: &BindingOutcome) -> Self {
        match outcome {
            BindingOutcome::Active { .. } => Self::Active,
            BindingOutcome::Occupied { status } => Self::Rejected(format!(
                "the shortcut is already registered elsewhere (status {status})"
            )),
            BindingOutcome::Failed { status, detail } => {
                Self::Rejected(format!("registration failed (status {status}): {detail}"))
            }
            BindingOutcome::Unsupported(what) => Self::Rejected(format!("unsupported: {what}")),
            BindingOutcome::Collision { .. } => {
                Self::Rejected("another binding owns the same physical chord".to_owned())
            }
            BindingOutcome::HandlerUnavailable { status } => {
                Self::Rejected(format!("the hotkey handler is unavailable (status {status})"))
            }
        }
    }
}

/// Everything the coordinator, `start` and `stop` mutate, guarded by one lock.
#[derive(Default)]
#[allow(clippy::struct_excessive_bools, reason = "independent lifecycle flags")]
struct Live {
    running: bool,
    adopted: bool,
    revision: u64,
    plan: Plan,
    serial: u64,
    carbon: HashMap<u64, Outcome>,
    caps_owned: Vec<u16>,
    caps_wanted: Vec<u16>,
    caps_outcome: Option<Outcome>,
    caps_touched: bool,
    caps_applying: bool,
    hotkey_ids: Vec<u32>,
    handler_pending: bool,
    pending_cleanup: usize,
}

/// A candidate staged by `reconfigure`.
struct Staged {
    config: KeybindingsConfig,
    plan: Plan,
    split: Split,
    id: Option<StagedId>,
}

struct Run {
    token: CancellationToken,
    /// Serializes the coordinator against stop and holds the live state.
    ops: Mutex<Live>,
    pending: Mutex<Option<Staged>>,
    serial: AtomicU64,
    /// Whether any Carbon call was made, so stop knows to release it.
    carbon_touched: AtomicBool,
    /// Keeps the runtime-cancellation listener alive for the run.
    guards: Mutex<Vec<stache_sdk::threading::CancelListener>>,
}

impl Run {
    fn next_serial(&self) -> u64 { self.serial.fetch_add(1, Ordering::SeqCst) + 1 }
}

fn error(operation: Operation, category: ErrorCategory, message: impl Into<String>) -> ModuleError {
    ModuleError::new(KeybindingsRuntime::ID, operation, category, message)
}

// ---------------------------------------------------------------------------
// Status publication
// ---------------------------------------------------------------------------

fn remap_status(status: &CapsStatus, applying: bool) -> RemapStatus {
    if applying {
        return RemapStatus {
            phase: RemapPhase::Applying,
            detail: None,
            f18_exposed: false,
        };
    }
    match status {
        CapsStatus::Inactive => RemapStatus::default(),
        CapsStatus::Active => RemapStatus {
            phase: RemapPhase::Owned,
            detail: None,
            f18_exposed: false,
        },
        CapsStatus::CleanupPending { reason, f18_exposed } => RemapStatus {
            phase: RemapPhase::CleanupPending,
            detail: Some(reason.clone()),
            f18_exposed: *f18_exposed,
        },
        CapsStatus::Uncertain { reason, f18_exposed } => RemapStatus {
            phase: RemapPhase::Uncertain,
            detail: Some(reason.clone()),
            f18_exposed: *f18_exposed,
        },
        CapsStatus::OwnershipLost { observed_destination } => RemapStatus {
            phase: RemapPhase::OwnershipLost,
            detail: Some(format!(
                "Caps Lock is mapped to {observed_destination:#x} by someone else"
            )),
            f18_exposed: false,
        },
        CapsStatus::Blocked(reason) => RemapStatus {
            phase: RemapPhase::Inactive,
            detail: Some(format!("blocked: {reason}")),
            f18_exposed: false,
        },
    }
}

const fn route_of(route: Route) -> BindingRoute {
    match route {
        Route::Ordinary { .. } => BindingRoute::Ordinary,
        Route::Section { .. } => BindingRoute::Section,
        Route::Caps { .. } => BindingRoute::Caps,
    }
}

/// Planned bindings that are registered and routed.
fn active_count(live: &Live) -> usize {
    if !live.adopted {
        return 0;
    }
    live.plan
        .bindings
        .iter()
        .enumerate()
        .filter(|(index, binding)| match binding.route {
            Route::Caps { key } => {
                live.caps_owned.contains(&key.0)
                    && !matches!(live.caps_outcome, Some(Outcome::Rejected(_)))
            }
            _ => live.carbon.get(&((live.serial << 32) | *index as u64)) == Some(&Outcome::Active),
        })
        .count()
}

fn binding_statuses(live: &Live, adopted: bool) -> Vec<BindingStatus> {
    let mut statuses = Vec::new();
    for (index, binding) in live.plan.bindings.iter().enumerate() {
        let outcome = match binding.route {
            Route::Caps { key } => match &live.caps_outcome {
                Some(Outcome::Rejected(reason)) => Some(Outcome::Rejected(reason.clone())),
                _ if live.caps_owned.contains(&key.0) => Some(Outcome::Active),
                other => other.clone(),
            },
            _ => live.carbon.get(&((live.serial << 32) | index as u64)).cloned(),
        };
        let (state, detail) = match outcome {
            Some(Outcome::Active) if adopted => (BindingState::Active, None),
            Some(Outcome::Rejected(reason)) => (BindingState::Rejected, Some(reason)),
            _ => (BindingState::Pending, None),
        };
        statuses.push(BindingStatus {
            shortcut: binding.raw.clone(),
            route: Some(route_of(binding.route)),
            state,
            detail,
        });
    }
    for diagnostic in &live.plan.diagnostics {
        statuses.push(match diagnostic {
            PlanDiagnostic::Invalid { raw, error } => BindingStatus {
                shortcut: raw.clone(),
                route: None,
                state: BindingState::Rejected,
                detail: Some(error.to_string()),
            },
            PlanDiagnostic::Duplicate { replaced, winner, route } => BindingStatus {
                shortcut: replaced.clone(),
                route: Some(route_of(*route)),
                state: BindingState::Rejected,
                detail: Some(format!("replaced by the later duplicate `{winner}`")),
            },
        });
    }
    statuses
}

impl Inner {
    fn note(&self, message: impl Into<String>) { self.parts.log.push(message); }

    fn publish(&self, live: &Live) {
        let caps = self.parts.caps.status();
        let mut bindings = binding_statuses(live, live.adopted);
        let omitted = bindings.len().saturating_sub(MAX_BINDING_STATUSES);
        bindings.truncate(MAX_BINDING_STATUSES);
        let planned = live.plan.bindings.len();
        let active = active_count(live);
        let cleanup = live.pending_cleanup > 0 || live.handler_pending;
        let state = if !live.running || planned == 0 && !cleanup {
            RuntimeState::Inactive
        } else if live.adopted && active == planned && !cleanup {
            RuntimeState::Active
        } else {
            RuntimeState::Degraded
        };
        let wants_tap = !live.caps_wanted.is_empty()
            || !live.caps_owned.is_empty()
            || !matches!(caps, CapsStatus::Inactive);
        let tap = if wants_tap {
            TapState::from(&self.parts.input.tap_status(TapKind::Suppressing))
        } else {
            TapState::NotRequired
        };
        self.snapshot.publish(Published {
            revision: live.revision,
            state,
            bindings,
            bindings_omitted: u32::try_from(omitted).unwrap_or(u32::MAX),
            tap,
            remap: remap_status(&caps, live.caps_applying),
            hotkey_ids: live.hotkey_ids.clone(),
            carbon_handler_pending: live.handler_pending,
        });
    }
}

// ---------------------------------------------------------------------------
// Adoption
// ---------------------------------------------------------------------------

struct Adoption<'a> {
    plan: Plan,
    split: &'a Split,
    staged: Option<StagedId>,
    revision: u64,
    generation: u64,
}

fn adopt(inner: &Inner, run: &Run, live: &mut Live, adoption: &Adoption<'_>) {
    live.running = true;
    live.adopted = false;
    live.plan = adoption.plan.clone();
    live.serial = adoption.split.serial;
    live.carbon.clear();
    // Routes for the observed revision exist before the native flip, so a
    // registration is never live without a route; the revision gate keeps
    // anything from an older set inert.
    inner.parts.router.install(Table::build(
        &adoption.plan,
        adoption.split,
        adoption.revision,
        adoption.generation,
    ));
    adopt_carbon(inner, run, live, adoption.split, adoption.staged);
    adopt_caps(inner, run, live, adoption.split);
    live.revision = adoption.revision;
    live.adopted = true;
}

fn adopt_carbon(
    inner: &Inner,
    run: &Run,
    live: &mut Live,
    split: &Split,
    staged: Option<StagedId>,
) {
    if split.requests.is_empty() && !run.carbon_touched.load(Ordering::SeqCst) {
        return;
    }
    run.carbon_touched.store(true, Ordering::SeqCst);
    let hotkeys = &inner.parts.hotkeys;
    let mut result = staged.map_or_else(
        || hotkeys.apply(&run.token, split.requests.clone()),
        |id| hotkeys.commit(&run.token, id),
    );
    if staged.is_some() && matches!(result, Err(HotkeyError::Stale(_))) {
        // The layout or an earlier flip moved on: re-stage and commit in one hop.
        result = hotkeys.apply(&run.token, split.requests.clone());
    }
    match result {
        Ok(report) => record_commit(live, &report),
        Err(failure) => {
            let reason = failure.to_string();
            inner.note(format!("the shortcut registrations were not applied: {reason}"));
            for request in &split.requests {
                live.carbon.insert(request.key, Outcome::Rejected(reason.clone()));
            }
            if !matches!(failure, HotkeyError::Cancelled) {
                // The last-good set may still be registered natively with no
                // route; release it so it cannot swallow a chord for nothing.
                release_carbon(inner, run, live);
            }
        }
    }
    refresh_retained(inner, run, live);
}

/// Whether the Carbon handler or layout observer still owes removal. A handler
/// that never installed owes nothing.
const fn handler_owed(report: &StopReport) -> bool {
    report.observer_pending || matches!(report.handler, HandlerStatus::RemovalPending(_))
}

fn record_commit(live: &mut Live, report: &CommitReport) {
    for (key, outcome) in &report.outcomes {
        live.carbon.insert(*key, Outcome::from_binding(outcome));
    }
    live.pending_cleanup = report.pending_cleanup;
    live.handler_pending = matches!(report.handler, HandlerStatus::RemovalPending(_));
}

fn release_carbon(inner: &Inner, run: &Run, live: &mut Live) {
    match inner.parts.hotkeys.stop(&run.token) {
        Ok(report) => {
            live.pending_cleanup = report.pending_cleanup;
            live.handler_pending = handler_owed(&report);
        }
        Err(failure) => inner.note(format!("releasing the shortcuts failed: {failure}")),
    }
}

/// Reads the retained native IDs only when cleanup is owed.
fn refresh_retained(inner: &Inner, run: &Run, live: &mut Live) {
    live.hotkey_ids.clear();
    if live.pending_cleanup == 0 {
        return;
    }
    if let Ok(status) = inner.parts.hotkeys.status(&run.token) {
        live.hotkey_ids = status.pending_cleanup;
    }
}

fn adopt_caps(inner: &Inner, run: &Run, live: &mut Live, split: &Split) {
    let wanted = caps_keys(split);
    live.caps_wanted.clone_from(&wanted);
    let owner = &inner.parts.caps;
    if wanted == live.caps_owned && live.caps_outcome != Some(Outcome::Active) {
        live.caps_outcome = None;
    }
    if wanted == live.caps_owned && !wanted.is_empty() {
        // The key set is unchanged: the same handler keeps serving, and the new
        // commands are in the route table.
        live.caps_outcome = Some(Outcome::Active);
        return;
    }
    if !live.caps_owned.is_empty() || owner.has_subscription() {
        let report = owner.stop(&run.token);
        live.caps_owned.clear();
        if !report.restore.is_resolved() {
            live.caps_outcome = Some(Outcome::Rejected(format!(
                "the previous Caps Lock remap is not restored yet: {:?}",
                report.status
            )));
            return;
        }
    }
    if wanted.is_empty() {
        live.caps_outcome = None;
        return;
    }
    live.caps_touched = true;
    live.caps_applying = true;
    inner.publish(live);
    let bindings: Vec<CapsKey> = wanted.iter().map(|&key| CapsKey(key)).collect();
    let sink: Arc<dyn CapsSink> = Arc::clone(&inner.parts.router) as Arc<dyn CapsSink>;
    let started = owner.start(&bindings, sink, &run.token);
    live.caps_applying = false;
    match started {
        Ok(()) => {
            live.caps_owned = wanted;
            live.caps_outcome = Some(Outcome::Active);
        }
        Err(failure) => {
            inner.note(format!("Caps Lock bindings were not applied: {failure}"));
            live.caps_outcome = Some(Outcome::Rejected(failure.to_string()));
        }
    }
}

// ---------------------------------------------------------------------------
// Coordinator
// ---------------------------------------------------------------------------

fn coordinate(inner: &Inner, run: &Run, watch: &AcceptedWatch<KeybindingsConfig>, mut seen: u64) {
    loop {
        match watch.wait_for_change(seen, inner.parts.timing.poll, &run.token) {
            AcceptedWait::Changed(pair) => {
                seen = pair.revision;
                if let Some(observed) = &inner.parts.timing.observed {
                    observed(pair.revision);
                }
                flip(inner, run, &pair);
            }
            AcceptedWait::TimedOut => poll(inner, run),
            AcceptedWait::Cancelled | AcceptedWait::Closed => return,
        }
    }
}

/// Adopts one observed accepted revision.
fn flip(inner: &Inner, run: &Run, pair: &AcceptedConfig<KeybindingsConfig>) {
    // Only the candidate staged for this very configuration is taken; a newer
    // one stays for the revision that carries it.
    let staged = {
        let mut pending = lock(&run.pending);
        if pending.as_ref().is_some_and(|staged| staged.config == *pair.config) {
            pending.take()
        } else {
            None
        }
    };
    let mut live = lock(&run.ops);
    if run.token.is_cancelled() || !live.running {
        return;
    }
    if let Some(adopting) = &inner.parts.timing.adopting {
        adopting(pair.revision);
    }
    // The old binding set ends at the observation: routes first, then the old
    // dispatcher generation.
    inner.parts.router.clear();
    let invalidated = inner.parts.dispatcher.invalidate();
    live.adopted = false;
    if !invalidated.drained {
        inner.note("a command spawn did not finish inside the invalidation bound");
    }
    let Some(staged) = staged.filter(|_| pair.config.enabled) else {
        inner.note(format!(
            "accepted revision {} was observed without staged resources; routes stay inert \
             until the module is restarted",
            pair.revision
        ));
        inner.publish(&live);
        return;
    };
    let adoption = Adoption {
        plan: staged.plan,
        split: &staged.split,
        staged: staged.id,
        revision: pair.revision,
        generation: invalidated.generation,
    };
    adopt(inner, run, &mut live, &adoption);
    inner.publish(&live);
}

/// Notices a lost Caps tap. Runs only while Caps is owned, so a pending
/// restore is never retried in a loop.
fn poll(inner: &Inner, run: &Run) {
    let mut live = lock(&run.ops);
    if run.token.is_cancelled() || !live.running {
        return;
    }
    if !live.caps_owned.is_empty()
        && let Some(report) = inner.parts.caps.check_tap(&run.token)
    {
        live.caps_owned.clear();
        live.caps_outcome = Some(Outcome::Rejected(format!(
            "the input tap was lost; Caps Lock was released: {:?}",
            report.status
        )));
        inner.note("the input tap was lost; Caps Lock bindings are inactive");
        inner.publish(&live);
        return;
    }
    let tap = if live.caps_wanted.is_empty() && live.caps_owned.is_empty() {
        TapState::NotRequired
    } else {
        TapState::from(&inner.parts.input.tap_status(TapKind::Suppressing))
    };
    if tap != inner.snapshot.current().tap {
        inner.publish(&live);
    }
}

// ---------------------------------------------------------------------------
// The module
// ---------------------------------------------------------------------------

/// The Keybindings module. Construction is inert.
pub struct KeybindingsModule {
    inner: Arc<Inner>,
    config: Option<KeybindingsConfig>,
    run: Option<Arc<Run>>,
}

impl KeybindingsModule {
    /// Clears routes, invalidates dispatch, cancels the run and releases the
    /// native resources with fresh bounded tokens. Returns what is still owed.
    fn teardown(&self, run: &Run) -> Vec<String> {
        let inner = &self.inner;
        inner.parts.router.clear();
        let _ = inner.parts.dispatcher.invalidate();
        run.token.cancel();
        lock(&run.pending).take();
        let mut live = lock(&run.ops);
        // An adoption that overlapped the stop may have installed routes for a
        // newer generation while this waited; end that one too.
        inner.parts.router.clear();
        let _ = inner.parts.dispatcher.invalidate();
        // The runtime token is already cancelled; cleanup must not inherit it.
        let cleanup = CancellationToken::new();
        let mut owed = Vec::new();
        if run.carbon_touched.load(Ordering::SeqCst) {
            match inner.parts.hotkeys.stop(&cleanup) {
                Ok(report) => {
                    live.pending_cleanup = report.pending_cleanup;
                    live.handler_pending = handler_owed(&report);
                    if report.pending_cleanup > 0 || live.handler_pending {
                        owed.push(format!(
                            "{} registration(s) and the Carbon handler still owe cleanup",
                            report.pending_cleanup
                        ));
                    }
                }
                Err(failure) => {
                    owed.push(format!("shortcut cleanup is not confirmed: {failure}"));
                    live.handler_pending = true;
                }
            }
            if let Ok(status) = inner.parts.hotkeys.status(&cleanup) {
                live.hotkey_ids = status.pending_cleanup;
            }
        }
        let caps_engaged = live.caps_touched
            || inner.parts.caps.has_subscription()
            || !matches!(inner.parts.caps.status(), CapsStatus::Inactive);
        if caps_engaged {
            let report = inner.parts.caps.stop(&cleanup);
            if !report.restore.is_resolved() {
                owed.push(format!(
                    "the Caps Lock remap is not restored: {:?}",
                    report.status
                ));
            }
        }
        live.caps_owned.clear();
        live.caps_wanted.clear();
        live.caps_outcome = None;
        live.caps_applying = false;
        live.running = false;
        live.adopted = false;
        live.plan = Plan::default();
        live.carbon.clear();
        inner.publish(&live);
        owed
    }
}

impl stache_sdk::Module for KeybindingsModule {
    type Config = KeybindingsConfig;

    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: KeybindingsRuntime::ID,
            provides: &[],
            requires: &[],
            permissions: &[],
            executables: &[],
            surfaces: &[],
            observes: &[],
            reserves_screen: false,
        }
    }

    fn required_permissions(&self, config: &KeybindingsConfig) -> Vec<PermissionRequirement> {
        if has_caps_binding(config) {
            vec![PermissionRequirement::shared_input(
                Permission::Accessibility,
            )]
        } else {
            Vec::new()
        }
    }

    fn configure(&mut self, config: &KeybindingsConfig, _ctx: &ModuleContext) -> ModuleResult<()> {
        self.config = Some(config.clone());
        Ok(())
    }

    fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        let inner = Arc::clone(&self.inner);
        if let Some(previous) = self.run.clone() {
            // A failed stop left a run behind; resolve it before a successor.
            let owed = self.teardown(&previous);
            if !owed.is_empty() {
                return Err(error(
                    Operation::Start,
                    ErrorCategory::Platform,
                    format!("earlier cleanup is still pending: {}", owed.join("; ")),
                ));
            }
            self.run = None;
        }
        let Some(config) = self.config.clone() else {
            return Err(error(
                Operation::Start,
                ErrorCategory::Lifecycle,
                "not configured",
            ));
        };
        let Some(watch) = inner.parts.router.watch.get().cloned() else {
            return Err(error(
                Operation::Start,
                ErrorCategory::Lifecycle,
                "the accepted configuration watch is not installed",
            ));
        };
        let Some(pair) = watch.read().filter(|pair| *pair.config == config) else {
            return Err(error(
                Operation::Start,
                ErrorCategory::Lifecycle,
                "the configured value is not the accepted one",
            ));
        };

        let token = CancellationToken::new();
        let run = Arc::new(Run {
            token: token.clone(),
            ops: Mutex::new(Live::default()),
            pending: Mutex::new(None),
            serial: AtomicU64::new(0),
            carbon_touched: AtomicBool::new(false),
            guards: Mutex::new(Vec::new()),
        });
        // Runtime cancellation and host invalidation both end this run's waits.
        let on_cancel = token.clone();
        lock(&run.guards).push(ctx.cancellation().on_cancel(Arc::new(move || on_cancel.cancel())));
        let on_invalidate = token;
        ctx.invalidation().on_invalidate(Arc::new(move || on_invalidate.cancel()));
        self.run = Some(Arc::clone(&run));

        let plan = Plan::from_config(&pair.config);
        let split = split(&plan, run.next_serial(), inner.parts.priority);
        let usable = split.requests.len() + split.caps.len();
        let generation = inner.parts.dispatcher.generation();
        let (adopted_active, failure) = {
            let mut live = lock(&run.ops);
            adopt(&inner, &run, &mut live, &Adoption {
                plan: plan.clone(),
                split: &split,
                staged: None,
                revision: pair.revision,
                generation,
            });
            inner.publish(&live);
            let active = active_count(&live);
            let failure = failure_summary(&live, &plan);
            (active, failure)
        };
        if usable > 0 && adopted_active == 0 {
            let owed = self.teardown(&run);
            if owed.is_empty() {
                self.run = None;
            }
            return Err(error(
                Operation::Start,
                ErrorCategory::Platform,
                format!("no requested shortcut could be activated: {failure}"),
            ));
        }

        let worker_inner = Arc::clone(&inner);
        let worker_run = Arc::clone(&run);
        let worker_watch = watch;
        let seen = pair.revision;
        if let Err(spawn) = ctx.spawn_worker("keybindings-coordinator", move |_| {
            coordinate(&worker_inner, &worker_run, &worker_watch, seen);
        }) {
            let _ = self.teardown(&run);
            self.run = None;
            return Err(spawn);
        }
        Ok(())
    }

    fn reconfigure(&mut self, config: &KeybindingsConfig, ctx: &ModuleContext) -> ModuleResult<()> {
        let inner = &self.inner;
        let Some(run) = self.run.clone() else {
            return Err(error(
                Operation::Reconfigure,
                ErrorCategory::Lifecycle,
                "not started",
            ));
        };
        let plan = Plan::from_config(config);
        let split = split(&plan, run.next_serial(), inner.parts.priority);
        let wants_caps = !split.caps.is_empty();
        if wants_caps
            && inner.parts.caps.status() != CapsStatus::Active
            && let Some(reason) = inner.parts.remap.pending()
        {
            return Err(error(
                Operation::Reconfigure,
                ErrorCategory::Platform,
                format!("Caps Lock ownership is blocked by an unresolved remap: {reason}"),
            ));
        }
        let id = if !split.requests.is_empty() || run.carbon_touched.load(Ordering::SeqCst) {
            let staged = inner
                .parts
                .hotkeys
                .prepare(&ctx.cancellation(), split.requests.clone())
                .map_err(|failure| {
                error(
                    Operation::Reconfigure,
                    ErrorCategory::Platform,
                    format!("the candidate shortcuts could not be staged: {failure}"),
                )
            })?;
            let viable = staged
                .bindings
                .iter()
                .filter(|(_, binding)| !matches!(binding, StagedBinding::Rejected(_)))
                .count();
            if viable == 0 && !split.requests.is_empty() && !wants_caps {
                return Err(error(
                    Operation::Reconfigure,
                    ErrorCategory::Platform,
                    "none of the candidate shortcuts can be registered",
                ));
            }
            Some(staged.id)
        } else {
            None
        };
        *lock(&run.pending) = Some(Staged {
            config: config.clone(),
            plan,
            split,
            id,
        });
        Ok(())
    }

    fn stop(&mut self, _ctx: &ModuleContext) -> ModuleResult<()> {
        let Some(run) = self.run.clone() else {
            return Ok(());
        };
        let owed = self.teardown(&run);
        if owed.is_empty() {
            self.run = None;
            Ok(())
        } else {
            Err(error(Operation::Stop, ErrorCategory::Platform, owed.join("; ")))
        }
    }
}

fn failure_summary(live: &Live, plan: &Plan) -> String {
    let mut reasons: Vec<String> = Vec::new();
    for outcome in live.carbon.values().chain(live.caps_outcome.iter()) {
        if let Outcome::Rejected(reason) = outcome
            && !reasons.contains(reason)
        {
            reasons.push(reason.clone());
        }
    }
    if reasons.is_empty() {
        format!("{} binding(s) planned", plan.bindings.len())
    } else {
        reasons.join("; ")
    }
}
