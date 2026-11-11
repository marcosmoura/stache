//! The owner thread and the source that owns it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::follower::{FollowCore, FollowCounters, FollowTiming};
use super::slots::{EventSlots, SlotsCounters};
use super::{
    FollowSink, FollowTarget, LiveFollow, LiveFollowFactory, LiveStatus, OWNER_THREAD_NAME,
    StopObserver, StopOutcome, WindowEventSession,
};
use crate::border_overlay::MAX_OVERLAYS;
use crate::border_overlay::space_sources::{Exit, ExitGuard, STOP_BUDGET};
use crate::display_link::{FrameClockFactory, StuckClock, WakeFn};

/// How long one run-loop turn may wait when nothing is scheduled (a day; wakes
/// come from callbacks, clocks and the actor).
const IDLE_TIMEOUT: Duration = Duration::from_hours(24);

/// Creates the session on the owner thread.
pub type SessionMaker = Arc<dyn Fn() -> Box<dyn WindowEventSession> + Send + Sync>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Shared {
    targets: Mutex<Option<Vec<FollowTarget>>>,
    stop: AtomicBool,
    status: Mutex<LiveStatus>,
    stuck: Mutex<Vec<StuckClock>>,
    counters: Mutex<FollowCounters>,
}

struct Running {
    handle: JoinHandle<()>,
    shared: Arc<Shared>,
    slots: Arc<EventSlots>,
    exit: Arc<Exit>,
}

/// The native live follow; see the module documentation of `live_follow`.
/// Nothing exists before `start`.
pub struct SystemLiveFollow {
    maker: SessionMaker,
    clocks: Arc<dyn FrameClockFactory>,
    timing: FollowTiming,
    budget: Duration,
    observer: Option<StopObserver>,
    running: Option<Running>,
    /// Clocks that could not stop: retained, retried, never dropped.
    retained: Vec<StuckClock>,
    status: LiveStatus,
    last_stop: Option<StopOutcome>,
    last_counters: FollowCounters,
}

impl SystemLiveFollow {
    /// A source over `maker` (the session constructor, run on the owner thread)
    /// and the shared frame clock factory.
    #[must_use]
    pub fn new(
        maker: SessionMaker,
        clocks: Arc<dyn FrameClockFactory>,
        observer: Option<StopObserver>,
    ) -> Self {
        Self {
            maker,
            clocks,
            timing: FollowTiming::default(),
            budget: STOP_BUDGET,
            observer,
            running: None,
            retained: Vec::new(),
            status: LiveStatus::Starting,
            last_stop: None,
            last_counters: FollowCounters::default(),
        }
    }

    /// The system source: native session, shared frame clocks. Creates nothing
    /// until `start`.
    #[must_use]
    pub fn system(clocks: Arc<dyn FrameClockFactory>, observer: Option<StopObserver>) -> Self {
        Self::new(super::native::session_maker(), clocks, observer)
    }

    /// A factory creating a fresh system source for each run of the module.
    #[must_use]
    pub fn factory(
        clocks: Arc<dyn FrameClockFactory>,
        observer: Option<StopObserver>,
    ) -> LiveFollowFactory {
        Arc::new(move || {
            Box::new(Self::system(Arc::clone(&clocks), observer.clone())) as Box<dyn LiveFollow>
        })
    }

    /// Overrides the pacing (tests use a short quiet period).
    #[must_use]
    pub const fn with_timing(mut self, timing: FollowTiming) -> Self {
        self.timing = timing;
        self
    }

    /// Overrides the stop budget (tests).
    #[must_use]
    pub const fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// The outcome of the latest stop.
    #[must_use]
    pub const fn last_stop(&self) -> Option<StopOutcome> { self.last_stop }

    /// Clocks retained because they could not stop.
    #[must_use]
    pub fn stuck_clocks(&self) -> usize {
        self.retained.len()
            + self.running.as_ref().map_or(0, |running| lock(&running.shared.stuck).len())
    }

