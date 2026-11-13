//! The instant-fallback set: applications that move without animation.
//!
//! An application enters the set when it cannot keep up (the coordinator, from
//! its metrics), when its Enhanced UI lease cannot be held (the actor), or when
//! the smooth path is unavailable for it. It is kept per application **and**
//! per [`MotionKind`], so an application that cannot animate resizes still
//! animates moves. A rate or lag fallback is left after a cool-down (60 s the
//! first time, doubling on every re-entry without a healthy animation in
//! between, at most 10 min), evaluated when the next plan is made; the Enhanced
//! UI reasons are sticky for that exact application instance until the
//! application terminates or animations are turned off. It never affects
//! another application and never changes the configuration. The planner reads
//! a copy of the ids before each transition.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use stache_macos::ApplicationId;

/// The cool-down of a first rate or lag fallback.
pub const FIRST_COOLDOWN: Duration = Duration::from_secs(60);
/// The longest cool-down (a re-entry without a healthy animation doubles it).
pub const MAX_COOLDOWN: Duration = Duration::from_secs(600);
/// Fallback notes kept for the actor's log before it drains them.
const MAX_EVENTS: usize = 64;

/// What a layout change does to one window: move it only, or change its size
/// (the plan wanted a size).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MotionKind {
    /// Position only.
    Move,
    /// The size changes (with or without the position).
    Resize,
}

impl MotionKind {
    /// Both kinds.
    pub const ALL: [Self; 2] = [Self::Move, Self::Resize];

    /// The kind of a write that wants (`true`) or does not want a size.
    #[must_use]
    pub const fn of(wants_size: bool) -> Self { if wants_size { Self::Resize } else { Self::Move } }

    /// `move` or `resize`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::Resize => "resize",
        }
    }
}

/// Why an application moves without animation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackReason {
    /// Wrote fewer than 15 frames per second in 2 of its last 3 layout changes.
    BelowMinimumRate,
    /// Ended more than 250 ms late in 2 of its last 3 layout changes.
    LateEnd,
    /// It turns Enhanced UI back on.
    ReEnablesEnhancedUi,
    /// Enhanced UI could not be turned off.
    EnhancedUiStuck,
    /// It shares the overflow AX thread and cannot keep up.
    Overflow,
    /// Its display has no frame clock and it cannot keep up.
    NoFrameClock,
}

impl FallbackReason {
    /// Whether the fallback ends after a cool-down (rate or lag, including the
    /// overflow and timer-paced variants); the Enhanced UI reasons are sticky.
    #[must_use]
    pub const fn recoverable(self) -> bool {
        !matches!(self, Self::ReEnablesEnhancedUi | Self::EnhancedUiStuck)
    }
}

impl fmt::Display for FallbackReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BelowMinimumRate => {
                "writes fewer than 15 frames per second in 2 of its last 3 layout changes"
            }
            Self::LateEnd => "ends more than 250 ms late in 2 of its last 3 layout changes",
            Self::ReEnablesEnhancedUi => "turns Enhanced UI back on",
            Self::EnhancedUiStuck => "Enhanced UI could not be turned off",
            Self::Overflow => "shares the overflow AX thread and cannot keep up",
            Self::NoFrameClock => "its display has no frame clock and it cannot keep up",
        })
    }
}

/// One application and kind in fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FallbackEntry {
    /// The exact application instance.
    pub application: ApplicationId,
    /// Which kind of layout change it does not animate.
    pub kind: MotionKind,
    /// Why.
    pub reason: FallbackReason,
    /// When it may animate again (on its next plan after this); `None` is
    /// sticky.
    pub until: Option<Instant>,
}

/// An entry or exit, for the actor's log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackEvent {
    /// The application entered the fallback for this kind.
    Entered(FallbackEntry),
    /// Its cool-down passed: it animates this kind again.
    Left(FallbackEntry),
}

#[derive(Debug, Default)]
struct Inner {
    held: BTreeMap<(ApplicationId, MotionKind), (FallbackReason, Option<Instant>)>,
    /// The cool-down of the next rate or lag entry, when not the first.
    next: BTreeMap<(ApplicationId, MotionKind), Duration>,
    events: Vec<FallbackEvent>,
}

