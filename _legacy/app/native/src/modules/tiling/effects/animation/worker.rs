//! Serialized native animation worker.
//!
//! The worker owns animation execution so actor and subscriber tasks never
//! wait for frame pacing or AX writes. Its one-slot mailbox replaces stale
//! desired work rather than accumulating animation jobs.

use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::{AnimationSystem, WindowTransition, begin_animation, cancel_animation};
use crate::modules::tiling::identity::WindowTarget;
use crate::modules::tiling::state::Rect;

/// Exact final frames confirmed by a completed animation job.
#[derive(Debug, Default)]
pub struct AnimationOutcome {
    /// Layout generation that submitted this work. A frame value can recur
    /// (A -> B -> A), so target/frame alone is not an acknowledgement.
    pub layout_revision: Option<(uuid::Uuid, u64)>,
    /// Every final target the worker attempted, including unresolved, failed,
    /// and cancelled targets.
    pub attempted: Vec<(WindowTarget, Rect)>,

    /// Targets whose intermediate and final native writes all succeeded.
    pub successful: Vec<(WindowTarget, Rect)>,
}

/// Receives one outcome when a submitted animation job stops.
pub type OutcomeReporter = std::sync::Arc<dyn Fn(AnimationOutcome) + Send + Sync>;

struct AnimationJob {
    transitions: Vec<WindowTransition>,
    attempted: Vec<(WindowTarget, Rect)>,
    reporter: Option<OutcomeReporter>,
    layout_revision: Option<(uuid::Uuid, u64)>,
}

#[derive(Default)]
struct WorkerState {
    pending: Option<AnimationJob>,
    paused: bool,
    running: bool,
}

struct AnimationWorker {
    state: Mutex<WorkerState>,
    changed: Condvar,
}

static WORKER: OnceLock<Arc<AnimationWorker>> = OnceLock::new();

fn worker() -> &'static Arc<AnimationWorker> {
    WORKER.get_or_init(|| {
        let worker = Arc::new(AnimationWorker {
            state: Mutex::new(WorkerState::default()),
            changed: Condvar::new(),
        });
        let runner = Arc::clone(&worker);
        std::thread::Builder::new()
            .name("stache-window-animation".to_string())
            .spawn(move || run(&runner))
            .expect("failed to start window animation worker");
        worker
    })
}

fn run(worker: &AnimationWorker) {
    loop {
        let job = {
            let mut state = worker.state.lock().expect("animation worker state poisoned");
            while state.paused || state.pending.is_none() {
                state = worker.changed.wait(state).expect("animation worker state poisoned");
            }
            state.running = true;
            // Cancellation and job selection share this mutex. A submit or
            // pause that follows can only increment cancellation after this
            // generation is active, so it cannot be cleared by this job.
            begin_animation();
            state.pending.take().expect("checked above")
        };

        let mut outcome = AnimationSystem::from_config().animate_with_outcome(job.transitions);
        outcome.attempted = job.attempted;
        outcome.layout_revision = job.layout_revision;
        if let Some(reporter) = job.reporter {
            reporter(outcome);
        }

        let mut state = worker.state.lock().expect("animation worker state poisoned");
        state.running = false;
        drop(state);
        worker.changed.notify_all();
    }
}

/// Replaces any queued animation and asynchronously starts the newest plan.
///
/// # Panics
///
/// Panics if the worker state mutex is poisoned or the worker cannot start.
pub fn submit(
    transitions: Vec<WindowTransition>,
    attempted: Vec<(WindowTarget, Rect)>,
    reporter: Option<OutcomeReporter>,
    layout_revision: Option<(uuid::Uuid, u64)>,
) {
    if attempted.is_empty() {
        return;
    }
    if transitions.is_empty() {
        if let Some(reporter) = reporter {
            reporter(AnimationOutcome {
                attempted,
                successful: Vec::new(),
                layout_revision,
            });
        }
        return;
    }
    let worker = worker();
    let mut state = worker.state.lock().expect("animation worker state poisoned");
    // Serialize cancellation with the worker's generation activation. This
    // makes either the current job observe cancellation or this replacement
    // become the next active generation, without a cancellation being lost.
    cancel_animation();
    let superseded = state.pending.replace(AnimationJob {
        transitions,
        attempted,
        reporter,
        layout_revision,
    });
    drop(state);
    if let Some(job) = superseded {
        report_cancelled(job);
    }
    worker.changed.notify_one();
}

