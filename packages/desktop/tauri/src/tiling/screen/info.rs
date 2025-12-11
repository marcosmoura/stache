//! Screen information and enumeration.
//!
//! This module provides functions to query connected displays.

use core_graphics::display::{
    CGDirectDisplayID, CGDisplay, CGDisplayBounds, CGDisplayCopyDisplayMode,
    CGDisplayModeGetRefreshRate, CGDisplayModeRelease, CGGetActiveDisplayList, CGMainDisplayID,
};
use objc2::MainThreadMarker;
use objc2_app_kit::NSScreen;
use objc2_foundation::{NSNumber, NSRect, NSString};

use crate::tiling::error::TilingError;
use crate::tiling::state::{Screen, ScreenFrame};

/// Result type for screen operations.
pub type ScreenResult<T> = Result<T, TilingError>;

/// Gets the list of all connected screens.
pub fn get_all_screens() -> ScreenResult<Vec<Screen>> {
    let display_ids = get_display_ids()?;
    let main_display_id = unsafe { CGMainDisplayID() };

    // Get NSScreen info indexed by display ID for exact matching
    let ns_screen_map = get_ns_screen_map();

    let mut screens = Vec::with_capacity(display_ids.len());

    for display_id in display_ids {
        let _display = CGDisplay::new(display_id);
        let bounds = unsafe { CGDisplayBounds(display_id) };

        let is_main = display_id == main_display_id;

        // Get display name
        let name = if is_main {
            "Main Display".to_string()
        } else {
            format!("Display {display_id}")
        };

        // Convert bounds to our frame type
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let frame = ScreenFrame {
            x: bounds.origin.x as i32,
            y: bounds.origin.y as i32,
            width: bounds.size.width as u32,
            height: bounds.size.height as u32,
        };

        // Find matching NSScreen by display ID for accurate visible frame
        let usable_frame =
            get_usable_frame_for_display(display_id, &ns_screen_map, &frame, is_main);

        screens.push(Screen {
            id: display_id.to_string(),
            name,
            is_main,
            frame,
            usable_frame,
        });
    }

    // Sort so main display is first
    screens.sort_by(|a, b| b.is_main.cmp(&a.is_main));

    Ok(screens)
}

/// Screen info from `NSScreen`: (`display_id`, `visible_frame`).
struct NSScreenInfo {
    display_id: CGDirectDisplayID,
    visible_frame: NSRect,
}

/// Gets `NSScreen` information mapped by display ID.
fn get_ns_screen_map() -> Vec<NSScreenInfo> {
    use objc2::msg_send;

    let mut result = Vec::new();

    // Get main thread marker; NSScreen requires main thread
    let Some(mtm) = MainThreadMarker::new() else {
        return result;
    };

    let screens = NSScreen::screens(mtm);

    let screen_number_key = NSString::from_str("NSScreenNumber");

    for screen in screens {
        // Get the deviceDescription dictionary
        let device_desc = screen.deviceDescription();

        // Get the NSScreenNumber key to get the CGDirectDisplayID
        let Some(screen_number) = device_desc.objectForKey(&screen_number_key) else {
            continue;
        };

        // SAFETY: We know this is an NSNumber from the deviceDescription dictionary
        let screen_number: &NSNumber = unsafe { std::mem::transmute(screen_number) };

        // NSNumber -> unsigned int (CGDirectDisplayID)
        let display_id: CGDirectDisplayID = screen_number.unsignedIntValue();

        // Get the visible frame using msg_send since visibleFrame returns NSRect
        let visible_frame: NSRect = unsafe { msg_send![&screen, visibleFrame] };

        result.push(NSScreenInfo { display_id, visible_frame });
    }

    result
}

