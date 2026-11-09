//! The run-loop layer: how the owner thread parks and how it is woken.
//!
//! # Wakeup choice
//!
//! Commands arrive on a bounded channel; the sender then calls
//! [`Wakeup::signal`]. The native waker uses `CFRunLoopPerformBlock` on the
//! owner's run loop followed by `CFRunLoopWakeUp`. A performed block stays
//! queued until the loop next runs, so a wakeup issued before the owner parks
//! is **never lost** (a bare `CFRunLoopStop`/`WakeUp` would be, if it raced
//! ahead of `CFRunLoopRunInMode`). A custom `CFRunLoopSource` would also work
//! but its Rust wrapper is not `Send`, so signalling it from other threads
//! would need an unsafe `Send` impl; `core_foundation::runloop::CFRunLoop` is
//! `Send + Sync`, so the block route needs none. The block only stops the
//! current wait; all work happens in the owner loop afterwards.
//!
//! The owner parks with no timeout while idle, so there is no polling timer.
//! A sentinel source (never signalled) keeps `CFRunLoopRunInMode` from
//! returning immediately before any observer source exists.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use block2::{Block, RcBlock};
use core_foundation::base::TCFType;
use core_foundation::runloop::{
    CFRunLoop, CFRunLoopSource, CFRunLoopSourceRef, kCFRunLoopDefaultMode,
};

/// An interval `CFRunLoopRunInMode` treats as infinite (no timer).
const FOREVER: Duration = Duration::from_secs(1_000_000_000_000);

/// Wakes the owner from any thread. Nonblocking.
pub trait Waker: Send + Sync {
    /// Makes the owner's current or next [`Driver::park`] return.
    fn wake(&self);
}

/// How the owner thread waits. Created on the owner thread; not `Send`.
pub trait Driver {
    /// A waker for other threads.
    fn waker(&self) -> Arc<dyn Waker>;

    /// Waits until woken or an observer callback was handled. `None` means no
    /// timeout (idle); `Some` is a bounded poll used only while work is active.
    fn park(&mut self, timeout: Option<Duration>);
}

/// A coalescing wakeup: at most one wake is in flight until the owner clears it.
pub(super) struct Wakeup {
    pending: AtomicBool,
    waker: Arc<dyn Waker>,
}

impl Wakeup {
    pub(super) fn new(waker: Arc<dyn Waker>) -> Self {
        Self {
            pending: AtomicBool::new(false),
            waker,
        }
    }

    /// Called by senders after queueing a command.
    pub(super) fn signal(&self) {
        if !self.pending.swap(true, Ordering::SeqCst) {
            self.waker.wake();
        }
    }

    /// Called by the owner before it drains the queue.
    pub(super) fn clear(&self) { self.pending.store(false, Ordering::SeqCst); }
}

#[repr(C)]
struct SourceContext {
    version: isize,
    info: *mut c_void,
    retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<unsafe extern "C" fn(*const c_void)>,
    copy_description: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    equal: Option<unsafe extern "C" fn(*const c_void, *const c_void) -> u8>,
    hash: Option<unsafe extern "C" fn(*const c_void) -> usize>,
    schedule: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_void)>,
    cancel: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_void)>,
    perform: Option<unsafe extern "C" fn(*mut c_void)>,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRunLoopSourceCreate(
        allocator: *const c_void,
        order: isize,
        context: *mut SourceContext,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopPerformBlock(run_loop: *mut c_void, mode: *const c_void, block: *const c_void);
    fn CFRunLoopWakeUp(run_loop: *mut c_void);
}

const unsafe extern "C" fn sentinel_perform(_info: *mut c_void) {}

/// `CFRunLoop` is `Send + Sync` in `core-foundation`, so this waker is too
/// without any unsafe impl.
struct RunLoopWaker {
    run_loop: CFRunLoop,
}

impl Waker for RunLoopWaker {
    fn wake(&self) {
        let block = RcBlock::new(|| CFRunLoop::get_current().stop());
        let block_ptr: *const Block<dyn Fn()> = &raw const *block;
        let run_loop = self.run_loop.as_concrete_TypeRef().cast::<c_void>();
        // SAFETY: `run_loop` is a live CFRunLoop retained by `self`.
        // `CFRunLoopPerformBlock` copies the block (`Block_copy`), so dropping
        // `block` afterwards is sound; the mode is the immutable default-mode
        // CFString constant. Both calls are documented thread-safe.
        unsafe {
            CFRunLoopPerformBlock(run_loop, kCFRunLoopDefaultMode.cast(), block_ptr.cast());
            CFRunLoopWakeUp(run_loop);
        }
    }
}

/// The production driver: parks in the owner thread's `CFRunLoop`, which also
/// delivers `AXObserver` callbacks.
pub struct RunLoopDriver {
    run_loop: CFRunLoop,
    _sentinel: CFRunLoopSource,
}

impl RunLoopDriver {
    /// Creates the driver for the **calling** thread's run loop.
    #[must_use]
    pub fn new() -> Self {
        let run_loop = CFRunLoop::get_current();
        let mut context = SourceContext {
            version: 0,
            info: std::ptr::null_mut(),
            retain: None,
            release: None,
            copy_description: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: Some(sentinel_perform),
        };
        // SAFETY: a version-0 context with only a no-op `perform`; the system
        // copies the context. The returned +1 source (Create rule) is wrapped
        // exactly once and released on drop.
        let source = unsafe {
            let raw = CFRunLoopSourceCreate(std::ptr::null(), 0, &raw mut context);
            CFRunLoopSource::wrap_under_create_rule(raw)
        };
        // SAFETY: `kCFRunLoopDefaultMode` is an immutable CF constant.
        run_loop.add_source(&source, unsafe { kCFRunLoopDefaultMode });
        Self { run_loop, _sentinel: source }
    }
}

impl Default for RunLoopDriver {
    fn default() -> Self { Self::new() }
}

impl Driver for RunLoopDriver {
    fn waker(&self) -> Arc<dyn Waker> {
        Arc::new(RunLoopWaker {
            run_loop: self.run_loop.clone(),
        })
    }

    fn park(&mut self, timeout: Option<Duration>) {
        // `CFRunLoopRunInMode` treats an interval beyond ~5e11 seconds as
        // "forever" and allocates no timeout timer for it; wakeups and observer
        // callbacks end the wait.
        let duration = timeout.unwrap_or(FOREVER);
        // SAFETY: `kCFRunLoopDefaultMode` is an immutable CF constant.
        let _ = CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, duration, true);
    }
}
