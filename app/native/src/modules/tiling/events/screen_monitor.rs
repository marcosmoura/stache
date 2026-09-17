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

use parking_lot::RwLock;

use crate::modules::tiling::events::EventProcessor;

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
    /// Display is being reconfigured (about to change).
    pub const kCGDisplayBeginConfigurationFlag: u32 = 1 << 0;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGDisplayRegisterReconfigurationCallback(
        callback: unsafe extern "C" fn(u32, u32, *mut c_void),
        user_info: *mut c_void,
    ) -> i32;
    fn CGDisplayRemoveReconfigurationCallback(
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

    /// Generation this adapter belongs to (from `init::current_generation()`).
    generation: u64,

    /// Whether the adapter is initialized (callback registered).
    initialized: AtomicBool,

    /// Whether we're currently processing a screen change.
    /// Prevents recursive callbacks during our own screen refresh operations.
    processing: AtomicBool,

    /// Whether callbacks should be forwarded.
    active: AtomicBool,
}

impl ScreenMonitorAdapter {
    /// Creates a new adapter with the given event processor.
    #[must_use]
    pub fn new(processor: Arc<EventProcessor>) -> Self {
        Self {
            processor,
            generation: crate::modules::tiling::init::current_generation(),
            initialized: AtomicBool::new(false),
            processing: AtomicBool::new(false),
            active: AtomicBool::new(false),
        }
    }

    /// Whether this adapter's callbacks may currently be forwarded.
    fn gate_open(&self) -> bool {
        self.active.load(Ordering::SeqCst)
            && self.generation == crate::modules::tiling::init::current_generation()
    }

    /// Removes the CoreGraphics reconfiguration callback and invalidates any
    /// in-flight delayed worker by bumping the gate. Idempotent.
    pub fn shutdown(&self) {
        self.active.store(false, Ordering::SeqCst);
        uninstall_adapter();
        if !self.initialized.swap(false, Ordering::SeqCst) {
            return;
        }
        unsafe {
            CGDisplayRemoveReconfigurationCallback(
                display_reconfiguration_callback,
                std::ptr::null_mut(),
            );
        }
        self.processing.store(false, Ordering::SeqCst);
        tracing::debug!("ScreenMonitorAdapter shut down");
    }

    /// Initializes the adapter by registering with CoreGraphics.
    ///
    /// # Returns
    ///
    /// `true` if initialization succeeded, `false` if already initialized or failed.
    pub fn init(&self) -> bool {
        if self.initialized.swap(true, Ordering::SeqCst) {
            tracing::warn!("ScreenMonitorAdapter already initialized");
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
            tracing::error!("Failed to register display reconfiguration callback: {result}");
            self.initialized.store(false, Ordering::SeqCst);
            return false;
        }

        self.active.store(true, Ordering::SeqCst);
        // Register current screens with the processor
        self.register_all_screens();

        tracing::debug!("ScreenMonitorAdapter initialized");
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
        let screens = crate::modules::tiling::actor::handlers::get_screens_from_macos();
        self.processor.reconcile_screens(&screens);
    }

    /// Handles a screen configuration change.
    ///
    /// This is called after the debounce delay, on the main thread (from the
    /// CoreGraphics callback). We detect screens here and pass them to the
    /// processor to avoid calling macOS APIs from the async actor task.
    fn on_screens_changed(&self) {
        self.set_processing(true);

        if !self.gate_open() {
            self.set_processing(false);
            return;
        }

        let screens = crate::platform::thread::dispatch_on_main_sync(|| {
            crate::modules::tiling::actor::handlers::get_screens_from_macos()
        });

        // Re-check after the main-thread hop: a pause may have torn us down.
        if !self.gate_open() {
            self.set_processing(false);
            return;
        }

        self.processor.reconcile_screens(&screens);
        self.processor.on_set_screens(screens);
        self.set_processing(false);
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
    if !should_debounce_reconfiguration(flags) {
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

    tracing::debug!("Screen display reconfiguration completed");

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

/// Returns whether a CoreGraphics display callback represents a completed
/// configuration that requires a debounced topology snapshot.
const fn should_debounce_reconfiguration(flags: u32) -> bool {
    flags & cg_flags::kCGDisplayBeginConfigurationFlag == 0
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

    /// Serializes tests that install into / clear the global adapter slot.
    static TEST_SLOT_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    #[tokio::test]
    async fn test_adapter_creation() {
        let (handle, _stopped) = StateActor::spawn();
        let processor = Arc::new(EventProcessor::new(handle.clone()));
        let adapter = ScreenMonitorAdapter::new(processor);

        assert!(!adapter.is_initialized());
        assert!(!adapter.is_processing());

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_processing_flag() {
        let (handle, _stopped) = StateActor::spawn();
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
        let _guard = TEST_SLOT_LOCK.lock();
        let (handle, _stopped) = StateActor::spawn();
        let processor = Arc::new(EventProcessor::new(handle.clone()));
        let adapter = Arc::new(ScreenMonitorAdapter::new(processor));

        install_adapter(Arc::clone(&adapter));
        assert!(get_installed_adapter().is_some());

        uninstall_adapter();
        assert!(get_installed_adapter().is_none());

        handle.shutdown().unwrap();
    }

    #[test]
    fn test_cg_flags() {
        // Verify flag values match CoreGraphics
        assert_eq!(cg_flags::kCGDisplayBeginConfigurationFlag, 0x01);
    }

    #[test]
    fn test_completed_reconfiguration_is_debounced_without_add_or_remove_flags() {
        assert!(!should_debounce_reconfiguration(
            cg_flags::kCGDisplayBeginConfigurationFlag
        ));
        assert!(should_debounce_reconfiguration(0));
    }

    #[tokio::test]
    async fn test_shutdown_clears_initialized_and_uninstalls() {
        let _guard = TEST_SLOT_LOCK.lock();
        let (handle, _stopped) = StateActor::spawn();
        let processor = Arc::new(EventProcessor::new(handle.clone()));
        let adapter = Arc::new(ScreenMonitorAdapter::new(processor));

        install_adapter(Arc::clone(&adapter));

        adapter.shutdown();
        assert!(!adapter.is_initialized());
        assert!(!adapter.is_processing());
        assert!(!adapter.gate_open());
        assert!(get_installed_adapter().is_none());

        handle.shutdown().unwrap();
    }
}
