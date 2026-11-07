//! The host-owned frontmost-application context publisher.
//!
//! One [`FrontmostContext`] publishes the frontmost application into the shared
//! input service's [`ContextPublisher`], so every input handler reads the same
//! `event.context()`. It is a reference-counted lease: the first
//! [`FrontmostContext::lease`] installs an `ApplicationControl` observation and
//! an observed frontmost watch, the last release stops them with
//! acknowledgement, and with no lessee nothing runs (no observer, no thread).
//!
//! # Publication
//!
//! An activation marks the context [`Stale`](stache_sdk::input::FrontmostState::Stale)
//! immediately from the activation sink. A confirmed refresh with a bundle
//! identifier publishes `Known` with the [`input_context`] bits (an exempt
//! application is still `Known`); everything else, and the final stop, marks it
//! unknown. The listener is generation-tagged and serialised with one small
//! lock, so a late callback from a previous lease can never overwrite a newer
//! lease, and a main-thread `published` for an older epoch can never overwrite
//! the `Stale` of a newer activation.
//!
//! # Races
//!
//! Lease, release and the retained-cleanup retry all serialise on one state
//! lock, held across the bounded install and stop. A release racing a new lease
//! therefore completes first, so there are never two observers, and never none
//! while a lease is held. Cleanup that timed out or failed stays owned here: it
//! blocks a duplicate install, and the next lease (or the teardown owner)
//! retries it first.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use stache_macos::{
    ApplicationControl, ApplicationError, ApplicationObservation, FrontmostListener,
    FrontmostSnapshot, FrontmostWatch, Unconfirmed,
};
use stache_sdk::input::{ContextPublisher, FrontmostApp};
use stache_sdk::{CancellationToken, MainThreadExecutor};

use crate::host::input_context;
use crate::shutdown::{NativeOwner, OwnerAnswer};

/// Why a lease could not be taken. A consumer maps it to a start-prerequisite
/// failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeaseError {
    /// A predecessor's cleanup is still owned and could not be completed, so a
    /// second observer is not installed.
    Cleanup(ApplicationError),
    /// The application observation could not be installed.
    Install(ApplicationError),
    /// The frontmost watch could not be installed.
    Watch(ApplicationError),
}

impl fmt::Display for LeaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cleanup(error) => write!(f, "frontmost context cleanup is pending: {error}"),
            Self::Install(error) => write!(f, "frontmost observation cannot start: {error}"),
            Self::Watch(error) => write!(f, "frontmost watch cannot start: {error}"),
        }
    }
}

impl std::error::Error for LeaseError {}

/// What releasing a lease did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReleaseOutcome {
    /// Other lessees remain; the observation keeps running.
    Held,
    /// This was the last lease and the observation was stopped with
    /// acknowledgement (or none was running).
    Stopped,
    /// This was the last lease but stopping was not acknowledged. The context
    /// is unknown, the observation stays owned and inert, and the next lease or
    /// the teardown owner retries it.
    Retained(ApplicationError),
}

struct Gate {
    /// The lease generation allowed to publish; `0` when none.
    generation: u64,
    /// The highest activation epoch whose invalidation was ingested.
    latest_epoch: u64,
}

struct Listener {
    generation: u64,
    gate: Arc<Mutex<Gate>>,
    publisher: ContextPublisher,
}

impl FrontmostListener for Listener {
    #[allow(clippy::significant_drop_tightening)] // The held guard is the critical section.
    fn invalidated(&self, epoch: u64) {
        let mut gate = lock(&self.gate);
        if gate.generation != self.generation {
            return;
        }
        gate.latest_epoch = gate.latest_epoch.max(epoch);
        self.publisher.mark_stale();
    }

