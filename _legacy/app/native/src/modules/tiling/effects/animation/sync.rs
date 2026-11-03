//! Display synchronization and timing utilities.
//!
//! Provides vsync via `CVDisplayLink`, precision sleep, thread priority,
//! and `CATransaction` management for smooth animations.

use std::ffi::c_void;
use std::os::raw::c_int;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

// ============================================================================
// FFI Declarations
// ============================================================================

#[link(name = "pthread")]
unsafe extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: c_int, relative_priority: c_int) -> c_int;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGMainDisplayID() -> u32;
    fn CGDisplayCopyDisplayMode(display: u32) -> *mut c_void;
    fn CGDisplayModeGetRefreshRate(mode: *mut c_void) -> f64;
    fn CGDisplayModeRelease(mode: *mut c_void);
}

// CVDisplayLink FFI
type CVDisplayLinkRef = *mut c_void;
type CVReturn = i32;
type CVOptionFlags = u64;

/// `CVTimeStamp` structure for display link callbacks.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct CVTimeStamp {
    version: u32,
    video_time_scale: i32,
    video_time: i64,
    host_time: u64,
    rate_scalar: f64,
    video_refresh_period: i64,
    smpte_time: [u8; 24],
    flags: u64,
    reserved: u64,
}

/// Callback type for `CVDisplayLink`.
type CVDisplayLinkOutputCallback = unsafe extern "C" fn(
    display_link: CVDisplayLinkRef,
    in_now: *const CVTimeStamp,
    in_output_time: *const CVTimeStamp,
    flags_in: CVOptionFlags,
    flags_out: *mut CVOptionFlags,
    context: *mut c_void,
) -> CVReturn;

#[link(name = "CoreVideo", kind = "framework")]
#[allow(clashing_extern_declarations)] // Intentional: different callback signature for our use
unsafe extern "C" {
    fn CVDisplayLinkCreateWithCGDisplay(
        display_id: u32,
        display_link_out: *mut CVDisplayLinkRef,
    ) -> CVReturn;
    fn CVDisplayLinkSetOutputCallback(
        display_link: CVDisplayLinkRef,
        callback: CVDisplayLinkOutputCallback,
        user_info: *mut c_void,
    ) -> CVReturn;
    fn CVDisplayLinkStart(display_link: CVDisplayLinkRef) -> CVReturn;
    fn CVDisplayLinkStop(display_link: CVDisplayLinkRef) -> CVReturn;
    fn CVDisplayLinkRelease(display_link: CVDisplayLinkRef);
}

// Objective-C runtime for CATransaction
#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const std::ffi::c_char) -> *const c_void;
    fn sel_registerName(name: *const std::ffi::c_char) -> *const c_void;
    fn objc_msgSend(receiver: *const c_void, selector: *const c_void, ...);
}

/// macOS `QoS` class for user-interactive work (highest priority).
const QOS_CLASS_USER_INTERACTIVE: c_int = 0x21;

// ============================================================================
// Constants
// ============================================================================

/// Default frame rate when display refresh rate cannot be detected.
const DEFAULT_FPS: u32 = 60;

/// Threshold for spin-wait vs sleep (microseconds).
const SPIN_WAIT_THRESHOLD_US: u64 = 1000;

// ============================================================================
// Display Refresh Rate Detection
// ============================================================================

/// Cached display refresh rate.
static DISPLAY_REFRESH_RATE: OnceLock<u32> = OnceLock::new();

/// Gets the main display's refresh rate, caching the result.
fn get_display_refresh_rate() -> u32 {
    *DISPLAY_REFRESH_RATE.get_or_init(|| {
        let rate = unsafe {
            let display = CGMainDisplayID();
            let mode = CGDisplayCopyDisplayMode(display);
            if mode.is_null() {
                return DEFAULT_FPS;
            }
            let rate = CGDisplayModeGetRefreshRate(mode);
            CGDisplayModeRelease(mode);

            if rate <= 0.0 {
                DEFAULT_FPS
            } else {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let rounded = rate.round() as u32;
                rounded.clamp(30, 360)
            }
        };
        tracing::debug!("tiling: detected display refresh rate: {rate} Hz");
        rate
    })
}

/// Returns the target FPS for animations.
#[inline]
#[must_use]
pub fn target_fps() -> u32 { get_display_refresh_rate() }

// ============================================================================
// CVDisplayLink Frame Synchronization
// ============================================================================

/// Shared state for display link callback synchronization.
struct DisplaySyncState {
    frame_count: AtomicU64,
    condvar: Condvar,
    mutex: Mutex<()>,
}

impl DisplaySyncState {
    const fn new() -> Self {
        Self {
            frame_count: AtomicU64::new(0),
            condvar: Condvar::new(),
            mutex: Mutex::new(()),
        }
    }

    fn signal_vsync(&self) {
        self.frame_count.fetch_add(1, Ordering::Release);
        self.condvar.notify_all();
    }

