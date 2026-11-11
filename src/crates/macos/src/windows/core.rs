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
//! 2. Take the pending frames (latest-frame-wins mailbox) and write at most
//!    `MAX_FRAMES_PER_TURN` of them, **before** the commands: a command such
//!    as a retarget `Snapshot` then reads the window after the old plan's
//!    pending frame was written. The bound keeps a burst of frames from
//!    starving commands. A stop, untrack or terminate still answers every
//!    frame left pending exactly once, and a sealed gate makes frames inert.
//! 3. Run at most [`MAX_COMMANDS_PER_TURN`] queued commands.
//! 4. Settle leases, drain observers again, then run **one** enumeration chunk.
//!
//! A turn that leaves work (more commands or enumeration) reports
//! `active`, and the driver parks with a zero timeout instead of blocking:
//! that is the re-post. An idle turn parks without any timeout.

use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use stache_sdk::CancellationToken;

use super::commands::{
    ActionRequest, CleanupReport, CloseOutcome, Command, FocusOutcome, FrameAck, FrameReply,
    FrameRequest, FrameStage, HiddenOperation, HiddenOutcome, HiddenRequest, InstallError,
    InstallRequest, LeaseReport, Reply, Resolution, SnapshotOutcome, StopOutcome, TrackOutcome,
    TrackRequest, WindowAction,
};
use super::events::{
    ApplicationChange, Capability, EnhancedUiNote, EnumerationAbort, EventQueue, SnapshotOrigin,
    TabBar, TabBarId, TabBarRead, TabInfo, TabMember, WindowChange, WindowEvent, WindowEventKind,
    WindowSnapshot,
};
use super::frames::{FrameMailbox, MAX_FRAMES_PER_TURN, OwnedMailbox, PendingFrame, done};
use super::identity::{AdapterId, RuntimeEpoch, WindowInstance};
use super::issuance::{
    CommitRefusal, GenerationCell, IssuedKind, NativeStatus, NotIssuedReason, StaleReason,
    WindowActionOutcome,
};
use super::lease::{Context as LeaseContext, Eui, NoVoiceOver, Settled, VoiceOverProbe};
use super::system::{WindowClock, WindowObserver, WindowSystem};
use crate::applications::ApplicationId;
use crate::ax::{
    AxApi, AxError, AxEvent, AxNotification, AxReading, BoolAttribute, ELEMENT_TIMEOUT, ElementKey,
    FrameRead, FrameWriteOutcome, PRESS_TIMEOUT, TabGroupRead, TabGroupSearch, WindowAx,
    WindowIdCapability, WindowString, WriteOrder, WriteStatus, find_tab_group, read_frame,
    read_tab_bar_shape, read_tab_group, resizable, write_frame_gated,
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
/// Distinct tab bars one application may hold at once.
const MAX_TAB_BARS: usize = 16;
/// The most suspended (alive, unlisted) windows one application keeps; beyond
/// it the oldest are retired.
pub const MAX_SUSPENDED_PER_APPLICATION: usize = 32;
/// Settled scans a tab bar no tracked window carries survives, so the window
/// that takes the bar over after a close can still be recognized.
const ORPHAN_SCANS: u8 = 3;

/// Which optional reads the owner performs.
#[derive(Clone, Copy, Debug)]
pub(super) struct Reads {
    /// The legacy tab member read (`CGWindowID` keyed; off in production).
    pub tabs: bool,
    /// The tab bar identity read.
    pub tab_bars: bool,
}

/// One retained `AXTabGroup` element and the opaque id minted for it.
struct BarEntry<E> {
    id: TabBarId,
    group: E,
    /// Settled scans in a row that no tracked window carried this bar.
    orphaned: u8,
}

/// The tab bars of one application, owner-confined like every element here.
///
/// Two windows get the same [`TabBarId`] exactly when their tab group elements
/// are `CFEqual`; nothing else (frame, title, `CGWindowID`) is ever compared.
/// Bounded by [`MAX_TAB_BARS`]; released with the application.
struct TabBars<E> {
    entries: Vec<BarEntry<E>>,
    next: u64,
}

impl<E> Default for TabBars<E> {
    fn default() -> Self { Self { entries: Vec::new(), next: 1 } }
}

impl<E> TabBars<E> {
    /// The id for `group`: the existing one when an equal element is held, else
    /// a freshly minted one. `None` when the table is full of referenced bars.
    fn id_for(
        &mut self,
        same: impl Fn(&E, &E) -> bool,
        group: E,
        referenced: &HashSet<TabBarId>,
    ) -> Option<TabBarId> {
        if let Some(position) = self.entries.iter().position(|held| same(&held.group, &group)) {
            let mut entry = self.entries.remove(position);
            entry.orphaned = 0;
            let id = entry.id;
            self.entries.push(entry);
            return Some(id);
        }
        if self.entries.len() >= MAX_TAB_BARS {
            let victim = self.entries.iter().position(|held| !referenced.contains(&held.id))?;
            self.entries.remove(victim);
        }
        let id = TabBarId::new(self.next);
        self.next += 1;
        self.entries.push(BarEntry { id, group, orphaned: 0 });
        Some(id)
    }

    /// Ages the bars no tracked window carries and drops the old ones.
    fn prune(&mut self, referenced: &HashSet<TabBarId>) {
        for entry in &mut self.entries {
            if referenced.contains(&entry.id) {
                entry.orphaned = 0;
            } else {
                entry.orphaned = entry.orphaned.saturating_add(1);
            }
        }
        self.entries.retain(|entry| entry.orphaned < ORPHAN_SCANS);
    }
}

/// Reads a window's tab bar identity on the owner: a bounded search for its
/// `AXTabGroup`, the bar's shape, and the id of the retained group element.
fn probe_tab_bar<A: WindowAx>(
    ax: &A,
    bars: &mut TabBars<A::Element>,
    referenced: &HashSet<TabBarId>,
    window: &A::Element,
) -> TabBarRead {
    match find_tab_group(ax, window) {
        TabGroupSearch::Absent => TabBarRead::None,
        TabGroupSearch::Unknown(_) => TabBarRead::Unreadable,
        TabGroupSearch::Found(group) => {
            let Ok(shape) = read_tab_bar_shape(ax, &group, TAB_LIMIT) else {
                return TabBarRead::Unreadable;
            };
            bars.id_for(|a, b| ax.same_element(a, b), group, referenced).map_or(
                TabBarRead::Unreadable,
                |id| {
                    TabBarRead::Bar(TabBar {
                        id,
                        count: shape.count,
                        selected: shape.selected,
                    })
                },
            )
        }
    }
}

const fn bar_id(read: TabBarRead) -> Option<TabBarId> {
    match read {
        TabBarRead::Bar(bar) => Some(bar.id),
        _ => None,
    }
}

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
    /// The last tab bar identity read (cached: geometry reads reuse it).
    tab: TabBarRead,
    /// Set while the window is absent from `AXWindows` yet its element is
    /// alive (an inactive native tab); the value orders suspensions.
    suspended: Option<u64>,
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
    /// The retained tab group elements and their opaque ids.
    tab_bars: TabBars<Element<S>>,
    /// Orders the suspensions of this application.
    suspensions: u64,
    /// The `AXEnhancedUserInterface` lease state.
    eui: Eui,
}

