//! The callback side of the Space sources: bounded, allocation-free, pure.
//!
//! Every native callback (the Dock Accessibility drain, the `NSWorkspace`
//! blocks, the private `SkyLight` notify proc) calls exactly one [`SourceHub`]
//! method. A method reads at most the active display list into a fixed buffer
//! ([`DisplayLister`]), updates fixed-size state and pushes [`SpaceFact`]s
//! through the [`SpaceSink`]. It performs no Accessibility read, allocates
//! nothing beyond the fixed buffers and holds no lock while calling out (the
//! sink's own short lock is the only one, and `emit` holds none of ours).
//!
//! # Closing
//!
//! [`SourceHub::close`] is the "close the sink first" step of a stop: it stops
//! accepting facts and waits (bounded) for emissions already in progress, so
//! once it returns `true` no callback can push.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::super::spaces::{SpaceDiagnostic, SpaceFact, SpaceSink, UnknownCause};
use crate::DisplayId;

/// Most displays one fact burst covers. A larger layout is treated as a failed
/// query (fail closed).
pub const MAX_DISPLAYS: usize = 16;

/// Wakes the owner thread's run loop; callable from any thread, never blocks.
pub type LoopWaker = Arc<dyn Fn() + Send + Sync>;

/// A command posted to the owner thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Stop and exit.
    Stop,
    /// The Dock application launched or terminated: re-register.
    DockChanged,
    /// Re-read the source state and report it in full.
    Resync,
}

impl Command {
    const fn bit(self) -> u32 {
        match self {
            Self::Stop => 1,
            Self::DockChanged => 2,
            Self::Resync => 4,
        }
    }
}

/// The posted commands and the owner's waker. Posting never blocks.
#[derive(Default)]
pub struct Commands {
    bits: AtomicU32,
    waker: Mutex<Option<LoopWaker>>,
}

impl Commands {
    /// Records `command` and wakes the owner when it has published a waker. A
    /// command posted earlier is found by the owner before it waits.
    pub fn post(&self, command: Command) {
        self.bits.fetch_or(command.bit(), Ordering::SeqCst);
        let waker = lock(&self.waker).clone();
        if let Some(waker) = waker {
            waker();
        }
    }

    /// The owner publishes (or withdraws) the waker of its run loop.
    pub fn set_waker(&self, waker: Option<LoopWaker>) { *lock(&self.waker) = waker; }

    /// Takes every posted command, in priority order.
    pub fn take(&self) -> Vec<Command> {
        let bits = self.bits.swap(0, Ordering::SeqCst);
        [Command::Stop, Command::DockChanged, Command::Resync]
            .into_iter()
            .filter(|command| bits & command.bit() != 0)
            .collect()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One display's geometry: its bounds in the global point space and its size
/// in pixel units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayGeometry {
    /// Left edge, in points.
    pub x: f64,
    /// Top edge, in points.
    pub y: f64,
    /// Width, in points.
    pub width: f64,
    /// Height, in points.
    pub height: f64,
    /// Width in pixel units.
    pub pixels_wide: usize,
    /// Height in pixel units.
    pub pixels_high: usize,
}

/// The active display ids.
pub trait DisplayLister: Send + Sync {
    /// Writes the active display ids into `out` and returns how many, or `None`
    /// when the query failed or does not fit.
    fn active(&self, out: &mut [u32; MAX_DISPLAYS]) -> Option<usize>;

    /// The geometry of the active display `id`, or `None` when it is unknown
    /// (the display then counts by its id alone).
    fn geometry(&self, id: u32) -> Option<DisplayGeometry> {
        let _ = id;
        None
    }
}

/// The displays and their geometry: what a real layout change changes.
#[derive(Clone, Copy, Debug, PartialEq)]
struct DisplayLayout {
    len: usize,
    displays: [(u32, Option<DisplayGeometry>); MAX_DISPLAYS],
}

/// A fixed set of display ids.
#[derive(Clone, Copy, Debug)]
struct DisplaySet {
    ids: [u32; MAX_DISPLAYS],
    len: usize,
}

impl DisplaySet {
    const fn new() -> Self { Self { ids: [0; MAX_DISPLAYS], len: 0 } }

