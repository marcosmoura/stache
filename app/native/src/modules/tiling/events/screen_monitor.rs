//! Screen configuration monitor adapter for the tiling v2 event pipeline.
//!
//! This module bridges the CoreGraphics display reconfiguration callbacks to
//! the new event processor architecture. It translates screen connect/disconnect
//! events into `StateMessage`s for the state actor.
//!
//! # Architecture
//!
//! ```text
//! ┌───────────────────────────────────────────────────────────────┐
//! │             CGDisplayRegisterReconfigurationCallback           │
//! │                 (CoreGraphics display events)                  │
//! └───────────────────────────┬───────────────────────────────────┘
//!                             │ C callback
//!                             ▼
//! ┌───────────────────────────────────────────────────────────────┐
//! │                  ScreenMonitorAdapter                          │
//! │  - Registers with CoreGraphics                                 │
//! │  - Debounces rapid configuration changes                       │
//! │  - Updates EventProcessor screen registrations                 │
//! │  - Forwards ScreensChanged to EventProcessor                   │
//! └───────────────────────────┬───────────────────────────────────┘
//!                             │ EventProcessor methods
//!                             ▼
//! ┌───────────────────────────────────────────────────────────────┐
//! │                    EventProcessor                              │
//! │  - Updates per-screen batch queues                             │
//! │  - Dispatches to StateActor                                    │
//! └───────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Debouncing
//!
//! Screen configuration changes can trigger multiple rapid callbacks.
//! This adapter uses a short delay to debounce these and only dispatch
//! once the configuration has stabilized.
//!
//! # Thread Safety
//!
//! CoreGraphics callbacks may come from any thread. The adapter uses
//! atomic flags and spawns worker threads to handle the actual processing
//! on a background thread.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use core_graphics::display::CGDisplay;
use parking_lot::RwLock;

use crate::modules::tiling::events::{EventProcessor, get_display_refresh_rate};

/// Delay before processing screen changes (ms).
///
/// This allows macOS to finish updating display state before we query it.
const SCREEN_CHANGE_DELAY_MS: u64 = 200;

// ============================================================================
// CoreGraphics Display Reconfiguration
// ============================================================================

/// Display reconfiguration flags from CoreGraphics.
#[allow(non_upper_case_globals)]
mod cg_flags {
    /// Display has been added.
    pub const kCGDisplayAddFlag: u32 = 1 << 4;
    /// Display has been removed.
    pub const kCGDisplayRemoveFlag: u32 = 1 << 5;
    /// Display is being reconfigured (about to change).
    pub const kCGDisplayBeginConfigurationFlag: u32 = 1 << 0;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGDisplayRegisterReconfigurationCallback(
        callback: unsafe extern "C" fn(u32, u32, *mut c_void),
        user_info: *mut c_void,
    ) -> i32;
}

// ============================================================================
// Screen Monitor Adapter
// ============================================================================

/// Adapter that monitors screen configuration changes and routes them to the `EventProcessor`.
pub struct ScreenMonitorAdapter {
    /// Reference to the event processor.
    processor: Arc<EventProcessor>,

    /// Whether the adapter is initialized (callback registered).
    initialized: AtomicBool,

    /// Whether we're currently processing a screen change.
    /// Prevents recursive callbacks during our own screen refresh operations.
    processing: AtomicBool,
}

impl ScreenMonitorAdapter {
    /// Creates a new adapter with the given event processor.
    #[must_use]
    pub const fn new(processor: Arc<EventProcessor>) -> Self {
        Self {
            processor,
            initialized: AtomicBool::new(false),
            processing: AtomicBool::new(false),
        }
    }

