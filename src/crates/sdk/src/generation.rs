//! The per-runtime generation scope.
//!
//! The registry creates one [`GenerationScope`] for every runtime it starts and
//! invalidates it the moment that runtime stops, pauses, is disabled or fails,
//! before any cleanup wait or worker join. A host capability that must not
//! outlive its runtime (published state today, screen reservations next)
//! registers a listener on the scope and so acts on exactly that generation: a
//! retained handle from an older runtime can never touch a successor, because
//! the successor owns a different scope with a different identifier.
//!
//! This is distinct from [`crate::ModuleInvalidation`], which is per module slot
//! and fires only at shutdown.

use std::fmt;
use std::sync::{Arc, Mutex};

use crate::lock;

/// Why a runtime generation ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RuntimeEnd {
    /// The runtime stopped (shutdown, restart or a stop that was not a pause).
    Stopped,
    /// The module was disabled.
    Disabled,
    /// The module was paused.
    Paused,
    /// The runtime failed to start or to stop.
    Failed,
    /// A required permission was revoked or is missing.
    PermissionBlocked,
    /// A required service or executable became unavailable.
    DependencyUnavailable,
}

type EndListener = Box<dyn FnOnce(RuntimeEnd) + Send>;

struct ScopeState {
    end: Option<RuntimeEnd>,
    listeners: Vec<EndListener>,
}

struct ScopeInner {
    id: u64,
    state: Mutex<ScopeState>,
}

/// A lock-free-for-readers token for one runtime generation.
///
/// Clones share the same generation. [`GenerationScope::invalidate`] ends it
/// once; every listener registered before that runs exactly once, after the
/// scope lock is released, and a listener registered afterwards runs at once.
#[derive(Clone)]
#[allow(clippy::redundant_pub_crate)] // crate-internal by design; the module is private
pub(crate) struct GenerationScope {
    inner: Arc<ScopeInner>,
}

impl GenerationScope {
    /// Creates a live scope with the registry-wide unique `id`.
    pub(crate) fn new(id: u64) -> Self {
        Self {
            inner: Arc::new(ScopeInner {
                id,
                state: Mutex::new(ScopeState {
                    end: None,
                    listeners: Vec::new(),
                }),
            }),
        }
    }

    /// The generation identifier, unique within one registry.
    pub(crate) fn id(&self) -> u64 { self.inner.id }

    /// Why the generation ended, or `None` while it is live.
    pub(crate) fn end(&self) -> Option<RuntimeEnd> { lock(&self.inner.state).end }

    /// Ends the generation. The first call wins; later calls are no-ops.
    pub(crate) fn invalidate(&self, end: RuntimeEnd) {
        let listeners = {
            let mut state = lock(&self.inner.state);
            if state.end.is_some() {
                return;
            }
            state.end = Some(end);
            std::mem::take(&mut state.listeners)
        };
        for listener in listeners {
            run(listener, end);
        }
    }

    /// Registers a listener, running it immediately when already ended.
    pub(crate) fn on_end(&self, listener: impl FnOnce(RuntimeEnd) + Send + 'static) {
        let ended = {
            let mut state = lock(&self.inner.state);
            match state.end {
                None => {
                    state.listeners.push(Box::new(listener));
                    return;
                }
                Some(end) => end,
            }
        };
        run(Box::new(listener), ended);
    }
}

fn run(listener: EndListener, end: RuntimeEnd) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(end)));
}

impl fmt::Debug for GenerationScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GenerationScope")
            .field("id", &self.inner.id)
            .field("end", &self.end())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn listeners_run_once_with_the_first_end_and_late_listeners_run_immediately() {
        let scope = GenerationScope::new(7);
        let runs = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(Vec::new()));
        {
            let runs = Arc::clone(&runs);
            let seen = Arc::clone(&seen);
            scope.on_end(move |end| {
                runs.fetch_add(1, Ordering::SeqCst);
                lock(&seen).push(end);
            });
        }
        assert_eq!(scope.id(), 7);
        assert!(scope.end().is_none());
        scope.invalidate(RuntimeEnd::Paused);
        scope.invalidate(RuntimeEnd::Failed);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(*lock(&seen), vec![RuntimeEnd::Paused]);
        assert_eq!(scope.end(), Some(RuntimeEnd::Paused));
        {
            let seen = Arc::clone(&seen);
            scope.on_end(move |end| lock(&seen).push(end));
        }
        assert_eq!(*lock(&seen), vec![RuntimeEnd::Paused, RuntimeEnd::Paused]);
    }

    #[test]
    fn a_panicking_listener_does_not_stop_the_others() {
        let scope = GenerationScope::new(1);
        let ran = Arc::new(AtomicUsize::new(0));
        scope.on_end(|_| panic!("listener failure"));
        {
            let ran = Arc::clone(&ran);
            scope.on_end(move |_| {
                ran.fetch_add(1, Ordering::SeqCst);
            });
        }
        scope.invalidate(RuntimeEnd::Stopped);
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn listeners_may_reenter_the_scope() {
        let scope = GenerationScope::new(2);
        let inner = scope.clone();
        scope.on_end(move |_| {
            assert!(inner.end().is_some());
            inner.on_end(|_| {});
        });
        scope.invalidate(RuntimeEnd::Disabled);
    }
}
