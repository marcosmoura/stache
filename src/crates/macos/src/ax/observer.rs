//! `AXObserver` wrapper: create, add/remove notification and run-loop source.
//!
//! # Callback purity
//!
//! The only code the system runs from an AX callback is [`Context::record`]. It
//! maps the notification name to the closed [`AxNotification`] enum, finds or
//! retains the delivered element and appends `(notification, ElementKey)` to a
//! bounded owner-local mailbox. It performs **no attribute read or write, no
//! AX message, no policy, no wait and no allocation beyond a bounded `Vec`
//! push**; every operation is a local `CFEqual`/`CFRetain`. Nothing but the pure
//! enum and the opaque [`ElementKey`] is handed out; the owner later resolves a
//! key to an element with [`AxObserver::element`] outside the callback.
//!
//! The mailbox is bounded ([`MAX_PENDING_EVENTS`], [`MAX_INTERNED_ELEMENTS`]).
//! Overflow, a re-entrant borrow and an unrecognised element are never silently
//! lost: they set [`AxDrain::overflowed`], so the owner re-enumerates instead of
//! trusting a possibly incomplete event stream. The body is wrapped in
//! `catch_unwind` because unwinding out of an `extern "C"` callback aborts;
//! under the release profile's `panic = "abort"` that wrapper cannot recover an
//! arbitrary panic, so the audited body above contains no panicking operation.
//!
//! # Ownership
//!
//! The observer, its run-loop source, the retained elements and the callback
//! context are owner-thread-local (`!Send`, `!Sync`). The callback context is a
//! heap allocation owned by the observer and handed to the system as an opaque
//! `refcon` pointer (a reference cast, never an integer cast); it outlives the
//! run-loop source because [`Drop`] detaches the source first. The run loop that
//! delivers callbacks must be the owner thread's.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;

use core_foundation::base::{CFEqual, CFGetTypeID, CFRetain, TCFType};
use core_foundation::runloop::{CFRunLoop, CFRunLoopSource, kCFRunLoopDefaultMode};
use core_foundation::string::{CFString, CFStringRef};

use super::api::{AXUIElementGetTypeID, AxElement, AxError, AxUiElementRef};
use super::ffi::{
    AXObserverAddNotification, AXObserverCreate, AXObserverGetRunLoopSource,
    AXObserverRemoveNotification, AxObserverRef,
};
use super::names::notification as names;

/// The most undrained events held; further events set the overflow flag.
pub const MAX_PENDING_EVENTS: usize = 256;

/// The most callback-delivered, unregistered elements held at once.
pub const MAX_INTERNED_ELEMENTS: usize = 256;

/// A notification the adapters observe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AxNotification {
    /// `AXWindowCreated`.
    WindowCreated,
    /// `AXUIElementDestroyed`.
    ElementDestroyed,
    /// `AXFocusedWindowChanged`.
    FocusedWindowChanged,
    /// `AXMainWindowChanged`.
    MainWindowChanged,
    /// `AXWindowMoved`.
    WindowMoved,
    /// `AXWindowResized`.
    WindowResized,
    /// `AXWindowMiniaturized`.
    WindowMiniaturized,
    /// `AXWindowDeminiaturized`.
    WindowDeminiaturized,
    /// `AXApplicationHidden`.
    ApplicationHidden,
    /// `AXApplicationShown`.
    ApplicationShown,
    /// `AXTitleChanged`.
    TitleChanged,
    /// Dock: Mission Control (all windows) was shown.
    ExposeShowAllWindows,
    /// Dock: App Exposé (front windows) was shown.
    ExposeShowFrontWindows,
    /// Dock: Show Desktop was shown.
    ExposeShowDesktop,
    /// Dock: Mission Control, App Exposé or Show Desktop ended.
    ExposeExit,
}

impl AxNotification {
    /// Every observed notification.
    pub const ALL: [Self; 11] = [
        Self::WindowCreated,
        Self::ElementDestroyed,
        Self::FocusedWindowChanged,
        Self::MainWindowChanged,
        Self::WindowMoved,
        Self::WindowResized,
        Self::WindowMiniaturized,
        Self::WindowDeminiaturized,
        Self::ApplicationHidden,
        Self::ApplicationShown,
        Self::TitleChanged,
    ];
    /// The Dock notifications. They are deliberately **not** in [`Self::ALL`]:
    /// the window adapters register `ALL`-derived sets and must stay unchanged.
    /// Names are unverified until the part 05 native check (see `names`).
    pub const DOCK: [Self; 4] = [
        Self::ExposeShowAllWindows,
        Self::ExposeShowFrontWindows,
        Self::ExposeShowDesktop,
        Self::ExposeExit,
    ];

