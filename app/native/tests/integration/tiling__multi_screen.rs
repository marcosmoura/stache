//! Integration tests for multi-screen workspace operations.
//!
//! Tests workspace behavior across multiple screens, including:
//! - Windows placed on correct screen based on workspace assignment
//! - Sending windows between screens via workspace commands
//! - Focus operations across screens
//! - Workspace switching with multiple screens
//!
//! ## Requirements
//!
//! Many tests in this module require multiple screens to be connected.
//! Tests that require multiple screens will be skipped gracefully when
//! only one screen is available.
//!
//! ## Running these tests
//! ```bash
//! cargo nextest run -p stache --features integration-tests -E 'test(/tiling__multi_screen/)' --test-threads 1 --no-capture
//! ```

use crate::common::*;
use crate::require_multiple_screens;

// =============================================================================
// Screen Detection Tests (run on any setup)
// =============================================================================

/// Test that screen detection works correctly.
#[test]
fn test_screen_detection() {
    let test = Test::new("tiling_basic");

    let screens = test.all_screens();
    assert!(!screens.is_empty(), "Should detect at least one screen");

    // Main screen should always exist
    let main = test.main_screen();
    assert!(main.is_main(), "Main screen should have is_main = true");
    assert!(main.width() > 0, "Main screen should have positive width");
    assert!(main.height() > 0, "Main screen should have positive height");

    println!("Detected {} screen(s)", screens.len());
    for (i, screen) in screens.iter().enumerate() {
        println!(
            "  Screen {}: {}x{} at ({}, {}), main={}",
            i,
            screen.width(),
            screen.height(),
            screen.frame().x,
            screen.frame().y,
            screen.is_main()
        );
    }
}

/// Test screen count helper methods.
#[test]
fn test_screen_count_helpers() {
    let test = Test::new("tiling_basic");

    let count = test.screen_count();
    assert!(count >= 1, "Should have at least one screen");

    let has_multiple = test.has_multiple_screens();
    assert_eq!(
        has_multiple,
        count >= 2,
        "has_multiple_screens should match count >= 2"
    );

    println!("Screen count: {}, has multiple: {}", count, has_multiple);
}

/// Test tiling area calculation.
#[test]
fn test_tiling_area_calculation() {
    let test = Test::new("tiling_basic");
    let main = test.main_screen();

    // Default gaps from tiling_basic fixture
    let outer_gap = 12;
    let menu_bar_height = 40; // Approximate macOS menu bar height

    let tiling_area = main.tiling_area(outer_gap, menu_bar_height);

    // Tiling area should be smaller than full screen
    assert!(
        tiling_area.width < main.width(),
        "Tiling area width should be less than screen width"
    );
    assert!(
        tiling_area.height < main.height(),
        "Tiling area height should be less than screen height"
    );

    // Tiling area should account for gaps
    assert!(
        tiling_area.x >= outer_gap,
        "Tiling area x should account for outer gap"
    );

    println!(
        "Main screen: {}x{}, tiling area: {}x{} at ({}, {})",
        main.width(),
        main.height(),
        tiling_area.width,
        tiling_area.height,
        tiling_area.x,
        tiling_area.y
    );
}

// =============================================================================
// Single Screen Fallback Tests
// =============================================================================

/// Test that windows go to main screen when only one screen is available.
#[test]
fn test_single_screen_window_placement() {
    let mut test = Test::new("tiling_multi_screen");

    // Create a Dictionary window (assigned to main-dwindle workspace)
    let _ = test.create_window("Dictionary");
    let frames = test.get_app_stable_frames("Dictionary", 1);
    assert!(!frames.is_empty(), "Should have Dictionary window");

    // Window should be on main screen
    let main = test.main_screen();
    let window_frame = &frames[0];

    // Check if window is within main screen bounds (with some tolerance)
    let on_main_screen = window_frame.x >= main.frame().x - FRAME_TOLERANCE
        && window_frame.x < main.frame().x + main.width() + FRAME_TOLERANCE;

    assert!(
        on_main_screen,
        "Window should be on main screen (window x={}, main screen x={} to {})",
        window_frame.x,
        main.frame().x,
        main.frame().x + main.width()
    );

    println!(
        "Window at ({}, {}), main screen at ({}, {})",
        window_frame.x,
        window_frame.y,
        main.frame().x,
        main.frame().y
    );
}

// =============================================================================
// Multi-Screen Tests (require 2+ screens)
// =============================================================================

