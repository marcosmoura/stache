//! Layout command handlers.
//!
//! These handlers manage layout switching and cycling.

use uuid::Uuid;

use crate::modules::tiling::init::get_subscriber_handle;
use crate::modules::tiling::state::{LayoutType, TilingState};

// ============================================================================
// Layout Commands
// ============================================================================

/// Set the layout for a workspace.
pub fn on_set_layout(state: &mut TilingState, workspace_id: Uuid, layout: LayoutType) {
    state.update_workspace(workspace_id, |ws| {
        ws.layout = layout;
        // Clear split ratios when layout changes
        ws.split_ratios.clear();
    });

    log::debug!("Set workspace {workspace_id} layout to {layout:?}");

    // Notify subscriber about layout change
    if let Some(handle) = get_subscriber_handle() {
        handle.notify_workspace_layout_changed(workspace_id, layout);
        handle.notify_layout_changed(workspace_id, true);
    }
}

/// Cycle through layouts for a workspace.
pub fn on_cycle_layout(state: &mut TilingState, workspace_id: Uuid) {
    let Some(workspace) = state.get_workspace(workspace_id) else {
        log::warn!("cycle_layout: workspace {workspace_id} not found");
        return;
    };

    let next_layout = match workspace.layout {
        LayoutType::Floating => LayoutType::Dwindle,
        LayoutType::Dwindle => LayoutType::Monocle,
        LayoutType::Monocle => LayoutType::Master,
        LayoutType::Master => LayoutType::Split,
        LayoutType::Split => LayoutType::SplitVertical,
        LayoutType::SplitVertical => LayoutType::SplitHorizontal,
        LayoutType::SplitHorizontal => LayoutType::Grid,
        LayoutType::Grid => LayoutType::Floating,
    };

    on_set_layout(state, workspace_id, next_layout);
    log::debug!("Cycled workspace {workspace_id} layout to {next_layout:?}");
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::tiling::state::{Screen, Workspace};

    fn create_test_state() -> (TilingState, Uuid) {
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

        // Update focus state
        state.update_focus(|focus| {
            focus.focused_workspace_id = Some(ws1_id);
            focus.focused_screen_id = Some(1);
        });

        (state, ws1_id)
    }

    #[test]
    fn test_set_layout() {
        let (mut state, ws_id) = create_test_state();

        on_set_layout(&mut state, ws_id, LayoutType::Master);

        let ws = state.get_workspace(ws_id).unwrap();
        assert_eq!(ws.layout, LayoutType::Master);
    }

    #[test]
    fn test_cycle_layout() {
        let (mut state, ws_id) = create_test_state();

        // Start with default (Floating)
        on_set_layout(&mut state, ws_id, LayoutType::Floating);

        // Cycle through
        on_cycle_layout(&mut state, ws_id);
        assert_eq!(state.get_workspace(ws_id).unwrap().layout, LayoutType::Dwindle);

        on_cycle_layout(&mut state, ws_id);
        assert_eq!(state.get_workspace(ws_id).unwrap().layout, LayoutType::Monocle);

        on_cycle_layout(&mut state, ws_id);
        assert_eq!(state.get_workspace(ws_id).unwrap().layout, LayoutType::Master);
    }
}
