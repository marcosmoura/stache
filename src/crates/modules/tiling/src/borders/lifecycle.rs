//! The stop-time ledger of the native borders.
//!
//! The border engine lives on the actor thread, the module's stop runs on
//! another one, and the main thread is the only one that can close an overlay.
//! [`BorderLifecycle`] is the small shared object that joins the three without a
//! thread ever waiting for one it must not wait for:
//!
//! 1. the **actor** (when the borders are disabled by a reload, when it ends, or
//!    when the engine panicked) stops the live-follow owner and the Spaces owner,
//!    retires the engine's handle ledger *without sending anything*, asks the
//!    control to `release_all` (a main-thread job it only enqueues) and hands the
//!    ticket and the expected handle count to [`BorderLifecycle::request_release`];
//! 2. the **stop** ([`BorderLifecycle::settle`], always off the main thread) waits
//!    for the actor's hand-over, retries owners that were stuck, waits for the
//!    release ticket within [`BORDERS_RELEASE_WAIT`] and compares;
//! 3. the **main thread** only runs the release job.
//!
//! # The comparison
//!
//! `expected` is the number of handles the engine's ledger accounts for: every
//! entry holding an [`OverlayHandle`](stache_macos::border_overlay::OverlayHandle)
//! plus the `Release` commands it could not send because the route was already
//! closed. The acknowledged number is
//! [`ReleaseAck::handles`](stache_macos::border_overlay::ReleaseAck::handles): the
//! handles the control held when the release was requested (shown, hidden and
//! kept, or pending, minus those whose `Release` was already queued). They must
//! be equal. [`ReleaseAck::closed`](stache_macos::border_overlay::ReleaseAck::closed)
//! also counts the pooled, ordered-out panels, which are in no ledger, so it is
//! reported and never compared.
//!
//! # What is owed
//!
//! A missing acknowledgement, a mismatch, an owner that did not stop or an actor
//! that did not hand over leaves the stage owed; the exact owners and the
//! release state stay here and a later [`BorderLifecycle::settle`] retries only
//! that. A *missing acknowledgement during process exit* is the one case the
//! module may report differently (see `module.rs`): the window server removes the
//! windows of an exiting process, so the stage is then not a leak.

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use stache_macos::border_overlay::{
    AckError, LiveFollow, OverlayControl, OverlayTicket, ReleaseAck, STOP_BUDGET, SpaceEvents,
    StopOutcome,
};

use super::engine::{BORDER_ACK_WAIT, MAIN_BUSY_DIAGNOSTIC};
/// The longest a stop waits for the main thread to acknowledge `release_all`.
/// Only ever waited for off the main thread.
pub use crate::tracking::BORDERS_RELEASE_WAIT;

/// The longest a stop waits for the actor to stop the owners and hand over: two
/// owner stops, one bounded stop each.
pub const OWNERS_STOP_WAIT: Duration =
    Duration::from_millis(2 * STOP_BUDGET.as_secs() * 1000 + 250);

/// An owner whose stop timed out, kept (never dropped while stuck, never
/// detached) until a later stop succeeds.
pub enum RetainedOwner {
    /// The live-follow owner thread and its clocks.
    Live(Box<dyn LiveFollow>),
    /// The Spaces owner thread.
    Spaces(Box<dyn SpaceEvents>),
}

impl RetainedOwner {
    /// A stable name for diagnostics.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Live(_) => "live follow owner",
            Self::Spaces(_) => "spaces owner",
        }
    }

    fn stop(&mut self) -> StopOutcome {
        match self {
            Self::Live(source) => source.stop(),
            Self::Spaces(source) => source.stop(),
        }
    }
}

/// Where the release of the overlays stands.
enum Release {
    /// Nothing to release (or already acknowledged and equal).
    Idle,
    /// Requested; the main thread has not answered yet.
    Waiting {
        /// The ledger count to compare with; `None` after a past mismatch (any
        /// acknowledged release settles it).
        expected: Option<usize>,
        ticket: OverlayTicket<ReleaseAck>,
        issued: Instant,
    },
    /// The job never ran (dispatch refused or the job was dropped): ask again.
    Again { expected: Option<usize> },
}

struct Life {
    /// A `BorderSide` exists and will hand over.
    attached: bool,
    /// The actor handed over for its final stop.
    finished: bool,
    retained: Vec<RetainedOwner>,
    release: Release,
    /// The last acknowledgement disagreed with the ledger: `(expected, acked)`.
    mismatch: Option<(usize, usize)>,
}

