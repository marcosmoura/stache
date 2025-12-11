//! Window focus, move, and send operations.
//!
//! This module handles operations on individual windows such as
//! focusing in a direction, swapping positions, and sending to workspaces/screens.

#![allow(clippy::assigning_clones)]
#![allow(clippy::cast_possible_wrap)]

use super::TilingManager;
use crate::tiling::error::TilingError;
use crate::tiling::state::ManagedWindow;
use crate::tiling::window;

/// Information needed for resize calculations.
struct ResizeInfo {
    layout_mode: barba_shared::LayoutMode,
    window_index: usize,
    window_count: usize,
    screen_width: u32,
    screen_height: u32,
}

impl TilingManager {
    /// Gets the focused window and ensures it's tracked in a workspace.
    /// Returns the window and its workspace name.
    pub(super) fn get_focused_window_and_workspace(
        &mut self,
    ) -> Result<(ManagedWindow, String), TilingError> {
        let focused_window = window::get_focused_window()?;

        // Try to find existing workspace for this window
        let existing_workspace = {
            let state = self.workspace_manager.state();
            state
                .workspaces
                .iter()
                .find(|ws| ws.windows.contains(&focused_window.id))
                .map(|ws| ws.name.clone())
        };

        if let Some(ws_name) = existing_workspace {
            return Ok((focused_window, ws_name));
        }

        // Window not in any workspace - add it now
        let workspace_name = self.find_workspace_for_window(&focused_window).ok_or_else(|| {
            TilingError::OperationFailed("Could not find workspace for window".to_string())
        })?;

        // Add window to state
        let window_id = focused_window.id;
        self.workspace_manager
            .state_mut()
            .windows
            .insert(window_id, focused_window.clone());

        // Add to workspace
        if let Some(ws) = self.workspace_manager.state_mut().get_workspace_mut(&workspace_name)
            && !ws.windows.contains(&window_id)
        {
            ws.windows.push(window_id);
        }

        Ok((focused_window, workspace_name))
    }

    /// Focuses a window in the given direction relative to the currently focused window.
    ///
    /// Direction can be: left, right, up, down, next, previous
    ///
    /// For left/right/up/down: finds the closest window in that geometric direction.
    /// For next/previous: cycles through windows in layout order.
    pub fn focus_window_in_direction(&mut self, direction: &str) -> Result<(), TilingError> {
        // Get the currently focused window and ensure it's tracked
        let (focused_window, workspace_name) = self.get_focused_window_and_workspace()?;

        // Ensure all visible windows in this workspace are tracked.
        // This handles windows that were never focused since app startup.
        // Required for ALL directions so find_window_in_direction can locate neighbors.
        self.ensure_workspace_windows_tracked(&workspace_name);

        // Get the window list for this workspace
        let window_ids = {
            let workspace = self
                .workspace_manager
                .state()
                .get_workspace(&workspace_name)
                .ok_or_else(|| TilingError::WorkspaceNotFound(workspace_name.clone()))?;
            workspace.windows.clone()
        };

        if window_ids.len() < 2 {
            return Ok(()); // Nothing to focus if only one window
        }

        match direction {
            "next" | "previous" => {
                // Filter out PiP windows from the cycle - they should never be focused
                let focusable_ids: Vec<u64> = {
                    let state = self.workspace_manager.state();
                    window_ids
                        .iter()
                        .filter(|&&id| {
                            state.get_window(id).is_none_or(|w| !window::is_pip_window(w))
                        })
                        .copied()
                        .collect()
                };

                if focusable_ids.len() < 2 {
                    return Ok(()); // Nothing to cycle if only one focusable window
                }

                // Always cycle through ALL focusable windows in the workspace in layout order.
                // This ensures focus moves between different apps, not just within the same app.
                let current_index =
                    focusable_ids.iter().position(|&id| id == focused_window.id).unwrap_or(0);
                let new_index = if direction == "next" {
                    (current_index + 1) % focusable_ids.len()
                } else {
                    (current_index + focusable_ids.len() - 1) % focusable_ids.len()
                };
                let target_id = focusable_ids[new_index];

                // Get fresh window info for the target window to focus it properly
                if let Ok(target_window) = window::get_window_by_id(target_id) {
                    window::focus_window_fast(&target_window)?;
                } else {
                    window::focus_window(target_id)?;
                }
            }
            "left" | "right" | "up" | "down" => {
                let target_id = self
                    .find_window_in_direction(&workspace_name, focused_window.id, direction)?
                    .ok_or_else(|| {
                        TilingError::OperationFailed(format!("No window in direction {direction}"))
                    })?;
                window::focus_window(target_id)?;
            }
            _ => {
                return Err(TilingError::OperationFailed(format!(
                    "Invalid direction: {direction}"
                )));
            }
        }

        Ok(())
    }

