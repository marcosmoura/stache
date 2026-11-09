//! The typed result of a layout calculation.
//!
//! A [`LayoutPlan`] accounts for **every** input identity exactly once: tiled
//! with a desired frame, grid overflow, untouched (floating) or excluded with a
//! reason. Nothing is silently dropped.

use crate::geometry::{GeometryError, Rect};

/// Observable flags of one tracked window that decide layout eligibility.
// Independent observable facts, not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowFlags {
    /// The user explicitly floated the window. Layout never changes this.
    pub explicit_floating: bool,
    /// The window is a native tab that is not the active tab.
    pub inactive_tab: bool,
    /// The window is in native fullscreen.
    pub fullscreen: bool,
    /// The window is minimized.
    pub minimized: bool,
    /// The window is hidden or not visible.
    pub hidden: bool,
}

/// A tracked window in actor-maintained stable workspace order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowEntry<K> {
    /// The caller's window identity.
    pub id: K,
    /// Eligibility flags.
    pub flags: WindowFlags,
}

impl<K> WindowEntry<K> {
    /// An entry with default flags (an ordinary, eligible window).
    #[must_use]
    pub const fn new(id: K) -> Self {
        Self {
            id,
            flags: WindowFlags {
                explicit_floating: false,
                inactive_tab: false,
                fullscreen: false,
                minimized: false,
                hidden: false,
            },
        }
    }

    /// An entry with explicit flags.
    #[must_use]
    pub const fn with_flags(id: K, flags: WindowFlags) -> Self { Self { id, flags } }
}

/// Why a window takes no part in the layout at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exclusion {
    /// A native tab that is not active.
    InactiveTab,
    /// Native fullscreen.
    Fullscreen,
    /// Minimized.
    Minimized,
    /// Hidden or not visible.
    Hidden,
}

/// Why an eligible-or-floating window has no frame in the plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UntouchedReason {
    /// The user floated it explicitly.
    ExplicitFloating,
    /// The workspace layout is Floating, which writes nothing.
    FloatingLayout,
    /// Geometry was infeasible, so no frame was produced.
    Infeasible,
}

/// A tracked window beyond the grid cap. It keeps its observed frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridOverflow<K> {
    /// The overflowed identity.
    pub id: K,
    /// Its zero-based position among the eligible windows (at least the cap).
    pub eligible_index: usize,
}

/// Whether the plan holds usable frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanStatus {
    /// Frames are finite and have positive area.
    Ready,
    /// Geometry could not be satisfied; no frame is written.
    Infeasible(GeometryError),
}

/// A non-fatal observation about a plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutDiagnostic {
    /// The grid cap was exceeded.
    GridCapacityExceeded {
        /// Windows that received frames.
        tiled: usize,
        /// Windows reported as overflow.
        overflow: usize,
    },
}

/// Desired frames, overflow identities and diagnostics for one workspace.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutPlan<K> {
    /// Ready or infeasible.
    pub status: PlanStatus,
    /// Desired frames in layout order.
    pub frames: Vec<(K, Rect)>,
    /// Grid overflow identities in order.
    pub overflow: Vec<GridOverflow<K>>,
    /// Identities left to their current frame, with the reason.
    pub untouched: Vec<(K, UntouchedReason)>,
    /// Identities excluded from layout, with the reason.
    pub excluded: Vec<(K, Exclusion)>,
    /// Typed diagnostics.
    pub diagnostics: Vec<LayoutDiagnostic>,
}

impl<K: Copy + PartialEq> LayoutPlan<K> {
    /// Number of identities the plan accounts for.
    #[must_use]
    pub const fn accounted(&self) -> usize {
        self.frames.len() + self.overflow.len() + self.untouched.len() + self.excluded.len()
    }

    /// The desired frame of `id`, if it is tiled.
    #[must_use]
    pub fn frame_of(&self, id: &K) -> Option<Rect> {
        self.frames
            .iter()
            .find(|(candidate, _)| candidate == id)
            .map(|(_, frame)| *frame)
    }

    /// Whether `id` is reported as grid overflow.
    #[must_use]
    pub fn is_overflow(&self, id: &K) -> bool { self.overflow.iter().any(|entry| entry.id == *id) }

    /// Identities that overflowed in `previous` and are tiled in `self`.
    #[must_use]
    pub fn promoted_since(&self, previous: &Self) -> Vec<K> {
        previous
            .overflow
            .iter()
            .filter(|entry| self.frame_of(&entry.id).is_some())
            .map(|entry| entry.id)
            .collect()
    }
}
