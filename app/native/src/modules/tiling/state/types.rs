//! Core state types for the tiling window manager.
//!
//! These types form a relational structure:
//! - `Screen` represents physical displays (ID from macOS)
//! - `Workspace` represents virtual desktops (ID is UUID v7)
//! - `Window` represents tracked windows (ID from macOS)
//!
//! Relations:
//! - `Workspace.screen_id` → `Screen.id`
//! - `Window.workspace_id` → `Workspace.id`
//! - `Workspace.window_ids` → list of `Window.id`

use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use uuid::Uuid;

// ============================================================================
// Geometry Types
// ============================================================================

/// A rectangle with position and size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    /// Create a new rectangle.
    #[must_use]
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self { x, y, width, height }
    }

    /// Create a zero-sized rectangle at origin.
    #[must_use]
    pub const fn zero() -> Self { Self::new(0.0, 0.0, 0.0, 0.0) }

    /// Check if this rectangle has valid dimensions.
    #[must_use]
    pub fn is_valid(&self) -> bool { self.width > 0.0 && self.height > 0.0 }

    /// Check if this rectangle contains a point.
    #[must_use]
    pub fn contains_point(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    /// Check if this rectangle intersects with another.
    #[must_use]
    pub fn intersects(&self, other: &Self) -> bool {
        self.x < other.x + other.width
            && self.x + self.width > other.x
            && self.y < other.y + other.height
            && self.y + self.height > other.y
    }

    /// Calculate the area of this rectangle.
    #[must_use]
    pub fn area(&self) -> f64 { self.width * self.height }

    /// Get the center point of this rectangle.
    #[must_use]
    pub fn center(&self) -> (f64, f64) { (self.x + self.width / 2.0, self.y + self.height / 2.0) }

    /// Check if two rectangles are approximately equal (within epsilon).
    #[must_use]
    pub fn approx_eq(&self, other: &Self, epsilon: f64) -> bool {
        (self.x - other.x).abs() < epsilon
            && (self.y - other.y).abs() < epsilon
            && (self.width - other.width).abs() < epsilon
            && (self.height - other.height).abs() < epsilon
    }
}

// ============================================================================
// Screen Type
// ============================================================================

/// A physical display.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Screen {
    /// macOS `CGDirectDisplayID`.
    pub id: u32,

    /// Display name (e.g., "Built-in Retina Display").
    pub name: String,

    /// Full frame including menu bar area.
    pub frame: Rect,

    /// Usable frame (excluding dock, menu bar).
    pub visible_frame: Rect,

    /// `HiDPI` scale factor (e.g., 2.0 for Retina).
    pub scale_factor: f64,

    /// Is this the main display?
    pub is_main: bool,

    /// Is this the built-in display (laptop screen)?
    pub is_builtin: bool,

    /// Display refresh rate in Hz (for batch timing).
    pub refresh_rate: f64,
}

impl Default for Screen {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            frame: Rect::zero(),
            visible_frame: Rect::zero(),
            scale_factor: 1.0,
            is_main: false,
            is_builtin: false,
            refresh_rate: 60.0,
        }
    }
}

impl Screen {
    /// Get the batch interval for geometry updates based on refresh rate.
    #[must_use]
    pub fn batch_interval_ms(&self) -> f64 { 1000.0 / self.refresh_rate }
}

// ============================================================================
// Layout Type
// ============================================================================

/// Layout algorithm for arranging windows in a workspace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LayoutType {
    /// No automatic positioning, windows stay where placed.
    #[default]
    Floating,

    /// Binary space partitioning with spiral pattern.
    Dwindle,

    /// All windows maximized, stacked on top of each other.
    Monocle,

    /// One master window with remaining windows in a stack.
    Master,

    /// Even horizontal or vertical splits (auto-detected).
    Split,

    /// Even vertical splits.
    SplitVertical,

    /// Even horizontal splits.
    SplitHorizontal,

    /// Balanced grid pattern.
    Grid,
}

