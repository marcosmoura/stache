//! State types for the tiling window manager.
//!
//! This module defines the core data structures used to track screens,
//! workspaces, and windows in the tiling system.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::config::LayoutType;

// ============================================================================
// Type Aliases
// ============================================================================

/// Inline capacity for split ratios.
///
/// Most workspaces have fewer than 8 windows with custom split ratios.
pub const SPLIT_RATIOS_INLINE_CAP: usize = 8;

/// Type alias for split ratios storage.
///
/// Uses `SmallVec` to avoid heap allocations for workspaces with
/// up to 8 custom split ratios.
pub type SplitRatios = SmallVec<[f64; SPLIT_RATIOS_INLINE_CAP]>;

// ============================================================================
// Geometric Types
// ============================================================================

/// A point in 2D space.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    /// X coordinate.
    pub x: f64,
    /// Y coordinate.
    pub y: f64,
}

impl Point {
    /// Creates a new point.
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self { Self { x, y } }
}

/// A rectangle defined by origin point and size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    /// X coordinate of the origin (top-left corner).
    pub x: f64,
    /// Y coordinate of the origin (top-left corner).
    pub y: f64,
    /// Width of the rectangle.
    pub width: f64,
    /// Height of the rectangle.
    pub height: f64,
}

impl Rect {
    /// Creates a new rectangle.
    #[must_use]
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self { x, y, width, height }
    }

    /// Creates a rectangle from origin point and size.
    #[must_use]
    pub const fn from_origin_size(origin: Point, width: f64, height: f64) -> Self {
        Self {
            x: origin.x,
            y: origin.y,
            width,
            height,
        }
    }

    /// Returns the origin point of the rectangle.
    #[must_use]
    pub const fn origin(&self) -> Point { Point { x: self.x, y: self.y } }

    /// Returns the center point of the rectangle.
    #[must_use]
    pub fn center(&self) -> Point {
        Point {
            x: self.x + self.width / 2.0,
            y: self.y + self.height / 2.0,
        }
    }

    /// Returns whether a point is inside the rectangle.
    #[must_use]
    pub fn contains(&self, point: Point) -> bool {
        point.x >= self.x
            && point.x <= self.x + self.width
            && point.y >= self.y
            && point.y <= self.y + self.height
    }

    /// Returns the area of the rectangle.
    #[must_use]
    pub fn area(&self) -> f64 { self.width * self.height }
}

// ============================================================================
// Screen
// ============================================================================

/// Represents a physical display/monitor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Screen {
    /// Unique identifier for the screen (display ID from macOS).
    pub id: u32,
    /// Human-readable name of the screen.
    pub name: String,
    /// The full frame of the screen in global coordinates.
    pub frame: Rect,
    /// The visible/usable frame (excluding menu bar and dock).
    pub visible_frame: Rect,
    /// Whether this is the main screen (has the menu bar).
    pub is_main: bool,
    /// Whether this is the built-in display (laptop screen).
    pub is_builtin: bool,
    /// Scale factor for Retina displays (1.0, 2.0, etc.).
    pub scale_factor: f64,
}

impl Screen {
    /// Creates a new screen.
    #[must_use]
    pub const fn new(
        id: u32,
        name: String,
        frame: Rect,
        visible_frame: Rect,
        is_main: bool,
        is_builtin: bool,
        scale_factor: f64,
    ) -> Self {
        Self {
            id,
            name,
            frame,
            visible_frame,
            is_main,
            is_builtin,
            scale_factor,
        }
    }
}

// ============================================================================
// Layout Cache
// ============================================================================

/// Cached layout calculation result.
///
/// Stores the result of a layout calculation along with a hash of the inputs.
/// This allows skipping expensive recalculations when the inputs haven't changed.
#[derive(Debug, Clone, Default)]
pub struct LayoutCache {
    /// Hash of the layout inputs (`window_ids`, `screen_frame`, layout type, ratios, etc.)
    pub input_hash: u64,
    /// Cached layout positions: (`window_id`, frame)
    pub positions: super::layout::LayoutResult,
}

impl LayoutCache {
    /// Creates a new empty layout cache.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            input_hash: 0,
            positions: smallvec::SmallVec::new_const(),
        }
    }

    /// Checks if the cache is valid for the given input hash.
    #[must_use]
    pub fn is_valid(&self, input_hash: u64) -> bool {
        self.input_hash != 0 && self.input_hash == input_hash && !self.positions.is_empty()
    }

    /// Updates the cache with new results.
    pub fn update(&mut self, input_hash: u64, positions: super::layout::LayoutResult) {
        self.input_hash = input_hash;
        self.positions = positions;
    }

    /// Invalidates the cache.
    pub fn invalidate(&mut self) {
        self.input_hash = 0;
        self.positions.clear();
    }
}

