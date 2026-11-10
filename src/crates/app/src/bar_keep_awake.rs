//! The app projection of Keep Awake's published state into the Bar view.
//!
//! Keep Awake is the first production provider of the SDK published-state
//! channel ([`crate::published_state::state_schemas`] registers its original
//! DTO as the codec). Bar declares it in `observes` but never imports the Keep
//! Awake crate: this app file, which may depend on both, observes the channel
//! and projects one concrete [`KeepAwakeIndicator`] into
//! [`BarView::keep_awake`](crate::bar_bridge::BarView::keep_awake).
//!
//! # Field semantics (legacy two-state visual plus the initial loading branch)
//!
//! * `null` - unresolved: only until the first observation resolves. A Keep
//!   Awake that is enabled, not paused and has not started yet (the registry
//!   starts it before Bar) is unresolved; once resolved it never returns to
//!   `null`.
//! * `{ active: true, .. }` - only for a current, available published state of
//!   the accepted generation whose runtime is a **confirmed adopted active**
//!   assertion with an unlocked screen, while the host reports the module
//!   running and not paused. Bar never infers this from lifecycle alone.
//! * `{ active: false, .. }` - everything else: locked, unknown lock, paused,
//!   disabled, absent (never registered or never published), failed,
//!   acquiring, release pending, a malformed or wrongly versioned payload and
//!   every tombstone.
//!
//! `paused` and `enabled` are the registry's session and accepted-configuration
//! facts (never inferred from the colour). A rejected `keepAwake.toml` never
//! changes the accepted `enabled` flag, so held file diagnostics cannot replace
//! the last accepted state; they are not part of this view.
//!
//! # Observation
//!
//! Reads are cheap and in-memory only: [`KeepAwakeProjection::view`] re-samples
//! the lock-free state cell and the published registry snapshot, so a view is
//! never older than the read. A late write from an older generation, a
//! backwards sequence or an old tombstone is ignored; a new generation resets
//! the sequence. The cell coalesces (there is no queue to overflow), and the
//! bridge's own coalesce limit turns a large gap into a resnapshot hint.
//!
//! There is **no timer**. Two event sources wake the bridge, and nothing else
//! does:
//!
//! * the published-state cell: one worker, alive from the first Bar run until
//!   shutdown, subscribes (reads the cell version first) and then waits for a
//!   change with no timeout; only a change, the registry closing or shutdown
//!   cancellation ends the wait;
//! * the host status sink: pause and enable flags of a provider that is not
//!   running do not move the cell, but every host lifecycle or session change
//!   publishes a host status, and [`BarBridge::host_changed`] (called from the
//!   sink, signal only) wakes the bridge, which re-samples the projection.
//!
//! The worker starts when Bar first runs (a disabled Bar owns no thread), only
//! wakes the bridge, and never touches native code, disk or a module lock.
//!
//! [`BarBridge::host_changed`]: crate::bar_bridge::BarBridge::host_changed

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use specta::Type;
use stache_keep_awake::{AssertionState, KeepAwakeConfig, KeepAwakeRuntimeStatus, LockState};
use stache_sdk::{
    CancellationToken, ModuleId, ModuleSnapshot, ModuleStatus, Registry, StateEnvelope,
    StateObserver, StateReading, StateUnavailableReason, StateWait,
};

/// The observed provider.
pub const KEEP_AWAKE: ModuleId = KeepAwakeConfig::ID;

/// The longest the worker is joined at shutdown.
pub const WATCH_JOIN: Duration = Duration::from_secs(2);

/// What the Bar renders for Keep Awake, plus the non-visual session facts the
/// click needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct KeepAwakeIndicator {
    /// Whether a confirmed adopted assertion is held (the green filled icon).
    pub active: bool,
    /// The host session pause flag of the Keep Awake module.
    pub paused: bool,
    /// The accepted `enabled` flag of the Keep Awake module.
    pub enabled: bool,
}

impl KeepAwakeIndicator {
    const ABSENT: Self = Self {
        active: false,
        paused: false,
        enabled: false,
    };
}

/// The registry facts the projection combines with the published state.
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent registry facts"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostFacts {
    /// The registry status is `Running`.
    pub running: bool,
    /// The accepted configuration enables the module.
    pub enabled: bool,
    /// The session pause flag.
    pub paused: bool,
    /// The module is enabled, not paused and has not started yet.
    pub expects_start: bool,
}

