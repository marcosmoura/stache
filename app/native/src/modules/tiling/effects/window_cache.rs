//! High-performance window element cache for animation hot paths.
//!
//! This module provides a caching layer for `AXUIElement` resolution to avoid
//! the expensive O(n*m) lookup on every animation frame (where n = apps, m = windows).
//!
//! # Architecture
//!
//! The cache maintains:
//! - `window_id -> (pid, AXUIElementRef)` mapping for fast window lookups
//! - `pid -> AXUIElementRef` mapping for app elements
//! - Automatic invalidation when windows are destroyed
//!
//! # Thread Safety
//!
//! Uses `DashMap` for lock-free concurrent access from multiple threads.
//! `AXUIElements` are retained when cached and released on removal.

use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::modules::tiling::ffi::skylight;
use crate::modules::tiling::identity::{AppIdentity, WindowTarget};
use crate::modules::tiling::state::Rect;

// ============================================================================
// FFI Declarations
// ============================================================================

type AXUIElementRef = *mut c_void;

/// Owns one retained `AXUIElement` reference returned by a create, copy, or
/// enumeration API.
struct OwnedAXElement {
    element: AXUIElementRef,
}

impl OwnedAXElement {
    const fn new(element: AXUIElementRef) -> Self { Self { element } }

    const fn as_raw(&self) -> AXUIElementRef { self.element }

    fn into_raw(mut self) -> AXUIElementRef {
        let element = self.element;
        self.element = std::ptr::null_mut();
        element
    }
}

impl Drop for OwnedAXElement {
    fn drop(&mut self) { release_element(self.element); }
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRetain(cf: *const c_void) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

fn retain_element(element: AXUIElementRef) {
    if !element.is_null() {
        #[cfg(test)]
        if record_cf_ownership_operation(true) {
            return;
        }
        unsafe { CFRetain(element.cast()) };
    }
}

fn release_element(element: AXUIElementRef) {
    if !element.is_null() {
        #[cfg(test)]
        if record_cf_ownership_operation(false) {
            return;
        }
        unsafe { CFRelease(element.cast()) };
    }
}

#[cfg(test)]
thread_local! {
    static CF_OWNERSHIP_COUNTER: std::cell::Cell<Option<(usize, usize)>> = const {
        std::cell::Cell::new(None)
    };
}

#[cfg(test)]
fn record_cf_ownership_operation(retain: bool) -> bool {
    CF_OWNERSHIP_COUNTER.with(|counter| {
        let Some((retains, releases)) = counter.get() else {
            return false;
        };
        counter.set(Some(if retain {
            (retains + 1, releases)
        } else {
            (retains, releases + 1)
        }));
        true
    })
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: *const c_void,
        value: *mut *mut c_void,
    ) -> i32;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: *const c_void,
        value: *const c_void,
    ) -> i32;
    fn AXUIElementGetTypeID() -> u64;
    fn _AXUIElementGetWindow(element: AXUIElementRef, window_id: *mut u32) -> i32;
    fn AXValueCreate(value_type: i32, value: *const c_void) -> *mut c_void;
    fn AXValueGetValue(value: *const c_void, value_type: i32, value_ptr: *mut c_void) -> bool;
}

const K_AX_ERROR_SUCCESS: i32 = 0;
const K_AX_VALUE_TYPE_CG_POINT: i32 = 1;
const K_AX_VALUE_TYPE_CG_SIZE: i32 = 2;

// ============================================================================
// Cached CFStrings
// ============================================================================

use std::cell::OnceCell;

use core_foundation::base::TCFType;
use core_foundation::string::CFString;

thread_local! {
    static CF_WINDOWS: OnceCell<CFString> = const { OnceCell::new() };
    static CF_POSITION: OnceCell<CFString> = const { OnceCell::new() };
    static CF_SIZE: OnceCell<CFString> = const { OnceCell::new() };
    static CF_ROLE: OnceCell<CFString> = const { OnceCell::new() };
}

#[inline]
fn cf_windows() -> *const c_void {
    CF_WINDOWS
        .with(|cell| cell.get_or_init(|| CFString::new("AXWindows")).as_concrete_TypeRef().cast())
}

#[inline]
fn cf_position() -> *const c_void {
    CF_POSITION
        .with(|cell| cell.get_or_init(|| CFString::new("AXPosition")).as_concrete_TypeRef().cast())
}

