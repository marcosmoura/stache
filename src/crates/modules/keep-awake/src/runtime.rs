//! Keep Awake policy, owned runtime and bounded reconciliation.
//!
//! One [`KeepAwakeModule`] owns one [`AssertionOwner`] for the module lifetime
//! and at most one live [`Generation`]. Creating the module, configuring it or
//! reading its snapshot performs no native work; `start` installs lock
//! observation, probes the lock state **before** any acquisition and only then
//! establishes the generation's single SDK worker.
//!
//! Native callbacks are refresh hints only: they increment a signal and wake the
//! worker. The worker is the only place that probes the lock, acquires or
//! releases assertions and publishes runtime state. It waits on one module-local
//! signal with a deadline: two seconds in polling fallback and after any pass
//! that left an unknown lock state, a failed acquisition or a pending release (in
//! both modes), and 30 seconds for a settled worker with lock notifications
//! installed. A dropped notification is recovered by the timer within 30 seconds
//! and a notification storm collapses into at most one extra pass. `stop`
//! invalidates the generation first, wakes that waiter, waits
//! for any native work the worker already started, then releases owned assertions
//! and removes observation; the SDK registry joins the worker after `stop`
//! returns. That wait lasts no longer than the registry's own join, and it means
//! `stop` reports the real outcome of that work instead of guessing at it.
//!
//! The registry's stage-1 teardown invalidation is wired to the same generation
//! before the initial reconciliation. It cancels the generation's own native
//! operation token, marks the generation dead and wakes the worker, without
//! probing, releasing, removing or logging anything. A native creation already in
//! flight therefore stops before the next assertion ID, and a candidate can no
//! longer be adopted, even while `stop` is still waiting behind a registry
//! lifecycle lock. The SDK-owned lifecycle token is only observed, never
//! cancelled here: the registry owns it and cancels it before `stop`.
//!
//! # Locks and publication
//!
//! The short policy gate and the published snapshot cell are separate locks. The
//! gate is never held across a native probe, acquisition, release, observation
//! wait or worker join. `acquiring` is published before the first native create
//! and `releasePending` before the first native release or rollback; only a
//! confirmed release with no lease, candidate or unresolved ID returns the
//! assertion to `inactive`.
//!
//! Unresolved IDs stay in the shared owner ledger. They block every successor
//! lease, are retried once per scheduled reconciliation or lifecycle cleanup, and
//! never become `inactive` evidence. Final `Drop` is best effort: it cannot prove
//! release and never claims completion.

// The policy gate is deliberately held while publishing and while the module
// records one transition; every native call happens after it is released.
#![allow(clippy::significant_drop_tightening)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use stache_macos::{
    AcquireError, AssertionLease, AssertionOwner, AssertionRequest,
    LockObservation as NativeObservation, LockObservationError, LockObservationInstaller,
    LockProbe, LockState as NativeLockState, ReleaseReport,
};
use stache_sdk::{
    CancellationToken, ErrorCategory, LogLevel, Module, ModuleContext, ModuleDescriptor,
    ModuleError, ModuleInvalidation, ModuleResult, Operation, ScopedLogger, StatePublisher,
};

use crate::config::KeepAwakeConfig;
use crate::lock;
use crate::status::{
    AssertionState, KeepAwakeCondition, KeepAwakeConditionKind, KeepAwakeRuntimeStatus,
    KeepAwakeSnapshot, LockObservation, LockState,
};

/// The worker name visible in thread listings and diagnostics.
const WORKER_NAME: &str = "keep-awake-reconcile";

/// How long a settled worker waits between scheduled passes while lock
/// notifications are installed (`notificationsAndPolling`). The pass is then
/// only a safety probe for a dropped notification. A pass that left something to
/// retry is followed by one after [`POLLING_RECONCILE_INTERVAL`] instead.
const NOTIFIED_RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

/// How long the owned worker waits between scheduled passes without lock
/// notifications (`pollingFallback`), where the pass is the only lock source,
/// and after any pass that left an unknown lock state, a failed acquisition or a
/// pending release, in both modes.
const POLLING_RECONCILE_INTERVAL: Duration = Duration::from_secs(2);

/// The scheduled interval for a settled worker with or without lock notifications.
pub const fn reconcile_interval(notifications: bool) -> Duration {
    if notifications {
        NOTIFIED_RECONCILE_INTERVAL
    } else {
        POLLING_RECONCILE_INTERVAL
    }
}

const OBSERVER_UNAVAILABLE: &str =
    "Screen lock notifications are unavailable; Keep Awake is polling instead.";
const LOCK_PROBE_FAILED: &str =
    "The screen lock state could not be read; awake prevention is paused until it can be.";
const ACQUIRE_FAILED: &str = "The awake assertion could not be created.";
const RELEASE_FAILED: &str =
    "The awake assertion could not be released yet; sleep may still be prevented.";
const OBSERVATION_REMOVAL_FAILED: &str =
    "Screen lock observation could not be removed; its callback is inert and retry is owned.";

/// One log line produced while the policy gate is held.
type LogLine = (LogLevel, String);

/// Test-only handle to the module's generation signals.
#[cfg(test)]
type SignalsProbe = Arc<Mutex<Vec<Arc<Signals>>>>;

/// Test-only handle to each generation's SDK and native tokens.
#[cfg(test)]
type TokensProbe = Arc<Mutex<Vec<(CancellationToken, CancellationToken)>>>;

/// Automatic prevention of display, idle and system sleep.
///
/// Construction and configuration acquire nothing. The production composition
/// root supplies one owner, one probe and one observation installer built over
/// the host main-thread executor.
pub struct KeepAwakeModule {
    owner: AssertionOwner,
    probe: Arc<dyn LockProbe>,
    observation: LockObservationInstaller,
    snapshot: KeepAwakeSnapshot,
    config: Option<KeepAwakeConfig>,
    runtime: Option<Arc<Generation>>,
    observation_handle: Option<Arc<NativeObservation>>,
    next_generation: u64,
    #[cfg(test)]
    signals_probe: Option<SignalsProbe>,
    #[cfg(test)]
    tokens_probe: Option<TokensProbe>,
    #[cfg(test)]
    fail_spawn: bool,
}

