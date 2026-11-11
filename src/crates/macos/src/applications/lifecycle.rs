//! Application lifecycle notifications: launch, terminate, hide and unhide,
//! plus the application-less active Space change.
//!
//! [`ApplicationObservation::watch_lifecycle`] registers one additive workspace
//! observer set on the same generation as the will-launch observer. The will-launch
//! observation, its sink and `activate_exact` are unchanged.
//!
//! # Events are pure
//!
//! A [`LifecycleEvent`] carries only the [`LifecycleKind`], the exact opaque
//! [`ApplicationId`] and an optional diagnostic PID. The native block interns
//! (or, for termination, looks up) the retained instance on main and defers the
//! pure event to a *later* main turn, never inline: the block can be running
//! inside a native send whose [`super::IssuancePermit`] holds the commit gate.
//! It performs no query, wait or policy. The sink rechecks the generation when
//! the deferred delivery runs and, like every sink here, takes no commit gate.
//!
//! # Identity
//!
//! The id is the retained instance's identity (`isEqual:`), never a PID, bundle
//! or name. A PID of `0` or a negative value (`NSNotFound`/unknown) is reported
//! as `None` and is diagnostic only. A terminated application keeps its id: the
//! next intern prunes it into a small bounded tombstone list that the terminate
//! lookup also checks, so [`LifecycleKind::Terminated`] names the same id a
//! consumer saw earlier even when another application's activation interned
//! first. A termination that still cannot be resolved (never interned, or its
//! tombstone was evicted) is delivered as [`LifecycleKind::TerminatedUnresolved`]
//! with a reserved id, so the consumer reconciles instead of silently keeping
//! the application. A PID reused by a replacement process is a different
//! instance and receives a new id.
//!
//! # Native binding
//!
//! `NSWorkspaceDidLaunchApplicationNotification`,
//! `NSWorkspaceDidTerminateApplicationNotification`,
//! `NSWorkspaceDidHideApplicationNotification` and
//! `NSWorkspaceDidUnhideApplicationNotification` on the workspace notification
//! center, delivered on the main operation queue with the
//! `NSWorkspaceApplicationKey` user-info value, exactly like the existing
//! will-launch observer. `NSWorkspaceActiveSpaceDidChangeNotification` is
//! observed on the same center and queue, in the same generation, and becomes
//! [`LifecycleKind::SpaceChanged`] with a reserved id (it names no
//! application); its block reads nothing and only defers the event. Delivery is native behavior that is **unverified** by
//! the default tests.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::Arc;

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2_app_kit::{
    NSWorkspaceActiveSpaceDidChangeNotification, NSWorkspaceDidHideApplicationNotification,
    NSWorkspaceDidLaunchApplicationNotification, NSWorkspaceDidTerminateApplicationNotification,
    NSWorkspaceDidUnhideApplicationNotification,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSOperationQueue};
use stache_sdk::{CancellationToken, ThreadError};

use super::{
    Ack, AckWait, ApplicationError, ApplicationId, ApplicationObservation, BackendHandle,
    CANCELLATION_QUANTUM, INSTANCES, InstallAck, OBSERVERS, ingestion_is_current,
    launch_application, require_main,
};

/// What happened to an application.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LifecycleKind {
    /// `NSWorkspaceDidLaunchApplicationNotification`.
    Launched,
    /// `NSWorkspaceDidTerminateApplicationNotification`.
    Terminated,
    /// A termination whose instance could not be resolved to an id (it was
    /// never interned, or its bounded tombstone was evicted). Synthesized, not
    /// a native kind, so it is **not** in [`Self::ALL`]; the event's
    /// `application` is a reserved id naming nothing. The consumer must
    /// reconcile its application list instead of keeping the application.
    TerminatedUnresolved,
    /// `NSWorkspaceDidHideApplicationNotification`.
    Hidden,
    /// `NSWorkspaceDidUnhideApplicationNotification`.
    Unhidden,
    /// `NSWorkspaceActiveSpaceDidChangeNotification`: the active Space changed
    /// (including entering or leaving a native fullscreen Space). It names no
    /// application, so it is **not** in [`Self::ALL`]; the event's
    /// `application` is a reserved id naming nothing and `pid` is `None`.
    SpaceChanged,
}

impl LifecycleKind {
    /// Every observed kind.
    pub const ALL: [Self; 4] = [
        Self::Launched,
        Self::Terminated,
        Self::Hidden,
        Self::Unhidden,
    ];
}

/// One pure lifecycle delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LifecycleEvent {
    /// What happened.
    pub kind: LifecycleKind,
    /// The exact instance.
    pub application: ApplicationId,
    /// The process identifier when the system reported a valid one. Diagnostic
    /// only: never identity.
    pub pid: Option<i32>,
}

/// The sink a backend calls (on a later main turn) for each lifecycle event.
pub type LifecycleSink = Arc<dyn Fn(LifecycleEvent) + Send + Sync>;

/// Normalizes a native PID: `0`, negatives and `NSNotFound` truncations are
/// unknown.
#[must_use]
pub const fn diagnostic_pid(raw: i32) -> Option<i32> { if raw > 0 { Some(raw) } else { None } }

