//! Workspace switching, visibility, and screen operations.
//!
//! This module handles workspace switching, showing/hiding apps,
//! and moving workspaces between screens.

#![allow(clippy::assigning_clones)]

use std::collections::HashSet;

use super::{TilingManager, mark_switch_completed};
use crate::tiling::error::TilingError;
use crate::tiling::window;

impl TilingManager {
    /// Updates the focused workspace on each screen to be the first one that has windows.
    /// This ensures that at startup, we don't hide windows that are currently visible.
    pub(super) fn sync_focused_workspaces_with_windows(&mut self) {
        let state = self.workspace_manager.state();

        // For each screen, find the first workspace that has windows
        let updates: Vec<(String, String)> = state
            .screens
            .iter()
            .filter_map(|screen| {
                // Get all workspaces on this screen
                let workspaces_on_screen = state.get_workspaces_on_screen(&screen.id);

                // Find the first workspace that has windows
                let ws_with_windows = workspaces_on_screen.iter().find(|ws| !ws.windows.is_empty());

                ws_with_windows.map(|ws| (screen.id.clone(), ws.name.clone()))
            })
            .collect();

        // Apply the updates
        for (screen_id, workspace_name) in updates {
            self.workspace_manager
                .state_mut()
                .focused_workspace_per_screen
                .insert(screen_id, workspace_name);
        }
    }

    /// Initializes the focus state based on a pre-captured focused app PID.
    ///
    /// This is used during startup when we capture the focused app's PID BEFORE
    /// unhiding all apps (which can change focus). We then use this PID to find
    /// which workspace should be focused.
    pub(super) fn initialize_focus_state_from_pid(&mut self, focused_pid: Option<i32>) {
        if let Some(pid) = focused_pid {
            // Find a workspace that has a window with this PID
            let workspace_info: Option<(String, String, Option<u64>)> =
                self.workspace_manager.state().workspaces.iter().find_map(|ws| {
                    // Find first window in this workspace with the target PID
                    let window_id = ws.windows.iter().find(|&&wid| {
                        self.workspace_manager.state().get_window(wid).is_some_and(|w| w.pid == pid)
                    });
                    window_id.map(|&wid| (ws.name.clone(), ws.screen.clone(), Some(wid)))
                });

            if let Some((ws_name, screen_id, window_id)) = workspace_info {
                // Set this workspace as the globally focused one
                self.workspace_manager.state_mut().focused_workspace = Some(ws_name.clone());

                // Set the focused window if we found one
                if let Some(wid) = window_id {
                    self.workspace_manager.state_mut().focused_window = Some(wid);
                }

                // Update the focused workspace for this screen
                self.workspace_manager
                    .state_mut()
                    .focused_workspace_per_screen
                    .insert(screen_id, ws_name);
                return;
            }
        }

        // Fallback: no focused PID or not found - use the first workspace on the main screen
        let first_workspace = self.workspace_manager.state().get_main_screen().and_then(|screen| {
            self.workspace_manager
                .state()
                .get_workspaces_on_screen(&screen.id)
                .first()
                .map(|ws| ws.name.clone())
        });

        if let Some(ws_name) = first_workspace {
            self.workspace_manager.state_mut().focused_workspace = Some(ws_name);
        }
    }

    /// Focuses the first window on the focused workspace after initialization.
    ///
    /// This ensures that when the app starts, there's always a focused window
    /// if there are any windows in the focused workspace.
    pub(super) fn focus_initial_window(&mut self) {
        // Get the focused workspace
        let focused_workspace = self.workspace_manager.state().focused_workspace.clone();

        let Some(ws_name) = focused_workspace else {
            return;
        };

        // Get the first window in this workspace
        let window_to_focus = self
            .workspace_manager
            .state()
            .get_workspace(&ws_name)
            .and_then(|ws| ws.windows.first().copied())
            .and_then(|wid| self.workspace_manager.state().get_window(wid).cloned());

        // Focus the window
        if let Some(ref win) = window_to_focus {
            // Update the focused window in state
            self.workspace_manager.state_mut().focused_window = Some(win.id);

            // Focus the window using the fast path
            let _ = window::focus_window_fast(win);
        }
    }

