//! Fakes for default tests: a notification session, a sink and a whole live
//! follow. Nothing here calls `SkyLight`, creates a display link or registers a
//! callback.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use super::{
    EventSlots, FastOrderResult, FastReorder, FollowMove, FollowSink, FollowTarget, LiveFollow,
    LiveStatus, LiveUnavailable, LoopWaker, SessionMaker, StopOutcome, WindowEventKind,
    WindowEventSession,
};
use crate::border_overlay::OverlayRect;
use crate::windows::WindowInstance;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ------------------------------------------------------------------ session

#[derive(Default)]
struct SessionState {
    register_error: Option<String>,
    subscribe_error: Option<String>,
    bounds: BTreeMap<u32, OverlayRect>,
    bounds_reads: Vec<u32>,
    subscriptions: Vec<Vec<u32>>,
    slots: Option<Arc<EventSlots>>,
    registered: bool,
    unregisters: usize,
    created: usize,
    woken: bool,
    pump_threads: Vec<String>,
    /// What `order_above` returns (`Ordered` by default).
    order_result: Option<FastOrderResult>,
    /// Every `order_above` call `(panel, target)`, in order.
    orders: Vec<(u32, u32)>,
    /// Threads that made the order calls.
    order_threads: Vec<String>,
}

#[derive(Default)]
struct SessionShared {
    state: Mutex<SessionState>,
    changed: Condvar,
}

/// The test handle of the fake notification session. Clone it freely.
#[derive(Clone, Default)]
pub struct FakeSessionControl {
    shared: Arc<SessionShared>,
}

impl FakeSessionControl {
    /// A control with a working session.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    fn state(&self) -> MutexGuard<'_, SessionState> { lock(&self.shared.state) }

    /// The constructor to give a `SystemLiveFollow`.
    #[must_use]
    pub fn maker(&self) -> SessionMaker {
        let shared = Arc::clone(&self.shared);
        Arc::new(move || {
            lock(&shared.state).created += 1;
            Box::new(FakeSession { shared: Arc::clone(&shared) }) as Box<dyn WindowEventSession>
        })
    }

    /// Makes registration fail with `reason` (a missing symbol, say).
    pub fn fail_register(&self, reason: &str) {
        self.state().register_error = Some(reason.to_owned());
    }

    /// Makes the next subscriptions fail with `reason`.
    pub fn fail_subscribe(&self, reason: &str) {
        self.state().subscribe_error = Some(reason.to_owned());
    }

    /// Sets the bounds a read of `cg` returns.
    pub fn set_bounds(&self, cg: u32, bounds: OverlayRect) {
        self.state().bounds.insert(cg, bounds);
    }

    /// Injects one event as the window server callback would. `false` when no
    /// callback is registered (nothing recorded).
    #[allow(clippy::must_use_candidate)] // a test may inject without checking
    pub fn inject(&self, kind: WindowEventKind, cg: u32) -> bool {
        let slots = self.state().slots.clone();
        slots.is_some_and(|slots| {
            slots.record(kind, cg);
            true
        })
    }

    /// Injects one raw private event id as the native callback would (raw log
    /// plus the kind bit for a known id). `false` when no callback is
    /// registered.
    #[allow(clippy::must_use_candidate)]
    pub fn inject_raw(&self, event: u32, cg: u32) -> bool {
        let slots = self.state().slots.clone();
        slots.is_some_and(|slots| {
            slots.on_window_event(event, cg);
            true
        })
    }

    /// The slots the registered callback records into.
    #[must_use]
    pub fn slots(&self) -> Option<Arc<EventSlots>> { self.state().slots.clone() }

    /// Whether the callbacks are registered.
    #[must_use]
    pub fn registered(&self) -> bool { self.state().registered }

    /// Sessions created.
    #[must_use]
    pub fn created(&self) -> usize { self.state().created }

    /// How many times the registrations were removed.
    #[must_use]
    pub fn unregisters(&self) -> usize { self.state().unregisters }

    /// Every subscription, in order.
    #[must_use]
    pub fn subscriptions(&self) -> Vec<Vec<u32>> { self.state().subscriptions.clone() }

    /// Every bounds read, in order.
    #[must_use]
    pub fn bounds_reads(&self) -> Vec<u32> { self.state().bounds_reads.clone() }

    /// Makes every later `order_above` return `result`.
    pub fn set_order_result(&self, result: FastOrderResult) {
        self.state().order_result = Some(result);
    }

