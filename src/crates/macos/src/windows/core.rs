//! The owner-thread state machine. Everything here runs on the dedicated AX
//! owner thread and is driven one [`Core::turn`] at a time, so tests step it
//! deterministically without a run loop.
//!
//! # Turn order
//!
//! 1. Drain observer mailboxes first: a destroy notification invalidates its
//!    token **before** any queued command for that window is looked at. Events
//!    form an ordered backlog; a burst of `WindowCreated` is adopted within the
//!    enumeration budget per turn and the rest waits (in order) for the next
//!    turn. Command revalidation checks live `AXWindows` membership regardless,
//!    so a destroy still waiting in the backlog never lets a stale token act.
//! 2. Run at most [`MAX_COMMANDS_PER_TURN`] queued commands.
//! 3. Drain observers again, then run **one** enumeration chunk.
//!
//! A turn that leaves work (more commands or enumeration) reports
//! `active`, and the driver parks with a zero timeout instead of blocking:
//! that is the re-post. An idle turn parks without any timeout.

use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use stache_sdk::CancellationToken;

use super::commands::{
    ActionRequest, CleanupReport, CloseOutcome, Command, HiddenOperation, HiddenOutcome,
    HiddenRequest, InstallError, InstallRequest, Reply, SnapshotOutcome, StopOutcome, TrackOutcome,
    TrackRequest, WindowAction,
};
use super::events::{
    ApplicationChange, Capability, EnumerationAbort, EventQueue, SnapshotOrigin, TabInfo,
    TabMember, WindowChange, WindowEvent, WindowEventKind, WindowSnapshot,
};
use super::identity::{AdapterId, RuntimeEpoch, WindowInstance};
use super::issuance::{
    CommitRefusal, GenerationCell, IssuedKind, NativeStatus, NotIssuedReason, StaleReason,
    WindowActionOutcome,
};
use super::system::{WindowClock, WindowObserver, WindowSystem};
use crate::applications::ApplicationId;
use crate::ax::{
    AxApi, AxError, AxEvent, AxNotification, AxReading, BoolAttribute, ELEMENT_TIMEOUT, ElementKey,
    FrameWriteOutcome, PRESS_TIMEOUT, TabGroupRead, WindowAx, WindowString, WriteStatus,
    read_frame, read_tab_group, resizable, write_frame_gated,
};

/// Elements an enumeration chunk may issue before it yields.
pub const ENUMERATION_CHUNK_ELEMENTS: usize = 64;
/// Issuing time an enumeration chunk may spend before it yields.
pub const ENUMERATION_CHUNK_BUDGET: Duration = Duration::from_millis(50);
/// The most windows tracked per application.
pub const MAX_WINDOWS_PER_APPLICATION: usize = 256;
/// The most commands one turn runs.
pub const MAX_COMMANDS_PER_TURN: usize = 16;
/// The most tabs counted per window.
const TAB_LIMIT: usize = 32;

/// Notifications registered on each window element.
pub(super) const WINDOW_NOTIFICATIONS: [AxNotification; 6] = [
    AxNotification::ElementDestroyed,
    AxNotification::WindowMoved,
    AxNotification::WindowResized,
    AxNotification::WindowMiniaturized,
    AxNotification::WindowDeminiaturized,
    AxNotification::TitleChanged,
];

/// Notifications registered on each application element.
pub(super) const APPLICATION_NOTIFICATIONS: [AxNotification; 5] = [
    AxNotification::WindowCreated,
    AxNotification::FocusedWindowChanged,
    AxNotification::MainWindowChanged,
    AxNotification::ApplicationHidden,
    AxNotification::ApplicationShown,
];

type Element<S> = <<S as WindowSystem>::Ax as AxApi>::Element;

struct Tracked<E> {
    serial: u64,
    element: E,
    key: ElementKey,
    cg: Option<u32>,
    /// Whether every per-window notification registered when it was adopted.
    observed: bool,
}

struct AppState<S: WindowSystem> {
    id: ApplicationId,
    element: Element<S>,
    observer: S::Observer,
    app_key: Option<ElementKey>,
    regs: Vec<(ElementKey, AxNotification)>,
    windows: Vec<Tracked<Element<S>>>,
    /// Callback-delivered keys of the current event backlog, released only once
    /// the whole backlog is processed (a later event may still name them).
    release: Vec<ElementKey>,
    /// Focus/main notifications naming a window that was not tracked when they
    /// were processed; resolved after the backlog (a creation may follow).
    focus: Vec<(ElementKey, AxNotification)>,
}

struct Enumeration<E> {
    app: ApplicationId,
    index: usize,
    pending: VecDeque<E>,
    adopted: usize,
    started: Option<Instant>,
    cancel: CancellationToken,
    deadline: Option<Instant>,
    /// Serials of tracked windows this enumeration reported.
    seen: HashSet<u64>,
    /// The retirement stage once the pages ended; spread across chunks.
    retire: Option<Retire<E>>,
}

/// Tracked windows the pages missed, checked against one fresh full listing a
/// chunk at a time. Bounded by [`MAX_WINDOWS_PER_APPLICATION`].
struct Retire<E> {
    list: Vec<E>,
    unseen: VecDeque<u64>,
}

/// The owner-side budget for AX-heavy event work in one turn.
struct EventBudget {
    heavy: usize,
    started: Option<Instant>,
}

struct Runtime<S: WindowSystem> {
    epoch: RuntimeEpoch,
    gate: Arc<GenerationCell>,
    queue: Arc<EventQueue>,
    sequence: u64,
    next_serial: u64,
    apps: HashMap<ApplicationId, AppState<S>>,
    enumerations: VecDeque<Enumeration<Element<S>>>,
    /// Drained, not yet processed observer events in arrival order.
    backlog: VecDeque<(ApplicationId, AxEvent)>,
    degraded: bool,
}

impl<S: WindowSystem> Runtime<S> {
    fn emit(&mut self, kind: WindowEventKind) {
        self.sequence += 1;
        let _ = self.queue.push(WindowEvent {
            epoch: self.epoch,
            sequence: self.sequence,
            kind,
        });
    }

    fn note_error(&mut self, error: AxError) {
        if error == AxError::ApiDisabled && !self.degraded {
            self.degraded = true;
            self.emit(WindowEventKind::CapabilityChanged {
                capability: Capability::Accessibility,
                available: false,
            });
        }
    }

    fn note_success(&mut self) {
        if self.degraded {
            self.degraded = false;
            self.emit(WindowEventKind::CapabilityChanged {
                capability: Capability::Accessibility,
                available: true,
            });
        }
    }

    const fn token(&self, adapter: AdapterId, app: ApplicationId, serial: u64) -> WindowInstance {
        WindowInstance::new(adapter, self.epoch, app, serial)
    }
}

struct Quarantined<S: WindowSystem> {
    app: AppState<S>,
}

