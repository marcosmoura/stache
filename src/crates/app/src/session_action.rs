//! The generic host session action: Pause or Resume of one module, with the
//! actual result.
//!
//! Session pause/resume already exists host-side ([`ModuleControl::pause`],
//! [`ModuleControl::resume`], the tray rows). This file adds the one typed,
//! result-returning entry point the Bar click uses; it does **not** add a second
//! lifecycle path. The host runner thread executes a [`SessionJob`] in its normal
//! command order, so it is serialized with tray pauses, configuration reloads and
//! shutdown, and it calls the same `ModuleControl` the tray calls.
//!
//! # The stale-click rule
//!
//! A request names the action the caller's observed state implies: `pause` for a
//! module the caller saw running (`paused: false`), `resume` for one it saw paused.
//! The host never trusts the colour of the indicator and never inverts the
//! request. At execution it rechecks the **current** host session metadata:
//!
//! | request  | current state                                | outcome                       |
//! | -------- | -------------------------------------------- | ----------------------------- |
//! | any      | module absent, or not enabled                | `disabled` (inert, no write)  |
//! | `pause`  | already paused                               | `stale`                       |
//! | `pause`  | running (locked or unlocked, acquiring...)   | pause, then `applied`         |
//! | `pause`  | release pending (ambiguous cleanup)          | `unavailable{releasePending}` |
//! | `resume` | not paused                                   | `stale`                       |
//! | `resume` | paused and enabled                           | resume, then `applied`        |
//! | any      | failed, unavailable, waiting for permission  | `unavailable{..}`             |
//!
//! A duplicate click, a second window or a tray pause racing the click therefore
//! cannot flip a newer decision: the loser sees `stale` and the Bar redraws from
//! the next `BarChanged`. Nothing here enables, disables, retries, writes
//! configuration or acquires anything.
//!
//! # Bounds
//!
//! At most one request is in flight per host handle (a second one is `busy`,
//! never queued), every wait is bounded, and a shutdown wakes the waiter: a
//! request still queued is cancelled (`unavailable{shuttingDown}`), one the runner
//! already started is `uncertain`.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use specta::Type;
use stache_keep_awake::{AssertionState, KeepAwakeRuntimeStatus};
use stache_sdk::{ModuleStatus, Registry, RegistryError, StateReading};

use crate::bar_keep_awake::KEEP_AWAKE;
use crate::host::ModuleControl;

/// The longest a request waits for the host runner.
pub const SESSION_WAIT: Duration = Duration::from_secs(15);

/// How often a waiting request rechecks that the host is still running.
const QUANTUM: Duration = Duration::from_millis(20);

/// A session action. Closed: anything else cannot even deserialize.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum SessionAction {
    /// Pauses a module that is not paused.
    Pause,
    /// Resumes a paused, enabled module.
    Resume,
}

/// Why the host tried an action and it did not take effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum SessionNotApplied {
    /// The lifecycle refused or failed the transition (for example a stop that
    /// did not complete). The module keeps whatever state it reached.
    Refused,
    /// The transition returned success but the published session state does not
    /// show it.
    NotReflected,
}

/// Why the host did not attempt an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum SessionUnavailable {
    /// The module is failed; there is no retry here.
    Failed,
    /// A prerequisite is missing.
    Unavailable,
    /// The module waits for a permission.
    WaitingForPermission,
    /// A release is pending or ambiguous; nothing is retried or acquired.
    ReleasePending,
    /// The host is shutting down.
    ShuttingDown,
    /// The Bar or the host handle is not running.
    NotRunning,
}

/// The typed result of a session action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SessionActionOutcome {
    /// The host applied the action and the published session state shows it.
    Applied,
    /// The host tried and the action did not take effect.
    NotApplied {
        /// Why.
        reason: SessionNotApplied,
    },
    /// The host state no longer matches what the request was based on.
    Stale,
    /// The module is disabled or absent; the click is inert.
    Disabled,
    /// The host did not attempt the action.
    Unavailable {
        /// Why.
        reason: SessionUnavailable,
    },
    /// Another request is in flight; this one was dropped, not queued.
    Busy,
    /// The host started the request but did not confirm it (shutdown, timeout).
    Uncertain,
    /// The calling window may not request this action.
    Unauthorized,
}

/// Whether the Bar capability may request `action` on `module`: Pause and
/// Resume of `keepAwake` only.
#[must_use]
pub fn bar_may(module: &str, action: SessionAction) -> bool {
    module == KEEP_AWAKE.as_str() && matches!(action, SessionAction::Pause | SessionAction::Resume)
}

/// What an authorized caller asks the host to do.
pub trait SessionControl: Send + Sync {
    /// Runs `action` on `module` and returns what actually happened. Blocks for
    /// at most [`SESSION_WAIT`], so it must not run on the main thread.
    fn session_action(&self, module: &str, action: SessionAction) -> SessionActionOutcome;
}

fn release_pending(registry: &Registry, module: &str) -> bool {
    if module != KEEP_AWAKE.as_str() {
        return false;
    }
    match registry.observe_state(KEEP_AWAKE).read().state {
        StateReading::Available(envelope) => envelope
            .decode::<KeepAwakeRuntimeStatus>(KEEP_AWAKE)
            .is_ok_and(|status| status.assertion_state == AssertionState::ReleasePending),
        _ => false,
    }
}

