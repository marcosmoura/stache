// TODO: Remove these allows once the module is fully implemented
#![allow(dead_code)]
#![allow(unused_imports)]

//! Tiling Window Manager for Stache.
//!
//! This module provides a tiling window manager with virtual workspace support,
//! multiple layout modes, and keyboard-centric window management.
//!
//! # Features
//!
//! - Virtual workspaces with configurable rules for window assignment
//! - Multiple layout modes: Dwindle, Monocle, Split, Master, Floating
//! - Multi-monitor support with per-screen workspace assignment
//! - Window animations for smooth transitions
//! - Gaps between windows and screen edges
//! - Floating window presets for quick positioning
//!
//! # Window Management Approach
//!
//! Unlike some tiling managers that move windows to screen corners, this
//! implementation uses macOS native window hiding (similar to Cmd+H) for
//! workspace switching. This provides cleaner transitions and better
//! integration with macOS focus handling.
//!
//! # Usage
//!
//! The tiling manager is controlled via CLI commands:
//!
//! ```bash
//! # Query commands
//! stache tiling query screens
//! stache tiling query workspaces
//! stache tiling query windows
//!
//! # Workspace commands
//! stache tiling workspace --focus <name>
//! stache tiling workspace --layout <layout>
//!
//! # Window commands
//! stache tiling window --focus <direction>
//! stache tiling window --swap <direction>
//! ```

pub mod animation;
pub mod drag_state;
pub mod layout;
pub mod manager;
pub mod mouse_monitor;
pub mod observer;
pub mod rules;
pub mod screen;
pub mod screen_monitor;
pub mod state;
pub mod window;
pub mod workspace;

// Re-export commonly used types
pub use animation::{
    AnimationConfig, AnimationSystem, WindowTransition, begin_animation, cancel_animation,
    get_interrupted_position,
};
pub use manager::{TilingManager, WorkspaceSwitchInfo, get_manager, init_manager};
pub use observer::{WindowEvent, WindowEventType};
pub use rules::{
    WorkspaceMatch, any_rule_matches, count_matching_rules, find_matching_workspace, matches_window,
};
pub use screen::{get_all_screens, get_main_screen, get_screen_by_name, get_screen_count};
pub use state::{Point, Rect, Screen, TilingState, TrackedWindow, Workspace};
use tauri::{AppHandle, Runtime};
pub use window::{
    AppInfo, CGWindowInfo, WindowInfo, focus_window, get_all_windows,
    get_all_windows_including_hidden, get_cg_window_list, get_cg_window_list_all,
    get_focused_window, get_running_apps, get_screen_for_window, hide_app, hide_window,
    move_window_to_screen, set_window_frame, show_window, unhide_app,
};
pub use workspace::{
    FocusHistory, WindowAssignment, WorkspaceSwitchResult, assign_window_to_workspace,
    find_workspace_for_window, get_visible_workspace_for_screen, get_workspaces_for_screen,
    hide_workspace_windows, should_ignore_window, show_workspace_windows, workspace_exists,
};

use crate::config::get_config;
use crate::is_accessibility_granted;

// ============================================================================
// Initialization
// ============================================================================

/// Initializes the tiling window manager.
///
/// This function checks if tiling is enabled in the configuration,
/// verifies accessibility permissions, and sets up window tracking
/// and event observers.
///
/// # Arguments
///
/// * `app_handle` - Optional Tauri app handle for emitting events.
///
/// # Behavior
///
/// - If `tiling.enabled` is `false` in config, this function returns immediately
/// - If accessibility permissions are not granted, returns early (warning already logged in lib.rs)
/// - On successful initialization, begins tracking windows and managing workspaces
pub fn init_with_handle<R: Runtime>(app_handle: Option<AppHandle<R>>) {
    let config = get_config();

    // Tiling is disabled by default
    if !config.tiling.is_enabled() {
        return;
    }

    // Use cached accessibility permission check from lib.rs
    if !is_accessibility_granted() {
        return;
    }

    // Initialize the tiling manager
    if !init_manager(app_handle) {
        return;
    }

    eprintln!("stache: tiling: manager initialized");

    // Track existing windows
    track_existing_windows();

    // Initialize the mouse monitor for drag detection
    if mouse_monitor::init() {
        // Set up the callback for when mouse is released
        mouse_monitor::set_mouse_up_callback(on_mouse_up);
    }

    // Initialize the observer for window events
    if observer::init(handle_window_event) {
        eprintln!(
            "stache: tiling: observer initialized, watching {} apps",
            observer::observer_count()
        );
    }

    // Initialize the screen monitor for hotplug detection
    if screen_monitor::init(handle_screen_change) {
        eprintln!("stache: tiling: screen monitor initialized");
    }

    // Apply startup behavior: switch to workspace containing focused window
    apply_startup_behavior();

    eprintln!("stache: tiling: initialization complete");
}

/// Initializes the tiling window manager without an app handle.
///
/// This is a convenience function for cases where we don't need to emit events.
pub fn init() { init_with_handle::<tauri::Wry>(None); }

// ============================================================================
// Window Tracking
// ============================================================================