/// What one turn left to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Turn {
    /// More work is ready: park with a zero timeout.
    pub active: bool,
    /// The owner loop must end.
    pub finished: bool,
}

/// Why a token failed revalidation.
enum Invalid {
    Stale(StaleReason),
    Unverified(AxError),
}

/// The owner state machine.
pub(super) struct Core<S: WindowSystem> {
    system: S,
    adapter: AdapterId,
    clock: Arc<dyn WindowClock>,
    read_tabs: bool,
    next_epoch: u64,
    runtime: Option<Runtime<S>>,
    quarantine: Vec<Quarantined<S>>,
    poisoned: bool,
    finished: bool,
}

fn answer<T>(reply: Reply<T>, value: T) { let _ = catch_unwind(AssertUnwindSafe(|| reply(value))); }

/// Removes every registration once. `Ok` means everything was removed and the
/// application was dropped (releasing its elements on this thread); `Err`
/// returns the application with only the registrations that failed.
#[allow(
    clippy::result_large_err,
    reason = "the error hands the whole application to the quarantine unchanged"
)]
fn teardown<S: WindowSystem>(mut app: AppState<S>) -> Result<(), AppState<S>> {
    app.observer.detach();
    let regs = std::mem::take(&mut app.regs);
    let mut gone: Vec<ElementKey> = Vec::new();
    let mut remaining = Vec::new();
    for (key, notification) in regs {
        if gone.contains(&key) {
            continue;
        }
        match app.observer.remove_notification(key, notification) {
            Ok(()) => {}
            Err(AxError::InvalidElement) => {
                app.observer.forget_destroyed(key);
                gone.push(key);
            }
            Err(_) => remaining.push((key, notification)),
        }
    }
    remaining.retain(|(key, _)| !gone.contains(key));
    if remaining.is_empty() {
        Ok(())
    } else {
        app.regs = remaining;
        Err(app)
    }
}

/// Classifies an issued frame write.
pub(super) fn classify_frame_write(position: WriteStatus, size: WriteStatus) -> IssuedKind {
    let failed = |status: WriteStatus| matches!(status, WriteStatus::Failed(_));
    let written = |status: WriteStatus| matches!(status, WriteStatus::Written);
    let uncertain =
        |status: WriteStatus| matches!(status, WriteStatus::Failed(AxError::CannotComplete));
    if !failed(position) && !failed(size) {
        IssuedKind::Applied
    } else if uncertain(position) || uncertain(size) {
        IssuedKind::Uncertain
    } else if (written(position) && failed(size)) || (failed(position) && written(size)) {
        IssuedKind::Partial
    } else {
        IssuedKind::Rejected
    }
}

const fn change_for(notification: AxNotification) -> Option<WindowChange> {
    match notification {
        AxNotification::ElementDestroyed => Some(WindowChange::Destroyed),
        AxNotification::WindowMoved => Some(WindowChange::Moved),
        AxNotification::WindowResized => Some(WindowChange::Resized),
        AxNotification::WindowMiniaturized => Some(WindowChange::Miniaturized),
        AxNotification::WindowDeminiaturized => Some(WindowChange::Deminiaturized),
        AxNotification::FocusedWindowChanged => Some(WindowChange::Focused),
        AxNotification::MainWindowChanged => Some(WindowChange::Main),
        AxNotification::TitleChanged => Some(WindowChange::TitleChanged),
        AxNotification::WindowCreated
        | AxNotification::ApplicationHidden
        | AxNotification::ApplicationShown => None,
    }
}

fn read_snapshot<A: WindowAx>(
    ax: &A,
    element: &A::Element,
    window: WindowInstance,
    observed: bool,
    read_tabs: bool,
) -> WindowSnapshot {
    let _ = ax.set_messaging_timeout(element, ELEMENT_TIMEOUT);
    let (tabs, tab_members) = if read_tabs {
        match read_tab_group(ax, element, TAB_LIMIT) {
            TabGroupRead::NoTabGroup => (TabInfo::None, Vec::new()),
            TabGroupRead::Group(members) => (
                TabInfo::Tabs(members.len()),
                members
                    .iter()
                    .map(|member| TabMember {
                        cg_window_id: member.cg_window_id,
                        selected: member.selected.value(),
                    })
                    .collect(),
            ),
            TabGroupRead::Unknown(_) => (TabInfo::Failed, Vec::new()),
        }
    } else {
        (TabInfo::Unknown, Vec::new())
    };
    WindowSnapshot {
        window,
        cg_window_id: ax.window_id(element).ok(),
        title: AxReading::classify(ax.window_string(element, WindowString::Title)),
        role: AxReading::classify(ax.window_string(element, WindowString::Role)),
        subrole: AxReading::classify(ax.window_string(element, WindowString::Subrole)),
        frame: read_frame(ax, element),
        minimized: AxReading::classify(ax.bool_attribute(element, BoolAttribute::Minimized)),
        fullscreen: AxReading::classify(ax.bool_attribute(element, BoolAttribute::FullScreen)),
        resizable: resizable(ax, element),
        tabs,
        tab_members,
        observed,
    }
}

enum Adopted {
    /// Already tracked: the index into the application's windows.
    Existing(usize),
    New(Box<WindowSnapshot>),
    Gone,
    Capped,
    NotObservable,
}

impl<S: WindowSystem> Core<S> {
    pub(super) fn new(
        system: S,
        adapter: AdapterId,
        clock: Arc<dyn WindowClock>,
        read_tabs: bool,
    ) -> Self {
        Self {
            system,
            adapter,
            clock,
            read_tabs,
            next_epoch: 0,
            runtime: None,
            quarantine: Vec::new(),
            poisoned: false,
            finished: false,
        }
    }

    pub(super) fn quarantined_registrations(&self) -> usize {
        self.quarantine.iter().map(|held| held.app.regs.len()).sum()
    }

    #[cfg(test)]
    pub(super) const fn is_poisoned(&self) -> bool { self.poisoned }

    /// Live (not destroyed) windows tracked for diagnostics and tests.
    #[cfg(test)]
    pub(super) fn tracked_windows(&self) -> usize {
        self.runtime
            .as_ref()
            .map_or(0, |rt| rt.apps.values().map(|app| app.windows.len()).sum())
    }

    // ---- turn ----------------------------------------------------------

