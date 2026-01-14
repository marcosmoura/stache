//! Integration tests for the tiling window manager.
//!
//! These tests create real windows on screen and require accessibility permissions.
//!
//! This test file is gated behind the `integration-tests` feature to prevent
//! it from running during normal `cargo test` invocations.
//!
//! ## Running Integration Tests
//!
//! ```bash
//! # Run all integration tests
//! cargo test -p stache --features integration-tests --test tiling_integration -- --nocapture
//!
//! # Run a specific test
//! cargo test -p stache --features integration-tests --test tiling_integration test_create_textedit -- --nocapture
//! ```
//!
//! ## Requirements
//!
//! - Accessibility permissions granted to the terminal/test runner
//! - Grant in: System Settings > Privacy & Security > Accessibility

use std::process::Command;
use std::thread;
use std::time::Duration;

// ============================================================================
// AppleScript Helpers
// ============================================================================

/// Checks if accessibility permissions are granted.
///
/// Returns `true` if the current process has accessibility permissions.
fn check_accessibility_permission() -> bool {
    let output = Command::new("osascript")
        .args([
            "-e",
            r#"
            tell application "System Events"
                return UI elements enabled
            end tell
            "#,
        ])
        .output();

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            stdout.trim() == "true"
        }
        Err(_) => false,
    }
}

/// Asserts that accessibility permissions are granted, with helpful error message.
fn require_accessibility_permission() {
    if !check_accessibility_permission() {
        panic!(
            "\n\n\
            ╔══════════════════════════════════════════════════════════════════╗\n\
            ║                  ACCESSIBILITY PERMISSION REQUIRED               ║\n\
            ╠══════════════════════════════════════════════════════════════════╣\n\
            ║  Integration tests need accessibility permissions to work.       ║\n\
            ║                                                                  ║\n\
            ║  To grant permission:                                            ║\n\
            ║  1. Open System Settings > Privacy & Security > Accessibility   ║\n\
            ║  2. Add and enable your terminal app (Terminal, iTerm2, etc.)   ║\n\
            ║  3. Re-run the tests                                            ║\n\
            ╚══════════════════════════════════════════════════════════════════╝\n\n"
        );
    }
}

/// Creates a new TextEdit window with the given title.
///
/// Returns the process ID of TextEdit if successful.
fn create_textedit_window(title: &str) -> Option<i32> {
    let output = Command::new("osascript")
        .args([
            "-e",
            &format!(
                r#"
                tell application "TextEdit"
                    activate
                    make new document with properties {{name:"{}"}}
                    set pid to (unix id of (info for (path to frontmost application)))
                    return pid
                end tell
                "#,
                title
            ),
        ])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.trim().parse().ok()
}

/// Closes all TextEdit windows and quits the application.
fn close_all_textedit_windows() {
    let _ = Command::new("osascript")
        .args([
            "-e",
            r#"
            tell application "TextEdit"
                close every window saving no
                quit
            end tell
            "#,
        ])
        .output();

    // Wait for TextEdit to fully quit
    thread::sleep(Duration::from_millis(200));
}

/// Creates a new Finder window at the given path.
///
/// Returns the window ID if available.
fn create_finder_window(path: &str) -> Option<u32> {
    let output = Command::new("osascript")
        .args([
            "-e",
            &format!(
                r#"
                tell application "Finder"
                    activate
                    make new Finder window to (POSIX file "{}")
                    set windowId to id of front window
                    return windowId
                end tell
                "#,
                path
            ),
        ])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.trim().parse().ok()
}

/// Closes all Finder windows.
fn close_all_finder_windows() {
    let _ = Command::new("osascript")
        .args([
            "-e",
            r#"
            tell application "Finder"
                close every window
            end tell
            "#,
        ])
        .output();

    // Wait for windows to close
    thread::sleep(Duration::from_millis(200));
}

/// Gets the frame (position and size) of the frontmost window.
fn get_frontmost_window_frame() -> Option<(f64, f64, f64, f64)> {
    let output = Command::new("osascript")
        .args([
            "-e",
            r#"
            tell application "System Events"
                set frontApp to first application process whose frontmost is true
                set frontWindow to first window of frontApp
                set {x, y} to position of frontWindow
                set {w, h} to size of frontWindow
                return (x as string) & "," & (y as string) & "," & (w as string) & "," & (h as string)
            end tell
            "#,
        ])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = stdout.trim().split(',').collect();
    if parts.len() == 4 {
        Some((
            parts[0].parse().ok()?,
            parts[1].parse().ok()?,
            parts[2].parse().ok()?,
            parts[3].parse().ok()?,
        ))
    } else {
        None
    }
}

/// Sets the frame (position and size) of the frontmost window.
fn set_frontmost_window_frame(x: f64, y: f64, width: f64, height: f64) -> bool {
    let output = Command::new("osascript")
        .args([
            "-e",
            &format!(
                r#"
                tell application "System Events"
                    set frontApp to first application process whose frontmost is true
                    set frontWindow to first window of frontApp
                    set position of frontWindow to {{{}, {}}}
                    set size of frontWindow to {{{}, {}}}
                end tell
                "#,
                x as i32, y as i32, width as i32, height as i32
            ),
        ])
        .output();

    output.is_ok()
}