    /// The follower counters as of the owner's last pass.
    #[must_use]
    pub fn follow_counters(&self) -> FollowCounters {
        self.running.as_ref().map_or(self.last_counters, |r| *lock(&r.shared.counters))
    }

    /// The slot counters of the running owner.
    #[must_use]
    pub fn slot_counters(&self) -> SlotsCounters {
        self.running.as_ref().map(|r| r.slots.counters()).unwrap_or_default()
    }

    /// Stops within `budget`: closes the slots first, posts a stop, joins.
    pub fn stop_bounded(&mut self, budget: Duration) -> StopOutcome {
        let outcome = self.stop_inner(budget);
        self.last_stop = Some(outcome);
        if let Some(observer) = &self.observer {
            observer(outcome);
        }
        outcome
    }

    fn stop_inner(&mut self, budget: Duration) -> StopOutcome {
        let had_retained = !self.retained.is_empty();
        self.retry_retained(budget);
        let Some(running) = self.running.as_ref() else {
            return if self.retained.is_empty() {
                if had_retained {
                    StopOutcome::Stopped
                } else {
                    StopOutcome::NotRunning
                }
            } else {
                StopOutcome::TimedOut {
                    owner_joined: true,
                    callbacks_drained: false,
                }
            };
        };
        // 1. Close the slots: no callback records after this returns true.
        let callbacks_drained = running.slots.close(budget);
        // 2. Ask the owner to leave and 3. join within the bound.
        running.shared.stop.store(true, Ordering::SeqCst);
        running.slots.wake();
        let owner_joined = running.exit.wait(budget);
        if owner_joined && let Some(running) = self.running.take() {
            // The owner finished; the join returns at once.
            let _ = running.handle.join();
            self.retained.append(&mut lock(&running.shared.stuck));
            self.last_counters = *lock(&running.shared.counters);
        }
        let clocks_free = self.retained.is_empty();
        if owner_joined && callbacks_drained && clocks_free {
            StopOutcome::Stopped
        } else {
            StopOutcome::TimedOut {
                owner_joined,
                callbacks_drained: callbacks_drained && clocks_free,
            }
        }
    }

    fn retry_retained(&mut self, budget: Duration) {
        for stuck in std::mem::take(&mut self.retained) {
            if let Err(stuck) = stuck.retry(budget.min(Duration::from_millis(200))) {
                self.retained.push(stuck);
            }
        }
    }
}

impl LiveFollow for SystemLiveFollow {
    fn start(&mut self, sink: Arc<dyn FollowSink>) {
        if self.running.is_some() && self.stop_bounded(self.budget) != StopOutcome::Stopped {
            self.status = LiveStatus::Unavailable("the previous owner is still stopping".into());
            return;
        }
        let shared = Arc::new(Shared {
            targets: Mutex::new(None),
            stop: AtomicBool::new(false),
            status: Mutex::new(LiveStatus::Starting),
            stuck: Mutex::new(Vec::new()),
            counters: Mutex::new(FollowCounters::default()),
        });
        let slots = Arc::new(EventSlots::for_current_process());
        let exit = Arc::new(Exit::default());
        self.status = LiveStatus::Starting;
        let spawned = std::thread::Builder::new().name(OWNER_THREAD_NAME.to_owned()).spawn({
            let shared = Arc::clone(&shared);
            let slots = Arc::clone(&slots);
            let exit = Arc::clone(&exit);
            let maker = Arc::clone(&self.maker);
            let clocks = Arc::clone(&self.clocks);
            let timing = self.timing;
            move || {
                let _guard = ExitGuard(exit);
                run_owner(&maker, &slots, &shared, sink, clocks, timing);
            }
        });
        match spawned {
            Ok(handle) => self.running = Some(Running { handle, shared, slots, exit }),
            Err(error) => {
                self.status = LiveStatus::Unavailable(format!("owner thread failed: {error}"));
            }
        }
    }