    pub(super) fn turn(&mut self, commands: &Receiver<Command>) -> Turn {
        if self.finished {
            return Turn { active: false, finished: true };
        }
        let mut budget = EventBudget { heavy: 0, started: None };
        self.pump(&mut budget);
        let mut more = false;
        for index in 0..MAX_COMMANDS_PER_TURN {
            match commands.try_recv() {
                Ok(command) => self.handle_guarded(command),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.stop_everything();
                    self.finished = true;
                    return Turn { active: false, finished: true };
                }
            }
            if index + 1 == MAX_COMMANDS_PER_TURN {
                more = true;
            }
        }
        if self.finished {
            return Turn { active: false, finished: true };
        }
        self.pump(&mut budget);
        if !self.poisoned {
            let _ = catch_unwind(AssertUnwindSafe(|| self.step_enumeration()));
        }
        Turn {
            active: more
                || self
                    .runtime
                    .as_ref()
                    .is_some_and(|rt| !rt.enumerations.is_empty() || !rt.backlog.is_empty()),
            finished: self.finished,
        }
    }

    fn handle_guarded(&mut self, command: Command) {
        if self.poisoned {
            // Dropping the command drops its reply; the waiting side observes
            // the owner as unavailable.
            return;
        }
        if catch_unwind(AssertUnwindSafe(|| self.handle(command))).is_err() {
            // Effective in unwind-enabled builds only; release aborts.
            self.poisoned = true;
            if let Some(rt) = &self.runtime {
                rt.gate.seal();
            }
        }
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Install { request, reply } => answer(reply, self.install(request)),
            Command::Track { epoch, request, reply } => {
                answer(reply, self.track(epoch, request));
            }
            Command::Untrack { epoch, application, reply } => {
                answer(reply, self.untrack(epoch, application));
            }
            Command::Refresh {
                epoch,
                application,
                cancel,
                deadline,
                reply,
            } => {
                answer(reply, self.refresh(epoch, application, cancel, deadline));
            }
            Command::Act { request, reply } => answer(reply, self.act(&request)),
            Command::Hidden { request, reply } => answer(reply, self.application_hidden(&request)),
            Command::Snapshot { window, reply } => answer(reply, self.snapshot(window)),
            Command::Resolve { cg_window_id, reply } => {
                answer(reply, self.resolve(cg_window_id));
            }
            Command::Stop { epoch, reply } => answer(reply, self.stop(epoch)),
            Command::Retry { reply } => answer(reply, self.retry_cleanup()),
            Command::Terminate { reply } => answer(reply, self.terminate()),
        }
    }

    // ---- lifecycle -----------------------------------------------------

    fn install(&mut self, request: InstallRequest) -> Result<RuntimeEpoch, InstallError> {
        if let Some(rt) = &self.runtime {
            return Err(InstallError::AlreadyInstalled(rt.epoch));
        }
        if !self.quarantine.is_empty() {
            return Err(InstallError::Quarantined {
                applications: self.quarantine.len(),
                registrations: self.quarantined_registrations(),
            });
        }
        self.next_epoch += 1;
        let epoch = RuntimeEpoch::new(self.next_epoch);
        self.runtime = Some(Runtime {
            epoch,
            gate: request.gate,
            queue: request.queue,
            sequence: 0,
            next_serial: 1,
            apps: HashMap::new(),
            enumerations: VecDeque::new(),
            backlog: VecDeque::new(),
            degraded: false,
        });
        Ok(epoch)
    }

    fn quarantine_outcome(&self, cleaned: usize) -> StopOutcome {
        if self.quarantine.is_empty() {
            StopOutcome::Clean { applications: cleaned }
        } else {
            StopOutcome::Quarantined {
                applications: self.quarantine.len(),
                registrations: self.quarantined_registrations(),
            }
        }
    }

    fn stop(&mut self, epoch: RuntimeEpoch) -> StopOutcome {
        if self.runtime.as_ref().is_none_or(|rt| rt.epoch != epoch) {
            return StopOutcome::NotCurrent;
        }
        let cleaned = self.stop_everything();
        self.quarantine_outcome(cleaned)
    }

    /// Seals, cancels enumeration and tears the running epoch down. Returns
    /// the number of applications released cleanly.
    fn stop_everything(&mut self) -> usize {
        let Some(rt) = self.runtime.take() else { return 0 };
        rt.gate.seal();
        let Runtime { apps, enumerations, .. } = rt;
        drop(enumerations);
        let mut ordered: Vec<_> = apps.into_iter().collect();
        ordered.sort_by_key(|(id, _)| *id);
        let mut cleaned = 0;
        for (_, app) in ordered {
            match teardown(app) {
                Ok(()) => cleaned += 1,
                Err(app) => self.quarantine.push(Quarantined { app }),
            }
        }
        cleaned
    }

    fn retry_cleanup(&mut self) -> CleanupReport {
        let held = std::mem::take(&mut self.quarantine);
        let mut cleaned = 0;
        for entry in held {
            match teardown(entry.app) {
                Ok(()) => cleaned += 1,
                Err(app) => self.quarantine.push(Quarantined { app }),
            }
        }
        CleanupReport {
            cleaned_applications: cleaned,
            remaining_applications: self.quarantine.len(),
            remaining_registrations: self.quarantined_registrations(),
        }
    }

    fn terminate(&mut self) -> CloseOutcome {
        if let Some(epoch) = self.runtime.as_ref().map(|rt| rt.epoch) {
            let _ = self.stop(epoch);
        }
        if self.quarantine.is_empty() {
            self.finished = true;
            CloseOutcome::Closed
        } else {
            CloseOutcome::Quarantined {
                registrations: self.quarantined_registrations(),
            }
        }
    }

    // ---- tracking ------------------------------------------------------

    fn track(&mut self, epoch: RuntimeEpoch, request: TrackRequest) -> TrackOutcome {
        let Self {
            system, runtime, quarantine, ..
        } = self;
        let Some(rt) = runtime.as_mut().filter(|rt| rt.epoch == epoch) else {
            return TrackOutcome::WrongEpoch;
        };
        if rt.gate.is_sealed() {
            return TrackOutcome::WrongEpoch;
        }
        if rt.apps.contains_key(&request.application) {
            return TrackOutcome::AlreadyTracked;
        }
        let fail = |rt: &mut Runtime<S>, error: AxError| {
            rt.note_error(error);
            if error == AxError::ApiDisabled {
                TrackOutcome::PermissionDenied
            } else {
                TrackOutcome::Failed(error)
            }
        };
        let ax = system.ax();
        let mut observer = match system.observer(request.pid) {
            Ok(observer) => observer,
            Err(error) => return fail(rt, error),
        };
        let element = match ax.create_application(request.pid) {
            Ok(element) => element,
            Err(error) => return fail(rt, error),
        };
        if let Err(error) = ax.set_messaging_timeout(&element, ELEMENT_TIMEOUT) {
            return fail(rt, error);
        }
        observer.attach();
        let mut app = AppState::<S> {
            id: request.application,
            element,
            observer,
            app_key: None,
            regs: Vec::new(),
            windows: Vec::new(),
            release: Vec::new(),
            focus: Vec::new(),
        };
        // Observation is installed BEFORE the first enumeration read.
        for notification in APPLICATION_NOTIFICATIONS {
            match app.observer.add_notification(&app.element, notification) {
                Ok(key) => {
                    app.app_key.get_or_insert(key);
                    if !app.regs.contains(&(key, notification)) {
                        app.regs.push((key, notification));
                    }
                }
                Err(error) => {
                    if let Err(left) = teardown(app) {
                        quarantine.push(Quarantined { app: left });
                    }
                    return fail(rt, error);
                }
            }
        }
        rt.note_success();
        rt.enumerations.push_back(Enumeration {
            app: request.application,
            index: 0,
            pending: VecDeque::new(),
            adopted: 0,
            started: None,
            cancel: request.cancel,
            deadline: request.deadline,
            seen: HashSet::new(),
            retire: None,
        });
        rt.apps.insert(request.application, app);
        TrackOutcome::Tracking
    }

    fn refresh(
        &mut self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        cancel: CancellationToken,
        deadline: Option<Instant>,
    ) -> TrackOutcome {
        let Some(rt) = self.runtime.as_mut().filter(|rt| rt.epoch == epoch) else {
            return TrackOutcome::WrongEpoch;
        };
        if !rt.apps.contains_key(&application) {
            return TrackOutcome::WrongEpoch;
        }
        rt.enumerations.retain(|held| held.app != application);
        rt.enumerations.push_back(Enumeration {
            app: application,
            index: 0,
            pending: VecDeque::new(),
            adopted: 0,
            started: None,
            cancel,
            deadline,
            seen: HashSet::new(),
            retire: None,
        });
        TrackOutcome::Tracking
    }

    fn untrack(&mut self, epoch: RuntimeEpoch, application: ApplicationId) -> StopOutcome {
        let Some(rt) = self.runtime.as_mut().filter(|rt| rt.epoch == epoch) else {
            return StopOutcome::NotCurrent;
        };
        rt.enumerations.retain(|held| held.app != application);
        // Keys are per observer: drop everything still naming the old one.
        rt.backlog.retain(|(held, _)| *held != application);
        let mut cleaned = 0;
        if let Some(mut app) = rt.apps.remove(&application) {
            app.focus.clear();
            for key in std::mem::take(&mut app.release) {
                app.observer.release(key);
            }
            match teardown(app) {
                Ok(()) => cleaned = 1,
                Err(app) => self.quarantine.push(Quarantined { app }),
            }
        }
        self.quarantine_outcome(cleaned)
    }

    // ---- observation ---------------------------------------------------

    /// Drains the observers into the backlog (only once the previous backlog
    /// is fully processed, so order is preserved and the mailboxes stay the
    /// bounded buffer) and processes it. `WindowCreated` costs full AX work, so
    /// a turn adopts at most [`ENUMERATION_CHUNK_ELEMENTS`] of them within
    /// [`ENUMERATION_CHUNK_BUDGET`]; the rest stay queued, in order, for the
    /// next turn. Cheap events never wait behind the budget, only behind an
    /// unprocessed creation that precedes them.
    fn pump(&mut self, budget: &mut EventBudget) {
        let Self {
            adapter,
            runtime,
            read_tabs,
            system,
            clock,
            ..
        } = self;
        let Some(rt) = runtime.as_mut() else { return };
        let mut ids: Vec<ApplicationId> = rt.apps.keys().copied().collect();
        ids.sort();
        if rt.backlog.is_empty() {
            for id in &ids {
                let Some(app) = rt.apps.get_mut(id) else { continue };
                let drain = app.observer.drain();
                if drain.overflowed {
                    rt.emit(WindowEventKind::ObserverOverflow { application: *id });
                }
                rt.backlog.extend(drain.events.into_iter().map(|event| (*id, event)));
            }
        }
        while let Some(&(id, event)) = rt.backlog.front() {
            if event.notification == AxNotification::WindowCreated {
                let now = clock.now();
                let started = *budget.started.get_or_insert(now);
                if budget.heavy >= ENUMERATION_CHUNK_ELEMENTS
                    || now.saturating_duration_since(started) >= ENUMERATION_CHUNK_BUDGET
                {
                    return;
                }
                budget.heavy += 1;
            }
            rt.backlog.pop_front();
            let kinds = ax_event(system, *adapter, *read_tabs, rt, id, event);
            for kind in kinds {
                rt.emit(kind);
            }
        }
        // The whole backlog is processed: resolve deferred focus changes against
        // the now-tracked windows, then release the callback-delivered keys.
        for id in ids {
            let Some(app) = rt.apps.get_mut(&id) else { continue };
            let kinds = settle_backlog(*adapter, rt.epoch, id, app);
            for kind in kinds {
                rt.emit(kind);
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one linear chunk loop: gate, page, adopt, report"
    )]
    fn step_enumeration(&mut self) {
        let Self {
            adapter,
            runtime,
            read_tabs,
            system,
            clock,
            ..
        } = self;
        let Some(rt) = runtime.as_mut() else { return };
        let Some(mut en) = rt.enumerations.pop_front() else {
            return;
        };
        let ax = system.ax();
        let mut processed = 0;
        let abort = |rt: &mut Runtime<S>, en: &Enumeration<Element<S>>, reason| {
            rt.emit(WindowEventKind::EnumerationAborted { application: en.app, reason });
        };
        loop {
            let now = clock.now();
            let started = *en.started.get_or_insert(now);
            if rt.gate.is_sealed() {
                abort(rt, &en, EnumerationAbort::Sealed);
                return;
            }
            if en.cancel.is_cancelled() {
                abort(rt, &en, EnumerationAbort::Cancelled);
                return;
            }
            if en.deadline.is_some_and(|deadline| now >= deadline) {
                abort(rt, &en, EnumerationAbort::DeadlineExceeded);
                return;
            }
            if processed >= ENUMERATION_CHUNK_ELEMENTS
                || now.saturating_duration_since(started) >= ENUMERATION_CHUNK_BUDGET
            {
                // The chunk budget is spent: yield and re-post.
                en.started = None;
                rt.enumerations.push_back(en);
                return;
            }
            if en.pending.is_empty() && en.retire.is_some() {
                processed += 1;
                if retire_step(system, *adapter, *read_tabs, rt, &mut en) {
                    return;
                }
                continue;
            }
            if en.pending.is_empty() {
                let Some(app) = rt.apps.get(&en.app) else { return };
                if en.index >= MAX_WINDOWS_PER_APPLICATION {
                    let kind = WindowEventKind::EnumerationComplete {
                        application: en.app,
                        windows: en.adopted,
                        truncated: true,
                    };
                    rt.emit(kind);
                    return;
                }
                let want = (ENUMERATION_CHUNK_ELEMENTS - processed)
                    .min(MAX_WINDOWS_PER_APPLICATION - en.index);
                let page = ax
                    .set_messaging_timeout(&app.element, ELEMENT_TIMEOUT)
                    .and_then(|()| ax.windows(&app.element, en.index, want));
                match page {
                    Ok(page) if page.is_empty() => {
                        processed += 1;
                        if begin_retirement(system, rt, &mut en) {
                            return;
                        }
                        continue;
                    }
                    Ok(page) => {
                        en.index += page.len();
                        en.pending.extend(page);
                    }
                    Err(error) => {
                        rt.note_error(error);
                        abort(rt, &en, EnumerationAbort::Failed(error));
                        return;
                    }
                }
            }
            let Some(element) = en.pending.pop_front() else {
                continue;
            };
            processed += 1;
            let Some(app) = rt.apps.get_mut(&en.app) else { return };
            let adopted = adopt::<S>(
                system,
                *adapter,
                rt.epoch,
                &mut rt.next_serial,
                app,
                element,
                *read_tabs,
            );
            match adopted {
                Adopted::New(snapshot) => {
                    en.adopted += 1;
                    en.seen.insert(snapshot.window.serial());
                    rt.emit(WindowEventKind::Snapshot {
                        snapshot: *snapshot,
                        origin: SnapshotOrigin::Enumeration,
                    });
                }
                Adopted::Existing(index) => {
                    // A listed, already tracked window is still reported: the
                    // consumer treats an enumeration as the set of live windows.
                    let snapshot = reread::<S>(system, *adapter, rt.epoch, app, index, *read_tabs);
                    en.adopted += 1;
                    en.seen.insert(snapshot.window.serial());
                    rt.emit(WindowEventKind::Snapshot {
                        snapshot,
                        origin: SnapshotOrigin::Enumeration,
                    });
                }
                Adopted::Gone | Adopted::Capped | Adopted::NotObservable => {}
            }
        }
    }

    // ---- revalidation --------------------------------------------------

    /// Revalidates a token against the epoch, the exact application instance,
    /// the retained element and the application's current `AXWindows`.
    fn validate(&mut self, window: WindowInstance) -> Result<usize, Invalid> {
        let Self { adapter, runtime, system, .. } = self;
        if window.adapter() != *adapter {
            return Err(Invalid::Stale(StaleReason::WrongAdapter));
        }
        let Some(rt) = runtime.as_mut().filter(|rt| rt.epoch == window.epoch()) else {
            return Err(Invalid::Stale(StaleReason::WrongEpoch));
        };
        let next_serial = rt.next_serial;
        let Some(app) = rt.apps.get_mut(&window.application()) else {
            return Err(Invalid::Stale(StaleReason::ApplicationGone));
        };
        let Some(index) = app.windows.iter().position(|held| held.serial == window.serial()) else {
            return Err(Invalid::Stale(if window.serial() < next_serial {
                StaleReason::Destroyed
            } else {
                StaleReason::UnknownWindow
            }));
        };
        let ax = system.ax();
        let listed = ax
            .set_messaging_timeout(&app.element, ELEMENT_TIMEOUT)
            .and_then(|()| ax.windows(&app.element, 0, MAX_WINDOWS_PER_APPLICATION));
        let list = match listed {
            Ok(list) => list,
            Err(AxError::InvalidElement) => {
                return Err(Invalid::Stale(StaleReason::ApplicationGone));
            }
            Err(error) => {
                rt.note_error(error);
                return Err(Invalid::Unverified(error));
            }
        };
        let element = &app.windows[index].element;
        if list.iter().any(|candidate| ax.same_element(candidate, element)) {
            rt.note_success();
            return Ok(index);
        }
        if list.len() >= MAX_WINDOWS_PER_APPLICATION {
            return Err(Invalid::Unverified(AxError::CannotComplete));
        }
        // Not a member any more: retire the token and tell the consumer.
        let gone = app.windows.remove(index);
        app.observer.forget_destroyed(gone.key);
        app.regs.retain(|(key, _)| *key != gone.key);
        let token = rt.token(window.adapter(), window.application(), gone.serial);
        rt.emit(WindowEventKind::Window {
            window: token,
            change: WindowChange::Destroyed,
        });
        Err(Invalid::Stale(StaleReason::NotMember))
    }

    fn snapshot(&mut self, window: WindowInstance) -> SnapshotOutcome {
        match self.validate(window) {
            Err(Invalid::Stale(reason)) => SnapshotOutcome::Stale(reason),
            Err(Invalid::Unverified(error)) => SnapshotOutcome::Unverified(error),
            Ok(index) => {
                let Some(app) =
                    self.runtime.as_ref().and_then(|rt| rt.apps.get(&window.application()))
                else {
                    return SnapshotOutcome::Stale(StaleReason::ApplicationGone);
                };
                let held = &app.windows[index];
                SnapshotOutcome::Snapshot(Box::new(read_snapshot(
                    self.system.ax(),
                    &held.element,
                    window,
                    true,
                    self.read_tabs,
                )))
            }
        }
    }

    fn resolve(&mut self, cg_window_id: u32) -> Option<WindowInstance> {
        let adapter = self.adapter;
        let rt = self.runtime.as_ref()?;
        let mut matches = rt.apps.values().flat_map(|app| {
            app.windows
                .iter()
                .filter(move |held| held.cg == Some(cg_window_id))
                .map(move |held| rt.token(adapter, app.id, held.serial))
        });
        let first = matches.next()?;
        // A duplicate claim is ambiguous: resolve nothing rather than guess.
        if matches.next().is_some() {
            return None;
        }
        drop(matches);
        self.validate(first).ok().map(|_| first)
    }

    // ---- actions -------------------------------------------------------

    /// Reads or writes `AXHidden` on one tracked application element.
    ///
    /// A write is the fallback for a public hide or unhide that returned
    /// `false`. It is checked against the epoch, the tracked exact application
    /// and its own [`super::issuance::ApplicationIssuance`]; it is **not**
    /// refused by the frame gate, because stop seals that gate before the
    /// restore that may need this write.
    fn application_hidden(&mut self, request: &HiddenRequest) -> HiddenOutcome {
        let Self { runtime, system, clock, .. } = self;
        let Some(rt) = runtime.as_mut().filter(|rt| rt.epoch == request.epoch) else {
            return HiddenOutcome::Stale(StaleReason::WrongEpoch);
        };
        let Some(app) = rt.apps.get(&request.application) else {
            return HiddenOutcome::Stale(StaleReason::ApplicationGone);
        };
        let ax = system.ax();
        let element = &app.element;
        let outcome = match request.operation {
            HiddenOperation::Read => {
                if let Err(error) = ax.set_messaging_timeout(element, ELEMENT_TIMEOUT) {
                    return HiddenOutcome::NotIssued(timeout_refusal(error));
                }
                HiddenOutcome::Read(AxReading::classify(
                    ax.bool_attribute(element, BoolAttribute::Hidden),
                ))
            }
            HiddenOperation::Set(hidden) => {
                let Some(issuance) = request.issuance.as_ref() else {
                    return HiddenOutcome::NotIssued(NotIssuedReason::EmptyRequest);
                };
                if issuance.application() != request.application {
                    return HiddenOutcome::Stale(StaleReason::ApplicationGone);
                }
                if let Err(error) = ax.set_messaging_timeout(element, PRESS_TIMEOUT) {
                    return HiddenOutcome::NotIssued(timeout_refusal(error));
                }
                match issuance.commit(clock.now()) {
                    Err(CommitRefusal::Expired) => return HiddenOutcome::ExpiredBeforeSend,
                    Err(CommitRefusal::NotIssued(reason)) => {
                        return HiddenOutcome::NotIssued(reason);
                    }
                    Ok(()) => {}
                }
                let status = ax.set_hidden(element, hidden);
                let kind = match status {
                    Ok(()) => IssuedKind::Applied,
                    Err(AxError::CannotComplete) => IssuedKind::Uncertain,
                    Err(_) => IssuedKind::Rejected,
                };
                let observed = match ax.set_messaging_timeout(element, ELEMENT_TIMEOUT) {
                    Ok(()) => {
                        AxReading::classify(ax.bool_attribute(element, BoolAttribute::Hidden))
                    }
                    Err(error) => AxReading::Unknown(error),
                };
                HiddenOutcome::Written { kind, status, observed }
            }
        };
        if matches!(outcome, HiddenOutcome::Written {
            status: Err(AxError::ApiDisabled),
            ..
        }) {
            rt.note_error(AxError::ApiDisabled);
        }
        outcome
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one linear send path: validate, gate, send, classify"
    )]
    fn act(&mut self, request: &ActionRequest) -> WindowActionOutcome {
        let issuance = &request.issuance;
        let window = issuance.window();
        let Some(gate) = self.runtime.as_ref().map(|rt| Arc::clone(&rt.gate)) else {
            return WindowActionOutcome::Stale(StaleReason::WrongEpoch);
        };
        if let Err(reason) = gate.admit(issuance) {
            return WindowActionOutcome::NotIssued(reason);
        }
        if request.action == (WindowAction::SetFrame { position: None, size: None }) {
            return WindowActionOutcome::NotIssued(NotIssuedReason::EmptyRequest);
        }
        if self.clock.now() >= issuance.deadline() {
            return WindowActionOutcome::ExpiredBeforeSend;
        }
        let index = match self.validate(window) {
            Ok(index) => index,
            Err(Invalid::Stale(reason)) => return WindowActionOutcome::Stale(reason),
            Err(Invalid::Unverified(AxError::ApiDisabled)) => {
                return WindowActionOutcome::NotIssued(NotIssuedReason::PermissionDenied);
            }
            Err(Invalid::Unverified(error)) => {
                return WindowActionOutcome::NotIssued(NotIssuedReason::MembershipUnverified(
                    error,
                ));
            }
        };
        let Self { runtime, system, clock, .. } = self;
        let Some(rt) = runtime.as_mut() else {
            return WindowActionOutcome::Stale(StaleReason::WrongEpoch);
        };
        let Some(app) = rt.apps.get(&window.application()) else {
            return WindowActionOutcome::Stale(StaleReason::ApplicationGone);
        };
        let ax = system.ax();
        let element = &app.windows[index].element;
        let mut refusal: Option<CommitRefusal> = None;
        let mut commit = || match issuance.commit(&gate, clock.now()) {
            Ok(()) => true,
            Err(why) => {
                refusal = Some(why);
                false
            }
        };
        let outcome = match request.action {
            WindowAction::SetFrame { position, size } => {
                match write_frame_gated(ax, element, position, size, &mut commit) {
                    None => None,
                    Some(FrameWriteOutcome::NotSent(error)) => Some(Err(timeout_refusal(error))),
                    Some(FrameWriteOutcome::Attempted { position, size }) => {
                        let observed = (!matches!(
                            (position, size),
                            (WriteStatus::Failed(AxError::ApiDisabled), _)
                                | (_, WriteStatus::Failed(AxError::ApiDisabled))
                        ))
                        .then(|| read_frame(ax, element));
                        Some(Ok((
                            classify_frame_write(position, size),
                            NativeStatus::Frame { position, size },
                            observed,
                        )))
                    }
                }
            }
            WindowAction::Raise => match ax.set_messaging_timeout(element, PRESS_TIMEOUT) {
                Err(error) => Some(Err(timeout_refusal(error))),
                Ok(()) => {
                    if commit() {
                        let result = ax.raise(element);
                        let kind = match result {
                            Ok(()) => IssuedKind::Applied,
                            Err(AxError::CannotComplete) => IssuedKind::Uncertain,
                            Err(_) => IssuedKind::Rejected,
                        };
                        Some(Ok((kind, NativeStatus::Action(result), None)))
                    } else {
                        None
                    }
                }
            },
        };
        match outcome {
            None => match refusal {
                Some(CommitRefusal::Expired) => WindowActionOutcome::ExpiredBeforeSend,
                Some(CommitRefusal::NotIssued(reason)) => WindowActionOutcome::NotIssued(reason),
                None => WindowActionOutcome::NotIssued(NotIssuedReason::Cancelled),
            },
            Some(Err(reason)) => {
                if reason == NotIssuedReason::PermissionDenied {
                    rt.note_error(AxError::ApiDisabled);
                }
                WindowActionOutcome::NotIssued(reason)
            }
            Some(Ok((kind, native, observed))) => {
                let denied = matches!(native, NativeStatus::Action(Err(AxError::ApiDisabled)))
                    || matches!(
                        native,
                        NativeStatus::Frame { position, size }
                            if matches!(position, WriteStatus::Failed(AxError::ApiDisabled))
                                || matches!(size, WriteStatus::Failed(AxError::ApiDisabled))
                    );
                if denied {
                    rt.note_error(AxError::ApiDisabled);
                }
                WindowActionOutcome::Issued { kind, native, observed }
            }
        }
    }
}

