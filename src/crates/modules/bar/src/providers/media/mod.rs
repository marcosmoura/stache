//! The media provider: one owned `MediaRemote` adapter child per Bar run.
//!
//! # Data path
//!
//! The vendored adapter (`/usr/bin/perl <script> <framework> <test client>
//! stream --no-diff`, absolute paths, see `resources/media-control/PROVENANCE.md`)
//! prints one JSON line per change. [`owner::run`] reads those lines through the
//! SDK's bounded [`LineStream`](stache_sdk::commands::LineStream), [`parse`]
//! replaces the complete state per line, and the result is published into the
//! Bar status cell as `BarPublished.media`. The artwork never travels in the
//! view: a decode thread turns the base64 image into a 128x128 PNG, stores it
//! under its SHA-256 id ([`artwork`]) and only the id is published. The UI then
//! fetches the bytes with `bar_media_artwork`.
//!
//! # Lifecycle
//!
//! The worker exists only inside a [`ProviderMonitor`](super::ProviderMonitor),
//! so a disabled, paused, zero-size, gated or failed Bar owns no media thread
//! and no child. Unavailable resources (missing files, not arm64) spawn
//! nothing and are not retried until the Bar restarts. A stop invalidates the
//! published media first, then terminates the exact child and reaps it; an
//! unreaped child stays owned by the executor and keeps the monitor unsettled.
//!
//! # Restart policy
//!
//! A child that exits, crashes or closes its output is restarted after a
//! cancellation-wakeable backoff of 1, 2, 5, 10, 30 s and then 60 s per
//! consecutive failure. A run that streamed for at least 60 s resets the count.
//! While restarting, the media is hidden; from the third consecutive failure
//! the status is `unavailable`. After [`GIVE_UP_AFTER_UNAVAILABLE`] (5) further
//! consecutive failed cycles (8 failures in all, 168 s of backoff) the
//! worker gives up: it spawns nothing more, the status stays `unavailable` with
//! [`GAVE_UP_REASON`] plus the last failure, and Media stays hidden until a new
//! Bar run (start, or resume after a pause) recreates the provider; a plain
//! reconfigure keeps the run and does not. Bar stays running
//! throughout.

#![allow(clippy::significant_drop_tightening)] // short guards around one publish

pub mod artwork;
pub mod owner;
pub mod parse;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

pub use artwork::{ArtworkLookup, ArtworkStore};
use serde::{Deserialize, Serialize};
use specta::Type;
use stache_sdk::commands::{
    Clock, CommandEnvironment, CommandExecutor, OsProcessFactory, SystemClock,
};

/// The fixed perl interpreter (a base-system tool).
pub const PERL_PATH: &str = "/usr/bin/perl";
/// The restart delays, by consecutive failure (the last repeats).
pub const BACKOFF: [Duration; 6] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(60),
];
/// A run that streamed this long resets the failure count.
pub const RESET_AFTER: Duration = Duration::from_secs(60);
/// Consecutive failures from which the status is `unavailable`.
pub const UNAVAILABLE_AFTER: u32 = 3;
/// Further consecutive failed cycles, once the status is `unavailable`, after
/// which the worker stops respawning the adapter.
pub const GIVE_UP_AFTER_UNAVAILABLE: u32 = 5;
/// The reason published when the worker has stopped respawning the adapter.
pub const GAVE_UP_REASON: &str = "the media adapter keeps failing; it is not restarted until the \
                                  Bar restarts";
/// The per-line cap of the adapter stream (an artwork line is base64 up to
/// 16 MiB plus the other fields).
pub const LINE_CAP: usize = 24 * 1024 * 1024;
/// The retained stderr tail of the adapter.
pub const STDERR_TAIL: usize = 8 * 1024;
/// The characters of stderr published in the diagnostic.
pub const STDERR_PUBLISHED_CHARS: usize = 512;