/// Tracks all existing windows on startup.
fn track_existing_windows() {
    let Some(manager) = get_manager() else {
        return;
    };

    let mut mgr = manager.write();
    if !mgr.is_enabled() {
        return;
    }

    mgr.track_existing_windows();
    let window_count = mgr.get_windows().len();
    let workspace_count = mgr.get_workspaces().len();
    drop(mgr);

    eprintln!("stache: tiling: tracked {window_count} windows across {workspace_count} workspaces");

    // Move windows to their assigned screens
    move_windows_to_assigned_screens();
}

/// Moves windows to their assigned workspace's screen.
///
/// For each tracked window, checks if it's on the correct screen (the screen
/// where its workspace is assigned). If not, moves it to the correct screen.
fn move_windows_to_assigned_screens() {
    let Some(manager) = get_manager() else {
        return;
    };

    let mgr = manager.read();
    if !mgr.is_enabled() {
        return;
    }

    let screens = mgr.get_screens().to_vec();

    // Collect window info for moving (to avoid holding lock during moves)
    let mut windows_to_move: Vec<(u32, String, String, Screen, Option<Screen>)> = Vec::new();

    for window in mgr.get_windows() {
        // Get the workspace this window belongs to
        let Some(workspace) = mgr.get_workspace(&window.workspace_name) else {
            eprintln!(
                "stache: tiling: window {} ({}) has no workspace '{}'",
                window.id, window.app_name, window.workspace_name
            );
            continue;
        };

        // Get the target screen for this workspace
        let Some(target_screen) = screens.iter().find(|s| s.id == workspace.screen_id) else {
            eprintln!(
                "stache: tiling: workspace '{}' has no screen {}",
                workspace.name, workspace.screen_id
            );
            continue;
        };

        // Determine which screen the window is currently on
        let current_screen = get_screen_for_window(&window.frame, &screens);

        // Check if window needs to be moved
        let needs_move = current_screen.is_none_or(|cs| cs.id != target_screen.id);

        if needs_move {
            windows_to_move.push((
                window.id,
                window.app_name.clone(),
                window.workspace_name.clone(),
                target_screen.clone(),
                current_screen.cloned(),
            ));
        }
    }

    drop(mgr); // Release lock before doing moves

    if windows_to_move.is_empty() {
        eprintln!("stache: tiling: all windows already on correct screens");
        return;
    }

    eprintln!(
        "stache: tiling: moving {} windows to their assigned screens",
        windows_to_move.len()
    );

    // Move windows to their target screens and collect successful moves
    let mut moved_windows: Vec<(u32, Rect)> = Vec::new();

    for (window_id, app_name, workspace_name, target_screen, current_screen) in windows_to_move {
        let current_name = current_screen.as_ref().map_or("unknown", |s| s.name.as_str());

        if move_window_to_screen(window_id, &target_screen, current_screen.as_ref()) {
            eprintln!(
                "stache: tiling: moved '{}' from '{}' to '{}' (workspace '{}')",
                app_name, current_name, target_screen.name, workspace_name
            );

            // Get the window's new frame after moving
            if let Some(new_frame) = get_window_frame_by_id(window_id) {
                moved_windows.push((window_id, new_frame));
            }
        } else {
            eprintln!(
                "stache: tiling: failed to move '{}' to '{}' (workspace '{}')",
                app_name, target_screen.name, workspace_name
            );
        }
    }

    // Update tracked window frames in the manager
    if !moved_windows.is_empty() {
        {
            let mut mgr = manager.write();
            for (window_id, new_frame) in &moved_windows {
                mgr.update_window_frame(*window_id, *new_frame);
            }
        }
        eprintln!(
            "stache: tiling: updated {} window frames after moving",
            moved_windows.len()
        );
    }
}

/// Gets a window's current frame by ID.
fn get_window_frame_by_id(window_id: u32) -> Option<Rect> {
    let windows = get_all_windows();
    windows.iter().find(|w| w.id == window_id).map(|w| w.frame)
}

/// Applies startup behavior: set initial workspace visibility based on focused window.
///
/// This should be called AFTER windows are tracked. It will:
/// - Detect the currently focused window
/// - Set the workspace containing that window as visible/focused on its screen
/// - For all other screens, set the first workspace (in config order) as visible
#[allow(clippy::significant_drop_tightening)] // Lock guard scope is intentional
fn apply_startup_behavior() {
    let Some(manager) = get_manager() else {
        return;
    };

    // Get the currently focused window
    let focused = get_focused_window();

    // Debug: log what focused window we detected
    if let Some(ref fw) = focused {
        eprintln!(
            "stache: tiling: startup: detected focused window: id={}, app='{}', title='{}'",
            fw.id, fw.app_name, fw.title
        );
    } else {
        eprintln!("stache: tiling: startup: no focused window detected");
    }

    // Get the focused window ID if it's tracked
    #[allow(clippy::option_if_let_else)] // More readable with if let for logging
    let focused_window_id = {
        let mgr = manager.read();
        if !mgr.is_enabled() {
            return;
        }

        focused.as_ref().and_then(|fw| {
            // Only use the focused window if it's actually tracked
            if let Some(tracked) = mgr.get_window(fw.id) {
                eprintln!(
                    "stache: tiling: startup: focused window is tracked in workspace '{}'",
                    tracked.workspace_name
                );
                Some(fw.id)
            } else {
                eprintln!(
                    "stache: tiling: startup: focused window id={} is NOT tracked (ignored or not found)",
                    fw.id
                );
                None
            }
        })
    };

    // Set initial workspace visibility
    let mut mgr = manager.write();
    mgr.set_initial_workspace_visibility(focused_window_id);

    // Log the result
    if let Some(focused_ws) = mgr.state().focused_workspace.as_ref() {
        if focused_window_id.is_some() {
            eprintln!(
                "stache: tiling: startup: focused workspace '{focused_ws}' (contains focused window)"
            );
        } else {
            eprintln!(
                "stache: tiling: startup: focused workspace '{focused_ws}' (default - no focused window found)"
            );
        }
    }

    // Log visible workspaces per screen
    for screen in mgr.get_screens() {
        if let Some(visible_ws) =
            mgr.get_workspaces().iter().find(|w| w.screen_id == screen.id && w.is_visible)
        {
            eprintln!(
                "stache: tiling: startup: screen '{}' -> workspace '{}'",
                screen.name, visible_ws.name
            );
        }
    }

    // Now hide windows from non-visible workspaces and show windows from visible ones
    apply_initial_window_visibility(&mgr);

    // Apply layouts to all visible workspaces (uses instant positioning since not yet initialized)
    apply_initial_layouts(&mut mgr);

    // Mark the manager as initialized - from now on, animations will be used (if enabled)
    mgr.mark_initialized();
}

