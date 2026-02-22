//! Window event handlers for the state actor.
//!
//! These handlers process window lifecycle events and update the state accordingly:
//! - Window created → track the window, assign to workspace
//! - Window destroyed → untrack the window
//! - Window focused → update focus state
//! - Window moved/resized → update frame
//! - Window minimized/fullscreen → update state flags

use uuid::Uuid;

use crate::modules::tiling::actor::messages::{
    GeometryUpdate, GeometryUpdateType, WindowCreatedInfo,
};
use crate::modules::tiling::effects::{get_window_cache, should_ignore_geometry_events};
use crate::modules::tiling::init::get_subscriber_handle;
use crate::modules::tiling::state::{Rect, TilingState, Window, WindowIdList, Workspace};
use crate::modules::tiling::tabs;

/// Handles a window created event.
///
/// Creates a new tracked window and assigns it to a workspace.
/// The workspace is determined by:
/// 1. Window rules (if any match)
/// 2. The focused workspace on the window's screen
/// 3. A default workspace
pub fn on_window_created(state: &mut TilingState, info: WindowCreatedInfo) {
    let workspace_id = on_window_created_internal(state, info);

    // Notify subscriber that layout needs to be recomputed for this workspace
    if let (Some(ws_id), Some(handle)) = (workspace_id, get_subscriber_handle()) {
        handle.notify_layout_changed(ws_id, false);
    }
}

/// Handles a window created event without triggering layout notifications.
///
/// Used during batch initialization to track all windows before applying layouts.
pub fn on_window_created_silent(state: &mut TilingState, info: WindowCreatedInfo) {
    let _ = on_window_created_internal(state, info);
    // No notification - caller will trigger layout after batch is complete
}

/// Internal implementation of window creation.
///
/// Returns the workspace ID if a new window was created and layout should be triggered,
/// None if window was just updated or is a tab (no layout needed).
fn on_window_created_internal(state: &mut TilingState, info: WindowCreatedInfo) -> Option<Uuid> {
    tracing::debug!(
        "Handling window created: id={}, app={}, title='{}'",
        info.window_id,
        info.app_id,
        info.title
    );

    // Check if window already exists
    if state.get_window(info.window_id).is_some() {
        tracing::debug!("Window {} already tracked, updating", info.window_id);
        state.update_window(info.window_id, |w| {
            w.title.clone_from(&info.title);
            w.frame = info.frame;
            w.is_minimized = info.is_minimized;
            w.is_fullscreen = info.is_fullscreen;
        });
        return None;
    }

    // Scan and register tabs for this app to update the tab registry
    tabs::scan_and_register_tabs_for_app(info.pid);

    // Check if this window is a tab (already tracked in the tab registry)
    if tabs::is_tab(info.window_id) {
        // Don't track tabs in state at all - they're managed by the tab registry
        return None;
    }

    // Find workspace to assign the window to
    let workspace_id = find_workspace_for_window(state, &info);

    // Get workspace window IDs for tab detection
    let workspace_window_ids: Vec<u32> = state
        .get_workspace(workspace_id)
        .map(|ws| ws.window_ids.to_vec())
        .unwrap_or_default();

    // Check if this new window is a tab being added to an existing window
    if tabs::is_new_window_a_tab(info.pid, info.window_id, &workspace_window_ids) {
        // Register this as a tab and skip layout
        tabs::register_tab(info.window_id, info.pid);
        return None;
    }

    // Create the window (this is a real window, not a tab)
    let window = Window {
        id: info.window_id,
        pid: info.pid,
        app_id: info.app_id,
        app_name: info.app_name,
        title: info.title,
        frame: info.frame,
        minimum_size: info.minimum_size,
        inferred_minimum_size: None,
        expected_frame: None,
        workspace_id,
        is_minimized: info.is_minimized,
        is_fullscreen: info.is_fullscreen,
        is_hidden: false,
        is_floating: false,  // TODO: Check window rules for float
        tab_group_id: None,  // Not using old tab detection
        is_active_tab: true, // Real windows are always "active"
        matched_rule: None,  // TODO: Set from window rules
    };

    // Track window in state
    state.upsert_window(window);

    // Get the focused window in this workspace to insert after
    let focused_window_id = state
        .get_focused_window()
        .filter(|w| w.workspace_id == workspace_id)
        .map(|w| w.id);

    // Add window to workspace's window list, inserting after the focused window
    state.update_workspace(workspace_id, |ws| {
        if ws.window_ids.contains(&info.window_id) {
            return; // Already in list
        }

        // Find where to insert: after the focused window, or at the end if no focus
        let insert_index = focused_window_id
            .and_then(|focused_id| ws.window_ids.iter().position(|&id| id == focused_id))
            .map_or(ws.window_ids.len(), |idx| idx + 1); // Insert after focused window, or end of list

        ws.window_ids.insert(insert_index, info.window_id);
    });

    tracing::debug!(
        "Window {} tracked in workspace {:?} (after focused window {:?})",
        info.window_id,
        workspace_id,
        focused_window_id
    );

    Some(workspace_id)
}

