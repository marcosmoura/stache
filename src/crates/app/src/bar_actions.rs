//! The Bar's fixed click actions (overview Decision 8; part 05).
//!
//! A click on a Bar component opens one fixed target. The UI sends only a
//! [`BarAction`] enum value; the host maps it to a constant target and accepts
//! no name, path, URL or argument:
//!
//! | action    | target                                                                  |
//! | --------- | ----------------------------------------------------------------------- |
//! | `cpu`     | Activity Monitor (`com.apple.ActivityMonitor`)                          |
//! | `clock`   | Calendar (`com.apple.iCal`)                                             |
//! | `battery` | `x-apple.systempreferences:com.apple.Battery-Settings.extension`        |
//! | `wifi`    | `x-apple.systempreferences:com.apple.wifi-settings-extension`           |
//! | `weather` | Weather (`com.apple.weather`)                                           |
//! | `media`   | the player of the currently published media (table below)               |
//!
//! # The media click
//!
//! `media` carries **no identifier from the UI**. The host reads the bundle
//! identifier of the media the Bar currently publishes and maps exactly four
//! values to fixed applications ([`media_player`]): `com.spotify.client`
//! (Spotify), `com.microsoft.edgemac.Dev` (Microsoft Edge **Dev**),
//! `com.tidal.desktop` (Tidal) and `org.jeffvli.feishin` (Feishin). Each opens
//! through the same application path as the other actions. An unknown player or
//! no published media is [`BarActionOutcome::NotAttempted`]: nothing is
//! launched.
//!
//! # Application targets
//!
//! They reuse the main application adapter: a short-lived
//! [`ApplicationObservation`] of the shared [`ApplicationControl`] resolves the
//! bundle identifier to an installed path (`application_path`) and opens that
//! path through the issuance permit (`open_tracked`), exactly as noTunes does.
//! There is no shell `open` and no second launcher. The observation lives for
//! one click, so a disabled Bar owns no native application observer.
//!
//! # Settings targets
//!
//! They use the host's [`SettingsOpener`](crate::access::SettingsOpener): a
//! main-thread `NSWorkspace openURL:` of a fixed constant, behind a guard that
//! is rechecked on the main thread right before the send.
//!
//! # Outcomes (issued is not launched)
//!
//! [`BarActionOutcome`] is concrete command data, never an error string.
//! `completed` means macOS reported the request done (for an application the
//! open completion; for a pane that `AppKit` accepted the URL), not that a
//! window is visible. `uncertainAfterIssuance` means the request was sent but no
//! confirmation arrived inside the bound; it is never retried automatically.
//!
//! # Bounds and concurrency
//!
//! At most one action is in flight (one acknowledgement-bounded native
//! sequence); a click meanwhile returns `busy` and is dropped, never queued.
//! Every wait is bounded and runs off the main thread. When the Bar runtime stops
//! or restarts, the in-flight click's observation is cancelled and invalidated
//! synchronously, so the issuance permit refuses the send; a click already
//! sent finishes inert.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use specta::Type;
use stache_macos::{
    ApplicationControl, ApplicationError, ApplicationObservation, OpenCompletion, OpenRequest,
};
use stache_sdk::CancellationToken;

use crate::access::{PaneOpen, SettingsOpener, SettingsPane};
use crate::bar_bridge::BarUnavailableReason;

/// How long a click waits for the open completion after the request issued.
pub const COMPLETION_WAIT: Duration = Duration::from_secs(5);

/// How often the completion wait rechecks that the Bar runtime is current.
const QUANTUM: Duration = Duration::from_millis(20);

/// The Activity Monitor bundle identifier.
pub const ACTIVITY_MONITOR: &str = "com.apple.ActivityMonitor";

/// The Calendar bundle identifier.
pub const CALENDAR: &str = "com.apple.iCal";

/// The Weather bundle identifier.
pub const WEATHER: &str = "com.apple.weather";

/// The Spotify bundle identifier.
pub const SPOTIFY: &str = "com.spotify.client";

/// The Microsoft Edge **Dev** bundle identifier (the legacy click map's choice;
/// there is no stable Edge substitution).
pub const EDGE_DEV: &str = "com.microsoft.edgemac.Dev";

/// The Tidal bundle identifier.
pub const TIDAL: &str = "com.tidal.desktop";

/// The Feishin bundle identifier.
pub const FEISHIN: &str = "org.jeffvli.feishin";

/// Maps a published media bundle identifier to the one application a media
/// click opens. Exact match only; anything else is `None`.
#[must_use]
pub fn media_player(bundle_identifier: &str) -> Option<&'static str> {
    [SPOTIFY, EDGE_DEV, TIDAL, FEISHIN]
        .into_iter()
        .find(|known| *known == bundle_identifier)
}

/// The Bar components that can be clicked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum BarAction {
    /// Opens Activity Monitor.
    Cpu,
    /// Opens Calendar.
    Clock,
    /// Opens the Battery pane of System Settings.
    Battery,
    /// Opens the Wi-Fi pane of System Settings.
    Wifi,
    /// Opens Weather.
    Weather,
    /// Opens the player of the currently published media (see the module
    /// docs); the UI sends no identifier.
    Media,
}

