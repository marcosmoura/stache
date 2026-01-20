//! Screen event handlers for the state actor.
//!
//! These handlers process display configuration changes:
//! - Screens changed → refresh screen list, create/reassign workspaces

use core_graphics::display::CGDisplay;

use crate::config::get_config;
use crate::modules::tiling::init::get_subscriber_handle;
use crate::modules::tiling::state::{LayoutType, Rect, Screen, TilingState, Workspace};

/// Handles a screens changed event.
///
/// Refreshes the screen list from macOS and creates/reassigns workspaces.
/// On first run (no existing screens), creates workspaces from config.
/// On subsequent runs (screen hotplug), reassigns workspaces as needed.
pub fn on_screens_changed(state: &mut TilingState) {
    log::debug!("Handling screens changed");

    // Get current screens from macOS
    let new_screens = get_screens_from_macos();

    if new_screens.is_empty() {
        log::warn!("No screens detected!");
        return;
    }

    log::info!("tiling: detected {} screen(s)", new_screens.len());

    // Check if this is initial setup (no screens yet)
    let is_initial_setup = state.screens.is_empty();

    // Build set of current screen IDs
    let new_screen_ids: std::collections::HashSet<u32> = new_screens.iter().map(|s| s.id).collect();

    // Find screens that were removed
    let old_screen_ids: Vec<u32> = state.screens.iter().map(|s| s.id).collect();
    let removed_screens: Vec<u32> =
        old_screen_ids.into_iter().filter(|id| !new_screen_ids.contains(id)).collect();

    // Update screens
    for screen in new_screens {
        state.upsert_screen(screen);
    }

    // Remove old screens
    for screen_id in &removed_screens {
        state.remove_screen(*screen_id);
        log::debug!("Removed screen {screen_id}");
    }

    // Track workspaces that need layout recomputation
    let mut affected_workspaces = Vec::new();

    // On initial setup, create workspaces from config
    if is_initial_setup {
        create_workspaces_from_config(state);
    } else {
        // Handle screen hotplug
        if !removed_screens.is_empty() {
            // Reassign workspaces from removed screens to main
            let reassigned = reassign_workspaces_from_removed_screens(state, &removed_screens);
            affected_workspaces.extend(reassigned);
        }

        // Restore workspaces to their configured screens if they came back
        let restored = restore_workspaces_to_configured_screens(state);
        affected_workspaces.extend(restored);
    }

    // Ensure each screen has at least one workspace
    ensure_screen_workspaces(state);

    // Set initial focus if not already set
    if state.get_focused_workspace().is_none() {
        set_initial_focus(state);
    }

    // Trigger layout recomputation for affected workspaces
    if let Some(handle) = get_subscriber_handle() {
        for ws_id in affected_workspaces {
            handle.notify_layout_changed(ws_id, true);
        }
    }

    log::info!("tiling: {} workspace(s) configured", state.workspaces.len());
}

/// Handles pre-detected screens being set.
///
/// This is the preferred way to set screens during initialization, as it
/// doesn't require calling macOS APIs from the async actor task.
pub fn on_set_screens(state: &mut TilingState, screens: Vec<Screen>) {
    log::debug!("tiling: on_set_screens called with {} screens", screens.len());

    if screens.is_empty() {
        log::warn!("tiling: no screens provided to on_set_screens");
        return;
    }

    // Check if this is initial setup (no screens yet)
    let is_initial_setup = state.screens.is_empty();

    // Build set of new screen IDs
    let new_screen_ids: std::collections::HashSet<u32> = screens.iter().map(|s| s.id).collect();

    // Find screens that were removed
    let old_screen_ids: Vec<u32> = state.screens.iter().map(|s| s.id).collect();
    let removed_screens: Vec<u32> =
        old_screen_ids.into_iter().filter(|id| !new_screen_ids.contains(id)).collect();

    // Update screens
    for screen in screens {
        state.upsert_screen(screen);
    }

    // Remove old screens
    for screen_id in &removed_screens {
        state.remove_screen(*screen_id);
        log::debug!("Removed screen {screen_id}");
    }

    // Track workspaces that need layout recomputation
    let mut affected_workspaces = Vec::new();

    // On initial setup, create workspaces from config
    if is_initial_setup {
        create_workspaces_from_config(state);
    } else {
        // Handle screen hotplug
        if !removed_screens.is_empty() {
            // Reassign workspaces from removed screens to main
            let reassigned = reassign_workspaces_from_removed_screens(state, &removed_screens);
            affected_workspaces.extend(reassigned);
        }

        // Restore workspaces to their configured screens if they came back
        let restored = restore_workspaces_to_configured_screens(state);
        affected_workspaces.extend(restored);
    }

    // Ensure each screen has at least one workspace
    ensure_screen_workspaces(state);

    // Set initial focus if not already set
    if state.get_focused_workspace().is_none() {
        set_initial_focus(state);
    }

    // Trigger layout recomputation for affected workspaces
    if let Some(handle) = get_subscriber_handle() {
        for ws_id in affected_workspaces {
            handle.notify_layout_changed(ws_id, true);
        }
    }

    log::info!(
        "tiling: {} workspace(s) configured on {} screen(s)",
        state.workspaces.len(),
        state.screens.len()
    );
}