    /// The `kAX*Notification` string value.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::WindowCreated => names::WINDOW_CREATED,
            Self::ElementDestroyed => names::ELEMENT_DESTROYED,
            Self::FocusedWindowChanged => names::FOCUSED_WINDOW_CHANGED,
            Self::MainWindowChanged => names::MAIN_WINDOW_CHANGED,
            Self::WindowMoved => names::WINDOW_MOVED,
            Self::WindowResized => names::WINDOW_RESIZED,
            Self::WindowMiniaturized => names::WINDOW_MINIATURIZED,
            Self::WindowDeminiaturized => names::WINDOW_DEMINIATURIZED,
            Self::ApplicationHidden => names::APPLICATION_HIDDEN,
            Self::ApplicationShown => names::APPLICATION_SHOWN,
            Self::TitleChanged => names::TITLE_CHANGED,
            Self::ExposeShowAllWindows => names::EXPOSE_SHOW_ALL_WINDOWS,
            Self::ExposeShowFrontWindows => names::EXPOSE_SHOW_FRONT_WINDOWS,
            Self::ExposeShowDesktop => names::EXPOSE_SHOW_DESKTOP,
            Self::ExposeExit => names::EXPOSE_EXIT,
        }
    }
}

/// An opaque key for an element held by one [`AxObserver`].
///
/// It is meaningful only to the observer that issued it, on that observer's
/// owner thread; resolve it with [`AxObserver::element`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ElementKey(u64);

#[cfg(any(test, feature = "testing"))]
impl ElementKey {
    /// A key from a raw value, for fake observers.
    #[must_use]
    pub const fn from_raw(value: u64) -> Self { Self(value) }

    /// The raw value, for fake observers.
    #[must_use]
    pub const fn raw_for_fakes(self) -> u64 { self.0 }
}

/// One recorded callback: a pure notification and an opaque element key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AxEvent {
    /// What happened.
    pub notification: AxNotification,
    /// Which element, as an owner-local key.
    pub element: ElementKey,
}

/// Everything recorded since the previous drain.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AxDrain {
    /// Events in delivery order.
    pub events: Vec<AxEvent>,
    /// Whether any callback was dropped (full mailbox, re-entrant borrow or an
    /// unrecognised delivery). The owner must re-enumerate to reconcile.
    pub overflowed: bool,
}

struct Entry<E> {
    key: ElementKey,
    element: E,
    /// Pinned by `add_notification`; released only by the owner.
    registered: bool,
}

/// The bounded owner-local event store, generic so its logic is tested without
/// native elements.
pub(super) struct Mailbox<E> {
    next_key: u64,
    entries: Vec<Entry<E>>,
    interned: usize,
    events: VecDeque<AxEvent>,
    overflowed: bool,
}

impl<E> Mailbox<E> {
    pub(super) const fn new() -> Self {
        Self {
            next_key: 1,
            entries: Vec::new(),
            interned: 0,
            events: VecDeque::new(),
            overflowed: false,
        }
    }

    const fn allocate(&mut self) -> ElementKey {
        let key = ElementKey(self.next_key);
        self.next_key += 1;
        key
    }

    /// Pins an element, reusing the key of an equal registered one.
    pub(super) fn register(&mut self, element: E, matches: impl Fn(&E) -> bool) -> ElementKey {
        if let Some(entry) = self.entries.iter_mut().find(|entry| matches(&entry.element)) {
            if !entry.registered {
                entry.registered = true;
                self.interned -= 1;
            }
            return entry.key;
        }
        let key = self.allocate();
        self.entries.push(Entry { key, element, registered: true });
        key
    }

