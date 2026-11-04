//! Bounded waits for coordinated teardown.
//!
//! Every teardown owner wait is bounded by `min(OWNER_WAIT_BUDGET, remaining)`
//! and wakes within one [`CANCELLATION_QUANTUM`] when a later request shortens
//! the live deadline. Time is injected through [`ShutdownWait`], so the host
//! uses its coordinator clock and tests use a controllable fake.

use std::time::{Duration, Instant};

/// Approved bound for one owner acknowledgement.
pub const OWNER_WAIT_BUDGET: Duration = Duration::from_secs(2);

/// Waking granularity for every bounded wait.
pub const CANCELLATION_QUANTUM: Duration = Duration::from_millis(10);

/// Time source and wake source for a bounded shutdown wait.
pub trait ShutdownWait: Send + Sync {
    /// Current instant on the coordinator's clock.
    fn now(&self) -> Instant;

    /// The live teardown deadline. A later request may shorten it.
    fn deadline(&self) -> Instant;

    /// Sleeps at most `timeout`, and returns early when the deadline shortens.
    fn wait(&self, timeout: Duration);
}

/// Waits until `target`, but never past `min(target, deadline)`.
///
/// Returns `true` when `target` was reached, `false` when the live deadline cut
/// it short. The wait proceeds in [`CANCELLATION_QUANTUM`] slices and re-reads
/// the deadline and clock before each slice, so a shortened deadline is
/// observed within one quantum, a clock that jumps cannot extend the wait past
/// the deadline, and no slice outlives the live deadline.
#[must_use]
pub fn wait_until(wait: &dyn ShutdownWait, target: Instant) -> bool {
    loop {
        let now = wait.now();
        if now >= target {
            return true;
        }
        let deadline = wait.deadline();
        if now >= deadline {
            return false;
        }
        wait.wait(next_slice(now, target, deadline));
    }
}

/// Waits for `finished()` until `min(now + OWNER_WAIT_BUDGET, deadline)`.
///
/// `finished` is polled once per [`CANCELLATION_QUANTUM`] slice. Returns `true`
/// when it finished inside the bound, `false` when the caller must abandon it.
#[must_use]
pub fn wait_for_finish(wait: &dyn ShutdownWait, finished: &dyn Fn() -> bool) -> bool {
    let start = wait.now();
    let target = start.checked_add(OWNER_WAIT_BUDGET).unwrap_or(start);
    loop {
        if finished() {
            return true;
        }
        let now = wait.now();
        if now >= target {
            return false;
        }
        let deadline = wait.deadline();
        if now >= deadline {
            return false;
        }
        wait.wait(next_slice(now, target, deadline));
    }
}

/// One wakeable slice, never past the target or the live deadline.
fn next_slice(now: Instant, target: Instant, deadline: Instant) -> Duration {
    CANCELLATION_QUANTUM
        .min(target.saturating_duration_since(now))
        .min(deadline.saturating_duration_since(now))
}

#[cfg(test)]
pub(crate) mod testing {
    use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
    use std::time::{Duration, Instant};

    use super::ShutdownWait;

    /// A change the fake applies inside its next `wait`, before sleeping.
    enum NextWait {
        None,
        Shorten(Duration),
        Jump(Duration),
    }

    struct FakeState {
        now: Instant,
        deadline: Instant,
        slices: Vec<Duration>,
        overslept: bool,
        next: NextWait,
    }

    /// A [`ShutdownWait`] with an injectable clock and deadline.
    ///
    /// The clock advances by the real time spent inside each `wait`, so the
    /// simulated timeline matches wall time while a test can jump it or shorten
    /// the deadline at any point.
    pub struct FakeShutdownWait {
        state: Mutex<FakeState>,
        wake: Condvar,
    }

    impl FakeShutdownWait {
        /// A wait source with `budget` left on its deadline.
        pub fn with_budget(budget: Duration) -> Self {
            let now = Instant::now();
            Self {
                state: Mutex::new(FakeState {
                    now,
                    deadline: now + budget,
                    slices: Vec::new(),
                    overslept: false,
                    next: NextWait::None,
                }),
                wake: Condvar::new(),
            }
        }

        /// Shortens the deadline as a later teardown request would.
        pub fn shorten_to(&self, budget: Duration) {
            let mut state = self.lock();
            let deadline = state.now + budget;
            if deadline < state.deadline {
                state.deadline = deadline;
                self.wake.notify_all();
            }
        }