/// Creates workspaces from configuration.
fn create_workspaces_from_config(state: &mut TilingState) {
    let config = get_config();
    let tiling_config = &config.tiling;

    if tiling_config.workspaces.is_empty() {
        // Create default workspaces (one per screen)
        create_default_workspaces(state);
    } else {
        // Create workspaces from config
        log::debug!(
            "tiling: creating {} workspaces from config",
            tiling_config.workspaces.len()
        );
        for ws_config in &tiling_config.workspaces {
            // Find the screen for this workspace
            // If the configured screen doesn't exist, fall back to main screen
            let screen_id = resolve_screen_name(state, &ws_config.screen).or_else(|| {
                log::trace!(
                    "tiling: workspace '{}' screen '{}' not found, falling back to main",
                    ws_config.name,
                    ws_config.screen
                );
                state.get_main_screen().map(|s| s.id)
            });

            if let Some(screen_id) = screen_id {
                let layout = convert_layout_type(ws_config.layout);
                let workspace = Workspace {
                    id: uuid::Uuid::now_v7(),
                    name: ws_config.name.clone(),
                    screen_id,
                    layout,
                    is_visible: false,
                    is_focused: false,
                    window_ids: Vec::new(),
                    focused_window_index: None,
                    split_ratios: Vec::new(),
                    configured_screen: Some(ws_config.screen.clone()),
                };
                state.upsert_workspace(workspace);
                log::debug!(
                    "Created workspace '{}' on screen {} with layout {:?}",
                    ws_config.name,
                    screen_id,
                    layout
                );
            }
        }
    }
}

/// Creates a default workspace for each screen.
fn create_default_workspaces(state: &mut TilingState) {
    let screen_info: Vec<(usize, u32)> =
        state.screens.iter().enumerate().map(|(i, s)| (i, s.id)).collect();

    for (i, screen_id) in screen_info {
        let name = format!("workspace-{}", i + 1);
        let workspace = Workspace {
            id: uuid::Uuid::now_v7(),
            name: name.clone(),
            screen_id,
            layout: LayoutType::Dwindle,
            is_visible: false,
            is_focused: false,
            window_ids: Vec::new(),
            focused_window_index: None,
            split_ratios: Vec::new(),
            configured_screen: None,
        };
        state.upsert_workspace(workspace);
        log::debug!("Created default workspace '{name}' on screen {screen_id}");
    }
}

/// Ensures each screen has at least one workspace.
fn ensure_screen_workspaces(state: &mut TilingState) {
    let screen_ids: Vec<u32> = state.screens.iter().map(|s| s.id).collect();

    for screen_id in screen_ids {
        let has_workspace = state.workspaces.iter().any(|w| w.screen_id == screen_id);

        if !has_workspace {
            let name = format!("default-{screen_id}");
            let workspace = Workspace {
                id: uuid::Uuid::now_v7(),
                name: name.clone(),
                screen_id,
                layout: LayoutType::Dwindle,
                is_visible: false,
                is_focused: false,
                window_ids: Vec::new(),
                focused_window_index: None,
                split_ratios: Vec::new(),
                configured_screen: None,
            };
            state.upsert_workspace(workspace);
            log::debug!("Created fallback workspace '{name}' for screen {screen_id}");
        }
    }
}