/// Gets the name of the frontmost application.
fn get_frontmost_app_name() -> Option<String> {
    let output = Command::new("osascript")
        .args([
            "-e",
            r#"
            tell application "System Events"
                set frontApp to first application process whose frontmost is true
                return name of frontApp
            end tell
            "#,
        ])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    Some(stdout.trim().to_string())
}

/// Gets the window count for an application.
fn get_app_window_count(app_name: &str) -> usize {
    let output = Command::new("osascript")
        .args([
            "-e",
            &format!(
                r#"
                tell application "System Events"
                    tell application process "{}"
                        return count of windows
                    end tell
                end tell
                "#,
                app_name
            ),
        ])
        .output();

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            stdout.trim().parse().unwrap_or(0)
        }
        Err(_) => 0,
    }
}

/// Waits for a condition to become true, with timeout.
fn wait_for<F>(timeout_ms: u64, interval_ms: u64, condition: F) -> bool
where F: Fn() -> bool {
    let start = std::time::Instant::now();
    while start.elapsed().as_millis() < timeout_ms as u128 {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(interval_ms));
    }
    false
}

// ============================================================================
// Test Fixture
// ============================================================================

/// Test fixture that ensures cleanup after each test.
struct TestFixture {
    textedit_created: bool,
    finder_created: bool,
}

impl TestFixture {
    fn new() -> Self {
        Self {
            textedit_created: false,
            finder_created: false,
        }
    }

    fn create_textedit_window(&mut self, title: &str) -> Option<i32> {
        self.textedit_created = true;
        create_textedit_window(title)
    }

    fn create_finder_window(&mut self, path: &str) -> Option<u32> {
        self.finder_created = true;
        create_finder_window(path)
    }
}

impl Drop for TestFixture {
    fn drop(&mut self) {
        if self.textedit_created {
            close_all_textedit_windows();
        }
        if self.finder_created {
            close_all_finder_windows();
        }
        // Extra delay to ensure cleanup completes
        thread::sleep(Duration::from_millis(100));
    }
}

// ============================================================================
// Integration Tests - Window Creation
// ============================================================================

#[test]
fn test_accessibility_permission_granted() {
    // This test verifies accessibility permissions are available
    require_accessibility_permission();
    println!("Accessibility permissions: GRANTED");
}

#[test]
fn test_create_textedit_window() {
    require_accessibility_permission();
    let mut fixture = TestFixture::new();

    // Create a TextEdit window
    let pid = fixture.create_textedit_window("Integration Test Document");
    assert!(pid.is_some(), "Failed to create TextEdit window");

    // Wait for window to appear and verify
    let appeared = wait_for(2000, 100, || {
        get_frontmost_app_name().as_deref() == Some("TextEdit")
    });
    assert!(appeared, "TextEdit window should appear");

    // Verify we can get the window frame
    let frame = get_frontmost_window_frame();
    assert!(frame.is_some(), "Should be able to get window frame");

    let (x, y, w, h) = frame.unwrap();
    println!("TextEdit window frame: x={x}, y={y}, width={w}, height={h}");
    assert!(w > 0.0, "Window width should be positive");
    assert!(h > 0.0, "Window height should be positive");
}

#[test]
fn test_create_finder_window() {
    require_accessibility_permission();
    let mut fixture = TestFixture::new();

    // Create a Finder window
    let window_id = fixture.create_finder_window("/tmp");
    assert!(window_id.is_some(), "Failed to create Finder window");

    // Wait for window to appear
    let appeared = wait_for(2000, 100, || {
        get_frontmost_app_name().as_deref() == Some("Finder")
    });
    assert!(appeared, "Finder window should appear");

    // Verify window frame
    let frame = get_frontmost_window_frame();
    assert!(frame.is_some(), "Should be able to get Finder window frame");
}

#[test]
fn test_create_multiple_windows() {
    require_accessibility_permission();
    let mut fixture = TestFixture::new();

    // Create multiple TextEdit windows
    let pid1 = fixture.create_textedit_window("Document 1");
    thread::sleep(Duration::from_millis(300));
    let pid2 = fixture.create_textedit_window("Document 2");
    thread::sleep(Duration::from_millis(300));
    let pid3 = fixture.create_textedit_window("Document 3");
    thread::sleep(Duration::from_millis(300));

    assert!(pid1.is_some(), "Failed to create first window");
    assert!(pid2.is_some(), "Failed to create second window");
    assert!(pid3.is_some(), "Failed to create third window");

    // All should have the same PID (same TextEdit process)
    assert_eq!(pid1, pid2, "All TextEdit windows should be in the same process");
    assert_eq!(pid2, pid3, "All TextEdit windows should be in the same process");

    // Verify window count
    let window_count = get_app_window_count("TextEdit");
    assert!(window_count >= 3, "Should have at least 3 TextEdit windows");
    println!("TextEdit window count: {window_count}");
}

