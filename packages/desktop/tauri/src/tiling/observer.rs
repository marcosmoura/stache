//! AXObserver-based window event monitoring.
//!
//! This module provides an event-driven approach to monitoring window events
//! using macOS Accessibility API observers. Unlike polling, this approach
//! receives immediate notifications when windows are created, destroyed,
//! moved, resized, or when focus changes.
//!
//! For app launch/terminate events, we use NSWorkspace notifications.

// Suppress warnings for this module as we're using raw FFI and complex matching
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::assigning_clones)]
#![allow(clippy::significant_drop_tightening)]
#![allow(clippy::significant_drop_in_scrutinee)]

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use parking_lot::RwLock;
use tauri::{AppHandle, Emitter};

use crate::tiling::debouncer::{Debouncer, KeyDebouncer};
use crate::tiling::error::TilingError;
use crate::tiling::{try_get_manager, window};

// ============================================================================
// FFI Declarations
// ============================================================================

type AXUIElementRef = *mut c_void;
type AXObserverRef = *mut c_void;
type CFStringRef = *const c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopTimerRef = *mut c_void;

/// Callback type for AXObserver notifications.
type AXObserverCallback = unsafe extern "C" fn(
    observer: AXObserverRef,
    element: AXUIElementRef,
    notification: CFStringRef,
    user_data: *mut c_void,
);

/// Callback type for CFRunLoopTimer.
type CFRunLoopTimerCallback = unsafe extern "C" fn(timer: CFRunLoopTimerRef, info: *mut c_void);

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXObserverCreate(
        application: i32,
        callback: AXObserverCallback,
        observer_out: *mut AXObserverRef,
    ) -> i32;

    fn AXObserverAddNotification(
        observer: AXObserverRef,
        element: AXUIElementRef,
        notification: CFStringRef,
        ref_con: *mut c_void,
    ) -> i32;

    fn AXObserverRemoveNotification(
        observer: AXObserverRef,
        element: AXUIElementRef,
        notification: CFStringRef,
    ) -> i32;

    fn AXObserverGetRunLoopSource(observer: AXObserverRef) -> CFRunLoopSourceRef;

    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;

    fn CFRelease(cf: *mut c_void);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRun();
    fn CFRunLoopTimerCreate(
        allocator: *const c_void,
        fire_date: f64,
        interval: f64,
        flags: u64,
        order: i64,
        callout: CFRunLoopTimerCallback,
        context: *mut c_void,
    ) -> CFRunLoopTimerRef;
    fn CFRunLoopAddTimer(rl: CFRunLoopRef, timer: CFRunLoopTimerRef, mode: CFStringRef);
    fn CFAbsoluteTimeGetCurrent() -> f64;
}

/// The common modes constant for CFRunLoop.
fn cf_run_loop_common_modes() -> CFStringRef {
    use core_foundation::runloop::kCFRunLoopCommonModes;
    unsafe { kCFRunLoopCommonModes.cast() }
}

// AX error codes
const AX_ERROR_SUCCESS: i32 = 0;

// ============================================================================
// Thread-Safe Wrappers for Raw Pointers
// ============================================================================

/// A wrapper around a raw CFRunLoop pointer that is Send + Sync.
/// This is safe because CFRunLoop is thread-safe when accessed correctly.
#[derive(Debug, Clone, Copy)]
struct SendSyncRunLoop(CFRunLoopRef);

// SAFETY: CFRunLoop is designed to be thread-safe.
unsafe impl Send for SendSyncRunLoop {}
unsafe impl Sync for SendSyncRunLoop {}

// ============================================================================
// Notification Types
// ============================================================================

/// Accessibility notification names we want to observe.
pub mod notifications {
    pub const WINDOW_CREATED: &str = "AXWindowCreated";
    pub const FOCUSED_WINDOW_CHANGED: &str = "AXFocusedWindowChanged";
    pub const WINDOW_MOVED: &str = "AXWindowMoved";
    pub const WINDOW_RESIZED: &str = "AXWindowResized";
    pub const WINDOW_MINIATURIZED: &str = "AXWindowMiniaturized";
    pub const WINDOW_DEMINIATURIZED: &str = "AXWindowDeminiaturized";
    pub const APPLICATION_ACTIVATED: &str = "AXApplicationActivated";
    pub const APPLICATION_DEACTIVATED: &str = "AXApplicationDeactivated";
    pub const APPLICATION_HIDDEN: &str = "AXApplicationHidden";
    pub const APPLICATION_SHOWN: &str = "AXApplicationShown";
    pub const UI_ELEMENT_DESTROYED: &str = "AXUIElementDestroyed";
    pub const MAIN_WINDOW_CHANGED: &str = "AXMainWindowChanged";
}

/// All notifications we want to observe for each application.
const APP_NOTIFICATIONS: &[&str] = &[
    notifications::WINDOW_CREATED,
    notifications::FOCUSED_WINDOW_CHANGED,
    notifications::WINDOW_MOVED,
    notifications::WINDOW_RESIZED,
    notifications::WINDOW_MINIATURIZED,
    notifications::WINDOW_DEMINIATURIZED,
    notifications::UI_ELEMENT_DESTROYED,
    notifications::MAIN_WINDOW_CHANGED,
    notifications::APPLICATION_HIDDEN,
    notifications::APPLICATION_SHOWN,
    notifications::APPLICATION_ACTIVATED,
    notifications::APPLICATION_DEACTIVATED,
];

// ============================================================================
// Event Types for Tauri
// ============================================================================

/// Event types emitted to the frontend.
pub mod events {
    // Window events
    pub const WINDOW_CREATED: &str = "tiling:window-created";
    pub const WINDOW_DESTROYED: &str = "tiling:window-destroyed";
    pub const WINDOW_FOCUSED: &str = "tiling:window-focused";
    pub const WINDOW_MOVED: &str = "tiling:window-moved";
    pub const WINDOW_RESIZED: &str = "tiling:window-resized";

    // App events
    pub const APP_ACTIVATED: &str = "tiling:app-activated";
    pub const APP_DEACTIVATED: &str = "tiling:app-deactivated";

    // Workspace events
    pub const WORKSPACES_CHANGED: &str = "tiling:workspaces-changed";

    // Screen events
    pub const SCREEN_FOCUSED: &str = "tiling:screen-focused";
}

/// Payload for window events.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WindowEventPayload {
    pub window_id: u64,
    pub app_name: String,
    pub title: String,
}

/// Payload for window geometry events.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WindowGeometryPayload {
    pub window_id: u64,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Payload for screen focus events.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScreenFocusedPayload {
    /// The screen ID that is now focused.
    pub screen: String,
    /// Whether this is the main screen.
    pub is_main: bool,
    /// The previously focused screen ID (if any).
    pub previous_screen: Option<String>,
}

