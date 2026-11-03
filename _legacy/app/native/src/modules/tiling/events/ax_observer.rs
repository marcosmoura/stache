//! `AXObserver` adapter for the tiling v2 event pipeline.
//!
//! This module bridges the macOS Accessibility observer system to the new
//! event processor architecture. It translates [`WindowEvent`]s from the
//! existing observer into [`StateMessage`]s for the state actor.
//!
//! # Architecture
//!
//! ```text
//! ┌───────────────────────────────────────────────────────────────┐
//! │                    macOS Accessibility API                     │
//! │                       (AXObserver)                             │
//! └───────────────────────────┬───────────────────────────────────┘
//!                             │ `WindowEvent` callback
//!                             ▼
//! ┌───────────────────────────────────────────────────────────────┐
//! │                  `AXObserverAdapter`                           │
//! │  - Translates `WindowEvent` → `EventProcessor` calls          │
//! │  - Extracts window info from AX element                        │
//! │  - Maps window IDs to screens                                  │
//! └───────────────────────────┬───────────────────────────────────┘
//!                             │ `EventProcessor` methods
//!                             ▼
//! ┌───────────────────────────────────────────────────────────────┐
//! │                    `EventProcessor`                            │
//! │  - Batches geometry events per display refresh                 │
//! │  - Dispatches to `StateActor`                                  │
//! └───────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Thread Safety
//!
//! The adapter is designed to be called from the main thread (where `AXObserver`
//! callbacks occur). The [`EventProcessor`] it references is thread-safe.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::RwLock;

use super::types::{WindowEvent, WindowEventType};
use crate::modules::tiling::actor::WindowCreatedInfo;
use crate::modules::tiling::events::EventProcessor;
use crate::modules::tiling::identity::{AppIdentity, process_id_for_app};
use crate::modules::tiling::rules::{is_pip_window, should_tile_window_with_rules};
use crate::modules::tiling::state::Rect;

// ============================================================================
// FFI Declarations (subset needed for window info extraction)
// ============================================================================

type AXUIElementRef = *mut c_void;
type AXError = i32;

const K_AX_ERROR_SUCCESS: AXError = 0;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: *const c_void,
        value: *mut *mut c_void,
    ) -> AXError;
    fn _AXUIElementGetWindow(element: AXUIElementRef, window_id: *mut u32) -> AXError;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

// ============================================================================
// AXObserver Adapter
// ============================================================================

/// Adapter that receives window events and routes them to the [`EventProcessor`].
pub struct AXObserverAdapter {
    /// Reference to the event processor.
    processor: Arc<EventProcessor>,

    /// Generation this adapter belongs to (from `init::current_generation()`).
    generation: u64,

    /// Whether the adapter is active (receiving events).
    active: AtomicBool,
}

impl AXObserverAdapter {
    /// Creates a new adapter with the given event processor.
    #[must_use]
    #[allow(clippy::missing_const_for_fn)] // AtomicBool::new() is not const in struct context
    pub fn new(processor: Arc<EventProcessor>) -> Self {
        Self {
            processor,
            generation: crate::modules::tiling::init::current_generation(),
            active: AtomicBool::new(false),
        }
    }

    /// Activates the adapter to start processing events.
    pub fn activate(&self) { self.active.store(true, Ordering::SeqCst); }

    /// Deactivates the adapter to stop processing events.
    pub fn deactivate(&self) { self.active.store(false, Ordering::SeqCst); }

    /// Returns whether the adapter is currently active.
    #[must_use]
    pub fn is_active(&self) -> bool { self.active.load(Ordering::SeqCst) }

    /// Whether this adapter's callbacks may currently be forwarded.
    fn gate_open(&self) -> bool {
        self.active.load(Ordering::SeqCst)
            && self.generation == crate::modules::tiling::init::current_generation()
    }

    /// Deactivates the adapter and uninstalls it from the global callback
    /// dispatch slot. Idempotent.
    pub fn shutdown(&self) {
        self.deactivate();
        uninstall_adapter();
        tracing::debug!("AXObserverAdapter shut down");
    }