// ============================================================================
// Integration Tests - Window Manipulation
// ============================================================================

#[test]
fn test_move_and_resize_window() {
    require_accessibility_permission();
    let mut fixture = TestFixture::new();

    // Create a TextEdit window
    fixture.create_textedit_window("Move Test");

    // Wait for window to appear
    let appeared = wait_for(2000, 100, || {
        get_frontmost_app_name().as_deref() == Some("TextEdit")
    });
    assert!(appeared, "TextEdit window should appear");

    // Get initial frame
    let initial_frame = get_frontmost_window_frame();
    assert!(initial_frame.is_some(), "Should get initial frame");
    let (init_x, init_y, init_w, init_h) = initial_frame.unwrap();
    println!("Initial frame: x={init_x}, y={init_y}, w={init_w}, h={init_h}");

    // Move and resize the window
    let new_x = 100.0;
    let new_y = 100.0;
    let new_w = 800.0;
    let new_h = 600.0;

    let success = set_frontmost_window_frame(new_x, new_y, new_w, new_h);
    assert!(success, "Should be able to set window frame");

    // Wait for the change to take effect
    thread::sleep(Duration::from_millis(200));

    // Verify the new frame
    let new_frame = get_frontmost_window_frame();
    assert!(new_frame.is_some(), "Should get new frame");
    let (actual_x, actual_y, actual_w, actual_h) = new_frame.unwrap();
    println!("New frame: x={actual_x}, y={actual_y}, w={actual_w}, h={actual_h}");

    // Allow some tolerance for window chrome and macOS adjustments
    let tolerance = 20.0;
    assert!(
        (actual_x - new_x).abs() < tolerance,
        "X position should be close to {new_x}, got {actual_x}"
    );
    assert!(
        (actual_y - new_y).abs() < tolerance,
        "Y position should be close to {new_y}, got {actual_y}"
    );
    assert!(
        (actual_w - new_w).abs() < tolerance,
        "Width should be close to {new_w}, got {actual_w}"
    );
    assert!(
        (actual_h - new_h).abs() < tolerance,
        "Height should be close to {new_h}, got {actual_h}"
    );
}

#[test]
fn test_window_frame_persistence() {
    require_accessibility_permission();
    let mut fixture = TestFixture::new();

    // Create a TextEdit window
    fixture.create_textedit_window("Persistence Test");

    // Wait for window to appear
    wait_for(2000, 100, || {
        get_frontmost_app_name().as_deref() == Some("TextEdit")
    });

    // Set a specific frame
    let target_x = 200.0;
    let target_y = 150.0;
    let target_w = 1000.0;
    let target_h = 700.0;

    set_frontmost_window_frame(target_x, target_y, target_w, target_h);
    thread::sleep(Duration::from_millis(200));

    // Read frame multiple times to verify stability
    let mut frames: Vec<(f64, f64, f64, f64)> = Vec::new();
    for _ in 0..3 {
        if let Some(frame) = get_frontmost_window_frame() {
            frames.push(frame);
        }
        thread::sleep(Duration::from_millis(100));
    }

    assert!(frames.len() >= 2, "Should have multiple frame readings");

    // All frames should be approximately the same (window shouldn't be moving)
    let (first_x, first_y, first_w, first_h) = frames[0];
    for (i, (x, y, w, h)) in frames.iter().enumerate().skip(1) {
        let tolerance = 2.0;
        assert!(
            (x - first_x).abs() < tolerance
                && (y - first_y).abs() < tolerance
                && (w - first_w).abs() < tolerance
                && (h - first_h).abs() < tolerance,
            "Frame {} should match frame 0: ({},{},{},{}) vs ({},{},{},{})",
            i,
            x,
            y,
            w,
            h,
            first_x,
            first_y,
            first_w,
            first_h
        );
    }

    println!("Window frame stable at: x={first_x}, y={first_y}, w={first_w}, h={first_h}");
}

// ============================================================================
// Integration Tests - Cleanup Verification
// ============================================================================

#[test]
fn test_fixture_cleanup() {
    require_accessibility_permission();

    // Create windows in a scope
    {
        let mut fixture = TestFixture::new();
        fixture.create_textedit_window("Cleanup Test 1");
        fixture.create_textedit_window("Cleanup Test 2");

        // Wait for windows
        wait_for(2000, 100, || get_app_window_count("TextEdit") >= 2);

        let count_before = get_app_window_count("TextEdit");
        println!("Windows before cleanup: {count_before}");
        assert!(count_before >= 2, "Should have at least 2 windows");

        // Fixture goes out of scope here and should clean up
    }

    // Wait for cleanup
    thread::sleep(Duration::from_millis(500));

    // TextEdit should be closed
    let count_after = get_app_window_count("TextEdit");
    println!("Windows after cleanup: {count_after}");
    assert_eq!(count_after, 0, "All TextEdit windows should be closed");
}