/// One reason a stage is owed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owe {
    /// The actor did not stop the owners and hand over in time.
    ActorSilent,
    /// An owner did not stop; it is retained.
    Owner(&'static str),
    /// The main thread did not acknowledge `release_all` in time.
    NoAck,
    /// The release job never ran.
    ReleaseFailed(AckError),
    /// The acknowledgement disagreed with the ledger.
    Mismatch {
        /// Handles the engine's ledger accounted for.
        expected: usize,
        /// Handles the control held.
        acked: usize,
    },
}

/// What a stop of the borders achieved.
#[derive(Debug, PartialEq, Eq)]
pub struct Settle {
    /// Everything still owed; empty is clean.
    pub owed: Vec<Owe>,
}

impl Settle {
    /// Nothing is owed.
    #[must_use]
    pub const fn is_clean(&self) -> bool { self.owed.is_empty() }

    /// Whether the only thing owed is the main thread's acknowledgement of a
    /// release (missing, or its job could not be queued): the one case a process
    /// exit settles, because the window server removes the exiting process's
    /// windows. An owner, a mismatch or a silent actor is never settled by exit.
    #[must_use]
    pub fn only_unacknowledged(&self) -> bool {
        !self.owed.is_empty()
            && self.owed.iter().all(|owe| matches!(owe, Owe::NoAck | Owe::ReleaseFailed(_)))
    }

    /// One sentence per owed item.
    #[must_use]
    pub fn describe(&self) -> String {
        let parts: Vec<String> = self
            .owed
            .iter()
            .map(|owe| match owe {
                Owe::ActorSilent => "the border engine did not stop its owners in time".to_owned(),
                Owe::Owner(name) => format!("the {name} did not stop"),
                Owe::NoAck => format!(
                    "the main thread did not acknowledge the overlay release within {} ms",
                    BORDERS_RELEASE_WAIT.as_millis()
                ),
                Owe::ReleaseFailed(error) => {
                    format!("the overlay release could not run ({error:?})")
                }
                Owe::Mismatch { expected, acked } => format!(
                    "the overlay release acknowledged {acked} handle(s) but the engine \
                     accounted for {expected}"
                ),
            })
            .collect();
        parts.join("; ")
    }
}

/// What the actor learns from polling a pending release.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Polled {
    /// Diagnostics to publish.
    pub notes: Vec<String>,
    /// When to poll again: the instant the acknowledgement becomes late.
    pub deadline: Option<Instant>,
}

/// Shared between the actor, the module's stop and the status surface.
pub struct BorderLifecycle {
    life: Mutex<Life>,
    changed: Condvar,
    /// Debug evidence for the native run (silent until the runtime sets it).
    log: super::blog::LogSlot,
}

impl Default for BorderLifecycle {
    fn default() -> Self { Self::new() }
}