#[inline]
fn cf_size() -> *const c_void {
    CF_SIZE.with(|cell| cell.get_or_init(|| CFString::new("AXSize")).as_concrete_TypeRef().cast())
}

#[inline]
fn cf_role() -> *const c_void {
    CF_ROLE.with(|cell| cell.get_or_init(|| CFString::new("AXRole")).as_concrete_TypeRef().cast())
}

// ============================================================================
// Cache Entry
// ============================================================================

/// A cached window element entry.
#[derive(Debug)]
struct CachedWindowElement {
    /// Retained `AXUIElement` reference.
    element: AXUIElementRef,
    /// When this entry was cached.
    cached_at: Instant,
}

impl CachedWindowElement {
    fn new(element: AXUIElementRef) -> Self {
        // Retain the element for cache storage
        retain_element(element);
        Self {
            element,
            cached_at: Instant::now(),
        }
    }
}

impl Drop for CachedWindowElement {
    fn drop(&mut self) { release_element(self.element); }
}

// SAFETY: AXUIElements are thread-safe per Apple's documentation
unsafe impl Send for CachedWindowElement {}
unsafe impl Sync for CachedWindowElement {}

/// A cached app element entry.
#[derive(Debug)]
struct CachedAppElement {
    /// Retained `AXUIElement` reference for the app.
    element: AXUIElementRef,
    /// When this entry was cached.
    cached_at: Instant,
}

impl CachedAppElement {
    fn new(element: AXUIElementRef) -> Self {
        // The cache owns an independent retain. Callers retain their own
        // reference while resolving windows, so eviction cannot invalidate an
        // in-flight AX operation.
        retain_element(element);
        Self {
            element,
            cached_at: Instant::now(),
        }
    }
}

impl Drop for CachedAppElement {
    fn drop(&mut self) { release_element(self.element); }
}

unsafe impl Send for CachedAppElement {}
unsafe impl Sync for CachedAppElement {}

// ============================================================================
// Window Element Cache
// ============================================================================

/// Maximum age for cache entries before they're considered stale.
const CACHE_MAX_AGE: Duration = Duration::from_secs(30);

/// High-performance cache for window `AXUIElements`.
///
/// This cache dramatically reduces the cost of window element resolution
/// during animations by avoiding repeated O(n*m) lookups.
pub struct WindowElementCache {
    /// Exact target -> cached element mapping.
    windows: DashMap<WindowTarget, CachedWindowElement>,
    /// Identity -> cached app element mapping.
    apps: DashMap<AppIdentity, CachedAppElement>,
    /// Counter for cache hits (for diagnostics).
    hits: AtomicU64,
    /// Counter for cache misses (for diagnostics).
    misses: AtomicU64,
}

impl WindowElementCache {
    /// Creates a new empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            windows: DashMap::with_capacity(64),
            apps: DashMap::with_capacity(16),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// Resolves an exact target to its `AXUIElement`, using cache if available.
    ///
    /// Returns a retained element that the caller must release.
    #[must_use]
    pub fn resolve(&self, target: WindowTarget) -> Option<AXUIElementRef> {
        // Fast path: check cache first
        if let Some(entry) = self.windows.get(&target) {
            // Check if entry is still fresh
            if entry.cached_at.elapsed() < CACHE_MAX_AGE {
                // Quick validity check - try to get window ID from element
                if is_element_valid(entry.element) {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    // Retain for caller
                    retain_element(entry.element);
                    return Some(entry.element);
                }
            }
            // Entry is stale or invalid, remove it
            drop(entry);
            self.windows.remove(&target);
        }

        self.misses.fetch_add(1, Ordering::Relaxed);
        // Slow path: full exact resolution
        self.resolve_and_cache(target)
    }

    /// Resolves a window element exactly and caches it.
    ///
    /// Obtains one local `NSRunningApplication` for the target's PID, captures
    /// `AppIdentity` from that same object, requires exact equality, then
    /// enumerates only that app's AX windows for `target.window_id`.
    fn resolve_and_cache(&self, target: WindowTarget) -> Option<AXUIElementRef> {
        // Revalidate the identity against the current process state.
        if resolve_current_identity(target.identity) != Some(target.identity) {
            tracing::trace!("window_cache: identity mismatch, refusing to resolve {target:?}");
            return None;
        }

        // Get or create app element
        let app_element = self.get_or_create_app_element(target.identity)?;

        // Search windows of this app
        if let Some(element) = find_window_in_app(app_element.as_raw(), target.window_id) {
            // Cache the result
            self.windows.insert(target, CachedWindowElement::new(element.as_raw()));
            return Some(element.into_raw());
        }

        tracing::trace!("window_cache: target {target:?} not found");
        None
    }