    fn set_targets(&mut self, targets: &[FollowTarget]) {
        let Some(running) = &self.running else { return };
        let mut targets = targets.to_vec();
        targets.truncate(MAX_OVERLAYS);
        *lock(&running.shared.targets) = Some(targets);
        running.slots.wake();
    }

    fn status(&self) -> LiveStatus {
        self.running
            .as_ref()
            .map_or_else(|| self.status.clone(), |r| lock(&r.shared.status).clone())
    }

    fn stop(&mut self) -> StopOutcome { self.stop_bounded(self.budget) }
}

/// Live follow owner threads forgotten by a drop whose stop timed out.
static FORGOTTEN_OWNERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many live follow owner threads were forgotten by a drop whose stop
/// timed out (a diagnostic; see `Drop for SystemLiveFollow`).
#[must_use]
pub fn forgotten_follow_owners() -> u64 { FORGOTTEN_OWNERS.load(Ordering::Relaxed) }

impl Drop for SystemLiveFollow {
    /// Stops within the budget. A stop that times out has nobody left to retry
    /// it: the owner thread, its handle and the state its callbacks use are
    /// forgotten explicitly (never freed under a running callback, never a
    /// silent detach). The observer already received the `TimedOut` outcome,
    /// and [`forgotten_follow_owners`] counts it.
    fn drop(&mut self) {
        if self.running.is_some() {
            let _ = self.stop_bounded(self.budget);
        }
        if let Some(running) = self.running.take() {
            FORGOTTEN_OWNERS.fetch_add(1, Ordering::Relaxed);
            std::mem::forget(running);
        }
        // A clock still stuck here is leaked on purpose (never freed under a
        // running callback): `StuckClock` has no destructor that releases it.
        for stuck in std::mem::take(&mut self.retained) {
            std::mem::forget(stuck);
        }
    }
}

/// The owner thread body.
fn run_owner(
    maker: &SessionMaker,
    slots: &Arc<EventSlots>,
    shared: &Arc<Shared>,
    sink: Arc<dyn FollowSink>,
    clocks: Arc<dyn FrameClockFactory>,
    timing: FollowTiming,
) {
    let mut session = maker();
    slots.set_waker(Some(session.waker()));
    let wake: WakeFn = {
        let slots = Arc::clone(slots);
        Arc::new(move || slots.wake())
    };
    let mut core = FollowCore::new(timing, clocks, sink, wake);
    let mut usable = match session.register(Arc::clone(slots)) {
        Ok(()) => {
            *lock(&shared.status) = LiveStatus::Active;
            true
        }
        Err(error) => {
            *lock(&shared.status) = LiveStatus::Unavailable(error.reason);
            false
        }
    };
    while !shared.stop.load(Ordering::SeqCst) {
        let now = Instant::now();
        let posted = lock(&shared.targets).take();
        if let Some(targets) = posted
            && usable
        {
            let windows: Vec<(u32, Option<i32>)> =
                targets.iter().map(|t| (t.cg_window_id, t.owner_pid)).collect();
            let watched = slots.watch(&windows);
            let followed: Vec<FollowTarget> =
                targets.into_iter().filter(|t| watched.contains(&t.cg_window_id)).collect();
            core.set_targets(&followed);
            if let Err(error) = session.subscribe(&watched) {
                *lock(&shared.status) = LiveStatus::Unavailable(error.reason);
                usable = false;
                let _ = core.shutdown();
                session.unregister();
                slots.watch(&[]);
            }
        }
        if usable {
            for event in slots.drain() {
                core.on_event(now, event, &mut |id| session.bounds(id));
            }
            core.tick(now, &mut |id| session.bounds(id));
        }
        *lock(&shared.counters) = core.counters();
        let timeout = core
            .next_deadline(now)
            .map_or(IDLE_TIMEOUT, |deadline| deadline.saturating_duration_since(now));
        session.pump(timeout);
    }
    let _ = core.shutdown();
    lock(&shared.stuck).append(&mut core.take_stuck());
    *lock(&shared.counters) = core.counters();
    session.unregister();
    slots.set_waker(None);
}
