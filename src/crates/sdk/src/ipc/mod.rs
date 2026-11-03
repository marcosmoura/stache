//! Unix socket control channel.
//!
//! [`protocol`] defines the newline-delimited wire types, [`router`] maps
//! names to handlers and owns the reserved `core` methods, [`server`] enforces
//! endpoint and connection safety, and [`client`] is the blocking CLI side.

pub mod client;
pub mod protocol;
pub mod router;
pub mod server;

pub use client::{ClientError, IO_TIMEOUT, IpcClient, MAX_RESPONSE_SIZE, RetryPolicy};
pub use protocol::{
    CoreApplyOutcome, CoreFileOutcome, CoreFileState, CoreFileStatus, CoreReload, CoreStatus,
    Request, Response, WireError, WireErrorKind,
};
pub use router::{
    CORE_MODULE, ConfigCore, CoreHandler, HandlerError, HandlerErrorKind, HandlerResult,
    IpcHandler, Router, RouterError, decode_params, encode_response,
};
pub use server::{IpcServer, MAX_CONNECTIONS, REQUEST_LIMIT, ServerConfig, ServerError};