// ============================================================================
// Global State
// ============================================================================

/// Global observer manager instance.
static OBSERVER_MANAGER: OnceLock<RwLock<ObserverManager>> = OnceLock::new();

/// Timestamp of the last layout application for cooldown.
static LAST_LAYOUT_TIME: AtomicU64 = AtomicU64::new(0);

/// Base cooldown period in ms after layout application when animations are disabled.
const BASE_LAYOUT_COOLDOWN_MS: u64 = 150;

/// Extra buffer time added to animation duration for cooldown.
const ANIMATION_COOLDOWN_BUFFER_MS: u64 = 50;

/// Timestamp of the last workspace switch for cooldown.
static LAST_SWITCH_TIME: AtomicU64 = AtomicU64::new(0);

/// Cooldown period in ms after workspace switch.
const SWITCH_COOLDOWN_MS: u64 = 500;

/// Whether the observer system is running.
static IS_RUNNING: AtomicBool = AtomicBool::new(false);

/// Gets the global observer manager.
fn get_observer_manager() -> &'static RwLock<ObserverManager> {
    OBSERVER_MANAGER.get_or_init(|| RwLock::new(ObserverManager::new()))
}

/// Checks if we're in the layout cooldown period.
/// The cooldown duration is based on the animation duration from config,
/// or falls back to the base cooldown if animations are disabled.
#[must_use]
pub fn is_in_layout_cooldown() -> bool {
    let last = LAST_LAYOUT_TIME.load(Ordering::SeqCst);
    if last == 0 {
        return false;
    }

    // Get cooldown duration: animation duration + buffer, or base cooldown if no animation
    let animation_duration = crate::tiling::animation::get_duration_ms();
    let cooldown = if animation_duration > 0 {
        animation_duration + ANIMATION_COOLDOWN_BUFFER_MS
    } else {
        BASE_LAYOUT_COOLDOWN_MS
    };

    let now = current_time_ms();
    now.saturating_sub(last) < cooldown
}

/// Marks that a layout was just applied.
pub fn mark_layout_applied() { LAST_LAYOUT_TIME.store(current_time_ms(), Ordering::SeqCst); }

/// Checks if we're in the switch cooldown period.
#[must_use]
pub fn is_in_switch_cooldown() -> bool {
    let last = LAST_SWITCH_TIME.load(Ordering::SeqCst);
    if last == 0 {
        return false;
    }
    let now = current_time_ms();
    now.saturating_sub(last) < SWITCH_COOLDOWN_MS
}

/// Marks that a workspace switch was just completed.
pub fn mark_switch_completed() { LAST_SWITCH_TIME.store(current_time_ms(), Ordering::SeqCst); }

/// Gets current time in milliseconds.
fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ============================================================================
// Pending Resize Tracking
// ============================================================================

/// How long (ms) the window must be stable before applying resize.
const RESIZE_SETTLE_TIME_MS: u64 = 200;

/// How long (ms) the window must be stable before snapping back after move.
const MOVE_SETTLE_TIME_MS: u64 = 100;

/// Data stored for pending resize operations.
#[derive(Debug, Clone)]
struct ResizeData {
    width: u32,
    height: u32,
}

// ============================================================================
// Thread-Safe Wrappers for Observer Pointers
// ============================================================================

/// Wrapper around AXObserverRef that is Send + Sync.
/// SAFETY: AXObserver is thread-safe when accessed through proper locking.
#[derive(Debug)]
struct SendSyncObserver(AXObserverRef);

unsafe impl Send for SendSyncObserver {}
unsafe impl Sync for SendSyncObserver {}

/// Wrapper around AXUIElementRef that is Send + Sync.
/// SAFETY: AXUIElement is thread-safe when accessed through proper locking.
#[derive(Debug)]
struct SendSyncElement(AXUIElementRef);

unsafe impl Send for SendSyncElement {}
unsafe impl Sync for SendSyncElement {}

// ============================================================================
// Observer Manager
// ============================================================================

/// Per-application observer data.
struct AppObserver {
    /// The AXObserver reference.
    observer: SendSyncObserver,
    /// The application element.
    app_element: SendSyncElement,
}

impl Drop for AppObserver {
    fn drop(&mut self) {
        // Remove notifications and release resources
        for notification in APP_NOTIFICATIONS {
            let notif_cf = CFString::new(notification);
            unsafe {
                AXObserverRemoveNotification(
                    self.observer.0,
                    self.app_element.0,
                    notif_cf.as_concrete_TypeRef().cast(),
                );
            }
        }
        if !self.observer.0.is_null() {
            unsafe { CFRelease(self.observer.0.cast()) };
        }
        if !self.app_element.0.is_null() {
            unsafe { CFRelease(self.app_element.0.cast()) };
        }
    }
}

/// Manages AXObservers for all running applications.
pub struct ObserverManager {
    /// App handle for emitting events.
    app_handle: Option<AppHandle>,
    /// Observers per PID.
    observers: HashMap<i32, AppObserver>,
    /// Known window IDs for tracking creation/destruction.
    known_windows: HashSet<u64>,
    /// Pending resize operations waiting to settle (window_id -> resize data).
    pending_resizes: Debouncer<u64, ResizeData>,
    /// Pending move operations waiting to settle (window_id only).
    pending_moves: KeyDebouncer<u64>,
    /// Run loop reference for the observer thread.
    run_loop: Option<SendSyncRunLoop>,
}

impl ObserverManager {
    /// Creates a new observer manager.
    #[must_use]
    pub fn new() -> Self {
        Self {
            app_handle: None,
            observers: HashMap::new(),
            known_windows: HashSet::new(),
            pending_resizes: Debouncer::new(Duration::from_millis(RESIZE_SETTLE_TIME_MS)),
            pending_moves: Debouncer::new(Duration::from_millis(MOVE_SETTLE_TIME_MS)),
            run_loop: None,
        }
    }

    /// Sets the app handle for event emission.
    pub fn set_app_handle(&mut self, handle: AppHandle) { self.app_handle = Some(handle); }

    /// Discovers all running applications and sets up observers.
    pub fn discover_applications(&mut self) -> Result<(), TilingError> {
        // First, get visible windows to track
        let windows = window::get_all_windows()?;

        // Track known windows
        self.known_windows = windows.iter().map(|w| w.id).collect();

        // Only observe apps that have visible windows - this is faster and avoids
        // wasting time on system processes that don't support accessibility
        let pids: HashSet<i32> = windows.iter().map(|w| w.pid).collect();

        // Create observers for each application (silently ignore failures)
        for pid in pids {
            if !self.observers.contains_key(&pid) {
                // Don't log errors - many system processes don't support AX observers
                let _ = self.add_observer_for_pid(pid);
            }
        }

        Ok(())
    }

