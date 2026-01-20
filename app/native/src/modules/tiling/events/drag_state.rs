//! Drag and resize state tracking for tiling v2.
//!
//! This module tracks user-initiated window move and resize operations.
//! It works with the mouse monitor to detect when a drag/resize starts and ends.
//!
//! # Flow
//!
//! 1. User starts resizing a window (mouse down + resize event)
//! 2. This module records the workspace and all window frames
//! 3. Layout changes are frozen during the resize
//! 4. User releases mouse (mouse up via mouse monitor callback)
//! 5. `finish_operation()` is called:
//!    - Calculate new split ratios based on final window positions
//!    - Apply the updated layout

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use uuid::Uuid;

use crate::modules::tiling::state::Rect;

// ============================================================================
// Operation Types
// ============================================================================

/// The type of drag operation in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragOperation {
    /// Window is being moved.
    Move,
    /// Window is being resized.
    Resize,
}

/// Information about a window's state before the drag started.
#[derive(Debug, Clone)]
pub struct WindowSnapshot {
    /// The window ID.
    pub window_id: u32,
    /// The frame before the drag started.
    pub original_frame: Rect,
    /// Whether the window is floating.
    pub is_floating: bool,
}

/// Complete state of an ongoing drag operation.
#[derive(Debug, Clone)]
pub struct DragInfo {
    /// The type of operation.
    pub operation: DragOperation,
    /// The process ID that triggered the event.
    pub pid: i32,
    /// The workspace ID.
    pub workspace_id: Uuid,
    /// The workspace name.
    pub workspace_name: String,
    /// Snapshots of all windows in the workspace before the drag.
    pub window_snapshots: Vec<WindowSnapshot>,
    /// The mouse drag sequence when operation started.
    pub drag_sequence: u32,
    /// The screen ID where the operation is happening.
    pub screen_id: u32,
}

impl DragInfo {
    /// Returns true if any window in the workspace is tiled (not floating).
    #[must_use]
    pub fn has_tiled_windows(&self) -> bool { self.window_snapshots.iter().any(|w| !w.is_floating) }
}

// ============================================================================
// Global State
// ============================================================================

/// Whether an operation is currently in progress.
static OPERATION_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// The drag sequence when the current operation started.
static OPERATION_DRAG_SEQUENCE: AtomicU32 = AtomicU32::new(0);

/// Details about the current operation.
static CURRENT_OPERATION: Mutex<Option<DragInfo>> = Mutex::new(None);

// ============================================================================
// Public API
// ============================================================================

/// Returns whether a drag/resize operation is currently in progress.
#[must_use]
pub fn is_operation_in_progress() -> bool { OPERATION_IN_PROGRESS.load(Ordering::Acquire) }

/// Gets the current operation info, if any.
#[must_use]
pub fn get_operation() -> Option<DragInfo> {
    CURRENT_OPERATION.lock().ok().and_then(|guard| guard.clone())
}

/// Starts tracking a drag/resize operation.
///
/// Call this when we detect that a window is being moved or resized
/// while the mouse button is down.
pub fn start_operation(
    operation: DragOperation,
    pid: i32,
    workspace_id: Uuid,
    workspace_name: &str,
    screen_id: u32,
    window_snapshots: Vec<WindowSnapshot>,
    drag_sequence: u32,
) {
    if let Ok(mut guard) = CURRENT_OPERATION.lock() {
        let info = DragInfo {
            operation,
            pid,
            workspace_id,
            workspace_name: workspace_name.to_string(),
            screen_id,
            window_snapshots,
            drag_sequence,
        };

        *guard = Some(info);
        OPERATION_DRAG_SEQUENCE.store(drag_sequence, Ordering::Release);
        OPERATION_IN_PROGRESS.store(true, Ordering::Release);
    }
}

/// Clears the current operation without triggering any action.
pub fn cancel_operation() {
    if let Ok(mut guard) = CURRENT_OPERATION.lock() {
        *guard = None;
    }
    OPERATION_IN_PROGRESS.store(false, Ordering::Release);
}

/// Finishes the current operation and returns the info for processing.
///
/// This should be called when the mouse button is released.
pub fn finish_operation() -> Option<DragInfo> {
    let info = CURRENT_OPERATION.lock().ok().and_then(|mut guard| guard.take());
    OPERATION_IN_PROGRESS.store(false, Ordering::Release);
    info
}

/// Gets the drag sequence when the current operation started.
#[must_use]
pub fn operation_drag_sequence() -> u32 { OPERATION_DRAG_SEQUENCE.load(Ordering::Acquire) }

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn reset_state() { cancel_operation(); }

    #[test]
    fn test_no_operation_initially() {
        reset_state();
        assert!(!is_operation_in_progress());
        assert!(get_operation().is_none());
    }

    #[test]
    fn test_start_and_finish_operation() {
        reset_state();

        let frame = Rect::new(100.0, 100.0, 800.0, 600.0);
        let snapshots = vec![WindowSnapshot {
            window_id: 123,
            original_frame: frame,
            is_floating: false,
        }];

        start_operation(
            DragOperation::Resize,
            456,
            Uuid::nil(),
            "workspace-1",
            1,
            snapshots,
            1,
        );

        assert!(is_operation_in_progress());

        let info = finish_operation();
        assert!(info.is_some());

        let info = info.unwrap();
        assert_eq!(info.operation, DragOperation::Resize);
        assert_eq!(info.workspace_name, "workspace-1");
        assert_eq!(info.window_snapshots.len(), 1);
        assert!(!info.window_snapshots[0].is_floating);

        assert!(!is_operation_in_progress());
        reset_state();
    }

    #[test]
    fn test_cancel_operation() {
        reset_state();

        let frame = Rect::new(0.0, 0.0, 100.0, 100.0);
        let snapshots = vec![WindowSnapshot {
            window_id: 789,
            original_frame: frame,
            is_floating: true,
        }];

        start_operation(DragOperation::Move, 111, Uuid::nil(), "test", 1, snapshots, 2);

        assert!(is_operation_in_progress());

        cancel_operation();

        assert!(!is_operation_in_progress());
        assert!(finish_operation().is_none());
        reset_state();
    }

    #[test]
    fn test_has_tiled_windows() {
        let info = DragInfo {
            operation: DragOperation::Resize,
            pid: 1,
            workspace_id: Uuid::nil(),
            workspace_name: "test".to_string(),
            screen_id: 1,
            window_snapshots: vec![
                WindowSnapshot {
                    window_id: 1,
                    original_frame: Rect::default(),
                    is_floating: true,
                },
                WindowSnapshot {
                    window_id: 2,
                    original_frame: Rect::default(),
                    is_floating: false,
                },
            ],
            drag_sequence: 1,
        };
        assert!(info.has_tiled_windows());
    }
}
