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

use super::animation::OutcomeReporter;
use super::{
    BorderState, TilingEffect, WindowTransition, get_interrupted_position, submit_animation,
    window_cache, window_ops,
};
use crate::modules::tiling::identity::WindowTarget;
use crate::modules::tiling::state::{LayoutType, Rect};

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

    /// Receives deferred animation outcomes for layout reconciliation.
    animation_outcome_reporter: Option<OutcomeReporter>,
}

impl Default for EffectExecutor {
    fn default() -> Self { Self::new() }
}

impl EffectExecutor {
    /// Creates a new effect executor without a Tauri handle.
    ///
    /// Events will be logged but not emitted.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            app_handle: None,
            borders_enabled: false,
            animation_outcome_reporter: None,
        }
    }

    /// Creates a new effect executor with a Tauri handle.
    #[must_use]
    pub const fn with_app_handle(app_handle: tauri::AppHandle) -> Self {
        Self {
            app_handle: Some(app_handle),
            borders_enabled: false,
            animation_outcome_reporter: None,
        }
    }

    /// Sets whether borders are enabled.
    pub const fn set_borders_enabled(&mut self, enabled: bool) { self.borders_enabled = enabled; }

    /// Sets the receiver for deferred animation outcomes.
    pub fn set_animation_outcome_reporter(&mut self, reporter: OutcomeReporter) {
        self.animation_outcome_reporter = Some(reporter);
    }

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
        self.execute_batch_with_applied_frames(effects, None).0
    }

    /// Executes a batch and returns exact targets whose frame writes completed
    /// immediately. Animated targets are deferred to the worker and are not
    /// reported as applied merely because they were accepted for execution.
    #[must_use]
    pub fn execute_batch_with_applied_frames(
        &self,
        effects: Vec<TilingEffect>,
        layout_revision: Option<(uuid::Uuid, u64)>,
    ) -> (usize, Vec<WindowTarget>) {
        if effects.is_empty() {
            return (0, Vec::new());
        }

        // Group effects by type
        let mut frame_updates: Vec<(WindowTarget, Rect, bool)> = Vec::new();
        let mut border_updates: Vec<(WindowTarget, BorderState)> = Vec::new();
        let mut events: Vec<(String, serde_json::Value)> = Vec::new();
        let mut focus_ops: Vec<WindowTarget> = Vec::new();
        let mut raise_ops: Vec<WindowTarget> = Vec::new();
        let mut visibility_ops: Vec<(WindowTarget, bool)> = Vec::new();
        let mut active_borders: Vec<(WindowTarget, LayoutType, bool)> = Vec::new();

        for effect in effects {
            match effect {
                TilingEffect::SetWindowFrame { target, frame, animate } => {
                    frame_updates.push((target, frame, animate));
                }
                TilingEffect::SetWindowVisible { target, visible } => {
                    visibility_ops.push((target, visible));
                }
                TilingEffect::FocusWindow { target } => {
                    focus_ops.push(target);
                }
                TilingEffect::RaiseWindow { target } => {
                    raise_ops.push(target);
                }
                TilingEffect::RefreshActiveBorder {
                    target,
                    layout,
                    is_window_floating,
                } => {
                    active_borders.push((target, layout, is_window_floating));
                }
                TilingEffect::HideBorders { targets } => {
                    for target in targets {
                        border_updates.push((target, BorderState::Hidden));
                    }
                }
                TilingEffect::ShowBorders { targets } => {
                    for target in targets {
                        border_updates.push((target, BorderState::Unfocused));
                    }
                }
                TilingEffect::EmitEvent { name, payload } => {
                    events.push((name, payload));
                }
            }
        }

        let mut success_count = 0;

        // Execute frame updates
        let (frame_successes, applied_frames) =
            self.execute_frame_updates(&frame_updates, layout_revision);
        success_count += frame_successes;

        // Execute focus operations
        success_count += self.execute_focus_ops(&focus_ops);

        // Execute raise operations
        success_count += self.execute_raise_ops(&raise_ops);

        // Execute visibility operations
        success_count += self.execute_visibility_ops(&visibility_ops);

        // Execute border updates (if enabled)
        if self.borders_enabled {
            success_count += self.execute_border_updates(&border_updates);
            success_count += self.execute_active_borders(&active_borders);
        }

        // Emit events
        success_count += self.emit_events(&events);

        (success_count, applied_frames)
    }

    /// Executes frame update effects.
    ///
    /// Uses the window element cache for efficient batch resolution,
    /// avoiding repeated O(n*m) lookups during animation setup.
    #[allow(clippy::unused_self)] // Kept for executor API symmetry.
    fn execute_frame_updates(
        &self,
        updates: &[(WindowTarget, Rect, bool)],
        layout_revision: Option<(uuid::Uuid, u64)>,
    ) -> (usize, Vec<WindowTarget>) {
        if updates.is_empty() {
            return (0, Vec::new());
        }

        let mut success_count = 0;
        let mut applied_frames = Vec::new();

        // Separate animated and immediate updates
        let (animated, immediate): (Vec<_>, Vec<_>) =
            updates.iter().partition(|(_, _, animate)| *animate);

        // Execute immediate updates first using the cache
        if !immediate.is_empty() {
            let cache = window_cache::get_cache();
            for (target, frame, _) in &immediate {
                if cache.set_window_frame_fast(*target, frame) {
                    success_count += 1;
                    applied_frames.push(*target);
                } else {
                    tracing::warn!("Failed to set frame for window {}", target.window_id);
                }
            }
        }

        // Execute animated updates using the animation system
        if !animated.is_empty() {
            let attempted: Vec<_> =
                animated.iter().map(|(target, frame, _)| (*target, *frame)).collect();
            // Use the cache for efficient batch frame retrieval
            let cache = window_cache::get_cache();

            // Build transitions by getting current frames
            // If a previous animation was interrupted, use the interrupted position
            // as the starting point for smoother continuation
            let transitions: Vec<WindowTransition> = animated
                .iter()
                .filter_map(|(target, target_frame, _)| {
                    // Check for interrupted position first, then fall back to cached frame
                    let from_frame = get_interrupted_position(*target)
                        .or_else(|| cache.get_window_frame_fast(*target))?;
                    Some(WindowTransition::new(*target, from_frame, *target_frame))
                })
                .collect();

            let count = transitions.len();
            submit_animation(
                transitions,
                attempted,
                self.animation_outcome_reporter.clone(),
                layout_revision,
            );
            if count > 0 {
                success_count += count;
            }
        }

        (success_count, applied_frames)
    }

    /// Executes focus operations.
    #[allow(clippy::unused_self)] // Self kept for consistency and future extensibility
    fn execute_focus_ops(&self, targets: &[WindowTarget]) -> usize {
        let mut success_count = 0;

        for target in targets {
            if window_ops::focus_window(*target) {
                success_count += 1;
            } else {
                tracing::warn!("Failed to focus window {}", target.window_id);
            }
        }

        success_count
    }

    /// Executes raise operations.
    #[allow(clippy::unused_self)] // Self kept for consistency and future extensibility
    fn execute_raise_ops(&self, targets: &[WindowTarget]) -> usize {
        let mut success_count = 0;

        for target in targets {
            if window_ops::raise_window(*target) {
                success_count += 1;
            } else {
                tracing::warn!("Failed to raise window {}", target.window_id);
            }
        }

        success_count
    }

    /// Executes visibility operations.
    #[allow(clippy::unused_self)] // Self kept for consistency and future extensibility
    const fn execute_visibility_ops(&self, _ops: &[(WindowTarget, bool)]) -> usize {
        // TODO: Implement window visibility operations
        // This would involve setting window minimized state or similar
        0
    }

    /// Executes border updates.
    ///
    /// Note: Borders are now handled directly in the subscriber via
    /// `borders::on_focus_changed()`. This just counts the effects.
    const fn execute_border_updates(&self, updates: &[(WindowTarget, BorderState)]) -> usize {
        if updates.is_empty() || !self.borders_enabled {
            return 0;
        }
        // Borders are handled in subscriber, just count as successful
        updates.len()
    }

    /// Executes active-border refreshes: validates the exact target, then
    /// invokes the border helper for the given layout state.
    #[allow(clippy::unused_self)] // Self kept for consistency and future config access
    fn execute_active_borders(&self, refreshes: &[(WindowTarget, LayoutType, bool)]) -> usize {
        let mut success_count = 0;
        let cache = window_cache::get_cache();

        for (target, layout, is_window_floating) in refreshes {
            // Validate the exact target before touching borders: if the window
            // is invalid (destroyed or PID-reused), skip the refresh.
            if !cache.find_invalid_windows(&[*target]).is_empty() {
                tracing::trace!("tiling: skipping border refresh for stale target {:?}", target);
                continue;
            }
            crate::modules::tiling::borders::on_focus_changed(*layout, *is_window_floating);
            success_count += 1;
        }

        success_count
    }

    /// Emits events to the frontend.
    fn emit_events(&self, events: &[(String, serde_json::Value)]) -> usize {
        if events.is_empty() {
            return 0;
        }

        let Some(app_handle) = &self.app_handle else {
            // No app handle - just log the events
            for (name, _) in events {
                tracing::debug!("Would emit event: {name}");
            }
            return events.len();
        };

        let mut success_count = 0;

        for (name, payload) in events {
            match app_handle.emit(name, payload.clone()) {
                Ok(()) => {
                    success_count += 1;
                    tracing::trace!("Emitted event: {name}");
                }
                Err(e) => {
                    tracing::warn!("Failed to emit event {name}: {e}");
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
    let old_positions: std::collections::HashMap<WindowTarget, &Rect> =
        change.old_positions.iter().map(|(target, frame)| (*target, frame)).collect();

    for (target, new_frame) in &change.new_positions {
        // For user-triggered changes (like after a drag), always move windows
        // to their calculated positions because the actual window positions
        // might differ from our tracked positions.
        // For programmatic changes, only update if our tracking shows a change.
        let needs_update = change.user_triggered
            || old_positions.get(target).is_none_or(|old_frame| *old_frame != new_frame);

        if needs_update {
            // Determine if we should animate:
            // - Animate existing windows (those in old_positions) that are moving
            // - Don't animate new windows (not in old_positions) - they just appear
            // This means when a window is created/destroyed, existing windows
            // animate to their new positions while the new window appears instantly.
            let animate = old_positions.contains_key(target);

            effects.push(TilingEffect::SetWindowFrame {
                target: *target,
                frame: *new_frame,
                animate,
            });
        }
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
            vec![(t(1), frame)],
            vec![(t(1), frame)],
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
            vec![(t(1), old_frame)],
            vec![(t(1), new_frame)],
            false,
        );

        let effects = effects_from_layout_change(&change);
        assert_eq!(effects.len(), 1);

        match &effects[0] {
            TilingEffect::SetWindowFrame { target, frame, animate } => {
                assert_eq!(target.window_id, 1);
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
            vec![(t(1), old_frame)],
            vec![(t(1), new_frame)],
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
            vec![(t(1), Rect::new(0.0, 0.0, 100.0, 100.0))],
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
}