/// The pure event for one active Space change of a generation.
pub(super) const fn space_changed(handle: BackendHandle, generation: u64) -> LifecycleEvent {
    LifecycleEvent {
        kind: LifecycleKind::SpaceChanged,
        application: ApplicationId::unresolved(handle, generation),
        pid: None,
    }
}

/// Queues one pure event for a later main turn; a rejected enqueue drops it and
/// never runs it inline (inline delivery could re-enter the commit gate).
pub(super) fn defer_lifecycle(
    enqueue: impl FnOnce(Box<dyn FnOnce() + Send>) -> Result<(), ThreadError>,
    sink: LifecycleSink,
    event: LifecycleEvent,
) {
    let delivery: Box<dyn FnOnce() + Send> = Box::new(move || sink(event));
    let _ = enqueue(delivery);
}

/// Registers the four lifecycle observers for one generation on main.
pub(super) fn observe_system(
    handle: BackendHandle,
    generation: u64,
    sink: &LifecycleSink,
) -> Result<(), ApplicationError> {
    let _main = require_main()?;
    OBSERVERS.with(|cell| {
        let mut observers = cell.borrow_mut();
        let Some(state) = observers.get_mut(&handle) else {
            return Err(ApplicationError::NotInstalled);
        };
        if state.generation != generation {
            return Err(ApplicationError::NotInstalled);
        }
        if !state.lifecycle.is_empty() {
            return Err(ApplicationError::AlreadyInstalled);
        }
        let queue = NSOperationQueue::mainQueue();
        for kind in LifecycleKind::ALL {
            let sink = Arc::clone(sink);
            let block = RcBlock::new(move |notification: NonNull<NSNotification>| {
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    let installed = OBSERVERS
                        .with(|cell| cell.borrow().get(&handle).map(|state| state.generation));
                    if !ingestion_is_current(installed, generation) {
                        return;
                    }
                    // SAFETY: the notification center invokes the block with a
                    // valid notification for the duration of the call.
                    let notification = unsafe { notification.as_ref() };
                    let Some(application) = launch_application(notification) else {
                        return;
                    };
                    let id = INSTANCES.with(|cell| {
                        let mut stores = cell.borrow_mut();
                        let store = stores.get_mut(&handle)?;
                        if store.generation != generation {
                            return None;
                        }
                        match kind {
                            // A terminated instance must keep the id a
                            // consumer already holds: look it up without
                            // pruning it away first.
                            LifecycleKind::Terminated => {
                                Some(store.take_terminated(&application).map_or_else(
                                    || {
                                        (
                                            LifecycleKind::TerminatedUnresolved,
                                            ApplicationId::unresolved(handle, generation),
                                        )
                                    },
                                    |token| {
                                        (LifecycleKind::Terminated, ApplicationId {
                                            handle,
                                            generation,
                                            token,
                                        })
                                    },
                                ))
                            }
                            _ => Some((kind, store.intern(handle, generation, &application))),
                        }
                    });
                    let Some((kind, application_id)) = id else {
                        return;
                    };
                    let event = LifecycleEvent {
                        kind,
                        application: application_id,
                        pid: diagnostic_pid(application.processIdentifier()),
                    };
                    defer_lifecycle(crate::main_thread::run_on_main, Arc::clone(&sink), event);
                }));
            });
            let block: &DynBlock<dyn Fn(NonNull<NSNotification>)> = &block;
            // SAFETY: the notification names are immutable AppKit constants; the
            // observer object is `None`; delivery is explicitly on the main
            // operation queue, and the center retains the block until the token
            // is removed in `unobserve_system`/`unobserve`.
            let token = unsafe {
                let name = match kind {
                    LifecycleKind::Launched => NSWorkspaceDidLaunchApplicationNotification,
                    LifecycleKind::Terminated => NSWorkspaceDidTerminateApplicationNotification,
                    LifecycleKind::Hidden => NSWorkspaceDidHideApplicationNotification,
                    // `ALL` never contains the synthesized kind.
                    LifecycleKind::Unhidden
                    | LifecycleKind::TerminatedUnresolved
                    | LifecycleKind::SpaceChanged => NSWorkspaceDidUnhideApplicationNotification,
                };
                state.center.addObserverForName_object_queue_usingBlock(
                    Some(name),
                    None,
                    Some(&queue),
                    block,
                )
            };
            state.lifecycle.push(token);
        }
        let token = observe_space_change(&state.center, &queue, handle, generation, sink);
        state.lifecycle.push(token);
        Ok(())
    })
}