    /// Adds an observer for a specific application PID.
    fn add_observer_for_pid(&mut self, pid: i32) -> Result<(), TilingError> {
        let mut observer: AXObserverRef = ptr::null_mut();

        // Create the observer
        let result = unsafe { AXObserverCreate(pid, ax_observer_callback, &raw mut observer) };

        if result != AX_ERROR_SUCCESS || observer.is_null() {
            return Err(TilingError::ObserverFailed(format!(
                "Failed to create observer for PID {pid}, error: {result}"
            )));
        }

        // Create the application element
        let app_element = unsafe { AXUIElementCreateApplication(pid) };
        if app_element.is_null() {
            unsafe { CFRelease(observer.cast()) };
            return Err(TilingError::ObserverFailed(format!(
                "Failed to create app element for PID {pid}"
            )));
        }

        // Add notifications
        let mut app_activated_ok = false;
        for notification in APP_NOTIFICATIONS {
            let notif_cf = CFString::new(notification);
            let add_result = unsafe {
                AXObserverAddNotification(
                    observer,
                    app_element,
                    notif_cf.as_concrete_TypeRef().cast(),
                    ptr::null_mut(),
                )
            };
            if add_result == AX_ERROR_SUCCESS
                && *notification == notifications::APPLICATION_ACTIVATED
            {
                app_activated_ok = true;
            }
        }
        let _ = app_activated_ok; // Suppress unused warning

        // Get the run loop source and add it to the run loop
        let source = unsafe { AXObserverGetRunLoopSource(observer) };
        if !source.is_null()
            && let Some(run_loop) = self.run_loop
        {
            unsafe {
                CFRunLoopAddSource(run_loop.0, source, cf_run_loop_common_modes());
            }
        }

        self.observers.insert(pid, AppObserver {
            observer: SendSyncObserver(observer),
            app_element: SendSyncElement(app_element),
        });

        Ok(())
    }

    /// Removes the observer for a specific PID.
    pub fn remove_observer_for_pid(&mut self, pid: i32) {
        use crate::tiling::window::invalidate_app;

        self.observers.remove(&pid);

        // Invalidate cached AX elements for this app
        invalidate_app(pid);
    }

    /// Syncs window list to detect new/destroyed windows.
    /// Called when we receive an event but couldn't identify the specific window.
    fn sync_windows(&mut self) {
        use crate::tiling::window::invalidate_window_list_cache;

        // Invalidate the cache to get fresh window list from the system
        invalidate_window_list_cache();

        if let Ok(windows) = window::get_all_windows() {
            let current_window_ids: HashSet<u64> = windows.iter().map(|w| w.id).collect();

            // Detect new windows
            for window in &windows {
                if !self.known_windows.contains(&window.id) {
                    self.handle_window_created(window.id);
                }
            }

            // Detect destroyed windows
            let destroyed: Vec<u64> = self
                .known_windows
                .iter()
                .filter(|id| !current_window_ids.contains(id))
                .copied()
                .collect();

            for window_id in destroyed {
                self.handle_window_destroyed(window_id);
            }

            self.known_windows = current_window_ids;
        }
    }

    /// Processes pending resizes that have settled.
    pub fn process_pending_resizes(&mut self) {
        use crate::tiling::command_queue::{TilingCommand, flush_commands, queue_command};

        // Drain all settled resize operations from the debouncer
        let settled = self.pending_resizes.drain_settled();

        // Queue all settled resizes
        if !is_in_switch_cooldown() {
            for (window_id, resize_data) in settled {
                queue_command(TilingCommand::WindowResized {
                    window_id,
                    width: resize_data.width,
                    height: resize_data.height,
                });
            }
            // Flush all queued commands in a single batch
            flush_commands();
        }

        // If there are still pending resizes that haven't settled, schedule another timer
        if !self.pending_resizes.is_empty()
            && let Some(run_loop) = self.run_loop
        {
            schedule_resize_timer(run_loop.0);
        }
    }

    /// Handles a window created notification.
    fn handle_window_created(&mut self, window_id: u64) {
        use crate::tiling::command_queue::{TilingCommand, flush_commands, queue_command};
        use crate::tiling::window::invalidate_window_list_cache;

        // Invalidate window list cache since a new window was created
        invalidate_window_list_cache();

        // Try to get window info and potentially add observer for new app
        if let Ok(window) = window::get_window_by_id(window_id) {
            // If this is from an app we don't have an observer for, add one
            if !self.observers.contains_key(&window.pid)
                && let Err(e) = self.add_observer_for_pid(window.pid)
            {
                eprintln!(
                    "barba: failed to add observer for new app PID {}: {e}",
                    window.pid
                );
            }

            if let Some(ref app_handle) = self.app_handle {
                let _ = app_handle.emit(events::WINDOW_CREATED, WindowEventPayload {
                    window_id,
                    app_name: window.app_name.clone(),
                    title: window.title,
                });
            }
        }

        // Queue command for batched processing
        queue_command(TilingCommand::WindowCreated { window_id });
        flush_commands();

        self.known_windows.insert(window_id);
    }

    /// Handles a window destroyed notification.
    fn handle_window_destroyed(&mut self, window_id: u64) {
        use crate::tiling::command_queue::{TilingCommand, flush_commands, queue_command};
        use crate::tiling::window::{invalidate_window, invalidate_window_list_cache};

        // Invalidate window list cache since a window was destroyed
        invalidate_window_list_cache();

        if let Some(ref app_handle) = self.app_handle {
            let _ = app_handle.emit(events::WINDOW_DESTROYED, WindowEventPayload {
                window_id,
                app_name: String::new(),
                title: String::new(),
            });
        }

        // Queue command for batched processing
        queue_command(TilingCommand::WindowDestroyed { window_id });
        flush_commands();

        self.known_windows.remove(&window_id);

        // Remove pending resizes/moves for this window
        self.pending_resizes.remove(&window_id);
        self.pending_moves.remove(&window_id);

        // Invalidate the AX element cache for this window
        invalidate_window(window_id);
    }

    /// Handles a window moved notification.
    ///
    /// For tiled windows, this snaps them back to their assigned position.
    /// Uses debouncing to avoid multiple snap-backs during a drag.
    fn handle_window_moved(&mut self, element: AXUIElementRef) {
        // Skip if we're in the layout cooldown (this move was caused by layout application)
        if is_in_layout_cooldown() {
            return;
        }

        // Try to identify the window
        let Some((window_id, _, _)) = get_window_info_from_element(element) else {
            return;
        };

        // Debounce moves to avoid flickering during drag
        let had_pending = !self.pending_moves.is_empty();
        let is_new_move = !self.pending_moves.contains(&window_id);

        // When a window starts being moved, focus should follow to it
        // This happens on the first move notification for this window
        if is_new_move {
            // Emit window moved event to frontend
            if let Some(ref app_handle) = self.app_handle {
                let _ = app_handle.emit(events::WINDOW_MOVED, WindowEventPayload {
                    window_id,
                    app_name: String::new(),
                    title: String::new(),
                });
            }

            self.handle_focus_changed(window_id);
        }

        // Update or insert pending move (touch updates timestamp automatically)
        self.pending_moves.touch(window_id);

        // Schedule a timer to process the move after settle time
        if !had_pending && let Some(run_loop) = self.run_loop {
            schedule_move_timer(run_loop.0);
        }
    }