/// The fixed target a [`BarAction`] maps to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarTarget {
    /// An installed application by bundle identifier.
    Application(&'static str),
    /// A System Settings pane.
    Pane(SettingsPane),
}

impl BarAction {
    /// The fixed target of this action; `None` for [`BarAction::Media`], whose
    /// target depends on the published media (see [`media_player`]).
    #[must_use]
    pub const fn target(self) -> Option<BarTarget> {
        match self {
            Self::Cpu => Some(BarTarget::Application(ACTIVITY_MONITOR)),
            Self::Clock => Some(BarTarget::Application(CALENDAR)),
            Self::Battery => Some(BarTarget::Pane(SettingsPane::Battery)),
            Self::Wifi => Some(BarTarget::Pane(SettingsPane::Wifi)),
            Self::Weather => Some(BarTarget::Application(WEATHER)),
            Self::Media => None,
        }
    }
}

/// The typed result of `bar_action`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BarActionOutcome {
    /// The request was issued and macOS reported it done. This is not proof
    /// that a window is visible.
    Completed,
    /// The Bar was no longer current (stopped, restarted or its panel
    /// replaced) before the request was sent; nothing was sent.
    Stale,
    /// The click was cancelled before issuance (for example a rejected
    /// dispatch); nothing was sent.
    NotIssued,
    /// The click has no target: a media click while no media is published or
    /// the player is not one of the four known ones. Nothing was launched.
    NotAttempted,
    /// macOS or the adapter refused the request.
    #[serde(rename_all = "camelCase")]
    Refused {
        /// A short diagnostic; not for display.
        detail: String,
    },
    /// The target application is not installed.
    NotInstalled,
    /// An acknowledgement did not arrive before the request could issue;
    /// nothing was sent and nothing will be.
    TimedOutBeforeIssuance,
    /// The request was issued but no confirmation arrived inside the bound. It
    /// may still complete; it is never retried automatically.
    UncertainAfterIssuance,
    /// Another action is in flight; this click was dropped, not queued.
    Busy,
    /// The calling window may not trigger actions.
    Unauthorized,
    /// The Bar is not running.
    #[serde(rename_all = "camelCase")]
    Unavailable {
        /// Why.
        reason: BarUnavailableReason,
        /// The failure detail, if any.
        detail: Option<String>,
    },
}

/// What authorizes one click and tells whether it is still current.
pub trait ActionSource: Send + Sync {
    /// Whether the Bar runtime generation that authorized the click is still
    /// running. Cheap and lock-light: it also runs from status notifications.
    fn runtime_current(&self) -> bool;

    /// Whether the calling window is still the live Bar panel.
    fn caller_current(&self) -> bool;

    /// The bundle identifier of the media the Bar currently publishes, if any.
    /// A media click maps it to a fixed application; the UI supplies none.
    fn media_bundle(&self) -> Option<String>;

    /// Calls `wake` after every Bar status change until the returned guard is
    /// dropped. `wake` only records and wakes.
    fn watch(&self, wake: Arc<dyn Fn() + Send + Sync>) -> Box<dyn Send>;
}

fn current(source: &dyn ActionSource) -> bool {
    source.runtime_current() && source.caller_current()
}

/// Releases the single in-flight slot.
struct Slot<'a>(&'a AtomicBool);

impl<'a> Slot<'a> {
    fn take(flag: &'a AtomicBool) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| Self(flag))
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) { self.0.store(false, Ordering::SeqCst); }
}

/// Stops the click's observation when the click ends.
struct Ending(Arc<ApplicationObservation>);

impl Drop for Ending {
    fn drop(&mut self) {
        self.0.invalidate();
        // A failed removal keeps ownership; the observation's own drop retries.
        let _ = self.0.stop();
    }
}

/// Runs fixed Bar actions over the host's native adapters.
pub struct BarActions {
    control: ApplicationControl,
    settings: Arc<dyn SettingsOpener>,
    busy: AtomicBool,
    completion_wait: Duration,
}

impl BarActions {
    /// Actions over the shared application control and the host settings opener.
    #[must_use]
    pub fn new(control: ApplicationControl, settings: Arc<dyn SettingsOpener>) -> Self {
        Self::with_completion_wait(control, settings, COMPLETION_WAIT)
    }

    /// Like [`new`](Self::new) with an explicit completion bound (tests).
    #[must_use]
    pub fn with_completion_wait(
        control: ApplicationControl,
        settings: Arc<dyn SettingsOpener>,
        completion_wait: Duration,
    ) -> Self {
        Self {
            control,
            settings,
            busy: AtomicBool::new(false),
            completion_wait,
        }
    }