/// Resolves a screen name to a screen ID.
fn resolve_screen_name(state: &TilingState, name: &str) -> Option<u32> {
    // "main" or "primary" matches the main screen
    if name == "main" || name == "primary" {
        return state.get_main_screen().map(|s| s.id);
    }

    // "secondary" matches the first non-main screen
    if name == "secondary" {
        return state.screens.iter().find(|s| !s.is_main).map(|s| s.id);
    }

    // Try to match by display name (exact)
    if let Some(screen) = state.screens.iter().find(|s| s.name == name) {
        return Some(screen.id);
    }

    // Try case-insensitive partial match
    let name_lower = name.to_lowercase();
    state
        .screens
        .iter()
        .find(|s| s.name.to_lowercase().contains(&name_lower))
        .map(|s| s.id)
}

/// Sets initial focus and visibility for workspaces.
///
/// - One workspace per screen is marked as visible (the first one assigned to that screen)
/// - Only the workspace on the main screen is focused
fn set_initial_focus(state: &mut TilingState) {
    let main_screen_id = state.get_main_screen().map(|s| s.id);

    // Collect all screen IDs
    let screen_ids: Vec<u32> = state.screens.iter().map(|s| s.id).collect();

    // For each screen, find the first workspace and mark it visible
    let mut focused_ws_id: Option<uuid::Uuid> = None;

    for screen_id in &screen_ids {
        // Find first workspace on this screen
        let first_ws_on_screen =
            state.workspaces.iter().find(|ws| ws.screen_id == *screen_id).map(|ws| ws.id);

        if let Some(ws_id) = first_ws_on_screen {
            let is_main_screen = main_screen_id == Some(*screen_id);

            state.update_workspace(ws_id, |ws| {
                ws.is_visible = true;
                ws.is_focused = is_main_screen;
            });

            // Track the focused workspace (on main screen)
            if is_main_screen {
                focused_ws_id = Some(ws_id);
            }

            log::debug!(
                "Set workspace {ws_id} as visible on screen {screen_id} (focused: {is_main_screen})"
            );
        }
    }

    // Set the focused workspace in the focus state
    if let Some(ws_id) = focused_ws_id {
        state.set_focused_workspace(Some(ws_id));
        log::debug!("Set initial focus to workspace {ws_id}");
    }
}

/// Converts config `LayoutType` to state `LayoutType`.
const fn convert_layout_type(config_layout: crate::config::LayoutType) -> LayoutType {
    match config_layout {
        crate::config::LayoutType::Dwindle => LayoutType::Dwindle,
        crate::config::LayoutType::Split => LayoutType::Split,
        crate::config::LayoutType::SplitVertical => LayoutType::SplitVertical,
        crate::config::LayoutType::SplitHorizontal => LayoutType::SplitHorizontal,
        crate::config::LayoutType::Monocle => LayoutType::Monocle,
        crate::config::LayoutType::Master => LayoutType::Master,
        crate::config::LayoutType::Grid => LayoutType::Grid,
        crate::config::LayoutType::Floating => LayoutType::Floating,
    }
}

/// Reassigns workspaces from removed screens to the main screen.
///
/// Returns the IDs of workspaces that were reassigned (for layout recomputation).
fn reassign_workspaces_from_removed_screens(
    state: &mut TilingState,
    removed_screens: &[u32],
) -> Vec<uuid::Uuid> {
    let main_screen_id = state.get_main_screen().map_or(0, |s| s.id);

    if main_screen_id == 0 {
        log::warn!("No main screen to reassign workspaces to");
        return Vec::new();
    }

    // Find workspaces on removed screens
    let workspaces_to_reassign: Vec<uuid::Uuid> = state
        .workspaces
        .iter()
        .filter(|ws| removed_screens.contains(&ws.screen_id))
        .map(|ws| ws.id)
        .collect();

    for ws_id in &workspaces_to_reassign {
        state.update_workspace(*ws_id, |ws| {
            log::info!(
                "Screen unplugged: moving workspace '{}' from screen {} to main screen {}",
                ws.name,
                ws.screen_id,
                main_screen_id
            );
            ws.screen_id = main_screen_id;
        });
    }

    workspaces_to_reassign
}

