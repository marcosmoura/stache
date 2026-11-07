//! Fakes for the presenter, geometry and main-thread dispatch. They never touch
//! `AppKit`.
//!
//! [`FakeMainThread`] is a real thread standing in for the main thread. Its
//! dispatch queue can be paused (a stalled main), can reject, and a fake menu's
//! nested tracking loop can either drain the dispatch queue (a tracking-mode
//! compatible dispatcher) or only user input (a GCD-style queue that does not
//! drain during tracking).

#![allow(missing_docs, reason = "test fakes are self-describing")]
#![allow(clippy::must_use_candidate, reason = "test fakes")]
#![allow(clippy::missing_panics_doc, reason = "test fakes")]
#![allow(
    clippy::significant_drop_tightening,
    reason = "the fakes hold one short-lived lock per scripted step"
)]

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, ThreadId};
use std::time::Duration;

use super::dispatch::{DispatchError, MainDispatcher, MainJob};
use super::geometry::{AppKitPoint, GeometryError, GeometrySnapshot, GeometrySource};
use super::presenter::{
    CancelResult, PresenterBackend, PresenterError, RemoveResult, RenderPlan, SelectionSink,
    TrackResult, contain_selection, plan,
};
use super::snapshot::{ActionToken, MenuSnapshot, PresentationId};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct MainState {
    queue: VecDeque<MainJob>,
    user: VecDeque<MainJob>,
    reject: bool,
    paused: bool,
    stop: bool,
    thread: Option<ThreadId>,
}

/// A thread standing in for the main thread.
pub struct FakeMainThread {
    state: Mutex<MainState>,
    signal: Condvar,
    handle: Mutex<Option<thread::JoinHandle<()>>>,
}

impl FakeMainThread {
    pub fn start() -> Arc<Self> {
        let main = Arc::new(Self {
            state: Mutex::new(MainState::default()),
            signal: Condvar::new(),
            handle: Mutex::new(None),
        });
        let runner = Arc::clone(&main);
        let handle = thread::Builder::new()
            .name("fake-main".to_owned())
            .spawn(move || {
                lock(&runner.state).thread = Some(thread::current().id());
                runner.pump(true, &|| lock(&runner.state).stop);
            })
            .expect("spawn fake main");
        *lock(&main.handle) = Some(handle);
        // Wait for the thread id to be published.
        while lock(&main.state).thread.is_none() {
            thread::sleep(Duration::from_millis(1));
        }
        main
    }

    pub fn is_main(&self) -> bool { lock(&self.state).thread == Some(thread::current().id()) }

    pub fn reject(&self, reject: bool) { lock(&self.state).reject = reject; }

    /// Stalls the main thread: no queued job runs until [`Self::resume`].
    pub fn pause(&self) { lock(&self.state).paused = true; }

    pub fn resume(&self) {
        lock(&self.state).paused = false;
        self.signal.notify_all();
    }

    pub fn queued(&self) -> usize { lock(&self.state).queue.len() }

    /// Queues simulated user input (always drained, even while tracking).
    pub fn user(&self, job: MainJob) {
        lock(&self.state).user.push_back(job);
        self.signal.notify_all();
    }

    /// Runs queued work until `done`. `drain` selects whether the normal
    /// dispatch queue runs here (tracking-mode compatible) or only user input.
    pub fn pump(&self, drain: bool, done: &dyn Fn() -> bool) {
        loop {
            if done() {
                return;
            }
            let job = {
                let mut state = lock(&self.state);
                if state.stop {
                    return;
                }
                let job = if state.paused {
                    None
                } else if let Some(job) = state.user.pop_front() {
                    Some(job)
                } else if drain {
                    state.queue.pop_front()
                } else {
                    None
                };
                if job.is_none() {
                    let _ = self
                        .signal
                        .wait_timeout(state, Duration::from_millis(2))
                        .unwrap_or_else(PoisonError::into_inner);
                }
                job
            };
            if let Some(job) = job {
                job();
            }
        }
    }

    pub fn shutdown(&self) {
        lock(&self.state).stop = true;
        self.signal.notify_all();
        let handle = lock(&self.handle).take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }
}

