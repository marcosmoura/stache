//! Screen detection for the tiling window manager.
//!
//! This module provides functions to detect and enumerate connected displays
//! using macOS's `NSScreen` API.

use objc::runtime::{Class, Object};
use objc::{msg_send, sel, sel_impl};

use super::state::{Rect, Screen};

// ============================================================================
// Objective-C Type Definitions
// ============================================================================

/// Objective-C `NSRect` structure.
#[repr(C)]
#[derive(Debug, Copy, Clone)]
struct NSRect {
    origin: NSPoint,
    size: NSSize,
}

/// Objective-C `NSPoint` structure.
#[repr(C)]
#[derive(Debug, Copy, Clone)]
struct NSPoint {
    x: f64,
    y: f64,
}

/// Objective-C `NSSize` structure.
#[repr(C)]
#[derive(Debug, Copy, Clone)]
struct NSSize {
    width: f64,
    height: f64,
}

impl From<NSRect> for Rect {
    fn from(rect: NSRect) -> Self {
        Self::new(rect.origin.x, rect.origin.y, rect.size.width, rect.size.height)
    }
}

// ============================================================================
// Screen Detection
// ============================================================================

/// Gets all connected screens.
///
/// Returns a vector of `Screen` objects representing all connected displays.
/// The first screen in the array is the main screen (with the menu bar).
///
/// # Returns
///
/// A vector of screens, or an empty vector if screen detection fails.
///
/// # Coordinate System
///
/// The returned screen frames are in the **top-left origin** coordinate system
/// used by `CGWindowList` and the Accessibility API. This is converted from
/// macOS's native `NSScreen` coordinates which use a bottom-left origin.
#[must_use]
pub fn get_all_screens() -> Vec<Screen> { unsafe { get_all_screens_unsafe() } }

/// Internal implementation for screen detection.
///
/// # Safety
///
/// This function uses Objective-C runtime calls which are inherently unsafe.
/// It is safe to call from Rust as long as:
/// - The Objective-C runtime is initialized (always true in a macOS app)
/// - `NSScreen` class exists (always true on macOS)
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
unsafe fn get_all_screens_unsafe() -> Vec<Screen> {
    let mut screens = Vec::new();

    // Get NSScreen class
    let Some(screen_class) = Class::get("NSScreen") else {
        eprintln!("stache: tiling: failed to get NSScreen class");
        return screens;
    };

    // Get the array of all screens
    let ns_screens: *mut Object = msg_send![screen_class, screens];
    if ns_screens.is_null() {
        eprintln!("stache: tiling: failed to get screens array");
        return screens;
    }

    // Get the main screen for comparison and coordinate conversion
    let main_screen: *mut Object = msg_send![screen_class, mainScreen];

    // Get the main screen's height for coordinate conversion
    // NSScreen uses bottom-left origin, but CGWindowList/AX use top-left origin
    let main_screen_height: f64 = if main_screen.is_null() {
        0.0
    } else {
        let main_frame: NSRect = msg_send![main_screen, frame];
        main_frame.size.height
    };

    // Get screen count
    let count: usize = msg_send![ns_screens, count];

    for i in 0..count {
        let ns_screen: *mut Object = msg_send![ns_screens, objectAtIndex: i];
        if ns_screen.is_null() {
            continue;
        }

        if let Some(screen) =
            unsafe { screen_from_nsscreen(ns_screen, main_screen, main_screen_height) }
        {
            screens.push(screen);
        }
    }

    screens
}