    /// Handles a window event from the `AXObserver` system.
    ///
    /// This is the main entry point called from the observer callback.
    pub fn handle_event(&self, event: WindowEvent) {
        if !self.gate_open() {
            tracing::trace!("Ignoring event {:?} - adapter not active", event.event_type);
            return;
        }

        // The AX element reference is passed as a usize in the event
        let ax_element = event.element as AXUIElementRef;
        match event.event_type {
            WindowEventType::Created => {
                self.handle_window_created(event.pid, event.identity, ax_element);
            }
            WindowEventType::Destroyed => {
                self.handle_window_destroyed(event.pid, event.identity, ax_element);
            }
            WindowEventType::Focused => {
                self.handle_window_focused(event.pid, event.identity, ax_element);
            }
            WindowEventType::Unfocused => {
                self.handle_window_unfocused(event.pid, event.identity, ax_element);
            }
            WindowEventType::Moved => {
                self.handle_window_moved(event.pid, event.identity, ax_element);
            }
            WindowEventType::Resized => {
                self.handle_window_resized(event.pid, event.identity, ax_element);
            }
            WindowEventType::Minimized => {
                self.handle_window_minimized(event.pid, event.identity, ax_element, true);
            }
            WindowEventType::Unminimized => {
                self.handle_window_minimized(event.pid, event.identity, ax_element, false);
            }
            WindowEventType::TitleChanged => {
                self.handle_title_changed(event.pid, event.identity, ax_element);
            }
            WindowEventType::AppActivated => {
                self.handle_app_activated(event.pid, event.identity);
            }
            WindowEventType::AppDeactivated => {
                // App deactivation is implicitly handled when another app activates
            }
            WindowEventType::AppHidden => {
                self.processor.on_app_hidden(event.identity, event.pid);
            }
            WindowEventType::AppShown => {
                self.processor.on_app_shown(event.identity, event.pid);
            }
        }
    }

    // ========================================================================
    // Event Handlers
    // ========================================================================

    fn handle_window_created(&self, pid: i32, identity: AppIdentity, ax_element: AXUIElementRef) {
        // Minimum size thresholds
        // Standard windows: 200x150 minimum
        // Dialogs: 400x300 minimum (real dialogs like preferences are larger;
        //          small dialogs are popups like date pickers, color pickers)
        const MIN_STANDARD_WIDTH: f64 = 200.0;
        const MIN_STANDARD_HEIGHT: f64 = 150.0;
        const MIN_DIALOG_WIDTH: f64 = 400.0;
        const MIN_DIALOG_HEIGHT: f64 = 300.0;

        // Extract window info from the AX element
        let Some(window_id) = get_window_id(ax_element) else {
            tracing::debug!("Window created event: could not get window ID");
            return;
        };

        // Get role and subrole for filtering
        let _role = get_element_role(ax_element);
        let subrole = get_window_subrole(ax_element);

        // Skip PiP (Picture-in-Picture) windows - they have subrole AXFloatingWindow
        if is_pip_window(subrole.as_deref()) {
            tracing::debug!(
                "Window created: id={window_id}, pid={pid} - skipping PiP window (subrole={subrole:?})"
            );
            return;
        }

        // Get window properties early so we can use them in filtering
        let title = get_window_title(ax_element).unwrap_or_default();
        let frame = get_window_frame(ax_element).unwrap_or_default();

        let (app_id, app_name) = get_app_info_for_pid(pid);
        if !should_tile_window_with_rules(
            &app_id,
            &app_name,
            &title,
            &crate::config::get_config().tiling.ignore,
        ) {
            return;
        }

        // Determine if window should be managed based on subrole and size
        // Use blacklist approach: only reject known popup/sheet subroles
        let should_manage = match subrole.as_deref() {
            // Explicitly reject popup-like subroles
            // Sheets and drawers are always attached to parent windows
            // AXUnknown subrole indicates popup/panel windows without standard controls
            // Examples: browser extension popups, toolbar popups, dropdown panels
            // These windows typically have no close/minimize/zoom buttons
            Some("AXSheet" | "AXDrawer" | "AXUnknown") => false,
            Some("AXDialog") => {
                // Dialogs - only accept large ones (preferences, settings)
                // Small dialogs are popups (date pickers, color pickers, alerts)
                frame.width >= MIN_DIALOG_WIDTH && frame.height >= MIN_DIALOG_HEIGHT
            }
            _ => {
                // Standard windows, no subrole, or custom subroles - accept if large enough
                // Many apps (like Ghostty) don't set subrole or use custom values
                frame.width >= MIN_STANDARD_WIDTH && frame.height >= MIN_STANDARD_HEIGHT
            }
        };

        if !should_manage {
            return;
        }

        // Get remaining window properties
        let is_minimized = get_window_minimized(ax_element).unwrap_or(false);
        let is_fullscreen = get_window_fullscreen(ax_element).unwrap_or(false);
        let minimum_size = get_window_minimum_size(ax_element);

        // Note: Tab detection is now handled in the window handler using the TabRegistry.
        // We pass tab_group_id=None and is_active_tab=true here; the handler will
        // check the TabRegistry and update accordingly.
        let info = WindowCreatedInfo {
            window_id,
            pid,
            identity: Some(identity),
            app_id,
            app_name,
            title,
            frame,
            is_minimized,
            is_fullscreen,
            minimum_size,
            tab_group_id: None,
            is_active_tab: true,
        };

        self.processor.on_window_created(info);
    }

