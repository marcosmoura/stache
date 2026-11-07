//! Tracking-compatible main-thread dispatch for the presenter.
//!
//! # Why not `run_on_main`
//!
//! [`crate::main_thread::run_on_main`] enqueues with `dispatch_async_f` on the
//! main **GCD queue**. While `NSMenu` runs its nested event-tracking loop the
//! main run loop is serviced in the event-tracking mode, and draining the main
//! GCD queue during that nested loop is **not assumed**: a job queued that way
//! may only run after tracking exits, which would make `cancelTracking` a
//! no-op. The manual native gate must verify both behaviours on a real menu
//! (`menu-native-check cancel-during-tracking`).
//!
//! # What this module does instead
//!
//! [`RunLoopDispatcher`] queues a block on the main `CFRunLoop` with
//! `CFRunLoopPerformBlock(CFRunLoopGetMain(), kCFRunLoopCommonModes, block)` and
//! calls `CFRunLoopWakeUp`. Blocks queued for the common-modes pseudo mode are
//! performed in every mode in the common set, and `AppKit` registers its
//! event-tracking mode in that set, so the block runs inside nested menu
//! tracking. Nothing here waits: the caller gets `Ok(())` once the block is
//! queued, and acknowledgement is a separate reply the block sends.
//!
//! Header evidence: `CFRunLoop.h` (`CFRunLoopPerformBlock`, `CFRunLoopWakeUp`,
//! `kCFRunLoopCommonModes`). The block is copied by `CFRunLoopPerformBlock`; it
//! owns the job in a one-shot slot, so the job runs at most once and a block
//! that never runs only leaks the closure with the run loop.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, PoisonError};

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_core_foundation::{CFRunLoop, kCFRunLoopCommonModes, kCFRunLoopDefaultMode};

/// A unit of work for the main thread.
pub type MainJob = Box<dyn FnOnce() + Send + 'static>;

/// Why a job could not be queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchError {
    /// The dispatcher rejected the job.
    Rejected,
    /// There is no main run loop.
    Unavailable,
}

/// Queues work for the main thread such that it runs **during** `NSMenu`
/// tracking. Implementations never wait for the job.
pub trait MainDispatcher: Send + Sync {
    /// Queues `job`; on `Err` it is dropped uncalled.
    ///
    /// # Errors
    ///
    /// [`DispatchError`] when the job was not queued.
    fn dispatch(&self, job: MainJob) -> Result<(), DispatchError>;
}

/// The system dispatcher over the main run loop in the common modes.
#[derive(Debug, Clone, Copy, Default)]
pub struct RunLoopDispatcher;

impl MainDispatcher for RunLoopDispatcher {
    fn dispatch(&self, job: MainJob) -> Result<(), DispatchError> {
        let main = CFRunLoop::main().ok_or(DispatchError::Unavailable)?;
        let slot = Mutex::new(Some(job));
        let block = RcBlock::new(move || {
            let job = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(job) = job {
                // Unwinding into the run loop would abort; contain it.
                let _ = catch_unwind(AssertUnwindSafe(job));
            }
        });
        // SAFETY: `kCFRunLoopCommonModes` is an immutable CFString constant and
        // the block is a valid `Fn()` block; `CFRunLoopPerformBlock` copies it,
        // so dropping our reference afterwards is sound.
        unsafe {
            let mode = kCFRunLoopCommonModes;
            main.perform_block(mode.map(|mode| &**mode), Some(&block));
        }
        main.wake_up();
        Ok(())
    }
}

/// Runs the main run loop while `worker` runs on its own thread.
///
/// `AppKit` is initialized and the worker's result is returned once it
/// finishes. For the opt-in native harness only; call it on the main thread.
///
/// Returns `None` when not on the main thread or the worker panicked.
pub fn run_main_loop<R: Send + 'static>(worker: impl FnOnce() -> R + Send + 'static) -> Option<R> {
    let mtm = MainThreadMarker::new()?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = Arc::clone(&done);
    let handle = std::thread::Builder::new()
        .name("menu-native-check-worker".to_owned())
        .spawn(move || {
            let result = worker();
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(main) = CFRunLoop::main() {
                main.wake_up();
            }
            result
        })
        .ok()?;
    while !done.load(std::sync::atomic::Ordering::SeqCst) {
        // Short slices keep the loop responsive to `done` without sources.
        // SAFETY: `kCFRunLoopDefaultMode` is an immutable CFString constant.
        let mode = unsafe { kCFRunLoopDefaultMode };
        let _ = CFRunLoop::run_in_mode(mode, 0.05, false);
    }
    handle.join().ok()
}