    /// Ensures all visible windows that belong to the given workspace are tracked.
    ///
    /// This is used before cycling through windows to ensure we include windows
    /// that haven't been focused since app startup.
    fn ensure_workspace_windows_tracked(&mut self, workspace_name: &str) {
        // Get all visible windows from the system
        let Ok(all_windows) = window::get_all_windows() else {
            return;
        };

        for win in all_windows {
            // Skip dialogs and sheets
            if window::is_dialog_or_sheet(&win) {
                continue;
            }

            // Skip windows that match ignore rules (higher priority than workspace rules)
            if self.should_ignore_window(&win) {
                continue;
            }

            // Skip if already tracked
            if self.workspace_manager.state().windows.contains_key(&win.id) {
                continue;
            }

            // Check if this window belongs to the target workspace
            if let Some(win_workspace) = self.find_workspace_for_window(&win)
                && win_workspace == workspace_name
            {
                // Add to state and workspace
                let window_id = win.id;
                let mut win = win;
                win.workspace = workspace_name.to_string();
                self.workspace_manager.state_mut().windows.insert(window_id, win);

                if let Some(ws) =
                    self.workspace_manager.state_mut().get_workspace_mut(workspace_name)
                    && !ws.windows.contains(&window_id)
                {
                    ws.windows.push(window_id);
                }
            }
        }
    }

    /// Finds a window in the given direction relative to a source window.
    fn find_window_in_direction(
        &self,
        workspace_name: &str,
        source_id: u64,
        direction: &str,
    ) -> Result<Option<u64>, TilingError> {
        let state = self.workspace_manager.state();
        let workspace = state
            .get_workspace(workspace_name)
            .ok_or_else(|| TilingError::WorkspaceNotFound(workspace_name.to_string()))?;

        // Get fresh window positions from the system
        let actual_windows = window::get_all_windows().unwrap_or_default();

        let source_window = actual_windows
            .iter()
            .find(|w| w.id == source_id)
            .ok_or(TilingError::WindowNotFound(source_id))?;

        let source_center_x = source_window.frame.x + source_window.frame.width as i32 / 2;
        let source_center_y = source_window.frame.y + source_window.frame.height as i32 / 2;

        let mut best_match: Option<(u64, i32)> = None;

        for &window_id in &workspace.windows {
            if window_id == source_id {
                continue;
            }

            // Get the actual window position from the system.
            // If the window is in actual_windows (from get_all_windows), it's visible.
            // get_all_windows already filters out hidden windows, so no need to check
            // our potentially stale state for hidden/minimized status.
            let Some(win) = actual_windows.iter().find(|w| w.id == window_id) else {
                continue;
            };

            let window_center_x = win.frame.x + win.frame.width as i32 / 2;
            let window_center_y = win.frame.y + win.frame.height as i32 / 2;

            let is_valid = match direction {
                "left" => window_center_x < source_center_x,
                "right" => window_center_x > source_center_x,
                "up" => window_center_y < source_center_y,
                "down" => window_center_y > source_center_y,
                _ => false,
            };

            if !is_valid {
                continue;
            }

            // Calculate distance
            let distance = (window_center_x - source_center_x).abs()
                + (window_center_y - source_center_y).abs();

            if best_match.is_none() || distance < best_match.unwrap().1 {
                best_match = Some((window_id, distance));
            }
        }

        Ok(best_match.map(|(id, _)| id))
    }

