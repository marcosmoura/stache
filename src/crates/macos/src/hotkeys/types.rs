//! Plain, `Send` values exchanged with the main-owned hotkey store.

use stache_sdk::input::{InputPriority, ModifierFlags, ReservationId};

use crate::keyboard_layout::{LayoutId, SectionSource};

/// What an opaque ID is scoped to: one control, one generation (bumped by every
/// stop) and one revision (bumped by every accepted commit).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Scope {
    pub(crate) control: u64,
    pub(crate) generation: u64,
    pub(crate) revision: u64,
}

impl Scope {
    /// The control that issued the ID.
    #[must_use]
    pub const fn control(&self) -> u64 { self.control }

    /// The generation (service lifetime between stops).
    #[must_use]
    pub const fn generation(&self) -> u64 { self.generation }

    /// The committed revision.
    #[must_use]
    pub const fn revision(&self) -> u64 { self.revision }
}

/// An opaque, `Send` handle to one registered binding, given to the sink.
///
/// It names a native registration that is never reused, scoped by the issuing
/// control, generation and revision. It carries no Carbon pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BindingToken {
    pub(crate) scope: Scope,
    pub(crate) native: u32,
    pub(crate) key: u64,
}

impl BindingToken {
    /// The scope the token was issued in.
    #[must_use]
    pub const fn scope(&self) -> Scope { self.scope }

    /// The caller's key of the binding (see [`HotkeyRequest::key`]).
    #[must_use]
    pub const fn key(&self) -> u64 { self.key }

    /// The never-reused native hotkey ID.
    #[must_use]
    pub const fn native_id(&self) -> u32 { self.native }
}

/// What to register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyTarget {
    /// A physical chord from the ordinary grammar.
    Physical {
        /// The physical key.
        code: keyboard_types::Code,
        /// The configured modifiers.
        modifiers: ModifierFlags,
    },
    /// `[modifiers+]§`, resolved from the keyboard layout on the main thread.
    Section {
        /// The configured modifiers; layout-required ones are OR-ed in.
        modifiers: ModifierFlags,
    },
}

/// One requested registration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HotkeyRequest {
    /// A caller-chosen identity, echoed in tokens and outcomes. Keys must be
    /// unique within one request list.
    pub key: u64,
    /// What to register.
    pub target: HotkeyTarget,
    /// The priority published on the central reservation.
    pub priority: InputPriority,
}

/// The physical chord a binding resolved to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedChord {
    /// The macOS virtual key code.
    pub key_code: u16,
    /// The full modifiers (configured OR layout-required).
    pub modifiers: ModifierFlags,
    /// For `§` bindings, how the key was found.
    pub section: Option<SectionSource>,
}

/// The truthful result for one binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingOutcome {
    /// Registered and routed.
    Active {
        /// The token the sink will receive.
        token: BindingToken,
        /// The chord.
        chord: ResolvedChord,
        /// Whether an existing registration was transferred, not re-registered.
        reused: bool,
    },
    /// The chord is held by another registration.
    Occupied {
        /// The `OSStatus`.
        status: i32,
    },
    /// Registration failed for another reason.
    Failed {
        /// The `OSStatus`, or 0 when the failure was not native.
        status: i32,
        /// A description.
        detail: String,
    },
    /// The key or modifier has no Carbon mapping.
    Unsupported(String),
    /// Another binding owns the same physical chord; this one was not registered.
    Collision {
        /// The key of the binding that kept the chord.
        winner: u64,
    },
    /// The handler could not be installed, so nothing was attempted.
    HandlerUnavailable {
        /// The `OSStatus` of the install.
        status: i32,
    },
}

impl BindingOutcome {
    /// Whether the binding is routed.
    #[must_use]
    pub const fn is_active(&self) -> bool { matches!(self, Self::Active { .. }) }
}

/// The state of the single Carbon handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandlerStatus {
    /// No handler is installed.
    NotInstalled,
    /// Installed.
    Installed,
    /// The last install attempt failed with this `OSStatus`.
    InstallFailed(i32),
    /// Removal failed with this `OSStatus`; the handler is still installed.
    RemovalPending(i32),
}

/// An opaque staged candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StagedId {
    pub(crate) scope: Scope,
    pub(crate) serial: u64,
}

/// A staged binding before any native registration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StagedBinding {
    /// Will be registered at commit.
    New(ResolvedChord),
    /// An existing registration of the same chord will be transferred.
    Reuse(ResolvedChord),
    /// Cannot be registered; the outcome is final.
    Rejected(BindingOutcome),
}