    fn contains(&self, id: u32) -> bool { self.ids[..self.len].contains(&id) }

    fn insert(&mut self, id: u32) {
        if !self.contains(id) && self.len < MAX_DISPLAYS {
            self.ids[self.len] = id;
            self.len += 1;
        }
    }
}

/// The callback bodies; see the module documentation.
pub struct SourceHub {
    sink: SpaceSink,
    displays: Box<dyn DisplayLister>,
    commands: Arc<Commands>,
    /// Displays whose Space change began and has not completed.
    transitions: Mutex<DisplaySet>,
    /// The display layout last seen; `None` before the first read and after an
    /// unreadable one.
    layout: Mutex<Option<DisplayLayout>>,
    mission_control: AtomicBool,
    accepting: AtomicBool,
    in_flight: AtomicUsize,
}

impl SourceHub {
    /// A hub pushing into `sink`.
    #[must_use]
    pub fn new(sink: SpaceSink, displays: Box<dyn DisplayLister>, commands: Arc<Commands>) -> Self {
        Self {
            sink,
            displays,
            commands,
            transitions: Mutex::new(DisplaySet::new()),
            layout: Mutex::new(None),
            mission_control: AtomicBool::new(false),
            accepting: AtomicBool::new(true),
            in_flight: AtomicUsize::new(0),
        }
    }

    /// Pushes one fact unless closed. The in-flight count lets [`Self::close`]
    /// wait for a push that already started; no lock of ours is held.
    pub fn emit(&self, fact: SpaceFact) {
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        if self.accepting.load(Ordering::SeqCst) {
            let _ = self.sink.push(fact);
        }
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }

