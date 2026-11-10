//! One-shot native location through `CoreLocation`, behind a fakeable trait.
//!
//! [`LocationSource::read`] is a **blocking, bounded** one-shot read that a
//! caller runs on its own worker thread, never on the main thread. The
//! production [`SystemLocation`] owns a `CLLocationManager` and its delegate on
//! the main thread for exactly one read and releases both afterwards, so
//! between reads there is no manager, no delegate and no observer. See
//! [`native`] for the unsafe code.
//!
//! # Authorization call
//!
//! The adapter calls `requestWhenInUseAuthorization` and never
//! `requestAlwaysAuthorization`:
//!
//! * Both are `macos(10.15)`+. `requestAlwaysAuthorization` needs the
//!   `NSLocationAlwaysAndWhenInUseUsageDescription` key as well, is "discouraged
//!   unless background launch behavior is genuinely required" in the SDK header
//!   and would over-ask for a read made while the app is running.
//! * The header says `requestWhenInUseAuthorization` "will do nothing" when the
//!   app is "not currently in use" or when `NSLocationWhenInUseUsageDescription`
//!   is missing. Stache is an `Accessory`-policy (tray-only) app, so whether
//!   macOS 26 treats it as "in use" and shows the prompt is **unverified on
//!   hardware**. The bounded wait turns a prompt that never appears into
//!   [`LocationUnavailable::NotDeterminedTimeout`], never into a hang or a
//!   coordinate.
//! * macOS reports a granted when-in-use decision as `authorizedAlways` (or
//!   `authorizedWhenInUse`); both count as authorized here.
//! * `NSLocationUsageDescription` is the pre-10.15 macOS key and is not used.
//!
//! The request is issued only while the status is `notDetermined`, and only
//! once per process; a decided status (denied, restricted, authorized) is never
//! re-requested, so `precise = true` can never produce a second prompt.
//!
//! # Accuracy
//!
//! * `precise = false` sets `desiredAccuracy = kCLLocationAccuracyReduced`
//!   (macOS 11+). It asks `CoreLocation` for an approximate fix and does not
//!   depend on the user's "Precise Location" switch, unlike
//!   `kCLLocationAccuracyKilometer`, which still only chooses the power/accuracy
//!   tradeoff of a precise authorization. A reduced accuracy authorization is a
//!   success with `reduced_accuracy: true`.
//! * `precise = true` sets `kCLLocationAccuracyBest`. If macOS then reports
//!   `accuracyAuthorization == reducedAccuracy`, the adapter calls
//!   `requestTemporaryFullAccuracyAuthorizationWithPurposeKey:completion:` at
//!   most once per process, and only when the bundle's
//!   `NSLocationTemporaryUsageDescriptionDictionary` has [`PURPOSE_KEY`].
//!   Otherwise (no purpose key, already asked, user declined) the reduced fix
//!   is accepted and flagged `reduced_accuracy: true`.
//!
//! # Usage description
//!
//! Before touching `CoreLocation` the adapter requires a non-empty
//! `NSLocationWhenInUseUsageDescription` in the main bundle's Info.plist;
//! without it the result is [`LocationUnavailable::NoUsageDescription`] (the
//! unbundled dev binary).
//!
//! # Privacy
//!
//! Coordinates never appear in `Debug` or `Display` output.

use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

pub mod native;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use native::SystemLocation;

/// The default bounded wait for one read, including a pending consent prompt.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// The `NSLocationTemporaryUsageDescriptionDictionary` purpose key the bundle
/// declares for the temporary full accuracy request.
pub const PURPOSE_KEY: &str = "StacheWeatherPreciseLocation";

/// The Info.plist key that must hold the when-in-use explanation.
pub const USAGE_DESCRIPTION_KEY: &str = "NSLocationWhenInUseUsageDescription";

/// The Info.plist key holding the temporary full accuracy purpose dictionary.
pub const TEMPORARY_DICTIONARY_KEY: &str = "NSLocationTemporaryUsageDescriptionDictionary";

