//! The Caps Lock to F18 ownership transaction.
//!
//! Every operation runs under the identity-wide lease. The order is always:
//!
//! 1. acquire the lease (no mutation when it times out or is cancelled);
//! 2. read and strictly parse the **complete** current mapping;
//! 3. make the intent durable in the journal (no mutation if that fails);
//! 4. change the mapping with `hidutil` (editing only the Caps entry of the
//!    latest read, never a stale snapshot);
//! 5. reread and verify before any phase change or journal retirement.
//!
//! The journal is deleted only after a reread proves the owned mapping absent
//! and no earlier `hidutil` child is still unconfirmed. Everything is idempotent,
//! so a crash after any durable or native step is reconciled by the next
//! operation from the actual state.
//!
//! # Limits stated honestly
//!
//! `hidutil` offers no compare-and-swap. Another program can change the global
//! mapping between our read and write; the lease coordinates only cooperating
//! Stache processes. The reread narrows that window and reports a changed
//! Caps entry as ownership lost, but it cannot close it. A journal that matches
//! the mapping proves only that this application intended to own it.

use std::fmt;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use stache_sdk::{AppPaths, CancellationToken};

use super::hidutil::{HIDUTIL_DEADLINE, Hidutil, HidutilError, ProcessHidutil};
use super::journal::{Journal, Phase};
use super::mapping::{
    CapsState, F18_USAGE, Mapping, caps_state, parse_user_key_mapping, render_set_payload,
    with_surrogate, without_surrogate,
};
use super::storage::{
    CapsLeases, FsJournalStorage, FsLeases, JournalStorage, LEASE_BUDGET, LeaseFailure, LeaseGuard,
};

/// How a restore or recovery ended. Only the first three are resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreOutcome {
    /// The owned mapping is verified absent and the journal is retired.
    Restored,
    /// There is no journal, so nothing of ours is pending.
    NoJournal,
    /// Caps Lock maps somewhere this application did not put it. Nothing was
    /// overwritten and the journal stays until that mapping is gone.
    OwnershipLost {
        /// The foreign destination usage.
        observed_destination: u64,
    },
    /// Our mapping is still pending; restoration did not (provably) change
    /// anything. A later attempt may succeed.
    CleanupPending(String),
    /// A native write may have happened and could not be verified. The journal
    /// and any child handle stay owned.
    Uncertain(String),
    /// The journal or the lease path cannot be trusted, so nothing was touched.
    Blocked(String),
}

impl RestoreOutcome {
    /// Whether no application-owned state remains pending.
    #[must_use]
    pub const fn is_resolved(&self) -> bool {
        matches!(
            self,
            Self::Restored | Self::NoJournal | Self::OwnershipLost { .. }
        )
    }
}

/// Why an apply was refused or did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyError {
    /// The lease was not acquired. Nothing was mutated.
    Lease(LeaseFailure),
    /// Earlier pending state, a corrupt journal or an unrestorable stale
    /// mapping blocks acquisition.
    Blocked(String),
    /// Caps Lock is already mapped (even to F18) and this application cannot
    /// prove it owns it. Nothing was touched.
    Unowned {
        /// The existing destination usage.
        destination: u64,
    },
    /// The host's startup recovery had not finished inside the bound. Nothing
    /// was mutated.
    Recovery(String),
    /// The current mapping could not be read or strictly parsed.
    Read(String),
    /// The write-ahead journal could not be made durable. Nothing was mutated.
    Journal(String),
    /// `hidutil` did not run, or ran and verifiably changed nothing.
    NotApplied(String),
    /// The mapping may be applied but could not be verified; the journal is
    /// retained and a restore is required.
    Uncertain(String),
}

impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lease(failure) => failure.fmt(f),
            Self::Blocked(reason) => write!(f, "Caps ownership is blocked: {reason}"),
            Self::Unowned { destination } => write!(
                f,
                "Caps Lock is already mapped to {destination:#x} and is not owned by Stache"
            ),
            Self::Recovery(reason) | Self::Read(reason) | Self::Journal(reason) => {
                f.write_str(reason)
            }
            Self::NotApplied(reason) => write!(f, "the Caps remap was not applied: {reason}"),
            Self::Uncertain(reason) => write!(f, "the Caps remap is uncertain: {reason}"),
        }
    }
}