impl<S: WindowSystem> AppState<S> {
    /// The tab bars some tracked window currently carries.
    fn referenced_bars(&self) -> HashSet<TabBarId> {
        self.windows.iter().filter_map(|held| bar_id(held.tab)).collect()
    }
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
    /// Listed windows skipped because their id or tab bar was unreadable.
    transient: usize,
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

/// The per-epoch counters every shard of one adapter shares: serials stay
/// unique and event sequences stay monotonic across shards.
#[derive(Clone, Debug)]
pub(super) struct EpochCounters {
    /// The next window serial to hand out.
    serial: Arc<AtomicU64>,
    /// The last event sequence handed out.
    sequence: Arc<AtomicU64>,
}

impl EpochCounters {
    pub(super) fn new() -> Self {
        Self {
            serial: Arc::new(AtomicU64::new(1)),
            sequence: Arc::new(AtomicU64::new(0)),
        }
    }
}

struct Runtime<S: WindowSystem> {
    epoch: RuntimeEpoch,
    gate: Arc<GenerationCell>,
    queue: Arc<EventQueue>,
    counters: EpochCounters,
    apps: HashMap<ApplicationId, AppState<S>>,
    enumerations: VecDeque<Enumeration<Element<S>>>,
    /// Drained, not yet processed observer events in arrival order.
    backlog: VecDeque<(ApplicationId, AxEvent)>,
    degraded: bool,
}

impl<S: WindowSystem> Runtime<S> {
    fn emit(&self, kind: WindowEventKind) {
        let sequence = self.counters.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.queue.push(WindowEvent {
            epoch: self.epoch,
            sequence,
            kind,
        });
    }

    fn emit_notes(&self, application: ApplicationId, notes: Vec<EnhancedUiNote>) {
        for note in notes {
            self.emit(WindowEventKind::EnhancedUi { application, note });
        }
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
    /// The next instant an open lease needs the shard (its idle grace ends or
    /// a failed restore is retried); `None` parks without a timeout.
    pub deadline: Option<Instant>,
}

/// What settling the open leases of a shard found.
#[derive(Debug, Default)]
struct LeaseSummary {
    /// Leases closed in this step.
    restored: usize,
    /// Applications whose lease could not be restored.
    owed: Vec<ApplicationId>,
    /// The next instant a lease needs the shard.
    deadline: Option<Instant>,
}

impl LeaseSummary {
    fn wake_at(&mut self, at: Instant) {
        self.deadline = Some(self.deadline.map_or(at, |held| held.min(at)));
    }
}

/// Why a token failed revalidation.
enum Invalid {
    Stale(StaleReason),
    Unverified(AxError),
}

impl Invalid {
    /// The action outcome of a failed revalidation: nothing was sent.
    const fn outcome(&self) -> WindowActionOutcome {
        match *self {
            Self::Stale(reason) => WindowActionOutcome::Stale(reason),
            Self::Unverified(AxError::ApiDisabled) => {
                WindowActionOutcome::NotIssued(NotIssuedReason::PermissionDenied)
            }
            Self::Unverified(error) => {
                WindowActionOutcome::NotIssued(NotIssuedReason::MembershipUnverified(error))
            }
        }
    }
}

/// The owner state machine.
pub(super) struct Core<S: WindowSystem> {
    system: S,
    adapter: AdapterId,
    clock: Arc<dyn WindowClock>,
    reads: Reads,
    #[cfg(test)]
    next_epoch: u64,
    runtime: Option<Runtime<S>>,
    /// The last stopped epoch and its gate, so late frames of it answer why.
    stopped: Option<(RuntimeEpoch, Arc<GenerationCell>)>,
    quarantine: Vec<Quarantined<S>>,
    poisoned: bool,
    finished: bool,
    frames: OwnedMailbox,
    voice_over: Arc<dyn VoiceOverProbe>,
}

const UNAVAILABLE: WindowActionOutcome =
    WindowActionOutcome::NotIssued(NotIssuedReason::OwnerUnavailable);

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
    // An exited process fails every removal (`CannotComplete`), yet its
    // registrations died with it: nothing is left to owe.
    if !remaining.is_empty() && app.observer.target_exited() {
        for (key, _) in std::mem::take(&mut remaining) {
            app.observer.forget_destroyed(key);
        }
    }
    if remaining.is_empty() {
        Ok(())
    } else {
        app.regs = remaining;
        Err(app)
    }
}

/// Restores an open enhanced user interface lease now (the grace does not
/// apply), then removes every registration. A lease that cannot be restored
/// keeps the application: it is returned for the quarantine with its observer
/// detached, and a retry restores it before removing the registrations. A
/// terminated application drops its lease with no write.
#[allow(
    clippy::result_large_err,
    reason = "the error hands the whole application to the quarantine unchanged"
)]
fn release_app<S: WindowSystem>(
    ax: &S::Ax,
    now: Instant,
    mut app: AppState<S>,
    notes: &mut Vec<EnhancedUiNote>,
) -> Result<(), AppState<S>> {
    if matches!(
        app.eui.settle(ax, &app.element, now, false, true, notes),
        Settled::Owed(_)
    ) {
        app.observer.detach();
        return Err(app);
    }
    teardown(app)
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
        | AxNotification::ApplicationShown
        | AxNotification::ExposeShowAllWindows
        | AxNotification::ExposeShowFrontWindows
        | AxNotification::ExposeShowDesktop
        | AxNotification::ExposeExit => None,
    }
}