/// Restores workspaces to their configured screens when those screens become available.
///
/// This is called when screens change (hotplug). For each workspace that has a
/// `configured_screen` set, we check if that screen is now available. If it is
/// and the workspace is currently on a different screen, we move it back.
///
/// Returns the IDs of workspaces that were restored (for layout recomputation).
fn restore_workspaces_to_configured_screens(state: &mut TilingState) -> Vec<uuid::Uuid> {
    let mut restored_workspaces = Vec::new();

    // Collect workspaces that might need restoration
    // (those with a configured_screen that doesn't match their current screen)
    let workspaces_to_check: Vec<(uuid::Uuid, String, u32)> = state
        .workspaces
        .iter()
        .filter_map(|ws| {
            ws.configured_screen
                .as_ref()
                .map(|configured| (ws.id, configured.clone(), ws.screen_id))
        })
        .collect();

    for (ws_id, configured_screen, current_screen_id) in workspaces_to_check {
        // Try to resolve the configured screen name to an ID
        if let Some(target_screen_id) = resolve_screen_name(state, &configured_screen) {
            // Only move if the workspace is not already on its configured screen
            if current_screen_id != target_screen_id {
                state.update_workspace(ws_id, |ws| {
                    log::info!(
                        "Screen plugged back in: restoring workspace '{}' to screen {} ('{}')",
                        ws.name,
                        target_screen_id,
                        configured_screen
                    );
                    ws.screen_id = target_screen_id;
                });
                restored_workspaces.push(ws_id);
            }
        }
    }

    restored_workspaces
}

/// Gets the current screen list from macOS.
///
/// NOTE: This function uses `NSScreen` APIs which must be called from the main thread.
/// Do not call this from async tasks or background threads.
///
/// The returned frames are converted from macOS's bottom-left origin coordinate system
/// to the top-left origin system used by CGWindowList and the Accessibility API.
pub fn get_screens_from_macos() -> Vec<Screen> {
    // First, get the main screen height for coordinate transformation
    let main_screen_height = get_main_screen_height();

    let display_ids = get_active_display_ids();

    display_ids
        .into_iter()
        .filter_map(|id| get_screen_info(id, main_screen_height))
        .collect()
}

/// Gets the height of the main screen for coordinate transformation.
fn get_main_screen_height() -> f64 {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    unsafe {
        let main_screen: *mut Object = msg_send![class!(NSScreen), mainScreen];
        if main_screen.is_null() {
            return 1080.0; // Fallback
        }

        let frame: NSRect = msg_send![main_screen, frame];
        frame.size.height
    }
}

/// Gets all active display IDs from CoreGraphics.
fn get_active_display_ids() -> Vec<u32> {
    let mut display_count: u32 = 0;

    // First, get the count
    let result = unsafe {
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGGetActiveDisplayList(
                max_displays: u32,
                active_displays: *mut u32,
                display_count: *mut u32,
            ) -> i32;
        }

        CGGetActiveDisplayList(0, std::ptr::null_mut(), &raw mut display_count)
    };

    if result != 0 || display_count == 0 {
        // Fall back to just the main display
        return vec![CGDisplay::main().id];
    }

    // Allocate buffer and get display list
    let mut displays = vec![0u32; display_count as usize];

    let result = unsafe {
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGGetActiveDisplayList(
                max_displays: u32,
                active_displays: *mut u32,
                display_count: *mut u32,
            ) -> i32;
        }

        CGGetActiveDisplayList(display_count, displays.as_mut_ptr(), &raw mut display_count)
    };

    if result != 0 {
        return vec![CGDisplay::main().id];
    }

    displays.truncate(display_count as usize);
    displays
}