impl std::error::Error for ApplyError {}

impl ApplyError {
    /// Whether the mapping may exist and a restore must follow.
    #[must_use]
    pub const fn needs_restore(&self) -> bool { matches!(self, Self::Uncertain(_)) }
}

/// The longest [`CapsRemap::apply`] waits for an armed startup recovery: its
/// lease (5 s) plus the three `hidutil` calls a recovery makes (read, write,
/// verify; 5 s each).
pub const STARTUP_RECOVERY_BOUND: Duration =
    Duration::from_secs(LEASE_BUDGET.as_secs() + 3 * HIDUTIL_DEADLINE.as_secs());

/// How often a waiting apply re-checks cancellation and the bound.
const STARTUP_QUANTUM: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Startup {
    /// No recovery is registered: apply never waits.
    Unarmed,
    /// A recovery is registered and has not finished.
    Pending,
    /// The recovery finished by any outcome.
    Done,
}

/// The one-shot startup-recovery latch.
struct StartupLatch {
    state: Mutex<Startup>,
    done: Condvar,
}

impl StartupLatch {
    fn set(&self, from: Startup, to: Startup) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if *state == from {
            *state = to;
            drop(state);
            self.done.notify_all();
        }
    }
}

/// Marks the startup recovery complete when dropped, including by a panic.
struct RecoveryDone<'a>(&'a StartupLatch);

impl Drop for RecoveryDone<'_> {
    fn drop(&mut self) { self.0.set(Startup::Pending, Startup::Done); }
}

/// The remap owner. One per application; clone the `Arc`.
pub struct CapsRemap {
    hidutil: Arc<dyn Hidutil>,
    storage: Arc<dyn JournalStorage>,
    leases: Arc<dyn CapsLeases>,
    /// Why successor acquisition is blocked, until a restore resolves it.
    pending: Mutex<Option<String>>,
    startup: StartupLatch,
    startup_bound: Duration,
}

struct Session<'a> {
    remap: &'a CapsRemap,
    guard: Box<dyn LeaseGuard>,
    cancel: &'a CancellationToken,
}

impl CapsRemap {
    /// Builds the owner over injected boundaries.
    #[must_use]
    pub fn new(
        hidutil: Arc<dyn Hidutil>,
        storage: Arc<dyn JournalStorage>,
        leases: Arc<dyn CapsLeases>,
    ) -> Self {
        Self {
            hidutil,
            storage,
            leases,
            pending: Mutex::new(None),
            startup: StartupLatch {
                state: Mutex::new(Startup::Unarmed),
                done: Condvar::new(),
            },
            startup_bound: STARTUP_RECOVERY_BOUND,
        }
    }

    /// Replaces the bound an apply waits for an armed startup recovery.
    #[must_use]
    pub const fn with_startup_bound(mut self, bound: Duration) -> Self {
        self.startup_bound = bound;
        self
    }

    /// Arms the one-shot startup-recovery latch: every [`apply`](Self::apply)
    /// waits until [`restore_at_startup`](Self::restore_at_startup) finishes
    /// (or the bound passes). The composition root calls this before the host
    /// can start a module. Without it, apply never waits.
    pub fn begin_startup_recovery(&self) { self.startup.set(Startup::Unarmed, Startup::Pending); }

    /// Releases an armed latch without running recovery. The owner of the
    /// startup step calls this when the step is dropped, so a recovery that
    /// never ran (for example, its thread could not start) cannot hold every
    /// apply until the bound. Idempotent; a finished latch is unchanged.
    pub fn abandon_startup_recovery(&self) { self.startup.set(Startup::Pending, Startup::Done); }

