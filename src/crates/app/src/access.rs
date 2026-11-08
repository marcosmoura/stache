//! The app-wide Accessibility launch gate.
//!
//! [`AccessGate`] is a pure state machine over one fresh Accessibility status.
//! It fails closed: only `Granted` opens it, `Denied` and `Unknown` block. Each
//! observation returns the [`AccessTransition`] the host runner turns into
//! effects (log record, deferred `execOnStartup`), so the runner never
//! re-derives them. Module stop and restart are not done here: the registry's
//! baseline requirement already stops and restarts modules on the same status.

use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde_json::Value;
use stache_sdk::ipc::{CoreAccess, CoreAccessState, HandlerError, HandlerResult, IpcHandler};
use stache_sdk::{MainThreadExecutor, PermissionStatus};

/// The gate's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessGateState {
    /// No status has been observed yet.
    Checking,
    /// Accessibility is missing. `revoked` is true when it was open earlier in
    /// this launch.
    Blocked {
        /// Whether the gate was open before in this launch.
        revoked: bool,
    },
    /// Accessibility is granted.
    Open,
}

/// What one observation changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessTransition {
    /// Nothing changed.
    None,
    /// First block of this launch (never open before).
    Blocked,
    /// The gate opened. `first` is true the first time in this launch.
    Opened {
        /// Whether this is the first opening in this launch.
        first: bool,
    },
    /// The gate was open and lost trust.
    Revoked,
}

/// Pure Accessibility gate. Holds no thread or native resource.
#[derive(Clone, Copy, Debug)]
pub struct AccessGate {
    state: AccessGateState,
    opened_before: bool,
}

impl Default for AccessGate {
    fn default() -> Self { Self::new() }
}

impl AccessGate {
    /// Creates a gate in [`AccessGateState::Checking`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: AccessGateState::Checking,
            opened_before: false,
        }
    }

    /// The current state.
    #[must_use]
    pub const fn state(&self) -> AccessGateState { self.state }

    /// Whether the gate is open.
    #[must_use]
    pub const fn is_open(&self) -> bool { matches!(self.state, AccessGateState::Open) }

    /// Feeds one fresh Accessibility status.
    pub const fn observe(&mut self, status: PermissionStatus) -> AccessTransition {
        match (status, self.state) {
            (PermissionStatus::Granted, AccessGateState::Open) => AccessTransition::None,
            (PermissionStatus::Granted, _) => {
                let first = !self.opened_before;
                self.opened_before = true;
                self.state = AccessGateState::Open;
                AccessTransition::Opened { first }
            }
            (_, AccessGateState::Open) => {
                self.state = AccessGateState::Blocked { revoked: true };
                AccessTransition::Revoked
            }
            (_, AccessGateState::Checking) => {
                self.state = AccessGateState::Blocked { revoked: false };
                AccessTransition::Blocked
            }
            (_, AccessGateState::Blocked { .. }) => AccessTransition::None,
        }
    }
}

/// The message every refused module method carries while the gate is closed.
pub const ACCESS_WAITING_MESSAGE: &str = "Stache Next is waiting for Accessibility permission. Grant it in System Settings > Privacy & Security > Accessibility.";

const CELL_CHECKING: u8 = 0;
const CELL_BLOCKED: u8 = 1;
const CELL_REVOKED: u8 = 2;
const CELL_OPEN: u8 = 3;

/// A lock-free copy of the gate state for the socket threads.
///
/// The host runner writes it on every gate transition; readers load one atomic
/// and never take the host worker, a module lock or the registry lifecycle lock.
#[derive(Clone, Debug)]
pub struct AccessCell(Arc<AtomicU8>);

impl AccessCell {
    /// A cell that reads `open`, for an ungated host.
    #[must_use]
    pub fn open() -> Self { Self(Arc::new(AtomicU8::new(CELL_OPEN))) }

    /// A cell that reads `checking`, for a gated host before its first check.
    #[must_use]
    pub fn checking() -> Self { Self(Arc::new(AtomicU8::new(CELL_CHECKING))) }

    /// Publishes the gate state.
    pub fn store(&self, state: AccessGateState) {
        let value = match state {
            AccessGateState::Checking => CELL_CHECKING,
            AccessGateState::Blocked { revoked: false } => CELL_BLOCKED,
            AccessGateState::Blocked { revoked: true } => CELL_REVOKED,
            AccessGateState::Open => CELL_OPEN,
        };
        self.0.store(value, Ordering::SeqCst);
    }

