//! Resize command handlers.
//!
//! These handlers manage split ratio manipulation, window resizing,
//! and user-initiated resize completion.

use uuid::Uuid;

use crate::modules::tiling::init::get_subscriber_handle;
use crate::modules::tiling::state::{LayoutType, Rect, TilingState};

// ============================================================================
// Split Ratio Initialization
// ============================================================================

/// Initialize default ratios based on layout type and window count.
///
/// Different layouts interpret ratios differently:
/// - Split layouts: cumulative ratios (e.g., [0.33, 0.66] for 3 windows)
/// - Dwindle: direct ratios per split level (e.g., [0.5, 0.5] for 3 windows)
/// - Grid: layout-specific (first ratio often controls master/primary split)
#[allow(clippy::cast_precision_loss)]
fn initialize_default_ratios(layout: LayoutType, window_count: usize) -> Vec<f64> {
    match layout {
        // Split layouts use cumulative ratios
        LayoutType::Split | LayoutType::SplitHorizontal | LayoutType::SplitVertical => {
            (1..window_count).map(|i| i as f64 / window_count as f64).collect()
        }
        // Dwindle uses direct ratios (0.5 for each split by default)
        LayoutType::Dwindle => vec![0.5; window_count.saturating_sub(1)],
        // Grid uses first ratio for primary split
        LayoutType::Grid => {
            // For most grid layouts, first ratio controls master width
            // Default depends on layout structure
            match window_count {
                2 => vec![0.5],        // 2 windows: 50/50 split
                3 => vec![0.5],        // Master + 2 stack: 50% master
                5 => vec![1.0 / 3.0],  // Master + 4 stack: 33% master (1 of 3 cols)
                7 => vec![1.0 / 3.0],  // Master + 6 stack: 33% master
                10 | 11 => vec![0.25], // 3x4 master: 25% master (1 of 4 cols)
                _ => vec![0.5],        // Default
            }
        }
        // Master layout uses master_ratio from workspace config, not split_ratios
        LayoutType::Master | LayoutType::Floating | LayoutType::Monocle => Vec::new(),
    }
}

// ============================================================================
// Split Ratio Resize
// ============================================================================

/// Resize a split at the given index, respecting minimum window sizes.
///
/// When a window that needs to shrink hits its minimum size, it "locks" and
/// the resize continues by shrinking other windows that can still resize.
/// If all windows are at minimum, the resize has no effect.
///
/// The delta is applied to the split ratio at `window_index`.
/// Behavior depends on the layout type:
/// - Split: ratios are cumulative positions, with cascade to other windows
/// - Dwindle: ratios are direct per-split values
/// - Grid: first ratio controls primary split
pub fn on_resize_split(
    state: &mut TilingState,
    workspace_id: Uuid,
    window_index: usize,
    delta: f64,
) {
    let Some(workspace) = state.get_workspace(workspace_id) else {
        log::warn!("resize_split: workspace {workspace_id} not found");
        return;
    };

    let layout = workspace.layout;
    let window_ids = workspace.window_ids.clone();

    // Get layoutable windows in order
    let layoutable: Vec<u32> = window_ids
        .iter()
        .filter(|&&id| state.get_window(id).is_some_and(|w| w.is_layoutable()))
        .copied()
        .collect();

    let window_count = layoutable.len();

    // Skip layouts that don't support split ratios
    if matches!(
        layout,
        LayoutType::Floating | LayoutType::Monocle | LayoutType::Master
    ) {
        log::debug!("resize_split: layout {:?} doesn't use split ratios", layout);
        return;
    }

    if window_count <= 1 {
        log::debug!("resize_split: need at least 2 windows");
        return;
    }

    // Initialize ratios if empty
    #[allow(clippy::redundant_clone)]
    let mut ratios = workspace.split_ratios.clone();
    if ratios.is_empty() {
        ratios = initialize_default_ratios(layout, window_count);
    }

    // Validate index based on layout
    let max_index = match layout {
        LayoutType::Dwindle => window_count.saturating_sub(1),
        LayoutType::Grid => 1,
        _ => window_count.saturating_sub(1),
    };

    if window_index >= max_index {
        log::debug!(
            "resize_split: invalid index {window_index} for {window_count} windows (max: {})",
            max_index.saturating_sub(1)
        );
        return;
    }

    // Ensure ratios vec is large enough
    while ratios.len() < max_index {
        ratios.push(0.5);
    }

    // Get screen for dimension calculations
    let Some(screen) = state.get_screen(workspace.screen_id) else {
        log::debug!("resize_split: screen not found");
        return;
    };

    // Determine which dimension we're resizing (for split layouts)
    let is_horizontal = matches!(layout, LayoutType::Split | LayoutType::SplitHorizontal)
        || (layout == LayoutType::Split
            && screen.visible_frame.width >= screen.visible_frame.height);
    let total_size = if is_horizontal {
        screen.visible_frame.width
    } else {
        screen.visible_frame.height
    };

    // Calculate minimum ratios for each window based on their minimum sizes
    let min_ratios: Vec<f64> = layoutable
        .iter()
        .map(|&id| {
            state
                .get_window(id)
                .and_then(|w| w.minimum_size)
                .map(|(min_w, min_h)| {
                    let min_size = if is_horizontal { min_w } else { min_h };
                    (min_size / total_size).max(0.05)
                })
                .unwrap_or(0.05) // Default minimum 5%
        })
        .collect();

    // Apply delta based on layout type
    match layout {
        LayoutType::Dwindle => {
            // For dwindle, check minimum size constraints before applying
            apply_dwindle_resize_with_minimums(
                &mut ratios,
                window_index,
                delta,
                &layoutable,
                state,
                &screen.visible_frame,
                &screen.name,
                screen.is_main,
            );
        }
        LayoutType::Grid => {
            // For grid, check minimum size constraints before applying
            apply_grid_resize_with_minimums(
                &mut ratios,
                window_index,
                delta,
                &layoutable,
                state,
                &screen.visible_frame,
                &screen.name,
                screen.is_main,
            );
        }
        _ => {
            // For split layouts, apply minimum-aware resizing
            apply_split_resize_with_minimums(&mut ratios, window_index, delta, &min_ratios);
        }
    }

    state.update_workspace(workspace_id, |ws| {
        ws.split_ratios = ratios;
    });

    log::debug!(
        "Resized split at index {window_index} by {delta} (layout: {:?})",
        layout
    );

    // Notify subscriber to recalculate layout
    if let Some(handle) = get_subscriber_handle() {
        handle.notify_layout_changed(workspace_id, true);
    }
}

