//! The real CoreGraphics event-tap layer. It exists only on the owner thread.
//!
//! Ownership and confinement:
//!
//! - [`SystemInput`] is created inside the owner thread by the factory and is
//!   `!Send` (it holds a `PhantomData<*const ()>` and non-`Send` CF types), so
//!   the compiler rejects moving it, its ports, sources or the run loop to
//!   another thread.
//! - The only value that crosses threads is [`RunLoopWake`], which can do
//!   nothing but signal. `CFRunLoopSourceSignal` and `CFRunLoopWakeUp` are
//!   documented as callable from any thread.
//! - The tap callback runs on the owner run loop, so it is single-threaded with
//!   respect to install and removal; the callback context is freed only after
//!   the port is invalidated and the source is removed.
//!
//! Creating a tap never prompts: a missing permission is reported as an error.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{self, NonNull};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFRunLoopMode, CFRunLoopSource, CFRunLoopSourceContext,
    kCFRunLoopDefaultMode,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};
use stache_sdk::input::{Disposition, InputService, TapKind};

use super::convert::{Classified, EventFields, classify, event_mask};
use super::owner::{NativeInput, WakeHandle};

/// The service the callbacks dispatch to, bound after the service exists.
pub type ServiceSlot = OnceLock<Weak<InputService>>;

/// Heap state a tap callback reads through `user_info`.
struct CallbackContext {
    service: Arc<InputService>,
    kind: TapKind,
}

fn default_mode() -> Option<&'static CFRunLoopMode> {
    // SAFETY: `kCFRunLoopDefaultMode` is an immutable, process-lifetime
    // CoreFoundation constant that is never written after load.
    unsafe { kCFRunLoopDefaultMode }
}

/// Signals the owner run loop. It is the only native handle that crosses threads.
struct RunLoopWake {
    source: CFRetained<CFRunLoopSource>,
    run_loop: CFRetained<CFRunLoop>,
}

#[allow(
    clippy::non_send_fields_in_send_ty,
    reason = "only signalling is reachable; see the safety comment"
)]
// SAFETY: the type exposes only `wake`, which calls `CFRunLoopSourceSignal` and
// `CFRunLoopWakeUp`; Apple documents both as thread-safe, and CF retain/release
// is atomic. No port, run-loop mutation or callback is reachable through it.
unsafe impl Send for RunLoopWake {}
// SAFETY: see the `Send` justification; `wake` takes `&self` and only signals.
unsafe impl Sync for RunLoopWake {}

impl WakeHandle for RunLoopWake {
    fn wake(&self) {
        self.source.signal();
        self.run_loop.wake_up();
    }
}

const unsafe extern "C-unwind" fn wake_perform(_info: *mut c_void) {}

/// One tap's native resources, torn down in a fixed order.
struct TapParts {
    run_loop: CFRetained<CFRunLoop>,
    source: Option<CFRetained<CFRunLoopSource>>,
    port: Option<CFRetained<CFMachPort>>,
    /// Dropped last: the callback reads it until the port is invalidated.
    context: Option<Box<CallbackContext>>,
}

impl TapParts {
    const fn new(run_loop: CFRetained<CFRunLoop>, context: Box<CallbackContext>) -> Self {
        Self {
            run_loop,
            source: None,
            port: None,
            context: Some(context),
        }
    }

    const fn complete(&self) -> bool { self.port.is_some() && self.source.is_some() }

    fn teardown(&mut self) {
        if let Some(port) = &self.port {
            CGEvent::tap_enable(port, false);
        }
        if let Some(source) = self.source.take() {
            self.run_loop.remove_source(Some(&source), default_mode());
            source.invalidate();
        }
        if let Some(port) = self.port.take() {
            port.invalidate();
        }
        self.context = None;
    }
}

impl Drop for TapParts {
    fn drop(&mut self) { self.teardown(); }
}

/// The owner-thread native layer.
pub struct SystemInput {
    service: Arc<ServiceSlot>,
    run_loop: CFRetained<CFRunLoop>,
    wake_source: CFRetained<CFRunLoopSource>,
    wake: Arc<RunLoopWake>,
    suppressing: Option<TapParts>,
    listen_only: Option<TapParts>,
    /// Makes the type `!Send` and `!Sync` independent of its fields.
    _confined: PhantomData<*const ()>,
}

impl SystemInput {
    /// Builds the layer on the current thread, which becomes the owner thread.
    pub fn new(service: Arc<ServiceSlot>) -> Result<Self, String> {
        let run_loop = CFRunLoop::current().ok_or("the owner thread has no run loop")?;
        let mut context = CFRunLoopSourceContext {
            version: 0,
            info: ptr::null_mut(),
            retain: None,
            release: None,
            copyDescription: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: Some(wake_perform),
        };
        // SAFETY: `context` is a valid, initialised version-0 context for the
        // call; CoreFoundation copies it. `perform` is a no-op that ignores its
        // null `info`.
        let source = unsafe { CFRunLoopSource::new(None, 0, &raw mut context) }
            .ok_or("cannot create the owner wake source")?;
        run_loop.add_source(Some(&source), default_mode());
        let wake = Arc::new(RunLoopWake {
            source: source.clone(),
            run_loop: run_loop.clone(),
        });
        Ok(Self {
            service,
            run_loop,
            wake_source: source,
            wake,
            suppressing: None,
            listen_only: None,
            _confined: PhantomData,
        })
    }

    const fn slot(&mut self, kind: TapKind) -> &mut Option<TapParts> {
        match kind {
            TapKind::Suppressing => &mut self.suppressing,
            TapKind::ListenOnly => &mut self.listen_only,
        }
    }
}