impl LayoutType {
    /// Returns the layout name as a static kebab-case string.
    ///
    /// This matches the `#[serde(rename_all = "kebab-case")]` format and avoids
    /// heap allocations from `format!("{self:?}").to_lowercase()`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Floating => "floating",
            Self::Dwindle => "dwindle",
            Self::Monocle => "monocle",
            Self::Master => "master",
            Self::Split => "split",
            Self::SplitVertical => "split-vertical",
            Self::SplitHorizontal => "split-horizontal",
            Self::Grid => "grid",
        }
    }

    /// Returns true if this layout stacks windows on top of each other.
    #[must_use]
    pub const fn is_stacking(&self) -> bool { matches!(self, Self::Monocle) }

    /// Returns true if this layout allows manual window positioning.
    #[must_use]
    pub const fn is_floating(&self) -> bool { matches!(self, Self::Floating) }

    /// Returns true if this layout tiles windows side by side.
    #[must_use]
    pub const fn is_tiling(&self) -> bool { !self.is_stacking() && !self.is_floating() }
}

// ============================================================================
// Workspace Type
// ============================================================================

/// Window ID list type alias. Uses `SmallVec` for inline storage of up to 8 window IDs,
/// avoiding heap allocation for the common case of workspaces with few windows.
pub type WindowIdList = SmallVec<[u32; 8]>;

/// A virtual desktop that contains windows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    /// Unique identifier (UUID v7 for time-ordering).
    pub id: Uuid,

    /// User-defined name.
    pub name: String,

    /// Screen this workspace is assigned to.
    pub screen_id: u32,

    /// Current layout algorithm.
    pub layout: LayoutType,

    /// Is this workspace currently visible?
    pub is_visible: bool,

    /// Is this the focused workspace?
    pub is_focused: bool,

    /// Window IDs in stack order (back to front).
    /// Uses `SmallVec` for inline storage of up to 8 windows.
    pub window_ids: WindowIdList,

    /// Index of focused window in `window_ids`.
    pub focused_window_index: Option<usize>,

    /// Custom split ratios for resizable layouts.
    /// Each ratio represents the proportion of space for a split.
    pub split_ratios: Vec<f64>,

    /// Runtime-overridden master ratio for the Master layout.
    ///
    /// `None` means "use the global config default (`tiling.master.ratio`)".
    /// Set to `Some(ratio)` when the user resizes windows in Master layout,
    /// and cleared back to `None` on balance or layout change.
    pub master_ratio: Option<f64>,

    /// Configured screen name (for reconnection after screen hotplug).
    pub configured_screen: Option<String>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            id: Uuid::now_v7(),
            name: String::new(),
            screen_id: 0,
            layout: LayoutType::default(),
            is_visible: false,
            is_focused: false,
            window_ids: WindowIdList::new(),
            focused_window_index: None,
            split_ratios: Vec::new(),
            master_ratio: None,
            configured_screen: None,
        }
    }
}

impl Workspace {
    /// Create a new workspace with the given name.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    /// Get the ID of the focused window, if any.
    #[must_use]
    pub fn focused_window_id(&self) -> Option<u32> {
        self.focused_window_index.and_then(|idx| self.window_ids.get(idx).copied())
    }

    /// Get the number of windows in this workspace.
    #[must_use]
    pub fn window_count(&self) -> usize { self.window_ids.len() }

    /// Check if this workspace has no windows.
    #[must_use]
    pub fn is_empty(&self) -> bool { self.window_ids.is_empty() }

    /// Check if a window is in this workspace.
    #[must_use]
    pub fn contains_window(&self, window_id: u32) -> bool { self.window_ids.contains(&window_id) }

    /// Get the index of a window in this workspace.
    #[must_use]
    pub fn window_index(&self, window_id: u32) -> Option<usize> {
        self.window_ids.iter().position(|&id| id == window_id)
    }
}

// ============================================================================
// Window Type
// ============================================================================

/// A tracked window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // Window state naturally has many boolean flags
pub struct Window {
    /// macOS `CGWindowID`.
    pub id: u32,

    /// Process ID of the owning application.
    pub pid: i32,

    /// Bundle identifier (e.g., "com.apple.Safari").
    pub app_id: String,

    /// Application name.
    pub app_name: String,

    /// Window title.
    pub title: String,

    /// Current frame (position and size).
    pub frame: Rect,

    /// Minimum size constraints (width, height) reported by the window via `AXMinimumSize`.
    /// `None` if the window doesn't report minimum size or it hasn't been queried.
    pub minimum_size: Option<(f64, f64)>,

    /// Inferred minimum size based on position mismatch detection.
    /// When a window is positioned but ends up at a different location/size than expected,
    /// we infer that the window has hit its minimum size constraint.
    /// This is a fallback for windows that don't report `AXMinimumSize`.
    pub inferred_minimum_size: Option<(f64, f64)>,