/// Converts an `NSScreen` object to our Screen struct.
///
/// The frames are converted from macOS's bottom-left origin coordinate system
/// to the top-left origin system used by `CGWindowList` and the Accessibility API.
///
/// # Safety
///
/// `ns_screen` must be a valid pointer to an `NSScreen` object.
/// `main_screen` can be null if there's no main screen.
/// `main_screen_height` is the height of the main screen for coordinate conversion.
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
unsafe fn screen_from_nsscreen(
    ns_screen: *mut Object,
    main_screen: *mut Object,
    main_screen_height: f64,
) -> Option<Screen> {
    if ns_screen.is_null() {
        return None;
    }

    // Get frames (in NSScreen's bottom-left coordinate system)
    let frame: NSRect = msg_send![ns_screen, frame];
    let visible_frame: NSRect = msg_send![ns_screen, visibleFrame];

    // Convert from bottom-left to top-left coordinate system
    // In NSScreen: Y=0 is at bottom, Y increases upward
    // In CGWindowList/AX: Y=0 is at top, Y increases downward
    //
    // Conversion formula: new_y = main_screen_height - old_y - rect_height
    let converted_frame = convert_nsrect_to_top_left(frame, main_screen_height);
    let converted_visible_frame = convert_nsrect_to_top_left(visible_frame, main_screen_height);

    // Get scale factor
    let scale_factor: f64 = msg_send![ns_screen, backingScaleFactor];

    // Check if this is the main screen
    let is_main = !main_screen.is_null() && std::ptr::eq(ns_screen, main_screen);

    // Get device description dictionary for display ID and other info
    let device_desc: *mut Object = msg_send![ns_screen, deviceDescription];
    if device_desc.is_null() {
        return None;
    }

    // Get display ID from device description
    let display_id = unsafe { get_display_id(device_desc) };

    // Get screen name
    let name = unsafe { get_screen_name(ns_screen, display_id) };

    // Check if this is a built-in display
    let is_builtin = is_builtin_display(display_id);

    Some(Screen::new(
        display_id,
        name,
        converted_frame,
        converted_visible_frame,
        is_main,
        is_builtin,
        scale_factor,
    ))
}

