//! Message types for the state actor.
//!
//! All communication with the state actor happens through messages:
//! - `StateMessage` - events and commands sent to the actor
//! - `StateQuery` - requests for state data (with response channel)
//! - `QueryResult` - responses from queries

use tokio::sync::oneshot;
use uuid::Uuid;

use crate::modules::tiling::state::{FocusState, LayoutType, Rect, Screen, Window, Workspace};

// ============================================================================
// State Messages
// ============================================================================

/// Messages sent to the state actor.
#[derive(Debug)]
pub enum StateMessage {
    // ════════════════════════════════════════════════════════════════════════
    // Window Events (from AXObserver)
    // ════════════════════════════════════════════════════════════════════════
    /// New window detected.
    WindowCreated(WindowCreatedInfo),

    /// Window closed.
    WindowDestroyed { window_id: u32 },

    /// Window gained focus.
    WindowFocused { window_id: u32 },

    /// Window lost focus.
    WindowUnfocused { window_id: u32 },

    /// Window position changed.
    WindowMoved { window_id: u32, frame: Rect },

    /// Window size changed.
    WindowResized { window_id: u32, frame: Rect },

    /// Window minimized/unminimized.
    WindowMinimized { window_id: u32, minimized: bool },

    /// Window title changed.
    WindowTitleChanged { window_id: u32, title: String },

    /// Window fullscreen state changed.
    WindowFullscreenChanged { window_id: u32, fullscreen: bool },

    // ════════════════════════════════════════════════════════════════════════
    // App Events (from NSWorkspace)
    // ════════════════════════════════════════════════════════════════════════
    /// Application launched.
    AppLaunched {
        pid: i32,
        bundle_id: String,
        name: String,
    },

    /// Application terminated.
    AppTerminated { pid: i32 },

    /// Application hidden (Cmd+H).
    AppHidden { pid: i32 },

    /// Application unhidden.
    AppShown { pid: i32 },

    /// Application activated (brought to front).
    AppActivated { pid: i32 },

    // ════════════════════════════════════════════════════════════════════════
    // Screen Events (from CGDisplay notifications)
    // ════════════════════════════════════════════════════════════════════════
    /// Display configuration changed (connect, disconnect, resolution).
    /// NOTE: This message requires the handler to call macOS APIs, which must
    /// run on the main thread. Use `SetScreens` for pre-detected screens.
    ScreensChanged,

    /// Set screens with pre-detected screen data.
    /// Use this instead of `ScreensChanged` when screens have been detected
    /// on the main thread already (e.g., during initialization).
    SetScreens { screens: Vec<Screen> },

    // ════════════════════════════════════════════════════════════════════════
    // User Commands (from CLI, hotkeys, frontend)
    // ════════════════════════════════════════════════════════════════════════
    /// Switch to workspace by name.
    SwitchWorkspace { name: String },

    /// Switch to next/previous workspace.
    CycleWorkspace { direction: CycleDirection },

    /// Change workspace layout.
    SetLayout {
        workspace_id: Uuid,
        layout: LayoutType,
    },

    /// Cycle through layouts.
    CycleLayout { workspace_id: Uuid },

    /// Move window to different workspace.
    MoveWindowToWorkspace { window_id: u32, workspace_id: Uuid },

    /// Swap two windows.
    SwapWindows { window_id_a: u32, window_id_b: u32 },

    /// Focus next/previous window (cycle).
    CycleFocus { direction: CycleDirection },

    /// Focus window in a direction (spatial or cycle).
    FocusWindow { direction: FocusDirection },

    /// Swap focused window with another in a direction.
    SwapWindowInDirection { direction: FocusDirection },

    /// Toggle window floating state.
    ToggleFloating { window_id: u32 },

    /// Resize split ratio.
    ResizeSplit {
        workspace_id: Uuid,
        window_index: usize,
        delta: f64,
    },

    /// Balance all split ratios.
    BalanceWorkspace { workspace_id: Uuid },

