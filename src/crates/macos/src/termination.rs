//! Observation of `AppKit`'s terminate decision.
//!
//! `NSApplication` asks its delegate `applicationShouldTerminate:` before it
//! terminates. The tao delegate class does not implement it, so the default
//! answer terminates the process before the host can coordinate anything or
//! even return from `run_return`. [`TerminateObservation::install`] registers
//! one process-wide answer callback, adds the selector to the live delegate's
//! class with `class_addMethod`, and answers from that callback.
//!
//! This module stays policy-free: the callback returns [`TerminateAnswer`] and
//! the caller decides when to keep waiting and when to let `AppKit` continue.
//! The added method cannot be removed, so installation and callback
//! registration are attempted at most once per process. Every Objective-C
//! interaction stays in this crate.

use std::ffi::CStr;
use std::panic::{AssertUnwindSafe, catch_unwind};
#[cfg(feature = "test")]
use std::ptr::NonNull;
use std::sync::OnceLock;
#[cfg(feature = "test")]
use std::time::Duration;

#[cfg(feature = "test")]
use block2::RcBlock;
use objc2::ffi::{NSUInteger, class_addMethod};
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{MainThreadMarker, sel};
use objc2_app_kit::{NSApplication, NSApplicationTerminateReply};
#[cfg(feature = "test")]
use objc2_foundation::NSTimer;

/// The plain answer one terminate request receives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminateAnswer {
    /// Keep `AppKit` waiting; the coordinated teardown replies later.
    Later,
    /// The teardown outcome already exists; let `AppKit` terminate now.
    Now,
}

type TerminateHandler = Box<dyn Fn() -> TerminateAnswer + Send + Sync + 'static>;

/// The one registered answer callback. `class_addMethod` has no context
/// pointer, so the added method can only reach this process-global slot.
static HANDLER: OnceLock<TerminateHandler> = OnceLock::new();

/// Objective-C encoding for `applicationShouldTerminate:`: an `NSUInteger`
/// return, an object receiver, a selector and an object sender.
const SHOULD_TERMINATE_ENCODING: &CStr = c"Q@:@";

/// Signature of the added `applicationShouldTerminate:` implementation.
type ShouldTerminate =
    unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> NSUInteger;

/// Why the terminate observation could not be installed or used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminateError {
    /// The caller was not the application's main thread.
    NotMainThread,
    /// `NSApplication` has no delegate yet, so there is no class to extend.
    MissingDelegate,
    /// The delegate class or an ancestor already answers the selector. A tao
    /// upgrade that implements the method lands here.
    AlreadyImplemented,
    /// Another terminate callback was already registered in this process.
    AlreadyRegistered,
    /// The Objective-C runtime refused to add the method.
    Rejected,
}

impl std::fmt::Display for TerminateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotMainThread => "the terminate observation requires the main thread",
            Self::MissingDelegate => "the application has no delegate to observe",
            Self::AlreadyImplemented => {
                "the application delegate already implements applicationShouldTerminate:"
            }
            Self::AlreadyRegistered => "a terminate observation callback is already registered",
            Self::Rejected => "the Objective-C runtime refused applicationShouldTerminate:",
        })
    }
}

/// A successfully installed `applicationShouldTerminate:` observation.
///
/// The method stays on the delegate class until the process exits; dropping
/// this value removes nothing.
#[derive(Debug)]
pub struct TerminateObservation(());

