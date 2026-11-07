//! The real Carbon native layer.
//!
//! Hand-written `extern "C"` declarations for the small slice of Carbon this
//! crate needs. Every `EventHandlerRef`, `EventHotKeyRef` and the handler's
//! context pointer lives in [`SystemNative`], which holds raw pointers and
//! `Rc` state and is therefore `!Send` by its real types: there is no
//! `unsafe impl Send` or `Sync` anywhere. The Carbon callback only ever runs on
//! the main thread, and it refuses to touch the context anywhere else.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;

use super::native::{
    Dispatch, Disposition, Native, NativeHotKeyId, RawEvent, RawEventKind, RegisterFailure,
    SIGNATURE, STATUS_HOT_KEY_EXISTS,
};
use crate::keyboard_layout::observer::LayoutObserver;
use crate::keyboard_layout::{LayoutRead, system};
use crate::main_thread::is_main_thread;

type OsStatus = i32;
type EventHandlerCallRef = *mut c_void;
type EventRef = *mut c_void;
type EventTargetRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventHotKeyRef = *mut c_void;
type EventHandlerProc =
    unsafe extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OsStatus;

const NO_ERR: OsStatus = 0;
const EVENT_NOT_HANDLED: OsStatus = -9874;

const fn four_char(bytes: [u8; 4]) -> u32 { u32::from_be_bytes(bytes) }

/// `kEventParamDirectObject`.
const PARAM_DIRECT_OBJECT: u32 = four_char(*b"----");
/// `typeEventHotKeyID`.
const TYPE_HOT_KEY_ID: u32 = four_char(*b"hkid");
/// `kEventClassKeyboard`.
const CLASS_KEYBOARD: u32 = four_char(*b"keyb");
const KIND_HOT_KEY_PRESSED: u32 = 5;
const KIND_HOT_KEY_RELEASED: u32 = 6;

/// `EventHotKeyID`: two 32-bit fields, no padding.
#[repr(C)]
#[derive(Clone, Copy)]
struct EventHotKeyId {
    signature: u32,
    id: u32,
}

/// `EventTypeSpec`: two 32-bit fields, no padding.
#[repr(C)]
#[derive(Clone, Copy)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    /// The application event target; always valid, owned by the system.
    fn GetApplicationEventTarget() -> EventTargetRef;

    /// Installs a handler on a target. `handler_ref` receives the new handler.
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: EventHandlerProc,
        num_types: u32,
        types: *const EventTypeSpec,
        user_data: *mut c_void,
        handler_ref: *mut EventHandlerRef,
    ) -> OsStatus;

    /// Removes an installed handler.
    fn RemoveEventHandler(handler_ref: EventHandlerRef) -> OsStatus;

    /// Registers a global hotkey; `hot_key_ref` receives the registration.
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        hot_key_id: EventHotKeyId,
        target: EventTargetRef,
        options: u32,
        hot_key_ref: *mut EventHotKeyRef,
    ) -> OsStatus;

    /// Releases a registration.
    fn UnregisterEventHotKey(hot_key_ref: EventHotKeyRef) -> OsStatus;

    /// Reads one event parameter into `data`.
    fn GetEventParameter(
        event: EventRef,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        buffer_size: usize,
        actual_size: *mut usize,
        data: *mut c_void,
    ) -> OsStatus;

    /// The kind of an event.
    fn GetEventKind(event: EventRef) -> u32;

    /// Passes an event to the next handler.
    fn CallNextEventHandler(next_handler: EventHandlerCallRef, event: EventRef) -> OsStatus;
}

/// What the Carbon handler receives as its `user_data`: the decision closure.
/// It is an `Rc`, so the context is main-thread state by type.
struct HandlerContext {
    dispatch: Dispatch,
}

struct InstalledHandler {
    handler: EventHandlerRef,
    /// Owned `Box<HandlerContext>`; freed only after `RemoveEventHandler`
    /// succeeded (or in `Drop`, after a best-effort removal).
    context: *mut HandlerContext,
}

/// The real Carbon and `TIS` layer. `!Send`: it owns raw Carbon references.
pub(super) struct SystemNative {
    handler: Option<InstalledHandler>,
    keys: HashMap<u32, EventHotKeyRef>,
    observer: Option<LayoutObserver>,
}

impl SystemNative {
    pub(super) fn new() -> Self {
        Self {
            handler: None,
            keys: HashMap::new(),
            observer: None,
        }
    }
}

unsafe extern "C" fn handle_event(
    next: EventHandlerCallRef,
    event: EventRef,
    user_data: *mut c_void,
) -> OsStatus {
    let decision = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // The context is main-thread state (an `Rc`). Carbon delivers
        // application-target hotkey events on the main thread; if that ever
        // proves false, forward instead of touching it.
        if !is_main_thread() || user_data.is_null() || event.is_null() {
            return Disposition::Forward;
        }
        // SAFETY: `user_data` is the `Box<HandlerContext>` leaked by
        // `install_handler`; it is freed only after `RemoveEventHandler`
        // succeeds, which cannot run concurrently with this callback because
        // both execute on the main thread.
        let context = unsafe { &*user_data.cast::<HandlerContext>() };
        // SAFETY: `event` is the live event Carbon passed to this callback.
        let kind = match unsafe { GetEventKind(event) } {
            KIND_HOT_KEY_PRESSED => RawEventKind::Pressed,
            KIND_HOT_KEY_RELEASED => RawEventKind::Released,
            _ => RawEventKind::Other,
        };
        let mut id = EventHotKeyId { signature: 0, id: 0 };
        // SAFETY: `event` is live; `id` is a local of exactly the size passed;
        // the optional out-parameters are null as the API allows.
        let status = unsafe {
            GetEventParameter(
                event,
                PARAM_DIRECT_OBJECT,
                TYPE_HOT_KEY_ID,
                std::ptr::null_mut(),
                std::mem::size_of::<EventHotKeyId>(),
                std::ptr::null_mut(),
                (&raw mut id).cast(),
            )
        };
        let hot_key = if status == NO_ERR {
            Ok((id.signature, id.id))
        } else {
            Err(status)
        };
        (context.dispatch)(RawEvent { kind, hot_key })
    }))
    .unwrap_or(Disposition::Forward);
    match decision {
        Disposition::Handled => NO_ERR,
        // SAFETY: `next` and `event` are the values Carbon passed to this call.
        Disposition::Forward => unsafe { CallNextEventHandler(next, event) },
    }
}