fn timeout_refusal(error: AxError) -> NotIssuedReason {
    if error == AxError::ApiDisabled {
        NotIssuedReason::PermissionDenied
    } else {
        NotIssuedReason::TimeoutSetFailed(error)
    }
}

/// Adopts `element` as a tracked window of `app`: dedup, register, mint a
/// serial and read its snapshot. Registration happens before the first
/// attribute read.
fn adopt<S: WindowSystem>(
    system: &S,
    adapter: AdapterId,
    epoch: RuntimeEpoch,
    next_serial: &mut u64,
    app: &mut AppState<S>,
    element: Element<S>,
    read_tabs: bool,
) -> Adopted {
    let ax = system.ax();
    if let Some(index) =
        app.windows.iter().position(|held| ax.same_element(&held.element, &element))
    {
        return Adopted::Existing(index);
    }
    if app.windows.len() >= MAX_WINDOWS_PER_APPLICATION {
        return Adopted::Capped;
    }
    if let Err(error) = ax.set_messaging_timeout(&element, ELEMENT_TIMEOUT) {
        return if error == AxError::InvalidElement {
            Adopted::Gone
        } else {
            Adopted::NotObservable
        };
    }
    let mut key: Option<ElementKey> = None;
    let mut observed = true;
    for notification in WINDOW_NOTIFICATIONS {
        match app.observer.add_notification(&element, notification) {
            Ok(added) => {
                key = Some(added);
                if !app.regs.contains(&(added, notification)) {
                    app.regs.push((added, notification));
                }
            }
            Err(AxError::InvalidElement) => {
                if let Some(held) = key {
                    app.observer.forget_destroyed(held);
                    app.regs.retain(|(registered, _)| *registered != held);
                }
                return Adopted::Gone;
            }
            Err(_) => observed = false,
        }
    }
    let Some(key) = key else { return Adopted::NotObservable };
    let serial = *next_serial;
    *next_serial += 1;
    let window = WindowInstance::new(adapter, epoch, app.id, serial);
    let snapshot = read_snapshot(ax, &element, window, observed, read_tabs);
    app.windows.push(Tracked {
        serial,
        element,
        key,
        cg: snapshot.cg_window_id,
        observed,
    });
    Adopted::New(Box::new(snapshot))
}