/// Computes a hash of layout inputs for cache validation.
///
/// The hash includes all inputs that affect the layout calculation:
/// - Layout type
/// - Window IDs (and their order)
/// - Screen frame dimensions
/// - Master ratio
/// - Split ratios
/// - Gap values
#[must_use]
pub fn compute_layout_hash(
    layout: LayoutType,
    window_ids: &[u32],
    screen_frame: &Rect,
    master_ratio: f64,
    split_ratios: &[f64],
    gaps_hash: u64,
) -> u64 {
    use std::collections::hash_map::DefaultHasher;

    let mut hasher = DefaultHasher::new();

    // Hash layout type
    std::mem::discriminant(&layout).hash(&mut hasher);

    // Hash window IDs (order matters)
    window_ids.len().hash(&mut hasher);
    for id in window_ids {
        id.hash(&mut hasher);
    }

    // Hash screen frame (convert to bits to avoid float comparison issues)
    screen_frame.x.to_bits().hash(&mut hasher);
    screen_frame.y.to_bits().hash(&mut hasher);
    screen_frame.width.to_bits().hash(&mut hasher);
    screen_frame.height.to_bits().hash(&mut hasher);

    // Hash master ratio
    master_ratio.to_bits().hash(&mut hasher);

    // Hash split ratios
    split_ratios.len().hash(&mut hasher);
    for ratio in split_ratios {
        ratio.to_bits().hash(&mut hasher);
    }

    // Include gaps hash
    gaps_hash.hash(&mut hasher);

    hasher.finish()
}

// ============================================================================
// Workspace
// ============================================================================

/// Represents a virtual workspace containing windows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    /// Unique name of the workspace.
    pub name: String,
    /// The screen this workspace is currently assigned to (by screen ID).
    /// This may differ from `configured_screen` when the configured screen is disconnected.
    pub screen_id: u32,
    /// The screen name from configuration (e.g., "main", "secondary", "LG HDR 4K").
    /// Used to restore workspace to correct screen when it reconnects.
    #[serde(default)]
    pub configured_screen: String,
    /// Original screen ID before the workspace was moved to another screen.
    /// Used to restore workspace to its original screen when moved back.
    /// `None` if the workspace hasn't been moved from its configured screen.
    #[serde(default)]
    pub original_screen_id: Option<u32>,
    /// Current layout type for this workspace.
    pub layout: LayoutType,
    /// Whether this workspace is currently visible on its screen.
    pub is_visible: bool,
    /// Whether this workspace is currently focused.
    pub is_focused: bool,
    /// IDs of windows in this workspace (in stack order).
    pub window_ids: Vec<u32>,
    /// Index of the focused window in this workspace (if any).
    pub focused_window_index: Option<usize>,
    /// Custom split ratios for this workspace.
    ///
    /// For split layouts with N windows, this contains N-1 ratios representing
    /// the split points. Each ratio is between 0.0 and 1.0, representing the
    /// cumulative position of the split.
    ///
    /// For example, with 3 windows and ratios [0.33, 0.66]:
    /// - Window 1 gets 0% to 33%
    /// - Window 2 gets 33% to 66%
    /// - Window 3 gets 66% to 100%
    ///
    /// If empty or wrong length, equal splits are used.
    ///
    /// Uses `SmallVec` to avoid heap allocations for the common case of
    /// workspaces with 8 or fewer custom split ratios.
    #[serde(default)]
    pub split_ratios: SplitRatios,

    /// Cached layout calculation result.
    ///
    /// This caches the result of the last layout calculation to avoid
    /// recomputing when inputs haven't changed.
    #[serde(skip)]
    pub layout_cache: LayoutCache,
}

impl Workspace {
    /// Creates a new workspace.
    #[must_use]
    pub const fn new(name: String, screen_id: u32, layout: LayoutType) -> Self {
        Self {
            name,
            screen_id,
            configured_screen: String::new(),
            original_screen_id: None,
            layout,
            is_visible: false,
            is_focused: false,
            window_ids: Vec::new(),
            focused_window_index: None,
            split_ratios: SmallVec::new_const(),
            layout_cache: LayoutCache::new(),
        }
    }