    /// Closes an open cell as revoked (a checking cell as blocked) without the
    /// gate. The runner's next observation writes the authoritative state.
    pub fn close(&self) {
        let _ =
            self.0
                .compare_exchange(CELL_OPEN, CELL_REVOKED, Ordering::SeqCst, Ordering::SeqCst);
        let _ = self.0.compare_exchange(
            CELL_CHECKING,
            CELL_BLOCKED,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    /// The published state.
    #[must_use]
    pub fn load(&self) -> AccessGateState {
        match self.0.load(Ordering::SeqCst) {
            CELL_CHECKING => AccessGateState::Checking,
            CELL_BLOCKED => AccessGateState::Blocked { revoked: false },
            CELL_REVOKED => AccessGateState::Blocked { revoked: true },
            _ => AccessGateState::Open,
        }
    }

    /// Whether the gate is open.
    #[must_use]
    pub fn is_open(&self) -> bool { matches!(self.load(), AccessGateState::Open) }

    /// The state as reported over the socket.
    #[must_use]
    pub fn report(&self) -> CoreAccess {
        match self.load() {
            AccessGateState::Checking => CoreAccess {
                state: CoreAccessState::Checking,
                revoked: false,
            },
            AccessGateState::Blocked { revoked } => CoreAccess {
                state: CoreAccessState::Blocked,
                revoked,
            },
            AccessGateState::Open => CoreAccess {
                state: CoreAccessState::Open,
                revoked: false,
            },
        }
    }
}

/// Wraps one module's socket handler so it answers `unavailable` and never
/// reaches the module while the gate is not open.
pub struct GatedHandler {
    cell: AccessCell,
    inner: Arc<dyn IpcHandler>,
}

impl GatedHandler {
    /// Wraps `inner` behind `cell`.
    #[must_use]
    pub fn new(cell: AccessCell, inner: Arc<dyn IpcHandler>) -> Self { Self { cell, inner } }
}

impl IpcHandler for GatedHandler {
    fn handle(&self, method: &str, params: Value) -> HandlerResult<Value> {
        if self.cell.is_open() {
            self.inner.handle(method, params)
        } else {
            Err(HandlerError::unavailable(ACCESS_WAITING_MESSAGE))
        }
    }
}

/// Opens the Accessibility pane of System Settings.
pub trait SettingsOpener: Send + Sync {
    /// Opens the pane.
    ///
    /// # Errors
    /// Returns a user-facing failure description.
    fn open_accessibility_settings(&self) -> Result<(), String>;
}

/// An opener for hosts without System Settings, such as test fixtures.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableSettings;

impl SettingsOpener for UnavailableSettings {
    fn open_accessibility_settings(&self) -> Result<(), String> {
        Err("System Settings is unavailable".to_owned())
    }
}

/// How long the host worker waits for the main thread to open System Settings.
const OPEN_WAIT: Duration = Duration::from_secs(2);

/// The main-thread job has not started and the waiter has not given up.
const PENDING: u8 = 0;
/// The main-thread job started opening; the waiter keeps waiting for its result.
const CLAIMED: u8 = 1;
/// The waiter timed out; a late job must not open anything.
const ABANDONED: u8 = 2;

type OpenFn = dyn Fn() -> Result<(), String> + Send + Sync;

/// The production opener: hops to the main thread through the host executor.
pub struct SystemSettingsOpener {
    main_thread: Arc<dyn MainThreadExecutor>,
    open: Arc<OpenFn>,
    wait: Duration,
}

impl SystemSettingsOpener {
    /// Creates an opener over the host main-thread executor.
    #[must_use]
    pub fn new(main_thread: Arc<dyn MainThreadExecutor>) -> Self {
        Self::with_open(main_thread, OPEN_WAIT, || {
            stache_macos::settings::open_accessibility_settings().map_err(|error| error.to_string())
        })
    }

    fn with_open(
        main_thread: Arc<dyn MainThreadExecutor>,
        wait: Duration,
        open: impl Fn() -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            main_thread,
            open: Arc::new(open),
            wait,
        }
    }
}

impl SettingsOpener for SystemSettingsOpener {
    fn open_accessibility_settings(&self) -> Result<(), String> {
        let (sender, receiver) = mpsc::channel();
        let claim = Arc::new(AtomicU8::new(PENDING));
        let job_claim = Arc::clone(&claim);
        let open = Arc::clone(&self.open);
        self.main_thread
            .execute(Box::new(move || {
                // A job that runs after the waiter gave up must not open.
                if job_claim
                    .compare_exchange(PENDING, CLAIMED, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    let _ = sender.send(open());
                }
            }))
            .map_err(|error| format!("cannot open System Settings: {error}"))?;
        if let Ok(result) = receiver.recv_timeout(self.wait) {
            return result;
        }
        // If the job already claimed the open, report its result instead.
        if claim
            .compare_exchange(PENDING, ABANDONED, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return receiver
                .recv()
                .unwrap_or_else(|_| Err("System Settings did not open in time".to_owned()));
        }
        Err("System Settings did not open in time".to_owned())
    }
}

/// The enclosing `.app` bundle path of an executable, or `None` when the
/// executable is not inside a bundle.
#[must_use]
pub fn enclosing_app(executable: &Path) -> Option<String> {
    executable
        .ancestors()
        .skip(1)
        .find(|path| path.extension().is_some_and(|extension| extension == "app"))
        .map(|path| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRANTED: PermissionStatus = PermissionStatus::Granted;
    const DENIED: PermissionStatus = PermissionStatus::Denied;
    const UNKNOWN: PermissionStatus = PermissionStatus::Unknown;

    fn open_gate() -> AccessGate {
        let mut gate = AccessGate::new();
        gate.observe(GRANTED);
        gate
    }

    #[test]
    fn checking_fails_closed_on_denied_and_unknown() {
        for status in [DENIED, UNKNOWN] {
            let mut gate = AccessGate::new();
            assert_eq!(gate.state(), AccessGateState::Checking);
            assert_eq!(gate.observe(status), AccessTransition::Blocked);
            assert_eq!(gate.state(), AccessGateState::Blocked { revoked: false });
            assert!(!gate.is_open());
        }
    }

    #[test]
    fn checking_opens_on_granted_as_the_first_opening() {
        let mut gate = AccessGate::new();
        assert_eq!(gate.observe(GRANTED), AccessTransition::Opened { first: true });
        assert!(gate.is_open());
    }

    #[test]
    fn blocked_stays_blocked_without_a_transition_and_opens_on_granted() {
        let mut gate = AccessGate::new();
        gate.observe(DENIED);
        assert_eq!(gate.observe(DENIED), AccessTransition::None);
        assert_eq!(gate.observe(UNKNOWN), AccessTransition::None);
        assert_eq!(gate.state(), AccessGateState::Blocked { revoked: false });
        assert_eq!(gate.observe(GRANTED), AccessTransition::Opened { first: true });
    }

    #[test]
    fn open_stays_open_on_granted_and_is_revoked_by_denied_or_unknown() {
        let mut gate = open_gate();
        assert_eq!(gate.observe(GRANTED), AccessTransition::None);
        for status in [DENIED, UNKNOWN] {
            let mut gate = open_gate();
            assert_eq!(gate.observe(status), AccessTransition::Revoked);
            assert_eq!(gate.state(), AccessGateState::Blocked { revoked: true });
        }
    }

    #[test]
    fn a_revoked_gate_keeps_revoked_until_it_reopens_and_the_reopening_is_not_first() {
        let mut gate = open_gate();
        gate.observe(DENIED);
        assert_eq!(gate.observe(UNKNOWN), AccessTransition::None);
        assert_eq!(gate.state(), AccessGateState::Blocked { revoked: true });
        assert_eq!(gate.observe(GRANTED), AccessTransition::Opened { first: false });
        assert!(gate.is_open());
        // A later revocation is revoked again, not a first block.
        assert_eq!(gate.observe(DENIED), AccessTransition::Revoked);
    }

    #[test]
    fn the_cell_round_trips_every_state_and_reports_them() {
        let cell = AccessCell::checking();
        assert_eq!(cell.load(), AccessGateState::Checking);
        assert_eq!(cell.report().state, CoreAccessState::Checking);
        for state in [
            AccessGateState::Blocked { revoked: false },
            AccessGateState::Blocked { revoked: true },
            AccessGateState::Open,
            AccessGateState::Checking,
        ] {
            cell.store(state);
            assert_eq!(cell.load(), state);
        }
        cell.store(AccessGateState::Blocked { revoked: true });
        assert_eq!(cell.report(), CoreAccess {
            state: CoreAccessState::Blocked,
            revoked: true
        });
        assert!(AccessCell::open().is_open());
    }

    #[test]
    fn closing_an_open_cell_reads_revoked_and_a_blocked_cell_is_untouched() {
        let cell = AccessCell::open();
        cell.close();
        assert_eq!(cell.load(), AccessGateState::Blocked { revoked: true });
        let cell = AccessCell::checking();
        cell.close();
        assert_eq!(cell.load(), AccessGateState::Blocked { revoked: false });
        cell.close();
        assert_eq!(cell.load(), AccessGateState::Blocked { revoked: false });
    }

    struct Counting(std::sync::atomic::AtomicUsize);

    impl IpcHandler for Counting {
        fn handle(&self, _: &str, params: Value) -> HandlerResult<Value> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(params)
        }
    }

    #[test]
    fn ag_03_01_a_wrapped_handler_refuses_while_checking_or_blocked_and_delegates_when_open() {
        let inner = Arc::new(Counting(std::sync::atomic::AtomicUsize::new(0)));
        let cell = AccessCell::checking();
        let handler = GatedHandler::new(cell.clone(), inner.clone());
        for state in [
            AccessGateState::Checking,
            AccessGateState::Blocked { revoked: false },
            AccessGateState::Blocked { revoked: true },
        ] {
            cell.store(state);
            let error = handler.handle("status", Value::Null).expect_err("refused");
            assert_eq!(error, HandlerError::unavailable(ACCESS_WAITING_MESSAGE));
        }
        assert_eq!(
            inner.0.load(Ordering::SeqCst),
            0,
            "the inner handler is never called"
        );
        assert_eq!(
            ACCESS_WAITING_MESSAGE,
            "Stache Next is waiting for Accessibility permission. Grant it in System Settings > Privacy & Security > Accessibility."
        );

        cell.store(AccessGateState::Open);
        assert_eq!(handler.handle("echo", Value::from(7)), Ok(Value::from(7)));
        assert_eq!(inner.0.load(Ordering::SeqCst), 1);
    }

    /// A main-thread executor that parks jobs for the test to run later.
    #[derive(Default)]
    struct DeferredMain {
        jobs: std::sync::Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>,
        inline: bool,
    }

    impl MainThreadExecutor for DeferredMain {
        fn is_main_thread(&self) -> bool { false }

        fn execute(
            &self,
            job: Box<dyn FnOnce() + Send + 'static>,
        ) -> Result<(), stache_sdk::ThreadError> {
            if self.inline {
                job();
            } else {
                self.jobs.lock().expect("jobs").push(job);
            }
            Ok(())
        }
    }

    fn counting_opener(
        main: Arc<DeferredMain>,
        wait: Duration,
    ) -> (SystemSettingsOpener, Arc<std::sync::atomic::AtomicUsize>) {
        let opens = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&opens);
        let opener = SystemSettingsOpener::with_open(main, wait, move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        (opener, opens)
    }

    #[test]
    fn a_timed_out_open_reports_an_error_and_the_late_job_never_opens() {
        let main = Arc::new(DeferredMain::default());
        let (opener, opens) = counting_opener(Arc::clone(&main), Duration::from_millis(20));
        assert_eq!(
            opener.open_accessibility_settings(),
            Err("System Settings did not open in time".to_owned())
        );
        let jobs: Vec<_> = main.jobs.lock().expect("jobs").drain(..).collect();
        assert_eq!(jobs.len(), 1);
        for job in jobs {
            job();
        }
        assert_eq!(
            opens.load(Ordering::SeqCst),
            0,
            "the abandoned job skips the open"
        );
    }

    #[test]
    fn a_prompt_main_thread_opens_once_and_reports_success() {
        let main = Arc::new(DeferredMain {
            inline: true,
            ..DeferredMain::default()
        });
        let (opener, opens) = counting_opener(main, Duration::from_secs(5));
        assert_eq!(opener.open_accessibility_settings(), Ok(()));
        assert_eq!(opens.load(Ordering::SeqCst), 1);
    }

    struct ThreadMain;

    impl MainThreadExecutor for ThreadMain {
        fn is_main_thread(&self) -> bool { false }

        fn execute(
            &self,
            job: Box<dyn FnOnce() + Send + 'static>,
        ) -> Result<(), stache_sdk::ThreadError> {
            std::thread::spawn(job);
            Ok(())
        }
    }

    #[test]
    fn a_claimed_open_that_finishes_after_the_wait_reports_its_own_result() {
        let opens = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&opens);
        // The job claims immediately on its own thread, then outlives the wait.
        let opener =
            SystemSettingsOpener::with_open(Arc::new(ThreadMain), Duration::from_millis(200), {
                move || {
                    std::thread::sleep(Duration::from_millis(400));
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            });
        assert_eq!(opener.open_accessibility_settings(), Ok(()));
        assert_eq!(opens.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_enclosing_bundle_is_found_or_absent() {
        assert_eq!(
            enclosing_app(Path::new("/Applications/Stache Next.app/Contents/MacOS/stache")),
            Some("/Applications/Stache Next.app".to_owned())
        );
        assert_eq!(enclosing_app(Path::new("/tmp/target/debug/stache")), None);
    }
}