    /// Sends all windows except the focused one to the back.
    ///
    /// This is called after initialization to ensure that when we unhide all apps
    /// to discover windows, only the focused window remains in front.
    /// All other windows on the focused workspace(s) are sent to the back.
    pub(super) fn send_non_focused_windows_to_back(&self) {
        let state = self.workspace_manager.state();
        let focused_window_id = state.focused_window;

        // If we have a focused window, re-raise it to ensure it's on top of any
        // windows that were just unhidden during discovery
        if let Some(focused_id) = focused_window_id
            && let Some(focused_win) = state.get_window(focused_id).cloned()
        {
            // Re-focus the focused window to ensure it's on top
            let _ = window::focus_window_fast(&focused_win);
        }
    }

    /// Hides all windows on workspaces that are not currently focused on their screen.
    pub(super) fn hide_non_focused_workspaces(&mut self) {
        let state = self.workspace_manager.state();

        // Collect workspaces that need their windows hidden
        let workspaces_to_hide: Vec<String> = state
            .workspaces
            .iter()
            .filter(|ws| {
                // Check if this workspace is the focused one on its screen
                let focused_on_screen = state.focused_workspace_per_screen.get(&ws.screen);
                focused_on_screen != Some(&ws.name)
            })
            .map(|ws| ws.name.clone())
            .collect();

        for workspace_name in workspaces_to_hide {
            if let Err(e) = self.hide_workspace_apps(&workspace_name) {
                eprintln!("barba: tiling: failed to hide workspace {workspace_name}: {e}");
            }
        }
    }

    /// Sends the current workspace to a different screen.
    ///
    /// This moves all windows from the current workspace to the target screen,
    /// swapping workspaces if the target screen already has a focused workspace.
    pub fn send_workspace_to_screen(&mut self, target: &str) -> Result<(), TilingError> {
        // Get the current focused workspace
        let current_workspace_name = self
            .workspace_manager
            .state()
            .focused_workspace
            .clone()
            .ok_or_else(|| TilingError::WorkspaceNotFound("current".to_string()))?;

        // Get the current workspace's screen
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

        // If already on the target screen, do nothing
        if current_screen_id == target_screen_id {
            return Ok(());
        }

        // Get the focused workspace on the target screen (if any)
        let target_workspace_name = self
            .workspace_manager
            .state()
            .focused_workspace_per_screen
            .get(&target_screen_id)
            .cloned();

        // Swap the workspaces between screens
        // Update current workspace to target screen
        if let Some(ws) =
            self.workspace_manager.state_mut().get_workspace_mut(&current_workspace_name)
        {
            ws.screen = target_screen_id.clone();
        }

        // Update target workspace to current screen (if exists)
        if let Some(ref target_ws_name) = target_workspace_name {
            if let Some(ws) = self.workspace_manager.state_mut().get_workspace_mut(target_ws_name) {
                ws.screen = current_screen_id.clone();
            }

            // Update focused workspace per screen for the old screen
            self.workspace_manager
                .state_mut()
                .focused_workspace_per_screen
                .insert(current_screen_id, target_ws_name.clone());
        }

        // Update focused workspace per screen for the target screen
        self.workspace_manager
            .state_mut()
            .focused_workspace_per_screen
            .insert(target_screen_id, current_workspace_name.clone());

        // Re-apply layouts for both workspaces
        let _ = self.apply_layout(&current_workspace_name);
        if let Some(ref target_ws_name) = target_workspace_name {
            let _ = self.apply_layout(target_ws_name);
        }

        Ok(())
    }