/// Applies initial window visibility based on workspace visibility.
///
/// For visible workspaces: Shows (unhides) all windows.
/// For non-visible workspaces: Hides all windows.
fn apply_initial_window_visibility(mgr: &TilingManager) {
    use workspace::{hide_workspace_windows, show_workspace_windows};

    let mut total_hidden = 0;
    let mut total_shown = 0;

    for ws in mgr.get_workspaces() {
        let windows: Vec<_> = mgr.get_windows_for_workspace(&ws.name);

        if windows.is_empty() {
            continue;
        }

        if ws.is_visible {
            // Show all windows in visible workspaces (some might be hidden)
            let (shown, _failures) = show_workspace_windows(&windows);
            // Note: show_workspace_windows returns false for already-visible apps,
            // which is not a real failure - we just count actual unhides
            if shown > 0 {
                total_shown += shown;
                eprintln!(
                    "stache: tiling: startup: showed {} windows in workspace '{}'",
                    shown, ws.name
                );
            }
        } else {
            // Hide all windows in non-visible workspaces
            let (hidden, failures) = hide_workspace_windows(&windows);
            if hidden > 0 {
                total_hidden += hidden;
                eprintln!(
                    "stache: tiling: startup: hid {} windows in workspace '{}'",
                    hidden, ws.name
                );
            }
            if !failures.is_empty() {
                eprintln!(
                    "stache: tiling: startup: failed to hide {} apps in workspace '{}'",
                    failures.len(),
                    ws.name
                );
            }
        }
    }

    eprintln!(
        "stache: tiling: startup: visibility applied - {total_shown} shown, {total_hidden} hidden"
    );
}

/// Applies layouts to all visible workspaces on startup.
fn apply_initial_layouts(mgr: &mut TilingManager) {
    // Collect visible workspace names first to avoid borrow issues
    let visible_workspaces: Vec<String> = mgr
        .get_workspaces()
        .iter()
        .filter(|ws| ws.is_visible)
        .map(|ws| ws.name.clone())
        .collect();

    for ws_name in visible_workspaces {
        // Use forced mode at startup to ensure all windows are positioned correctly
        let repositioned = mgr.apply_layout_forced(&ws_name);
        if repositioned > 0 {
            eprintln!(
                "stache: tiling: startup: applied layout to {repositioned} windows in workspace '{ws_name}'"
            );
        }
    }
}

// ============================================================================
// Event Handling
// ============================================================================

/// Handles window events from the observer.
///
/// This is the callback function registered with the observer system.
fn handle_window_event(event: WindowEvent) {
    match event.event_type {
        WindowEventType::Created => handle_window_created(event.pid),
        WindowEventType::Destroyed => handle_window_destroyed(event.pid),
        WindowEventType::Focused => handle_window_focused(event.pid),
        WindowEventType::AppActivated => handle_app_activated(event.pid),
        WindowEventType::AppHidden => handle_app_hidden(event.pid),
        WindowEventType::AppShown => handle_app_shown(event.pid),
        WindowEventType::Moved => handle_window_moved(event.pid),
        WindowEventType::Resized => handle_window_resized(event.pid),
        // Events we track but don't need special handling for
        WindowEventType::Minimized
        | WindowEventType::Unminimized
        | WindowEventType::TitleChanged
        | WindowEventType::Unfocused
        | WindowEventType::AppDeactivated => {}
    }
}

/// Handles a window being moved.
///
/// If the mouse is down and no drag operation is in progress, this starts
/// tracking a move operation. During a drag, events are ignored.
fn handle_window_moved(pid: i32) {
    // If mouse is not down, this is a programmatic move (from us) - ignore
    if !mouse_monitor::is_mouse_down() {
        return;
    }

    // If we're already tracking an operation, ignore additional events
    if drag_state::is_operation_in_progress() {
        return;
    }

    // Start tracking this as a move operation
    start_drag_operation(pid, drag_state::DragOperation::Move);
}