    /// Expected frame from the last layout calculation.
    /// Used to detect position mismatch and infer minimum size constraints.
    pub expected_frame: Option<Rect>,

    /// Assigned workspace ID.
    pub workspace_id: Uuid,

    /// Is the window minimized?
    pub is_minimized: bool,

    /// Is the window in native fullscreen?
    pub is_fullscreen: bool,

    /// Is the window hidden (app hidden)?
    pub is_hidden: bool,

    /// Is the window floating (excluded from tiling)?
    pub is_floating: bool,

    /// Tab group ID if this window is part of a tab group.
    pub tab_group_id: Option<Uuid>,

    /// Is this the active tab in its tab group?
    pub is_active_tab: bool,

    /// Name of the rule that matched this window (for debugging).
    pub matched_rule: Option<String>,
}

impl Default for Window {
    fn default() -> Self {
        Self {
            id: 0,
            pid: 0,
            app_id: String::new(),
            app_name: String::new(),
            title: String::new(),
            frame: Rect::zero(),
            minimum_size: None,
            inferred_minimum_size: None,
            expected_frame: None,
            workspace_id: Uuid::nil(),
            is_minimized: false,
            is_fullscreen: false,
            is_hidden: false,
            is_floating: false,
            tab_group_id: None,
            is_active_tab: true,
            matched_rule: None,
        }
    }
}

impl Window {
    /// Check if this window should be included in layout calculations.
    #[must_use]
    pub const fn is_layoutable(&self) -> bool {
        !self.is_minimized
            && !self.is_hidden
            && !self.is_fullscreen
            && !self.is_floating
            && (self.tab_group_id.is_none() || self.is_active_tab)
    }

    /// Check if this window is in a tab group.
    #[must_use]
    pub const fn is_tabbed(&self) -> bool { self.tab_group_id.is_some() }

    /// Returns the effective minimum size, preferring reported over inferred.
    ///
    /// Uses `minimum_size` (from `AXMinimumSize`) if available, otherwise falls back
    /// to `inferred_minimum_size` (detected from position mismatch).
    #[must_use]
    pub const fn effective_minimum_size(&self) -> Option<(f64, f64)> {
        // Prefer reported minimum_size if available
        if let Some(reported) = self.minimum_size {
            return Some(reported);
        }
        // Fall back to inferred minimum size
        self.inferred_minimum_size
    }

    /// Check if the given target rect would violate this window's minimum size constraints.
    ///
    /// Returns `true` if the window has minimum size constraints and the target
    /// would make the window smaller than allowed. Considers both reported and
    /// inferred minimum sizes.
    #[must_use]
    pub fn would_violate_minimum_size(&self, target: &Rect) -> bool {
        if let Some((min_w, min_h)) = self.effective_minimum_size() {
            target.width < min_w - 1.0 || target.height < min_h - 1.0
        } else {
            false
        }
    }

    /// Returns the effective minimum width, or 0 if not constrained.
    #[must_use]
    pub fn min_width(&self) -> f64 { self.effective_minimum_size().map_or(0.0, |(w, _)| w) }

    /// Returns the effective minimum height, or 0 if not constrained.
    #[must_use]
    pub fn min_height(&self) -> f64 { self.effective_minimum_size().map_or(0.0, |(_, h)| h) }
}

// ============================================================================
// Focus State
// ============================================================================

/// Global focus tracking state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusState {
    /// Currently focused window ID.
    pub focused_window_id: Option<u32>,

    /// Currently focused workspace ID.
    pub focused_workspace_id: Option<Uuid>,

    /// Currently focused screen ID.
    pub focused_screen_id: Option<u32>,
}

