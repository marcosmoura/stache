//! Host-owned screen reservations and the SDK-side display identity.
//!
//! A module that declares [`crate::ModuleDescriptor::reserves_screen`] obtains a
//! [`ScreenReservations`] capability from its [`crate::ModuleContext`]. The
//! capability stages validated logical edge insets per display for exactly one
//! runtime generation. The host aggregates every owner's insets per display and
//! edge with the maximum (overlapping reservations are never summed) and
//! publishes one coherent, revisioned [`ScreenReservationSnapshot`] that any
//! consumer reads through a [`ScreenReservationObserver`]. Consumers never read
//! a producer's configuration. The name differs from the unrelated keyboard
//! chord registry in `input::reservations`.
//!
//! # Display identity
//!
//! [`DisplayId`] is a numeric `CGDirectDisplayID`-compatible value; the SDK
//! stores no native pointer and never enumerates displays. The host projects
//! the platform's identity into it and reports the live set with
//! [`crate::Registry::apply_screen_topology`], tagged with a strictly
//! increasing [`TopologyGeneration`].
//!
//! * A `DisplayId` is only meaningful together with the topology generation it
//!   was observed in. macOS may reuse a numeric ID after a display disappears.
//! * A display is *present* from the topology generation that first listed it
//!   (its [`DisplayReservations::since`]) until a topology omits it. A display
//!   that disappears and returns is a new presence with a later `since`.
//! * Applying a topology drops every reservation for a display it omits. A
//!   later return of the same numeric ID does not resurrect them.
//! * Staging names the topology generation the caller observed. A mismatch with
//!   the registry's current generation, or an ID that is not present, is
//!   rejected and changes nothing, so a stale plan can never land on a reused
//!   ID. The SDK cannot see a reuse inside one topology step: the host must
//!   omit such an ID from the intermediate topology it applies.
//!
//! # Lifecycle
//!
//! The capability is bound to one runtime generation. When that generation ends
//! (stop, pause, disable, failure, including a failed or partial start) the
//! registry removes exactly that owner and generation's reservations before any
//! cleanup wait and wakes readers. A retained handle of an ended generation can
//! never stage, replace or remove anything, including a successor's set.
//!
//! # Reading
//!
//! Subscribe before fetch: take [`ScreenReservationObserver::read`], then call
//! [`ScreenReservationObserver::wait_for_change`] with its revision. The check
//! and the sleep share one lock, so a publication between them is not lost.
//! Cancellation, shutdown and timeout end the wait deterministically. Readers
//! take no registry, module or lifecycle lock.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::generation::GenerationScope;
use crate::lock;
use crate::module::ModuleId;
use crate::threading::CancellationToken;

/// The largest accepted inset on one edge, in logical points.
pub const MAX_RESERVATION_INSET: f64 = 1_000_000.0;
/// The most displays one owner may stage in one replacement.
pub const MAX_RESERVED_DISPLAYS: usize = 64;

/// A numeric, `CGDirectDisplayID`-compatible display identity.
///
/// See the [module documentation](self) for lifetime, reuse and removal. `0` is
/// the platform's null display and is never present in a topology.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DisplayId(u32);

impl DisplayId {
    /// Wraps a raw display ID.
    #[must_use]
    pub const fn new(raw: u32) -> Self { Self(raw) }

    /// The raw numeric ID.
    #[must_use]
    pub const fn raw(self) -> u32 { self.0 }
}

impl fmt::Display for DisplayId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{}", self.0) }
}

/// A strictly increasing identifier of one display topology.
///
/// Generation zero is the initial empty topology.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TopologyGeneration(u64);

impl TopologyGeneration {
    /// The initial generation before the host reported any display.
    pub const INITIAL: Self = Self(0);

    /// Wraps a host-chosen generation.
    #[must_use]
    pub const fn new(value: u64) -> Self { Self(value) }

    /// The numeric value.
    #[must_use]
    pub const fn get(self) -> u64 { self.0 }
}

/// A screen edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Edge {
    /// The top edge.
    Top,
    /// The left edge.
    Left,
    /// The bottom edge.
    Bottom,
    /// The right edge.
    Right,
}

/// Validated logical edge insets, measured inward from the full display bounds.
///
/// Every value is finite, nonnegative (negative zero is normalized to zero) and
/// at most [`MAX_RESERVATION_INSET`], so sums of opposite edges cannot overflow.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EdgeInsets {
    top: f64,
    left: f64,
    bottom: f64,
    right: f64,
}