    /// Processes pending moves that have settled.
    pub fn process_pending_moves(&mut self) {
        use crate::tiling::command_queue::{TilingCommand, flush_commands, queue_command};

        // Drain all settled move operations from the debouncer
        let settled = self.pending_moves.drain_settled_keys();

        // Queue all settled moves
        for window_id in settled {
            queue_command(TilingCommand::WindowMoved { window_id });
        }
        // Flush all queued commands in a single batch
        flush_commands();

        // If there are still pending moves, schedule another timer
        if !self.pending_moves.is_empty()
            && let Some(run_loop) = self.run_loop
        {
            schedule_move_timer(run_loop.0);
        }
    }

    /// Handles a window resized notification.
    fn handle_window_resized(&mut self, window_id: u64, width: u32, height: u32) {
        if let Some(ref app_handle) = self.app_handle {
            let _ = app_handle.emit(events::WINDOW_RESIZED, WindowGeometryPayload {
                window_id,
                x: 0,
                y: 0,
                width,
                height,
            });
        }

        // If not in layout cooldown, this is a user-initiated resize
        if !is_in_layout_cooldown() {
            let had_pending = !self.pending_resizes.is_empty();

            // Update or insert pending resize (update method handles both cases)
            self.pending_resizes.update(window_id, ResizeData { width, height });

            // Schedule a timer to process the resize after settle time
            // Only schedule if we didn't already have pending resizes (to avoid duplicate timers)
            if !had_pending && let Some(run_loop) = self.run_loop {
                schedule_resize_timer(run_loop.0);
            }
        }
    }

    /// Handles a focus change notification.
    fn handle_focus_changed(&self, window_id: u64) {
        if let Some(ref app_handle) = self.app_handle
            && let Ok(window) = window::get_window_by_id(window_id)
        {
            let _ = app_handle.emit(events::WINDOW_FOCUSED, WindowEventPayload {
                window_id,
                app_name: window.app_name.clone(),
                title: window.title,
            });
        }

        // Handle workspace switching based on focus if not in cooldown
        if !is_in_switch_cooldown()
            && let Some(manager) = try_get_manager()
        {
            let guard = manager.read();
            let state = guard.workspace_manager.state();

            // Find which workspace this window belongs to
            for ws in &state.workspaces {
                if ws.windows.contains(&window_id) {
                    let ws_name = ws.name.clone();
                    let current_global_focus = state.focused_workspace.as_deref();

                    // Check if this workspace is already focused on its screen
                    if state.focused_workspace_per_screen.get(&ws.screen) != Some(&ws.name) {
                        // Need to switch workspaces on this screen
                        drop(guard);
                        if let Some(manager) = try_get_manager() {
                            // Pass the window ID so we focus the right window after switch
                            if let Err(e) =
                                manager.write().switch_workspace_focusing(&ws_name, Some(window_id))
                            {
                                eprintln!("barba: failed to switch to workspace {ws_name}: {e}");
                            }
                        }
                    } else if current_global_focus != Some(ws_name.as_str()) {
                        // Workspace is already focused on its screen, but the global focus
                        // is on a different workspace (e.g., user clicked on a window on
                        // secondary screen). Update global focus so commands target the
                        // correct workspace.
                        let screen_id = ws.screen.clone();
                        let is_main = state.screens.iter().any(|s| s.id == screen_id && s.is_main);
                        let previous_screen = current_global_focus.and_then(|prev_ws| {
                            state.get_workspace(prev_ws).map(|w| w.screen.clone())
                        });
                        drop(guard);
                        if let Some(manager) = try_get_manager() {
                            let mut guard = manager.write();

                            guard.workspace_manager.state_mut().focused_workspace = Some(ws_name);

                            // Emit workspace focused event
                            guard.emit_workspaces_changed();

                            // Emit screen focused event if screen changed
                            if previous_screen.as_deref() != Some(&screen_id) {
                                guard.emit_screen_focused(
                                    &screen_id,
                                    is_main,
                                    previous_screen.as_deref(),
                                );
                            }
                        }
                    }
                    break;
                }
            }
        }
    }

    /// Handles an application activated notification (e.g., via Cmd+Tab).
    ///
    /// When an app is activated, we need to find its focused window and
    /// potentially switch workspaces.
    fn handle_app_activated(&self, element: AXUIElementRef) {
        use crate::tiling::accessibility::AccessibilityElement;

        // Wrap the element safely - use ManuallyDrop since we don't own it
        let app_element =
            std::mem::ManuallyDrop::new(unsafe { AccessibilityElement::from_raw(element) });

        // Get the PID of the activated app (used for error messages if needed)
        let app_pid = match app_element.pid() {
            Ok(pid) => pid,
            Err(e) => {
                eprintln!("barba: APP_ACTIVATED but couldn't get PID: {e:?}");
                return;
            }
        };

        // Emit app activated event to frontend
        if let Some(ref app_handle) = self.app_handle {
            let _ = app_handle.emit(events::APP_ACTIVATED, WindowEventPayload {
                window_id: 0,
                app_name: format!("pid:{app_pid}"),
                title: String::new(),
            });
        }

        // APP_ACTIVATED received for this PID

        // Get the focused window of this app
        let focused_window = match app_element.get_focused_window() {
            Ok(w) => w,
            Err(e) => {
                eprintln!("barba: APP_ACTIVATED but couldn't get focused window: {e:?}");
                return;
            }
        };

        // Get the window frame to identify it
        let Ok(frame) = focused_window.get_frame() else {
            eprintln!("barba: APP_ACTIVATED but couldn't get window frame");
            return;
        };

        // Find the window ID by matching frame
        let Ok(windows) = window::get_all_windows() else {
            return;
        };

        let matching_window =
            window::find_window_by_frame(&windows, &frame, window::DEFAULT_FRAME_TOLERANCE)
                .cloned();

        if let Some(window) = matching_window {
            let window_id = window.id;
            // Found matching window for activated app

            // Ensure the window is tracked in the tiling manager
            // This is important for windows from apps that were hidden at startup
            if let Some(manager) = try_get_manager() {
                let mut guard = manager.write();

                // Skip windows that match ignore rules (higher priority than workspace rules)
                if guard.should_ignore_window(&window) {
                    return;
                }

                // Check if this window is already in a workspace
                let is_tracked = guard
                    .workspace_manager
                    .state()
                    .workspaces
                    .iter()
                    .any(|ws| ws.windows.contains(&window_id));

                if !is_tracked {
                    // Find workspace for this window based on rules
                    if let Some(workspace_name) = guard.find_workspace_for_window(&window) {
                        // Adding newly visible window to workspace

                        // Add to state with workspace assignment
                        let mut window = window.clone();
                        window.workspace = workspace_name.clone();
                        guard.workspace_manager.state_mut().windows.insert(window_id, window);

                        // Add to workspace
                        if let Some(ws) =
                            guard.workspace_manager.state_mut().get_workspace_mut(&workspace_name)
                            && !ws.windows.contains(&window_id)
                        {
                            ws.windows.push(window_id);
                        }
                    }
                }
            }

            self.handle_focus_changed(window_id);
        } else {
            // Couldn't find matching window for activated app
        }
    }

