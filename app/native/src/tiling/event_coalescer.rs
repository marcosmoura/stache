//! Event coalescing for rapid window events.
//!
//! This module provides coalescing of rapid move/resize events to reduce CPU usage
//! during user drag operations. Events within the coalesce window are skipped,
//! but the final event (when mouse is released) is always processed.
//!
//! # How It Works
//!
//! During a window drag operation, macOS sends many move/resize events (often 60+ per
//! second). Processing each event is wasteful since:
//! - The user is still dragging (intermediate positions don't matter)
//! - Layout recalculations are expensive
//! - Border updates are handled by `JankyBorders` anyway
//!
//! The coalescer tracks the last processed time for each (pid, `event_type`) pair.
//! Events within the coalesce window (default 4ms) are skipped. The final event
//! is guaranteed to be processed because `on_mouse_up()` always triggers processing.
//!
//! # Thread Safety
//!
//! The coalescer uses `RwLock` for thread-safe access since events arrive on the
//! main run loop thread and may be queried from other threads.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use super::constants::timing::EVENT_COALESCE_MS;
use super::observer::WindowEventType;

// ============================================================================
// Types
// ============================================================================

/// Key for tracking coalesced events: (pid, `event_type` discriminant).
type CoalesceKey = (i32, u8);

/// Entry tracking the last processed time for an event.
#[derive(Clone, Copy)]
struct CoalesceEntry {
    /// When the last event was processed.
    last_processed: Instant,
}

impl CoalesceEntry {
    /// Creates a new entry with the current time.
    fn new() -> Self { Self { last_processed: Instant::now() } }

    /// Checks if enough time has passed since the last processed event.
    fn should_process(&self, coalesce_window: Duration) -> bool {
        self.last_processed.elapsed() >= coalesce_window
    }

    /// Updates the last processed time to now.
    fn mark_processed(&mut self) { self.last_processed = Instant::now(); }
}

// ============================================================================
// EventCoalescer
// ============================================================================

/// Thread-safe event coalescer for reducing rapid event processing.
///
/// Tracks the last processed time for each (pid, `event_type`) combination
/// and allows skipping events that arrive too quickly.
struct EventCoalescer {
    /// Map of (pid, `event_type`) -> last processed entry.
    entries: RwLock<HashMap<CoalesceKey, CoalesceEntry>>,
    /// Duration to wait between processing events of the same type.
    coalesce_window: Duration,
}

impl EventCoalescer {
    /// Creates a new coalescer with the specified coalesce window.
    fn new(coalesce_ms: u64) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            coalesce_window: Duration::from_millis(coalesce_ms),
        }
    }

    /// Checks if an event should be processed or coalesced.
    ///
    /// Returns `true` if the event should be processed (enough time has passed),
    /// `false` if it should be skipped (too soon after last event).
    ///
    /// If returning `true`, also updates the last processed time.
    fn should_process(&self, pid: i32, event_type: WindowEventType) -> bool {
        let key = make_key(pid, event_type);

        // Fast path: check if we should skip without write lock
        if let Ok(entries) = self.entries.read()
            && let Some(entry) = entries.get(&key)
            && !entry.should_process(self.coalesce_window)
        {
            return false;
        }

        // Need to process - update the entry
        if let Ok(mut entries) = self.entries.write() {
            entries
                .entry(key)
                .and_modify(CoalesceEntry::mark_processed)
                .or_insert_with(CoalesceEntry::new);
        }

        true
    }

    /// Clears all entries for a given PID.
    ///
    /// Called when an app terminates to clean up stale entries.
    #[allow(dead_code)]
    fn clear_pid(&self, pid: i32) {
        if let Ok(mut entries) = self.entries.write() {
            entries.retain(|&(p, _), _| p != pid);
        }
    }

    /// Clears all entries.
    #[allow(dead_code)]
    fn clear(&self) {
        if let Ok(mut entries) = self.entries.write() {
            entries.clear();
        }
    }
}

/// Creates a coalesce key from pid and event type.
const fn make_key(pid: i32, event_type: WindowEventType) -> CoalesceKey {
    // Use the discriminant of the enum as a cheap u8 identifier
    let event_discriminant = match event_type {
        WindowEventType::Created => 0,
        WindowEventType::Destroyed => 1,
        WindowEventType::Focused => 2,
        WindowEventType::Unfocused => 3,
        WindowEventType::Moved => 4,
        WindowEventType::Resized => 5,
        WindowEventType::Minimized => 6,
        WindowEventType::Unminimized => 7,
        WindowEventType::TitleChanged => 8,
        WindowEventType::AppActivated => 9,
        WindowEventType::AppDeactivated => 10,
        WindowEventType::AppHidden => 11,
        WindowEventType::AppShown => 12,
    };
    (pid, event_discriminant)
}

// ============================================================================
// Global Instance
// ============================================================================

use std::sync::OnceLock;

/// Global event coalescer instance (lazily initialized).
static EVENT_COALESCER: OnceLock<EventCoalescer> = OnceLock::new();

/// Gets the global event coalescer, initializing it if necessary.
fn get_coalescer() -> &'static EventCoalescer {
    EVENT_COALESCER.get_or_init(|| EventCoalescer::new(EVENT_COALESCE_MS))
}

// ============================================================================
// Public API
// ============================================================================