fn snapshot_of(registry: &Registry, module: &str) -> Option<stache_sdk::ModuleSnapshot> {
    registry.snapshots().into_iter().find(|snapshot| snapshot.id.as_str() == module)
}

/// Decides and runs one request against the registry, on the host runner.
///
/// Pure of any UI concern: it reads the current published session metadata,
/// compares it with the request (see the module docs) and calls the module's
/// existing `ModuleControl`.
#[must_use]
pub fn execute(
    registry: &Registry,
    controls: &BTreeMap<String, Arc<dyn ModuleControl>>,
    module: &str,
    action: SessionAction,
) -> SessionActionOutcome {
    use SessionActionOutcome::{Applied, Disabled, NotApplied, Stale, Unavailable};

    let (Some(snapshot), Some(control)) = (snapshot_of(registry, module), controls.get(module))
    else {
        return Disabled;
    };
    if !snapshot.enabled || snapshot.status == ModuleStatus::Disabled {
        return Disabled;
    }
    let blocked = match &snapshot.status {
        ModuleStatus::Failed(_) => Some(SessionUnavailable::Failed),
        ModuleStatus::Unavailable(_) => Some(SessionUnavailable::Unavailable),
        ModuleStatus::WaitingForPermission(_) => Some(SessionUnavailable::WaitingForPermission),
        ModuleStatus::Running
        | ModuleStatus::Stopped
        | ModuleStatus::Paused
        | ModuleStatus::Disabled => None,
    };
    let wants_paused = action == SessionAction::Pause;
    // The request was based on the opposite session state: the host moved on.
    if snapshot.paused == wants_paused {
        return Stale;
    }
    if let Some(reason) = blocked {
        return Unavailable { reason };
    }
    if wants_paused && release_pending(registry, module) {
        return Unavailable {
            reason: SessionUnavailable::ReleasePending,
        };
    }
    let result = match action {
        SessionAction::Pause => control.pause(registry),
        SessionAction::Resume => control.resume(registry),
    };
    match result {
        Err(RegistryError::ShuttingDown) => Unavailable {
            reason: SessionUnavailable::ShuttingDown,
        },
        Err(_) => NotApplied {
            reason: SessionNotApplied::Refused,
        },
        Ok(()) => match snapshot_of(registry, module) {
            Some(after) if after.paused == wants_paused => Applied,
            _ => NotApplied {
                reason: SessionNotApplied::NotReflected,
            },
        },
    }
}

#[derive(Clone, Copy, Debug)]
enum JobState {
    Queued,
    Running,
    Finished(SessionActionOutcome),
    Cancelled,
}

/// One request travelling to the host runner and back.
pub struct SessionJob {
    /// The module the request names.
    pub module: String,
    /// The requested action.
    pub action: SessionAction,
    state: Mutex<JobState>,
    changed: Condvar,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl SessionJob {
    /// A queued job.
    #[must_use]
    pub fn new(module: &str, action: SessionAction) -> Arc<Self> {
        Arc::new(Self {
            module: module.to_owned(),
            action,
            state: Mutex::new(JobState::Queued),
            changed: Condvar::new(),
        })
    }

    /// The runner claims the job. `false` when the waiter already cancelled it.
    pub fn begin(&self) -> bool {
        let mut state = lock(&self.state);
        if matches!(*state, JobState::Queued) {
            *state = JobState::Running;
            true
        } else {
            false
        }
    }

    /// The runner publishes the result and wakes the waiter.
    pub fn finish(&self, outcome: SessionActionOutcome) {
        *lock(&self.state) = JobState::Finished(outcome);
        self.changed.notify_all();
    }

    /// Waits for the runner, at most `bound`. `abandoned` reports that the host
    /// stopped (shutdown requested or runner gone); it is polled at a coarse
    /// quantum only while a click is waiting, never while idle.
    pub fn wait(&self, bound: Duration, abandoned: impl Fn() -> bool) -> SessionActionOutcome {
        let deadline = Instant::now().checked_add(bound);
        let mut state = lock(&self.state);
        loop {
            match *state {
                JobState::Finished(outcome) => return outcome,
                JobState::Cancelled => {
                    return SessionActionOutcome::Unavailable {
                        reason: SessionUnavailable::ShuttingDown,
                    };
                }
                JobState::Queued | JobState::Running => {}
            }
            let stopped = abandoned();
            let expired = deadline.is_some_and(|deadline| Instant::now() >= deadline);
            if stopped || expired {
                return if matches!(*state, JobState::Queued) {
                    *state = JobState::Cancelled;
                    if stopped {
                        SessionActionOutcome::Unavailable {
                            reason: SessionUnavailable::ShuttingDown,
                        }
                    } else {
                        SessionActionOutcome::Busy
                    }
                } else {
                    SessionActionOutcome::Uncertain
                };
            }
            state = self
                .changed
                .wait_timeout(state, QUANTUM)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

#[cfg(test)]
#[path = "session_action_tests.rs"]
mod tests;