    #[allow(clippy::significant_drop_tightening)]
    fn wait_for_vsync(&self, timeout: Duration) -> bool {
        let current_frame = self.frame_count.load(Ordering::Acquire);
        let guard = self.mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

        let result = self
            .condvar
            .wait_timeout_while(guard, timeout, |()| {
                self.frame_count.load(Ordering::Acquire) == current_frame
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        !result.1.timed_out()
    }
}

/// Global display sync state.
static DISPLAY_SYNC: OnceLock<Arc<DisplaySyncState>> = OnceLock::new();

/// `CVDisplayLink` callback.
///
/// # Safety
///
/// This function is called by Core Video on a high-priority thread.
/// The context is a valid pointer to `Arc<DisplaySyncState>`.
unsafe extern "C" fn display_link_callback(
    _display_link: CVDisplayLinkRef,
    _in_now: *const CVTimeStamp,
    _in_output_time: *const CVTimeStamp,
    _flags_in: CVOptionFlags,
    _flags_out: *mut CVOptionFlags,
    context: *mut c_void,
) -> CVReturn {
    let state = unsafe { &*(context.cast::<DisplaySyncState>()) };
    state.signal_vsync();
    0 // kCVReturnSuccess
}

/// RAII wrapper for `CVDisplayLink`.
struct DisplayLink {
    link: CVDisplayLinkRef,
    #[allow(dead_code)]
    state: Arc<DisplaySyncState>,
}

// SAFETY: CVDisplayLink is thread-safe per Apple's Core Video documentation.
unsafe impl Send for DisplayLink {}
unsafe impl Sync for DisplayLink {}

impl DisplayLink {
    fn new() -> Option<Self> {
        let state = Arc::new(DisplaySyncState::new());
        let mut link: CVDisplayLinkRef = std::ptr::null_mut();

        unsafe {
            let display_id = CGMainDisplayID();

            if CVDisplayLinkCreateWithCGDisplay(display_id, &raw mut link) != 0 {
                return None;
            }

            let state_ptr = Arc::as_ptr(&state).cast_mut().cast::<c_void>();
            if CVDisplayLinkSetOutputCallback(link, display_link_callback, state_ptr) != 0 {
                CVDisplayLinkRelease(link);
                return None;
            }

            if CVDisplayLinkStart(link) != 0 {
                CVDisplayLinkRelease(link);
                return None;
            }
        }

        let _ = DISPLAY_SYNC.set(Arc::clone(&state));
        Some(Self { link, state })
    }
}

impl Drop for DisplayLink {
    fn drop(&mut self) {
        unsafe {
            CVDisplayLinkStop(self.link);
            CVDisplayLinkRelease(self.link);
        }
    }
}

/// Global display link instance.
static DISPLAY_LINK: OnceLock<Option<DisplayLink>> = OnceLock::new();

/// Initializes the display link if not already initialized.
pub fn init_display_link() {
    DISPLAY_LINK.get_or_init(|| {
        let link = DisplayLink::new();
        if link.is_some() {
            tracing::debug!("tiling: CVDisplayLink initialized for vsync");
        } else {
            tracing::debug!("tiling: CVDisplayLink failed, using fallback timing");
        }
        link
    });
}

/// Waits for the next vsync, falling back to precision sleep if unavailable.
#[inline]
pub fn wait_for_next_frame(fallback_duration: Duration) {
    if let Some(state) = DISPLAY_SYNC.get()
        && state.wait_for_vsync(fallback_duration * 2)
    {
        return;
    }
    precision_sleep(fallback_duration);
}

// ============================================================================
// CATransaction - Disable Implicit Animations
// ============================================================================

/// Cached selectors for `CATransaction` methods.
struct CATransactionSelectors {
    class: *const c_void,
    begin: *const c_void,
    commit: *const c_void,
    set_disable_actions: *const c_void,
}

// SAFETY: These pointers reference Objective-C runtime metadata that is
// immutable once registered and thread-safe to read.
unsafe impl Send for CATransactionSelectors {}
unsafe impl Sync for CATransactionSelectors {}

/// Cached `CATransaction` selectors.
static CA_TRANSACTION: OnceLock<CATransactionSelectors> = OnceLock::new();

fn get_ca_transaction() -> &'static CATransactionSelectors {
    CA_TRANSACTION.get_or_init(|| unsafe {
        CATransactionSelectors {
            class: objc_getClass(c"CATransaction".as_ptr()),
            begin: sel_registerName(c"begin".as_ptr()),
            commit: sel_registerName(c"commit".as_ptr()),
            set_disable_actions: sel_registerName(c"setDisableActions:".as_ptr()),
        }
    })
}

/// Begins a `CATransaction` with implicit animations disabled.
#[inline]
pub fn ca_transaction_begin_disabled() {
    let ca = get_ca_transaction();
    unsafe {
        objc_msgSend(ca.class, ca.begin);
        objc_msgSend(ca.class, ca.set_disable_actions, 1 as c_int);
    }
}

/// Commits the current `CATransaction`.
#[inline]
pub fn ca_transaction_commit() {
    let ca = get_ca_transaction();
    unsafe {
        objc_msgSend(ca.class, ca.commit);
    }
}

// ============================================================================
// Thread Priority
// ============================================================================

/// Sets the current thread to high priority for smooth animations.
pub fn set_high_priority_thread() {
    unsafe {
        pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0);
    }
}

/// High-precision sleep that uses spin-waiting for the final microseconds.
#[inline]
pub fn precision_sleep(duration: Duration) {
    if duration.is_zero() {
        return;
    }

    let target = Instant::now() + duration;
    let spin_threshold = Duration::from_micros(SPIN_WAIT_THRESHOLD_US);

    if let Some(sleep_duration) = duration.checked_sub(spin_threshold) {
        std::thread::sleep(sleep_duration);
    }

    while Instant::now() < target {
        std::hint::spin_loop();
    }
}