impl EdgeInsets {
    /// No reservation on any edge.
    pub const ZERO: Self = Self {
        top: 0.0,
        left: 0.0,
        bottom: 0.0,
        right: 0.0,
    };

    /// Validates four logical insets in points.
    ///
    /// # Errors
    /// Returns the first invalid edge in `top, left, bottom, right` order.
    pub fn new(
        top: f64,
        left: f64,
        bottom: f64,
        right: f64,
    ) -> Result<Self, ScreenReservationError> {
        Ok(Self {
            top: check(Edge::Top, top)?,
            left: check(Edge::Left, left)?,
            bottom: check(Edge::Bottom, bottom)?,
            right: check(Edge::Right, right)?,
        })
    }

    /// The top inset.
    #[must_use]
    pub const fn top(&self) -> f64 { self.top }

    /// The left inset.
    #[must_use]
    pub const fn left(&self) -> f64 { self.left }

    /// The bottom inset.
    #[must_use]
    pub const fn bottom(&self) -> f64 { self.bottom }

    /// The right inset.
    #[must_use]
    pub const fn right(&self) -> f64 { self.right }

    /// The inset on `edge`.
    #[must_use]
    pub const fn get(&self, edge: Edge) -> f64 {
        match edge {
            Edge::Top => self.top,
            Edge::Left => self.left,
            Edge::Bottom => self.bottom,
            Edge::Right => self.right,
        }
    }

    /// The per-edge maximum of two inset sets.
    #[must_use]
    pub const fn max(self, other: Self) -> Self {
        Self {
            top: self.top.max(other.top),
            left: self.left.max(other.left),
            bottom: self.bottom.max(other.bottom),
            right: self.right.max(other.right),
        }
    }
}

fn check(edge: Edge, value: f64) -> Result<f64, ScreenReservationError> {
    if !value.is_finite() || value < 0.0 {
        return Err(ScreenReservationError::InvalidInset { edge, value });
    }
    if value > MAX_RESERVATION_INSET {
        return Err(ScreenReservationError::InsetTooLarge { edge, value });
    }
    // Normalizes negative zero.
    Ok(value + 0.0)
}

/// A deterministic screen-reservation failure. A failure never changes state.
#[derive(Clone, Debug, PartialEq)]
pub enum ScreenReservationError {
    /// The module did not declare `reserves_screen`.
    NotDeclared {
        /// The module that asked.
        module: ModuleId,
    },
    /// An inset is negative, NaN or infinite.
    InvalidInset {
        /// The offending edge.
        edge: Edge,
        /// The rejected value.
        value: f64,
    },
    /// An inset exceeds [`MAX_RESERVATION_INSET`].
    InsetTooLarge {
        /// The offending edge.
        edge: Edge,
        /// The rejected value.
        value: f64,
    },
    /// A replacement names the same display twice.
    DuplicateDisplay {
        /// The repeated display.
        display: DisplayId,
    },
    /// A replacement names more than [`MAX_RESERVED_DISPLAYS`] displays.
    TooManyDisplays {
        /// The number supplied.
        count: usize,
    },
    /// The handle's runtime generation has ended or been superseded.
    StaleGeneration {
        /// The owning module.
        owner: ModuleId,
        /// The generation the handle was bound to.
        generation: u64,
    },
    /// The caller staged against a topology other than the current one.
    StaleTopology {
        /// The generation the caller observed.
        staged: TopologyGeneration,
        /// The registry's current generation.
        current: TopologyGeneration,
    },
    /// The display is not present in the current topology.
    UnknownDisplay {
        /// The display that is not present.
        display: DisplayId,
    },
    /// A topology lists the platform's null display (`0`).
    NullDisplay,
    /// A topology generation is not newer than the current one.
    NonMonotonicTopology {
        /// The registry's current generation.
        current: TopologyGeneration,
        /// The rejected generation.
        requested: TopologyGeneration,
    },
}

impl fmt::Display for ScreenReservationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotDeclared { module } => {
                write!(f, "module `{module}` does not declare `reserves_screen`")
            }
            Self::InvalidInset { edge, value } => {
                write!(f, "{edge:?} inset {value} is not a finite nonnegative number")
            }
            Self::InsetTooLarge { edge, value } => {
                write!(f, "{edge:?} inset {value} exceeds {MAX_RESERVATION_INSET}")
            }
            Self::DuplicateDisplay { display } => write!(f, "display {display} is listed twice"),
            Self::TooManyDisplays { count } => {
                write!(f, "{count} displays exceed the limit of {MAX_RESERVED_DISPLAYS}")
            }
            Self::StaleGeneration { owner, generation } => {
                write!(f, "generation {generation} of `{owner}` has ended")
            }
            Self::StaleTopology { staged, current } => write!(
                f,
                "staged against topology {} but the current topology is {}",
                staged.get(),
                current.get()
            ),
            Self::UnknownDisplay { display } => {
                write!(f, "display {display} is not present in the current topology")
            }
            Self::NullDisplay => f.write_str("the null display cannot be present"),
            Self::NonMonotonicTopology { current, requested } => write!(
                f,
                "topology {} is not newer than {}",
                requested.get(),
                current.get()
            ),
        }
    }
}

