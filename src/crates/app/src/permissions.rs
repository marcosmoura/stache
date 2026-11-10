//! Accessibility permission policy for the desktop host.
//!
//! The registry asks for a fresh trust status whenever it starts a module; this
//! manager adds the host policy on top: fast polling while a declared module is
//! waiting, slower backoff otherwise (which still notices revocation), and at
//! most one automatic system prompt per permission per session for requirements
//! that allow it.
//!
//! A permission is needed only when an enabled, non-paused registered module
//! currently requires it for its accepted configuration. Shared-input
//! requirements (Keybindings Caps today, Hold-to-Quit and `MenuAnywhere` later)
//! are user-only: startup, enable, reload, tick and activation only check and
//! poll, and the system prompt appears only for an explicit tray request. Only
//! the Caps bindings of Keybindings need Accessibility, so ordinary shortcuts
//! never cause a prompt.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use stache_sdk::shutdown::{ShutdownWait, wait_for_finish};
use stache_sdk::{Permission, PermissionProvider, PermissionRequirement, PermissionStatus};

/// Why a fresh permission check was requested.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshTrigger {
    /// Initial setup, after modules were registered.
    Startup,
    /// Periodic poll.
    Tick,
    /// The application became active.
    Activation,
    /// The user asked to retry or grant from the tray.
    Explicit,
}

/// The registry facing side of permission policy.
pub trait PermissionHost: Send + Sync {
    /// Permissions required by the accepted configuration of enabled,
    /// non-paused modules, with their prompt policy.
    fn needed(&self) -> Vec<PermissionRequirement>;

    /// Whether any module currently observes `WaitingForPermission`.
    fn waiting(&self) -> bool;

    /// Applies a fresh status through the registry.
    fn apply(&self, permission: Permission, status: PermissionStatus);
}

/// The observable result of one refresh.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefreshReport {
    /// Whether a registry transition was requested.
    pub changed: bool,
    /// Whether a module is still waiting for a permission.
    pub waiting: bool,
    /// The permission prompted or retried by this refresh.
    pub prompted: Option<Permission>,
    /// A prompt that could not be presented.
    pub prompt_error: Option<String>,
    /// The latest status of every needed permission after this refresh: the
    /// fresh status read, replaced by the answer of an explicit request. The
    /// host feeds its Accessibility gate from here, with no second platform
    /// status call.
    pub statuses: Vec<(Permission, PermissionStatus)>,
}

/// Applies prompt policy and forwards fresh statuses to the registry.
pub struct PermissionManager {
    provider: Arc<dyn PermissionProvider>,
    prompted: HashSet<Permission>,
    granted: HashSet<Permission>,
    last: HashMap<Permission, PermissionStatus>,
}

impl PermissionManager {
    /// Creates a manager over a fresh-check provider.
    #[must_use]
    pub fn new(provider: Arc<dyn PermissionProvider>) -> Self {
        Self {
            provider,
            prompted: HashSet::new(),
            granted: HashSet::new(),
            last: HashMap::new(),
        }
    }