impl Native for SystemNative {
    fn install_handler(&mut self, dispatch: Dispatch) -> Result<(), i32> {
        if self.handler.is_some() {
            return Ok(());
        }
        let types = [
            EventTypeSpec {
                event_class: CLASS_KEYBOARD,
                event_kind: KIND_HOT_KEY_PRESSED,
            },
            EventTypeSpec {
                event_class: CLASS_KEYBOARD,
                event_kind: KIND_HOT_KEY_RELEASED,
            },
        ];
        let context = Box::into_raw(Box::new(HandlerContext { dispatch }));
        let mut handler: EventHandlerRef = std::ptr::null_mut();
        // SAFETY: the target is the system's application target; `types` is a
        // live two-element array; `context` is a valid heap pointer that
        // outlives the handler (freed only after removal); `handler` is a local
        // out-parameter.
        let status = unsafe {
            InstallEventHandler(
                GetApplicationEventTarget(),
                handle_event,
                2,
                types.as_ptr(),
                context.cast(),
                &raw mut handler,
            )
        };
        if status == NO_ERR && !handler.is_null() {
            self.handler = Some(InstalledHandler { handler, context });
            Ok(())
        } else {
            // SAFETY: installation failed, so Carbon holds no copy of `context`;
            // reclaiming the box we just leaked is the only owner.
            drop(unsafe { Box::from_raw(context) });
            Err(if status == NO_ERR {
                EVENT_NOT_HANDLED
            } else {
                status
            })
        }
    }

    fn remove_handler(&mut self) -> Result<(), i32> {
        let Some(installed) = self.handler.as_ref() else {
            return Ok(());
        };
        // SAFETY: `installed.handler` came from a successful `InstallEventHandler`
        // and has not been removed (it is still stored).
        let status = unsafe { RemoveEventHandler(installed.handler) };
        if status != NO_ERR {
            // Keep the handler and its context alive: Carbon may still call it.
            return Err(status);
        }
        if let Some(installed) = self.handler.take() {
            // SAFETY: removal succeeded, so Carbon no longer references the
            // context; this reclaims the single owner.
            drop(unsafe { Box::from_raw(installed.context) });
        }
        Ok(())
    }

    fn register(
        &mut self,
        key_code: u16,
        carbon_modifiers: u32,
        id: NativeHotKeyId,
    ) -> Result<(), RegisterFailure> {
        let mut hot_key: EventHotKeyRef = std::ptr::null_mut();
        // SAFETY: the target is the system's application target; the ID is a
        // plain value; `hot_key` is a local out-parameter written on success.
        let status = unsafe {
            RegisterEventHotKey(
                u32::from(key_code),
                carbon_modifiers,
                EventHotKeyId { signature: SIGNATURE, id: id.0 },
                GetApplicationEventTarget(),
                0,
                &raw mut hot_key,
            )
        };
        match status {
            NO_ERR if !hot_key.is_null() => {
                self.keys.insert(id.0, hot_key);
                Ok(())
            }
            NO_ERR => Err(RegisterFailure::Failed(EVENT_NOT_HANDLED)),
            STATUS_HOT_KEY_EXISTS => Err(RegisterFailure::Occupied(status)),
            other => Err(RegisterFailure::Failed(other)),
        }
    }

    fn unregister(&mut self, id: NativeHotKeyId) -> Result<(), i32> {
        let Some(&hot_key) = self.keys.get(&id.0) else {
            return Ok(());
        };
        // SAFETY: `hot_key` came from a successful `RegisterEventHotKey` and is
        // still stored, so it has not been released.
        let status = unsafe { UnregisterEventHotKey(hot_key) };
        if status == NO_ERR {
            self.keys.remove(&id.0);
            Ok(())
        } else {
            Err(status)
        }
    }

    fn read_layout(&mut self) -> LayoutRead { system::read_current_layout() }

    fn observe_layout(&mut self, hint: Arc<dyn Fn() + Send + Sync>) -> Result<(), String> {
        if self.observer.is_some() {
            return Ok(());
        }
        self.observer = Some(LayoutObserver::install(hint)?);
        Ok(())
    }

    fn unobserve_layout(&mut self) -> Result<(), String> {
        if let Some(observer) = self.observer.take() {
            observer.remove();
        }
        Ok(())
    }
}

impl Drop for SystemNative {
    fn drop(&mut self) {
        // Best effort: the owner's `stop` is the reported path. Releasing here
        // keeps a dropped layer from leaving a live global hotkey behind.
        for (_, hot_key) in self.keys.drain() {
            // SAFETY: each stored reference came from a successful register and
            // is released exactly once here.
            let _ = unsafe { UnregisterEventHotKey(hot_key) };
        }
        let _ = self.remove_handler();
        let _ = self.unobserve_layout();
    }
}
