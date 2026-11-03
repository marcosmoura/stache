//! Exit convergence shared by the tray, Tauri's exit event and POSIX signals.
//!
//! Every path calls the same [`HostHandle::request_shutdown`](crate::host::HostHandle)
//! once; the host runner performs the ordered teardown on its own thread and
//! asks the application to exit afterwards. Signal handling uses `signal-hook`
//! rather than project-authored unsafe code.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::thread::JoinHandle;
use std::{io, thread};

use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::{Handle, Signals};

/// The final-exit permission shared with Tauri's `ExitRequested` handler.
#[derive(Clone, Debug)]
pub struct ExitGate {
    allowed: Arc<AtomicBool>,
    exit_code: Arc<AtomicI32>,
}

impl Default for ExitGate {
    fn default() -> Self {
        Self {
            allowed: Arc::new(AtomicBool::new(false)),
            exit_code: Arc::new(AtomicI32::new(i32::MIN)),
        }
    }
}

impl ExitGate {
    /// Creates a gate that still prevents exit.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Allows the next Tauri exit event to proceed.
    pub fn allow(&self) { self.allowed.store(true, Ordering::SeqCst); }

    /// Records the host's intended process outcome and permits exit.
    ///
    /// `run_return` does not reliably report the code passed to `app.exit` on
    /// macOS, so the caller must combine this value with the runtime result.
    pub fn record_exit(&self, code: i32) {
        self.exit_code.store(code, Ordering::SeqCst);
        self.allow();
    }

    /// The host's requested exit code, if teardown reached `HostUi::exit`.
    #[must_use]
    pub fn exit_code(&self) -> Option<i32> {
        let code = self.exit_code.load(Ordering::SeqCst);
        (code != i32::MIN).then_some(code)
    }

    /// Whether the host finished its teardown.
    #[must_use]
    pub fn is_allowed(&self) -> bool { self.allowed.load(Ordering::SeqCst) }
}

/// What the run loop must do for one `RunEvent::ExitRequested`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitAction {
    /// Let the runtime exit; the host already finished its teardown.
    Allow,
    /// Keep the run loop alive: only a window closed, the host stays up.
    PreventOnly,
    /// Prevent this exit and start the coordinated host shutdown instead.
    PreventAndShutdown,
}

/// Decides how to answer one Tauri exit request.
///
/// `tauri-runtime-wry` emits `code: None` only when the last window was
/// destroyed, and `Some(code)` only for `AppHandle::exit`/`restart`. A window
/// close must not tear the host down, while an explicit quit request must.
#[must_use]
pub const fn exit_request_action(code: Option<i32>, allowed: bool) -> ExitAction {
    match (allowed, code) {
        (true, _) => ExitAction::Allow,
        (false, None) => ExitAction::PreventOnly,
        (false, Some(_)) => ExitAction::PreventAndShutdown,
    }
}

/// Installs SIGINT/SIGTERM/SIGHUP handling for one process.
pub struct SignalBridge {
    handle: Handle,
    thread: Option<JoinHandle<()>>,
}

impl SignalBridge {
    /// Starts a thread that calls `notify` for each termination signal.
    ///
    /// # Errors
    /// Returns the OS failure when the signal set cannot be registered or the
    /// thread cannot be created.
    pub fn install(notify: impl Fn() + Send + 'static) -> io::Result<Self> {
        let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP])?;
        let handle = signals.handle();
        let thread = thread::Builder::new().name("stache-signals".to_owned()).spawn(move || {
            for _signal in signals.forever() {
                notify();
            }
        })?;
        Ok(Self { handle, thread: Some(thread) })
    }

    /// Stops the signal thread, closes its handle and joins it.
    ///
    /// The host calls this on its worker thread during teardown, never on the
    /// main thread, so the join cannot block the event loop.
    pub fn stop(mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for SignalBridge {
    fn drop(&mut self) {
        // Normal shutdown takes the bridge and calls `stop` off the main thread.
        // This fallback only releases the handle; it never blocks an unexpected
        // main-thread teardown on a join.
        self.handle.close();
        self.thread.take();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn the_gate_blocks_until_allowed() {
        let gate = ExitGate::new();
        assert!(!gate.is_allowed());
        gate.allow();
        assert!(gate.is_allowed());
    }

    #[test]
    fn recording_an_exit_code_allows_exit_and_keeps_the_outcome() {
        let gate = ExitGate::new();
        assert_eq!(gate.exit_code(), None);
        gate.record_exit(1);
        assert!(gate.is_allowed());
        assert_eq!(gate.exit_code(), Some(1));
        gate.record_exit(0);
        assert_eq!(gate.exit_code(), Some(0));
    }

    #[test]
    fn an_allowed_gate_lets_every_exit_request_through() {
        for code in [None, Some(0), Some(1), Some(70)] {
            assert_eq!(exit_request_action(code, true), ExitAction::Allow);
        }
    }

    #[test]
    fn a_closed_last_window_only_prevents_the_exit() {
        assert_eq!(exit_request_action(None, false), ExitAction::PreventOnly);
    }

    #[test]
    fn an_explicit_quit_code_always_starts_the_shutdown() {
        for code in [Some(0), Some(1), Some(70)] {
            assert_eq!(exit_request_action(code, false), ExitAction::PreventAndShutdown);
        }
    }

    #[test]
    fn the_signal_bridge_installs_and_stops_without_a_signal() {
        let (sender, receiver) = mpsc::channel();
        let bridge = SignalBridge::install(move || {
            let _ = sender.send(());
        })
        .expect("signals");
        assert!(receiver.try_recv().is_err());
        bridge.stop();
    }
}
