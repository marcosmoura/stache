//! Workspace command handlers.
//!
//! These handlers manage workspace switching, cycling, balancing,
//! and sending workspaces to different screens.

use uuid::Uuid;

use super::window::sync_window_visibility_for_workspaces;
use crate::modules::tiling::actor::messages::TargetScreen;
use crate::modules::tiling::init::get_subscriber_handle;
use crate::modules::tiling::state::TilingState;

// ============================================================================
// Workspace Switching
// ============================================================================

/// Switch to a workspace by name.
///
/// If the workspace exists and is not already visible, it becomes the focused
/// workspace on its assigned screen.
pub fn on_switch_workspace(state: &mut TilingState, name: &str) {
    tracing::debug!("Switching to workspace '{name}'");

    let Some(workspace) = state.get_workspace_by_name(name) else {
        tracing::warn!("Workspace '{name}' not found");
        return;
    };

    let workspace_id = workspace.id;
    let screen_id = workspace.screen_id;

    // Check if already visible
    if workspace.is_visible && workspace.is_focused {
        tracing::trace!("Workspace '{name}' already visible and focused, skipping");
        return;
    }

    // Capture previous workspace for event emission and focus history
    let previous_focus = eyeball::Observable::get(&state.focus).clone();
    let previous_workspace_id = previous_focus.focused_workspace_id;
    let previous_workspace_name =
        previous_workspace_id.and_then(|id| state.get_workspace(id)).map(|ws| ws.name);

    // Record focus history for the workspace we're leaving
    if let (Some(prev_ws_id), Some(prev_window_id)) =
        (previous_workspace_id, previous_focus.focused_window_id)
    {
        state.record_focus_history(prev_ws_id, prev_window_id);
        tracing::debug!(
            "Recorded focus history: workspace {prev_ws_id} -> window {prev_window_id}"
        );
    }

    // Track workspaces becoming visible/hidden
    let workspaces_becoming_visible: Vec<Uuid> = vec![workspace_id];
    let mut workspaces_becoming_hidden: Vec<Uuid> = Vec::new();

    // Mark all workspaces on this screen as not visible/focused
    for i in 0..state.workspaces.len() {
        let ws = &state.workspaces[i];
        if ws.screen_id == screen_id && ws.id != workspace_id {
            if ws.is_visible {
                workspaces_becoming_hidden.push(ws.id);
            }
            let ws_id = ws.id;
            state.update_workspace(ws_id, |ws| {
                ws.is_visible = false;
                ws.is_focused = false;
            });
        }
    }

    // Mark target workspace as visible and focused
    state.update_workspace(workspace_id, |ws| {
        ws.is_visible = true;
        ws.is_focused = true;
    });

    // Update focus state
    state.update_focus(|focus| {
        focus.focused_workspace_id = Some(workspace_id);
        focus.focused_screen_id = Some(screen_id);
        // Window focus will be updated separately if needed
    });

    tracing::debug!("Switched to workspace '{name}' (id={workspace_id})");

    // Sync window visibility (hide windows from old workspace, show windows from new)
    sync_window_visibility_for_workspaces(
        state,
        &workspaces_becoming_visible,
        &workspaces_becoming_hidden,
    );

    // Notify subscriber about visibility and layout changes
    if let Some(handle) = get_subscriber_handle() {
        // Hide borders for workspaces becoming hidden
        for ws_id in &workspaces_becoming_hidden {
            handle.notify_visibility_changed(*ws_id, false);
        }
        // Show borders and apply layout for new workspace
        handle.notify_visibility_changed(workspace_id, true);
        handle.notify_layout_changed(workspace_id, true);
    }

    // Focus a window in the new workspace, preferring focus history
    if let Some(ws) = state.get_workspace(workspace_id) {
        // Check focus history first - prefer the last focused window in this workspace
        let target_window_id = state
            .get_focus_history(workspace_id)
            .filter(|&id| ws.window_ids.contains(&id))
            .or_else(|| ws.window_ids.first().copied());

        if let Some(window_id) = target_window_id {
            tracing::debug!(
                "Focusing window {} in workspace '{}' (from {})",
                window_id,
                name,
                if state.get_focus_history(workspace_id).is_some_and(|id| id == window_id) {
                    "focus history"
                } else {
                    "first window"
                }
            );
            // Update focus state to track the new focused window
            state.update_focus(|focus| {
                focus.focused_window_id = Some(window_id);
            });
            // Use the window_ops to focus the window via AX API
            let _ = crate::modules::tiling::effects::window_ops::focus_window(window_id);
        } else {
            // No windows in workspace - clear focused window
            state.update_focus(|focus| {
                focus.focused_window_id = None;
            });
        }
    }

    // Notify subscriber about focus change to update borders
    if let Some(handle) = get_subscriber_handle() {
        handle.notify_focus_changed();
    }

    // Emit workspace changed event to frontend
    let screen_name = state
        .get_screen(screen_id)
        .map_or_else(|| format!("screen-{screen_id}"), |s| s.name);

    crate::modules::tiling::init::emit_workspace_changed(
        name,
        &screen_name,
        previous_workspace_name.as_deref(),
    );
}

