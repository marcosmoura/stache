//! Menu-bar visibility observation for one Bar runtime with a panel.
//!
//! The adapter and the legacy semantics live in `stache_macos::menu_bar`
//! (read its module documentation first). This file is the policy around it:
//! when to sample, when to poll and how to publish.
//!
//! # Event versus poll
//!
//! A [`MenuMonitor`] owns one worker thread and one native observer, and exists
//! only while Bar runs **with a panel**: zero size, no primary display, a failed
//! panel, disabled and paused all own no observer and no thread. The worker
//!
//! 1. subscribes to the native notifications **first**, so a change between the
//!    subscription and the seed is never lost;
//! 2. seeds the state with one sample;
//! 3. re-samples on every coalesced hint (the hint is one pending slot plus an
//!    increasing sequence, so a burst is one refresh and a hint that arrives
//!    during a refresh causes one more);
//! 4. polls every [`BarTiming::menu_poll`](crate::BarTiming) **only while the
//!    last sample says auto-hide is on or unknown** (the pointer entering the
//!    menu-bar row posts no notification; [`MenuBarSample::needs_poll`]), and
//!    not at all once auto-hide is known to be off;
//! 5. retries a failed sample (and a failed observer registration, polling in
//!    the meantime) every [`BarTiming::menu_retry`](crate::BarTiming).
//!
//! The decision is [`next_wake`], a pure function over an injected `now`, so
//! the schedule is unit-tested without a clock.
//!
//! A notification macOS never delivers cannot be detected without polling; the
//! auto-hide poll covers the case that matters (the reveal), and the
//! auto-hide preference is re-read by every sample (it has no usable
//! notification, see the adapter).
//!
//! # Publication
//!
//! Only a change of `(state, reason)` is published, so a steady poll publishes
//! nothing and a persistent failure reports one diagnostic, not one per tick.
//! A publish after [`MenuMonitor::request_stop`] is dropped.
//!
//! # Ending
//!
//! [`MenuMonitor::end`] sets the stop flag, wakes the worker, joins it within a
//! bound and retries a failed native removal; `false` keeps the monitor owned
//! for another `end`. The worker itself removes its observer on exit, so a
//! monitor that was only asked to stop usually settles by itself.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use stache_macos::{
    MenuBarBasis, MenuBarControl, MenuBarError, MenuBarHint, MenuBarObserver, MenuBarSample,
    MenuBarVisibility,
};

use crate::status::{MenuBarObservation, MenuBarState};

/// The delay between polls while auto-hide is on or unknown: legacy's cadence
/// (`menubar.rs:33`), now running only while it is needed.
pub const MENU_POLL: Duration = Duration::from_millis(100);

/// The delay before a failed sample or observer registration is retried.
pub const MENU_RETRY: Duration = Duration::from_secs(2);

/// The longest [`MenuMonitor::end`] waits for the worker to finish.
pub const MENU_JOIN: Duration = Duration::from_secs(2);

/// Receives each changed observation. Must not block.
pub type Publish = Arc<dyn Fn(MenuBarObservation) + Send + Sync>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the last refresh left for the schedule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Schedule {
    /// The last sample failed or could not be read.
    pub failed: bool,
    /// The native observer is not registered.
    pub observer_missing: bool,
    /// The last sample said a poll is needed.
    pub poll_needed: bool,
}

/// When the worker next wakes by itself, if at all.
///
/// A failed sample or a missing observer retries after `retry`; otherwise the
/// poll runs after `poll` only while it is needed. With the observer healthy,
/// auto-hide known off and the last sample good, there is no wake at all: the
/// worker sleeps until a hint or a stop.
#[must_use]
pub fn next_wake(
    now: Instant,
    schedule: Schedule,
    poll: Duration,
    retry: Duration,
) -> Option<Instant> {
    let polling = schedule.poll_needed || schedule.observer_missing;
    match (schedule.failed || schedule.observer_missing, polling) {
        (true, true) => Some(now + poll.min(retry)),
        (true, false) => Some(now + retry),
        (false, true) => Some(now + poll),
        (false, false) => None,
    }
}