/// Handles a window being resized.
///
/// If the mouse is down and no resize operation is in progress, this starts
/// tracking a resize operation. During a resize, events are ignored.
fn handle_window_resized(pid: i32) {
    // If mouse is not down, this is a programmatic resize (from us) - ignore
    if !mouse_monitor::is_mouse_down() {
        return;
    }

    // If we're already tracking an operation, ignore additional events
    if drag_state::is_operation_in_progress() {
        return;
    }

    // Start tracking this as a resize operation
    start_drag_operation(pid, drag_state::DragOperation::Resize);
}

/// Starts tracking a drag/resize operation for a window from the given PID.
fn start_drag_operation(pid: i32, operation: drag_state::DragOperation) {
    let Some(manager) = get_manager() else {
        return;
    };

    let mgr = manager.read();
    if !mgr.is_enabled() {
        return;
    }

    // Find a tracked window from this PID to determine the workspace
    let tracked_windows: Vec<_> = mgr.get_windows().iter().filter(|w| w.pid == pid).collect();

    if tracked_windows.is_empty() {
        return;
    }

    // Get the workspace name from the first tracked window
    let workspace_name = tracked_windows[0].workspace_name.clone();

    // Get ALL windows in this workspace (not just from this PID)
    let workspace_windows: Vec<_> = mgr.get_windows_for_workspace(&workspace_name);

    // Create snapshots of all windows in the workspace
    let window_snapshots: Vec<drag_state::WindowSnapshot> = workspace_windows
        .iter()
        .map(|w| drag_state::WindowSnapshot {
            window_id: w.id,
            original_frame: w.frame,
            is_floating: w.is_floating,
        })
        .collect();

    drop(mgr);

    // Record the operation with all workspace windows
    drag_state::start_operation(
        operation,
        pid,
        &workspace_name,
        window_snapshots,
        mouse_monitor::drag_sequence(),
    );
}

/// Called when the mouse button is released after a drag/resize operation.
///
/// This is registered as a callback with the mouse monitor.
fn on_mouse_up() {
    // Finish any ongoing operation
    let Some(info) = drag_state::finish_operation() else {
        return;
    };

    // Process the completed operation
    match info.operation {
        drag_state::DragOperation::Move => handle_move_finished(&info),
        drag_state::DragOperation::Resize => handle_resize_finished(&info),
    }
}

/// Handles the completion of a move operation.
///
/// For tiled windows: reapply the layout to snap them back to position.
/// For floating windows: leave them where they are (this is their new position).
fn handle_move_finished(info: &drag_state::DragInfo) {
    // Update tracked frames for all windows
    update_all_tracked_frames(&info.workspace_name);

    if !info.has_tiled_windows() {
        // All floating windows - nothing to snap back
        eprintln!(
            "stache: tiling: drag: all windows floating in '{}' - keeping positions",
            info.workspace_name
        );
        return;
    }

    // Tiled windows get snapped back to their layout position
    eprintln!(
        "stache: tiling: drag: tiled windows moved - reapplying layout for workspace '{}'",
        info.workspace_name
    );

    let Some(manager) = get_manager() else {
        return;
    };

    // Cancel any running animation before acquiring lock
    cancel_animation();

    let mut mgr = manager.write();
    begin_animation(); // Signal we're no longer waiting
    if mgr.is_enabled() {
        mgr.apply_layout_forced(&info.workspace_name);
    }
}

/// Handles the completion of a resize operation.
///
/// For tiled windows: find which window was resized and calculate new ratios.
/// For floating windows: just update the tracked frames.
fn handle_resize_finished(info: &drag_state::DragInfo) {
    // First, get the current window frames
    let current_windows = get_all_windows();

    // Find which window was resized by comparing current frames to snapshots
    let resized_window = find_resized_window(&info.window_snapshots, &current_windows);

    // Update all tracked frames
    update_all_tracked_frames(&info.workspace_name);

    if !info.has_tiled_windows() {
        // All floating windows - just keep new sizes
        eprintln!(
            "stache: tiling: drag: all windows floating in '{}' - keeping sizes",
            info.workspace_name
        );
        return;
    }

    let Some(manager) = get_manager() else {
        return;
    };

    // Cancel any running animation before acquiring lock
    cancel_animation();

    let mut mgr = manager.write();
    begin_animation(); // Signal we're no longer waiting
    if !mgr.is_enabled() {
        return;
    }

    // Calculate and apply new ratios, passing the resized window info
    if let Some((window_id, old_frame, new_frame)) = resized_window {
        eprintln!(
            "stache: tiling: drag: window {} resized from ({:.0}x{:.0}) to ({:.0}x{:.0}) - calculating ratios",
            window_id, old_frame.width, old_frame.height, new_frame.width, new_frame.height
        );
        mgr.calculate_and_apply_ratios_for_window(
            &info.workspace_name,
            window_id,
            old_frame,
            new_frame,
        );
    } else {
        eprintln!("stache: tiling: drag: couldn't identify resized window - reapplying layout");
        mgr.apply_layout_forced(&info.workspace_name);
    }
}