/// Handles a window destroyed event.
///
/// Removes the window from tracking and from its workspace.
/// Returns the workspace ID if the window was tracked AND was a real window (for layout recomputation).
/// Tabs return None since they don't affect layout.
pub fn on_window_destroyed(state: &mut TilingState, window_id: u32) -> Option<uuid::Uuid> {
    tracing::debug!("tiling: handler on_window_destroyed called for window_id={window_id}");

    // Check if this window is a tracked tab - if so, just unregister and skip layout
    if tabs::is_tab(window_id) {
        tabs::unregister_tab(window_id);
        return None;
    }

    // Get the window info before removing
    let window_info = state.get_window(window_id).map(|w| w.workspace_id);

    let Some(workspace_id) = window_info else {
        tracing::debug!("tiling: window {window_id} was not tracked in state");
        // Still try to unregister from tab registry in case it was there
        tabs::unregister_tab(window_id);
        return None;
    };

    tracing::debug!("tiling: window {window_id} workspace_id={workspace_id:?}");

    // Remove the window from state
    state.remove_window(window_id);
    tracing::debug!("tiling: window {window_id} removed from state");

    // Invalidate window cache entry for this window
    get_window_cache().invalidate_window(window_id);

    // Remove from workspace's window list
    state.update_workspace(workspace_id, |ws| {
        let before_count = ws.window_ids.len();
        ws.window_ids.retain(|id| *id != window_id);
        let after_count = ws.window_ids.len();
        tracing::debug!(
            "tiling: workspace {workspace_id} window count: {before_count} -> {after_count}"
        );

        // Update focused window index if needed
        if let Some(idx) = ws.focused_window_index {
            if ws.window_ids.is_empty() {
                ws.focused_window_index = None;
            } else if idx >= ws.window_ids.len() {
                ws.focused_window_index = Some(ws.window_ids.len().saturating_sub(1));
            }
        }
    });

    // Clear focus if this was the focused window
    let focus = eyeball::Observable::get(&state.focus);
    if focus.focused_window_id == Some(window_id) {
        tracing::debug!("tiling: cleared focus since destroyed window was focused");
        state.clear_focus();
    }

    // Remove window from focus history (it may have been the last focused window in some workspace)
    state.remove_window_from_focus_history(window_id);
    tracing::debug!("tiling: removed window {window_id} from focus history");

    tracing::debug!("tiling: returning workspace_id={workspace_id} for layout recalculation");
    Some(workspace_id)
}