impl MainDispatcher for FakeMainThread {
    fn dispatch(&self, job: MainJob) -> Result<(), DispatchError> {
        let mut state = lock(&self.state);
        if state.reject {
            return Err(DispatchError::Rejected);
        }
        state.queue.push_back(job);
        drop(state);
        self.signal.notify_all();
        Ok(())
    }
}

struct FakeMenu {
    plan: RenderPlan,
    sink: SelectionSink,
    tracking: bool,
    closed: bool,
    cancelled_while_tracking: bool,
}

#[derive(Default)]
struct PresenterState {
    menus: HashMap<PresentationId, FakeMenu>,
    prepared: Vec<PresentationId>,
    tracked: Vec<PresentationId>,
    track_locations: Vec<AppKitPoint>,
    removed: Vec<PresentationId>,
    fail_prepare: bool,
    panic_in_sink: bool,
}

type PreparedHook = Arc<dyn Fn() + Send + Sync>;

/// A fake presenter whose "native" state lives behind a mutex but is only
/// touched from the fake main thread (asserted).
pub struct FakePresenter {
    main: Arc<FakeMainThread>,
    state: Mutex<PresenterState>,
    drain_during_tracking: AtomicBool,
    off_main_calls: AtomicUsize,
    on_prepared: Mutex<Option<PreparedHook>>,
}

impl FakePresenter {
    pub fn new(main: Arc<FakeMainThread>) -> Arc<Self> {
        Arc::new(Self {
            main,
            state: Mutex::new(PresenterState::default()),
            drain_during_tracking: AtomicBool::new(true),
            off_main_calls: AtomicUsize::new(0),
            on_prepared: Mutex::new(None),
        })
    }

    /// `false` models a dispatcher that does not run during tracking.
    pub fn drain_during_tracking(&self, drain: bool) {
        self.drain_during_tracking.store(drain, Ordering::SeqCst);
    }

    /// Runs `hook` on the main thread right after each successful `prepare`,
    /// modelling a stop that races the show.
    pub fn after_prepare(&self, hook: Option<PreparedHook>) { *lock(&self.on_prepared) = hook; }

    pub fn fail_prepare(&self, fail: bool) { lock(&self.state).fail_prepare = fail; }

    pub fn off_main_calls(&self) -> usize { self.off_main_calls.load(Ordering::SeqCst) }

    pub fn prepared(&self) -> Vec<PresentationId> { lock(&self.state).prepared.clone() }

    pub fn tracked(&self) -> Vec<PresentationId> { lock(&self.state).tracked.clone() }

    /// Where each tracked menu was popped up, in `AppKit` coordinates.
    pub fn track_locations(&self) -> Vec<AppKitPoint> { lock(&self.state).track_locations.clone() }

    pub fn removed(&self) -> Vec<PresentationId> { lock(&self.state).removed.clone() }

    pub fn has_menu(&self, id: PresentationId) -> bool { lock(&self.state).menus.contains_key(&id) }

    pub fn is_tracking(&self, id: PresentationId) -> bool {
        lock(&self.state).menus.get(&id).is_some_and(|menu| menu.tracking)
    }

    pub fn cancelled_while_tracking(&self, id: PresentationId) -> bool {
        lock(&self.state)
            .menus
            .get(&id)
            .is_some_and(|menu| menu.cancelled_while_tracking)
    }

    pub fn plan(&self, id: PresentationId) -> Option<RenderPlan> {
        lock(&self.state).menus.get(&id).map(|menu| menu.plan.clone())
    }

    /// Makes selections panic inside the sink to prove containment.
    pub fn panic_in_sink(&self, panic: bool) { lock(&self.state).panic_in_sink = panic; }

    /// Simulates the user choosing `token`: runs the menu action callback on
    /// the fake main thread, then closes the menu.
    pub fn choose(self: &Arc<Self>, id: PresentationId, token: ActionToken) {
        let presenter = Arc::clone(self);
        self.main.user(Box::new(move || {
            let (sink, panics) = {
                let state = lock(&presenter.state);
                (
                    state.menus.get(&id).map(|menu| Arc::clone(&menu.sink)),
                    state.panic_in_sink,
                )
            };
            if let Some(sink) = sink {
                let sink: SelectionSink = if panics {
                    Arc::new(move |token| {
                        sink(token);
                        panic!("injected menu action panic");
                    })
                } else {
                    sink
                };
                let _ = contain_selection(&sink, token);
            }
            if let Some(menu) = lock(&presenter.state).menus.get_mut(&id) {
                menu.closed = true;
            }
        }));
    }

