//! The app-owned Keep Awake surface.
//!
//! [`KeepAwakeIpc`] serves the read-only `keepAwake.status` envelope;
//! [`PowerAssertionOwner`] acknowledges the module-owned release to the teardown
//! coordinator without performing any native work of its own.
//!
//! The module crate owns the runtime DTOs and the clonable snapshot reader; the
//! app owns the envelope because [`ModuleStatusView`] is an app projection of
//! registry metadata. The handler only copies published and held data: it never
//! probes the lock, acquires or releases a power assertion, starts a worker,
//! installs an observer, reads a file, applies configuration or opens a window.
//!
//! Lifecycle, held diagnostics and runtime are sampled from three separate
//! cells while a transition may be in progress. A response is therefore a
//! point-in-time observation, not one globally atomic snapshot across them. The
//! runtime fields themselves are published together and remain coherent.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use specta::Type;
use stache_keep_awake::{KeepAwakeConfig, KeepAwakeRuntimeStatus, KeepAwakeSnapshot};
use stache_macos::{AssertionOwner, OwnerReleaseState};
use stache_sdk::ipc::{HandlerError, HandlerResult, IpcHandler, decode_params, encode_response};

use crate::bindings::ModuleStatusView;
use crate::host::FactoryContext;
use crate::shutdown::{NativeOwner, OwnerAnswer};

/// The typed response of `keepAwake.status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct KeepAwakeStatus {
    /// Published registry metadata for the Keep Awake module.
    ///
    /// The projection keeps the exact decimal revision string. A held
    /// configuration diagnostic takes precedence over the snapshot diagnostic.
    pub module: ModuleStatusView,
    /// The module's single published runtime observation.
    pub runtime: KeepAwakeRuntimeStatus,
}

/// Serves the read-only `keepAwake.status` socket method.
pub struct KeepAwakeIpc {
    context: FactoryContext,
    snapshot: KeepAwakeSnapshot,
}

impl KeepAwakeIpc {
    /// Composes status from published registry metadata, held file diagnostics
    /// and one runtime snapshot reader.
    #[must_use]
    pub const fn new(context: FactoryContext, snapshot: KeepAwakeSnapshot) -> Self {
        Self { context, snapshot }
    }

    fn status(&self) -> HandlerResult<KeepAwakeStatus> {
        let id = KeepAwakeConfig::ID;
        let snapshot = self.context.module_snapshot(id).ok_or_else(|| {
            HandlerError::handler(format!("no published snapshot for module `{id}`"))
        })?;
        let mut module = ModuleStatusView::from_snapshot(&snapshot);
        // The file-first overlay keeps a parse/type/I/O/deletion rejection that
        // can happen before the registry records a snapshot diagnostic. The
        // snapshot diagnostic remains the fallback.
        if let Some(diagnostic) = self.context.diagnostic(id) {
            module.diagnostic = Some(diagnostic.to_string());
        }
        Ok(KeepAwakeStatus {
            module,
            runtime: self.snapshot.read(),
        })
    }
}

impl IpcHandler for KeepAwakeIpc {
    fn handle(&self, method: &str, params: Value) -> HandlerResult<Value> {
        match method {
            "status" => {
                zero_argument_params(params)?;
                encode_response(&self.status()?)
            }
            _ => Err(HandlerError::unknown_method(method)),
        }
    }
}

/// Accepts only the three spellings of a zero-argument request.
///
/// An omitted parameter arrives as `null`, and an empty object is normalized to
/// `null` before one strict unit decode. Nonempty objects, strings, numbers,
/// booleans and arrays (including `[]`) are rejected; an empty `serde` struct
/// would silently accept some of them.
fn zero_argument_params(params: Value) -> HandlerResult<()> {
    let params = match params {
        Value::Object(map) if map.is_empty() => Value::Null,
        other => other,
    };
    decode_params::<()>(params)
}

/// The app's teardown acknowledgement over the module's assertion owner.
///
/// This is not a second cleanup path: `KeepAwakeModule::stop` and its rollback
/// keep sole ownership of every native release. The adapter reads the same
/// ledger the production [`KeepAwakeFactory`](crate::host::KeepAwakeFactory)
/// shares with the module and answers success only when that ledger proves no
/// active or acquiring generation, no release pass in flight and no unresolved
/// ID. It performs no native work, starts no retry and never blocks.
pub struct PowerAssertionOwner {
    owner: AssertionOwner,
}

impl PowerAssertionOwner {
    /// The stable teardown record name.
    pub const NAME: &'static str = "power-assertion";

