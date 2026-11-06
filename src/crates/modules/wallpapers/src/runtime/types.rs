//! Typed `wallpapers.set` parameters and operation outcomes.
//!
//! These are the shared wire types for the host socket route and the CLI. The
//! transport keeps `Response::Ok` unchanged: a handled request always carries
//! one [`WallpapersSetOutcome`] in its data, and transport success never implies
//! that a wallpaper was applied.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use specta::Type;
use stache_macos::{DesktopError, DisplayId};
use stache_sdk::ipc::HandlerError;

use crate::apply::ApplyOutcome;
use crate::sources::SourceError;

/// Which wallpaper one request should use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WallpaperSelection {
    /// One explicit absolute image file.
    ///
    /// The path is literal: it is not trimmed, expanded from `~`, resolved
    /// against the host's working directory or looked up in the configured
    /// pool.
    File {
        /// An absolute path to an existing supported image file.
        path: String,
    },
    /// One zero-based index into the accepted pool order.
    ///
    /// The pool is the one produced by [`crate::resolve_pool`] from the
    /// accepted configuration, including duplicate entries and the
    /// configuration-directory base. `0` is the first entry.
    Collection {
        /// Zero-based pool position.
        index: u32,
    },
    /// One entry of the accepted pool found by file name or file stem.
    ///
    /// The host matches the untrimmed token case-insensitively against each
    /// accepted pool entry's file name or file stem, in pool order, and the
    /// first matching entry wins ([`crate::find_pool_entry`]). A miss is a typed
    /// `InvalidSelection`. Otherwise it behaves exactly like
    /// [`WallpaperSelection::Collection`] for the matched position.
    Name {
        /// The file name or stem to look up.
        name: String,
    },
    /// A random entry drawn independently for every addressed display.
    Random,
}

/// Which displays one request should address.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "camelCase")]
pub enum WallpaperScreen {
    /// Every display in the native snapshot.
    All,
    /// Entry zero of the native snapshot, the primary screen.
    Main,
    /// A one-based display index. Zero keeps the legacy primary alias and is
    /// treated as [`WallpaperScreen::Main`].
    Index {
        /// The requested one-based index (`0` means primary).
        index: u32,
    },
}

impl WallpaperScreen {
    /// Converts the wire selector into the shared target selector.
    #[must_use]
    pub const fn as_target(&self) -> crate::selection::ScreenTarget {
        match self {
            Self::All => crate::selection::ScreenTarget::All,
            Self::Main | Self::Index { index: 0 } => crate::selection::ScreenTarget::Main,
            Self::Index { index } => crate::selection::ScreenTarget::Index(*index),
        }
    }
}

/// Parameters for `wallpapers.set`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "camelCase")]
pub struct WallpapersSetParams {
    /// The client's canonical configuration directory.
    ///
    /// The server canonicalizes this value and its own accepted identity before
    /// any work. An existing symlink alias or a not-yet-created suffix resolves
    /// through its existing ancestors.
    pub config_directory: String,
    /// The image to apply.
    pub selection: WallpaperSelection,
    /// The displays to address.
    pub screen: WallpaperScreen,
}

/// The overall outcome of one handled request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "camelCase")]
pub enum WallpapersOutcomeStatus {
    /// Every addressed display received the image.
    FullSuccess,
    /// Some displays were applied and at least one failed or was skipped.
    Partial,
    /// No display was applied and the failure is known.
    Failure,
    /// Work was refused before a native change was issued, or was cancelled
    /// before every native change. No native effect is implied.
    Cancelled,
    /// A native call was issued but its result cannot be known here.
    Uncertain,
}

/// Why an outcome is not a full success.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WallpapersOutcomeCause {
    /// The client named a different configuration directory.
    DifferentConfig {
        /// The server's canonical accepted configuration directory.
        host: String,
        /// The client's canonical requested directory.
        requested: String,
    },
    /// No accepted configuration exists (initial malformed or rejected file).
    NoAcceptedConfig,
    /// One job was executing and one was already queued.
    QueueFull,
    /// The request's absolute monotonic deadline expired.
    DeadlineExpired,
    /// A caller or host cancellation was observed.
    Cancelled,
    /// The selector, explicit path or collection index is invalid.
    InvalidSelection {
        /// The actionable reason.
        message: String,
    },
    /// The accepted source pool could not be resolved or was empty.
    Source {
        /// The actionable reason.
        message: String,
    },
    /// Reading, decoding, resizing or publishing one output failed.
    Processing {
        /// The actionable reason.
        message: String,
    },
    /// A native desktop operation failed or was refused.
    Native {
        /// The actionable reason.
        message: String,
    },
    /// Required host state, such as the rotation coordinator thread, is
    /// unavailable.
    Unavailable {
        /// The actionable reason.
        message: String,
    },
}

