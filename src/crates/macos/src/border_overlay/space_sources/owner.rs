//! The owner thread body and its pure registration state.
//!
//! [`run_owner`] runs on `stache-tiling-borders-spaces`. It opens a
//! [`SpaceSession`] (the native one creates its run loop, Accessibility
//! observer and notification registrations there), registers every source,
//! reports the result as facts, then loops: take posted [`Command`]s, handle
//! them, otherwise [`SpaceSession::pump`] the run loop until it is woken.

use std::sync::Arc;

use super::super::spaces::{SpaceFact, UnknownCause};
use super::hub::{Command, Commands, LoopWaker, SourceHub};

/// One detection source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Dock Accessibility notifications (Mission Control, Exposé, Show Desktop).
    Dock,
    /// Public `NSWorkspace` Space and display notifications.
    Workspace,
    /// The private `SkyLight` Space-change notification.
    SkyLight,
}

impl Source {
    /// Every source, in registration order.
    pub const ALL: [Self; 3] = [Self::Dock, Self::Workspace, Self::SkyLight];

    const fn index(self) -> usize {
        match self {
            Self::Dock => 0,
            Self::Workspace => 1,
            Self::SkyLight => 2,
        }
    }
}

/// The owner-thread side of the native sources. Created, used and dropped on
/// the owner thread; deliberately not `Send`.
pub trait SpaceSession {
    /// A waker for this session's run loop, callable from any thread. It must
    /// make a pending or future [`Self::pump`] return.
    fn waker(&self) -> LoopWaker;
    /// Registers one source. The cause is what the gate holds while it fails.
    ///
    /// # Errors
    /// The [`UnknownCause`] to report with `SourceLost`.
    fn register(&mut self, source: Source) -> Result<(), UnknownCause>;
    /// Removes one source's registration (idempotent).
    fn unregister(&mut self, source: Source);
    /// Runs the run loop until it is woken or a source was handled, then turns
    /// whatever the callbacks recorded into facts through the hub.
    fn pump(&mut self);
}

/// Creates the session on the owner thread.
pub type SessionFactory = Box<dyn FnOnce(Arc<SourceHub>) -> Box<dyn SpaceSession> + Send>;

/// Which sources are lost and whether the startup report was made.
#[derive(Clone, Copy, Debug, Default)]
pub struct OwnerState {
    lost: [Option<UnknownCause>; 3],
    reported_started: bool,
    initial_done: bool,
}

impl OwnerState {
    /// The causes currently lost, per source.
    #[must_use]
    pub const fn lost(&self, source: Source) -> Option<UnknownCause> { self.lost[source.index()] }

    /// Whether the startup `SourceRestored(SourcesNotStarted)` was reported.
    #[must_use]
    pub const fn started(&self) -> bool { self.reported_started }

    /// Marks the first registration pass finished; reports "registered" when
    /// every source succeeded.
    pub fn finish_initial(&mut self, emit: &mut dyn FnMut(SpaceFact)) {
        self.initial_done = true;
        self.maybe_started(emit);
    }

    /// Records the outcome of registering `source`.
    ///
    /// A failure reports `SourceLost` once per cause. A success after a loss
    /// reports `SourceRestored` unless another source is still lost with the
    /// same cause (the gate keys reasons by cause). The startup report follows
    /// once everything is registered.
    pub fn registered(
        &mut self,
        source: Source,
        result: Result<(), UnknownCause>,
        emit: &mut dyn FnMut(SpaceFact),
    ) {
        let slot = source.index();
        match result {
            Err(cause) => {
                if self.lost[slot] != Some(cause) {
                    let previous = self.lost[slot].replace(cause);
                    emit(SpaceFact::SourceLost(cause));
                    // A different earlier cause of the same source is superseded.
                    if let Some(previous) = previous
                        && !self.lost.contains(&Some(previous))
                    {
                        emit(SpaceFact::SourceRestored(previous));
                    }
                }
            }
            Ok(()) => {
                if let Some(cause) = self.lost[slot].take()
                    && !self.lost.contains(&Some(cause))
                {
                    emit(SpaceFact::SourceRestored(cause));
                }
            }
        }
        self.maybe_started(emit);
    }

    fn maybe_started(&mut self, emit: &mut dyn FnMut(SpaceFact)) {
        if self.initial_done && !self.reported_started && self.lost.iter().all(Option::is_none) {
            self.reported_started = true;
            emit(SpaceFact::SourceRestored(UnknownCause::SourcesNotStarted));
        }
    }

    /// The lost capabilities, to re-report after an overflow.
    pub fn resync_lost(&self, emit: &mut dyn FnMut(SpaceFact)) {
        for cause in self.lost.iter().flatten() {
            emit(SpaceFact::SourceLost(*cause));
        }
    }
}

/// The owner thread body.
pub fn run_owner(factory: SessionFactory, hub: &Arc<SourceHub>, commands: &Arc<Commands>) {
    let mut session = factory(Arc::clone(hub));
    commands.set_waker(Some(session.waker()));
    let mut state = OwnerState::default();
    let mut emit = |fact| hub.emit(fact);
    for source in Source::ALL {
        let result = session.register(source);
        state.registered(source, result, &mut emit);
    }
    state.finish_initial(&mut emit);
    'run: loop {
        let posted = commands.take();
        if posted.is_empty() {
            session.pump();
            continue;
        }
        for command in posted {
            match command {
                Command::Stop => break 'run,
                Command::DockChanged => {
                    session.unregister(Source::Dock);
                    // The Dock is unregistered until the new registration works.
                    state.registered(Source::Dock, Err(UnknownCause::DockUnregistered), &mut emit);
                    let result = session.register(Source::Dock);
                    state.registered(Source::Dock, result, &mut emit);
                }
                Command::Resync => {
                    state.resync_lost(&mut emit);
                    hub.resync();
                    emit(SpaceFact::SourceRestored(UnknownCause::MailboxOverflow));
                }
            }
        }
    }
    commands.set_waker(None);
    for source in Source::ALL {
        session.unregister(source);
    }
}
