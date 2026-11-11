//! The selected-keyboard-input-source change observer.
//!
//! A distributed notification is delivery-at-best: the callback ignores the
//! payload, contains panics (unwind-enabled builds only; release aborts) and
//! only calls the supplied hint closure, which the hotkey owner turns into a
//! coalesced refresh request. It never queries the layout, registers a hotkey
//! or waits. The observer object is `!Send` (`MainThreadOnly`), so it lives in
//! the main-owned native layer and is removed there.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_foundation::{
    NSDistributedNotificationCenter, NSNotification, NSNotificationSuspensionBehavior, NSObject,
    NSString,
};

/// `kTISNotifySelectedKeyboardInputSourceChanged`.
pub const INPUT_SOURCE_CHANGED: &str =
    "com.apple.Carbon.TISNotifySelectedKeyboardInputSourceChanged";

struct Ivars {
    hint: Arc<dyn Fn() + Send + Sync>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. The class owns only an
    // `Arc` hint closure; the center calls the selector with a valid
    // `NSNotification`. It is `MainThreadOnly`, so no Send/Sync shim exists.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "StacheKeyboardLayoutObserver"]
    #[ivars = Ivars]
    struct LayoutObserverObject;

    impl LayoutObserverObject {
        /// Ignores the payload, contains panics (unwind-enabled builds only; release aborts) and
        /// only signals.
        #[unsafe(method(stacheKeyboardInputSourceChanged:))]
        fn input_source_changed(&self, _notification: &NSNotification) {
            let hint = Arc::clone(&self.ivars().hint);
            let _ = catch_unwind(AssertUnwindSafe(move || hint()));
        }
    }

    unsafe impl NSObjectProtocol for LayoutObserverObject {}
);

/// One installed observer. The center does not retain selector observers, so
/// this value owns the target until [`LayoutObserver::remove`].
pub struct LayoutObserver {
    center: Retained<NSDistributedNotificationCenter>,
    observer: Retained<LayoutObserverObject>,
}

impl LayoutObserver {
    /// Subscribes `hint` to input-source changes. Main thread only.
    pub fn install(hint: Arc<dyn Fn() + Send + Sync>) -> Result<Self, String> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| "the observer must be installed on the main thread".to_owned())?;
        let allocated = LayoutObserverObject::alloc(mtm).set_ivars(Ivars { hint });
        // SAFETY: `alloc` returns an uninitialized instance of this exact
        // subclass and `init` is NSObject's designated initializer; the class
        // has no `Drop`.
        let observer: Retained<LayoutObserverObject> = unsafe { msg_send![super(allocated), init] };
        let center = NSDistributedNotificationCenter::defaultCenter();
        let name = NSString::from_str(INPUT_SOURCE_CHANGED);
        // SAFETY: the observer is a live selector target retained by the
        // returned value; the selector is implemented above with an
        // `NSNotification` argument; the name is an immutable string and a nil
        // object means any sender.
        unsafe {
            center.addObserver_selector_name_object_suspensionBehavior(
                AsRef::<AnyObject>::as_ref(&*observer),
                sel!(stacheKeyboardInputSourceChanged:),
                Some(&*name),
                None,
                NSNotificationSuspensionBehavior::Coalesce,
            );
        }
        Ok(Self { center, observer })
    }

    /// Removes the observer from the center. Idempotent natively.
    pub fn remove(self) {
        let name = NSString::from_str(INPUT_SOURCE_CHANGED);
        // SAFETY: the observer was registered with this center and name and is
        // still live; removal is idempotent for an installed observer.
        unsafe {
            self.center.removeObserver_name_object(
                AsRef::<AnyObject>::as_ref(&*self.observer),
                Some(&*name),
                None,
            );
        }
    }
}