    fn handle_window_destroyed(&self, pid: i32, identity: AppIdentity, ax_element: AXUIElementRef) {
        tracing::debug!(
            "tiling: handle_window_destroyed called for pid={pid}, element={ax_element:?}"
        );

        // First try to get window ID directly (might work if element is still valid)
        if let Some(window_id) = get_window_id(ax_element) {
            tracing::debug!("tiling: window destroyed - got window_id={window_id} from AX element");
            self.processor.on_window_destroyed(window_id, identity);
            return;
        }

        // Can't get window ID - check if this was actually a window element.
        // AXUIElementDestroyed fires for ALL UI elements, not just windows.
        // Tab elements, buttons, etc. also trigger this notification.
        //
        // Logic:
        // - If role IS "AXWindow" → definitely a window → fall back to PID detection
        // - If role is something else (e.g., "AXTabGroup") → not a window → ignore
        // - If role check fails (element invalid) → might be a destroyed window → fall back to PID detection
        let role = get_element_role(ax_element);

        if let Some(ref r) = role
            && r != "AXWindow"
        {
            // Definitely NOT a window (e.g., tab, button) - ignore
            tracing::trace!(
                "tiling: AXUIElementDestroyed for non-window element (role={r}, pid={pid}), ignoring"
            );
            return;
        }

        // Either it IS a window, or we couldn't determine (element invalid) - fall back to identity detection
        tracing::debug!(
            "tiling: window destroyed - role={role:?}, falling back to identity-based detection for pid={pid}"
        );
        self.processor.on_window_destroyed_for_identity(identity);
    }

    fn handle_window_focused(&self, pid: i32, identity: AppIdentity, ax_element: AXUIElementRef) {
        let Some(window_id) = get_window_id(ax_element) else {
            tracing::debug!("tiling: Window focused event: could not get window ID (pid={pid})");
            return;
        };

        tracing::debug!("tiling: AX focus event received - window_id={window_id}, pid={pid}");
        self.processor.on_window_focused(window_id, identity);
    }

    fn handle_window_unfocused(
        &self,
        _pid: i32,
        identity: AppIdentity,
        ax_element: AXUIElementRef,
    ) {
        let Some(window_id) = get_window_id(ax_element) else {
            return;
        };

        self.processor.on_window_unfocused(window_id, identity);
    }

    fn handle_window_moved(&self, _pid: i32, identity: AppIdentity, ax_element: AXUIElementRef) {
        let Some(window_id) = get_window_id(ax_element) else {
            return;
        };

        let Some(frame) = get_window_frame(ax_element) else {
            return;
        };

        self.handle_window_geometry(
            window_id,
            identity,
            frame,
            get_window_fullscreen(ax_element),
            true,
        );
    }

    fn handle_window_resized(&self, _pid: i32, identity: AppIdentity, ax_element: AXUIElementRef) {
        let Some(window_id) = get_window_id(ax_element) else {
            return;
        };

        let Some(frame) = get_window_frame(ax_element) else {
            return;
        };

        self.handle_window_geometry(
            window_id,
            identity,
            frame,
            get_window_fullscreen(ax_element),
            false,
        );
    }