    /// Creates a new workspace with a configured screen name.
    ///
    /// The `configured_screen` is the screen name from configuration (e.g., "main", "secondary").
    /// This is used to restore the workspace to its intended screen when it reconnects.
    #[must_use]
    pub const fn new_with_screen(
        name: String,
        screen_id: u32,
        configured_screen: String,
        layout: LayoutType,
    ) -> Self {
        Self {
            name,
            screen_id,
            configured_screen,
            original_screen_id: None,
            layout,
            is_visible: false,
            is_focused: false,
            window_ids: Vec::new(),
            focused_window_index: None,
            split_ratios: SmallVec::new_const(),
            layout_cache: LayoutCache::new(),
        }
    }

    /// Returns the number of windows in this workspace.
    #[must_use]
    pub const fn window_count(&self) -> usize { self.window_ids.len() }

    /// Returns whether this workspace has any windows.
    #[must_use]
    pub const fn is_empty(&self) -> bool { self.window_ids.is_empty() }

    /// Returns the ID of the focused window, if any.
    #[must_use]
    pub fn focused_window_id(&self) -> Option<u32> {
        self.focused_window_index.and_then(|idx| self.window_ids.get(idx).copied())
    }
}

// ============================================================================
// Tracked Window
// ============================================================================

/// Represents a window being managed by the tiling system.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackedWindow {
    /// Unique window ID (from macOS).
    pub id: u32,
    /// Process ID of the owning application.
    pub pid: i32,
    /// Bundle identifier of the owning application (e.g., "com.apple.Safari").
    pub app_id: String,
    /// Name of the owning application (e.g., "Safari").
    pub app_name: String,
    /// Window title.
    pub title: String,
    /// Current frame of the window.
    pub frame: Rect,
    /// Whether the window is currently minimized.
    pub is_minimized: bool,
    /// Whether the window is currently hidden (by workspace switching).
    pub is_hidden: bool,
    /// Whether this is a floating window (not tiled).
    pub is_floating: bool,
    /// The workspace this window belongs to.
    pub workspace_name: String,
}

impl TrackedWindow {
    /// Creates a new tracked window.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        id: u32,
        pid: i32,
        app_id: String,
        app_name: String,
        title: String,
        frame: Rect,
        workspace_name: String,
        is_minimized: bool,
    ) -> Self {
        Self {
            id,
            pid,
            app_id,
            app_name,
            title,
            frame,
            is_minimized,
            is_hidden: false,
            is_floating: false,
            workspace_name,
        }
    }
}

// ============================================================================
// Tiling State
// ============================================================================

/// The complete state of the tiling window manager.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TilingState {
    /// All detected screens.
    pub screens: Vec<Screen>,
    /// All workspaces.
    pub workspaces: Vec<Workspace>,
    /// All tracked windows.
    pub windows: Vec<TrackedWindow>,
    /// Name of the currently focused workspace.
    pub focused_workspace: Option<String>,
    /// ID of the currently focused screen.
    pub focused_screen_id: Option<u32>,

    /// Index for O(1) workspace lookup by name.
    ///
    /// Maps lowercase workspace names to their index in `workspaces`.
    /// This is maintained automatically by `add_workspace()` and `rebuild_workspace_index()`.
    #[serde(skip)]
    workspace_index: HashMap<String, usize>,
}

impl TilingState {
    /// Creates a new empty tiling state.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    // ========================================================================
    // Screen Methods
    // ========================================================================

    /// Finds a screen by ID.
    #[must_use]
    pub fn screen_by_id(&self, id: u32) -> Option<&Screen> {
        self.screens.iter().find(|s| s.id == id)
    }

    /// Finds a screen by name.
    ///
    /// Supports special names:
    /// - `"main"` or `"primary"` - the main screen (with menu bar)
    /// - `"builtin"` - the built-in display (laptop screen)
    /// - `"secondary"` - the non-main screen (only when exactly 2 screens)
    #[must_use]
    pub fn screen_by_name(&self, name: &str) -> Option<&Screen> {
        // Handle special names
        match name.to_lowercase().as_str() {
            "main" | "primary" => self.screens.iter().find(|s| s.is_main),
            "builtin" => self.screens.iter().find(|s| s.is_builtin),
            "secondary" => {
                // Return the non-main screen when there are exactly 2 screens
                if self.screens.len() == 2 {
                    self.screens.iter().find(|s| !s.is_main)
                } else {
                    None
                }
            }
            _ => self.screens.iter().find(|s| s.name.eq_ignore_ascii_case(name)),
        }
    }