impl KeepAwakeModule {
    /// Creates the module over the shared owner, probe and installer.
    ///
    /// The owner must be created once per module instance and kept across every
    /// runtime generation so unresolved IDs keep gating successors.
    #[must_use]
    pub fn new(
        owner: AssertionOwner,
        probe: Arc<dyn LockProbe>,
        observation: LockObservationInstaller,
    ) -> Self {
        Self::with_snapshot(owner, probe, observation, KeepAwakeSnapshot::default())
    }

    /// Creates the module publishing into the supplied snapshot cell.
    ///
    /// Composition injects one cell so the app reads exactly the module's
    /// publication rather than a mirror.
    #[must_use]
    pub fn with_snapshot(
        owner: AssertionOwner,
        probe: Arc<dyn LockProbe>,
        observation: LockObservationInstaller,
        snapshot: KeepAwakeSnapshot,
    ) -> Self {
        Self {
            owner,
            probe,
            observation,
            snapshot,
            config: None,
            runtime: None,
            observation_handle: None,
            next_generation: 0,
            #[cfg(test)]
            signals_probe: None,
            #[cfg(test)]
            tokens_probe: None,
            #[cfg(test)]
            fail_spawn: false,
        }
    }

    /// The clonable read-only runtime observation.
    #[must_use]
    pub fn snapshot_reader(&self) -> KeepAwakeSnapshot { self.snapshot.clone() }

    /// Receives each started generation's signals so a test can drive fake time.
    #[cfg(test)]
    pub(crate) fn with_signals_probe(mut self, probe: SignalsProbe) -> Self {
        self.signals_probe = Some(probe);
        self
    }

    /// Receives each started generation's SDK and native tokens.
    #[cfg(test)]
    pub(crate) fn with_tokens_probe(mut self, probe: TokensProbe) -> Self {
        self.tokens_probe = Some(probe);
        self
    }

    /// Injects a worker-spawn failure after observation and acquisition.
    #[cfg(test)]
    pub(crate) const fn with_spawn_failure(mut self) -> Self {
        self.fail_spawn = true;
        self
    }

    /// Releases the current generation's resources. Safe to repeat.
    fn stop_resources(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        let Some(runtime) = self.runtime.clone() else {
            return self.stop_without_runtime(ctx);
        };
        // Invalidation is logical: it cancels the generation's own native token,
        // marks the generation dead and wakes the worker. It performs no native
        // work and leaves the SDK lifecycle token to the registry.
        runtime.invalidate();

        // Invalidation stops new native work, but the worker may already be in
        // an uninterruptible call. Its outcome must reach this lifecycle result.
        runtime.wait_settled();

        // An adopted lease is released explicitly; the caller observes the
        // failure through the lifecycle error and the published pending state.
        if let Some(lease) = runtime.take_lease_for_release(false) {
            let report = lease.release();
            runtime.flush(runtime.settle_release(&report));
        }

        // Unresolved IDs from this or an earlier generation get exactly one
        // bounded pass per lifecycle cleanup.
        if !self.owner.pending().is_empty() {
            let _ = runtime.cleanup_pending_pass();
        }

        let pending = self.owner.pending();
        let observation_error = self.remove_observation();
        runtime.flush(runtime.publish_stopped(!pending.is_empty(), observation_error.is_some()));

        let mut message = String::new();
        if !pending.is_empty() {
            let _ = write!(
                message,
                "{} awake assertion ID(s) remain unreleased",
                pending.len()
            );
        }
        if let Some(error) = &observation_error {
            if !message.is_empty() {
                message.push_str("; ");
            }
            let _ = write!(message, "screen lock observation removal failed: {error}");
        }
        if message.is_empty() {
            self.runtime = None;
            ctx.logger().debug(format!("Keep Awake generation {} stopped", runtime.number));
            Ok(())
        } else {
            ctx.logger().error(message.clone());
            Err(module_error(Operation::Stop, ErrorCategory::Platform, message))
        }
    }

    /// Stops a module with no live generation but possibly retained IDs.
    fn stop_without_runtime(&self, ctx: &ModuleContext) -> ModuleResult<()> {
        let pending = self.owner.pending();
        if pending.is_empty() {
            self.snapshot.publish(KeepAwakeRuntimeStatus::default());
            return Ok(());
        }
        self.snapshot.publish(pending_status(pending.len()));
        let report = self.owner.cleanup_pending();
        if report.is_complete() {
            self.snapshot.publish(KeepAwakeRuntimeStatus::default());
            Ok(())
        } else {
            let remaining = self.owner.pending().len();
            self.snapshot.publish(pending_status(remaining));
            let message = format!("{remaining} awake assertion ID(s) remain unreleased");
            ctx.logger().error(message.clone());
            Err(module_error(Operation::Stop, ErrorCategory::Platform, message))
        }
    }

    /// Removes the exact observation generation with an independent wait token.
    ///
    /// The registry token is already cancelled during `stop`; passing it would
    /// make an expected cancellation look like a removal failure and could skip
    /// the acknowledgement entirely.
    fn remove_observation(&mut self) -> Option<LockObservationError> {
        let handle = self.observation_handle.clone()?;
        let wait = CancellationToken::new();
        match handle.stop(&wait) {
            Ok(()) => {
                self.observation_handle = None;
                None
            }
            Err(error) => Some(error),
        }
    }

