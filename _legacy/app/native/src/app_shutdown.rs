//! Centralized shutdown coordinator.
//!
//! Guarantees correct cleanup order on exit or restart:
//! 1. Restore applications hidden by Stache.
//! 2. Restore the Caps Lock remapping applied for `CapsLock` keybindings.
//! 3. Shut down the tiling window manager.
//! 4. Stop the IPC socket server.
//!
//! # Signal Handling
//!
//! The first termination signal (SIGINT/SIGTERM/SIGHUP via ctrlc's
//! `termination` feature) schedules an orderly exit through Tauri's main
//! thread.  A second signal is treated as a force-exit escalation because
//! the main thread may be stalled.  All termination signals share the
//! escalation counter.
//!
//! # Terminal-Request Arbitration
//!
//! Multiple threads may call `exit` or `restart` concurrently.  An atomic
//! arbiter ensures that:
//! - Exit has precedence over Restart.
//! - At most one terminal action is executed.
//! - A queued closure re-checks whether its action is still current before
//!   running cleanup and again before calling `exit`/`restart`, so an Exit
//!   that overrides a pending Restart suppresses the restart closure even
//!   if already queued.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use tauri::{AppHandle, Runtime};

use crate::modules::{hotkey, tiling};
use crate::platform;

// ============================================================================
// Terminal-Request Arbiter — CAS state machine
// ============================================================================
//
// States (atomic u8):
//
//   NONE (0)             — no terminal action requested
//   RESTART_PENDING (1)  — restart claimed, closure may be queued
//   EXIT_PENDING (2)     — exit claimed, closure may be queued
//   RESTART_COMMITTED (3)— restart closure passed commit barrier
//   EXIT_COMMITTED (4)   — exit closure passed commit barrier
//   NATURAL_EXIT (5)     — app is exiting via RunEvent::Exit (irrevocable)
//
// Allowed transitions (CAS, no unconditional stores):
//
//   try_claim_restart
//     NONE → RESTART_PENDING
//
//   try_claim_exit  (CAS loop, exactly one caller wins per transition)
//     NONE → EXIT_PENDING
//     RESTART_PENDING → EXIT_PENDING
//
//   try_commit (called on main thread right before terminal call)
//     RESTART_PENDING → RESTART_COMMITTED
//     EXIT_PENDING → EXIT_COMMITTED
//
//   release_claim (on dispatch failure, or any CAS mismatch = no-op)
//     RESTART_PENDING → NONE
//     EXIT_PENDING → NONE
//
//   establish_exit_precedence (CAS loop)
//     NONE → NATURAL_EXIT
//     RESTART_PENDING → NATURAL_EXIT
//     EXIT_PENDING → NATURAL_EXIT
//     (COMMITTED / NATURAL_EXIT left alone)

const ARBITER_NONE: u8 = 0;
const ARBITER_RESTART_PENDING: u8 = 1;
const ARBITER_EXIT_PENDING: u8 = 2;
const ARBITER_RESTART_COMMITTED: u8 = 3;
const ARBITER_EXIT_COMMITTED: u8 = 4;
const ARBITER_NATURAL_EXIT: u8 = 5;

static TERMINAL_REQUEST: AtomicU8 = AtomicU8::new(ARBITER_NONE);

