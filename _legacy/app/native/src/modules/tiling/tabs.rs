//! Tab detection and registry for native macOS window tabs.
//!
//! macOS allows multiple windows to be "tabbed" together, appearing as a single
//! window with tabs in the title bar. Each tab is technically a separate window
//! with its own `CGWindowID`.
//!
//! # Detection Strategy
//!
//! We detect tabs using the Accessibility API:
//! 1. Get windows for an app
//! 2. For windows with an `AXTabGroup` child, get the `AXTabs` attribute
//! 3. Each tab element in `AXTabs` can give us a window ID via `_AXUIElementGetWindow`
//! 4. We track those window IDs as "tabs"
//!
//! # Registry
//!
//! The [`TabRegistry`] tracks which window IDs are tabs.
//! This allows us to:
//! - Skip layout recalculations when tabs are created/destroyed
//! - Properly identify when a window operation is actually a tab operation

use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::OnceLock;

use parking_lot::RwLock;

use crate::modules::tiling::identity::AppIdentity;

// ============================================================================
// FFI for accessing tab window IDs
// ============================================================================

type AXUIElementRef = *mut c_void;
type AXError = i32;

const K_AX_ERROR_SUCCESS: AXError = 0;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: *const c_void,
        value: *mut *mut c_void,
    ) -> AXError;
    fn AXUIElementGetTypeID() -> u64;
    fn _AXUIElementGetWindow(element: AXUIElementRef, window_id: *mut u32) -> AXError;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFGetTypeID(cf: *const c_void) -> u64;
    fn CFArrayGetCount(array: *const c_void) -> i64;
    fn CFArrayGetValueAtIndex(array: *const c_void, idx: i64) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

// ============================================================================
// Tab Registry
// ============================================================================

/// Global tab registry for tracking tab window IDs.
static TAB_REGISTRY: OnceLock<RwLock<TabRegistry>> = OnceLock::new();

/// Gets the global tab registry.
fn get_registry() -> &'static RwLock<TabRegistry> {
    TAB_REGISTRY.get_or_init(|| RwLock::new(TabRegistry::new()))
}

/// Registry for tracking tab window IDs.
///
/// Window IDs in this registry are tabs - operations on them should NOT
/// trigger layout recalculations.
#[derive(Debug, Default)]
pub struct TabRegistry {
    /// Window IDs that are tabs.
    tab_window_ids: HashSet<u32>,

    /// Maps window ID to its owning identity (for cleanup on app termination).
    window_to_identity: std::collections::HashMap<u32, AppIdentity>,
}

impl TabRegistry {
    /// Creates a new empty registry.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Registers a window ID as a tab.
    pub fn register_tab(&mut self, window_id: u32, identity: AppIdentity) {
        self.tab_window_ids.insert(window_id);
        self.window_to_identity.insert(window_id, identity);
    }

    /// Unregisters a window ID from the registry — only when it belongs to
    /// the exact identity.
    pub fn unregister_tab_for_identity(&mut self, window_id: u32, identity: AppIdentity) {
        if self.window_to_identity.get(&window_id) == Some(&identity) {
            self.tab_window_ids.remove(&window_id);
            self.window_to_identity.remove(&window_id);
        }
    }

    /// Checks if a window ID is a tab of the exact identity.
    #[must_use]
    pub fn is_tab_for_identity(&self, window_id: u32, identity: AppIdentity) -> bool {
        self.window_to_identity.get(&window_id) == Some(&identity)
    }

    /// Gets all tracked tab window IDs.
    #[must_use]
    pub fn all_tabs(&self) -> Vec<u32> { self.tab_window_ids.iter().copied().collect() }

    /// Gets all tab window IDs for a given identity.
    #[must_use]
    pub fn tabs_for_identity(&self, identity: AppIdentity) -> Vec<u32> {
        self.tab_window_ids
            .iter()
            .filter(|&&wid| self.window_to_identity.get(&wid) == Some(&identity))
            .copied()
            .collect()
    }