fn read_snapshot<A: WindowAx>(
    ax: &A,
    element: &A::Element,
    window: WindowInstance,
    observed: bool,
    read_tabs: bool,
    tab_bar: TabBarRead,
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
        tab_bar,
        observed,
    }
}

enum Adopted {
    /// Already tracked: the index into the application's windows, and whether
    /// it was suspended and is listed again.
    Existing {
        index: usize,
        returned: bool,
    },
    New(Box<WindowSnapshot>),
    Gone,
    Capped,
    NotObservable,
    /// Listed, but its id and tab bar could not be read yet: not adopted.
    Transient,
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
            reads: Reads {
                tabs: read_tabs,
                tab_bars: false,
            },
            #[cfg(test)]
            next_epoch: 0,
            runtime: None,
            stopped: None,
            quarantine: Vec::new(),
            poisoned: false,
            finished: false,
            frames: OwnedMailbox::new(),
            voice_over: Arc::new(NoVoiceOver),
        }
    }

    /// The shard clock's current reading.
    pub(super) fn now(&self) -> Instant { self.clock.now() }

    /// Uses `probe` to decide whether `VoiceOver` is running.
    pub(super) fn with_voice_over(mut self, probe: Arc<dyn VoiceOverProbe>) -> Self {
        self.voice_over = probe;
        self
    }

    /// Serves the router's shared frame mailbox instead of a private one.
    pub(super) fn with_frames(mut self, shared: Arc<Mutex<FrameMailbox>>) -> Self {
        self.frames = OwnedMailbox::from_shared(shared);
        self
    }

    /// The mailbox handle the router submits frames to.
    #[cfg(test)]
    pub(super) fn frame_mailbox(&self) -> Arc<Mutex<FrameMailbox>> { self.frames.shared() }

    /// Turns the tab bar identity reads on or off.
    pub(super) const fn with_tab_bars(mut self, on: bool) -> Self {
        self.reads.tab_bars = on;
        self
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
            return Turn {
                active: false,
                finished: true,
                deadline: None,
            };
        }
        let mut budget = EventBudget { heavy: 0, started: None };
        self.pump(&mut budget);
        // Frames first: a retarget `Snapshot` queued behind an old plan's
        // pending frame must observe that frame as written.
        let more_frames = self.run_frames();
        let mut more = false;
        for index in 0..MAX_COMMANDS_PER_TURN {
            match commands.try_recv() {
                Ok(command) => self.handle_guarded(command),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.stop_everything();
                    self.finished = true;
                    let gate = self.gate();
                    self.answer_frames(self.frames.close(), gate.as_deref(), UNAVAILABLE);
                    return Turn {
                        active: false,
                        finished: true,
                        deadline: None,
                    };
                }
            }
            if index + 1 == MAX_COMMANDS_PER_TURN {
                more = true;
            }
        }
        if self.finished {
            let gate = self.gate();
            self.answer_frames(self.frames.close(), gate.as_deref(), UNAVAILABLE);
            return Turn {
                active: false,
                finished: true,
                deadline: None,
            };
        }
        let leases = if self.poisoned {
            LeaseSummary::default()
        } else {
            self.settle_leases(false)
        };
        self.pump(&mut budget);
        if !self.poisoned {
            let _ = catch_unwind(AssertUnwindSafe(|| self.step_enumeration()));
        }
        Turn {
            active: more
                || more_frames
                || self
                    .runtime
                    .as_ref()
                    .is_some_and(|rt| !rt.enumerations.is_empty() || !rt.backlog.is_empty()),
            finished: self.finished,
            deadline: leases.deadline,
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
            self.restore_after_panic();
        }
    }

    fn handle(&mut self, command: Command) {
        match command {
            #[cfg(test)]
            Command::Install { request, reply } => answer(reply, self.install(request)),
            Command::InstallShard {
                request,
                epoch,
                counters,
                reply,
            } => {
                answer(reply, self.install_shared(request, epoch, counters));
            }
            Command::Track { epoch, request, reply } => {
                answer(reply, self.track(epoch, request));
            }
            Command::Untrack { epoch, application, reply } => {
                let gate = self.gate();
                let outcome = self.untrack(epoch, application);
                let pending = self.frames.drain_application(application);
                let gone = WindowActionOutcome::Stale(StaleReason::ApplicationGone);
                self.answer_frames(pending, gate.as_deref(), gone);
                answer(reply, outcome);
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
            Command::FocusedWindow { epoch, application, reply } => {
                answer(reply, self.focused_window(epoch, application));
            }
            Command::Act { request, reply } => answer(reply, self.act(&request)),
            Command::Hidden { request, reply } => answer(reply, self.application_hidden(&request)),
            Command::Snapshot { window, reply } => answer(reply, self.snapshot(window)),
            #[cfg(test)]
            Command::Resolve { cg_window_id, reply } => {
                answer(reply, self.resolve(cg_window_id));
            }
            Command::ResolveRaw { cg_window_id, reply } => {
                answer(reply, self.resolve_raw(cg_window_id));
            }
            Command::Stop { epoch, reply } => {
                let gate = self.gate();
                let outcome = self.stop(epoch);
                if outcome != StopOutcome::NotCurrent {
                    self.answer_frames(self.frames.drain_all(), gate.as_deref(), UNAVAILABLE);
                }
                answer(reply, outcome);
            }
            Command::Retry { reply } => answer(reply, self.retry_cleanup()),
            Command::RestoreLeases { reply } => {
                let summary = self.settle_leases(true);
                answer(reply, LeaseReport {
                    restored: summary.restored,
                    owed: summary.owed,
                });
            }
            Command::Terminate { reply } => {
                let gate = self.gate();
                let outcome = self.terminate();
                self.answer_frames(self.frames.drain_all(), gate.as_deref(), UNAVAILABLE);
                answer(reply, outcome);
            }
        }
    }

    // ---- lifecycle -----------------------------------------------------

    #[cfg(test)]
    fn install(&mut self, request: InstallRequest) -> Result<RuntimeEpoch, InstallError> {
        self.next_epoch += 1;
        let epoch = RuntimeEpoch::new(self.next_epoch);
        self.install_shared(request, epoch, EpochCounters::new())
    }

    /// Installs an epoch minted elsewhere (the shard router) with counters its
    /// sibling shards share.
    fn install_shared(
        &mut self,
        request: InstallRequest,
        epoch: RuntimeEpoch,
        counters: EpochCounters,
    ) -> Result<RuntimeEpoch, InstallError> {
        if let Some(rt) = &self.runtime {
            return Err(InstallError::AlreadyInstalled(rt.epoch));
        }
        if !self.quarantine.is_empty() {
            return Err(InstallError::Quarantined {
                applications: self.quarantine.len(),
                registrations: self.quarantined_registrations(),
            });
        }
        self.stopped = None;
        self.runtime = Some(Runtime {
            epoch,
            gate: request.gate,
            queue: request.queue,
            counters,
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
        let Some(mut rt) = self.runtime.take() else { return 0 };
        rt.gate.seal();
        self.stopped = Some((rt.epoch, Arc::clone(&rt.gate)));
        drop(std::mem::take(&mut rt.enumerations));
        let mut ordered: Vec<_> = std::mem::take(&mut rt.apps).into_iter().collect();
        ordered.sort_by_key(|(id, _)| *id);
        let mut cleaned = 0;
        for (id, app) in ordered {
            let mut notes = Vec::new();
            let released = release_app(self.system.ax(), self.clock.now(), app, &mut notes);
            rt.emit_notes(id, notes);
            match released {
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
            // The epoch is over: a restore here has no sink for its notes.
            match release_app(self.system.ax(), self.clock.now(), entry.app, &mut Vec::new()) {
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
        // A shard with no application left ends without sealing: its gate is
        // shared with the shards that are still running.
        if self.runtime.as_ref().is_some_and(|rt| rt.apps.is_empty()) {
            self.runtime = None;
        } else if let Some(epoch) = self.runtime.as_ref().map(|rt| rt.epoch) {
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
            tab_bars: TabBars::default(),
            suspensions: 0,
            eui: Eui::default(),
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
            transient: 0,
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
            transient: 0,
        });
        TrackOutcome::Tracking
    }

    fn focused_window(&mut self, epoch: RuntimeEpoch, application: ApplicationId) -> FocusOutcome {
        let adapter = self.adapter;
        let Some(rt) = self.runtime.as_mut().filter(|rt| rt.epoch == epoch) else {
            return FocusOutcome::WrongEpoch;
        };
        let Some(app) = rt.apps.get(&application) else {
            return FocusOutcome::WrongEpoch;
        };
        match focused_token(&self.system, adapter, epoch, app) {
            Ok(Some(window)) => FocusOutcome::Resolved(window),
            Ok(None) => FocusOutcome::Unresolved,
            Err(error) => {
                rt.note_error(error);
                FocusOutcome::Failed(error)
            }
        }
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
            let mut notes = Vec::new();
            let released = release_app(self.system.ax(), self.clock.now(), app, &mut notes);
            rt.emit_notes(application, notes);
            match released {
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
            reads,
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
            let kinds = ax_event(system, *adapter, *reads, rt, id, event);
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
            reads,
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
                if retire_step(system, *adapter, *reads, rt, &mut en) {
                    return;
                }
                continue;
            }
            if en.pending.is_empty() {
                let Some(app) = rt.apps.get(&en.app) else { return };
                if en.index >= MAX_WINDOWS_PER_APPLICATION {
                    finish_enumeration(rt, &en, true);
                    return;
                }
                let want = (ENUMERATION_CHUNK_ELEMENTS - processed)
                    .min(MAX_WINDOWS_PER_APPLICATION - en.index);
                // An empty page retires live windows, so a zero-size request
                // must never be mistaken for the end of the list.
                debug_assert!(want > 0, "paging with an empty limit");
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
                &rt.counters.serial,
                app,
                element,
                *reads,
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
                Adopted::Existing { index, .. } => {
                    // A listed, already tracked window is still reported: the
                    // consumer treats an enumeration as the set of live windows.
                    let snapshot = reread::<S>(system, *adapter, rt.epoch, app, index, *reads);
                    en.adopted += 1;
                    en.seen.insert(snapshot.window.serial());
                    rt.emit(WindowEventKind::Snapshot {
                        snapshot,
                        origin: SnapshotOrigin::Enumeration,
                    });
                }
                Adopted::Transient => en.transient += 1,
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
        let next_serial = rt.counters.serial.load(Ordering::SeqCst);
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
        // Not a member any more: suspend it while its element lives, retire the
        // token otherwise, and tell the consumer.
        let kinds = depart::<S>(system, window.adapter(), window.epoch(), app, index);
        let suspended = app.windows.iter().any(|held| held.serial == window.serial());
        for kind in kinds {
            rt.emit(kind);
        }
        Err(Invalid::Stale(if suspended {
            StaleReason::Suspended
        } else {
            StaleReason::NotMember
        }))
    }

    fn snapshot(&mut self, window: WindowInstance) -> SnapshotOutcome {
        match self.validate(window) {
            Err(Invalid::Stale(reason)) => SnapshotOutcome::Stale(reason),
            Err(Invalid::Unverified(error)) => SnapshotOutcome::Unverified(error),
            Ok(index) => {
                let Some(app) =
                    self.runtime.as_mut().and_then(|rt| rt.apps.get_mut(&window.application()))
                else {
                    return SnapshotOutcome::Stale(StaleReason::ApplicationGone);
                };
                // `validate` just found it in `AXWindows`: a suspended window is
                // listed again. The consumer takes this snapshot as its return,
                // so the owner must stop refusing it too (an intermediate frame
                // checks only this flag), and a later unlisting is reported
                // again.
                app.windows[index].suspended = None;
                let held = &app.windows[index];
                SnapshotOutcome::Snapshot(Box::new(read_snapshot(
                    self.system.ax(),
                    &held.element,
                    window,
                    true,
                    self.reads.tabs,
                    held.tab,
                )))
            }
        }
    }

    #[cfg(test)]
    fn resolve(&mut self, cg_window_id: u32) -> Option<WindowInstance> {
        match self.resolve_raw(cg_window_id) {
            Resolution::Single(token) => token,
            Resolution::NoMatch | Resolution::Ambiguous => None,
        }
    }

    fn resolve_raw(&mut self, cg_window_id: u32) -> Resolution {
        let adapter = self.adapter;
        let Some(rt) = self.runtime.as_ref() else {
            return Resolution::NoMatch;
        };
        let mut matches = rt.apps.values().flat_map(|app| {
            app.windows
                .iter()
                .filter(move |held| held.cg == Some(cg_window_id))
                .map(move |held| rt.token(adapter, app.id, held.serial))
        });
        let Some(first) = matches.next() else {
            return Resolution::NoMatch;
        };
        // A duplicate claim is ambiguous: resolve nothing rather than guess.
        if matches.next().is_some() {
            return Resolution::Ambiguous;
        }
        drop(matches);
        Resolution::Single(self.validate(first).ok().map(|_| first))
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
            Err(invalid) => return invalid.outcome(),
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
                match write_frame_gated(
                    ax,
                    element,
                    position,
                    size,
                    WriteOrder::Settled,
                    &mut commit,
                ) {
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
        settle_write(rt, refusal, outcome)
    }

    // ---- enhanced user interface leases --------------------------------

    /// Restores every open lease whose application is idle (all of them when
    /// `force`d, which ignores the grace and any pending frames). Allowed after
    /// the seal: it is owned cleanup, never a frame write.
    fn settle_leases(&mut self, force: bool) -> LeaseSummary {
        let Self {
            runtime,
            system,
            clock,
            frames,
            quarantine,
            ..
        } = self;
        let mut summary = LeaseSummary::default();
        let ax = system.ax();
        if let Some(rt) = runtime.as_mut() {
            let mut open: Vec<ApplicationId> =
                rt.apps.iter().filter(|(_, app)| app.eui.is_open()).map(|(id, _)| *id).collect();
            open.sort();
            for id in open {
                let now = clock.now();
                let pending = frames.has_pending(id);
                let mut notes = Vec::new();
                let Some(app) = rt.apps.get_mut(&id) else { continue };
                let settled = app.eui.settle(ax, &app.element, now, pending, force, &mut notes);
                rt.emit_notes(id, notes);
                match settled {
                    Settled::Wait(at) => summary.wake_at(at),
                    Settled::Closed => summary.restored += 1,
                    Settled::Owed(retry) => {
                        summary.owed.push(id);
                        if let Some(at) = retry.filter(|_| !force) {
                            summary.wake_at(at);
                        }
                    }
                    Settled::None | Settled::Open => {}
                }
            }
        }
        // A quarantined application still owes its lease after the epoch ended;
        // the epoch is over, so its notes have no sink.
        if force {
            for held in quarantine.iter_mut().filter(|held| held.app.eui.is_open()) {
                let app = &mut held.app;
                match app.eui.settle(ax, &app.element, clock.now(), false, true, &mut Vec::new()) {
                    Settled::Closed => summary.restored += 1,
                    Settled::Owed(_) => summary.owed.push(app.id),
                    Settled::None | Settled::Open | Settled::Wait(_) => {}
                }
            }
        }
        summary
    }

    /// After a panic the shard will not run another command: restore its open
    /// leases before it ends. A second panic here is contained.
    fn restore_after_panic(&mut self) {
        let _ = catch_unwind(AssertUnwindSafe(|| self.settle_leases(true)));
    }

    // ---- frames --------------------------------------------------------

    fn gate(&self) -> Option<Arc<GenerationCell>> {
        self.runtime.as_ref().map(|rt| Arc::clone(&rt.gate))
    }

    /// Answers frames the shard will not write: sealed when the gate is
    /// sealed, `otherwise` if not. Runs outside the mailbox lock.
    #[allow(clippy::unused_self, reason = "kept with the other frame helpers")]
    fn answer_frames(
        &self,
        pending: Vec<PendingFrame>,
        gate: Option<&GenerationCell>,
        otherwise: WindowActionOutcome,
    ) {
        let outcome = if gate.is_some_and(GenerationCell::is_sealed) {
            WindowActionOutcome::NotIssued(NotIssuedReason::Sealed)
        } else {
            otherwise
        };
        for frame in pending {
            frame.reply.answer(done(outcome));
        }
    }

    /// Writes the pending frames (at most [`MAX_FRAMES_PER_TURN`]). `true`
    /// when more remain.
    fn run_frames(&mut self) -> bool {
        if self.poisoned {
            // Dropping each pending frame answers `OwnerUnavailable`.
            drop(self.frames.drain_all());
            return false;
        }
        let (batch, more) = self.frames.take(MAX_FRAMES_PER_TURN);
        let mut batch = batch.into_iter();
        for pending in batch.by_ref() {
            let PendingFrame { request, reply } = pending;
            if let Ok(ack) = catch_unwind(AssertUnwindSafe(|| self.frame(&request))) {
                reply.answer(FrameReply::Done(ack));
            } else {
                // Effective in unwind-enabled builds only; release aborts.
                self.poisoned = true;
                if let Some(rt) = &self.runtime {
                    rt.gate.seal();
                }
                self.restore_after_panic();
                drop(reply);
            }
            if self.poisoned {
                break;
            }
        }
        // Whatever a panic left unwritten answers `OwnerUnavailable` on drop.
        drop(batch);
        more
    }

    /// Re-checks a token against the adapter, the epoch, the exact
    /// application and the retained element, **without** listing `AXWindows`.
    fn validate_light(&self, window: WindowInstance) -> Result<usize, Invalid> {
        if window.adapter() != self.adapter {
            return Err(Invalid::Stale(StaleReason::WrongAdapter));
        }
        let Some(rt) = self.runtime.as_ref().filter(|rt| rt.epoch == window.epoch()) else {
            return Err(Invalid::Stale(StaleReason::WrongEpoch));
        };
        let next_serial = rt.counters.serial.load(Ordering::SeqCst);
        let Some(app) = rt.apps.get(&window.application()) else {
            return Err(Invalid::Stale(StaleReason::ApplicationGone));
        };
        let Some(index) = app.windows.iter().position(|held| held.serial == window.serial()) else {
            return Err(Invalid::Stale(if window.serial() < next_serial {
                StaleReason::Destroyed
            } else {
                StaleReason::UnknownWindow
            }));
        };
        if app.windows[index].suspended.is_some() {
            return Err(Invalid::Stale(StaleReason::Suspended));
        }
        Ok(index)
    }

    /// What a frame of `window` answers when no epoch runs: sealed when it
    /// belongs to the epoch that was just stopped, otherwise a wrong epoch.
    fn no_runtime_outcome(&self, window: WindowInstance) -> WindowActionOutcome {
        let sealed = self
            .stopped
            .as_ref()
            .is_some_and(|(epoch, gate)| *epoch == window.epoch() && gate.is_sealed());
        if sealed {
            WindowActionOutcome::NotIssued(NotIssuedReason::Sealed)
        } else {
            WindowActionOutcome::Stale(StaleReason::WrongEpoch)
        }
    }

    /// One mailbox frame: the checks of [`Self::act`], then a gated write.
    /// Only [`FrameStage::Direct`] reads the frame back, and only
    /// [`FrameStage::Intermediate`] skips the `AXWindows` membership listing.
    #[allow(clippy::too_many_lines)] // one gated send: admission, lease, write, readback
    fn frame(&mut self, request: &FrameRequest) -> FrameAck {
        let issuance = &request.issuance;
        let window = issuance.window();
        let unsent = |outcome| FrameAck { outcome, sent_at: None };
        let Some(gate) = self.gate() else {
            return unsent(self.no_runtime_outcome(window));
        };
        if let Err(reason) = gate.admit(issuance) {
            return unsent(WindowActionOutcome::NotIssued(reason));
        }
        if request.position.is_none() && request.size.is_none() {
            return unsent(WindowActionOutcome::NotIssued(NotIssuedReason::EmptyRequest));
        }
        if self.clock.now() >= issuance.deadline() {
            return unsent(WindowActionOutcome::ExpiredBeforeSend);
        }
        let checked = if request.stage == FrameStage::Intermediate {
            self.validate_light(window)
        } else {
            self.validate(window)
        };
        let index = match checked {
            Ok(index) => index,
            Err(invalid) => return unsent(invalid.outcome()),
        };
        let Self {
            runtime,
            system,
            clock,
            voice_over,
            ..
        } = self;
        let Some(rt) = runtime.as_mut() else {
            return unsent(WindowActionOutcome::Stale(StaleReason::WrongEpoch));
        };
        let ax = system.ax();
        // The seal may have landed since `admit`: never touch the attribute
        // after it.
        if gate.is_sealed() {
            return unsent(WindowActionOutcome::NotIssued(NotIssuedReason::Sealed));
        }
        // The lease wraps every frame write: take it before the first write of
        // a burst when the application has the attribute on.
        let mut notes = Vec::new();
        if let Some(app) = rt.apps.get_mut(&window.application()) {
            let context = LeaseContext {
                now: clock.now(),
                voice_over: &**voice_over,
            };
            app.eui.before_write(ax, &app.element, &context, &mut notes);
        }
        rt.emit_notes(window.application(), std::mem::take(&mut notes));
        let Some(app) = rt.apps.get(&window.application()) else {
            return unsent(WindowActionOutcome::Stale(StaleReason::ApplicationGone));
        };
        let element = &app.windows[index].element;
        let mut refusal: Option<CommitRefusal> = None;
        let mut sent_at: Option<Instant> = None;
        let commit = || match issuance.commit(&gate, clock.now()) {
            Ok(()) => {
                sent_at = Some(clock.now());
                true
            }
            Err(why) => {
                refusal = Some(why);
                false
            }
        };
        let order = write_order(request.stage);
        let written = write_frame_gated(ax, element, request.position, request.size, order, commit);
        let outcome = match written {
            None => None,
            Some(FrameWriteOutcome::NotSent(error)) => Some(Err(timeout_refusal(error))),
            Some(FrameWriteOutcome::Attempted { position, size }) => {
                if request.stage == FrameStage::Intermediate
                    && (position == WriteStatus::Failed(AxError::InvalidElement)
                        || size == WriteStatus::Failed(AxError::InvalidElement))
                {
                    return FrameAck {
                        outcome: WindowActionOutcome::Stale(StaleReason::Destroyed),
                        sent_at,
                    };
                }
                let observed = (request.stage == FrameStage::Direct
                    && !matches!(
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
        };
        let sent = sent_at.is_some();
        let outcome = settle_write(rt, refusal, outcome);
        if sent && let Some(app) = rt.apps.get_mut(&window.application()) {
            let ends_batch = matches!(request.stage, FrameStage::Final | FrameStage::Direct);
            app.eui.after_write(ax, &app.element, clock.now(), ends_batch, &mut notes);
        }
        rt.emit_notes(window.application(), notes);
        FrameAck { outcome, sent_at }
    }
}

/// What a gated write attempt produced before it is reported: nothing sent, a
/// refusal before the send, or the issued result.
type WriteAttempt = Option<Result<(IssuedKind, NativeStatus, Option<FrameRead>), NotIssuedReason>>;

/// Turns a write attempt into the outcome, noting a permission loss.
fn settle_write<S: WindowSystem>(
    rt: &mut Runtime<S>,
    refusal: Option<CommitRefusal>,
    outcome: WriteAttempt,
) -> WindowActionOutcome {
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
    next_serial: &AtomicU64,
    app: &mut AppState<S>,
    element: Element<S>,
    reads: Reads,
) -> Adopted {
    let ax = system.ax();
    if let Some(index) =
        app.windows.iter().position(|held| ax.same_element(&held.element, &element))
    {
        let returned = app.windows[index].suspended.take().is_some();
        return Adopted::Existing { index, returned };
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
    // Serials are unique across shards; a refusal below gives this one back
    // when it is still the newest, so a lone shard stays gapless.
    let serial = next_serial.fetch_add(1, Ordering::SeqCst);
    let window = WindowInstance::new(adapter, epoch, app.id, serial);
    let tab = if reads.tab_bars {
        let referenced = app.referenced_bars();
        probe_tab_bar(ax, &mut app.tab_bars, &referenced, &element)
    } else {
        TabBarRead::NotRead
    };
    let snapshot = read_snapshot(ax, &element, window, observed, reads.tabs, tab);
    // A window in transition (a tab group switching or closing) answers with
    // no id and no readable tab bar. It is not adopted: no token is minted and
    // its registrations are removed, so a later listing adopts it once it
    // settles.
    if tab == TabBarRead::Unreadable
        && ax.window_id_capability() == WindowIdCapability::Available
        && snapshot.cg_window_id.is_none()
    {
        unregister(app, key);
        let _ =
            next_serial.compare_exchange(serial + 1, serial, Ordering::SeqCst, Ordering::SeqCst);
        return Adopted::Transient;
    }
    app.windows.push(Tracked {
        serial,
        element,
        key,
        cg: snapshot.cg_window_id,
        observed,
        tab,
        suspended: None,
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
    reads: Reads,
) -> WindowSnapshot {
    let ax = system.ax();
    let cached = app.windows[index].tab;
    // A window known to carry no tab bar is not searched again (the search is
    // the costly read); one with a bar, or not yet read, is refreshed. A read
    // that fails keeps the last known bar.
    let tab = if reads.tab_bars && cached != TabBarRead::None {
        let referenced = app.referenced_bars();
        let _ = ax.set_messaging_timeout(&app.windows[index].element, ELEMENT_TIMEOUT);
        match probe_tab_bar(ax, &mut app.tab_bars, &referenced, &app.windows[index].element) {
            TabBarRead::Unreadable => cached,
            fresh => fresh,
        }
    } else {
        cached
    };
    let held = &app.windows[index];
    let window = WindowInstance::new(adapter, epoch, app.id, held.serial);
    let snapshot = read_snapshot(ax, &held.element, window, held.observed, reads.tabs, tab);
    let held = &mut app.windows[index];
    held.cg = snapshot.cg_window_id.or(held.cg);
    held.tab = tab;
    snapshot
}

/// Ends an enumeration. A listing that skipped transient windows says so first,
/// so the consumer lists again after a settle delay instead of trusting it. A
/// settled, untruncated listing ages the tab bars no window carries.
fn finish_enumeration<S: WindowSystem>(
    rt: &mut Runtime<S>,
    en: &Enumeration<Element<S>>,
    truncated: bool,
) {
    if en.transient > 0 {
        rt.emit(WindowEventKind::EnumerationUnsettled {
            application: en.app,
            windows: en.transient,
        });
    } else if !truncated && let Some(app) = rt.apps.get_mut(&en.app) {
        let referenced = app.referenced_bars();
        app.tab_bars.prune(&referenced);
    }
    rt.emit(WindowEventKind::EnumerationComplete {
        application: en.app,
        windows: en.adopted,
        truncated,
    });
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
        finish_enumeration(rt, en, false);
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
            finish_enumeration(rt, en, true);
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
    reads: Reads,
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
        finish_enumeration(rt, en, false);
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
        // Created while the pages were read, or listed again: report it.
        app.windows[index].suspended = None;
        let snapshot = reread::<S>(system, adapter, epoch, app, index, reads);
        en.adopted += 1;
        rt.emit(WindowEventKind::Snapshot {
            snapshot,
            origin: SnapshotOrigin::Enumeration,
        });
    } else {
        for kind in depart::<S>(system, adapter, epoch, app, index) {
            rt.emit(kind);
        }
    }
    false
}

/// Whether a tracked window's element is still alive: a bounded read of its
/// role succeeds and its `CGWindowID` is still the one it had.
fn element_alive<S: WindowSystem>(system: &S, held: &Tracked<Element<S>>) -> bool {
    let ax = system.ax();
    if ax.set_messaging_timeout(&held.element, ELEMENT_TIMEOUT).is_err() {
        return false;
    }
    if ax.window_string(&held.element, WindowString::Role).is_err() {
        return false;
    }
    held.cg.is_none_or(|cg| ax.window_id(&held.element) == Ok(cg))
}

/// What a tracked window absent from `AXWindows` becomes.
///
/// A live element means the window is merely unlisted (an inactive native tab):
/// it is **suspended**, its element and registrations stay retained and no
/// destroy is reported. A dead element is retired and reported `Destroyed`. At
/// most [`MAX_SUSPENDED_PER_APPLICATION`] windows stay suspended; the oldest
/// beyond that is retired.
fn depart<S: WindowSystem>(
    system: &S,
    adapter: AdapterId,
    epoch: RuntimeEpoch,
    app: &mut AppState<S>,
    index: usize,
) -> Vec<WindowEventKind> {
    let id = app.id;
    let token = |serial: u64| WindowInstance::new(adapter, epoch, id, serial);
    if element_alive::<S>(system, &app.windows[index]) {
        if app.windows[index].suspended.is_some() {
            return Vec::new();
        }
        app.suspensions += 1;
        app.windows[index].suspended = Some(app.suspensions);
        let mut kinds = vec![WindowEventKind::Window {
            window: token(app.windows[index].serial),
            change: WindowChange::Suspended,
        }];
        let held = app.windows.iter().filter(|window| window.suspended.is_some()).count();
        if held > MAX_SUSPENDED_PER_APPLICATION
            && let Some(oldest) = app
                .windows
                .iter()
                .enumerate()
                .filter_map(|(position, window)| window.suspended.map(|order| (order, position)))
                .min()
                .map(|(_, position)| position)
        {
            let gone = app.windows.remove(oldest);
            unregister(app, gone.key);
            kinds.push(WindowEventKind::Window {
                window: token(gone.serial),
                change: WindowChange::Destroyed,
            });
        }
        kinds
    } else {
        let gone = app.windows.remove(index);
        unregister(app, gone.key);
        vec![WindowEventKind::Window {
            window: token(gone.serial),
            change: WindowChange::Destroyed,
        }]
    }
}

/// Suspends the tracked windows a complete, fresh `AXWindows` listing no longer
/// holds while their elements are alive. Windows with dead elements are left
/// for their destroy notification or the next listing. A listing at the cap is
/// no proof of absence and suspends nothing.
fn suspend_unlisted<S: WindowSystem>(
    system: &S,
    adapter: AdapterId,
    epoch: RuntimeEpoch,
    app: &mut AppState<S>,
    list: &[Element<S>],
) -> Vec<WindowEventKind> {
    if list.len() >= MAX_WINDOWS_PER_APPLICATION {
        return Vec::new();
    }
    let ax = system.ax();
    let mut kinds = Vec::new();
    let absent: Vec<u64> = app
        .windows
        .iter()
        .filter(|held| {
            held.suspended.is_none()
                && !list.iter().any(|candidate| ax.same_element(candidate, &held.element))
        })
        .map(|held| held.serial)
        .collect();
    for serial in absent {
        let Some(index) = app.windows.iter().position(|held| held.serial == serial) else {
            continue;
        };
        if element_alive::<S>(system, &app.windows[index]) {
            kinds.extend(depart::<S>(system, adapter, epoch, app, index));
        }
    }
    kinds
}

/// The tracked window that is the application's real `AXFocusedWindow`, read
/// on the owner within the element timeout. `Ok(None)` when it has none or it is
/// not tracked.
fn focused_token<S: WindowSystem>(
    system: &S,
    adapter: AdapterId,
    epoch: RuntimeEpoch,
    app: &AppState<S>,
) -> Result<Option<WindowInstance>, AxError> {
    let ax = system.ax();
    ax.set_messaging_timeout(&app.element, ELEMENT_TIMEOUT)?;
    let focused = match ax.focused_window(&app.element) {
        Ok(element) => element,
        Err(error) if error.is_absent() => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(app
        .windows
        .iter()
        .find(|held| ax.same_element(&held.element, &focused))
        .map(|held| WindowInstance::new(adapter, epoch, app.id, held.serial)))
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
    reads: Reads,
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
                    // A creation naming a suspended window means it is listed
                    // again: report it so the consumer gives it its slot back.
                    if app.windows[index].suspended.take().is_some() {
                        let snapshot = reread::<S>(system, adapter, epoch, app, index, reads);
                        return vec![WindowEventKind::Snapshot {
                            snapshot,
                            origin: SnapshotOrigin::Created,
                        }];
                    }
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
                if notification == AxNotification::FocusedWindowChanged
                    && let Ok(Some(window)) = focused_token(system, adapter, epoch, app)
                {
                    // The application's real focused window is known: report it
                    // for its token instead of a change that names none.
                    return vec![WindowEventKind::Window {
                        window,
                        change: WindowChange::Focused,
                    }];
                }
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
            let listed = ax
                .set_messaging_timeout(&app.element, ELEMENT_TIMEOUT)
                .and_then(|()| ax.windows(&app.element, 0, MAX_WINDOWS_PER_APPLICATION));
            let list = match listed {
                Ok(list) if list.iter().any(|held| ax.same_element(held, &element)) => list,
                Ok(_) => {
                    app.release.push(key);
                    return Vec::new();
                }
                Err(_) => {
                    // Unverifiable: ask the consumer to reconcile by refreshing.
                    app.release.push(key);
                    return vec![WindowEventKind::ObserverOverflow { application: id }];
                }
            };
            // Windows this listing no longer holds but whose elements live are
            // suspended first, so a consumer sees the departure of a tab group's
            // previous window before the window that replaces it.
            let mut kinds = suspend_unlisted::<S>(system, adapter, epoch, app, &list);
            let adopted =
                adopt::<S>(system, adapter, epoch, &rt.counters.serial, app, element, reads);
            app.release.push(key);
            match adopted {
                Adopted::New(snapshot) => {
                    let window = snapshot.window;
                    kinds.push(WindowEventKind::Snapshot {
                        snapshot: *snapshot,
                        origin: SnapshotOrigin::Created,
                    });
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
                }
                Adopted::Transient => {
                    // Not adopted and not lost: the consumer lists the
                    // application again once the window settles.
                    kinds.push(WindowEventKind::EnumerationUnsettled {
                        application: id,
                        windows: 1,
                    });
                }
                Adopted::Existing { index, returned } => {
                    if returned {
                        // A suspended window is listed again: report it so the
                        // consumer can give it its slot back.
                        let snapshot = reread::<S>(system, adapter, epoch, app, index, reads);
                        kinds.push(WindowEventKind::Snapshot {
                            snapshot,
                            origin: SnapshotOrigin::Created,
                        });
                    }
                }
                Adopted::Gone | Adopted::Capped | Adopted::NotObservable => {}
            }
            kinds
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

/// Only the frames a window ends at use the settled (size, position, size)
/// order; motion frames stay at two calls.
const fn write_order(stage: FrameStage) -> WriteOrder {
    match stage {
        FrameStage::First | FrameStage::Intermediate => WriteOrder::Motion,
        FrameStage::Final | FrameStage::Direct => WriteOrder::Settled,
    }
}