/// Finds which window was resized by comparing snapshots to current frames.
///
/// Returns the window ID, old frame, and new frame if found.
fn find_resized_window(
    snapshots: &[drag_state::WindowSnapshot],
    current_windows: &[WindowInfo],
) -> Option<(u32, Rect, Rect)> {
    let mut max_diff = 0.0f64;
    let mut resized: Option<(u32, Rect, Rect)> = None;

    for snapshot in snapshots {
        // Skip floating windows
        if snapshot.is_floating {
            continue;
        }

        // Find the current frame for this window
        let Some(current) = current_windows.iter().find(|w| w.id == snapshot.window_id) else {
            continue;
        };

        // Calculate how much the frame changed (focus on size changes for resize)
        let width_diff = (current.frame.width - snapshot.original_frame.width).abs();
        let height_diff = (current.frame.height - snapshot.original_frame.height).abs();
        let size_diff = width_diff + height_diff;

        if size_diff > max_diff {
            max_diff = size_diff;
            resized = Some((snapshot.window_id, snapshot.original_frame, current.frame));
        }
    }

    // Only return if there was a significant change (more than 5 pixels)
    if max_diff > 5.0 { resized } else { None }
}

/// Updates tracked frames for all windows in a workspace from their current on-screen positions.
fn update_all_tracked_frames(workspace_name: &str) {
    let Some(manager) = get_manager() else {
        return;
    };

    // Get the current frames from the window list
    let current_windows = get_all_windows();

    let mut mgr = manager.write();

    // Get window IDs for this workspace
    let workspace_window_ids: Vec<u32> =
        mgr.get_windows_for_workspace(workspace_name).iter().map(|w| w.id).collect();

    // Update each window's frame
    for window_id in workspace_window_ids {
        if let Some(current) = current_windows.iter().find(|w| w.id == window_id) {
            mgr.update_window_frame(window_id, current.frame);
        }
    }
}

/// Maximum time to wait for windows to be ready (in milliseconds).
const WINDOW_READY_TIMEOUT_MS: u64 = 150;

/// How often to poll for window readiness (in milliseconds).
const WINDOW_READY_POLL_INTERVAL_MS: u64 = 5;

/// Handles a new window being created.
///
/// This function polls for window readiness instead of using a fixed delay:
/// 1. Poll until windows have valid AX properties (position/size) or timeout
/// 2. Then track and apply layout
///
/// This avoids race conditions where we try to position a window before
/// its AX element is fully ready, while also being faster for apps that
/// initialize windows quickly.
fn handle_window_created(pid: i32) {
    // Spawn a thread to handle this asynchronously
    std::thread::spawn(move || {
        // Poll until windows are ready (have valid AX frames) or timeout
        // This is faster than a fixed delay for most apps, and more reliable for slow apps
        let app_windows = window::wait_for_app_windows_ready(
            pid,
            WINDOW_READY_TIMEOUT_MS,
            WINDOW_READY_POLL_INTERVAL_MS,
        );

        let current_ids: std::collections::HashSet<u32> =
            app_windows.iter().map(|w| w.id).collect();

        let Some(manager) = get_manager() else {
            return;
        };

        // Cancel any running animation before acquiring lock
        cancel_animation();

        let mut mgr = manager.write();
        begin_animation(); // Signal we're no longer waiting
        if !mgr.is_enabled() {
            return;
        }

        // Get visible workspace names
        let visible_workspaces: std::collections::HashSet<String> = mgr
            .get_workspaces()
            .iter()
            .filter(|ws| ws.is_visible)
            .map(|ws| ws.name.clone())
            .collect();

        // Find stale windows (tracked but no longer in deduplicated list)
        // These are old tab IDs that need to be swapped or removed
        let stale_windows: Vec<(u32, Rect)> = mgr
            .get_windows()
            .iter()
            .filter(|w| {
                w.pid == pid
                    && !current_ids.contains(&w.id)
                    && visible_workspaces.contains(&w.workspace_name)
            })
            .map(|w| (w.id, w.frame))
            .collect();

        // Find new windows (in deduplicated list but not tracked)
        let new_windows: Vec<&WindowInfo> =
            app_windows.iter().filter(|w| mgr.get_window(w.id).is_none()).collect();

        // DETECT TAB SWAPS FIRST: Match stale windows with new windows by frame
        // If frames match, it's just a tab ID change - swap in place WITHOUT triggering layout
        let mut swapped_stale_ids: std::collections::HashSet<u32> =
            std::collections::HashSet::new();
        let mut swapped_new_ids: std::collections::HashSet<u32> = std::collections::HashSet::new();

        for (stale_id, stale_frame) in &stale_windows {
            for new_window in &new_windows {
                if !swapped_new_ids.contains(&new_window.id)
                    && frames_approximately_equal(stale_frame, &new_window.frame)
                {
                    // This is a tab swap - just update the ID in place (no layout change)
                    if mgr.swap_window_id(*stale_id, new_window.id) {
                        eprintln!(
                            "stache: tiling: tab swap: {} -> {} ({}) - no layout change",
                            stale_id, new_window.id, new_window.app_name
                        );
                        swapped_stale_ids.insert(*stale_id);
                        swapped_new_ids.insert(new_window.id);
                    }
                    break;
                }
            }
        }

        // Untrack stale windows that weren't swapped (real window closures)
        // Use no_layout variant - we'll apply layout once at the end if needed
        let mut workspaces_changed: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for (stale_id, _) in &stale_windows {
            if !swapped_stale_ids.contains(stale_id) {
                // Get workspace before untracking
                if let Some(w) = mgr.get_window(*stale_id) {
                    workspaces_changed.insert(w.workspace_name.clone());
                }
                eprintln!("stache: tiling: untracking stale window {stale_id}");
                mgr.untrack_window_no_layout(*stale_id);
            }
        }

        // Track new windows that weren't swapped (real new windows)
        // Use no_layout variant - we'll apply layout once at the end if needed
        for new_window in new_windows {
            if !swapped_new_ids.contains(&new_window.id)
                && let Some(workspace_name) = mgr.track_window_no_layout(new_window)
            {
                eprintln!(
                    "stache: tiling: tracked new window {} ({}) in workspace '{}'",
                    new_window.id, new_window.app_name, workspace_name
                );
                workspaces_changed.insert(workspace_name);
            }
        }

        // Only apply layout if there were real window changes (not just tab swaps)
        if !workspaces_changed.is_empty() {
            // Apply layout immediately - windows are already ready from polling
            for workspace_name in &workspaces_changed {
                eprintln!("stache: tiling: applying layout for workspace '{workspace_name}'");
                mgr.apply_layout_forced(workspace_name);
            }
        }
    });
}