// ============================================================================
// Split Resize Helpers
// ============================================================================

/// Applies a resize to split layout ratios while respecting minimum sizes.
///
/// For cumulative ratios [r0, r1, r2, ...]:
/// - Window 0: 0.0 to r0
/// - Window 1: r0 to r1
/// - Window 2: r1 to r2
/// - Window N: r(N-1) to 1.0
///
/// When growing window i+1 (decreasing ratio[i]):
/// - Window i shrinks
/// - If window i is at minimum, cascade to shrink earlier windows
///
/// When shrinking window i+1 (increasing ratio[i]):
/// - Window i grows
/// - Windows after i shrink
/// - If any of those windows is at minimum, stop
fn apply_split_resize_with_minimums(
    ratios: &mut [f64],
    index: usize,
    delta: f64,
    min_ratios: &[f64],
) {
    let n = ratios.len() + 1; // number of windows = ratios + 1

    // Calculate current window sizes (as ratios)
    let mut window_sizes: Vec<f64> = Vec::with_capacity(n);
    let mut prev = 0.0;
    for &r in ratios.iter() {
        window_sizes.push(r - prev);
        prev = r;
    }
    window_sizes.push(1.0 - prev); // Last window

    // Growing the window after index (delta < 0 means ratio decreases, window grows)
    // Shrinking the window after index (delta > 0 means ratio increases, window shrinks)
    let growing_window = index + 1; // The window that grows when we decrease ratio[index]
    let shrinking_starts_at = index; // Windows from 0..=index might shrink

    if delta < 0.0 {
        // Growing window at index+1, need to shrink windows 0..=index
        let needed = -delta;
        let mut taken = 0.0;

        // Try to take space from windows, starting from the one adjacent to growing window
        // and cascading backwards
        for i in (0..=shrinking_starts_at).rev() {
            let current_size = window_sizes[i];
            let min_size = min_ratios.get(i).copied().unwrap_or(0.05);
            let available = (current_size - min_size).max(0.0);

            if available > 0.0 {
                let take = (needed - taken).min(available);
                window_sizes[i] -= take;
                taken += take;

                if (taken - needed).abs() < 0.001 {
                    break;
                }
            }
        }

        // Give what we took to the growing window
        window_sizes[growing_window] += taken;
    } else {
        // Shrinking window at index+1, growing windows 0..=index
        // Check if window at index+1 can shrink
        let current_size = window_sizes[growing_window];
        let min_size = min_ratios.get(growing_window).copied().unwrap_or(0.05);
        let available = (current_size - min_size).max(0.0);

        let actual_delta = delta.min(available);

        if actual_delta > 0.0 {
            window_sizes[growing_window] -= actual_delta;
            // Give to the adjacent window (index)
            window_sizes[index] += actual_delta;
        }
    }

    // Convert window sizes back to cumulative ratios
    let mut cumulative = 0.0;
    for i in 0..ratios.len() {
        cumulative += window_sizes[i];
        ratios[i] = cumulative.clamp(0.05, 0.95);
    }

    // Ensure ratios are strictly increasing
    for i in 1..ratios.len() {
        if ratios[i] <= ratios[i - 1] {
            ratios[i] = ratios[i - 1] + 0.05;
        }
    }
}