    /// Focuses a workspace on an adjacent screen (directional focus).
    ///
    /// This switches focus to the focused workspace on the screen in the given direction.
    pub fn focus_workspace_on_screen(&mut self, direction: &str) -> Result<(), TilingError> {
        // Get the current workspace and its screen
        let current_workspace_name = self
            .workspace_manager
            .state()
            .focused_workspace
            .clone()
            .ok_or_else(|| TilingError::WorkspaceNotFound("current".to_string()))?;

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

        // Find the screen in the given direction
        let target_screen_id = self
            .workspace_manager
            .state()
            .resolve_screen_target(direction, Some(&current_screen_id))
            .ok_or_else(|| TilingError::ScreenNotFound(direction.to_string()))?;

        // Get the focused workspace on that screen
        let target_workspace_name = self
            .workspace_manager
            .state()
            .focused_workspace_per_screen
            .get(&target_screen_id)
            .cloned()
            .ok_or_else(|| {
                TilingError::WorkspaceNotFound(format!("no workspace on screen {target_screen_id}"))
            })?;

        // Switch to that workspace
        self.switch_workspace(&target_workspace_name)
    }

    /// Switches to a workspace, hiding windows from the old workspace and showing windows from the new one.
    ///
    /// This only affects workspaces on the same screen - each screen has its own focused workspace.
    pub fn switch_workspace(&mut self, workspace_name: &str) -> Result<(), TilingError> {
        self.switch_workspace_focusing(workspace_name, None)
    }

    /// Switches to a workspace, optionally focusing a specific window.
    ///
    /// If `focus_window_id` is Some, that window will be focused instead of the first window.
    /// This is used when switching workspaces due to Cmd+Tab, where we want to focus the
    /// window that was Cmd+Tab'd to, not the first window in layout order.
    pub fn switch_workspace_focusing(
        &mut self,
        workspace_name: &str,
        focus_window_id: Option<u64>,
    ) -> Result<(), TilingError> {
        let result = self.switch_workspace_internal(workspace_name, focus_window_id);

        // Mark the switch as completed to start the cooldown period
        mark_switch_completed();

        result
    }

    /// Internal implementation of workspace switching.
    #[allow(clippy::too_many_lines)]
    fn switch_workspace_internal(
        &mut self,
        workspace_name: &str,
        focus_window_id: Option<u64>,
    ) -> Result<(), TilingError> {
        // Get the target workspace's screen
        let target_screen_id = {
            let workspace = self
                .workspace_manager
                .state()
                .get_workspace(workspace_name)
                .ok_or_else(|| TilingError::WorkspaceNotFound(workspace_name.to_string()))?;
            workspace.screen.clone()
        };

        // Get PIDs for the new workspace from our persistent tracking
        // This is more reliable than deriving from window IDs which may be stale
        let new_workspace_pids =
            self.workspace_pids.get(workspace_name).cloned().unwrap_or_default();

        // Get the currently focused workspace on this screen
        let current_workspace_name = self
            .workspace_manager
            .state()
            .focused_workspace_per_screen
            .get(&target_screen_id)
            .cloned();

        // Track the previous global focus for screen change detection
        let previous_global_workspace = self.workspace_manager.state().focused_workspace.clone();
        let previous_screen_id = previous_global_workspace.as_ref().and_then(|ws_name| {
            self.workspace_manager
                .state()
                .get_workspace(ws_name)
                .map(|ws| ws.screen.clone())
        });

        // Note: We intentionally don't skip when switching to the same workspace.
        // This ensures windows are always visible and focused, handling cases where
        // the state may be out of sync with reality (e.g., after app restart).

        // Get PIDs from the old workspace from our persistent tracking
        // Also track PIDs that have PiP windows - we cannot hide those apps because
        // hiding is app-level (AXHidden) and would hide the PiP too. As a trade-off,
        // all windows from apps with PiP will remain visible across workspaces.
        let all_windows = window::get_all_windows().unwrap_or_default();
        let pids_with_pip: HashSet<i32> =
            all_windows.iter().filter(|w| window::is_pip_window(w)).map(|w| w.pid).collect();

        let old_workspace_pids: HashSet<i32> =
            if let Some(ref current_ws_name) = current_workspace_name {
                self.workspace_pids
                    .get(current_ws_name)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|pid| !new_workspace_pids.contains(pid))
                    // Don't hide apps that have PiP windows
                    .filter(|pid| !pids_with_pip.contains(pid))
                    .collect()
            } else {
                HashSet::new()
            };