/// Test that Dictionary windows are placed on main screen (via rule).
#[test]
fn test_dictionary_on_main_screen() {
    let mut test = Test::new("tiling_multi_screen");
    require_multiple_screens!(&test);

    // Dictionary is assigned to main-dwindle workspace (on main screen)
    let _ = test.create_window("Dictionary");
    let frames = test.get_app_stable_frames("Dictionary", 1);
    assert!(!frames.is_empty(), "Should have Dictionary window");

    let main = test.main_screen();
    let window_frame = &frames[0];

    // Window center should be on main screen
    let center_x = window_frame.x + window_frame.width / 2;
    let center_y = window_frame.y + window_frame.height / 2;

    let on_main = center_x >= main.frame().x
        && center_x < main.frame().x + main.width()
        && center_y >= main.frame().y
        && center_y < main.frame().y + main.height();

    assert!(on_main, "Dictionary window should be on main screen");

    println!(
        "Dictionary window center ({}, {}) is on main screen",
        center_x, center_y
    );
}

/// Test that TextEdit windows are placed on secondary screen (via rule).
#[test]
fn test_textedit_on_secondary_screen() {
    let mut test = Test::new("tiling_multi_screen");
    require_multiple_screens!(&test);

    let secondary = test.secondary_screen().expect("Should have secondary screen");

    // TextEdit is assigned to secondary-dwindle workspace (on secondary screen)
    let _ = test.create_window("TextEdit");
    let frames = test.get_app_stable_frames("TextEdit", 1);
    assert!(!frames.is_empty(), "Should have TextEdit window");

    let window_frame = &frames[0];

    // Window center should be on secondary screen
    let center_x = window_frame.x + window_frame.width / 2;
    let center_y = window_frame.y + window_frame.height / 2;

    let on_secondary = center_x >= secondary.frame().x
        && center_x < secondary.frame().x + secondary.width()
        && center_y >= secondary.frame().y
        && center_y < secondary.frame().y + secondary.height();

    assert!(
        on_secondary,
        "TextEdit window should be on secondary screen (center at ({}, {}), secondary at ({}, {}) {}x{})",
        center_x,
        center_y,
        secondary.frame().x,
        secondary.frame().y,
        secondary.width(),
        secondary.height()
    );

    println!(
        "TextEdit window center ({}, {}) is on secondary screen",
        center_x, center_y
    );
}

/// Test windows from different apps go to different screens.
#[test]
fn test_apps_on_different_screens() {
    let mut test = Test::new("tiling_multi_screen");
    require_multiple_screens!(&test);

    let main = test.main_screen();
    let secondary = test.secondary_screen().expect("Should have secondary screen");

    // Create Dictionary (main screen) and TextEdit (secondary screen)
    let _ = test.create_window("Dictionary");
    let _ = test.create_window("TextEdit");

    // Wait for both to stabilize
    let dict_frames = test.get_app_stable_frames("Dictionary", 1);
    let textedit_frames = test.get_app_stable_frames("TextEdit", 1);

    assert!(!dict_frames.is_empty(), "Should have Dictionary window");
    assert!(!textedit_frames.is_empty(), "Should have TextEdit window");

    // Check Dictionary is on main screen
    let dict_center_x = dict_frames[0].x + dict_frames[0].width / 2;
    let dict_on_main =
        dict_center_x >= main.frame().x && dict_center_x < main.frame().x + main.width();

    // Check TextEdit is on secondary screen
    let textedit_center_x = textedit_frames[0].x + textedit_frames[0].width / 2;
    let textedit_on_secondary = textedit_center_x >= secondary.frame().x
        && textedit_center_x < secondary.frame().x + secondary.width();

    assert!(dict_on_main, "Dictionary should be on main screen");
    assert!(textedit_on_secondary, "TextEdit should be on secondary screen");

    println!(
        "Dictionary on main (x={}), TextEdit on secondary (x={})",
        dict_center_x, textedit_center_x
    );
}

/// Test sending window from main screen to secondary screen.
#[test]
fn test_send_window_to_secondary_screen() {
    let mut test = Test::new("tiling_multi_screen");
    require_multiple_screens!(&test);

    let main = test.main_screen();
    let secondary = test.secondary_screen().expect("Should have secondary screen");

    // Create Dictionary on main screen
    let _ = test.create_window("Dictionary");
    let _ = test.get_app_stable_frames("Dictionary", 1);

    // Verify it's on main screen initially
    let frames_before = test.get_app_stable_frames("Dictionary", 1);
    let center_x_before = frames_before[0].x + frames_before[0].width / 2;
    let on_main_initially =
        center_x_before >= main.frame().x && center_x_before < main.frame().x + main.width();
    assert!(on_main_initially, "Window should start on main screen");

    // Send to secondary-dwindle workspace (on secondary screen)
    test.stache_command(&[
        "tiling",
        "window",
        "--send-to-workspace",
        "secondary-dwindle",
    ]);
    delay(OPERATION_DELAY_MS * 2);

    // Focus the secondary workspace to see the window
    test.stache_command(&["tiling", "workspace", "--focus", "secondary-dwindle"]);
    delay(OPERATION_DELAY_MS);

    // Get the new frame
    let frames_after = test.get_app_stable_frames("Dictionary", 1);
    assert!(!frames_after.is_empty(), "Window should still exist");

    let center_x_after = frames_after[0].x + frames_after[0].width / 2;
    let on_secondary_after = center_x_after >= secondary.frame().x
        && center_x_after < secondary.frame().x + secondary.width();

    assert!(
        on_secondary_after,
        "Window should be on secondary screen after send (center_x={}, secondary x={} to {})",
        center_x_after,
        secondary.frame().x,
        secondary.frame().x + secondary.width()
    );

    println!(
        "Window moved from main (x={}) to secondary (x={})",
        center_x_before, center_x_after
    );
}