/// Reads a fresh snapshot of an already tracked window (same token) and
/// refreshes its `CGWindowID` metadata.
fn reread<S: WindowSystem>(
    system: &S,
    adapter: AdapterId,
    epoch: RuntimeEpoch,
    app: &mut AppState<S>,
    index: usize,
    read_tabs: bool,
) -> WindowSnapshot {
    let held = &app.windows[index];
    let window = WindowInstance::new(adapter, epoch, app.id, held.serial);
    let snapshot = read_snapshot(system.ax(), &held.element, window, held.observed, read_tabs);
    let held = &mut app.windows[index];
    held.cg = snapshot.cg_window_id.or(held.cg);
    snapshot
}

/// Removes a tracked window's observer registrations. Each is removed with
/// `remove_notification`; `InvalidElement` proves the element gone and
/// `forget_destroyed` releases it. Any other failure keeps that registration
/// in `regs`, so the application's teardown retries it and quarantines it if it
/// still fails.
fn unregister<S: WindowSystem>(app: &mut AppState<S>, key: ElementKey) {
    let mine: Vec<(ElementKey, AxNotification)> =
        app.regs.iter().filter(|(held, _)| *held == key).copied().collect();
    let mut proven = false;
    let mut failed = Vec::new();
    for (held, notification) in mine {
        match app.observer.remove_notification(held, notification) {
            Ok(()) => {}
            Err(AxError::InvalidElement) => {
                proven = true;
                break;
            }
            Err(_) => failed.push((held, notification)),
        }
    }
    app.regs.retain(|(held, _)| *held != key);
    if proven {
        app.observer.forget_destroyed(key);
    } else {
        app.regs.extend(failed);
    }
}

