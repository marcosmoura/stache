//! The native Space and Mission Control detection sources (part 05, second
//! half).
//!
//! [`SystemSpaceEvents`] implements [`SpaceEvents`]. Nothing is created at
//! construction: `start` spawns the one owner thread
//! `stache-tiling-borders-spaces`, which registers every source and reports the
//! result as [`SpaceFact`]s. `stop` closes the sink first, posts a stop, and
//! joins the owner within a bound (a typed [`StopOutcome`]; a thread that does
//! not exit in time keeps its handle, is never detached on purpose and is
//! reported so part 07 can mark it owed).
//!
//! Approach only, never code (decision 5): the reference border tool's `src/events.c`
//! (`space_handler`) and yabai `src/mission_control.c` (Dock Accessibility
//! notifications) were studied for which signals exist. The registrations and
//! declarations here are written from Apple documentation and the spec; the
//! private declarations come from the signatures recorded in `skylight.rs`, not
//! from any header of either project.
//!
//! # Sources
//!
//! | Source | Mechanism | Thread of the callback |
//! | --- | --- | --- |
//! | Mission Control, App Exposé, Show Desktop and exit | AX notifications on the Dock process (`AxObserver` on the owner run loop); the owner drains the observer after the run loop handled it and pushes facts | owner |
//! | Dock launch / terminate | `NSWorkspace` did-launch / did-terminate for `com.apple.dock`; posts a re-register command | `NSOperationQueue` worker |
//! | Space change completed | public `NSWorkspaceActiveSpaceDidChangeNotification`, no display info, so every active display | `NSOperationQueue` worker |
//! | Display layout change | `NSApplicationDidChangeScreenParametersNotification` (posted on main, delivered to the queue), counted only when the displays or their geometry differ from the last read (it is also posted for HDR headroom changes); only the end is observable | `NSOperationQueue` worker |
//! | Space change began | private `SkyLight` connection notify proc, `dlsym`-resolved (`skylight.rs`), no display, so every active display | unverified (the connection's event source; possibly main) |
//! | Fullscreen | not a source: the Tiling engine reports `FullscreenChanged` from the window flags it already observes | actor |
//!
//! Callbacks only call one [`SourceHub`] method (bounded, allocation-free apart
//! from reading the Dock bundle identifier in the workspace blocks, no AX read,
//! no lock held while calling out).
//!
//! # Fail closed
//!
//! A source that cannot register pushes `SourceLost(cause)`; only when all are
//! registered does the owner push `SourceRestored(SourcesNotStarted)`. A Dock
//! relaunch pushes `SourceLost(DockUnregistered)`, re-registers and pushes
//! `SourceRestored(DockUnregistered)`. After a mailbox overflow the owner
//! re-pushes every lost capability, Mission Control if active and each display
//! in a transition, then `SourceRestored(MailboxOverflow)`.
//!
//! # Unverified until the part 05 native check
//!
//! The Dock notification names (see `ax::names`), the private event id and
//! signatures (see `skylight`), the delivery thread of the private callback,
//! the timing of every source relative to the first transition frame and the
//! delivery of the screen-parameter notification to a non-main queue.

mod hub;
mod native;
mod owner;
pub mod skylight;
#[cfg(test)]
mod tests;
mod window_list;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

pub use hub::{
    Command, Commands, DisplayGeometry, DisplayLister, LoopWaker, MAX_DISPLAYS, SourceHub,
};
pub use owner::{OwnerState, SessionFactory, Source, SpaceSession, run_owner};
pub use window_list::{OnScreenWindows, SystemOnScreenWindows, WindowListError, confirm_on_screen};

use super::spaces::{SpaceEvents, SpaceEventsFactory, SpaceFact, SpaceSink, UnknownCause};

/// The owner thread name.
pub const OWNER_THREAD_NAME: &str = "stache-tiling-borders-spaces";

/// How long `stop` waits for the owner and for callbacks in progress.
pub const STOP_BUDGET: Duration = Duration::from_secs(1);

/// What a stop achieved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopOutcome {
    /// Nothing was running.
    NotRunning,
    /// The sink was closed, the owner exited and was joined.
    Stopped,
    /// A bound passed. The thread handle is retained (never detached on
    /// purpose); part 07 marks the shutdown owed.
    TimedOut {
        /// Whether the owner thread exited and was joined.
        owner_joined: bool,
        /// Whether every callback in progress finished.
        callbacks_drained: bool,
    },
}

/// Observes every stop outcome (part 07 marks a timeout owed).
pub type StopObserver = Arc<dyn Fn(StopOutcome) + Send + Sync>;

#[derive(Default)]
pub(crate) struct Exit {
    done: Mutex<bool>,
    changed: Condvar,
}

impl Exit {
    pub(crate) fn finish(&self) {
        *self.done.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.changed.notify_all();
    }

    pub(crate) fn wait(&self, budget: Duration) -> bool {
        let guard = self.done.lock().unwrap_or_else(PoisonError::into_inner);
        let (guard, _) = self
            .changed
            .wait_timeout_while(guard, budget, |done| !*done)
            .unwrap_or_else(PoisonError::into_inner);
        *guard
    }
}

/// Marks the owner exited even when the body unwinds.
pub(crate) struct ExitGuard(pub(crate) Arc<Exit>);

impl Drop for ExitGuard {
    fn drop(&mut self) { self.0.finish(); }
}