    /// Clears all entries for a given identity.
    pub fn clear_for_identity(&mut self, identity: AppIdentity) {
        let to_remove: Vec<u32> = self
            .window_to_identity
            .iter()
            .filter(|&(_, ident)| *ident == identity)
            .map(|(&w, _)| w)
            .collect();

        for window_id in to_remove {
            self.tab_window_ids.remove(&window_id);
            self.window_to_identity.remove(&window_id);
        }
    }

    /// Replaces all tracked tabs for a given identity with a fresh snapshot.
    pub fn replace_tabs_for_identity(
        &mut self,
        identity: AppIdentity,
        window_ids: impl IntoIterator<Item = u32>,
    ) {
        self.clear_for_identity(identity);

        for window_id in window_ids {
            self.register_tab(window_id, identity);
        }
    }

    /// Clears the entire registry.
    pub fn clear(&mut self) {
        self.tab_window_ids.clear();
        self.window_to_identity.clear();
    }

    /// Returns the count of tracked tabs.
    #[must_use]
    pub fn count(&self) -> usize { self.tab_window_ids.len() }
}

// ============================================================================
// Public API
// ============================================================================

/// Checks if a window ID is a tracked tab of the exact identity.
///
/// If this returns true, operations on this window should NOT trigger
/// layout recalculations.
#[must_use]
pub fn is_tab_for_identity(window_id: u32, identity: AppIdentity) -> bool {
    get_registry().read().is_tab_for_identity(window_id, identity)
}

/// Registers a window ID as a tab.
pub fn register_tab(window_id: u32, identity: AppIdentity) {
    get_registry().write().register_tab(window_id, identity);
}

/// Unregisters a window ID from the tab registry (exact identity only).
pub fn unregister_tab_for_identity(window_id: u32, identity: AppIdentity) {
    get_registry().write().unregister_tab_for_identity(window_id, identity);
}

/// Clears all tab entries for a given identity.
pub fn clear_tabs_for_identity(identity: AppIdentity) {
    get_registry().write().clear_for_identity(identity);
}

/// Clears the entire tab registry.
pub fn clear_all_tabs() { get_registry().write().clear(); }

/// Gets all tracked tab window IDs.
#[must_use]
pub fn all_tracked_tabs() -> Vec<u32> { get_registry().read().all_tabs() }

/// Gets all tab window IDs for a given identity.
#[must_use]
pub fn tabs_for_identity(identity: AppIdentity) -> Vec<u32> {
    get_registry().read().tabs_for_identity(identity)
}

/// Replaces all tracked tabs for a given identity with a fresh snapshot.
pub fn replace_tabs_for_identity(identity: AppIdentity, window_ids: impl IntoIterator<Item = u32>) {
    get_registry().write().replace_tabs_for_identity(identity, window_ids);
}

// ============================================================================
// Tab Detection
// ============================================================================

/// Deterministic publish seam: only replaces the identity's tabs when the
/// post-scan re-resolved identity still matches the expected one.
fn publish_tabs_if_identity_matches(
    registry: &mut TabRegistry,
    expected: AppIdentity,
    observed_after_scan: Option<AppIdentity>,
    window_ids: impl IntoIterator<Item = u32>,
) -> bool {
    if observed_after_scan != Some(expected) {
        return false;
    }
    registry.replace_tabs_for_identity(expected, window_ids);
    true
}