/// Starts the retirement stage of an enumeration whose last page was empty.
/// Returns whether the enumeration ended.
///
/// The listing is **complete**: windows tracked by the owner but absent from
/// it are retired and reported `Destroyed` before `EnumerationComplete`, so the
/// consumer's set of live windows equals the listing. Retirement is decided
/// from one fresh full listing, never from the paged pass alone (a creation
/// can shift pages). A listing that cannot be read fresh, or that is at the
/// per-application cap, retires nothing: it ends as an abort or a truncated
/// completion, which the consumer never treats as proof of destruction. The
/// fresh read is one element of the chunk budget; the per-window work then
/// continues in [`retire_step`], one element at a time, so cancellation, the
/// deadline and the seal are honoured between calls.
fn begin_retirement<S: WindowSystem>(
    system: &S,
    rt: &mut Runtime<S>,
    en: &mut Enumeration<Element<S>>,
) -> bool {
    let ax = system.ax();
    let Some(app) = rt.apps.get(&en.app) else { return true };
    let unseen: VecDeque<u64> = app
        .windows
        .iter()
        .map(|held| held.serial)
        .filter(|serial| !en.seen.contains(serial))
        .collect();
    if unseen.is_empty() {
        rt.emit(WindowEventKind::EnumerationComplete {
            application: en.app,
            windows: en.adopted,
            truncated: false,
        });
        return true;
    }
    let fresh = ax
        .set_messaging_timeout(&app.element, ELEMENT_TIMEOUT)
        .and_then(|()| ax.windows(&app.element, 0, MAX_WINDOWS_PER_APPLICATION));
    match fresh {
        Err(error) => {
            rt.note_error(error);
            rt.emit(WindowEventKind::EnumerationAborted {
                application: en.app,
                reason: EnumerationAbort::Failed(error),
            });
            true
        }
        Ok(list) if list.len() >= MAX_WINDOWS_PER_APPLICATION => {
            rt.emit(WindowEventKind::EnumerationComplete {
                application: en.app,
                windows: en.adopted,
                truncated: true,
            });
            true
        }
        Ok(list) => {
            en.retire = Some(Retire { list, unseen });
            false
        }
    }
}