/// One location read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocationRequest {
    /// `false`: approximate fix, accepting the user's reduced accuracy choice.
    /// `true`: best accuracy, asking for temporary full accuracy when needed.
    pub precise: bool,
    /// The longest the caller waits, a pending consent prompt included.
    pub timeout: Duration,
}

impl LocationRequest {
    /// A request with the [`DEFAULT_TIMEOUT`].
    #[must_use]
    pub const fn new(precise: bool) -> Self {
        Self {
            precise,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

/// A located fix. `Debug` and `Display` redact the coordinates.
#[derive(Clone, Copy, PartialEq)]
pub struct LocationFix {
    /// Latitude in degrees.
    pub latitude: f64,
    /// Longitude in degrees.
    pub longitude: f64,
    /// Horizontal accuracy radius in meters.
    pub accuracy_meters: f64,
    /// `true` when macOS delivered an approximate (reduced accuracy) fix.
    pub reduced_accuracy: bool,
}

impl fmt::Debug for LocationFix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocationFix")
            .field("accuracy_meters", &self.accuracy_meters)
            .field("reduced_accuracy", &self.reduced_accuracy)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for LocationFix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "located (coordinates redacted, reduced accuracy: {})",
            self.reduced_accuracy
        )
    }
}

/// Why no location was produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LocationUnavailable {
    /// The user denied Location access.
    Denied,
    /// Location access is restricted (for example by a profile).
    Restricted,
    /// Location Services are switched off system-wide.
    ServicesDisabled,
    /// The consent decision stayed undetermined until the timeout.
    NotDeterminedTimeout,
    /// No fix arrived before the timeout.
    Timeout,
    /// `CoreLocation` reported an error or an unusable fix.
    Error,
    /// The main bundle has no location usage description.
    NoUsageDescription,
    /// `CoreLocation` or the main thread is not available to this caller.
    Unavailable,
}

impl fmt::Display for LocationUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Denied => "Location access was denied",
            Self::Restricted => "Location access is restricted",
            Self::ServicesDisabled => "Location Services are turned off",
            Self::NotDeterminedTimeout => "Location consent was not decided in time",
            Self::Timeout => "no location arrived in time",
            Self::Error => "Location reported an error",
            Self::NoUsageDescription => "the app bundle has no location usage description",
            Self::Unavailable => "Location is not available here",
        })
    }
}

/// The result of one read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LocationOutcome {
    /// A fix.
    Located(LocationFix),
    /// No fix, and why.
    Unavailable(LocationUnavailable),
}

impl fmt::Display for LocationOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Located(fix) => fix.fmt(f),
            Self::Unavailable(reason) => reason.fmt(f),
        }
    }
}

/// The late consent callback.
///
/// Called once, from the main thread, when a pending consent decision becomes
/// decided after the read that waited for it already timed out. It must only
/// record and wake: no blocking, no native call, no location read.
pub type ConsentNotifier = Arc<dyn Fn() + Send + Sync>;

/// A one-shot location source. A read blocks for at most `request.timeout`
/// (plus scheduling slack) and must not be called on the main thread.
///
/// # Late consent
///
/// A read that times out while the consent prompt is still open
/// ([`LocationUnavailable::NotDeterminedTimeout`]) may leave **one** consent
/// watcher behind: the single manager and delegate stay alive only to observe
/// the authorization status. When the status becomes decided (authorized,
/// denied or restricted) the watcher releases both and calls the
/// [`ConsentNotifier`] given to [`read_watched`](Self::read_watched) exactly
/// once. The watcher never re-requests authorization and never reads a
/// location. [`cancel_pending`](Self::cancel_pending), or a newer read, ends it
/// silently.
pub trait LocationSource: Send + Sync {
    /// Reads the location once.
    fn read(&self, request: LocationRequest) -> LocationOutcome;

