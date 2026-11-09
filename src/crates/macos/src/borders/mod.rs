//! Safe transport to an externally managed `JankyBorders` service.
//!
//! This module is the native half of Tiling part 09. It sends NUL-separated
//! argv batches to the Mach service `git.felix.borders` and offers a bounded
//! CLI fallback. It contains no policy: colors, styles, blacklists, pacing and
//! epochs belong to the Tiling runtime.
//!
//! # Authority and lifecycle
//!
//! * Stache **never launches, restarts or kills** a borders service. Every
//!   attempt starts with [`BordersTransport::lookup`]; only a successful lookup
//!   yields a [`ServiceAttempt`], and only a [`ServiceAttempt`] can send or run
//!   the CLI fallback. A missing service therefore sends nothing and spawns
//!   nothing.
//! * Owning a send right ([`BordersConnection`]) does not own the service or its
//!   global settings. Dropping or [`BordersConnection::release`] deallocates the
//!   right exactly once and sends no mutation.
//! * Residual race: the service can exit between the lookup and a CLI fallback
//!   invocation. The CLI is then run against no service; this is documented, not
//!   prevented, and the fallback is never used after an ambiguous Mach send.
//!
//! # Outcomes
//!
//! See [`SendOutcome`] for the exact `kern_return_t` mapping. `Sent` proves the
//! kernel accepted the message for the service, not that `JankyBorders`
//! understood or applied it.
//!
//! # Confinement
//!
//! [`BordersConnection`] is `!Send` and `!Sync`: one owner thread creates, uses
//! and drops it. There are no statics or caches.
//!
//! # Verification level
//!
//! Layout is asserted at compile time against the macOS SDK headers; everything
//! else is tested with fakes. Compatibility with a real `JankyBorders` version
//! is **unverified**.

mod cli;
mod encode;
mod mach;
mod transport;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use cli::{BORDERS_BINARY, CLI_CHILD_DEADLINE, CLI_OUTPUT_CAP, CliOutcome, CliRunner};
pub use encode::{EncodeError, MAX_PAYLOAD_BYTES, encode_mach_args};
pub use mach::{
    BordersConnection, MACH_SEND_DEADLINE_MAX, MACH_SEND_TIMED_OUT, SERVICE_NAME,
    classify_send_result, send_timeout_ms,
};
pub use transport::{BordersTransport, MissingService, Rejection, SendOutcome, ServiceAttempt};
