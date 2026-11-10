//! Weather data for the Bar UI: the location resolver and the key reader.
//!
//! There is **no weather worker, timer or observer** in Rust. The UI asks once
//! per weather refresh (20 minutes and on reconnect) and [`WeatherService::read`]
//! answers with everything the UI fetch needs: the configured provider, the
//! trimmed Visual Crossing key and the location. The provider *availability*
//! (`isProviderAvailable` in the legacy UI: `auto` works without a key and uses
//! Visual Crossing only when the key is non-empty, `visualCrossing` is
//! unconfigured without a key, `openMeteo` never needs one) stays in the UI,
//! decided from [`BarWeather::provider`] and [`BarWeather::api_key`], so the
//! legacy semantics are preserved. The legacy Rust `is_enabled` comments say
//! otherwise and are wrong.
//!
//! # Secret and coordinate boundary
//!
//! The key and the coordinates exist only in [`BarWeather`], the response of
//! one `bar_weather` call. They are never part of [`BarPublished`](crate::BarPublished),
//! [`BarRuntimeStatus`](crate::BarRuntimeStatus), logs, errors or `Debug`
//! output (redacted by hand-written `Debug` impls). The service keeps the last
//! fix in memory only for [`WeatherTiming::reuse`] and only while a call could
//! use it.
//!
//! # Location resolver
//!
//! Per call, on the caller's (blocking-pool) thread: read the *currently
//! accepted* configuration, then at most one [`LocationSource::read`] at a time
//! (single flight). Concurrent calls for the same Bar run and configuration
//! revision share the in-flight result, bounded by the read timeout plus a
//! small margin; they never start a second read, even when the first hangs. The
//! outcome of a read (a fix or the reason there was none) is reused for
//! [`WeatherTiming::reuse`] for the same `(runtime generation, config revision)`,
//! so a double mount does not read twice. A reconfigure changes the revision, so
//! `precise_location` and `refresh_interval` apply at the next read without restarting Bar.
//!
//! Any [`LocationOutcome::Unavailable`] falls back to `default_location`
//! (`defaultLocation`, or `none` when that is empty) and records a typed
//! [`LocationDiagnostic`] in the Bar status (outside the Bar visuals; recording
//! it changes no sequence). When Bar is not running, has no panel or has no
//! accepted configuration, nothing is read at all.
//!
//! # Late consent
//!
//! A read that timed out on an open consent prompt answers
//! [`LocationUnavailable::NotDeterminedTimeout`] (so `default_location` is used
//! for that answer) and the source keeps one consent watcher (see
//! [`LocationSource`]). When the user decides, the watcher calls back once: the
//! service drops its cached outcome and bumps the Bar `weather_epoch` of the
//! run that asked, through the status cell, so the bridge emits a change and the
//! UI refetches at once. The bump is checked against the live runtime
//! generation, so a decision after a stop, failure or restart changes nothing.
//! The service watches the status cell and cancels the pending watcher
//! (releasing the manager, notifying nobody) as soon as the run it belongs to
//! is no longer running. Denied and restricted decisions bump too; the refetch
//! then cheaply returns the fallback with the right diagnostic. The signal
//! carries no coordinate, key or decision.
//!
//! After the read the run generation and configuration revision are checked
//! again; if either changed (stop, restart, reconfigure) the answer is
//! [`WeatherRead::Stale`] and nothing is recorded.

pub mod key;

use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use specta::Type;
use stache_macos::{
    ConsentNotifier, LocationFix, LocationOutcome, LocationRequest, LocationSource,
    LocationUnavailable,
};
use stache_sdk::commands::{Clock, SystemClock};