    /// Every private order call `(panel, target)`, in order.
    #[must_use]
    pub fn orders(&self) -> Vec<(u32, u32)> { self.state().orders.clone() }

    /// Names of the threads that made the order calls.
    #[must_use]
    pub fn order_threads(&self) -> Vec<String> { self.state().order_threads.clone() }

    /// Names of the threads that pumped (the owner thread).
    #[must_use]
    pub fn pump_threads(&self) -> Vec<String> { self.state().pump_threads.clone() }
}

struct FakeSession {
    shared: Arc<SessionShared>,
}

impl WindowEventSession for FakeSession {
    fn waker(&self) -> LoopWaker {
        let shared = Arc::clone(&self.shared);
        Arc::new(move || {
            lock(&shared.state).woken = true;
            shared.changed.notify_all();
        })
    }

    fn register(&mut self, slots: Arc<EventSlots>) -> Result<(), LiveUnavailable> {
        let mut state = lock(&self.shared.state);
        if let Some(reason) = &state.register_error {
            return Err(LiveUnavailable::new(reason.clone()));
        }
        state.slots = Some(slots);
        state.registered = true;
        drop(state);
        Ok(())
    }

    fn subscribe(&mut self, ids: &[u32]) -> Result<(), LiveUnavailable> {
        let mut state = lock(&self.shared.state);
        state.subscriptions.push(ids.to_vec());
        state
            .subscribe_error
            .as_ref()
            .map_or(Ok(()), |reason| Err(LiveUnavailable::new(reason.clone())))
    }

    fn bounds(&mut self, cg_window_id: u32) -> Option<OverlayRect> {
        let mut state = lock(&self.shared.state);
        state.bounds_reads.push(cg_window_id);
        state.bounds.get(&cg_window_id).copied()
    }

    fn order_above(&mut self, panel: u32, target: u32) -> FastOrderResult {
        let mut state = lock(&self.shared.state);
        state.orders.push((panel, target));
        state
            .order_threads
            .push(std::thread::current().name().unwrap_or("unnamed").to_owned());
        state.order_result.unwrap_or(FastOrderResult::Ordered)
    }

    fn pump(&mut self, timeout: Duration) {
        lock(&self.shared.state)
            .pump_threads
            .push(std::thread::current().name().unwrap_or("unnamed").to_owned());
        let (mut after, _) = self
            .shared
            .changed
            .wait_timeout_while(lock(&self.shared.state), timeout, |state| !state.woken)
            .unwrap_or_else(PoisonError::into_inner);
        after.woken = false;
        drop(after);
    }

    fn unregister(&mut self) {
        let mut state = lock(&self.shared.state);
        state.registered = false;
        state.slots = None;
        state.unregisters += 1;
    }
}

// --------------------------------------------------------------------- sink

/// One call a [`FakeSink`] recorded.
#[derive(Clone, Debug, PartialEq)]
pub enum SinkCall {
    /// A batch of live samples.
    Moves(Vec<FollowMove>),
    /// A follow state change.
    Following(WindowInstance, bool),
    /// A private hide.
    Hide(WindowInstance),
    /// A private unhide.
    Unhide(WindowInstance),
    /// A private reorder.
    Reorder(WindowInstance),
    /// A private close.
    Close(WindowInstance),
    /// Raw events of one owner pass (`(event id, window id)`, in order).
    Raw(Vec<(u32, u32)>),
    /// One fast reorder attempt.
    FastReorder(FastReorder),
}

#[derive(Default)]
struct SinkState {
    calls: Vec<SinkCall>,
    busy: Vec<WindowInstance>,
    stamp: u64,
}

/// A recording sink. Tiling owns the windows marked busy.
#[derive(Clone, Default)]
pub struct FakeSink {
    state: Arc<Mutex<SinkState>>,
}

impl FakeSink {
    /// An empty sink.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Every call so far.
    #[must_use]
    pub fn calls(&self) -> Vec<SinkCall> { lock(&self.state).calls.clone() }

    /// Only the batches of live samples.
    #[must_use]
    pub fn batches(&self) -> Vec<Vec<FollowMove>> {
        self.calls()
            .into_iter()
            .filter_map(|call| match call {
                SinkCall::Moves(moves) => Some(moves),
                _ => None,
            })
            .collect()
    }

    /// Marks `window` as written by Tiling (no live sample is read).
    pub fn set_busy(&self, window: WindowInstance, busy: bool) {
        let mut state = lock(&self.state);
        state.busy.retain(|w| *w != window);
        if busy {
            state.busy.push(window);
        }
    }
}

