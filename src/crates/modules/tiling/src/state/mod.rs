//! The pure, owned Tiling state and its read-only published snapshot.
//!
//! [`TilingState`] is owned by exactly one writer, the actor. Every change is a
//! pure transition `(state, input) -> (state', effects)` in [`transition`]; the
//! effects are read-only backend requests, gated frame writes and publication.
//! Nothing here calls native code, takes a lock or reads a clock.
//!
//! Desired state (workspace assignment, stable order and the planned frames in
//! `frames`) is kept apart from observed state (title, frame, flags). A frame is
//! only ever *confirmed* by a readback or an observation, never by admission.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use stache_macos::applications::LifecycleKind;
use stache_macos::windows::{
    EnhancedUiNote, FocusOutcome, RuntimeEpoch, SnapshotOutcome, TabBar, TabInfo, TrackOutcome,
    WindowActionOutcome, WindowEvent, WindowInstance,
};
use stache_macos::{ApplicationId, DisplayId, DisplayLayout};

use crate::config::{LayoutType, TilingConfig};
use crate::geometry::Rect;
use crate::intent::{Intent, IntentRecord};
use crate::layout::WindowFlags;
use crate::minimum_size::RatioState;
use crate::outcome::FrameComponents;
use crate::reservations::HostReservations;
use crate::rules::{ManagementExclusion, PreparedRules};
use crate::tabs::{TabRegistry, TabsStatus};
use crate::workspaces::VisibilityRequirement;

mod drag;
mod frames;
mod intents;
mod slots;
mod tab_sync;
mod transition;
pub use drag::{DragOutcome, DragRecord, DragReport, DragState};
pub use frames::{
    AttemptStatus, Binding, DISPLAY_CHANGE_ATTEMPTS, FrameAttempt, FrameBook, FrameRecord,
    FrameStatus, MAX_EXTERNAL_ATTEMPTS, MAX_IN_FLIGHT_FRAMES, MAX_INFERENCES, NoFrameReason,
};
pub use transition::transition;

#[cfg(test)]
mod animation_tests;
#[cfg(test)]
mod drag_tests;
#[cfg(test)]
mod frame_tests;
#[cfg(test)]
mod inactive_tests;
#[cfg(test)]
mod minimum_tests;
#[cfg(test)]
mod tab_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod workspace_tests;

/// Retries a failed or truncated application gets (on activation or relaunch).
pub const MAX_TRACK_RETRIES: u8 = 3;
/// Applications handed to the backend before earlier acknowledgements return.
pub const TRACK_CHUNK: usize = 8;
/// Unconfirmed applications remembered before the scope widens to everything.
pub const MAX_UNCONFIRMED_APPS: usize = 256;
/// Destroyed tokens remembered so a late snapshot cannot resurrect one.
pub const TOMBSTONE_CAPACITY: usize = 4096;
/// Focus history entries kept.
pub const FOCUS_HISTORY_CAPACITY: usize = 64;
/// Workspace focus history entries kept.
pub const WORKSPACE_HISTORY_CAPACITY: usize = 32;
/// Consecutive overflows (without a confirmed reconciliation) that mark the
/// stream overloaded.
pub const OVERLOAD_THRESHOLD: u32 = 4;

/// A workspace identity, allocated monotonically by the actor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkspaceId(pub u32);

/// The registry generation and backend epoch every effect and completion carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    /// The backend runtime epoch.
    pub epoch: RuntimeEpoch,
    /// The actor registry generation; bumps on every restart.
    pub registry: u64,
}

/// How complete the observed state is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Startup enumeration has not finished for the first time.
    Initializing,
    /// A reconciliation is in flight.
    Reconciling,
    /// Everything tracked has been confirmed.
    Complete,
    /// Something could not be confirmed (failure, truncation, overload, no
    /// screens, missing permission or failed health).
    Incomplete,
}

impl Phase {
    /// The lowercase name used by the status query and logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Initializing => "initializing",
            Self::Reconciling => "reconciling",
            Self::Complete => "complete",
            Self::Incomplete => "incomplete",
        }
    }
}

/// An application whose tracking or enumeration did not complete. Only its own
/// windows are held back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedApp {
    /// The exact application instance.
    pub application: ApplicationId,
    /// Why (the backend outcome, or truncation).
    pub reason: String,
    /// Retries spent so far.
    pub retries: u8,
}

/// Why the actor can no longer be trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthFault {
    /// The backend owner is unavailable.
    BackendUnavailable,
    /// A channel to a collaborator closed.
    Disconnected,
    /// A transition panicked (unwind builds only); restart required.
    Panicked,
}

/// Actor health.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// Running.
    Healthy,
    /// Failed; the last published snapshot stays the safe one.
    Failed(HealthFault),
}

/// What an unconfirmed marking applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// One exact application instance.
    App(ApplicationId),
    /// Everything.
    All,
}

/// Why a scope became unconfirmed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The semantic queue was full.
    SemanticOverflow,
    /// The backend sink dropped events.
    SinkOverflow,
    /// A sequence gap proved a missed event.
    SequenceGap,
    /// An application observer mailbox overflowed.
    ObserverOverflow,
    /// Geometry cardinality overflowed.
    GeometryOverflow,
    /// A reserved read-only effect could not be queued.
    EffectRejected,
    /// A drag gesture was cancelled because observation was lost.
    DragLost,
    /// A native tab group changed (a window opened, closed or was swapped); the
    /// application is listed again once `AppKit` settled.
    TabSettle,
    /// The application has a window that left `AXWindows` without a
    /// replacement; it is listed again after a Space change or an activation.
    Unlisted,
}

/// Identity and metadata of a running application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppInfo {
    /// The exact application instance.
    pub application: ApplicationId,
    /// Process id: diagnostic and the AX handle only.
    pub pid: i32,
    /// Bundle identifier (empty when unknown).
    pub bundle_id: String,
    /// Localized name (empty when unknown).
    pub name: String,
}

