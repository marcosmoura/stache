//! The native [`WindowEventSession`]: a run loop and the private notifications.
//! Default tests never construct it (they use fake sessions); it is exercised
//! only by an approved native check.
//!
//! Everything here is created on the owner thread and is not `Send`.

use std::sync::Arc;
use std::time::Duration;

use core_foundation::runloop::{CFRunLoop, kCFRunLoopDefaultMode};

use super::skylight::SkyLightWindowNotify;
use super::{
    EventSlots, FastOrderResult, LiveUnavailable, LoopWaker, SessionMaker, WindowEventSession,
};
use crate::border_overlay::OverlayRect;
use crate::border_overlay::wake::WakeSource;

/// The constructor the system source uses.
#[must_use]
pub fn session_maker() -> SessionMaker {
    Arc::new(|| Box::new(NativeWindowSession::new()) as Box<dyn WindowEventSession>)
}

/// The owner-thread session.
pub struct NativeWindowSession {
    run_loop: CFRunLoop,
    wake: Option<Arc<WakeSource>>,
    notify: SkyLightWindowNotify,
}

impl NativeWindowSession {
    fn new() -> Self {
        let run_loop = CFRunLoop::get_current();
        let wake = WakeSource::new(&run_loop).map(Arc::new);
        Self {
            run_loop,
            wake,
            notify: SkyLightWindowNotify::system(),
        }
    }
}

impl WindowEventSession for NativeWindowSession {
    fn waker(&self) -> LoopWaker {
        // Without a wake source a wake stops the run loop if it is running.
        self.wake.as_ref().map_or_else(
            || {
                let run_loop = self.run_loop.clone();
                Arc::new(move || run_loop.stop()) as LoopWaker
            },
            |wake| {
                let wake = Arc::clone(wake);
                Arc::new(move || wake.wake()) as LoopWaker
            },
        )
    }

    fn register(&mut self, slots: Arc<EventSlots>) -> Result<(), LiveUnavailable> {
        self.notify.register(slots)
    }

    fn subscribe(&mut self, ids: &[u32]) -> Result<(), LiveUnavailable> {
        self.notify.subscribe(ids)
    }

    fn bounds(&mut self, cg_window_id: u32) -> Option<OverlayRect> {
        self.notify.bounds(cg_window_id)
    }

    fn order_above(&mut self, panel: u32, target: u32) -> FastOrderResult {
        self.notify.order_above(panel, target)
    }

    fn pump(&mut self, timeout: Duration) {
        // SAFETY: `kCFRunLoopDefaultMode` is an immutable CF constant.
        let mode = unsafe { kCFRunLoopDefaultMode };
        let _ = CFRunLoop::run_in_mode(mode, timeout, true);
    }

    fn unregister(&mut self) { self.notify.unregister(); }
}
