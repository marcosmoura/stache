//! Application event handlers for the state actor.
//!
//! These handlers process application lifecycle events:
//! - App launched → prepare for new windows from this app
//! - App terminated → remove all windows from this app
//! - App hidden → mark all windows from this app as hidden
//! - App shown → mark all windows from this app as visible
//! - App activated → potentially switch workspace

use std::collections::HashSet;

use uuid::Uuid;

use crate::modules::tiling::init::get_subscriber_handle;
use crate::modules::tiling::state::TilingState;

/// Handles an app launched event.
///
/// This is called when a new application starts. Windows from this app
/// will be tracked as they are created via window events.
pub fn on_app_launched(state: &mut TilingState, pid: i32, bundle_id: &str, name: &str) {
    log::debug!("Handling app launched: pid={pid}, bundle={bundle_id}, name={name}");

    // Nothing to do immediately - windows will be tracked as they're created
    // via WindowCreated events. We just log for debugging.
    let _ = state;
}

/// Handles an app terminated event.
///
/// Removes all windows belonging to this application from tracking.
/// Returns the set of affected workspace IDs (for layout recomputation).
pub fn on_app_terminated(state: &mut TilingState, pid: i32) -> HashSet<Uuid> {
    log::debug!("Handling app terminated: pid={pid}");

    // Find all windows for this PID
    let window_ids: Vec<u32> = state.get_windows_for_pid(pid).iter().map(|w| w.id).collect();

    if window_ids.is_empty() {
        log::debug!("No windows to remove for pid {pid}");
        return HashSet::new();
    }

    let count = window_ids.len();
    log::debug!("Removing {count} windows for pid {pid}");

    // Track affected workspaces
    let mut affected_workspaces: HashSet<Uuid> = HashSet::new();

    // Remove each window
    for window_id in &window_ids {
        // Get workspace before removing
        let workspace_id = state.get_window(*window_id).map(|w| w.workspace_id);

        // Remove from state
        state.remove_window(*window_id);

        // Remove from workspace's window list
        if let Some(ws_id) = workspace_id {
            affected_workspaces.insert(ws_id);

            state.update_workspace(ws_id, |ws| {
                ws.window_ids.retain(|id| *id != *window_id);

                // Update focused window index if needed
                if let Some(idx) = ws.focused_window_index {
                    if ws.window_ids.is_empty() {
                        ws.focused_window_index = None;
                    } else if idx >= ws.window_ids.len() {
                        ws.focused_window_index = Some(ws.window_ids.len().saturating_sub(1));
                    }
                }
            });
        }
    }

    // Clear focus if any removed window was focused
    let focus = eyeball::Observable::get(&state.focus);
    if focus
        .focused_window_id
        .is_some_and(|focused_id| window_ids.contains(&focused_id))
    {
        state.clear_focus();
    }

    // Notify subscriber to recompute layouts for affected workspaces
    if let Some(handle) = get_subscriber_handle() {
        for ws_id in &affected_workspaces {
            handle.notify_layout_changed(*ws_id, false);
        }
    }

    affected_workspaces
}

/// Handles an app hidden event (Cmd+H).
///
/// Marks all windows belonging to this application as hidden.
/// Hidden windows are excluded from layout calculations.
pub fn on_app_hidden(state: &mut TilingState, pid: i32) {
    log::debug!("Handling app hidden: pid={pid}");

    // Find all windows for this PID and mark as hidden
    let window_ids: Vec<u32> = state.get_windows_for_pid(pid).iter().map(|w| w.id).collect();

    for window_id in window_ids {
        state.update_window(window_id, |w| {
            w.is_hidden = true;
        });
    }
}

/// Handles an app shown event.
///
/// Marks all windows belonging to this application as visible.
pub fn on_app_shown(state: &mut TilingState, pid: i32) {
    log::debug!("Handling app shown: pid={pid}");

    // Find all windows for this PID and mark as visible
    let window_ids: Vec<u32> = state.get_windows_for_pid(pid).iter().map(|w| w.id).collect();

    for window_id in window_ids {
        state.update_window(window_id, |w| {
            w.is_hidden = false;
        });
    }
}