    /// Like [`read`](Self::read), and `on_decided` is called if a consent
    /// watcher left behind by this read sees the decision. The default has no
    /// watcher and never calls it.
    fn read_watched(
        &self,
        request: LocationRequest,
        on_decided: ConsentNotifier,
    ) -> LocationOutcome {
        let _ = on_decided;
        self.read(request)
    }

    /// Ends a pending consent watcher without notifying. Never blocks. A read
    /// in flight is not affected. The default has nothing to cancel.
    fn cancel_pending(&self) {}
}

// ---------------------------------------------------------------------------
// Pure policy shared by the native adapter and its tests.

/// `CLAuthorizationStatus` raw values.
pub mod status {
    /// `kCLAuthorizationStatusNotDetermined`.
    pub const NOT_DETERMINED: i32 = 0;
    /// `kCLAuthorizationStatusRestricted`.
    pub const RESTRICTED: i32 = 1;
    /// `kCLAuthorizationStatusDenied`.
    pub const DENIED: i32 = 2;
    /// `kCLAuthorizationStatusAuthorizedAlways`.
    pub const AUTHORIZED_ALWAYS: i32 = 3;
    /// `kCLAuthorizationStatusAuthorizedWhenInUse`.
    pub const AUTHORIZED_WHEN_IN_USE: i32 = 4;
}

/// What to do for an authorization status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorizationAction {
    /// Ask for when-in-use authorization (first and only time).
    Request,
    /// Undetermined and already asked in this process: only wait.
    Wait,
    /// Authorized: request the location.
    Proceed,
    /// Decided against: stop with this reason.
    Fail(LocationUnavailable),
}

/// Maps a raw `CLAuthorizationStatus` to an action. A decided status is never
/// re-requested, and an undetermined one is requested at most once.
#[must_use]
pub const fn authorization_action(status: i32, already_asked: bool) -> AuthorizationAction {
    match status {
        status::NOT_DETERMINED if already_asked => AuthorizationAction::Wait,
        status::NOT_DETERMINED => AuthorizationAction::Request,
        status::RESTRICTED => AuthorizationAction::Fail(LocationUnavailable::Restricted),
        status::DENIED => AuthorizationAction::Fail(LocationUnavailable::Denied),
        status::AUTHORIZED_ALWAYS | status::AUTHORIZED_WHEN_IN_USE => AuthorizationAction::Proceed,
        _ => AuthorizationAction::Fail(LocationUnavailable::Error),
    }
}

/// The Info.plist facts the adapter depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BundleKeys {
    /// A non-empty `NSLocationWhenInUseUsageDescription`.
    pub usage_description: bool,
    /// [`PURPOSE_KEY`] in `NSLocationTemporaryUsageDescriptionDictionary`.
    pub temporary_purpose: bool,
}

/// Whether to ask for temporary full accuracy: the caller wants precision,
/// macOS reports reduced accuracy, the bundle declares the purpose and this
/// process has not asked yet.
#[must_use]
pub const fn wants_temporary_full_accuracy(
    precise: bool,
    reduced: bool,
    keys: BundleKeys,
    already_asked: bool,
) -> bool {
    precise && reduced && keys.temporary_purpose && !already_asked
}

/// Maps a `CLError` code from `locationManager:didFailWithError:`.
#[must_use]
pub const fn error_outcome(code: isize) -> LocationUnavailable {
    match code {
        // kCLErrorDenied
        1 => LocationUnavailable::Denied,
        _ => LocationUnavailable::Error,
    }
}

/// Builds the outcome for a delivered fix; an invalid fix is an error, never a
/// bogus coordinate.
#[must_use]
pub fn fix_outcome(
    latitude: f64,
    longitude: f64,
    accuracy_meters: f64,
    reduced_accuracy: bool,
) -> LocationOutcome {
    let valid = latitude.is_finite()
        && longitude.is_finite()
        && (-90.0..=90.0).contains(&latitude)
        && (-180.0..=180.0).contains(&longitude)
        && accuracy_meters.is_finite()
        && accuracy_meters >= 0.0;
    if valid {
        LocationOutcome::Located(LocationFix {
            latitude,
            longitude,
            accuracy_meters,
            reduced_accuracy,
        })
    } else {
        LocationOutcome::Unavailable(LocationUnavailable::Error)
    }
}