    /// Reconciles the AX fullscreen attribute before forwarding a geometry event.
    fn handle_window_geometry(
        &self,
        window_id: u32,
        identity: AppIdentity,
        frame: Rect,
        fullscreen: Option<bool>,
        moved: bool,
    ) {
        if let Some(fullscreen) = fullscreen {
            self.processor.on_window_fullscreen_changed(window_id, identity, fullscreen);
        }

        if moved {
            self.processor.on_window_moved(window_id, identity, frame);
        } else {
            self.processor.on_window_resized(window_id, identity, frame);
        }
    }

    fn handle_window_minimized(
        &self,
        _pid: i32,
        identity: AppIdentity,
        ax_element: AXUIElementRef,
        minimized: bool,
    ) {
        let Some(window_id) = get_window_id(ax_element) else {
            return;
        };

        self.processor.on_window_minimized(window_id, identity, minimized);
    }

    fn handle_title_changed(&self, _pid: i32, identity: AppIdentity, ax_element: AXUIElementRef) {
        let Some(window_id) = get_window_id(ax_element) else {
            return;
        };

        let title = get_window_title(ax_element).unwrap_or_default();
        self.processor.on_window_title_changed(window_id, identity, title);
    }

    /// Handles app activation by querying the focused window and emitting a focus event.
    ///
    /// macOS doesn't reliably fire `AXFocusedWindowChanged` when switching between apps,
    /// so we query the focused window when an app is activated.
    fn handle_app_activated(&self, pid: i32, identity: AppIdentity) {
        tracing::trace!("App activated: pid={pid}, querying focused window...");

        // Query the focused window of this app
        if let Some(window_id) = get_focused_window_for_app(pid) {
            tracing::debug!("App activated: found focused window {window_id}");
            self.processor.on_window_focused(window_id, identity);
        } else {
            tracing::trace!("App activated: no focused window found for pid={pid}");
        }

        // Also notify app activation (for other purposes)
        self.processor.on_app_activated(identity, pid);
    }
}

// ============================================================================
// AX Attribute Helpers
// ============================================================================

/// Gets the [`CGWindowID`] for an [`AXUIElement`].
fn get_window_id(element: AXUIElementRef) -> Option<u32> {
    if element.is_null() {
        return None;
    }

    let mut window_id: u32 = 0;
    let result = unsafe { _AXUIElementGetWindow(element, &raw mut window_id) };

    if result == K_AX_ERROR_SUCCESS && window_id != 0 {
        Some(window_id)
    } else {
        None
    }
}

/// Gets the window title from an [`AXUIElement`].
fn get_window_title(element: AXUIElementRef) -> Option<String> { get_ax_string(element, "AXTitle") }

/// Gets the window subrole from an [`AXUIElement`].
///
/// Picture-in-Picture windows have subrole `AXFloatingWindow`.
fn get_window_subrole(element: AXUIElementRef) -> Option<String> {
    get_ax_string(element, "AXSubrole")
}

/// Gets the role of an [`AXUIElement`].
///
/// Windows have role `"AXWindow"`. Tab elements have role `"AXTabGroup"` or similar.
fn get_element_role(element: AXUIElementRef) -> Option<String> { get_ax_string(element, "AXRole") }

/// Gets a string AX attribute.
fn get_ax_string(element: AXUIElementRef, attr_name: &str) -> Option<String> {
    if element.is_null() {
        return None;
    }

    unsafe {
        use core_foundation::base::TCFType;
        use core_foundation::string::CFString;

        let attr = CFString::new(attr_name);
        let mut value: *mut c_void = std::ptr::null_mut();

        let result = AXUIElementCopyAttributeValue(
            element,
            attr.as_concrete_TypeRef().cast(),
            &raw mut value,
        );

        if result != K_AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }

        // Check if it's a CFString
        let cf_string = CFString::wrap_under_create_rule(value.cast());
        Some(cf_string.to_string())
    }
}

/// Gets the window frame from an [`AXUIElement`].
fn get_window_frame(element: AXUIElementRef) -> Option<Rect> {
    if element.is_null() {
        return None;
    }

    let position = get_ax_position(element)?;
    let size = get_ax_size(element)?;

    Some(Rect::new(position.0, position.1, size.0, size.1))
}