    /// Gets or creates a cached app element for an identity.
    fn get_or_create_app_element(&self, identity: AppIdentity) -> Option<OwnedAXElement> {
        // Check cache first
        if let Some(entry) = self.apps.get(&identity) {
            if entry.cached_at.elapsed() < CACHE_MAX_AGE {
                retain_element(entry.element);
                return Some(OwnedAXElement::new(entry.element));
            }
            drop(entry);
            self.apps.remove(&identity);
        }

        // Create new app element
        let element = OwnedAXElement::new(unsafe { AXUIElementCreateApplication(identity.pid) });
        if element.as_raw().is_null() {
            return None;
        }

        // Keep the creation retain as the caller's lease and give the cache
        // its own retain. Cache eviction may now safely race with resolution.
        self.apps.insert(identity, CachedAppElement::new(element.as_raw()));
        Some(element)
    }

    /// Pre-resolves multiple targets in batch, returning resolved elements.
    ///
    /// This is optimized for animation setup where we need all elements at once.
    /// Returns a vector of (`WindowTarget`, element) pairs. Elements are retained
    /// and must be released by the caller.
    #[must_use]
    pub fn batch_resolve(&self, targets: &[WindowTarget]) -> Vec<(WindowTarget, AXUIElementRef)> {
        let mut results = Vec::with_capacity(targets.len());
        let mut missing: Vec<WindowTarget> = Vec::new();

        // First pass: collect from cache
        for &target in targets {
            if let Some(entry) = self.windows.get(&target) {
                if entry.cached_at.elapsed() < CACHE_MAX_AGE && is_element_valid(entry.element) {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    retain_element(entry.element);
                    results.push((target, entry.element));
                    continue;
                }
                drop(entry);
                self.windows.remove(&target);
            }
            missing.push(target);
        }

        // If nothing missing, we're done
        if missing.is_empty() {
            return results;
        }

        self.misses.fetch_add(missing.len() as u64, Ordering::Relaxed);

        // Second pass: resolve missing targets, grouped by identity.
        let mut remaining: std::collections::HashSet<WindowTarget> = missing.into_iter().collect();

        let identities: Vec<AppIdentity> = remaining.iter().map(|t| t.identity).collect();
        for identity in identities {
            if remaining.is_empty() {
                break;
            }

            if resolve_current_identity(identity) != Some(identity) {
                continue;
            }

            let Some(app_element) = self.get_or_create_app_element(identity) else {
                continue;
            };

            // Get all windows for this app
            let windows = get_app_window_ids_with_elements(app_element.as_raw());

            for (wid, element) in windows {
                let target = WindowTarget { identity, window_id: wid };
                if remaining.remove(&target) {
                    // Cache and add to results
                    self.windows.insert(target, CachedWindowElement::new(element.as_raw()));
                    results.push((target, element.into_raw()));
                }
            }
        }

        results
    }

    /// Invalidates a window entry (call when window is destroyed).
    pub fn invalidate_window(&self, target: WindowTarget) { self.windows.remove(&target); }

    /// Invalidates all windows for an identity (call when app terminates).
    pub fn invalidate_app_identity(&self, identity: AppIdentity) {
        self.apps.remove(&identity);
        // Remove all windows belonging to this app
        self.windows.retain(|target, _| target.identity != identity);
    }

    /// Clears the entire cache.
    pub fn clear(&self) {
        self.windows.clear();
        self.apps.clear();
    }

    /// Returns window IDs that are no longer valid from the given targets.
    ///
    /// This is an efficient way to detect destroyed windows without enumerating
    /// all windows from macOS. Uses cached elements for O(1) validity checks
    /// where possible.
    #[must_use]
    pub fn find_invalid_windows(&self, targets: &[WindowTarget]) -> Vec<u32> {
        let mut invalid = Vec::new();

        for &target in targets {
            // First check if we have a cached element
            if let Some(entry) = self.windows.get(&target) {
                if !is_element_valid(entry.element) {
                    // Cached element is invalid - window was destroyed
                    drop(entry);
                    self.windows.remove(&target);
                    invalid.push(target.window_id);
                }
                // If element is valid, window still exists
                continue;
            }

            // No cached element - try to resolve
            // If resolution fails, window is likely destroyed
            if let Some(element) = self.resolve(target) {
                release_element(element);
            } else {
                invalid.push(target.window_id);
            }
        }

        invalid
    }