impl FocusState {
    /// Create a new empty focus state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            focused_window_id: None,
            focused_workspace_id: None,
            focused_screen_id: None,
        }
    }

    /// Check if anything is focused.
    #[must_use]
    pub const fn has_focus(&self) -> bool { self.focused_window_id.is_some() }

    /// Clear all focus state.
    pub const fn clear(&mut self) {
        self.focused_window_id = None;
        self.focused_workspace_id = None;
        self.focused_screen_id = None;
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    mod rect_tests {
        use super::*;

        #[test]
        fn test_rect_new() {
            let rect = Rect::new(10.0, 20.0, 100.0, 200.0);
            assert_eq!(rect.x, 10.0);
            assert_eq!(rect.y, 20.0);
            assert_eq!(rect.width, 100.0);
            assert_eq!(rect.height, 200.0);
        }

        #[test]
        fn test_rect_zero() {
            let rect = Rect::zero();
            assert_eq!(rect.x, 0.0);
            assert_eq!(rect.y, 0.0);
            assert_eq!(rect.width, 0.0);
            assert_eq!(rect.height, 0.0);
        }

        #[test]
        fn test_rect_is_valid() {
            assert!(Rect::new(0.0, 0.0, 100.0, 100.0).is_valid());
            assert!(!Rect::new(0.0, 0.0, 0.0, 100.0).is_valid());
            assert!(!Rect::new(0.0, 0.0, 100.0, 0.0).is_valid());
            assert!(!Rect::new(0.0, 0.0, -1.0, 100.0).is_valid());
        }

        #[test]
        fn test_rect_contains_point() {
            let rect = Rect::new(10.0, 10.0, 100.0, 100.0);
            assert!(rect.contains_point(50.0, 50.0));
            assert!(rect.contains_point(10.0, 10.0));
            assert!(!rect.contains_point(110.0, 110.0));
            assert!(!rect.contains_point(5.0, 50.0));
        }

        #[test]
        fn test_rect_intersects() {
            let a = Rect::new(0.0, 0.0, 100.0, 100.0);
            let b = Rect::new(50.0, 50.0, 100.0, 100.0);
            let c = Rect::new(200.0, 200.0, 100.0, 100.0);

            assert!(a.intersects(&b));
            assert!(b.intersects(&a));
            assert!(!a.intersects(&c));
            assert!(!c.intersects(&a));
        }

        #[test]
        fn test_rect_area() {
            let rect = Rect::new(0.0, 0.0, 100.0, 50.0);
            assert_eq!(rect.area(), 5000.0);
        }

        #[test]
        fn test_rect_center() {
            let rect = Rect::new(0.0, 0.0, 100.0, 100.0);
            assert_eq!(rect.center(), (50.0, 50.0));

            let rect2 = Rect::new(10.0, 20.0, 100.0, 100.0);
            assert_eq!(rect2.center(), (60.0, 70.0));
        }

        #[test]
        fn test_rect_approx_eq() {
            let a = Rect::new(10.0, 20.0, 100.0, 100.0);
            let b = Rect::new(10.001, 20.001, 100.001, 100.001);
            let c = Rect::new(10.1, 20.0, 100.0, 100.0);

            assert!(a.approx_eq(&b, 0.01));
            assert!(!a.approx_eq(&c, 0.01));
        }
    }

    mod screen_tests {
        use super::*;

        #[test]
        fn test_screen_default() {
            let screen = Screen::default();
            assert_eq!(screen.id, 0);
            assert_eq!(screen.refresh_rate, 60.0);
            assert!(!screen.is_main);
        }

        #[test]
        fn test_screen_batch_interval() {
            let screen = Screen {
                refresh_rate: 60.0,
                ..Screen::default()
            };
            assert!((screen.batch_interval_ms() - 16.666).abs() < 0.01);

            let screen = Screen {
                refresh_rate: 120.0,
                ..Screen::default()
            };
            assert!((screen.batch_interval_ms() - 8.333).abs() < 0.01);

            let screen = Screen {
                refresh_rate: 240.0,
                ..Screen::default()
            };
            assert!((screen.batch_interval_ms() - 4.166).abs() < 0.01);
        }
    }

    mod layout_type_tests {
        use super::*;

        #[test]
        fn test_layout_type_properties() {
            assert!(LayoutType::Floating.is_floating());
            assert!(!LayoutType::Floating.is_tiling());
            assert!(!LayoutType::Floating.is_stacking());

            assert!(LayoutType::Monocle.is_stacking());
            assert!(!LayoutType::Monocle.is_tiling());
            assert!(!LayoutType::Monocle.is_floating());

            assert!(LayoutType::Dwindle.is_tiling());
            assert!(!LayoutType::Dwindle.is_stacking());
            assert!(!LayoutType::Dwindle.is_floating());
        }
    }

    mod workspace_tests {
        use super::*;

        #[test]
        fn test_workspace_new() {
            let ws = Workspace::new("dev");
            assert_eq!(ws.name, "dev");
            assert!(!ws.id.is_nil());
            assert!(ws.is_empty());
        }

        #[test]
        fn test_workspace_window_management() {
            use smallvec::smallvec;

            let mut ws = Workspace::new("test");
            ws.window_ids = smallvec![100, 200, 300];
            ws.focused_window_index = Some(1);

            assert_eq!(ws.window_count(), 3);
            assert!(!ws.is_empty());
            assert!(ws.contains_window(200));
            assert!(!ws.contains_window(999));
            assert_eq!(ws.window_index(200), Some(1));
            assert_eq!(ws.window_index(999), None);
            assert_eq!(ws.focused_window_id(), Some(200));
        }
    }

    mod window_tests {
        use super::*;

        #[test]
        fn test_window_default() {
            let window = Window::default();
            assert_eq!(window.id, 0);
            assert!(window.workspace_id.is_nil());
            assert!(window.is_active_tab);
        }

        #[test]
        fn test_window_is_layoutable() {
            let window = Window { id: 1, ..Window::default() };
            assert!(window.is_layoutable());

            let window = Window {
                id: 1,
                is_minimized: true,
                ..Window::default()
            };
            assert!(!window.is_layoutable());

            let window = Window {
                id: 1,
                is_hidden: true,
                ..Window::default()
            };
            assert!(!window.is_layoutable());

            let window = Window {
                id: 1,
                is_fullscreen: true,
                ..Window::default()
            };
            assert!(!window.is_layoutable());

            let window = Window {
                id: 1,
                is_floating: true,
                ..Window::default()
            };
            assert!(!window.is_layoutable());

            // Tab that's not active
            let window = Window {
                id: 1,
                tab_group_id: Some(Uuid::now_v7()),
                is_active_tab: false,
                ..Window::default()
            };
            assert!(!window.is_layoutable());

            // Tab that is active
            let window = Window {
                id: 1,
                tab_group_id: Some(Uuid::now_v7()),
                is_active_tab: true,
                ..Window::default()
            };
            assert!(window.is_layoutable());
        }

        #[test]
        fn test_window_is_tabbed() {
            let mut window = Window::default();
            assert!(!window.is_tabbed());

            window.tab_group_id = Some(Uuid::now_v7());
            assert!(window.is_tabbed());
        }

        #[test]
        fn test_window_minimum_size_helpers() {
            let mut window = Window::default();

            // No minimum size
            assert_eq!(window.min_width(), 0.0);
            assert_eq!(window.min_height(), 0.0);
            assert!(!window.would_violate_minimum_size(&Rect::new(0.0, 0.0, 100.0, 100.0)));

            // Set minimum size
            window.minimum_size = Some((200.0, 150.0));
            assert_eq!(window.min_width(), 200.0);
            assert_eq!(window.min_height(), 150.0);

            // Target is larger than minimum - no violation
            assert!(
                !window.would_violate_minimum_size(&Rect::new(0.0, 0.0, 300.0, 200.0)),
                "Larger than minimum should not violate"
            );

            // Target equals minimum - no violation
            assert!(
                !window.would_violate_minimum_size(&Rect::new(0.0, 0.0, 200.0, 150.0)),
                "Equal to minimum should not violate"
            );

            // Target is smaller in width - violation
            assert!(
                window.would_violate_minimum_size(&Rect::new(0.0, 0.0, 100.0, 200.0)),
                "Smaller width should violate"
            );

            // Target is smaller in height - violation
            assert!(
                window.would_violate_minimum_size(&Rect::new(0.0, 0.0, 300.0, 100.0)),
                "Smaller height should violate"
            );

            // Target is smaller in both - violation
            assert!(
                window.would_violate_minimum_size(&Rect::new(0.0, 0.0, 100.0, 100.0)),
                "Smaller in both dimensions should violate"
            );
        }

        #[test]
        fn test_window_default_has_no_minimum_size() {
            let window = Window::default();
            assert!(window.minimum_size.is_none());
        }
    }

    mod focus_state_tests {
        use super::*;

        #[test]
        fn test_focus_state_new() {
            let focus = FocusState::new();
            assert!(!focus.has_focus());
            assert!(focus.focused_window_id.is_none());
        }

        #[test]
        fn test_focus_state_clear() {
            let mut focus = FocusState {
                focused_window_id: Some(123),
                focused_workspace_id: Some(Uuid::now_v7()),
                focused_screen_id: Some(1),
            };

            assert!(focus.has_focus());
            focus.clear();
            assert!(!focus.has_focus());
        }
    }
}