/// Gets the usable frame for a specific display ID.
/// Uses `NSScreen`'s visibleFrame which properly accounts for menu bar and dock.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn get_usable_frame_for_display(
    display_id: CGDirectDisplayID,
    ns_screens: &[NSScreenInfo],
    cg_frame: &ScreenFrame,
    is_main: bool,
) -> ScreenFrame {
    use objc2::msg_send;

    // Find the matching NSScreen by display ID
    for info in ns_screens {
        if info.display_id == display_id {
            let visible = &info.visible_frame;

            // NSScreen uses bottom-left origin (Cocoa coordinates)
            // CGDisplay uses top-left origin (Quartz coordinates)
            // We need to convert the visibleFrame to Quartz coordinates

            // Get the main screen height to convert coordinates
            let main_screen_height =
                MainThreadMarker::new()
                    .and_then(NSScreen::mainScreen)
                    .map_or(0.0, |main_screen| {
                        let main_frame: NSRect = unsafe { msg_send![&main_screen, frame] };
                        main_frame.size.height
                    });

            // In Cocoa, origin.y is distance from bottom of main screen to bottom of visible area
            // In Quartz, y=0 is at the top of the main screen
            // quartz_y = main_screen_height - cocoa_y - height
            let quartz_y = main_screen_height - visible.origin.y - visible.size.height;

            return ScreenFrame {
                x: visible.origin.x as i32,
                y: quartz_y as i32,
                width: visible.size.width as u32,
                height: visible.size.height as u32,
            };
        }
    }

    // Fallback: estimate menu bar height for main only
    let menu_bar_height = if is_main { 25 } else { 0 };
    ScreenFrame {
        x: cg_frame.x,
        y: cg_frame.y + menu_bar_height,
        width: cg_frame.width,
        height: cg_frame.height.saturating_sub(menu_bar_height as u32),
    }
}

/// Gets raw display IDs from Core Graphics.
fn get_display_ids() -> ScreenResult<Vec<CGDirectDisplayID>> {
    // First call to get count
    let mut display_count: u32 = 0;
    let result = unsafe { CGGetActiveDisplayList(0, std::ptr::null_mut(), &raw mut display_count) };

    if result != 0 {
        return Err(TilingError::OperationFailed(format!(
            "Failed to get display count: {result}"
        )));
    }

    if display_count == 0 {
        return Ok(Vec::new());
    }

    // Second call to get display IDs
    let mut display_ids = vec![0u32; display_count as usize];
    let result = unsafe {
        CGGetActiveDisplayList(display_count, display_ids.as_mut_ptr(), &raw mut display_count)
    };

    if result != 0 {
        return Err(TilingError::OperationFailed(format!(
            "Failed to get display list: {result}"
        )));
    }

    Ok(display_ids)
}

/// Gets the maximum refresh rate across all connected displays.
///
/// Returns the highest refresh rate in Hz. Falls back to 60Hz if no displays
/// are found or if the refresh rate cannot be determined.
///
/// This is useful for determining animation frame timing when `CVDisplayLink`
/// is not available.
#[must_use]
pub fn get_max_refresh_rate() -> u32 {
    const DEFAULT_REFRESH_RATE: u32 = 120;

    let Ok(display_ids) = get_display_ids() else {
        return DEFAULT_REFRESH_RATE;
    };

    if display_ids.is_empty() {
        return DEFAULT_REFRESH_RATE;
    }

    let mut max_rate: f64 = 0.0;

    for display_id in display_ids {
        // Get the current display mode
        let mode = unsafe { CGDisplayCopyDisplayMode(display_id) };
        if mode.is_null() {
            continue;
        }

        // Get the refresh rate from the display mode
        let rate = unsafe { CGDisplayModeGetRefreshRate(mode) };

        // Release the display mode
        unsafe { CGDisplayModeRelease(mode) };

        // Some displays report 0 Hz (e.g., LCD panels with variable refresh)
        // In this case, assume 120 Hz
        let effective_rate = if rate > 0.0 {
            rate
        } else {
            f64::from(DEFAULT_REFRESH_RATE)
        };

        if effective_rate > max_rate {
            max_rate = effective_rate;
        }
    }

    // Return at least the default rate
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    if max_rate > 0.0 {
        max_rate.round() as u32
    } else {
        DEFAULT_REFRESH_RATE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_all_screens() {
        // This test requires a display, skip in headless CI
        if std::env::var("CI").is_ok() {
            return;
        }

        let screens = get_all_screens().unwrap();
        assert!(!screens.is_empty());
        assert!(screens.iter().any(|s| s.is_main));
    }

    #[test]
    fn test_get_max_refresh_rate() {
        // This test requires a display, skip in headless CI
        if std::env::var("CI").is_ok() {
            return;
        }

        let rate = get_max_refresh_rate();
        // Refresh rate should be at least 60Hz
        assert!(rate >= 60, "Expected refresh rate >= 60Hz, got {rate}Hz");
        // And at most 240Hz (current high-end displays)
        assert!(rate <= 240, "Expected refresh rate <= 240Hz, got {rate}Hz");
    }
}