    /// Checks every needed permission and applies prompt policy.
    ///
    /// `status` never prompts. `request` is called at most once automatically
    /// for each needed permission whose requirement allows automatic prompting;
    /// a user-only requirement (every shared-input consumer) is requested only
    /// by [`RefreshTrigger::Explicit`]. An explicit user action may prompt again.
    pub fn refresh(&mut self, host: &dyn PermissionHost, trigger: RefreshTrigger) -> RefreshReport {
        let needed = host.needed();
        // A permission nobody needs now is forgotten, so a later requirement
        // re-reads and re-applies the fresh status instead of trusting a stale one.
        self.last
            .retain(|permission, _| needed.iter().any(|r| r.permission() == *permission));
        let mut report = RefreshReport::default();
        for requirement in needed {
            let permission = requirement.permission();
            let fresh = self.provider.status(permission);
            report.statuses.push((permission, fresh));
            if self.last.get(&permission) != Some(&fresh) {
                host.apply(permission, fresh);
                report.changed = true;
                self.last.insert(permission, fresh);
            }
            if fresh == PermissionStatus::Granted {
                self.granted.insert(permission);
                continue;
            }
            let automatic = requirement.may_auto_prompt()
                && !self.prompted.contains(&permission)
                && !self.granted.contains(&permission);
            let explicit = trigger == RefreshTrigger::Explicit;
            if !automatic && !explicit {
                continue;
            }
            self.prompted.insert(permission);
            match self.provider.request(permission) {
                Ok(PermissionStatus::Granted) => {
                    set_status(&mut report.statuses, permission, PermissionStatus::Granted);
                    self.granted.insert(permission);
                    host.apply(permission, PermissionStatus::Granted);
                    self.last.insert(permission, PermissionStatus::Granted);
                    report.changed = true;
                    report.prompted = Some(permission);
                }
                Ok(status) => {
                    set_status(&mut report.statuses, permission, status);
                    host.apply(permission, status);
                    self.last.insert(permission, status);
                    report.changed = true;
                    report.prompted = Some(permission);
                }
                Err(error) => {
                    report.prompt_error = Some(error.to_string());
                    report.prompted = Some(permission);
                }
            }
        }
        report.waiting = host.waiting();
        report
    }
}

fn set_status(
    statuses: &mut [(Permission, PermissionStatus)],
    permission: Permission,
    status: PermissionStatus,
) {
    if let Some(entry) = statuses.iter_mut().find(|(candidate, _)| *candidate == permission) {
        entry.1 = status;
    }
}

/// Poll delays used by the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PermissionPoll {
    /// Delay while a module waits for a grant.
    pub waiting: Duration,
    /// First delay when nothing waits.
    pub backoff_start: Duration,
    /// Largest delay when nothing waits.
    pub backoff_max: Duration,
}

impl Default for PermissionPoll {
    fn default() -> Self {
        Self {
            waiting: Duration::from_secs(1),
            backoff_start: Duration::from_secs(5),
            backoff_max: Duration::from_secs(30),
        }
    }
}

/// Grows the poll delay while nothing waits and resets it while something does.
#[derive(Clone, Copy, Debug)]
pub struct PollSchedule {
    poll: PermissionPoll,
    delay: Duration,
}

impl PollSchedule {
    /// Creates a schedule at the waiting delay so the first non-waiting call
    /// starts at [`PermissionPoll::backoff_start`].
    #[must_use]
    pub const fn new(poll: PermissionPoll) -> Self { Self { poll, delay: poll.waiting } }

    /// Returns the delay to sleep and advances the schedule.
    pub fn next(&mut self, waiting: bool) -> Duration {
        if waiting {
            self.delay = self.poll.waiting;
        } else if self.delay == self.poll.waiting {
            self.delay = self.poll.backoff_start;
        } else {
            self.delay = self.poll.backoff_max.min(self.delay.saturating_mul(2));
        }
        self.delay
    }
}

/// A small gate that keeps at most one pending periodic permission tick.
///
/// The ticker marks it busy; the host clears it when it starts handling the
/// tick. While a lifecycle operation blocks the host, later periodic ticks are
/// coalesced instead of filling the command channel.
#[derive(Clone, Debug, Default)]
pub struct TickGate(Arc<AtomicBool>);

impl TickGate {
    /// Creates an idle gate.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Marks a tick pending, returning false when one already is.
    #[must_use]
    pub fn try_begin(&self) -> bool { !self.0.swap(true, Ordering::SeqCst) }

    /// Marks the pending tick as received.
    pub fn finish(&self) { self.0.store(false, Ordering::SeqCst); }
}

/// A cancellable thread that wakes the host on the current poll delay.
pub struct PermissionTicker {
    shared: Arc<TickerShared>,
    thread: Option<JoinHandle<()>>,
}

struct TickerShared {
    state: Mutex<TickerState>,
    signal: std::sync::Condvar,
}

struct TickerState {
    delay: Duration,
    generation: u64,
    stopped: bool,
}