    /// Returns cache statistics for diagnostics.
    #[must_use]
    pub fn stats(&self) -> (u64, u64, usize, usize) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
            self.windows.len(),
            self.apps.len(),
        )
    }

    /// Gets the frame of a window using a cached or resolved element.
    ///
    /// This combines resolution and frame retrieval in one call.
    #[must_use]
    pub fn get_window_frame(&self, target: WindowTarget) -> Option<Rect> {
        let element = self.resolve(target)?;
        let frame = get_frame_from_element(element);
        unsafe { CFRelease(element.cast()) };
        frame
    }

    /// Gets the frame of a window using the fast `SkyLight` path when available.
    ///
    /// Falls back to the AX-based cache if `SkyLight` fails.
    #[must_use]
    pub fn get_window_frame_fast(&self, target: WindowTarget) -> Option<Rect> {
        skylight::get_window_bounds_fast(target.window_id).or_else(|| self.get_window_frame(target))
    }

    /// Sets the frame of a window using a cached or resolved element.
    ///
    /// Uses the fast path (2 AX calls) suitable for animations.
    #[must_use]
    pub fn set_window_frame_fast(&self, target: WindowTarget, frame: &Rect) -> bool {
        let Some(element) = self.resolve(target) else {
            return false;
        };
        let result = set_frame_on_element(element, frame);
        unsafe { CFRelease(element.cast()) };
        result
    }
}

impl Default for WindowElementCache {
    fn default() -> Self { Self::new() }
}

// ============================================================================
// Global Cache Instance
// ============================================================================

static GLOBAL_CACHE: OnceLock<WindowElementCache> = OnceLock::new();

/// Gets the global window element cache.
#[must_use]
pub fn get_cache() -> &'static WindowElementCache {
    GLOBAL_CACHE.get_or_init(WindowElementCache::new)
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Checks if an `AXUIElement` is still valid.
fn is_element_valid(element: AXUIElementRef) -> bool {
    if element.is_null() {
        return false;
    }

    // Quick check: try to get the window ID
    let mut window_id: u32 = 0;
    let result = unsafe { _AXUIElementGetWindow(element, &raw mut window_id) };
    result == K_AX_ERROR_SUCCESS && window_id != 0
}

/// Finds a specific window in an app's window list.
fn find_window_in_app(
    app_element: AXUIElementRef,
    target_window_id: u32,
) -> Option<OwnedAXElement> {
    if app_element.is_null() {
        return None;
    }

    let mut value: *mut c_void = std::ptr::null_mut();
    let result =
        unsafe { AXUIElementCopyAttributeValue(app_element, cf_windows(), &raw mut value) };

    if result != K_AX_ERROR_SUCCESS || value.is_null() {
        return None;
    }

    let count = unsafe { CFArrayGetCount(value) };
    if count <= 0 {
        unsafe { CFRelease(value) };
        return None;
    }

    let ax_type_id = unsafe { AXUIElementGetTypeID() };

    for i in 0..count {
        let window = unsafe { CFArrayGetValueAtIndex(value, i) };
        if window.is_null() || unsafe { CFGetTypeID(window) } != ax_type_id {
            continue;
        }

        let mut window_id: u32 = 0;
        if unsafe { _AXUIElementGetWindow(window.cast_mut(), &raw mut window_id) }
            == K_AX_ERROR_SUCCESS
            && window_id == target_window_id
        {
            // Verify it's actually a window (has AXWindow role)
            if is_window_element(window.cast_mut()) {
                // Retain before returning
                retain_element(window.cast_mut());
                unsafe { CFRelease(value) };
                return Some(OwnedAXElement::new(window.cast_mut()));
            }
        }
    }

    unsafe { CFRelease(value) };
    None
}