impl HostFacts {
    /// Reads the facts from one published registry snapshot.
    #[must_use]
    pub fn of(snapshot: &ModuleSnapshot) -> Self {
        Self {
            running: snapshot.status == ModuleStatus::Running,
            enabled: snapshot.enabled,
            paused: snapshot.paused,
            expects_start: snapshot.enabled
                && !snapshot.paused
                && snapshot.status == ModuleStatus::Stopped,
        }
    }
}

/// One coherent read of the projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeepAwakeView {
    /// `None` until the first observation resolves.
    pub indicator: Option<KeepAwakeIndicator>,
    /// Grows by one for every change of `indicator`; never resets. The bridge
    /// adds it to Bar's own sequence.
    pub version: u64,
}

/// The validating state machine. Pure: it takes readings and host facts.
#[derive(Debug, Default)]
pub struct Tracker {
    resolved: bool,
    /// The highest generation accepted.
    generation: u64,
    /// The last accepted sequence of that generation.
    sequence: u64,
    /// Whether the last accepted reading is a confirmed adopted active state.
    confirmed: bool,
    indicator: Option<KeepAwakeIndicator>,
    version: u64,
}

impl Tracker {
    /// Applies one reading with the host facts sampled just before or after it
    /// and returns the resulting view.
    pub fn observe(&mut self, reading: &StateReading, host: Option<HostFacts>) -> KeepAwakeView {
        match reading {
            StateReading::Available(envelope) => self.accept_envelope(envelope),
            StateReading::Unavailable(tombstone) => match tombstone.generation {
                // A late tombstone of an older generation says nothing about
                // the one already accepted.
                Some(generation) if generation < self.generation => {}
                generation => {
                    if let Some(generation) = generation
                        && generation > self.generation
                    {
                        self.generation = generation;
                        self.sequence = 0;
                    }
                    self.confirmed = false;
                }
            },
            StateReading::Absent => self.confirmed = false,
        }
        let waiting = matches!(
            reading,
            StateReading::Unavailable(tombstone)
                if matches!(
                    tombstone.reason,
                    StateUnavailableReason::NotStarted | StateUnavailableReason::Starting
                )
        ) && host.is_some_and(|host| host.expects_start);
        if !self.resolved && waiting {
            return self.view();
        }
        self.resolved = true;
        let next = host.map_or(KeepAwakeIndicator::ABSENT, |host| KeepAwakeIndicator {
            active: self.confirmed && host.running && host.enabled && !host.paused,
            paused: host.paused,
            enabled: host.enabled,
        });
        if self.indicator != Some(next) {
            self.indicator = Some(next);
            self.version += 1;
        }
        self.view()
    }

    fn accept_envelope(&mut self, envelope: &StateEnvelope) {
        let older = envelope.generation < self.generation;
        let backwards = envelope.generation == self.generation && envelope.sequence < self.sequence;
        if older || backwards {
            return;
        }
        self.generation = envelope.generation;
        self.sequence = envelope.sequence;
        self.confirmed = confirmed_active(envelope);
    }

    /// The current view without a new reading.
    #[must_use]
    pub const fn view(&self) -> KeepAwakeView {
        KeepAwakeView {
            indicator: self.indicator,
            version: self.version,
        }
    }
}

/// Whether `envelope` is Keep Awake's own schema at the supported version and
/// its runtime is an adopted active assertion on an unlocked screen.
///
/// A mismatched schema version, a foreign provider or a malformed payload is
/// rejected by the codec and never reads as active.
fn confirmed_active(envelope: &StateEnvelope) -> bool {
    envelope.decode::<KeepAwakeRuntimeStatus>(KEEP_AWAKE).is_ok_and(|status| {
        status.assertion_state == AssertionState::Active && status.lock_state == LockState::Unlocked
    })
}

struct Source {
    observer: StateObserver,
    registry: Weak<Registry>,
}

impl Source {
    fn host(&self) -> Option<HostFacts> {
        let registry = self.registry.upgrade()?;
        registry
            .snapshots()
            .iter()
            .find(|snapshot| snapshot.id == KEEP_AWAKE)
            .map(HostFacts::of)
    }
}

type Waker = Arc<dyn Fn() + Send + Sync>;

