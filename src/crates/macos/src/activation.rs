//! Application-activation observation.
//!
//! Tauri's `RunEvent::Resumed` is a loop-poll notification, not macOS
//! application activation. This adapter subscribes to
//! `NSApplicationDidBecomeActiveNotification` through the default notification
//! center and removes the observer on drop. All Objective-C interaction is
//! contained here; callers get a safe installer.
//!
//! Registration is only performed on the main thread, checked with
//! `MainThreadMarker`. The returned guard is `!Send`, so a caller cannot move it
//! to another thread to drop it: the host keeps it as a local around the Tauri
//! event loop. `AppKit` posts activation notifications on the main thread; the
//! block runs on the posting thread because the observer queue is `None`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2_app_kit::NSApplicationDidBecomeActiveNotification;
use objc2_foundation::{NSNotification, NSNotificationCenter};

/// An installed macOS application-activation observer.
///
/// Construct it with [`ActivationObserver::install`] on the main thread and keep
/// it alive for the lifetime of the event loop. The observer removes itself from
/// the notification center when dropped.
///
/// The guard is `!Send`, so it cannot be moved to another thread to be dropped:
///
/// ```compile_fail,E0277
/// fn assert_send<T: Send>() {}
/// assert_send::<stache_macos::ActivationObserver>();
/// ```
pub struct ActivationObserver {
    center: Retained<NSNotificationCenter>,
    token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

impl ActivationObserver {
    /// Subscribes `notify` to application activation.
    ///
    /// Returns `None` when called off the main thread; the caller must hold the
    /// returned guard across the event loop. The callback runs on the thread
    /// that posts the notification, normally the main thread, and a panic is
    /// contained before it can unwind through the Objective-C block boundary
    /// (unwind-enabled builds only; release aborts).
    #[must_use]
    pub fn install(notify: impl Fn() + Send + Sync + 'static) -> Option<Self> {
        let _main = MainThreadMarker::new()?;
        let center = NSNotificationCenter::defaultCenter();
        let block = RcBlock::new(move |_notification: NonNull<NSNotification>| {
            let _ = catch_unwind(AssertUnwindSafe(&notify));
        });
        let block: &block2::DynBlock<dyn Fn(NonNull<NSNotification>)> = &block;
        // SAFETY: the notification name is an immutable AppKit constant, the
        // observer object is `None` (any sender), and the queue is `None` (deliver
        // on the posting thread). The center retains the block for as long as the
        // returned observer token; the token is removed in `Drop`.
        let token = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSApplicationDidBecomeActiveNotification),
                None,
                None,
                block,
            )
        };
        Some(Self { center, token })
    }
}

impl Drop for ActivationObserver {
    fn drop(&mut self) {
        // SAFETY: `token` came from this center and has not been removed; removal
        // is idempotent for an installed observer.
        unsafe {
            self.center.removeObserver(AsRef::<AnyObject>::as_ref(&self.token));
        }
    }
}

impl std::fmt::Debug for ActivationObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivationObserver").finish_non_exhaustive()
    }
}

/// Posts the activation notification on the current thread for the opt-in
/// smoke harness. Missing from production builds.
#[cfg(feature = "test")]
pub fn post_activation_for_test() {
    let center = NSNotificationCenter::defaultCenter();
    // SAFETY: posting an immutable AppKit notification name through the default
    // center has no additional ownership requirement. The smoke harness calls
    // this only on the main thread.
    unsafe {
        center.postNotificationName_object(NSApplicationDidBecomeActiveNotification, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installation_is_refused_off_the_main_thread() {
        assert!(
            ActivationObserver::install(|| {}).is_none(),
            "the unit test thread must not fabricate main-thread authority"
        );
    }
}
