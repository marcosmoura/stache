//! Effect executor for applying effects to the system.
//!
//! The executor takes a batch of effects and applies them efficiently,
//! grouping similar operations and handling errors gracefully.
//!
//! # Architecture
//!
//! The executor receives effects from subscribers and:
//! 1. Groups effects by type (frame updates, border updates, events)
//! 2. Applies frame updates (potentially animated)
//! 3. Batches border updates to `JankyBorders`
//! 4. Emits frontend events via Tauri
//!
//! # Thread Safety
//!
//! The executor can be called from any async context. Window operations
//! are thread-safe for different windows.

use tauri::Emitter;

use super::{
    AnimationSystem, BorderState, TilingEffect, WindowTransition, get_interrupted_position,
    window_ops,
};
use crate::modules::tiling::state::Rect;

// ============================================================================
// Effect Executor
// ============================================================================

/// Executes tiling effects on the system.
///
/// The executor batches effects by type for efficient execution:
/// - Frame updates are applied via AX API (with animations when enabled)
/// - Border updates are batched to `JankyBorders`
/// - Events are emitted to the Tauri frontend
pub struct EffectExecutor {
    /// Optional Tauri app handle for emitting events.
    /// Can be None for testing.
    app_handle: Option<tauri::AppHandle>,

    /// Whether borders are enabled.
    borders_enabled: bool,

    /// Animation system for smooth window transitions.
    animation_system: AnimationSystem,
}

impl Default for EffectExecutor {
    fn default() -> Self { Self::new() }
}

impl EffectExecutor {
    /// Creates a new effect executor without a Tauri handle.
    ///
    /// Events will be logged but not emitted.
    #[must_use]
    pub fn new() -> Self {
        Self {
            app_handle: None,
            borders_enabled: false,
            animation_system: AnimationSystem::from_config(),
        }
    }

    /// Creates a new effect executor with a Tauri handle.
    #[must_use]
    pub fn with_app_handle(app_handle: tauri::AppHandle) -> Self {
        Self {
            app_handle: Some(app_handle),
            borders_enabled: false,
            animation_system: AnimationSystem::from_config(),
        }
    }

    /// Sets whether borders are enabled.
    pub const fn set_borders_enabled(&mut self, enabled: bool) { self.borders_enabled = enabled; }

    /// Executes a batch of effects.
    ///
    /// Effects are grouped by type and executed efficiently:
    /// - Frame updates are applied via AX API
    /// - Border updates are batched (when enabled)
    /// - Events are emitted to frontend
    ///
    /// # Returns
    ///
    /// Number of effects successfully executed.
    #[must_use]
    pub fn execute_batch(&self, effects: Vec<TilingEffect>) -> usize {
        if effects.is_empty() {
            return 0;
        }

        // Group effects by type
        let mut frame_updates: Vec<(u32, Rect, bool)> = Vec::new();
        let mut border_updates: Vec<(u32, BorderState)> = Vec::new();
        let mut events: Vec<(String, serde_json::Value)> = Vec::new();
        let mut focus_ops: Vec<u32> = Vec::new();
        let mut raise_ops: Vec<u32> = Vec::new();
        let mut visibility_ops: Vec<(u32, bool)> = Vec::new();

        for effect in effects {
            match effect {
                TilingEffect::SetWindowFrame { window_id, frame, animate } => {
                    frame_updates.push((window_id, frame, animate));
                }
                TilingEffect::SetWindowVisible { window_id, visible } => {
                    visibility_ops.push((window_id, visible));
                }
                TilingEffect::FocusWindow { window_id } => {
                    focus_ops.push(window_id);
                }
                TilingEffect::RaiseWindow { window_id } => {
                    raise_ops.push(window_id);
                }
                TilingEffect::UpdateBorder { window_id, state } => {
                    border_updates.push((window_id, state));
                }
                TilingEffect::HideBorders { window_ids } => {
                    for id in window_ids {
                        border_updates.push((id, BorderState::Hidden));
                    }
                }
                TilingEffect::ShowBorders { window_ids } => {
                    for id in window_ids {
                        border_updates.push((id, BorderState::Unfocused));
                    }
                }
                TilingEffect::EmitEvent { name, payload } => {
                    events.push((name, payload));
                }
            }
        }

        let mut success_count = 0;

        // Execute frame updates
        success_count += self.execute_frame_updates(&frame_updates);

        // Execute focus operations
        success_count += self.execute_focus_ops(&focus_ops);

        // Execute raise operations
        success_count += self.execute_raise_ops(&raise_ops);

        // Execute visibility operations
        success_count += self.execute_visibility_ops(&visibility_ops);

        // Execute border updates (if enabled)
        if self.borders_enabled {
            success_count += self.execute_border_updates(&border_updates);
        }

        // Emit events
        success_count += self.emit_events(&events);

        success_count
    }