    /// Finds the main screen.
    #[must_use]
    pub fn main_screen(&self) -> Option<&Screen> { self.screens.iter().find(|s| s.is_main) }

    // ========================================================================
    // Workspace Methods
    // ========================================================================

    /// Adds a workspace and updates the index.
    ///
    /// This is the preferred way to add workspaces as it maintains the index.
    pub fn add_workspace(&mut self, workspace: Workspace) {
        let key = workspace.name.to_ascii_lowercase();
        let index = self.workspaces.len();
        self.workspaces.push(workspace);
        self.workspace_index.insert(key, index);
    }

    /// Rebuilds the workspace index from scratch.
    ///
    /// Call this after bulk modifications to `workspaces` or after deserialization.
    pub fn rebuild_workspace_index(&mut self) {
        self.workspace_index.clear();
        for (index, workspace) in self.workspaces.iter().enumerate() {
            let key = workspace.name.to_ascii_lowercase();
            self.workspace_index.insert(key, index);
        }
    }

    /// Finds a workspace by name using O(1) index lookup.
    ///
    /// Falls back to linear search if the index is stale.
    #[must_use]
    pub fn workspace_by_name(&self, name: &str) -> Option<&Workspace> {
        let key = name.to_ascii_lowercase();

        // Try index lookup first (O(1))
        if let Some(&index) = self.workspace_index.get(&key)
            && let Some(workspace) = self.workspaces.get(index)
            && workspace.name.eq_ignore_ascii_case(name)
        {
            // Index hit and name verified
            return Some(workspace);
        }

        // Fallback to linear search if index miss or stale
        self.workspaces.iter().find(|w| w.name.eq_ignore_ascii_case(name))
    }

    /// Finds a workspace by name (mutable) using O(1) index lookup.
    ///
    /// Falls back to linear search if the index is stale.
    pub fn workspace_by_name_mut(&mut self, name: &str) -> Option<&mut Workspace> {
        let key = name.to_ascii_lowercase();

        // Try index lookup first (O(1))
        if let Some(&index) = self.workspace_index.get(&key) {
            // Check if the index is valid and name matches
            if self.workspaces.get(index).is_some_and(|w| w.name.eq_ignore_ascii_case(name)) {
                return self.workspaces.get_mut(index);
            }
        }

        // Fallback to linear search if index miss or stale
        self.workspaces.iter_mut().find(|w| w.name.eq_ignore_ascii_case(name))
    }

    /// Finds the focused workspace.
    #[must_use]
    pub fn focused_workspace(&self) -> Option<&Workspace> {
        self.focused_workspace.as_ref().and_then(|name| self.workspace_by_name(name))
    }

    /// Returns workspaces for a given screen.
    #[must_use]
    pub fn workspaces_for_screen(&self, screen_id: u32) -> Vec<&Workspace> {
        self.workspaces.iter().filter(|w| w.screen_id == screen_id).collect()
    }

    // ========================================================================
    // Window Methods
    // ========================================================================

    /// Finds a window by ID.
    #[must_use]
    pub fn window_by_id(&self, id: u32) -> Option<&TrackedWindow> {
        self.windows.iter().find(|w| w.id == id)
    }