impl TerminateObservation {
    /// Registers `handler` and adds `applicationShouldTerminate:` to the live
    /// application delegate.
    ///
    /// # Errors
    /// Returns [`TerminateError::NotMainThread`] off the main thread,
    /// [`TerminateError::MissingDelegate`] before the delegate exists,
    /// [`TerminateError::AlreadyImplemented`] when the selector is already
    /// present, [`TerminateError::AlreadyRegistered`] after a first callback
    /// registration, and [`TerminateError::Rejected`] when the runtime refuses
    /// the method.
    pub fn install(
        handler: impl Fn() -> TerminateAnswer + Send + Sync + 'static,
    ) -> Result<Self, TerminateError> {
        let main = MainThreadMarker::new().ok_or(TerminateError::NotMainThread)?;
        let application = NSApplication::sharedApplication(main);
        let delegate = application.delegate().ok_or(TerminateError::MissingDelegate)?;
        let delegate = AsRef::<AnyObject>::as_ref(&*delegate);
        let class = delegate.class();
        ensure_absent(class)?;
        HANDLER.set(Box::new(handler)).map_err(|_| TerminateError::AlreadyRegistered)?;
        add_method(class)?;
        Ok(Self(()))
    }
}

/// Answers the pending `applicationShouldTerminate:` with `true`.
///
/// `AppKit` keeps the process alive in its modal panel run loop until this
/// call. There is deliberately no way to send `false`: teardown never cancels
/// a terminate decision, and a premature cancel would leave the process
/// running after the user asked it to stop.
///
/// # Errors
/// Returns [`TerminateError::NotMainThread`] when called off the main thread.
pub fn reply_to_terminate() -> Result<(), TerminateError> {
    let main = MainThreadMarker::new().ok_or(TerminateError::NotMainThread)?;
    let application = NSApplication::sharedApplication(main);
    application.replyToApplicationShouldTerminate(true);
    Ok(())
}

/// Schedules the real `terminate:` on this process for the opt-in smoke
/// scenario.
///
/// The timer fires from the run loop like a menu action, so the main queue and
/// tao's sources are not already inside a callback when `AppKit` starts
/// waiting for the reply. Missing from production builds; the caller must be on
/// the main thread.
///
/// # Errors
/// Returns [`TerminateError::NotMainThread`] when called off the main thread.
///
/// # Panics
/// Panics if the scheduled timer fires somewhere other than the main thread,
/// which a run-loop timer cannot do.
#[cfg(feature = "test")]
pub fn terminate_after(delay: Duration) -> Result<(), TerminateError> {
    let _main = MainThreadMarker::new().ok_or(TerminateError::NotMainThread)?;
    let block = RcBlock::new(|_timer: NonNull<NSTimer>| {
        let main = MainThreadMarker::new().expect("the terminate timer runs on the main thread");
        NSApplication::sharedApplication(main).terminate(None);
    });
    // SAFETY: the interval is finite, the one-shot timer is retained by the
    // run loop it is scheduled on, and the block only terminates this process.
    unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(delay.as_secs_f64(), false, &block);
    }
    Ok(())
}

fn ensure_absent(class: &AnyClass) -> Result<(), TerminateError> {
    if class.instance_method(sel!(applicationShouldTerminate:)).is_some() {
        return Err(TerminateError::AlreadyImplemented);
    }
    Ok(())
}

fn add_method(class: &AnyClass) -> Result<(), TerminateError> {
    // SAFETY: `class` is a live Objective-C class, `should_terminate` matches
    // the `Q@:@` encoding exactly, and the caller owns the process's single
    // install attempt. The runtime locks the class while adding the method.
    // The function-pointer cast only erases the signature that the encoding
    // already declares, which is how `objc2` hands implementations to the
    // runtime.
    let added = unsafe {
        class_addMethod(
            std::ptr::from_ref(class).cast_mut(),
            sel!(applicationShouldTerminate:),
            std::mem::transmute::<ShouldTerminate, Imp>(should_terminate),
            SHOULD_TERMINATE_ENCODING.as_ptr(),
        )
    };
    if added.as_bool() {
        Ok(())
    } else {
        Err(TerminateError::Rejected)
    }
}

/// Installs the method on a class without `NSApplication`, for tests.
#[cfg(test)]
fn install_on_class(class: &AnyClass) -> Result<(), TerminateError> {
    ensure_absent(class)?;
    add_method(class)
}

