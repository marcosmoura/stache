//! The fakeable transport seam and the lookup-gated attempt.

use std::fmt;
use std::time::Duration;

use stache_sdk::commands::SpawnGate;
use stache_sdk::threading::CancellationToken;

use super::cli::{CliOutcome, CliRunner};
use super::encode::EncodeError;

/// The bootstrap lookup of `git.felix.borders` failed: no service, nothing sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MissingService {
    /// The `kern_return_t` of the failing lookup (`-1` when the lookup succeeded
    /// without a usable port).
    pub code: i32,
}

impl fmt::Display for MissingService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "borders service not found (code {})", self.code)
    }
}

impl std::error::Error for MissingService {}

/// Why a send is proven not to have been enqueued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// The kernel rejected the message while validating it.
    Kernel(i32),
    /// The argv could not be encoded; nothing reached the kernel.
    InvalidArguments(EncodeError),
}

/// The result of one Mach send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    /// `mach_msg` returned `KERN_SUCCESS`: the message was delivered to the
    /// service queue. It is **not** an acknowledgement that settings applied.
    Sent,
    /// No right is held (no successful lookup); nothing was sent.
    MissingService,
    /// Proven not sent; see [`Rejection`].
    Rejected(Rejection),
    /// A timeout, interruption or unknown code: the message may or may not have
    /// been enqueued. Callers must not fall back or retry autonomously.
    Uncertain(i32),
}

/// Lookup and send against the borders service. Implemented by
/// [`BordersConnection`](super::BordersConnection) and by fakes.
pub trait BordersTransport {
    /// Looks the service up, replacing any previously held right.
    ///
    /// # Errors
    /// [`MissingService`] when the service is not registered.
    fn lookup(&mut self) -> Result<(), MissingService>;

    /// Sends one argv batch with a bounded deadline (clamped to at most
    /// [`MACH_SEND_DEADLINE_MAX`](super::MACH_SEND_DEADLINE_MAX)).
    fn send(&mut self, args: &[String], deadline: Duration) -> SendOutcome;
}

/// Proof that a lookup succeeded in this attempt. The only way to send or run
/// the CLI fallback.
pub struct ServiceAttempt<'a, T: BordersTransport + ?Sized> {
    transport: &'a mut T,
}

impl<'a, T: BordersTransport + ?Sized> ServiceAttempt<'a, T> {
    /// Starts an attempt with a fresh lookup. Nothing is sent or spawned when
    /// the service is absent.
    ///
    /// # Errors
    /// [`MissingService`] when the lookup fails.
    pub fn begin(transport: &'a mut T) -> Result<Self, MissingService> {
        transport.lookup()?;
        Ok(Self { transport })
    }

    /// Sends one batch over Mach.
    pub fn send(&mut self, args: &[String], deadline: Duration) -> SendOutcome {
        self.transport.send(args, deadline)
    }

    /// Looks the service up again through the same transport, to narrow the
    /// window between this attempt's lookup and a CLI fallback.
    ///
    /// # Errors
    /// [`MissingService`] when the service is no longer registered.
    pub fn confirm_present(&mut self) -> Result<(), MissingService> { self.transport.lookup() }

    /// Runs the bounded CLI fallback. Callers must not use it after a
    /// [`SendOutcome::Uncertain`] from this attempt. The service can still exit
    /// between lookup and CLI invocation (documented residual race, narrowed by [`Self::confirm_present`]).
    pub fn cli_fallback(
        &self,
        runner: &CliRunner,
        args: &[String],
        cancel: &CancellationToken,
        gate: &dyn SpawnGate,
    ) -> CliOutcome {
        runner.run(args, cancel, gate)
    }
}