    /// The callback body: bounded, no native call except through `matches` and
    /// `retain`, which the caller restricts to local `CFEqual`/`CFRetain`.
    pub(super) fn record(
        &mut self,
        notification: AxNotification,
        matches: impl Fn(&E) -> bool,
        retain: impl FnOnce() -> E,
    ) {
        if self.events.len() >= MAX_PENDING_EVENTS {
            self.overflowed = true;
            return;
        }
        let key = if let Some(entry) = self.entries.iter().find(|entry| matches(&entry.element)) {
            entry.key
        } else if self.interned < MAX_INTERNED_ELEMENTS {
            let key = self.allocate();
            self.entries.push(Entry {
                key,
                element: retain(),
                registered: false,
            });
            self.interned += 1;
            key
        } else {
            self.overflowed = true;
            return;
        };
        self.events.push_back(AxEvent { notification, element: key });
    }

    pub(super) fn drain(&mut self) -> AxDrain {
        AxDrain {
            events: self.events.drain(..).collect(),
            overflowed: std::mem::take(&mut self.overflowed),
        }
    }

    pub(super) fn element(&self, key: ElementKey) -> Option<&E> {
        self.entries.iter().find(|entry| entry.key == key).map(|entry| &entry.element)
    }

    /// Whether an undrained event still names `key`.
    pub(super) fn references(&self, key: ElementKey) -> bool {
        self.events.iter().any(|event| event.element == key)
    }

    /// Drops an unregistered (callback-delivered) element.
    pub(super) fn release(&mut self, key: ElementKey) {
        if let Some(index) =
            self.entries.iter().position(|entry| entry.key == key && !entry.registered)
        {
            self.entries.swap_remove(index);
            self.interned -= 1;
        }
    }

    /// Drops an element regardless of how it was held.
    pub(super) fn forget(&mut self, key: ElementKey) {
        if let Some(index) = self.entries.iter().position(|entry| entry.key == key) {
            let entry = self.entries.swap_remove(index);
            if !entry.registered {
                self.interned -= 1;
            }
        }
    }

    #[cfg(test)]
    pub(super) const fn interned(&self) -> usize { self.interned }
}

/// The state the system callback reaches through `refcon`.
struct Context {
    mailbox: RefCell<Mailbox<AxElement>>,
    /// Set when a callback could not be recorded (re-entrant borrow).
    dropped: Cell<bool>,
    ax_type: usize,
    names: Vec<(AxNotification, CFString)>,
}

impl Context {
    fn new() -> Self {
        Self {
            mailbox: RefCell::new(Mailbox::new()),
            dropped: Cell::new(false),
            // SAFETY: no arguments; returns the process-constant type id.
            ax_type: unsafe { AXUIElementGetTypeID() },
            names: AxNotification::ALL
                .iter()
                .chain(AxNotification::DOCK.iter())
                .map(|&notification| (notification, CFString::new(notification.name())))
                .collect(),
        }
    }

    /// The audited callback body. See the module documentation.
    fn record(&self, element: AxUiElementRef, notification: CFStringRef) {
        if element.is_null() || notification.is_null() {
            self.dropped.set(true);
            return;
        }
        // SAFETY: `element` is a live CF object for the duration of the callback.
        if unsafe { CFGetTypeID(element) } != self.ax_type {
            self.dropped.set(true);
            return;
        }
        let Some(kind) = self.names.iter().find_map(|(kind, name)| {
            // SAFETY: both are live CF objects; CFEqual only reads them.
            (unsafe { CFEqual(name.as_concrete_TypeRef().cast(), notification.cast()) } != 0)
                .then_some(*kind)
        }) else {
            self.dropped.set(true);
            return;
        };
        let Ok(mut mailbox) = self.mailbox.try_borrow_mut() else {
            self.dropped.set(true);
            return;
        };
        mailbox.record(
            kind,
            // SAFETY: both are live AXUIElements; CFEqual is local (it compares
            // the underlying pid and element identifier) and sends no message.
            |held| unsafe { CFEqual(held.as_ref(), element) } != 0,
            || {
                // SAFETY: `element` is a live AXUIElement (type checked above);
                // CFRetain adds the reference the new wrapper owns.
                unsafe {
                    CFRetain(element);
                    AxElement::from_owned(NonNull::new_unchecked(element.cast_mut()))
                }
            },
        );
    }
}

