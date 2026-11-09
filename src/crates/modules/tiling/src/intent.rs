//! Typed workspace and window intents with their exact token grammar.
//!
//! An [`Intent`] is a pure actor input. Part 07 will parse CLI/IPC arguments
//! into these values with the parsers below; nothing here reads a socket. The
//! grammar is deliberately asymmetric (decision 13): focus directions are
//! case-insensitive and accept `prev`; the six-value swap grammar is
//! case-sensitive and has no `prev` alias.

use stache_macos::windows::WindowInstance;

use crate::config::LayoutType;
use crate::geometry::GeometryError;
use crate::presets::PresetNoOp;

/// A direction or cycle step for focusing a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusDirection {
    /// Spatially up.
    Up,
    /// Spatially down.
    Down,
    /// Spatially left.
    Left,
    /// Spatially right.
    Right,
    /// The next window in workspace order (wraps).
    Next,
    /// The previous window in workspace order (wraps).
    Previous,
}

impl FocusDirection {
    /// Parses a focus direction. Case-insensitive; `prev` and `previous` both
    /// name [`Self::Previous`].
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "next" => Some(Self::Next),
            "prev" | "previous" => Some(Self::Previous),
            _ => None,
        }
    }

    /// Whether the direction is spatial (not a cycle step).
    #[must_use]
    pub const fn is_spatial(self) -> bool {
        matches!(self, Self::Up | Self::Down | Self::Left | Self::Right)
    }
}

/// The six-value swap grammar. Case-sensitive; there is no `prev` alias.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapDirection {
    /// Swap with the window above.
    Up,
    /// Swap with the window below.
    Down,
    /// Swap with the window to the left.
    Left,
    /// Swap with the window to the right.
    Right,
    /// Swap with the next window in order.
    Next,
    /// Swap with the previous window in order.
    Previous,
}

impl SwapDirection {
    /// Parses exactly `up`, `down`, `left`, `right`, `next` or `previous`.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "next" => Some(Self::Next),
            "previous" => Some(Self::Previous),
            _ => None,
        }
    }

    /// The matching focus direction (same geometry and cycle rules).
    #[must_use]
    pub const fn as_focus(self) -> FocusDirection {
        match self {
            Self::Up => FocusDirection::Up,
            Self::Down => FocusDirection::Down,
            Self::Left => FocusDirection::Left,
            Self::Right => FocusDirection::Right,
            Self::Next => FocusDirection::Next,
            Self::Previous => FocusDirection::Previous,
        }
    }
}

/// A cycle step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cycle {
    /// Forward, wrapping.
    Next,
    /// Backward, wrapping.
    Previous,
}

/// A screen named by an intent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScreenTarget {
    /// The main display.
    Main,
    /// Any display that is not the main one (the first such display).
    Secondary,
    /// A display name, compared case-insensitively.
    Named(String),
}

impl ScreenTarget {
    /// Parses `main`/`primary`, `secondary` or a display name.
    #[must_use]
    pub fn parse(token: &str) -> Self {
        match token.to_ascii_lowercase().as_str() {
            "main" | "primary" => Self::Main,
            "secondary" => Self::Secondary,
            _ => Self::Named(token.to_owned()),
        }
    }
}

/// The dimension a resize changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResizeDimension {
    /// Horizontal size.
    Width,
    /// Vertical size.
    Height,
}

/// What a focus-window intent names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FocusTarget {
    /// A direction or cycle step from the focused window.
    Direction(FocusDirection),
    /// An exact, already resolved window token.
    Window(WindowInstance),
}

/// A user command for the workspace model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    /// Make the workspace with this exact name visible and focused.
    FocusWorkspace(String),
    /// Cycle the workspaces of the focused display.
    CycleWorkspace(Cycle),
    /// Focus a window.
    FocusWindow(FocusTarget),
    /// Swap the focused window with another one.
    SwapWindow(SwapDirection),
    /// Toggle the focused window's explicit floating state.
    ToggleFloating,
    /// Apply a floating preset to the focused window (Floating workspaces only).
    Preset(String),
    /// Resize the focused window by `amount` logical points (signed).
    Resize {
        /// The dimension.
        dimension: ResizeDimension,
        /// Points to add; negative shrinks.
        amount: i32,
    },
    /// Reset ratios and inferred minimums of the focused workspace.
    Balance,
    /// Set the focused workspace layout.
    SetLayout(LayoutType),
    /// Cycle the focused workspace layout.
    CycleLayout,
    /// Move a window (default: the focused one) to a workspace by name.
    SendWindowToWorkspace {
        /// The exact window; `None` is the focused window.
        window: Option<WindowInstance>,
        /// The destination workspace name.
        workspace: String,
    },
    /// Move the focused window to the visible workspace of another display.
    SendWindowToScreen(ScreenTarget),
    /// Move the focused workspace to another display.
    SendWorkspaceToScreen(ScreenTarget),
}

/// Why an admitted intent changed nothing. These are internal reasons: the user
/// sees a silent success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntentNoOp {
    /// An explicit preset request that does nothing (see [`PresetNoOp`]).
    Preset(PresetNoOp),
    /// The requested state already holds.
    Unchanged,
    /// No window lies in the requested direction.
    NoCandidate,
    /// There is no focused window.
    NoFocusedWindow,
    /// There is no focused workspace.
    NoFocusedWorkspace,
    /// Fewer windows than the command needs.
    TooFewWindows,
    /// The layout has no ratio for this command.
    LayoutIgnoresCommand,
}

/// Why an intent was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntentRejection {
    /// No workspace has that name.
    UnknownWorkspace,
    /// The token names no tracked window.
    UnknownWindow,
    /// No display matches.
    UnknownScreen,
    /// The display has no visible workspace.
    NoVisibleWorkspace,
    /// The usable area or frame could not be computed.
    Geometry(GeometryError),
    /// The display produced no usable area.
    NoDisplayArea,
}

/// The result of one admitted intent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntentOutcome {
    /// Desired state changed. Native effects, if any, are reported separately.
    Applied,
    /// Admitted; nothing to do.
    NoOp(IntentNoOp),
    /// Refused.
    Rejected(IntentRejection),
}

/// The latest intent and what became of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntentRecord {
    /// The transition sequence that processed it.
    pub sequence: u64,
    /// The intent.
    pub intent: Intent,
    /// The outcome.
    pub outcome: IntentOutcome,
}
