//! Window control operations.
//!
//! This module provides functions to manipulate window position, size, and state.

use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};

use super::ax_cache::{cache_element, get_cached_element};
use super::info::{get_all_windows_including_hidden, get_window_by_id};
use crate::tiling::accessibility::{AccessibilityElement, is_accessibility_enabled};
use crate::tiling::error::TilingError;
use crate::tiling::state::{ManagedWindow, WindowFrame};
use crate::tiling::window::is_pip_window;

/// Result type for window control operations.
pub type ControlResult<T> = Result<T, TilingError>;

/// Gets an accessibility element for a window using its cached info.
/// This is faster than `get_ax_element_for_window` as it doesn't query window info.
///
/// Matching strategy:
/// 1. Check the AX element cache first
/// 2. If only one window, return it directly
/// 3. Try exact title match
/// 4. Try title substring match (handles Edge's different title formats)
/// 5. Try position/frame match
/// 6. Fallback to first window
fn get_ax_element_with_window_info(window: &ManagedWindow) -> ControlResult<AccessibilityElement> {
    if !is_accessibility_enabled() {
        return Err(TilingError::AccessibilityNotAuthorized);
    }

    // Check the cache first
    if let Some(element) = get_cached_element(window.id) {
        return Ok(element);
    }

    // Cache miss - perform lookup
    let element = lookup_ax_element_for_window(window)?;

    // Cache the result for future use
    cache_element(window.id, &element, window.pid);

    Ok(element)
}

/// Performs the actual AX element lookup (cache miss path).
fn lookup_ax_element_for_window(window: &ManagedWindow) -> ControlResult<AccessibilityElement> {
    // Create app element and find the window
    let app = AccessibilityElement::application(window.pid);

    // Get all windows and find the one matching our ID
    let ax_windows = app.get_windows()?;

    // If there's only one window, use it directly without matching
    if ax_windows.len() == 1 {
        return Ok(ax_windows.into_iter().next().unwrap());
    }

    // Collect AX window titles and frames for matching
    let ax_info: Vec<(Option<String>, Option<WindowFrame>)> =
        ax_windows.iter().map(|w| (w.get_title(), w.get_frame().ok())).collect();

    // Try to match by title (most reliable for same-app windows)
    if !window.title.is_empty() {
        // First try: exact title match
        for (i, (ax_title, _)) in ax_info.iter().enumerate() {
            if ax_title.as_ref().is_some_and(|t| t == &window.title) {
                // Need to get the window again since we consumed ax_windows for ax_info
                let ax_windows = app.get_windows()?;
                if let Some(w) = ax_windows.into_iter().nth(i) {
                    return Ok(w);
                }
            }
        }

        // Second try: title substring/contains match (handles Edge's title variations)
        // Edge may report "Page Title - Microsoft Edge" via CGWindowList
        // but "Page Title" or different format via AX API
        let cg_title_base = extract_title_base(&window.title);
        for (i, (ax_title, _)) in ax_info.iter().enumerate() {
            if let Some(ax_t) = ax_title {
                let ax_title_base = extract_title_base(ax_t);
                // Match if either title contains the other's base
                if titles_match_fuzzy(&cg_title_base, &ax_title_base) {
                    let ax_windows = app.get_windows()?;
                    if let Some(w) = ax_windows.into_iter().nth(i) {
                        return Ok(w);
                    }
                }
            }
        }
    }

    // Third try: match by position/size
    let ax_windows = app.get_windows()?;
    match_window_by_position(ax_windows, window)
}

/// Extracts the base title by removing common browser suffixes.
/// For example: "Page Title - Microsoft Edge" -> "Page Title"
fn extract_title_base(title: &str) -> String {
    // Common browser suffixes to strip
    const SUFFIXES: &[&str] = &[
        " - Microsoft Edge",
        " - Microsoft\u{00a0}Edge", // Non-breaking space variant
        " – Microsoft Edge",        // En-dash variant
        " — Microsoft Edge",        // Em-dash variant
        " - Google Chrome",
        " - Mozilla Firefox",
        " - Safari",
        " - Arc",
        " - Brave",
    ];

    let mut base = title.to_string();
    for suffix in SUFFIXES {
        if let Some(pos) = base.rfind(suffix) {
            base.truncate(pos);
            break;
        }
    }
    base.trim().to_string()
}