/// Applies minimum-size-aware resize for Dwindle layout.
///
/// In Dwindle, ratio[i] controls the split between window i and window i+1.
/// This function checks if the resize would violate minimum sizes and
/// limits the resize accordingly.
fn apply_dwindle_resize_with_minimums(
    ratios: &mut [f64],
    index: usize,
    delta: f64,
    layoutable: &[u32],
    state: &TilingState,
    screen_frame: &Rect,
    screen_name: &str,
    is_main_screen: bool,
) {
    use crate::config::get_config;
    use crate::modules::tiling::layout::{Gaps, MasterPosition, calculate_layout_full};

    if index >= ratios.len() {
        return;
    }

    // Get config for gaps
    let config = get_config();
    let bar_offset = if config.bar.is_enabled() && is_main_screen {
        f64::from(config.bar.height) + f64::from(config.bar.padding)
    } else {
        0.0
    };
    let gaps = Gaps::from_config(&config.tiling.gaps, screen_name, is_main_screen, bar_offset);

    // Calculate proposed new ratio
    let current_ratio = ratios[index];
    let proposed_ratio = (current_ratio + delta).clamp(0.1, 0.9);

    // Create proposed ratios
    let mut proposed_ratios = ratios.to_vec();
    proposed_ratios[index] = proposed_ratio;

    // Calculate what the layout would look like with proposed ratios
    let proposed_layout = calculate_layout_full(
        LayoutType::Dwindle,
        layoutable,
        screen_frame,
        0.5,
        &gaps,
        &proposed_ratios,
        MasterPosition::Auto,
    );

    // Check if any window would violate its minimum size
    let mut has_violation = false;

    for (window_id, frame) in &proposed_layout {
        if let Some(window) = state.get_window(*window_id) {
            if let Some((min_w, min_h)) = window.effective_minimum_size() {
                // Check both width and height
                if frame.width < min_w - 1.0 || frame.height < min_h - 1.0 {
                    has_violation = true;
                    break;
                }
            }
        }
    }

    // Only apply the ratio change if no violations
    if !has_violation {
        ratios[index] = proposed_ratio;
    }
}

/// Applies minimum-size-aware resize for Grid layout.
///
/// In Grid, the first ratio typically controls the primary split.
/// This function checks if the resize would violate minimum sizes.
fn apply_grid_resize_with_minimums(
    ratios: &mut [f64],
    index: usize,
    delta: f64,
    layoutable: &[u32],
    state: &TilingState,
    screen_frame: &Rect,
    screen_name: &str,
    is_main_screen: bool,
) {
    use crate::config::get_config;
    use crate::modules::tiling::layout::{Gaps, MasterPosition, calculate_layout_full};

    if index >= ratios.len() {
        return;
    }

    // Get config for gaps
    let config = get_config();
    let bar_offset = if config.bar.is_enabled() && is_main_screen {
        f64::from(config.bar.height) + f64::from(config.bar.padding)
    } else {
        0.0
    };
    let gaps = Gaps::from_config(&config.tiling.gaps, screen_name, is_main_screen, bar_offset);

    // Calculate proposed new ratio
    let current_ratio = ratios[index];
    let proposed_ratio = (current_ratio + delta).clamp(0.1, 0.9);

    // Create proposed ratios
    let mut proposed_ratios = ratios.to_vec();
    proposed_ratios[index] = proposed_ratio;

    // Calculate what the layout would look like with proposed ratios
    let proposed_layout = calculate_layout_full(
        LayoutType::Grid,
        layoutable,
        screen_frame,
        0.5,
        &gaps,
        &proposed_ratios,
        MasterPosition::Auto,
    );

    // Check if any window would violate its minimum size
    let mut has_violation = false;
    for (window_id, frame) in &proposed_layout {
        if let Some(window) = state.get_window(*window_id) {
            if let Some((min_w, min_h)) = window.effective_minimum_size() {
                if frame.width < min_w - 1.0 || frame.height < min_h - 1.0 {
                    has_violation = true;
                    log::debug!(
                        "Grid resize blocked: window {} would violate minimum size \
                         (frame: {:.0}x{:.0}, min: {:.0}x{:.0})",
                        window_id,
                        frame.width,
                        frame.height,
                        min_w,
                        min_h
                    );
                    break;
                }
            }
        }
    }

    // Only apply the ratio change if no violations
    if !has_violation {
        ratios[index] = proposed_ratio;
    }
}

