//! Animation state management.
//!
//! Tracks animation lifecycle, cancellation, interrupted positions,
//! and the settling period after animations complete.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use crate::modules::tiling::state::Rect;

// ============================================================================
// Constants
// ============================================================================

/// Grace period after animation ends during which geometry events are ignored.
/// This accounts for the batch timer delay in EventProcessor (~16ms at 60Hz)
/// plus some margin for event propagation.
pub const ANIMATION_SETTLE_DURATION_MS: u64 = 50;

// ============================================================================
// Animation Cancellation
// ============================================================================

/// Counter of commands waiting for the lock to start an animation.
static WAITING_COMMANDS: AtomicU64 = AtomicU64::new(0);

/// Whether animation is currently active.
static ANIMATION_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Timestamp when the last animation ended.
/// Used to implement a settling period where geometry events are still ignored
/// to account for queued/batched events that were generated during animation.
static ANIMATION_END_TIME: OnceLock<RwLock<Option<Instant>>> = OnceLock::new();

/// Gets the animation end time storage.
fn get_animation_end_time() -> &'static RwLock<Option<Instant>> {
    ANIMATION_END_TIME.get_or_init(|| RwLock::new(None))
}

/// Records when an animation ends.
fn record_animation_end() {
    if let Ok(mut guard) = get_animation_end_time().write() {
        *guard = Some(Instant::now());
    }
}

/// Clears the animation end time (called when new animation starts).
pub fn clear_animation_end_time() {
    if let Ok(mut guard) = get_animation_end_time().write() {
        *guard = None;
    }
}

/// Stores the last rendered position for each window when animation is cancelled.
static INTERRUPTED_POSITIONS: OnceLock<Mutex<HashMap<u32, Rect>>> = OnceLock::new();

/// Gets the interrupted positions map, initializing if needed.
fn get_interrupted_positions() -> &'static Mutex<HashMap<u32, Rect>> {
    INTERRUPTED_POSITIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Signals that a command is waiting to run an animation.
///
/// Call this BEFORE trying to acquire any locks. This increments the waiting
/// counter, which the running animation will detect and cancel early.
pub fn cancel_animation() { WAITING_COMMANDS.fetch_add(1, Ordering::Relaxed); }

/// Called after acquiring the lock to signal we're no longer waiting.
///
/// IMPORTANT: This MUST be called after every `cancel_animation()` call.
pub fn begin_animation() {
    let _ = WAITING_COMMANDS.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_sub(1))
    });
}

/// Checks if other commands are waiting to run.
#[inline]
pub fn should_cancel() -> bool { WAITING_COMMANDS.load(Ordering::Relaxed) > 0 }

/// Returns whether an animation is currently active.
#[must_use]
pub fn is_animation_active() -> bool { ANIMATION_ACTIVE.load(Ordering::Relaxed) }

/// Returns whether animation is in the settling period.
///
/// After an animation finishes, there's a brief period where geometry events
/// from the animation may still be queued/batched. This function returns true
/// during that period so handlers can ignore stale events.
#[must_use]
pub fn is_animation_settling() -> bool {
    if let Ok(guard) = get_animation_end_time().read() {
        if let Some(end_time) = *guard {
            return end_time.elapsed() < Duration::from_millis(ANIMATION_SETTLE_DURATION_MS);
        }
    }
    false
}

/// Returns whether geometry events should be ignored.
///
/// This is true when:
/// - An animation is currently running, OR
/// - An animation recently finished (settling period)
///
/// Use this in geometry handlers to avoid processing stale events.
#[must_use]
pub fn should_ignore_geometry_events() -> bool { is_animation_active() || is_animation_settling() }

/// Sets the animation active state.
pub fn set_animation_active(active: bool) {
    if active {
        // Starting animation - clear any previous end time
        clear_animation_end_time();
    } else {
        // Ending animation - record the end time for settling period
        record_animation_end();
    }
    ANIMATION_ACTIVE.store(active, Ordering::Relaxed);
}

/// Gets the interrupted position for a window, if any.
#[must_use]
pub fn get_interrupted_position(window_id: u32) -> Option<Rect> {
    get_interrupted_positions()
        .lock()
        .ok()
        .and_then(|map| map.get(&window_id).copied())
}

/// Stores interrupted positions for the given windows.
///
/// Called when animation is cancelled to record where windows are.
#[allow(dead_code)] // Will be used when animation cancellation stores positions
pub fn store_interrupted_positions(positions: &[(u32, Rect)]) {
    if let Ok(mut map) = get_interrupted_positions().lock() {
        for (window_id, rect) in positions {
            map.insert(*window_id, *rect);
        }
    }
}

/// Clears interrupted positions for the given windows.
pub fn clear_interrupted_positions(window_ids: &[u32]) {
    if let Ok(mut map) = get_interrupted_positions().lock() {
        for window_id in window_ids {
            map.remove(window_id);
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cancel_begin_animation() {
        cancel_animation();
        assert!(should_cancel());

        begin_animation();
        assert!(!should_cancel());
    }

    #[test]
    fn test_animation_active_state() {
        assert!(!is_animation_active());
        set_animation_active(true);
        assert!(is_animation_active());
        set_animation_active(false);
        assert!(!is_animation_active());
    }

    #[test]
    fn test_interrupted_positions() {
        let rect = Rect::new(10.0, 20.0, 100.0, 200.0);
        store_interrupted_positions(&[(123, rect)]);

        let retrieved = get_interrupted_position(123);
        assert_eq!(retrieved, Some(rect));

        clear_interrupted_positions(&[123]);
        let cleared = get_interrupted_position(123);
        assert_eq!(cleared, None);
    }

    #[test]
    fn test_animation_settling_lifecycle() {
        // This test validates the entire settling lifecycle in order:
        // 1. Clean state (not settling)
        // 2. Animation active (not settling, but should ignore events)
        // 3. Animation ends (settling period active, should ignore events)
        // 4. Settling expires (no longer settling, should not ignore events)

        // Step 1: Start from clean state
        ANIMATION_ACTIVE.store(false, Ordering::Relaxed);
        clear_animation_end_time();
        assert!(!is_animation_active());
        assert!(!is_animation_settling());
        assert!(!should_ignore_geometry_events());

        // Step 2: Start animation (via internal function since set_animation_active
        // is being tested)
        clear_animation_end_time(); // Clear any end time
        ANIMATION_ACTIVE.store(true, Ordering::Relaxed);
        assert!(is_animation_active());
        assert!(!is_animation_settling()); // Not settling (animation is active)
        assert!(should_ignore_geometry_events()); // Should ignore (animation active)

        // Step 3: End animation - records end time for settling period
        record_animation_end();
        ANIMATION_ACTIVE.store(false, Ordering::Relaxed);
        assert!(!is_animation_active());
        assert!(is_animation_settling()); // Now in settling period
        assert!(should_ignore_geometry_events()); // Should still ignore (settling)

        // Step 4: Wait for settling period to expire
        std::thread::sleep(Duration::from_millis(ANIMATION_SETTLE_DURATION_MS + 20));
        assert!(!is_animation_settling());
        assert!(!should_ignore_geometry_events());

        // Cleanup
        clear_animation_end_time();
    }
}
