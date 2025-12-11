//! Window ID → `AXUIElement` cache.
//!
//! This module provides a cache that maps window IDs to their corresponding
//! `AXUIElement` references. This avoids the expensive operation of:
//! 1. Creating an app element from PID
//! 2. Querying all windows from the app
//! 3. Matching by title/position to find the right window
//!
//! The cache is automatically invalidated when windows are destroyed.

#![allow(clippy::cast_possible_truncation)]
#![allow(dead_code)] // Public API methods may be unused currently

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use crate::tiling::accessibility::AccessibilityElement;
use crate::tiling::error::TilingError;

// FFI declarations for Core Foundation reference counting
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRetain(cf: *mut c_void) -> *mut c_void;
    fn CFRelease(cf: *mut c_void);
}

/// How long a cached entry is considered valid before requiring revalidation.
const CACHE_ENTRY_TTL: Duration = Duration::from_secs(30);

/// Maximum number of entries in the cache before cleanup is triggered.
const MAX_CACHE_SIZE: usize = 256;

/// Thread-safe wrapper for `AXUIElementRef`.
///
/// This wrapper retains the element on creation and releases it on drop,
/// ensuring proper memory management across cache operations.
#[derive(Debug)]
struct CachedElement {
    /// The raw `AXUIElement` pointer (retained).
    element: *mut c_void,
    /// When this entry was cached.
    cached_at: Instant,
    /// The PID of the application (for validation).
    pid: i32,
}

// SAFETY: AXUIElementRef is thread-safe according to Apple documentation.
unsafe impl Send for CachedElement {}
unsafe impl Sync for CachedElement {}

impl CachedElement {
    /// Creates a new cached element by retaining the given element reference.
    ///
    /// # Safety
    /// The caller must ensure that `element_ref` is a valid `AXUIElementRef`.
    unsafe fn new(element_ref: *mut c_void, pid: i32) -> Self {
        // Retain the element so it survives in the cache
        // SAFETY: Caller guarantees element_ref is a valid AXUIElementRef
        unsafe { CFRetain(element_ref) };
        Self {
            element: element_ref,
            cached_at: Instant::now(),
            pid,
        }
    }

    /// Checks if this entry is still valid (not expired).
    fn is_valid(&self) -> bool { self.cached_at.elapsed() < CACHE_ENTRY_TTL }

    /// Returns a new `AccessibilityElement` from this cached entry.
    ///
    /// This retains the underlying element again so the returned
    /// `AccessibilityElement` has its own ownership.
    fn to_element(&self) -> AccessibilityElement {
        // Retain again for the new AccessibilityElement
        unsafe {
            CFRetain(self.element);
            AccessibilityElement::from_raw(self.element)
        }
    }
}

impl Drop for CachedElement {
    fn drop(&mut self) {
        if !self.element.is_null() {
            unsafe { CFRelease(self.element) };
        }
    }
}

/// Global AX element cache.
static AX_ELEMENT_CACHE: LazyLock<RwLock<AxElementCache>> =
    LazyLock::new(|| RwLock::new(AxElementCache::new()));

/// The AX element cache structure.
struct AxElementCache {
    /// Map from window ID to cached element.
    entries: HashMap<u64, CachedElement>,
    /// Stats for monitoring cache performance.
    stats: CacheStats,
}

/// Cache statistics.
#[derive(Debug, Default)]
struct CacheStats {
    hits: u64,
    misses: u64,
    evictions: u64,
}