/// The Keep Awake projection owned by the Bar bridge. Construction is inert.
pub struct KeepAwakeProjection {
    tracker: Mutex<Tracker>,
    source: OnceLock<Source>,
    waker: Mutex<Option<Waker>>,
    cancel: CancellationToken,
    worker: Mutex<Option<JoinHandle<()>>>,
    /// How many times the worker woke up. An idle Bar leaves it unchanged.
    wakeups: AtomicU64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl KeepAwakeProjection {
    /// An unbound projection: its view is unresolved (`null`).
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            tracker: Mutex::new(Tracker::default()),
            source: OnceLock::new(),
            waker: Mutex::new(None),
            cancel: CancellationToken::new(),
            worker: Mutex::new(None),
            wakeups: AtomicU64::new(0),
        })
    }

    /// Sets the callback that tells the bridge the view changed. Never called
    /// under a projection lock.
    pub fn set_waker(&self, waker: Waker) { *lock(&self.waker) = Some(waker); }

    /// Binds the host registry. Later calls are ignored. Starts nothing: the
    /// worker only exists once Bar has run ([`ensure_worker`](Self::ensure_worker)).
    pub fn bind(&self, registry: &Arc<Registry>) {
        let _ = self.source.set(Source {
            observer: registry.observe_state(KEEP_AWAKE),
            registry: Arc::downgrade(registry),
        });
    }

    /// Starts the single push worker if a source is bound and none ran yet. A
    /// disabled Bar never calls this, so it owns no thread.
    pub fn ensure_worker(self: &Arc<Self>) {
        if self.source.get().is_none() || self.cancel.is_cancelled() {
            return;
        }
        let mut worker = lock(&self.worker);
        if worker.is_some() {
            return;
        }
        let projection = Arc::clone(self);
        // On a spawn failure reads still sample; only the push wake-up is
        // missing, and the next Bar change or snapshot carries the newest view.
        *worker = std::thread::Builder::new()
            .name("stache-bar-keep-awake".to_owned())
            .spawn(move || projection.watch())
            .ok();
    }

    /// Re-samples both sources and returns the coherent view.
    #[must_use]
    pub fn view(&self) -> KeepAwakeView {
        let mut tracker = lock(&self.tracker);
        let Some(source) = self.source.get() else {
            return tracker.view();
        };
        // Subscribe-before-read order does not matter here: the cell is
        // authoritative and `read` is coherent with its version.
        let reading = source.observer.read();
        tracker.observe(&reading.state, source.host())
    }

    /// How many times the worker woke up since it started. A quiet provider
    /// never wakes it: there is no periodic timeout.
    #[must_use]
    pub fn wakeups(&self) -> u64 { self.wakeups.load(Ordering::SeqCst) }

    fn watch(&self) {
        let Some(source) = self.source.get() else { return };
        // Subscribe first: any change after this version wakes the wait.
        let mut seen = source.observer.read().version;
        let mut notified = self.view().version;
        loop {
            // No timeout: an overflowing deadline is an unbounded wait that
            // only a change, the registry closing or cancellation ends.
            match source.observer.wait_for_change(seen, Duration::MAX, &self.cancel) {
                StateWait::Changed(snapshot) => seen = snapshot.version,
                StateWait::TimedOut => continue,
                StateWait::Cancelled | StateWait::Closed => return,
            }
            self.wakeups.fetch_add(1, Ordering::SeqCst);
            let view = self.view();
            if view.version != notified {
                notified = view.version;
                let waker = lock(&self.waker).clone();
                if let Some(waker) = waker {
                    waker();
                }
            }
        }
    }

    /// Stage 1: wakes the worker so it exits. Never joins.
    pub fn begin_shutdown(&self) { self.cancel.cancel(); }

    /// Stage 3: joins the worker within a bound. `false` keeps it owned.
    #[must_use]
    pub fn finish_shutdown(&self) -> bool {
        self.begin_shutdown();
        let Some(handle) = lock(&self.worker).take() else {
            return true;
        };
        let deadline = Instant::now() + WATCH_JOIN;
        while !handle.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        if handle.is_finished() {
            let _ = handle.join();
            true
        } else {
            *lock(&self.worker) = Some(handle);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use stache_keep_awake::LockObservation;
    use stache_sdk::{PublishedState, RuntimeEnd, SchemaIdentity, StateUnavailable};

    use super::*;

    const RUNNING: HostFacts = HostFacts {
        running: true,
        enabled: true,
        paused: false,
        expects_start: false,
    };

    fn status(assertion_state: AssertionState, lock_state: LockState) -> KeepAwakeRuntimeStatus {
        KeepAwakeRuntimeStatus {
            assertion_state,
            lock_state,
            observation: LockObservation::NotificationsAndPolling,
            conditions: Vec::new(),
        }
    }

    fn envelope_with(
        generation: u64,
        sequence: u64,
        version: u32,
        payload: serde_json::Value,
    ) -> StateReading {
        StateReading::Available(StateEnvelope {
            provider: KEEP_AWAKE,
            generation,
            revision: 1,
            sequence,
            schema: SchemaIdentity {
                provider: KEEP_AWAKE,
                name: KeepAwakeRuntimeStatus::SCHEMA.name,
                version,
            },
            payload: Arc::new(payload),
        })
    }

    fn available(generation: u64, sequence: u64, status: &KeepAwakeRuntimeStatus) -> StateReading {
        envelope_with(
            generation,
            sequence,
            KeepAwakeRuntimeStatus::SCHEMA.version,
            serde_json::to_value(status).expect("encode"),
        )
    }

    fn tombstone(reason: StateUnavailableReason, generation: Option<u64>) -> StateReading {
        StateReading::Unavailable(StateUnavailable { reason, generation })
    }

    fn indicator(view: KeepAwakeView) -> KeepAwakeIndicator { view.indicator.expect("resolved") }

    fn active_of(reading: &StateReading, host: HostFacts) -> bool {
        indicator(Tracker::default().observe(reading, Some(host))).active
    }

    #[test]
    fn it_is_unresolved_until_the_first_observation_resolves() {
        let mut tracker = Tracker::default();
        assert_eq!(tracker.view(), KeepAwakeView { indicator: None, version: 0 });
        let waiting = HostFacts {
            running: false,
            enabled: true,
            paused: false,
            expects_start: true,
        };
        let reading = tombstone(StateUnavailableReason::NotStarted, None);
        assert_eq!(tracker.observe(&reading, Some(waiting)).indicator, None);
        let starting = tombstone(StateUnavailableReason::Starting, Some(1));
        assert_eq!(tracker.observe(&starting, Some(waiting)).indicator, None);
        assert_eq!(tracker.view().version, 0, "unresolved never bumps the version");

        let live = available(1, 1, &status(AssertionState::Active, LockState::Unlocked));
        let view = tracker.observe(&live, Some(RUNNING));
        assert_eq!(
            view.indicator,
            Some(KeepAwakeIndicator {
                active: true,
                paused: false,
                enabled: true
            })
        );
        assert_eq!(view.version, 1);

        // Once resolved it never returns to null, even while a restart waits.
        let view = tracker.observe(&starting, Some(waiting));
        assert!(!indicator(view).active);
        assert_eq!(view.version, 2);
    }

    #[test]
    fn an_absent_provider_resolves_to_inactive_and_inert() {
        for host in [None, Some(HostFacts { running: false, ..RUNNING })] {
            let mut tracker = Tracker::default();
            let view = tracker.observe(&StateReading::Absent, host);
            let expected = host.map_or(KeepAwakeIndicator::ABSENT, |host| KeepAwakeIndicator {
                active: false,
                paused: host.paused,
                enabled: host.enabled,
            });
            assert_eq!(view.indicator, Some(expected));
        }
        let view = Tracker::default().observe(&StateReading::Absent, None);
        assert_eq!(indicator(view), KeepAwakeIndicator {
            active: false,
            paused: false,
            enabled: false
        });
    }

    #[test]
    fn only_a_confirmed_adopted_active_assertion_on_a_running_module_is_active() {
        let good = available(1, 1, &status(AssertionState::Active, LockState::Unlocked));
        assert!(active_of(&good, RUNNING));
        for (assertion, lock) in [
            (AssertionState::Inactive, LockState::Unlocked),
            (AssertionState::Acquiring, LockState::Unlocked),
            (AssertionState::ReleasePending, LockState::Unlocked),
            (AssertionState::Active, LockState::Locked),
            (AssertionState::Active, LockState::Unknown),
            (AssertionState::Inactive, LockState::Locked),
            (AssertionState::Inactive, LockState::Unknown),
        ] {
            let reading = available(1, 1, &status(assertion, lock));
            assert!(!active_of(&reading, RUNNING), "{assertion:?} / {lock:?}");
        }
        // Host facts gate it: not running, paused or disabled is never active.
        for host in [
            HostFacts { running: false, ..RUNNING },
            HostFacts { paused: true, ..RUNNING },
            HostFacts { enabled: false, ..RUNNING },
        ] {
            assert!(!active_of(&good, host), "{host:?}");
        }
    }

    #[test]
    fn host_session_facts_are_carried_without_being_inferred_from_the_colour() {
        let good = available(1, 1, &status(AssertionState::Active, LockState::Unlocked));
        let paused = HostFacts {
            running: false,
            paused: true,
            ..RUNNING
        };
        assert_eq!(
            indicator(Tracker::default().observe(&good, Some(paused))),
            KeepAwakeIndicator {
                active: false,
                paused: true,
                enabled: true
            }
        );
        let disabled = HostFacts {
            running: false,
            enabled: false,
            ..RUNNING
        };
        assert_eq!(
            indicator(Tracker::default().observe(&good, Some(disabled))),
            KeepAwakeIndicator {
                active: false,
                paused: false,
                enabled: false
            }
        );
        // Running but locked: still pausable, never green.
        let locked = available(1, 1, &status(AssertionState::Inactive, LockState::Locked));
        assert_eq!(
            indicator(Tracker::default().observe(&locked, Some(RUNNING))),
            KeepAwakeIndicator {
                active: false,
                paused: false,
                enabled: true
            }
        );
    }

    #[test]
    fn every_tombstone_is_inactive() {
        for reason in [
            StateUnavailableReason::NotPublished,
            StateUnavailableReason::Ended(RuntimeEnd::Stopped),
            StateUnavailableReason::Ended(RuntimeEnd::Disabled),
            StateUnavailableReason::Ended(RuntimeEnd::Paused),
            StateUnavailableReason::Ended(RuntimeEnd::Failed),
            StateUnavailableReason::Ended(RuntimeEnd::PermissionBlocked),
            StateUnavailableReason::Ended(RuntimeEnd::DependencyUnavailable),
        ] {
            let mut tracker = Tracker::default();
            let live = available(1, 1, &status(AssertionState::Active, LockState::Unlocked));
            assert!(indicator(tracker.observe(&live, Some(RUNNING))).active);
            let view = tracker.observe(&tombstone(reason, Some(1)), Some(RUNNING));
            assert!(!indicator(view).active, "{reason:?}");
        }
    }

    #[test]
    fn a_malformed_or_wrongly_versioned_payload_is_never_active() {
        let good = serde_json::to_value(status(AssertionState::Active, LockState::Unlocked))
            .expect("encode");
        let wrong_version = envelope_with(1, 1, 2, good);
        assert!(!active_of(&wrong_version, RUNNING));
        let malformed = envelope_with(1, 1, 1, serde_json::json!({ "assertionState": "active" }));
        assert!(!active_of(&malformed, RUNNING));
        let wrong_bool = envelope_with(1, 1, 1, serde_json::json!(true));
        assert!(!active_of(&wrong_bool, RUNNING));
    }

    #[test]
    fn late_old_generation_writes_and_tombstones_are_ignored() {
        let mut tracker = Tracker::default();
        let live = available(5, 3, &status(AssertionState::Active, LockState::Unlocked));
        assert!(indicator(tracker.observe(&live, Some(RUNNING))).active);
        let version = tracker.view().version;

        // A late tombstone of the previous generation changes nothing.
        let late_end = tombstone(StateUnavailableReason::Ended(RuntimeEnd::Paused), Some(4));
        assert!(indicator(tracker.observe(&late_end, Some(RUNNING))).active);
        // A late old-generation write cannot flip the state either.
        let late_write = available(4, 9, &status(AssertionState::Inactive, LockState::Locked));
        assert!(indicator(tracker.observe(&late_write, Some(RUNNING))).active);
        // Nor can a backwards sequence of the current generation.
        let backwards = available(5, 2, &status(AssertionState::Inactive, LockState::Locked));
        assert!(indicator(tracker.observe(&backwards, Some(RUNNING))).active);
        assert_eq!(
            tracker.view().version,
            version,
            "ignored readings change nothing"
        );

        // The current generation's own tombstone applies.
        let end = tombstone(StateUnavailableReason::Ended(RuntimeEnd::Disabled), Some(5));
        assert!(!indicator(tracker.observe(&end, Some(RUNNING))).active);
    }

    #[test]
    fn a_new_generation_resets_the_sequence() {
        let mut tracker = Tracker::default();
        let first = available(1, 40, &status(AssertionState::Inactive, LockState::Locked));
        assert!(!indicator(tracker.observe(&first, Some(RUNNING))).active);
        // The next start is generation 2 and its sequence restarts at one.
        let second = available(2, 1, &status(AssertionState::Active, LockState::Unlocked));
        assert!(indicator(tracker.observe(&second, Some(RUNNING))).active);
        let third = available(2, 2, &status(AssertionState::Inactive, LockState::Locked));
        assert!(!indicator(tracker.observe(&third, Some(RUNNING))).active);
    }

    #[test]
    fn an_identical_reading_does_not_change_the_version() {
        let mut tracker = Tracker::default();
        let live = available(1, 1, &status(AssertionState::Active, LockState::Unlocked));
        let first = tracker.observe(&live, Some(RUNNING));
        assert_eq!(tracker.observe(&live, Some(RUNNING)), first);
    }
}