    /// Handles an app deactivation notification.
    ///
    /// When an app is deactivated (loses focus to another app), we emit an event
    /// to the frontend.
    fn handle_app_deactivated(&self, element: AXUIElementRef) {
        use crate::tiling::accessibility::AccessibilityElement;

        // Wrap the element safely - use ManuallyDrop since we don't own it
        let app_element =
            std::mem::ManuallyDrop::new(unsafe { AccessibilityElement::from_raw(element) });

        // Get the PID of the deactivated app
        let Ok(app_pid) = app_element.pid() else {
            return;
        };

        // Emit app deactivated event to frontend
        if let Some(ref app_handle) = self.app_handle {
            let _ = app_handle.emit(events::APP_DEACTIVATED, WindowEventPayload {
                window_id: 0,
                app_name: format!("pid:{app_pid}"),
                title: String::new(),
            });
        }
    }

    /// Handles an app activation by PID (called from NSWorkspace notification).
    /// This finds the focused window of the app and triggers focus handling.
    pub fn handle_app_activated_by_pid(&mut self, pid: i32) {
        use crate::tiling::accessibility::AccessibilityElement;

        // Ensure we have an observer for this app - this is important for apps
        // that were hidden/minimized at startup and are now being activated
        let was_new_observer = !self.observers.contains_key(&pid);
        if was_new_observer {
            let _ = self.add_observer_for_pid(pid);
            // Also sync windows since we might have missed window creation events
            // from this app before we had an observer
            self.sync_windows();
        }

        // First try to get the focused window via accessibility API
        // This works even for windows on other virtual desktops
        let app_element = AccessibilityElement::application(pid);

        let Ok(focused_window) = app_element.get_focused_window() else {
            // App activated but no focused window - this is normal for some apps
            return;
        };

        let Ok(frame) = focused_window.get_frame() else {
            // Couldn't get window frame - skip
            return;
        };

        // Try to find this window in our known windows (may be on current space)
        let windows = window::get_all_windows().unwrap_or_default();

        let matching_window = window::find_window_by_frame_and_pid(
            &windows,
            &frame,
            pid,
            window::DEFAULT_FRAME_TOLERANCE,
        );

        if let Some(window) = matching_window {
            let window_id = window.id;
            // Found window in window list
            self.ensure_window_tracked(window_id, window.clone());
            self.handle_focus_changed(window_id);
        } else {
            // Window not in CGWindowList (probably on another virtual desktop)
            // Check if we have any tracked window for this PID
            if let Some(manager) = try_get_manager() {
                let guard = manager.read();
                let tracked_window = guard
                    .workspace_manager
                    .state()
                    .windows
                    .values()
                    .find(|w| w.pid == pid)
                    .cloned();
                drop(guard);

                if let Some(window) = tracked_window {
                    // Found window from tracked state
                    self.handle_focus_changed(window.id);
                } else {
                    // Window not tracked yet and not in CGWindowList
                    // This happens when the app is on a different virtual desktop
                    // Try to create a window entry from AX data
                    self.create_window_from_ax_and_focus(pid, &app_element, &frame);
                }
            }
        }
    }

    /// Creates a window entry from accessibility data and focuses it.
    /// This is used when a window isn't visible in CGWindowList (e.g., on another virtual desktop).
    fn create_window_from_ax_and_focus(
        &self,
        pid: i32,
        app_element: &crate::tiling::accessibility::AccessibilityElement,
        frame: &crate::tiling::state::WindowFrame,
    ) {
        // Get app name from accessibility
        let app_name = app_element
            .get_string_attribute("AXTitle")
            .unwrap_or_else(|_| "Unknown".to_string());

        // Get bundle ID if possible
        let bundle_id = window::get_bundle_id_for_pid(pid);

        // Generate a synthetic window ID based on PID and frame
        // This won't match the real CGWindowID, but it's unique enough
        let synthetic_id = ((pid as u64) << 32)
            | (u64::from(frame.x.unsigned_abs()) << 16)
            | u64::from(frame.y.unsigned_abs());

        // Creating synthetic window from AX data

        // Create a ManagedWindow
        let window = crate::tiling::state::ManagedWindow {
            id: synthetic_id,
            title: app_name.clone(),
            app_name,
            bundle_id,
            class: None,
            pid,
            workspace: String::new(), // Will be set by find_workspace_for_window
            is_floating: false,
            is_minimized: false,
            is_fullscreen: false,
            is_hidden: false,
            frame: crate::tiling::state::WindowFrame {
                x: frame.x,
                y: frame.y,
                width: frame.width,
                height: frame.height,
            },
        };

        // Add to tiling manager and find workspace
        if let Some(manager) = try_get_manager() {
            let mut guard = manager.write();

            // Skip windows that match ignore rules (higher priority than workspace rules)
            if guard.should_ignore_window(&window) {
                return;
            }

            if let Some(workspace_name) = guard.find_workspace_for_window(&window) {
                // Adding synthetic window to workspace

                let mut window = window;
                window.workspace = workspace_name.clone();

                // Add to state
                guard.workspace_manager.state_mut().windows.insert(synthetic_id, window);

                // Add to workspace
                if let Some(ws) =
                    guard.workspace_manager.state_mut().get_workspace_mut(&workspace_name)
                    && !ws.windows.contains(&synthetic_id)
                {
                    ws.windows.push(synthetic_id);
                }
            }
        }

        // Now focus this window
        self.handle_focus_changed(synthetic_id);
    }