/// A read-only effect result.
#[derive(Clone, Debug, PartialEq)]
pub enum CompletionResult {
    /// A `track` request returned.
    Track(ApplicationId, TrackOutcome),
    /// A `refresh` request returned.
    Refresh(ApplicationId, TrackOutcome),
    /// A snapshot read returned.
    Snapshot(WindowInstance, SnapshotOutcome),
    /// An application's focused-window read returned. A resolved window was
    /// already reported as a window event; the outcome is diagnostic.
    Focus(ApplicationId, FocusOutcome),
    /// A frame write was acknowledged. Matched against the exact window, plan
    /// sequence and accepted revision it was issued for.
    Frame {
        /// The exact window token.
        window: WindowInstance,
        /// The layout plan sequence of the attempt.
        plan_sequence: u64,
        /// The accepted configuration revision of the attempt.
        revision: u64,
        /// What the backend reported.
        outcome: WindowActionOutcome,
    },
    /// A hide, unhide or focus request finished. The worker's result is in the
    /// shared visibility control; this only wakes the actor to fold it.
    Visibility(ApplicationId),
    /// The effect could not be queued; nothing was requested.
    Rejected(Effect),
}

/// A pure input to the actor.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// A (new) backend epoch and registry begins.
    Start(Stamp),
    /// A pure, epoch-tagged backend event.
    Window(WindowEvent),
    /// An application lifecycle delivery with resolved metadata.
    Lifecycle {
        /// What happened.
        kind: LifecycleKind,
        /// The application.
        info: AppInfo,
    },
    /// An application became the frontmost one (informational).
    Activated(ApplicationId),
    /// List the running applications again: the watches finished installing, so
    /// anything launched before they did was never reported.
    Relist,
    /// The authoritative list of running applications.
    Applications {
        /// The registry it answers.
        stamp: Stamp,
        /// Every running application.
        apps: Vec<AppInfo>,
    },
    /// The current display layout.
    Displays(Arc<DisplayLayout>),
    /// An accepted configuration.
    Config {
        /// The accepted revision; older or equal revisions are dropped.
        revision: u64,
        /// The accepted configuration.
        config: Arc<TilingConfig>,
    },
    /// Coalesced geometry samples that came due.
    GeometryBatch(Vec<WindowInstance>),
    /// A scope could not be confirmed.
    Unconfirmed(Scope, Cause),
    /// An effect completed.
    Completion {
        /// The registry the effect was issued for.
        stamp: Stamp,
        /// The result.
        result: CompletionResult,
    },
    /// A health fact.
    Fault(HealthFault),
    /// The runtime published (`true`) or revoked (`false`) its frame-write
    /// authority.
    FrameWrites(bool),
    /// A user command: attempt every window again and forget inferred minimums.
    Relayout,
    /// A normalized copy of the host screen reservations (a pure hint).
    Reservations(HostReservations),
    /// A typed user command for the workspace model.
    Intent(Intent),
    /// A passive mouse down, up or lost observation from the drag observer.
    Drag(crate::drag::DragSignal),
    /// The settle retries of a native tab group ran out: windows still waiting
    /// for a successor are closed for real.
    SettleExpired(ApplicationId),
    /// The actor's backoff for a transient track failure came due: the
    /// application is tracked again (no activation needed).
    RetryTrack(ApplicationId),
    /// Test hook: panics inside the transition (unwind builds).
    #[cfg(test)]
    PanicForTest,
}

/// A request the actor makes of its environment: read-only backend requests,
/// frame writes and publication.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Install observation and start chunked enumeration.
    Track {
        /// Registry.
        stamp: Stamp,
        /// Application.
        application: ApplicationId,
        /// Process id.
        pid: i32,
    },
    /// Re-enumerate a tracked application.
    Refresh {
        /// Registry.
        stamp: Stamp,
        /// Application.
        application: ApplicationId,
    },
    /// Read one window snapshot.
    Snapshot {
        /// Registry.
        stamp: Stamp,
        /// Window.
        window: WindowInstance,
    },
    /// Stop observing a terminated application.
    Untrack {
        /// Registry.
        stamp: Stamp,
        /// Application.
        application: ApplicationId,
    },
    /// Read the application's real focused window. A tracked one is reported
    /// back as a window `Focused` event.
    ReadFocus {
        /// Registry.
        stamp: Stamp,
        /// Application.
        application: ApplicationId,
    },
    /// Ask for the authoritative running-application list.
    ListApplications {
        /// Registry.
        stamp: Stamp,
    },
    /// Write the changed components of one window frame. The runner issues each
    /// component once and returns the readback as
    /// [`CompletionResult::Frame`].
    WriteFrame {
        /// Registry (the issuance generation).
        stamp: Stamp,
        /// The accepted configuration revision the plan was made under.
        revision: u64,
        /// The layout plan sequence.
        plan_sequence: u64,
        /// The exact window.
        window: WindowInstance,
        /// The desired frame.
        target: Rect,
        /// The components to write.
        components: FrameComponents,
        /// The window's actual frame when the attempt was created.
        from: Rect,
        /// The plan asks for an animated transition (animations enabled and a
        /// meaningful travel). The runner writes directly when it has no
        /// animation worker.
        animate: bool,
        /// The target display's reported refresh rate, for animation pacing.
        refresh_hz: Option<f64>,
        /// The display containing the target, whose frame clock paces the
        /// animation. `None` when unknown (the timer paces it).
        display: Option<DisplayId>,
        /// The window starts on another display than the target's.
        cross_display: bool,
    },
    /// A queued frame write for this window is no longer wanted; it must become
    /// inert before it is issued.
    CancelFrame {
        /// The exact window.
        window: WindowInstance,
    },
    /// Hide this exact application instance. Emitted by the actor shell from
    /// the visibility requirement, never by a pure transition.
    HideApp {
        /// Registry.
        stamp: Stamp,
        /// The exact application instance.
        application: ApplicationId,
    },
    /// Unhide this exact application instance the runtime hid.
    UnhideApp {
        /// Registry.
        stamp: Stamp,
        /// The exact application instance.
        application: ApplicationId,
    },
    /// Activate this exact application: the windowless desktop host (not
    /// Finder), through `activate_exact`, before the hides of its switch.
    ActivateApp {
        /// Registry.
        stamp: Stamp,
        /// The exact application instance.
        application: ApplicationId,
    },
    /// Focus this exact window. Emitted only while the runtime holds the action
    /// authority. The runner unhides an owned-hidden application first, then
    /// activates it through `activate_exact` (the only activation path) and
    /// raises the window through the issuance gate.
    Focus {
        /// Registry.
        stamp: Stamp,
        /// The exact window to focus.
        window: WindowInstance,
    },
    /// The state changed; publish a snapshot.
    Publish,
}