/// Gets the `AXPosition` attribute as (x, y).
fn get_ax_position(element: AXUIElementRef) -> Option<(f64, f64)> {
    unsafe {
        use core_foundation::base::TCFType;
        use core_foundation::string::CFString;

        let attr = CFString::new("AXPosition");
        let mut value: *mut c_void = std::ptr::null_mut();

        let result = AXUIElementCopyAttributeValue(
            element,
            attr.as_concrete_TypeRef().cast(),
            &raw mut value,
        );

        if result != K_AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }

        // AXPosition is an AXValue containing a CGPoint
        let point = ax_value_to_point(value)?;
        CFRelease(value.cast());
        Some(point)
    }
}

/// Gets the `AXSize` attribute as (width, height).
fn get_ax_size(element: AXUIElementRef) -> Option<(f64, f64)> {
    unsafe {
        use core_foundation::base::TCFType;
        use core_foundation::string::CFString;

        let attr = CFString::new("AXSize");
        let mut value: *mut c_void = std::ptr::null_mut();

        let result = AXUIElementCopyAttributeValue(
            element,
            attr.as_concrete_TypeRef().cast(),
            &raw mut value,
        );

        if result != K_AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }

        // AXSize is an AXValue containing a CGSize
        let size = ax_value_to_size(value)?;
        CFRelease(value.cast());
        Some(size)
    }
}

/// Gets the minimized state of a window.
fn get_window_minimized(element: AXUIElementRef) -> Option<bool> {
    get_ax_boolean(element, "AXMinimized")
}

/// Gets the fullscreen state of a window.
fn get_window_fullscreen(element: AXUIElementRef) -> Option<bool> {
    // AXFullScreen is the attribute for native fullscreen
    get_ax_boolean(element, "AXFullScreen")
}

/// Gets the minimum size of a window.
fn get_window_minimum_size(element: AXUIElementRef) -> Option<(f64, f64)> {
    if element.is_null() {
        return None;
    }

    unsafe {
        use core_foundation::base::TCFType;
        use core_foundation::string::CFString;

        let attr = CFString::new("AXMinimumSize");
        let mut value: *mut c_void = std::ptr::null_mut();

        let result = AXUIElementCopyAttributeValue(
            element,
            attr.as_concrete_TypeRef().cast(),
            &raw mut value,
        );

        if result != K_AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }

        // AXMinimumSize is an AXValue containing a CGSize
        let size = ax_value_to_size(value)?;
        CFRelease(value.cast());
        Some(size)
    }
}

/// Gets a boolean AX attribute.
fn get_ax_boolean(element: AXUIElementRef, attr_name: &str) -> Option<bool> {
    if element.is_null() {
        return None;
    }

    unsafe {
        use core_foundation::base::TCFType;
        use core_foundation::boolean::CFBoolean;
        use core_foundation::string::CFString;

        let attr = CFString::new(attr_name);
        let mut value: *mut c_void = std::ptr::null_mut();

        let result = AXUIElementCopyAttributeValue(
            element,
            attr.as_concrete_TypeRef().cast(),
            &raw mut value,
        );

        if result != K_AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }

        // CFBoolean → bool
        let cf_bool = CFBoolean::wrap_under_create_rule(value.cast());
        Some(cf_bool.into())
    }
}

// AXValue extraction helpers
#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

const K_AX_VALUE_CG_POINT_TYPE: i32 = 1;
const K_AX_VALUE_CG_SIZE_TYPE: i32 = 2;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXValueGetValue(value: *const c_void, value_type: i32, value_ptr: *mut c_void) -> bool;
}

fn ax_value_to_point(value: *mut c_void) -> Option<(f64, f64)> {
    if value.is_null() {
        return None;
    }

    let mut point = CGPoint { x: 0.0, y: 0.0 };
    let success =
        unsafe { AXValueGetValue(value.cast(), K_AX_VALUE_CG_POINT_TYPE, (&raw mut point).cast()) };

    if success {
        Some((point.x, point.y))
    } else {
        None
    }
}

fn ax_value_to_size(value: *mut c_void) -> Option<(f64, f64)> {
    if value.is_null() {
        return None;
    }

    let mut size = CGSize { width: 0.0, height: 0.0 };
    let success =
        unsafe { AXValueGetValue(value.cast(), K_AX_VALUE_CG_SIZE_TYPE, (&raw mut size).cast()) };

    if success {
        Some((size.width, size.height))
    } else {
        None
    }
}