    /// Ensures a window is tracked in the tiling manager.
    #[allow(clippy::unused_self)]
    fn ensure_window_tracked(&self, window_id: u64, window: crate::tiling::state::ManagedWindow) {
        if let Some(manager) = try_get_manager() {
            let mut guard = manager.write();

            // Skip windows that match ignore rules (higher priority than workspace rules)
            if guard.should_ignore_window(&window) {
                return;
            }

            // Check if this window is already in a workspace
            let is_tracked = guard
                .workspace_manager
                .state()
                .workspaces
                .iter()
                .any(|ws| ws.windows.contains(&window_id));

            if !is_tracked {
                // Find workspace for this window based on rules
                if let Some(workspace_name) = guard.find_workspace_for_window(&window) {
                    // Add to state with workspace assignment
                    let mut window = window;
                    window.workspace = workspace_name.clone();
                    guard.workspace_manager.state_mut().windows.insert(window_id, window);

                    // Add to workspace
                    if let Some(ws) =
                        guard.workspace_manager.state_mut().get_workspace_mut(&workspace_name)
                        && !ws.windows.contains(&window_id)
                    {
                        ws.windows.push(window_id);
                    }

                    // Apply layout to this workspace if it's focused
                    let is_focused =
                        guard.workspace_manager.state().get_workspace(&workspace_name).is_some_and(
                            |ws| {
                                guard
                                    .workspace_manager
                                    .state()
                                    .focused_workspace_per_screen
                                    .get(&ws.screen)
                                    == Some(&workspace_name)
                            },
                        );

                    if is_focused {
                        let _ = guard.apply_layout(&workspace_name);
                    }
                }
            }
        }
    }

    /// Sets the run loop reference.
    pub fn set_run_loop(&mut self, run_loop: CFRunLoopRef) {
        self.run_loop = Some(SendSyncRunLoop(run_loop));

        // Add existing observers to the run loop
        for app_observer in self.observers.values() {
            let source = unsafe { AXObserverGetRunLoopSource(app_observer.observer.0) };
            if !source.is_null() {
                unsafe {
                    CFRunLoopAddSource(run_loop, source, cf_run_loop_common_modes());
                }
            }
        }
    }
}

impl Default for ObserverManager {
    fn default() -> Self { Self::new() }
}

// ============================================================================
// AXObserver Callback
// ============================================================================

/// The callback function called by the Accessibility API when events occur.
///
/// # Safety
///
/// This function is called by the Accessibility framework and must only be
/// invoked by the system with valid AX references.
unsafe extern "C" fn ax_observer_callback(
    _observer: AXObserverRef,
    element: AXUIElementRef,
    notification: CFStringRef,
    _user_data: *mut c_void,
) {
    // Convert notification to Rust string
    let notif_cf: CFString = unsafe { CFString::wrap_under_get_rule(notification.cast()) };
    let notif_str = notif_cf.to_string();

    // Get window info from the element if possible
    let window_info = get_window_info_from_element(element);

    let manager = get_observer_manager();
    let mut guard = manager.write();

    match notif_str.as_str() {
        notifications::WINDOW_CREATED => {
            if let Some((window_id, _, _)) = window_info {
                guard.handle_window_created(window_id);
            } else {
                // New window but couldn't get ID - sync window list
                guard.sync_windows();

                // Schedule delayed syncs because some apps (Electron/VSCode) take time
                // for new windows to appear in CGWindowListCopyWindowInfo
                if let Some(run_loop) = guard.run_loop {
                    schedule_sync_timer(run_loop.0, 50); // Try after 50ms
                    schedule_sync_timer(run_loop.0, 150); // Try after 150ms
                    schedule_sync_timer(run_loop.0, 300); // Try after 300ms
                }
            }
        }
        notifications::UI_ELEMENT_DESTROYED => {
            if let Some((window_id, _, _)) = window_info {
                guard.handle_window_destroyed(window_id);
            } else {
                // Element destroyed but couldn't identify - sync window list
                // The window may still be in the system list briefly, so schedule delayed syncs
                guard.sync_windows();

                // Schedule delayed syncs because macOS may not have removed the window yet
                if let Some(run_loop) = guard.run_loop {
                    schedule_sync_timer(run_loop.0, 50); // Try after 50ms
                    schedule_sync_timer(run_loop.0, 150); // Try after 150ms
                }
            }
        }
        notifications::WINDOW_MOVED => {
            guard.handle_window_moved(element);
        }
        notifications::WINDOW_RESIZED => {
            if let Some((window_id, width, height)) = window_info {
                guard.handle_window_resized(window_id, width, height);
            }
        }
        notifications::FOCUSED_WINDOW_CHANGED | notifications::MAIN_WINDOW_CHANGED => {
            if let Some((window_id, _, _)) = window_info {
                guard.handle_focus_changed(window_id);
            }
        }
        notifications::WINDOW_MINIATURIZED | notifications::WINDOW_DEMINIATURIZED => {
            // Window minimized/restored - sync window list for visibility changes
            guard.sync_windows();
        }
        notifications::APPLICATION_HIDDEN | notifications::APPLICATION_SHOWN => {
            // App hidden/shown - sync window list for visibility changes
            guard.sync_windows();
        }
        notifications::APPLICATION_ACTIVATED => {
            // App was activated (e.g., via Cmd+Tab)
            // Get the focused window of this app and trigger focus handling
            // APPLICATION_ACTIVATED received from AX observer
            guard.handle_app_activated(element);
        }
        notifications::APPLICATION_DEACTIVATED => {
            // App was deactivated (lost focus to another app)
            guard.handle_app_deactivated(element);
        }
        _ => {}
    }
}

/// Tries to get window info from an AX element.
///
/// # Safety
/// The `element` is a borrowed reference from the Accessibility framework.
/// We must NOT call CFRelease on it (or drop an AccessibilityElement wrapping it).
fn get_window_info_from_element(element: AXUIElementRef) -> Option<(u64, u32, u32)> {
    use crate::tiling::accessibility::AccessibilityElement;

    if element.is_null() {
        return None;
    }

    // SAFETY: Create a wrapper but use ManuallyDrop to ensure we never accidentally
    // drop and release the borrowed element. This is critical because the element
    // is owned by the Accessibility framework, not us.
    let ax_element =
        std::mem::ManuallyDrop::new(unsafe { AccessibilityElement::from_raw(element) });

    // Get the frame - if this fails, ManuallyDrop ensures no CFRelease is called
    let Ok(frame) = ax_element.get_frame() else {
        return None;
    };

    // Try to find matching window ID from the window list
    let Ok(windows) = window::get_all_windows() else {
        return None;
    };

    // Use the unified frame matching utility
    window::find_window_match_by_frame(&windows, &frame, window::DEFAULT_FRAME_TOLERANCE)
        .map(|result| (result.window_id, result.width, result.height))
}

// ============================================================================
// Resize Timer Callback
// ============================================================================