/// One attached display.
#[derive(Clone, Debug, PartialEq)]
pub struct ScreenState {
    /// Native identity.
    pub id: DisplayId,
    /// Display name.
    pub name: Option<String>,
    /// `NSScreen.screens[0]`.
    pub is_primary: bool,
    /// Raw refresh rate (the pipeline clamps it).
    pub refresh_hz: Option<f64>,
    /// Tiling's count of this display's native presences. A display that
    /// disappears and returns gets a new one, so a reused id is detectable.
    pub presence: u64,
}

/// A focus request refused because its workspace was inactive. `serial`
/// increases by one (wrapping) on every refusal of a generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefusedFocus {
    /// The workspace that was refused.
    pub workspace: WorkspaceId,
    /// The refusal counter.
    pub serial: u32,
}

/// One workspace: desired assignment and order.
// Independent facts (visible, focused, overridden, displaced), not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceState {
    /// Identity.
    pub id: WorkspaceId,
    /// Name.
    pub name: String,
    /// The screen it is on.
    pub screen: DisplayId,
    /// The configured screen string, to restore after a hotplug.
    pub configured_screen: Option<String>,
    /// The layout the configuration names for it, if any.
    pub configured_layout: Option<LayoutType>,
    /// The layout in effect (configured, default or a user override).
    pub layout: LayoutType,
    /// The user changed the layout away from the configured one.
    pub layout_overridden: bool,
    /// The configured display is absent, so the main one stands in until it
    /// returns.
    pub displaced: bool,
    /// Runtime ratio overrides.
    pub ratios: RatioState,
    /// Visible on its screen.
    pub visible: bool,
    /// The globally focused workspace.
    pub focused: bool,
    /// Stable window order (desired).
    pub order: Vec<WindowInstance>,
    /// Last focused window in this workspace.
    pub focused_window: Option<WindowInstance>,
}

/// Desired (runtime-decided) facts about a window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DesiredWindow {
    /// The workspace it belongs to; `None` until screens exist.
    pub workspace: Option<WorkspaceId>,
    /// The user floated it explicitly. Layout, reload and refresh never change it.
    pub floating: bool,
    /// Default positioning already happened once for the current floating state.
    pub positioned: bool,
}

/// Observed (reported by the system) facts about a window.
// Independent observable facts, not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq)]
pub struct ObservedWindow {
    /// Title.
    pub title: String,
    /// Role.
    pub role: Option<String>,
    /// Subrole.
    pub subrole: Option<String>,
    /// Frame in the AX global space, when completely read.
    pub frame: Option<Rect>,
    /// Native fullscreen.
    pub fullscreen: bool,
    /// Minimized.
    pub minimized: bool,
    /// The application is hidden (never runtime-owned here).
    pub hidden: bool,
    /// Tab information.
    pub tabs: TabInfo,
    /// Whether every per-window notification registered.
    pub observed: bool,
}

/// A tracked window.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent slot facts of one window, not a state machine"
)]
#[derive(Clone, Debug, PartialEq)]
pub struct WindowState {
    /// The exact token.
    pub token: WindowInstance,
    /// The owning application.
    pub application: ApplicationId,
    /// `CGWindowID` metadata only.
    pub cg_window_id: Option<u32>,
    /// Desired state.
    pub desired: DesiredWindow,
    /// Observed state.
    pub observed: ObservedWindow,
    /// Backend sequence that adopted it; `None` when adopted by a completion.
    pub adopted_seq: Option<u64>,
    /// An inactive member of a native tab group: tracked, never a tile. Derived
    /// from the tab registry, never set by a user.
    pub inactive_tab: bool,
    /// The native tab bar the owner last read for this window. Windows of one
    /// application that carry the same bar are the same tab group.
    pub tab_bar: Option<TabBar>,
    /// The backend sequence at which the window's destroy was held back because
    /// it belonged to a tab group with other tabs: its slot waits for the
    /// window that takes the tab bar over. `None` for a live window.
    pub departing: Option<u64>,
    /// The window is absent from `AXWindows` while its element is alive (an
    /// inactive native tab of a group whose tab bar is hidden or elsewhere): it
    /// keeps its token and is never a tile. A suspended window that just lost
    /// the active role of its slot still [holds](Self::slot_held) the slot until
    /// the listing settles.
    pub suspended: bool,
    /// A suspended window still holding the tile of its slot, waiting for the
    /// window that takes the slot over (settle pending). Laid out like a tile,
    /// never written.
    pub slot_held: bool,
    /// The slot (tab group) the window belongs to: one active member and any
    /// number of suspended members, all of one application. `None` for a window
    /// that never shared a slot.
    pub tab_group: Option<u64>,
    /// A new window carrying a tab bar of two or more tabs, not yet matched to
    /// the suspension it may replace (cleared when the listing settles).
    pub arrival: bool,
    /// A suspended window that was listed again and waits for its slot to be
    /// resolved (cleared when the listing completes).
    pub returning: bool,
    /// Settle listings asked for a window listed again while it still reads
    /// fullscreen (bounded by [`MAX_FULLSCREEN_RECHECKS`]); 0 once it reads
    /// windowed.
    pub fullscreen_rechecks: u8,
    /// A whole re-list series ran out while this window stayed unlisted and
    /// it was not observed fullscreen: it is taken to be on another macOS
    /// Space. Later activations list its application once, without a new
    /// series; a Space change clears it and starts a full series again.
    /// Reset by every new suspension.
    pub relists_spent: bool,
}

impl WindowState {
    /// Whether the window is an inactive tab for layout: the registry-derived
    /// flag, or a suspended member whose slot another window already holds.
    #[must_use]
    pub const fn is_inactive_tab(&self) -> bool {
        self.inactive_tab || (self.suspended && !self.slot_held)
    }

    /// Whether the window occupies the tile of its slot in the workspace order.
    #[must_use]
    pub const fn holds_slot(&self) -> bool { !self.suspended || self.slot_held }

    /// Layout eligibility flags.
    #[must_use]
    pub const fn flags(&self) -> WindowFlags {
        WindowFlags {
            explicit_floating: self.desired.floating,
            inactive_tab: self.is_inactive_tab(),
            fullscreen: self.observed.fullscreen,
            minimized: self.observed.minimized,
            hidden: self.observed.hidden,
        }
    }
}

/// Focus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FocusState {
    /// Focused window.
    pub window: Option<WindowInstance>,
    /// Its workspace.
    pub workspace: Option<WorkspaceId>,
    /// Its screen.
    pub screen: Option<DisplayId>,
}

