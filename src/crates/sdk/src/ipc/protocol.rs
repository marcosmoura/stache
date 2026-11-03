//! Newline-delimited JSON wire types for the control socket.
//!
//! A client sends exactly one [`Request`] per connection and reads exactly one
//! [`Response`]. Requests are typed by module and method; parameters stay an
//! untyped [`Value`] only until the module adapter deserializes them once.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One request for a module method.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// The target module, or the reserved `core` namespace.
    pub module: String,
    /// The method declared by that module.
    pub method: String,
    /// Method parameters, decoded by the target adapter.
    #[serde(default)]
    pub params: Value,
}

impl Request {
    /// Serializes typed parameters exactly once.
    ///
    /// # Errors
    /// Returns the serialization error instead of fabricating a fallback body.
    pub fn new(
        module: impl Into<String>,
        method: impl Into<String>,
        params: impl Serialize,
    ) -> Result<Self, serde_json::Error> {
        Ok(Self {
            module: module.into(),
            method: method.into(),
            params: serde_json::to_value(params)?,
        })
    }

    /// A request whose method takes no parameters.
    #[must_use]
    pub fn without_params(module: impl Into<String>, method: impl Into<String>) -> Self {
        Self {
            module: module.into(),
            method: method.into(),
            params: Value::Null,
        }
    }
}

/// One response to a [`Request`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum Response {
    /// The method succeeded with serialized data.
    Ok {
        /// The method's typed result.
        data: Value,
    },
    /// The request or the method failed.
    Error {
        /// The structured failure.
        error: WireError,
    },
}

impl Response {
    /// A successful response.
    #[must_use]
    pub const fn ok(data: Value) -> Self { Self::Ok { data } }

    /// A failed response.
    #[must_use]
    pub fn failure(kind: WireErrorKind, message: impl Into<String>) -> Self {
        Self::Error {
            error: WireError { kind, message: message.into() },
        }
    }
}

/// A structured wire failure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireError {
    /// The failure category.
    pub kind: WireErrorKind,
    /// A human-readable reason; never used for control flow.
    pub message: String,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

/// The category of a [`WireError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WireErrorKind {
    /// The request could not be read, decoded or was too large.
    MalformedRequest,
    /// The method parameters do not match the adapter's type.
    InvalidParams,
    /// No module registered that name.
    UnknownModule,
    /// The module has no such method.
    UnknownMethod,
    /// The module is disabled in configuration.
    Disabled,
    /// The module or a dependency is unavailable.
    Unavailable,
    /// The handler failed internally.
    Handler,
}

impl fmt::Display for WireErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MalformedRequest => "malformed request",
            Self::InvalidParams => "invalid params",
            Self::UnknownModule => "unknown module",
            Self::UnknownMethod => "unknown method",
            Self::Disabled => "disabled",
            Self::Unavailable => "unavailable",
            Self::Handler => "handler error",
        })
    }
}

/// The result of `core.reload`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreReload {
    /// One entry per registered configuration file, in file order.
    pub files: Vec<CoreFileOutcome>,
}

impl CoreReload {
    /// Whether any file was rejected or produced an application diagnostic.
    #[must_use]
    pub fn has_failures(&self) -> bool { self.files.iter().any(|file| file.diagnostic.is_some()) }
}

/// The outcome of applying one file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreFileOutcome {
    /// The configuration file name.
    pub file: String,
    /// What the store did with the candidate.
    pub outcome: CoreApplyOutcome,
    /// A diagnostic when the candidate was rejected or failed to start.
    pub diagnostic: Option<String>,
}

/// What a reload did with one file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum CoreApplyOutcome {
    /// The candidate was committed at this revision.
    Applied {
        /// The committed revision.
        revision: u64,
    },
    /// The candidate decodes to the accepted value.
    Unchanged,
    /// Nothing was applied; the last-good state is preserved.
    Rejected,
}

/// The result of `core.status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreStatus {
    /// The configuration directory the host is using.
    pub directory: PathBuf,
    /// One entry per registered file, in file order.
    pub files: Vec<CoreFileStatus>,
}

