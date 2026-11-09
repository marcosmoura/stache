//! Display adapters for `wallpapers.set` outcomes.
//!
//! These strings are the frozen contract of Part 04: they render typed data and
//! are never parsed back. Every substituted path, identity or message is a
//! serde JSON string, so control characters such as ESC become `\u001b`.
//! Target lists are compact JSON arrays in the operation's target order.

use stache_sdk::ipc::{WireError, WireErrorKind};

use crate::runtime::{
    WallpapersOutcomeCause, WallpapersOutcomeStatus, WallpapersSetOutcome, WallpapersTargetResult,
};

/// The success line, printed only for a complete known success.
pub const SUCCESS_LINE: &str = "Wallpaper set successfully.";

/// JSON string quoting, so control characters such as ESC become `\u001b`.
pub fn quoted(text: &str) -> String { serde_json::Value::String(text.to_owned()).to_string() }

/// The body for a lost, late, malformed or otherwise uncertain exchange.
///
/// No target identity is invented.
#[must_use]
pub fn response_uncertainty_body() -> String {
    "Wallpaper outcome is unknown after a send or response failure. Check the desktop before \
     retrying; no automatic retry was attempted."
        .to_owned()
}

/// The body for a typed refusal before any native change.
fn pre_native_body(category: &str, detail: &str) -> String {
    format!(
        "Wallpaper operation failed before native application ({category}): {}. No native change \
         was issued.",
        quoted(detail)
    )
}

/// The body for a typed wire error that proves no native change was issued.
///
/// The category is the wire kind tag and the detail is the server's message.
#[must_use]
pub fn wire_error_body(error: &WireError) -> Option<String> {
    let category = match error.kind {
        WireErrorKind::MalformedRequest => "malformedRequest",
        WireErrorKind::InvalidParams => "invalidParams",
        WireErrorKind::UnknownModule => "unknownModule",
        WireErrorKind::UnknownMethod => "unknownMethod",
        WireErrorKind::Disabled => "disabled",
        WireErrorKind::Unavailable => "unavailable",
        // A handler failure, such as a panic, may follow native issuance.
        WireErrorKind::Handler => return None,
    };
    Some(pre_native_body(category, &error.message))
}

fn ids(values: impl Iterator<Item = u32>) -> String {
    serde_json::to_string(&values.collect::<Vec<_>>()).unwrap_or_else(|_| "[]".to_owned())
}

fn application_body(outcome: &WallpapersSetOutcome) -> String {
    let tag = match outcome.status {
        WallpapersOutcomeStatus::Partial => "partial",
        WallpapersOutcomeStatus::Failure => "failure",
        WallpapersOutcomeStatus::Cancelled => "cancelled",
        WallpapersOutcomeStatus::Uncertain | WallpapersOutcomeStatus::FullSuccess => "uncertain",
    };
    let pick = |wanted: fn(&WallpapersTargetResult) -> bool, fallback: &[u32]| -> String {
        if outcome.targets.is_empty() {
            ids(fallback.iter().copied())
        } else {
            ids(outcome
                .targets
                .iter()
                .filter(|target| wanted(&target.target))
                .map(|target| target.display))
        }
    };
    let applied = pick(
        |t| matches!(t, WallpapersTargetResult::Applied),
        &outcome.applied,
    );
    let failed = pick(
        |t| matches!(t, WallpapersTargetResult::Failed { .. }),
        &outcome.failed,
    );
    let not_attempted = pick(
        |t| matches!(t, WallpapersTargetResult::NotAttempted),
        &outcome.not_attempted,
    );
    let unknown = pick(
        |t| matches!(t, WallpapersTargetResult::Unknown { .. }),
        &outcome.unknown,
    );
    format!(
        "Wallpaper application {tag}: applied={applied}; failed={failed}; \
         not-attempted={not_attempted}; unknown={unknown}. Check the desktop before retrying; no \
         automatic retry was attempted."
    )
}

fn cause_detail(cause: &WallpapersOutcomeCause) -> String {
    match cause {
        WallpapersOutcomeCause::InvalidSelection { message }
        | WallpapersOutcomeCause::Source { message }
        | WallpapersOutcomeCause::Processing { message }
        | WallpapersOutcomeCause::Native { message }
        | WallpapersOutcomeCause::Unavailable { message } => message.clone(),
        other => other.tag().to_owned(),
    }
}

/// Renders a handled outcome: `Ok(())` for a complete known success, or the
/// error body that follows `stache: Wallpaper error: `.
///
/// A native change counts as issued when any target applied, failed or has an
/// unknown result, or when the server reported `uncertain`. An `uncertain`
/// outcome without any target identity uses the response-uncertainty body
/// rather than inventing empty identity lists.
///
/// # Errors
/// Returns the body for every outcome that is not a full success.
pub fn render_outcome(outcome: &WallpapersSetOutcome) -> Result<(), String> {
    if outcome.status == WallpapersOutcomeStatus::FullSuccess {
        return Ok(());
    }
    let issued = !outcome.applied.is_empty()
        || !outcome.failed.is_empty()
        || !outcome.unknown.is_empty()
        || outcome
            .targets
            .iter()
            .any(|target| !matches!(target.target, WallpapersTargetResult::NotAttempted));
    if issued {
        return Err(application_body(outcome));
    }
    if outcome.status == WallpapersOutcomeStatus::Uncertain {
        return Err(response_uncertainty_body());
    }
    let Some(cause) = &outcome.cause else {
        return Err(response_uncertainty_body());
    };
    Err(match cause {
        WallpapersOutcomeCause::DifferentConfig { host, requested } => format!(
            "Running host uses config directory {}; requested {}. Use the same config directory \
             or stop that host.",
            quoted(host),
            quoted(requested)
        ),
        WallpapersOutcomeCause::NoAcceptedConfig => {
            "No accepted Wallpapers configuration is available. Fix wallpapers.toml and reload, \
             then retry."
                .to_owned()
        }
        WallpapersOutcomeCause::QueueFull => {
            "Wallpaper queue is full; no native change was issued. Retry after current \
             operations finish."
                .to_owned()
        }
        WallpapersOutcomeCause::DeadlineExpired => {
            "Wallpaper deadline expired before native application; no native change was issued. \
             Retry when ready."
                .to_owned()
        }
        WallpapersOutcomeCause::Cancelled => {
            "Wallpaper operation cancelled before native application; no native change was \
             issued."
                .to_owned()
        }
        other => pre_native_body(other.tag(), &cause_detail(other)),
    })
}