    /// Swaps the focused window with the window in the given direction.
    ///
    /// Direction can be: left, right, up, down, next, previous
    pub fn swap_window_in_direction(&mut self, direction: &str) -> Result<(), TilingError> {
        // Get the currently focused window and ensure it's tracked
        let (focused_window, workspace_name) = self.get_focused_window_and_workspace()?;

        // Ensure all visible windows in this workspace are tracked.
        // This handles windows that were never focused since app startup.
        // Required for ALL directions so find_window_in_direction can locate neighbors.
        self.ensure_workspace_windows_tracked(&workspace_name);

        // Find the target window
        let target_id = match direction {
            "next" | "previous" => {
                let workspace = self
                    .workspace_manager
                    .state()
                    .get_workspace(&workspace_name)
                    .ok_or_else(|| TilingError::WorkspaceNotFound(workspace_name.clone()))?;
                let window_ids = &workspace.windows;

                if window_ids.len() < 2 {
                    return Ok(()); // Nothing to swap if only one window
                }

                let current_index =
                    window_ids.iter().position(|&id| id == focused_window.id).unwrap_or(0);
                let new_index = if direction == "next" {
                    (current_index + 1) % window_ids.len()
                } else {
                    (current_index + window_ids.len() - 1) % window_ids.len()
                };
                window_ids[new_index]
            }
            "left" | "right" | "up" | "down" => self
                .find_window_in_direction(&workspace_name, focused_window.id, direction)?
                .ok_or_else(|| {
                    TilingError::OperationFailed(format!("No window in direction {direction}"))
                })?,
            _ => {
                return Err(TilingError::OperationFailed(format!(
                    "Invalid direction: {direction}"
                )));
            }
        };

        // Swap the windows in the workspace's window list
        let source_window_id = focused_window.id;
        let are_same_app = {
            if let Ok(target_window) = window::get_window_by_id(target_id) {
                target_window.pid == focused_window.pid
            } else {
                false
            }
        };

        {
            let workspace =
                self.workspace_manager
                    .state_mut()
                    .get_workspace_mut(&workspace_name)
                    .ok_or_else(|| TilingError::WorkspaceNotFound(workspace_name.clone()))?;

            let source_idx = workspace.windows.iter().position(|&id| id == source_window_id);
            let target_idx = workspace.windows.iter().position(|&id| id == target_id);

            if let (Some(s), Some(t)) = (source_idx, target_idx) {
                workspace.windows.swap(s, t);
            }
        }

        // Re-apply the layout
        self.apply_layout(&workspace_name)?;

        // For same-app windows, add a small delay to let the animation settle
        // before refocusing. This prevents flickering caused by position-based
        // AX element matching getting confused during rapid position changes.
        if are_same_app {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        // Get fresh window info after layout application and focus by window ID
        // This ensures we focus the correct window even after positions changed
        window::focus_window(source_window_id)?;

        Ok(())
    }

    /// Sends the focused window to a screen (moving it to that screen's focused workspace).
    pub fn send_window_to_screen(&mut self, target: &str) -> Result<(), TilingError> {
        // Get the currently focused window and ensure it's tracked
        let (focused_window, current_workspace_name) = self.get_focused_window_and_workspace()?;

        let current_screen_id = {
            let workspace = self
                .workspace_manager
                .state()
                .get_workspace(&current_workspace_name)
                .ok_or_else(|| {
                TilingError::WorkspaceNotFound(current_workspace_name.clone())
            })?;
            workspace.screen.clone()
        };

        // Resolve the target screen
        let target_screen_id = self
            .workspace_manager
            .state()
            .resolve_screen_target(target, Some(&current_screen_id))
            .ok_or_else(|| TilingError::ScreenNotFound(target.to_string()))?;

        // Don't do anything if already on the target screen
        if target_screen_id == current_screen_id {
            return Ok(());
        }

        // Get the focused workspace on the target screen
        let target_workspace_name = self
            .workspace_manager
            .state()
            .focused_workspace_per_screen
            .get(&target_screen_id)
            .cloned()
            .ok_or_else(|| {
                TilingError::WorkspaceNotFound(format!("no workspace on screen {target_screen_id}"))
            })?;

        // Remove window from current workspace
        {
            let workspace = self
                .workspace_manager
                .state_mut()
                .get_workspace_mut(&current_workspace_name)
                .ok_or_else(|| TilingError::WorkspaceNotFound(current_workspace_name.clone()))?;
            workspace.windows.retain(|id| *id != focused_window.id);
        }

        // Add window to target workspace
        {
            let workspace = self
                .workspace_manager
                .state_mut()
                .get_workspace_mut(&target_workspace_name)
                .ok_or_else(|| TilingError::WorkspaceNotFound(target_workspace_name.clone()))?;
            workspace.windows.push(focused_window.id);
        }

        // Update the window's workspace in state
        if let Some(win) = self.workspace_manager.state_mut().get_window_mut(focused_window.id) {
            win.workspace = target_workspace_name.clone();
        }

        // Re-apply layouts for both workspaces
        self.apply_layout(&current_workspace_name)?;
        self.apply_layout(&target_workspace_name)?;

        // Keep focus on the moved window
        window::focus_window_fast(&focused_window)?;

        Ok(())
    }

    /// Sends the focused window to a specific workspace with options.
    ///
    /// If `focus_after` is true, switches to the target workspace and focuses the window.
    pub fn send_window_to_workspace_with_options(
        &mut self,
        target_workspace: &str,
        focus_after: bool,
    ) -> Result<(), TilingError> {
        // Check target workspace exists
        let target_ws_info = self
            .workspace_manager
            .state()
            .get_workspace(target_workspace)
            .ok_or_else(|| TilingError::WorkspaceNotFound(target_workspace.to_string()))?;
        let target_screen_id = target_ws_info.screen.clone();

        // Get the currently focused window and ensure it's tracked
        let (focused_window, current_workspace_name) = self.get_focused_window_and_workspace()?;

        // Don't do anything if already on the target workspace
        if current_workspace_name == target_workspace {
            return Ok(());
        }

        let current_screen_id = {
            let ws = self
                .workspace_manager
                .state()
                .get_workspace(&current_workspace_name)
                .ok_or_else(|| TilingError::WorkspaceNotFound(current_workspace_name.clone()))?;
            ws.screen.clone()
        };

        // Check if target workspace is currently focused on its screen
        let is_target_focused = self
            .workspace_manager
            .state()
            .focused_workspace_per_screen
            .get(&target_screen_id)
            .is_some_and(|focused| focused == target_workspace);

        // Remove from current workspace
        {
            let workspace = self
                .workspace_manager
                .state_mut()
                .get_workspace_mut(&current_workspace_name)
                .ok_or_else(|| TilingError::WorkspaceNotFound(current_workspace_name.clone()))?;
            workspace.windows.retain(|id| *id != focused_window.id);
        }

        // Add to target workspace
        {
            let workspace = self
                .workspace_manager
                .state_mut()
                .get_workspace_mut(target_workspace)
                .ok_or_else(|| TilingError::WorkspaceNotFound(target_workspace.to_string()))?;
            workspace.windows.push(focused_window.id);
        }

        // Update the window's workspace in state
        if let Some(win) = self.workspace_manager.state_mut().get_window_mut(focused_window.id) {
            win.workspace = target_workspace.to_string();
        }

        // If focus_after is set, switch to the target workspace and focus the window
        if focus_after {
            // Show the window (in case it was going to be hidden)
            let _ = window::unhide_app(focused_window.pid);
            if let Some(win) = self.workspace_manager.state_mut().get_window_mut(focused_window.id)
            {
                win.is_hidden = false;
            }

            // Re-apply layout for current workspace (window was removed)
            self.apply_layout(&current_workspace_name)?;

            // Switch to target workspace (this will apply layout and focus)
            self.switch_workspace(target_workspace)?;

            // Focus the window we just sent
            window::focus_window_fast(&focused_window)?;
        } else {
            // If target workspace is not focused, hide the window
            // But don't hide apps that have PiP windows
            if !is_target_focused && !window::is_pip_window(&focused_window) {
                let _ = window::hide_app(focused_window.pid);
                if let Some(win) =
                    self.workspace_manager.state_mut().get_window_mut(focused_window.id)
                {
                    win.is_hidden = true;
                }
            }

            // Re-apply layout for current workspace (window was removed)
            self.apply_layout(&current_workspace_name)?;

            // Apply target layout - this will position the window on the target screen
            if is_target_focused {
                self.apply_layout(target_workspace)?;
            }
        }

        // Suppress unused variable warning
        let _ = current_screen_id;

        Ok(())
    }

    /// Resizes the focused window by adjusting split ratios.
    ///
    /// For tiled layouts, this adjusts the split ratio at the window's position
    /// in the layout tree. The `dimension` specifies whether to resize width or height,
    /// and `delta_pixels` is the amount to change in pixels (positive to grow, negative to shrink).
    ///
    /// Returns an error if:
    /// - No window is focused
    /// - The focused window is not in a tiled layout
    /// - The window is in a monocle layout (no resizing possible)
    #[allow(clippy::cast_precision_loss)]
    pub fn resize_focused_window(
        &mut self,
        dimension: &str,
        delta_pixels: i32,
    ) -> Result<(), TilingError> {
        // Get the currently focused window and its workspace
        let (focused_window, workspace_name) = self.get_focused_window_and_workspace()?;

        // Check if the window is individually floating - these can only be resized via presets
        if let Some(managed_win) = self.workspace_manager.state().get_window(focused_window.id)
            && managed_win.is_floating
        {
            // Floating windows cannot be resized via resize commands, only via presets
            return Ok(());
        }

        // Get workspace info
        let (layout_mode, window_index, window_count, screen_id, screen_width, screen_height) = {
            let state = self.workspace_manager.state();
            let workspace = state
                .get_workspace(&workspace_name)
                .ok_or_else(|| TilingError::WorkspaceNotFound(workspace_name.clone()))?;

            let index = workspace
                .windows
                .iter()
                .position(|&id| id == focused_window.id)
                .ok_or(TilingError::WindowNotFound(focused_window.id))?;

            let screen = state
                .screens
                .iter()
                .find(|s| s.id == workspace.screen)
                .ok_or_else(|| TilingError::ScreenNotFound(workspace.screen.clone()))?;

            (
                workspace.layout.clone(),
                index,
                workspace.windows.len(),
                workspace.screen.clone(),
                screen.usable_frame.width,
                screen.usable_frame.height,
            )
        };

        // Only allow resizing in tiled layouts
        match layout_mode {
            barba_shared::LayoutMode::Monocle | barba_shared::LayoutMode::Floating => {
                // Monocle and floating windows can't be resized via resize commands
                return Ok(());
            }
            barba_shared::LayoutMode::Master => {
                // Master layout: only width resize affects master/stack ratio
                return self.resize_master_layout(
                    &workspace_name,
                    window_index,
                    dimension,
                    delta_pixels,
                    screen_width,
                );
            }
            barba_shared::LayoutMode::Tiling
            | barba_shared::LayoutMode::Split
            | barba_shared::LayoutMode::SplitVertical
            | barba_shared::LayoutMode::SplitHorizontal
            | barba_shared::LayoutMode::Scrolling => {
                // These layouts support split ratio resizing via dwindle algorithm
            }
        }

        // Can't resize if there's only one window
        if window_count < 2 {
            return Ok(());
        }

        // Determine the initial split direction based on screen aspect ratio
        // (same logic as in TilingLayout)
        let is_landscape = screen_width >= screen_height;

        // In dwindle layout, splits alternate direction at each level:
        // - Landscape screens start horizontal (width), then vertical (height), etc.
        // - Portrait screens start vertical (height), then horizontal (width), etc.
        //
        // To resize width, we need to find a split that affects width (horizontal split).
        // To resize height, we need to find a split that affects height (vertical split).
        //
        // For window at index N, it's affected by splits at indices 0..N
        // We need to find the appropriate split based on the dimension.

        // Determine which split affects the requested dimension
        // Start from the window's level and work backwards to find a matching split
        let target_is_horizontal = dimension == "width";

        // Find the split index that corresponds to the requested dimension
        // The split at depth D is horizontal if (is_landscape XOR (D % 2 == 1))
        let mut ratio_index: Option<usize> = None;

        // Check splits from the window's position backwards
        let max_depth = if window_index == 0 { 0 } else { window_index };
        for depth in (0..=max_depth).rev() {
            // Determine if this split is horizontal or vertical
            let split_is_horizontal = if is_landscape {
                depth % 2 == 0 // Even depths are horizontal on landscape
            } else {
                depth % 2 == 1 // Odd depths are horizontal on portrait
            };

            if split_is_horizontal == target_is_horizontal {
                ratio_index = Some(depth);
                break;
            }
        }

        let Some(ratio_index) = ratio_index else {
            // No matching split found - this can happen with single window or edge cases
            return Ok(());
        };

        // Get screen dimension for ratio calculation
        let screen_dimension = if target_is_horizontal {
            screen_width
        } else {
            screen_height
        };

        // Convert pixel delta to ratio delta
        // A ratio of 1.0 corresponds to the full screen dimension
        let ratio_delta = f64::from(delta_pixels) / f64::from(screen_dimension);

        // Update the split ratio
        {
            let workspace =
                self.workspace_manager
                    .state_mut()
                    .get_workspace_mut(&workspace_name)
                    .ok_or_else(|| TilingError::WorkspaceNotFound(workspace_name.clone()))?;

            // Ensure split_ratios vector is large enough
            while workspace.split_ratios.len() <= ratio_index {
                workspace.split_ratios.push(0.5); // Default 50/50 split
            }

            // Adjust the ratio, clamping to valid range [0.1, 0.9]
            let current_ratio = workspace.split_ratios[ratio_index];
            let new_ratio = (current_ratio + ratio_delta).clamp(0.1, 0.9);
            workspace.split_ratios[ratio_index] = new_ratio;
        }

        // Suppress unused variable warning
        let _ = screen_id;

        // Re-apply the layout with new ratios
        self.apply_layout(&workspace_name)
    }

    /// Resizes the master layout by adjusting the master/stack split ratio.
    /// For width: adjusts the horizontal split between master and stack.
    /// For height: currently no effect (stack windows share height equally).
    #[allow(clippy::cast_precision_loss)]
    fn resize_master_layout(
        &mut self,
        workspace_name: &str,
        window_index: usize,
        dimension: &str,
        delta_pixels: i32,
        screen_width: u32,
    ) -> Result<(), TilingError> {
        // In master layout, only width resizing makes sense
        // It adjusts the master/stack horizontal split ratio
        if dimension != "width" {
            // Height resizing doesn't apply to master layout
            // (stack windows share height equally, can't resize individually)
            return Ok(());
        }

        // Get the current split ratios
        // If no split_ratios exist, use the config's master ratio as the starting point
        let current_ratio = {
            let state = self.workspace_manager.state();
            let workspace = state
                .get_workspace(workspace_name)
                .ok_or_else(|| TilingError::WorkspaceNotFound(workspace_name.to_string()))?;
            workspace.split_ratios.first().copied().unwrap_or_else(|| {
                // Convert config ratio (0-100) to 0.0-1.0 format
                f64::from(self.config.master.ratio) / 100.0
            })
        };

        // Determine if this window is the master (index 0) or a stack window
        // Master window: positive delta = grow master (increase ratio)
        // Stack window: positive delta = grow stack (decrease ratio)
        let is_master = window_index == 0;

        // Calculate the ratio delta based on pixel change relative to screen width
        let ratio_delta = f64::from(delta_pixels) / f64::from(screen_width);

        // Adjust ratio based on which window is being resized
        let new_ratio = if is_master {
            current_ratio + ratio_delta
        } else {
            current_ratio - ratio_delta
        };

        // Clamp the ratio to reasonable bounds (10% to 90%)
        let clamped_ratio = new_ratio.clamp(0.1, 0.9);

        // Update the split ratios in the workspace
        {
            let state = self.workspace_manager.state_mut();
            let workspace = state
                .get_workspace_mut(workspace_name)
                .ok_or_else(|| TilingError::WorkspaceNotFound(workspace_name.to_string()))?;

            if workspace.split_ratios.is_empty() {
                workspace.split_ratios.push(clamped_ratio);
            } else {
                workspace.split_ratios[0] = clamped_ratio;
            }
        }

        // Re-apply the layout with new ratios
        self.apply_layout(workspace_name)
    }

    /// Handles a user-initiated window move by snapping the window back to its tiled position.
    ///
    /// This is called when the observer detects a window move. For windows in tiling layouts
    /// that are not floating, the window will be snapped back to its assigned position.
    ///
    /// For floating windows, this is a no-op. Monocle and tiling layouts snap back.
    pub fn handle_window_moved(&mut self, window_id: u64) -> Result<(), TilingError> {
        // Find the workspace containing this window
        let workspace_name = {
            let state = self.workspace_manager.state();
            state
                .workspaces
                .iter()
                .find(|ws| ws.windows.contains(&window_id))
                .map(|ws| ws.name.clone())
        };

        let Some(workspace_name) = workspace_name else {
            // Window not tracked - nothing to do
            return Ok(());
        };

        // Check if window is floating or in floating layout (only layout that allows free movement)
        let should_snap = {
            let state = self.workspace_manager.state();

            // Check if the window itself is floating
            if let Some(window) = state.get_window(window_id)
                && window.is_floating
            {
                return Ok(()); // Floating windows can move freely
            }

            // Check the workspace layout - only Floating layout allows free movement
            // Monocle windows should snap back to fullscreen position
            if let Some(workspace) = state.get_workspace(&workspace_name) {
                match workspace.layout {
                    barba_shared::LayoutMode::Floating => {
                        return Ok(()); // Only floating layout allows free movement
                    }
                    _ => true, // All other layouts (including Monocle) need to snap back
                }
            } else {
                false
            }
        };

        if should_snap {
            // Re-apply the layout to snap the window back to its position
            self.apply_layout(&workspace_name)?;
        }

        Ok(())
    }

    /// Handles a user-initiated window resize by updating split ratios.
    ///
    /// This is called when the event watcher detects a window resize that occurred
    /// outside of the layout cooldown period (i.e., the user resized via mouse).
    ///
    /// For tiled layouts, this calculates the new split ratio based on the window's
    /// new size and updates the workspace's `split_ratios` accordingly.
    ///
    /// For floating and monocle layouts, this is a no-op.
    #[allow(clippy::cast_precision_loss)]
    pub fn handle_user_resize(
        &mut self,
        window_id: u64,
        new_width: u32,
        new_height: u32,
    ) -> Result<(), TilingError> {
        // Find the workspace containing this window
        let Some(workspace_name) = self.find_workspace_for_window_id(window_id) else {
            return Ok(());
        };

        // Get workspace info needed for resize handling
        let Some(resize_info) = self.get_resize_info(&workspace_name, window_id)? else {
            return Ok(());
        };

        // Skip for layouts that don't use split ratios
        match resize_info.layout_mode {
            barba_shared::LayoutMode::Monocle | barba_shared::LayoutMode::Floating => {
                return Ok(());
            }
            barba_shared::LayoutMode::Master => {
                return self.handle_user_resize_master(
                    &workspace_name,
                    window_id,
                    resize_info.window_index,
                    new_width,
                    resize_info.screen_width,
                );
            }
            barba_shared::LayoutMode::Tiling
            | barba_shared::LayoutMode::Split
            | barba_shared::LayoutMode::SplitVertical
            | barba_shared::LayoutMode::SplitHorizontal
            | barba_shared::LayoutMode::Scrolling => {}
        }

        // Can't have split ratios with only one window
        if resize_info.window_count < 2 {
            return Ok(());
        }

        // Calculate and apply the new split ratio
        self.apply_dwindle_resize(&workspace_name, &resize_info, new_width, new_height)
    }

    /// Finds the workspace name containing a window by ID.
    fn find_workspace_for_window_id(&self, window_id: u64) -> Option<String> {
        self.workspace_manager
            .state()
            .workspaces
            .iter()
            .find(|ws| ws.windows.contains(&window_id))
            .map(|ws| ws.name.clone())
    }

    /// Gets the resize information for a window in a workspace.
    fn get_resize_info(
        &self,
        workspace_name: &str,
        window_id: u64,
    ) -> Result<Option<ResizeInfo>, TilingError> {
        let state = self.workspace_manager.state();
        let workspace = state
            .get_workspace(workspace_name)
            .ok_or_else(|| TilingError::WorkspaceNotFound(workspace_name.to_string()))?;

        let Some(window_index) = workspace.windows.iter().position(|&id| id == window_id) else {
            return Ok(None);
        };

        let screen = state
            .get_screen(&workspace.screen)
            .ok_or_else(|| TilingError::ScreenNotFound(workspace.screen.clone()))?;

        Ok(Some(ResizeInfo {
            layout_mode: workspace.layout.clone(),
            window_index,
            window_count: workspace.windows.len(),
            screen_width: screen.usable_frame.width,
            screen_height: screen.usable_frame.height,
        }))
    }

    /// Applies resize for dwindle-style layouts (Tiling, Split, etc.).
    #[allow(clippy::cast_precision_loss)]
    fn apply_dwindle_resize(
        &mut self,
        workspace_name: &str,
        info: &ResizeInfo,
        new_width: u32,
        new_height: u32,
    ) -> Result<(), TilingError> {
        // Get gaps
        let gaps = self.get_workspace_gaps(workspace_name)?;

        // Calculate usable dimensions
        let usable_width = info.screen_width.saturating_sub(gaps.outer_left + gaps.outer_right);
        let usable_height = info.screen_height.saturating_sub(gaps.outer_top + gaps.outer_bottom);
        let is_landscape = info.screen_width >= info.screen_height;

        // Determine split index and position
        let ratio_index = Self::calculate_ratio_index(info.window_index);
        let is_first_at_split = info.window_index == 0 || ratio_index > 0;

        // Determine which dimension this split uses
        let uses_width = Self::split_uses_width(&info.layout_mode, ratio_index, is_landscape);

        // Calculate the new ratio
        let new_ratio = Self::calculate_new_ratio(
            uses_width,
            usable_width,
            usable_height,
            gaps.inner,
            new_width,
            new_height,
            is_first_at_split,
        )?;

        // Update the split ratio in workspace
        self.update_split_ratio(workspace_name, ratio_index, new_ratio)?;

        // Re-apply the layout
        self.apply_layout(workspace_name)
    }

    /// Gets gaps for a workspace.
    fn get_workspace_gaps(
        &self,
        workspace_name: &str,
    ) -> Result<crate::tiling::layout::ResolvedGaps, TilingError> {
        let screen_count = self.workspace_manager.state().screens.len();
        let state = self.workspace_manager.state();
        let workspace = state
            .get_workspace(workspace_name)
            .ok_or_else(|| TilingError::WorkspaceNotFound(workspace_name.to_string()))?;
        let screen = state
            .get_screen(&workspace.screen)
            .ok_or_else(|| TilingError::ScreenNotFound(workspace.screen.clone()))?;

        Ok(crate::tiling::layout::ResolvedGaps::from_config(
            &self.config.gaps,
            screen,
            screen_count,
        ))
    }

    /// Calculates the ratio index for a window based on its position.
    ///
    /// In dwindle layout:
    /// - Window 0: affects ratio[0]
    /// - Window N (N > 0): affects ratio[N-1]
    const fn calculate_ratio_index(window_index: usize) -> usize {
        if window_index == 0 {
            0
        } else {
            window_index - 1
        }
    }

    /// Determines if a split at the given depth uses width or height.
    const fn split_uses_width(
        layout_mode: &barba_shared::LayoutMode,
        ratio_index: usize,
        is_landscape: bool,
    ) -> bool {
        match layout_mode {
            barba_shared::LayoutMode::SplitHorizontal => false, // Height determines ratio
            barba_shared::LayoutMode::SplitVertical => true,    // Width determines ratio
            _ => {
                // Dwindle-style: alternate based on screen aspect ratio and depth
                if is_landscape {
                    ratio_index.is_multiple_of(2)
                } else {
                    ratio_index % 2 == 1
                }
            }
        }
    }

    /// Calculates the new split ratio based on window size.
    #[allow(clippy::cast_precision_loss, clippy::too_many_arguments)]
    fn calculate_new_ratio(
        uses_width: bool,
        usable_width: u32,
        usable_height: u32,
        gap: u32,
        new_width: u32,
        new_height: u32,
        is_first_at_split: bool,
    ) -> Result<f64, TilingError> {
        let (container_size, window_size) = if uses_width {
            (usable_width.saturating_sub(gap), new_width)
        } else {
            (usable_height.saturating_sub(gap), new_height)
        };

        if container_size == 0 {
            return Err(TilingError::OperationFailed(
                "Container size is zero".to_string(),
            ));
        }

        let raw_ratio = f64::from(window_size) / f64::from(container_size);

        // Invert if window is on the "second" side of the split
        let adjusted_ratio = if is_first_at_split {
            raw_ratio
        } else {
            1.0 - raw_ratio
        };

        Ok(adjusted_ratio.clamp(0.1, 0.9))
    }

    /// Updates a split ratio in the workspace.
    fn update_split_ratio(
        &mut self,
        workspace_name: &str,
        ratio_index: usize,
        new_ratio: f64,
    ) -> Result<(), TilingError> {
        let workspace = self
            .workspace_manager
            .state_mut()
            .get_workspace_mut(workspace_name)
            .ok_or_else(|| TilingError::WorkspaceNotFound(workspace_name.to_string()))?;

        // Ensure split_ratios vector is large enough
        while workspace.split_ratios.len() <= ratio_index {
            workspace.split_ratios.push(0.5);
        }

        workspace.split_ratios[ratio_index] = new_ratio;
        Ok(())
    }

    /// Handles user-initiated resize for master layout.
    #[allow(clippy::cast_precision_loss)]
    fn handle_user_resize_master(
        &mut self,
        workspace_name: &str,
        window_id: u64,
        window_index: usize,
        new_width: u32,
        screen_width: u32,
    ) -> Result<(), TilingError> {
        // In master layout, the master window's width determines the ratio
        // Only the master window (index 0) resizing affects the ratio

        // Get gaps to calculate usable width
        let screen_count = self.workspace_manager.state().screens.len();
        let screen = {
            let state = self.workspace_manager.state();
            let workspace = state
                .get_workspace(workspace_name)
                .ok_or_else(|| TilingError::WorkspaceNotFound(workspace_name.to_string()))?;
            state
                .get_screen(&workspace.screen)
                .ok_or_else(|| TilingError::ScreenNotFound(workspace.screen.clone()))?
                .clone()
        };

        let gaps = crate::tiling::layout::ResolvedGaps::from_config(
            &self.config.gaps,
            &screen,
            screen_count,
        );

        // Calculate usable width
        let usable_width = screen_width.saturating_sub(gaps.outer_left + gaps.outer_right);
        let gap = gaps.inner;
        let total_width_for_ratio = usable_width.saturating_sub(gap);

        if total_width_for_ratio == 0 {
            return Ok(());
        }

        // Calculate ratio based on which window was resized
        let new_ratio = if window_index == 0 {
            // Master window was resized - its width directly determines the ratio
            f64::from(new_width) / f64::from(total_width_for_ratio)
        } else {
            // Stack window was resized - calculate master ratio from remaining space
            let stack_width = new_width;
            1.0 - (f64::from(stack_width) / f64::from(total_width_for_ratio))
        };

        // Clamp to valid range
        let clamped_ratio = new_ratio.clamp(0.1, 0.9);

        // Update the split ratio
        {
            let workspace = self
                .workspace_manager
                .state_mut()
                .get_workspace_mut(workspace_name)
                .ok_or_else(|| TilingError::WorkspaceNotFound(workspace_name.to_string()))?;

            if workspace.split_ratios.is_empty() {
                workspace.split_ratios.push(clamped_ratio);
            } else {
                workspace.split_ratios[0] = clamped_ratio;
            }
        }

        // Suppress unused variable warning
        let _ = window_id;

        // Re-apply the layout
        self.apply_layout(workspace_name)
    }

    /// Applies a floating preset to the focused window.
    ///
    /// Presets are defined in the configuration and specify window size and position.
    /// The window will be resized and positioned according to the preset settings.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No window is focused
    /// - The workspace is not in floating layout mode
    /// - The preset name doesn't exist in the configuration
    /// - Failed to resize/position the window
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    pub fn apply_preset(&mut self, preset_name: &str) -> Result<(), TilingError> {
        // Get the focused window and its workspace
        let (focused_window, workspace_name) = self.get_focused_window_and_workspace()?;

        // Check if the workspace is in floating layout mode
        let is_floating_layout = {
            let state = self.workspace_manager.state();
            state
                .get_workspace(&workspace_name)
                .is_some_and(|ws| ws.layout == barba_shared::LayoutMode::Floating)
        };

        if !is_floating_layout {
            return Err(TilingError::OperationFailed(
                "Presets can only be applied in floating layout mode".to_string(),
            ));
        }

        self.apply_preset_to_window(focused_window.id, preset_name)?;

        // Get workspace name for the window to mark it as floating
        if let Some(win) = self.workspace_manager.state_mut().windows.get_mut(&focused_window.id) {
            win.is_floating = true;
        }

        Ok(())
    }

    /// Applies a floating preset to a specific window by ID.
    ///
    /// This is used internally when windows are opened in floating workspaces
    /// with a `preset-on-open` configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The window doesn't exist or isn't in a workspace
    /// - The preset name doesn't exist in the configuration
    /// - Failed to resize/position the window
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    pub fn apply_preset_to_window(
        &mut self,
        window_id: u64,
        preset_name: &str,
    ) -> Result<(), TilingError> {
        // Find the workspace for this window
        let workspace_name = {
            let state = self.workspace_manager.state();
            state
                .workspaces
                .iter()
                .find(|ws| ws.windows.contains(&window_id))
                .map(|ws| ws.name.clone())
                .ok_or_else(|| {
                    TilingError::OperationFailed(format!(
                        "Window {window_id} not found in any workspace"
                    ))
                })?
        };

        // Find the preset in the configuration
        let preset = self
            .config
            .floating
            .presets
            .iter()
            .find(|p| p.name == preset_name)
            .ok_or_else(|| {
                TilingError::OperationFailed(format!("Preset not found: {preset_name}"))
            })?
            .clone();

        // Get screen info for the window's workspace
        let screen = {
            let state = self.workspace_manager.state();
            let workspace = state
                .get_workspace(&workspace_name)
                .ok_or_else(|| TilingError::WorkspaceNotFound(workspace_name.clone()))?;
            state
                .get_screen(&workspace.screen)
                .ok_or_else(|| TilingError::ScreenNotFound(workspace.screen.clone()))?
                .clone()
        };

        // Get gaps for proper positioning within screen
        let screen_count = self.workspace_manager.state().screens.len();
        let gaps = crate::tiling::layout::ResolvedGaps::from_config(
            &self.config.gaps,
            &screen,
            screen_count,
        );

        // Calculate the usable area (screen minus gaps)
        let usable_x = screen.usable_frame.x + gaps.outer_left as i32;
        let usable_y = screen.usable_frame.y + gaps.outer_top as i32;
        let usable_width =
            screen.usable_frame.width.saturating_sub(gaps.outer_left + gaps.outer_right);
        let usable_height =
            screen.usable_frame.height.saturating_sub(gaps.outer_top + gaps.outer_bottom);

        // Resolve dimensions
        let new_width = preset.width.resolve(usable_width).min(usable_width);
        let new_height = preset.height.resolve(usable_height).min(usable_height);

        // Calculate position
        let (new_x, new_y) = if preset.center {
            // Center the window on the usable area
            let center_x = usable_x + (usable_width as i32 - new_width as i32) / 2;
            let center_y = usable_y + (usable_height as i32 - new_height as i32) / 2;
            (center_x, center_y)
        } else {
            // Use specified x/y position (relative to usable area)
            let x_offset = preset.x.as_ref().map_or(0, |v| v.resolve(usable_width));
            let y_offset = preset.y.as_ref().map_or(0, |v| v.resolve(usable_height));
            (usable_x + x_offset as i32, usable_y + y_offset as i32)
        };

        // Ensure window stays within usable area
        let final_x = new_x.max(usable_x).min(usable_x + usable_width as i32 - new_width as i32);
        let final_y = new_y.max(usable_y).min(usable_y + usable_height as i32 - new_height as i32);

        // Apply the new frame to the window (with animation if enabled)
        let new_frame = crate::tiling::state::WindowFrame {
            x: final_x,
            y: final_y,
            width: new_width,
            height: new_height,
        };

        // Use animation for smooth preset transitions
        crate::tiling::animation::animate_window(window_id, new_frame);

        // Mark the window as floating so it's not rearranged by tiled layouts
        if let Some(win) = self.workspace_manager.state_mut().windows.get_mut(&window_id) {
            win.is_floating = true;
        }

        Ok(())
    }
}