/// `AXObserverCallback`. Runs on the thread whose run loop holds the source.
unsafe extern "C" fn trampoline(
    _observer: AxObserverRef,
    element: AxUiElementRef,
    notification: CFStringRef,
    refcon: *mut c_void,
) {
    // Unwinding out of an `extern "C"` function aborts the process.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `refcon` is the `Context` box owned by the `AxObserver` that
        // registered this callback. It is freed only after the run-loop source
        // is detached and the observer released, and callbacks run on the owner
        // thread only, so a shared reference is valid and unaliased by `&mut`.
        if let Some(context) = unsafe { refcon.cast::<Context>().as_ref() } {
            context.record(element, notification);
        }
    }));
}

/// An owned `AXObserverRef` with its run-loop source and event mailbox.
/// `!Send` and `!Sync`.
pub struct AxObserver {
    raw: NonNull<c_void>,
    source: CFRunLoopSource,
    attached: Option<CFRunLoop>,
    registrations: Vec<(ElementKey, AxNotification)>,
    names: Vec<(AxNotification, CFString)>,
    context: Box<Context>,
    pid: i32,
    _owner_local: PhantomData<*const ()>,
}

impl AxObserver {
    /// Creates an observer for `pid`'s notifications. Sends no AX message and
    /// attaches nothing; call [`Self::attach_current_run_loop`] to receive
    /// callbacks.
    ///
    /// # Errors
    ///
    /// The AX failure, or [`AxError::InvalidElement`] for a null result.
    pub fn new(pid: i32) -> Result<Self, AxError> {
        let mut raw: AxObserverRef = std::ptr::null_mut();
        // SAFETY: `trampoline` has the `AXObserverCallback` signature and `raw`
        // is a valid out pointer. On success the callee returns a +1 observer
        // (Create rule) that this wrapper releases exactly once.
        let code = unsafe { AXObserverCreate(pid, trampoline, &raw mut raw) };
        if code != 0 {
            return Err(AxError::from_code(code));
        }
        let raw = NonNull::new(raw).ok_or(AxError::InvalidElement)?;
        // SAFETY: a valid observer; the Get-rule source is borrowed from it, so
        // the wrapper retains it (`wrap_under_get_rule`) and outlives nothing.
        let source = unsafe {
            let source = AXObserverGetRunLoopSource(raw.as_ptr());
            if source.is_null() {
                core_foundation::base::CFRelease(raw.as_ptr().cast_const());
                return Err(AxError::InvalidElement);
            }
            CFRunLoopSource::wrap_under_get_rule(source.cast())
        };
        Ok(Self {
            raw,
            source,
            attached: None,
            registrations: Vec::new(),
            names: AxNotification::ALL
                .iter()
                .chain(AxNotification::DOCK.iter())
                .map(|&notification| (notification, CFString::new(notification.name())))
                .collect(),
            context: Box::new(Context::new()),
            pid,
            _owner_local: PhantomData,
        })
    }

    /// Whether the observed process no longer exists. A registration on an
    /// exited process cannot be removed (the removal fails with
    /// `CannotComplete`), and it died with the process. A reused pid reads as
    /// alive, which only keeps a registration owed.
    #[must_use]
    pub fn target_exited(&self) -> bool { process_exited(self.pid) }

    /// Adds the observer's source to the **calling thread's** run loop. Call it
    /// on the owner thread; callbacks are then delivered while that thread runs
    /// its run loop. Idempotent.
    pub fn attach_current_run_loop(&mut self) {
        if self.attached.is_some() {
            return;
        }
        let run_loop = CFRunLoop::get_current();
        // SAFETY: `kCFRunLoopDefaultMode` is an immutable CF constant.
        run_loop.add_source(&self.source, unsafe { kCFRunLoopDefaultMode });
        self.attached = Some(run_loop);
    }

    /// Removes the source from the run loop it was attached to. Idempotent.
    pub fn detach_run_loop(&mut self) {
        if let Some(run_loop) = self.attached.take() {
            // SAFETY: `kCFRunLoopDefaultMode` is an immutable CF constant.
            run_loop.remove_source(&self.source, unsafe { kCFRunLoopDefaultMode });
        }
    }

    /// Whether the source is attached to a run loop.
    #[must_use]
    pub const fn is_attached(&self) -> bool { self.attached.is_some() }