impl AxElementCache {
    /// Creates a new empty cache.
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            stats: CacheStats::default(),
        }
    }

    /// Gets an element from the cache if it exists and is valid.
    fn get(&mut self, window_id: u64) -> Option<AccessibilityElement> {
        if let Some(entry) = self.entries.get(&window_id)
            && entry.is_valid()
        {
            self.stats.hits += 1;
            return Some(entry.to_element());
        }
        self.stats.misses += 1;
        None
    }

    /// Inserts an element into the cache.
    ///
    /// # Safety
    /// The caller must ensure the element pointer is valid.
    unsafe fn insert(&mut self, window_id: u64, element_ref: *mut c_void, pid: i32) {
        // Check if we need to cleanup before inserting
        if self.entries.len() >= MAX_CACHE_SIZE {
            self.cleanup_expired();
        }

        // If still at capacity after cleanup, evict oldest entries
        if self.entries.len() >= MAX_CACHE_SIZE {
            self.evict_oldest(MAX_CACHE_SIZE / 4);
        }

        // SAFETY: Caller guarantees element_ref is valid
        self.entries.insert(window_id, unsafe { CachedElement::new(element_ref, pid) });
    }

    /// Removes an element from the cache.
    fn remove(&mut self, window_id: u64) { self.entries.remove(&window_id); }

    /// Removes all elements for a specific PID.
    fn remove_by_pid(&mut self, pid: i32) { self.entries.retain(|_, entry| entry.pid != pid); }

    /// Cleans up expired entries.
    fn cleanup_expired(&mut self) {
        let before = self.entries.len();
        self.entries.retain(|_, entry| entry.is_valid());
        self.stats.evictions += (before - self.entries.len()) as u64;
    }

    /// Evicts the oldest N entries.
    fn evict_oldest(&mut self, count: usize) {
        if self.entries.is_empty() {
            return;
        }

        // Collect entries sorted by age (oldest first)
        let mut entries: Vec<_> = self.entries.iter().map(|(&id, e)| (id, e.cached_at)).collect();
        entries.sort_by_key(|(_, time)| *time);

        // Remove the oldest entries
        for (id, _) in entries.into_iter().take(count) {
            self.entries.remove(&id);
            self.stats.evictions += 1;
        }
    }

    /// Clears all entries from the cache.
    fn clear(&mut self) {
        self.stats.evictions += self.entries.len() as u64;
        self.entries.clear();
    }
}

// ============================================================================
// Public API
// ============================================================================

/// Tries to get a cached AX element for a window ID.
///
/// Returns `Some(element)` if the element is in the cache and still valid,
/// `None` otherwise.
#[must_use]
pub fn get_cached_element(window_id: u64) -> Option<AccessibilityElement> {
    AX_ELEMENT_CACHE.write().get(window_id)
}

/// Caches an AX element for a window ID.
///
/// The element is retained in the cache, so the caller can drop their
/// reference after calling this function.
///
/// # Safety
/// This function is safe because `AccessibilityElement` guarantees the
/// underlying pointer is valid.
pub fn cache_element(window_id: u64, element: &AccessibilityElement, pid: i32) {
    // Get the raw pointer from the element
    let element_ref = element.as_raw();
    if element_ref.is_null() {
        return;
    }

    unsafe {
        AX_ELEMENT_CACHE.write().insert(window_id, element_ref, pid);
    }
}

/// Removes a window from the cache.
///
/// This should be called when a window is destroyed.
pub fn invalidate_window(window_id: u64) { AX_ELEMENT_CACHE.write().remove(window_id); }

/// Removes all windows for a specific application from the cache.
///
/// This should be called when an application terminates.
pub fn invalidate_app(pid: i32) { AX_ELEMENT_CACHE.write().remove_by_pid(pid); }

/// Clears the entire cache.
///
/// This should be called on screen changes or other major events.
pub fn clear_cache() { AX_ELEMENT_CACHE.write().clear(); }

/// Gets cache statistics for debugging/monitoring.
#[must_use]
pub fn cache_stats() -> (u64, u64, u64) {
    let cache = AX_ELEMENT_CACHE.read();
    (cache.stats.hits, cache.stats.misses, cache.stats.evictions)
}

/// Result type alias for cache operations.
pub type CacheResult<T> = Result<T, TilingError>;

/// Gets an AX element for a window, using the cache if available.
///
/// This is the main entry point for getting AX elements. It:
/// 1. Checks the cache first
/// 2. Falls back to the slow lookup if not cached
/// 3. Caches the result for future use
pub fn get_element_cached<F>(
    window_id: u64,
    pid: i32,
    lookup_fn: F,
) -> CacheResult<AccessibilityElement>
where
    F: FnOnce() -> CacheResult<AccessibilityElement>,
{
    // Try cache first
    if let Some(element) = get_cached_element(window_id) {
        return Ok(element);
    }

    // Cache miss - perform lookup
    let element = lookup_fn()?;

    // Cache the result
    cache_element(window_id, &element, pid);

    Ok(element)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_stats_initial() {
        // Just verify we can read stats
        let (hits, misses, evictions) = cache_stats();
        // Stats should be non-negative (they're cumulative)
        // Just verify we can read stats without panicking
        let _ = (hits, misses, evictions);
    }

    #[test]
    fn test_invalidate_nonexistent_window() {
        // Should not panic
        invalidate_window(999_999);
    }

    #[test]
    fn test_invalidate_nonexistent_app() {
        // Should not panic
        invalidate_app(-1);
    }

    #[test]
    fn test_clear_cache() {
        // Should not panic
        clear_cache();
    }
}