    /// Cleans up a failed start and reports the original start failure.
    fn abort_start(
        &mut self,
        ctx: &ModuleContext,
        category: ErrorCategory,
        message: &str,
    ) -> ModuleError {
        let suffix = self.stop_resources(ctx).err().map_or_else(String::new, |error| {
            format!("; cleanup also failed: {}", error.message())
        });
        let message = format!("{message}{suffix}");
        ctx.logger().warn(message.clone());
        module_error(Operation::Start, category, message)
    }
}

impl Module for KeepAwakeModule {
    type Config = KeepAwakeConfig;

    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: KeepAwakeConfig::ID,
            provides: &[],
            requires: &[],
            permissions: &[],
            executables: &[],
            surfaces: &[],
            observes: &[],
            reserves_screen: false,
        }
    }

    fn configure(&mut self, config: &KeepAwakeConfig, _ctx: &ModuleContext) -> ModuleResult<()> {
        self.config = Some(config.clone());
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // One ordered install, initial pass and worker spawn.
    fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        if self.runtime.as_ref().is_some_and(|runtime| runtime.is_live()) {
            return Ok(());
        }
        // A previous failed cleanup must resolve before a fresh generation can
        // own resources. The SDK normally does this through its own quiesce.
        if self.runtime.is_some() || self.observation_handle.is_some() {
            self.stop_resources(ctx).map_err(|error| {
                module_error(
                    Operation::Start,
                    error.category(),
                    format!("previous cleanup is unresolved: {}", error.message()),
                )
            })?;
        }
        let Some(config) = self.config.clone() else {
            return Err(module_error(
                Operation::Start,
                ErrorCategory::Lifecycle,
                "no accepted configuration",
            ));
        };
        debug_assert!(config.enabled, "the registry only starts an enabled module");

        let signals = Arc::new(Signals::new());
        #[cfg(test)]
        if let Some(probe) = &self.signals_probe {
            lock(probe).push(Arc::clone(&signals));
        }

        let installed = {
            let wake = Arc::clone(&signals);
            self.observation.install(&ctx.cancellation(), move || wake.signal())
        };
        let (mode, handle, install_failure) = match installed {
            Ok(observation) => (
                LockObservation::NotificationsAndPolling,
                Some(Arc::new(observation)),
                None,
            ),
            Err(error) => {
                let detail = format!("{error}");
                (LockObservation::PollingFallback, None, Some(detail))
            }
        };

        self.next_generation = self.next_generation.saturating_add(1);
        let runtime = Arc::new(Generation {
            number: self.next_generation,
            owner: self.owner.clone(),
            probe: Arc::clone(&self.probe),
            snapshot: self.snapshot.clone(),
            states: ctx.state_publisher::<KeepAwakeRuntimeStatus>().ok(),
            signals: Arc::clone(&signals),
            logger: ctx.logger().clone(),
            cancellation: ctx.cancellation(),
            native_cancellation: CancellationToken::new(),
            settled: Condvar::new(),
            policy: Mutex::new(Policy {
                live: true,
                assertion: AssertionState::Inactive,
                lock: LockState::Unknown,
                observation: mode,
                conditions: BTreeMap::new(),
                lease: None,
                in_flight: false,
                releasing: false,
            }),
        });
        runtime.publish();
        if let Some(detail) = install_failure {
            runtime.flush(runtime.report_observer_unavailable(&detail));
        }
        #[cfg(test)]
        if let Some(probe) = &self.tokens_probe {
            lock(probe).push((runtime.cancellation.clone(), runtime.native_cancellation.clone()));
        }
        self.runtime = Some(Arc::clone(&runtime));
        self.observation_handle.clone_from(&handle);

        // Stage-1 teardown invalidation must reach this generation before the
        // first native acquisition. A listener registered after the handle was
        // already invalidated runs immediately, so a start racing teardown stays
        // inert before it can probe or create anything.
        wire_invalidation(ctx.invalidation(), &runtime);

        if ctx.cancellation().is_cancelled() {
            return Err(self.abort_start(
                ctx,
                ErrorCategory::Lifecycle,
                "start was cancelled before reconciliation",
            ));
        }

        // The initial pass runs synchronously so a required probe or acquisition
        // failure is a typed start error rather than a silently degraded runtime.
        let report = runtime.reconcile(handle.as_deref());
        if let Some(failure) = report.start_failure() {
            return Err(self.abort_start(ctx, ErrorCategory::Platform, &failure));
        }
        // Stage-1 invalidation can arrive while that pass is parked in a native
        // call. The generation is then dead and its native token cancelled: no
        // worker is spawned and the start fails instead of publishing a running
        // module.
        if !runtime.is_live() || ctx.cancellation().is_cancelled() {
            return Err(self.abort_start(
                ctx,
                ErrorCategory::Lifecycle,
                "start was invalidated before reconciliation completed",
            ));
        }

        #[cfg(test)]
        if self.fail_spawn {
            return Err(self.abort_start(
                ctx,
                ErrorCategory::Platform,
                "failed to spawn the reconciliation worker",
            ));
        }

        let worker_runtime = Arc::clone(&runtime);
        let worker_observation = handle;
        let worker_signals = Arc::clone(&signals);
        let job = move |token: CancellationToken| {
            run_worker(worker_runtime, worker_observation, worker_signals, token);
        };
        if let Err(error) = ctx.spawn_worker(WORKER_NAME, job) {
            return Err(self.abort_start(
                ctx,
                ErrorCategory::Platform,
                &format!("failed to spawn the reconciliation worker: {error}"),
            ));
        }
        if ctx.cancellation().is_cancelled() || !runtime.is_live() {
            return Err(self.abort_start(
                ctx,
                ErrorCategory::Lifecycle,
                "start was cancelled while the reconciliation worker was starting",
            ));
        }
        ctx.logger().info(format!(
            "Keep Awake generation {} started ({mode:?}, lock {:?})",
            runtime.number,
            runtime.locked_state()
        ));
        Ok(())
    }

    fn reconfigure(&mut self, config: &KeepAwakeConfig, _ctx: &ModuleContext) -> ModuleResult<()> {
        // `enabled` is the only field, so an enabled-to-enabled reconfigure has
        // nothing to restart. Disable and pause semantics stay SDK-owned.
        self.config = Some(config.clone());
        Ok(())
    }

    fn stop(&mut self, ctx: &ModuleContext) -> ModuleResult<()> { self.stop_resources(ctx) }
}

