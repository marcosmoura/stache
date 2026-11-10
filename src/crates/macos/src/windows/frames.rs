//! The per-shard latest-frame-wins mailbox.
//!
//! The router and the shard share one `Mutex<FrameMailbox>`. The lock is held
//! only to insert or take: replies, AX calls and wakeups all run after it is
//! released. At most one frame is pending per window.

use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::commands::{FrameAck, FrameReply, FrameReplyFn, FrameRequest, FrameStage};
use super::identity::WindowInstance;
use super::issuance::{DispatchError, NotIssuedReason, WindowActionOutcome};

/// The most frames one shard keeps pending. A batch that would exceed it is
/// refused whole.
pub const FRAME_MAILBOX_BOUND: usize = 256;

/// The most pending frames one shard turn writes before it yields to commands.
pub(super) const MAX_FRAMES_PER_TURN: usize = 32;

/// The callback a frame is answered through.
pub(super) type FrameCallback = FrameReplyFn;

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(super) const fn done(outcome: WindowActionOutcome) -> FrameReply {
    FrameReply::Done(FrameAck { outcome, sent_at: None })
}

/// Guarantees a frame's callback runs exactly once: dropping an unanswered
/// guard answers `NotIssued(OwnerUnavailable)`.
pub(super) struct ReplyGuard(Option<FrameCallback>);

impl ReplyGuard {
    pub(super) fn new(callback: FrameCallback) -> Self { Self(Some(callback)) }

    /// Answers once. A panicking callback is contained.
    pub(super) fn answer(mut self, reply: FrameReply) {
        if let Some(callback) = self.0.take() {
            let _ = catch_unwind(AssertUnwindSafe(|| callback(reply)));
        }
    }
}

impl Drop for ReplyGuard {
    fn drop(&mut self) {
        if let Some(callback) = self.0.take() {
            let reply = done(WindowActionOutcome::NotIssued(NotIssuedReason::OwnerUnavailable));
            let _ = catch_unwind(AssertUnwindSafe(|| callback(reply)));
        }
    }
}

/// One pending frame and its answer.
pub(super) struct PendingFrame {
    pub request: FrameRequest,
    pub reply: ReplyGuard,
}

/// At most one pending frame per window, in arrival order.
#[derive(Default)]
pub(super) struct FrameMailbox {
    pending: HashMap<WindowInstance, PendingFrame>,
    order: VecDeque<WindowInstance>,
    closed: bool,
}

impl FrameMailbox {
    fn insert(
        &mut self,
        request: FrameRequest,
        callback: FrameCallback,
        displaced: &mut Vec<(ReplyGuard, FrameReply)>,
    ) {
        let window = request.issuance.window();
        let incoming = PendingFrame {
            request,
            reply: ReplyGuard::new(callback),
        };
        let superseded = done(WindowActionOutcome::NotIssued(NotIssuedReason::Superseded));
        let Some(held) = self.pending.get_mut(&window) else {
            self.order.push_back(window);
            self.pending.insert(window, incoming);
            return;
        };
        let old = held.request.issuance.plan_sequence();
        let new = incoming.request.issuance.plan_sequence();
        if new < old {
            displaced.push((incoming.reply, superseded));
            return;
        }
        let mut incoming = incoming;
        // An intermediate frame must not erase the first frame's membership
        // listing or its growth-envelope size.
        if new == old
            && held.request.stage == FrameStage::First
            && incoming.request.stage == FrameStage::Intermediate
        {
            incoming.request.stage = FrameStage::First;
            if incoming.request.size.is_none() {
                incoming.request.size = held.request.size;
            }
        }
        let previous = std::mem::replace(held, incoming);
        displaced.push((
            previous.reply,
            if new == old {
                FrameReply::Replaced
            } else {
                superseded
            },
        ));
    }

    /// Removes up to `limit` frames in arrival order; `true` when more remain.
    fn take(&mut self, limit: usize) -> (Vec<PendingFrame>, bool) {
        let mut out = Vec::new();
        while out.len() < limit {
            let Some(window) = self.order.pop_front() else { break };
            if let Some(frame) = self.pending.remove(&window) {
                out.push(frame);
            }
        }
        (out, !self.order.is_empty())
    }

    fn drain_matching(&mut self, matches: impl Fn(&WindowInstance) -> bool) -> Vec<PendingFrame> {
        let mut out = Vec::new();
        for window in std::mem::take(&mut self.order) {
            if matches(&window) {
                out.extend(self.pending.remove(&window));
            } else {
                self.order.push_back(window);
            }
        }
        out
    }
}

/// The router side: inserts a whole batch under one lock. Replies for replaced
/// or superseded frames run after the lock is released. On `Err` nothing was
/// queued and no callback ran.
pub(super) fn submit(
    mailbox: &Mutex<FrameMailbox>,
    batch: Vec<(FrameRequest, FrameCallback)>,
) -> Result<(), DispatchError> {
    if batch.is_empty() {
        return Ok(());
    }
    let mut displaced = Vec::new();
    {
        let mut held = locked(mailbox);
        if held.closed {
            return Err(DispatchError::Closed);
        }
        let fresh: HashSet<WindowInstance> = batch
            .iter()
            .map(|(request, _)| request.issuance.window())
            .filter(|window| !held.pending.contains_key(window))
            .collect();
        if held.pending.len() + fresh.len() > FRAME_MAILBOX_BOUND {
            return Err(DispatchError::Full);
        }
        for (request, callback) in batch {
            held.insert(request, callback, &mut displaced);
        }
    }
    for (guard, reply) in displaced {
        guard.answer(reply);
    }
    Ok(())
}

/// The shard's handle. Dropping it (the shard ended) closes the mailbox and
/// answers every pending frame.
pub(super) struct OwnedMailbox(Arc<Mutex<FrameMailbox>>);

impl OwnedMailbox {
    pub(super) fn new() -> Self { Self(Arc::new(Mutex::new(FrameMailbox::default()))) }

    pub(super) const fn from_shared(shared: Arc<Mutex<FrameMailbox>>) -> Self { Self(shared) }

    #[cfg(test)]
    pub(super) fn shared(&self) -> Arc<Mutex<FrameMailbox>> { Arc::clone(&self.0) }

    pub(super) fn take(&self, limit: usize) -> (Vec<PendingFrame>, bool) {
        locked(&self.0).take(limit)
    }

    /// Whether a frame for `application` is waiting. The lock is held only to
    /// look.
    pub(super) fn has_pending(&self, application: crate::applications::ApplicationId) -> bool {
        locked(&self.0).pending.keys().any(|window| window.application() == application)
    }

    pub(super) fn drain_all(&self) -> Vec<PendingFrame> { locked(&self.0).drain_matching(|_| true) }

    pub(super) fn drain_application(
        &self,
        application: crate::applications::ApplicationId,
    ) -> Vec<PendingFrame> {
        locked(&self.0).drain_matching(|window| window.application() == application)
    }

    /// Refuses later inserts and returns what was pending.
    pub(super) fn close(&self) -> Vec<PendingFrame> {
        let mut held = locked(&self.0);
        held.closed = true;
        held.drain_matching(|_| true)
    }
}

impl Drop for OwnedMailbox {
    fn drop(&mut self) {
        // Dropping each guard answers `OwnerUnavailable`, after the lock.
        drop(self.close());
    }
}