/// An application the actor knows about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppRecord {
    /// Metadata.
    pub info: AppInfo,
    /// Hidden by the user or system (observed only).
    pub hidden: bool,
    /// Never tracked: Stache itself or a system component.
    pub skipped: bool,
    /// Observation was installed.
    pub tracked: bool,
}

/// What the application's shard reported about its `AXEnhancedUserInterface`
/// lease: counters plus the latest problem. Part 06 reads it to decide whether
/// an application can be animated at all (see [`Self::cannot_keep_up`] and
/// [`Self::set_failed`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EnhancedUiStats {
    /// Leases taken (the attribute was on and was turned off).
    pub leases: u64,
    /// Leases restored.
    pub restores: u64,
    /// Failed attribute reads.
    pub read_failures: u64,
    /// Failed attempts to turn the attribute off.
    pub set_failures: u64,
    /// Failed restores.
    pub restore_failures: u64,
    /// Leases a third party ended by turning the attribute back on.
    pub re_enables: u64,
    /// Consecutive re-enabled leases right now (zero after a clean lease).
    pub consecutive_re_enables: u32,
    /// Whether `VoiceOver` kept the lease off for this application.
    pub voice_over_skipped: bool,
    /// The latest failure, re-enable or restore problem that is still current.
    pub latest_problem: Option<EnhancedUiNote>,
}

impl EnhancedUiStats {
    /// Folds one shard note in.
    pub const fn record(&mut self, note: EnhancedUiNote) {
        match note {
            EnhancedUiNote::Leased => self.leases += 1,
            EnhancedUiNote::Restored => {
                self.restores += 1;
                self.consecutive_re_enables = 0;
                if matches!(self.latest_problem, Some(EnhancedUiNote::RestoreFailed(_))) {
                    self.latest_problem = None;
                }
            }
            EnhancedUiNote::ReadFailed(_) => {
                self.read_failures += 1;
                self.latest_problem = Some(note);
            }
            EnhancedUiNote::SetFailed(_) => {
                self.set_failures += 1;
                self.latest_problem = Some(note);
            }
            EnhancedUiNote::ReEnabled { consecutive } => {
                self.re_enables += 1;
                self.consecutive_re_enables = consecutive;
                self.latest_problem = Some(note);
            }
            EnhancedUiNote::RestoreFailed(_) => {
                self.restore_failures += 1;
                self.latest_problem = Some(note);
            }
            EnhancedUiNote::VoiceOverSkipped => self.voice_over_skipped = true,
        }
    }

    /// Whether something keeps turning the attribute back on: two or more
    /// consecutive leases ended with a re-enable.
    #[must_use]
    pub const fn cannot_keep_up(&self) -> bool { self.consecutive_re_enables >= 2 }

    /// Whether the attribute was on and could not be turned off.
    #[must_use]
    pub const fn set_failed(&self) -> bool { self.set_failures > 0 }
}

/// Unused placeholder kept for the state shape: the runtime-owned visibility
/// ledger lives in [`crate::visibility`] because the stop path needs it after
/// the actor ends.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VisibilityLedger {}

/// State-side diagnostics (admission counters are added when publishing).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diagnostics {
    /// Events dropped for a stale epoch.
    pub stale_epoch_events: u64,
    /// Completions dropped for a stale registry or epoch.
    pub stale_completions: u64,
    /// Snapshots dropped because the token was already destroyed.
    pub ignored_after_destroy: u64,
    /// Windows adopted for an application with no metadata.
    pub unknown_app_windows: u64,
    /// Windows never managed.
    pub excluded_windows: u64,
    /// Reconciliations started.
    pub reconciliations_started: u64,
    /// Reconciliations confirmed.
    pub reconciliations_completed: u64,
    /// Destroys proven missed and repaired by a reconciliation.
    pub missed_destroys_repaired: u64,
    /// Stale numeric `CGWindowID` owners evicted.
    pub cg_reuse_evictions: u64,
    /// Overflow-style markings by cause.
    pub unconfirmed_markings: u64,
    /// Stale configuration revisions dropped.
    pub stale_configs: u64,
    /// Reload mappings refused; the old workspace state stayed.
    pub rejected_reloads: u64,
    /// Intents that changed desired state.
    pub intents_applied: u64,
    /// Intents admitted that changed nothing.
    pub intents_noop: u64,
    /// Intents refused.
    pub intents_rejected: u64,
    /// Focus intents recorded without a native focus effect (no authority).
    pub focus_unauthorized: u64,
    /// Focus events for an unknown, unexcluded window.
    pub unknown_focus: u64,
    /// Snapshot reads the backend refused or could not verify.
    pub snapshot_refusals: u64,
    /// Registry restarts.
    pub registry_restarts: u64,
    /// Applications whose enumeration was truncated or failed.
    pub enumeration_failures: u64,
    /// Whether the stream is overloaded (consecutive overflows).
    pub overloaded: bool,
    /// Frame attempts created (each component at most once per attempt).
    pub frame_attempts: u64,
    /// Attempts confirmed by readback.
    pub frames_applied: u64,
    /// Attempts with an unconfirmed or refused component.
    pub frames_partial: u64,
    /// Attempts issued without a trustworthy result.
    pub frames_uncertain: u64,
    /// Attempts the system refused outright.
    pub frames_failed: u64,
    /// Attempts proven not sent (including stale tokens).
    pub frames_not_attempted: u64,
    /// Acknowledgements of an old plan, window or revision, dropped.
    pub stale_frame_acks: u64,
    /// Own geometry echoes of a settled animation, ignored (bounded).
    pub animation_echoes_ignored: u64,
    /// Host reservation hints older than the one held, dropped.
    pub stale_reservations: u64,
    /// Drag gestures captured.
    pub drag_gestures: u64,
    /// Mouse downs that started no gesture (writes off, no tiles, floating layout).
    pub drag_ignored: u64,
    /// Gestures that ended in a swap.
    pub drag_swaps: u64,
    /// Gestures that ended in a resize.
    pub drag_resizes: u64,
    /// Gestures that ended in a snap-back plan.
    pub drag_snap_backs: u64,
    /// Gestures that failed revalidation at commit.
    pub drag_stale: u64,
    /// Gestures cancelled by a lost observation.
    pub drag_cancelled: u64,
    /// Lost observations reported (with or without a gesture).
    pub drag_lost_observations: u64,
    /// Tab groups whose representing window was swapped for another one that
    /// carries the same tab bar (a new tab, a switch or a close).
    pub tab_handovers: u64,
    /// Destroys held back until the tab group settled.
    pub tab_departures_held: u64,
    /// Held-back windows closed for real: no window took their tab bar over.
    pub tab_departures_closed: u64,
    /// Windows that left `AXWindows` while their element stayed alive.
    pub tab_suspensions: u64,
    /// Suspended windows listed again that took their slot back.
    pub tab_returns: u64,
    /// Single-tab handovers: a new tabbed window took the slot of the one
    /// suspended window it replaced.
    pub tab_single_handovers: u64,
    /// Suspended windows whose slot nobody took over: the tile was released.
    pub tab_slots_released: u64,
}

