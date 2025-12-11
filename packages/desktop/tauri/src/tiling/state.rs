//! Tiling state management.
//!
//! This module contains the ephemeral state for the tiling window manager.
//! State is not persisted and is rebuilt on each app launch.

use std::collections::HashMap;

use barba_shared::{FocusedAppInfo, LayoutMode, ScreenInfo, WindowInfo, WorkspaceInfo};
use smallvec::SmallVec;

/// Type alias for window ID lists that are typically small.
///
/// Most workspaces have fewer than 8 windows, so we use `SmallVec` to avoid
/// heap allocations for the common case. This reduces memory allocations
/// in hot paths like layout computation.
pub type WindowIdList = SmallVec<[u64; 8]>;

/// Type alias for split ratio lists that are typically small.
///
/// Split ratios rarely exceed 4 levels deep, so we stack-allocate for the common case.
pub type SplitRatioList = SmallVec<[f64; 4]>;

/// A managed window tracked by the tiling manager.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct ManagedWindow {
    /// Unique window identifier (`CGWindowID`).
    pub id: u64,

    /// Window title.
    pub title: String,

    /// Application name.
    pub app_name: String,

    /// Application bundle identifier.
    pub bundle_id: Option<String>,

    /// Window class name.
    pub class: Option<String>,

    /// Process ID of the owning application.
    pub pid: i32,

    /// Workspace this window belongs to.
    pub workspace: String,

    /// Whether this window is floating (exempt from tiling).
    pub is_floating: bool,

    /// Whether this window is minimized.
    pub is_minimized: bool,

    /// Whether this window is fullscreen.
    pub is_fullscreen: bool,

    /// Whether this window is currently hidden (moved off-screen).
    pub is_hidden: bool,

    /// Current window frame.
    pub frame: WindowFrame,
}

impl ManagedWindow {
    /// Converts to the shared `WindowInfo` type for CLI output.
    #[must_use]
    pub fn to_info(&self, is_focused: bool) -> WindowInfo {
        WindowInfo {
            id: self.id,
            title: self.title.clone(),
            app_name: self.app_name.clone(),
            bundle_id: self.bundle_id.clone(),
            class: self.class.clone(),
            workspace: self.workspace.clone(),
            is_focused,
            is_floating: self.is_floating,
            x: self.frame.x,
            y: self.frame.y,
            width: self.frame.width,
            height: self.frame.height,
        }
    }
}

/// Window frame (position and size).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowFrame {
    /// X position (from left edge of screen).
    pub x: i32,

    /// Y position (from top edge of screen).
    pub y: i32,

    /// Width in pixels.
    pub width: u32,

    /// Height in pixels.
    pub height: u32,
}

impl WindowFrame {
    /// Creates a new window frame.
    #[must_use]
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }
}

/// A virtual workspace.
#[derive(Debug, Clone)]
pub struct Workspace {
    /// Workspace name/identifier.
    pub name: String,

    /// Current layout mode.
    pub layout: LayoutMode,

    /// Screen this workspace is on.
    pub screen: String,

    /// The intended screen target from config.
    /// If this differs from `screen`, the workspace is in fallback mode
    /// and should be migrated when the intended screen becomes available.
    pub intended_screen: Option<barba_shared::ScreenTarget>,

    /// Window IDs in this workspace (in layout order).
    /// Uses `SmallVec` to avoid heap allocation for typical workspaces with < 8 windows.
    pub windows: WindowIdList,

    /// Split ratios for tiling layout (BSP tree representation).
    /// Each value is the ratio for the split at that level.
    /// Uses `SmallVec` to avoid heap allocation for typical layouts with < 4 splits.
    pub split_ratios: SplitRatioList,
}

impl Workspace {
    /// Creates a new workspace with default settings.
    #[must_use]
    pub fn new(name: String, layout: LayoutMode, screen: String) -> Self {
        Self {
            name,
            layout,
            screen,
            intended_screen: None,
            windows: SmallVec::new(),
            split_ratios: SmallVec::new(),
        }
    }