// ============================================================================
// Workspace Cycling
// ============================================================================

use crate::modules::tiling::actor::CycleDirection;

/// Cycle through workspaces in a direction.
///
/// Cycles through visible workspaces on the currently focused screen.
pub fn on_cycle_workspace(state: &mut TilingState, direction: CycleDirection) {
    let focus = state.get_focus_state();
    let Some(current_workspace_id) = focus.focused_workspace_id else {
        tracing::debug!("cycle_workspace: no focused workspace");
        return;
    };

    let Some(screen_id) = focus.focused_screen_id else {
        tracing::debug!("cycle_workspace: no focused screen");
        return;
    };

    // Capture previous workspace name for event emission
    let previous_workspace_name = state.get_workspace(current_workspace_id).map(|ws| ws.name);

    // Record focus history for the workspace we're leaving
    if let Some(current_window_id) = focus.focused_window_id {
        state.record_focus_history(current_workspace_id, current_window_id);
        tracing::debug!(
            "Recorded focus history: workspace {current_workspace_id} -> window {current_window_id}"
        );
    }

    // Get all workspaces on this screen
    let screen_workspaces: Vec<Uuid> = state
        .workspaces
        .iter()
        .filter(|ws| ws.screen_id == screen_id)
        .map(|ws| ws.id)
        .collect();

    if screen_workspaces.len() <= 1 {
        tracing::debug!("cycle_workspace: only one workspace on screen");
        return;
    }

    // Find current position
    let current_idx =
        screen_workspaces.iter().position(|&id| id == current_workspace_id).unwrap_or(0);

    // Calculate next index
    let next_idx = match direction {
        CycleDirection::Next => (current_idx + 1) % screen_workspaces.len(),
        CycleDirection::Previous => {
            if current_idx == 0 {
                screen_workspaces.len() - 1
            } else {
                current_idx - 1
            }
        }
    };

    let next_workspace_id = screen_workspaces[next_idx];

    // Switch to next workspace
    // Mark current as not visible/focused
    state.update_workspace(current_workspace_id, |ws| {
        ws.is_visible = false;
        ws.is_focused = false;
    });

    // Mark next as visible and focused
    state.update_workspace(next_workspace_id, |ws| {
        ws.is_visible = true;
        ws.is_focused = true;
    });

    // Update focus state
    state.update_focus(|focus| {
        focus.focused_workspace_id = Some(next_workspace_id);
    });

    tracing::debug!("Cycled to workspace {next_workspace_id} ({direction:?})");

    // Focus a window in the new workspace, preferring focus history
    if let Some(ws) = state.get_workspace(next_workspace_id) {
        // Check focus history first - prefer the last focused window in this workspace
        let target_window_id = state
            .get_focus_history(next_workspace_id)
            .filter(|&id| ws.window_ids.contains(&id))
            .or_else(|| ws.window_ids.first().copied());

        if let Some(window_id) = target_window_id {
            tracing::debug!(
                "Focusing window {} in workspace {} (from {})",
                window_id,
                next_workspace_id,
                if state.get_focus_history(next_workspace_id).is_some_and(|id| id == window_id) {
                    "focus history"
                } else {
                    "first window"
                }
            );
            // Update focus state to track the new focused window
            state.update_focus(|focus| {
                focus.focused_window_id = Some(window_id);
            });
            // Use the window_ops to focus the window via AX API
            let _ = crate::modules::tiling::effects::window_ops::focus_window(window_id);
        } else {
            // No windows in workspace - clear focused window
            state.update_focus(|focus| {
                focus.focused_window_id = None;
            });
        }
    }

    // Notify subscriber about visibility and layout changes
    if let Some(handle) = get_subscriber_handle() {
        handle.notify_visibility_changed(current_workspace_id, false);
        handle.notify_visibility_changed(next_workspace_id, true);
        handle.notify_layout_changed(next_workspace_id, true);
        // Notify about focus change to update borders
        handle.notify_focus_changed();
    }

    // Emit workspace changed event to frontend
    if let Some(next_ws) = state.get_workspace(next_workspace_id) {
        let screen_name = state
            .get_screen(screen_id)
            .map_or_else(|| format!("screen-{screen_id}"), |s| s.name);

        crate::modules::tiling::init::emit_workspace_changed(
            &next_ws.name,
            &screen_name,
            previous_workspace_name.as_deref(),
        );
    }
}

