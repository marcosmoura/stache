//! The generic non-blocking work-reservation contract.
//!
//! A callback never runs a command and never owns a second queue. For work it
//! needs later, the router reserves a slot with [`WorkReservations::try_reserve`]
//! before the callback runs. The reservation is rejected when the owner is full;
//! overflow is a counter, never a late or deferred execution. The real owner
//! (the Keybindings dispatcher) implements the trait with its own capacity.

use std::fmt;
use std::sync::Arc;

/// A non-blocking capacity owner.
pub trait WorkReservations: Send + Sync {
    /// Tries to take one slot. Must never block; returns `false` when full.
    fn try_reserve(&self) -> bool;

    /// Returns one slot taken by a successful [`Self::try_reserve`].
    fn release(&self);
}

/// The owner had no free slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkRejected;

impl fmt::Display for WorkRejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("no work slot available")
    }
}

impl std::error::Error for WorkRejected {}

/// One reserved work slot, released when dropped.
pub struct WorkReservation {
    /// `None` only after [`WorkReservation::redeem`] handed the slot over.
    owner: Option<Arc<dyn WorkReservations>>,
}

impl WorkReservation {
    /// Reserves one slot without blocking.
    ///
    /// # Errors
    /// Returns [`WorkRejected`] when the owner is full.
    pub fn try_new(owner: &Arc<dyn WorkReservations>) -> Result<Self, WorkRejected> {
        if owner.try_reserve() {
            Ok(Self { owner: Some(Arc::clone(owner)) })
        } else {
            Err(WorkRejected)
        }
    }

    /// Whether this reservation was taken from `owner`.
    ///
    /// An owner that converts a reservation into its own accepted work uses
    /// this to refuse a reservation that belongs to a different owner.
    #[must_use]
    pub fn is_owned_by(&self, owner: &Arc<dyn WorkReservations>) -> bool {
        self.owner
            .as_ref()
            .is_some_and(|held| std::ptr::addr_eq(Arc::as_ptr(held), Arc::as_ptr(owner)))
    }

    /// Consumes the reservation **without** calling
    /// [`WorkReservations::release`].
    ///
    /// The owner calls this after it has already moved the slot into its own
    /// accounting (for example, turning it into accepted work or returning it
    /// under its own lock), so the slot is never released twice.
    ///
    /// # Errors
    /// Returns the reservation unchanged when it belongs to another owner.
    pub fn redeem(mut self, owner: &Arc<dyn WorkReservations>) -> Result<(), Self> {
        if !self.is_owned_by(owner) {
            return Err(self);
        }
        self.owner = None;
        Ok(())
    }
}

impl Drop for WorkReservation {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.take() {
            owner.release();
        }
    }
}

impl fmt::Debug for WorkReservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("WorkReservation") }
}

/// A reservation owner with no capacity: every request is rejected.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoWork;

impl WorkReservations for NoWork {
    fn try_reserve(&self) -> bool { false }

    fn release(&self) {}
}