    /// The host's startup recovery: [`restore`](Self::restore), then the latch
    /// is released on every outcome, including a panic.
    pub fn restore_at_startup(&self, cancel: &CancellationToken) -> RestoreOutcome {
        let _done = RecoveryDone(&self.startup);
        self.restore(cancel)
    }

    // The guard is the condvar's wait state and must be held across the loop.
    #[allow(clippy::significant_drop_tightening)]
    fn await_startup_recovery(&self, cancel: &CancellationToken) -> Result<(), ApplyError> {
        let started = Instant::now();
        let mut state = self.startup.state.lock().unwrap_or_else(PoisonError::into_inner);
        while *state == Startup::Pending {
            if cancel.is_cancelled() {
                return Err(ApplyError::Lease(LeaseFailure::Cancelled));
            }
            let left = self.startup_bound.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Err(ApplyError::Recovery(format!(
                    "the startup Caps recovery had not finished after {:?}; nothing was changed",
                    self.startup_bound
                )));
            }
            state = self
                .startup
                .done
                .wait_timeout(state, left.min(STARTUP_QUANTUM))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        Ok(())
    }

    /// The production owner: real `/usr/bin/hidutil`, the durable journal and
    /// the identity-wide lease under the identity's Application Support
    /// directory. Constructing it runs nothing.
    #[must_use]
    pub fn production(paths: &AppPaths) -> Self {
        Self::new(
            Arc::new(ProcessHidutil::system()),
            Arc::new(FsJournalStorage::new(paths.clone())),
            Arc::new(FsLeases::new(paths.clone())),
        )
    }

    /// Why a successor remap is blocked, when it is.
    #[must_use]
    pub fn pending(&self) -> Option<String> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn settle(&self, outcome: &RestoreOutcome) {
        let reason = match outcome {
            RestoreOutcome::Restored
            | RestoreOutcome::NoJournal
            | RestoreOutcome::OwnershipLost { .. } => None,
            RestoreOutcome::CleanupPending(reason)
            | RestoreOutcome::Uncertain(reason)
            | RestoreOutcome::Blocked(reason) => Some(reason.clone()),
        };
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = reason;
    }

    fn block(&self, reason: String) {
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(reason);
    }

    /// Whether a journal file exists, without taking the lease or creating
    /// anything. A journal that cannot be read counts as present.
    #[must_use]
    pub fn journal_present(&self) -> bool { !matches!(self.storage.read(), Ok(None)) }

    /// Takes ownership of Caps Lock by mapping it to F18.
    ///
    /// The caller must already hold a confirmed active input tap: this layer
    /// does not know about taps and a remap without one would send F18 to the
    /// system.
    ///
    /// # Errors
    /// Returns an [`ApplyError`]. [`ApplyError::Uncertain`] means a restore is
    /// required before anything else.
    pub fn apply(&self, cancel: &CancellationToken) -> Result<(), ApplyError> {
        self.await_startup_recovery(cancel)?;
        if let Some(reason) = self.pending() {
            return Err(ApplyError::Blocked(reason));
        }
        let guard = self.leases.acquire(cancel).map_err(ApplyError::Lease)?;
        let session = Session { remap: self, guard, cancel };
        let result = session.apply();
        if let Err(error) = &result
            && error.needs_restore()
        {
            self.block(error.to_string());
        }
        result
    }

    /// Restores Caps Lock if this application's journal says it owns the
    /// mapping, and reports the truthful outcome.
    ///
    /// Idempotent and safe to call when nothing is owned. It reads the journal
    /// first: with no journal it neither takes the lease nor runs `hidutil`.
    pub fn restore(&self, cancel: &CancellationToken) -> RestoreOutcome {
        let outcome = self.restore_inner(cancel);
        self.settle(&outcome);
        outcome
    }