/// Checks if two title bases match using fuzzy comparison.
/// Handles cases where titles may differ slightly between `CGWindowList` and AX API.
fn titles_match_fuzzy(title1: &str, title2: &str) -> bool {
    if title1.is_empty() || title2.is_empty() {
        return false;
    }

    // Exact match after base extraction
    if title1 == title2 {
        return true;
    }

    // One contains the other (handles truncation differences)
    if title1.contains(title2) || title2.contains(title1) {
        return true;
    }

    // Check if they share a significant common prefix (at least 10 chars or 50% of shorter)
    let min_len = title1.len().min(title2.len());
    let required_prefix = (min_len / 2).max(10).min(min_len);

    let common_prefix_len = title1.chars().zip(title2.chars()).take_while(|(a, b)| a == b).count();

    common_prefix_len >= required_prefix
}

/// Matches a window by position/size from a list of accessibility elements.
/// Uses a scoring system to find the best match, which is important for monocle
/// layouts where all windows have nearly identical frames.
fn match_window_by_position(
    ax_windows: Vec<AccessibilityElement>,
    window: &ManagedWindow,
) -> ControlResult<AccessibilityElement> {
    let mut best_match: Option<(AccessibilityElement, i32)> = None;

    for ax_window in ax_windows {
        if let Ok(frame) = ax_window.get_frame() {
            // Calculate the total difference (Manhattan distance) between frames
            #[allow(clippy::cast_possible_wrap)]
            let diff = (frame.x - window.frame.x).abs()
                + (frame.y - window.frame.y).abs()
                + (frame.width as i32 - window.frame.width as i32).abs()
                + (frame.height as i32 - window.frame.height as i32).abs();

            // Exact match - return immediately
            if diff == 0 {
                return Ok(ax_window);
            }

            // Keep track of the best (closest) match
            if best_match.as_ref().is_none_or(|(_, best_diff)| diff < *best_diff) {
                best_match = Some((ax_window, diff));
            }
        }
    }

    best_match
        .map(|(element, _)| element)
        .ok_or(TilingError::WindowNotFound(window.id))
}

/// Gets an accessibility element for a window.
fn get_ax_element_for_window(window_id: u64) -> ControlResult<AccessibilityElement> {
    if !is_accessibility_enabled() {
        return Err(TilingError::AccessibilityNotAuthorized);
    }

    // Check the cache first
    if let Some(element) = get_cached_element(window_id) {
        return Ok(element);
    }

    // Get the window info to find the PID
    // Try on-screen windows first, then fall back to all windows (including hidden)
    let window = get_window_by_id(window_id).or_else(|_| {
        get_all_windows_including_hidden()?
            .into_iter()
            .find(|w| w.id == window_id)
            .ok_or(TilingError::WindowNotFound(window_id))
    })?;

    // Use the shared lookup function
    let element = lookup_ax_element_for_window(&window)?;

    // Cache the result
    cache_element(window_id, &element, window.pid);

    Ok(element)
}

/// Closes a window using the Accessibility API.
///
/// This performs the `AXPress` action on the window's close button.
pub fn close_window(window_id: u64) -> ControlResult<()> {
    let ax_element = get_ax_element_for_window(window_id)?;

    // Get the close button (first child of window with AXCloseButton subrole)
    // We use AXPress action on the window which triggers the close
    ax_element.perform_action("AXRaise")?;

    // Try to find and press the close button
    // The close button is typically accessed via the window's AXCloseButton attribute
    let close_button = ax_element.get_element_attribute("AXCloseButton")?;
    close_button.perform_action("AXPress")
}

