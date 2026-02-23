use std::ffi::c_void;
use std::thread;

pub fn spawn_named_thread<F>(name: &str, task: F)
where F: FnOnce() + Send + 'static {
    let thread_name = format!("stache-{name}");

    if let Err(err) = thread::Builder::new().name(thread_name.clone()).spawn(task) {
        tracing::error!(thread = %thread_name, error = %err, "failed to spawn thread");
    }
}

// ============================================================================
// Main Thread Dispatch (macOS GCD)
// ============================================================================

/// Dispatch queue type alias.
type DispatchQueue = *const c_void;

/// Quality of Service class for dispatch queues.
#[allow(dead_code)]
mod qos {
    pub const USER_INTERACTIVE: u32 = 0x21;
    pub const USER_INITIATED: u32 = 0x19;
    pub const DEFAULT: u32 = 0x15;
    pub const UTILITY: u32 = 0x11;
    pub const BACKGROUND: u32 = 0x09;
}

#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    /// The main dispatch queue (this is the actual symbol, not the macro).
    static _dispatch_main_q: c_void;
    fn dispatch_async_f(
        queue: DispatchQueue,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
    #[allow(dead_code)] // Used by dispatch_on_high_priority
    fn dispatch_get_global_queue(identifier: isize, flags: usize) -> DispatchQueue;
}

/// Returns the main dispatch queue.
///
/// This is equivalent to `dispatch_get_main_queue()` in C, which is a macro
/// that returns `&_dispatch_main_q`.
fn get_main_queue() -> DispatchQueue { std::ptr::addr_of!(_dispatch_main_q) }

/// Context for dispatching a closure to the main thread.
struct DispatchContext<F: FnOnce() + Send + 'static> {
    closure: Option<F>,
}

/// C-compatible trampoline function that executes the closure.
extern "C" fn dispatch_trampoline<F: FnOnce() + Send + 'static>(context: *mut c_void) {
    unsafe {
        let ctx = Box::from_raw(context.cast::<DispatchContext<F>>());
        if let Some(closure) = ctx.closure {
            closure();
        }
    }
}

/// Dispatches a closure to run on the main thread asynchronously.
///
/// This uses Grand Central Dispatch (GCD) to schedule work on the main queue.
/// The closure will be executed on the main thread at some point in the future.
///
/// # Safety
///
/// This function is safe to call from any thread. The closure must be `Send`
/// because it will be transferred to the main thread.
///
/// # Example
///
/// ```ignore
/// dispatch_on_main(|| {
///     // This code runs on the main thread
///     println!("Running on main thread!");
/// });
/// ```
pub fn dispatch_on_main<F>(closure: F)
where F: FnOnce() + Send + 'static {
    let ctx = Box::new(DispatchContext { closure: Some(closure) });
    let ctx_ptr = Box::into_raw(ctx).cast::<c_void>();

    unsafe {
        let main_queue = get_main_queue();
        dispatch_async_f(main_queue, ctx_ptr, dispatch_trampoline::<F>);
    }
}

/// Dispatches a closure to run on a high-priority global queue asynchronously.
///
/// This uses Grand Central Dispatch (GCD) to schedule work on a user-interactive
/// `QoS` queue, which has the highest priority. This is ideal for time-sensitive
/// UI updates like border following during window drag operations.
///
/// # Safety
///
/// This function is safe to call from any thread. The closure must be `Send`
/// because it will be transferred to a background thread.
#[allow(dead_code)] // Reserved for future high-priority dispatch needs
#[allow(clippy::cast_possible_wrap)] // QoS constants are small positive values
pub fn dispatch_on_high_priority<F>(closure: F)
where F: FnOnce() + Send + 'static {
    let ctx = Box::new(DispatchContext { closure: Some(closure) });
    let ctx_ptr = Box::into_raw(ctx).cast::<c_void>();

    unsafe {
        // QOS_CLASS_USER_INTERACTIVE = 0x21, which gives highest priority
        let queue = dispatch_get_global_queue(qos::USER_INTERACTIVE as isize, 0);
        dispatch_async_f(queue, ctx_ptr, dispatch_trampoline::<F>);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::*;

    #[test]
    fn spawn_named_thread_executes_task() {
        let executed = Arc::new(AtomicBool::new(false));
        let executed_clone = Arc::clone(&executed);

        spawn_named_thread("test-task", move || {
            executed_clone.store(true, Ordering::SeqCst);
        });

        // Give the thread time to execute
        thread::sleep(Duration::from_millis(100));

        assert!(executed.load(Ordering::SeqCst));
    }

    #[test]
    fn spawn_named_thread_uses_correct_prefix() {
        use std::sync::mpsc::channel;

        let (tx, rx) = channel();

        spawn_named_thread("name-test", move || {
            let current_thread = thread::current();
            let name = current_thread.name().unwrap_or("").to_string();
            tx.send(name).unwrap();
        });

        let thread_name = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(thread_name, "stache-name-test");
    }

    #[test]
    fn spawn_named_thread_handles_empty_name() {
        use std::sync::mpsc::channel;

        let (tx, rx) = channel();

        spawn_named_thread("", move || {
            let current_thread = thread::current();
            let name = current_thread.name().unwrap_or("").to_string();
            tx.send(name).unwrap();
        });

        let thread_name = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(thread_name, "stache-");
    }

    #[test]
    fn spawn_named_thread_handles_special_characters_in_name() {
        use std::sync::mpsc::channel;

        let (tx, rx) = channel();

        spawn_named_thread("test-with-dashes_and_underscores", move || {
            let current_thread = thread::current();
            let name = current_thread.name().unwrap_or("").to_string();
            tx.send(name).unwrap();
        });

        let thread_name = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(thread_name, "stache-test-with-dashes_and_underscores");
    }

    #[test]
    fn spawn_named_thread_runs_concurrently() {
        use std::sync::mpsc::channel;

        let (tx1, rx1) = channel();
        let (tx2, rx2) = channel();

        spawn_named_thread("concurrent-1", move || {
            thread::sleep(Duration::from_millis(50));
            tx1.send(1).unwrap();
        });

        spawn_named_thread("concurrent-2", move || {
            thread::sleep(Duration::from_millis(50));
            tx2.send(2).unwrap();
        });

        let result1 = rx1.recv_timeout(Duration::from_secs(1)).unwrap();
        let result2 = rx2.recv_timeout(Duration::from_secs(1)).unwrap();

        assert_eq!(result1, 1);
        assert_eq!(result2, 2);
    }
}