/// What a consent watcher does with an authorization callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchStep {
    /// Still undetermined: keep waiting.
    Keep,
    /// Decided: the watcher ends and notifies, exactly once.
    Decided,
    /// No watcher, or a callback of another generation: nothing happens.
    Dropped,
}

/// The pure state of the single consent watcher: which read generation, if any,
/// is still waiting for the user's decision.
#[derive(Debug, Default)]
pub struct ConsentWatch {
    pending: Option<u64>,
}

impl ConsentWatch {
    /// Makes `generation` the only watched one (a newer read replaces it).
    pub const fn begin(&mut self, generation: u64) { self.pending = Some(generation); }

    /// Whether a watcher is pending.
    #[must_use]
    pub const fn is_pending(&self) -> bool { self.pending.is_some() }

    /// Ends the watcher without a decision; `true` when one was pending.
    pub const fn cancel(&mut self) -> bool { self.pending.take().is_some() }

    /// Judges an authorization callback of `generation` at raw `status`.
    pub const fn on_authorization(&mut self, generation: u64, status: i32) -> WatchStep {
        match self.pending {
            Some(pending) if pending == generation => {
                if status == status::NOT_DETERMINED {
                    WatchStep::Keep
                } else {
                    self.pending = None;
                    WatchStep::Decided
                }
            }
            _ => WatchStep::Dropped,
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A first-writer-wins completion for one read generation. A later `complete`
/// (a late delegate callback after the timeout, or a second callback) is
/// dropped.
pub struct Completion {
    generation: u64,
    slot: Mutex<Option<LocationOutcome>>,
    ready: Condvar,
}

impl Completion {
    /// A pending completion for `generation`.
    #[must_use]
    pub fn new(generation: u64) -> Arc<Self> {
        Arc::new(Self {
            generation,
            slot: Mutex::new(None),
            ready: Condvar::new(),
        })
    }

    /// The read generation this completion belongs to.
    #[must_use]
    pub const fn generation(&self) -> u64 { self.generation }

    /// Stores `outcome` when nothing is stored yet; `false` when dropped.
    pub fn complete(&self, outcome: LocationOutcome) -> bool {
        let mut slot = lock(&self.slot);
        if slot.is_some() {
            return false;
        }
        *slot = Some(outcome);
        drop(slot);
        self.ready.notify_all();
        true
    }

    /// Whether an outcome is stored.
    #[must_use]
    pub fn is_complete(&self) -> bool { lock(&self.slot).is_some() }

    /// Waits for the outcome up to `timeout`; `None` on timeout.
    #[must_use]
    #[allow(clippy::significant_drop_tightening)] // the guard is the wait state
    pub fn wait(&self, timeout: Duration) -> Option<LocationOutcome> {
        let deadline = Instant::now() + timeout;
        let mut slot = lock(&self.slot);
        loop {
            if let Some(outcome) = *slot {
                return Some(outcome);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            slot = self
                .ready
                .wait_timeout(slot, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// Which read generation is allowed to act. Events of any other generation
/// (late callbacks after a finish, a cancel or a newer read) are dropped.
#[derive(Debug, Default)]
pub struct GenerationGate {
    current: Option<u64>,
}

impl GenerationGate {
    /// Makes `generation` the only accepted one.
    pub const fn begin(&mut self, generation: u64) { self.current = Some(generation); }

    /// Whether an event of `generation` is still wanted.
    #[must_use]
    pub const fn accepts(&self, generation: u64) -> bool {
        matches!(self.current, Some(current) if current == generation)
    }

    /// Ends `generation`; `false` (and no change) when it is not current.
    pub const fn finish(&mut self, generation: u64) -> bool {
        if self.accepts(generation) {
            self.current = None;
            true
        } else {
            false
        }
    }
}