/// The result of [`HotkeyControl::prepare`](super::HotkeyControl::prepare).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedReport {
    /// The ID to commit.
    pub id: StagedId,
    /// The layout the chords were resolved against; `None` when unavailable.
    pub layout: Option<LayoutId>,
    /// One entry per request, in request order.
    pub bindings: Vec<(u64, StagedBinding)>,
}

/// The result of a commit or refresh.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitReport {
    /// The scope after the commit (unchanged when nothing was accepted).
    pub scope: Scope,
    /// The layout the active set corresponds to.
    pub layout: Option<LayoutId>,
    /// One entry per request, in request order. Never claims more than is active.
    pub outcomes: Vec<(u64, BindingOutcome)>,
    /// Previously active registrations that were logically removed.
    pub removed: usize,
    /// Registrations whose native cleanup is still owed.
    pub pending_cleanup: usize,
    /// The handler state.
    pub handler: HandlerStatus,
}

impl CommitReport {
    /// Whether every requested binding is active and no cleanup is owed.
    #[must_use]
    pub fn all_active(&self) -> bool {
        self.pending_cleanup == 0 && self.outcomes.iter().all(|(_, outcome)| outcome.is_active())
    }
}

/// The result of [`HotkeyControl::stop`](super::HotkeyControl::stop).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StopReport {
    /// Registrations removed natively.
    pub unregistered: usize,
    /// Registrations that could not be removed and remain as inert guards.
    pub pending_cleanup: usize,
    /// The handler state.
    pub handler: HandlerStatus,
    /// Whether the layout observer could not be removed.
    pub observer_pending: bool,
}

impl StopReport {
    /// Whether nothing native remains.
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        self.pending_cleanup == 0
            && !self.observer_pending
            && matches!(self.handler, HandlerStatus::NotInstalled)
    }
}

/// One active binding in a [`Status`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveBinding {
    /// The caller's key.
    pub key: u64,
    /// The sink token.
    pub token: BindingToken,
    /// The chord.
    pub chord: ResolvedChord,
    /// The central Carbon reservation.
    pub reservation: ReservationId,
}

/// A snapshot of the main-owned store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    /// The current scope.
    pub scope: Scope,
    /// The handler state.
    pub handler: HandlerStatus,
    /// Routed bindings.
    pub active: Vec<ActiveBinding>,
    /// Native IDs still registered but inert (owed cleanup).
    pub pending_cleanup: Vec<u32>,
    /// The layout the active set corresponds to.
    pub layout: Option<LayoutId>,
    /// Whether the layout observer is installed.
    pub observing: bool,
    /// How many layout refresh jobs have run.
    pub refreshes: u64,
    /// The most recent refresh result.
    pub last_refresh: Option<CommitReport>,
    /// Bounded diagnostics, oldest first.
    pub diagnostics: Vec<String>,
}

/// Why a request to the main-owned store was not carried out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HotkeyError {
    /// The caller cancelled before or during the operation.
    Cancelled,
    /// No acknowledgement arrived within the budget; the work may still run
    /// late, and a late job rechecks cancellation before it mutates.
    TimedOut,
    /// The host dispatcher rejected the job.
    Dispatch(String),
    /// The job panicked on the main thread.
    Panicked,
    /// The control was closed.
    Closed,
    /// The staged candidate no longer matches the store.
    Stale(StaleReason),
    /// A rollback after cancellation or staleness left registrations owed
    /// cleanup.
    CleanupPending {
        /// The number of inert guards retained.
        pending: usize,
        /// The original reason.
        cause: Box<Self>,
    },
}

/// What invalidated a staged candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StaleReason {
    /// The service was stopped (generation changed).
    Generation,
    /// A newer commit changed the revision.
    Revision,
    /// The keyboard layout changed.
    Layout,
    /// The candidate was replaced or never staged.
    Unknown,
}

impl std::fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("the operation was cancelled"),
            Self::TimedOut => f.write_str("the main thread did not acknowledge in time"),
            Self::Dispatch(detail) => write!(f, "the main thread rejected the job: {detail}"),
            Self::Panicked => f.write_str("the native job panicked"),
            Self::Closed => f.write_str("the hotkey control is closed"),
            Self::Stale(reason) => write!(f, "the staged candidate is stale ({reason:?})"),
            Self::CleanupPending { pending, cause } => {
                write!(f, "{cause}; {pending} registration(s) still owe cleanup")
            }
        }
    }
}

impl std::error::Error for HotkeyError {}

/// The non-blocking consumer of accepted presses. The Keybindings router
/// implements it; it is called on the main thread from the Carbon callback and
/// must only enqueue bounded work.
pub trait HotkeySink: Send + Sync {
    /// A bound chord was pressed and may dispatch.
    fn pressed(&self, token: BindingToken);
}