/// Checks if two frames are approximately equal (within a small tolerance).
/// This accounts for minor floating-point differences in frame coordinates.
fn frames_approximately_equal(a: &Rect, b: &Rect) -> bool {
    const TOLERANCE: f64 = 2.0;
    (a.x - b.x).abs() < TOLERANCE
        && (a.y - b.y).abs() < TOLERANCE
        && (a.width - b.width).abs() < TOLERANCE
        && (a.height - b.height).abs() < TOLERANCE
}

/// Handles a window being destroyed.
///
/// Also handles tab closure: when the "representative" tab (highest ID) is closed,
/// the remaining tab becomes the new representative and needs to be tracked.
fn handle_window_destroyed(pid: i32) {
    let Some(manager) = get_manager() else {
        return;
    };

    // Get current window list - windows that no longer exist won't be in this list
    // This list is already deduplicated for tabs
    // IMPORTANT: This only returns ON-SCREEN windows
    let current_windows = get_all_windows();
    let app_windows: Vec<_> = current_windows.iter().filter(|w| w.pid == pid).collect();
    let current_ids: std::collections::HashSet<u32> = app_windows.iter().map(|w| w.id).collect();

    // Cancel any running animation before acquiring lock
    cancel_animation();

    let mut mgr = manager.write();
    begin_animation(); // Signal we're no longer waiting
    if !mgr.is_enabled() {
        return;
    }

    // Get visible workspace names - only untrack windows in visible workspaces
    // Windows in hidden workspaces won't appear in CGWindowList
    let visible_workspaces: std::collections::HashSet<String> = mgr
        .get_workspaces()
        .iter()
        .filter(|ws| ws.is_visible)
        .map(|ws| ws.name.clone())
        .collect();

    // Find destroyed windows (tracked but no longer exist)
    let destroyed_windows: Vec<(u32, Rect)> = mgr
        .get_windows()
        .iter()
        .filter(|w| {
            w.pid == pid
                && !current_ids.contains(&w.id)
                && visible_workspaces.contains(&w.workspace_name)
        })
        .map(|w| (w.id, w.frame))
        .collect();

    // Find new windows (exist but not tracked) - these are tabs that became representatives
    let new_windows: Vec<&WindowInfo> =
        app_windows.iter().filter(|w| mgr.get_window(w.id).is_none()).copied().collect();

    // DETECT TAB SWAPS FIRST: Match destroyed windows with new windows by frame
    // If frames match, it's just a tab ID change - swap in place WITHOUT triggering layout
    let mut swapped_destroyed_ids: std::collections::HashSet<u32> =
        std::collections::HashSet::new();
    let mut swapped_new_ids: std::collections::HashSet<u32> = std::collections::HashSet::new();

    for (destroyed_id, destroyed_frame) in &destroyed_windows {
        for new_window in &new_windows {
            if !swapped_new_ids.contains(&new_window.id)
                && frames_approximately_equal(destroyed_frame, &new_window.frame)
            {
                // This is a tab swap - just update the ID in place (no layout change)
                if mgr.swap_window_id(*destroyed_id, new_window.id) {
                    eprintln!(
                        "stache: tiling: tab swap: {} -> {} ({}) - no layout change",
                        destroyed_id, new_window.id, new_window.app_name
                    );
                    swapped_destroyed_ids.insert(*destroyed_id);
                    swapped_new_ids.insert(new_window.id);
                }
                break;
            }
        }
    }

    // Untrack destroyed windows that weren't swapped (real window closures)
    // Use no_layout variant - we'll apply layout once at the end if needed
    let mut workspaces_changed: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for (destroyed_id, _) in &destroyed_windows {
        if !swapped_destroyed_ids.contains(destroyed_id) {
            // Get workspace before untracking
            if let Some(w) = mgr.get_window(*destroyed_id) {
                workspaces_changed.insert(w.workspace_name.clone());
            }
            eprintln!("stache: tiling: untracking destroyed window {destroyed_id}");
            mgr.untrack_window_no_layout(*destroyed_id);
        }
    }

    // Track new windows that weren't swapped (shouldn't normally happen in destroy handler)
    // Use no_layout variant - we'll apply layout once at the end if needed
    for new_window in new_windows {
        if !swapped_new_ids.contains(&new_window.id)
            && let Some(workspace_name) = mgr.track_window_no_layout(new_window)
        {
            eprintln!(
                "stache: tiling: tracked new window {} ({}) in workspace '{}'",
                new_window.id, new_window.app_name, workspace_name
            );
            workspaces_changed.insert(workspace_name);
        }
    }

    // Apply layout for workspaces that had real window changes (not just tab swaps)
    for workspace_name in &workspaces_changed {
        eprintln!(
            "stache: tiling: applying layout for workspace '{workspace_name}' after window destruction"
        );
        mgr.apply_layout_forced(workspace_name);
    }
}