/// One enumeration or refresh in progress.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scan {
    /// Backend sequence processed when the scan was requested.
    pub floor: u64,
    /// Tokens an enumeration snapshot reported.
    pub seen: BTreeSet<WindowInstance>,
}

/// Reconciliation bookkeeping; every collection is bounded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reconcile {
    /// Applications whose state is not confirmed and need a refresh.
    pub unconfirmed: BTreeSet<ApplicationId>,
    /// Everything is unconfirmed.
    pub all: bool,
    /// Applications with an enumeration outstanding.
    pub inflight: BTreeSet<ApplicationId>,
    /// Open scans.
    pub scans: BTreeMap<ApplicationId, Scan>,
    /// Overflows since the last confirmed reconciliation.
    pub consecutive_overflows: u32,
    /// The running-application list was requested.
    pub listing: bool,
    /// An unresolved termination arrived while a list was in flight; that list
    /// may predate it, so exactly one follow-up list runs when it completes.
    pub relist: bool,
    /// Unlisted applications whose activation already asked for a list, so a
    /// permanently unlisted one (a non-regular application) asks only once.
    pub probed: BTreeSet<ApplicationId>,
    /// Applications whose latest listing skipped windows that were still in
    /// transition; their held-back windows wait for the next listing.
    pub unsettled: BTreeSet<ApplicationId>,
}

/// How many settle listings re-read a window that was listed again (a native
/// fullscreen exit) while it still reads fullscreen: macOS flips the flag after
/// the exit's last notification, so nothing else reports it. 20 listings at
/// the tab settle delay (150 ms) cover about 3 s of a slow exit animation.
pub const MAX_FULLSCREEN_RECHECKS: u8 = 20;

/// What the pure transition asks the actor shell to do with its clock: the
/// shell owns time, the state never reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TabOutbox {
    /// Applications that need one settled listing: `false` for a fresh trigger,
    /// `true` for a retry after a listing that skipped windows in transition.
    pub settle: BTreeMap<ApplicationId, bool>,
    /// Handovers `(old, new)` since the shell last looked, for the debug log.
    pub handovers: Vec<(WindowInstance, WindowInstance)>,
    /// Slot events for the debug log (suspensions, returns, releases).
    pub notes: Vec<TabNote>,
}

/// One slot event the shell logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabNote {
    /// A window left `AXWindows` while its element is alive.
    Suspended(WindowInstance),
    /// A suspended window was listed again and took the slot back from the
    /// second token (the window that held it, now closed or suspended).
    Returned(WindowInstance, WindowInstance),
    /// A suspended window was listed again and holds a slot that was free.
    Resumed(WindowInstance),
    /// A new tabbed window took over the slot of the one suspended window
    /// `(suspended, new)`.
    SingleHandover(WindowInstance, WindowInstance),
    /// A suspended window's slot had no active member after the listing
    /// settled: its tile was released.
    Released(WindowInstance),
}

/// The most handovers remembered for the log between two shell turns.
pub const MAX_PENDING_HANDOVERS: usize = 64;

/// Startup/tracking bookkeeping.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Startup {
    /// Applications waiting for a track slot.
    pub pending: VecDeque<AppInfo>,
    /// `track` requests awaiting acknowledgement.
    pub awaiting: BTreeSet<ApplicationId>,
    /// Applications that failed to track or enumerate.
    pub failed: BTreeSet<ApplicationId>,
    /// Why each failed application failed (diagnostics).
    pub failure_reasons: BTreeMap<ApplicationId, String>,
    /// Failed applications whose failure is plausibly transient (the actor
    /// retries those on a time backoff).
    pub transient: BTreeSet<ApplicationId>,
    /// Retries spent per failed or truncated application.
    pub retries: BTreeMap<ApplicationId, u8>,
    /// Applications whose enumeration was truncated.
    pub truncated: BTreeSet<ApplicationId>,
    /// The application list was received at least once.
    pub listed: bool,
}

/// Focus notifications and reads that wait for a window to be adopted. Bounded:
/// one buffered notification per application, a short life, a small map.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PendingFocus {
    /// The latest focus notification of an application for a window not yet
    /// tracked, with the transition sequence it arrived at.
    pub buffered: BTreeMap<ApplicationId, (WindowInstance, u64)>,
    /// Frontmost applications whose real focused window is read at the end of
    /// the transition (after a window was adopted or an app-level focus event).
    pub reads: BTreeSet<ApplicationId>,
    /// Applications whose focused-window read was issued and has not answered.
    /// The frontmost one is not hidden meanwhile: the answer may make its
    /// workspace the visible one, and a hide would take the system focus away.
    pub inflight: BTreeSet<ApplicationId>,
}

/// Transitions a buffered focus notification stays valid for.
pub const PENDING_FOCUS_LIFE: u64 = 256;
/// Upper bound on buffered notifications.
pub const MAX_PENDING_FOCUS: usize = 64;

