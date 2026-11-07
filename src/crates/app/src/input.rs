//! The app-owned input tap teardown acknowledgement.
//!
//! The shared input service and its macOS owner are created once in the
//! composition root. Only the enabled Keybindings module with Caps bindings
//! subscribes, so the owner thread and every tap stay absent otherwise; this
//! adapter lets coordinated teardown stop them.

use std::sync::Arc;

use stache_macos::MacInputBackend;

use crate::shutdown::{NativeOwner, OwnerAnswer};

/// Acknowledges the input owner's shutdown to the teardown coordinator.
///
/// `release` starts the owner's shutdown and returns at once. The owner thread
/// removes any tap and destroys every native object on its own thread, off the
/// main thread, and only then completes the answer, so success proves the
/// release. An owner that never started answers success immediately. A hung
/// native call leaves the answer pending and the coordinator names the owner
/// unacknowledged at its bound.
pub struct InputTapOwner {
    backend: Arc<MacInputBackend>,
}

impl InputTapOwner {
    /// The stable teardown record name.
    pub const NAME: &'static str = "input-tap";

    /// Wraps the one backend behind the host's input service.
    #[must_use]
    pub const fn new(backend: Arc<MacInputBackend>) -> Self { Self { backend } }
}

impl NativeOwner for InputTapOwner {
    fn name(&self) -> &'static str { Self::NAME }

    fn on_main(&self) -> bool { false }

    fn release(&self) -> OwnerAnswer {
        let answer = OwnerAnswer::new();
        let completion = answer.clone();
        self.backend.shutdown(move |result| match result {
            Ok(()) => completion.succeed(),
            Err(_) => completion.fail(),
        });
        answer
    }
}

#[cfg(test)]
mod tests {
    use stache_sdk::input::NoWork;

    use super::*;
    use crate::shutdown::OwnerState;

    #[test]
    fn an_idle_owner_acknowledges_release_off_main_without_starting_a_thread() {
        let (_service, backend) = MacInputBackend::service(Arc::new(NoWork));
        let owner = InputTapOwner::new(Arc::clone(&backend));
        assert_eq!(owner.name(), "input-tap");
        assert!(!owner.on_main());
        let answer = owner.release();
        assert_eq!(answer.state(), OwnerState::Succeeded);
        // A repeated release stays answered and never blocks.
        assert_eq!(owner.release().state(), OwnerState::Succeeded);
        assert!(backend.join(std::time::Duration::from_secs(1)));
    }
}