impl Drop for KeepAwakeModule {
    /// Best-effort cleanup when a host drops the module without a lifecycle stop.
    ///
    /// This attempts one release pass but proves nothing: unresolved IDs stay in
    /// the shared owner ledger, and observation cleanup is dispatched without
    /// waiting for the main thread.
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.invalidate();
            if let Some(lease) = runtime.take_lease_for_release(false) {
                let report = lease.release();
                runtime.flush(runtime.settle_release(&report));
            }
            let _ = self.owner.cleanup_pending();
        }
        if let Some(handle) = self.observation_handle.take() {
            // A cancelled wait token still dispatches the exact removal; it only
            // skips the acknowledgement.
            let wait = CancellationToken::new();
            wait.cancel();
            let _ = handle.stop(&wait);
        }
    }
}

/// One runtime generation: live policy, publication and the owned worker.
struct Generation {
    number: u64,
    owner: AssertionOwner,
    probe: Arc<dyn LockProbe>,
    snapshot: KeepAwakeSnapshot,
    /// The typed published-state handle bound to this runtime generation.
    states: Option<StatePublisher<KeepAwakeRuntimeStatus>>,
    signals: Arc<Signals>,
    logger: ScopedLogger,
    /// The SDK-owned lifecycle token for this runtime, observed but never
    /// cancelled by this module.
    cancellation: CancellationToken,
    /// The module-owned token for this generation's native operations.
    ///
    /// Invalidation cancels only this token, so a blocked native creation stops
    /// before the next assertion ID without touching the SDK lifecycle token.
    native_cancellation: CancellationToken,
    /// Wakes a stopping thread whenever a publication may have settled the work.
    settled: Condvar,
    policy: Mutex<Policy>,
}

/// The short-locked policy state of one generation.
struct Policy {
    live: bool,
    assertion: AssertionState,
    lock: LockState,
    observation: LockObservation,
    conditions: BTreeMap<KeepAwakeConditionKind, String>,
    lease: Option<Arc<AssertionLease>>,
    /// A native acquisition is in flight and unadopted.
    in_flight: bool,
    /// One release or bounded cleanup pass owns native cleanup.
    releasing: bool,
}

/// A short policy-gate borrow that publishes while the gate is held.
struct Gate<'a> {
    policy: MutexGuard<'a, Policy>,
    snapshot: &'a KeepAwakeSnapshot,
    states: Option<&'a StatePublisher<KeepAwakeRuntimeStatus>>,
    settled: &'a Condvar,
    logs: Vec<LogLine>,
}

impl Gate<'_> {
    fn set_condition(
        &mut self,
        kind: KeepAwakeConditionKind,
        message: &'static str,
        detail: Option<&str>,
    ) {
        let changed = {
            let previous = self.policy.conditions.insert(kind, message.to_owned());
            previous.as_deref() != Some(message)
        };
        if changed {
            let detail = detail.map_or_else(String::new, |detail| format!(": {detail}"));
            self.logs.push((
                LogLevel::Warn,
                format!("Keep Awake condition set: {kind:?}{detail}"),
            ));
        }
    }

    fn clear_condition(&mut self, kind: KeepAwakeConditionKind) {
        if self.policy.conditions.remove(&kind).is_some() {
            self.logs
                .push((LogLevel::Info, format!("Keep Awake condition cleared: {kind:?}")));
        }
    }

    /// Publishes the whole status while the gate still serializes transitions.
    fn publish(&self) {
        let status = KeepAwakeRuntimeStatus {
            assertion_state: self.policy.assertion,
            lock_state: self.policy.lock,
            observation: self.policy.observation,
            conditions: self
                .policy
                .conditions
                .iter()
                .map(|(kind, message)| KeepAwakeCondition {
                    kind: *kind,
                    message: message.clone(),
                })
                .collect(),
        };
        self.snapshot.publish(status.clone());
        self.snapshot.forward(self.states, &status);
        self.settled.notify_all();
    }

    fn finish(self) -> Vec<LogLine> { self.logs }
}

/// The outcome of one reconciliation pass, used to fail an initial start.
#[derive(Default)]
struct ReconcileReport {
    probe_unknown: bool,
    acquire_failure: Option<String>,
    release_pending: bool,
}

impl ReconcileReport {
    /// Whether the pass left an unknown lock state, a failed acquisition or a
    /// pending release, which the next pass retries after two seconds.
    const fn needs_retry(&self) -> bool {
        self.probe_unknown || self.acquire_failure.is_some() || self.release_pending
    }

    /// The initial start error this pass requires, if any.
    fn start_failure(&self) -> Option<String> {
        if self.probe_unknown {
            return Some(
                "the screen lock state could not be read; awake prevention did not start"
                    .to_owned(),
            );
        }
        if let Some(failure) = &self.acquire_failure {
            return Some(format!("the awake assertion could not be created: {failure}"));
        }
        if self.release_pending {
            return Some("previous awake assertion cleanup is unresolved".to_owned());
        }
        None
    }
}

/// The outcome of one acquisition attempt.
#[derive(Default)]
struct AcquireOutcome {
    failure: Option<String>,
    release_pending: bool,
}

/// The outcome of one bounded pending cleanup pass.
enum CleanupPass {
    Settled,
    Unresolved,
    Busy,
}