    /// Stops accepting facts and waits up to `budget` for pushes in progress.
    /// `true` when none can push any more.
    pub fn close(&self, budget: Duration) -> bool {
        self.accepting.store(false, Ordering::SeqCst);
        let deadline = Instant::now() + budget;
        while self.in_flight.load(Ordering::SeqCst) != 0 {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        true
    }

    /// Whether the hub still accepts facts.
    #[must_use]
    pub fn accepting(&self) -> bool { self.accepting.load(Ordering::SeqCst) }

    /// A Dock Mission Control, App Exposé or Show Desktop notification arrived
    /// (`entering`), or the exit notification did.
    pub fn mission_control(&self, entering: bool) {
        self.mission_control.store(entering, Ordering::SeqCst);
        self.emit(if entering {
            SpaceFact::MissionControlEntered
        } else {
            SpaceFact::MissionControlExited
        });
    }

    /// Reports what a source saw, for the debug log only (never a gate reason).
    pub fn diagnostic(&self, diagnostic: SpaceDiagnostic) {
        self.emit(SpaceFact::Diagnostic(diagnostic));
    }

    /// A private Space-change candidate event fired: its id is reported for the
    /// log, then it is treated as a Space change beginning.
    pub fn private_space_event(&self, event: u32) {
        self.diagnostic(SpaceDiagnostic::PrivateEvent { event });
        self.space_change_began();
    }

    /// The Dock application launched or terminated.
    pub fn dock_application_changed(&self) { self.commands.post(Command::DockChanged); }

    /// The private Space-change notification arrived: the event carries no
    /// display, so every active display starts a transition. An unreadable
    /// display list is a failed query (fail closed).
    pub fn space_change_began(&self) {
        let mut buffer = [0_u32; MAX_DISPLAYS];
        let Some(count) = self.displays.active(&mut buffer).filter(|count| *count > 0) else {
            self.emit(SpaceFact::SourceLost(UnknownCause::QueryFailed));
            return;
        };
        for &id in &buffer[..count] {
            lock(&self.transitions).insert(id);
            self.emit(SpaceFact::SpaceChangeBegan { display: DisplayId::new(id) });
        }
    }

    /// The public `activeSpaceDidChange` notification arrived (it has no
    /// display): every active display and every display still in a transition
    /// completes.
    pub fn space_change_completed(&self) {
        let mut buffer = [0_u32; MAX_DISPLAYS];
        let count = self.displays.active(&mut buffer);
        let mut done = DisplaySet::new();
        if let Some(count) = count {
            for &id in &buffer[..count] {
                done.insert(id);
            }
        }
        {
            let mut pending = lock(&self.transitions);
            for &id in &pending.ids[..pending.len] {
                done.insert(id);
            }
            *pending = DisplaySet::new();
        }
        if count.is_none() {
            self.emit(SpaceFact::SourceLost(UnknownCause::QueryFailed));
        }
        for &id in &done.ids[..done.len] {
            self.emit(SpaceFact::SpaceChangeCompleted { display: DisplayId::new(id) });
        }
    }

    /// The current displays and their geometry, or `None` when the display
    /// list is unreadable. No lock of ours is held while asking.
    fn read_layout(&self) -> Option<DisplayLayout> {
        let mut ids = [0_u32; MAX_DISPLAYS];
        let len = self.displays.active(&mut ids)?;
        let mut displays = [(0_u32, None); MAX_DISPLAYS];
        for (slot, &id) in displays.iter_mut().zip(&ids[..len]) {
            *slot = (id, self.displays.geometry(id));
        }
        Some(DisplayLayout { len, displays })
    }

    /// Records the current display layout as the one later screen-parameter
    /// notifications are compared with. The owner calls it once at start, so
    /// the first notification that changes nothing is already ignored.
    pub fn prime_display_layout(&self) {
        let layout = self.read_layout();
        *lock(&self.layout) = layout;
    }

    /// The screen parameters changed. The system posts this for more than
    /// layout changes: a display's HDR headroom follows brightness in small
    /// steps, many times a second, with the same displays and geometry. Only
    /// a different set of displays or a different geometry is a layout change
    /// (a display added, removed, rearranged or resized); anything else is
    /// ignored. An unreadable display list fails closed and is reported.
    ///
    /// Only the end of a change is observable: the layout is reported as
    /// changing and changed in one burst, which hides every border and
    /// re-confirms. A transition recorded for a display that is gone completes.
    pub fn display_layout_changed(&self) {
        let layout = self.read_layout();
        {
            let mut known = lock(&self.layout);
            if layout.is_some() && *known == layout {
                return;
            }
            *known = layout;
        }
        self.emit(SpaceFact::DisplayLayoutChanging);
        let mut buffer = [0_u32; MAX_DISPLAYS];
        let active = self.displays.active(&mut buffer);
        let mut gone = DisplaySet::new();
        {
            let mut pending = lock(&self.transitions);
            let mut kept = DisplaySet::new();
            for &id in &pending.ids[..pending.len] {
                let present = active.is_some_and(|count| buffer[..count].contains(&id));
                if present {
                    kept.insert(id);
                } else {
                    gone.insert(id);
                }
            }
            *pending = kept;
        }
        for &id in &gone.ids[..gone.len] {
            self.emit(SpaceFact::SpaceChangeCompleted { display: DisplayId::new(id) });
        }
        if active.is_none() {
            self.emit(SpaceFact::SourceLost(UnknownCause::QueryFailed));
        }
        self.emit(SpaceFact::DisplayLayoutChanged);
    }

    /// Re-pushes the state the hub itself holds (after a mailbox overflow):
    /// Mission Control if active and each display in transition.
    pub fn resync(&self) {
        if self.mission_control.load(Ordering::SeqCst) {
            self.emit(SpaceFact::MissionControlEntered);
        }
        let pending = *lock(&self.transitions);
        for &id in &pending.ids[..pending.len] {
            self.emit(SpaceFact::SpaceChangeBegan { display: DisplayId::new(id) });
        }
    }
}
