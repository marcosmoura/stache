use std::os::raw::c_void;
use std::sync::mpsc::{Sender, channel};

use core_graphics::display::CGDisplayRegisterReconfigurationCallback;

use crate::utils::thread::spawn_named_thread;

pub fn init_screen_watcher(callback: impl Fn() + Send + 'static) {
    spawn_named_thread("screen-watcher", move || {
        let (tx, rx) = channel();

        // Register display reconfiguration callback
        unsafe {
            extern "C" fn display_reconfiguration_callback(
                _display: u32,
                _flags: u32,
                user_info: *const c_void,
            ) {
                if !user_info.is_null() {
                    let tx = unsafe { &*user_info.cast::<Sender<()>>() };
                    let _ = tx.send(());
                }
            }

            let tx_ptr: *const Sender<()> = Box::into_raw(Box::new(tx));

            CGDisplayRegisterReconfigurationCallback(
                display_reconfiguration_callback,
                tx_ptr.cast::<c_void>(),
            );
        }

        while rx.recv().is_ok() {
            callback();
        }
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    #[test]
    fn init_screen_watcher_spawns_thread() {
        let called = Arc::new(AtomicUsize::new(0));
        let called_clone = Arc::clone(&called);

        init_screen_watcher(move || {
            called_clone.fetch_add(1, Ordering::SeqCst);
        });

        // Give the thread time to start
        std::thread::sleep(Duration::from_millis(10));

        // The callback shouldn't be called without a display reconfiguration event
        assert_eq!(called.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn init_screen_watcher_accepts_closure() {
        // Test that various closure types are accepted
        init_screen_watcher(|| {});

        init_screen_watcher(|| {
            let _ = 1 + 1;
        });

        let value = Arc::new(AtomicUsize::new(0));
        let value_clone = Arc::clone(&value);
        init_screen_watcher(move || {
            value_clone.fetch_add(1, Ordering::SeqCst);
        });

        // Give threads time to start
        std::thread::sleep(Duration::from_millis(10));
    }

    #[test]
    fn init_screen_watcher_accepts_fn_pointer() {
        fn my_callback() {
            // Do nothing
        }

        init_screen_watcher(my_callback);

        // Give thread time to start
        std::thread::sleep(Duration::from_millis(10));
    }

    #[test]
    fn multiple_screen_watchers_can_be_created() {
        let counter1 = Arc::new(AtomicUsize::new(0));
        let counter2 = Arc::new(AtomicUsize::new(0));

        let c1 = Arc::clone(&counter1);
        let c2 = Arc::clone(&counter2);

        init_screen_watcher(move || {
            c1.fetch_add(1, Ordering::SeqCst);
        });

        init_screen_watcher(move || {
            c2.fetch_add(1, Ordering::SeqCst);
        });

        // Give threads time to start
        std::thread::sleep(Duration::from_millis(10));

        // Neither should be called without actual display events
        assert_eq!(counter1.load(Ordering::SeqCst), 0);
        assert_eq!(counter2.load(Ordering::SeqCst), 0);
    }
}