    #[allow(clippy::significant_drop_tightening)] // The held guard is the critical section.
    fn published(&self, snapshot: &FrontmostSnapshot) {
        let gate = lock(&self.gate);
        if gate.generation != self.generation {
            return;
        }
        let dead = matches!(
            snapshot.state,
            stache_macos::FrontmostState::Unconfirmed(Unconfirmed::Inactive)
        );
        if !dead && snapshot.epoch < gate.latest_epoch {
            // A newer activation was ingested; its `Stale` stands.
            return;
        }
        match (snapshot.is_confirmed(), snapshot.bundle_identifier.as_ref()) {
            (true, Some(bundle)) => {
                let mut exemptions = 0;
                if snapshot.is_finder {
                    exemptions |= input_context::FINDER.0;
                }
                if snapshot.is_current_process {
                    exemptions |= input_context::CURRENT_PROCESS.0;
                }
                self.publisher.publish_frontmost(FrontmostApp {
                    bundle_id: Arc::clone(bundle),
                    exemptions: stache_sdk::input::ExemptionFlags(exemptions),
                });
            }
            _ => {
                self.publisher.mark_unknown();
            }
        }
    }
}

struct Active {
    observation: ApplicationObservation,
    // Held so the watch outlives the lease; stopping the observation ends it.
    _watch: FrontmostWatch,
}

#[derive(Default)]
struct State {
    leases: usize,
    active: Option<Active>,
    /// An observation whose stop was not acknowledged.
    retained: Option<ApplicationObservation>,
    next_generation: u64,
}

struct Inner {
    control: ApplicationControl,
    publisher: ContextPublisher,
    gate: Arc<Mutex<Gate>>,
    state: Mutex<State>,
}

/// The shared, cloneable handle to the host frontmost publisher.
#[derive(Clone)]
pub struct FrontmostContext {
    inner: Arc<Inner>,
}

impl FrontmostContext {
    /// Creates an inert publisher over `control` and the shared input service's
    /// publisher.
    #[must_use]
    pub fn new(control: ApplicationControl, publisher: ContextPublisher) -> Self {
        Self {
            inner: Arc::new(Inner {
                control,
                publisher,
                gate: Arc::new(Mutex::new(Gate { generation: 0, latest_epoch: 0 })),
                state: Mutex::new(State::default()),
            }),
        }
    }

    /// Creates the production publisher over its own system `ApplicationControl`.
    #[must_use]
    pub fn system(main_thread: Arc<dyn MainThreadExecutor>, publisher: ContextPublisher) -> Self {
        Self::new(ApplicationControl::system(main_thread), publisher)
    }

    /// Takes one lease. The first lease installs the observation and watch.
    ///
    /// Call off the main thread: the first lease waits (bounded) for main-thread
    /// acknowledgements. A retained predecessor is cleaned first.
    ///
    /// # Errors
    /// Returns a typed [`LeaseError`]; no lease is held after an error.
    pub fn lease(&self, cancellation: &CancellationToken) -> Result<FrontmostLease, LeaseError> {
        let inner = &self.inner;
        let mut state = lock(&inner.state);
        if state.leases == 0 {
            inner.install(&mut state, cancellation)?;
        }
        state.leases += 1;
        drop(state);
        Ok(FrontmostLease { inner: Some(Arc::clone(inner)) })
    }

    /// The number of leases currently held.
    #[must_use]
    pub fn leases(&self) -> usize { lock(&self.inner.state).leases }

    /// Whether a cleanup that was not acknowledged is still owned.
    #[must_use]
    pub fn has_retained_cleanup(&self) -> bool { lock(&self.inner.state).retained.is_some() }

    /// Retries retained cleanup. `None` when leases are held (nothing to clean).
    #[allow(clippy::significant_drop_tightening)] // The held guard is the critical section.
    fn cleanup(&self) -> Option<Result<(), ApplicationError>> {
        let inner = &self.inner;
        let mut state = lock(&inner.state);
        if state.leases > 0 {
            return None;
        }
        Some(Inner::clean_retained(&mut state).and_then(|()| inner.control.stop()))
    }
}

