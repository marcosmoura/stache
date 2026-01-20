//! Floating preset command handlers.
//!
//! These handlers manage applying floating presets to windows.

use crate::modules::tiling::state::{LayoutType, TilingState};

// ============================================================================
// Floating Preset Commands
// ============================================================================

/// Apply a floating preset to the focused window.
///
/// Presets define window size and position relative to the screen (centered, half-screen, etc.).
/// Only works when the workspace is in Floating layout mode.
///
/// # Arguments
///
/// * `state` - The tiling state
/// * `preset_name` - Name of the preset to apply (case-insensitive)
pub fn on_apply_preset(state: &mut TilingState, preset_name: &str) {
    use crate::config::get_config;
    use crate::modules::tiling::layout::{Gaps, calculate_preset_frame, find_preset};

    // Find the preset
    let Some(preset) = find_preset(preset_name) else {
        log::warn!("apply_preset: preset '{preset_name}' not found");
        return;
    };

    let focus = state.get_focus_state();
    let Some(workspace_id) = focus.focused_workspace_id else {
        log::debug!("apply_preset: no focused workspace");
        return;
    };

    let Some(workspace) = state.get_workspace(workspace_id) else {
        log::debug!("apply_preset: workspace not found");
        return;
    };

    // Presets only work in floating layout
    if workspace.layout != LayoutType::Floating {
        log::debug!(
            "apply_preset: workspace '{}' is not in Floating layout (is {:?})",
            workspace.name,
            workspace.layout
        );
        return;
    }

    let focused_idx = workspace.focused_window_index.unwrap_or(0);
    let Some(&window_id) = workspace.window_ids.get(focused_idx) else {
        log::debug!("apply_preset: no window at focused index");
        return;
    };

    let Some(screen) = state.get_screen(workspace.screen_id) else {
        log::debug!("apply_preset: screen not found");
        return;
    };

    // Get gaps from config
    let config = get_config();
    let bar_offset = if config.bar.is_enabled() {
        f64::from(config.bar.height) + f64::from(config.bar.padding)
    } else {
        0.0
    };
    let gaps = Gaps::from_config(&config.tiling.gaps, &screen.name, screen.is_main, bar_offset);

    // Calculate the target frame
    let target_frame = calculate_preset_frame(&preset, &screen.visible_frame, &gaps);

    // Get current frame for animation
    let current_frame = state.get_window(window_id).map(|w| w.frame);

    // Update window frame in state
    state.update_window(window_id, |w| {
        w.frame = target_frame;
    });

    // Apply the frame with animation
    if let Some(from_frame) = current_frame {
        use crate::modules::tiling::effects::{AnimationSystem, WindowTransition};

        let animation = AnimationSystem::from_config();
        let transition = WindowTransition::new(window_id, from_frame, target_frame);
        let _ = animation.animate(vec![transition]);
    } else {
        // Fallback: no current frame, just set directly
        let _ =
            crate::modules::tiling::effects::window_ops::set_window_frame(window_id, &target_frame);
    }

    log::debug!(
        "Applied preset '{}' to window {window_id}: ({}, {}, {}, {})",
        preset_name,
        target_frame.x as i32,
        target_frame.y as i32,
        target_frame.width as i32,
        target_frame.height as i32
    );
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::tiling::state::{Rect, Screen, Window, Workspace};

    fn create_test_state() -> TilingState {
        let mut state = TilingState::new();

        // Add a screen with a visible frame
        let screen = Screen {
            id: 1,
            name: "Test Screen".to_string(),
            is_main: true,
            visible_frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            ..Default::default()
        };
        state.upsert_screen(screen);

        // Add workspace in Floating layout
        let mut ws1 = Workspace::new("workspace1");
        ws1.screen_id = 1;
        ws1.is_visible = true;
        ws1.is_focused = true;
        ws1.layout = LayoutType::Floating;
        let ws1_id = ws1.id;
        state.upsert_workspace(ws1);

        // Update focus state
        state.update_focus(|focus| {
            focus.focused_workspace_id = Some(ws1_id);
            focus.focused_screen_id = Some(1);
        });

        state
    }

    #[test]
    fn test_apply_preset_not_floating() {
        let mut state = create_test_state();
        let ws_id = state.get_workspace_by_name("workspace1").unwrap().id;

        // Change to non-floating layout
        state.update_workspace(ws_id, |ws| {
            ws.layout = LayoutType::Dwindle;
        });

        // Add a window
        let window = Window {
            id: 100,
            workspace_id: ws_id,
            frame: Rect::new(100.0, 100.0, 400.0, 300.0),
            ..Default::default()
        };
        state.upsert_window(window);
        state.update_workspace(ws_id, |ws| {
            ws.window_ids.push(100);
            ws.focused_window_index = Some(0);
        });

        // Try to apply preset (should be ignored)
        let original_frame = state.get_window(100).unwrap().frame;
        on_apply_preset(&mut state, "center");

        // Frame should be unchanged since not in floating layout
        let current_frame = state.get_window(100).unwrap().frame;
        assert_eq!(original_frame.x, current_frame.x);
        assert_eq!(original_frame.y, current_frame.y);
    }

    #[test]
    fn test_apply_preset_no_window() {
        let mut state = create_test_state();

        // Try to apply preset with no windows (should not panic)
        on_apply_preset(&mut state, "center");
    }

    #[test]
    fn test_apply_preset_invalid() {
        let mut state = create_test_state();
        let ws_id = state.get_workspace_by_name("workspace1").unwrap().id;

        // Add a window
        let window = Window {
            id: 100,
            workspace_id: ws_id,
            frame: Rect::new(100.0, 100.0, 400.0, 300.0),
            ..Default::default()
        };
        state.upsert_window(window);
        state.update_workspace(ws_id, |ws| {
            ws.window_ids.push(100);
            ws.focused_window_index = Some(0);
        });

        // Try to apply invalid preset (should not panic)
        on_apply_preset(&mut state, "nonexistent_preset_xyz");
    }
}
