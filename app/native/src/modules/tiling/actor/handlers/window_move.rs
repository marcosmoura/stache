//! Window movement command handlers.
//!
//! These handlers manage moving windows between workspaces, swapping windows,
//! toggling floating state, and sending windows to screens.

use uuid::Uuid;

use super::workspace::resolve_screen;
use crate::modules::tiling::actor::messages::TargetScreen;
use crate::modules::tiling::init::get_subscriber_handle;
use crate::modules::tiling::state::TilingState;

// ============================================================================
// Move Window to Workspace
// ============================================================================

/// Move a window to a different workspace.
pub fn on_move_window_to_workspace(state: &mut TilingState, window_id: u32, workspace_id: Uuid) {
    let Some(window) = state.get_window(window_id) else {
        log::warn!("move_window: window {window_id} not found");
        return;
    };

    let old_workspace_id = window.workspace_id;
    if old_workspace_id == workspace_id {
        log::debug!("move_window: window {window_id} already in workspace {workspace_id}");
        return;
    }

    // Remove from old workspace
    state.update_workspace(old_workspace_id, |ws| {
        ws.window_ids.retain(|id| *id != window_id);
        // Update focused index if needed
        if let Some(idx) = ws.focused_window_index
            && let Some(pos) = ws.window_ids.iter().position(|&id| id == window_id)
        {
            if idx > pos {
                ws.focused_window_index = Some(idx - 1);
            } else if idx == pos {
                ws.focused_window_index = None;
            }
        }
    });

    // Add to new workspace
    state.update_workspace(workspace_id, |ws| {
        ws.window_ids.push(window_id);
    });

    // Update window's workspace reference
    state.update_window(window_id, |w| {
        w.workspace_id = workspace_id;
    });

    log::debug!("Moved window {window_id} to workspace {workspace_id}");

    // Notify subscriber to recalculate layouts for both workspaces
    if let Some(handle) = get_subscriber_handle() {
        handle.notify_layout_changed(old_workspace_id, true);
        handle.notify_layout_changed(workspace_id, true);
    }
}

// ============================================================================
// Swap Windows
// ============================================================================

/// Swap two windows in the stack order.
pub fn on_swap_windows(state: &mut TilingState, window_id_a: u32, window_id_b: u32) {
    if window_id_a == window_id_b {
        return;
    }

    let Some(window_a) = state.get_window(window_id_a) else {
        log::warn!("swap_windows: window {window_id_a} not found");
        return;
    };

    let Some(window_b) = state.get_window(window_id_b) else {
        log::warn!("swap_windows: window {window_id_b} not found");
        return;
    };

    // Windows must be in the same workspace
    if window_a.workspace_id != window_b.workspace_id {
        log::warn!(
            "swap_windows: windows in different workspaces ({} vs {})",
            window_a.workspace_id,
            window_b.workspace_id
        );
        return;
    }

    let workspace_id = window_a.workspace_id;

    // Swap positions in window_ids
    state.update_workspace(workspace_id, |ws| {
        let pos_a = ws.window_ids.iter().position(|&id| id == window_id_a);
        let pos_b = ws.window_ids.iter().position(|&id| id == window_id_b);

        if let (Some(a), Some(b)) = (pos_a, pos_b) {
            ws.window_ids.swap(a, b);
        }
    });

    log::debug!("Swapped windows {window_id_a} <-> {window_id_b}");

    // Notify subscriber to recalculate layout
    if let Some(handle) = get_subscriber_handle() {
        handle.notify_layout_changed(workspace_id, true);
    }
}

// ============================================================================
// Toggle Floating
// ============================================================================

/// Toggle floating state for a window.
pub fn on_toggle_floating(state: &mut TilingState, window_id: u32) {
    let Some(window) = state.get_window(window_id) else {
        log::warn!("toggle_floating: window {window_id} not found");
        return;
    };

    let workspace_id = window.workspace_id;
    let new_floating = !window.is_floating;
    state.update_window(window_id, |w| {
        w.is_floating = new_floating;
    });

    log::debug!("Window {window_id} floating = {new_floating}");

    // Notify subscriber about floating change and layout recalculation
    if let Some(handle) = get_subscriber_handle() {
        handle.notify_floating_changed(window_id, new_floating);
        handle.notify_layout_changed(workspace_id, true);
    }
}

// ============================================================================
// Send Window to Screen
// ============================================================================