/// Why the owned worker stopped waiting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wakeup {
    Hint,
    Deadline,
    Stopped,
}

/// Coalesced hints plus the owned deadline for one generation.
///
/// The worker is the only waiter. Native callbacks call [`Signals::signal`],
/// which is a pure flag update and wake; `stop` calls [`Signals::stop`] after
/// invalidation so the waiter never relies on an interval elapsing.
pub struct Signals {
    state: Mutex<SignalState>,
    signal: Condvar,
}

#[derive(Default)]
struct SignalState {
    pending: bool,
    stopped: bool,
    /// Fake monotonic time, advanced only by the module's own tests.
    #[cfg(test)]
    now: Duration,
    /// How many waiters are parked, so a test can advance deterministically.
    #[cfg(test)]
    waiting: usize,
}

impl Signals {
    fn new() -> Self {
        Self {
            state: Mutex::new(SignalState::default()),
            signal: Condvar::new(),
        }
    }

    /// Coalesces one refresh hint and wakes the worker.
    pub(crate) fn signal(&self) {
        let mut state = lock(&self.state);
        state.pending = true;
        drop(state);
        self.signal.notify_all();
    }

    /// Wakes the worker so it exits after a generation invalidation.
    pub(crate) fn stop(&self) {
        let mut state = lock(&self.state);
        state.stopped = true;
        drop(state);
        self.signal.notify_all();
    }

    /// Takes the coalesced wake flag without waiting.
    fn take_pending(&self) -> bool {
        let mut state = lock(&self.state);
        let pending = state.pending;
        state.pending = false;
        pending
    }

    const fn take_wakeup(state: &mut SignalState) -> Option<Wakeup> {
        if state.stopped {
            return Some(Wakeup::Stopped);
        }
        if state.pending {
            state.pending = false;
            return Some(Wakeup::Hint);
        }
        None
    }

    #[cfg(not(test))]
    fn wait(&self, timeout: Duration) -> Wakeup {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.state);
        loop {
            if let Some(wakeup) = Self::take_wakeup(&mut state) {
                return wakeup;
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Wakeup::Deadline;
            };
            state = self
                .signal
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    #[cfg(test)]
    fn wait(&self, timeout: Duration) -> Wakeup {
        let mut state = lock(&self.state);
        let deadline = state.now.saturating_add(timeout);
        loop {
            if let Some(wakeup) = Self::take_wakeup(&mut state) {
                return wakeup;
            }
            if state.now >= deadline {
                return Wakeup::Deadline;
            }
            state.waiting = state.waiting.saturating_add(1);
            self.signal.notify_all();
            state = self.signal.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
            state.waiting = state.waiting.saturating_sub(1);
        }
    }

    /// Advances the fake monotonic clock and wakes the worker.
    #[cfg(test)]
    pub(crate) fn advance(&self, by: Duration) {
        let mut state = lock(&self.state);
        state.now = state.now.saturating_add(by);
        drop(state);
        self.signal.notify_all();
    }

    /// Waits (in real time) until the worker is parked on this signal.
    #[cfg(test)]
    pub(crate) fn wait_until_waiting(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.state);
        while state.waiting == 0 {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return;
            };
            state = self
                .signal
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
}

/// Ends a stop's wait if the worker unwinds mid-work.
///
/// The unwind has already dropped any candidate, which attempts its release and
/// leaves unresolved IDs in the owner ledger for `stop` to report. Release builds
/// abort on panic, so this only matters where panics unwind.
struct SettleOnUnwind<'a>(&'a Generation);

impl Drop for SettleOnUnwind<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let mut gate = self.0.gate();
            gate.policy.in_flight = false;
            gate.policy.releasing = false;
            gate.publish();
        }
    }
}

/// The single owned worker of one generation.
#[allow(clippy::needless_pass_by_value)] // The spawned closure owns these handles.
fn run_worker(
    generation: Arc<Generation>,
    observation: Option<Arc<NativeObservation>>,
    signals: Arc<Signals>,
    cancellation: CancellationToken,
) {
    let _unwind = SettleOnUnwind(&generation);
    let settled = reconcile_interval(observation.is_some());
    // The worker starts settled: a start whose first pass fails starts no worker.
    let mut interval = settled;
    loop {
        if !generation.is_live() || cancellation.is_cancelled() {
            break;
        }
        let hinted = observation.as_deref().is_some_and(NativeObservation::take_refresh);
        if hinted {
            // The installer's coalesced slot and the wake flag carry the same
            // hint; consume both so one notification cannot cause two passes.
            signals.take_pending();
        } else {
            match signals.wait(interval) {
                Wakeup::Stopped => break,
                Wakeup::Hint | Wakeup::Deadline => {}
            }
        }
        if !generation.is_live() || cancellation.is_cancelled() {
            break;
        }
        // A notification that arrives while the pass is running is consumed by
        // the next iteration (the installer coalesces it), so at most one extra
        // pass follows a delivered notification without a sleep loop.
        let report = generation.reconcile(observation.as_deref());
        interval = if report.needs_retry() {
            POLLING_RECONCILE_INTERVAL
        } else {
            settled
        };
    }
}

/// Connects the registry's stage-1 invalidation to one runtime generation.
///
/// The listener only invalidates logically: it cancels the generation's own
/// native token so an in-flight native creation stops between assertion IDs,
/// marks the generation dead and wakes the worker. It never cancels the
/// SDK-owned lifecycle token, probes, creates, releases or removes native state
/// and never logs, so it returns without waiting for native work even while a
/// start or stop holds a registry lifecycle lock.
///
/// A listener registered after the handle was already invalidated runs
/// immediately, so a start racing teardown cannot acquire. Listeners run at most
/// once and the handle belongs to this module slot for its whole lifetime, so
/// there is nothing to unregister; the weak capture keeps obsolete generations
/// from being retained by that handle across repeated start and stop.
fn wire_invalidation(invalidation: &ModuleInvalidation, runtime: &Arc<Generation>) {
    let runtime = Arc::downgrade(runtime);
    invalidation.on_invalidate(Arc::new(move || {
        if let Some(runtime) = runtime.upgrade() {
            runtime.invalidate();
        }
    }));
}