// ============================================================================
// Workspace Balancing
// ============================================================================

/// Balance split ratios in a workspace.
///
/// Resets all split ratios to equal distribution and clears inferred minimum
/// sizes so the layout is calculated fresh, as if the app had just started.
pub fn on_balance_workspace(state: &mut TilingState, workspace_id: Uuid) {
    // Get window IDs in this workspace before clearing ratios
    let window_ids: Vec<u32> = state
        .get_workspace(workspace_id)
        .map(|ws| ws.window_ids.to_vec())
        .unwrap_or_default();

    // Clear all runtime ratio overrides, restoring config defaults
    state.update_workspace(workspace_id, |ws| {
        ws.split_ratios.clear();
        ws.master_ratio = None;
    });

    // Clear inferred minimum sizes for all windows in the workspace
    // This resets the layout calculation to use only reported minimums
    for window_id in window_ids {
        state.update_window(window_id, |w| {
            w.inferred_minimum_size = None;
        });
    }

    tracing::debug!("Balanced workspace {workspace_id}");

    // Notify subscriber to recalculate layout
    if let Some(handle) = get_subscriber_handle() {
        handle.notify_layout_changed(workspace_id, true);
    }
}

// ============================================================================
// Send Workspace to Screen
// ============================================================================

/// Send the focused workspace to another screen.
///
/// The workspace becomes visible on the target screen.
/// If it was visible on the source screen, another workspace becomes visible there.
pub fn on_send_workspace_to_screen(state: &mut TilingState, target_screen: &TargetScreen) {
    // Get focused workspace
    let focus = state.get_focus_state();
    let Some(workspace_id) = focus.focused_workspace_id else {
        tracing::debug!("send_workspace_to_screen: no focused workspace");
        return;
    };

    let Some(workspace) = state.get_workspace(workspace_id) else {
        tracing::debug!("send_workspace_to_screen: workspace not found");
        return;
    };

    let workspace_name = workspace.name;
    let source_screen_id = workspace.screen_id;
    let was_visible = workspace.is_visible;

    // Resolve target screen
    let target_screen_id = resolve_screen(state, target_screen);
    let Some(target_screen_id) = target_screen_id else {
        tracing::warn!(
            "send_workspace_to_screen: screen '{}' not found",
            target_screen.as_str()
        );
        return;
    };

    // Don't move to same screen
    if source_screen_id == target_screen_id {
        tracing::debug!("send_workspace_to_screen: workspace already on target screen");
        return;
    }

    // Track workspaces becoming visible/hidden
    let mut workspaces_becoming_visible: Vec<Uuid> = vec![workspace_id];
    let mut workspaces_becoming_hidden: Vec<Uuid> = Vec::new();

    // If workspace was visible on source screen, find a fallback to make visible
    if was_visible {
        // Find another workspace on source screen
        let fallback_workspace_id = state
            .workspaces
            .iter()
            .find(|ws| ws.screen_id == source_screen_id && ws.id != workspace_id)
            .map(|ws| ws.id);

        if let Some(fallback_id) = fallback_workspace_id {
            workspaces_becoming_visible.push(fallback_id);

            // Make fallback visible and focused on source screen
            state.update_workspace(fallback_id, |ws| {
                ws.is_visible = true;
                ws.is_focused = false; // Will be focused if user switches to it
            });
        }
    }

    // Hide currently visible workspace on target screen
    for ws in state.workspaces.iter() {
        if ws.screen_id == target_screen_id && ws.is_visible && ws.id != workspace_id {
            workspaces_becoming_hidden.push(ws.id);
        }
    }

    for ws_id in &workspaces_becoming_hidden {
        state.update_workspace(*ws_id, |ws| {
            ws.is_visible = false;
            ws.is_focused = false;
        });
    }

    // Update workspace's screen assignment
    state.update_workspace(workspace_id, |ws| {
        ws.screen_id = target_screen_id;
        ws.is_visible = true;
        ws.is_focused = true;
    });

    // Update focus state
    state.update_focus(|focus| {
        focus.focused_workspace_id = Some(workspace_id);
        focus.focused_screen_id = Some(target_screen_id);
    });

    tracing::debug!(
        "Sent workspace '{workspace_name}' from screen {source_screen_id} to '{}'",
        target_screen.as_str()
    );

    // Sync window visibility
    sync_window_visibility_for_workspaces(
        state,
        &workspaces_becoming_visible,
        &workspaces_becoming_hidden,
    );

    // Notify subscriber about visibility and layout changes
    if let Some(handle) = get_subscriber_handle() {
        for ws_id in &workspaces_becoming_hidden {
            handle.notify_visibility_changed(*ws_id, false);
        }
        handle.notify_visibility_changed(workspace_id, true);
        handle.notify_layout_changed(workspace_id, true);
    }
}

