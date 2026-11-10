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

/// An opaque identity of one native tab bar, scoped to one application in one
/// runtime epoch.
///
/// The owner mints it: two windows of an application carry the same id exactly
/// when their `AXTabGroup` elements are the same accessibility object
/// (`CFEqual`), which is how `AppKit`'s one shared tab bar follows the selected
/// tab from window to window. It is never derived from a frame, a title or a
/// `CGWindowID`, and the consumer can only compare it for equality.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabBarId(u64);

impl TabBarId {
    pub(super) const fn new(value: u64) -> Self { Self(value) }

    /// A synthetic id for fakes.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub const fn for_testing(value: u64) -> Self { Self(value) }

    /// The numeric value, diagnostic only.
    #[must_use]
    pub const fn value(self) -> u64 { self.0 }
}

/// A window's native tab bar as the owner read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabBar {
    /// The owner-scoped identity of the shared tab bar.
    pub id: TabBarId,
    /// How many tab buttons the bar has (bounded).
    pub count: usize,
    /// The index of the selected tab button, when it could be read.
    pub selected: Option<usize>,
}

/// What is known about a window's tab bar identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabBarRead {
    /// Not read (tab bar reads are off, or the snapshot predates a read).
    NotRead,
    /// The read failed or the bar could not be told apart yet: unknown, never
    /// "no tab bar".
    Unreadable,
    /// The bounded search found no `AXTabGroup`.
    None,
    /// The window carries this tab bar.
    Bar(TabBar),
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
    /// The window's native tab bar identity.
    pub tab_bar: TabBarRead,
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
    /// The window left `AXWindows` but its element is alive: the inactive tab
    /// of a native tab group, or one that was replaced by another window of the
    /// group. Its token stays valid, its element and observer registrations
    /// stay retained, and it is reported again (`Snapshot`) when it is listed
    /// again. It is retired (`Destroyed`) only when its element dies.
    Suspended,
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
#[allow(
    clippy::large_enum_variant,
    reason = "events move once into a bounded queue; boxing every snapshot costs an allocation per event"
)]
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
    /// The owner skipped listed windows whose id or tab bar could not be read
    /// yet (a tab group in transition). They were neither adopted nor
    /// treated as gone; the consumer should list the application again after a
    /// short settle delay. Emitted before the matching completion, or alone for
    /// a creation.
    EnumerationUnsettled {
        /// The application.
        application: ApplicationId,
        /// Windows skipped.
        windows: usize,
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
    /// The `AXEnhancedUserInterface` lease of an application changed or
    /// failed. Only lease transitions, failures and re-enables are reported,
    /// never one event per frame.
    EnhancedUi {
        /// The exact application instance.
        application: ApplicationId,
        /// What happened.
        note: EnhancedUiNote,
    },
}

/// What happened to an application's `AXEnhancedUserInterface` lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnhancedUiNote {
    /// The attribute was on; the shard turned it off for a burst of frame
    /// writes and owes it back.
    Leased,
    /// The shard restored the attribute to on and closed the lease.
    Restored,
    /// The attribute could not be read: the writes proceed without a lease.
    ReadFailed(AxError),
    /// The attribute was on but could not be turned off: the writes proceed
    /// without a lease.
    SetFailed(AxError),
    /// A third party turned the attribute back on during the lease. The shard
    /// does not fight it. `consecutive` counts the leases in a row that ended
    /// this way; two or more mean the application cannot be kept off.
    ReEnabled {
        /// Consecutive re-enabled leases of this application.
        consecutive: u32,
    },
    /// Restoring the attribute failed: the shard keeps the lease and retries.
    RestoreFailed(AxError),
    /// `VoiceOver` is running: the attribute is never touched. Reported once per
    /// application per epoch.
    VoiceOverSkipped,
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