/// Gets screen info for a specific display ID.
///
/// The `main_screen_height` parameter is used for coordinate transformation from
/// macOS's bottom-left origin to the top-left origin used by the Accessibility API.
fn get_screen_info(display_id: u32, main_screen_height: f64) -> Option<Screen> {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    unsafe {
        // Get NSScreen for this display
        let screens: *mut Object = msg_send![class!(NSScreen), screens];
        if screens.is_null() {
            return None;
        }

        let count: usize = msg_send![screens, count];

        for i in 0..count {
            let screen: *mut Object = msg_send![screens, objectAtIndex: i];
            if screen.is_null() {
                continue;
            }

            // Get the display ID for this NSScreen
            let device_description: *mut Object = msg_send![screen, deviceDescription];
            if device_description.is_null() {
                continue;
            }

            let screen_number_key = crate::utils::objc::nsstring("NSScreenNumber");
            let screen_number_obj: *mut Object =
                msg_send![device_description, objectForKey: screen_number_key];
            if screen_number_obj.is_null() {
                continue;
            }

            let screen_id: u32 = msg_send![screen_number_obj, unsignedIntValue];
            if screen_id != display_id {
                continue;
            }

            // Found the screen, extract info (in NSScreen bottom-left coordinates)
            let frame: NSRect = msg_send![screen, frame];
            let visible_frame: NSRect = msg_send![screen, visibleFrame];
            let backing_scale: f64 = msg_send![screen, backingScaleFactor];

            // Get localized name (macOS 10.15+)
            let name: String = {
                let name_ns: *mut Object = msg_send![screen, localizedName];
                if name_ns.is_null() {
                    format!("Display {display_id}")
                } else {
                    crate::utils::objc::nsstring_to_string(name_ns)
                }
            };

            // Check if main screen
            let main_screen: *mut Object = msg_send![class!(NSScreen), mainScreen];
            let is_main = !main_screen.is_null() && std::ptr::eq(screen, main_screen);

            // Get refresh rate
            let refresh_rate = get_display_refresh_rate(display_id);

            // Check if built-in (laptop screen)
            let is_builtin = CGDisplay::new(display_id).is_builtin();

            // Convert from NSScreen (bottom-left origin) to AX API (top-left origin)
            // Formula: new_y = main_screen_height - old_y - rect_height
            let converted_frame = convert_to_top_left_origin(frame, main_screen_height);
            let converted_visible_frame =
                convert_to_top_left_origin(visible_frame, main_screen_height);

            return Some(Screen {
                id: display_id,
                name,
                frame: converted_frame,
                visible_frame: converted_visible_frame,
                scale_factor: backing_scale,
                is_main,
                is_builtin,
                refresh_rate,
            });
        }

        None
    }
}

/// Converts an NSRect from macOS's bottom-left coordinate system to top-left.
///
/// # Arguments
///
/// * `rect` - The rectangle in NSScreen coordinates (bottom-left origin)
/// * `main_screen_height` - Height of the main screen for coordinate transformation
///
/// # Returns
///
/// A `Rect` in top-left coordinate system (used by CGWindowList and AX API)
fn convert_to_top_left_origin(rect: NSRect, main_screen_height: f64) -> Rect {
    // In NSScreen coordinates (bottom-left origin):
    // - rect.origin.y is the distance from the bottom of the main screen to the bottom of the rect
    //
    // In CGWindowList/AX coordinates (top-left origin):
    // - We need the distance from the top of the main screen to the top of the rect
    //
    // Formula: new_y = main_screen_height - old_y - rect_height
    let new_y = main_screen_height - rect.origin.y - rect.size.height;

    Rect::new(rect.origin.x, new_y, rect.size.width, rect.size.height)
}

/// Gets the refresh rate for a display.
fn get_display_refresh_rate(display_id: u32) -> f64 {
    let display = CGDisplay::new(display_id);

    if let Some(mode) = display.display_mode() {
        let rate = mode.refresh_rate();
        if rate > 0.0 {
            return rate;
        }
    }

    // Default to 60 Hz
    60.0
}

// NSRect for Objective-C interop
#[repr(C)]
#[derive(Clone, Copy)]
struct NSPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NSSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NSRect {
    origin: NSPoint,
    size: NSSize,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::modules::tiling::state::{LayoutType, Workspace};

    fn make_screen(id: u32, name: &str, is_main: bool) -> Screen {
        Screen {
            id,
            name: name.to_string(),
            frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            visible_frame: Rect::new(0.0, 25.0, 1920.0, 1055.0),
            scale_factor: 1.0,
            is_main,
            is_builtin: is_main,
            refresh_rate: 60.0,
        }
    }