/// What the Bar shows for the playing media. The artwork is an id, never bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MediaReading {
    /// The track title (never empty).
    pub title: String,
    /// The artist; empty when the player supplies none.
    pub artist: String,
    /// The album.
    pub album: Option<String>,
    /// The player's bundle identifier; empty when unknown.
    pub bundle_identifier: String,
    /// The media type reported by the player.
    pub media_type: Option<String>,
    /// Whether the media is playing (otherwise the UI shows `Paused: `).
    pub playing: bool,
    /// The id of the 128x128 PNG artwork: 64 lowercase hex digits. Fetch the
    /// bytes with `bar_media_artwork`. `None` while decoding or when the
    /// current state carries no usable artwork.
    pub artwork_id: Option<String>,
}

/// Whether the adapter is delivering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum MediaRunState {
    /// The adapter child is running.
    Running,
    /// The adapter ended and is being restarted; media is hidden.
    Restarting,
    /// The adapter cannot be used (resources, architecture or repeated
    /// failure); media is hidden and Bar stays running.
    Unavailable,
}

/// The typed media condition shown in `BarRuntimeStatus` (never in the view).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MediaStatus {
    /// The state.
    pub state: MediaRunState,
    /// Why it is not running.
    pub reason: Option<String>,
    /// The sanitized, bounded tail of the adapter's stderr from the last run.
    pub stderr_tail: Option<String>,
}

impl MediaStatus {
    pub(crate) const fn running() -> Self {
        Self {
            state: MediaRunState::Running,
            reason: None,
            stderr_tail: None,
        }
    }
}

/// One publication of the media worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaUpdate {
    /// The media to show, or `None` to hide it.
    pub reading: Option<MediaReading>,
    /// The adapter condition.
    pub status: MediaStatus,
}

/// Receives each media publication. Must not block.
pub type PublishMedia = Arc<dyn Fn(MediaUpdate) + Send + Sync>;

/// The validated absolute adapter paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaPaths {
    /// `mediaremote-adapter.pl`.
    pub script: PathBuf,
    /// `MediaRemoteAdapter.framework`.
    pub framework: PathBuf,
    /// `MediaRemoteAdapterTestClient`.
    pub test_client: PathBuf,
}

/// Resolves the adapter or says why it is unavailable. Runs on the media
/// worker, never on the host thread.
pub type MediaResolver = Arc<dyn Fn() -> Result<MediaPaths, String> + Send + Sync>;

/// Time and size bounds, injectable so tests need no real delay.
#[derive(Clone, Copy, Debug)]
pub struct MediaTiming {
    /// The restart delays by consecutive failure (the last repeats).
    pub backoff: [Duration; 6],
    /// A run this long resets the failure count.
    pub reset_after: Duration,
    /// The `SIGTERM` grace of a stop.
    pub term_grace: Duration,
    /// How many times an unreaped child is rechecked at stop.
    pub reap_retries: u32,
    /// The wait between those rechecks.
    pub reap_retry_wait: Duration,
    /// The per-line cap.
    pub line_cap: usize,
}

impl Default for MediaTiming {
    fn default() -> Self {
        Self {
            backoff: BACKOFF,
            reset_after: RESET_AFTER,
            term_grace: stache_sdk::commands::DEFAULT_TERM_GRACE,
            reap_retries: 5,
            reap_retry_wait: Duration::from_millis(200),
            line_cap: LINE_CAP,
        }
    }
}

/// Everything the media provider needs, injected by the host.
#[derive(Clone)]
pub struct MediaParts {
    /// The executor that owns the adapter child (its own retained slots).
    pub executor: Arc<CommandExecutor>,
    /// The one time source of the executor and the restart schedule.
    pub clock: Arc<dyn Clock>,
    /// The interpreter.
    pub perl: PathBuf,
    /// Resolves the adapter paths.
    pub resolver: MediaResolver,
    /// The artwork store shared with `bar_media_artwork`.
    pub store: Arc<ArtworkStore>,
    /// Bounds.
    pub timing: MediaTiming,
}