pub use self::key::{
    ApiKeyDiagnostic, ApiKeyProblem, ApiKeyRead, KEY_NAME, MAX_ENV_FILE_BYTES, parse_api_key,
    read_api_key,
};
use crate::config::{BarConfig, WeatherProvider};
use crate::status::{
    BarPanelState, BarPublished, BarRuntimeState, BarStatusSource, StatusSubscription,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How long a read outcome is reused for the same run and revision.
pub const REUSE_WINDOW: Duration = Duration::from_secs(60);

/// The slack added to the read timeout before a joining call gives up.
pub const JOIN_MARGIN: Duration = Duration::from_secs(2);

/// The configured provider mode (not the resolved one).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum BarWeatherProvider {
    /// Visual Crossing when the key is non-empty, otherwise Open Meteo.
    Auto,
    /// Visual Crossing only (unconfigured without a key).
    VisualCrossing,
    /// Open Meteo only.
    OpenMeteo,
}

impl From<WeatherProvider> for BarWeatherProvider {
    fn from(provider: WeatherProvider) -> Self {
        match provider {
            WeatherProvider::Auto => Self::Auto,
            WeatherProvider::VisualCrossing => Self::VisualCrossing,
            WeatherProvider::OpenMeteo => Self::OpenMeteo,
        }
    }
}

/// Where the weather location comes from.
#[derive(Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "source", rename_all = "camelCase")]
pub enum BarWeatherLocation {
    /// A native `CoreLocation` fix.
    #[serde(rename_all = "camelCase")]
    CoreLocation {
        /// Latitude in degrees.
        latitude: f64,
        /// Longitude in degrees.
        longitude: f64,
        /// macOS delivered an approximate fix.
        reduced_accuracy: bool,
    },
    /// The trimmed `weather.default_location` text. The UI parses coordinates
    /// or geocodes a city name, as legacy did.
    #[serde(rename_all = "camelCase")]
    DefaultLocation {
        /// The configured text.
        query: String,
    },
    /// No fix and `default_location` is empty.
    None,
}

impl fmt::Debug for BarWeatherLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CoreLocation { reduced_accuracy, .. } => f
                .debug_struct("CoreLocation")
                .field("reduced_accuracy", reduced_accuracy)
                .finish_non_exhaustive(),
            Self::DefaultLocation { .. } => f.write_str("DefaultLocation(<redacted>)"),
            Self::None => f.write_str("None"),
        }
    }
}

/// Everything one weather refresh needs. Carries a secret and coordinates:
/// returned only to the authorized Bar window, never persisted or broadcast.
#[derive(Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BarWeather {
    /// The accepted configuration revision this was computed for, as a decimal
    /// string.
    pub config_revision: String,
    /// The configured provider mode.
    pub provider: BarWeatherProvider,
    /// The trimmed Visual Crossing key; `None` when missing, empty, unreadable
    /// or not needed (`openMeteo` never reads the file).
    pub api_key: Option<String>,
    /// The location.
    pub location: BarWeatherLocation,
    /// Minutes between automatic refreshes, from the same accepted
    /// configuration as `config_revision`. Not secret.
    pub refresh_interval_minutes: u32,
}

impl fmt::Debug for BarWeather {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BarWeather")
            .field("config_revision", &self.config_revision)
            .field("provider", &self.provider)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("location", &self.location)
            .field("refresh_interval_minutes", &self.refresh_interval_minutes)
            .finish()
    }
}

/// The last location result, as a bare reason. Never a coordinate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum LocationDiagnostic {
    /// A fix was delivered.
    Located,
    /// The user denied Location access.
    Denied,
    /// Location access is restricted.
    Restricted,
    /// Location Services are off.
    ServicesDisabled,
    /// The consent decision stayed undetermined until the timeout.
    NotDeterminedTimeout,
    /// No fix arrived in time.
    Timeout,
    /// `CoreLocation` reported an error or an unusable fix.
    Error,
    /// The main bundle has no location usage description.
    NoUsageDescription,
    /// `CoreLocation` is not available to this caller.
    Unavailable,
}