/// The method `AppKit` calls before it terminates.
///
/// It asks the registered callback for one plain answer. It holds no policy,
/// runs no teardown and never waits, so it is safe on the main thread while
/// `AppKit` owns the terminate decision. A panic is contained and answers
/// [`TerminateAnswer::Later`] before it can unwind into Objective-C dispatch.
unsafe extern "C-unwind" fn should_terminate(
    _delegate: *mut AnyObject,
    _selector: Sel,
    _sender: *mut AnyObject,
) -> NSUInteger {
    let answer = HANDLER.get().map_or(TerminateAnswer::Later, |handler| {
        catch_unwind(AssertUnwindSafe(handler)).unwrap_or(TerminateAnswer::Later)
    });
    match answer {
        TerminateAnswer::Later => NSApplicationTerminateReply::TerminateLater.0,
        TerminateAnswer::Now => NSApplicationTerminateReply::TerminateNow.0,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use objc2::runtime::ClassBuilder;

    use super::*;

    fn test_class(name: &CStr) -> &'static AnyClass {
        let superclass = AnyClass::get(c"NSObject").expect("NSObject is registered");
        ClassBuilder::new(name, superclass)
            .expect("allocate the test delegate class")
            .register()
    }

    #[test]
    fn installing_once_succeeds_and_a_second_install_is_a_typed_error() {
        let class = test_class(c"StacheTerminateDelegate");
        assert!(install_on_class(class).is_ok());
        assert_eq!(
            install_on_class(class).err(),
            Some(TerminateError::AlreadyImplemented)
        );
    }

    #[test]
    fn an_existing_selector_is_a_typed_error() {
        let class = test_class(c"StacheTerminatePreexistingDelegate");
        // SAFETY: the test class is live, `preexisting` matches the encoding,
        // and the class does not answer the selector yet.
        unsafe {
            class_addMethod(
                std::ptr::from_ref(class).cast_mut(),
                sel!(applicationShouldTerminate:),
                std::mem::transmute::<ShouldTerminate, Imp>(preexisting),
                SHOULD_TERMINATE_ENCODING.as_ptr(),
            );
        }
        assert_eq!(
            install_on_class(class).err(),
            Some(TerminateError::AlreadyImplemented)
        );
    }

    #[test]
    fn replies_are_refused_off_the_main_thread() {
        assert_eq!(reply_to_terminate(), Err(TerminateError::NotMainThread));
    }

    #[test]
    fn the_registered_callback_answers_and_contains_a_panic() {
        let calls = AtomicUsize::new(0);
        assert!(
            HANDLER
                .set(Box::new(move || match calls.fetch_add(1, Ordering::SeqCst) {
                    0 => TerminateAnswer::Later,
                    1 => TerminateAnswer::Now,
                    _ => panic!("injected callback panic"),
                }))
                .is_ok(),
            "the test owns the first callback registration"
        );
        let delegate = std::ptr::null_mut();
        // SAFETY: the added method ignores every argument, so null is valid.
        let later =
            unsafe { should_terminate(delegate, sel!(applicationShouldTerminate:), delegate) };
        // SAFETY: as above.
        let now =
            unsafe { should_terminate(delegate, sel!(applicationShouldTerminate:), delegate) };
        // SAFETY: as above; the contained panic answers `Later` instead of
        // unwinding through this `extern "C-unwind"` return.
        let panicked =
            unsafe { should_terminate(delegate, sel!(applicationShouldTerminate:), delegate) };
        assert_eq!(later, NSApplicationTerminateReply::TerminateLater.0);
        assert_eq!(now, NSApplicationTerminateReply::TerminateNow.0);
        assert_eq!(panicked, NSApplicationTerminateReply::TerminateLater.0);
    }

    unsafe extern "C-unwind" fn preexisting(
        _delegate: *mut AnyObject,
        _selector: Sel,
        _sender: *mut AnyObject,
    ) -> NSUInteger {
        NSApplicationTerminateReply::TerminateNow.0
    }
}