impl FollowSink for FakeSink {
    fn authority(&self, window: WindowInstance) -> Option<u64> {
        let state = lock(&self.state);
        (!state.busy.contains(&window)).then_some(state.stamp)
    }

    fn moves(&self, moves: &[FollowMove]) {
        lock(&self.state).calls.push(SinkCall::Moves(moves.to_vec()));
    }

    fn following(&self, window: WindowInstance, following: bool) {
        lock(&self.state).calls.push(SinkCall::Following(window, following));
    }

    fn hide(&self, window: WindowInstance) { lock(&self.state).calls.push(SinkCall::Hide(window)); }

    fn unhide(&self, window: WindowInstance) {
        lock(&self.state).calls.push(SinkCall::Unhide(window));
    }

    fn reorder(&self, window: WindowInstance) {
        lock(&self.state).calls.push(SinkCall::Reorder(window));
    }

    fn close(&self, window: WindowInstance) {
        lock(&self.state).calls.push(SinkCall::Close(window));
    }

    fn fast_reorder(&self, report: FastReorder) {
        lock(&self.state).calls.push(SinkCall::FastReorder(report));
    }

    fn raw_events(&self, events: &[super::RawWindowEvent]) {
        let raw = events.iter().map(|e| (e.event, e.cg_window_id)).collect();
        lock(&self.state).calls.push(SinkCall::Raw(raw));
    }
}

// -------------------------------------------------------------- live follow

struct FollowState {
    status: LiveStatus,
    started: usize,
    stopped: usize,
    targets: Vec<Vec<FollowTarget>>,
    sink: Option<Arc<dyn FollowSink>>,
    /// While set, `stop` times out like a stuck owner (the sink is kept).
    stuck: bool,
}

/// A manual [`LiveFollow`]: the test sets the status and plays the owner by
/// calling the sink it was given. Clone it to keep a handle.
#[derive(Clone)]
pub struct FakeLiveFollow {
    state: Arc<Mutex<FollowState>>,
}

impl FakeLiveFollow {
    /// A live follow that reports `status`.
    #[must_use]
    pub fn new(status: LiveStatus) -> Self {
        Self {
            state: Arc::new(Mutex::new(FollowState {
                status,
                started: 0,
                stopped: 0,
                targets: Vec::new(),
                sink: None,
                stuck: false,
            })),
        }
    }

    /// An active live follow.
    #[must_use]
    pub fn active() -> Self { Self::new(LiveStatus::Active) }

    /// An unavailable live follow with the missing-symbol style reason.
    #[must_use]
    pub fn unavailable(reason: &str) -> Self {
        Self::new(LiveStatus::Unavailable(reason.to_owned()))
    }

    /// While `stuck`, `stop` reports a timed out owner and keeps the source
    /// running; a later `stop` after `hold_stop(false)` succeeds (the retry).
    pub fn hold_stop(&self, stuck: bool) { lock(&self.state).stuck = stuck; }

    /// Changes the reported status.
    pub fn set_status(&self, status: LiveStatus) { lock(&self.state).status = status; }

    /// The sink the engine gave at `start`.
    #[must_use]
    pub fn sink(&self) -> Option<Arc<dyn FollowSink>> { lock(&self.state).sink.clone() }

    /// Times started.
    #[must_use]
    pub fn started(&self) -> usize { lock(&self.state).started }

    /// Times stopped.
    #[must_use]
    pub fn stopped(&self) -> usize { lock(&self.state).stopped }

    /// Every target set the engine pushed, in order.
    #[must_use]
    pub fn target_sets(&self) -> Vec<Vec<FollowTarget>> { lock(&self.state).targets.clone() }
}

impl LiveFollow for FakeLiveFollow {
    fn start(&mut self, sink: Arc<dyn FollowSink>) {
        let mut state = lock(&self.state);
        state.started += 1;
        state.sink = Some(sink);
    }

    fn set_targets(&mut self, targets: &[FollowTarget]) {
        lock(&self.state).targets.push(targets.to_vec());
    }

    fn status(&self) -> LiveStatus { lock(&self.state).status.clone() }

    fn stop(&mut self) -> StopOutcome {
        let mut state = lock(&self.state);
        state.stopped += 1;
        if state.stuck {
            return StopOutcome::TimedOut {
                owner_joined: false,
                callbacks_drained: true,
            };
        }
        if state.sink.take().is_some() {
            StopOutcome::Stopped
        } else {
            StopOutcome::NotRunning
        }
    }
}