impl Generation {
    fn gate(&self) -> Gate<'_> {
        Gate {
            policy: lock(&self.policy),
            snapshot: &self.snapshot,
            states: self.states.as_ref(),
            settled: &self.settled,
            logs: Vec::new(),
        }
    }

    /// Waits until no native acquisition or release is running.
    ///
    /// The wait releases the policy gate, so the worker can finish and publish.
    /// Call only after invalidation, which keeps new native work from starting.
    /// Regular lifecycle cleanup waits here; coordinated teardown bounds its
    /// wait for the entire stop and reports a stalled owner unacknowledged.
    fn wait_settled(&self) {
        let mut policy = lock(&self.policy);
        while policy.in_flight || policy.releasing {
            policy = self.settled.wait(policy).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn is_live(&self) -> bool { lock(&self.policy).live }

    /// Whether the SDK lifecycle or this generation's native token is cancelled.
    ///
    /// Candidate adoption must reject on either: the registry may cancel its own
    /// token before `stop` reaches this module, and invalidation cancels only the
    /// native one.
    fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled() || self.native_cancellation.is_cancelled()
    }

    fn locked_state(&self) -> LockState { lock(&self.policy).lock }

    /// Makes this generation inert and wakes its worker.
    ///
    /// Only the module-owned native token is cancelled: `acquire_with_rollback`
    /// checks it between native creations, so a creation already in flight cannot
    /// continue to the next ID when it returns. The SDK-owned lifecycle token is
    /// left to the registry, which cancels it before `stop`. The policy gate is
    /// taken only long enough to clear `live`, and this performs no native call,
    /// publication or logging, so stage-1 teardown can run it while a start or
    /// stop holds a registry lock.
    fn invalidate(&self) {
        self.native_cancellation.cancel();
        lock(&self.policy).live = false;
        self.signals.stop();
    }

    fn flush(&self, logs: Vec<LogLine>) {
        for (level, message) in logs {
            match level {
                LogLevel::Warn => self.logger.warn(message),
                LogLevel::Info => self.logger.info(message),
                LogLevel::Error => self.logger.error(message),
                LogLevel::Debug => self.logger.debug(message),
            }
        }
    }

    /// Publishes the generation's initial state without changing policy.
    fn publish(&self) { self.gate().publish(); }

    /// Records that observation installation failed for this generation.
    fn report_observer_unavailable(&self, detail: &str) -> Vec<LogLine> {
        let mut gate = self.gate();
        gate.set_condition(
            KeepAwakeConditionKind::ObserverUnavailable,
            OBSERVER_UNAVAILABLE,
            Some(detail),
        );
        gate.publish();
        gate.finish()
    }

    /// One reconciliation pass: cleanup gate, probe, release or acquire.
    #[allow(clippy::too_many_lines)]
    fn reconcile(self: &Arc<Self>, observation: Option<&NativeObservation>) -> ReconcileReport {
        let mut report = ReconcileReport::default();
        let _ = take_hint(observation);
        if !self.is_live() {
            return report;
        }

        let lock_state = map_lock_state(self.probe.probe());
        if !self.is_live() {
            return report;
        }
        report.probe_unknown = lock_state == LockState::Unknown;
        self.flush(self.publish_probe(lock_state));

        // Unresolved IDs from any generation block a fresh lease. Exactly one
        // bounded pass runs per scheduled reconciliation, and a recovered pass
        // probes again before it may acquire.
        let mut lock_state = lock_state;
        let mut cleaned = false;
        if !self.owner.pending().is_empty() {
            match self.worker_cleanup_pass() {
                CleanupPass::Settled => cleaned = true,
                CleanupPass::Unresolved | CleanupPass::Busy => {
                    report.release_pending = true;
                    return report;
                }
            }
            if !self.is_live() {
                return report;
            }
            if !self.owner.pending().is_empty() {
                report.release_pending = true;
                return report;
            }
        }
        if cleaned {
            lock_state = map_lock_state(self.probe.probe());
            if !self.is_live() {
                return report;
            }
            report.probe_unknown = lock_state == LockState::Unknown;
            self.flush(self.publish_probe(lock_state));
        }

        match lock_state {
            LockState::Locked | LockState::Unknown => {
                if let Some(lease) = self.take_lease_for_release(true) {
                    let released = lease.release();
                    self.flush(self.settle_release(&released));
                    if !released.is_complete() {
                        report.release_pending = true;
                    }
                }
            }
            LockState::Unlocked => {
                if !self.has_work() {
                    let outcome = self.try_acquire(observation);
                    report.acquire_failure = outcome.failure;
                    report.release_pending |= outcome.release_pending;
                }
            }
        }

        report
    }

    /// Publishes the lock knowledge and its probe condition.
    fn publish_probe(&self, lock_state: LockState) -> Vec<LogLine> {
        let mut gate = self.gate();
        if !gate.policy.live {
            // A stop already reset lock knowledge to unknown; a late probe must
            // not present an old observation as current.
            return gate.finish();
        }
        gate.policy.lock = lock_state;
        if lock_state == LockState::Unknown {
            gate.set_condition(KeepAwakeConditionKind::LockProbeFailed, LOCK_PROBE_FAILED, None);
        } else {
            gate.clear_condition(KeepAwakeConditionKind::LockProbeFailed);
        }
        gate.publish();
        gate.finish()
    }

    fn has_work(&self) -> bool {
        let policy = lock(&self.policy);
        policy.lease.is_some()
            || policy.in_flight
            || policy.releasing
            || policy.assertion != AssertionState::Inactive
    }

    /// Publishes `acquiring` before the first native creation.
    ///
    /// A cancelled generation is rejected here too: the registry can cancel its
    /// own lifecycle token before `stop` invalidates this generation, and that
    /// must not begin a creation the module-owned token cannot interrupt.
    fn begin_acquire(&self) -> bool {
        let mut gate = self.gate();
        let policy = &mut gate.policy;
        if !policy.live
            || self.is_cancelled()
            || policy.in_flight
            || policy.releasing
            || policy.lease.is_some()
            || policy.assertion != AssertionState::Inactive
            || policy.lock != LockState::Unlocked
        {
            return false;
        }
        policy.assertion = AssertionState::Acquiring;
        policy.in_flight = true;
        gate.publish();
        true
    }

    /// Creates one complete lease, publishing cleanup before an automatic rollback.
    fn try_acquire(self: &Arc<Self>, observation: Option<&NativeObservation>) -> AcquireOutcome {
        if !self.begin_acquire() {
            return AcquireOutcome::default();
        }
        let hook = Arc::clone(self);
        let result = self.owner.acquire_with_rollback(
            &AssertionRequest::system_wake_lock(),
            &self.native_cancellation,
            move || hook.announce_rollback(),
        );
        match result {
            Ok(lease) => self.finish_candidate(observation, lease),
            Err(error) => self.finish_failed_acquire(&error),
        }
    }

    /// The non-panicking publication hook that runs before native rollback.
    fn announce_rollback(&self) {
        let mut gate = self.gate();
        if gate.policy.in_flight && gate.policy.assertion != AssertionState::ReleasePending {
            gate.policy.assertion = AssertionState::ReleasePending;
            gate.policy.releasing = true;
            gate.publish();
        }
    }

    /// Rechecks cancellation, generation, hints and the current probe before adoption.
    #[allow(clippy::needless_pass_by_value)] // Ownership prevents an implicit drop release.
    fn finish_candidate(
        self: &Arc<Self>,
        observation: Option<&NativeObservation>,
        lease: AssertionLease,
    ) -> AcquireOutcome {
        let stale = self.is_cancelled() || !self.is_live() || take_hint(observation);
        if stale {
            return self.discard_candidate(lease, None);
        }
        let validated = map_lock_state(self.probe.probe());
        if validated != LockState::Unlocked {
            self.flush(self.publish_probe(validated));
            return self.discard_candidate(lease, None);
        }
        let mut gate = self.gate();
        let late_hint = take_hint(observation);
        if !gate.policy.live || !gate.policy.in_flight || late_hint || self.is_cancelled() {
            drop(gate);
            return self.discard_candidate(lease, None);
        }
        gate.policy.in_flight = false;
        gate.policy.assertion = AssertionState::Active;
        gate.policy.lease = Some(Arc::new(lease));
        gate.clear_condition(KeepAwakeConditionKind::AssertionAcquireFailed);
        gate.publish();
        let logs = gate.finish();
        self.flush(logs);
        AcquireOutcome::default()
    }

    /// Publishes `releasePending` and explicitly releases an unadopted candidate.
    #[allow(clippy::needless_pass_by_value)] // Ownership prevents an implicit drop release.
    fn discard_candidate(
        self: &Arc<Self>,
        lease: AssertionLease,
        acquire_failure: Option<String>,
    ) -> AcquireOutcome {
        {
            let mut gate = self.gate();
            gate.policy.in_flight = false;
            gate.policy.assertion = AssertionState::ReleasePending;
            gate.policy.releasing = true;
            if let Some(detail) = &acquire_failure {
                gate.set_condition(
                    KeepAwakeConditionKind::AssertionAcquireFailed,
                    ACQUIRE_FAILED,
                    Some(detail),
                );
            }
            gate.publish();
            let logs = gate.finish();
            self.flush(logs);
        }
        let released = lease.release();
        self.flush(self.settle_release(&released));
        AcquireOutcome {
            failure: acquire_failure,
            release_pending: !released.is_complete(),
        }
    }

    /// Settles a failed acquisition or rollback without claiming success.
    fn finish_failed_acquire(&self, error: &AcquireError) -> AcquireOutcome {
        let mut outcome = AcquireOutcome::default();
        let logs = {
            let mut gate = self.gate();
            gate.policy.in_flight = false;
            gate.policy.releasing = false;
            match error {
                AcquireError::Cancelled { rollback } => {
                    outcome.release_pending = !rollback.is_complete();
                    if rollback.is_complete() {
                        gate.policy.assertion = AssertionState::Inactive;
                    } else {
                        gate.policy.assertion = AssertionState::ReleasePending;
                        if !rollback.pending().is_empty() {
                            gate.set_condition(
                                KeepAwakeConditionKind::AssertionReleaseFailed,
                                RELEASE_FAILED,
                                None,
                            );
                        }
                    }
                }
                AcquireError::CreationFailed { rollback, .. } => {
                    let detail = format!("{error}");
                    gate.set_condition(
                        KeepAwakeConditionKind::AssertionAcquireFailed,
                        ACQUIRE_FAILED,
                        Some(&detail),
                    );
                    outcome.failure = Some(detail);
                    outcome.release_pending = !rollback.is_complete();
                    if rollback.is_complete() {
                        gate.policy.assertion = AssertionState::Inactive;
                    } else {
                        gate.policy.assertion = AssertionState::ReleasePending;
                        if !rollback.pending().is_empty() {
                            gate.set_condition(
                                KeepAwakeConditionKind::AssertionReleaseFailed,
                                RELEASE_FAILED,
                                None,
                            );
                        }
                    }
                }
                AcquireError::PendingCleanup { .. } => {
                    gate.policy.assertion = AssertionState::ReleasePending;
                    outcome.release_pending = true;
                }
                AcquireError::Busy => {
                    // Another pass still owns the shared ledger. Nothing of ours
                    // was created; the next scheduled pass retries.
                    gate.policy.assertion = AssertionState::Inactive;
                }
            }
            gate.publish();
            gate.finish()
        };
        self.flush(logs);
        outcome
    }

    /// Takes the adopted lease and publishes `releasePending` before any native call.
    ///
    /// The worker passes `require_live`, so once a stop invalidates the generation
    /// only that stop can take the lease and report its release.
    fn take_lease_for_release(&self, require_live: bool) -> Option<Arc<AssertionLease>> {
        let mut gate = self.gate();
        if require_live && !gate.policy.live {
            return None;
        }
        let lease = gate.policy.lease.take()?;
        gate.policy.assertion = AssertionState::ReleasePending;
        gate.policy.releasing = true;
        gate.publish();
        let logs = gate.finish();
        self.flush(logs);
        Some(lease)
    }

    /// Settles one release or cleanup report without promoting Busy/InProgress.
    fn settle_release(&self, report: &ReleaseReport) -> Vec<LogLine> {
        let mut gate = self.gate();
        gate.policy.releasing = false;
        if report.is_complete() {
            gate.clear_condition(KeepAwakeConditionKind::AssertionReleaseFailed);
            if gate.policy.lease.is_none() && !gate.policy.in_flight {
                gate.policy.assertion = AssertionState::Inactive;
            }
        } else if report.pending().is_empty() {
            // Busy or in-progress: unconfirmed, but not a proven failure.
            gate.policy.assertion = AssertionState::ReleasePending;
        } else {
            gate.set_condition(
                KeepAwakeConditionKind::AssertionReleaseFailed,
                RELEASE_FAILED,
                None,
            );
            gate.policy.assertion = AssertionState::ReleasePending;
        }
        gate.publish();
        gate.finish()
    }

    /// The worker's cleanup pass, which a stop that won the gate takes over.
    fn worker_cleanup_pass(&self) -> CleanupPass {
        if self.begin_cleanup(true) {
            self.finish_cleanup()
        } else {
            CleanupPass::Busy
        }
    }

    /// The lifecycle cleanup pass, which runs on an invalidated generation.
    fn cleanup_pending_pass(&self) -> CleanupPass {
        if self.begin_cleanup(false) {
            self.finish_cleanup()
        } else {
            CleanupPass::Busy
        }
    }

    /// Claims the one cleanup pass and publishes `releasePending` before it runs.
    fn begin_cleanup(&self, require_live: bool) -> bool {
        let mut gate = self.gate();
        if gate.policy.releasing || (require_live && !gate.policy.live) {
            return false;
        }
        gate.policy.releasing = true;
        gate.policy.assertion = AssertionState::ReleasePending;
        gate.publish();
        let logs = gate.finish();
        self.flush(logs);
        true
    }

    /// One bounded native cleanup pass for unresolved IDs.
    fn finish_cleanup(&self) -> CleanupPass {
        let report = self.owner.cleanup_pending();
        self.flush(self.settle_release(&report));
        if report.is_complete() {
            CleanupPass::Settled
        } else if report.pending().is_empty() {
            CleanupPass::Busy
        } else {
            CleanupPass::Unresolved
        }
    }

    /// Publishes the final stopped state without claiming unconfirmed release.
    fn publish_stopped(&self, pending: bool, observation_failed: bool) -> Vec<LogLine> {
        let mut gate = self.gate();
        gate.policy.live = false;
        gate.policy.lock = LockState::Unknown;
        gate.policy.observation = LockObservation::Inactive;
        gate.clear_condition(KeepAwakeConditionKind::ObserverUnavailable);
        gate.clear_condition(KeepAwakeConditionKind::LockProbeFailed);
        gate.clear_condition(KeepAwakeConditionKind::AssertionAcquireFailed);
        if pending {
            gate.policy.assertion = AssertionState::ReleasePending;
            gate.set_condition(
                KeepAwakeConditionKind::AssertionReleaseFailed,
                RELEASE_FAILED,
                None,
            );
        } else {
            gate.policy.assertion = AssertionState::Inactive;
            gate.clear_condition(KeepAwakeConditionKind::AssertionReleaseFailed);
        }
        if observation_failed {
            gate.set_condition(
                KeepAwakeConditionKind::ObservationDispatchFailed,
                OBSERVATION_REMOVAL_FAILED,
                None,
            );
        } else {
            gate.clear_condition(KeepAwakeConditionKind::ObservationDispatchFailed);
        }
        gate.publish();
        gate.finish()
    }
}

fn take_hint(observation: Option<&NativeObservation>) -> bool {
    observation.is_some_and(NativeObservation::take_refresh)
}

const fn map_lock_state(state: NativeLockState) -> LockState {
    match state {
        NativeLockState::Locked => LockState::Locked,
        NativeLockState::Unlocked => LockState::Unlocked,
        NativeLockState::Unknown(_) => LockState::Unknown,
    }
}

fn pending_status(count: usize) -> KeepAwakeRuntimeStatus {
    KeepAwakeRuntimeStatus {
        assertion_state: AssertionState::ReleasePending,
        lock_state: LockState::Unknown,
        observation: LockObservation::Inactive,
        conditions: if count == 0 {
            Vec::new()
        } else {
            vec![KeepAwakeCondition {
                kind: KeepAwakeConditionKind::AssertionReleaseFailed,
                message: RELEASE_FAILED.to_owned(),
            }]
        },
    }
}

fn module_error(
    operation: Operation,
    category: ErrorCategory,
    message: impl Into<String>,
) -> ModuleError {
    ModuleError::new(KeepAwakeConfig::ID, operation, category, message)
}
