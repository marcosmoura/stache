//! Focus command handlers.
//!
//! These handlers manage focus cycling, directional focus, and swapping
//! windows in a direction.

use crate::modules::tiling::actor::{CycleDirection, FocusDirection};
use crate::modules::tiling::init::get_subscriber_handle;
use crate::modules::tiling::state::{Rect, TilingState};

// ============================================================================
// Focus Cycling
// ============================================================================

/// Cycle focus through windows in the current workspace.
pub fn on_cycle_focus(state: &mut TilingState, direction: CycleDirection) {
    let focus = state.get_focus_state();
    let Some(workspace_id) = focus.focused_workspace_id else {
        log::debug!("cycle_focus: no focused workspace");
        return;
    };

    let Some(workspace) = state.get_workspace(workspace_id) else {
        log::debug!("cycle_focus: workspace {workspace_id} not found");
        return;
    };

    // Get layoutable windows (exclude minimized, hidden, etc.)
    let layoutable: Vec<u32> = workspace
        .window_ids
        .iter()
        .filter(|&&id| state.get_window(id).is_some_and(|w| w.is_layoutable()))
        .copied()
        .collect();

    if layoutable.is_empty() {
        log::debug!("cycle_focus: no layoutable windows");
        return;
    }

    // Find current focused position
    let current_idx = focus
        .focused_window_id
        .and_then(|id| layoutable.iter().position(|&w| w == id))
        .unwrap_or(0);

    // Calculate next index
    let next_idx = match direction {
        CycleDirection::Next => (current_idx + 1) % layoutable.len(),
        CycleDirection::Previous => {
            if current_idx == 0 {
                layoutable.len() - 1
            } else {
                current_idx - 1
            }
        }
    };

    let next_window_id = layoutable[next_idx];

    // Update focus state
    state.update_focus(|focus| {
        focus.focused_window_id = Some(next_window_id);
    });

    // Update workspace focused window index
    if let Some(idx) = workspace.window_ids.iter().position(|&id| id == next_window_id) {
        state.update_workspace(workspace_id, |ws| {
            ws.focused_window_index = Some(idx);
        });
    }

    log::debug!("Cycled focus to window {next_window_id} ({direction:?})");

    // Notify subscriber to actually focus the window
    if let Some(handle) = get_subscriber_handle() {
        handle.notify_focus_changed();
    }

    // Actually focus the window via AX API
    let _ = crate::modules::tiling::effects::window_ops::focus_window(next_window_id);
}

// ============================================================================
// Directional Focus
// ============================================================================

/// Focus a window in a direction.
///
/// Supports both spatial directions (up/down/left/right) and cycling (next/previous).
pub fn on_focus_window(state: &mut TilingState, direction: FocusDirection) {
    log::debug!("on_focus_window called with direction={direction:?}");

    // For next/previous, delegate to cycle_focus
    if !direction.is_spatial() {
        let cycle_dir = match direction {
            FocusDirection::Next => CycleDirection::Next,
            FocusDirection::Previous => CycleDirection::Previous,
            _ => return,
        };
        on_cycle_focus(state, cycle_dir);
        return;
    }

    // Spatial focus (up/down/left/right)
    let focus = state.get_focus_state();
    let Some(workspace_id) = focus.focused_workspace_id else {
        log::debug!("focus_window: no focused workspace in state");
        return;
    };

    let Some(focused_window_id) = focus.focused_window_id else {
        log::debug!("focus_window: no focused window in state");
        return;
    };

    let Some(workspace) = state.get_workspace(workspace_id) else {
        log::debug!("focus_window: workspace {workspace_id} not found");
        return;
    };

    // Get layoutable windows only
    let layoutable: Vec<u32> = workspace
        .window_ids
        .iter()
        .filter(|&&id| state.get_window(id).is_some_and(|w| w.is_layoutable()))
        .copied()
        .collect();

    if layoutable.len() < 2 {
        log::debug!("focus_window: need at least 2 windows for spatial focus");
        return;
    }

    // Get current window's frame
    let Some(from_window) = state.get_window(focused_window_id) else {
        log::debug!("focus_window: focused window {focused_window_id} not found in state");
        return;
    };

    // Find best candidate in the direction
    let candidate = find_window_in_direction(state, &from_window.frame, direction, &layoutable);

    if let Some(target_window_id) = candidate {
        // Update focus state
        state.update_focus(|focus| {
            focus.focused_window_id = Some(target_window_id);
        });

        // Update workspace focused window index
        if let Some(idx) = workspace.window_ids.iter().position(|&id| id == target_window_id) {
            state.update_workspace(workspace_id, |ws| {
                ws.focused_window_index = Some(idx);
            });
        }

        log::debug!("Focused window {target_window_id} ({direction:?})");

        // Notify subscriber about focus change
        if let Some(handle) = get_subscriber_handle() {
            handle.notify_focus_changed();
        }

        // Actually focus the window via AX API
        let _ = crate::modules::tiling::effects::window_ops::focus_window(target_window_id);
    } else {
        log::debug!("focus_window: no window found in direction {direction:?}");
    }
}