/// The whole actor-owned state.
// Independent facts (screens known, completed once, accessibility), not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug)]
pub struct TilingState {
    pub(crate) self_bundle_id: String,
    pub(crate) config: Option<Arc<TilingConfig>>,
    pub(crate) config_revision: u64,
    pub(crate) workspace_rules: Vec<(String, PreparedRules)>,
    pub(crate) ignore: PreparedRules,
    pub(crate) stamp: Option<Stamp>,
    pub(crate) sequence: u64,
    pub(crate) last_event_sequence: u64,
    pub(crate) layout: DisplayLayout,
    pub(crate) screens: Vec<ScreenState>,
    pub(crate) screens_known: bool,
    pub(crate) next_presence: u64,
    pub(crate) host: HostReservations,
    pub(crate) frames: FrameBook,
    pub(crate) no_screens: bool,
    pub(crate) workspaces: Vec<WorkspaceState>,
    pub(crate) next_workspace: u32,
    pub(crate) windows: BTreeMap<WindowInstance, WindowState>,
    pub(crate) excluded: BTreeMap<WindowInstance, ManagementExclusion>,
    pub(crate) cg_index: BTreeMap<u32, WindowInstance>,
    pub(crate) apps: BTreeMap<ApplicationId, AppRecord>,
    pub(crate) active_application: Option<ApplicationId>,
    pub(crate) pending_focus: PendingFocus,
    pub(crate) focus: FocusState,
    pub(crate) focus_history: VecDeque<WindowInstance>,
    pub(crate) workspace_history: VecDeque<WorkspaceId>,
    pub(crate) last_intent: Option<IntentRecord>,
    pub(crate) last_reload_error: Option<String>,
    /// The last focus request refused because its workspace was inactive.
    pub(crate) refused_focus: Option<RefusedFocus>,
    pub(crate) tombstones: BTreeSet<WindowInstance>,
    pub(crate) tombstone_order: VecDeque<WindowInstance>,
    pub(crate) ledger: VisibilityLedger,
    /// Applications Stache itself holds hidden (or is unhiding). The actor, the
    /// only writer, copies them from the visibility ledger before each
    /// transition; their windows keep their layout slot while their workspace
    /// is visible, unlike an application someone else hid.
    pub(crate) owned_hidden: BTreeSet<ApplicationId>,
    /// Applications that move without animation (the instant fallback). The
    /// actor copies them from the animation control before each transition;
    /// the planner writes their windows directly.
    pub(crate) animation_fallback: BTreeSet<(ApplicationId, crate::animation::MotionKind)>,
    pub(crate) tabs: TabRegistry,
    pub(crate) tab_outbox: TabOutbox,
    /// Applications listed again after a Space change or an activation because
    /// a window left `AXWindows` without a replacement: the settle listings
    /// asked so far while it stays unlisted (bounded by
    /// [`MAX_FULLSCREEN_RECHECKS`]).
    pub(crate) unlisted_relists: BTreeMap<ApplicationId, u8>,
    /// Mints the ids of slots (tab groups).
    pub(crate) next_tab_group: u64,
    pub(crate) reconcile: Reconcile,
    pub(crate) startup: Startup,
    pub(crate) accessibility: bool,
    pub(crate) completed_once: bool,
    pub(crate) health: Health,
    pub(crate) diagnostics: Diagnostics,
    pub(crate) drag: DragState,
    /// Lease facts per application (bounded by the known applications).
    pub(crate) enhanced_ui: BTreeMap<ApplicationId, EnhancedUiStats>,
}

impl TilingState {
    /// Replaces the applications the runtime holds hidden. Called by the actor
    /// before a transition, so the transition itself stays pure.
    pub fn set_owned_hidden(&mut self, owned: BTreeSet<ApplicationId>) {
        self.owned_hidden = owned;
    }

    /// Replaces the applications that move without animation. Called by the
    /// actor before a transition, so the transition itself stays pure.
    pub fn set_animation_fallback(
        &mut self,
        fallback: BTreeSet<(ApplicationId, crate::animation::MotionKind)>,
    ) {
        self.animation_fallback = fallback;
    }

    /// Takes what the last transition asked of the shell's clock.
    pub fn take_tab_outbox(&mut self) -> TabOutbox { std::mem::take(&mut self.tab_outbox) }

    /// Layout eligibility flags of a window. A window whose application the
    /// runtime hid is not `hidden` here: it keeps its slot (it is shown again
    /// as soon as its workspace is visible). Only an application hidden by
    /// someone else is excluded.
    #[must_use]
    pub fn flags_of(&self, window: &WindowState) -> WindowFlags {
        let mut flags = window.flags();
        flags.hidden &= !self.owned_hidden.contains(&window.application);
        flags
    }

    /// An empty state. `self_bundle_id` is Stache's own bundle identifier
    /// (empty disables the self exclusion).
    #[must_use]
    pub fn new(self_bundle_id: impl Into<String>) -> Self {
        Self {
            self_bundle_id: self_bundle_id.into(),
            config: None,
            config_revision: 0,
            workspace_rules: Vec::new(),
            ignore: PreparedRules::default(),
            stamp: None,
            sequence: 0,
            last_event_sequence: 0,
            layout: DisplayLayout::default(),
            screens: Vec::new(),
            screens_known: false,
            next_presence: 0,
            host: HostReservations::empty(),
            frames: FrameBook::default(),
            no_screens: false,
            workspaces: Vec::new(),
            next_workspace: 1,
            windows: BTreeMap::new(),
            excluded: BTreeMap::new(),
            cg_index: BTreeMap::new(),
            apps: BTreeMap::new(),
            active_application: None,
            pending_focus: PendingFocus::default(),
            focus: FocusState::default(),
            focus_history: VecDeque::new(),
            workspace_history: VecDeque::new(),
            last_intent: None,
            last_reload_error: None,
            refused_focus: None,
            tombstones: BTreeSet::new(),
            tombstone_order: VecDeque::new(),
            ledger: VisibilityLedger::default(),
            owned_hidden: BTreeSet::new(),
            animation_fallback: BTreeSet::new(),
            tabs: TabRegistry::default(),
            tab_outbox: TabOutbox::default(),
            unlisted_relists: BTreeMap::new(),
            next_tab_group: 1,
            reconcile: Reconcile::default(),
            startup: Startup::default(),
            accessibility: true,
            completed_once: false,
            health: Health::Healthy,
            diagnostics: Diagnostics::default(),
            drag: DragState::default(),
            enhanced_ui: BTreeMap::new(),
        }
    }

    /// What the shard reported about `application`'s enhanced user interface
    /// lease, if anything.
    #[must_use]
    pub fn enhanced_ui(&self, application: ApplicationId) -> Option<&EnhancedUiStats> {
        self.enhanced_ui.get(&application)
    }

    /// Declares whether the backend reads native tab groups. Without reads
    /// every window is an ordinary window.
    pub fn set_tab_reads(&mut self, reading: bool) {
        self.tabs.set_reading(reading);
        if !reading {
            for window in self.windows.values_mut() {
                window.inactive_tab = false;
            }
        }
    }

    /// The future visibility ledger (empty until part 06).
    #[must_use]
    pub const fn ledger(&self) -> &VisibilityLedger { &self.ledger }

