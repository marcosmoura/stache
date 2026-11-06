//! Injected monotonic time, wake signal and random draw.
//!
//! Production uses [`SystemRuntimeClock`] and [`SystemRandom`]. Tests inject a
//! manual clock so a deadline or interval can expire without a real sleep, and
//! a scripted random source so random-all selection is deterministic.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

static COUNTER: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

/// A monotonic clock used by request deadlines and rotation timers.
pub trait RuntimeClock: Send + Sync {
    /// The current monotonic instant.
    fn now(&self) -> Instant;
}

/// The production clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRuntimeClock;

impl RuntimeClock for SystemRuntimeClock {
    fn now(&self) -> Instant { Instant::now() }
}

/// Why one runtime signal wait returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    /// A notification was observed after the caller's snapshot.
    Wake,
    /// The supplied deadline was reached first.
    Deadline,
    /// The signal was stopped by teardown.
    Stopped,
}

struct SignalState {
    generation: u64,
    stopped: bool,
}

/// A lossless wake source: a caller snapshots [`RuntimeSignal::generation`]
/// while it holds the state it is waiting on, then waits for a newer
/// generation. A notification between the check and the wait is therefore
/// always observed.
pub struct RuntimeSignal {
    state: Mutex<SignalState>,
    changed: Condvar,
}

impl RuntimeSignal {
    /// Creates a live signal.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(SignalState { generation: 0, stopped: false }),
            changed: Condvar::new(),
        }
    }

    /// The current generation. Read it under the same lock as the condition it
    /// guards, then pass it to [`RuntimeSignal::wait_after`].
    #[must_use]
    pub fn generation(&self) -> u64 { lock(&self.state).generation }

    /// Wakes every waiter. Spurious wakes are harmless; waiters re-check their
    /// own condition and generation.
    pub fn notify(&self) {
        {
            let mut state = lock(&self.state);
            state.generation = state.generation.wrapping_add(1);
        }
        self.changed.notify_all();
    }

    /// Wakes and permanently refuses later waits.
    pub fn stop(&self) {
        {
            let mut state = lock(&self.state);
            state.stopped = true;
            state.generation = state.generation.wrapping_add(1);
        }
        self.changed.notify_all();
    }

    /// Waits until the generation differs from `seen`, the deadline passes on
    /// `clock`, or the signal stops.
    ///
    /// Precedence is stopped, changed, deadline. No slice outlives the supplied
    /// deadline on the injected clock, and a wake never requires polling the
    /// accepted configuration.
    #[must_use]
    pub fn wait_after(
        &self,
        seen: u64,
        deadline: Option<Instant>,
        clock: &dyn RuntimeClock,
    ) -> WaitOutcome {
        let mut state = lock(&self.state);
        loop {
            if state.stopped {
                return WaitOutcome::Stopped;
            }
            if state.generation != seen {
                return WaitOutcome::Wake;
            }
            if let Some(deadline) = deadline {
                let now = clock.now();
                if now >= deadline {
                    return WaitOutcome::Deadline;
                }
                let remaining = deadline.saturating_duration_since(now);
                let (next, _) = self
                    .changed
                    .wait_timeout(state, remaining)
                    .unwrap_or_else(PoisonError::into_inner);
                state = next;
            } else {
                state = self.changed.wait(state).unwrap_or_else(PoisonError::into_inner);
            }
        }
    }
}

impl Default for RuntimeSignal {
    fn default() -> Self { Self::new() }
}

/// A random draw used by random and random-all selection.
pub trait RandomSource: Send + Sync {
    /// Returns an index in `0..length`. `length` is never zero.
    fn index(&self, length: usize) -> usize;
}

/// The production random source.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRandom;

impl RandomSource for SystemRandom {
    fn index(&self, length: usize) -> usize {
        if length <= 1 {
            return 0;
        }
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut hasher = RandomState::new().build_hasher();
        counter.hash(&mut hasher);
        std::process::id().hash(&mut hasher);
        usize::try_from(hasher.finish() % u64::try_from(length).unwrap_or(u64::MAX)).unwrap_or(0)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A manual clock for tests and scripted time.
#[cfg(test)]
pub(crate) struct ManualClock {
    now: Mutex<Instant>,
}

#[cfg(test)]
impl ManualClock {
    pub(crate) fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            now: Mutex::new(Instant::now()),
        })
    }

    pub(crate) fn advance(&self, duration: Duration) {
        let mut now = lock(&self.now);
        *now = now.checked_add(duration).unwrap_or(*now);
    }
}

#[cfg(test)]
impl RuntimeClock for ManualClock {
    fn now(&self) -> Instant { *lock(&self.now) }
}

#[cfg(test)]
impl stache_macos::DesktopClock for ManualClock {
    fn now(&self) -> Instant { RuntimeClock::now(self) }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, mpsc};
    use std::thread;
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_notification_between_the_snapshot_and_wait_is_observed() {
        let signal = Arc::new(RuntimeSignal::new());
        let clock = ManualClock::new();
        let seen = signal.generation();
        signal.notify();
        assert_eq!(signal.wait_after(seen, None, clock.as_ref()), WaitOutcome::Wake);
    }

    #[test]
    fn a_deadline_on_the_injected_clock_ends_the_wait() {
        let signal = RuntimeSignal::new();
        let clock = ManualClock::new();
        clock.advance(Duration::from_secs(5));
        assert_eq!(
            signal.wait_after(0, Some(clock.now()), clock.as_ref()),
            WaitOutcome::Deadline
        );
    }

    #[test]
    fn stop_ends_even_an_unbounded_wait() {
        let signal = Arc::new(RuntimeSignal::new());
        let clock = ManualClock::new();
        let (entered_tx, entered_rx) = mpsc::channel();
        let waiter = thread::spawn({
            let signal = Arc::clone(&signal);
            let clock: Arc<dyn RuntimeClock> = clock;
            move || {
                let seen = signal.generation();
                entered_tx.send(()).expect("test listens");
                signal.wait_after(seen, None, clock.as_ref())
            }
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).expect("waiter started");
        signal.stop();
        assert_eq!(waiter.join().expect("waiter"), WaitOutcome::Stopped);
    }

    #[test]
    fn the_system_random_index_is_always_in_range() {
        let random = SystemRandom;
        for length in 1..=16 {
            for _ in 0..100 {
                assert!(random.index(length) < length);
            }
        }
        assert_eq!(random.index(1), 0);
    }
}