impl Drop for SystemInput {
    fn drop(&mut self) {
        self.suppressing = None;
        self.listen_only = None;
        self.run_loop.remove_source(Some(&self.wake_source), default_mode());
        self.wake_source.invalidate();
    }
}

impl NativeInput for SystemInput {
    fn wake_handle(&self) -> Arc<dyn WakeHandle> { Arc::clone(&self.wake) as Arc<dyn WakeHandle> }

    fn install(&mut self, kind: TapKind) -> Result<(), String> {
        if self.slot(kind).as_ref().is_some_and(TapParts::complete) {
            return Ok(());
        }
        // A partial earlier install is cleaned before a fresh attempt.
        *self.slot(kind) = None;
        let service =
            self.service.get().and_then(Weak::upgrade).ok_or("the input service is gone")?;
        let run_loop = self.run_loop.clone();
        let parts = self.slot(kind).insert(TapParts::new(
            run_loop,
            Box::new(CallbackContext { service, kind }),
        ));
        let user_info = parts.context.as_deref().map_or(ptr::null_mut(), |context| {
            ptr::from_ref(context).cast_mut().cast()
        });
        let options = match kind {
            TapKind::Suppressing => CGEventTapOptions::Default,
            TapKind::ListenOnly => CGEventTapOptions::ListenOnly,
        };
        // SAFETY: `tap_callback` has the exact `CGEventTapCallBack`
        // signature and contains every panic. `user_info` points into the boxed
        // context stored in `parts`, which lives until `teardown` has
        // invalidated the port and removed the source on this same thread, and
        // it is never mutated while shared.
        let port = unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::HIDEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                options,
                event_mask(),
                Some(tap_callback),
                user_info,
            )
        }
        .ok_or_else(|| creation_failure(kind))?;
        CGEvent::tap_enable(&port, false);
        let created = CFMachPort::new_run_loop_source(None, Some(&port), 0);
        parts.port = Some(port);
        let source = created.ok_or("cannot create the event tap run-loop source")?;
        parts.run_loop.add_source(Some(&source), default_mode());
        parts.source = Some(source);
        Ok(())
    }

    fn enable(&mut self, kind: TapKind) -> Result<(), String> {
        let parts = self
            .slot(kind)
            .as_ref()
            .filter(|parts| parts.complete())
            .ok_or("the event tap is not installed")?;
        let port = parts.port.as_ref().ok_or("the event tap is not installed")?;
        CGEvent::tap_enable(port, true);
        if CGEvent::tap_is_enabled(port) {
            Ok(())
        } else {
            Err("the event tap did not enable".to_owned())
        }
    }

    fn remove(&mut self, kind: TapKind) -> Result<(), String> {
        // Dropping tears down whatever exists, including a partial install.
        *self.slot(kind) = None;
        Ok(())
    }

    fn park(&mut self, limit: Duration) {
        CFRunLoop::run_in_mode(default_mode(), limit.as_secs_f64(), true);
    }
}

fn creation_failure(kind: TapKind) -> String {
    let permission = match kind {
        TapKind::Suppressing => "Accessibility",
        TapKind::ListenOnly => "Input Monitoring",
    };
    format!(
        "cannot create the {kind:?} event tap: macOS refused it, most likely because {permission} \
         access is not granted to Stache; no permission prompt was requested"
    )
}

fn is_disabled_notice(event_type: CGEventType) -> bool {
    event_type == CGEventType::TapDisabledByTimeout
        || event_type == CGEventType::TapDisabledByUserInput
}

fn read_fields(event_type: CGEventType, event: &CGEvent) -> EventFields {
    if is_disabled_notice(event_type) {
        return EventFields {
            event_type: event_type.0,
            key_code: 0,
            autorepeat: 0,
            button: 0,
            flags: 0,
            timestamp_ns: 0,
            location: (0.0, 0.0),
        };
    }
    let field = |field| CGEvent::integer_value_field(Some(event), field);
    // A plain field read of the tapped event's own point, no allocation.
    let point = CGEvent::location(Some(event));
    EventFields {
        event_type: event_type.0,
        key_code: field(CGEventField::KeyboardEventKeycode),
        autorepeat: field(CGEventField::KeyboardEventAutorepeat),
        button: field(CGEventField::MouseEventButtonNumber),
        flags: CGEvent::flags(Some(event)).0,
        timestamp_ns: CGEvent::timestamp(Some(event)),
        location: (point.x, point.y),
    }
}

/// Returns whether the event must be consumed. Only the suppressing tap can.
fn route(context: &CallbackContext, event_type: CGEventType, event: &CGEvent) -> bool {
    match classify(read_fields(event_type, event)) {
        Classified::TapDisabled => {
            context.service.tap_disabled(context.kind);
            false
        }
        Classified::Ignore => false,
        Classified::Input(raw) => {
            let disposition = context.service.dispatch(context.kind, raw);
            context.kind == TapKind::Suppressing && disposition == Disposition::Suppress
        }
    }
}

unsafe extern "C-unwind" fn tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    let passthrough = event.as_ptr();
    if user_info.is_null() {
        return passthrough;
    }
    // SAFETY: `user_info` is the boxed `CallbackContext` registered by
    // `install`; it outlives every callback because it is freed only after the
    // port is invalidated, and both happen on this thread.
    let context = unsafe { &*user_info.cast::<CallbackContext>() };
    // SAFETY: CoreGraphics passes a valid event that lives for this call.
    let event = unsafe { event.as_ref() };
    // Fail open: a contained panic passes the event through.
    match catch_unwind(AssertUnwindSafe(|| route(context, event_type, event))) {
        Ok(true) => ptr::null_mut(),
        Ok(false) | Err(_) => passthrough,
    }
}