/// Handles a window gaining focus.
///
/// Implements focus-follows-workspace: when a window is focused that belongs
/// to a different workspace on the same screen, switch to that workspace.
fn handle_window_focused(pid: i32) {
    let Some(manager) = get_manager() else {
        return;
    };

    // Get the currently focused window
    let Some(focused) = get_focused_window() else {
        eprintln!("stache: tiling: focus event: could not get focused window for PID {pid}");
        return;
    };

    // Only handle if it's from the app that triggered the event
    if focused.pid != pid {
        return;
    }

    eprintln!(
        "stache: tiling: focus event: window {} ({} - '{}') focused",
        focused.id, focused.app_name, focused.title
    );

    // Cancel any running animation before acquiring lock
    cancel_animation();

    let mut mgr = manager.write();
    begin_animation(); // Signal we're no longer waiting
    if !mgr.is_enabled() {
        return;
    }

    // Find the tracked window - if not found, ignore this focus event
    // The window will be tracked by handle_window_created with proper tab detection
    let Some(tracked) = mgr.get_window(focused.id).cloned() else {
        // Window not tracked yet - this is normal for new windows/tabs
        // Let handle_window_created handle it with proper tab detection
        eprintln!(
            "stache: tiling: focus event: window {} not tracked yet, ignoring (will be handled by window_created)",
            focused.id
        );
        return;
    };

    // Check if we should skip this focus event (it might be a stale event
    // from macOS after we programmatically focused a different window)
    if mgr.should_skip_focus_event(focused.id) {
        return;
    }

    let window_workspace = tracked.workspace_name;

    // Check if this workspace is already visible on its screen
    let target_ws = mgr.get_workspace(&window_workspace);

    if let Some(target) = target_ws {
        if target.is_visible {
            // Workspace is already visible, just update the focused window
            eprintln!(
                "stache: tiling: focus event: workspace '{window_workspace}' already visible, updating focus"
            );
            mgr.set_focused_window(&window_workspace, focused.id);
            return;
        }

        // Check if we should skip this workspace switch (debounce recent switches)
        // This prevents race conditions where focus events from hide/show operations
        // during a workspace switch trigger another switch.
        if mgr.should_skip_workspace_switch(&window_workspace) {
            return;
        }

        // Workspace is not visible - switch to it
        eprintln!(
            "stache: tiling: focus event: switching to workspace '{}' (screen {})",
            window_workspace, target.screen_id
        );

        if let Some(info) = mgr.switch_workspace(&window_workspace) {
            eprintln!(
                "stache: tiling: focus-follows-workspace: switched to '{}' (window {} focused)",
                info.workspace, focused.id
            );
        }
    } else {
        eprintln!("stache: tiling: focus event: workspace '{window_workspace}' not found");
    }
}

/// Handles an application being activated.
///
/// When an app is activated (brought to front), we need to:
/// 1. Track any new windows that might have been created while hidden
/// 2. Handle focus-follows-workspace: switch to the workspace containing the focused window
fn handle_app_activated(pid: i32) {
    // First, track any new windows from this app
    handle_window_created(pid);

    // Then handle focus - this implements focus-follows-workspace when switching apps
    handle_window_focused(pid);
}

/// Handles an application being hidden.
const fn handle_app_hidden(_pid: i32) {
    // Currently no special handling needed
    // Windows remain tracked but hidden
}

/// Handles an application being shown (unhidden).
fn handle_app_shown(pid: i32) {
    // Check for any new windows that might have appeared
    handle_window_created(pid);

    // Handle focus - this implements focus-follows-workspace when unhiding an app
    handle_window_focused(pid);
}

// ============================================================================
// Screen Change Handler
// ============================================================================

/// Handles screen configuration changes (screens connected/disconnected).
///
/// This is called by the screen monitor when displays are added or removed.
fn handle_screen_change() {
    let Some(manager) = get_manager() else {
        eprintln!("stache: tiling: screen change: manager not available");
        return;
    };

    // Prevent recursive callbacks during our screen refresh
    screen_monitor::set_processing(true);

    // Cancel any running animation before acquiring lock
    cancel_animation();

    let (added, removed) = {
        let mut mgr = manager.write();
        begin_animation(); // Signal we're no longer waiting
        if !mgr.is_enabled() {
            screen_monitor::set_processing(false);
            return;
        }
        mgr.handle_screen_change()
    };

    eprintln!(
        "stache: tiling: screen change handled: {added} screens added, {removed} screens removed"
    );

    screen_monitor::set_processing(false);
}

// ============================================================================
// IPC Query Handler
// ============================================================================

use crate::utils::ipc_socket::{IpcQuery, IpcResponse};

