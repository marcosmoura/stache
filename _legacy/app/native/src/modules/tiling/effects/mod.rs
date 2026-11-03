//! Effect types and orchestration for the tiling window manager.
//!
//! This module defines the effects that need to be applied to the system
//! when tiling state changes. Effects are computed from state changes,
//! not imperatively triggered.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                     State Actor                                  │
//! │  (TilingState changes via Observable notifications)             │
//! └─────────────────────────┬───────────────────────────────────────┘
//!                           │ Observable subscriptions
//!                           ▼
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                   Effect Subscriber                              │
//! │  - Subscribes to layout changes                                 │
//! │  - Subscribes to focus changes                                  │
//! │  - Computes effects from state deltas                           │
//! └─────────────────────────┬───────────────────────────────────────┘
//!                           │ Vec<TilingEffect>
//!                           ▼
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                   Effect Executor                                │
//! │  - Batches window frame updates                                 │
//! │  - Batches border updates                                       │
//! │  - Emits frontend events                                        │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Effect Types
//!
//! - [`TilingEffect`]: All possible effects (window ops, borders, events)
//! - [`BorderState`]: Visual state of a window's border
//! - [`LayoutChange`]: Describes a change in computed layout positions
//! - [`FocusChange`]: Describes a change in focus state

pub mod animation;
pub mod executor;
pub mod subscriber;
pub mod window_cache;
pub mod window_ops;

pub use animation::{
    AnimationConfig, AnimationSystem, WindowTransition, begin_animation, cancel_animation,
    get_interrupted_position, is_animation_active, is_animation_settling,
    pause_worker as pause_animation_worker, reset_transient_state,
    resume_worker as resume_animation_worker, should_ignore_geometry_events, submit_animation,
};
pub use executor::EffectExecutor;
pub use subscriber::{EffectSubscriber, EffectSubscriberHandle};
use uuid::Uuid;
pub use window_cache::{WindowElementCache, get_cache as get_window_cache};
pub use window_ops::{
    focus_window, get_window_frame, raise_window, set_window_frame, set_window_frame_fast,
};

use crate::modules::tiling::identity::WindowTarget;
use crate::modules::tiling::state::{LayoutType, Rect};

// ============================================================================
// Effect Types
// ============================================================================

/// Effects that need to be applied to the system.
///
/// Effects are the bridge between reactive state changes and actual system
/// operations. They are computed by subscribers and executed by the executor.
#[derive(Debug, Clone, PartialEq)]
pub enum TilingEffect {
    /// Move/resize a window to a target frame.
    SetWindowFrame {
        /// Exact target window to move/resize.
        target: WindowTarget,
        /// Target frame (position and size).
        frame: Rect,
        /// Whether to animate the transition.
        animate: bool,
    },

    /// Show or hide a window.
    SetWindowVisible {
        /// Exact target window to show/hide.
        target: WindowTarget,
        /// Whether the window should be visible.
        visible: bool,
    },

    /// Focus a window.
    FocusWindow {
        /// Exact target window to focus.
        target: WindowTarget,
    },

    /// Raise (bring to front) a window.
    RaiseWindow {
        /// Exact target window to raise.
        target: WindowTarget,
    },

    /// Refresh the active border for a window after its layout/focus state
    /// changed.
    RefreshActiveBorder {
        /// Exact target window to refresh the border for.
        target: WindowTarget,
        /// Layout of the window's workspace.
        layout: LayoutType,
        /// Whether the window is floating.
        is_window_floating: bool,
    },

    /// Hide borders for multiple windows.
    HideBorders {
        /// Exact target windows to hide borders for.
        targets: Vec<WindowTarget>,
    },

    /// Show borders for multiple windows.
    ShowBorders {
        /// Exact target windows to show borders for.
        targets: Vec<WindowTarget>,
    },

    /// Emit an event to the frontend.
    EmitEvent {
        /// Event name (e.g., `stache://tiling/layout-applied`).
        name: String,
        /// Event payload as JSON.
        payload: serde_json::Value,
    },
}

// ============================================================================
// Border State
// ============================================================================

/// Visual state of a window's border for color selection.
///
/// The border color changes based on window state to provide visual feedback
/// to the user about focus and layout mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum BorderState {
    /// Window is currently focused.
    Focused,

    /// Window is visible but not focused.
    #[default]
    Unfocused,

    /// Window is in monocle layout (maximized).
    Monocle,

    /// Window is floating (not tiled).
    Floating,

    /// Border should be hidden.
    Hidden,
}

impl BorderState {
    /// Returns whether this state should show a border.
    #[must_use]
    pub const fn is_visible(&self) -> bool { !matches!(self, Self::Hidden) }