    /// Send focused window to another screen.
    SendWindowToScreen { target_screen: String },

    /// Send focused workspace to another screen.
    SendWorkspaceToScreen { target_screen: String },

    /// Resize the focused window in a dimension.
    ResizeFocusedWindow { dimension: String, amount: i32 },

    /// Apply a floating preset to the focused window.
    ApplyPreset { preset: String },

    /// Enable/disable tiling.
    SetEnabled { enabled: bool },

    // ════════════════════════════════════════════════════════════════════════
    // Queries (with response channel)
    // ════════════════════════════════════════════════════════════════════════
    /// Execute a query and send result back.
    Query {
        query: StateQuery,
        respond_to: oneshot::Sender<QueryResult>,
    },

    // ════════════════════════════════════════════════════════════════════════
    // Batched Events (from event processor)
    // ════════════════════════════════════════════════════════════════════════
    /// Batch of geometry updates (collected per frame).
    BatchedGeometryUpdates(Vec<GeometryUpdate>),

    /// Batch of window creations during initialization (no individual layout notifications).
    /// Used during startup to track all existing windows before applying layouts.
    BatchWindowsCreated(Vec<WindowCreatedInfo>),

    // ════════════════════════════════════════════════════════════════════════
    // User-Initiated Drag Operations
    // ════════════════════════════════════════════════════════════════════════
    /// User completed a resize operation (mouse released after dragging window edge).
    /// This calculates and applies new split ratios based on the resize.
    UserResizeCompleted {
        workspace_id: Uuid,
        window_id: u32,
        old_frame: Rect,
        new_frame: Rect,
    },

    /// User completed a move operation without swapping (mouse released).
    /// This triggers a layout refresh to snap windows back to their tiled positions.
    UserMoveCompleted { workspace_id: Uuid },

    // ════════════════════════════════════════════════════════════════════════
    // Internal
    // ════════════════════════════════════════════════════════════════════════
    /// Initialization complete - apply layouts for all visible workspaces.
    InitComplete,

    /// Update expected frames for windows (for minimum size detection).
    /// Called after layout is computed but before effects are applied.
    SetExpectedFrames { frames: Vec<(u32, Rect)> },

    /// Shutdown the actor gracefully.
    Shutdown,
}

impl StateMessage {
    /// Returns a human-readable name for this message type.
    ///
    /// Used for logging and debugging, especially in panic recovery.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            // Window Events
            Self::WindowCreated(_) => "WindowCreated",
            Self::WindowDestroyed { .. } => "WindowDestroyed",
            Self::WindowFocused { .. } => "WindowFocused",
            Self::WindowUnfocused { .. } => "WindowUnfocused",
            Self::WindowMoved { .. } => "WindowMoved",
            Self::WindowResized { .. } => "WindowResized",
            Self::WindowMinimized { .. } => "WindowMinimized",
            Self::WindowTitleChanged { .. } => "WindowTitleChanged",
            Self::WindowFullscreenChanged { .. } => "WindowFullscreenChanged",

            // App Events
            Self::AppLaunched { .. } => "AppLaunched",
            Self::AppTerminated { .. } => "AppTerminated",
            Self::AppHidden { .. } => "AppHidden",
            Self::AppShown { .. } => "AppShown",
            Self::AppActivated { .. } => "AppActivated",

            // Screen Events
            Self::ScreensChanged => "ScreensChanged",
            Self::SetScreens { .. } => "SetScreens",