// ============================================================================
// Swap in Direction
// ============================================================================

/// Swap focused window with another in a direction.
///
/// Supports both spatial directions (up/down/left/right) and cycling (next/previous).
pub fn on_swap_window_in_direction(state: &mut TilingState, direction: FocusDirection) {
    let focus = state.get_focus_state();
    let Some(workspace_id) = focus.focused_workspace_id else {
        log::debug!("swap_in_direction: no focused workspace");
        return;
    };

    let Some(focused_window_id) = focus.focused_window_id else {
        log::debug!("swap_in_direction: no focused window");
        return;
    };

    let Some(workspace) = state.get_workspace(workspace_id) else {
        log::debug!("swap_in_direction: workspace {workspace_id} not found");
        return;
    };

    let all_window_ids = workspace.window_ids.clone();

    // Get layoutable windows for finding swap targets
    let layoutable: Vec<u32> = all_window_ids
        .iter()
        .filter(|&&id| state.get_window(id).is_some_and(|w| w.is_layoutable()))
        .copied()
        .collect();

    if layoutable.len() < 2 {
        log::debug!("swap_in_direction: need at least 2 windows to swap");
        return;
    }

    // Check if focused window is layoutable
    if !layoutable.contains(&focused_window_id) {
        log::debug!("swap_in_direction: focused window is not layoutable");
        return;
    }

    // Find the target window to swap with
    let target_id = if direction.is_spatial() {
        // Spatial swap
        let Some(from_window) = state.get_window(focused_window_id) else {
            return;
        };
        find_window_in_direction(state, &from_window.frame, direction, &layoutable)
    } else {
        // Cycle-based swap
        let current_idx = layoutable.iter().position(|&id| id == focused_window_id).unwrap_or(0);
        let target_idx = match direction {
            FocusDirection::Next => (current_idx + 1) % layoutable.len(),
            FocusDirection::Previous => {
                if current_idx == 0 {
                    layoutable.len() - 1
                } else {
                    current_idx - 1
                }
            }
            _ => return,
        };
        Some(layoutable[target_idx])
    };

    let Some(target_window_id) = target_id else {
        log::debug!("swap_in_direction: no target found in direction {direction:?}");
        return;
    };

    if target_window_id == focused_window_id {
        return;
    }

    // Swap positions in window_ids (using original list, not layoutable)
    state.update_workspace(workspace_id, |ws| {
        let pos_a = ws.window_ids.iter().position(|&id| id == focused_window_id);
        let pos_b = ws.window_ids.iter().position(|&id| id == target_window_id);

        if let (Some(a), Some(b)) = (pos_a, pos_b) {
            ws.window_ids.swap(a, b);
            // Keep focus on the originally focused window (now at position b)
            ws.focused_window_index = Some(b);
        }
    });

    log::debug!("Swapped windows {focused_window_id} <-> {target_window_id} ({direction:?})");

    // Notify subscriber to recalculate layout
    if let Some(handle) = get_subscriber_handle() {
        handle.notify_layout_changed(workspace_id, true);
    }
}

// ============================================================================
// Direction Helper
// ============================================================================

