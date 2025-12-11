//! Generic debouncer for event handling.
//!
//! This module provides a reusable debouncer that can be used to delay
//! processing of rapid events until they settle. This is useful for handling
//! window moves, resizes, and other events that can fire rapidly.

#![allow(clippy::cast_possible_truncation)] // Duration values won't exceed u64
#![allow(dead_code)] // Public API methods may be unused currently

use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A generic debouncer that tracks pending items and allows draining settled ones.
///
/// The debouncer tracks items by their key (typically a window ID) and only
/// returns them after they have been stable for the specified settle time.
///
/// # Type Parameters
///
/// * `K` - The key type (e.g., `u64` for window IDs)
/// * `V` - The value type to store with each pending item
#[derive(Debug)]
pub struct Debouncer<K, V> {
    /// Pending items waiting to settle.
    pending: HashMap<K, PendingItem<V>>,
    /// How long items must be stable before being returned.
    settle_time: Duration,
}

/// A pending item tracked by the debouncer.
#[derive(Debug, Clone)]
struct PendingItem<V> {
    /// The value associated with this item.
    value: V,
    /// Timestamp when this item was last updated.
    last_updated_ms: u64,
}

impl<K, V> Debouncer<K, V>
where
    K: Eq + Hash + Clone,
    V: Clone,
{
    /// Creates a new debouncer with the specified settle time.
    #[must_use]
    pub fn new(settle_time: Duration) -> Self {
        Self {
            pending: HashMap::new(),
            settle_time,
        }
    }

    /// Updates or inserts a pending item.
    ///
    /// Returns `true` if this is a new item (not already pending).
    pub fn update(&mut self, key: K, value: V) -> bool {
        let now = current_time_ms();
        let is_new = !self.pending.contains_key(&key);

        self.pending.insert(key, PendingItem { value, last_updated_ms: now });

        is_new
    }

    /// Checks if there are any pending items.
    #[must_use]
    pub fn is_empty(&self) -> bool { self.pending.is_empty() }

    /// Checks if there were pending items before an operation.
    ///
    /// This is useful for determining whether to schedule a timer.
    #[must_use]
    pub fn had_pending(&self) -> bool { !self.pending.is_empty() }

    /// Drains all items that have settled (been stable for the settle time).
    ///
    /// Returns a vector of (key, value) pairs for settled items.
    pub fn drain_settled(&mut self) -> Vec<(K, V)> {
        let now = current_time_ms();
        let settle_ms = self.settle_time.as_millis() as u64;

        let settled: Vec<(K, V)> = self
            .pending
            .iter()
            .filter(|(_, item)| now.saturating_sub(item.last_updated_ms) >= settle_ms)
            .map(|(k, item)| (k.clone(), item.value.clone()))
            .collect();

        // Remove settled items
        for (key, _) in &settled {
            self.pending.remove(key);
        }

        settled
    }

    /// Removes a specific key from pending items.
    ///
    /// This is useful when a window is destroyed and we should stop tracking it.
    pub fn remove(&mut self, key: &K) { self.pending.remove(key); }

    /// Clears all pending items.
    pub fn clear(&mut self) { self.pending.clear(); }

    /// Gets the number of pending items.
    #[must_use]
    pub fn len(&self) -> usize { self.pending.len() }

    /// Checks if a specific key is pending.
    #[must_use]
    pub fn contains(&self, key: &K) -> bool { self.pending.contains_key(key) }
}

/// A simpler debouncer for cases where we only track keys (no associated values).
///
/// This is useful for move operations where we only need to track the window ID.
pub type KeyDebouncer<K> = Debouncer<K, ()>;

impl<K> Debouncer<K, ()>
where K: Eq + Hash + Clone
{
    /// Updates or inserts a pending key (no value).
    ///
    /// Returns `true` if this is a new key (not already pending).
    pub fn touch(&mut self, key: K) -> bool { self.update(key, ()) }

    /// Drains all keys that have settled.
    ///
    /// Returns a vector of keys that have settled.
    pub fn drain_settled_keys(&mut self) -> Vec<K> {
        self.drain_settled().into_iter().map(|(k, ())| k).collect()
    }
}

/// Gets current time in milliseconds since UNIX epoch.
fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_debouncer_new_item() {
        let mut debouncer: Debouncer<u64, u32> = Debouncer::new(Duration::from_millis(100));

        assert!(debouncer.is_empty());

        let is_new = debouncer.update(1, 100);
        assert!(is_new);
        assert!(!debouncer.is_empty());
        assert_eq!(debouncer.len(), 1);
    }

    #[test]
    fn test_debouncer_update_existing() {
        let mut debouncer: Debouncer<u64, u32> = Debouncer::new(Duration::from_millis(100));

        let is_new1 = debouncer.update(1, 100);
        assert!(is_new1);

        let is_new2 = debouncer.update(1, 200);
        assert!(!is_new2);
        assert_eq!(debouncer.len(), 1);
    }

    #[test]
    fn test_debouncer_remove() {
        let mut debouncer: Debouncer<u64, u32> = Debouncer::new(Duration::from_millis(100));

        debouncer.update(1, 100);
        debouncer.update(2, 200);
        assert_eq!(debouncer.len(), 2);

        debouncer.remove(&1);
        assert_eq!(debouncer.len(), 1);
        assert!(!debouncer.contains(&1));
        assert!(debouncer.contains(&2));
    }

    #[test]
    fn test_debouncer_clear() {
        let mut debouncer: Debouncer<u64, u32> = Debouncer::new(Duration::from_millis(100));

        debouncer.update(1, 100);
        debouncer.update(2, 200);
        assert_eq!(debouncer.len(), 2);

        debouncer.clear();
        assert!(debouncer.is_empty());
    }

    #[test]
    fn test_key_debouncer() {
        let mut debouncer: KeyDebouncer<u64> = Debouncer::new(Duration::from_millis(100));

        let is_new = debouncer.touch(1);
        assert!(is_new);

        let is_new2 = debouncer.touch(1);
        assert!(!is_new2);
    }

    #[test]
    fn test_debouncer_drain_settled_immediate() {
        // With 0ms settle time, items should settle immediately
        let mut debouncer: Debouncer<u64, u32> = Debouncer::new(Duration::from_millis(0));

        debouncer.update(1, 100);
        debouncer.update(2, 200);

        let settled = debouncer.drain_settled();
        assert_eq!(settled.len(), 2);
        assert!(debouncer.is_empty());
    }

    #[test]
    fn test_debouncer_not_settled_yet() {
        // With very long settle time, items should not settle
        let mut debouncer: Debouncer<u64, u32> = Debouncer::new(Duration::from_secs(3600));

        debouncer.update(1, 100);

        let settled = debouncer.drain_settled();
        assert!(settled.is_empty());
        assert!(!debouncer.is_empty());
    }
}