        // Update focus state
        self.workspace_manager.state_mut().focused_workspace = Some(workspace_name.to_string());
        self.workspace_manager
            .state_mut()
            .focused_workspace_per_screen
            .insert(target_screen_id.clone(), workspace_name.to_string());

        // Emit workspace focused event
        self.emit_workspaces_changed();

        // Check if screen focus changed
        if previous_screen_id.as_deref() != Some(&target_screen_id) {
            let is_main = self
                .workspace_manager
                .state()
                .screens
                .iter()
                .find(|s| s.id == target_screen_id)
                .is_some_and(|s| s.is_main);
            self.emit_screen_focused(&target_screen_id, is_main, previous_screen_id.as_deref());
        }

        // Determine which window to focus BEFORE doing any slow operations
        // This allows us to focus immediately after unhiding
        let window_to_focus = focus_window_id.or_else(|| {
            self.workspace_manager
                .state()
                .get_workspace(workspace_name)
                .and_then(|ws| ws.windows.first().copied())
        });

        // Get the window info while we still have access to state
        let focus_window_info =
            window_to_focus.and_then(|wid| self.workspace_manager.state().get_window(wid).cloned());

        // First: unhide all new workspace apps, then hide old workspace apps
        // For apps with PiP windows, minimize/restore individual windows instead
        // Wait for all to complete before proceeding
        std::thread::scope(|s| {
            for pid in &new_workspace_pids {
                let pid = *pid;
                s.spawn(move || {
                    let _ = window::unhide_app(pid);
                });
            }

            for pid in &old_workspace_pids {
                let pid = *pid;
                s.spawn(move || {
                    let _ = window::hide_app(pid);
                });
            }
        });

        // Focus the window IMMEDIATELY after unhiding, before slow discovery operations
        // This makes the workspace switch feel instant to the user
        if let Some(ref window) = focus_window_info {
            let _ = window::focus_window_fast(window);
        }

        // Now do the slower discovery work in the background-style (still blocking but less critical)
        // After unhiding apps, give macOS a moment to register windows, then discover them
        std::thread::sleep(std::time::Duration::from_millis(20));

        // Discover windows from newly unhidden apps and add them to this workspace
        // Use get_all_windows_including_hidden because the windows may not yet be marked "on screen"
        if let Ok(all_windows) = window::get_all_windows_including_hidden() {
            for win in all_windows {
                // Only process windows from apps that belong to this workspace
                if new_workspace_pids.contains(&win.pid) {
                    // Skip windows that match ignore rules
                    if self.should_ignore_window(&win) {
                        continue;
                    }

                    let window_id = win.id;

                    // Add window to state if not already tracked
                    if !self.workspace_manager.state().windows.contains_key(&window_id) {
                        self.workspace_manager.state_mut().windows.insert(window_id, win);
                    }

                    // Add window to workspace if not already in the list
                    if let Some(ws) =
                        self.workspace_manager.state_mut().get_workspace_mut(workspace_name)
                        && !ws.windows.contains(&window_id)
                    {
                        ws.windows.push(window_id);
                    }
                }
            }
        }

        // Apply preset-on-open for floating workspaces when switching for the first time
        // This ensures windows get the preset applied when you first switch to a workspace
        // that was populated before the app started or via another mechanism
        if !self.preset_applied_workspaces.contains(workspace_name) {
            let workspace_config =
                self.config.workspaces.iter().find(|wc| wc.name == workspace_name);

            if let Some(wc) = workspace_config
                && wc.layout == barba_shared::LayoutMode::Floating
                && let Some(ref preset_name) = wc.preset_on_open
            {
                // Get windows in this workspace
                let window_ids: Vec<u64> = self
                    .workspace_manager
                    .state()
                    .get_workspace(workspace_name)
                    .map(|ws| ws.windows.to_vec())
                    .unwrap_or_default();

                // Apply preset to each window
                let preset_name = preset_name.clone();
                for window_id in window_ids {
                    if let Err(e) = self.apply_preset_to_window(window_id, &preset_name) {
                        eprintln!("barba: failed to apply preset-on-open during switch: {e}");
                    }
                }
            }

            // Mark as applied regardless of whether we had a preset
            // This ensures we only try once per workspace
            self.preset_applied_workspaces.insert(workspace_name.to_string());
        }