    /// Creates a new workspace that is in fallback mode.
    ///
    /// The workspace will be created on the given screen but remembers its
    /// intended screen target for migration when that screen becomes available.
    #[must_use]
    pub fn new_with_fallback(
        name: String,
        layout: LayoutMode,
        screen: String,
        intended_screen: barba_shared::ScreenTarget,
    ) -> Self {
        Self {
            name,
            layout,
            screen,
            intended_screen: Some(intended_screen),
            windows: SmallVec::new(),
            split_ratios: SmallVec::new(),
        }
    }

    /// Converts to the shared `WorkspaceInfo` type for CLI output.
    ///
    /// Takes the screens list to resolve screen ID to a human-readable name,
    /// the windows map to find the focused app, and the focused window ID.
    #[must_use]
    pub fn to_info(
        &self,
        is_focused: bool,
        screens: &[Screen],
        windows: &HashMap<u64, ManagedWindow>,
        focused_window_id: Option<u64>,
    ) -> WorkspaceInfo {
        // Look up the screen name from the screen ID
        let screen_name = screens.iter().find(|s| s.id == self.screen).map_or_else(
            || self.screen.clone(),
            |s| {
                if s.is_main {
                    "main".to_string()
                } else if screens.len() == 2 {
                    "secondary".to_string()
                } else {
                    s.name.clone()
                }
            },
        );

        // Find the focused app in this workspace
        // If the globally focused window is in this workspace, use that app.
        // Otherwise, fall back to the first window in the workspace.
        let focused_app = focused_window_id
            .filter(|&wid| self.windows.contains(&wid))
            .and_then(|wid| windows.get(&wid))
            .or_else(|| {
                // Fall back to first window in workspace if no focused window here
                self.windows.first().and_then(|wid| windows.get(wid))
            })
            .map(|target_win| {
                // Count windows from this app in the workspace
                let window_count = self
                    .windows
                    .iter()
                    .filter_map(|wid| windows.get(wid))
                    .filter(|w| w.bundle_id == target_win.bundle_id)
                    .count();

                FocusedAppInfo {
                    name: target_win.app_name.clone(),
                    app_id: target_win.bundle_id.clone().unwrap_or_default(),
                    window_count,
                }
            });

        WorkspaceInfo {
            name: self.name.clone(),
            layout: self.layout.clone(),
            screen: screen_name,
            is_focused,
            window_count: self.windows.len(),
            focused_app,
        }
    }
}

/// A connected screen/display.
#[derive(Debug, Clone)]
pub struct Screen {
    /// Unique identifier (`CGDirectDisplayID` as string).
    pub id: String,

    /// Human-readable name.
    pub name: String,

    /// Whether this is the main display.
    pub is_main: bool,

    /// Screen bounds.
    pub frame: ScreenFrame,

    /// Usable area (excluding menu bar, dock, etc.).
    pub usable_frame: ScreenFrame,
}

impl Screen {
    /// Converts to the shared `ScreenInfo` type for CLI output.
    ///
    /// Takes the total screen count to determine if we should use "secondary" as a name.
    #[must_use]
    pub fn to_info(&self, screen_count: usize) -> ScreenInfo {
        // Use friendly names: "main" for main display, "secondary" if there are only 2 screens
        let friendly_name = if self.is_main {
            "main".to_string()
        } else if screen_count == 2 {
            "secondary".to_string()
        } else {
            self.name.clone()
        };

        ScreenInfo {
            id: friendly_name,
            name: self.name.clone(),
            is_main: self.is_main,
            x: self.frame.x,
            y: self.frame.y,
            width: self.frame.width,
            height: self.frame.height,
            usable_x: self.usable_frame.x,
            usable_y: self.usable_frame.y,
            usable_width: self.usable_frame.width,
            usable_height: self.usable_frame.height,
        }
    }
}