/// Handles a window focused event.
///
/// Updates the focus state to point to this window, its workspace, and screen.
/// Also updates workspace visibility - when a window is focused, its workspace
/// becomes visible (and any other workspace on the same screen becomes hidden).
pub fn on_window_focused(state: &mut TilingState, window_id: u32) {
    let Some(window) = state.get_window(window_id) else {
        tracing::trace!("Window {window_id} not tracked - ignoring focus event");
        return;
    };

    tracing::debug!(
        "Window {} focused -> workspace {} (app: {})",
        window_id,
        window.workspace_id,
        window.app_name
    );

    // Capture previous focus state to detect workspace changes
    let previous_focus = eyeball::Observable::get(&state.focus).clone();
    let previous_workspace_id = previous_focus.focused_workspace_id;

    let workspace = state.get_workspace(window.workspace_id);
    let screen_id = workspace.as_ref().map(|ws| ws.screen_id);

    // Update focus state
    state.set_focus(Some(window_id), Some(window.workspace_id), screen_id);

    // Update workspace's focused window index
    if let Some(ws) = workspace
        && let Some(idx) = ws.window_index(window_id)
    {
        state.update_workspace(ws.id, |ws| {
            ws.focused_window_index = Some(idx);
        });
    }

    // Get the screen ID for visibility updates
    let focused_ws_id = window.workspace_id;
    let focused_ws_screen_id = state.get_workspace(focused_ws_id).map(|ws| ws.screen_id);

    // Track workspaces that change visibility
    let mut workspaces_becoming_visible: Vec<Uuid> = Vec::new();
    let mut workspaces_becoming_hidden: Vec<Uuid> = Vec::new();

    // Update workspace focus AND visibility
    // - The focused workspace becomes visible and focused
    // - Other workspaces on the SAME screen become non-visible
    // - Workspaces on OTHER screens keep their visibility (they're visible on their own screen)
    for i in 0..state.workspaces.len() {
        let ws = &state.workspaces[i];
        let is_target_ws = ws.id == focused_ws_id;
        let on_same_screen = focused_ws_screen_id == Some(ws.screen_id);

        let should_focus = is_target_ws;
        let should_be_visible = if on_same_screen {
            // On the same screen, only the focused workspace should be visible
            is_target_ws
        } else {
            // On other screens, keep existing visibility
            ws.is_visible
        };

        // Track visibility changes
        if ws.is_visible != should_be_visible {
            if should_be_visible {
                workspaces_becoming_visible.push(ws.id);
            } else {
                workspaces_becoming_hidden.push(ws.id);
            }
        }

        if ws.is_focused != should_focus || ws.is_visible != should_be_visible {
            let id = ws.id;
            state.update_workspace(id, |ws| {
                ws.is_focused = should_focus;
                ws.is_visible = should_be_visible;
            });
        }
    }

    // Sync window visibility if any workspace visibility changed
    if !workspaces_becoming_visible.is_empty() || !workspaces_becoming_hidden.is_empty() {
        tracing::debug!(
            "Visibility changed - showing: {workspaces_becoming_visible:?}, hiding: {workspaces_becoming_hidden:?}"
        );

        sync_window_visibility_for_workspaces(
            state,
            &workspaces_becoming_visible,
            &workspaces_becoming_hidden,
        );

        // Notify subscriber to apply layouts for newly visible workspaces
        if let Some(handle) = get_subscriber_handle() {
            for ws_id in &workspaces_becoming_visible {
                handle.notify_layout_changed(*ws_id, false);
            }
        }
    }

    // Notify subscriber that focus changed
    if let Some(handle) = get_subscriber_handle() {
        handle.notify_focus_changed();
    }

    // Emit workspace changed event if the focused workspace changed
    let workspace_changed = previous_workspace_id != Some(window.workspace_id);
    if workspace_changed {
        // Get workspace and screen names for the event
        if let Some(ws) = state.get_workspace(window.workspace_id) {
            let screen_name = state.get_screen(ws.screen_id).map_or_else(
                || {
                    let screen_id = ws.screen_id;
                    format!("screen-{screen_id}")
                },
                |s| s.name,
            );

            let previous_workspace_name =
                previous_workspace_id.and_then(|id| state.get_workspace(id)).map(|ws| ws.name);

            crate::modules::tiling::init::emit_workspace_changed(
                &ws.name,
                &screen_name,
                previous_workspace_name.as_deref(),
            );
        }
    }
}

