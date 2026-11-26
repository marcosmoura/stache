//! Power-source change signals through the `notify(3)` keys `IOKit` posts.
//!
//! [`BatteryEventMonitor::start`] registers a handler block with
//! `notify_register_dispatch` on a global (background) dispatch queue for each
//! key in [`POWER_SOURCE_KEYS`]: `com.apple.system.powersources.source` (the
//! providing source changed: plug or unplug), `...powersources.percent` (the
//! charge percentage changed), `...powersources.timeremaining` (the time
//! estimate changed) and `com.apple.system.powersources` (any power-source
//! attribute changed, such as charging starting seconds after plug-in or a
//! charge limit being reached). `.source`, `.timeremaining` and the
//! any-power-source key are the public `kIOPSNotify*` keys of
//! `IOPowerSources.h`; `.percent` is a private key `powerd` posts, not in the
//! public SDK. The handler only calls the subscriber: it reads nothing and holds no lock.
//!
//! This needs no run loop and no thread of Stache's own, unlike
//! `IOPSNotificationCreateRunLoopSource`, which would need a `CFRunLoop`
//! running on an owned thread. Dropping the monitor first disarms the handler
//! (a callback already in flight on the queue calls nothing) and then cancels
//! every registration with `notify_cancel`, releasing the tokens and blocks.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use block2::{DynBlock, RcBlock};

use super::SystemReadError;

/// Receives each power-source change. Called on a dispatch queue thread: must
/// be cheap and must not block.
pub type BatterySignalSink = Arc<dyn Fn() + Send + Sync>;

/// The `notify(3)` keys watched: `kIOPSNotifyPowerSource`,
/// `kIOPSNotifyTimeRemaining` and `kIOPSNotifyAnyPowerSource` (public, in
/// `IOPowerSources.h`), plus the private `.percent` key `powerd` posts.
pub const POWER_SOURCE_KEYS: [&CStr; 4] = [
    c"com.apple.system.powersources.source",
    c"com.apple.system.powersources.percent",
    c"com.apple.system.powersources.timeremaining",
    c"com.apple.system.powersources",
];

/// `NOTIFY_STATUS_OK`.
const NOTIFY_STATUS_OK: u32 = 0;
/// `QOS_CLASS_UTILITY`.
const QOS_CLASS_UTILITY: isize = 0x11;

unsafe extern "C" {
    fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *mut c_void;
    fn notify_register_dispatch(
        name: *const c_char,
        out_token: *mut c_int,
        queue: *mut c_void,
        handler: &DynBlock<dyn Fn(c_int)>,
    ) -> u32;
    fn notify_cancel(token: c_int) -> u32;
}

/// A running power-source subscription. Dropping it disarms the handler and
/// cancels every registration.
pub struct BatteryEventMonitor {
    tokens: Vec<c_int>,
    armed: Arc<AtomicBool>,
}

impl BatteryEventMonitor {
    /// Starts watching with `sink` as the subscriber.
    ///
    /// # Errors
    /// A key could not be registered. Nothing is left registered.
    pub fn start(sink: BatterySignalSink) -> Result<Self, SystemReadError> {
        Self::start_keys(&POWER_SOURCE_KEYS, sink)
    }

    fn start_keys(keys: &[&CStr], sink: BatterySignalSink) -> Result<Self, SystemReadError> {
        let armed = Arc::new(AtomicBool::new(true));
        let handler = {
            let armed = Arc::clone(&armed);
            RcBlock::new(move |_token: c_int| {
                if armed.load(Ordering::Acquire) {
                    let _ = catch_unwind(AssertUnwindSafe(|| sink()));
                }
            })
        };
        // SAFETY: a documented libdispatch call; global queues are never freed.
        let queue = unsafe { dispatch_get_global_queue(QOS_CLASS_UTILITY, 0) };
        let mut monitor = Self {
            tokens: Vec::with_capacity(keys.len()),
            armed,
        };
        for &key in keys {
            let mut token: c_int = 0;
            // SAFETY: `key` is NUL-terminated, `token` is a valid out pointer,
            // `queue` is a live global queue and libnotify copies the block.
            let status =
                unsafe { notify_register_dispatch(key.as_ptr(), &raw mut token, queue, &handler) };
            if status != NOTIFY_STATUS_OK {
                // `monitor` drops here: cancels what was registered.
                return Err(SystemReadError::new(format!(
                    "power-source notification {} could not be registered ({status})",
                    key.to_string_lossy()
                )));
            }
            monitor.tokens.push(token);
        }
        Ok(monitor)
    }
}

impl Drop for BatteryEventMonitor {
    fn drop(&mut self) {
        self.armed.store(false, Ordering::Release);
        for token in self.tokens.drain(..) {
            // SAFETY: each token came from a successful registration and is
            // cancelled exactly once.
            let _ = unsafe { notify_cancel(token) };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    use super::*;

    unsafe extern "C" {
        fn notify_post(name: *const c_char) -> u32;
    }

    fn wait_for(count: &AtomicUsize, at_least: usize) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if count.load(Ordering::SeqCst) >= at_least {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    /// Installs on the real keys, then tears down. Only root may post the
    /// `com.apple.system.` keys, so delivery is checked on a private key.
    #[test]
    fn installs_signals_and_tears_down() {
        let monitor = BatteryEventMonitor::start(Arc::new(|| {})).expect("installs");
        assert_eq!(monitor.tokens.len(), POWER_SOURCE_KEYS.len());
        drop(monitor);
    }

    #[test]
    fn a_posted_key_calls_the_sink_until_drop() {
        const KEY: &CStr = c"com.marcosmoura.stache.test.battery-events";
        let count = Arc::new(AtomicUsize::new(0));
        let sink: BatterySignalSink = {
            let count = Arc::clone(&count);
            Arc::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
            })
        };
        let monitor = BatteryEventMonitor::start_keys(&[KEY], sink).expect("installs");
        // SAFETY: a NUL-terminated key.
        unsafe { notify_post(KEY.as_ptr()) };
        assert!(wait_for(&count, 1), "the sink was called");
        drop(monitor);
        let after = count.load(Ordering::SeqCst);
        // SAFETY: as above.
        unsafe { notify_post(KEY.as_ptr()) };
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(count.load(Ordering::SeqCst), after, "no call after drop");
    }
}