/// Maps one refresh to the published observation.
fn observe(
    result: &Result<MenuBarSample, MenuBarError>,
    prior: Option<MenuBarVisibility>,
    observer_error: Option<&str>,
) -> (MenuBarObservation, Option<MenuBarVisibility>) {
    let mut reasons: Vec<String> = Vec::new();
    let (sample, failure) = match result {
        Ok(sample) => (sample.clone(), None),
        Err(error) => (
            MenuBarSample {
                menu_bar_visible: None,
                pointer_in_row: None,
                window_list: None,
                auto_hide: None,
            },
            Some(error.to_string()),
        ),
    };
    let resolution = sample.resolve(prior);
    let state = match resolution.visibility {
        MenuBarVisibility::Visible => MenuBarState::Visible,
        MenuBarVisibility::Hidden => MenuBarState::Hidden,
        MenuBarVisibility::Unknown => MenuBarState::Unknown,
    };
    match resolution.basis {
        MenuBarBasis::Signals => {}
        MenuBarBasis::WindowList => {
            reasons.push("estimated from the window list".to_owned());
        }
        MenuBarBasis::Held => reasons.push(format!(
            "held from the previous reading: {}",
            failure.as_deref().unwrap_or("the menu-bar signals were unavailable")
        )),
        MenuBarBasis::Unresolved => reasons.push(format!(
            "the menu-bar state could not be read: {}",
            failure.as_deref().unwrap_or("the menu-bar signals were unavailable")
        )),
    }
    if let Some(error) = observer_error {
        reasons.push(format!("event observation unavailable, polling: {error}"));
    }
    let reason = (!reasons.is_empty()).then(|| reasons.join("; "));
    let prior = match resolution.visibility {
        MenuBarVisibility::Unknown => prior,
        decided => Some(decided),
    };
    (MenuBarObservation { state, reason }, prior)
}

#[derive(Default)]
struct State {
    hint: Option<MenuBarHint>,
    stop: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    /// The native observer; retained after a failed removal for a retry.
    observer: Mutex<Option<MenuBarObserver>>,
    /// Removal of the native observer failed and is still owed.
    owed: AtomicBool,
}

/// One menu-bar observation: a worker and a native observer.
pub struct MenuMonitor {
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl MenuMonitor {
    /// Starts the monitor. Returns at once: the worker registers the observer,
    /// seeds the state and publishes, all off the caller's thread.
    ///
    /// # Errors
    /// The worker could not be spawned; nothing is left running.
    pub fn start(
        control: MenuBarControl,
        publish: Publish,
        poll: Duration,
        retry: Duration,
    ) -> Result<Self, String> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            observer: Mutex::new(None),
            owed: AtomicBool::new(false),
        });
        let worker = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("stache-bar-menu".to_owned())
            .spawn(move || run(&worker, &control, &publish, poll, retry))
            .map_err(|error| format!("the menu-bar worker could not start: {error}"))?;
        Ok(Self {
            shared,
            thread: Mutex::new(Some(handle)),
        })
    }

    /// Asks the worker to stop. Never blocks; later publishes are dropped.
    pub fn request_stop(&self) {
        lock(&self.shared.state).stop = true;
        self.shared.wake.notify_all();
    }

    /// Whether the worker finished and nothing is owed.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        !self.shared.owed.load(Ordering::SeqCst)
            && lock(&self.thread).as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Joins a settled worker (instant). `true` once nothing remains.
    #[must_use]
    pub fn reap(&self) -> bool {
        if !self.is_settled() {
            return false;
        }
        let handle = lock(&self.thread).take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
        true
    }

    /// Stops the worker, joins it within `join` and retries a failed native
    /// removal. Idempotent. `false` keeps everything owned for another `end`.
    #[must_use]
    pub fn end(&self, join: Duration) -> bool {
        self.request_stop();
        let mut done = true;
        let mut thread = lock(&self.thread);
        if let Some(handle) = thread.as_ref() {
            let deadline = Instant::now() + join;
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            if handle.is_finished() {
                if let Some(handle) = thread.take() {
                    let _ = handle.join();
                }
            } else {
                done = false;
            }
        }
        drop(thread);
        if done {
            done = remove_observer(&self.shared);
        }
        done
    }
}

/// Removes the native observer; it stays owned after a failed removal.
fn remove_observer(shared: &Shared) -> bool {
    let mut observer = lock(&shared.observer);
    let removed = match observer.as_ref() {
        Some(held) if held.stop().is_err() => false,
        Some(_) => {
            *observer = None;
            true
        }
        None => true,
    };
    drop(observer);
    shared.owed.store(!removed, Ordering::SeqCst);
    removed
}

impl Drop for MenuMonitor {
    fn drop(&mut self) {
        // A dropped monitor never leaves a worker behind: ask, do not wait
        // (the worker removes its observer on exit).
        self.request_stop();
    }
}

fn register(shared: &Arc<Shared>, control: &MenuBarControl) -> Result<(), MenuBarError> {
    let sink = Arc::clone(shared);
    let observer = control.observe(move |hint| {
        let mut state = lock(&sink.state);
        if state.stop {
            return;
        }
        if state.hint.is_none_or(|held| held.sequence < hint.sequence) {
            state.hint = Some(hint);
        }
        drop(state);
        // `notify_all`: the condvar is shared with nothing else today, but a
        // single wake must never be consumed by the wrong waiter.
        sink.wake.notify_all();
    })?;
    *lock(&shared.observer) = Some(observer);
    Ok(())
}