/// Converts an `NSRect` from macOS's bottom-left coordinate system to top-left.
///
/// # Arguments
///
/// * `rect` - The rectangle in `NSScreen` coordinates (bottom-left origin)
/// * `main_screen_height` - Height of the main screen for coordinate transformation
///
/// # Returns
///
/// A `Rect` in top-left coordinate system (used by `CGWindowList` and AX API)
fn convert_nsrect_to_top_left(rect: NSRect, main_screen_height: f64) -> Rect {
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

/// Gets the display ID from the device description dictionary.
///
/// # Safety
///
/// `device_desc` must be a valid pointer to an `NSDictionary`.
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
unsafe fn get_display_id(device_desc: *mut Object) -> u32 {
    // Key: @"NSScreenNumber"
    let key = unsafe { create_nsstring("NSScreenNumber") };
    if key.is_null() {
        return 0;
    }

    let id_obj: *mut Object = msg_send![device_desc, objectForKey: key];
    if id_obj.is_null() {
        return 0;
    }

    // The value is an NSNumber, get its unsigned int value
    let id: u32 = msg_send![id_obj, unsignedIntValue];
    id
}

/// Gets a human-readable name for the screen.
///
/// Tries to use `localizedName` (macOS 10.15+), falls back to display ID.
///
/// # Safety
///
/// `ns_screen` must be a valid pointer to an `NSScreen` object.
unsafe fn get_screen_name(ns_screen: *mut Object, display_id: u32) -> String {
    // Try localizedName first (available on macOS 10.15+)
    // This gives names like "Built-in Retina Display" or "LG HDR 4K"
    let localized_name: *mut Object = msg_send![ns_screen, localizedName];

    if !localized_name.is_null()
        && let Some(name) = unsafe { nsstring_to_rust(localized_name) }
        && !name.is_empty()
    {
        return name;
    }

    // Fallback to display ID-based name
    format!("Display {display_id}")
}

/// Checks if a display is a built-in display (laptop screen).
///
/// Uses `CGDisplayIsBuiltin` from CoreGraphics.
#[allow(clippy::cast_possible_truncation)]
fn is_builtin_display(display_id: u32) -> bool {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGDisplayIsBuiltin(display: u32) -> i32;
    }

    unsafe { CGDisplayIsBuiltin(display_id) != 0 }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Creates an `NSString` from a Rust string.
///
/// # Safety
///
/// Returns a pointer to an `NSString`. The string is autoreleased.
unsafe fn create_nsstring(s: &str) -> *mut Object {
    let Some(nsstring_class) = Class::get("NSString") else {
        return std::ptr::null_mut();
    };

    let c_str = std::ffi::CString::new(s).ok();
    let Some(c_str) = c_str else {
        return std::ptr::null_mut();
    };

    msg_send![nsstring_class, stringWithUTF8String: c_str.as_ptr()]
}

/// Converts an `NSString` to a Rust String.
///
/// # Safety
///
/// `nsstring` must be a valid pointer to an `NSString` object or null.
unsafe fn nsstring_to_rust(nsstring: *mut Object) -> Option<String> {
    if nsstring.is_null() {
        return None;
    }

    let utf8_ptr: *const i8 = msg_send![nsstring, UTF8String];
    if utf8_ptr.is_null() {
        return None;
    }

    let c_str = unsafe { std::ffi::CStr::from_ptr(utf8_ptr) };
    c_str.to_str().ok().map(String::from)
}

/// Gets the main screen.
///
/// # Returns
///
/// The main screen (with the menu bar), or None if detection fails.
#[must_use]
pub fn get_main_screen() -> Option<Screen> { get_all_screens().into_iter().find(|s| s.is_main) }

/// Gets the number of connected screens.
#[must_use]
pub fn get_screen_count() -> usize {
    unsafe {
        let Some(screen_class) = Class::get("NSScreen") else {
            return 1;
        };

        let screens: *mut Object = msg_send![screen_class, screens];
        if screens.is_null() {
            return 1;
        }

        let count: usize = msg_send![screens, count];
        if count == 0 { 1 } else { count }
    }
}

/// Finds a screen by its display ID.
#[must_use]
pub fn get_screen_by_id(display_id: u32) -> Option<Screen> {
    get_all_screens().into_iter().find(|s| s.id == display_id)
}

/// Finds a screen by name (case-insensitive).
///
/// Special names:
/// - `"main"` or `"primary"` - returns the main screen (with menu bar)
/// - `"builtin"` - returns the built-in display (laptop screen)
/// - `"secondary"` - returns the non-main screen (only when exactly 2 screens)
#[must_use]
pub fn get_screen_by_name(name: &str) -> Option<Screen> {
    let screens = get_all_screens();

    match name.to_lowercase().as_str() {
        "main" | "primary" => screens.into_iter().find(|s| s.is_main),
        "builtin" => screens.into_iter().find(|s| s.is_builtin),
        "secondary" => {
            // Return the non-main screen when there are exactly 2 screens
            if screens.len() == 2 {
                screens.into_iter().find(|s| !s.is_main)
            } else {
                None
            }
        }
        _ => screens.into_iter().find(|s| s.name.eq_ignore_ascii_case(name)),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_screen_count() {
        let count = get_screen_count();
        assert!(count >= 1, "Should have at least one screen");
    }

    #[test]
    fn test_get_all_screens() {
        let screens = get_all_screens();
        assert!(!screens.is_empty(), "Should have at least one screen");

        // Verify there's exactly one main screen
        let main_count = screens.iter().filter(|s| s.is_main).count();
        assert_eq!(main_count, 1, "Should have exactly one main screen");

        // Verify all screens have valid dimensions
        for screen in &screens {
            assert!(screen.frame.width > 0.0, "Screen width should be positive");
            assert!(screen.frame.height > 0.0, "Screen height should be positive");
            assert!(screen.scale_factor >= 1.0, "Scale factor should be at least 1.0");
        }
    }

    #[test]
    fn test_get_main_screen() {
        let main = get_main_screen();
        assert!(main.is_some(), "Should have a main screen");

        let main = main.unwrap();
        assert!(main.is_main, "Main screen should have is_main = true");
    }

    #[test]
    fn test_screen_by_name_main() {
        let main = get_screen_by_name("main");
        assert!(main.is_some(), "Should find main screen by name 'main'");
        assert!(main.unwrap().is_main);
    }

    #[test]
    fn test_screen_by_name_primary() {
        let primary = get_screen_by_name("primary");
        assert!(primary.is_some(), "Should find main screen by name 'primary'");
        assert!(primary.unwrap().is_main);

        // Verify "primary" and "main" return the same screen
        let main = get_screen_by_name("main").unwrap();
        let primary = get_screen_by_name("primary").unwrap();
        assert_eq!(
            main.id, primary.id,
            "'main' and 'primary' should return the same screen"
        );
    }

    #[test]
    fn test_screen_by_name_case_insensitive() {
        let main1 = get_screen_by_name("MAIN");
        let main2 = get_screen_by_name("Main");
        let main3 = get_screen_by_name("main");

        assert!(main1.is_some());
        assert!(main2.is_some());
        assert!(main3.is_some());
    }

    #[test]
    fn test_screen_by_name_secondary() {
        let screens = get_all_screens();
        let secondary = get_screen_by_name("secondary");

        if screens.len() == 2 {
            // Should find the non-main screen
            assert!(
                secondary.is_some(),
                "Should find secondary screen with 2 screens"
            );
            assert!(
                !secondary.unwrap().is_main,
                "Secondary should not be main screen"
            );
        } else {
            // Should not find secondary with != 2 screens
            assert!(
                secondary.is_none(),
                "Should not find 'secondary' with {} screens",
                screens.len()
            );
        }
    }

    #[test]
    fn test_nsrect_to_rect_conversion() {
        let ns_rect = NSRect {
            origin: NSPoint { x: 10.0, y: 20.0 },
            size: NSSize { width: 100.0, height: 200.0 },
        };

        let rect: Rect = ns_rect.into();
        assert!((rect.x - 10.0).abs() < f64::EPSILON);
        assert!((rect.y - 20.0).abs() < f64::EPSILON);
        assert!((rect.width - 100.0).abs() < f64::EPSILON);
        assert!((rect.height - 200.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_screen_has_display_id() {
        let screens = get_all_screens();
        for screen in &screens {
            // Display ID should be non-zero on macOS
            // (ID 0 might be valid in some edge cases, so we just check it's set)
            assert!(
                screen.id > 0 || screens.len() == 1,
                "Screen should have a display ID"
            );
        }
    }

    #[test]
    fn test_screen_visible_frame_smaller_than_frame() {
        let screens = get_all_screens();
        for screen in &screens {
            // Visible frame should be smaller or equal to full frame
            // (menu bar and dock take up space)
            assert!(
                screen.visible_frame.height <= screen.frame.height,
                "Visible height should be <= full height"
            );
        }
    }

    #[test]
    fn test_convert_nsrect_to_top_left_main_screen() {
        // Test coordinate conversion for a rect on the main screen
        // Main screen: 1920x1080, with a 25px menu bar at top
        let main_screen_height = 1080.0;

        // NSScreen visible_frame for main screen:
        // In NSScreen coords: y=0 is at bottom, so visible_frame.y = 0 (bottom of screen)
        // and the height is reduced by menu bar: height = 1055
        let ns_rect = NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size: NSSize { width: 1920.0, height: 1055.0 },
        };

        let converted = convert_nsrect_to_top_left(ns_rect, main_screen_height);

        // In top-left coords:
        // The visible area should start at y=25 (below the menu bar)
        assert!((converted.x - 0.0).abs() < f64::EPSILON);
        assert!((converted.y - 25.0).abs() < f64::EPSILON);
        assert!((converted.width - 1920.0).abs() < f64::EPSILON);
        assert!((converted.height - 1055.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_convert_nsrect_to_top_left_secondary_screen_above() {
        // Test coordinate conversion for a secondary screen positioned above the main screen
        // Main screen: 1920x1080
        // Secondary screen: 1920x1080, positioned above main screen
        let main_screen_height = 1080.0;

        // In NSScreen coords: secondary screen above main has y = main_height = 1080
        let ns_rect = NSRect {
            origin: NSPoint {
                x: 0.0,
                y: 1080.0, // Bottom of secondary is at top of main in NS coords
            },
            size: NSSize { width: 1920.0, height: 1080.0 },
        };

        let converted = convert_nsrect_to_top_left(ns_rect, main_screen_height);

        // In top-left coords: secondary screen above main has negative Y
        // new_y = 1080 - 1080 - 1080 = -1080
        assert!((converted.x - 0.0).abs() < f64::EPSILON);
        assert!((converted.y - (-1080.0)).abs() < f64::EPSILON);
        assert!((converted.width - 1920.0).abs() < f64::EPSILON);
        assert!((converted.height - 1080.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_convert_nsrect_to_top_left_portrait_screen_left() {
        // Test coordinate conversion for a portrait screen positioned to the left
        // Main screen: 2560x1440
        // Portrait screen: 1440x2560, positioned to the left of main
        let main_screen_height = 1440.0;

        // In NSScreen coords: portrait screen left of main
        // The bottom of the portrait screen aligns with the bottom of main
        // So y = 0 in NS coords
        let ns_rect = NSRect {
            origin: NSPoint {
                x: -1440.0, // To the left of main
                y: 0.0,     // Bottom-aligned with main
            },
            size: NSSize { width: 1440.0, height: 2560.0 },
        };

        let converted = convert_nsrect_to_top_left(ns_rect, main_screen_height);

        // In top-left coords:
        // new_y = 1440 - 0 - 2560 = -1120 (top of portrait is above top of main)
        assert!((converted.x - (-1440.0)).abs() < f64::EPSILON);
        assert!((converted.y - (-1120.0)).abs() < f64::EPSILON);
        assert!((converted.width - 1440.0).abs() < f64::EPSILON);
        assert!((converted.height - 2560.0).abs() < f64::EPSILON);
    }
}