// ============================================================================
// Focused Window Resize
// ============================================================================

/// Resize the focused window in a dimension.
///
/// Adjusts the split ratios to resize the window by the specified amount.
/// Works with layouts that support split ratios (dwindle, grid, split).
///
/// # Arguments
///
/// * `state` - The tiling state
/// * `dimension` - "width" or "height"
/// * `amount` - Pixels to add (positive) or remove (negative)
pub fn on_resize_focused_window(state: &mut TilingState, dimension: &str, amount: i32) {
    let focus = state.get_focus_state();
    let Some(workspace_id) = focus.focused_workspace_id else {
        log::debug!("resize_focused_window: no focused workspace");
        return;
    };

    let Some(workspace) = state.get_workspace(workspace_id) else {
        log::debug!("resize_focused_window: workspace not found");
        return;
    };

    let layout = workspace.layout;
    let window_ids = workspace.window_ids.clone();
    if window_ids.len() < 2 {
        log::debug!("resize_focused_window: need at least 2 windows to resize");
        return;
    }

    let focused_idx = workspace.focused_window_index.unwrap_or(0);
    let Some(&focused_id) = window_ids.get(focused_idx) else {
        log::debug!("resize_focused_window: no window at focused index");
        return;
    };

    let Some(_focused_window) = state.get_window(focused_id) else {
        log::debug!("resize_focused_window: focused window not in state");
        return;
    };

    // Get the screen for workspace to calculate delta ratio
    let Some(screen) = state.get_screen(workspace.screen_id) else {
        log::debug!("resize_focused_window: screen not found");
        return;
    };

    let is_landscape = screen.visible_frame.width >= screen.visible_frame.height;

    // Calculate delta as a ratio of screen dimension
    let delta_ratio = match dimension.to_lowercase().as_str() {
        "width" => f64::from(amount) / screen.visible_frame.width,
        "height" => f64::from(amount) / screen.visible_frame.height,
        _ => {
            log::warn!("resize_focused_window: invalid dimension '{dimension}'");
            return;
        }
    };

    // Get layoutable windows
    let layoutable: Vec<u32> = window_ids
        .iter()
        .filter(|&&id| state.get_window(id).is_some_and(|w| w.is_layoutable()))
        .copied()
        .collect();

    let Some(window_index) = layoutable.iter().position(|&id| id == focused_id) else {
        log::debug!("resize_focused_window: focused window not layoutable");
        return;
    };

    // Determine which ratio index to modify based on layout type
    let (ratio_index, effective_delta) = match layout {
        LayoutType::Dwindle => {
            // Dwindle: each window (except last) controls a split
            // The split direction alternates based on screen orientation and depth
            let split_index = window_index;
            if split_index >= layoutable.len() - 1 {
                // Last window - try to resize previous split
                if window_index > 0 {
                    (window_index - 1, -delta_ratio)
                } else {
                    log::debug!("resize_focused_window: cannot resize single window in dwindle");
                    return;
                }
            } else {
                // Determine if this split is horizontal or vertical
                let is_horizontal_split = if is_landscape {
                    (split_index + 1) % 2 == 1 // odd splits are horizontal in landscape
                } else {
                    (split_index + 1) % 2 == 0 // even splits are horizontal in portrait
                };

                // Apply delta based on dimension matching split direction
                let effective = if (dimension == "width" && is_horizontal_split)
                    || (dimension == "height" && !is_horizontal_split)
                {
                    delta_ratio
                } else {
                    // Dimension doesn't match this split direction
                    // Try to find the appropriate split for this dimension
                    log::debug!(
                        "resize_focused_window: dimension {} doesn't match split direction for window at index {}",
                        dimension,
                        window_index
                    );
                    return;
                };

                (split_index, effective)
            }
        }
        LayoutType::Grid => {
            // Grid: first ratio typically controls the primary split (master width)
            // For width resize: always use index 0
            // For height resize: depends on grid structure (not yet supported)
            if dimension == "width" {
                (0, delta_ratio)
            } else {
                log::debug!(
                    "resize_focused_window: height resize not fully supported for grid yet"
                );
                return;
            }
        }
        LayoutType::Split | LayoutType::SplitHorizontal | LayoutType::SplitVertical => {
            // Split: cumulative ratios, each window (except last) has a ratio
            if window_index >= layoutable.len() - 1 {
                if window_index > 0 {
                    (window_index - 1, -delta_ratio)
                } else {
                    log::debug!("resize_focused_window: cannot resize single window");
                    return;
                }
            } else {
                (window_index, delta_ratio)
            }
        }
        _ => {
            log::debug!(
                "resize_focused_window: layout {:?} doesn't support resize",
                layout
            );
            return;
        }
    };

    on_resize_split(state, workspace_id, ratio_index, effective_delta);

    log::debug!(
        "Resized window {focused_id} {dimension} by {amount}px (layout: {:?}, ratio_index: {}, delta: {:.4})",
        layout,
        ratio_index,
        effective_delta
    );
}