    fn make_workspace(name: &str, screen_id: u32) -> Workspace {
        Workspace {
            id: Uuid::now_v7(),
            name: name.to_string(),
            screen_id,
            layout: LayoutType::Dwindle,
            is_visible: true,
            is_focused: false,
            window_ids: Vec::new(),
            focused_window_index: None,
            split_ratios: Vec::new(),
            configured_screen: None,
        }
    }

    #[test]
    fn test_get_active_display_ids() {
        let ids = get_active_display_ids();
        // Should have at least one display
        assert!(!ids.is_empty());
        // Main display should be in the list
        assert!(ids.contains(&CGDisplay::main().id));
    }

    #[test]
    fn test_reassign_workspaces_from_removed_screens() {
        let mut state = TilingState::new();

        // Add screens
        state.upsert_screen(make_screen(1, "Main", true));
        state.upsert_screen(make_screen(2, "External", false));

        // Add workspaces
        let ws1 = make_workspace("dev", 1);
        let ws2 = make_workspace("web", 2);
        let ws2_id = ws2.id;

        state.upsert_workspace(ws1);
        state.upsert_workspace(ws2);

        // Workspace on screen 2
        assert_eq!(state.get_workspace(ws2_id).unwrap().screen_id, 2);

        // Remove screen 2
        reassign_workspaces_from_removed_screens(&mut state, &[2]);

        // Workspace should now be on screen 1 (main)
        assert_eq!(state.get_workspace(ws2_id).unwrap().screen_id, 1);
    }

    #[test]
    fn test_restore_workspaces_to_configured_screens() {
        let mut state = TilingState::new();

        // Start with only the main screen
        state.upsert_screen(make_screen(1, "Built-in Retina Display", true));

        // Add a workspace that was configured for "secondary" but is currently on main
        let mut ws = make_workspace("web", 1);
        ws.configured_screen = Some("secondary".to_string());
        let ws_id = ws.id;
        state.upsert_workspace(ws);

        // Workspace is on screen 1 (main) because secondary doesn't exist
        assert_eq!(state.get_workspace(ws_id).unwrap().screen_id, 1);

        // Now plug in the secondary screen
        state.upsert_screen(make_screen(2, "LG UltraFine", false));

        // Restore workspaces to their configured screens
        restore_workspaces_to_configured_screens(&mut state);

        // Workspace should now be on screen 2 (secondary)
        assert_eq!(state.get_workspace(ws_id).unwrap().screen_id, 2);
    }

    #[test]
    fn test_screen_hotplug_round_trip() {
        let mut state = TilingState::new();

        // Setup: main screen + external screen
        state.upsert_screen(make_screen(1, "Built-in Retina Display", true));
        state.upsert_screen(make_screen(2, "LG UltraFine", false));

        // Create workspace configured for secondary screen
        let mut ws = make_workspace("external-work", 2);
        ws.configured_screen = Some("secondary".to_string());
        let ws_id = ws.id;
        state.upsert_workspace(ws);

        // Verify it's on the external screen
        assert_eq!(state.get_workspace(ws_id).unwrap().screen_id, 2);

        // UNPLUG: Remove the external screen
        state.remove_screen(2);
        reassign_workspaces_from_removed_screens(&mut state, &[2]);

        // Workspace should move to main screen
        assert_eq!(state.get_workspace(ws_id).unwrap().screen_id, 1);
        // configured_screen should still remember "secondary"
        assert_eq!(
            state.get_workspace(ws_id).unwrap().configured_screen,
            Some("secondary".to_string())
        );

        // PLUG BACK IN: Add the external screen back
        state.upsert_screen(make_screen(2, "LG UltraFine", false));
        restore_workspaces_to_configured_screens(&mut state);

        // Workspace should return to external screen
        assert_eq!(state.get_workspace(ws_id).unwrap().screen_id, 2);
    }

    #[test]
    fn test_get_screen_info_main() {
        let main_id = CGDisplay::main().id;
        let main_height = get_main_screen_height();
        let screen = get_screen_info(main_id, main_height);

        // Should be able to get main screen info
        assert!(screen.is_some());
        let screen = screen.unwrap();
        assert_eq!(screen.id, main_id);
        assert!(screen.is_main);
        assert!(screen.frame.is_valid());
    }
}