/// Send the focused window to another screen.
///
/// The window is moved to the visible workspace on the target screen.
pub fn on_send_window_to_screen(state: &mut TilingState, target_screen: &TargetScreen) {
    // Resolve target screen
    let target_screen_id = resolve_screen(state, target_screen);
    let Some(target_screen_id) = target_screen_id else {
        log::warn!(
            "send_window_to_screen: screen '{}' not found",
            target_screen.as_str()
        );
        return;
    };

    // Get focused window
    let focus = state.get_focus_state();
    let Some(window_id) = focus.focused_window_id else {
        log::debug!("send_window_to_screen: no focused window");
        return;
    };

    let Some(window) = state.get_window(window_id) else {
        log::debug!("send_window_to_screen: window {window_id} not found");
        return;
    };

    // Get current workspace
    let current_workspace_id = window.workspace_id;
    let Some(current_workspace) = state.get_workspace(current_workspace_id) else {
        log::debug!("send_window_to_screen: workspace not found");
        return;
    };

    // Don't move if already on target screen
    if current_workspace.screen_id == target_screen_id {
        log::debug!("send_window_to_screen: window already on target screen");
        return;
    }

    // Find visible workspace on target screen
    let target_workspace_id = state
        .workspaces
        .iter()
        .find(|ws| ws.screen_id == target_screen_id && ws.is_visible)
        .map(|ws| ws.id);

    let Some(target_workspace_id) = target_workspace_id else {
        log::warn!("send_window_to_screen: no visible workspace on target screen");
        return;
    };

    // Use move_window_to_workspace to do the actual work
    on_move_window_to_workspace(state, window_id, target_workspace_id);
    log::debug!("Sent window {window_id} to screen '{}'", target_screen.as_str());
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::tiling::state::{Screen, Window, Workspace};

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

        // Add workspace
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

    fn add_window_to_workspace(state: &mut TilingState, window_id: u32, workspace_id: Uuid) {
        let window = Window {
            id: window_id,
            workspace_id,
            ..Default::default()
        };
        state.upsert_window(window);
        state.update_workspace(workspace_id, |ws| {
            ws.window_ids.push(window_id);
        });
    }

    #[test]
    fn test_move_window_to_workspace() {
        let mut state = create_test_state();
        let ws1_id = state.get_workspace_by_name("workspace1").unwrap().id;
        let ws2_id = state.get_workspace_by_name("workspace2").unwrap().id;

        add_window_to_workspace(&mut state, 100, ws1_id);

        on_move_window_to_workspace(&mut state, 100, ws2_id);

        // Window should be in new workspace
        let window = state.get_window(100).unwrap();
        assert_eq!(window.workspace_id, ws2_id);

        // Old workspace shouldn't have the window
        let ws1 = state.get_workspace(ws1_id).unwrap();
        assert!(!ws1.window_ids.contains(&100));

        // New workspace should have the window
        let ws2 = state.get_workspace(ws2_id).unwrap();
        assert!(ws2.window_ids.contains(&100));
    }

    #[test]
    fn test_swap_windows() {
        let mut state = create_test_state();
        let ws_id = state.get_workspace_by_name("workspace1").unwrap().id;

        add_window_to_workspace(&mut state, 100, ws_id);
        add_window_to_workspace(&mut state, 200, ws_id);
        add_window_to_workspace(&mut state, 300, ws_id);

        // Initial order: [100, 200, 300]
        let ws = state.get_workspace(ws_id).unwrap();
        assert_eq!(ws.window_ids.as_slice(), &[100, 200, 300]);

        // Swap 100 and 300
        on_swap_windows(&mut state, 100, 300);

        // New order: [300, 200, 100]
        let ws = state.get_workspace(ws_id).unwrap();
        assert_eq!(ws.window_ids.as_slice(), &[300, 200, 100]);
    }

    #[test]
    fn test_toggle_floating() {
        let mut state = create_test_state();
        let ws_id = state.get_workspace_by_name("workspace1").unwrap().id;

        add_window_to_workspace(&mut state, 100, ws_id);

        // Initially not floating
        let window = state.get_window(100).unwrap();
        assert!(!window.is_floating);

        // Toggle on
        on_toggle_floating(&mut state, 100);
        let window = state.get_window(100).unwrap();
        assert!(window.is_floating);

        // Toggle off
        on_toggle_floating(&mut state, 100);
        let window = state.get_window(100).unwrap();
        assert!(!window.is_floating);
    }
}