/// Timer callback for processing pending resizes.
///
/// This is called by CFRunLoopTimer when the resize settle time has elapsed.
unsafe extern "C" fn resize_timer_callback(_timer: CFRunLoopTimerRef, _info: *mut c_void) {
    let manager = get_observer_manager();
    let mut guard = manager.write();
    guard.process_pending_resizes();
}

/// Schedules a timer to process pending resizes after the settle time.
fn schedule_resize_timer(run_loop: CFRunLoopRef) {
    let fire_date = unsafe { CFAbsoluteTimeGetCurrent() } + (RESIZE_SETTLE_TIME_MS as f64 / 1000.0);

    let timer = unsafe {
        CFRunLoopTimerCreate(
            ptr::null(), // allocator (default)
            fire_date,   // fire date
            0.0,         // interval (0 = one-shot)
            0,           // flags
            0,           // order
            resize_timer_callback,
            ptr::null_mut(), // context
        )
    };

    if !timer.is_null() {
        unsafe {
            CFRunLoopAddTimer(run_loop, timer, cf_run_loop_common_modes());
            // Timer will be automatically released after firing (one-shot)
        }
    }
}

// ============================================================================
// Move Timer Callback
// ============================================================================

/// Timer callback for processing pending moves.
///
/// This is called by CFRunLoopTimer when the move settle time has elapsed.
unsafe extern "C" fn move_timer_callback(_timer: CFRunLoopTimerRef, _info: *mut c_void) {
    let manager = get_observer_manager();
    let mut guard = manager.write();
    guard.process_pending_moves();
}

/// Schedules a timer to process pending moves after the settle time.
fn schedule_move_timer(run_loop: CFRunLoopRef) {
    let fire_date = unsafe { CFAbsoluteTimeGetCurrent() } + (MOVE_SETTLE_TIME_MS as f64 / 1000.0);

    let timer = unsafe {
        CFRunLoopTimerCreate(
            ptr::null(), // allocator (default)
            fire_date,   // fire date
            0.0,         // interval (0 = one-shot)
            0,           // flags
            0,           // order
            move_timer_callback,
            ptr::null_mut(), // context
        )
    };

    if !timer.is_null() {
        unsafe {
            CFRunLoopAddTimer(run_loop, timer, cf_run_loop_common_modes());
            // Timer will be automatically released after firing (one-shot)
        }
    }
}

// ============================================================================
// Sync Timer Callback
// ============================================================================

/// Timer callback for delayed window sync.
///
/// This is called to retry sync_windows after a delay, to catch windows
/// that don't appear in CGWindowListCopyWindowInfo immediately.
unsafe extern "C" fn sync_timer_callback(_timer: CFRunLoopTimerRef, _info: *mut c_void) {
    let manager = get_observer_manager();
    let mut guard = manager.write();
    guard.sync_windows();
}

/// Schedules a timer to sync windows after a delay.
fn schedule_sync_timer(run_loop: CFRunLoopRef, delay_ms: u64) {
    let fire_date = unsafe { CFAbsoluteTimeGetCurrent() } + (delay_ms as f64 / 1000.0);

    let timer = unsafe {
        CFRunLoopTimerCreate(
            ptr::null(), // allocator (default)
            fire_date,   // fire date
            0.0,         // interval (0 = one-shot)
            0,           // flags
            0,           // order
            sync_timer_callback,
            ptr::null_mut(), // context
        )
    };

    if !timer.is_null() {
        unsafe {
            CFRunLoopAddTimer(run_loop, timer, cf_run_loop_common_modes());
            // Timer will be automatically released after firing (one-shot)
        }
    }
}

// ============================================================================
// Public API
// ============================================================================

/// Starts the event observation system.
pub fn start_observing(app_handle: AppHandle) {
    if IS_RUNNING.swap(true, Ordering::SeqCst) {
        // Already running
        return;
    }

    // Set up NSWorkspace observer for app activations (Cmd+Tab detection)
    // This must be done on the main thread
    setup_app_activation_observer();

    // Set up the manager
    {
        let manager = get_observer_manager();
        let mut guard = manager.write();
        guard.set_app_handle(app_handle);
    }

    // Spawn the observer thread with its own run loop
    std::thread::spawn(move || {
        // Get the current run loop (for this thread)
        let run_loop = unsafe { CFRunLoopGetCurrent() };

        {
            let manager = get_observer_manager();
            let mut guard = manager.write();
            guard.set_run_loop(run_loop);
        }

        // Set up a low-frequency timer for cleanup tasks (dead process detection)
        // This runs every 5 seconds as a fallback, not for primary event detection
        let cleanup_fire_date = unsafe { CFAbsoluteTimeGetCurrent() } + 5.0;
        let cleanup_timer = unsafe {
            CFRunLoopTimerCreate(
                ptr::null(),
                cleanup_fire_date,
                5.0, // 5 second interval
                0,
                0,
                cleanup_timer_callback,
                ptr::null_mut(),
            )
        };
        if !cleanup_timer.is_null() {
            unsafe {
                CFRunLoopAddTimer(run_loop, cleanup_timer, cf_run_loop_common_modes());
            }
        }

        // Set up a one-shot timer to discover applications after the run loop starts
        // This allows the run loop to process events while we're discovering apps
        let discovery_fire_date = unsafe { CFAbsoluteTimeGetCurrent() } + 0.1; // 100ms delay
        let discovery_timer = unsafe {
            CFRunLoopTimerCreate(
                ptr::null(),
                discovery_fire_date,
                0.0, // No repeat (one-shot)
                0,
                0,
                discovery_timer_callback,
                ptr::null_mut(),
            )
        };
        if !discovery_timer.is_null() {
            unsafe {
                CFRunLoopAddTimer(run_loop, discovery_timer, cf_run_loop_common_modes());
            }
        }

        // Run the run loop - this blocks and processes AXObserver events
        unsafe {
            CFRunLoopRun();
        }
    });
}

/// Timer callback for initial application discovery.
/// This runs once shortly after the run loop starts, allowing events to be processed
/// while we're setting up observers for all running applications.
unsafe extern "C" fn discovery_timer_callback(_timer: CFRunLoopTimerRef, _info: *mut c_void) {
    if !IS_RUNNING.load(Ordering::SeqCst) {
        return;
    }

    let manager = get_observer_manager();
    let mut guard = manager.write();

    if let Err(e) = guard.discover_applications() {
        eprintln!("barba: failed to discover applications: {e}");
    }
}