impl Inner {
    fn install(
        &self,
        state: &mut State,
        cancellation: &CancellationToken,
    ) -> Result<(), LeaseError> {
        Self::clean_retained(state).map_err(LeaseError::Cleanup)?;
        let observation =
            self.control.install(cancellation, |_| {}).map_err(LeaseError::Install)?;
        state.next_generation += 1;
        let generation = state.next_generation;
        {
            let mut gate = lock(&self.gate);
            gate.generation = generation;
            gate.latest_epoch = 0;
        }
        let listener = Arc::new(Listener {
            generation,
            gate: Arc::clone(&self.gate),
            publisher: self.publisher.clone(),
        });
        match observation.watch_frontmost_observed(cancellation, listener) {
            Ok(watch) => {
                state.active = Some(Active { observation, _watch: watch });
                Ok(())
            }
            Err(error) => {
                self.retire();
                if observation.stop().is_err() {
                    state.retained = Some(observation);
                }
                Err(LeaseError::Watch(error))
            }
        }
    }

    /// Stops a retained observation, keeping it when the stop is not
    /// acknowledged.
    fn clean_retained(state: &mut State) -> Result<(), ApplicationError> {
        if let Some(observation) = &state.retained {
            observation.stop()?;
            state.retained = None;
        }
        Ok(())
    }

    /// Stops publishing from the current generation and marks the context
    /// unknown, in one critical section.
    #[allow(clippy::significant_drop_tightening)] // The held guard is the critical section.
    fn retire(&self) {
        let mut gate = lock(&self.gate);
        gate.generation = 0;
        gate.latest_epoch = 0;
        self.publisher.mark_unknown();
    }

    #[allow(clippy::significant_drop_tightening)] // The held guard is the critical section.
    fn release(&self) -> ReleaseOutcome {
        let mut state = lock(&self.state);
        state.leases = state.leases.saturating_sub(1);
        if state.leases > 0 {
            return ReleaseOutcome::Held;
        }
        let Some(active) = state.active.take() else {
            return ReleaseOutcome::Stopped;
        };
        // Stopping kills the watch, which reports `Inactive` (unknown) through
        // the still-current generation; retiring afterwards fences late calls.
        let stopped = active.observation.stop();
        self.retire();
        match stopped {
            Ok(()) => ReleaseOutcome::Stopped,
            Err(error) => {
                state.retained = Some(active.observation);
                ReleaseOutcome::Retained(error)
            }
        }
    }
}

/// One held lease. Dropping it releases; [`Self::release`] reports the outcome.
#[must_use = "a lease keeps the frontmost publisher running until released"]
pub struct FrontmostLease {
    inner: Option<Arc<Inner>>,
}

impl FrontmostLease {
    /// Releases the lease and reports whether the observation stopped.
    #[must_use]
    pub fn release(mut self) -> ReleaseOutcome {
        self.inner.take().map_or(ReleaseOutcome::Stopped, |inner| inner.release())
    }
}

impl Drop for FrontmostLease {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            let _ = inner.release();
        }
    }
}

impl fmt::Debug for FrontmostLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrontmostLease").finish_non_exhaustive()
    }
}

/// Acknowledges retained frontmost-context cleanup during coordinated teardown.
///
/// Registered under [`NativeSlot::ApplicationObservers`](crate::shutdown::NativeSlot).
/// The answer comes from a short worker thread because stopping waits on the
/// main thread's acknowledgement; this call never blocks. A held lease is not an
/// answer (modules stop first), so the coordinator names the owner
/// unacknowledged at its bound.
pub struct FrontmostContextOwner {
    context: FrontmostContext,
}

impl FrontmostContextOwner {
    /// The stable teardown record name.
    pub const NAME: &'static str = "frontmost-context";

    /// Wraps the shared context.
    #[must_use]
    pub const fn new(context: FrontmostContext) -> Self { Self { context } }
}

impl NativeOwner for FrontmostContextOwner {
    fn name(&self) -> &'static str { Self::NAME }

    fn on_main(&self) -> bool { false }

    fn release(&self) -> OwnerAnswer {
        let answer = OwnerAnswer::new();
        let completion = answer.clone();
        let context = self.context.clone();
        let spawned = std::thread::Builder::new()
            .name("stache-frontmost-cleanup".to_owned())
            .spawn(move || match context.cleanup() {
                Some(Ok(())) => completion.succeed(),
                Some(Err(_)) => completion.fail(),
                None => {}
            });
        if spawned.is_err() {
            answer.fail();
        }
        answer
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "frontmost_context_tests.rs"]
mod tests;