/// Stops accepting runnable work and waits until the active native loop has
/// observed cancellation and returned. The worker remains alive for resume.
///
/// # Panics
///
/// Panics if the worker state mutex is poisoned or the worker cannot start.
#[must_use]
pub fn pause(timeout: Duration) -> bool {
    let worker = worker();
    let deadline = Instant::now() + timeout;
    let mut state = worker.state.lock().expect("animation worker state poisoned");
    // As in `submit`, keep cancellation and generation selection atomic.
    cancel_animation();
    state.paused = true;
    let cancelled_pending = state.pending.take();
    worker.changed.notify_all();

    drop(state);
    if let Some(job) = cancelled_pending {
        report_cancelled(job);
    }
    let mut state = worker.state.lock().expect("animation worker state poisoned");

    while state.running {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let (next, result) = worker
            .changed
            .wait_timeout(state, remaining)
            .expect("animation worker state poisoned");
        state = next;
        if result.timed_out() && state.running {
            return false;
        }
    }
    true
}

fn report_cancelled(job: AnimationJob) {
    if let Some(reporter) = job.reporter {
        reporter(AnimationOutcome {
            attempted: job.attempted,
            successful: Vec::new(),
            layout_revision: job.layout_revision,
        });
    }
}

/// Allows the durable worker to accept plans for the next runtime generation.
///
/// # Panics
///
/// Panics if the worker state mutex is poisoned or the worker cannot start.
pub fn resume() {
    let worker = worker();
    let mut state = worker.state.lock().expect("animation worker state poisoned");
    state.paused = false;
    drop(state);
    worker.changed.notify_all();
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::modules::tiling::identity::{AppIdentity, LaunchDateBits};

    #[test]
    fn unresolved_targets_report_a_failed_outcome_without_starting_the_worker() {
        let target = WindowTarget {
            identity: AppIdentity {
                pid: 42,
                launch_date: LaunchDateBits::from_time_interval_since_reference_date(1.0).unwrap(),
            },
            window_id: 7,
        };
        let attempted = (target, Rect::new(0.0, 0.0, 100.0, 100.0));
        let received = Arc::new(Mutex::new(None));
        let reporter_received = Arc::clone(&received);
        let reporter: OutcomeReporter = Arc::new(move |outcome| {
            *reporter_received.lock().unwrap() = Some(outcome);
        });

        submit(Vec::new(), vec![attempted], Some(reporter), None);

        let outcome = received.lock().unwrap().take().expect("expected failed outcome");
        assert_eq!(outcome.attempted, vec![attempted]);
        assert!(outcome.successful.is_empty());
    }

    #[test]
    fn superseded_job_reports_a_cancelled_outcome() {
        let target = WindowTarget {
            identity: AppIdentity {
                pid: 42,
                launch_date: LaunchDateBits::from_time_interval_since_reference_date(1.0).unwrap(),
            },
            window_id: 7,
        };
        let attempted = (target, Rect::new(0.0, 0.0, 100.0, 100.0));
        let received = Arc::new(Mutex::new(None));
        let reporter_received = Arc::clone(&received);
        let reporter: OutcomeReporter = Arc::new(move |outcome| {
            *reporter_received.lock().unwrap() = Some(outcome);
        });

        report_cancelled(AnimationJob {
            transitions: Vec::new(),
            attempted: vec![attempted],
            reporter: Some(reporter),
            layout_revision: None,
        });

        let outcome = received.lock().unwrap().take().expect("expected cancelled outcome");
        assert_eq!(outcome.attempted, vec![attempted]);
        assert!(outcome.successful.is_empty());
    }
}