    /// The current registry stamp.
    #[must_use]
    pub const fn stamp(&self) -> Option<Stamp> { self.stamp }

    /// The display a window is on: by frame, else its workspace's screen.
    #[must_use]
    pub fn display_of(&self, token: WindowInstance) -> Option<DisplayId> {
        let window = self.windows.get(&token)?;
        window
            .observed
            .frame
            .and_then(|frame| {
                self.layout.display_for_ax_rect(stache_macos::applications::DisplayRect::new(
                    frame.x,
                    frame.y,
                    frame.width,
                    frame.height,
                ))
            })
            .or_else(|| {
                let id = window.desired.workspace?;
                self.workspaces.iter().find(|ws| ws.id == id).map(|ws| ws.screen)
            })
    }

    /// Whether a destroyed token was seen (kept for diagnostics and tests).
    #[must_use]
    pub fn is_tombstoned(&self, token: WindowInstance) -> bool { self.tombstones.contains(&token) }

    /// The phase derived from the current facts.
    #[must_use]
    pub fn phase(&self) -> Phase {
        // Only global faults make the phase `Incomplete`. An application that
        // failed or was truncated degrades itself alone (see `unsettled_apps`).
        if self.faulted() {
            return Phase::Incomplete;
        }
        let working = self.stamp.is_none()
            || !self.screens_known
            || !self.startup.listed
            || !self.startup.pending.is_empty()
            || !self.startup.awaiting.is_empty()
            || !self.reconcile.inflight.is_empty()
            || !self.reconcile.unconfirmed.is_empty()
            || self.reconcile.all
            || self.reconcile.listing;
        match (working, self.completed_once) {
            (false, _) => Phase::Complete,
            (true, false) => Phase::Initializing,
            (true, true) => Phase::Reconciling,
        }
    }

    /// A fault that affects every window: overload, no screens, missing
    /// Accessibility permission or a failed actor.
    #[must_use]
    pub fn faulted(&self) -> bool { !self.fault_reasons().is_empty() }

    /// The global faults that currently hold every frame write back.
    #[must_use]
    pub fn fault_reasons(&self) -> Vec<String> {
        let mut reasons = Vec::new();
        if self.no_screens {
            reasons.push("no display is attached".to_owned());
        }
        if !self.accessibility {
            reasons.push("Accessibility permission is missing".to_owned());
        }
        if self.diagnostics.overloaded {
            reasons.push("the event stream is overloaded".to_owned());
        }
        if let Health::Failed(fault) = self.health {
            reasons.push(format!("the actor failed: {fault:?}"));
        }
        reasons
    }

    /// Whether the planner may write frames at all: the first reconciliation is
    /// done and no global fault stands. Applications that failed or are being
    /// reconciled are excluded individually (see `unsettled_apps`).
    #[must_use]
    pub fn planning_ready(&self) -> bool {
        matches!(self.phase(), Phase::Complete | Phase::Reconciling)
    }

    /// Applications whose windows must not be written because their own state
    /// is unconfirmed: failed or truncated enumerations, tracking in progress,
    /// a refresh in flight or owed. `None` means every application (a global
    /// reconciliation is owed).
    #[must_use]
    pub fn unsettled_apps(&self) -> Option<BTreeSet<ApplicationId>> {
        if self.reconcile.all {
            return None;
        }
        let mut apps: BTreeSet<ApplicationId> = BTreeSet::new();
        apps.extend(self.startup.failed.iter().copied());
        apps.extend(self.startup.truncated.iter().copied());
        apps.extend(self.startup.awaiting.iter().copied());
        apps.extend(self.startup.pending.iter().map(|info| info.application));
        apps.extend(self.reconcile.inflight.iter().copied());
        apps.extend(self.reconcile.unconfirmed.iter().copied());
        Some(apps)
    }

    fn failed_apps(&self, truncated: bool) -> Vec<FailedApp> {
        let (set, default_reason) = if truncated {
            (&self.startup.truncated, "enumeration truncated")
        } else {
            (&self.startup.failed, "tracking failed")
        };
        set.iter()
            .map(|application| FailedApp {
                application: *application,
                reason: self
                    .startup
                    .failure_reasons
                    .get(application)
                    .cloned()
                    .unwrap_or_else(|| default_reason.to_owned()),
                retries: self.startup.retries.get(application).copied().unwrap_or(0),
            })
            .collect()
    }

    /// An immutable view for queries.
    #[must_use]
    pub fn snapshot(&self) -> TilingSnapshot {
        let phase = self.phase();
        let visibility = crate::workspaces::visibility_requirement(self);
        let unreachable = crate::workspaces::unreachable(self, &visibility);
        TilingSnapshot {
            sequence: self.sequence,
            phase,
            health: self.health,
            actions_allowed: matches!(phase, Phase::Complete | Phase::Reconciling),
            faults: self.fault_reasons(),
            failed_apps: self.failed_apps(false),
            truncated_apps: self.failed_apps(true),
            registry: self.stamp.map(|stamp| stamp.registry),
            epoch: self.stamp.map(|stamp| stamp.epoch.value()),
            config_revision: self.config_revision,
            screens: self.screens.clone(),
            workspaces: self.workspaces.clone(),
            windows: self.windows.values().cloned().collect(),
            excluded: self.excluded.iter().map(|(token, why)| (*token, *why)).collect(),
            focus: self.focus,
            unconfirmed_apps: self.reconcile.unconfirmed.len(),
            all_unconfirmed: self.reconcile.all,
            inflight_apps: self.reconcile.inflight.len(),
            no_screens: self.no_screens,
            diagnostics: self.diagnostics.clone(),
            admission: crate::events::AdmissionStats::default(),
            plan_sequence: self.frames.plan_sequence,
            frames: frames::statuses(self),
            no_frame: self.frames.no_frame.iter().map(|(id, why)| (*id, *why)).collect(),
            host_revision: self.host.revision,
            visibility,
            last_intent: self.last_intent.clone(),
            workspace_history: self.workspace_history.iter().copied().collect(),
            last_reload_error: self.last_reload_error.clone(),
            unreachable,
            refused_focus: self.refused_focus,
            hidden_apps: Vec::new(),
            animation_fallbacks: Vec::new(),
            display_layout: self.layout.clone(),
            apps: self.apps.values().cloned().collect(),
            tabs: self.tabs.status(self.windows.values().filter(|w| w.is_inactive_tab()).count()),
            drag: drag::report(self),
            enhanced_ui: self.enhanced_ui.iter().map(|(app, stats)| (*app, *stats)).collect(),
        }
    }

