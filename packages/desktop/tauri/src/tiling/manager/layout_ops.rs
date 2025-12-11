//! Layout application and management.
//!
//! This module handles applying layouts to workspaces, including
//! computing window positions and sizes.

use std::collections::{HashMap, HashSet};

use barba_shared::LayoutMode;
use rayon::prelude::*;

use super::TilingManager;
use crate::tiling::error::TilingError;
use crate::tiling::layout::{self, Layout, ResolvedGaps};
use crate::tiling::state::ManagedWindow;
use crate::tiling::window;

/// Data collected from a workspace for parallel layout computation.
/// This struct contains all the data needed to compute a layout without
/// holding any references to the `TilingManager`.
struct WorkspaceLayoutData {
    /// Workspace name.
    name: String,
    /// Screen ID for the workspace.
    #[allow(dead_code)]
    screen_id: String,
    /// Layout mode for the workspace.
    layout_mode: LayoutMode,
    /// Window IDs assigned to the workspace.
    window_ids: Vec<u64>,
    /// App IDs assigned to this workspace from rules.
    #[allow(dead_code)]
    app_ids: Vec<String>,
    /// Layout context for computing window positions.
    context: layout::LayoutContext,
    /// Master layout config (cloned for parallel access).
    master_config: barba_shared::MasterConfig,
}

/// Result of computing a layout for a workspace.
struct ComputedLayout {
    /// Workspace name.
    #[allow(dead_code)]
    name: String,
    /// Computed window layouts.
    layouts: Vec<layout::WindowLayout>,
}

