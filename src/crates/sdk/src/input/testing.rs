//! Fakes for the input contract. They never touch a real tap or hotkey.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use super::control::{
    AckSignal, ControlAction, ControlQueue, ControlRejected, ControlRequest, InputBackend,
    InputClock, NativeError, TapKind,
};
use super::work::WorkReservations;

type Hook = Arc<dyn Fn() + Send + Sync>;

/// A manually advanced clock. `wait_for` advances fake time instead of sleeping
/// and runs registered hooks so fake owners can deliver due acknowledgements.
pub struct FakeClock {
    now: Mutex<Instant>,
    hooks: Mutex<Vec<Hook>>,
    waits: Mutex<Vec<Duration>>,
}

impl FakeClock {
    /// A clock at an arbitrary fixed origin.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            now: Mutex::new(Instant::now()),
            hooks: Mutex::new(Vec::new()),
            waits: Mutex::new(Vec::new()),
        })
    }

    /// Advances fake time and runs hooks.
    pub fn advance(&self, by: Duration) {
        {
            let mut now = self.now.lock().unwrap_or_else(PoisonError::into_inner);
            *now += by;
        }
        let hooks: Vec<Hook> = self.hooks.lock().unwrap_or_else(PoisonError::into_inner).clone();
        for hook in hooks {
            hook();
        }
    }

    /// Registers a closure run after every advance.
    pub fn on_advance(&self, hook: impl Fn() + Send + Sync + 'static) {
        self.hooks.lock().unwrap_or_else(PoisonError::into_inner).push(Arc::new(hook));
    }

    /// Every timeout requested through `wait_for`.
    #[must_use]
    pub fn waits(&self) -> Vec<Duration> {
        self.waits.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Time elapsed since `since`.
    #[must_use]
    pub fn elapsed_since(&self, since: Instant) -> Duration {
        self.now().saturating_duration_since(since)
    }
}

impl InputClock for FakeClock {
    fn now(&self) -> Instant { *self.now.lock().unwrap_or_else(PoisonError::into_inner) }

    fn wait_for(&self, signal: &AckSignal, timeout: Duration) {
        if signal.is_set() {
            return;
        }
        self.waits.lock().unwrap_or_else(PoisonError::into_inner).push(timeout);
        self.advance(timeout);
    }
}

/// What the fake owner does with a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeResponse {
    /// Perform and acknowledge success at once.
    Ack,
    /// Perform and acknowledge a native failure at once.
    Fail(String),
    /// Reject at submit time.
    Reject(ControlRejected),
    /// Queue and never acknowledge on its own; tests call
    /// [`FakeBackend::process_next`].
    Hold,
    /// Acknowledge once fake time has advanced this far.
    AckAfter(Duration),
    /// Drop the request without acknowledging.
    Drop,
}

/// One native operation the fake performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeCall {
    /// The tap.
    pub kind: TapKind,
    /// The action actually performed on native state.
    pub action: ControlAction,
    /// The tap generation.
    pub generation: u64,
}

#[derive(Default)]
struct BackendState {
    script: VecDeque<(Option<(TapKind, ControlAction)>, FakeResponse)>,
    delayed: Vec<(Instant, ControlRequest)>,
    submitted: Vec<(TapKind, ControlAction)>,
    performed: Vec<NativeCall>,
    suppressing_live: bool,
    listen_only_live: bool,
}

/// A fake native owner over a real [`ControlQueue`] (256 bound).
pub struct FakeBackend {
    queue: ControlQueue,
    default: Mutex<FakeResponse>,
    state: Mutex<BackendState>,
    clock: Arc<FakeClock>,
    submits: AtomicUsize,
}

impl FakeBackend {
    /// An owner that acknowledges everything immediately.
    #[must_use]
    pub fn new(clock: &Arc<FakeClock>) -> Arc<Self> {
        let backend = Arc::new(Self {
            queue: ControlQueue::new(),
            default: Mutex::new(FakeResponse::Ack),
            state: Mutex::new(BackendState::default()),
            clock: Arc::clone(clock),
            submits: AtomicUsize::new(0),
        });
        let weak: Weak<Self> = Arc::downgrade(&backend);
        clock.on_advance(move || {
            if let Some(backend) = weak.upgrade() {
                backend.deliver_due();
            }
        });
        backend
    }

    fn state(&self) -> std::sync::MutexGuard<'_, BackendState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sets the response used when no scripted response matches.
    pub fn set_default(&self, response: FakeResponse) {
        *self.default.lock().unwrap_or_else(PoisonError::into_inner) = response;
    }

    /// Scripts the response for the next request matching `kind` and `action`.
    pub fn script(&self, kind: TapKind, action: ControlAction, response: FakeResponse) {
        self.state().script.push_back((Some((kind, action)), response));
    }

    /// Scripts the next request regardless of kind.
    pub fn script_next(&self, response: FakeResponse) {
        self.state().script.push_back((None, response));
    }