impl std::error::Error for ScreenReservationError {}

/// One owner's contribution to one display.
#[derive(Clone, Debug, PartialEq)]
pub struct ReservationContribution {
    /// The reserving module.
    pub owner: ModuleId,
    /// The exact runtime generation that staged it.
    pub generation: u64,
    /// The insets it staged.
    pub insets: EdgeInsets,
}

/// The aggregated reservations of one present display.
#[derive(Clone, Debug, PartialEq)]
pub struct DisplayReservations {
    /// The display.
    pub display: DisplayId,
    /// The topology generation in which this presence of the ID began.
    ///
    /// A reused numeric ID has a later `since` than its predecessor.
    pub since: TopologyGeneration,
    /// The per-edge maximum over `contributions`; zero when there are none.
    pub insets: EdgeInsets,
    /// Every owner's contribution, ordered by owner.
    pub contributions: Vec<ReservationContribution>,
}

/// One coherent, immutable view of every reservation.
#[derive(Clone, Debug, PartialEq)]
pub struct ScreenReservationSnapshot {
    /// Monotonic revision; it advances on every observable change.
    pub revision: u64,
    /// The topology the displays below belong to.
    pub topology: TopologyGeneration,
    /// Every present display, ordered by [`DisplayId`].
    pub displays: Vec<DisplayReservations>,
}

impl ScreenReservationSnapshot {
    /// The entry of `display`, if it is present in this topology.
    #[must_use]
    pub fn display(&self, display: DisplayId) -> Option<&DisplayReservations> {
        self.displays
            .binary_search_by_key(&display, |entry| entry.display)
            .ok()
            .map(|index| &self.displays[index])
    }

    /// The aggregated insets of `display`; zero when absent or unreserved.
    #[must_use]
    pub fn insets(&self, display: DisplayId) -> EdgeInsets {
        self.display(display).map_or(EdgeInsets::ZERO, |entry| entry.insets)
    }
}

/// The outcome of a wait for a newer snapshot.
#[derive(Clone, Debug, PartialEq)]
pub enum ScreenReservationWait {
    /// A snapshot with a different revision.
    Changed(Arc<ScreenReservationSnapshot>),
    /// The timeout elapsed first.
    TimedOut,
    /// The caller's token was cancelled.
    Cancelled,
    /// The host is shutting down.
    Closed,
}

struct OwnerEntry {
    generation: u64,
    insets: BTreeMap<DisplayId, EdgeInsets>,
}

struct HubInner {
    topology: TopologyGeneration,
    displays: BTreeMap<DisplayId, TopologyGeneration>,
    owners: BTreeMap<ModuleId, OwnerEntry>,
    /// The newest ended generation per owner; generations only grow.
    ended: BTreeMap<ModuleId, u64>,
    /// Owner generations that already have an end listener registered.
    listening: BTreeSet<(ModuleId, u64)>,
    view: Arc<ScreenReservationSnapshot>,
    closed: bool,
}

impl HubInner {
    /// Rebuilds the snapshot and advances the revision when it changed.
    /// Returns whether readers must be woken.
    fn republish(&mut self) -> bool {
        let displays: Vec<DisplayReservations> = self
            .displays
            .iter()
            .map(|(&display, &since)| {
                let contributions: Vec<ReservationContribution> = self
                    .owners
                    .iter()
                    .filter_map(|(&owner, entry)| {
                        entry.insets.get(&display).map(|&insets| ReservationContribution {
                            owner,
                            generation: entry.generation,
                            insets,
                        })
                    })
                    .collect();
                let insets =
                    contributions.iter().fold(EdgeInsets::ZERO, |acc, item| acc.max(item.insets));
                DisplayReservations {
                    display,
                    since,
                    insets,
                    contributions,
                }
            })
            .collect();
        if displays == self.view.displays && self.topology == self.view.topology {
            return false;
        }
        self.view = Arc::new(ScreenReservationSnapshot {
            revision: self.view.revision + 1,
            topology: self.topology,
            displays,
        });
        true
    }
}

