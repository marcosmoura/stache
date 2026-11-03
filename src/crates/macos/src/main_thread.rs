//! Main-thread detection and dispatch.
//!
//! [`is_main_thread`] uses `pthread_main_np`; [`run_on_main`] enqueues a boxed
//! closure on the process-wide main dispatch queue with `dispatch_async_f` and
//! returns immediately. The queue owns the closure until the trampoline runs,
//! where it is reclaimed exactly once.

use std::ffi::{c_int, c_void};

use stache_sdk::{MainThreadExecutor, ThreadError};

type DispatchQueue = *mut c_void;
type DispatchFunction = unsafe extern "C" fn(*mut c_void);

unsafe extern "C" {
    /// Returns non-zero when the caller is the main thread.
    fn pthread_main_np() -> c_int;

    /// The process-wide main queue object. `dispatch_get_main_queue` is an
    /// inline C function that returns its address, so the wrapper does the same.
    static _dispatch_main_q: c_void;

    /// Enqueues a function and its context; always succeeds and never waits.
    fn dispatch_async_f(queue: DispatchQueue, context: *mut c_void, work: DispatchFunction);
}

/// Whether the calling thread is the macOS main thread.
#[must_use]
pub fn is_main_thread() -> bool {
    // SAFETY: pthread_main_np takes no arguments, has no side effects and is
    // documented as safe from any thread.
    unsafe { pthread_main_np() != 0 }
}

/// Enqueues a job on the main thread without waiting for it.
///
/// Ownership of `job` transfers to the main queue exactly once. The trampoline
/// reconstructs and runs the box; there is no path where the job is freed twice
/// or leaked, because `dispatch_async_f` cannot fail.
///
/// # Errors
/// Currently never fails; the result is part of the [`MainThreadExecutor`]
/// contract for hosts whose dispatcher can reject a job.
pub fn run_on_main(job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
    // Boxing the fat pointer keeps the dispatch context a thin pointer and
    // gives the trampoline exactly one thing to reclaim.
    let context = Box::into_raw(Box::new(job)).cast::<c_void>();
    // `_dispatch_main_q` is the process-wide, immutable main queue object;
    // taking its address is exactly what the C inline helper does.
    let queue = (&raw const _dispatch_main_q).cast_mut().cast::<c_void>();
    // SAFETY: `context` comes from Box::into_raw immediately above and is
    // reclaimed exactly once by `trampoline`. dispatch_async_f copies the
    // pointer and returns; it cannot fail, so no error path can leak the box.
    unsafe { dispatch_async_f(queue, context, trampoline) };
    Ok(())
}

unsafe extern "C" fn trampoline(context: *mut c_void) {
    // SAFETY: run_on_main passes exactly one Box<Box<dyn FnOnce + Send>> per
    // enqueue and dispatch_async_f invokes this function with that same
    // pointer exactly once. Moving the inner box out frees the outer
    // allocation; the inner box owns the closure until `job()` consumes it.
    let job = unsafe { *Box::from_raw(context.cast::<Box<dyn FnOnce() + Send>>()) };
    // Unwinding out of an `extern "C"` function aborts the process, and the
    // main dispatch queue has no logger to report a panic. Catch it and drop
    // the payload so one buggy job cannot take the application down.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
}

/// A [`MainThreadExecutor`] backed by `pthread_main_np` and libdispatch.
#[derive(Debug, Clone, Copy, Default)]
pub struct MainThread;

impl MainThreadExecutor for MainThread {
    fn is_main_thread(&self) -> bool { is_main_thread() }

    fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
        if is_main_thread() {
            job();
            Ok(())
        } else {
            run_on_main(job)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spawned_thread_is_not_the_main_thread() {
        let observed = std::thread::spawn(is_main_thread).join().expect("worker thread");
        assert!(!observed);
    }
}