// ============================================================================
// Screen Resolution Helper
// ============================================================================

/// Resolve a target screen to a screen ID.
///
/// Supports `Main`/`Secondary` or named display.
#[must_use]
pub fn resolve_screen(state: &TilingState, target: &TargetScreen) -> Option<u32> {
    match target {
        TargetScreen::Main => state.screens.iter().find(|s| s.is_main).map(|s| s.id),
        TargetScreen::Secondary => state.screens.iter().find(|s| !s.is_main).map(|s| s.id),
        TargetScreen::Named(name) => {
            state.screens.iter().find(|s| s.name.eq_ignore_ascii_case(name)).map(|s| s.id)
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::tiling::state::{Screen, Workspace};

    fn create_test_state() -> TilingState {
        let mut state = TilingState::new();

        // Add a screen
        let screen = Screen {
            id: 1,
            name: "Test Screen".to_string(),
            is_main: true,
            ..Default::default()
        };
        state.upsert_screen(screen);

        // Add workspaces
        let mut ws1 = Workspace::new("workspace1");
        ws1.screen_id = 1;
        ws1.is_visible = true;
        ws1.is_focused = true;
        let ws1_id = ws1.id;
        state.upsert_workspace(ws1);

        let mut ws2 = Workspace::new("workspace2");
        ws2.screen_id = 1;
        state.upsert_workspace(ws2);

        // Update focus state
        state.update_focus(|focus| {
            focus.focused_workspace_id = Some(ws1_id);
            focus.focused_screen_id = Some(1);
        });

        state
    }

    #[test]
    fn test_switch_workspace() {
        let mut state = create_test_state();

        // Switch to workspace2
        on_switch_workspace(&mut state, "workspace2");

        let ws2 = state.get_workspace_by_name("workspace2").unwrap();
        assert!(ws2.is_visible);
        assert!(ws2.is_focused);

        let ws1 = state.get_workspace_by_name("workspace1").unwrap();
        assert!(!ws1.is_visible);
        assert!(!ws1.is_focused);

        let focus = state.get_focus_state();
        assert_eq!(focus.focused_workspace_id, Some(ws2.id));
    }

    #[test]
    fn test_switch_workspace_not_found() {
        let mut state = create_test_state();

        // Should not panic
        on_switch_workspace(&mut state, "nonexistent");

        // State should be unchanged
        let ws1 = state.get_workspace_by_name("workspace1").unwrap();
        assert!(ws1.is_focused);
    }

    #[test]
    fn test_cycle_workspace() {
        let mut state = create_test_state();
        let ws1_id = state.get_workspace_by_name("workspace1").unwrap().id;
        let ws2_id = state.get_workspace_by_name("workspace2").unwrap().id;

        // Cycle next
        on_cycle_workspace(&mut state, CycleDirection::Next);

        let focus = state.get_focus_state();
        assert_eq!(focus.focused_workspace_id, Some(ws2_id));

        // Cycle next again (should wrap to first)
        on_cycle_workspace(&mut state, CycleDirection::Next);

        let focus = state.get_focus_state();
        assert_eq!(focus.focused_workspace_id, Some(ws1_id));

        // Cycle previous
        on_cycle_workspace(&mut state, CycleDirection::Previous);

        let focus = state.get_focus_state();
        assert_eq!(focus.focused_workspace_id, Some(ws2_id));
    }
}