    fn restore_inner(&self, cancel: &CancellationToken) -> RestoreOutcome {
        match read_journal(self.storage.as_ref()) {
            Ok(None) => return RestoreOutcome::NoJournal,
            Err(reason) => return RestoreOutcome::Blocked(reason),
            Ok(Some(_)) => {}
        }
        let guard = match self.leases.acquire(cancel) {
            Ok(guard) => guard,
            Err(failure @ LeaseFailure::Unsafe(_)) => {
                return RestoreOutcome::Blocked(failure.to_string());
            }
            Err(failure) => return RestoreOutcome::CleanupPending(failure.to_string()),
        };
        let session = Session { remap: self, guard, cancel };
        // The journal is read again under the lease: it is the authority.
        match read_journal(self.storage.as_ref()) {
            Ok(None) => RestoreOutcome::NoJournal,
            Err(reason) => RestoreOutcome::Blocked(reason),
            Ok(Some(journal)) => session.reconcile(journal),
        }
    }
}

fn read_journal(storage: &dyn JournalStorage) -> Result<Option<Journal>, String> {
    match storage.read() {
        Ok(None) => Ok(None),
        Ok(Some(bytes)) => Journal::decode(&bytes).map(Some).map_err(|error| error.to_string()),
        Err(error) => Err(format!("the Caps journal cannot be read safely: {error}")),
    }
}