/// Gets the focused window ID for an app by PID.
///
/// This queries the app's `AXFocusedWindow` attribute and returns its window ID.
fn get_focused_window_for_app(pid: i32) -> Option<u32> {
    unsafe {
        use core_foundation::base::TCFType;
        use core_foundation::string::CFString;

        // Create AXUIElement for the application
        #[link(name = "ApplicationServices", kind = "framework")]
        unsafe extern "C" {
            fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
        }

        let app_element = AXUIElementCreateApplication(pid);
        if app_element.is_null() {
            return None;
        }

        // Query AXFocusedWindow
        let attr = CFString::new("AXFocusedWindow");
        let mut value: *mut c_void = std::ptr::null_mut();

        let result = AXUIElementCopyAttributeValue(
            app_element,
            attr.as_concrete_TypeRef().cast(),
            &raw mut value,
        );

        CFRelease(app_element.cast());

        if result != K_AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }

        // value is the focused window AXUIElement
        let window_element: AXUIElementRef = value;
        let window_id = get_window_id(window_element);

        CFRelease(window_element.cast());

        window_id
    }
}

/// Gets app info (`bundle_id`, name) for a PID.
///
/// Apps that report `NSNotFound` from `processIdentifier` (e.g. Device Hub)
/// cannot be matched directly, so they are re-resolved from their executable
/// path. Without this, their windows would be tracked with no bundle ID, and
/// workspace rules could never match them.
fn get_app_info_for_pid(pid: i32) -> (String, String) {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    unsafe {
        let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        if workspace.is_null() {
            return (String::new(), String::new());
        }

        let apps: *mut Object = msg_send![workspace, runningApplications];
        if apps.is_null() {
            return (String::new(), String::new());
        }

        let count: usize = msg_send![apps, count];
        let mut unresolved: Vec<*mut Object> = Vec::new();
        for i in 0..count {
            let app: *mut Object = msg_send![apps, objectAtIndex: i];
            if app.is_null() {
                continue;
            }

            let app_pid: i32 = msg_send![app, processIdentifier];
            if app_pid == pid {
                let bundle_id = ns_string_to_rust(msg_send![app, bundleIdentifier]);
                let name = ns_string_to_rust(msg_send![app, localizedName]);
                return (bundle_id, name);
            }
            if app_pid <= 0 {
                unresolved.push(app);
            }
        }

        for app in unresolved {
            if process_id_for_app(app) == Some(pid) {
                let bundle_id = ns_string_to_rust(msg_send![app, bundleIdentifier]);
                let name = ns_string_to_rust(msg_send![app, localizedName]);
                return (bundle_id, name);
            }
        }

        (String::new(), String::new())
    }
}

/// Converts an `NSString` to a Rust String.
fn ns_string_to_rust(ns_string: *mut objc::runtime::Object) -> String {
    if ns_string.is_null() {
        return String::new();
    }

    unsafe {
        use objc::{msg_send, sel, sel_impl};

        let utf8: *const i8 = msg_send![ns_string, UTF8String];
        if utf8.is_null() {
            return String::new();
        }

        std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned()
    }
}

// ============================================================================
// Global Adapter Instance
// ============================================================================

/// Global adapter instance for use with the observer callback.
static ADAPTER: OnceLock<Arc<RwLock<Option<Arc<AXObserverAdapter>>>>> = OnceLock::new();

fn get_adapter_storage() -> &'static Arc<RwLock<Option<Arc<AXObserverAdapter>>>> {
    ADAPTER.get_or_init(|| Arc::new(RwLock::new(None)))
}

/// Installs the adapter as the global event handler.
///
/// This should be called once during initialization, after creating the adapter.
pub fn install_adapter(adapter: Arc<AXObserverAdapter>) {
    *get_adapter_storage().write() = Some(adapter);
}

/// Removes the installed adapter.
pub fn uninstall_adapter() { *get_adapter_storage().write() = None; }

/// Gets the installed adapter, if any.
#[must_use]
pub fn get_installed_adapter() -> Option<Arc<AXObserverAdapter>> {
    get_adapter_storage().read().clone()
}

