//! The central registry of normalized physical chords and declarative HID
//! filters, used to diagnose cross-backend overlaps.
//!
//! The registry records conflicts and arbitrates one narrow case: a HID claimant
//! with strictly higher priority makes an overlapping Carbon reservation
//! *yield*. A yielded reservation reports "must not dispatch" through
//! [`ReservationRegistry::dispatchable`]; nothing more is claimed about
//! cross-backend priority. Yields are derived state: they are restored when the
//! claimant is removed, replaced away from the chord or marked inactive.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use super::diagnostics::{Diagnostics, InputDiagnostic};
use super::event::{InputEventKind, KeyCode, ModifierFlags, PressStream};
use super::filter::InputFilter;
use super::subscribers::InputPriority;

/// Which native mechanism delivers a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationBackend {
    /// A Carbon registered hotkey.
    Carbon,
    /// The HID event tap.
    Hid,
}

/// A normalized physical chord: one key code plus held modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    key: KeyCode,
    modifiers: ModifierFlags,
}

impl Chord {
    /// Builds a chord; Caps Lock is dropped so equal physical chords compare equal.
    #[must_use]
    pub const fn new(key: KeyCode, modifiers: ModifierFlags) -> Self {
        Self {
            key,
            modifiers: modifiers.normalized(),
        }
    }

    /// The key code.
    #[must_use]
    pub const fn key(&self) -> KeyCode { self.key }

    /// The normalized modifiers.
    #[must_use]
    pub const fn modifiers(&self) -> ModifierFlags { self.modifiers }
}

/// What a reservation claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// One exact chord.
    Chord(Chord),
    /// A declarative HID filter.
    Filter(InputFilter),
}

impl Claim {
    fn overlaps(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Chord(a), Self::Chord(b)) => a == b,
            (Self::Filter(f), Self::Chord(c)) | (Self::Chord(c), Self::Filter(f)) => {
                [InputEventKind::KeyDown, InputEventKind::KeyUp]
                    .into_iter()
                    .any(|kind| f.matches(kind, PressStream::Key(c.key), c.modifiers))
            }
            (Self::Filter(a), Self::Filter(b)) => a.overlaps(b),
        }
    }
}

/// Opaque owner of a set of reservations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReservationOwner(u64);

/// Opaque reservation identity scoped by registry, generation and revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReservationId {
    registry: u64,
    generation: u64,
    revision: u64,
}

/// Why a reservation change was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationError {
    /// An explicit yield was requested for an overlap that cannot be arbitrated:
    /// the claimant does not have strictly higher priority.
    UnsupportedOverlap {
        /// The Carbon reservation in the way.
        existing: ReservationId,
    },
    /// The yield is not meaningful: the reservation is not Carbon, the claimant
    /// is not an active HID claim, or the claims do not overlap.
    NotYieldable,
    /// The ID belongs to another registry.
    ForeignRegistry,
    /// The ID is from a retired generation or was already removed.
    Stale,
}

impl fmt::Display for ReservationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnsupportedOverlap { .. } => {
                "the overlapping claimant does not have strictly higher priority; it cannot be arbitrated"
            }
            Self::NotYieldable => "the reservation cannot yield to that claimant",
            Self::ForeignRegistry => "reservation belongs to another registry",
            Self::Stale => "reservation is stale",
        })
    }
}

impl std::error::Error for ReservationError {}

/// A recorded cross-backend overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservationConflict {
    /// The Carbon reservation.
    pub carbon: ReservationId,
    /// The HID reservation.
    pub hid: ReservationId,
}

struct Entry {
    id: ReservationId,
    owner: ReservationOwner,
    backend: ReservationBackend,
    claim: Claim,
    priority: InputPriority,
    /// A paused claim neither conflicts nor makes others yield.
    active: bool,
    /// The HID claimant this Carbon reservation yields to, if any.
    yielded_to: Option<ReservationId>,
}

impl Entry {
    /// Whether `self` is an active HID claim that outranks and overlaps `carbon`.
    fn outranks(&self, carbon: &Self) -> bool {
        self.active
            && self.backend == ReservationBackend::Hid
            && carbon.backend == ReservationBackend::Carbon
            && self.priority > carbon.priority
            && self.claim.overlaps(&carbon.claim)
    }
}

/// The central reservation registry. Safe to share; all methods are off-tap.
pub struct ReservationRegistry {
    registry: u64,
    state: Mutex<State>,
    diagnostics: Arc<Diagnostics>,
}

#[derive(Default)]
struct State {
    generation: u64,
    next_revision: u64,
    next_owner: u64,
    entries: Vec<Entry>,
}