struct Running {
    handle: JoinHandle<()>,
    hub: Arc<SourceHub>,
    commands: Arc<Commands>,
    exit: Arc<Exit>,
}

/// The native source; see the module documentation.
pub struct SystemSpaceEvents {
    sessions: Arc<dyn Fn() -> (SessionFactory, Box<dyn DisplayLister>) + Send + Sync>,
    budget: Duration,
    observer: Option<StopObserver>,
    running: Option<Running>,
    last_stop: Option<StopOutcome>,
}

impl SystemSpaceEvents {
    /// The system source. Creates nothing until `start`.
    #[must_use]
    pub fn system(observer: Option<StopObserver>) -> Self {
        Self::with_sessions(
            Arc::new(|| {
                (
                    native::session_factory(),
                    Box::new(native::SystemDisplays) as Box<dyn DisplayLister>,
                )
            }),
            STOP_BUDGET,
            observer,
        )
    }

    /// A factory creating a fresh system source for each run of the module.
    #[must_use]
    pub fn factory(observer: Option<StopObserver>) -> SpaceEventsFactory {
        Arc::new(move || Box::new(Self::system(observer.clone())) as Box<dyn SpaceEvents>)
    }

    /// A source over injected sessions (tests use fakes; nothing native).
    pub(crate) fn with_sessions(
        sessions: Arc<dyn Fn() -> (SessionFactory, Box<dyn DisplayLister>) + Send + Sync>,
        budget: Duration,
        observer: Option<StopObserver>,
    ) -> Self {
        Self {
            sessions,
            budget,
            observer,
            running: None,
            last_stop: None,
        }
    }

    /// The outcome of the latest stop.
    #[must_use]
    pub const fn last_stop(&self) -> Option<StopOutcome> { self.last_stop }

    /// Stops within `budget`: closes the sink, posts a stop, joins.
    pub fn stop_bounded(&mut self, budget: Duration) -> StopOutcome {
        let outcome = self.stop_inner(budget);
        self.last_stop = Some(outcome);
        if let Some(observer) = &self.observer {
            observer(outcome);
        }
        outcome
    }

    fn stop_inner(&mut self, budget: Duration) -> StopOutcome {
        let Some(running) = self.running.as_ref() else {
            return StopOutcome::NotRunning;
        };
        // 1. Close the sink: no callback pushes after this returns true.
        let callbacks_drained = running.hub.close(budget);
        // 2. Ask the owner to leave its run loop and 3. join within the bound.
        running.commands.post(Command::Stop);
        let owner_joined = running.exit.wait(budget);
        if owner_joined && let Some(running) = self.running.take() {
            // The owner finished; the join returns at once.
            let _ = running.handle.join();
        }
        if owner_joined && callbacks_drained {
            StopOutcome::Stopped
        } else {
            StopOutcome::TimedOut {
                owner_joined,
                callbacks_drained,
            }
        }
    }
}

impl SpaceEvents for SystemSpaceEvents {
    fn start(&mut self, sink: SpaceSink) {
        if self.running.is_some() {
            // A previous owner is still running or stuck: try to retire it.
            if self.stop_bounded(self.budget) != StopOutcome::Stopped {
                let _ = sink.push(SpaceFact::SourceLost(UnknownCause::OwnerStopped));
                return;
            }
        }
        let (factory, displays) = (self.sessions)();
        let commands = Arc::new(Commands::default());
        let hub = Arc::new(SourceHub::new(sink.clone(), displays, Arc::clone(&commands)));
        let exit = Arc::new(Exit::default());
        let spawned = std::thread::Builder::new().name(OWNER_THREAD_NAME.to_owned()).spawn({
            let hub = Arc::clone(&hub);
            let commands = Arc::clone(&commands);
            let exit = Arc::clone(&exit);
            move || {
                let _guard = ExitGuard(exit);
                hub.prime_display_layout();
                run_owner(factory, &hub, &commands);
            }
        });
        match spawned {
            Ok(handle) => {
                self.running = Some(Running { handle, hub, commands, exit });
            }
            Err(_) => {
                let _ = sink.push(SpaceFact::SourceLost(UnknownCause::OwnerStopped));
            }
        }
    }

    fn resync(&mut self) {
        if let Some(running) = &self.running {
            running.commands.post(Command::Resync);
        }
    }

    fn stop(&mut self) -> StopOutcome { self.stop_bounded(self.budget) }
}

/// Owner threads whose stop timed out while their source was dropped (see
/// `Drop for SystemSpaceEvents`). A diagnostic: each one is a thread and its
/// shared state kept alive on purpose.
static FORGOTTEN_OWNERS: AtomicU64 = AtomicU64::new(0);

/// How many Space owner threads were forgotten by a drop whose stop timed out.
#[must_use]
pub fn forgotten_space_owners() -> u64 { FORGOTTEN_OWNERS.load(Ordering::Relaxed) }

impl Drop for SystemSpaceEvents {
    /// Stops within the budget. A stop that times out has nobody left to retry
    /// it: the owner thread, its handle and the state its callbacks use are
    /// forgotten explicitly (never freed under a running callback, never a
    /// silent detach). The observer already received the `TimedOut` outcome,
    /// and [`forgotten_space_owners`] counts it.
    fn drop(&mut self) {
        if self.running.is_some() {
            let _ = self.stop_bounded(self.budget);
        }
        if let Some(running) = self.running.take() {
            FORGOTTEN_OWNERS.fetch_add(1, Ordering::Relaxed);
            std::mem::forget(running);
        }
    }
}