    /// Finds a window by ID (mutable).
    #[must_use]
    pub fn window_by_id_mut(&mut self, id: u32) -> Option<&mut TrackedWindow> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    /// Returns windows for a given workspace.
    #[must_use]
    pub fn windows_for_workspace(&self, workspace_name: &str) -> Vec<&TrackedWindow> {
        self.windows
            .iter()
            .filter(|w| w.workspace_name.eq_ignore_ascii_case(workspace_name))
            .collect()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_point_new() {
        let p = Point::new(10.0, 20.0);
        assert!((p.x - 10.0).abs() < f64::EPSILON);
        assert!((p.y - 20.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_rect_new() {
        let r = Rect::new(10.0, 20.0, 100.0, 200.0);
        assert!((r.x - 10.0).abs() < f64::EPSILON);
        assert!((r.y - 20.0).abs() < f64::EPSILON);
        assert!((r.width - 100.0).abs() < f64::EPSILON);
        assert!((r.height - 200.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_rect_origin() {
        let r = Rect::new(10.0, 20.0, 100.0, 200.0);
        let origin = r.origin();
        assert!((origin.x - 10.0).abs() < f64::EPSILON);
        assert!((origin.y - 20.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_rect_center() {
        let r = Rect::new(0.0, 0.0, 100.0, 200.0);
        let center = r.center();
        assert!((center.x - 50.0).abs() < f64::EPSILON);
        assert!((center.y - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_rect_contains() {
        let r = Rect::new(0.0, 0.0, 100.0, 100.0);
        assert!(r.contains(Point::new(50.0, 50.0)));
        assert!(r.contains(Point::new(0.0, 0.0)));
        assert!(r.contains(Point::new(100.0, 100.0)));
        assert!(!r.contains(Point::new(-1.0, 50.0)));
        assert!(!r.contains(Point::new(101.0, 50.0)));
    }

    #[test]
    fn test_rect_area() {
        let r = Rect::new(0.0, 0.0, 100.0, 200.0);
        assert!((r.area() - 20000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_screen_new() {
        let screen = Screen::new(
            1,
            "Main Display".to_string(),
            Rect::new(0.0, 0.0, 1920.0, 1080.0),
            Rect::new(0.0, 25.0, 1920.0, 1055.0),
            true,
            false,
            2.0,
        );
        assert_eq!(screen.id, 1);
        assert_eq!(screen.name, "Main Display");
        assert!(screen.is_main);
        assert!(!screen.is_builtin);
        assert!((screen.scale_factor - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_workspace_new() {
        let ws = Workspace::new("coding".to_string(), 1, LayoutType::Dwindle);
        assert_eq!(ws.name, "coding");
        assert_eq!(ws.screen_id, 1);
        assert_eq!(ws.layout, LayoutType::Dwindle);
        assert!(!ws.is_visible);
        assert!(!ws.is_focused);
        assert!(ws.is_empty());
    }

    #[test]
    fn test_workspace_window_count() {
        let mut ws = Workspace::new("test".to_string(), 1, LayoutType::Dwindle);
        assert_eq!(ws.window_count(), 0);
        ws.window_ids.push(1);
        ws.window_ids.push(2);
        assert_eq!(ws.window_count(), 2);
    }

    #[test]
    fn test_workspace_focused_window_id() {
        let mut ws = Workspace::new("test".to_string(), 1, LayoutType::Dwindle);
        ws.window_ids = vec![10, 20, 30];
        assert!(ws.focused_window_id().is_none());

        ws.focused_window_index = Some(1);
        assert_eq!(ws.focused_window_id(), Some(20));

        ws.focused_window_index = Some(10); // Out of bounds
        assert!(ws.focused_window_id().is_none());
    }

    #[test]
    fn test_tracked_window_new() {
        let window = TrackedWindow::new(
            123,
            456,
            "com.apple.Safari".to_string(),
            "Safari".to_string(),
            "Apple".to_string(),
            Rect::new(100.0, 100.0, 800.0, 600.0),
            "browser".to_string(),
            false,
        );
        assert_eq!(window.id, 123);
        assert_eq!(window.pid, 456);
        assert_eq!(window.app_id, "com.apple.Safari");
        assert!(!window.is_minimized);
        assert!(!window.is_hidden);
        assert!(!window.is_floating);
    }

    #[test]
    fn test_tiling_state_screen_by_id() {
        let mut state = TilingState::new();
        state.screens.push(Screen::new(
            1,
            "Main".to_string(),
            Rect::default(),
            Rect::default(),
            true,
            false,
            1.0,
        ));
        state.screens.push(Screen::new(
            2,
            "Secondary".to_string(),
            Rect::default(),
            Rect::default(),
            false,
            false,
            1.0,
        ));

        assert!(state.screen_by_id(1).is_some());
        assert_eq!(state.screen_by_id(1).unwrap().name, "Main");
        assert!(state.screen_by_id(2).is_some());
        assert!(state.screen_by_id(99).is_none());
    }

    #[test]
    fn test_tiling_state_screen_by_name() {
        let mut state = TilingState::new();
        state.screens.push(Screen::new(
            1,
            "Main Display".to_string(),
            Rect::default(),
            Rect::default(),
            true,
            false,
            1.0,
        ));
        state.screens.push(Screen::new(
            2,
            "Built-in".to_string(),
            Rect::default(),
            Rect::default(),
            false,
            true,
            1.0,
        ));

        // Special name "main" should find the main screen
        assert!(state.screen_by_name("main").is_some());
        assert_eq!(state.screen_by_name("main").unwrap().id, 1);

        // Special name "builtin" should find the built-in screen
        assert!(state.screen_by_name("builtin").is_some());
        assert_eq!(state.screen_by_name("builtin").unwrap().id, 2);

        // Regular name lookup (case insensitive)
        assert!(state.screen_by_name("Main Display").is_some());
        assert!(state.screen_by_name("main display").is_some());
    }

    #[test]
    fn test_tiling_state_workspace_by_name() {
        let mut state = TilingState::new();
        state.add_workspace(Workspace::new("coding".to_string(), 1, LayoutType::Dwindle));
        state.add_workspace(Workspace::new("browser".to_string(), 1, LayoutType::Monocle));

        assert!(state.workspace_by_name("coding").is_some());
        assert!(state.workspace_by_name("Coding").is_some()); // Case insensitive
        assert!(state.workspace_by_name("unknown").is_none());
    }

    #[test]
    fn test_tiling_state_workspace_index_o1_lookup() {
        let mut state = TilingState::new();
        // Add many workspaces
        for i in 0..100 {
            state.add_workspace(Workspace::new(format!("ws{i}"), 1, LayoutType::Dwindle));
        }

        // Lookups should be O(1) via index
        assert!(state.workspace_by_name("ws0").is_some());
        assert!(state.workspace_by_name("ws50").is_some());
        assert!(state.workspace_by_name("ws99").is_some());
        assert!(state.workspace_by_name("WS50").is_some()); // Case insensitive
    }

    #[test]
    fn test_tiling_state_workspace_index_rebuild() {
        let mut state = TilingState::new();
        // Add directly to vec (bypassing index)
        state
            .workspaces
            .push(Workspace::new("test1".to_string(), 1, LayoutType::Dwindle));
        state
            .workspaces
            .push(Workspace::new("test2".to_string(), 1, LayoutType::Monocle));

        // Index is empty, but fallback should work
        assert!(state.workspace_by_name("test1").is_some());

        // Rebuild index
        state.rebuild_workspace_index();

        // Now index-based lookup works
        assert!(state.workspace_by_name("test1").is_some());
        assert!(state.workspace_by_name("test2").is_some());
    }

    #[test]
    fn test_tiling_state_workspace_by_name_mut_with_index() {
        let mut state = TilingState::new();
        state.add_workspace(Workspace::new("coding".to_string(), 1, LayoutType::Dwindle));

        // Mutable lookup via index
        if let Some(ws) = state.workspace_by_name_mut("coding") {
            ws.is_focused = true;
        }

        assert!(state.workspace_by_name("coding").unwrap().is_focused);
    }

    #[test]
    fn test_tiling_state_focused_workspace() {
        let mut state = TilingState::new();
        state.add_workspace(Workspace::new("coding".to_string(), 1, LayoutType::Dwindle));

        assert!(state.focused_workspace().is_none());

        state.focused_workspace = Some("coding".to_string());
        assert!(state.focused_workspace().is_some());
        assert_eq!(state.focused_workspace().unwrap().name, "coding");
    }

    #[test]
    fn test_tiling_state_workspaces_for_screen() {
        let mut state = TilingState::new();
        state.add_workspace(Workspace::new("ws1".to_string(), 1, LayoutType::Dwindle));
        state.add_workspace(Workspace::new("ws2".to_string(), 1, LayoutType::Dwindle));
        state.add_workspace(Workspace::new("ws3".to_string(), 2, LayoutType::Dwindle));

        let screen1_ws = state.workspaces_for_screen(1);
        assert_eq!(screen1_ws.len(), 2);

        let screen2_ws = state.workspaces_for_screen(2);
        assert_eq!(screen2_ws.len(), 1);
    }

    #[test]
    fn test_tiling_state_windows_for_workspace() {
        let mut state = TilingState::new();
        state.windows.push(TrackedWindow::new(
            1,
            100,
            "app1".to_string(),
            "App1".to_string(),
            "Title1".to_string(),
            Rect::default(),
            "coding".to_string(),
            false,
        ));
        state.windows.push(TrackedWindow::new(
            2,
            100,
            "app2".to_string(),
            "App2".to_string(),
            "Title2".to_string(),
            Rect::default(),
            "coding".to_string(),
            false,
        ));
        state.windows.push(TrackedWindow::new(
            3,
            100,
            "app3".to_string(),
            "App3".to_string(),
            "Title3".to_string(),
            Rect::default(),
            "browser".to_string(),
            false,
        ));

        let coding_windows = state.windows_for_workspace("coding");
        assert_eq!(coding_windows.len(), 2);

        let browser_windows = state.windows_for_workspace("browser");
        assert_eq!(browser_windows.len(), 1);
    }

    #[test]
    fn test_screen_serialization() {
        let screen = Screen::new(
            1,
            "Test".to_string(),
            Rect::new(0.0, 0.0, 1920.0, 1080.0),
            Rect::new(0.0, 25.0, 1920.0, 1055.0),
            true,
            false,
            2.0,
        );

        let json = serde_json::to_string(&screen).unwrap();
        assert!(json.contains("\"id\":1"));
        assert!(json.contains("\"name\":\"Test\""));
        assert!(json.contains("\"isMain\":true"));
        assert!(json.contains("\"scaleFactor\":2"));
    }

    #[test]
    fn test_workspace_original_screen_id_none_by_default() {
        let ws = Workspace::new("test".to_string(), 1, LayoutType::Dwindle);
        assert!(ws.original_screen_id.is_none());
    }

    #[test]
    fn test_workspace_original_screen_id_can_be_set() {
        let mut ws = Workspace::new("test".to_string(), 1, LayoutType::Dwindle);
        ws.original_screen_id = Some(2);
        assert_eq!(ws.original_screen_id, Some(2));
    }

    #[test]
    fn test_workspace_original_screen_id_cleared_when_returning_home() {
        let mut ws = Workspace::new("test".to_string(), 1, LayoutType::Dwindle);
        // Simulate moving to screen 2
        ws.original_screen_id = Some(1);
        ws.screen_id = 2;

        // Simulate returning home
        ws.screen_id = 1;
        ws.original_screen_id = None;

        assert!(ws.original_screen_id.is_none());
        assert_eq!(ws.screen_id, 1);
    }

    #[test]
    fn test_workspace_with_screen_has_no_original_screen() {
        let ws = Workspace::new_with_screen(
            "test".to_string(),
            1,
            "main".to_string(),
            LayoutType::Dwindle,
        );
        assert!(ws.original_screen_id.is_none());
        assert_eq!(ws.configured_screen, "main");
    }

    // ========================================================================
    // Layout Cache Tests
    // ========================================================================

    #[test]
    fn test_layout_cache_new() {
        let cache = LayoutCache::new();
        assert_eq!(cache.input_hash, 0);
        assert!(cache.positions.is_empty());
    }

    #[test]
    fn test_layout_cache_is_valid_empty() {
        let cache = LayoutCache::new();
        // Empty cache should not be valid for any hash
        assert!(!cache.is_valid(0));
        assert!(!cache.is_valid(12345));
    }

    #[test]
    fn test_layout_cache_update_and_is_valid() {
        use crate::tiling::layout::LayoutResult;

        let mut cache = LayoutCache::new();
        let positions: LayoutResult = smallvec::smallvec![
            (1, Rect::new(0.0, 0.0, 100.0, 100.0)),
            (2, Rect::new(100.0, 0.0, 100.0, 100.0)),
        ];

        cache.update(12345, positions.clone());

        assert_eq!(cache.input_hash, 12345);
        assert_eq!(cache.positions.len(), 2);
        assert!(cache.is_valid(12345));
        assert!(!cache.is_valid(99999)); // Different hash
    }

    #[test]
    fn test_layout_cache_invalidate() {
        use crate::tiling::layout::LayoutResult;

        let mut cache = LayoutCache::new();
        let positions: LayoutResult = smallvec::smallvec![(1, Rect::new(0.0, 0.0, 100.0, 100.0))];

        cache.update(12345, positions);
        assert!(cache.is_valid(12345));

        cache.invalidate();
        assert!(!cache.is_valid(12345));
        assert_eq!(cache.input_hash, 0);
        assert!(cache.positions.is_empty());
    }

    #[test]
    fn test_layout_cache_is_valid_requires_positions() {
        let mut cache = LayoutCache::new();
        cache.input_hash = 12345;
        // Empty positions should make cache invalid even with matching hash
        assert!(!cache.is_valid(12345));
    }

    #[test]
    fn test_compute_layout_hash_deterministic() {
        let hash1 = compute_layout_hash(
            LayoutType::Dwindle,
            &[1, 2, 3],
            &Rect::new(0.0, 0.0, 1920.0, 1080.0),
            0.5,
            &[0.33, 0.66],
            12345,
        );

        let hash2 = compute_layout_hash(
            LayoutType::Dwindle,
            &[1, 2, 3],
            &Rect::new(0.0, 0.0, 1920.0, 1080.0),
            0.5,
            &[0.33, 0.66],
            12345,
        );

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_compute_layout_hash_different_layouts() {
        let hash1 = compute_layout_hash(
            LayoutType::Dwindle,
            &[1, 2],
            &Rect::new(0.0, 0.0, 1920.0, 1080.0),
            0.5,
            &[],
            0,
        );

        let hash2 = compute_layout_hash(
            LayoutType::Monocle,
            &[1, 2],
            &Rect::new(0.0, 0.0, 1920.0, 1080.0),
            0.5,
            &[],
            0,
        );

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_compute_layout_hash_different_windows() {
        let hash1 = compute_layout_hash(
            LayoutType::Dwindle,
            &[1, 2, 3],
            &Rect::new(0.0, 0.0, 1920.0, 1080.0),
            0.5,
            &[],
            0,
        );

        let hash2 = compute_layout_hash(
            LayoutType::Dwindle,
            &[1, 2, 4], // Different window ID
            &Rect::new(0.0, 0.0, 1920.0, 1080.0),
            0.5,
            &[],
            0,
        );

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_compute_layout_hash_window_order_matters() {
        let hash1 = compute_layout_hash(
            LayoutType::Dwindle,
            &[1, 2, 3],
            &Rect::new(0.0, 0.0, 1920.0, 1080.0),
            0.5,
            &[],
            0,
        );

        let hash2 = compute_layout_hash(
            LayoutType::Dwindle,
            &[3, 2, 1], // Different order
            &Rect::new(0.0, 0.0, 1920.0, 1080.0),
            0.5,
            &[],
            0,
        );

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_compute_layout_hash_different_screen_frame() {
        let hash1 = compute_layout_hash(
            LayoutType::Dwindle,
            &[1, 2],
            &Rect::new(0.0, 0.0, 1920.0, 1080.0),
            0.5,
            &[],
            0,
        );

        let hash2 = compute_layout_hash(
            LayoutType::Dwindle,
            &[1, 2],
            &Rect::new(0.0, 0.0, 2560.0, 1440.0), // Different resolution
            0.5,
            &[],
            0,
        );

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_compute_layout_hash_different_master_ratio() {
        let hash1 = compute_layout_hash(
            LayoutType::Dwindle,
            &[1, 2],
            &Rect::new(0.0, 0.0, 1920.0, 1080.0),
            0.5,
            &[],
            0,
        );

        let hash2 = compute_layout_hash(
            LayoutType::Dwindle,
            &[1, 2],
            &Rect::new(0.0, 0.0, 1920.0, 1080.0),
            0.6, // Different ratio
            &[],
            0,
        );

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_compute_layout_hash_different_split_ratios() {
        let hash1 = compute_layout_hash(
            LayoutType::Split,
            &[1, 2, 3],
            &Rect::new(0.0, 0.0, 1920.0, 1080.0),
            0.5,
            &[0.33, 0.66],
            0,
        );

        let hash2 = compute_layout_hash(
            LayoutType::Split,
            &[1, 2, 3],
            &Rect::new(0.0, 0.0, 1920.0, 1080.0),
            0.5,
            &[0.5, 0.75], // Different ratios
            0,
        );

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_compute_layout_hash_different_gaps() {
        let hash1 = compute_layout_hash(
            LayoutType::Dwindle,
            &[1, 2],
            &Rect::new(0.0, 0.0, 1920.0, 1080.0),
            0.5,
            &[],
            12345,
        );

        let hash2 = compute_layout_hash(
            LayoutType::Dwindle,
            &[1, 2],
            &Rect::new(0.0, 0.0, 1920.0, 1080.0),
            0.5,
            &[],
            99999, // Different gaps hash
        );

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_workspace_layout_cache_default() {
        let ws = Workspace::new("test".to_string(), 1, LayoutType::Dwindle);
        assert_eq!(ws.layout_cache.input_hash, 0);
        assert!(ws.layout_cache.positions.is_empty());
    }

    #[test]
    fn test_workspace_layout_cache_skipped_in_serialization() {
        let mut ws = Workspace::new("test".to_string(), 1, LayoutType::Dwindle);
        ws.layout_cache.update(12345, smallvec::smallvec![(
            1,
            Rect::new(0.0, 0.0, 100.0, 100.0)
        )]);

        // Serialize and deserialize
        let json = serde_json::to_string(&ws).unwrap();
        let deserialized: Workspace = serde_json::from_str(&json).unwrap();

        // Cache should be reset after deserialization (skipped in serde)
        assert_eq!(deserialized.layout_cache.input_hash, 0);
        assert!(deserialized.layout_cache.positions.is_empty());
    }
}