    fn name_of(&self, notification: AxNotification) -> &CFString {
        self.names
            .iter()
            .find(|(kind, _)| *kind == notification)
            .map(|(_, name)| name)
            .expect("every notification has a cached name")
    }

    /// Registers `notification` on `element` and returns its owner-local key.
    ///
    /// The observer retains the element. The messaging timeout of the element
    /// object should already be set by the caller.
    ///
    /// # Errors
    ///
    /// The AX failure. On failure nothing stays registered.
    pub fn add_notification(
        &mut self,
        element: &AxElement,
        notification: AxNotification,
    ) -> Result<ElementKey, AxError> {
        let key = {
            let mut mailbox = self.context.mailbox.borrow_mut();
            mailbox.register(element.retained(), |held| {
                // SAFETY: both are live AXUIElements; CFEqual is local.
                unsafe { CFEqual(held.as_ref(), element.as_ref()) != 0 }
            })
        };
        let refcon = std::ptr::from_ref::<Context>(&self.context).cast_mut().cast::<c_void>();
        // SAFETY: live observer, element and notification name; `refcon` is the
        // context box that outlives the registration (see the module docs).
        let code = unsafe {
            AXObserverAddNotification(
                self.raw.as_ptr(),
                element.as_ref(),
                self.name_of(notification).as_concrete_TypeRef(),
                refcon,
            )
        };
        if code != 0 {
            if !self.registrations.iter().any(|(held, _)| *held == key) {
                self.context.mailbox.borrow_mut().forget(key);
            }
            return Err(AxError::from_code(code));
        }
        if !self.registrations.contains(&(key, notification)) {
            self.registrations.push((key, notification));
        }
        Ok(key)
    }

    /// Removes one registration. When it was the key's last registration the
    /// retained element is released.
    ///
    /// # Errors
    ///
    /// The AX failure (for example [`AxError::InvalidElement`] for a destroyed
    /// element). On failure the registration and the element stay owned so the
    /// owner can retry; nothing is dropped silently.
    pub fn remove_notification(
        &mut self,
        key: ElementKey,
        notification: AxNotification,
    ) -> Result<(), AxError> {
        if !self.registrations.contains(&(key, notification)) {
            return Ok(());
        }
        let element = self.context.mailbox.borrow().element(key).map(AxElement::retained);
        let Some(element) = element else {
            self.registrations.retain(|entry| *entry != (key, notification));
            return Ok(());
        };
        // SAFETY: live observer, element and notification name.
        let code = unsafe {
            AXObserverRemoveNotification(
                self.raw.as_ptr(),
                element.as_ref(),
                self.name_of(notification).as_concrete_TypeRef(),
            )
        };
        if code != 0 {
            return Err(AxError::from_code(code));
        }
        self.registrations.retain(|entry| *entry != (key, notification));
        if !self.registrations.iter().any(|(held, _)| *held == key) {
            self.context.mailbox.borrow_mut().forget(key);
        }
        Ok(())
    }

    /// The events recorded since the previous drain.
    pub fn drain(&mut self) -> AxDrain {
        let mut drain = self.context.mailbox.borrow_mut().drain();
        drain.overflowed |= self.context.dropped.replace(false);
        drain
    }

    /// Forgets every registration of `key` without an AX call, then releases the
    /// element.
    ///
    /// Valid only after the system proved the element is gone (the
    /// `AXUIElementDestroyed` notification was delivered, or a removal failed
    /// with [`AxError::InvalidElement`]): the system drops a destroyed element's
    /// registrations itself, so there is nothing left to remove natively.
    pub fn forget_destroyed(&mut self, key: ElementKey) {
        self.registrations.retain(|(held, _)| *held != key);
        self.context.mailbox.borrow_mut().forget(key);
    }

    /// Another owning handle to the element behind `key`, if still held.
    #[must_use]
    pub fn element(&self, key: ElementKey) -> Option<AxElement> {
        self.context.mailbox.borrow().element(key).map(AxElement::retained)
    }

    /// Whether a recorded, not yet drained event still names `key`.
    #[must_use]
    pub fn has_pending_event(&self, key: ElementKey) -> bool {
        self.context.mailbox.borrow().references(key)
    }

    /// Releases a callback-delivered (unregistered) element once handled.
    pub fn release(&mut self, key: ElementKey) { self.context.mailbox.borrow_mut().release(key); }

