//! The macOS owner of the shared input service's event taps.
//!
//! # Ownership
//!
//! ```text
//! InputService (sdk, Arc)  --submit-->  MacInputBackend (Send + Sync)
//!                                         ├─ ControlQueue (bound 256)   crosses threads
//!                                         ├─ WakeHandle (signal only)   crosses threads
//!                                         └─ owner thread "stache-input-owner"
//!                                              └─ SystemInput (!Send)   never leaves it
//!                                                   ├─ CFRunLoop of the owner thread
//!                                                   ├─ wake CFRunLoopSource
//!                                                   └─ per tap: CFMachPort + CFRunLoopSource
//!                                                              + boxed callback context
//! ```
//!
//! The thread starts lazily on the first request and no tap exists before an
//! `Install`. The owner never dispatches to the main thread, so there is no
//! main -> worker -> main wait. Install creates a disabled HID tap (suppressing:
//! head-insert, default options; passive: listen-only); creation failure (for
//! example a missing Input Monitoring or Accessibility grant) is acknowledged as
//! an error and never prompts. Each request is checked for cancellation
//! immediately before its native mutation; removals are never cancelled.
//!
//! Teardown calls [`MacInputBackend::shutdown`]: queued removals run, both taps
//! and the run-loop objects are destroyed on the owner thread, and only then does
//! the completion run. Default tests use a fake native layer and never create a
//! real tap.

mod convert;
mod owner;
mod system;
#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use stache_sdk::input::{
    ControlRejected, ControlRequest, InputBackend, InputService, WorkReservations,
};

use self::owner::{Factory, InputOwner};
use self::system::{ServiceSlot, SystemInput};
use crate::applications;

// The SDK states these constants for the platform-independent service; the
// shipped native owner must agree with the application owner's bounds.
const _: () = {
    assert!(
        stache_sdk::input::ACKNOWLEDGEMENT_BUDGET.as_nanos()
            == applications::ACKNOWLEDGEMENT_BUDGET.as_nanos()
    );
    assert!(
        stache_sdk::input::CANCELLATION_QUANTUM.as_nanos()
            == applications::CANCELLATION_QUANTUM.as_nanos()
    );
};

/// The macOS [`InputBackend`]: one lazy owner thread over the real event taps.
pub struct MacInputBackend {
    owner: InputOwner,
    service: Arc<ServiceSlot>,
}

impl MacInputBackend {
    /// Creates the shared service over a new macOS backend.
    ///
    /// Nothing native exists yet: the owner thread starts on the first request
    /// and no tap exists until an install.
    #[must_use]
    pub fn service(work: Arc<dyn WorkReservations>) -> (Arc<InputService>, Arc<Self>) {
        let slot = Arc::new(OnceLock::new());
        let factory_slot = Arc::clone(&slot);
        let backend = Arc::new(Self::from_factory(
            Box::new(move || SystemInput::new(factory_slot).map(|input| Box::new(input) as Box<_>)),
            slot,
        ));
        let service = InputService::new(Arc::clone(&backend) as Arc<dyn InputBackend>, work);
        // A weak link: the service owns the backend, so the callbacks reach the
        // service without a reference cycle.
        let _ = backend.service.set(Arc::downgrade(&service));
        (service, backend)
    }

    fn from_factory(factory: Factory, service: Arc<ServiceSlot>) -> Self {
        Self {
            owner: InputOwner::new(factory),
            service,
        }
    }

    /// Stops the owner: queued removals run, both taps and every run-loop
    /// object are destroyed on the owner thread, and then `done` runs there with
    /// the cleanup result. Never blocks the caller; safe to call repeatedly.
    pub fn shutdown(&self, done: impl FnOnce(Result<(), String>) + Send + 'static) {
        self.owner.shutdown(done);
    }

    /// Waits at most `timeout` for the owner thread to finish, then joins it.
    /// Returns whether it finished.
    #[must_use]
    pub fn join(&self, timeout: Duration) -> bool { self.owner.join(timeout) }
}

impl InputBackend for MacInputBackend {
    fn submit(&self, request: ControlRequest) -> Result<(), ControlRejected> {
        self.owner.submit(request)
    }
}