    /// Wraps the same owner the Keep Awake factory uses.
    #[must_use]
    pub const fn new(owner: AssertionOwner) -> Self { Self { owner } }
}

impl NativeOwner for PowerAssertionOwner {
    fn name(&self) -> &'static str { Self::NAME }

    fn on_main(&self) -> bool { false }

    fn release(&self) -> OwnerAnswer {
        let answer = OwnerAnswer::new();
        match self.owner.release_state() {
            OwnerReleaseState::Released => answer.succeed(),
            OwnerReleaseState::Failed => answer.fail(),
            // An active or acquiring generation, or a release already in flight,
            // is not an answer. The coordinator names this owner unacknowledged
            // at its bound; it never retries the module-owned cleanup.
            OwnerReleaseState::Unanswered => {}
        }
        answer
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use stache_macos::{
        AssertionKind, AssertionOwner, AssertionRequest, FakeAssertionBackend, ReleaseStatus,
    };
    use stache_sdk::CancellationToken;

    use super::*;
    use crate::shutdown::OwnerState;

    const FAILURE_CODE: i32 = -536_870_184;

    fn adapter() -> (PowerAssertionOwner, AssertionOwner, FakeAssertionBackend) {
        let backend = FakeAssertionBackend::new();
        let owner = AssertionOwner::new(Arc::new(backend.clone()));
        (PowerAssertionOwner::new(owner.clone()), owner, backend)
    }

    #[test]
    fn a_disabled_never_started_owner_acknowledges_release() {
        let (adapter, _owner, backend) = adapter();

        let answer = adapter.release();
        assert_eq!(answer.state(), OwnerState::Succeeded);
        assert!(backend.create_attempts().is_empty());
        assert!(backend.release_attempts().is_empty());
    }

    #[test]
    fn an_active_owner_with_no_pending_ids_stays_unacknowledged() {
        let (adapter, owner, _backend) = adapter();
        let token = CancellationToken::new();
        let lease = owner
            .acquire(&AssertionRequest::system_wake_lock(), &token)
            .expect("complete acquisition");
        assert!(owner.pending().is_empty(), "a live lease is not pending cleanup");

        let answer = adapter.release();
        assert_eq!(answer.state(), OwnerState::Pending);
        assert!(!answer.is_answered());

        assert!(lease.release().is_complete());
        assert_eq!(adapter.release().state(), OwnerState::Succeeded);
    }

    #[test]
    fn an_in_flight_acquisition_cannot_be_acknowledged() {
        let (adapter, owner, backend) = adapter();
        let barrier = backend.block_next_create();
        let token = CancellationToken::new();

        std::thread::scope(|scope| {
            let worker =
                scope.spawn(|| owner.acquire(&AssertionRequest::system_wake_lock(), &token));
            assert!(barrier.wait_entered(Duration::from_secs(2)));

            assert_eq!(adapter.release().state(), OwnerState::Pending);

            assert!(barrier.release());
            let lease = worker.join().expect("acquisition worker must not panic").expect("acquire");
            assert!(lease.release().is_complete());
        });

        assert_eq!(adapter.release().state(), OwnerState::Succeeded);
    }

    #[test]
    fn an_in_flight_release_cannot_be_acknowledged() {
        let (adapter, owner, backend) = adapter();
        let token = CancellationToken::new();
        let lease = owner
            .acquire(&AssertionRequest::system_wake_lock(), &token)
            .expect("complete acquisition");
        let barrier = backend.block_next_release();

        std::thread::scope(|scope| {
            let worker = scope.spawn(|| lease.release());
            assert!(barrier.wait_entered(Duration::from_secs(2)));

            assert_eq!(adapter.release().state(), OwnerState::Pending);

            assert!(barrier.release());
            assert!(worker.join().expect("release worker must not panic").is_complete());
        });

        assert_eq!(adapter.release().state(), OwnerState::Succeeded);
    }

    #[test]
    fn a_confirmed_unresolved_release_failure_is_reported_as_failed() {
        let (adapter, owner, backend) = adapter();
        let token = CancellationToken::new();
        let lease = owner
            .acquire(&AssertionRequest::system_wake_lock(), &token)
            .expect("complete acquisition");
        backend.fail_release_always(AssertionKind::Display, FAILURE_CODE);

        assert_eq!(lease.release().status(), ReleaseStatus::Pending);
        assert_eq!(adapter.release().state(), OwnerState::Failed);
    }
}