/// Registers the active Space change observer of one generation: the block
/// reads nothing and only defers the application-less event.
fn observe_space_change(
    center: &NSNotificationCenter,
    queue: &NSOperationQueue,
    handle: BackendHandle,
    generation: u64,
    sink: &LifecycleSink,
) -> Retained<ProtocolObject<dyn NSObjectProtocol>> {
    let sink = Arc::clone(sink);
    let block = RcBlock::new(move |_notification: NonNull<NSNotification>| {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let installed =
                OBSERVERS.with(|cell| cell.borrow().get(&handle).map(|state| state.generation));
            if !ingestion_is_current(installed, generation) {
                return;
            }
            defer_lifecycle(
                crate::main_thread::run_on_main,
                Arc::clone(&sink),
                space_changed(handle, generation),
            );
        }));
    });
    let block: &DynBlock<dyn Fn(NonNull<NSNotification>)> = &block;
    // SAFETY: the notification name is an immutable AppKit constant; the
    // observer object is `None`; delivery is on the given main operation queue,
    // and the caller stores the token with the lifecycle tokens, removed in
    // `unobserve_system`/`unobserve`.
    unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(NSWorkspaceActiveSpaceDidChangeNotification),
            None,
            Some(queue),
            block,
        )
    }
}

/// Removes only the lifecycle observers of one generation. Idempotent; a
/// superseded generation is left untouched.
pub(super) fn unobserve_system(
    handle: BackendHandle,
    generation: u64,
) -> Result<(), ApplicationError> {
    let _main = require_main()?;
    OBSERVERS.with(|cell| {
        let mut observers = cell.borrow_mut();
        let Some(state) = observers.get_mut(&handle) else {
            return;
        };
        if state.generation != generation {
            return;
        }
        for token in state.lifecycle.drain(..) {
            // SAFETY: the token came from this center and has not been removed;
            // removal is idempotent for an installed observer.
            unsafe { state.center.removeObserver(AsRef::<AnyObject>::as_ref(&token)) };
        }
    });
    Ok(())
}

impl ApplicationObservation {
    /// Installs the lifecycle observers (launch, terminate, hide, unhide) for
    /// this generation.
    ///
    /// `sink` receives pure [`LifecycleEvent`]s on a later main turn for as long
    /// as the generation is live; it must only enqueue (no query, wait or
    /// policy). The wait observes cancellation every [`CANCELLATION_QUANTUM`]
    /// and the acknowledgement budget. A failed, cancelled or timed-out install
    /// stays owned by this observation and is removed with it (or by the next
    /// successful attempt), exactly once.
    ///
    /// # Errors
    /// Returns [`ApplicationError::AlreadyInstalled`] while a lifecycle watch
    /// exists, cancellation, a dispatch rejection, a registration failure, a
    /// timeout or [`ApplicationError::Unavailable`] for a backend without
    /// lifecycle support.
    pub fn watch_lifecycle(
        &self,
        cancellation: &CancellationToken,
        sink: impl Fn(LifecycleEvent) + Send + Sync + 'static,
    ) -> Result<(), ApplicationError> {
        if cancellation.is_cancelled() {
            return Err(ApplicationError::Cancelled);
        }
        let generation = self.generation;
        let weak_state = Arc::downgrade(&self.state);
        let wrapped: LifecycleSink = Arc::new(move |event| {
            let Some(state) = weak_state.upgrade() else {
                return;
            };
            // Advisory, gate-free read: this runs on main and must not wait for
            // a stalled native send.
            if !state.is_published(generation) {
                return;
            }
            let _ = catch_unwind(AssertUnwindSafe(|| sink(event)));
        });
        let ack = Arc::new(Ack::new());
        let job_ack = Arc::clone(&ack);
        let job_state = Arc::clone(&self.state);
        let token = cancellation.clone();
        let job = Box::new(move || {
            if token.is_cancelled() || !job_state.is_active(generation) {
                job_ack.complete(InstallAck::Aborted);
                return;
            }
            let result = job_state.backend.observe_lifecycle(job_state.handle, generation, wrapped);
            if result.is_ok() && token.is_cancelled() {
                job_ack.complete(InstallAck::Aborted);
            } else {
                job_ack.complete(InstallAck::Done(result));
            }
        });
        if let Err(error) = self.state.executor.execute(job) {
            return Err(ApplicationError::Dispatch(error.to_string()));
        }
        let outcome = ack.wait_cancellable(self.state.budget, cancellation, CANCELLATION_QUANTUM);
        let failure = match outcome {
            AckWait::Ready(InstallAck::Done(Ok(()))) if !cancellation.is_cancelled() => {
                return Ok(());
            }
            AckWait::Ready(InstallAck::Done(Ok(())) | InstallAck::Aborted) | AckWait::Cancelled => {
                ApplicationError::Cancelled
            }
            AckWait::Ready(InstallAck::Done(Err(error))) => error,
            AckWait::TimedOut => ApplicationError::TimedOut,
        };
        // A registered observer stays owned by this generation: remove it now if
        // possible; `stop`/drop removes it exactly once otherwise. An existing
        // live watch (`AlreadyInstalled`) is left untouched.
        if failure == ApplicationError::AlreadyInstalled {
            return Err(failure);
        }
        let state = Arc::clone(&self.state);
        let _ = self.state.executor.execute(Box::new(move || {
            let _ = state.backend.unobserve_lifecycle(state.handle, generation);
        }));
        Err(failure)
    }
}