impl WallpapersOutcomeCause {
    /// A compact stable tag for logs and CLI adapters.
    #[must_use]
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::DifferentConfig { .. } => "different-config",
            Self::NoAcceptedConfig => "no-accepted-config",
            Self::QueueFull => "queue-full",
            Self::DeadlineExpired => "deadline-expired",
            Self::Cancelled => "cancelled",
            Self::InvalidSelection { .. } => "invalid-selection",
            Self::Source { .. } => "source",
            Self::Processing { .. } => "processing",
            Self::Native { .. } => "native",
            Self::Unavailable { .. } => "unavailable",
        }
    }
}

/// A typed per-target failure category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "camelCase")]
pub enum WallpapersTargetFailure {
    /// The native setter rejected the image.
    Native,
    /// The native dispatch acknowledgement timed out before the call.
    Timeout,
    /// Cancellation was observed before the call.
    Cancelled,
    /// The request deadline passed before the call.
    Expired,
    /// The accepted configuration changed before the call.
    Stale,
    /// A call was issued and its result is unknown.
    Unknown,
    /// Reading, decoding or publishing an output failed.
    Processing,
    /// Any other typed native failure.
    Other,
}

impl WallpapersTargetFailure {
    const fn from_desktop(error: &DesktopError) -> Self {
        match error {
            DesktopError::Native { .. } => Self::Native,
            DesktopError::TimedOut { .. } => Self::Timeout,
            DesktopError::Cancelled { .. } => Self::Cancelled,
            DesktopError::Expired { .. } => Self::Expired,
            DesktopError::Stale { .. } => Self::Stale,
            DesktopError::Unknown { .. } => Self::Unknown,
            DesktopError::Dispatch { .. }
            | DesktopError::NotMainThread
            | DesktopError::DisplayMissing(_)
            | DesktopError::DisplayChanged(_)
            | DesktopError::RelativePath(_)
            | DesktopError::NonUtf8Path(_) => Self::Other,
        }
    }
}

/// The recorded result for one addressed display.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(
    tag = "result",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WallpapersTargetResult {
    /// The native call returned success.
    Applied,
    /// The native call returned a known failure.
    Failed {
        /// The typed failure category.
        reason: WallpapersTargetFailure,
        /// The actionable reason.
        message: String,
    },
    /// No native call was made for this display.
    NotAttempted,
    /// A native call was issued and its result is unknown.
    Unknown {
        /// The typed failure category.
        reason: WallpapersTargetFailure,
        /// The actionable reason.
        message: String,
    },
}

/// The recorded result for one display identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "camelCase")]
pub struct WallpapersTargetOutcome {
    /// The stable native display identity as an unsigned 32-bit number.
    pub display: u32,
    /// What happened to this display.
    pub target: WallpapersTargetResult,
}

/// A complete typed operation outcome carried inside `Response::Ok`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "camelCase")]
pub struct WallpapersSetOutcome {
    /// Overall status.
    pub status: WallpapersOutcomeStatus,
    /// Cause when the status is not [`WallpapersOutcomeStatus::FullSuccess`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<WallpapersOutcomeCause>,
    /// Displays whose native call returned success.
    pub applied: Vec<u32>,
    /// Displays whose native call returned a known failure.
    pub failed: Vec<u32>,
    /// Displays for which no native call was made.
    pub not_attempted: Vec<u32>,
    /// Displays whose native call result is unknown.
    pub unknown: Vec<u32>,
    /// One entry per addressed display, in operation order.
    pub targets: Vec<WallpapersTargetOutcome>,
}

impl WallpapersSetOutcome {
    /// Builds an outcome with no addressed display, such as a pre-flight
    /// refusal or a source failure.
    #[must_use]
    pub const fn refused(status: WallpapersOutcomeStatus, cause: WallpapersOutcomeCause) -> Self {
        Self {
            status,
            cause: Some(cause),
            applied: Vec::new(),
            failed: Vec::new(),
            not_attempted: Vec::new(),
            unknown: Vec::new(),
            targets: Vec::new(),
        }
    }

    /// Builds a cancelled-before-any-native outcome.
    #[must_use]
    pub const fn cancelled(cause: WallpapersOutcomeCause) -> Self {
        Self::refused(WallpapersOutcomeStatus::Cancelled, cause)
    }