        // Apply layout now that we have discovered windows
        let _ = self.apply_layout(workspace_name);

        Ok(())
    }

    /// Hides all applications that have windows in a workspace.
    /// Uses the `AXHidden` attribute (same as Cmd+H).
    ///
    /// Note: This is careful not to hide apps that have windows on other focused workspaces
    /// (i.e., on different screens), since hiding an app hides ALL its windows.
    pub(super) fn hide_workspace_apps(&mut self, workspace_name: &str) -> Result<(), TilingError> {
        // Get the set of focused workspaces (one per screen)
        let focused_workspaces: HashSet<String> = self
            .workspace_manager
            .state()
            .focused_workspace_per_screen
            .values()
            .cloned()
            .collect();

        // Collect PIDs that have windows ONLY on non-focused workspaces
        // If an app has windows on any focused workspace, we must NOT hide it
        let pids_on_focused_workspaces: HashSet<i32> = {
            self.workspace_manager
                .state()
                .workspaces
                .iter()
                .filter(|ws| focused_workspaces.contains(&ws.name))
                .flat_map(|ws| {
                    ws.windows.iter().filter_map(|window_id| {
                        self.workspace_manager.state().get_window(*window_id).map(|w| w.pid)
                    })
                })
                .collect()
        };

        // Collect unique PIDs from windows in this workspace
        // Also track PIDs that have PiP windows (we must not hide those apps)
        // Check ALL windows including those not tracked in state (use fresh window list)
        let all_windows = window::get_all_windows().unwrap_or_default();
        let pids_with_pip: HashSet<i32> =
            all_windows.iter().filter(|w| window::is_pip_window(w)).map(|w| w.pid).collect();

        let pids_to_hide: HashSet<i32> = {
            let workspace = self
                .workspace_manager
                .state()
                .get_workspace(workspace_name)
                .ok_or_else(|| TilingError::WorkspaceNotFound(workspace_name.to_string()))?;

            workspace
                .windows
                .iter()
                .filter_map(|window_id| {
                    self.workspace_manager.state().get_window(*window_id).map(|w| w.pid)
                })
                // Only hide if this PID has no windows on any focused workspace
                .filter(|pid| !pids_on_focused_workspaces.contains(pid))
                // Don't hide apps that have PiP windows
                .filter(|pid| !pids_with_pip.contains(pid))
                .collect()
        };

        // Hide each app (silently ignore apps that don't support AXHidden)
        for pid in pids_to_hide {
            let _ = window::hide_app(pid);
        }

        // Mark windows as hidden in state
        let window_ids: Vec<u64> = {
            self.workspace_manager
                .state()
                .get_workspace(workspace_name)
                .map(|ws| ws.windows.to_vec())
                .unwrap_or_default()
        };

        for window_id in window_ids {
            if let Some(win) = self.workspace_manager.state_mut().get_window_mut(window_id) {
                win.is_hidden = true;
            }
        }

        Ok(())
    }

    /// Balances window sizes in a workspace.
    ///
    /// This resets all split ratios to their default values (equal splits)
    /// and re-applies the layout, restoring windows to their balanced sizes.
    ///
    /// # Errors
    ///
    /// Returns an error if the workspace is not found.
    pub fn balance_workspace(&mut self, workspace_name: &str) -> Result<(), TilingError> {
        // Clear split ratios to reset to equal splits
        {
            let workspace = self
                .workspace_manager
                .state_mut()
                .get_workspace_mut(workspace_name)
                .ok_or_else(|| TilingError::WorkspaceNotFound(workspace_name.to_string()))?;

            workspace.split_ratios.clear();
        }

        // Re-apply the layout with default (equal) ratios
        self.apply_layout(workspace_name)
    }
}