/// Gets all window IDs and elements for an app.
fn get_app_window_ids_with_elements(app_element: AXUIElementRef) -> Vec<(u32, OwnedAXElement)> {
    if app_element.is_null() {
        return Vec::new();
    }

    let mut value: *mut c_void = std::ptr::null_mut();
    let result =
        unsafe { AXUIElementCopyAttributeValue(app_element, cf_windows(), &raw mut value) };

    if result != K_AX_ERROR_SUCCESS || value.is_null() {
        return Vec::new();
    }

    let count = unsafe { CFArrayGetCount(value) };
    if count <= 0 {
        unsafe { CFRelease(value) };
        return Vec::new();
    }

    let ax_type_id = unsafe { AXUIElementGetTypeID() };
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let mut results = Vec::with_capacity(count as usize);

    for i in 0..count {
        let window = unsafe { CFArrayGetValueAtIndex(value, i) };
        if window.is_null() || unsafe { CFGetTypeID(window) } != ax_type_id {
            continue;
        }

        let mut window_id: u32 = 0;
        if unsafe { _AXUIElementGetWindow(window.cast_mut(), &raw mut window_id) }
            == K_AX_ERROR_SUCCESS
            && window_id != 0
            && is_window_element(window.cast_mut())
        {
            retain_element(window.cast_mut());
            results.push((window_id, OwnedAXElement::new(window.cast_mut())));
        }
    }

    unsafe { CFRelease(value) };
    results
}

/// Checks if an element has the `AXWindow` role.
fn is_window_element(element: AXUIElementRef) -> bool {
    if element.is_null() {
        return false;
    }

    let mut value: *mut c_void = std::ptr::null_mut();
    let result = unsafe { AXUIElementCopyAttributeValue(element, cf_role(), &raw mut value) };

    if result != K_AX_ERROR_SUCCESS || value.is_null() {
        return false;
    }

    let cf_string_type_id = CFString::type_id() as u64;
    if unsafe { CFGetTypeID(value) } != cf_string_type_id {
        unsafe { CFRelease(value) };
        return false;
    }

    let cf_string = unsafe { CFString::wrap_under_get_rule(value.cast()) };
    let role = cf_string.to_string();
    unsafe { CFRelease(value) };

    role == "AXWindow"
}

/// Re-resolves the exact identity for a PID from one local
/// `NSRunningApplication` object.
fn resolve_current_identity(identity: AppIdentity) -> Option<AppIdentity> {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    objc::rc::autoreleasepool(|| unsafe {
        let app: *mut Object = msg_send![
            class!(NSRunningApplication),
            runningApplicationWithProcessIdentifier: identity.pid
        ];
        if app.is_null() {
            return None;
        }
        // `identity.pid` is authoritative: apps that report NSNotFound for
        // `processIdentifier` still re-resolve to the same exact identity.
        AppIdentity::from_ns_running_app_with_pid(app, identity.pid)
    })
}

/// Gets the frame from an already-resolved `AXUIElement`.
fn get_frame_from_element(element: AXUIElementRef) -> Option<Rect> {
    if element.is_null() {
        return None;
    }

    // Get position
    let mut pos_value: *mut c_void = std::ptr::null_mut();
    let pos_result =
        unsafe { AXUIElementCopyAttributeValue(element, cf_position(), &raw mut pos_value) };

    if pos_result != K_AX_ERROR_SUCCESS || pos_value.is_null() {
        return None;
    }

    let mut point = core_graphics::geometry::CGPoint::new(0.0, 0.0);
    let pos_success = unsafe {
        AXValueGetValue(
            pos_value.cast(),
            K_AX_VALUE_TYPE_CG_POINT,
            (&raw mut point).cast(),
        )
    };
    unsafe { CFRelease(pos_value) };

    if !pos_success {
        return None;
    }

    // Get size
    let mut size_value: *mut c_void = std::ptr::null_mut();
    let size_result =
        unsafe { AXUIElementCopyAttributeValue(element, cf_size(), &raw mut size_value) };

    if size_result != K_AX_ERROR_SUCCESS || size_value.is_null() {
        return None;
    }

    let mut size = core_graphics::geometry::CGSize::new(0.0, 0.0);
    let size_success = unsafe {
        AXValueGetValue(
            size_value.cast(),
            K_AX_VALUE_TYPE_CG_SIZE,
            (&raw mut size).cast(),
        )
    };
    unsafe { CFRelease(size_value) };

    if !size_success {
        return None;
    }

    Some(Rect::new(point.x, point.y, size.width, size.height))
}