    /// Initializes the adapter by registering with CoreGraphics.
    ///
    /// # Returns
    ///
    /// `true` if initialization succeeded, `false` if already initialized or failed.
    pub fn init(&self) -> bool {
        if self.initialized.swap(true, Ordering::SeqCst) {
            log::warn!("ScreenMonitorAdapter already initialized");
            return false;
        }

        // Register for display reconfiguration notifications
        let result = unsafe {
            CGDisplayRegisterReconfigurationCallback(
                display_reconfiguration_callback,
                std::ptr::null_mut(),
            )
        };

        if result != 0 {
            log::error!("Failed to register display reconfiguration callback: {result}");
            self.initialized.store(false, Ordering::SeqCst);
            return false;
        }

        // Register current screens with the processor
        self.register_all_screens();

        log::debug!("ScreenMonitorAdapter initialized");
        true
    }

    /// Returns whether the adapter is initialized.
    #[must_use]
    pub fn is_initialized(&self) -> bool { self.initialized.load(Ordering::SeqCst) }

    /// Sets the processing flag to prevent recursive callbacks.
    pub fn set_processing(&self, processing: bool) {
        self.processing.store(processing, Ordering::SeqCst);
    }

    /// Returns whether we're currently processing a change.
    #[must_use]
    pub fn is_processing(&self) -> bool { self.processing.load(Ordering::SeqCst) }

    /// Registers all currently connected screens with the `EventProcessor`.
    pub fn register_all_screens(&self) {
        let displays = get_all_display_ids();

        for display_id in displays {
            let refresh_rate = get_display_refresh_rate(display_id);
            self.processor.register_screen(display_id, refresh_rate);
            log::debug!("Registered screen {display_id} with refresh rate {refresh_rate} Hz");
        }
    }

    /// Handles a screen configuration change.
    ///
    /// This is called after the debounce delay, on the main thread (from the
    /// CoreGraphics callback). We detect screens here and pass them to the
    /// processor to avoid calling macOS APIs from the async actor task.
    fn on_screens_changed(&self) {
        log::debug!("Screens changed, updating registrations");

        // Get current displays
        let current_displays: std::collections::HashSet<_> =
            get_all_display_ids().into_iter().collect();

        // Get registered screens from processor
        // Note: We don't have direct access to registered screens, so we'll
        // just re-register all current screens (register_screen handles duplicates)
        for display_id in &current_displays {
            let refresh_rate = get_display_refresh_rate(*display_id);
            self.processor.register_screen(*display_id, refresh_rate);
        }

        // Detect screens on main thread and send to actor
        // This callback runs on the main thread, so NSScreen APIs work here
        let screens = crate::modules::tiling::actor::handlers::get_screens_from_macos();
        self.processor.on_set_screens(screens);
    }
}

// ============================================================================
// CoreGraphics Callback
// ============================================================================

/// CoreGraphics display reconfiguration callback.
///
/// # Safety
///
/// This function is called by the Core Graphics framework when displays are
/// added, removed, or reconfigured.
unsafe extern "C" fn display_reconfiguration_callback(
    _display: u32,
    flags: u32,
    _user_info: *mut c_void,
) {
    // Ignore "begin configuration" events - wait for the actual change
    if flags & cg_flags::kCGDisplayBeginConfigurationFlag != 0 {
        return;
    }

    // Only handle add/remove events (actual screen connect/disconnect)
    let is_add = flags & cg_flags::kCGDisplayAddFlag != 0;
    let is_remove = flags & cg_flags::kCGDisplayRemoveFlag != 0;

    if !is_add && !is_remove {
        return;
    }

    // Get the installed adapter
    let Some(adapter) = get_installed_adapter() else {
        return;
    };

    // Prevent recursive callbacks
    if adapter.is_processing() {
        return;
    }

    let event_type = if is_add { "connected" } else { "disconnected" };
    log::debug!("Screen {event_type} (display reconfiguration)");

    // Mark that we're processing to prevent reentrancy
    adapter.set_processing(true);

    // Clone the adapter Arc for the spawned thread
    let adapter_clone = adapter;

    // Spawn a thread to handle the change with a small delay
    // This ensures all CoreGraphics updates are complete
    std::thread::spawn(move || {
        // Brief delay to let macOS finish updating display state
        std::thread::sleep(Duration::from_millis(SCREEN_CHANGE_DELAY_MS));

        adapter_clone.on_screens_changed();
        adapter_clone.set_processing(false);
    });
}