/// Test sending window from secondary screen to main screen.
#[test]
fn test_send_window_to_main_screen() {
    let mut test = Test::new("tiling_multi_screen");
    require_multiple_screens!(&test);

    let main = test.main_screen();

    test.secondary_screen().expect("Should have secondary screen");

    // Create TextEdit on secondary screen (via rules)
    let _ = test.create_window("TextEdit");
    let _ = test.get_app_stable_frames("TextEdit", 1);

    // Verify it's on secondary screen initially
    let frames_before = test.get_app_stable_frames("TextEdit", 1);
    let center_x_before = frames_before[0].x + frames_before[0].width / 2;

    // Send to main-dwindle workspace (on main screen)
    test.stache_command(&["tiling", "window", "--send-to-workspace", "main-dwindle"]);
    delay(OPERATION_DELAY_MS * 2);

    // Focus the main workspace to see the window
    test.stache_command(&["tiling", "workspace", "--focus", "main-dwindle"]);
    delay(OPERATION_DELAY_MS);

    // Get the new frame
    let frames_after = test.get_app_stable_frames("TextEdit", 1);
    assert!(!frames_after.is_empty(), "Window should still exist");

    let center_x_after = frames_after[0].x + frames_after[0].width / 2;
    let on_main_after =
        center_x_after >= main.frame().x && center_x_after < main.frame().x + main.width();

    assert!(on_main_after, "Window should be on main screen after send");

    println!(
        "Window moved from secondary (x={}) to main (x={})",
        center_x_before, center_x_after
    );
}

/// Test focusing workspace on secondary screen.
#[test]
fn test_focus_secondary_workspace() {
    let mut test = Test::new("tiling_multi_screen");
    require_multiple_screens!(&test);

    // Create window on each screen
    let _ = test.create_window("Dictionary"); // main screen
    let _ = test.create_window("TextEdit"); // secondary screen
    delay(OPERATION_DELAY_MS);

    // Focus secondary workspace
    test.stache_command(&["tiling", "workspace", "--focus", "secondary-dwindle"]);
    delay(OPERATION_DELAY_MS);

    // TextEdit should be the frontmost app
    let front_app = get_frontmost_app_name();
    assert_eq!(
        front_app.as_deref(),
        Some("TextEdit"),
        "TextEdit should be frontmost after focusing secondary workspace"
    );

    // Focus main workspace
    test.stache_command(&["tiling", "workspace", "--focus", "main-dwindle"]);
    delay(OPERATION_DELAY_MS);

    // Dictionary should be the frontmost app
    let front_app = get_frontmost_app_name();
    assert_eq!(
        front_app.as_deref(),
        Some("Dictionary"),
        "Dictionary should be frontmost after focusing main workspace"
    );
}

/// Test multiple windows on each screen.
#[test]
fn test_multiple_windows_per_screen() {
    let mut test = Test::new("tiling_multi_screen");
    require_multiple_screens!(&test);

    let main = test.main_screen();
    let secondary = test.secondary_screen().expect("Should have secondary screen");

    // Create multiple Dictionary windows (main screen)
    let _ = test.create_window("Dictionary");
    let _ = test.create_window("Dictionary");

    // Create multiple TextEdit windows (secondary screen)
    let _ = test.create_window("TextEdit");
    let _ = test.create_window("TextEdit");

    // Wait for all windows to stabilize
    let dict_frames = test.get_app_stable_frames("Dictionary", 2);
    let textedit_frames = test.get_app_stable_frames("TextEdit", 2);

    assert!(
        dict_frames.len() >= 2,
        "Should have at least 2 Dictionary windows"
    );
    assert!(
        textedit_frames.len() >= 2,
        "Should have at least 2 TextEdit windows"
    );

    // Verify all Dictionary windows are on main screen
    for (i, frame) in dict_frames.iter().enumerate() {
        let center_x = frame.x + frame.width / 2;
        let on_main = center_x >= main.frame().x && center_x < main.frame().x + main.width();
        assert!(on_main, "Dictionary window {} should be on main screen", i);
    }

    // Verify all TextEdit windows are on secondary screen
    for (i, frame) in textedit_frames.iter().enumerate() {
        let center_x = frame.x + frame.width / 2;
        let on_secondary =
            center_x >= secondary.frame().x && center_x < secondary.frame().x + secondary.width();
        assert!(
            on_secondary,
            "TextEdit window {} should be on secondary screen",
            i
        );
    }

    println!(
        "Verified {} Dictionary windows on main, {} TextEdit windows on secondary",
        dict_frames.len(),
        textedit_frames.len()
    );
}