/// Processes one missed window: re-reads it when the fresh listing holds it,
/// retires it when not. Returns whether the enumeration ended.
fn retire_step<S: WindowSystem>(
    system: &S,
    adapter: AdapterId,
    read_tabs: bool,
    rt: &mut Runtime<S>,
    en: &mut Enumeration<Element<S>>,
) -> bool {
    let epoch = rt.epoch;
    let ax = system.ax();
    let Some(retire) = en.retire.as_mut() else { return true };
    let Some(app) = rt.apps.get_mut(&en.app) else {
        return true;
    };
    let Some(serial) = retire.unseen.pop_front() else {
        rt.emit(WindowEventKind::EnumerationComplete {
            application: en.app,
            windows: en.adopted,
            truncated: false,
        });
        return true;
    };
    let Some(index) = app.windows.iter().position(|held| held.serial == serial) else {
        return false;
    };
    let present = retire
        .list
        .iter()
        .any(|candidate| ax.same_element(candidate, &app.windows[index].element));
    if present {
        // Created while the pages were read: report it.
        let snapshot = reread::<S>(system, adapter, epoch, app, index, read_tabs);
        en.adopted += 1;
        rt.emit(WindowEventKind::Snapshot {
            snapshot,
            origin: SnapshotOrigin::Enumeration,
        });
    } else {
        let gone = app.windows.remove(index);
        unregister(app, gone.key);
        rt.emit(WindowEventKind::Window {
            window: WindowInstance::new(adapter, epoch, en.app, gone.serial),
            change: WindowChange::Destroyed,
        });
    }
    false
}