/// The local state of one file reported by the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreFileStatus {
    /// The configuration file name.
    pub file: String,
    /// Whether the file parses and validates locally.
    pub state: CoreFileState,
    /// Whether the host currently uses an accepted revision of this file.
    pub active: bool,
    /// A diagnostic held for the file.
    pub diagnostic: Option<String>,
    /// Unknown keys ignored in an otherwise valid file.
    pub warnings: Vec<String>,
}

/// The local validity of one configuration file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CoreFileState {
    /// The file is absent.
    Missing,
    /// The file parses and validates.
    Valid,
    /// The file exists but is rejected.
    Invalid,
}

#[cfg(test)]
mod tests {
    use serde::Serializer;
    use serde::ser::Error as _;
    use serde_json::json;

    use super::*;

    #[test]
    fn requests_serialize_typed_params_once() {
        let request = Request::new("demo", "echo", json!({ "text": "hi" })).expect("request");
        assert_eq!(
            serde_json::to_string(&request).expect("json"),
            r#"{"module":"demo","method":"echo","params":{"text":"hi"}}"#
        );
    }

    #[test]
    fn requests_without_params_round_trip() {
        let request = Request::without_params("core", "status");
        let json = serde_json::to_string(&request).expect("json");
        assert_eq!(json, r#"{"module":"core","method":"status","params":null}"#);
        assert_eq!(serde_json::from_str::<Request>(&json).expect("parse"), request);
    }

    #[test]
    fn responses_are_tagged_and_round_trip() {
        let response = Response::ok(json!({ "revision": 2 }));
        let json = serde_json::to_string(&response).expect("json");
        assert_eq!(json, r#"{"status":"ok","data":{"revision":2}}"#);
        assert_eq!(serde_json::from_str::<Response>(&json).expect("parse"), response);

        let response = Response::failure(WireErrorKind::UnknownModule, "no `demo`");
        let json = serde_json::to_string(&response).expect("json");
        assert_eq!(
            json,
            r#"{"status":"error","error":{"kind":"unknownModule","message":"no `demo`"}}"#
        );
    }

    #[test]
    fn malformed_bodies_do_not_decode() {
        let error = serde_json::from_str::<Request>(r#"{"module": 4}"#).expect_err("reject");
        assert!(error.to_string().contains("string"), "{error}");
    }

    #[test]
    fn core_reports_round_trip() {
        let reload = CoreReload {
            files: vec![
                CoreFileOutcome {
                    file: "global.jsonc".to_owned(),
                    outcome: CoreApplyOutcome::Applied { revision: 2 },
                    diagnostic: None,
                },
                CoreFileOutcome {
                    file: "proxyAudio.jsonc".to_owned(),
                    outcome: CoreApplyOutcome::Rejected,
                    diagnostic: Some("bad value".to_owned()),
                },
            ],
        };
        let json = serde_json::to_string(&reload).expect("json");
        assert!(json.contains(r#""status":"rejected""#));
        assert_eq!(serde_json::from_str::<CoreReload>(&json).expect("parse"), reload);
        assert!(reload.has_failures());

        let status = CoreStatus {
            directory: PathBuf::from("/config"),
            files: vec![CoreFileStatus {
                file: "global.jsonc".to_owned(),
                state: CoreFileState::Valid,
                active: true,
                diagnostic: None,
                warnings: vec!["enable".to_owned()],
            }],
        };
        let json = serde_json::to_string(&status).expect("json");
        assert_eq!(serde_json::from_str::<CoreStatus>(&json).expect("parse"), status);
    }

    #[test]
    fn serialization_errors_are_returned_not_replaced() {
        struct Failing;

        impl Serialize for Failing {
            fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(S::Error::custom("cannot encode"))
            }
        }

        let error = Request::new("demo", "broken", Failing).expect_err("serialize");
        assert_eq!(error.to_string(), "cannot encode");
    }
}