/// Screen frame (position and dimensions).
#[derive(Debug, Clone, Copy, Default)]
pub struct ScreenFrame {
    /// X position (for multi-monitor setups).
    pub x: i32,

    /// Y position (for multi-monitor setups).
    pub y: i32,

    /// Width in pixels.
    pub width: u32,

    /// Height in pixels.
    pub height: u32,
}

/// The complete tiling state.
#[derive(Debug, Default)]
pub struct TilingState {
    /// All connected screens.
    pub screens: Vec<Screen>,

    /// All workspaces.
    pub workspaces: Vec<Workspace>,

    /// All managed windows by ID.
    pub windows: HashMap<u64, ManagedWindow>,

    /// Currently focused workspace name.
    pub focused_workspace: Option<String>,

    /// Currently focused window ID.
    pub focused_window: Option<u64>,

    /// Focused workspace per screen (`screen_id` -> `workspace_name`).
    pub focused_workspace_per_screen: HashMap<String, String>,
}

impl TilingState {
    /// Creates a new empty state.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Gets a workspace by name.
    #[must_use]
    pub fn get_workspace(&self, name: &str) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.name == name)
    }

    /// Gets a mutable workspace by name.
    pub fn get_workspace_mut(&mut self, name: &str) -> Option<&mut Workspace> {
        self.workspaces.iter_mut().find(|w| w.name == name)
    }

    /// Gets a screen by ID.
    #[must_use]
    pub fn get_screen(&self, id: &str) -> Option<&Screen> {
        self.screens.iter().find(|s| s.id == id)
    }

    /// Gets the main screen.
    #[must_use]
    pub fn get_main_screen(&self) -> Option<&Screen> { self.screens.iter().find(|s| s.is_main) }

    /// Gets a window by ID.
    #[must_use]
    pub fn get_window(&self, id: u64) -> Option<&ManagedWindow> { self.windows.get(&id) }

    /// Gets a mutable window by ID.
    pub fn get_window_mut(&mut self, id: u64) -> Option<&mut ManagedWindow> {
        self.windows.get_mut(&id)
    }

    /// Gets all workspaces on a screen.
    #[must_use]
    pub fn get_workspaces_on_screen(&self, screen_id: &str) -> Vec<&Workspace> {
        self.workspaces.iter().filter(|w| w.screen == screen_id).collect()
    }

    /// Finds the screen in a given direction relative to a source screen.
    ///
    /// Direction logic:
    /// - `left`: Screen with center to the left of source center
    /// - `right`: Screen with center to the right of source center
    /// - `up`: Screen with center above source center
    /// - `down`: Screen with center below source center
    #[must_use]
    #[allow(clippy::cast_possible_wrap)]
    pub fn get_screen_in_direction(
        &self,
        source_screen_id: &str,
        direction: &str,
    ) -> Option<&Screen> {
        let source = self.get_screen(source_screen_id)?;
        let source_center_x = source.frame.x + source.frame.width as i32 / 2;
        let source_center_y = source.frame.y + source.frame.height as i32 / 2;

        let mut best_match: Option<(&Screen, i32)> = None;

        for screen in &self.screens {
            if screen.id == source_screen_id {
                continue;
            }

            let screen_center_x = screen.frame.x + screen.frame.width as i32 / 2;
            let screen_center_y = screen.frame.y + screen.frame.height as i32 / 2;

            let is_valid_direction = match direction {
                "left" => screen_center_x < source_center_x,
                "right" => screen_center_x > source_center_x,
                "up" => screen_center_y < source_center_y,
                "down" => screen_center_y > source_center_y,
                _ => false,
            };

            if !is_valid_direction {
                continue;
            }

            // Calculate distance (Manhattan distance for simplicity)
            let distance = (screen_center_x - source_center_x).abs()
                + (screen_center_y - source_center_y).abs();

            if best_match.is_none() || distance < best_match.unwrap().1 {
                best_match = Some((screen, distance));
            }
        }

        best_match.map(|(s, _)| s)
    }

    /// Resolves a screen target to a screen ID.
    ///
    /// Target can be:
    /// - `main`: The main display
    /// - `secondary`: The secondary display (if exactly 2 screens)
    /// - `left`, `right`, `up`, `down`: Directional relative to current screen
    /// - A screen name or ID
    #[must_use]
    pub fn resolve_screen_target(
        &self,
        target: &str,
        current_screen_id: Option<&str>,
    ) -> Option<String> {
        match target.to_lowercase().as_str() {
            "main" => self.screens.iter().find(|s| s.is_main).map(|s| s.id.clone()),
            "secondary" => {
                if self.screens.len() == 2 {
                    self.screens.iter().find(|s| !s.is_main).map(|s| s.id.clone())
                } else {
                    None
                }
            }
            "left" | "right" | "up" | "down" => current_screen_id
                .and_then(|current_id| self.get_screen_in_direction(current_id, target))
                .map(|s| s.id.clone()),
            _ => {
                // Try to find by name or ID
                self.screens
                    .iter()
                    .find(|s| s.name == target || s.id == target)
                    .map(|s| s.id.clone())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // Window Frame Tests
    // ========================================================================

    #[test]
    fn test_window_frame_new() {
        let frame = WindowFrame::new(100, 200, 800, 600);
        assert_eq!(frame.x, 100);
        assert_eq!(frame.y, 200);
        assert_eq!(frame.width, 800);
        assert_eq!(frame.height, 600);
    }

    #[test]
    fn test_window_frame_default() {
        let frame = WindowFrame::default();
        assert_eq!(frame.x, 0);
        assert_eq!(frame.y, 0);
        assert_eq!(frame.width, 0);
        assert_eq!(frame.height, 0);
    }

    // ========================================================================
    // Workspace Tests
    // ========================================================================

    #[test]
    fn test_workspace_new() {
        let ws = Workspace::new("1".to_string(), LayoutMode::Tiling, "main".to_string());
        assert_eq!(ws.name, "1");
        assert_eq!(ws.layout, LayoutMode::Tiling);
        assert_eq!(ws.screen, "main");
        assert!(ws.windows.is_empty());
        assert!(ws.split_ratios.is_empty());
    }

    #[test]
    fn test_workspace_to_info() {
        let screens = vec![Screen {
            id: "main-id".to_string(),
            name: "Built-in Display".to_string(),
            is_main: true,
            frame: ScreenFrame::default(),
            usable_frame: ScreenFrame::default(),
        }];

        let mut ws =
            Workspace::new("coding".to_string(), LayoutMode::Master, "main-id".to_string());
        ws.windows.push(1);
        ws.windows.push(2);

        let info = ws.to_info(true, &screens, &std::collections::HashMap::new(), None);
        assert_eq!(info.name, "coding");
        assert_eq!(info.layout, LayoutMode::Master);
        assert_eq!(info.screen, "main");
        assert!(info.is_focused);
        assert_eq!(info.window_count, 2);
    }

    #[test]
    fn test_workspace_to_info_secondary_screen() {
        let screens = vec![
            Screen {
                id: "main-id".to_string(),
                name: "Built-in Display".to_string(),
                is_main: true,
                frame: ScreenFrame::default(),
                usable_frame: ScreenFrame::default(),
            },
            Screen {
                id: "secondary-id".to_string(),
                name: "External Display".to_string(),
                is_main: false,
                frame: ScreenFrame::default(),
                usable_frame: ScreenFrame::default(),
            },
        ];

        let ws = Workspace::new("2".to_string(), LayoutMode::Floating, "secondary-id".to_string());
        let info = ws.to_info(false, &screens, &std::collections::HashMap::new(), None);
        assert_eq!(info.screen, "secondary");
        assert!(!info.is_focused);
    }

    // ========================================================================
    // TilingState Tests
    // ========================================================================

    #[test]
    fn test_tiling_state_get_workspace() {
        let mut state = TilingState::new();
        state.workspaces.push(Workspace::new(
            "1".to_string(),
            LayoutMode::Tiling,
            "main".to_string(),
        ));

        assert!(state.get_workspace("1").is_some());
        assert!(state.get_workspace("2").is_none());
    }

    #[test]
    fn test_tiling_state_get_workspace_mut() {
        let mut state = TilingState::new();
        state.workspaces.push(Workspace::new(
            "1".to_string(),
            LayoutMode::Tiling,
            "main".to_string(),
        ));

        // Modify the workspace
        if let Some(ws) = state.get_workspace_mut("1") {
            ws.layout = LayoutMode::Monocle;
        }

        assert_eq!(state.get_workspace("1").unwrap().layout, LayoutMode::Monocle);
    }

    #[test]
    fn test_tiling_state_get_workspaces_on_screen() {
        let mut state = TilingState::new();
        state.workspaces.push(Workspace::new(
            "1".to_string(),
            LayoutMode::Tiling,
            "main".to_string(),
        ));
        state.workspaces.push(Workspace::new(
            "2".to_string(),
            LayoutMode::Monocle,
            "main".to_string(),
        ));
        state.workspaces.push(Workspace::new(
            "3".to_string(),
            LayoutMode::Floating,
            "secondary".to_string(),
        ));

        let main_workspaces = state.get_workspaces_on_screen("main");
        assert_eq!(main_workspaces.len(), 2);

        let secondary_workspaces = state.get_workspaces_on_screen("secondary");
        assert_eq!(secondary_workspaces.len(), 1);
    }

    // ========================================================================
    // Screen Direction Tests
    // ========================================================================

    #[test]
    fn test_get_screen_in_direction_horizontal() {
        let mut state = TilingState::new();

        // Left screen at x=0
        state.screens.push(Screen {
            id: "left".to_string(),
            name: "Left Display".to_string(),
            is_main: false,
            frame: ScreenFrame {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
            usable_frame: ScreenFrame::default(),
        });

        // Right screen at x=1920
        state.screens.push(Screen {
            id: "right".to_string(),
            name: "Right Display".to_string(),
            is_main: true,
            frame: ScreenFrame {
                x: 1920,
                y: 0,
                width: 1920,
                height: 1080,
            },
            usable_frame: ScreenFrame::default(),
        });

        // From left screen, going right should find right screen
        let target = state.get_screen_in_direction("left", "right");
        assert!(target.is_some());
        assert_eq!(target.unwrap().id, "right");

        // From right screen, going left should find left screen
        let target = state.get_screen_in_direction("right", "left");
        assert!(target.is_some());
        assert_eq!(target.unwrap().id, "left");

        // From left screen, going left should find nothing
        let target = state.get_screen_in_direction("left", "left");
        assert!(target.is_none());
    }

    #[test]
    fn test_get_screen_in_direction_vertical() {
        let mut state = TilingState::new();

        // Top screen
        state.screens.push(Screen {
            id: "top".to_string(),
            name: "Top Display".to_string(),
            is_main: true,
            frame: ScreenFrame {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
            usable_frame: ScreenFrame::default(),
        });

        // Bottom screen
        state.screens.push(Screen {
            id: "bottom".to_string(),
            name: "Bottom Display".to_string(),
            is_main: false,
            frame: ScreenFrame {
                x: 0,
                y: 1080,
                width: 1920,
                height: 1080,
            },
            usable_frame: ScreenFrame::default(),
        });

        // From top, going down should find bottom
        let target = state.get_screen_in_direction("top", "down");
        assert!(target.is_some());
        assert_eq!(target.unwrap().id, "bottom");

        // From bottom, going up should find top
        let target = state.get_screen_in_direction("bottom", "up");
        assert!(target.is_some());
        assert_eq!(target.unwrap().id, "top");
    }

    #[test]
    fn test_resolve_screen_target() {
        let mut state = TilingState::new();
        state.screens.push(Screen {
            id: "main-id".to_string(),
            name: "Main Display".to_string(),
            is_main: true,
            frame: ScreenFrame {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
            usable_frame: ScreenFrame::default(),
        });
        state.screens.push(Screen {
            id: "secondary-id".to_string(),
            name: "External Display".to_string(),
            is_main: false,
            frame: ScreenFrame {
                x: 1920,
                y: 0,
                width: 1920,
                height: 1080,
            },
            usable_frame: ScreenFrame::default(),
        });

        // Test "main" resolves to main screen
        let result = state.resolve_screen_target("main", None);
        assert_eq!(result, Some("main-id".to_string()));

        // Test "secondary" resolves to secondary screen (with 2 screens)
        let result = state.resolve_screen_target("secondary", None);
        assert_eq!(result, Some("secondary-id".to_string()));

        // Test directional resolution
        let result = state.resolve_screen_target("right", Some("main-id"));
        assert_eq!(result, Some("secondary-id".to_string()));

        // Test screen name resolution
        let result = state.resolve_screen_target("External Display", None);
        assert_eq!(result, Some("secondary-id".to_string()));
    }

    // ========================================================================
    // ManagedWindow Tests
    // ========================================================================

    #[test]
    fn test_managed_window_to_info() {
        let window = ManagedWindow {
            id: 42,
            title: "Test Window".to_string(),
            app_name: "TestApp".to_string(),
            bundle_id: Some("com.test.app".to_string()),
            class: Some("NSWindow".to_string()),
            pid: 1234,
            workspace: "coding".to_string(),
            is_floating: true,
            is_minimized: false,
            is_fullscreen: false,
            is_hidden: false,
            frame: WindowFrame::new(100, 200, 800, 600),
        };

        let info = window.to_info(true);
        assert_eq!(info.id, 42);
        assert_eq!(info.title, "Test Window");
        assert_eq!(info.app_name, "TestApp");
        assert_eq!(info.bundle_id, Some("com.test.app".to_string()));
        assert_eq!(info.class, Some("NSWindow".to_string()));
        assert_eq!(info.workspace, "coding");
        assert!(info.is_focused);
        assert!(info.is_floating);
        assert_eq!(info.x, 100);
        assert_eq!(info.y, 200);
        assert_eq!(info.width, 800);
        assert_eq!(info.height, 600);
    }

    // ========================================================================
    // Screen Tests
    // ========================================================================

    #[test]
    fn test_screen_to_info_main() {
        let screen = Screen {
            id: "12345".to_string(),
            name: "Built-in Retina Display".to_string(),
            is_main: true,
            frame: ScreenFrame {
                x: 0,
                y: 0,
                width: 2560,
                height: 1440,
            },
            usable_frame: ScreenFrame {
                x: 0,
                y: 25,
                width: 2560,
                height: 1415,
            },
        };

        let info = screen.to_info(2); // 2 screens total
        assert_eq!(info.id, "main");
        assert_eq!(info.name, "Built-in Retina Display");
        assert!(info.is_main);
        assert_eq!(info.width, 2560);
        assert_eq!(info.height, 1440);
        assert_eq!(info.usable_y, 25); // Menu bar offset
    }

    #[test]
    fn test_screen_to_info_secondary() {
        let screen = Screen {
            id: "67890".to_string(),
            name: "LG UltraFine".to_string(),
            is_main: false,
            frame: ScreenFrame {
                x: 2560,
                y: 0,
                width: 3840,
                height: 2160,
            },
            usable_frame: ScreenFrame {
                x: 2560,
                y: 0,
                width: 3840,
                height: 2160,
            },
        };

        let info = screen.to_info(2); // 2 screens total
        assert_eq!(info.id, "secondary");
        assert_eq!(info.name, "LG UltraFine");
        assert!(!info.is_main);
    }
}