impl MediaParts {
    /// The production parts over real processes and the real clock.
    #[must_use]
    pub fn system(resolver: MediaResolver, store: Arc<ArtworkStore>) -> Self {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::default());
        let executor = Arc::new(CommandExecutor::with_parts(
            Arc::new(OsProcessFactory),
            Arc::clone(&clock),
            CommandEnvironment::from_env(),
        ));
        Self {
            executor,
            clock,
            perl: PathBuf::from(PERL_PATH),
            resolver,
            store,
            timing: MediaTiming::default(),
        }
    }
}

/// How the artwork of the current state is known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ArtPlan {
    /// The state carries no usable artwork.
    None,
    /// The artwork is already stored under this id.
    Known(String),
    /// The artwork with this fingerprint is being decoded.
    Pending(u64),
}

struct PublisherState {
    reading: Option<MediaReading>,
    art_key: Option<u64>,
    status: MediaStatus,
    closed: bool,
    sent: Option<MediaUpdate>,
}

/// Serializes every media publication of one worker and drops duplicates.
///
/// The owner thread and the decode thread both publish through it, under one
/// lock, so the order is the order of the calls and nothing is sent after
/// [`close`](Self::close).
pub(crate) struct Publisher {
    state: Mutex<PublisherState>,
    publish: PublishMedia,
}

impl Publisher {
    pub(crate) fn new(publish: PublishMedia) -> Self {
        Self {
            state: Mutex::new(PublisherState {
                reading: None,
                art_key: None,
                status: MediaStatus::running(),
                closed: false,
                sent: None,
            }),
            publish,
        }
    }

    fn flush(&self, state: &mut PublisherState) {
        let update = MediaUpdate {
            reading: state.reading.clone(),
            status: state.status.clone(),
        };
        if state.sent.as_ref() != Some(&update) {
            state.sent = Some(update.clone());
            (self.publish)(update);
        }
    }

    /// Replaces the complete state.
    pub(crate) fn set_state(&self, mut reading: MediaReading, plan: ArtPlan) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closed {
            return;
        }
        match plan {
            ArtPlan::None => state.art_key = None,
            ArtPlan::Known(id) => {
                state.art_key = None;
                reading.artwork_id = Some(id);
            }
            ArtPlan::Pending(key) => state.art_key = Some(key),
        }
        state.reading = Some(reading);
        self.flush(&mut state);
    }

    /// Nothing is playing.
    pub(crate) fn set_absent(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closed {
            return;
        }
        state.reading = None;
        state.art_key = None;
        self.flush(&mut state);
    }

    /// Hides the media and records the condition.
    pub(crate) fn hide(&self, status: MediaStatus) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closed {
            return;
        }
        state.reading = None;
        state.art_key = None;
        state.status = status;
        self.flush(&mut state);
    }

    /// Records the condition without touching the media.
    pub(crate) fn set_status(&self, status: MediaStatus) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closed {
            return;
        }
        state.status = status;
        self.flush(&mut state);
    }

    /// Whether the current state still waits for the artwork `key`.
    pub(crate) fn expects(&self, key: u64) -> bool {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).art_key == Some(key)
    }

    /// Attaches a decoded artwork to the state that is still waiting for it.
    pub(crate) fn attach(&self, key: u64, id: &str) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closed || state.art_key != Some(key) {
            return false;
        }
        let Some(reading) = state.reading.as_mut() else {
            return false;
        };
        reading.artwork_id = Some(id.to_owned());
        state.art_key = None;
        self.flush(&mut state);
        true
    }

    /// Hides the media for good: nothing is published after this.
    pub(crate) fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closed {
            return;
        }
        state.reading = None;
        state.art_key = None;
        self.flush(&mut state);
        state.closed = true;
    }
}

#[cfg(test)]
mod tests;
