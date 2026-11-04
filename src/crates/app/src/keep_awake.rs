//! The app-owned read-only `keepAwake.status` surface.
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
use stache_sdk::ipc::{HandlerError, HandlerResult, IpcHandler, decode_params, encode_response};

use crate::bindings::ModuleStatusView;
use crate::host::FactoryContext;

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
