//! The owner-local system seam: the AX API, the per-application observer and
//! the clock. Tests substitute fakes; production uses [`SystemWindows`].

use std::time::Instant;

use crate::ax::{
    AxApi, AxDrain, AxElement, AxError, AxNotification, AxObserver, ElementKey, SystemAx, WindowAx,
};

/// The observer operations the owner needs, generic over the element type so a
/// fake observer can serve a fake AX arena. Owner-thread-local like
/// [`AxObserver`].
pub trait WindowObserver<E> {
    /// Attaches the observer's source to the calling thread's run loop.
    fn attach(&mut self);
    /// Detaches the source. Idempotent.
    fn detach(&mut self);
    /// Registers a notification and returns the element's owner-local key.
    ///
    /// # Errors
    ///
    /// The AX failure; nothing stays registered.
    fn add_notification(
        &mut self,
        element: &E,
        notification: AxNotification,
    ) -> Result<ElementKey, AxError>;
    /// Removes one registration, keeping state on failure.
    ///
    /// # Errors
    ///
    /// The AX failure.
    fn remove_notification(
        &mut self,
        key: ElementKey,
        notification: AxNotification,
    ) -> Result<(), AxError>;
    /// Events recorded since the previous drain.
    fn drain(&mut self) -> AxDrain;
    /// Another handle to the element behind `key`.
    fn element(&self, key: ElementKey) -> Option<E>;
    /// Whether a recorded, not yet drained event still names `key`.
    fn has_pending_event(&self, key: ElementKey) -> bool;
    /// Releases a callback-delivered, unregistered element.
    fn release(&mut self, key: ElementKey);
    /// Forgets registrations of an element the system proved gone.
    fn forget_destroyed(&mut self, key: ElementKey);
}

impl WindowObserver<AxElement> for AxObserver {
    fn attach(&mut self) { self.attach_current_run_loop(); }

    fn detach(&mut self) { self.detach_run_loop(); }

    fn add_notification(
        &mut self,
        element: &AxElement,
        notification: AxNotification,
    ) -> Result<ElementKey, AxError> {
        Self::add_notification(self, element, notification)
    }

    fn remove_notification(
        &mut self,
        key: ElementKey,
        notification: AxNotification,
    ) -> Result<(), AxError> {
        Self::remove_notification(self, key, notification)
    }

    fn drain(&mut self) -> AxDrain { Self::drain(self) }

    fn element(&self, key: ElementKey) -> Option<AxElement> { Self::element(self, key) }

    fn has_pending_event(&self, key: ElementKey) -> bool { Self::has_pending_event(self, key) }

    fn release(&mut self, key: ElementKey) { Self::release(self, key); }

    fn forget_destroyed(&mut self, key: ElementKey) { Self::forget_destroyed(self, key); }
}

/// Everything native the owner thread uses. Created **inside** the owner
/// thread by a `Send` factory, so it may hold `!Send` objects.
pub trait WindowSystem: 'static {
    /// The window AX API.
    type Ax: WindowAx;
    /// The per-application observer.
    type Observer: WindowObserver<<Self::Ax as AxApi>::Element>;

    /// The AX API.
    fn ax(&self) -> &Self::Ax;

    /// Creates an observer for `pid`'s notifications. Sends no AX message.
    ///
    /// # Errors
    ///
    /// The AX failure ([`AxError::ApiDisabled`] without permission).
    fn observer(&self, pid: i32) -> Result<Self::Observer, AxError>;
}

/// The real AX bindings.
pub struct SystemWindows {
    ax: SystemAx,
}

impl SystemWindows {
    /// Creates the bindings. Call it on the owner thread.
    #[must_use]
    pub fn new() -> Self { Self { ax: SystemAx::new() } }
}

impl Default for SystemWindows {
    fn default() -> Self { Self::new() }
}

impl WindowSystem for SystemWindows {
    type Ax = SystemAx;
    type Observer = AxObserver;

    fn ax(&self) -> &SystemAx { &self.ax }

    fn observer(&self, pid: i32) -> Result<AxObserver, AxError> { AxObserver::new(pid) }
}

/// A monotonic clock, injected so tests control time.
pub trait WindowClock: Send + Sync {
    /// The current instant.
    fn now(&self) -> Instant;
}

/// The system monotonic clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemWindowClock;

impl WindowClock for SystemWindowClock {
    fn now(&self) -> Instant { Instant::now() }
}