/// Timer callback for periodic cleanup (dead process detection).
/// This is a fallback mechanism that runs infrequently.
unsafe extern "C" fn cleanup_timer_callback(_timer: CFRunLoopTimerRef, _info: *mut c_void) {
    if !IS_RUNNING.load(Ordering::SeqCst) {
        return;
    }

    let manager = get_observer_manager();
    let mut guard = manager.write();

    // Only check for dead processes - window events are handled by AXObserver
    let dead_pids: Vec<i32> = window::get_all_windows().map_or_else(
        |_| Vec::new(),
        |windows| {
            let current_pids: HashSet<i32> = windows.iter().map(|w| w.pid).collect();
            guard
                .observers
                .keys()
                .filter(|pid| !current_pids.contains(pid))
                .copied()
                .collect()
        },
    );

    for pid in dead_pids {
        guard.remove_observer_for_pid(pid);
    }
}

// ============================================================================
// NSWorkspace App Activation Observer
// ============================================================================

/// Sets up an NSWorkspace observer to detect app activations (Cmd+Tab, Dock clicks, etc.).
/// This is more reliable than per-app AX observers for detecting app switches.
///
/// # Safety
/// This function calls Objective-C runtime functions.
pub fn setup_app_activation_observer() {
    use std::ptr::null_mut;

    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    unsafe {
        // Get the workspace and notification center
        let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        let notification_center: *mut Object = msg_send![workspace, notificationCenter];

        // Create an observer object (handles both activation and launch)
        let observer = create_app_activation_observer_object();

        // Observe app activation (Cmd+Tab, dock clicks, etc.)
        let activation_name = nsstring("NSWorkspaceDidActivateApplicationNotification");
        let _: () = msg_send![
            notification_center,
            addObserver: observer
            selector: sel!(handleAppActivated:)
            name: activation_name
            object: null_mut::<Object>()
        ];

        // Observe app launch (new apps starting)
        let launch_name = nsstring("NSWorkspaceDidLaunchApplicationNotification");
        let _: () = msg_send![
            notification_center,
            addObserver: observer
            selector: sel!(handleAppLaunched:)
            name: launch_name
            object: null_mut::<Object>()
        ];

        // NSWorkspace app activation and launch observer set up successfully
    }
}

/// Creates an NSString from a Rust string.
unsafe fn nsstring(s: &str) -> *mut objc::runtime::Object {
    use std::ffi::CString;

    use objc::{class, msg_send, sel, sel_impl};

    let c_str = CString::new(s).unwrap();
    let ns_string: *mut objc::runtime::Object = msg_send![
        class!(NSString),
        stringWithUTF8String: c_str.as_ptr()
    ];
    ns_string
}

/// Creates the Objective-C observer object for app activation notifications.
#[allow(clippy::items_after_statements)]
unsafe fn create_app_activation_observer_object() -> *mut objc::runtime::Object {
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Sel};
    use objc::{class, msg_send, sel, sel_impl};

    let superclass = class!(NSObject);
    let class_name = "TilingAppActivationObserver";

    // Check if class already exists
    let existing_class = Class::get(class_name);
    let observer_class = existing_class.unwrap_or_else(|| {
        // Register new class
        let mut decl = ClassDecl::new(class_name, superclass)
            .expect("Failed to create TilingAppActivationObserver class");

        // Add the handler method for app activation
        extern "C" fn handle_app_activated(_this: &Object, _sel: Sel, notification: *mut Object) {
            handle_nsworkspace_app_activated(notification);
        }

        // Add the handler method for app launch
        extern "C" fn handle_app_launched(_this: &Object, _sel: Sel, notification: *mut Object) {
            handle_nsworkspace_app_launched(notification);
        }

        unsafe {
            decl.add_method(
                sel!(handleAppActivated:),
                handle_app_activated as extern "C" fn(&Object, Sel, *mut Object),
            );
            decl.add_method(
                sel!(handleAppLaunched:),
                handle_app_launched as extern "C" fn(&Object, Sel, *mut Object),
            );
        }

        decl.register()
    });

    // Create an instance
    let observer: *mut Object = msg_send![observer_class, alloc];
    let observer: *mut Object = msg_send![observer, init];
    observer
}

/// Handles the NSWorkspaceDidActivateApplicationNotification.
fn handle_nsworkspace_app_activated(notification: *mut objc::runtime::Object) {
    use objc::runtime::Object;
    use objc::{msg_send, sel, sel_impl};

    unsafe {
        // Get the userInfo dictionary
        let user_info: *mut Object = msg_send![notification, userInfo];
        if user_info.is_null() {
            return;
        }

        // Get the NSWorkspaceApplicationKey
        let app_key = nsstring("NSWorkspaceApplicationKey");
        let app: *mut Object = msg_send![user_info, objectForKey: app_key];
        if app.is_null() {
            return;
        }

        // Get the PID of the activated app
        let pid: i32 = msg_send![app, processIdentifier];

        // NSWorkspace APP_ACTIVATED for this PID

        // Now handle the activation in our observer manager
        let manager = get_observer_manager();
        let mut guard = manager.write();
        guard.handle_app_activated_by_pid(pid);
    }
}

/// Handles the NSWorkspaceDidLaunchApplicationNotification.
/// This is called when a new app is launched, before it has windows.
fn handle_nsworkspace_app_launched(notification: *mut objc::runtime::Object) {
    use objc::runtime::Object;
    use objc::{msg_send, sel, sel_impl};

    unsafe {
        // Get the userInfo dictionary
        let user_info: *mut Object = msg_send![notification, userInfo];
        if user_info.is_null() {
            return;
        }

        // Get the NSWorkspaceApplicationKey
        let app_key = nsstring("NSWorkspaceApplicationKey");
        let app: *mut Object = msg_send![user_info, objectForKey: app_key];
        if app.is_null() {
            return;
        }

        // Get the PID of the launched app
        let pid: i32 = msg_send![app, processIdentifier];

        // Add an observer for the new app immediately so we can catch window creation events
        let manager = get_observer_manager();
        let mut guard = manager.write();

        // Add observer if we don't have one yet
        if !guard.observers.contains_key(&pid) {
            let _ = guard.add_observer_for_pid(pid);
        }

        // Always schedule delayed syncs for newly launched apps
        // Apps often take time to create windows after launch
        drop(guard); // Release lock before spawning thread

        // Do multiple delayed syncs to catch windows at different stages of app startup
        for delay_ms in [300, 600, 1000] {
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                let manager = get_observer_manager();
                let mut guard = manager.write();
                guard.sync_windows();
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_observer_manager_new() {
        let manager = ObserverManager::new();
        assert!(manager.observers.is_empty());
        assert!(manager.known_windows.is_empty());
    }

    #[test]
    fn test_cooldown_functions() {
        assert!(!is_in_layout_cooldown());
        assert!(!is_in_switch_cooldown());

        mark_layout_applied();
        assert!(is_in_layout_cooldown());

        mark_switch_completed();
        assert!(is_in_switch_cooldown());
    }
}