    /// Executes frame update effects.
    fn execute_frame_updates(&self, updates: &[(u32, Rect, bool)]) -> usize {
        if updates.is_empty() {
            return 0;
        }

        let mut success_count = 0;

        // Separate animated and immediate updates
        let (animated, immediate): (Vec<_>, Vec<_>) =
            updates.iter().partition(|(_, _, animate)| *animate);

        // Execute immediate updates first
        for (window_id, frame, _) in &immediate {
            if window_ops::set_window_frame(*window_id, frame) {
                success_count += 1;
            } else {
                log::warn!("Failed to set frame for window {window_id}");
            }
        }

        // Execute animated updates using the animation system
        if !animated.is_empty() {
            // Build transitions by getting current frames
            // If a previous animation was interrupted, use the interrupted position
            // as the starting point for smoother continuation
            let transitions: Vec<WindowTransition> = animated
                .iter()
                .filter_map(|(window_id, target_frame, _)| {
                    // Check for interrupted position first, then fall back to current frame
                    let from_frame = get_interrupted_position(*window_id)
                        .or_else(|| window_ops::get_window_frame(*window_id))?;
                    Some(WindowTransition::new(*window_id, from_frame, *target_frame))
                })
                .collect();

            if !transitions.is_empty() {
                success_count += self.animation_system.animate(transitions);
            }
        }

        success_count
    }

    /// Executes focus operations.
    #[allow(clippy::unused_self)] // Self kept for consistency and future extensibility
    fn execute_focus_ops(&self, window_ids: &[u32]) -> usize {
        let mut success_count = 0;

        for window_id in window_ids {
            if window_ops::focus_window(*window_id) {
                success_count += 1;
            } else {
                log::warn!("Failed to focus window {window_id}");
            }
        }

        success_count
    }

    /// Executes raise operations.
    #[allow(clippy::unused_self)] // Self kept for consistency and future extensibility
    fn execute_raise_ops(&self, window_ids: &[u32]) -> usize {
        let mut success_count = 0;

        for window_id in window_ids {
            if window_ops::raise_window(*window_id) {
                success_count += 1;
            } else {
                log::warn!("Failed to raise window {window_id}");
            }
        }

        success_count
    }

    /// Executes visibility operations.
    #[allow(clippy::unused_self)] // Self kept for consistency and future extensibility
    const fn execute_visibility_ops(&self, _ops: &[(u32, bool)]) -> usize {
        // TODO: Implement window visibility operations
        // This would involve setting window minimized state or similar
        0
    }

    /// Executes border updates.
    ///
    /// Note: Borders are now handled directly in the subscriber via
    /// `borders::on_focus_changed()`. This just counts the effects.
    fn execute_border_updates(&self, updates: &[(u32, BorderState)]) -> usize {
        if updates.is_empty() || !self.borders_enabled {
            return 0;
        }
        // Borders are handled in subscriber, just count as successful
        updates.len()
    }