// ============================================================================
// Display Enumeration
// ============================================================================

/// Gets all connected display IDs.
fn get_all_display_ids() -> Vec<u32> {
    // CGGetActiveDisplayList
    let mut display_count: u32 = 0;

    // First, get the count
    let result = unsafe {
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGGetActiveDisplayList(
                max_displays: u32,
                active_displays: *mut u32,
                display_count: *mut u32,
            ) -> i32;
        }

        CGGetActiveDisplayList(0, std::ptr::null_mut(), &raw mut display_count)
    };

    if result != 0 || display_count == 0 {
        // Fall back to just the main display
        return vec![CGDisplay::main().id];
    }

    // Allocate buffer and get display list
    let mut displays = vec![0u32; display_count as usize];

    let result = unsafe {
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGGetActiveDisplayList(
                max_displays: u32,
                active_displays: *mut u32,
                display_count: *mut u32,
            ) -> i32;
        }

        CGGetActiveDisplayList(display_count, displays.as_mut_ptr(), &raw mut display_count)
    };

    if result != 0 {
        return vec![CGDisplay::main().id];
    }

    displays.truncate(display_count as usize);
    displays
}

// ============================================================================
// Global Adapter Instance
// ============================================================================

/// Global adapter instance for use with the CoreGraphics callback.
static ADAPTER: OnceLock<Arc<RwLock<Option<Arc<ScreenMonitorAdapter>>>>> = OnceLock::new();

fn get_adapter_storage() -> &'static Arc<RwLock<Option<Arc<ScreenMonitorAdapter>>>> {
    ADAPTER.get_or_init(|| Arc::new(RwLock::new(None)))
}

/// Installs the adapter as the global event handler.
///
/// This should be called once during initialization, after creating the adapter.
pub fn install_adapter(adapter: Arc<ScreenMonitorAdapter>) {
    *get_adapter_storage().write() = Some(adapter);
}

/// Removes the installed adapter.
pub fn uninstall_adapter() { *get_adapter_storage().write() = None; }

/// Gets the installed adapter, if any.
#[must_use]
pub fn get_installed_adapter() -> Option<Arc<ScreenMonitorAdapter>> {
    get_adapter_storage().read().clone()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::tiling::actor::StateActor;

    #[tokio::test]
    async fn test_adapter_creation() {
        let handle = StateActor::spawn();
        let processor = Arc::new(EventProcessor::new(handle.clone()));
        let adapter = ScreenMonitorAdapter::new(processor);

        assert!(!adapter.is_initialized());
        assert!(!adapter.is_processing());

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_processing_flag() {
        let handle = StateActor::spawn();
        let processor = Arc::new(EventProcessor::new(handle.clone()));
        let adapter = ScreenMonitorAdapter::new(processor);

        assert!(!adapter.is_processing());
        adapter.set_processing(true);
        assert!(adapter.is_processing());
        adapter.set_processing(false);
        assert!(!adapter.is_processing());

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_global_adapter_install() {
        let handle = StateActor::spawn();
        let processor = Arc::new(EventProcessor::new(handle.clone()));
        let adapter = Arc::new(ScreenMonitorAdapter::new(processor));

        install_adapter(adapter.clone());
        assert!(get_installed_adapter().is_some());

        uninstall_adapter();
        assert!(get_installed_adapter().is_none());

        handle.shutdown().unwrap();
    }

    #[test]
    fn test_get_all_display_ids() {
        let displays = get_all_display_ids();
        // Should have at least one display
        assert!(!displays.is_empty());
        // Main display should be in the list
        let main_id = CGDisplay::main().id;
        assert!(displays.contains(&main_id));
    }

    #[test]
    fn test_cg_flags() {
        // Verify flag values match CoreGraphics
        assert_eq!(cg_flags::kCGDisplayAddFlag, 0x10);
        assert_eq!(cg_flags::kCGDisplayRemoveFlag, 0x20);
        assert_eq!(cg_flags::kCGDisplayBeginConfigurationFlag, 0x01);
    }
}