fn lock(mutex: &Mutex<Life>) -> MutexGuard<'_, Life> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl BorderLifecycle {
    /// A lifecycle with no actor attached: it settles as clean at once.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            life: Mutex::new(Life {
                attached: false,
                finished: false,
                retained: Vec::new(),
                release: Release::Idle,
                mismatch: None,
            }),
            changed: Condvar::new(),
            log: super::blog::LogSlot::new(),
        }
    }

    /// The debug log of the stop path.
    #[must_use]
    pub const fn log(&self) -> &super::blog::LogSlot { &self.log }

    /// Registers the actor-side half; [`Self::settle`] then waits for its final
    /// hand-over.
    pub fn attach(&self) { lock(&self.life).attached = true; }

    /// Records a release the actor requested and will not wait for.
    pub fn request_release(
        &self,
        expected: usize,
        ticket: OverlayTicket<ReleaseAck>,
        issued: Instant,
    ) {
        self.log.debug(|| format!("release_all enqueued expected_handles={expected}"));
        lock(&self.life).release = Release::Waiting {
            expected: Some(expected),
            ticket,
            issued,
        };
    }

    /// The actor's final hand-over: its owners that did not stop are kept here.
    pub fn finish(&self, retained: Vec<RetainedOwner>) {
        let mut life = lock(&self.life);
        life.retained.extend(retained);
        life.finished = true;
        drop(life);
        self.changed.notify_all();
    }

    /// Whether the actor handed over for the last time.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        let life = lock(&self.life);
        !life.attached || life.finished
    }

    /// Looks at a pending release without waiting (the actor, every turn it is
    /// woken): an acknowledgement is compared, a late one is reported as a busy
    /// main thread, a mismatch is a diagnostic until a stop retries it.
    pub fn poll(&self, now: Instant) -> Polled {
        let mut life = lock(&self.life);
        let mut polled = Polled::default();
        if let Release::Waiting { expected, ticket, issued } = &life.release {
            let (expected, issued) = (*expected, *issued);
            match ticket.try_get() {
                Some(Ok(ack)) => {
                    life.release = Release::Idle;
                    self.log.debug(|| {
                        format!(
                            "release_all acknowledged (actor poll) handles={} closed={} \
                             expected={expected:?} elapsed_ms={:.1}",
                            ack.handles,
                            ack.closed,
                            now.saturating_duration_since(issued).as_secs_f64() * 1000.0
                        )
                    });
                    match expected {
                        Some(expected) if expected != ack.handles => {
                            life.mismatch = Some((expected, ack.handles));
                        }
                        _ => life.mismatch = None,
                    }
                }
                Some(Err(_)) => life.release = Release::Again { expected },
                None => {
                    let late = issued + BORDER_ACK_WAIT;
                    if now >= late {
                        polled.notes.push(MAIN_BUSY_DIAGNOSTIC.to_owned());
                        // Look again so the note clears once main catches up.
                        polled.deadline = Some(now + BORDER_ACK_WAIT);
                    } else {
                        polled.deadline = Some(late);
                    }
                }
            }
        }
        let mismatch = life.mismatch;
        drop(life);
        if let Some((expected, acked)) = mismatch {
            polled.notes.push(format!(
                "borders: released {acked} overlay handle(s) but the engine accounted for \
                 {expected}"
            ));
        }
        polled
    }

    /// Stops the borders for good from a thread that may wait (never main):
    /// waits for the actor's hand-over within `actor_wait`, retries retained
    /// owners and waits for the release acknowledgement within `ack_wait`.
    pub fn settle(
        &self,
        control: &OverlayControl,
        actor_wait: Duration,
        ack_wait: Duration,
    ) -> Settle {
        let mut owed = Vec::new();
        // 1. The actor's final hand-over.
        let life = lock(&self.life);
        let waiting = if life.attached && !life.finished {
            let (life, _) = self
                .changed
                .wait_timeout_while(life, actor_wait, |life| !life.finished)
                .unwrap_or_else(PoisonError::into_inner);
            let silent = !life.finished;
            drop(life);
            silent
        } else {
            drop(life);
            false
        };
        if waiting {
            return Settle { owed: vec![Owe::ActorSilent] };
        }
        // 2. Owners that did not stop: retry them without the lock (a stop joins).
        let mut retained = std::mem::take(&mut lock(&self.life).retained);
        retained.retain_mut(|owner| {
            let outcome = owner.stop();
            self.log
                .debug(|| format!("retained {} stop retry outcome={outcome:?}", owner.name()));
            let stuck = matches!(outcome, StopOutcome::TimedOut { .. });
            if stuck {
                owed.push(Owe::Owner(owner.name()));
            }
            stuck
        });
        lock(&self.life).retained.extend(retained);
        // 3. The release.
        let (release, mismatch) = {
            let mut life = lock(&self.life);
            (
                std::mem::replace(&mut life.release, Release::Idle),
                life.mismatch,
            )
        };
        let pending = match release {
            Release::Waiting { expected, ticket, issued } => Some((expected, ticket, Some(issued))),
            Release::Again { expected } => Some((expected, control.release_all(), None)),
            // A past mismatch is settled by a release that is acknowledged: the
            // control then holds nothing and every panel it owned is closed.
            Release::Idle if mismatch.is_some() => Some((None, control.release_all(), None)),
            Release::Idle => None,
        };
        if let Some((expected, ticket, issued)) = pending {
            let waited = Instant::now();
            let result = ticket.wait(ack_wait);
            self.log.debug(|| {
                let since_enqueue = issued.map(|at| at.elapsed().as_secs_f64() * 1000.0);
                match &result {
                    Ok(ack) => format!(
                        "release_all acknowledged (settle) handles={} closed={} \
                         expected={expected:?} waited_ms={:.1} since_enqueue_ms={since_enqueue:?}",
                        ack.handles,
                        ack.closed,
                        waited.elapsed().as_secs_f64() * 1000.0
                    ),
                    Err(error) => format!(
                        "release_all not acknowledged (settle) error={error:?} \
                         waited_ms={:.1} since_enqueue_ms={since_enqueue:?}",
                        waited.elapsed().as_secs_f64() * 1000.0
                    ),
                }
            });
            match result {
                Ok(ack) => match expected {
                    Some(expected) if expected != ack.handles => {
                        let mismatch = Owe::Mismatch { expected, acked: ack.handles };
                        lock(&self.life).mismatch = Some((expected, ack.handles));
                        owed.push(mismatch);
                    }
                    _ => lock(&self.life).mismatch = None,
                },
                Err(AckError::TimedOut | AckError::WouldBlockMain) => {
                    // Still queued: the same ticket is waited for again by a retry.
                    let issued = issued.unwrap_or_else(Instant::now);
                    lock(&self.life).release = Release::Waiting { expected, ticket, issued };
                    owed.push(Owe::NoAck);
                }
                Err(error) => {
                    lock(&self.life).release = Release::Again { expected };
                    owed.push(Owe::ReleaseFailed(error));
                }
            }
        }
        Settle { owed }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use stache_macos::border_overlay::testing::FakeOverlayBackend;
    use stache_macos::border_overlay::{OverlayBackend, OverlayCapabilities};
    use stache_sdk::{MainThreadExecutor, ThreadError};

    use super::*;

    /// "Main" while the application terminates: the caller is on the main
    /// thread and the queue is not run.
    struct BusyMain {
        queued: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
    }

    impl MainThreadExecutor for BusyMain {
        fn is_main_thread(&self) -> bool { true }

        fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
            self.queued.lock().unwrap_or_else(PoisonError::into_inner).push(job);
            Ok(())
        }
    }

    fn control(executor: Arc<dyn MainThreadExecutor>) -> OverlayControl {
        OverlayControl::new(
            executor,
            Arc::new(FakeOverlayBackend::single_display()) as Arc<dyn OverlayBackend>,
            OverlayCapabilities::default(),
        )
    }

    /// Stop never waits on main from main: it reports the missing
    /// acknowledgement at once instead of blocking, and keeps the very ticket.
    #[test]
    fn settling_on_the_main_thread_never_blocks_and_keeps_the_ticket() {
        let main = Arc::new(BusyMain { queued: Mutex::new(Vec::new()) });
        let control = control(Arc::clone(&main) as Arc<dyn MainThreadExecutor>);
        let life = BorderLifecycle::new();
        life.attach();
        let now = Instant::now();
        life.request_release(0, control.release_all(), now);
        life.finish(Vec::new());
        let started = Instant::now();
        let settle = life.settle(&control, Duration::from_secs(30), Duration::from_secs(30));
        assert!(started.elapsed() < Duration::from_secs(5), "did not wait");
        assert_eq!(settle, Settle { owed: vec![Owe::NoAck] });
        assert!(settle.only_unacknowledged());
        // Main finally runs the job: the retry waits for that same ticket.
        let jobs = std::mem::take(&mut *main.queued.lock().unwrap_or_else(PoisonError::into_inner));
        assert_eq!(jobs.len(), 1, "exactly one release job exists");
        for job in jobs {
            job();
        }
        let retry = life.settle(&control, Duration::ZERO, Duration::ZERO);
        assert!(retry.is_clean(), "{retry:?}");
    }

    #[test]
    fn an_actor_that_never_hands_over_is_owed_and_never_settled_by_exit() {
        let control = control(Arc::new(BusyMain { queued: Mutex::new(Vec::new()) }));
        let life = BorderLifecycle::new();
        life.attach();
        let settle = life.settle(&control, Duration::from_millis(5), Duration::from_millis(5));
        assert_eq!(settle, Settle { owed: vec![Owe::ActorSilent] });
        assert!(!settle.only_unacknowledged());
    }

    /// Status wording of every lifecycle diagnostic, as the status surface shows
    /// it.
    #[test]
    fn the_lifecycle_diagnostics_read_as_documented() {
        let main = Arc::new(BusyMain { queued: Mutex::new(Vec::new()) });
        let control = control(Arc::clone(&main) as Arc<dyn MainThreadExecutor>);
        let life = BorderLifecycle::new();
        let issued = Instant::now();
        life.request_release(3, control.release_all(), issued);
        // Not late yet: no note, a deadline at the moment it becomes late.
        let early = life.poll(issued + Duration::from_millis(10));
        assert_eq!(early.notes, Vec::<String>::new());
        assert_eq!(early.deadline, Some(issued + BORDER_ACK_WAIT));
        // Late: the one busy note, polled again later.
        let late = life.poll(issued + BORDER_ACK_WAIT);
        assert_eq!(late.notes, vec!["borders: main thread busy".to_owned()]);
        // The job ran with a different handle count than the ledger.
        for job in std::mem::take(&mut *main.queued.lock().unwrap_or_else(PoisonError::into_inner))
        {
            job();
        }
        let mismatched = life.poll(issued + Duration::from_secs(1));
        assert_eq!(mismatched.notes, vec![
            "borders: released 0 overlay handle(s) but the engine accounted for 3".to_owned()
        ]);
        assert_eq!(
            Settle {
                owed: vec![Owe::NoAck, Owe::Owner("spaces owner"), Owe::Mismatch {
                    expected: 3,
                    acked: 0
                },]
            }
            .describe(),
            "the main thread did not acknowledge the overlay release within 500 ms; the spaces \
             owner did not stop; the overlay release acknowledged 0 handle(s) but the engine \
             accounted for 3"
        );
    }
}
