//! A signalable run-loop source, shared by the owner threads of the border
//! event sources (spaces, part 05; window events, part 06).

use std::ffi::c_void;

use core_foundation::base::TCFType;
use core_foundation::runloop::{
    CFRunLoop, CFRunLoopSource, CFRunLoopSourceContext, CFRunLoopSourceCreate,
    CFRunLoopSourceSignal, CFRunLoopWakeUp, kCFRunLoopDefaultMode,
};

const extern "C" fn wake_perform(_info: *const c_void) {}

/// A signalable run-loop source and its run loop.
pub(super) struct WakeSource {
    source: CFRunLoopSource,
    run_loop: CFRunLoop,
}

// SAFETY: `CFRunLoopSourceSignal` and `CFRunLoopWakeUp` are the documented way
// to signal a run loop from another thread; the wrapper only retains the two
// objects and exposes nothing else.
#[allow(
    clippy::non_send_fields_in_send_ty,
    reason = "signal and wake-up are the documented cross-thread operations"
)]
unsafe impl Send for WakeSource {}
// SAFETY: see above; both calls are thread-safe.
unsafe impl Sync for WakeSource {}

impl WakeSource {
    pub(super) fn new(run_loop: &CFRunLoop) -> Option<Self> {
        let mut context = CFRunLoopSourceContext {
            version: 0,
            info: std::ptr::null_mut(),
            retain: None,
            release: None,
            copyDescription: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: wake_perform,
        };
        // SAFETY: a version 0 context with a valid `perform`; the Create rule
        // result is owned by the wrapper.
        let raw = unsafe { CFRunLoopSourceCreate(std::ptr::null(), 0, &raw mut context) };
        if raw.is_null() {
            return None;
        }
        // SAFETY: non-null Create-rule source.
        let source = unsafe { CFRunLoopSource::wrap_under_create_rule(raw) };
        // SAFETY: `kCFRunLoopDefaultMode` is an immutable CF constant.
        run_loop.add_source(&source, unsafe { kCFRunLoopDefaultMode });
        Some(Self {
            source,
            run_loop: run_loop.clone(),
        })
    }

    pub(super) fn wake(&self) {
        // SAFETY: live source and run loop (see the `Send` justification).
        unsafe {
            CFRunLoopSourceSignal(self.source.as_concrete_TypeRef());
            CFRunLoopWakeUp(self.run_loop.as_concrete_TypeRef());
        }
    }
}