/// Find the nearest window in a spatial direction.
///
/// Uses weighted distance to prefer windows that are more aligned with the direction.
pub fn find_window_in_direction(
    state: &TilingState,
    from_frame: &Rect,
    direction: FocusDirection,
    window_ids: &[u32],
) -> Option<u32> {
    let from_center_x = from_frame.x + from_frame.width / 2.0;
    let from_center_y = from_frame.y + from_frame.height / 2.0;

    let mut best_candidate: Option<(u32, f64)> = None;

    for &window_id in window_ids {
        let Some(window) = state.get_window(window_id) else {
            continue;
        };

        // Skip if same frame (same window)
        if (window.frame.x - from_frame.x).abs() < 1.0
            && (window.frame.y - from_frame.y).abs() < 1.0
            && (window.frame.width - from_frame.width).abs() < 1.0
            && (window.frame.height - from_frame.height).abs() < 1.0
        {
            continue;
        }

        let center_x = window.frame.x + window.frame.width / 2.0;
        let center_y = window.frame.y + window.frame.height / 2.0;

        // Check if this window is in the right direction
        let is_valid = match direction {
            FocusDirection::Up => center_y < from_center_y,
            FocusDirection::Down => center_y > from_center_y,
            FocusDirection::Left => center_x < from_center_x,
            FocusDirection::Right => center_x > from_center_x,
            _ => false,
        };

        if !is_valid {
            continue;
        }

        // Calculate distance
        let dx = center_x - from_center_x;
        let dy = center_y - from_center_y;
        let distance = dx * dx + dy * dy;

        // Apply alignment penalty (prefer windows aligned with direction)
        let weighted_distance = match direction {
            FocusDirection::Up | FocusDirection::Down => {
                // Prefer vertically aligned windows
                let alignment_penalty = dx.abs() * 2.0;
                distance + alignment_penalty * alignment_penalty
            }
            FocusDirection::Left | FocusDirection::Right => {
                // Prefer horizontally aligned windows
                let alignment_penalty = dy.abs() * 2.0;
                distance + alignment_penalty * alignment_penalty
            }
            _ => distance,
        };

        if best_candidate.is_none() || weighted_distance < best_candidate.unwrap().1 {
            best_candidate = Some((window_id, weighted_distance));
        }
    }

    best_candidate.map(|(id, _)| id)
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

        // Update focus state
        state.update_focus(|focus| {
            focus.focused_workspace_id = Some(ws1_id);
            focus.focused_screen_id = Some(1);
        });

        state
    }

    fn add_window(state: &mut TilingState, window_id: u32, x: f64, y: f64, w: f64, h: f64) {
        let ws_id = state.get_focus_state().focused_workspace_id.unwrap();
        let window = Window {
            id: window_id,
            workspace_id: ws_id,
            frame: Rect::new(x, y, w, h),
            ..Default::default()
        };
        state.upsert_window(window);
        state.update_workspace(ws_id, |ws| {
            ws.window_ids.push(window_id);
        });
    }

    #[test]
    fn test_cycle_focus() {
        let mut state = create_test_state();
        add_window(&mut state, 100, 0.0, 0.0, 400.0, 300.0);
        add_window(&mut state, 200, 400.0, 0.0, 400.0, 300.0);
        add_window(&mut state, 300, 0.0, 300.0, 800.0, 300.0);

        // Set initial focus
        state.update_focus(|f| f.focused_window_id = Some(100));

        // Cycle next
        on_cycle_focus(&mut state, CycleDirection::Next);
        assert_eq!(state.get_focus_state().focused_window_id, Some(200));

        // Cycle next again
        on_cycle_focus(&mut state, CycleDirection::Next);
        assert_eq!(state.get_focus_state().focused_window_id, Some(300));

        // Cycle next (wrap around)
        on_cycle_focus(&mut state, CycleDirection::Next);
        assert_eq!(state.get_focus_state().focused_window_id, Some(100));

        // Cycle previous
        on_cycle_focus(&mut state, CycleDirection::Previous);
        assert_eq!(state.get_focus_state().focused_window_id, Some(300));
    }

    #[test]
    fn test_find_window_in_direction() {
        let mut state = create_test_state();

        // Create a 2x2 grid of windows
        add_window(&mut state, 1, 0.0, 0.0, 400.0, 300.0); // top-left
        add_window(&mut state, 2, 400.0, 0.0, 400.0, 300.0); // top-right
        add_window(&mut state, 3, 0.0, 300.0, 400.0, 300.0); // bottom-left
        add_window(&mut state, 4, 400.0, 300.0, 400.0, 300.0); // bottom-right

        let window_ids = vec![1, 2, 3, 4];

        // From top-left, find right
        let from_frame = Rect::new(0.0, 0.0, 400.0, 300.0);
        let result =
            find_window_in_direction(&state, &from_frame, FocusDirection::Right, &window_ids);
        assert_eq!(result, Some(2));

        // From top-left, find down
        let result =
            find_window_in_direction(&state, &from_frame, FocusDirection::Down, &window_ids);
        assert_eq!(result, Some(3));

        // From bottom-right, find left
        let from_frame = Rect::new(400.0, 300.0, 400.0, 300.0);
        let result =
            find_window_in_direction(&state, &from_frame, FocusDirection::Left, &window_ids);
        assert_eq!(result, Some(3));

        // From bottom-right, find up
        let result = find_window_in_direction(&state, &from_frame, FocusDirection::Up, &window_ids);
        assert_eq!(result, Some(2));
    }
}