// ============================================================================
// User-Initiated Resize Completion
// ============================================================================

/// Handles completion of a user-initiated resize operation (mouse drag).
///
/// This calculates new split ratios based on how the window was resized by the user
/// and applies them to the workspace.
///
/// # Arguments
///
/// * `state` - The tiling state
/// * `workspace_id` - The workspace where the resize occurred
/// * `window_id` - The window that was resized
/// * `old_frame` - The window's frame before the resize
/// * `new_frame` - The window's frame after the resize
pub fn on_user_resize_completed(
    state: &mut TilingState,
    workspace_id: Uuid,
    window_id: u32,
    old_frame: Rect,
    new_frame: Rect,
) {
    let Some(workspace) = state.get_workspace(workspace_id) else {
        log::warn!("user_resize_completed: workspace {workspace_id} not found");
        return;
    };

    let layout = workspace.layout;
    let window_ids = workspace.window_ids.clone();

    // Get layoutable windows in order
    let layoutable: Vec<u32> = window_ids
        .iter()
        .filter(|&&id| state.get_window(id).is_some_and(|w| w.is_layoutable()))
        .copied()
        .collect();

    let window_count = layoutable.len();

    // Skip layouts that don't support split ratios
    if matches!(
        layout,
        LayoutType::Floating | LayoutType::Monocle | LayoutType::Master
    ) {
        log::debug!(
            "user_resize_completed: layout {:?} doesn't use split ratios",
            layout
        );
        // Just re-apply layout to snap back
        if let Some(handle) = get_subscriber_handle() {
            handle.notify_layout_changed(workspace_id, true);
        }
        return;
    }

    if window_count <= 1 {
        log::debug!("user_resize_completed: need at least 2 windows");
        return;
    }

    // Find the resized window's index in the layoutable list
    let Some(window_index) = layoutable.iter().position(|&id| id == window_id) else {
        log::debug!("user_resize_completed: resized window not in layoutable list");
        if let Some(handle) = get_subscriber_handle() {
            handle.notify_layout_changed(workspace_id, true);
        }
        return;
    };

    // Get screen for dimension calculations
    let Some(screen) = state.get_screen(workspace.screen_id) else {
        log::debug!("user_resize_completed: screen not found");
        return;
    };

    // Calculate the size changes
    let width_delta = new_frame.width - old_frame.width;
    let height_delta = new_frame.height - old_frame.height;

    // Convert pixel delta to ratio delta
    let screen_width = screen.visible_frame.width;
    let screen_height = screen.visible_frame.height;

    // Determine which dimension had the primary change
    let (ratio_delta, resize_dimension) = if width_delta.abs() > height_delta.abs() {
        (width_delta / screen_width, "width")
    } else {
        (height_delta / screen_height, "height")
    };

    // Determine which split ratio to modify based on layout type
    let (ratio_index, effective_delta) = match layout {
        LayoutType::Dwindle => {
            // In Dwindle, each window (except the first two) has its own ratio
            // affecting its share vs the rest
            // Window 0: no ratio (always takes the "main" share)
            // Window 1: ratio[0] controls split between window 0 and rest
            // Window 2: ratio[1] controls split within the "rest"
            // etc.

            // For Dwindle:
            // - If window 0 is resized, adjust ratio[0]
            // - If window N is resized, adjust ratio[N-1] if it exists

            if window_index == 0 && !layoutable.is_empty() {
                // Resizing the main window affects ratio[0]
                (0, ratio_delta)
            } else if window_index > 0 {
                // Resizing secondary window affects its ratio
                // The delta direction depends on the split orientation at that level
                let split_index = window_index.saturating_sub(1);
                (split_index.min(window_count.saturating_sub(2)), ratio_delta)
            } else {
                log::debug!("user_resize_completed: cannot determine ratio for Dwindle resize");
                if let Some(handle) = get_subscriber_handle() {
                    handle.notify_layout_changed(workspace_id, true);
                }
                return;
            }
        }
        LayoutType::Grid => {
            // Grid: first ratio controls primary split
            if resize_dimension == "width" {
                (0, ratio_delta)
            } else {
                // Height changes in grid - not well supported yet
                log::debug!("user_resize_completed: height resize in grid, re-applying layout");
                if let Some(handle) = get_subscriber_handle() {
                    handle.notify_layout_changed(workspace_id, true);
                }
                return;
            }
        }
        LayoutType::Split | LayoutType::SplitHorizontal | LayoutType::SplitVertical => {
            // Split: cumulative ratios
            // Each window (except last) has a ratio marking where it ends
            if window_index >= layoutable.len() - 1 {
                // Last window - adjust the ratio of the previous window
                if window_index > 0 {
                    (window_index - 1, -ratio_delta)
                } else {
                    log::debug!("user_resize_completed: cannot resize single window");
                    return;
                }
            } else {
                (window_index, ratio_delta)
            }
        }
        _ => {
            log::debug!(
                "user_resize_completed: layout {:?} doesn't support user resize",
                layout
            );
            if let Some(handle) = get_subscriber_handle() {
                handle.notify_layout_changed(workspace_id, true);
            }
            return;
        }
    };

    // Apply the resize using the existing resize_split logic
    on_resize_split(state, workspace_id, ratio_index, effective_delta);
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::tiling::state::{Screen, Workspace};

    fn create_test_state() -> (TilingState, Uuid) {
        let mut state = TilingState::new();

        // Add a screen
        let screen = Screen {
            id: 1,
            name: "Test Screen".to_string(),
            is_main: true,
            visible_frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            ..Default::default()
        };
        state.upsert_screen(screen);

        // Add workspace
        let mut ws1 = Workspace::new("workspace1");
        ws1.screen_id = 1;
        ws1.is_visible = true;
        ws1.is_focused = true;
        ws1.layout = LayoutType::Dwindle;
        let ws1_id = ws1.id;
        state.upsert_workspace(ws1);

        // Update focus state
        state.update_focus(|focus| {
            focus.focused_workspace_id = Some(ws1_id);
            focus.focused_screen_id = Some(1);
        });

        (state, ws1_id)
    }

    #[test]
    fn test_initialize_default_ratios_split() {
        let ratios = initialize_default_ratios(LayoutType::Split, 3);
        assert_eq!(ratios.len(), 2);
        assert!((ratios[0] - 0.333).abs() < 0.01);
        assert!((ratios[1] - 0.666).abs() < 0.01);
    }

    #[test]
    fn test_initialize_default_ratios_dwindle() {
        let ratios = initialize_default_ratios(LayoutType::Dwindle, 4);
        assert_eq!(ratios.len(), 3);
        assert!(ratios.iter().all(|&r| (r - 0.5).abs() < 0.001));
    }

    #[test]
    fn test_initialize_default_ratios_grid() {
        let ratios = initialize_default_ratios(LayoutType::Grid, 3);
        assert_eq!(ratios.len(), 1);
        assert!((ratios[0] - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_resize_split_unsupported_layout() {
        let (mut state, ws_id) = create_test_state();

        // Change to floating layout
        state.update_workspace(ws_id, |ws| {
            ws.layout = LayoutType::Floating;
        });

        // Should not panic
        on_resize_split(&mut state, ws_id, 0, 0.1);
    }
}