/// Handles an app activated event (brought to front).
///
/// This is informational - focus changes happen via window focus events.
pub fn on_app_activated(state: &mut TilingState, pid: i32) {
    log::debug!("Handling app activated: pid={pid}");

    // Nothing specific to do - focus will be handled by window focus events
    let _ = state;
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use smallvec::smallvec;

    use super::*;
    use crate::modules::tiling::state::{LayoutType, Rect, Window, WindowIdList, Workspace};

    fn make_state_with_workspace() -> (TilingState, Uuid) {
        let mut state = TilingState::new();

        let ws = Workspace {
            id: Uuid::now_v7(),
            name: "test".to_string(),
            screen_id: 1,
            layout: LayoutType::Dwindle,
            is_visible: true,
            is_focused: true,
            window_ids: WindowIdList::new(),
            focused_window_index: None,
            split_ratios: Vec::new(),
            configured_screen: None,
        };
        let ws_id = ws.id;
        state.upsert_workspace(ws);

        (state, ws_id)
    }

    fn make_window(id: u32, pid: i32, workspace_id: Uuid) -> Window {
        Window {
            id,
            pid,
            app_id: format!("com.test.app{pid}"),
            app_name: format!("App {pid}"),
            title: format!("Window {id}"),
            frame: Rect::new(0.0, 0.0, 800.0, 600.0),
            minimum_size: None,
            inferred_minimum_size: None,
            expected_frame: None,
            workspace_id,
            is_minimized: false,
            is_fullscreen: false,
            is_hidden: false,
            is_floating: false,
            tab_group_id: None,
            is_active_tab: true,
            matched_rule: None,
        }
    }

    #[test]
    fn test_app_terminated_removes_windows() {
        let (mut state, ws_id) = make_state_with_workspace();

        // Add windows from same app (pid 1000)
        let win1 = make_window(100, 1000, ws_id);
        let win2 = make_window(200, 1000, ws_id);
        let win3 = make_window(300, 2000, ws_id); // Different app

        state.upsert_window(win1);
        state.upsert_window(win2);
        state.upsert_window(win3);

        state.update_workspace(ws_id, |ws| {
            ws.window_ids = smallvec![100, 200, 300];
        });

        assert_eq!(state.windows.len(), 3);

        // Terminate app 1000
        let affected = on_app_terminated(&mut state, 1000);

        // Only window from app 2000 should remain
        assert_eq!(state.windows.len(), 1);
        assert!(state.get_window(300).is_some());
        assert!(state.get_window(100).is_none());
        assert!(state.get_window(200).is_none());

        // Workspace should only have window 300
        let ws = state.get_workspace(ws_id).unwrap();
        assert_eq!(ws.window_ids.as_slice(), &[300]);

        // Should report the affected workspace
        assert!(affected.contains(&ws_id));
    }

    #[test]
    fn test_app_hidden_marks_windows_hidden() {
        let (mut state, ws_id) = make_state_with_workspace();

        let win1 = make_window(100, 1000, ws_id);
        let win2 = make_window(200, 1000, ws_id);
        state.upsert_window(win1);
        state.upsert_window(win2);

        // Initially not hidden
        assert!(!state.get_window(100).unwrap().is_hidden);
        assert!(!state.get_window(200).unwrap().is_hidden);

        // Hide app
        on_app_hidden(&mut state, 1000);

        // Should be hidden
        assert!(state.get_window(100).unwrap().is_hidden);
        assert!(state.get_window(200).unwrap().is_hidden);
    }

    #[test]
    fn test_app_shown_marks_windows_visible() {
        let (mut state, ws_id) = make_state_with_workspace();

        let mut win1 = make_window(100, 1000, ws_id);
        let mut win2 = make_window(200, 1000, ws_id);
        win1.is_hidden = true;
        win2.is_hidden = true;
        state.upsert_window(win1);
        state.upsert_window(win2);

        // Initially hidden
        assert!(state.get_window(100).unwrap().is_hidden);
        assert!(state.get_window(200).unwrap().is_hidden);

        // Show app
        on_app_shown(&mut state, 1000);

        // Should be visible
        assert!(!state.get_window(100).unwrap().is_hidden);
        assert!(!state.get_window(200).unwrap().is_hidden);
    }

    #[test]
    fn test_app_terminated_clears_focus_if_needed() {
        let (mut state, ws_id) = make_state_with_workspace();

        let win1 = make_window(100, 1000, ws_id);
        state.upsert_window(win1);

        // Focus the window
        state.set_focus(Some(100), Some(ws_id), Some(1));
        assert!(eyeball::Observable::get(&state.focus).has_focus());

        // Terminate app
        let affected = on_app_terminated(&mut state, 1000);

        // Focus should be cleared
        assert!(!eyeball::Observable::get(&state.focus).has_focus());

        // Should report the affected workspace
        assert!(affected.contains(&ws_id));
    }
}