/// Scans an application's windows and registers all tab window IDs.
///
/// This function:
/// 1. Re-resolves the current `NSRunningApplication` and requires the
///    captured identity to match before scanning.
/// 2. Gets all windows for the app
/// 3. For each window with an `AXTabGroup` child, gets the `AXTabs`
/// 4. Extracts window IDs from each tab element using `_AXUIElementGetWindow`
/// 5. Re-resolves the identity immediately before publication and only then
///    registers those window IDs in the global tab registry.
///
/// Call this when:
/// - An app is first tracked
/// - A window is created (to detect if it's part of a tab group)
pub fn scan_and_register_tabs_for_app(identity: AppIdentity) {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    let resolve = || -> Option<AppIdentity> {
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
    };

    // Re-resolve before scanning; a changed identity means PID reuse.
    if resolve() != Some(identity) {
        return;
    }

    unsafe {
        let app = AXUIElementCreateApplication(identity.pid);
        if app.is_null() {
            return;
        }

        // Get AXWindows
        let windows_attr = CFString::new("AXWindows");
        let mut windows_value: *mut c_void = std::ptr::null_mut();
        let result = AXUIElementCopyAttributeValue(
            app,
            windows_attr.as_concrete_TypeRef().cast(),
            &raw mut windows_value,
        );

        CFRelease(app.cast());

        if result != K_AX_ERROR_SUCCESS || windows_value.is_null() {
            return;
        }

        let window_count = CFArrayGetCount(windows_value);
        let ax_type_id = AXUIElementGetTypeID();
        let mut tab_window_ids = Vec::new();

        for i in 0..window_count {
            let window = CFArrayGetValueAtIndex(windows_value, i);
            if window.is_null() || CFGetTypeID(window) != ax_type_id {
                continue;
            }

            // Find AXTabGroup child and get tabs from it
            let tabs_from_window = get_tab_window_ids_from_window(window.cast_mut());

            tab_window_ids.extend(tabs_from_window);
        }

        CFRelease(windows_value);

        // Re-resolve immediately before publication to close the PID-reuse window.
        publish_tabs_if_identity_matches(
            &mut get_registry().write(),
            identity,
            resolve(),
            tab_window_ids,
        );
    }
}

/// Gets tab window IDs from a window's `AXTabGroup`.
///
/// Returns window IDs extracted from the `AXTabs` attribute of the window's
/// `AXTabGroup` child element.
///
/// **Important:** Only returns window IDs that are DIFFERENT from the parent window.
/// Some apps (like Ghostty) report the parent window ID for all tabs, which would
/// incorrectly cause the main window to be skipped.
fn get_tab_window_ids_from_window(window: AXUIElementRef) -> Vec<u32> {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;

    let mut result = Vec::new();

    // Get the parent window's ID so we can exclude it
    let mut parent_window_id: u32 = 0;
    let parent_has_id = unsafe { _AXUIElementGetWindow(window, &raw mut parent_window_id) }
        == K_AX_ERROR_SUCCESS
        && parent_window_id != 0;

    unsafe {
        // Get children of the window
        let children_attr = CFString::new("AXChildren");
        let mut children_value: *mut c_void = std::ptr::null_mut();
        let err = AXUIElementCopyAttributeValue(
            window,
            children_attr.as_concrete_TypeRef().cast(),
            &raw mut children_value,
        );

        if err != K_AX_ERROR_SUCCESS || children_value.is_null() {
            return result;
        }

        let ax_type_id = AXUIElementGetTypeID();
        let child_count = CFArrayGetCount(children_value);

        // Find AXTabGroup among children
        let role_attr = CFString::new("AXRole");
        let tab_group_role = "AXTabGroup";

        for i in 0..child_count {
            let child = CFArrayGetValueAtIndex(children_value, i);
            if child.is_null() || CFGetTypeID(child) != ax_type_id {
                continue;
            }

            // Check if this child has role AXTabGroup
            let mut role_value: *mut c_void = std::ptr::null_mut();
            let role_err = AXUIElementCopyAttributeValue(
                child.cast_mut(),
                role_attr.as_concrete_TypeRef().cast(),
                &raw mut role_value,
            );

            if role_err != K_AX_ERROR_SUCCESS || role_value.is_null() {
                continue;
            }

            let role_cf = core_foundation::string::CFString::wrap_under_get_rule(role_value.cast());
            let role_str = role_cf.to_string();
            CFRelease(role_value);

            if role_str == tab_group_role {
                // Found AXTabGroup! Get its AXTabs
                let tabs = get_tabs_from_tab_group(child.cast_mut());

                // Only include tab window IDs that are DIFFERENT from the parent window
                // Apps like Ghostty report the parent window ID for all tabs, which would
                // incorrectly cause the main window to be skipped
                for tab_wid in tabs {
                    if !parent_has_id || tab_wid != parent_window_id {
                        result.push(tab_wid);
                    }
                }
            }
        }

        CFRelease(children_value);
    }

    result
}