            // User Commands
            Self::SwitchWorkspace { .. } => "SwitchWorkspace",
            Self::CycleWorkspace { .. } => "CycleWorkspace",
            Self::SetLayout { .. } => "SetLayout",
            Self::CycleLayout { .. } => "CycleLayout",
            Self::MoveWindowToWorkspace { .. } => "MoveWindowToWorkspace",
            Self::SwapWindows { .. } => "SwapWindows",
            Self::CycleFocus { .. } => "CycleFocus",
            Self::FocusWindow { .. } => "FocusWindow",
            Self::SwapWindowInDirection { .. } => "SwapWindowInDirection",
            Self::ToggleFloating { .. } => "ToggleFloating",
            Self::ResizeSplit { .. } => "ResizeSplit",
            Self::BalanceWorkspace { .. } => "BalanceWorkspace",
            Self::SendWindowToScreen { .. } => "SendWindowToScreen",
            Self::SendWorkspaceToScreen { .. } => "SendWorkspaceToScreen",
            Self::ResizeFocusedWindow { .. } => "ResizeFocusedWindow",
            Self::ApplyPreset { .. } => "ApplyPreset",
            Self::SetEnabled { .. } => "SetEnabled",

            // Queries
            Self::Query { .. } => "Query",

            // Batched Events
            Self::BatchedGeometryUpdates(_) => "BatchedGeometryUpdates",
            Self::BatchWindowsCreated(_) => "BatchWindowsCreated",

            // User Drag Operations
            Self::UserResizeCompleted { .. } => "UserResizeCompleted",
            Self::UserMoveCompleted { .. } => "UserMoveCompleted",

            // Internal
            Self::InitComplete => "InitComplete",
            Self::SetExpectedFrames { .. } => "SetExpectedFrames",
            Self::Shutdown => "Shutdown",
        }
    }
}

// ============================================================================
// Supporting Types
// ============================================================================

/// Information about a newly created window.
#[derive(Debug, Clone)]
pub struct WindowCreatedInfo {
    pub window_id: u32,
    pub pid: i32,
    pub app_id: String,
    pub app_name: String,
    pub title: String,
    pub frame: Rect,
    pub is_minimized: bool,
    pub is_fullscreen: bool,
    /// Minimum size constraints (width, height) if the window reports them.
    pub minimum_size: Option<(f64, f64)>,
    /// Tab group ID if this window is part of a native macOS tab group.
    pub tab_group_id: Option<uuid::Uuid>,
    /// Whether this is the active/visible tab in its group.
    pub is_active_tab: bool,
}

/// A geometry update for a single window.
#[derive(Debug, Clone)]
pub struct GeometryUpdate {
    pub window_id: u32,
    pub frame: Rect,
    pub update_type: GeometryUpdateType,
}

/// Type of geometry update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeometryUpdateType {
    Move,
    Resize,
    MoveResize,
}

/// Direction for cycling through workspaces or windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleDirection {
    Next,
    Previous,
}

/// Direction for spatial focus/swap operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusDirection {
    Up,
    Down,
    Left,
    Right,
    Next,
    Previous,
}

impl FocusDirection {
    /// Parses a direction string (case-insensitive).
    ///
    /// Valid values: "up", "down", "left", "right", "next", "previous", "prev"
    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "next" => Some(Self::Next),
            "previous" | "prev" => Some(Self::Previous),
            _ => None,
        }
    }

    /// Returns true if this is a spatial direction (up/down/left/right).
    #[must_use]
    pub const fn is_spatial(self) -> bool {
        matches!(self, Self::Up | Self::Down | Self::Left | Self::Right)
    }
}

// ============================================================================
// Queries
// ============================================================================

/// Queries that can be executed against the state.
#[derive(Debug, Clone)]
pub enum StateQuery {
    // Snapshots
    GetAllScreens,
    GetAllWorkspaces,
    GetAllWindows,
    GetFocusState,
    GetEnabled,

    // By ID
    GetScreen { id: u32 },
    GetWorkspace { id: Uuid },
    GetWorkspaceByName { name: String },
    GetWindow { id: u32 },

    // Relations
    GetWindowsForWorkspace { workspace_id: Uuid },
    GetWindowsForPid { pid: i32 },
    GetWorkspacesForScreen { screen_id: u32 },
    GetVisibleWorkspaces,
    GetFocusedWorkspace,
    GetFocusedWindow,
    GetLayoutableWindows { workspace_id: Uuid },

    // Tab groups
    GetTabGroup { tab_group_id: Uuid },