impl From<LocationOutcome> for LocationDiagnostic {
    fn from(outcome: LocationOutcome) -> Self {
        match outcome {
            LocationOutcome::Located(_) => Self::Located,
            LocationOutcome::Unavailable(reason) => match reason {
                LocationUnavailable::Denied => Self::Denied,
                LocationUnavailable::Restricted => Self::Restricted,
                LocationUnavailable::ServicesDisabled => Self::ServicesDisabled,
                LocationUnavailable::NotDeterminedTimeout => Self::NotDeterminedTimeout,
                LocationUnavailable::Timeout => Self::Timeout,
                LocationUnavailable::Error => Self::Error,
                LocationUnavailable::NoUsageDescription => Self::NoUsageDescription,
                LocationUnavailable::Unavailable => Self::Unavailable,
            },
        }
    }
}

/// The weather conditions published with the Bar runtime status (host status,
/// tray). Enums and a path only: no coordinate, no key, no Bar visuals.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WeatherStatus {
    /// The result of the last location read of this run.
    pub location: Option<LocationDiagnostic>,
    /// Why the last key read produced no key.
    pub api_key: Option<ApiKeyDiagnostic>,
}

/// The seams and bounds of the service.
#[derive(Clone)]
pub struct WeatherParts {
    /// The one-shot location source (`SystemLocation` in production).
    pub location: Arc<dyn LocationSource>,
    /// The home directory for `~` in `api_keys`.
    pub home: Option<PathBuf>,
    /// The configuration directory relative `api_keys` resolve against.
    pub config_dir: PathBuf,
    /// The clock of the reuse window.
    pub clock: Arc<dyn Clock>,
    /// Time bounds.
    pub timing: WeatherTiming,
}

/// Time bounds, injectable for tests.
#[derive(Clone, Copy, Debug)]
pub struct WeatherTiming {
    /// The read timeout handed to the location source.
    pub read_timeout: Duration,
    /// Slack past the read timeout before a joining call gives up.
    pub join_margin: Duration,
    /// How long a read outcome is reused.
    pub reuse: Duration,
}

impl Default for WeatherTiming {
    fn default() -> Self {
        Self {
            read_timeout: stache_macos::location::DEFAULT_TIMEOUT,
            join_margin: JOIN_MARGIN,
            reuse: REUSE_WINDOW,
        }
    }
}

impl WeatherParts {
    /// Parts with the system clock and production bounds.
    #[must_use]
    pub fn new(
        location: Arc<dyn LocationSource>,
        home: Option<PathBuf>,
        config_dir: PathBuf,
    ) -> Self {
        Self {
            location,
            home,
            config_dir,
            clock: Arc::new(SystemClock::default()),
            timing: WeatherTiming::default(),
        }
    }

    /// The same parts with `clock`.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The same parts with `timing`.
    #[must_use]
    pub const fn with_timing(mut self, timing: WeatherTiming) -> Self {
        self.timing = timing;
        self
    }
}