/// Gets window IDs from an `AXTabGroup`'s `AXTabs` attribute.
fn get_tabs_from_tab_group(tab_group: AXUIElementRef) -> Vec<u32> {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;

    let mut result = Vec::new();

    unsafe {
        let tabs_attr = CFString::new("AXTabs");
        let mut tabs_value: *mut c_void = std::ptr::null_mut();
        let err = AXUIElementCopyAttributeValue(
            tab_group,
            tabs_attr.as_concrete_TypeRef().cast(),
            &raw mut tabs_value,
        );

        if err != K_AX_ERROR_SUCCESS || tabs_value.is_null() {
            return result;
        }

        let ax_type_id = AXUIElementGetTypeID();
        let tab_count = CFArrayGetCount(tabs_value);

        for i in 0..tab_count {
            let tab = CFArrayGetValueAtIndex(tabs_value, i);
            if tab.is_null() || CFGetTypeID(tab) != ax_type_id {
                continue;
            }

            // Try to get window ID from this tab element
            let mut window_id: u32 = 0;
            let wid_err = _AXUIElementGetWindow(tab.cast_mut(), &raw mut window_id);

            if wid_err == K_AX_ERROR_SUCCESS && window_id != 0 {
                result.push(window_id);
            }
        }

        CFRelease(tabs_value);
    }

    result
}