    /// Returns whether this is a focused state (Focused or Monocle).
    #[must_use]
    pub const fn is_focused(&self) -> bool { matches!(self, Self::Focused | Self::Monocle) }
}

// ============================================================================
// Change Descriptors
// ============================================================================

/// Describes a change in computed layout positions.
///
/// This is produced by the layout subscriber when window positions need
/// to change due to workspace, window, or layout changes.
#[derive(Debug, Clone)]
pub struct LayoutChange {
    /// The workspace that changed.
    pub workspace_id: Uuid,

    /// Previous window positions (exact target -> `frame`).
    pub old_positions: Vec<(WindowTarget, Rect)>,

    /// New window positions (exact target -> `frame`).
    pub new_positions: Vec<(WindowTarget, Rect)>,

    /// Whether this change was triggered by user action (should animate).
    pub user_triggered: bool,
}

impl LayoutChange {
    /// Creates a new layout change.
    #[must_use]
    pub const fn new(
        workspace_id: Uuid,
        old_positions: Vec<(WindowTarget, Rect)>,
        new_positions: Vec<(WindowTarget, Rect)>,
        user_triggered: bool,
    ) -> Self {
        Self {
            workspace_id,
            old_positions,
            new_positions,
            user_triggered,
        }
    }

    /// Returns true if this change requires any window movements.
    #[must_use]
    pub fn has_changes(&self) -> bool {
        if self.old_positions.len() != self.new_positions.len() {
            return true;
        }

        // Check if any position actually changed
        for (new_target, new_frame) in &self.new_positions {
            let old_frame = self.old_positions.iter().find(|(t, _)| t == new_target);
            match old_frame {
                Some((_, old)) if old != new_frame => return true,
                None => return true, // New window added
                _ => {}
            }
        }

        false
    }

    /// Returns window IDs that were added (new windows).
    #[must_use]
    pub fn added_windows(&self) -> Vec<u32> {
        let old_targets: std::collections::HashSet<_> =
            self.old_positions.iter().map(|(t, _)| *t).collect();
        self.new_positions
            .iter()
            .filter(|(t, _)| !old_targets.contains(t))
            .map(|(t, _)| t.window_id)
            .collect()
    }

    /// Returns window IDs that were removed.
    #[must_use]
    pub fn removed_windows(&self) -> Vec<u32> {
        let new_targets: std::collections::HashSet<_> =
            self.new_positions.iter().map(|(t, _)| *t).collect();
        self.old_positions
            .iter()
            .filter(|(t, _)| !new_targets.contains(t))
            .map(|(t, _)| t.window_id)
            .collect()
    }
}

/// Describes a change in focus state.
///
/// This is produced by the focus subscriber when focus changes between
/// windows or workspaces.
#[derive(Debug, Clone)]
pub struct FocusChange {
    /// Previously focused window (if any).
    pub old_window: Option<WindowTarget>,

    /// Newly focused window (if any).
    pub new_window: Option<WindowTarget>,

    /// Previously focused workspace (if any).
    pub old_workspace_id: Option<Uuid>,

    /// Newly focused workspace (if any).
    pub new_workspace_id: Option<Uuid>,
}

impl FocusChange {
    /// Creates a new focus change.
    #[must_use]
    pub const fn new(
        old_window: Option<WindowTarget>,
        new_window: Option<WindowTarget>,
        old_workspace_id: Option<Uuid>,
        new_workspace_id: Option<Uuid>,
    ) -> Self {
        Self {
            old_window,
            new_window,
            old_workspace_id,
            new_workspace_id,
        }
    }

    /// Returns true if the focused window changed.
    #[must_use]
    pub fn window_changed(&self) -> bool {
        match (self.old_window, self.new_window) {
            (Some(old), Some(new)) => old != new,
            (None, None) => false,
            _ => true,
        }
    }

    /// Returns true if the focused workspace changed.
    #[must_use]
    pub fn workspace_changed(&self) -> bool {
        match (self.old_workspace_id, self.new_workspace_id) {
            (Some(old), Some(new)) => old != new,
            (None, None) => false,
            _ => true,
        }
    }
}

/// Describes a change in workspace visibility.
#[derive(Debug, Clone)]
pub struct VisibilityChange {
    /// Workspace that became visible.
    pub shown_workspace: Option<Uuid>,

    /// Workspace that became hidden.
    pub hidden_workspace: Option<Uuid>,

    /// Screen ID where the change occurred.
    pub screen_id: u32,
}