impl Session<'_> {
    fn read_mappings(&self) -> Result<Vec<Mapping>, String> {
        let raw = self
            .remap
            .hidutil
            .get(self.cancel)
            .map_err(|error| format!("cannot read the key mapping: {error}"))?;
        parse_user_key_mapping(&raw).map_err(|error| format!("refusing the key mapping: {error}"))
    }

    fn write_journal(&self, journal: Journal) -> Result<(), String> {
        self.guard.verify().map_err(|error| error.to_string())?;
        self.remap
            .storage
            .write(&journal.encode())
            .map_err(|error| format!("cannot write the Caps journal: {error}"))
    }

    fn set(&self, mappings: &[Mapping]) -> Result<(), HidutilError> {
        self.guard.verify().map_err(|error| HidutilError::not_run(error.to_string()))?;
        self.remap.hidutil.set(&render_set_payload(mappings), self.cancel)
    }

    /// Retires the journal after absence was verified.
    fn retire(&self) -> RestoreOutcome {
        if self.remap.hidutil.unreaped() > 0 {
            return RestoreOutcome::Uncertain(
                "a hidutil child is not confirmed gone; the journal is retained".to_owned(),
            );
        }
        if let Err(error) = self.guard.verify() {
            return RestoreOutcome::Blocked(error.to_string());
        }
        match self.remap.storage.remove() {
            Ok(()) => RestoreOutcome::Restored,
            Err(error) => RestoreOutcome::CleanupPending(format!(
                "Caps Lock is restored but the journal could not be retired: {error}"
            )),
        }
    }

    fn reconcile(&self, journal: Journal) -> RestoreOutcome {
        let mappings = match self.read_mappings() {
            Ok(mappings) => mappings,
            Err(reason) => return RestoreOutcome::CleanupPending(reason),
        };
        match caps_state(&mappings) {
            CapsState::Absent => self.retire(),
            CapsState::Foreign(destination) => self.ownership_lost(journal, destination),
            CapsState::Surrogate => self.restore_owned(journal, &mappings),
        }
    }

    fn ownership_lost(&self, journal: Journal, destination: u64) -> RestoreOutcome {
        if journal.phase != Phase::OwnershipLost {
            // Best effort: the record stays either way.
            let _ = self.write_journal(journal.next(Phase::OwnershipLost));
        }
        RestoreOutcome::OwnershipLost {
            observed_destination: destination,
        }
    }

    fn restore_owned(&self, journal: Journal, mappings: &[Mapping]) -> RestoreOutcome {
        if journal.phase != Phase::RestorePending
            && let Err(reason) = self.write_journal(journal.next(Phase::RestorePending))
        {
            return RestoreOutcome::CleanupPending(reason);
        }
        let written = self.set(&without_surrogate(mappings));
        if let Err(error) = &written
            && !error.may_have_mutated()
        {
            return RestoreOutcome::CleanupPending(error.message.clone());
        }
        let after = match self.read_mappings() {
            Ok(after) => after,
            Err(reason) => {
                return RestoreOutcome::Uncertain(format!(
                    "the restore may have been applied but was not verified: {reason}"
                ));
            }
        };
        match (caps_state(&after), written) {
            (CapsState::Absent, _) => self.retire(),
            (CapsState::Foreign(destination), _) => {
                self.ownership_lost(journal.next(Phase::RestorePending), destination)
            }
            (CapsState::Surrogate, Ok(())) => RestoreOutcome::Uncertain(
                "hidutil reported success but the owned mapping is still present".to_owned(),
            ),
            (CapsState::Surrogate, Err(error)) if error.child_unconfirmed => {
                RestoreOutcome::Uncertain(error.message)
            }
            (CapsState::Surrogate, Err(error)) => RestoreOutcome::CleanupPending(error.message),
        }
    }

    fn apply(&self) -> Result<(), ApplyError> {
        // A stale journal is reconciled first, under this same lease.
        match read_journal(self.remap.storage.as_ref()) {
            Err(reason) => return Err(ApplyError::Blocked(reason)),
            Ok(None) => {}
            Ok(Some(journal)) => match self.reconcile(journal) {
                RestoreOutcome::Restored | RestoreOutcome::NoJournal => {}
                RestoreOutcome::OwnershipLost { observed_destination } => {
                    return Err(ApplyError::Unowned {
                        destination: observed_destination,
                    });
                }
                RestoreOutcome::CleanupPending(reason)
                | RestoreOutcome::Uncertain(reason)
                | RestoreOutcome::Blocked(reason) => {
                    return Err(ApplyError::Blocked(format!(
                        "a stale Caps remap could not be restored first: {reason}"
                    )));
                }
            },
        }
        let mappings = self.read_mappings().map_err(ApplyError::Read)?;
        match caps_state(&mappings) {
            CapsState::Absent => {}
            CapsState::Surrogate => {
                return Err(ApplyError::Unowned { destination: F18_USAGE });
            }
            CapsState::Foreign(destination) => return Err(ApplyError::Unowned { destination }),
        }
        let journal = Journal::new(Phase::ApplyPending);
        self.write_journal(journal).map_err(ApplyError::Journal)?;
        let written = self.set(&with_surrogate(&mappings));
        if let Err(error) = &written
            && !error.may_have_mutated()
        {
            // Nothing ran: the pending intent is stale, retire it if we can.
            let _ = self.retire();
            return Err(ApplyError::NotApplied(error.message.clone()));
        }
        let after = self.read_mappings().map_err(|reason| {
            ApplyError::Uncertain(format!(
                "the write may have applied but was not verified: {reason}"
            ))
        })?;
        match (caps_state(&after), written) {
            (CapsState::Surrogate, Ok(())) if self.remap.hidutil.unreaped() == 0 => {
                self.write_journal(journal.next(Phase::Applied)).map_err(|reason| {
                    ApplyError::Uncertain(format!(
                        "the remap is live but its phase is not recorded: {reason}"
                    ))
                })
            }
            (CapsState::Surrogate, Ok(())) => Err(ApplyError::Uncertain(
                "a hidutil child is not confirmed gone".to_owned(),
            )),
            (CapsState::Surrogate, Err(error)) => Err(ApplyError::Uncertain(format!(
                "the remap is live although hidutil failed: {}",
                error.message
            ))),
            (CapsState::Absent, Err(error)) if error.child_unconfirmed => {
                Err(ApplyError::Uncertain(error.message))
            }
            (CapsState::Absent, written) => {
                let reason = written.err().map_or_else(
                    || "hidutil reported success but the mapping is absent".to_owned(),
                    |error| error.message,
                );
                // Verified absent with no unconfirmed child: the intent is stale.
                let _ = self.retire();
                Err(ApplyError::NotApplied(reason))
            }
            (CapsState::Foreign(destination), _) => Err(ApplyError::Uncertain(format!(
                "Caps Lock was changed to {destination:#x} while applying"
            ))),
        }
    }
}