    /// The number of live registrations.
    #[must_use]
    pub const fn registration_count(&self) -> usize { self.registrations.len() }
}

impl Drop for AxObserver {
    fn drop(&mut self) {
        // Detach first so no callback can reach the context being freed.
        self.detach_run_loop();
        let registrations = std::mem::take(&mut self.registrations);
        for (key, notification) in registrations {
            let element = self.context.mailbox.borrow().element(key).map(AxElement::retained);
            if let Some(element) = element {
                // SAFETY: live observer, element and name; best-effort removal,
                // the observer is released right after.
                let _ = unsafe {
                    AXObserverRemoveNotification(
                        self.raw.as_ptr(),
                        element.as_ref(),
                        self.name_of(notification).as_concrete_TypeRef(),
                    )
                };
            }
        }
        // SAFETY: `raw` holds the +1 reference from `AXObserverCreate`, released
        // once here. The source wrapper holds its own retain.
        unsafe { core_foundation::base::CFRelease(self.raw.as_ptr().cast_const()) };
    }
}

unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

/// `kill(pid, 0)`: no signal is sent; `ESRCH` means no such process.
fn process_exited(pid: i32) -> bool {
    const ESRCH: i32 = 3;
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 only checks that the process exists.
    let code = unsafe { kill(pid, 0) };
    code != 0 && std::io::Error::last_os_error().raw_os_error() == Some(ESRCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exited_process_reads_as_exited() {
        assert!(!process_exited(std::process::id().cast_signed()));
        let mut child = std::process::Command::new("/usr/bin/true").spawn().expect("spawn");
        let pid = child.id().cast_signed();
        child.wait().expect("wait");
        assert!(process_exited(pid), "a reaped child no longer exists");
        assert!(!process_exited(0));
    }
    use crate::ax::{AxApi, SystemAx};

    fn drain_keys(mailbox: &mut Mailbox<u32>) -> Vec<(AxNotification, ElementKey)> {
        mailbox
            .drain()
            .events
            .iter()
            .map(|event| (event.notification, event.element))
            .collect()
    }

    #[test]
    fn the_mailbox_reuses_keys_for_equal_elements_and_keeps_order() {
        let mut mailbox = Mailbox::<u32>::new();
        let registered = mailbox.register(7, |held| *held == 7);
        mailbox.record(AxNotification::WindowMoved, |held| *held == 7, || 7);
        mailbox.record(AxNotification::WindowCreated, |held| *held == 9, || 9);
        mailbox.record(AxNotification::WindowResized, |held| *held == 9, || 9);
        let events = mailbox.drain().events;
        assert_eq!(events[0].element, registered);
        assert_eq!(events[1].element, events[2].element);
        assert_ne!(events[0].element, events[1].element);
        assert_eq!(mailbox.interned(), 1);
        assert_eq!(mailbox.element(events[1].element), Some(&9));
    }

    #[test]
    fn a_full_event_queue_sets_overflow_and_never_grows() {
        let mut mailbox = Mailbox::<u32>::new();
        for _ in 0..(MAX_PENDING_EVENTS + 5) {
            mailbox.record(AxNotification::WindowMoved, |held| *held == 1, || 1);
        }
        let drain = mailbox.drain();
        assert_eq!(drain.events.len(), MAX_PENDING_EVENTS);
        assert!(drain.overflowed);
        assert!(!mailbox.drain().overflowed, "overflow is reported once");
    }

    #[test]
    fn too_many_distinct_unregistered_elements_overflow_instead_of_growing() {
        let mut mailbox = Mailbox::<u32>::new();
        for value in 0..u32::try_from(MAX_INTERNED_ELEMENTS + 3).expect("small") {
            mailbox.record(AxNotification::WindowCreated, |held| *held == value, || value);
            if mailbox.events.len() >= MAX_PENDING_EVENTS - 1 {
                let _ = mailbox.drain();
            }
        }
        assert_eq!(mailbox.interned(), MAX_INTERNED_ELEMENTS);
        assert!(mailbox.drain().overflowed);
    }

    #[test]
    fn release_drops_only_callback_delivered_elements() {
        let mut mailbox = Mailbox::<u32>::new();
        let registered = mailbox.register(1, |held| *held == 1);
        mailbox.record(AxNotification::WindowCreated, |held| *held == 2, || 2);
        let delivered = drain_keys(&mut mailbox)[0].1;
        mailbox.release(registered);
        assert_eq!(mailbox.element(registered), Some(&1), "registered stays pinned");
        mailbox.release(delivered);
        assert_eq!(mailbox.element(delivered), None);
        assert_eq!(mailbox.interned(), 0);
        mailbox.forget(registered);
        assert_eq!(mailbox.element(registered), None);
    }

    #[test]
    fn an_undrained_event_still_references_its_key() {
        let mut mailbox = Mailbox::<u32>::new();
        mailbox.record(AxNotification::WindowCreated, |held| *held == 9, || 9);
        let key = mailbox.events[0].element;
        assert!(mailbox.references(key));
        let _ = mailbox.drain();
        assert!(
            !mailbox.references(key),
            "a drained event no longer references it"
        );
    }

    #[test]
    fn registering_a_delivered_element_pins_it() {
        let mut mailbox = Mailbox::<u32>::new();
        mailbox.record(AxNotification::WindowCreated, |held| *held == 5, || 5);
        let delivered = drain_keys(&mut mailbox)[0].1;
        assert_eq!(mailbox.register(5, |held| *held == 5), delivered);
        assert_eq!(mailbox.interned(), 0);
        mailbox.release(delivered);
        assert_eq!(mailbox.element(delivered), Some(&5));
    }

    /// `AXUIElementCreateApplication` is a local constructor: it sends no
    /// message, so these elements make no AX query.
    fn element(pid: i32) -> AxElement { SystemAx::new().create_application(pid).expect("element") }

    #[test]
    fn the_callback_body_maps_names_keys_and_never_walks_attributes() {
        let context = Context::new();
        let first = element(11);
        let same = element(11);
        let other = element(12);
        let moved = CFString::new("AXWindowMoved");
        let created = CFString::new("AXWindowCreated");
        let unknown = CFString::new("AXSomethingNew");

        context.record(first.as_ref(), moved.as_concrete_TypeRef());
        context.record(same.as_ref(), created.as_concrete_TypeRef());
        context.record(other.as_ref(), created.as_concrete_TypeRef());
        context.record(other.as_ref(), unknown.as_concrete_TypeRef());
        context.record(std::ptr::null(), moved.as_concrete_TypeRef());
        let non_element = CFString::new("not an element");
        context.record(
            non_element.as_concrete_TypeRef().cast(),
            moved.as_concrete_TypeRef(),
        );

        let mut mailbox = context.mailbox.borrow_mut();
        let drain = mailbox.drain();
        assert_eq!(
            drain.events.iter().map(|event| event.notification).collect::<Vec<_>>(),
            [
                AxNotification::WindowMoved,
                AxNotification::WindowCreated,
                AxNotification::WindowCreated
            ]
        );
        assert_eq!(
            drain.events[0].element, drain.events[1].element,
            "equal elements share a key"
        );
        assert_ne!(drain.events[1].element, drain.events[2].element);
        assert!(context.dropped.get(), "unrecognised deliveries are flagged");
    }

    #[test]
    fn a_reentrant_borrow_is_flagged_not_lost_silently() {
        let context = Context::new();
        let element = element(21);
        let moved = CFString::new("AXWindowMoved");
        let guard = context.mailbox.borrow_mut();
        context.record(element.as_ref(), moved.as_concrete_TypeRef());
        drop(guard);
        assert!(context.dropped.get());
    }

    #[test]
    fn notification_names_are_the_ax_string_values() {
        assert_eq!(AxNotification::WindowCreated.name(), "AXWindowCreated");
        assert_eq!(AxNotification::ElementDestroyed.name(), "AXUIElementDestroyed");
        assert_eq!(AxNotification::ApplicationHidden.name(), "AXApplicationHidden");
        let unique: std::collections::HashSet<_> =
            AxNotification::ALL.iter().map(|n| n.name()).collect();
        assert_eq!(unique.len(), AxNotification::ALL.len());
        let dock: std::collections::HashSet<_> =
            AxNotification::DOCK.iter().map(|n| n.name()).collect();
        assert_eq!(dock.len(), 4);
        assert!(
            dock.is_disjoint(&unique),
            "Dock names stay out of the window sets"
        );
    }
}