    /// Emits events to the frontend.
    fn emit_events(&self, events: &[(String, serde_json::Value)]) -> usize {
        if events.is_empty() {
            return 0;
        }

        let Some(app_handle) = &self.app_handle else {
            // No app handle - just log the events
            for (name, _) in events {
                log::debug!("Would emit event: {name}");
            }
            return events.len();
        };

        let mut success_count = 0;

        for (name, payload) in events {
            match app_handle.emit(name, payload.clone()) {
                Ok(()) => {
                    success_count += 1;
                    log::trace!("Emitted event: {name}");
                }
                Err(e) => {
                    log::warn!("Failed to emit event {name}: {e}");
                }
            }
        }

        success_count
    }
}

// ============================================================================
// Convenience Functions
// ============================================================================

/// Computes effects from a layout change.
///
/// This generates `SetWindowFrame` effects for all windows that need
/// to be moved or resized.
///
/// # Arguments
///
/// * `change` - The layout change to process.
///
/// # Returns
///
/// Vector of effects to execute.
#[must_use]
pub fn effects_from_layout_change(change: &super::LayoutChange) -> Vec<TilingEffect> {
    let mut effects = Vec::new();

    // Build old positions map for lookup
    let old_positions: std::collections::HashMap<u32, &Rect> =
        change.old_positions.iter().map(|(id, frame)| (*id, frame)).collect();

    for (window_id, new_frame) in &change.new_positions {
        // For user-triggered changes (like after a drag), always move windows
        // to their calculated positions because the actual window positions
        // might differ from our tracked positions.
        // For programmatic changes, only update if our tracking shows a change.
        let needs_update = change.user_triggered
            || old_positions.get(window_id).is_none_or(|old_frame| *old_frame != new_frame);

        if needs_update {
            // Determine if we should animate:
            // - Animate existing windows (those in old_positions) that are moving
            // - Don't animate new windows (not in old_positions) - they just appear
            // This means when a window is created/destroyed, existing windows
            // animate to their new positions while the new window appears instantly.
            let animate = old_positions.contains_key(window_id);

            effects.push(TilingEffect::SetWindowFrame {
                window_id: *window_id,
                frame: *new_frame,
                animate,
            });
        }
    }

    effects
}

