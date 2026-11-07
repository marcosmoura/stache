//! The single owner thread: bounded queue, cancellation-aware execution,
//! acknowledgements and shutdown.
//!
//! Everything native lives behind [`NativeInput`], which is created *inside* the
//! owner thread and is deliberately not `Send`: only the [`ControlQueue`], the
//! [`WakeHandle`] and the result callbacks cross threads. The owner never
//! dispatches to the main thread, so no main -> worker -> main wait can exist.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use stache_sdk::input::{
    ControlAction, ControlQueue, ControlRejected, ControlRequest, NativeError, TapKind,
};

use crate::applications::ACKNOWLEDGEMENT_BUDGET;

/// The stable owner thread name.
pub const OWNER_THREAD_NAME: &str = "stache-input-owner";

/// Longest the owner parks without a wake. A wake is level-triggered, so this is
/// only a defensive bound, not a polling interval.
const PARK_LIMIT: Duration = Duration::from_secs(1);

/// Wakes the owner's run loop from any thread without blocking.
pub trait WakeHandle: Send + Sync {
    /// Makes the owner's next (or current) park return.
    fn wake(&self);
}

/// The native layer. It lives and dies on the owner thread.
pub trait NativeInput {
    /// The handle other threads use to wake [`Self::park`].
    fn wake_handle(&self) -> Arc<dyn WakeHandle>;

    /// Creates the tap in a disabled state. Idempotent when already installed.
    /// A failure may leave partial resources that [`Self::remove`] must clean.
    fn install(&mut self, kind: TapKind) -> Result<(), String>;

    /// Enables (or re-enables) an installed tap.
    fn enable(&mut self, kind: TapKind) -> Result<(), String>;

    /// Removes the tap and whatever partial resources exist. Idempotent.
    fn remove(&mut self, kind: TapKind) -> Result<(), String>;

    /// Runs the native run loop until a wake or `limit`.
    fn park(&mut self, limit: Duration);
}

/// Creates the native layer on the owner thread.
pub type Factory = Box<dyn FnOnce() -> Result<Box<dyn NativeInput>, String> + Send>;

type Completion = Box<dyn FnOnce(Result<(), String>) + Send>;

#[derive(Default)]
struct SharedState {
    closing: bool,
    finished: bool,
    result: Option<Result<(), String>>,
    completions: Vec<Completion>,
}

struct Shared {
    queue: ControlQueue,
    wake: Mutex<Option<Arc<dyn WakeHandle>>>,
    state: Mutex<SharedState>,
    finished: Condvar,
    submitted: AtomicU64,
    owner_thread: Mutex<Option<ThreadId>>,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, SharedState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wake(&self) {
        let handle = self.wake.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(handle) = handle {
            handle.wake();
        }
    }

    fn is_closing(&self) -> bool { self.state().closing }

    /// Acknowledges every queued request with a failure.
    fn fail_queued(&self, message: &str) {
        while let Some(request) = self.queue.try_pop() {
            request.acknowledge(Err(NativeError(message.to_owned())));
        }
    }

    fn finish(&self, result: &Result<(), String>) {
        let completions = {
            let mut state = self.state();
            state.closing = true;
            state.finished = true;
            state.result = Some(result.clone());
            std::mem::take(&mut state.completions)
        };
        self.fail_queued("the input owner has stopped");
        self.finished.notify_all();
        for completion in completions {
            completion(result.clone());
        }
    }
}

enum Life {
    Idle(Factory),
    Running(JoinHandle<()>),
    Gone,
}

/// The lazily started owner thread and its queue.
pub struct InputOwner {
    shared: Arc<Shared>,
    life: Mutex<Life>,
}

impl InputOwner {
    /// An owner that starts its thread on the first request.
    pub fn new(factory: Factory) -> Self {
        Self {
            shared: Arc::new(Shared {
                queue: ControlQueue::new(),
                wake: Mutex::new(None),
                state: Mutex::new(SharedState::default()),
                finished: Condvar::new(),
                submitted: AtomicU64::new(0),
                owner_thread: Mutex::new(None),
            }),
            life: Mutex::new(Life::Idle(factory)),
        }
    }

    fn life(&self) -> MutexGuard<'_, Life> {
        self.life.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the owner thread was ever started.
    #[cfg(test)]
    pub fn started(&self) -> bool { !matches!(*self.life(), Life::Idle(_)) }

    /// Requests accepted into the queue so far.
    #[cfg(test)]
    pub fn submitted(&self) -> u64 { self.shared.submitted.load(Ordering::SeqCst) }

    /// Queues one request and wakes the owner. Never blocks on native work.
    pub fn submit(&self, request: ControlRequest) -> Result<(), ControlRejected> {
        if self.shared.is_closing() {
            return Err(ControlRejected::Closed);
        }
        self.ensure_started()?;
        self.shared.queue.try_push(request)?;
        self.shared.submitted.fetch_add(1, Ordering::SeqCst);
        self.shared.wake();
        if self.shared.state().finished {
            self.shared.fail_queued("the input owner has stopped");
        }
        Ok(())
    }