#[cfg(test)]
type Hook = Arc<dyn Fn() + Send + Sync>;

/// The host's authoritative reservation registry.
pub(crate) struct ReservationHub {
    inner: Mutex<HubInner>,
    changed: Condvar,
    #[cfg(test)]
    before_wait: Mutex<Option<Hook>>,
}

impl Default for ReservationHub {
    fn default() -> Self {
        Self {
            inner: Mutex::new(HubInner {
                topology: TopologyGeneration::INITIAL,
                displays: BTreeMap::new(),
                owners: BTreeMap::new(),
                ended: BTreeMap::new(),
                listening: BTreeSet::new(),
                view: Arc::new(ScreenReservationSnapshot {
                    revision: 0,
                    topology: TopologyGeneration::INITIAL,
                    displays: Vec::new(),
                }),
                closed: false,
            }),
            changed: Condvar::new(),
            #[cfg(test)]
            before_wait: Mutex::new(None),
        }
    }
}

impl ReservationHub {
    /// Applies a new topology and drops reservations of omitted displays.
    pub(crate) fn apply_topology(
        &self,
        generation: TopologyGeneration,
        present: &[DisplayId],
    ) -> Result<u64, ScreenReservationError> {
        if present.contains(&DisplayId::new(0)) {
            return Err(ScreenReservationError::NullDisplay);
        }
        let revision = {
            let mut inner = lock(&self.inner);
            if generation <= inner.topology {
                return Err(ScreenReservationError::NonMonotonicTopology {
                    current: inner.topology,
                    requested: generation,
                });
            }
            let previous = std::mem::take(&mut inner.displays);
            inner.displays = present
                .iter()
                .map(|&display| (display, previous.get(&display).copied().unwrap_or(generation)))
                .collect();
            let live = inner.displays.clone();
            for entry in inner.owners.values_mut() {
                entry.insets.retain(|display, _| live.contains_key(display));
            }
            inner.owners.retain(|_, entry| !entry.insets.is_empty());
            inner.topology = generation;
            // The topology generation is part of the snapshot, so it always
            // advances the revision.
            inner.republish();
            inner.view.revision
        };
        self.changed.notify_all();
        Ok(revision)
    }

    fn replace(
        &self,
        owner: ModuleId,
        scope: &GenerationScope,
        topology: TopologyGeneration,
        entries: &[(DisplayId, EdgeInsets)],
    ) -> Result<u64, ScreenReservationError> {
        let generation = scope.id();
        if entries.len() > MAX_RESERVED_DISPLAYS {
            return Err(ScreenReservationError::TooManyDisplays { count: entries.len() });
        }
        let mut staged = BTreeMap::new();
        for &(display, insets) in entries {
            if staged.insert(display, insets).is_some() {
                return Err(ScreenReservationError::DuplicateDisplay { display });
            }
        }
        let (revision, wake) = {
            let mut inner = lock(&self.inner);
            let stale = ScreenReservationError::StaleGeneration { owner, generation };
            if scope.end().is_some() || inner.ended.get(&owner).is_some_and(|&e| e >= generation) {
                return Err(stale);
            }
            if inner.owners.get(&owner).is_some_and(|entry| entry.generation > generation) {
                return Err(stale);
            }
            if topology != inner.topology {
                return Err(ScreenReservationError::StaleTopology {
                    staged: topology,
                    current: inner.topology,
                });
            }
            if let Some(&display) = staged.keys().find(|d| !inner.displays.contains_key(d)) {
                return Err(ScreenReservationError::UnknownDisplay { display });
            }
            if staged.is_empty() {
                inner.owners.remove(&owner);
            } else {
                inner.owners.insert(owner, OwnerEntry { generation, insets: staged });
            }
            let wake = inner.republish();
            (inner.view.revision, wake)
        };
        if wake {
            self.changed.notify_all();
        }
        Ok(revision)
    }

    fn remove(&self, owner: ModuleId, generation: u64) {
        let wake = {
            let mut inner = lock(&self.inner);
            if inner.owners.get(&owner).is_none_or(|entry| entry.generation != generation) {
                return;
            }
            inner.owners.remove(&owner);
            inner.republish()
        };
        if wake {
            self.changed.notify_all();
        }
    }

    /// The generation ended: removes exactly that owner and generation and
    /// rejects its later writes.
    fn end_generation(&self, owner: ModuleId, generation: u64) {
        let wake = {
            let mut inner = lock(&self.inner);
            inner.listening.remove(&(owner, generation));
            let ended = inner.ended.entry(owner).or_insert(generation);
            *ended = (*ended).max(generation);
            if inner.owners.get(&owner).is_some_and(|entry| entry.generation == generation) {
                inner.owners.remove(&owner);
                inner.republish()
            } else {
                false
            }
        };
        if wake {
            self.changed.notify_all();
        }
    }