/// Core: claim a restart on `state`.  Only succeeds from NONE.
fn try_claim_restart_on(state: &AtomicU8) -> bool {
    state
        .compare_exchange(
            ARBITER_NONE,
            ARBITER_RESTART_PENDING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
}

/// Core: claim an exit on `state`.  CAS loop: exactly one caller wins.
/// Permits `NONE` → `EXIT_PENDING` and `RESTART_PENDING` → `EXIT_PENDING`.
/// Returns `false` if already claimed, committed, or natural exit.
fn try_claim_exit_on(state: &AtomicU8) -> bool {
    loop {
        let s = state.load(Ordering::Acquire);
        match s {
            ARBITER_NONE => {
                if state
                    .compare_exchange(
                        ARBITER_NONE,
                        ARBITER_EXIT_PENDING,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    return true;
                }
            }
            ARBITER_RESTART_PENDING => {
                if state
                    .compare_exchange(
                        ARBITER_RESTART_PENDING,
                        ARBITER_EXIT_PENDING,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    return true;
                }
                // CAS failed — another thread may have transitioned away.
                // Loop to re-check the current state.
            }
            // Not NONE or RESTART_PENDING — already claimed, committed,
            // or natural exit established.  Reject.
            _ => return false,
        }
    }
}

/// Core: atomically commit an action on `state` right before the terminal
/// call.  Transitions pending → committed.  Returns false if another action
/// overrode or natural-exit suppression occurred in the meantime.
fn try_commit_on(state: &AtomicU8, action: ShutdownAction) -> bool {
    let (expected, committed) = match action {
        ShutdownAction::Exit => (ARBITER_EXIT_PENDING, ARBITER_EXIT_COMMITTED),
        ShutdownAction::Restart => (ARBITER_RESTART_PENDING, ARBITER_RESTART_COMMITTED),
    };
    state
        .compare_exchange(expected, committed, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

/// Check whether `action` is still pending on `state`.
fn is_current_on(state: &AtomicU8, action: ShutdownAction) -> bool {
    let s = state.load(Ordering::Acquire);
    match action {
        ShutdownAction::Exit => s == ARBITER_EXIT_PENDING,
        ShutdownAction::Restart => s == ARBITER_RESTART_PENDING,
    }
}

/// Core: release the claim on `state` only if it matches the pending state
/// for `action`.  CAS-mismatch (different state) is a safe no-op.
fn release_claim_on(state: &AtomicU8, action: ShutdownAction) {
    let expected = match action {
        ShutdownAction::Exit => ARBITER_EXIT_PENDING,
        ShutdownAction::Restart => ARBITER_RESTART_PENDING,
    };
    let _ = state.compare_exchange(expected, ARBITER_NONE, Ordering::Release, Ordering::Acquire);
}

// --- Global-state wrappers ------------------------------------------------

fn try_claim_exit() -> bool { try_claim_exit_on(&TERMINAL_REQUEST) }
fn try_claim_restart() -> bool { try_claim_restart_on(&TERMINAL_REQUEST) }
fn try_commit(action: ShutdownAction) -> bool { try_commit_on(&TERMINAL_REQUEST, action) }
fn is_current(action: ShutdownAction) -> bool { is_current_on(&TERMINAL_REQUEST, action) }
fn release_claim(action: ShutdownAction) { release_claim_on(&TERMINAL_REQUEST, action) }

/// Establish exit precedence, irrevocably suppressing any pending action.
///
/// Called from the `RunEvent::Exit` handler so a queued restart closure
/// cannot run after the app has entered a natural exit.  Idempotent.
/// Once `NATURAL_EXIT` is set, no future claim or release can touch it.
pub fn establish_exit_precedence() {
    loop {
        let s = TERMINAL_REQUEST.load(Ordering::Acquire);
        match s {
            ARBITER_NATURAL_EXIT | ARBITER_RESTART_COMMITTED | ARBITER_EXIT_COMMITTED => return,
            ARBITER_NONE => {
                if TERMINAL_REQUEST
                    .compare_exchange(
                        ARBITER_NONE,
                        ARBITER_NATURAL_EXIT,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    return;
                }
            }
            ARBITER_RESTART_PENDING => {
                if TERMINAL_REQUEST
                    .compare_exchange(
                        ARBITER_RESTART_PENDING,
                        ARBITER_NATURAL_EXIT,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    return;
                }
            }
            ARBITER_EXIT_PENDING => {
                if TERMINAL_REQUEST
                    .compare_exchange(
                        ARBITER_EXIT_PENDING,
                        ARBITER_NATURAL_EXIT,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    return;
                }
            }
            // SAFETY: all u8 values covered by the match arms above.
            _ => debug_assert!(false, "unreachable arbiter state: {s}"),
        }
    }
}

static CLEANUP_STARTED: AtomicBool = AtomicBool::new(false);
static SIGNAL_COUNT: AtomicU8 = AtomicU8::new(0);

// ============================================================================
// Internal Types
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShutdownAction {
    Exit,
    Restart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignalAction {
    Orderly,
    Force,
}

// ============================================================================
// Signal Tracking
// ============================================================================

fn next_signal_action(signals: &AtomicU8) -> SignalAction {
    if signals.fetch_add(1, Ordering::AcqRel) == 0 {
        SignalAction::Orderly
    } else {
        SignalAction::Force
    }
}

// ============================================================================
// Idempotent Cleanup
// ============================================================================

fn run_cleanup_once(
    started: &AtomicBool,
    restore: impl FnOnce(),
    stop_tiling: impl FnOnce(),
    stop_ipc: impl FnOnce(),
) -> bool {
    if started.swap(true, Ordering::AcqRel) {
        return false;
    }
    restore();
    stop_tiling();
    stop_ipc();
    true
}

/// Runs the full cleanup sequence if it hasn't run yet.
///
/// Safe to call multiple times — only the first call executes the closures.
pub fn cleanup_once() {
    let _ = run_cleanup_once(
        &CLEANUP_STARTED,
        || {
            let summary = tiling::restore_stache_hidden_apps();
            tracing::info!(
                attempted = summary.attempted,
                restored = summary.restored,
                "restored applications hidden by Stache"
            );
            hotkey::shutdown();
        },
        tiling::shutdown,
        platform::ipc_socket::stop_server,
    );
}

// ============================================================================
// Main-Thread Dispatch
// ============================================================================

fn request<R: Runtime>(app: &AppHandle<R>, action: ShutdownAction) {
    // Attempt to claim this terminal action through the arbiter.
    let claimed = match action {
        ShutdownAction::Exit => try_claim_exit(),
        ShutdownAction::Restart => try_claim_restart(),
    };
    if !claimed {
        tracing::warn!(?action, "terminal request rejected by arbiter");
        return;
    }

    let action_handle = app.clone();
    if let Err(error) = app.run_on_main_thread(move || {
        // Re-check before cleanup — a later Exit may have overridden Restart.
        if !is_current(action) {
            tracing::debug!(?action, "terminal request superseded before cleanup");
            return;
        }
        cleanup_once();
        // Re-check after cleanup — an Exit claimed during cleanup suppresses
        // a queued Restart.
        if !is_current(action) {
            tracing::debug!(?action, "terminal request superseded after cleanup");
            return;
        }
        // Atomically commit the action.  If the commit CAS fails (another
        // action overrode, or natural-exit suppression occurred), bail out
        // without calling the terminal function.
        if !try_commit(action) {
            tracing::debug!(?action, "terminal request commit failed — superseded");
            return;
        }
        match action {
            ShutdownAction::Exit => action_handle.exit(0),
            ShutdownAction::Restart => action_handle.restart(),
        }
    }) {
        // Scheduling failed; release the claim only if we still own it.
        release_claim(action);
        tracing::error!(%error, "failed to dispatch orderly shutdown to main thread");
    }
}

/// Schedules an orderly exit (restore → tiling shutdown → IPC stop → exit).
pub fn exit<R: Runtime>(app: &AppHandle<R>) { request(app, ShutdownAction::Exit); }
/// Schedules an orderly restart (restore → tiling shutdown → IPC stop → restart).
#[allow(dead_code)]
pub fn restart<R: Runtime>(app: &AppHandle<R>) { request(app, ShutdownAction::Restart); }
// ============================================================================
// Signal Handler
// ============================================================================

/// Installs one global signal handler for SIGINT/SIGTERM/SIGHUP (via ctrlc's
/// `termination` feature).
///
/// The first signal schedules an orderly exit.  If a second signal arrives
/// before the main thread completes cleanup, the process is force-exited.
/// All termination signals share the escalation counter.
///
/// Must be called exactly once, before `App::run`.
pub fn install_signal_handler<R: Runtime>(app: AppHandle<R>) -> Result<(), ctrlc::Error> {
    ctrlc::set_handler(move || match next_signal_action(&SIGNAL_COUNT) {
        SignalAction::Orderly => exit(&app),
        SignalAction::Force => std::process::exit(1),
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

    use super::{
        ARBITER_EXIT_COMMITTED, ARBITER_EXIT_PENDING, ARBITER_NATURAL_EXIT, ARBITER_NONE,
        ARBITER_RESTART_COMMITTED, ARBITER_RESTART_PENDING, ShutdownAction, SignalAction,
        is_current_on, next_signal_action, release_claim_on, run_cleanup_once, try_claim_exit_on,
        try_claim_restart_on, try_commit_on,
    };

    // =========================================================================
    // Existing cleanup / signal tests (unchanged semantics)
    // =========================================================================

    #[test]
    fn cleanup_restores_before_stopping_tiling_and_ipc() {
        let started = AtomicBool::new(false);
        let order = Mutex::new(Vec::new());

        assert!(run_cleanup_once(
            &started,
            || order.lock().unwrap().push("restore"),
            || order.lock().unwrap().push("tiling"),
            || order.lock().unwrap().push("ipc"),
        ));

        assert_eq!(*order.lock().unwrap(), ["restore", "tiling", "ipc"]);
    }

    #[test]
    fn cleanup_runs_only_once() {
        let started = AtomicBool::new(false);
        let calls = AtomicUsize::new(0);

        assert!(run_cleanup_once(
            &started,
            || {
                calls.fetch_add(1, Ordering::Relaxed);
            },
            || {
                calls.fetch_add(1, Ordering::Relaxed);
            },
            || {
                calls.fetch_add(1, Ordering::Relaxed);
            },
        ));
        assert!(!run_cleanup_once(
            &started,
            || {
                calls.fetch_add(1, Ordering::Relaxed);
            },
            || {
                calls.fetch_add(1, Ordering::Relaxed);
            },
            || {
                calls.fetch_add(1, Ordering::Relaxed);
            },
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn second_termination_signal_forces_exit() {
        let signals = AtomicU8::new(0);

        assert_eq!(next_signal_action(&signals), SignalAction::Orderly);
        assert_eq!(next_signal_action(&signals), SignalAction::Force);
    }

    // =========================================================================
    // State-machine arbiter tests (deterministic, local atomics only)
    // =========================================================================

    /// Exit override wins before Restart commit; restart commit fails and
    /// exit commit succeeds — the critical check/action race scenario.
    #[test]
    fn exit_override_wins_before_restart_commit() {
        let state = AtomicU8::new(ARBITER_NONE);

        // Thread B claims restart
        assert!(try_claim_restart_on(&state));
        assert_eq!(state.load(Ordering::Acquire), ARBITER_RESTART_PENDING);

        // Thread A claims exit (overrides RESTART_PENDING)
        assert!(try_claim_exit_on(&state));
        assert_eq!(state.load(Ordering::Acquire), ARBITER_EXIT_PENDING);

        // Thread B's closure tries to commit — fails (state is EXIT_PENDING)
        assert!(!try_commit_on(&state, ShutdownAction::Restart));

        // Thread A's closure tries to commit — succeeds
        assert!(try_commit_on(&state, ShutdownAction::Exit));
        assert_eq!(state.load(Ordering::Acquire), ARBITER_EXIT_COMMITTED);
    }

    /// Restart commit wins before Exit claim; later Exit claim fails as too
    /// late and cannot override `RESTART_COMMITTED`.
    #[test]
    fn restart_commit_wins_before_exit_claim() {
        let state = AtomicU8::new(ARBITER_NONE);

        // Thread A claims restart and commits before Thread B acts
        assert!(try_claim_restart_on(&state));
        assert!(try_commit_on(&state, ShutdownAction::Restart));
        assert_eq!(state.load(Ordering::Acquire), ARBITER_RESTART_COMMITTED);

        // Thread B tries to claim exit — rejected (already committed)
        assert!(!try_claim_exit_on(&state));
        assert_eq!(state.load(Ordering::Acquire), ARBITER_RESTART_COMMITTED);
    }

    /// Exactly one of multiple concurrent Exit callers can override
    /// `RESTART_PENDING`.  Uses a barrier so both threads race to the CAS.
    #[test]
    fn concurrent_exit_override_restart() {
        use std::sync::Barrier;

        let state = AtomicU8::new(ARBITER_NONE);
        assert!(try_claim_restart_on(&state));

        let winner_count = AtomicUsize::new(0);
        let barrier = Barrier::new(3);

        std::thread::scope(|s| {
            for _ in 0..2 {
                s.spawn(|| {
                    barrier.wait();
                    if try_claim_exit_on(&state) {
                        winner_count.fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
            barrier.wait(); // sync main thread too
        });

        assert_eq!(winner_count.load(Ordering::Relaxed), 1);
        assert_eq!(state.load(Ordering::Acquire), ARBITER_EXIT_PENDING);
    }

    /// Failed Restart dispatch release cannot clear `EXIT_PENDING` after
    /// Exit already overrode `RESTART_PENDING`.
    #[test]
    fn failed_restart_dispatch_cannot_clear_exit_pending() {
        let state = AtomicU8::new(ARBITER_NONE);

        assert!(try_claim_restart_on(&state));
        assert!(try_claim_exit_on(&state)); // overrides → EXIT_PENDING

        // Release restart — should be no-op (state is EXIT_PENDING, not
        // RESTART_PENDING)
        release_claim_on(&state, ShutdownAction::Restart);
        assert_eq!(state.load(Ordering::Acquire), ARBITER_EXIT_PENDING);
        assert!(is_current_on(&state, ShutdownAction::Exit));
    }

    /// Failed Exit dispatch release cannot clear `NATURAL_EXIT`.
    #[test]
    fn failed_exit_release_cannot_clear_natural_exit() {
        // Simulate NATURAL_EXIT being set by establish_exit_precedence
        let state = AtomicU8::new(ARBITER_NATURAL_EXIT);

        // Release exit — should be no-op (state is NATURAL_EXIT, not
        // EXIT_PENDING)
        release_claim_on(&state, ShutdownAction::Exit);
        assert_eq!(state.load(Ordering::Acquire), ARBITER_NATURAL_EXIT);
    }

    /// `NATURAL_EXIT` suppresses both pending Restart and pending Exit:
    /// `is_current` returns false, commit fails, new claims rejected.
    #[test]
    fn natural_exit_suppresses_pending_actions() {
        let state = AtomicU8::new(ARBITER_NONE);

        // Both restart and exit can be claimed initially
        assert!(try_claim_restart_on(&state));
        assert!(is_current_on(&state, ShutdownAction::Restart));

        // NATURAL_EXIT suppresses everything (simulating
        // establish_exit_precedence)
        assert!(
            state
                .compare_exchange(
                    ARBITER_RESTART_PENDING,
                    ARBITER_NATURAL_EXIT,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
        );

        assert!(!is_current_on(&state, ShutdownAction::Restart));
        assert!(!is_current_on(&state, ShutdownAction::Exit));
        assert!(!try_claim_restart_on(&state));
        assert!(!try_claim_exit_on(&state));
        assert!(!try_commit_on(&state, ShutdownAction::Restart));
        assert!(!try_commit_on(&state, ShutdownAction::Exit));
    }

    /// Duplicate exit and restart requests are rejected.
    #[test]
    fn duplicate_requests_rejected() {
        let state = AtomicU8::new(ARBITER_NONE);

        // Exit: first succeeds, second rejected
        assert!(try_claim_exit_on(&state));
        assert!(!try_claim_exit_on(&state));

        // Reset for restart test
        let state2 = AtomicU8::new(ARBITER_NONE);
        assert!(try_claim_restart_on(&state2));
        assert!(!try_claim_restart_on(&state2));
    }

    /// Commit fails when `NATURAL_EXIT` has been established (e.g.
    /// `RunEvent::Exit` ran before a queued closure could commit).
    #[test]
    fn commit_fails_after_natural_exit() {
        let state = AtomicU8::new(ARBITER_NONE);

        assert!(try_claim_restart_on(&state));
        // Simulate establish_exit_precedence
        assert!(
            state
                .compare_exchange(
                    ARBITER_RESTART_PENDING,
                    ARBITER_NATURAL_EXIT,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
        );

        // The queued restart closure cannot commit
        assert!(!try_commit_on(&state, ShutdownAction::Restart));
        assert_eq!(state.load(Ordering::Acquire), ARBITER_NATURAL_EXIT);
    }
}