        /// Expires the deadline immediately.
        pub fn expire_now(&self) { self.shorten_to(Duration::ZERO); }

        /// Shortens the deadline inside the next `wait`, without a test thread.
        pub fn shorten_during_next_wait(&self, budget: Duration) {
            self.lock().next = NextWait::Shorten(budget);
        }

        /// Jumps the clock forward inside the next `wait`.
        pub fn jump_during_next_wait(&self, delta: Duration) {
            self.lock().next = NextWait::Jump(delta);
        }

        /// Every timeout requested so far, in order.
        pub fn slices(&self) -> Vec<Duration> { self.lock().slices.clone() }

        /// Whether any requested slice reached past the live deadline.
        pub fn overslept(&self) -> bool { self.lock().overslept }

        fn lock(&self) -> MutexGuard<'_, FakeState> {
            self.state.lock().unwrap_or_else(PoisonError::into_inner)
        }
    }

    impl ShutdownWait for FakeShutdownWait {
        fn now(&self) -> Instant { self.lock().now }

        fn deadline(&self) -> Instant { self.lock().deadline }

        fn wait(&self, timeout: Duration) {
            let mut state = self.lock();
            state.slices.push(timeout);
            if state.now.checked_add(timeout).is_some_and(|end| end > state.deadline) {
                state.overslept = true;
            }
            let started = Instant::now();
            let (next, _) =
                self.wake.wait_timeout(state, timeout).unwrap_or_else(PoisonError::into_inner);
            let mut state = next;
            state.now += started.elapsed().min(timeout);
            match std::mem::replace(&mut state.next, NextWait::None) {
                NextWait::None => {}
                NextWait::Shorten(budget) => {
                    let deadline = state.now + budget;
                    state.deadline = state.deadline.min(deadline);
                }
                NextWait::Jump(delta) => state.now += delta,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::testing::FakeShutdownWait;
    use super::*;

    #[test]
    fn a_reached_target_returns_true() {
        let wait = FakeShutdownWait::with_budget(Duration::from_secs(2));
        let target = wait.now() + Duration::from_millis(30);
        assert!(wait_until(&wait, target));
        assert!(wait.now() >= target);
        assert!(!wait.overslept());
        assert!(wait.slices().iter().all(|slice| *slice <= CANCELLATION_QUANTUM));
    }

    #[test]
    fn a_shortened_deadline_cuts_the_wait_short() {
        let wait = FakeShutdownWait::with_budget(Duration::from_secs(2));
        let target = wait.now() + Duration::from_secs(2);
        wait.shorten_during_next_wait(Duration::from_millis(10));
        let started = Instant::now();
        assert!(!wait_until(&wait, target));
        assert!(wait.now() < target);
        assert!(
            wait.slices().len() <= 3,
            "a shortened deadline must end the wait within a quantum: {:?}",
            wait.slices()
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_clock_jump_does_not_extend_the_wait() {
        let wait = FakeShutdownWait::with_budget(Duration::from_secs(2));
        let target = wait.now() + Duration::from_secs(1);
        wait.jump_during_next_wait(Duration::from_secs(5));
        assert!(wait_until(&wait, target));
        assert!(wait.slices().len() <= 2, "{:?}", wait.slices());
    }

    #[test]
    fn no_slice_sleeps_past_the_live_deadline() {
        let wait = FakeShutdownWait::with_budget(Duration::from_millis(35));
        let target = wait.now() + Duration::from_secs(1);
        assert!(!wait_until(&wait, target));
        assert!(!wait.overslept());
        let slices = wait.slices();
        assert!(!slices.is_empty());
        assert!(
            slices.iter().all(|slice| *slice <= CANCELLATION_QUANTUM),
            "{slices:?}"
        );
    }

    #[test]
    fn wait_for_finish_polls_until_finished() {
        let wait = FakeShutdownWait::with_budget(Duration::from_secs(2));
        let polls = AtomicUsize::new(0);
        assert!(wait_for_finish(&wait, &|| polls.fetch_add(1, Ordering::SeqCst) >= 2));
        assert_eq!(polls.load(Ordering::SeqCst), 3);
        assert!(!wait.overslept());
    }

    #[test]
    fn wait_for_finish_abandons_at_the_live_deadline() {
        let wait = Arc::new(FakeShutdownWait::with_budget(Duration::from_secs(2)));
        wait.expire_now();
        assert!(!wait_for_finish(wait.as_ref(), &|| false));
        assert!(wait.slices().is_empty(), "an expired deadline must not sleep");
    }
}
