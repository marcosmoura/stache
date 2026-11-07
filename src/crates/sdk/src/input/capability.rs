//! The module-scoped input capability handed out through `ModuleContext::input`.

use std::sync::{Arc, Mutex, PoisonError};

use super::service::{InputError, InputService, Subscription, SubscriptionInner};
use super::subscribers::{HandlerSpec, InputHandler, InputObserver, ObserverSpec};
use crate::module::ModuleId;
use crate::threading::CancellationToken;

/// A module's view of the shared [`InputService`].
///
/// Subscriptions made through it are cancelled by the module runtime's
/// cancellation token while starting, are logically invalidated at stage-1
/// teardown and are released when the runtime stops, so an obsolete capability
/// can never keep delivering callbacks to a successor runtime.
#[derive(Clone)]
pub struct InputCapability {
    session: Arc<Session>,
}

struct Session {
    owner: ModuleId,
    service: Arc<InputService>,
    cancellation: CancellationToken,
    state: Mutex<State>,
}

struct State {
    active: bool,
    subscriptions: Vec<Arc<SubscriptionInner>>,
}

impl InputCapability {
    pub(crate) fn new(
        owner: ModuleId,
        service: Arc<InputService>,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            session: Arc::new(Session {
                owner,
                service,
                cancellation,
                state: Mutex::new(State {
                    active: true,
                    subscriptions: Vec::new(),
                }),
            }),
        }
    }

    /// The module this capability belongs to.
    #[must_use]
    pub fn owner(&self) -> ModuleId { self.session.owner }

    /// Whether a native input owner is attached to the host.
    #[must_use]
    pub fn is_available(&self) -> bool { self.session.service.is_available() }

    /// Subscribes a suppressing handler.
    ///
    /// # Errors
    /// Returns an [`InputError`], including [`InputError::Unavailable`] when the
    /// host has no native owner.
    pub fn subscribe_handler(
        &self,
        spec: HandlerSpec,
        handler: Arc<dyn InputHandler>,
    ) -> Result<Subscription, InputError> {
        self.ensure_active()?;
        let subscription =
            self.session
                .service
                .subscribe_handler(spec, handler, &self.session.cancellation)?;
        self.track(subscription)
    }

    /// Subscribes a passive observer.
    ///
    /// # Errors
    /// Returns an [`InputError`], including [`InputError::Unavailable`] when the
    /// host has no native owner.
    pub fn subscribe_observer(
        &self,
        spec: ObserverSpec,
        observer: Arc<dyn InputObserver>,
    ) -> Result<Subscription, InputError> {
        self.ensure_active()?;
        let subscription =
            self.session
                .service
                .subscribe_observer(spec, observer, &self.session.cancellation)?;
        self.track(subscription)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.session.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn ensure_active(&self) -> Result<(), InputError> {
        if self.lock().active {
            Ok(())
        } else {
            Err(InputError::Invalidated)
        }
    }

    fn track(&self, subscription: Subscription) -> Result<Subscription, InputError> {
        let mut state = self.lock();
        if !state.active {
            drop(state);
            drop(subscription);
            return Err(InputError::Invalidated);
        }
        state.subscriptions.retain(|inner| !inner.is_released());
        state.subscriptions.push(subscription.inner());
        drop(state);
        Ok(subscription)
    }

    /// Stage-1 teardown: stop callbacks immediately, without native work.
    pub(crate) fn invalidate(&self) {
        let mut state = self.lock();
        state.active = false;
        for inner in &state.subscriptions {
            inner.invalidate();
        }
    }

    /// Releases every remaining subscription after the module stopped.
    pub(crate) fn release_owner(&self) {
        let subscriptions = {
            let mut state = self.lock();
            state.active = false;
            std::mem::take(&mut state.subscriptions)
        };
        for inner in subscriptions {
            let _ = inner.release();
        }
    }
}