    /// Wakes every waiter with [`ScreenReservationWait::Closed`].
    pub(crate) fn close(&self) {
        lock(&self.inner).closed = true;
        self.changed.notify_all();
    }

    fn read(&self) -> Arc<ScreenReservationSnapshot> { Arc::clone(&lock(&self.inner).view) }

    #[cfg(test)]
    pub(crate) fn set_before_wait(&self, hook: Option<Hook>) { *lock(&self.before_wait) = hook; }
}

/// The write capability of one owner, bound to one runtime generation.
///
/// Obtained from [`crate::ModuleContext::screen_reservations`]. It cannot read
/// another owner's set, change a topology or outlive its generation.
#[derive(Clone)]
pub struct ScreenReservations {
    hub: Arc<ReservationHub>,
    owner: ModuleId,
    scope: GenerationScope,
}

impl ScreenReservations {
    pub(crate) fn new(hub: Arc<ReservationHub>, owner: ModuleId, scope: GenerationScope) -> Self {
        let generation = scope.id();
        // One removal listener per (owner, generation), however many times the
        // capability is requested.
        let first = lock(&hub.inner).listening.insert((owner, generation));
        if first {
            let listener_hub = Arc::clone(&hub);
            scope.on_end(move |_| listener_hub.end_generation(owner, generation));
        }
        Self { hub, owner, scope }
    }

    /// Atomically replaces this owner's whole set with `entries`.
    ///
    /// `topology` is the generation the caller observed. An empty slice removes
    /// the set. Returns the snapshot revision after the change (unchanged when
    /// the replacement is identical to the current set).
    ///
    /// # Errors
    /// Returns a [`ScreenReservationError`] for a stale generation or topology,
    /// a display that is not present, a duplicate or excess display. The
    /// previous set is kept.
    pub fn replace(
        &self,
        topology: TopologyGeneration,
        entries: &[(DisplayId, EdgeInsets)],
    ) -> Result<u64, ScreenReservationError> {
        self.hub.replace(self.owner, &self.scope, topology, entries)
    }

    /// Removes this owner's set. Idempotent; a no-op once the generation ended,
    /// and never touches a successor's set.
    pub fn remove(&self) { self.hub.remove(self.owner, self.scope.id()); }
}

/// A read-only view of the aggregated reservations.
#[derive(Clone)]
pub struct ScreenReservationObserver {
    hub: Arc<ReservationHub>,
}

impl ScreenReservationObserver {
    pub(crate) const fn new(hub: Arc<ReservationHub>) -> Self { Self { hub } }

    /// The latest coherent snapshot.
    #[must_use]
    pub fn read(&self) -> Arc<ScreenReservationSnapshot> { self.hub.read() }

    /// Blocks until the revision differs from `seen_revision`.
    ///
    /// Precedence is cancellation, then close, then change, then timeout. A
    /// timeout that overflows [`Instant`] becomes an unbounded wait. The check
    /// and the sleep share one lock, so a publication in between is not lost.
    #[must_use]
    pub fn wait_for_change(
        &self,
        seen_revision: u64,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> ScreenReservationWait {
        let hub = &self.hub;
        let deadline = Instant::now().checked_add(timeout);
        let _wake = {
            let hub = Arc::clone(hub);
            cancel.on_cancel(Arc::new(move || {
                let _inner = lock(&hub.inner);
                hub.changed.notify_all();
            }))
        };

        let mut inner = lock(&hub.inner);
        let mut timed_out = false;
        loop {
            if cancel.is_cancelled() {
                return ScreenReservationWait::Cancelled;
            }
            if inner.closed {
                return ScreenReservationWait::Closed;
            }
            if inner.view.revision != seen_revision {
                return ScreenReservationWait::Changed(Arc::clone(&inner.view));
            }
            if timed_out {
                return ScreenReservationWait::TimedOut;
            }
            #[cfg(test)]
            {
                let hook = lock(&hub.before_wait).take();
                if let Some(hook) = hook {
                    hook();
                }
            }
            match deadline {
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    let (next, result) = hub
                        .changed
                        .wait_timeout(inner, remaining)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    inner = next;
                    timed_out = result.timed_out();
                }
                None => {
                    inner =
                        hub.changed.wait(inner).unwrap_or_else(std::sync::PoisonError::into_inner);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