    /// Every request submitted (including rejected and held), in order.
    #[must_use]
    pub fn submitted(&self) -> Vec<(TapKind, ControlAction)> { self.state().submitted.clone() }

    /// Native operations actually performed (cancelled requests are absent).
    #[must_use]
    pub fn performed(&self) -> Vec<NativeCall> { self.state().performed.clone() }

    /// Number of performed calls with this kind and action.
    #[must_use]
    pub fn count(&self, kind: TapKind, action: ControlAction) -> usize {
        self.state()
            .performed
            .iter()
            .filter(|c| c.kind == kind && c.action == action)
            .count()
    }

    /// Whether the fake's native tap of this kind currently exists.
    #[must_use]
    pub fn is_live(&self, kind: TapKind) -> bool {
        let state = self.state();
        match kind {
            TapKind::Suppressing => state.suppressing_live,
            TapKind::ListenOnly => state.listen_only_live,
        }
    }

    /// Requests waiting in the owner queue.
    #[must_use]
    pub fn queued(&self) -> usize { self.queue.len() }

    /// Total `submit` calls.
    #[must_use]
    pub fn submit_calls(&self) -> usize { self.submits.load(Ordering::SeqCst) }

    /// Owner-thread step: pops the oldest held request, rechecks cancellation,
    /// performs it and acknowledges. Returns whether a request was processed.
    pub fn process_next(&self, response: &FakeResponse) -> bool {
        let Some(request) = self.queue.try_pop() else {
            return false;
        };
        self.perform(request, response);
        true
    }

    fn perform(&self, request: ControlRequest, response: &FakeResponse) {
        // A cancelled queued request must never act late.
        if request.is_cancelled() {
            return;
        }
        let kind = request.kind();
        let action = request.action();
        let outcome = if let FakeResponse::Fail(message) = response {
            Err(NativeError(message.clone()))
        } else {
            let mut state = self.state();
            state.performed.push(NativeCall {
                kind,
                action,
                generation: request.generation(),
            });
            if action != ControlAction::Enable {
                let live = action != ControlAction::Remove;
                match kind {
                    TapKind::Suppressing => state.suppressing_live = live,
                    TapKind::ListenOnly => state.listen_only_live = live,
                }
            }
            Ok(())
        };
        request.acknowledge(outcome);
    }

    fn deliver_due(&self) {
        let now = self.clock.now();
        let due: Vec<ControlRequest> = {
            let mut state = self.state();
            let mut due = Vec::new();
            let mut keep = Vec::new();
            for (at, request) in state.delayed.drain(..) {
                if at <= now {
                    due.push(request);
                } else {
                    keep.push((at, request));
                }
            }
            state.delayed = keep;
            due
        };
        for request in due {
            self.perform(request, &FakeResponse::Ack);
        }
    }
}

impl InputBackend for FakeBackend {
    fn submit(&self, request: ControlRequest) -> Result<(), ControlRejected> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        let response = {
            let mut state = self.state();
            state.submitted.push((request.kind(), request.action()));
            let key = (request.kind(), request.action());
            let index = state.script.iter().position(|(k, _)| k.is_none_or(|k| k == key));
            index.and_then(|i| state.script.remove(i)).map(|(_, r)| r)
        }
        .unwrap_or_else(|| self.default.lock().unwrap_or_else(PoisonError::into_inner).clone());
        match response {
            FakeResponse::Reject(rejected) => Err(rejected),
            FakeResponse::Hold => self.queue.try_push(request),
            FakeResponse::Drop => Ok(()),
            FakeResponse::AckAfter(delay) => {
                let at = self.clock.now() + delay;
                self.state().delayed.push((at, request));
                Ok(())
            }
            other => {
                // Drain older held requests first so owner order stays FIFO.
                while let Some(older) = self.queue.try_pop() {
                    self.perform(older, &FakeResponse::Ack);
                }
                self.perform(request, &other);
                Ok(())
            }
        }
    }
}

/// A fixed-capacity [`WorkReservations`] fake. It never blocks.
pub struct CapacityWork {
    capacity: usize,
    used: AtomicUsize,
    rejected: AtomicUsize,
}

impl CapacityWork {
    /// A fake with `capacity` slots.
    #[must_use]
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            capacity,
            used: AtomicUsize::new(0),
            rejected: AtomicUsize::new(0),
        })
    }

    /// Slots in use.
    #[must_use]
    pub fn used(&self) -> usize { self.used.load(Ordering::SeqCst) }

    /// Rejected reservations.
    #[must_use]
    pub fn rejected(&self) -> usize { self.rejected.load(Ordering::SeqCst) }
}

impl WorkReservations for CapacityWork {
    fn try_reserve(&self) -> bool {
        let taken = self
            .used
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                (used < self.capacity).then_some(used + 1)
            })
            .is_ok();
        if !taken {
            self.rejected.fetch_add(1, Ordering::SeqCst);
        }
        taken
    }

    fn release(&self) {
        let _ = self
            .used
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |used| used.checked_sub(1));
    }
}