impl PermissionTicker {
    /// Starts a ticker that calls `notify` after every delay.
    ///
    /// A delay change wakes an in-progress wait and restarts it from the new
    /// duration.
    ///
    /// # Errors
    /// Returns the OS failure when the thread cannot be created.
    pub fn start(initial: Duration, notify: impl Fn() + Send + 'static) -> io::Result<Self> {
        let shared = Arc::new(TickerShared {
            state: Mutex::new(TickerState {
                delay: initial,
                generation: 0,
                stopped: false,
            }),
            signal: std::sync::Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("stache-permissions".to_owned())
            .spawn(move || {
                let mut state =
                    worker.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                loop {
                    if state.stopped {
                        return;
                    }
                    let generation = state.generation;
                    let delay = state.delay;
                    let (next, timeout) = worker
                        .signal
                        .wait_timeout_while(state, delay, |state| {
                            state.generation == generation && !state.stopped
                        })
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state = next;
                    if state.stopped {
                        return;
                    }
                    if state.generation != generation || !timeout.timed_out() {
                        continue;
                    }
                    drop(state);
                    notify();
                    state = worker.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                }
            })
            .map_err(|error| {
                io::Error::other(format!("cannot start permission ticker: {error}"))
            })?;
        Ok(Self { shared, thread: Some(thread) })
    }

    /// Replaces the next delay and wakes an in-progress wait.
    pub fn set_delay(&self, delay: Duration) {
        let mut state = self.shared.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.delay = delay;
        state.generation = state.generation.wrapping_add(1);
        drop(state);
        self.shared.signal.notify_all();
    }

    /// Cancels the ticker without joining it.
    pub fn cancel(&self) {
        {
            let mut state =
                self.shared.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.stopped = true;
        }
        self.shared.signal.notify_all();
    }

    /// Cancels and joins the ticker thread. Call off the main thread.
    pub fn stop(self) {
        self.cancel();
        if let Some(thread) = self.thread {
            let _ = thread.join();
        }
    }

    /// Cancels the ticker and waits for its thread within the teardown bound.
    ///
    /// Returns `true` when the thread finished inside the bound. On `false` the
    /// handle is dropped, detaching the thread instead of joining it.
    pub fn stop_bounded(mut self, wait: &dyn ShutdownWait) -> bool {
        self.cancel();
        let Some(thread) = self.thread.take() else {
            return true;
        };
        if wait_for_finish(wait, &|| thread.is_finished()) {
            let _ = thread.join();
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use stache_sdk::PermissionError;

    use super::*;

    struct FakeProvider {
        status: Mutex<PermissionStatus>,
        requests: AtomicUsize,
        grant_on_request: AtomicBool,
    }

    impl Default for FakeProvider {
        fn default() -> Self {
            Self {
                status: Mutex::new(PermissionStatus::Denied),
                requests: AtomicUsize::new(0),
                grant_on_request: AtomicBool::new(false),
            }
        }
    }

    impl PermissionProvider for FakeProvider {
        fn status(&self, _: Permission) -> PermissionStatus { *self.status.lock().expect("status") }

        fn request(&self, permission: Permission) -> Result<PermissionStatus, PermissionError> {
            self.requests.fetch_add(1, Ordering::SeqCst);
            if self.grant_on_request.load(Ordering::SeqCst) {
                *self.status.lock().expect("status") = PermissionStatus::Granted;
            }
            Ok(self.status(permission))
        }
    }

    #[derive(Default)]
    struct FakeHost {
        needed: Mutex<Vec<PermissionRequirement>>,
        waiting: AtomicBool,
        applied: Mutex<Vec<(Permission, PermissionStatus)>>,
    }

    impl FakeHost {
        fn need(&self, permission: Permission) {
            self.require(PermissionRequirement::automatic(permission));
        }

        fn require(&self, requirement: PermissionRequirement) {
            let mut needed = self.needed.lock().expect("needed");
            if !needed.contains(&requirement) {
                needed.push(requirement);
            }
        }

        fn clear(&self) { self.needed.lock().expect("needed").clear(); }
    }

    impl PermissionHost for FakeHost {
        fn needed(&self) -> Vec<PermissionRequirement> {
            self.needed.lock().expect("needed").clone()
        }

        fn waiting(&self) -> bool { self.waiting.load(Ordering::SeqCst) }

        fn apply(&self, permission: Permission, status: PermissionStatus) {
            self.applied.lock().expect("applied").push((permission, status));
        }
    }

    fn provider(status: PermissionStatus) -> Arc<FakeProvider> {
        Arc::new(FakeProvider {
            status: Mutex::new(status),
            ..FakeProvider::default()
        })
    }

    #[test]
    fn no_declared_need_never_prompts() {
        let provider = provider(PermissionStatus::Denied);
        let mut manager = PermissionManager::new(provider.clone());
        let host = FakeHost::default();
        let report = manager.refresh(&host, RefreshTrigger::Startup);
        assert_eq!(report.prompted, None);
        assert_eq!(provider.requests.load(Ordering::SeqCst), 0);
        assert!(!report.waiting);
    }

    #[test]
    fn a_needed_permission_prompts_once_automatic_and_again_explicitly() {
        let provider = provider(PermissionStatus::Denied);
        let mut manager = PermissionManager::new(provider.clone());
        let host = FakeHost::default();
        host.need(Permission::Accessibility);
        let first = manager.refresh(&host, RefreshTrigger::Startup);
        assert_eq!(first.prompted, Some(Permission::Accessibility));
        let second = manager.refresh(&host, RefreshTrigger::Tick);
        assert_eq!(second.prompted, None);
        assert_eq!(provider.requests.load(Ordering::SeqCst), 1);
        let explicit = manager.refresh(&host, RefreshTrigger::Explicit);
        assert_eq!(explicit.prompted, Some(Permission::Accessibility));
        assert_eq!(provider.requests.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn user_only_requirements_are_never_requested_automatically() {
        let provider = provider(PermissionStatus::Denied);
        let mut manager = PermissionManager::new(provider.clone());
        let host = FakeHost::default();
        host.require(PermissionRequirement::shared_input(Permission::Accessibility));
        for trigger in [
            RefreshTrigger::Startup,
            RefreshTrigger::Tick,
            RefreshTrigger::Activation,
            RefreshTrigger::Tick,
        ] {
            let report = manager.refresh(&host, trigger);
            assert_eq!(report.prompted, None);
            assert!(!report.waiting);
        }
        assert_eq!(provider.requests.load(Ordering::SeqCst), 0);
        // The status is still checked and applied without prompting.
        assert_eq!(host.applied.lock().expect("applied").as_slice(), [(
            Permission::Accessibility,
            PermissionStatus::Denied
        )]);
        let explicit = manager.refresh(&host, RefreshTrigger::Explicit);
        assert_eq!(explicit.prompted, Some(Permission::Accessibility));
        assert_eq!(provider.requests.load(Ordering::SeqCst), 1);
        // The explicit request does not consume or grant later automatic prompts.
        manager.refresh(&host, RefreshTrigger::Tick);
        assert_eq!(provider.requests.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_user_only_requirement_wins_over_automatic_prompting_state() {
        let provider = provider(PermissionStatus::Denied);
        let mut manager = PermissionManager::new(provider.clone());
        let host = FakeHost::default();
        host.require(PermissionRequirement::user_only(Permission::Accessibility));
        manager.refresh(&host, RefreshTrigger::Startup);
        // The config stops needing it, then needs it again: still no auto prompt.
        host.clear();
        manager.refresh(&host, RefreshTrigger::Tick);
        host.require(PermissionRequirement::user_only(Permission::Accessibility));
        manager.refresh(&host, RefreshTrigger::Tick);
        assert_eq!(provider.requests.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_requirement_that_returns_rereads_a_fresh_status() {
        let provider = provider(PermissionStatus::Granted);
        let mut manager = PermissionManager::new(provider.clone());
        let host = FakeHost::default();
        host.require(PermissionRequirement::user_only(Permission::Accessibility));
        manager.refresh(&host, RefreshTrigger::Startup);
        host.clear();
        manager.refresh(&host, RefreshTrigger::Tick);
        *provider.status.lock().expect("status") = PermissionStatus::Denied;
        host.require(PermissionRequirement::user_only(Permission::Accessibility));
        manager.refresh(&host, RefreshTrigger::Tick);
        let applied = host.applied.lock().expect("applied");
        assert_eq!(
            applied.last(),
            Some(&(Permission::Accessibility, PermissionStatus::Denied))
        );
    }

    #[test]
    fn a_grant_starts_waiting_modules_without_restarting_the_host() {
        let provider = provider(PermissionStatus::Denied);
        provider.grant_on_request.store(true, Ordering::SeqCst);
        let mut manager = PermissionManager::new(provider);
        let host = FakeHost::default();
        host.need(Permission::Accessibility);
        host.waiting.store(true, Ordering::SeqCst);
        let report = manager.refresh(&host, RefreshTrigger::Startup);
        assert_eq!(report.prompted, Some(Permission::Accessibility));
        assert!(report.changed);
        let applied = host.applied.lock().expect("applied");
        let contains = applied.contains(&(Permission::Accessibility, PermissionStatus::Granted));
        drop(applied);
        assert!(contains);
    }

    #[test]
    fn revocation_is_applied_on_the_next_tick_without_a_new_prompt() {
        let provider = provider(PermissionStatus::Granted);
        let mut manager = PermissionManager::new(provider.clone());
        let host = FakeHost::default();
        host.need(Permission::Accessibility);
        manager.refresh(&host, RefreshTrigger::Startup);
        *provider.status.lock().expect("status") = PermissionStatus::Denied;
        let report = manager.refresh(&host, RefreshTrigger::Tick);
        assert!(report.changed);
        assert_eq!(report.prompted, None, "automatic prompt already happened");
        assert!(
            host.applied
                .lock()
                .expect("applied")
                .contains(&(Permission::Accessibility, PermissionStatus::Denied))
        );
    }

    #[test]
    fn the_poll_schedule_waits_fast_then_backs_off_and_keeps_checking() {
        let poll = PermissionPoll {
            waiting: Duration::from_millis(10),
            backoff_start: Duration::from_millis(20),
            backoff_max: Duration::from_millis(80),
        };
        let mut schedule = PollSchedule::new(poll);
        assert_eq!(schedule.next(false), Duration::from_millis(20));
        assert_eq!(schedule.next(false), Duration::from_millis(40));
        assert_eq!(schedule.next(false), Duration::from_millis(80));
        assert_eq!(schedule.next(false), Duration::from_millis(80));
        assert_eq!(schedule.next(true), Duration::from_millis(10));
        assert_eq!(schedule.next(false), Duration::from_millis(20));
        assert_eq!(schedule.next(false), Duration::from_millis(40));
    }

    #[test]
    fn the_ticker_notifies_until_stopped() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let ticker = PermissionTicker::start(Duration::from_millis(5), move || {
            let _ = sender.send(());
        })
        .expect("ticker");
        receiver.recv_timeout(Duration::from_secs(2)).expect("first tick");
        ticker.stop();
        while receiver.recv_timeout(Duration::from_millis(20)).is_ok() {}
    }

    #[test]
    fn a_shorter_delay_wakes_the_current_wait_immediately() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let ticker = PermissionTicker::start(Duration::from_secs(60), move || {
            let _ = sender.send(());
        })
        .expect("ticker");
        let started = std::time::Instant::now();
        ticker.set_delay(Duration::from_millis(10));
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("the changed delay must produce a tick");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the original 60-second wait was not interrupted"
        );
        ticker.stop();
    }

    #[test]
    fn the_tick_gate_keeps_only_one_pending_tick() {
        let gate = TickGate::new();
        assert!(gate.try_begin());
        assert!(!gate.try_begin());
        gate.finish();
        assert!(gate.try_begin());
    }
}