    /// The global application visibility requirement (a pure function of this
    /// state). Slice B's ledger consumes it.
    #[must_use]
    pub fn visibility_requirement(&self) -> VisibilityRequirement {
        crate::workspaces::visibility_requirement(self)
    }

    /// Whether `FocusWorkspace(name)` would be refused because the workspace
    /// is inactive (the same rule the intent applies).
    #[must_use]
    pub fn refuses_focus(&self, name: &str) -> bool { intents::refuses_focus(self, name) }

    /// The identity of the drag gesture in progress, if any.
    #[must_use]
    pub fn drag_gesture(&self) -> Option<u64> { self.drag.gesture_id() }

    /// The most recent intent and its outcome.
    #[must_use]
    pub const fn last_intent(&self) -> Option<&IntentRecord> { self.last_intent.as_ref() }

    /// A state rebuilt from a published snapshot, for staging a reload before
    /// the actor sees it. It has no configuration and no application metadata.
    #[must_use]
    pub fn replica(snapshot: &TilingSnapshot) -> Self {
        let mut state = Self::new("");
        state.screens.clone_from(&snapshot.screens);
        state.screens_known = !snapshot.screens.is_empty();
        state.workspaces.clone_from(&snapshot.workspaces);
        state.next_workspace =
            snapshot.workspaces.iter().map(|ws| ws.id.0).max().map_or(1, |max| max + 1);
        state.windows = snapshot.windows.iter().map(|w| (w.token, w.clone())).collect();
        state.excluded = snapshot.excluded.iter().copied().collect();
        state.focus = snapshot.focus;
        state
    }
}

/// The atomic, immutable result of the actor's last published turn.
///
/// Queries clone an `Arc` of this under a short lock; no field is ever sampled
/// separately from the others.
#[derive(Clone, Debug, PartialEq)]
pub struct TilingSnapshot {
    /// Inputs applied so far.
    pub sequence: u64,
    /// Completeness.
    pub phase: Phase,
    /// Health.
    pub health: Health,
    /// Whether later parts may issue native mutations (always read-only now).
    pub actions_allowed: bool,
    /// Global faults holding every frame write back (empty when none).
    pub faults: Vec<String>,
    /// Applications whose tracking or enumeration failed. Only their own
    /// windows are held back; they are retried a bounded number of times.
    pub failed_apps: Vec<FailedApp>,
    /// Applications whose enumeration was truncated.
    pub truncated_apps: Vec<FailedApp>,
    /// The registry generation.
    pub registry: Option<u64>,
    /// The backend epoch value.
    pub epoch: Option<u64>,
    /// The accepted configuration revision.
    pub config_revision: u64,
    /// Screens.
    pub screens: Vec<ScreenState>,
    /// Workspaces (desired order).
    pub workspaces: Vec<WorkspaceState>,
    /// Tracked windows (desired and observed).
    pub windows: Vec<WindowState>,
    /// Windows deliberately not managed.
    pub excluded: Vec<(WindowInstance, ManagementExclusion)>,
    /// Focus.
    pub focus: FocusState,
    /// Applications waiting for a refresh.
    pub unconfirmed_apps: usize,
    /// Everything is waiting for a refresh.
    pub all_unconfirmed: bool,
    /// Applications with an enumeration in flight.
    pub inflight_apps: usize,
    /// No display is attached.
    pub no_screens: bool,
    /// State diagnostics.
    pub diagnostics: Diagnostics,
    /// Admission accounting.
    pub admission: crate::events::AdmissionStats,
    /// The monotonic layout plan sequence.
    pub plan_sequence: u64,
    /// Desired versus confirmed frames and the latest outcome per window.
    pub frames: Vec<FrameStatus>,
    /// Displays that produced no frame, with the reason.
    pub no_frame: Vec<(DisplayId, NoFrameReason)>,
    /// The host reservation revision in use.
    pub host_revision: u64,
    /// The application visibility requirement across all visible workspaces.
    pub visibility: VisibilityRequirement,
    /// The latest intent and its outcome.
    pub last_intent: Option<IntentRecord>,
    /// Workspace focus history, oldest first.
    pub workspace_history: Vec<WorkspaceId>,
    /// Why the latest reload mapping was refused, if it was.
    pub last_reload_error: Option<String>,
    /// Workspaces Stache cannot show as an empty desktop (published as
    /// `inactive`; see [`crate::workspaces::unreachable`]).
    pub unreachable: Vec<WorkspaceId>,
    /// The last focus request refused because its workspace was inactive.
    pub refused_focus: Option<RefusedFocus>,
    /// Applications the runtime hid (or may have hid), with their ownership.
    pub hidden_apps: Vec<crate::visibility::OwnedApp>,
    /// Applications in the animation instant fallback, per kind, with the time
    /// left of a cool-down when the snapshot was published (`None` is sticky;
    /// zero means on its next plan). Filled by the actor.
    pub animation_fallbacks: Vec<(crate::animation::FallbackEntry, Option<std::time::Duration>)>,
    /// The native display layout (frames, scale, built-in flag) behind `screens`.
    pub display_layout: DisplayLayout,
    /// The application catalog the actor knows, in stable application order.
    pub apps: Vec<AppRecord>,
    /// The native tab capability and group counts.
    pub tabs: TabsStatus,
    /// The drag gesture in progress and the latest finished one.
    pub drag: DragReport,
    /// The `AXEnhancedUserInterface` lease facts per application.
    pub enhanced_ui: Vec<(ApplicationId, EnhancedUiStats)>,
}

impl TilingSnapshot {
    /// The lease facts of one application.
    #[must_use]
    pub fn enhanced_ui_of(&self, application: ApplicationId) -> Option<&EnhancedUiStats> {
        self.enhanced_ui
            .iter()
            .find(|(app, _)| *app == application)
            .map(|(_, stats)| stats)
    }

    /// An empty snapshot before the first turn.
    #[must_use]
    pub fn initial() -> Self { TilingState::new("").snapshot() }

    /// The workspace with this name.
    #[must_use]
    pub fn workspace(&self, name: &str) -> Option<&WorkspaceState> {
        self.workspaces.iter().find(|ws| ws.name == name)
    }

    /// The window with this token.
    #[must_use]
    pub fn window(&self, token: WindowInstance) -> Option<&WindowState> {
        self.windows.iter().find(|window| window.token == token)
    }
}