    /// Runs `action` for an already authorized click. Blocks for at most the
    /// adapters' bounds, so it must not be called on the main thread.
    #[must_use]
    pub fn run(&self, action: BarAction, source: &Arc<dyn ActionSource>) -> BarActionOutcome {
        let Some(_slot) = Slot::take(&self.busy) else {
            return BarActionOutcome::Busy;
        };
        if !current(source.as_ref()) {
            return BarActionOutcome::Stale;
        }
        let target = match action.target() {
            Some(target) => target,
            None => match source.media_bundle().as_deref().and_then(media_player) {
                Some(bundle) => BarTarget::Application(bundle),
                None => return BarActionOutcome::NotAttempted,
            },
        };
        match target {
            BarTarget::Pane(pane) => self.open_pane(pane, source),
            BarTarget::Application(bundle) => self.open_application(bundle, source),
        }
    }

    fn open_pane(&self, pane: SettingsPane, source: &Arc<dyn ActionSource>) -> BarActionOutcome {
        let at_send = Arc::clone(source);
        match self.settings.open_pane(pane, Arc::new(move || current(at_send.as_ref()))) {
            PaneOpen::Opened => BarActionOutcome::Completed,
            PaneOpen::NotIssued if !current(source.as_ref()) => BarActionOutcome::Stale,
            PaneOpen::NotIssued => BarActionOutcome::NotIssued,
            PaneOpen::Refused(detail) => BarActionOutcome::Refused { detail },
            PaneOpen::TimedOutBeforeIssuance => BarActionOutcome::TimedOutBeforeIssuance,
            PaneOpen::UncertainAfterIssuance => BarActionOutcome::UncertainAfterIssuance,
        }
    }

    fn open_application(
        &self,
        bundle: &'static str,
        source: &Arc<dyn ActionSource>,
    ) -> BarActionOutcome {
        let token = CancellationToken::new();
        let slot: Arc<OnceLock<Arc<ApplicationObservation>>> = Arc::new(OnceLock::new());
        // Registered before the observation exists, so a stop at any point
        // cancels the install and invalidates the permit's generation.
        let _watch = {
            let current_source = Arc::clone(source);
            let token = token.clone();
            let slot = Arc::clone(&slot);
            source.watch(Arc::new(move || {
                if !current_source.runtime_current() {
                    token.cancel();
                    if let Some(observation) = slot.get() {
                        observation.invalidate();
                    }
                }
            }))
        };
        let observation = match self.control.install(&token, |_| {}) {
            Ok(observation) => Arc::new(observation),
            Err(error) => return Self::before_issuance(&error),
        };
        let _ending = Ending(Arc::clone(&observation));
        let _ = slot.set(Arc::clone(&observation));
        if !source.runtime_current() {
            observation.invalidate();
            return BarActionOutcome::Stale;
        }
        let path = match observation.application_path(bundle) {
            Ok(Some(path)) => path,
            Ok(None) => return BarActionOutcome::NotInstalled,
            Err(error) => return Self::before_issuance(&error),
        };
        if !current(source.as_ref()) {
            return BarActionOutcome::Stale;
        }
        let accepted = match observation.accept() {
            Ok(accepted) => accepted,
            Err(error) => return Self::before_issuance(&error),
        };
        let (sender, receiver) = mpsc::channel();
        match observation.open_tracked(accepted, &path, move |completion| {
            let _ = sender.send(completion);
        }) {
            Ok(OpenRequest::Stale) => BarActionOutcome::Stale,
            Ok(OpenRequest::Issued) => self.await_completion(&receiver, source.as_ref()),
            // Issued but unconfirmed: never retried.
            Err(failure) if failure.issued => BarActionOutcome::UncertainAfterIssuance,
            Err(failure) => Self::before_issuance(&failure.error),
        }
    }

    fn await_completion(
        &self,
        receiver: &mpsc::Receiver<OpenCompletion>,
        source: &dyn ActionSource,
    ) -> BarActionOutcome {
        let deadline = Instant::now() + self.completion_wait;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match receiver.recv_timeout(remaining.min(QUANTUM)) {
                Ok(OpenCompletion::Current(_, result) | OpenCompletion::Stale(_, result)) => {
                    return match result {
                        Ok(()) => BarActionOutcome::Completed,
                        Err(error) => BarActionOutcome::Refused { detail: error.to_string() },
                    };
                }
                Err(RecvTimeoutError::Timeout) => {
                    // Already issued: a stopped runtime or the bound ends the wait.
                    if remaining <= QUANTUM || !source.runtime_current() {
                        return BarActionOutcome::UncertainAfterIssuance;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return BarActionOutcome::UncertainAfterIssuance;
                }
            }
        }
    }

    /// Classifies a failure that happened before the request could issue.
    fn before_issuance(error: &ApplicationError) -> BarActionOutcome {
        match error {
            ApplicationError::TimedOut => BarActionOutcome::TimedOutBeforeIssuance,
            ApplicationError::Cancelled | ApplicationError::NotInstalled => BarActionOutcome::Stale,
            ApplicationError::Dispatch(_) => BarActionOutcome::NotIssued,
            other => BarActionOutcome::Refused { detail: other.to_string() },
        }
    }
}

#[cfg(test)]
#[path = "bar_actions_tests.rs"]
mod tests;