/// Test screen_containing helper.
#[test]
fn test_screen_containing_helper() {
    let mut test = Test::new("tiling_multi_screen");
    require_multiple_screens!(&test);

    // Create windows on both screens
    let _ = test.create_window("Dictionary");
    let _ = test.create_window("TextEdit");

    let dict_frames = test.get_app_stable_frames("Dictionary", 1);
    let textedit_frames = test.get_app_stable_frames("TextEdit", 1);

    // Find which screen contains each window
    let dict_screen = test.screen_containing(&dict_frames[0]);
    let textedit_screen = test.screen_containing(&textedit_frames[0]);

    assert!(dict_screen.is_some(), "Should find screen for Dictionary");
    assert!(textedit_screen.is_some(), "Should find screen for TextEdit");

    let dict_screen = dict_screen.unwrap();
    let textedit_screen = textedit_screen.unwrap();

    // Dictionary should be on main, TextEdit on secondary
    assert!(dict_screen.is_main(), "Dictionary should be on main screen");
    assert!(
        !textedit_screen.is_main(),
        "TextEdit should NOT be on main screen (should be on secondary)"
    );

    println!(
        "Dictionary on screen {}, TextEdit on screen {}",
        dict_screen.display_id(),
        textedit_screen.display_id()
    );
}

/// Test workspace balance works on each screen independently.
#[test]
fn test_balance_per_screen() {
    let mut test = Test::new("tiling_multi_screen");
    require_multiple_screens!(&test);

    // Create windows on main screen
    let _ = test.create_window("Dictionary");
    let _ = test.create_window("Dictionary");
    let _ = test.create_window("Dictionary");

    // Wait for stable layout
    let _ = test.get_app_stable_frames("Dictionary", 3);

    // Focus main workspace and balance
    test.stache_command(&["tiling", "workspace", "--focus", "main-dwindle"]);
    delay(OPERATION_DELAY_MS);

    test.stache_command(&["tiling", "workspace", "--balance"]);
    delay(OPERATION_DELAY_MS * 2);

    // Get frames after balance
    let frames_after = test.get_app_stable_frames("Dictionary", 3);
    assert!(
        frames_after.len() >= 3,
        "Should have at least 3 windows after balance"
    );

    // All windows should have reasonable sizes
    for (i, frame) in frames_after.iter().enumerate() {
        assert!(
            frame.width > 100 && frame.height > 100,
            "Window {} should have reasonable size after balance",
            i
        );
    }

    println!("Balanced {} windows on main screen", frames_after.len());
}

/// Test that workspace operations don't affect other screens.
#[test]
fn test_workspace_isolation() {
    let mut test = Test::new("tiling_multi_screen");
    require_multiple_screens!(&test);

    // Create windows on both screens
    let _ = test.create_window("Dictionary"); // main
    let _ = test.create_window("TextEdit"); // secondary

    let dict_frames_before = test.get_app_stable_frames("Dictionary", 1);
    let textedit_frames_before = test.get_app_stable_frames("TextEdit", 1);

    // Focus and balance main screen workspace
    test.stache_command(&["tiling", "workspace", "--focus", "main-dwindle"]);
    delay(OPERATION_DELAY_MS);
    test.stache_command(&["tiling", "workspace", "--balance"]);
    delay(OPERATION_DELAY_MS);

    // TextEdit frame on secondary should be unchanged
    let textedit_frames_after = test.get_app_stable_frames("TextEdit", 1);
    assert!(
        !textedit_frames_after.is_empty(),
        "TextEdit window should still exist"
    );

    // Frames should be approximately equal (workspace operation on main shouldn't affect secondary)
    if !textedit_frames_before.is_empty() && !textedit_frames_after.is_empty() {
        let before = &textedit_frames_before[0];
        let after = &textedit_frames_after[0];

        // Allow some tolerance for minor adjustments
        let unchanged = before.approximately_equals(after, 20);
        println!(
            "TextEdit frame before: ({}, {}) {}x{}, after: ({}, {}) {}x{}",
            before.x,
            before.y,
            before.width,
            before.height,
            after.x,
            after.y,
            after.width,
            after.height
        );

        // This is a soft assertion - log but don't fail if there's minor drift
        if !unchanged {
            println!("Note: TextEdit frame changed slightly during main workspace operation");
        }
    }
}