    // Computed (may require layout calculation)
    GetWindowLayout { workspace_id: Uuid },
}

/// Results from queries.
#[derive(Debug, Clone)]
pub enum QueryResult {
    Screens(Vec<Screen>),
    Workspaces(Vec<Workspace>),
    Windows(Vec<Window>),
    WindowIds(Vec<u32>),
    Screen(Option<Screen>),
    Workspace(Option<Workspace>),
    Window(Option<Window>),
    Focus(FocusState),
    Enabled(bool),
    Layout(Vec<(u32, Rect)>),
}

impl QueryResult {
    /// Try to get screens from the result.
    #[must_use]
    pub fn into_screens(self) -> Option<Vec<Screen>> {
        match self {
            Self::Screens(screens) => Some(screens),
            _ => None,
        }
    }

    /// Try to get workspaces from the result.
    #[must_use]
    pub fn into_workspaces(self) -> Option<Vec<Workspace>> {
        match self {
            Self::Workspaces(workspaces) => Some(workspaces),
            _ => None,
        }
    }

    /// Try to get windows from the result.
    #[must_use]
    pub fn into_windows(self) -> Option<Vec<Window>> {
        match self {
            Self::Windows(windows) => Some(windows),
            _ => None,
        }
    }

    /// Try to get a single screen from the result.
    #[must_use]
    pub fn into_screen(self) -> Option<Option<Screen>> {
        match self {
            Self::Screen(screen) => Some(screen),
            _ => None,
        }
    }

    /// Try to get a single workspace from the result.
    #[must_use]
    pub fn into_workspace(self) -> Option<Option<Workspace>> {
        match self {
            Self::Workspace(workspace) => Some(workspace),
            _ => None,
        }
    }

    /// Try to get a single window from the result.
    #[must_use]
    pub fn into_window(self) -> Option<Option<Window>> {
        match self {
            Self::Window(window) => Some(window),
            _ => None,
        }
    }

    /// Try to get focus state from the result.
    #[must_use]
    pub fn into_focus(self) -> Option<FocusState> {
        match self {
            Self::Focus(focus) => Some(focus),
            _ => None,
        }
    }

    /// Try to get enabled state from the result.
    #[must_use]
    pub fn into_enabled(self) -> Option<bool> {
        match self {
            Self::Enabled(enabled) => Some(enabled),
            _ => None,
        }
    }

    /// Try to get layout from the result.
    #[must_use]
    pub fn into_layout(self) -> Option<Vec<(u32, Rect)>> {
        match self {
            Self::Layout(layout) => Some(layout),
            _ => None,
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cycle_direction() {
        assert_ne!(CycleDirection::Next, CycleDirection::Previous);
    }

    #[test]
    fn test_geometry_update_type() {
        assert_ne!(GeometryUpdateType::Move, GeometryUpdateType::Resize);
        assert_ne!(GeometryUpdateType::Move, GeometryUpdateType::MoveResize);
    }

    #[test]
    fn test_query_result_conversions() {
        let screens_result = QueryResult::Screens(vec![]);
        assert!(screens_result.into_screens().is_some());

        let focus_result = QueryResult::Focus(FocusState::new());
        assert!(focus_result.into_focus().is_some());

        let enabled_result = QueryResult::Enabled(true);
        assert_eq!(enabled_result.into_enabled(), Some(true));
    }

    #[test]
    fn test_window_created_info() {
        let info = WindowCreatedInfo {
            window_id: 123,
            pid: 456,
            app_id: "com.test.app".to_string(),
            app_name: "Test App".to_string(),
            title: "Window Title".to_string(),
            frame: Rect::new(0.0, 0.0, 800.0, 600.0),
            is_minimized: false,
            is_fullscreen: false,
            minimum_size: Some((200.0, 150.0)),
            tab_group_id: None,
            is_active_tab: true,
        };

        assert_eq!(info.window_id, 123);
        assert_eq!(info.pid, 456);
        assert_eq!(info.minimum_size, Some((200.0, 150.0)));
    }
}