/// Computes effects from a focus change.
///
/// This generates border update effects for the old and new focused windows.
///
/// # Arguments
///
/// * `change` - The focus change to process.
/// * `is_monocle` - Whether the focused workspace is in monocle layout.
/// * `is_floating` - Whether the focused window is floating.
///
/// # Returns
///
/// Vector of effects to execute.
#[must_use]
pub fn effects_from_focus_change(
    change: &super::FocusChange,
    is_monocle: bool,
    is_floating: bool,
) -> Vec<TilingEffect> {
    let mut effects = Vec::new();

    // Update old focused window to unfocused
    if let Some(old_id) = change.old_window_id {
        effects.push(TilingEffect::UpdateBorder {
            window_id: old_id,
            state: BorderState::Unfocused,
        });
    }

    // Update new focused window
    if let Some(new_id) = change.new_window_id {
        let state = if is_monocle {
            BorderState::Monocle
        } else if is_floating {
            BorderState::Floating
        } else {
            BorderState::Focused
        };

        effects.push(TilingEffect::UpdateBorder { window_id: new_id, state });
    }

    effects
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    #[test]
    fn test_executor_default() {
        let executor = EffectExecutor::default();
        assert!(executor.app_handle.is_none());
        assert!(!executor.borders_enabled);
    }

    #[test]
    fn test_executor_empty_batch() {
        let executor = EffectExecutor::new();
        let count = executor.execute_batch(vec![]);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_effects_from_layout_change_no_changes() {
        let frame = Rect::new(0.0, 0.0, 100.0, 100.0);
        let change = super::super::LayoutChange::new(
            Uuid::now_v7(),
            vec![(1, frame)],
            vec![(1, frame)],
            false,
        );

        let effects = effects_from_layout_change(&change);
        assert!(effects.is_empty());
    }

    #[test]
    fn test_effects_from_layout_change_moved() {
        let old_frame = Rect::new(0.0, 0.0, 100.0, 100.0);
        let new_frame = Rect::new(50.0, 50.0, 100.0, 100.0);
        let change = super::super::LayoutChange::new(
            Uuid::now_v7(),
            vec![(1, old_frame)],
            vec![(1, new_frame)],
            false,
        );

        let effects = effects_from_layout_change(&change);
        assert_eq!(effects.len(), 1);

        match &effects[0] {
            TilingEffect::SetWindowFrame { window_id, frame, animate } => {
                assert_eq!(*window_id, 1);
                assert_eq!(*frame, new_frame);
                // Window is in old_positions, so it should animate even when not user-triggered
                assert!(animate);
            }
            _ => panic!("Expected SetWindowFrame effect"),
        }
    }

    #[test]
    fn test_effects_from_layout_change_user_triggered() {
        let old_frame = Rect::new(0.0, 0.0, 100.0, 100.0);
        let new_frame = Rect::new(50.0, 50.0, 100.0, 100.0);
        let change = super::super::LayoutChange::new(
            Uuid::now_v7(),
            vec![(1, old_frame)],
            vec![(1, new_frame)],
            true, // User triggered
        );

        let effects = effects_from_layout_change(&change);
        assert_eq!(effects.len(), 1);

        match &effects[0] {
            TilingEffect::SetWindowFrame { animate, .. } => {
                assert!(animate); // Should animate for user-triggered changes
            }
            _ => panic!("Expected SetWindowFrame effect"),
        }
    }

    #[test]
    fn test_effects_from_layout_change_new_window() {
        let change = super::super::LayoutChange::new(
            Uuid::now_v7(),
            vec![],
            vec![(1, Rect::new(0.0, 0.0, 100.0, 100.0))],
            true, // Even if user triggered
        );

        let effects = effects_from_layout_change(&change);
        assert_eq!(effects.len(), 1);

        match &effects[0] {
            TilingEffect::SetWindowFrame { animate, .. } => {
                assert!(!animate); // New windows don't animate
            }
            _ => panic!("Expected SetWindowFrame effect"),
        }
    }

    #[test]
    fn test_effects_from_focus_change() {
        let change = super::super::FocusChange::new(Some(1), Some(2), None, None);

        let effects = effects_from_focus_change(&change, false, false);
        assert_eq!(effects.len(), 2);

        // Old window should be unfocused
        match &effects[0] {
            TilingEffect::UpdateBorder { window_id, state } => {
                assert_eq!(*window_id, 1);
                assert_eq!(*state, BorderState::Unfocused);
            }
            _ => panic!("Expected UpdateBorder effect"),
        }

        // New window should be focused
        match &effects[1] {
            TilingEffect::UpdateBorder { window_id, state } => {
                assert_eq!(*window_id, 2);
                assert_eq!(*state, BorderState::Focused);
            }
            _ => panic!("Expected UpdateBorder effect"),
        }
    }

    #[test]
    fn test_effects_from_focus_change_monocle() {
        let change = super::super::FocusChange::new(None, Some(1), None, None);

        let effects = effects_from_focus_change(&change, true, false);
        assert_eq!(effects.len(), 1);

        match &effects[0] {
            TilingEffect::UpdateBorder { state, .. } => {
                assert_eq!(*state, BorderState::Monocle);
            }
            _ => panic!("Expected UpdateBorder effect"),
        }
    }

    #[test]
    fn test_effects_from_focus_change_floating() {
        let change = super::super::FocusChange::new(None, Some(1), None, None);

        let effects = effects_from_focus_change(&change, false, true);
        assert_eq!(effects.len(), 1);

        match &effects[0] {
            TilingEffect::UpdateBorder { state, .. } => {
                assert_eq!(*state, BorderState::Floating);
            }
            _ => panic!("Expected UpdateBorder effect"),
        }
    }
}