impl ReservationRegistry {
    pub(crate) fn new(registry: u64, diagnostics: Arc<Diagnostics>) -> Self {
        Self {
            registry,
            state: Mutex::new(State {
                generation: 1,
                ..State::default()
            }),
            diagnostics,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Allocates a fresh opaque owner.
    pub fn new_owner(&self) -> ReservationOwner {
        let mut state = self.lock();
        state.next_owner += 1;
        ReservationOwner(state.next_owner)
    }

    /// Records a reservation.
    ///
    /// A HID claim with strictly higher priority makes every overlapping Carbon
    /// reservation yield, and a Carbon reservation that overlaps such a claim
    /// starts out yielded. Equal or lower priority overlaps are only recorded as
    /// conflicts.
    ///
    /// # Errors
    /// Currently infallible; the `Result` keeps the signature stable.
    pub fn register(
        &self,
        owner: ReservationOwner,
        backend: ReservationBackend,
        claim: Claim,
        priority: InputPriority,
    ) -> Result<ReservationId, ReservationError> {
        let mut state = self.lock();
        state.next_revision += 1;
        let id = ReservationId {
            registry: self.registry,
            generation: state.generation,
            revision: state.next_revision,
        };
        state.entries.push(Entry {
            id,
            owner,
            backend,
            claim,
            priority,
            active: true,
            yielded_to: None,
        });
        self.settle(&mut state);
        self.record_conflicts(&state, id);
        Ok(id)
    }

    /// Replaces the claim and priority of a reservation atomically.
    ///
    /// Yields are re-derived: they follow the new claim and priority.
    ///
    /// # Errors
    /// Returns a [`ReservationError`] for foreign or stale IDs.
    pub fn update(
        &self,
        id: ReservationId,
        claim: Claim,
        priority: InputPriority,
    ) -> Result<ReservationId, ReservationError> {
        let mut state = self.lock();
        let index = self.find(&state, id)?;
        state.next_revision += 1;
        let new_id = ReservationId {
            revision: state.next_revision,
            ..id
        };
        let entry = &mut state.entries[index];
        entry.id = new_id;
        entry.claim = claim;
        entry.priority = priority;
        for other in &mut state.entries {
            if other.yielded_to == Some(id) {
                other.yielded_to = Some(new_id);
            }
        }
        self.settle(&mut state);
        self.record_conflicts(&state, new_id);
        Ok(new_id)
    }

    /// Removes one reservation.
    ///
    /// # Errors
    /// Returns a [`ReservationError`] for foreign or stale IDs.
    pub fn remove(&self, id: ReservationId) -> Result<(), ReservationError> {
        let mut state = self.lock();
        let index = self.find(&state, id)?;
        state.entries.remove(index);
        self.settle(&mut state);
        Ok(())
    }

    /// Pauses or resumes a reservation. A paused HID claim neither conflicts nor
    /// makes Carbon reservations yield; they are restored while it is paused.
    ///
    /// # Errors
    /// Returns a [`ReservationError`] for foreign or stale IDs.
    pub fn set_active(&self, id: ReservationId, active: bool) -> Result<(), ReservationError> {
        let mut state = self.lock();
        let index = self.find(&state, id)?;
        state.entries[index].active = active;
        self.settle(&mut state);
        Ok(())
    }

    /// Explicitly yields the Carbon `reservation` to the HID `claimant`.
    ///
    /// Idempotent: yielding an already yielded reservation succeeds and keeps its
    /// claimant. The yield lasts exactly as long as the claimant stays an active
    /// HID claim that overlaps and outranks the reservation, and survives
    /// updates of either side while that holds. It is never made permanent.
    ///
    /// # Errors
    /// - [`ReservationError::ForeignRegistry`] or [`ReservationError::Stale`] for
    ///   either ID that is not live here.
    /// - [`ReservationError::NotYieldable`] when `reservation` is not Carbon,
    ///   `claimant` is not an active HID claim, or they do not overlap.
    /// - [`ReservationError::UnsupportedOverlap`] when the claimant does not have
    ///   strictly higher priority; this is also recorded as a diagnostic.
    pub fn yield_to(
        &self,
        reservation: ReservationId,
        claimant: ReservationId,
    ) -> Result<(), ReservationError> {
        let mut state = self.lock();
        let carbon = self.find(&state, reservation)?;
        let hid = self.find(&state, claimant)?;
        let (c, h) = (&state.entries[carbon], &state.entries[hid]);
        if c.backend != ReservationBackend::Carbon
            || h.backend != ReservationBackend::Hid
            || !h.active
            || !h.claim.overlaps(&c.claim)
        {
            return Err(ReservationError::NotYieldable);
        }
        if h.priority <= c.priority {
            self.diagnostics
                .record(InputDiagnostic::UnsupportedOverlap { existing: reservation });
            return Err(ReservationError::UnsupportedOverlap { existing: reservation });
        }
        // An outranking claimant always yields the reservation, so settling is
        // the whole operation and repeating it changes nothing.
        self.settle(&mut state);
        Ok(())
    }

    /// Whether the reservation may dispatch. Fails closed: a stale, foreign or
    /// yielded reservation must not dispatch.
    pub fn dispatchable(&self, id: ReservationId) -> bool { self.is_dispatchable(&self.lock(), id) }

    /// Non-blocking [`Self::dispatchable`] for native callbacks: `None` when the
    /// registry is momentarily busy, so a callback never waits on its lock.
    pub fn try_dispatchable(&self, id: ReservationId) -> Option<bool> {
        let state = match self.state.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        Some(self.is_dispatchable(&state, id))
    }

    fn is_dispatchable(&self, state: &State, id: ReservationId) -> bool {
        self.find(state, id)
            .is_ok_and(|index| state.entries[index].yielded_to.is_none())
    }

    /// The claimant a live reservation currently yields to.
    pub fn yielded_to(&self, id: ReservationId) -> Option<ReservationId> {
        let state = self.lock();
        let index = self.find(&state, id).ok()?;
        state.entries[index].yielded_to
    }

    /// Removes every reservation of `owner` and returns how many were removed.
    pub fn release_owner(&self, owner: ReservationOwner) -> usize {
        let mut state = self.lock();
        let before = state.entries.len();
        state.entries.retain(|entry| entry.owner != owner);
        let removed = before - state.entries.len();
        self.settle(&mut state);
        removed
    }

    /// Number of live reservations.
    pub fn len(&self) -> usize { self.lock().entries.len() }

    /// Whether no reservation exists.
    pub fn is_empty(&self) -> bool { self.len() == 0 }

    /// Every current cross-backend overlap.
    pub fn conflicts(&self) -> Vec<ReservationConflict> {
        let state = self.lock();
        let mut out = Vec::new();
        for (i, a) in state.entries.iter().enumerate() {
            for b in &state.entries[i + 1..] {
                if a.backend != b.backend
                    && a.active
                    && b.active
                    && a.yielded_to != Some(b.id)
                    && b.yielded_to != Some(a.id)
                    && a.claim.overlaps(&b.claim)
                {
                    let (carbon, hid) = if a.backend == ReservationBackend::Carbon {
                        (a.id, b.id)
                    } else {
                        (b.id, a.id)
                    };
                    out.push(ReservationConflict { carbon, hid });
                }
            }
        }
        out
    }

    fn find(&self, state: &State, id: ReservationId) -> Result<usize, ReservationError> {
        if id.registry != self.registry {
            return Err(ReservationError::ForeignRegistry);
        }
        state
            .entries
            .iter()
            .position(|entry| entry.id == id)
            .ok_or(ReservationError::Stale)
    }

    /// Re-derives every yield: a still-valid claimant is kept, otherwise the
    /// highest-priority active outranking HID claim (earliest on ties) is chosen.
    fn settle(&self, state: &mut State) {
        for index in 0..state.entries.len() {
            let carbon = &state.entries[index];
            if carbon.backend != ReservationBackend::Carbon {
                continue;
            }
            let current = carbon.yielded_to;
            let kept = current.filter(|claimant| {
                state.entries.iter().any(|e| e.id == *claimant && e.outranks(carbon))
            });
            let next = kept.or_else(|| {
                let mut best: Option<&Entry> = None;
                for e in state.entries.iter().filter(|e| e.outranks(carbon)) {
                    if best.is_none_or(|b| e.priority > b.priority) {
                        best = Some(e);
                    }
                }
                best.map(|e| e.id)
            });
            if next != current {
                let id = carbon.id;
                state.entries[index].yielded_to = next;
                self.diagnostics.record(
                    next.map_or(InputDiagnostic::YieldRestored { reservation: id }, |claimant| {
                        InputDiagnostic::Yielded { reservation: id, claimant }
                    }),
                );
            }
        }
    }

    fn record_conflicts(&self, state: &State, id: ReservationId) {
        let Some(me) = state.entries.iter().find(|entry| entry.id == id) else {
            return;
        };
        for other in &state.entries {
            let resolved = other.yielded_to == Some(id) || me.yielded_to == Some(other.id);
            if other.id != id
                && other.backend != me.backend
                && other.active
                && me.active
                && !resolved
                && other.claim.overlaps(&me.claim)
            {
                self.diagnostics
                    .record(InputDiagnostic::ReservationConflict { first: other.id, second: id });
            }
        }
    }
}