    /// Whether this outcome is a complete known success.
    #[must_use]
    pub fn is_full_success(&self) -> bool { self.status == WallpapersOutcomeStatus::FullSuccess }

    /// Builds the outcome from one apply report.
    #[must_use]
    pub fn from_apply(outcome: &ApplyOutcome, cause: Option<WallpapersOutcomeCause>) -> Self {
        let order: Vec<DisplayId> = outcome
            .applied
            .iter()
            .chain(outcome.failed.iter().map(|(display, _)| display))
            .chain(outcome.unknown.iter().map(|(display, _)| display))
            .chain(outcome.not_attempted.iter())
            .copied()
            .collect();
        Self::from_apply_ordered(outcome, &order, cause)
    }

    /// Builds the outcome from one apply report in the caller's operation order.
    #[must_use]
    pub fn from_apply_ordered(
        outcome: &ApplyOutcome,
        order: &[DisplayId],
        cause: Option<WallpapersOutcomeCause>,
    ) -> Self {
        let targets = order
            .iter()
            .filter_map(|display| {
                let result = if outcome.applied.contains(display) {
                    WallpapersTargetResult::Applied
                } else if let Some((_, error)) =
                    outcome.failed.iter().find(|(entry, _)| entry == display)
                {
                    WallpapersTargetResult::Failed {
                        reason: WallpapersTargetFailure::from_desktop(error),
                        message: error.to_string(),
                    }
                } else if let Some((_, error)) =
                    outcome.unknown.iter().find(|(entry, _)| entry == display)
                {
                    WallpapersTargetResult::Unknown {
                        reason: WallpapersTargetFailure::from_desktop(error),
                        message: error.to_string(),
                    }
                } else if outcome.not_attempted.contains(display) {
                    WallpapersTargetResult::NotAttempted
                } else {
                    return None;
                };
                Some(WallpapersTargetOutcome {
                    display: display.raw(),
                    target: result,
                })
            })
            .collect();
        let status = if outcome.is_complete() {
            WallpapersOutcomeStatus::FullSuccess
        } else if !outcome.unknown.is_empty() {
            WallpapersOutcomeStatus::Uncertain
        } else if outcome.applied.is_empty() {
            WallpapersOutcomeStatus::Failure
        } else {
            WallpapersOutcomeStatus::Partial
        };
        let applied = outcome.applied.iter().map(|display| display.raw()).collect();
        let failed = outcome.failed.iter().map(|(display, _)| display.raw()).collect();
        let not_attempted = outcome.not_attempted.iter().map(|display| display.raw()).collect();
        let unknown = outcome.unknown.iter().map(|(display, _)| display.raw()).collect();
        Self {
            status,
            cause: if status == WallpapersOutcomeStatus::FullSuccess {
                None
            } else {
                cause.or_else(|| {
                    Some(WallpapersOutcomeCause::Native {
                        message: "one or more displays did not receive the wallpaper".to_owned(),
                    })
                })
            },
            applied,
            failed,
            not_attempted,
            unknown,
            targets,
        }
    }

    /// Converts a source-resolution failure into a pre-native outcome.
    #[must_use]
    pub fn source_failure(error: &SourceError) -> Self {
        Self::refused(
            WallpapersOutcomeStatus::Failure,
            WallpapersOutcomeCause::Source { message: error.to_string() },
        )
    }

    /// Converts a native error into a pre-native failure outcome.
    #[must_use]
    pub fn native_failure(error: &DesktopError) -> Self {
        let cause = match error {
            DesktopError::Cancelled { .. } => WallpapersOutcomeCause::Cancelled,
            DesktopError::Expired { .. } => WallpapersOutcomeCause::DeadlineExpired,
            DesktopError::Stale { .. } => WallpapersOutcomeCause::Native {
                message: "the accepted configuration changed before a native change".to_owned(),
            },
            _ => WallpapersOutcomeCause::Native { message: error.to_string() },
        };
        let status = match cause {
            WallpapersOutcomeCause::Cancelled | WallpapersOutcomeCause::DeadlineExpired => {
                WallpapersOutcomeStatus::Cancelled
            }
            _ => WallpapersOutcomeStatus::Failure,
        };
        Self::refused(status, cause)
    }
}

impl From<HandlerError> for WallpapersSetOutcome {
    fn from(error: HandlerError) -> Self {
        Self::refused(
            WallpapersOutcomeStatus::Failure,
            WallpapersOutcomeCause::InvalidSelection {
                message: error.message().to_owned(),
            },
        )
    }
}