/// Handles IPC queries for tiling state.
///
/// This is called by the IPC server when a query is received from the CLI.
pub fn handle_ipc_query(query: IpcQuery) -> IpcResponse {
    match query {
        IpcQuery::Ping => IpcResponse::success(serde_json::json!({"status": "ok"})),
        IpcQuery::Screens => query_screens(),
        IpcQuery::Workspaces { screen, focused_screen } => {
            query_workspaces(screen.as_deref(), focused_screen)
        }
        IpcQuery::Windows {
            screen,
            workspace,
            focused_screen,
            focused_workspace,
        } => query_windows(
            screen.as_deref(),
            workspace.as_deref(),
            focused_screen,
            focused_workspace,
        ),
    }
}

/// Queries all screens.
fn query_screens() -> IpcResponse {
    let screens = get_all_screens();
    IpcResponse::success(screens)
}

/// Queries workspaces with optional filters.
#[allow(clippy::significant_drop_tightening)] // Guard needs to be held for entire function
fn query_workspaces(screen_filter: Option<&str>, focused_screen: bool) -> IpcResponse {
    let Some(manager) = get_manager() else {
        return IpcResponse::error("Tiling not initialized");
    };

    let mgr = manager.read();
    if !mgr.is_enabled() {
        return IpcResponse::error("Tiling not enabled");
    }

    let mut workspaces: Vec<serde_json::Value> = Vec::new();

    // Determine which screen(s) to filter by
    let filter_screen_id: Option<u32> = if focused_screen {
        mgr.get_focused_screen().map(|s| s.id)
    } else if let Some(name) = screen_filter {
        match mgr.get_screen_by_name(name) {
            Some(s) => Some(s.id),
            None => return IpcResponse::error(format!("Screen '{name}' not found")),
        }
    } else {
        None
    };

    for ws in mgr.get_workspaces() {
        // Apply screen filter if specified
        if let Some(screen_id) = filter_screen_id
            && ws.screen_id != screen_id
        {
            continue;
        }

        let screen_name = mgr
            .get_screen(ws.screen_id)
            .map_or_else(|| "unknown".to_string(), |s| s.name.clone());

        workspaces.push(serde_json::json!({
            "name": ws.name,
            "screenId": ws.screen_id,
            "screenName": screen_name,
            "layout": format!("{:?}", ws.layout).to_lowercase(),
            "isVisible": ws.is_visible,
            "isFocused": ws.is_focused,
            "windowCount": ws.window_ids.len(),
            "windowIds": ws.window_ids,
        }));
    }

    IpcResponse::success(workspaces)
}

/// Queries windows with optional filters.
#[allow(clippy::significant_drop_tightening)] // Guard needs to be held for entire function
fn query_windows(
    screen_filter: Option<&str>,
    workspace_filter: Option<&str>,
    focused_screen: bool,
    focused_workspace: bool,
) -> IpcResponse {
    let Some(manager) = get_manager() else {
        return IpcResponse::error("Tiling not initialized");
    };

    let mgr = manager.read();
    if !mgr.is_enabled() {
        return IpcResponse::error("Tiling not enabled");
    }

    // Determine filter criteria
    let filter_screen_id: Option<u32> = if focused_screen {
        mgr.get_focused_screen().map(|s| s.id)
    } else if let Some(name) = screen_filter {
        match mgr.get_screen_by_name(name) {
            Some(s) => Some(s.id),
            None => return IpcResponse::error(format!("Screen '{name}' not found")),
        }
    } else {
        None
    };

    let filter_workspace: Option<String> = if focused_workspace {
        mgr.state().focused_workspace.clone()
    } else {
        workspace_filter.map(String::from)
    };

    // Validate workspace filter if specified
    if let Some(ref ws_name) = filter_workspace
        && mgr.get_workspace(ws_name).is_none()
    {
        return IpcResponse::error(format!("Workspace '{ws_name}' not found"));
    }

    // Build list of workspaces to include (for screen filtering)
    let workspace_names_for_screen: Option<Vec<String>> = filter_screen_id.map(|screen_id| {
        mgr.get_workspaces()
            .iter()
            .filter(|ws| ws.screen_id == screen_id)
            .map(|ws| ws.name.clone())
            .collect()
    });

    let mut windows: Vec<serde_json::Value> = Vec::new();

    for w in mgr.get_windows() {
        // Apply workspace filter
        if let Some(ref ws_name) = filter_workspace
            && !w.workspace_name.eq_ignore_ascii_case(ws_name)
        {
            continue;
        }

        // Apply screen filter (via workspace)
        if let Some(ref ws_names) = workspace_names_for_screen
            && !ws_names.iter().any(|name| name.eq_ignore_ascii_case(&w.workspace_name))
        {
            continue;
        }

        windows.push(serde_json::json!({
            "id": w.id,
            "pid": w.pid,
            "appId": w.app_id,
            "appName": w.app_name,
            "title": w.title,
            "workspace": w.workspace_name,
            "frame": {
                "x": w.frame.x,
                "y": w.frame.y,
                "width": w.frame.width,
                "height": w.frame.height,
            },
        }));
    }

    IpcResponse::success(windows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_module_compiles() {
        // Basic sanity check that the module structure is valid
        assert!(true);
    }

    #[test]
    fn test_exports() {
        // Verify that re-exports work
        let _point = Point::new(0.0, 0.0);
        let _rect = Rect::new(0.0, 0.0, 100.0, 100.0);
    }

    #[test]
    fn test_screen_functions_available() {
        let count = get_screen_count();
        assert!(count >= 1);

        let screens = get_all_screens();
        assert!(!screens.is_empty());

        let main = get_main_screen();
        assert!(main.is_some());
    }
}