impl TilingManager {
    /// Apps known to create auxiliary windows that should be filtered.
    /// Format: (`bundle_id_pattern`, `suspicious_title_pattern`)
    const ELECTRON_HELPER_PATTERNS: &'static [(&'static str, &'static str)] = &[
        ("com.microsoft.teams", "Microsoft Teams"),
        ("com.tinyspeck.slackmacgap", "Slack"),
        ("com.hnc.Discord", "Discord"),
        ("com.microsoft.VSCode", "Visual Studio Code"),
        ("com.microsoft.VSCodeInsiders", "Visual Studio Code - Insiders"),
        ("com.visualstudio.code.oss", "Code - OSS"),
        ("com.github.Electron", ""), // Generic Electron helper
    ];

    /// Applies layouts to all workspaces using parallel computation.
    ///
    /// Layout computation (CPU-bound, pure) is parallelized across workspaces,
    /// while window manipulation (AX API calls) remains sequential.
    pub(super) fn apply_all_layouts(&mut self) {
        // Get the current list of actual windows from the system (expensive, do once)
        let actual_windows: Vec<ManagedWindow> =
            window::get_all_windows_including_hidden().unwrap_or_default();
        let actual_window_ids: HashSet<u64> = actual_windows.iter().map(|w| w.id).collect();

        // Collect all workspace data needed for layout computation
        let workspace_data =
            self.collect_workspace_layout_data(&actual_windows, &actual_window_ids);

        // Compute layouts in parallel (CPU-bound, no system calls)
        let computed_layouts: Vec<ComputedLayout> = workspace_data
            .into_par_iter()
            .filter_map(|data| Self::compute_workspace_layout_parallel(&data, &actual_windows).ok())
            .collect();

        // Apply layouts sequentially (AX API calls must be serial)
        for computed in computed_layouts {
            Self::apply_window_layouts(&computed.layouts);
        }
    }

    /// Collects all data needed for parallel layout computation from all workspaces.
    fn collect_workspace_layout_data(
        &mut self,
        actual_windows: &[ManagedWindow],
        actual_window_ids: &HashSet<u64>,
    ) -> Vec<WorkspaceLayoutData> {
        let state = self.workspace_manager.state();

        // Collect workspace names and basic info first
        let workspace_infos: Vec<(String, String, LayoutMode, Vec<u64>)> = state
            .workspaces
            .iter()
            .map(|ws| {
                (
                    ws.name.clone(),
                    ws.screen.clone(),
                    ws.layout.clone(),
                    ws.windows.to_vec(),
                )
            })
            .collect();

        let mut workspace_data = Vec::with_capacity(workspace_infos.len());

        for (name, screen_id, layout_mode, workspace_window_ids) in workspace_infos {
            // Get the apps assigned to this workspace from rules
            let app_ids = self.get_workspace_app_ids(&name);

            // Count windows per app to detect multi-window apps
            let app_window_counts = Self::count_windows_per_app(actual_windows, &app_ids);

            // Filter to valid window IDs for this workspace
            let window_ids = self.filter_valid_window_ids(
                &workspace_window_ids,
                actual_windows,
                actual_window_ids,
                &app_ids,
                &app_window_counts,
                &name,
                &layout_mode,
            );

            // Clean up stale windows (mutates state, must be done here)
            self.cleanup_stale_windows(&name, &workspace_window_ids, &window_ids);

            // Build layout context
            let context = match self.build_layout_context(&name, &screen_id) {
                Ok(ctx) => ctx,
                Err(e) => {
                    eprintln!("barba: failed to build layout context for '{name}': {e}");
                    continue;
                }
            };

            // Get split ratios from config
            let split_ratios = self
                .workspace_manager
                .state()
                .get_workspace(&name)
                .map(|ws| ws.split_ratios.to_vec())
                .unwrap_or_default();

            // Update context with split ratios
            let context = layout::LayoutContext {
                screen_frame: context.screen_frame,
                gaps: context.gaps,
                split_ratios,
            };

            workspace_data.push(WorkspaceLayoutData {
                name,
                screen_id,
                layout_mode,
                window_ids,
                app_ids,
                context,
                master_config: self.config.master.clone(),
            });
        }

        workspace_data
    }

    /// Computes the layout for a single workspace (pure, can run in parallel).
    fn compute_workspace_layout_parallel(
        data: &WorkspaceLayoutData,
        actual_windows: &[ManagedWindow],
    ) -> Result<ComputedLayout, TilingError> {
        // Build layout windows from the pre-filtered window IDs
        // Note: We can't access managed windows here since we're in parallel,
        // so we use actual_windows which contains all the info we need
        let layout_windows: Vec<layout::LayoutWindow> = data
            .window_ids
            .iter()
            .filter_map(|&id| {
                actual_windows.iter().find(|w| w.id == id).map(|w| {
                    layout::LayoutWindow {
                        id: w.id,
                        is_floating: false, // Will be handled by the layout
                        is_minimized: w.is_minimized,
                        is_fullscreen: w.is_fullscreen,
                    }
                })
            })
            .collect();

        // Compute the layout (pure computation)
        let layouts = Self::compute_layouts_static(
            &data.layout_mode,
            &layout_windows,
            &data.context,
            &data.master_config,
        )?;

        Ok(ComputedLayout {
            name: data.name.clone(),
            layouts,
        })
    }

    /// Static version of `compute_layouts` that doesn't require `&self`.
    /// Used for parallel computation.
    fn compute_layouts_static(
        layout_mode: &LayoutMode,
        layout_windows: &[layout::LayoutWindow],
        context: &layout::LayoutContext,
        master_config: &barba_shared::MasterConfig,
    ) -> Result<Vec<layout::WindowLayout>, TilingError> {
        match layout_mode {
            LayoutMode::Monocle => {
                let monocle = layout::MonocleLayout::new();
                monocle.layout(layout_windows, context)
            }
            LayoutMode::Tiling | LayoutMode::Scrolling => {
                let tiling = layout::TilingLayout::new();
                tiling.layout(layout_windows, context)
            }
            LayoutMode::Split => {
                let split = layout::SplitLayout::auto();
                split.layout(layout_windows, context)
            }
            LayoutMode::SplitVertical => {
                let split = layout::SplitLayout::vertical();
                split.layout(layout_windows, context)
            }
            LayoutMode::SplitHorizontal => {
                let split = layout::SplitLayout::horizontal();
                split.layout(layout_windows, context)
            }
            LayoutMode::Master => {
                let master = layout::MasterLayout::new(master_config.clone());
                master.layout(layout_windows, context)
            }
            LayoutMode::Floating => {
                let floating = layout::FloatingLayout::new();
                floating.layout(layout_windows, context)
            }
        }
    }

    /// Applies the layout to a workspace.
    pub fn apply_layout(&mut self, workspace_name: &str) -> Result<(), TilingError> {
        // Get the current list of actual windows from the system
        let actual_windows: Vec<ManagedWindow> =
            window::get_all_windows_including_hidden().unwrap_or_default();
        let actual_window_ids: HashSet<u64> = actual_windows.iter().map(|w| w.id).collect();

        let workspace = self
            .workspace_manager
            .state()
            .get_workspace(workspace_name)
            .ok_or_else(|| TilingError::WorkspaceNotFound(workspace_name.to_string()))?;

        let screen_id = workspace.screen.clone();
        let layout_mode = workspace.layout.clone();
        let workspace_window_ids = workspace.windows.clone();

        // Get the apps assigned to this workspace from rules
        let workspace_app_ids = self.get_workspace_app_ids(workspace_name);

        // Count windows per app to detect multi-window apps
        let app_window_counts = Self::count_windows_per_app(&actual_windows, &workspace_app_ids);

        // Filter to valid window IDs for this workspace
        let window_ids = self.filter_valid_window_ids(
            &workspace_window_ids,
            &actual_windows,
            &actual_window_ids,
            &workspace_app_ids,
            &app_window_counts,
            workspace_name,
            &layout_mode,
        );

        /*
            TODO: Native macOS tabs (multiple windows with same frame from same app) are not
            currently filtered. Apps using native tabs may show "empty" tiles for inactive tabs.
            A proper solution would require tracking tab relationships at window discovery time.
        */

        // Clean up stale windows
        self.cleanup_stale_windows(workspace_name, &workspace_window_ids, &window_ids);

        // Build layout context and compute layouts
        let context = self.build_layout_context(workspace_name, &screen_id)?;
        let layout_windows = self.build_layout_windows(&window_ids);
        let layouts = self.compute_layouts(&layout_mode, &layout_windows, &context)?;

        // Apply the computed layouts
        Self::apply_window_layouts(&layouts);

        Ok(())
    }

    /// Gets the app IDs assigned to a workspace from rules.
    fn get_workspace_app_ids(&self, workspace_name: &str) -> Vec<String> {
        self.config
            .workspaces
            .iter()
            .find(|ws| ws.name == workspace_name)
            .map(|ws| ws.rules.iter().filter_map(|rule| rule.app_id.clone()).collect())
            .unwrap_or_default()
    }

    /// Counts windows per app for multi-window detection.
    fn count_windows_per_app(
        actual_windows: &[ManagedWindow],
        workspace_app_ids: &[String],
    ) -> HashMap<String, usize> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for win in actual_windows {
            if let Some(ref bundle_id) = win.bundle_id
                && workspace_app_ids
                    .iter()
                    .any(|app_id| bundle_id.contains(app_id) || bundle_id == app_id)
            {
                *counts.entry(bundle_id.clone()).or_insert(0) += 1;
            }
        }
        counts
    }

    /// Filters out native tabs by detecting windows from the same app with identical frames.
    ///
    /// In tiling layouts (tiling/split/master), each window should occupy a unique position.
    /// If multiple windows from the same app share the exact same frame, they are native tabs.
    /// Filters window IDs to only include valid windows for the workspace.
    ///
    /// Valid windows must:
    /// 1. Still exist on the system
    /// 2. Either belong to apps assigned by rules OR be explicitly added
    /// 3. Not be small helper/splash windows for multi-window apps
    /// 4. Not be `PiP` windows (for non-floating layouts)
    #[allow(clippy::too_many_arguments)]
    fn filter_valid_window_ids(
        &self,
        workspace_window_ids: &[u64],
        actual_windows: &[ManagedWindow],
        actual_window_ids: &HashSet<u64>,
        workspace_app_ids: &[String],
        app_window_counts: &HashMap<String, usize>,
        workspace_name: &str,
        layout_mode: &LayoutMode,
    ) -> Vec<u64> {
        workspace_window_ids
            .iter()
            .copied()
            .filter(|id| {
                self.is_valid_layout_window(
                    *id,
                    actual_windows,
                    actual_window_ids,
                    workspace_app_ids,
                    app_window_counts,
                    workspace_name,
                    layout_mode,
                )
            })
            .collect()
    }

    /// Checks if a window is valid for inclusion in the layout.
    #[allow(clippy::too_many_arguments)]
    fn is_valid_layout_window(
        &self,
        window_id: u64,
        actual_windows: &[ManagedWindow],
        actual_window_ids: &HashSet<u64>,
        workspace_app_ids: &[String],
        app_window_counts: &HashMap<String, usize>,
        workspace_name: &str,
        layout_mode: &LayoutMode,
    ) -> bool {
        // First, try to get window info from CGWindowList snapshot
        let window_info = if actual_window_ids.contains(&window_id) {
            actual_windows.iter().find(|w| w.id == window_id)
        } else {
            // Window not in CGWindowList snapshot.
            // This can happen when:
            // 1. Window was just created and macOS hasn't updated CGWindowList yet
            // 2. Window is transitioning during workspace switch (hiding/unhiding)
            // 3. Window is truly stale/destroyed

            // Check if this window is in our managed state (was added via handle_new_window)
            let managed_window = self.workspace_manager.state().get_window(window_id);

            if let Some(win) = managed_window {
                // Window is in our state - trust it if it belongs to this workspace
                if win.workspace == workspace_name {
                    // Validate using managed window info since CGWindowList doesn't have it yet
                    // Skip PiP windows for non-floating layouts
                    if *layout_mode != LayoutMode::Floating && window::is_pip_window(win) {
                        return false;
                    }
                    // Skip helper windows
                    if Self::is_helper_window(win, app_window_counts) {
                        return false;
                    }
                    return true;
                }
            }

            // Not in our state either - window is invalid
            return false;
        };

        let Some(win) = window_info else {
            return false;
        };

        // Check if explicitly assigned or matches rules
        if !self.window_belongs_to_workspace(win, workspace_app_ids, workspace_name) {
            return false;
        }

        // Filter out small helper/splash windows for multi-window apps
        if Self::is_helper_window(win, app_window_counts) {
            return false;
        }

        // Skip PiP windows for non-floating layouts
        if *layout_mode != LayoutMode::Floating && window::is_pip_window(win) {
            return false;
        }

        true
    }

    /// Checks if a window belongs to a workspace (explicitly assigned or matches rules).
    fn window_belongs_to_workspace(
        &self,
        window: &ManagedWindow,
        workspace_app_ids: &[String],
        workspace_name: &str,
    ) -> bool {
        // Check if explicitly assigned
        let is_explicitly_assigned = self
            .workspace_manager
            .state()
            .get_window(window.id)
            .is_some_and(|w| w.workspace == workspace_name);

        // Check if matches workspace rules
        let matches_rules = window.bundle_id.as_ref().is_some_and(|bundle_id| {
            workspace_app_ids
                .iter()
                .any(|app_id| bundle_id.contains(app_id) || bundle_id == app_id)
        });

        is_explicitly_assigned || matches_rules
    }

    /// Checks if a window is a helper/auxiliary window that should be filtered out.
    ///
    /// Heuristics for detecting helper windows:
    /// 1. Known Electron app helper patterns (Teams, Slack, Discord, `VSCode`)
    /// 2. Position (0, 0) with standard default size - likely placeholder
    /// 3. Small windows (< 600x400) with empty/generic titles
    /// 4. Title matches app name exactly for non-main windows
    fn is_helper_window(
        window: &ManagedWindow,
        app_window_counts: &HashMap<String, usize>,
    ) -> bool {
        let Some(ref bundle_id) = window.bundle_id else {
            return false;
        };

        let window_count = app_window_counts.get(bundle_id).copied().unwrap_or(0);
        if window_count <= 1 {
            return false;
        }

        // Heuristic 1: Check known Electron app helper patterns first
        if Self::is_known_electron_helper(window) {
            return true;
        }

        // Heuristic 2: Position (0, 0) with standard default sizes is often a placeholder
        // These are windows that haven't been properly positioned yet
        let is_at_origin = window.frame.x == 0 && window.frame.y == 0;
        let is_default_size = (window.frame.width == 800 && window.frame.height == 600)
            || (window.frame.width == 1024 && window.frame.height == 768);

        if is_at_origin && is_default_size {
            // For multi-window apps, origin windows with default size are placeholders
            return true;
        }

        // Heuristic 3: Small windows (< 600x400)
        let is_small_window = window.frame.width < 600 || window.frame.height < 400;
        if is_small_window {
            // Skip small windows with empty titles
            if window.title.is_empty() {
                return true;
            }
            // Skip small windows where title is just the app name
            if window.title == window.app_name {
                return true;
            }
        }

        // Heuristic 4: Title is exactly the app name (no content-specific title)
        // This often indicates a background/helper window for multi-window apps
        if window.title == window.app_name && !is_small_window {
            // Use AX API to check if this is the main window
            if matches!(Self::is_main_window_via_ax(window), Ok(false)) {
                return true;
            }
        }

        false
    }

    /// Checks if a window matches known Electron helper patterns.
    ///
    /// Electron apps often create hidden/background windows at position (0, 0)
    /// with the app name as the title. These are used for IPC, notifications, etc.
    fn is_known_electron_helper(window: &ManagedWindow) -> bool {
        let Some(ref bundle_id) = window.bundle_id else {
            return false;
        };

        for (pattern, suspicious_title) in Self::ELECTRON_HELPER_PATTERNS {
            if bundle_id.contains(pattern) {
                // Check if it's at origin (0, 0) - common for helper windows
                let is_at_origin = window.frame.x == 0 && window.frame.y == 0;

                // Match by title if pattern specified, or just by origin position
                if suspicious_title.is_empty() {
                    // For generic patterns, only filter if at origin with default size
                    if is_at_origin && (window.frame.width == 800 && window.frame.height == 600) {
                        return true;
                    }
                } else if window.title == *suspicious_title && is_at_origin {
                    return true;
                }
            }
        }
        false
    }

    /// Checks if a window is the main window using Accessibility API.
    ///
    /// Uses the `AXMain` attribute to determine if a window is the main window
    /// of its application. Returns `true` if we can't determine (fail-safe).
    fn is_main_window_via_ax(window: &ManagedWindow) -> Result<bool, TilingError> {
        use crate::tiling::accessibility::AccessibilityElement;

        let app = AccessibilityElement::application(window.pid);
        let ax_windows = app.get_windows()?;

        for ax_window in ax_windows {
            if let Ok(frame) = ax_window.get_frame() {
                // Match by position within a small tolerance
                if (frame.x - window.frame.x).abs() <= 2
                    && (frame.y - window.frame.y).abs() <= 2
                    && frame.width.abs_diff(window.frame.width) <= 2
                    && frame.height.abs_diff(window.frame.height) <= 2
                {
                    // Found matching window, check AXMain attribute
                    return ax_window.get_bool_attribute("AXMain");
                }
            }
        }

        // Assume main if we can't determine (fail-safe)
        Ok(true)
    }

    /// Removes stale windows from the workspace and state.
    fn cleanup_stale_windows(
        &mut self,
        workspace_name: &str,
        workspace_window_ids: &[u64],
        valid_window_ids: &[u64],
    ) {
        // Find stale IDs
        let stale_ids: Vec<u64> = workspace_window_ids
            .iter()
            .copied()
            .filter(|id| !valid_window_ids.contains(id))
            .collect();

        // Remove from windows map
        for stale_id in stale_ids {
            self.workspace_manager.state_mut().windows.remove(&stale_id);
        }

        // Update workspace's window list
        let valid_ids = valid_window_ids.to_vec();
        if let Some(ws) = self.workspace_manager.state_mut().get_workspace_mut(workspace_name) {
            ws.windows.retain(|id| valid_ids.contains(id));
        }
    }

    /// Builds the layout context for a workspace.
    fn build_layout_context(
        &self,
        workspace_name: &str,
        screen_id: &str,
    ) -> Result<layout::LayoutContext, TilingError> {
        let screen_count = self.workspace_manager.state().screens.len();
        let screen = self
            .workspace_manager
            .state()
            .get_screen(screen_id)
            .ok_or_else(|| TilingError::ScreenNotFound(screen_id.to_string()))?
            .clone();

        let split_ratios = self
            .workspace_manager
            .state()
            .get_workspace(workspace_name)
            .map(|ws| ws.split_ratios.to_vec())
            .unwrap_or_default();

        let gaps = ResolvedGaps::from_config(&self.config.gaps, &screen, screen_count);

        Ok(layout::LayoutContext {
            screen_frame: screen.usable_frame,
            gaps,
            split_ratios,
        })
    }

    /// Builds layout window objects from window IDs.
    fn build_layout_windows(&self, window_ids: &[u64]) -> Vec<layout::LayoutWindow> {
        window_ids
            .iter()
            .filter_map(|&id| {
                let managed_win = self.workspace_manager.state().get_window(id)?;
                Some(layout::LayoutWindow {
                    id: managed_win.id,
                    is_floating: managed_win.is_floating,
                    is_minimized: managed_win.is_minimized,
                    is_fullscreen: managed_win.is_fullscreen,
                })
            })
            .collect()
    }

    /// Computes window layouts based on the layout mode.
    fn compute_layouts(
        &self,
        layout_mode: &LayoutMode,
        layout_windows: &[layout::LayoutWindow],
        context: &layout::LayoutContext,
    ) -> Result<Vec<layout::WindowLayout>, TilingError> {
        match layout_mode {
            LayoutMode::Monocle => {
                let monocle = layout::MonocleLayout::new();
                monocle.layout(layout_windows, context)
            }
            // TODO: Implement scrolling layout (currently falls back to tiling)
            LayoutMode::Tiling | LayoutMode::Scrolling => {
                let tiling = layout::TilingLayout::new();
                tiling.layout(layout_windows, context)
            }
            LayoutMode::Split => {
                let split = layout::SplitLayout::auto();
                split.layout(layout_windows, context)
            }
            LayoutMode::SplitVertical => {
                let split = layout::SplitLayout::vertical();
                split.layout(layout_windows, context)
            }
            LayoutMode::SplitHorizontal => {
                let split = layout::SplitLayout::horizontal();
                split.layout(layout_windows, context)
            }
            LayoutMode::Master => {
                let master = layout::MasterLayout::new(self.config.master.clone());
                master.layout(layout_windows, context)
            }
            LayoutMode::Floating => {
                let floating = layout::FloatingLayout::new();
                floating.layout(layout_windows, context)
            }
        }
    }

    /// Applies computed layouts to windows (with animation if enabled).
    fn apply_window_layouts(layouts: &[layout::WindowLayout]) {
        if crate::tiling::animation::is_enabled() {
            let targets: Vec<(u64, crate::tiling::state::WindowFrame)> =
                layouts.iter().map(|wl| (wl.id, wl.frame)).collect();
            crate::tiling::animation::animate_windows(targets);
        } else {
            super::mark_layout_applied();
            for window_layout in layouts {
                if let Err(e) = window::set_window_frame(window_layout.id, &window_layout.frame) {
                    eprintln!(
                        "barba: failed to set window frame for {}: {}",
                        window_layout.id, e
                    );
                }
            }
        }
    }

    /// Sets the layout for a workspace and re-applies it.
    pub fn set_workspace_layout(
        &mut self,
        workspace_name: &str,
        layout_mode: LayoutMode,
    ) -> Result<(), TilingError> {
        // Update the workspace's layout
        {
            let workspace = self
                .workspace_manager
                .state_mut()
                .get_workspace_mut(workspace_name)
                .ok_or_else(|| TilingError::WorkspaceNotFound(workspace_name.to_string()))?;

            // When switching FROM Floating to any other layout, reset is_floating on all windows
            // in this workspace so they participate in the new layout
            let was_floating = workspace.layout == LayoutMode::Floating;
            let is_now_floating = layout_mode == LayoutMode::Floating;

            workspace.layout = layout_mode;
            // Reset split ratios when layout changes
            workspace.split_ratios.clear();

            // Get window IDs for the workspace
            let window_ids: Vec<u64> = workspace.windows.to_vec();

            // If switching from Floating to a tiled layout, reset is_floating flags
            if was_floating && !is_now_floating {
                for window_id in window_ids {
                    if let Some(win) =
                        self.workspace_manager.state_mut().windows.get_mut(&window_id)
                    {
                        win.is_floating = false;
                    }
                }
            }
        }

        // Emit workspaces changed event for layout change
        self.emit_workspaces_changed();

        // Re-apply the layout
        self.apply_layout(workspace_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiling::state::ScreenFrame;

    fn make_layout_window(id: u64) -> layout::LayoutWindow {
        layout::LayoutWindow {
            id,
            is_floating: false,
            is_minimized: false,
            is_fullscreen: false,
        }
    }

    fn make_context() -> layout::LayoutContext {
        layout::LayoutContext {
            screen_frame: ScreenFrame {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
            gaps: layout::ResolvedGaps::default(),
            split_ratios: vec![],
        }
    }

    #[test]
    fn test_compute_layouts_static_monocle() {
        let windows = vec![make_layout_window(1), make_layout_window(2)];
        let context = make_context();
        let master_config = barba_shared::MasterConfig::default();

        let result = TilingManager::compute_layouts_static(
            &LayoutMode::Monocle,
            &windows,
            &context,
            &master_config,
        );

        assert!(result.is_ok());
        let layouts = result.unwrap();
        // Monocle: all windows should have the same (full screen) frame
        assert_eq!(layouts.len(), 2);
        assert_eq!(layouts[0].frame.width, 1920);
        assert_eq!(layouts[0].frame.height, 1080);
        assert_eq!(layouts[1].frame.width, 1920);
        assert_eq!(layouts[1].frame.height, 1080);
    }

    #[test]
    fn test_compute_layouts_static_tiling() {
        let windows = vec![make_layout_window(1), make_layout_window(2)];
        let context = make_context();
        let master_config = barba_shared::MasterConfig::default();

        let result = TilingManager::compute_layouts_static(
            &LayoutMode::Tiling,
            &windows,
            &context,
            &master_config,
        );

        assert!(result.is_ok());
        let layouts = result.unwrap();
        // Tiling with 2 windows: side by side (each 960 wide)
        assert_eq!(layouts.len(), 2);
        assert_eq!(layouts[0].frame.width, 960);
        assert_eq!(layouts[1].frame.width, 960);
    }

    #[test]
    fn test_compute_layouts_static_master() {
        let windows = vec![
            make_layout_window(1),
            make_layout_window(2),
            make_layout_window(3),
        ];
        let context = make_context();
        let master_config = barba_shared::MasterConfig { ratio: 70, max_masters: 1 };

        let result = TilingManager::compute_layouts_static(
            &LayoutMode::Master,
            &windows,
            &context,
            &master_config,
        );

        assert!(result.is_ok());
        let layouts = result.unwrap();
        assert_eq!(layouts.len(), 3);
        // Master window should be 70% of width
        let expected_master_width = (1920.0 * 0.7) as u32;
        assert!((layouts[0].frame.width as i32 - expected_master_width as i32).abs() <= 1);
    }

    #[test]
    fn test_compute_layouts_static_split_vertical() {
        let windows = vec![make_layout_window(1), make_layout_window(2)];
        let context = make_context();
        let master_config = barba_shared::MasterConfig::default();

        let result = TilingManager::compute_layouts_static(
            &LayoutMode::SplitVertical,
            &windows,
            &context,
            &master_config,
        );

        assert!(result.is_ok());
        let layouts = result.unwrap();
        assert_eq!(layouts.len(), 2);
        // SplitVertical: windows side by side (left/right), half width each, full height
        assert_eq!(layouts[0].frame.width, 960);
        assert_eq!(layouts[1].frame.width, 960);
        assert_eq!(layouts[0].frame.height, 1080);
        assert_eq!(layouts[1].frame.height, 1080);
    }

    #[test]
    fn test_compute_layouts_static_split_horizontal() {
        let windows = vec![make_layout_window(1), make_layout_window(2)];
        let context = make_context();
        let master_config = barba_shared::MasterConfig::default();

        let result = TilingManager::compute_layouts_static(
            &LayoutMode::SplitHorizontal,
            &windows,
            &context,
            &master_config,
        );

        assert!(result.is_ok());
        let layouts = result.unwrap();
        assert_eq!(layouts.len(), 2);
        // SplitHorizontal: windows stacked (top/bottom), full width, half height each
        assert_eq!(layouts[0].frame.width, 1920);
        assert_eq!(layouts[1].frame.width, 1920);
        assert_eq!(layouts[0].frame.height, 540);
        assert_eq!(layouts[1].frame.height, 540);
    }

    #[test]
    fn test_compute_layouts_static_floating() {
        let windows = vec![make_layout_window(1), make_layout_window(2)];
        let context = make_context();
        let master_config = barba_shared::MasterConfig::default();

        let result = TilingManager::compute_layouts_static(
            &LayoutMode::Floating,
            &windows,
            &context,
            &master_config,
        );

        assert!(result.is_ok());
        let layouts = result.unwrap();
        // Floating layout returns empty layouts (windows are not repositioned)
        assert!(layouts.is_empty());
    }

    #[test]
    fn test_compute_layouts_static_empty_windows() {
        let windows: Vec<layout::LayoutWindow> = vec![];
        let context = make_context();
        let master_config = barba_shared::MasterConfig::default();

        // All layout modes should handle empty windows gracefully
        for mode in [
            LayoutMode::Monocle,
            LayoutMode::Tiling,
            LayoutMode::Split,
            LayoutMode::Master,
            LayoutMode::Floating,
        ] {
            let result =
                TilingManager::compute_layouts_static(&mode, &windows, &context, &master_config);
            assert!(
                result.is_ok(),
                "Layout mode {:?} should handle empty windows",
                mode
            );
            assert!(result.unwrap().is_empty());
        }
    }
}