/// Syncs window visibility when workspaces change visibility.
///
/// - Shows (unhides) apps that have windows in newly visible workspaces
/// - Hides apps that have windows ONLY in hidden workspaces (not in any visible workspace)
pub fn sync_window_visibility_for_workspaces(
    state: &TilingState,
    becoming_visible: &[Uuid],
    becoming_hidden: &[Uuid],
) {
    use std::collections::HashSet;

    use crate::modules::tiling::effects::window_ops::{hide_app, unhide_app};

    if becoming_visible.is_empty() && becoming_hidden.is_empty() {
        tracing::trace!("No visibility changes to sync");
        return;
    }

    tracing::debug!(
        "Syncing visibility - becoming_visible: {becoming_visible:?}, becoming_hidden: {becoming_hidden:?}"
    );

    // Collect all currently visible workspace IDs
    let visible_ws_ids: HashSet<Uuid> =
        state.get_visible_workspaces().iter().map(|ws| ws.id).collect();

    // Collect PIDs from windows in becoming-visible workspaces (need to unhide)
    let mut pids_to_show: HashSet<i32> = HashSet::new();
    for ws_id in becoming_visible {
        for window in state.windows.iter().filter(|w| w.workspace_id == *ws_id) {
            pids_to_show.insert(window.pid);
        }
    }

    // Collect PIDs from windows in becoming-hidden workspaces
    let mut pids_in_hidden: HashSet<i32> = HashSet::new();
    for ws_id in becoming_hidden {
        for window in state.windows.iter().filter(|w| w.workspace_id == *ws_id) {
            pids_in_hidden.insert(window.pid);
        }
    }

    // Find PIDs that have windows in ANY visible workspace (shouldn't be hidden)
    let mut pids_in_visible: HashSet<i32> = HashSet::new();
    for window in state.windows.iter() {
        if visible_ws_ids.contains(&window.workspace_id) {
            pids_in_visible.insert(window.pid);
        }
    }

    // PIDs to hide: in hidden workspaces but NOT in any visible workspace
    let pids_to_hide: Vec<i32> = pids_in_hidden.difference(&pids_in_visible).copied().collect();

    tracing::trace!("PIDs to show: {pids_to_show:?}, PIDs to hide: {pids_to_hide:?}");

    // Show apps first (so they become visible before we hide others)
    for pid in &pids_to_show {
        let result = unhide_app(*pid);
        tracing::trace!("unhide_app({pid}) = {result}");
    }

    // Hide apps that only have windows in non-visible workspaces
    for pid in &pids_to_hide {
        let result = hide_app(*pid);
        tracing::trace!("hide_app({pid}) = {result}");
    }
}

/// Handles a window unfocused event.
///
/// Note: We don't clear focus here because another window will typically
/// receive focus immediately after. Focus is only cleared when explicitly needed.
pub fn on_window_unfocused(state: &mut TilingState, window_id: u32) {
    tracing::debug!("Handling window unfocused: {window_id}");

    // Just log for now - actual focus change happens in on_window_focused
    let _ = state.get_window(window_id);
}

/// Handles a window moved event.
pub fn on_window_moved(state: &mut TilingState, window_id: u32, frame: Rect) {
    use crate::modules::tiling::events::drag_state;

    // If a drag operation is in progress, just update the frame
    if drag_state::is_operation_in_progress() {
        state.update_window(window_id, |w| {
            w.frame = frame;
        });
        return;
    }

    // During animation (or the settling period after), ignore geometry events.
    // These are intermediate frames from the animation system and should not
    // trigger minimum size detection or update our tracked frame state.
    if should_ignore_geometry_events() {
        tracing::trace!(
            "on_window_moved: ignoring geometry event for window {window_id} during animation/settling"
        );
        return;
    }

    // Check for minimum size mismatch and update inferred minimum if needed
    // If detected, trigger a layout recalculation to restore valid positions
    if let (Some(workspace_id), Some(handle)) = (
        detect_and_update_inferred_minimum(state, window_id, &frame),
        get_subscriber_handle(),
    ) {
        handle.notify_layout_changed(workspace_id, false);
    }

    state.update_window(window_id, |w| {
        w.frame = frame;
    });
}

/// Handles a window resized event.
pub fn on_window_resized(state: &mut TilingState, window_id: u32, frame: Rect) {
    use crate::modules::tiling::events::drag_state;

    // If a drag operation is in progress, just update the frame
    if drag_state::is_operation_in_progress() {
        state.update_window(window_id, |w| {
            w.frame = frame;
        });
        return;
    }

    // During animation (or the settling period after), ignore geometry events.
    // These are intermediate frames from the animation system and should not
    // trigger minimum size detection or update our tracked frame state.
    if should_ignore_geometry_events() {
        return;
    }

    // Check for minimum size mismatch and update inferred minimum if needed
    // If detected, trigger a layout recalculation to restore valid positions
    if let (Some(workspace_id), Some(handle)) = (
        detect_and_update_inferred_minimum(state, window_id, &frame),
        get_subscriber_handle(),
    ) {
        handle.notify_layout_changed(workspace_id, false);
    }

    state.update_window(window_id, |w| {
        w.frame = frame;
    });
}