fn run(
    shared: &Arc<Shared>,
    control: &MenuBarControl,
    publish: &Publish,
    poll: Duration,
    retry: Duration,
) {
    // 1. Subscribe before the first sample.
    let mut observer_error = register(shared, control).err().map(|error| error.to_string());
    let mut prior: Option<MenuBarVisibility> = None;
    let mut published: Option<MenuBarObservation> = None;
    let mut retry_observer_at: Option<Instant> =
        observer_error.as_ref().map(|_| Instant::now() + retry);
    loop {
        if lock(&shared.state).stop {
            break;
        }
        // The pending hint is consumed before the sample, so a hint that
        // arrives during it causes one more refresh and is never lost.
        lock(&shared.state).hint = None;
        if observer_error.is_some()
            && retry_observer_at.is_none_or(|at| Instant::now() >= at)
            && lock(&shared.observer).is_none()
        {
            observer_error = register(shared, control).err().map(|error| error.to_string());
            retry_observer_at = Some(Instant::now() + retry);
        }
        // 2. Seed / refresh. The query is a bounded main-thread job.
        let result = control.sample();
        let (observation, next_prior) = observe(&result, prior, observer_error.as_deref());
        prior = next_prior;
        if published.as_ref() != Some(&observation) {
            // A request_stop that raced the sample drops the publication.
            if !lock(&shared.state).stop {
                publish(observation.clone());
                published = Some(observation);
            }
        }
        let schedule = Schedule {
            failed: result.is_err(),
            observer_missing: observer_error.is_some(),
            poll_needed: result.as_ref().map_or(true, MenuBarSample::needs_poll),
        };
        let wake_at = next_wake(Instant::now(), schedule, poll, retry);
        // 3. Sleep until a hint, the schedule or a stop.
        let mut state = lock(&shared.state);
        loop {
            if state.stop || state.hint.is_some() {
                break;
            }
            match wake_at {
                Some(at) => {
                    let now = Instant::now();
                    if now >= at {
                        break;
                    }
                    state = shared
                        .wake
                        .wait_timeout(state, at - now)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
                None => {
                    state = shared.wake.wait(state).unwrap_or_else(PoisonError::into_inner);
                }
            }
        }
    }
    // 4. Exit: remove the observer here, off the main thread, so a monitor
    // that was only asked to stop settles on its own.
    let _ = remove_observer(shared);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(offset_ms: u64, base: Instant) -> Instant { base + Duration::from_millis(offset_ms) }

    #[test]
    fn the_schedule_polls_only_while_needed_and_sleeps_otherwise() {
        let now = Instant::now();
        let poll = Duration::from_millis(100);
        let retry = Duration::from_secs(2);
        let good = |poll_needed| Schedule {
            failed: false,
            observer_missing: false,
            poll_needed,
        };
        assert_eq!(next_wake(now, good(true), poll, retry), Some(at(100, now)));
        assert_eq!(
            next_wake(now, good(false), poll, retry),
            None,
            "auto-hide off: no wake"
        );
        let failed = Schedule {
            failed: true,
            observer_missing: false,
            poll_needed: false,
        };
        assert_eq!(next_wake(now, failed, poll, retry), Some(now + retry));
        let failed_polling = Schedule { poll_needed: true, ..failed };
        assert_eq!(next_wake(now, failed_polling, poll, retry), Some(at(100, now)));
        let blind = Schedule {
            failed: false,
            observer_missing: true,
            poll_needed: false,
        };
        assert_eq!(
            next_wake(now, blind, poll, retry),
            Some(at(100, now)),
            "without events the worker polls"
        );
    }

    #[test]
    fn observations_name_their_basis_and_dedupe_by_value() {
        let (visible, prior) = observe(
            &Ok(stache_macos::menu_bar::testing::visible_sample()),
            None,
            None,
        );
        assert_eq!(visible, MenuBarObservation {
            state: MenuBarState::Visible,
            reason: None
        });
        assert_eq!(prior, Some(MenuBarVisibility::Visible));
        let failure = Err(MenuBarError::TimedOut);
        let (held, _) = observe(&failure, prior, None);
        assert_eq!(held.state, MenuBarState::Visible);
        assert!(held.reason.as_deref().is_some_and(|reason| reason.starts_with("held")));
        let (unknown, none) = observe(&failure, None, None);
        assert_eq!(unknown.state, MenuBarState::Unknown);
        assert!(none.is_none());
        let (twice, _) = observe(&failure, None, None);
        assert_eq!(unknown, twice, "an identical failure is one diagnostic");
        let (blind, _) = observe(
            &Ok(stache_macos::menu_bar::testing::hidden_sample()),
            None,
            Some("denied"),
        );
        assert_eq!(blind.state, MenuBarState::Hidden);
        assert!(blind.reason.is_some_and(|reason| reason.contains("denied")));
    }
}