/// Sets the frame on an already-resolved `AXUIElement`.
fn set_frame_on_element(element: AXUIElementRef, frame: &Rect) -> bool {
    if element.is_null() {
        return false;
    }

    // Set position
    let point = core_graphics::geometry::CGPoint::new(frame.x, frame.y);
    let pos_value = unsafe { AXValueCreate(K_AX_VALUE_TYPE_CG_POINT, (&raw const point).cast()) };
    if pos_value.is_null() {
        return false;
    }
    let pos_result =
        unsafe { AXUIElementSetAttributeValue(element, cf_position(), pos_value.cast()) };
    unsafe { CFRelease(pos_value.cast()) };

    // Set size
    let size = core_graphics::geometry::CGSize::new(frame.width, frame.height);
    let size_value = unsafe { AXValueCreate(K_AX_VALUE_TYPE_CG_SIZE, (&raw const size).cast()) };
    if size_value.is_null() {
        return false;
    }
    let size_result =
        unsafe { AXUIElementSetAttributeValue(element, cf_size(), size_value.cast()) };
    unsafe { CFRelease(size_value.cast()) };

    pos_result == K_AX_ERROR_SUCCESS && size_result == K_AX_ERROR_SUCCESS
}

// ============================================================================
// FFI for CFArray
// ============================================================================

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFGetTypeID(cf: *const c_void) -> u64;
    fn CFArrayGetCount(array: *const c_void) -> i64;
    fn CFArrayGetValueAtIndex(array: *const c_void, idx: i64) -> *const c_void;
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::tiling::identity::{AppIdentity, LaunchDateBits};

    fn test_with_cf_ownership_counter(
        action: impl FnOnce(),
        assert_counts: impl FnOnce(usize, usize),
    ) {
        CF_OWNERSHIP_COUNTER.with(|counter| {
            assert!(counter.replace(Some((0, 0))).is_none());
        });
        action();
        let counts = CF_OWNERSHIP_COUNTER.with(|counter| counter.take().unwrap());
        assert_counts(counts.0, counts.1);
    }

    fn test_identity() -> AppIdentity {
        AppIdentity {
            pid: 42,
            launch_date: LaunchDateBits::from_time_interval_since_reference_date(1.0).unwrap(),
        }
    }

    #[test]
    fn test_cache_creation() {
        let cache = WindowElementCache::new();
        assert_eq!(cache.windows.len(), 0);
        assert_eq!(cache.apps.len(), 0);
    }

    #[test]
    fn test_cache_stats() {
        let cache = WindowElementCache::new();
        let (hits, misses, windows, apps) = cache.stats();
        assert_eq!(hits, 0);
        assert_eq!(misses, 0);
        assert_eq!(windows, 0);
        assert_eq!(apps, 0);
    }

    #[test]
    fn test_cache_invalidation() {
        let cache = WindowElementCache::new();
        // These should not panic even with no entries
        cache.invalidate_window(WindowTarget {
            identity: test_identity(),
            window_id: 12345,
        });
        cache.invalidate_app_identity(test_identity());
        cache.clear();
    }

    #[test]
    fn test_owned_element_releases_its_single_reference() {
        test_with_cf_ownership_counter(
            || {
                let element = OwnedAXElement::new(std::ptr::dangling_mut());
                drop(element);
            },
            |retains, releases| {
                assert_eq!(retains, 0);
                assert_eq!(releases, 1);
            },
        );
    }

    #[test]
    fn test_cached_window_retains_borrowed_element() {
        test_with_cf_ownership_counter(
            || {
                let entry = CachedWindowElement::new(std::ptr::dangling_mut());
                drop(entry);
            },
            |retains, releases| {
                assert_eq!(retains, 1);
                assert_eq!(releases, 1);
            },
        );
    }

    #[test]
    fn test_cached_app_retains_an_independent_reference() {
        test_with_cf_ownership_counter(
            || {
                let entry = CachedAppElement::new(std::ptr::dangling_mut());
                drop(entry);
            },
            |retains, releases| {
                assert_eq!(retains, 1);
                assert_eq!(releases, 1);
            },
        );
    }

    #[test]
    fn test_global_cache() {
        let cache1 = get_cache();
        let cache2 = get_cache();
        // Should be the same instance
        assert!(std::ptr::eq(cache1, cache2));
    }
}