/// Detect if a window failed to resize to its expected frame and update inferred minimum size.
///
/// When a window reports a position/size that is larger than what we calculated,
/// it means the window refused to shrink below its minimum size. We record this
/// as `inferred_minimum_size` for future resize checks.
///
/// Returns the workspace ID if a minimum size mismatch was detected, so the caller
/// can trigger a layout recalculation to restore valid positions.
fn detect_and_update_inferred_minimum(
    state: &mut TilingState,
    window_id: u32,
    actual_frame: &Rect,
) -> Option<uuid::Uuid> {
    // Tolerance for position/size comparison (pixels)
    const TOLERANCE: f64 = 5.0;

    // Get the window's expected frame and workspace
    let Some(window) = state.get_window(window_id) else {
        tracing::trace!("detect_minimum: window {window_id} not found in state");
        return None;
    };

    let Some(expected_frame) = window.expected_frame else {
        return None; // No expected frame set, can't detect mismatch
    };

    // Check if the window already reports minimum_size (no need to infer)
    if window.minimum_size.is_some() {
        return None;
    }

    let workspace_id = window.workspace_id;

    // Check if actual frame is significantly larger than expected
    // This indicates the window refused to shrink to the expected size
    let width_mismatch = actual_frame.width > expected_frame.width + TOLERANCE;
    let height_mismatch = actual_frame.height > expected_frame.height + TOLERANCE;

    if width_mismatch || height_mismatch {
        // Window refused to shrink - infer minimum size from actual dimensions
        let inferred_min_width = if width_mismatch {
            actual_frame.width
        } else {
            window.inferred_minimum_size.map_or(0.0, |(w, _)| w)
        };

        let inferred_min_height = if height_mismatch {
            actual_frame.height
        } else {
            window.inferred_minimum_size.map_or(0.0, |(_, h)| h)
        };

        // Update the window's inferred minimum size
        state.update_window(window_id, |w| {
            w.inferred_minimum_size = Some((inferred_min_width, inferred_min_height));
        });

        // Return workspace ID so caller can trigger layout recalculation
        return Some(workspace_id);
    }

    None
}

/// Handles a window minimized/unminimized event.
pub fn on_window_minimized(state: &mut TilingState, window_id: u32, minimized: bool) {
    tracing::debug!("Handling window minimized: {window_id} = {minimized}");

    // Get workspace info before updating
    let workspace_info = state.get_window(window_id).and_then(|w| {
        state.get_workspace(w.workspace_id).map(|ws| (ws.id, ws.name, ws.window_ids))
    });

    state.update_window(window_id, |w| {
        w.is_minimized = minimized;
    });

    // Minimized state affects layout and window list
    if let Some((ws_id, ws_name, window_ids)) = workspace_info {
        // Notify subscriber to recalculate layout
        if let Some(handle) = get_subscriber_handle() {
            handle.notify_layout_changed(ws_id, false);
        }

        // Emit workspace windows changed event to frontend
        crate::modules::tiling::init::emit_workspace_windows_changed(&ws_name, &window_ids);
    }
}

/// Handles a window title changed event.
pub fn on_window_title_changed(state: &mut TilingState, window_id: u32, title: &str) {
    tracing::debug!("Handling window title changed: {window_id} to '{title}'");

    // Get window's workspace before updating
    let window_workspace_id = state.get_window(window_id).map(|w| w.workspace_id);

    state.update_window(window_id, |w| {
        w.title = title.to_string();
    });

    // Only emit event to frontend if window is in the focused workspace
    let focused_workspace_id = state.get_focus_state().focused_workspace_id;
    if window_workspace_id.is_some() && window_workspace_id == focused_workspace_id {
        crate::modules::tiling::init::emit_window_title_changed(window_id, title);
    }
}