    /// Simulates the user dismissing the menu.
    pub fn dismiss(self: &Arc<Self>, id: PresentationId) {
        let presenter = Arc::clone(self);
        self.main.user(Box::new(move || {
            if let Some(menu) = lock(&presenter.state).menus.get_mut(&id) {
                menu.closed = true;
            }
        }));
    }

    fn check_main(&self) {
        if !self.main.is_main() {
            self.off_main_calls.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl PresenterBackend for FakePresenter {
    fn prepare(
        &self,
        presentation: PresentationId,
        snapshot: &MenuSnapshot,
        select: SelectionSink,
    ) -> Result<(), PresenterError> {
        self.check_main();
        let mut state = lock(&self.state);
        if state.fail_prepare {
            return Err(PresenterError::Build("injected".to_owned()));
        }
        if state.menus.contains_key(&presentation) {
            return Err(PresenterError::Duplicate);
        }
        state.prepared.push(presentation);
        state.menus.insert(presentation, FakeMenu {
            plan: plan(snapshot),
            sink: select,
            tracking: false,
            closed: false,
            cancelled_while_tracking: false,
        });
        drop(state);
        let hook = lock(&self.on_prepared).clone();
        if let Some(hook) = hook {
            hook();
        }
        Ok(())
    }

    fn track(&self, presentation: PresentationId, location: AppKitPoint) -> TrackResult {
        self.check_main();
        {
            let mut state = lock(&self.state);
            let Some(menu) = state.menus.get_mut(&presentation) else {
                return TrackResult::Unknown;
            };
            menu.tracking = true;
            state.tracked.push(presentation);
            state.track_locations.push(location);
        }
        let drain = self.drain_during_tracking.load(Ordering::SeqCst);
        self.main.pump(drain, &|| {
            lock(&self.state).menus.get(&presentation).is_none_or(|menu| menu.closed)
        });
        if let Some(menu) = lock(&self.state).menus.get_mut(&presentation) {
            menu.tracking = false;
        }
        TrackResult::Closed
    }

    fn cancel_tracking(&self, presentation: PresentationId) -> CancelResult {
        self.check_main();
        let mut state = lock(&self.state);
        match state.menus.get_mut(&presentation) {
            None => CancelResult::Unknown,
            Some(menu) if menu.tracking => {
                menu.closed = true;
                menu.cancelled_while_tracking = true;
                CancelResult::Requested
            }
            Some(_) => CancelResult::NotTracking,
        }
    }

    fn remove(&self, presentation: PresentationId) -> RemoveResult {
        self.check_main();
        let mut state = lock(&self.state);
        match state.menus.get(&presentation) {
            None => RemoveResult::Unknown,
            Some(menu) if menu.tracking => RemoveResult::StillTracking,
            Some(_) => {
                state.menus.remove(&presentation);
                state.removed.push(presentation);
                RemoveResult::Removed
            }
        }
    }
}

/// A scriptable geometry source.
pub struct FakeGeometry {
    current: Mutex<Result<GeometrySnapshot, GeometryError>>,
    reads: AtomicUsize,
}

impl FakeGeometry {
    pub fn new(snapshot: GeometrySnapshot) -> Arc<Self> {
        Arc::new(Self {
            current: Mutex::new(Ok(snapshot)),
            reads: AtomicUsize::new(0),
        })
    }

    pub fn set(&self, result: Result<GeometrySnapshot, GeometryError>) {
        *lock(&self.current) = result;
    }

    pub fn reads(&self) -> usize { self.reads.load(Ordering::SeqCst) }
}

impl GeometrySource for FakeGeometry {
    fn snapshot(&self) -> Result<GeometrySnapshot, GeometryError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        lock(&self.current).clone()
    }
}