/// The answer of [`WeatherService::read`].
#[derive(Debug)]
pub enum WeatherRead {
    /// The weather inputs.
    Ready(BarWeather),
    /// Bar is not serving weather (not running, no panel, no accepted
    /// configuration); nothing was read.
    NotRunning(Box<BarPublished>),
    /// Bar stopped, restarted or was reconfigured during the read; the caller
    /// refetches.
    Stale,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct RunKey {
    generation: u64,
    revision: u64,
}

struct Snapshot {
    key: RunKey,
    config: Arc<BarConfig>,
}

struct Flight {
    key: RunKey,
    done: Mutex<Option<LocationOutcome>>,
    ready: Condvar,
}

impl Flight {
    fn wait(&self, timeout: Duration) -> Option<LocationOutcome> {
        let deadline = Instant::now() + timeout;
        let mut done = lock(&self.done);
        loop {
            if let Some(outcome) = *done {
                return Some(outcome);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            done = self
                .ready
                .wait_timeout(done, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

struct Cached {
    key: RunKey,
    at: Duration,
    outcome: LocationOutcome,
}

#[derive(Default)]
struct State {
    cached: Option<Cached>,
    flight: Option<Arc<Flight>>,
}

/// Completes the flight even if the source panics.
struct Leader<'a> {
    service: &'a WeatherService,
    flight: Arc<Flight>,
    finished: bool,
}

impl Leader<'_> {
    fn finish(&mut self, outcome: LocationOutcome) {
        self.finished = true;
        let mut state = lock(&self.service.state);
        state.flight = None;
        state.cached = Some(Cached {
            key: self.flight.key,
            at: self.service.clock.now(),
            outcome,
        });
        drop(state);
        *lock(&self.flight.done) = Some(outcome);
        self.flight.ready.notify_all();
    }
}

impl Drop for Leader<'_> {
    fn drop(&mut self) {
        if !self.finished {
            // The source panicked: followers get an error, nothing is reused.
            let mut state = lock(&self.service.state);
            state.flight = None;
            drop(state);
            *lock(&self.flight.done) =
                Some(LocationOutcome::Unavailable(LocationUnavailable::Error));
            self.flight.ready.notify_all();
        }
    }
}

/// Answers `bar_weather`. Cheap to share; construction is inert and it owns no
/// thread, timer, observer or native handle.
pub struct WeatherService {
    status: BarStatusSource,
    location: Arc<dyn LocationSource>,
    home: Option<PathBuf>,
    config_dir: PathBuf,
    clock: Arc<dyn Clock>,
    timing: WeatherTiming,
    state: Arc<Mutex<State>>,
    /// The run generation whose read may have left a consent watcher; `0` none.
    watching: Arc<AtomicU64>,
    /// Cancels the watcher when its run stops. Dropping it unregisters.
    _run_watch: StatusSubscription,
}

impl WeatherService {
    /// A service over `parts`, reading Bar's run state from `status`.
    #[must_use]
    pub fn new(parts: WeatherParts, status: BarStatusSource) -> Self {
        let watching = Arc::new(AtomicU64::new(0));
        let run_watch = {
            let watching = Arc::clone(&watching);
            let status = status.clone();
            let location = Arc::clone(&parts.location);
            status.clone().subscribe(Arc::new(move || {
                let generation = watching.load(Ordering::SeqCst);
                if generation != 0
                    && !status.is_running(generation)
                    && watching
                        .compare_exchange(generation, 0, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                {
                    location.cancel_pending();
                }
            }))
        };
        Self {
            status,
            location: parts.location,
            home: parts.home,
            config_dir: parts.config_dir,
            clock: parts.clock,
            timing: parts.timing,
            state: Arc::new(Mutex::new(State::default())),
            watching,
            _run_watch: run_watch,
        }
    }

    fn snapshot(&self) -> Result<Snapshot, Box<BarPublished>> {
        let published = self.status.published();
        let panel = matches!(
            published.panel,
            BarPanelState::Requested | BarPanelState::Created | BarPanelState::Loaded
        );
        if published.state != BarRuntimeState::Running || !panel {
            return Err(Box::new(published));
        }
        let Some((revision, config)) = self.status.accepted_config() else {
            return Err(Box::new(published));
        };
        Ok(Snapshot {
            key: RunKey {
                generation: published.generation,
                revision,
            },
            config,
        })
    }

    /// Reads the weather inputs for the currently accepted configuration.
    ///
    /// Blocks for at most the location timeout plus the join margin and does
    /// file I/O: call it on the blocking pool, never the main thread, and hold
    /// no registry, module or status lock.
    #[must_use]
    pub fn read(&self) -> WeatherRead {
        let start = match self.snapshot() {
            Ok(snapshot) => snapshot,
            Err(published) => {
                lock(&self.state).cached = None;
                return WeatherRead::NotRunning(published);
            }
        };
        let weather = &start.config.weather;
        let outcome = self.locate(start.key, weather.precise_location);
        if outcome == LocationOutcome::Unavailable(LocationUnavailable::NotDeterminedTimeout)
            && !self.status.is_running(start.key.generation)
        {
            // The run ended while the read waited: the watcher it left is moot.
            self.watching.store(0, Ordering::SeqCst);
            self.location.cancel_pending();
        }
        let key = if weather.provider == WeatherProvider::OpenMeteo {
            ApiKeyRead { key: None, diagnostic: None }
        } else {
            key::read_api_key(weather, self.home.as_deref(), &self.config_dir)
        };
        // The run or revision may have changed while the read was pending.
        match self.snapshot() {
            Ok(now) if now.key == start.key => {}
            _ => return WeatherRead::Stale,
        }
        let note = WeatherStatus {
            location: Some(outcome.into()),
            api_key: key.diagnostic,
        };
        if !self.status.weather_note(start.key.generation, note) {
            return WeatherRead::Stale;
        }
        let location = match outcome {
            LocationOutcome::Located(LocationFix {
                latitude,
                longitude,
                reduced_accuracy,
                ..
            }) => BarWeatherLocation::CoreLocation {
                latitude,
                longitude,
                reduced_accuracy,
            },
            LocationOutcome::Unavailable(_) => {
                let query = weather.default_location.trim();
                if query.is_empty() {
                    BarWeatherLocation::None
                } else {
                    BarWeatherLocation::DefaultLocation { query: query.to_owned() }
                }
            }
        };
        WeatherRead::Ready(BarWeather {
            config_revision: start.key.revision.to_string(),
            provider: weather.provider.into(),
            api_key: key.key,
            location,
            refresh_interval_minutes: weather.refresh_interval,
        })
    }

    /// What a consent watcher calls when the user decides: forget the reused
    /// outcome and bump the epoch of `generation` (a no-op unless that run is
    /// still live). Only records and wakes.
    fn consent_notifier(&self, generation: u64) -> ConsentNotifier {
        let state = Arc::clone(&self.state);
        let status = self.status.clone();
        let watching = Arc::clone(&self.watching);
        Arc::new(move || {
            lock(&state).cached = None;
            let _ = watching.compare_exchange(generation, 0, Ordering::SeqCst, Ordering::SeqCst);
            let _ = status.weather_decided(generation);
        })
    }

    /// One location outcome for `key`: reused, shared with the in-flight read,
    /// or read once by this call.
    fn locate(&self, key: RunKey, precise: bool) -> LocationOutcome {
        let timeout_outcome = LocationOutcome::Unavailable(LocationUnavailable::Timeout);
        // A joined read of another run or revision is finished first, then the
        // loop looks again; three rounds is more than a race can need.
        for _ in 0..3 {
            let mut state = lock(&self.state);
            if let Some(cached) = &state.cached
                && cached.key == key
                && self.clock.now().saturating_sub(cached.at) < self.timing.reuse
            {
                return cached.outcome;
            }
            if let Some(flight) = state.flight.clone() {
                drop(state);
                let waited = flight.wait(self.timing.read_timeout + self.timing.join_margin);
                match waited {
                    // A hung read: never start a second one.
                    None => return timeout_outcome,
                    Some(outcome) if flight.key == key => return outcome,
                    Some(_) => continue,
                }
            }
            let flight = Arc::new(Flight {
                key,
                done: Mutex::new(None),
                ready: Condvar::new(),
            });
            state.flight = Some(Arc::clone(&flight));
            drop(state);
            let mut leader = Leader {
                service: self,
                flight,
                finished: false,
            };
            self.watching.store(key.generation, Ordering::SeqCst);
            let outcome = self.location.read_watched(
                LocationRequest {
                    precise,
                    timeout: self.timing.read_timeout,
                },
                self.consent_notifier(key.generation),
            );
            leader.finish(outcome);
            return outcome;
        }
        LocationOutcome::Unavailable(LocationUnavailable::Error)
    }
}