/// Resolves what a processed backlog deferred: a focus/main notification that
/// named a window which is tracked **now** (it may have been created later in
/// the same backlog) is reported for its token, then every callback-delivered
/// key is released.
fn settle_backlog<S: WindowSystem>(
    adapter: AdapterId,
    epoch: RuntimeEpoch,
    id: ApplicationId,
    app: &mut AppState<S>,
) -> Vec<WindowEventKind> {
    let mut kinds = Vec::new();
    for (key, notification) in std::mem::take(&mut app.focus) {
        if let Some(held) = app.windows.iter().find(|held| held.key == key) {
            if let Some(change) = change_for(notification) {
                kinds.push(WindowEventKind::Window {
                    window: WindowInstance::new(adapter, epoch, id, held.serial),
                    change,
                });
            }
        } else if app.observer.has_pending_event(key) {
            // A later, not yet drained event (its creation) still names it.
            app.focus.push((key, notification));
        }
    }
    for key in std::mem::take(&mut app.release) {
        if app.observer.has_pending_event(key) {
            // Not yet drained events still name it: keep it until they ran.
            if !app.release.contains(&key) {
                app.release.push(key);
            }
        } else {
            app.observer.release(key);
        }
    }
    kinds
}

const fn is_focus_notification(notification: AxNotification) -> bool {
    matches!(
        notification,
        AxNotification::FocusedWindowChanged | AxNotification::MainWindowChanged
    )
}

/// A focus/main notification of this kind supersedes every older deferred one
/// of the same kind: the consumer must end on the latest.
fn supersede_focus<S: WindowSystem>(app: &mut AppState<S>, notification: AxNotification) {
    if is_focus_notification(notification) {
        app.focus.retain(|(_, deferred)| *deferred != notification);
    }
}

/// Turns one recorded notification into pure event kinds. Runs on the owner,
/// outside the AX callback.
#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive notification dispatch"
)]
fn ax_event<S: WindowSystem>(
    system: &S,
    adapter: AdapterId,
    read_tabs: bool,
    rt: &mut Runtime<S>,
    id: ApplicationId,
    event: AxEvent,
) -> Vec<WindowEventKind> {
    let epoch = rt.epoch;
    let Some(app) = rt.apps.get_mut(&id) else {
        return Vec::new();
    };
    let key = event.element;
    let notification = event.notification;
    let known = app.windows.iter().position(|held| held.key == key);
    let token = |serial: u64| WindowInstance::new(adapter, epoch, id, serial);
    match notification {
        AxNotification::ElementDestroyed => {
            if let Some(index) = known {
                let gone = app.windows.remove(index);
                app.observer.forget_destroyed(gone.key);
                app.regs.retain(|(registered, _)| *registered != gone.key);
                vec![WindowEventKind::Window {
                    window: token(gone.serial),
                    change: WindowChange::Destroyed,
                }]
            } else {
                app.release.push(key);
                Vec::new()
            }
        }
        AxNotification::ApplicationHidden => {
            vec![WindowEventKind::Application {
                application: id,
                change: ApplicationChange::Hidden,
            }]
        }
        AxNotification::ApplicationShown => {
            vec![WindowEventKind::Application {
                application: id,
                change: ApplicationChange::Shown,
            }]
        }
        _ => {
            if let Some(index) = known {
                if notification == AxNotification::WindowCreated {
                    return Vec::new();
                }
                supersede_focus(app, notification);
                let serial = app.windows[index].serial;
                return change_for(notification)
                    .map(|change| WindowEventKind::Window { window: token(serial), change })
                    .into_iter()
                    .collect();
            }
            if app.app_key == Some(key) {
                supersede_focus(app, notification);
                let change = match notification {
                    AxNotification::FocusedWindowChanged => {
                        Some(ApplicationChange::FocusedWindowChanged)
                    }
                    AxNotification::MainWindowChanged => Some(ApplicationChange::MainWindowChanged),
                    _ => None,
                };
                return change
                    .map(|change| WindowEventKind::Application { application: id, change })
                    .into_iter()
                    .collect();
            }
            // Only a creation adopts a new window. A delayed change for an
            // element this owner does not track (for example one that was just
            // destroyed) mints no token. A focus/main delivery is remembered
            // until the backlog is done, because the creation of that very
            // window may follow it; the key is released only after that.
            if notification != AxNotification::WindowCreated {
                if is_focus_notification(notification) {
                    // Latest wins: an older deferred change of this kind is stale.
                    supersede_focus(app, notification);
                    app.focus.push((key, notification));
                }
                app.release.push(key);
                return Vec::new();
            }
            let Some(element) = app.observer.element(key) else {
                // The creation names an element that is no longer held: never
                // drop it silently, ask the consumer to reconcile.
                return vec![WindowEventKind::ObserverOverflow { application: id }];
            };
            // The element must still be a member of the application's windows.
            let ax = system.ax();
            let member = ax
                .set_messaging_timeout(&app.element, ELEMENT_TIMEOUT)
                .and_then(|()| ax.windows(&app.element, 0, MAX_WINDOWS_PER_APPLICATION))
                .map(|list| list.iter().any(|held| ax.same_element(held, &element)));
            match member {
                Ok(true) => {}
                Ok(false) => {
                    app.release.push(key);
                    return Vec::new();
                }
                Err(_) => {
                    // Unverifiable: ask the consumer to reconcile by refreshing.
                    app.release.push(key);
                    return vec![WindowEventKind::ObserverOverflow { application: id }];
                }
            }
            let adopted = adopt::<S>(
                system,
                adapter,
                epoch,
                &mut rt.next_serial,
                app,
                element,
                read_tabs,
            );
            match adopted {
                Adopted::New(snapshot) => {
                    app.release.push(key);
                    let window = snapshot.window;
                    let mut kinds = vec![WindowEventKind::Snapshot {
                        snapshot: *snapshot,
                        origin: SnapshotOrigin::Created,
                    }];
                    // Focus/main changes deferred for this very window are
                    // reported right after its creation, at their original
                    // position relative to later notifications.
                    let (mine, rest): (Vec<_>, Vec<_>) =
                        std::mem::take(&mut app.focus).into_iter().partition(|(k, _)| *k == key);
                    app.focus = rest;
                    for (_, deferred) in mine {
                        if let Some(change) = change_for(deferred) {
                            kinds.push(WindowEventKind::Window { window, change });
                        }
                    }
                    kinds
                }
                Adopted::Existing(_) | Adopted::Gone | Adopted::Capped | Adopted::NotObservable => {
                    app.release.push(key);
                    Vec::new()
                }
            }
        }
    }
}

// Native-backed window state never crosses threads: a core over the real
// system is neither `Send` nor `Sync`.
mod owner_local {
    use super::{Core, SystemWindowsCheck};

    trait NotSend<A> {
        fn check() {}
    }
    impl<T: ?Sized> NotSend<()> for T {}
    impl<T: ?Sized + Send> NotSend<u8> for T {}

    trait NotSync<A> {
        fn check() {}
    }
    impl<T: ?Sized> NotSync<()> for T {}
    impl<T: ?Sized + Sync> NotSync<u8> for T {}

    const _: fn() = || {
        <Core<SystemWindowsCheck> as NotSend<_>>::check();
        <Core<SystemWindowsCheck> as NotSync<_>>::check();
    };
}

type SystemWindowsCheck = super::system::SystemWindows;