/// Handles a window fullscreen state changed event.
pub fn on_window_fullscreen_changed(state: &mut TilingState, window_id: u32, fullscreen: bool) {
    tracing::debug!("Handling window fullscreen changed: {window_id} = {fullscreen}");

    // Get workspace before updating
    let workspace_id = state.get_window(window_id).map(|w| w.workspace_id);

    state.update_window(window_id, |w| {
        w.is_fullscreen = fullscreen;
    });

    // Fullscreen state affects layout
    if let (Some(ws_id), Some(handle)) = (workspace_id, get_subscriber_handle()) {
        handle.notify_layout_changed(ws_id, false);
    }
}

/// Handles a batch of geometry updates.
///
/// Updates frames for multiple windows at once, typically from the event processor.
/// Also checks for minimum size mismatches and triggers layout recalculation if needed.
///
/// When the mouse is down, this detects user-initiated resize/move operations and
/// tracks them via the `drag_state` module. Layout changes are frozen during drag
/// operations and ratios are calculated on mouse up.
pub fn on_batched_geometry_updates(state: &mut TilingState, updates: &[GeometryUpdate]) {
    use crate::modules::tiling::events::{drag_state, mouse_monitor};

    let mouse_down = mouse_monitor::is_mouse_down();
    let operation_in_progress = drag_state::is_operation_in_progress();

    // If mouse is down but no operation tracked yet, start tracking
    if mouse_down && !operation_in_progress && !updates.is_empty() {
        // Determine the operation type from the updates
        let has_resize = updates.iter().any(|u| {
            matches!(
                u.update_type,
                GeometryUpdateType::Resize | GeometryUpdateType::MoveResize
            )
        });
        let operation = if has_resize {
            drag_state::DragOperation::Resize
        } else {
            drag_state::DragOperation::Move
        };

        // Get the first window's workspace info
        if let Some(first_window) = updates.first().and_then(|u| state.get_window(u.window_id)) {
            let workspace_id = first_window.workspace_id;
            let pid = first_window.pid;

            if let Some(workspace) = state.get_workspace(workspace_id) {
                // Collect snapshots of all windows in the workspace
                let window_snapshots: Vec<drag_state::WindowSnapshot> = workspace
                    .window_ids
                    .iter()
                    .filter_map(|&id| state.get_window(id))
                    .map(|w| drag_state::WindowSnapshot {
                        window_id: w.id,
                        original_frame: w.frame,
                        is_floating: w.is_floating,
                    })
                    .collect();

                drag_state::start_operation(
                    operation,
                    pid,
                    workspace_id,
                    &workspace.name,
                    workspace.screen_id,
                    window_snapshots,
                    mouse_monitor::drag_sequence(),
                );
            }
        }
    }

    // If a drag operation is in progress, just update frames without triggering relayouts
    // The ratios will be calculated on mouse up
    if drag_state::is_operation_in_progress() {
        for update in updates {
            state.update_window(update.window_id, |w| {
                w.frame = update.frame;
            });
        }
        return;
    }

    // During animation (or the settling period after), ignore geometry events.
    // These are intermediate frames from the animation system and should not
    // trigger minimum size detection or corrupt our layout state.
    // This is critical for preventing the "animation ratio bug" where intermediate
    // animation frames are mistakenly interpreted as minimum size violations,
    // leading to incorrect `inferred_minimum_size` values and corrupted layouts.
    // The settling period catches events that were batched during animation but
    // flushed after the animation ended.
    if should_ignore_geometry_events() {
        return;
    }

    // Normal processing: check for minimum size violations
    let mut workspaces_to_relayout = Vec::new();

    for update in updates {
        // Check for minimum size mismatch
        if let Some(workspace_id) =
            detect_and_update_inferred_minimum(state, update.window_id, &update.frame)
                .filter(|ws_id| !workspaces_to_relayout.contains(ws_id))
        {
            workspaces_to_relayout.push(workspace_id);
        }

        // Update the frame
        state.update_window(update.window_id, |w| {
            w.frame = update.frame;
        });
    }

    // Trigger layout recalculation for workspaces with minimum size violations
    if let Some(handle) = get_subscriber_handle() {
        for workspace_id in workspaces_to_relayout {
            handle.notify_layout_changed(workspace_id, false);
        }
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Finds an appropriate workspace for a new window.
///
/// Priority:
/// 1. Window rules from config (match by `app_id`)
/// 2. Focused workspace
/// 3. First visible workspace
/// 4. Create a default workspace
fn find_workspace_for_window(state: &mut TilingState, info: &WindowCreatedInfo) -> Uuid {
    // Check window rules from config
    if let Some(workspace_id) = find_workspace_by_rules(state, info) {
        tracing::debug!(
            "Window {} (app={}) matched rule, assigned to workspace {:?}",
            info.window_id,
            info.app_id,
            workspace_id
        );
        return workspace_id;
    }

    // Try focused workspace as fallback
    if let Some(ws) = state.get_focused_workspace() {
        tracing::debug!(
            "Window {} (app={}) no rule match, using focused workspace '{}'",
            info.window_id,
            info.app_id,
            ws.name
        );
        return ws.id;
    }

    // Try first visible workspace
    if let Some(ws) = state.get_visible_workspaces().first() {
        return ws.id;
    }

    // Try any workspace
    if let Some(ws) = state.workspaces.iter().next() {
        return ws.id;
    }

    // Create a default workspace
    tracing::debug!("No workspace found, creating default");
    let ws = create_default_workspace(state);
    let id = ws.id;
    state.upsert_workspace(ws);
    id
}

/// Finds a workspace for a window based on config rules.
///
/// Checks each workspace's rules against the window's `app_id`/`app_name`/`title`.
/// Returns the UUID of the first matching workspace, or None if no match.
///
/// Rules use AND logic - all specified criteria must match.
fn find_workspace_by_rules(state: &TilingState, info: &WindowCreatedInfo) -> Option<Uuid> {
    use crate::config::get_config;

    let config = get_config();
    let workspace_configs = &config.tiling.workspaces;

    // For each workspace config, check its rules
    for ws_config in workspace_configs {
        for rule in &ws_config.rules {
            if rule_matches_window(rule, info) {
                // Found a match - find the workspace by name in state
                if let Some(ws) = state.get_workspace_by_name(&ws_config.name) {
                    tracing::debug!(
                        "Rule match: app_id='{}' → workspace '{}'",
                        info.app_id,
                        ws_config.name
                    );
                    return Some(ws.id);
                }
            }
        }
    }

    None
}

/// Checks if a rule matches a window.
///
/// All specified criteria must match (AND logic).
/// Returns false if the rule has no criteria.
fn rule_matches_window(rule: &crate::config::WindowRule, info: &WindowCreatedInfo) -> bool {
    // Rule must have at least one criterion
    if !rule.is_valid() {
        return false;
    }

    // Check app_id (bundle identifier) - case-insensitive exact match
    if rule
        .app_id
        .as_ref()
        .is_some_and(|rule_app_id| !info.app_id.eq_ignore_ascii_case(rule_app_id))
    {
        return false;
    }

    // Check app_name - case-insensitive substring match
    if rule.app_name.as_ref().is_some_and(|rule_app_name| {
        !info.app_name.to_lowercase().contains(&rule_app_name.to_lowercase())
    }) {
        return false;
    }

    // Check title - case-insensitive substring match
    if rule
        .title
        .as_ref()
        .is_some_and(|rule_title| !info.title.to_lowercase().contains(&rule_title.to_lowercase()))
    {
        return false;
    }

    // All specified criteria matched
    true
}

/// Creates a default workspace.
fn create_default_workspace(state: &TilingState) -> Workspace {
    let screen_id = state.get_main_screen().map_or(0, |s| s.id);

    Workspace {
        id: Uuid::now_v7(),
        name: "default".to_string(),
        screen_id,
        layout: crate::modules::tiling::state::LayoutType::Dwindle,
        is_visible: true,
        is_focused: true,
        window_ids: WindowIdList::new(),
        focused_window_index: None,
        split_ratios: Vec::new(),
        master_ratio: None,
        configured_screen: None,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::modules::tiling::state::LayoutType;

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
            master_ratio: None,
            configured_screen: None,
        };
        let ws_id = ws.id;
        state.upsert_workspace(ws);

        (state, ws_id)
    }

    fn make_window_info(window_id: u32) -> WindowCreatedInfo {
        WindowCreatedInfo {
            window_id,
            pid: 1000,
            app_id: "com.test.app".to_string(),
            app_name: "Test App".to_string(),
            title: format!("Window {window_id}"),
            frame: Rect::new(0.0, 0.0, 800.0, 600.0),
            is_minimized: false,
            is_fullscreen: false,
            minimum_size: None,
            tab_group_id: None,
            is_active_tab: true,
        }
    }

    #[test]
    fn test_window_created() {
        let (mut state, ws_id) = make_state_with_workspace();
        let info = make_window_info(100);

        on_window_created(&mut state, info);

        // Window should be tracked
        let window = state.get_window(100).expect("Window should exist");
        assert_eq!(window.id, 100);
        assert_eq!(window.workspace_id, ws_id);

        // Window should be in workspace's window list
        let ws = state.get_workspace(ws_id).unwrap();
        assert!(ws.window_ids.contains(&100));
    }

    #[test]
    fn test_window_destroyed() {
        let (mut state, ws_id) = make_state_with_workspace();
        let info = make_window_info(100);

        on_window_created(&mut state, info);
        on_window_destroyed(&mut state, 100);

        // Window should be gone
        assert!(state.get_window(100).is_none());

        // Window should be removed from workspace
        let ws = state.get_workspace(ws_id).unwrap();
        assert!(!ws.window_ids.contains(&100));
    }

    #[test]
    fn test_window_focused() {
        let (mut state, ws_id) = make_state_with_workspace();
        let info = make_window_info(100);

        on_window_created(&mut state, info);
        on_window_focused(&mut state, 100);

        // Focus should be set
        let focus = eyeball::Observable::get(&state.focus);
        assert_eq!(focus.focused_window_id, Some(100));
        assert_eq!(focus.focused_workspace_id, Some(ws_id));

        // Workspace should be focused
        let ws = state.get_workspace(ws_id).unwrap();
        assert!(ws.is_focused);
    }

    #[test]
    fn test_window_minimized() {
        let (mut state, _) = make_state_with_workspace();
        let info = make_window_info(100);

        on_window_created(&mut state, info);
        on_window_minimized(&mut state, 100, true);

        let window = state.get_window(100).unwrap();
        assert!(window.is_minimized);

        on_window_minimized(&mut state, 100, false);
        let window = state.get_window(100).unwrap();
        assert!(!window.is_minimized);
    }

    #[test]
    fn test_window_moved() {
        let (mut state, _) = make_state_with_workspace();
        let info = make_window_info(100);

        on_window_created(&mut state, info);

        let new_frame = Rect::new(100.0, 100.0, 800.0, 600.0);
        on_window_moved(&mut state, 100, new_frame);

        let window = state.get_window(100).unwrap();
        assert_eq!(window.frame.x, 100.0);
        assert_eq!(window.frame.y, 100.0);
    }

    #[test]
    fn test_batched_geometry_updates() {
        let (mut state, _) = make_state_with_workspace();

        on_window_created(&mut state, make_window_info(100));
        on_window_created(&mut state, make_window_info(200));

        let updates = vec![
            GeometryUpdate {
                window_id: 100,
                frame: Rect::new(10.0, 10.0, 400.0, 300.0),
                update_type: crate::modules::tiling::actor::GeometryUpdateType::Move,
            },
            GeometryUpdate {
                window_id: 200,
                frame: Rect::new(420.0, 10.0, 400.0, 300.0),
                update_type: crate::modules::tiling::actor::GeometryUpdateType::Move,
            },
        ];

        on_batched_geometry_updates(&mut state, &updates);

        assert_eq!(state.get_window(100).unwrap().frame.x, 10.0);
        assert_eq!(state.get_window(200).unwrap().frame.x, 420.0);
    }

    #[test]
    fn test_destroy_focused_window_clears_focus() {
        let (mut state, _) = make_state_with_workspace();

        on_window_created(&mut state, make_window_info(100));
        on_window_focused(&mut state, 100);

        assert!(eyeball::Observable::get(&state.focus).has_focus());

        on_window_destroyed(&mut state, 100);

        assert!(!eyeball::Observable::get(&state.focus).has_focus());
    }
}
