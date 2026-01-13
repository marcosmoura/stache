//! Drag and resize state tracking for the tiling window manager.
//!
//! This module tracks the state of user-initiated window move and resize
//! operations. It works in conjunction with the mouse monitor to detect
//! when a drag/resize starts and ends.
//!
//! # Flow
//!
//! 1. User starts dragging a window (mouse down + `AXWindowMoved`)
//! 2. This module records the workspace and all window frames
//! 3. `AXWindowMoved`/`AXWindowResized` events are ignored during the drag
//! 4. User releases mouse (mouse up via mouse monitor callback)
//! 5. `finish_operation()` is called:
//!    - For moves: reapply layout (tiled windows snap back, floating stay)
//!    - For resizes: find which window changed, calculate new ratios
//!
//! # Memory Ordering
//!
//! This module uses `Acquire`/`Release` ordering for atomic operations:
//!
//! - **`Release`** on store: Ensures all writes to `CURRENT_OPERATION` (via the mutex)
//!   happen-before the atomic flag is set. Readers who see the flag will also see
//!   the complete operation state.
//!
//! - **`Acquire`** on load: Ensures the reader sees all writes that happened-before
//!   the corresponding `Release` store. This guarantees that if `is_operation_in_progress()`
//!   returns `true`, the operation details are fully visible.
//!
//! This is weaker than `SeqCst` but sufficient because:
//! 1. The `Mutex` on `CURRENT_OPERATION` provides the main synchronization
//! 2. The atomics provide a fast path check before acquiring the mutex
//! 3. There's a single writer (event thread) and multiple readers (query threads)

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::state::Rect;

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
    /// The workspace name.
    pub workspace_name: String,
    /// Snapshots of all windows in the workspace before the drag.
    pub window_snapshots: Vec<WindowSnapshot>,
    /// The mouse drag sequence when operation started (for detecting stale state).
    pub drag_sequence: u32,
}

impl DragInfo {
    /// Returns true if any window in the workspace is tiled (not floating).
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
///
/// Uses `Acquire` ordering to ensure visibility of the operation state
/// written before the corresponding `Release` store.
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
///
/// # Arguments
///
/// * `operation` - The type of operation (move or resize)
/// * `pid` - The process ID that triggered the event
/// * `workspace_name` - The workspace being affected
/// * `window_snapshots` - Snapshots of all windows in the workspace
/// * `drag_sequence` - The mouse drag sequence number
pub fn start_operation(
    operation: DragOperation,
    pid: i32,
    workspace_name: &str,
    window_snapshots: Vec<WindowSnapshot>,
    drag_sequence: u32,
) {
    if let Ok(mut guard) = CURRENT_OPERATION.lock() {
        let info = DragInfo {
            operation,
            pid,
            workspace_name: workspace_name.to_string(),
            window_snapshots,
            drag_sequence,
        };

        *guard = Some(info);
        // Store sequence first, then set in-progress flag.
        // Release ordering ensures the mutex writes are visible to readers
        // who observe the flag as true via Acquire load.
        OPERATION_DRAG_SEQUENCE.store(drag_sequence, Ordering::Release);
        OPERATION_IN_PROGRESS.store(true, Ordering::Release);
    }
}

/// Clears the current operation without triggering any action.
///
/// Use this when the operation should be abandoned (e.g., window was destroyed).
pub fn cancel_operation() {
    if let Ok(mut guard) = CURRENT_OPERATION.lock() {
        *guard = None;
    }
    // Release ordering ensures the mutex write (clearing the operation)
    // happens-before readers see the flag as false.
    OPERATION_IN_PROGRESS.store(false, Ordering::Release);
}

/// Finishes the current operation and returns the info for processing.
///
/// This should be called when the mouse button is released. Returns the
/// operation info so the caller can take appropriate action (reapply layout
/// or calculate new ratios).
///
/// # Returns
///
/// The completed operation info, or `None` if no operation was in progress.
pub fn finish_operation() -> Option<DragInfo> {
    let info = CURRENT_OPERATION.lock().ok().and_then(|mut guard| guard.take());
    // Release ordering ensures the mutex write (taking the operation)
    // happens-before readers see the flag as false.
    OPERATION_IN_PROGRESS.store(false, Ordering::Release);
    info
}

/// Gets the drag sequence when the current operation started.
///
/// Used to detect if the operation is stale (a new drag started before
/// we processed the end of the previous one).
///
/// Uses `Acquire` ordering to pair with the `Release` store in `start_operation()`.
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

        start_operation(DragOperation::Move, 456, "workspace-1", snapshots, 1);

        assert!(is_operation_in_progress());

        let info = finish_operation();
        assert!(info.is_some());

        let info = info.unwrap();
        assert_eq!(info.operation, DragOperation::Move);
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

        start_operation(DragOperation::Resize, 111, "test", snapshots, 2);

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
            workspace_name: "test".to_string(),
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

        let info_all_floating = DragInfo {
            operation: DragOperation::Move,
            pid: 1,
            workspace_name: "test".to_string(),
            window_snapshots: vec![WindowSnapshot {
                window_id: 1,
                original_frame: Rect::default(),
                is_floating: true,
            }],
            drag_sequence: 1,
        };
        assert!(!info_all_floating.has_tiled_windows());
    }

    #[test]
    fn test_operation_types() {
        assert_ne!(DragOperation::Move, DragOperation::Resize);
    }
}