impl VisibilityChange {
    /// Creates a new visibility change.
    #[must_use]
    pub const fn new(
        shown_workspace: Option<Uuid>,
        hidden_workspace: Option<Uuid>,
        screen_id: u32,
    ) -> Self {
        Self {
            shown_workspace,
            hidden_workspace,
            screen_id,
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::tiling::identity::{AppIdentity, LaunchDateBits};

    fn t(window_id: u32) -> WindowTarget {
        WindowTarget {
            identity: AppIdentity {
                pid: 42,
                launch_date: LaunchDateBits::from_time_interval_since_reference_date(1.0).unwrap(),
            },
            window_id,
        }
    }

    #[test]
    fn test_border_state_default() {
        assert_eq!(BorderState::default(), BorderState::Unfocused);
    }

    #[test]
    fn test_border_state_is_visible() {
        assert!(BorderState::Focused.is_visible());
        assert!(BorderState::Unfocused.is_visible());
        assert!(BorderState::Monocle.is_visible());
        assert!(BorderState::Floating.is_visible());
        assert!(!BorderState::Hidden.is_visible());
    }

    #[test]
    fn test_border_state_is_focused() {
        assert!(BorderState::Focused.is_focused());
        assert!(BorderState::Monocle.is_focused());
        assert!(!BorderState::Unfocused.is_focused());
        assert!(!BorderState::Floating.is_focused());
        assert!(!BorderState::Hidden.is_focused());
    }

    #[test]
    fn test_layout_change_has_changes_empty() {
        let change = LayoutChange::new(Uuid::now_v7(), vec![], vec![], false);
        assert!(!change.has_changes());
    }

    #[test]
    fn test_layout_change_has_changes_added() {
        let change = LayoutChange::new(
            Uuid::now_v7(),
            vec![],
            vec![(t(1), Rect::new(0.0, 0.0, 100.0, 100.0))],
            false,
        );
        assert!(change.has_changes());
    }

    #[test]
    fn test_layout_change_has_changes_removed() {
        let change = LayoutChange::new(
            Uuid::now_v7(),
            vec![(t(1), Rect::new(0.0, 0.0, 100.0, 100.0))],
            vec![],
            false,
        );
        assert!(change.has_changes());
    }

    #[test]
    fn test_layout_change_has_changes_moved() {
        let change = LayoutChange::new(
            Uuid::now_v7(),
            vec![(t(1), Rect::new(0.0, 0.0, 100.0, 100.0))],
            vec![(t(1), Rect::new(50.0, 50.0, 100.0, 100.0))],
            false,
        );
        assert!(change.has_changes());
    }

    #[test]
    fn test_layout_change_no_changes_same() {
        let frame = Rect::new(0.0, 0.0, 100.0, 100.0);
        let change =
            LayoutChange::new(Uuid::now_v7(), vec![(t(1), frame)], vec![(t(1), frame)], false);
        assert!(!change.has_changes());
    }

    #[test]
    fn test_layout_change_added_windows() {
        let change = LayoutChange::new(
            Uuid::now_v7(),
            vec![(t(1), Rect::new(0.0, 0.0, 100.0, 100.0))],
            vec![
                (t(1), Rect::new(0.0, 0.0, 100.0, 100.0)),
                (t(2), Rect::new(100.0, 0.0, 100.0, 100.0)),
            ],
            false,
        );
        assert_eq!(change.added_windows(), vec![2]);
    }

    #[test]
    fn test_layout_change_removed_windows() {
        let change = LayoutChange::new(
            Uuid::now_v7(),
            vec![
                (t(1), Rect::new(0.0, 0.0, 100.0, 100.0)),
                (t(2), Rect::new(100.0, 0.0, 100.0, 100.0)),
            ],
            vec![(t(1), Rect::new(0.0, 0.0, 200.0, 100.0))],
            false,
        );
        assert_eq!(change.removed_windows(), vec![2]);
    }

    #[test]
    fn test_focus_change_window_changed() {
        let change = FocusChange::new(Some(t(1)), Some(t(2)), None, None);
        assert!(change.window_changed());

        let change = FocusChange::new(None, Some(t(1)), None, None);
        assert!(change.window_changed());

        let change = FocusChange::new(Some(t(1)), None, None, None);
        assert!(change.window_changed());

        let change = FocusChange::new(Some(t(1)), Some(t(1)), None, None);
        assert!(!change.window_changed());

        let change = FocusChange::new(None, None, None, None);
        assert!(!change.window_changed());
    }

    #[test]
    fn test_focus_change_workspace_changed() {
        let ws1 = Uuid::now_v7();
        let ws2 = Uuid::now_v7();

        let change = FocusChange::new(None, None, Some(ws1), Some(ws2));
        assert!(change.workspace_changed());

        let change = FocusChange::new(None, None, Some(ws1), Some(ws1));
        assert!(!change.workspace_changed());
    }
}
