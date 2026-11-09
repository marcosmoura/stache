//! Pure values that leave the owner: snapshots, events and the bounded sink.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use super::identity::{RuntimeEpoch, WindowInstance};
use crate::applications::ApplicationId;
use crate::ax::{AxError, AxReading, FrameRead};

/// What is known about a window's tabs. Tabs are never assumed supported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabInfo {
    /// Not read (tab reads are off).
    Unknown,
    /// The read was attempted and failed or returned a malformed value: tab
    /// information is unavailable for this window, never "no tabs".
    Failed,
    /// No `AXTabGroup` child.
    None,
    /// A tab group with this many tabs.
    Tabs(usize),
}

/// One member of a window's tab group as read from the tab strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabMember {
    /// The member's `CGWindowID`, metadata only; `None` when unavailable.
    pub cg_window_id: Option<u32>,
    /// Whether it is the selected tab; `None` when it could not be read.
    pub selected: Option<bool>,
}

/// A pure, point-in-time description of one window. Every attribute that could
/// not be read is explicit, never zero or empty.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowSnapshot {
    /// The exact window token.
    pub window: WindowInstance,
    /// The `CGWindowID`, metadata only; `None` when the capability is missing
    /// or the read failed.
    pub cg_window_id: Option<u32>,
    /// `AXTitle`.
    pub title: AxReading<String>,
    /// `AXRole`.
    pub role: AxReading<String>,
    /// `AXSubrole`.
    pub subrole: AxReading<String>,
    /// Position and size, each explicit.
    pub frame: FrameRead,
    /// `AXMinimized`.
    pub minimized: AxReading<bool>,
    /// `AXFullScreen`.
    pub fullscreen: AxReading<bool>,
    /// Whether `AXSize` is settable.
    pub resizable: AxReading<bool>,
    /// Tab information.
    pub tabs: TabInfo,
    /// The tab group's members when [`TabInfo::Tabs`], in strip order; empty
    /// otherwise.
    pub tab_members: Vec<TabMember>,
    /// Whether every per-window notification registered.
    pub observed: bool,
}

/// Why a snapshot was produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotOrigin {
    /// Initial or refresh enumeration.
    Enumeration,
    /// A `WindowCreated` (or first-seen) notification.
    Created,
}

/// A window-level change. Carries no values: the consumer queries a snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowChange {
    /// The window was destroyed; its token is already invalid.
    Destroyed,
    /// Moved.
    Moved,
    /// Resized.
    Resized,
    /// Miniaturized.
    Miniaturized,
    /// Deminiaturized.
    Deminiaturized,
    /// Became the focused window.
    Focused,
    /// Became the main window.
    Main,
    /// The title changed.
    TitleChanged,
}

/// An application-level change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplicationChange {
    /// `AXApplicationHidden`.
    Hidden,
    /// `AXApplicationShown`.
    Shown,
    /// The focused window changed (application-level delivery).
    FocusedWindowChanged,
    /// The main window changed (application-level delivery).
    MainWindowChanged,
}

/// Why an enumeration ended without finishing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnumerationAbort {
    /// The caller's cancellation fired.
    Cancelled,
    /// The enumeration deadline passed.
    DeadlineExceeded,
    /// The runtime was sealed.
    Sealed,
    /// The `AXWindows` read failed.
    Failed(AxError),
}

/// A capability that can degrade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    /// Accessibility permission.
    Accessibility,
}

/// What happened.
#[derive(Clone, Debug, PartialEq)]
pub enum WindowEventKind {
    /// A window snapshot.
    Snapshot {
        /// The snapshot.
        snapshot: WindowSnapshot,
        /// Why it was produced.
        origin: SnapshotOrigin,
    },
    /// A window changed.
    Window {
        /// The exact window.
        window: WindowInstance,
        /// What changed.
        change: WindowChange,
    },
    /// An application changed.
    Application {
        /// The exact application instance.
        application: ApplicationId,
        /// What changed.
        change: ApplicationChange,
    },
    /// Initial enumeration finished.
    EnumerationComplete {
        /// The application.
        application: ApplicationId,
        /// Windows adopted by this enumeration.
        windows: usize,
        /// Whether the per-application window cap truncated the list.
        truncated: bool,
    },
    /// Enumeration ended early.
    EnumerationAborted {
        /// The application.
        application: ApplicationId,
        /// Why.
        reason: EnumerationAbort,
    },
    /// The application's observer mailbox overflowed; refresh to reconcile.
    ObserverOverflow {
        /// The application.
        application: ApplicationId,
    },
    /// A capability changed.
    CapabilityChanged {
        /// Which.
        capability: Capability,
        /// Whether it is available now.
        available: bool,
    },
}

/// One event: pure value, epoch and monotonic per-epoch sequence.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowEvent {
    /// The runtime epoch.
    pub epoch: RuntimeEpoch,
    /// Strictly increasing per epoch; a gap means a dropped event.
    pub sequence: u64,
    /// What happened.
    pub kind: WindowEventKind,
}

/// Events taken from the sink.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EventBatch {
    /// Events in order.
    pub events: Vec<WindowEvent>,
    /// How many events were dropped since the previous drain. Non-zero means
    /// the consumer must re-enumerate every tracked application.
    pub dropped: u64,
}

impl EventBatch {
    /// Whether the sink overflowed.
    #[must_use]
    pub const fn overflowed(&self) -> bool { self.dropped > 0 }
}

type Notify = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct QueueState {
    events: VecDeque<WindowEvent>,
    dropped: u64,
}

/// The bounded consumer sink. `push` never blocks; when full the event is
/// dropped and counted, which [`EventBatch::dropped`] reports explicitly.
pub struct EventQueue {
    capacity: usize,
    state: Mutex<QueueState>,
    notify: Option<Notify>,
}

impl EventQueue {
    /// A sink holding at most `capacity` undrained events (at least one).
    #[must_use]
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            capacity: capacity.max(1),
            state: Mutex::new(QueueState::default()),
            notify: None,
        })
    }

    /// A sink that calls `notify` (outside any lock) when an event arrives on
    /// an empty queue. The callback must be nonblocking.
    #[must_use]
    pub fn with_notify(capacity: usize, notify: Notify) -> Arc<Self> {
        Arc::new(Self {
            capacity: capacity.max(1),
            state: Mutex::new(QueueState::default()),
            notify: Some(notify),
        })
    }

    /// Queues an event. Returns whether it was kept.
    pub fn push(&self, event: WindowEvent) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (kept, was_empty) = if state.events.len() >= self.capacity {
            state.dropped += 1;
            (false, false)
        } else {
            let was_empty = state.events.is_empty();
            state.events.push_back(event);
            (true, was_empty)
        };
        drop(state);
        if was_empty && let Some(notify) = &self.notify {
            notify();
        }
        kept
    }

    /// Takes everything queued and the dropped count.
    pub fn drain(&self) -> EventBatch {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        EventBatch {
            events: state.events.drain(..).collect(),
            dropped: std::mem::take(&mut state.dropped),
        }
    }

    /// The configured capacity.
    #[must_use]
    pub const fn capacity(&self) -> usize { self.capacity }
}