    fn ensure_started(&self) -> Result<(), ControlRejected> {
        let mut life = self.life();
        match std::mem::replace(&mut *life, Life::Gone) {
            Life::Running(handle) => {
                *life = Life::Running(handle);
                Ok(())
            }
            Life::Gone => Err(ControlRejected::Closed),
            Life::Idle(factory) => {
                let shared = Arc::clone(&self.shared);
                let spawned = thread::Builder::new()
                    .name(OWNER_THREAD_NAME.to_owned())
                    .spawn(move || owner_main(factory, &shared));
                if let Ok(handle) = spawned {
                    *life = Life::Running(handle);
                    Ok(())
                } else {
                    drop(life);
                    self.shared.finish(&Err("cannot start the input owner thread".to_owned()));
                    Err(ControlRejected::Closed)
                }
            }
        }
    }

    /// Stops the owner: every queued removal runs, both taps are removed, the
    /// native layer is dropped on the owner thread and then `done` runs with the
    /// cleanup result. Never blocks the caller. Safe to call repeatedly.
    pub fn shutdown(&self, done: impl FnOnce(Result<(), String>) + Send + 'static) {
        {
            let mut state = self.shared.state();
            if state.finished {
                let result = state.result.clone().unwrap_or(Ok(()));
                drop(state);
                done(result);
                return;
            }
            state.closing = true;
            state.completions.push(Box::new(done));
        }
        let never_started = {
            let mut life = self.life();
            if matches!(*life, Life::Idle(_)) {
                *life = Life::Gone;
                true
            } else {
                false
            }
        };
        if never_started {
            self.shared.finish(&Ok(()));
        } else {
            self.shared.wake();
        }
    }

    /// Waits at most `timeout` for the owner thread to finish, then joins it.
    pub fn join(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.state();
        while !state.finished {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            state = self
                .shared
                .finished
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        drop(state);
        let previous = std::mem::replace(&mut *self.life(), Life::Gone);
        if let Life::Running(handle) = previous {
            // `finish` is the thread's last statement, so this returns at once.
            let _ = handle.join();
        }
        true
    }
}

impl Drop for InputOwner {
    fn drop(&mut self) {
        self.shutdown(|_| {});
        let on_owner = *self.shared.owner_thread.lock().unwrap_or_else(PoisonError::into_inner)
            == Some(thread::current().id());
        if on_owner {
            // The last reference dropped inside a tap callback: the thread
            // finishes by itself and cannot join itself.
            return;
        }
        // Bounded: a native call that never returns is abandoned, not awaited.
        let _ = self.join(ACKNOWLEDGEMENT_BUDGET);
    }
}

fn owner_main(factory: Factory, shared: &Arc<Shared>) {
    *shared.owner_thread.lock().unwrap_or_else(PoisonError::into_inner) =
        Some(thread::current().id());
    let mut native = match factory() {
        Ok(native) => native,
        Err(message) => {
            shared.finish(&Err(message));
            return;
        }
    };
    *shared.wake.lock().unwrap_or_else(PoisonError::into_inner) = Some(native.wake_handle());
    loop {
        drain(&mut *native, shared, false);
        if shared.is_closing() {
            break;
        }
        native.park(PARK_LIMIT);
    }
    // Requests that raced the close: removals still run, the rest fail.
    drain(&mut *native, shared, true);
    let result = remove_everything(&mut *native);
    // Native objects are destroyed here, on their own thread, before the answer.
    drop(native);
    shared.finish(&result);
}

fn drain(native: &mut dyn NativeInput, shared: &Shared, shutting_down: bool) {
    while let Some(request) = shared.queue.try_pop() {
        handle(native, request, shutting_down);
    }
}

fn handle(native: &mut dyn NativeInput, request: ControlRequest, shutting_down: bool) {
    let kind = request.kind();
    let action = request.action();
    // A removal is never cancelled: eventual owner cleanup must still run.
    if action != ControlAction::Remove {
        if shutting_down {
            request.acknowledge(Err(NativeError("the input owner is shutting down".to_owned())));
            return;
        }
        // Checked immediately before the native mutation.
        if request.is_cancelled() {
            request.acknowledge(Err(NativeError("the request was cancelled".to_owned())));
            return;
        }
    }
    let outcome = catch_unwind(AssertUnwindSafe(|| match action {
        ControlAction::Install => native.install(kind),
        ControlAction::Enable => native.enable(kind),
        ControlAction::Remove => native.remove(kind),
    }))
    .unwrap_or_else(|_| Err("the native input operation panicked".to_owned()));
    request.acknowledge(outcome.map_err(NativeError));
}

fn remove_everything(native: &mut dyn NativeInput) -> Result<(), String> {
    let mut failures = Vec::new();
    for kind in [TapKind::Suppressing, TapKind::ListenOnly] {
        let outcome = catch_unwind(AssertUnwindSafe(|| native.remove(kind)))
            .unwrap_or_else(|_| Err("the native input operation panicked".to_owned()));
        if let Err(message) = outcome {
            failures.push(format!("{kind:?}: {message}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}