/// Sets a window's frame (position and size).
pub fn set_window_frame(window_id: u64, frame: &WindowFrame) -> ControlResult<()> {
    let ax_element = get_ax_element_for_window(window_id)?;
    ax_element.set_frame(frame)
}

/// Focuses a window (brings it to front) using cached window info.
/// This is much faster than `focus_window` as it doesn't query the window list.
pub fn focus_window_fast(window: &ManagedWindow) -> ControlResult<()> {
    if is_pip_window(window) {
        return Err(TilingError::OperationFailed(
            "Cannot focus Picture-in-Picture windows".to_string(),
        ));
    }

    // Get the accessibility element for the window first
    let ax_element = get_ax_element_with_window_info(window)?;

    // Raise and focus the window BEFORE activating the app
    // This ensures the correct window is on top when the app becomes active
    ax_element.focus()?;

    // Then activate the application to bring it to the foreground
    activate_app(window.pid)?;

    // Focus the window again after activation to ensure it's the main window
    ax_element.focus()
}

/// Focuses a window (brings it to front).
/// Note: Prefer `focus_window_fast` when you have the `ManagedWindow` available.
pub fn focus_window(window_id: u64) -> ControlResult<()> {
    let window = get_window_by_id(window_id)?;
    focus_window_fast(&window)
}

/// Cycles to the next or previous window of the given app.
///
/// This uses the AX window list which is ordered by z-order (front to back).
/// For "next": focuses the second window (the one behind the current front window)
/// For "previous": focuses the last window (will become front after focus)
///
/// This is more reliable than trying to match specific windows by title,
/// since some apps (like Edge) report different titles via `CGWindowList` vs AX.
///
/// Note: This function is currently unused but kept for potential future use
/// (e.g., a "cycle within app" command distinct from "cycle all windows").
#[allow(dead_code)]
pub fn cycle_app_window(pid: i32, direction: &str) -> ControlResult<()> {
    if !is_accessibility_enabled() {
        return Err(TilingError::AccessibilityNotAuthorized);
    }

    let app = AccessibilityElement::application(pid);
    let ax_windows = app.get_windows()?;

    if ax_windows.len() < 2 {
        return Ok(()); // Nothing to cycle if only one window
    }

    // AX windows are ordered by z-order (front to back)
    // For "next": focus the second window (index 1)
    // For "previous": focus the last window
    let target_index = if direction == "next" {
        1 // The window right behind the current front window
    } else {
        ax_windows.len() - 1 // The backmost window
    };

    let target_window =
        ax_windows.into_iter().nth(target_index).ok_or(TilingError::WindowNotFound(0))?;

    // Focus and raise the target window
    target_window.focus()?;

    // Activate the app to ensure it's in the foreground
    activate_app(pid)?;

    // Focus again after activation
    target_window.focus()
}

/// Activates an application by PID.
fn activate_app(pid: i32) -> ControlResult<()> {
    let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) else {
        return Err(TilingError::OperationFailed(
            "Failed to find application".to_string(),
        ));
    };

    // Activate the application and bring all its windows forward
    // Note: ActivateIgnoringOtherApps was deprecated in macOS 14, but we still
    // need to activate the app. Use ActivateAllWindows for now.
    app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);

    Ok(())
}

/// Hides an application (all its windows) using the `AXHidden` attribute.
/// This is equivalent to pressing Cmd+H.
pub fn hide_app(pid: i32) -> ControlResult<()> {
    if !is_accessibility_enabled() {
        return Err(TilingError::AccessibilityNotAuthorized);
    }

    let app = AccessibilityElement::application(pid);
    app.set_hidden(true)
}

/// Unhides an application (shows all its windows) using the `AXHidden` attribute.
pub fn unhide_app(pid: i32) -> ControlResult<()> {
    if !is_accessibility_enabled() {
        return Err(TilingError::AccessibilityNotAuthorized);
    }

    let app = AccessibilityElement::application(pid);
    app.set_hidden(false)
}