/// The callback function to register with the `AXObserver` system.
///
/// This function retrieves the installed adapter and forwards the event to it.
pub fn adapter_callback(event: WindowEvent) {
    if let Some(adapter) = get_installed_adapter() {
        adapter.handle_event(event);
    } else {
        let event_type = &event.event_type;
        tracing::warn!("No adapter installed, dropping event {event_type:?}");
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::tiling::actor::{QueryResult, StateActor, WindowCreatedInfo};
    use crate::modules::tiling::identity::{AppIdentity, LaunchDateBits};

    /// Serializes tests that install into / clear the global adapter slot.
    static TEST_SLOT_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    fn test_identity() -> AppIdentity {
        AppIdentity {
            pid: 1000,
            launch_date: LaunchDateBits::from_time_interval_since_reference_date(1.0).unwrap(),
        }
    }

    fn window_info(identity: AppIdentity) -> WindowCreatedInfo {
        WindowCreatedInfo {
            window_id: 100,
            pid: identity.pid,
            identity: Some(identity),
            app_id: "com.example.Test".to_string(),
            app_name: "Test".to_string(),
            title: "Test window".to_string(),
            frame: Rect::new(0.0, 0.0, 800.0, 600.0),
            is_minimized: false,
            is_fullscreen: false,
            minimum_size: None,
            tab_group_id: None,
            is_active_tab: true,
        }
    }

    #[tokio::test]
    async fn test_geometry_callback_reconciles_fullscreen_state() {
        let (handle, _stopped) = StateActor::spawn();
        let processor = Arc::new(EventProcessor::new(handle.clone()));
        let adapter = AXObserverAdapter::new(Arc::clone(&processor));
        let identity = test_identity();
        processor.on_window_created(window_info(identity));

        adapter.handle_window_geometry(
            100,
            identity,
            Rect::new(0.0, 0.0, 1_000.0, 800.0),
            Some(true),
            false,
        );

        let QueryResult::Window(Some(window)) = handle.get_window(100).await.unwrap() else {
            panic!("expected the tracked window");
        };
        assert!(window.is_fullscreen);

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_adapter_creation() {
        let (handle, _stopped) = StateActor::spawn();
        let processor = Arc::new(EventProcessor::new(handle.clone()));
        let adapter = AXObserverAdapter::new(processor);

        assert!(!adapter.is_active());
        adapter.activate();
        assert!(adapter.is_active());
        adapter.deactivate();
        assert!(!adapter.is_active());

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_global_adapter_install() {
        let _guard = TEST_SLOT_LOCK.lock();
        let (handle, _stopped) = StateActor::spawn();
        let processor = Arc::new(EventProcessor::new(handle.clone()));
        let adapter = Arc::new(AXObserverAdapter::new(processor));

        install_adapter(Arc::clone(&adapter));
        assert!(get_installed_adapter().is_some());

        uninstall_adapter();
        assert!(get_installed_adapter().is_none());

        handle.shutdown().unwrap();
    }

    #[test]
    fn test_get_window_id_null_element() {
        let result = get_window_id(std::ptr::null_mut());
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_shutdown_deactivates_and_uninstalls() {
        let _guard = TEST_SLOT_LOCK.lock();
        let (handle, _stopped) = StateActor::spawn();
        let processor = Arc::new(EventProcessor::new(handle.clone()));
        let adapter = Arc::new(AXObserverAdapter::new(processor));

        install_adapter(Arc::clone(&adapter));
        adapter.activate();

        adapter.shutdown();
        assert!(!adapter.is_active());
        assert!(!adapter.gate_open());
        assert!(get_installed_adapter().is_none());

        handle.shutdown().unwrap();
    }

    #[test]
    fn test_get_window_title_null_element() {
        let result = get_window_title(std::ptr::null_mut());
        assert!(result.is_none());
    }

    #[test]
    fn test_get_window_frame_null_element() {
        let result = get_window_frame(std::ptr::null_mut());
        assert!(result.is_none());
    }

    #[test]
    fn test_ax_value_to_point_null() {
        let result = ax_value_to_point(std::ptr::null_mut());
        assert!(result.is_none());
    }

    #[test]
    fn test_ax_value_to_size_null() {
        let result = ax_value_to_size(std::ptr::null_mut());
        assert!(result.is_none());
    }
}