impl Inner {
    fn note(&mut self, event: FallbackEvent) {
        if self.events.len() < MAX_EVENTS {
            self.events.push(event);
        }
    }
}

/// The applications in fallback, shared by the coordinator, the actor and
/// status. The lock is never held across a call-out.
#[derive(Debug, Default)]
pub struct FallbackSet {
    inner: Mutex<Inner>,
}

impl FallbackSet {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Puts `application` in fallback for `kind` at `now`. A rate or lag
    /// reason gets the current cool-down (and doubles the next one); the first
    /// reason stays, except that a sticky Enhanced UI reason replaces a
    /// recoverable one. `true` when it was not in the set for that kind.
    pub fn insert(
        &self,
        application: ApplicationId,
        kind: MotionKind,
        reason: FallbackReason,
        now: Instant,
    ) -> bool {
        let mut inner = self.lock();
        let key = (application, kind);
        if let Some(held) = inner.held.get_mut(&key) {
            if !reason.recoverable() && held.0.recoverable() {
                // Now sticky for the run: noted, so the log says so.
                *held = (reason, None);
                inner.note(FallbackEvent::Entered(FallbackEntry {
                    application,
                    kind,
                    reason,
                    until: None,
                }));
            }
            return false;
        }
        let until = if reason.recoverable() {
            let cooldown = inner.next.get(&key).copied().unwrap_or(FIRST_COOLDOWN);
            inner.next.insert(key, (cooldown * 2).min(MAX_COOLDOWN));
            Some(now + cooldown)
        } else {
            None
        };
        inner.held.insert(key, (reason, until));
        inner.note(FallbackEvent::Entered(FallbackEntry {
            application,
            kind,
            reason,
            until,
        }));
        true
    }

    /// A healthy animation of `kind`: the next cool-down is the first again.
    pub fn healthy(&self, application: ApplicationId, kind: MotionKind) {
        self.lock().next.remove(&(application, kind));
    }

    /// Releases every entry whose cool-down passed by `now`, and returns them.
    pub fn expire(&self, now: Instant) -> Vec<FallbackEntry> {
        let mut inner = self.lock();
        let due: Vec<FallbackEntry> = inner
            .held
            .iter()
            .filter_map(|(&(application, kind), &(reason, until))| {
                until.filter(|until| *until <= now).map(|_| FallbackEntry {
                    application,
                    kind,
                    reason,
                    until,
                })
            })
            .collect();
        for entry in &due {
            inner.held.remove(&(entry.application, entry.kind));
            inner.note(FallbackEvent::Left(*entry));
        }
        due
    }

    /// Whether `application` moves without animation for `kind`.
    #[must_use]
    pub fn contains(&self, application: ApplicationId, kind: MotionKind) -> bool {
        self.lock().held.contains_key(&(application, kind))
    }

    /// The applications and kinds in fallback.
    #[must_use]
    pub fn ids(&self) -> BTreeSet<(ApplicationId, MotionKind)> {
        self.lock().held.keys().copied().collect()
    }

    /// Each entry, in application and kind order.
    #[must_use]
    pub fn entries(&self) -> Vec<FallbackEntry> {
        self.lock()
            .held
            .iter()
            .map(|(&(application, kind), &(reason, until))| FallbackEntry {
                application,
                kind,
                reason,
                until,
            })
            .collect()
    }

    /// The entries and exits since the last call, oldest first.
    pub fn take_events(&self) -> Vec<FallbackEvent> { std::mem::take(&mut self.lock().events) }

    /// Releases one application (it terminated), cool-downs included.
    pub fn forget(&self, application: ApplicationId) {
        let mut inner = self.lock();
        inner.held.retain(|(app, _), _| *app != application);
        inner.next.retain(|(app, _), _| *app != application);
    }

    /// Releases everything (animations were turned off).
    pub fn clear(&self) {
        let mut inner = self.lock();
        inner.held.clear();
        inner.next.clear();
        inner.events.clear();
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool { self.lock().held.is_empty() }
}