/// Checks if a move event should be processed or coalesced.
///
/// Returns `true` if the event should be processed, `false` if it should be skipped.
/// This is called from `handle_window_moved()` to reduce processing of rapid events.
pub fn should_process_move(pid: i32) -> bool {
    get_coalescer().should_process(pid, WindowEventType::Moved)
}

/// Checks if a resize event should be processed or coalesced.
///
/// Returns `true` if the event should be processed, `false` if it should be skipped.
/// This is called from `handle_window_resized()` to reduce processing of rapid events.
pub fn should_process_resize(pid: i32) -> bool {
    get_coalescer().should_process(pid, WindowEventType::Resized)
}

/// Clears coalescing state for a terminated app.
///
/// Called when an app terminates to clean up stale entries.
#[allow(dead_code)]
pub fn clear_app(pid: i32) { get_coalescer().clear_pid(pid); }

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coalesce_entry_new() {
        let entry = CoalesceEntry::new();
        // Just created, should not have enough time elapsed for any reasonable window
        assert!(!entry.should_process(Duration::from_secs(1)));
        // But should process for zero window
        assert!(entry.should_process(Duration::ZERO));
    }

    #[test]
    fn test_coalesce_entry_mark_processed() {
        let mut entry = CoalesceEntry::new();
        std::thread::sleep(Duration::from_millis(5));
        entry.mark_processed();
        // After marking, elapsed time should be near zero again
        assert!(!entry.should_process(Duration::from_millis(10)));
    }

    #[test]
    fn test_make_key_different_events() {
        let key1 = make_key(123, WindowEventType::Moved);
        let key2 = make_key(123, WindowEventType::Resized);
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_make_key_different_pids() {
        let key1 = make_key(123, WindowEventType::Moved);
        let key2 = make_key(456, WindowEventType::Moved);
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_make_key_same_inputs() {
        let key1 = make_key(123, WindowEventType::Moved);
        let key2 = make_key(123, WindowEventType::Moved);
        assert_eq!(key1, key2);
    }

    #[test]
    fn test_event_coalescer_first_event_always_processed() {
        let coalescer = EventCoalescer::new(100); // 100ms window
        // First event should always be processed
        assert!(coalescer.should_process(999, WindowEventType::Moved));
    }

    #[test]
    fn test_event_coalescer_rapid_events_coalesced() {
        let coalescer = EventCoalescer::new(100); // 100ms window

        // First event processed
        assert!(coalescer.should_process(888, WindowEventType::Moved));

        // Immediate second event should be coalesced
        assert!(!coalescer.should_process(888, WindowEventType::Moved));
    }

    #[test]
    fn test_event_coalescer_different_pids_independent() {
        let coalescer = EventCoalescer::new(100);

        // First event for pid 1
        assert!(coalescer.should_process(1, WindowEventType::Moved));

        // First event for pid 2 should also process (independent)
        assert!(coalescer.should_process(2, WindowEventType::Moved));
    }

    #[test]
    fn test_event_coalescer_different_events_independent() {
        let coalescer = EventCoalescer::new(100);

        // Move event
        assert!(coalescer.should_process(777, WindowEventType::Moved));

        // Resize event for same PID should also process (independent)
        assert!(coalescer.should_process(777, WindowEventType::Resized));
    }

    #[test]
    fn test_event_coalescer_zero_window_no_coalescing() {
        let coalescer = EventCoalescer::new(0); // No coalescing

        // All events should process with zero window
        assert!(coalescer.should_process(666, WindowEventType::Moved));
        assert!(coalescer.should_process(666, WindowEventType::Moved));
        assert!(coalescer.should_process(666, WindowEventType::Moved));
    }

    #[test]
    fn test_event_coalescer_clear_pid() {
        let coalescer = EventCoalescer::new(1000); // Long window

        // Process an event
        assert!(coalescer.should_process(555, WindowEventType::Moved));

        // Should be coalesced
        assert!(!coalescer.should_process(555, WindowEventType::Moved));

        // Clear the PID
        coalescer.clear_pid(555);

        // Should process again (entry cleared)
        assert!(coalescer.should_process(555, WindowEventType::Moved));
    }

    #[test]
    fn test_event_coalescer_clear() {
        let coalescer = EventCoalescer::new(1000);

        // Process events for multiple PIDs
        assert!(coalescer.should_process(111, WindowEventType::Moved));
        assert!(coalescer.should_process(222, WindowEventType::Moved));

        // Both should be coalesced
        assert!(!coalescer.should_process(111, WindowEventType::Moved));
        assert!(!coalescer.should_process(222, WindowEventType::Moved));

        // Clear all
        coalescer.clear();

        // Both should process again
        assert!(coalescer.should_process(111, WindowEventType::Moved));
        assert!(coalescer.should_process(222, WindowEventType::Moved));
    }

    #[test]
    fn test_coalesce_constant() {
        // Coalesce window should be reasonable (2-16ms for ~60-500fps)
        assert!(EVENT_COALESCE_MS >= 2);
        assert!(EVENT_COALESCE_MS <= 16);
    }

    #[test]
    fn test_get_coalescer_returns_same_instance() {
        let c1 = get_coalescer();
        let c2 = get_coalescer();
        assert!(std::ptr::eq(c1, c2));
    }
}