/// Checks if a newly created window is a tab by checking if its "parent" window
/// (another window from the same app in the same workspace) is already tracked.
///
/// This is used for the window creation case: if we're creating a window and
/// there's already a non-tab window from the same app in the same workspace,
/// this new window is likely a tab being opened.
///
/// Returns true if this appears to be a new tab (skip layout), false otherwise.
#[must_use]
pub fn is_new_window_a_tab(
    identity: AppIdentity,
    new_window_id: u32,
    workspace_window_ids: &[u32],
) -> bool {
    tracing::debug!(
        ?identity,
        new_window_id,
        workspace_window_count = workspace_window_ids.len(),
        ?workspace_window_ids,
        "tiling: evaluating new-window tab classification"
    );

    // First, scan to update the registry with current tab state
    scan_and_register_tabs_for_app(identity);

    // If this window is already registered as a tab of this identity, it's a tab
    if is_tab_for_identity(new_window_id, identity) {
        tracing::debug!(
            ?identity,
            new_window_id,
            result = true,
            reason = "already-registered-tab",
            "tiling: tab classification result"
        );
        return true;
    }

    // Check if there's another window from the same app in the workspace
    // that is NOT a tab - if so, this new window might be joining as a tab
    let registry = get_registry().read();
    let tabs_for_this_identity: HashSet<u32> =
        registry.tabs_for_identity(identity).into_iter().collect();

    for &wid in workspace_window_ids {
        if wid == new_window_id {
            continue;
        }

        // Check if this window belongs to the same identity
        // We need to verify this is from the same app instance
        if registry.window_to_identity.get(&wid) == Some(&identity)
            && !tabs_for_this_identity.contains(&wid)
        {
            // Found a non-tab window from the same app in the workspace
            // This new window is likely a new tab
            tracing::debug!(
                ?identity,
                new_window_id,
                sibling_window = wid,
                result = true,
                reason = "same-identity-non-tab-sibling",
                "tiling: tab classification result"
            );
            return true;
        }
    }

    tracing::debug!(
        ?identity,
        new_window_id,
        result = false,
        reason = "no-tab-evidence",
        "tiling: tab classification result"
    );
    false
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::tiling::identity::{AppIdentity, LaunchDateBits};

    #[allow(clippy::cast_precision_loss)] // test-only identity construction
    fn identity(pid: i32, v: u64) -> AppIdentity {
        AppIdentity {
            pid,
            launch_date: LaunchDateBits::from_time_interval_since_reference_date(v as f64).unwrap(),
        }
    }

    #[test]
    fn test_tab_registry_basic() {
        let a = identity(1000, 1);
        let mut registry = TabRegistry::new();

        registry.register_tab(100, a);
        assert!(registry.is_tab_for_identity(100, a));
        assert_eq!(registry.count(), 1);

        registry.register_tab(101, a);
        assert!(registry.is_tab_for_identity(101, a));
        assert_eq!(registry.count(), 2);

        registry.unregister_tab_for_identity(100, a);
        assert!(!registry.is_tab_for_identity(100, a));
        assert!(registry.is_tab_for_identity(101, a));
        assert_eq!(registry.count(), 1);
    }

    #[test]
    fn test_tab_registry_clear_for_identity() {
        let a = identity(1000, 1);
        let b = identity(2000, 2);
        let mut registry = TabRegistry::new();

        registry.register_tab(100, a);
        registry.register_tab(101, a);
        registry.register_tab(200, b);

        assert_eq!(registry.tabs_for_identity(a).len(), 2);
        assert_eq!(registry.tabs_for_identity(b).len(), 1);

        registry.clear_for_identity(a);

        assert!(!registry.is_tab_for_identity(100, a));
        assert!(!registry.is_tab_for_identity(101, a));
        assert!(registry.is_tab_for_identity(200, b));
        assert_eq!(registry.count(), 1);
    }

    #[test]
    fn test_tab_registry_replace_tabs_for_identity() {
        let a = identity(1000, 1);
        let b = identity(2000, 2);
        let mut registry = TabRegistry::new();

        registry.register_tab(100, a);
        registry.register_tab(101, a);
        registry.register_tab(200, b);

        registry.replace_tabs_for_identity(a, [102, 103]);

        assert!(!registry.is_tab_for_identity(100, a));
        assert!(!registry.is_tab_for_identity(101, a));
        assert!(registry.is_tab_for_identity(102, a));
        assert!(registry.is_tab_for_identity(103, a));
        assert!(registry.is_tab_for_identity(200, b));
        assert_eq!(registry.tabs_for_identity(a).len(), 2);
        assert_eq!(registry.tabs_for_identity(b).len(), 1);
    }

    #[test]
    fn test_tab_registry_clear_all() {
        let a = identity(1000, 1);
        let b = identity(2000, 2);
        let mut registry = TabRegistry::new();

        registry.register_tab(100, a);
        registry.register_tab(200, b);
        assert_eq!(registry.count(), 2);

        registry.clear();
        assert_eq!(registry.count(), 0);
    }

    #[test]
    fn same_pid_identities_are_independent() {
        // Identities A/B share PID 42 but have different launch dates.
        let a = identity(42, 1);
        let b = identity(42, 2);
        let mut registry = TabRegistry::new();

        registry.register_tab(101, a);
        registry.register_tab(202, b);

        // Clearing A must not affect B.
        registry.clear_for_identity(a);
        assert!(registry.is_tab_for_identity(202, b));
        assert!(!registry.is_tab_for_identity(101, a));

        // Replacing A's scan cannot replace B's tabs.
        registry.replace_tabs_for_identity(a, [303]);
        assert!(registry.is_tab_for_identity(303, a));
        assert!(registry.is_tab_for_identity(202, b));

        // Same window-ID 303: A→B replacement survives exact unregister of A
        // and is removed by exact unregister of B.
        registry.register_tab(303, b);
        registry.unregister_tab_for_identity(303, a);
        assert!(
            registry.is_tab_for_identity(303, b),
            "exact unregister of A must not touch B"
        );

        registry.unregister_tab_for_identity(303, b);
        assert!(
            !registry.is_tab_for_identity(303, b),
            "exact unregister of B removes it"
        );
    }

    #[test]
    fn publish_seam_requires_matching_post_scan_identity() {
        let a = identity(42, 1);
        let b = identity(42, 2);
        let mut registry = TabRegistry::new();

        // Mismatched post-scan identity publishes nothing.
        assert!(!publish_tabs_if_identity_matches(&mut registry, a, Some(b), [
            101
        ]));
        assert_eq!(registry.count(), 0);

        // Matching post-scan identity publishes.
        assert!(publish_tabs_if_identity_matches(&mut registry, a, Some(a), [
            101, 102
        ]));
        let mut tabs = registry.tabs_for_identity(a);
        tabs.sort_unstable();
        assert_eq!(tabs, vec![101, 102]);

        // None (app gone) publishes nothing.
        assert!(!publish_tabs_if_identity_matches(&mut registry, b, None, [303]));
        assert_eq!(registry.count(), 2);
    }
}
