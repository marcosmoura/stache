//! Safe per-display desktop-image adapter.
//!
//! This is the native boundary the Wallpapers module uses. It snapshots
//! public `AppKit` screen data and sets one display's desktop image through the
//! public `NSWorkspace` per-display setter. The [`GuardedDesktopBackend`]
//! extension adds caller cancellation, an absolute monotonic deadline and a
//! pure current-generation predicate to both operations; the unguarded
//! [`DesktopBackend`] methods remain for existing callers. No processing, cache
//! or orchestration lives here.
//!
//! # Ownership
//!
//! ```text
//! module
//!   ├─ DisplayId / DisplayFrame / DisplayEntry / DisplaySnapshot (plain owned data)
//!   ├─ Arc<dyn DesktopBackend> or NativeDesktop (Send + Sync)
//!   │    └─ Arc<dyn MainThreadExecutor>
//!   │         └─ MainThread -> pthread_main_np + libdispatch
//!   └─ NativeDesktop
//!        ├─ snapshot: NSScreen::screens -> owned entries
//!        └─ set_image: re-resolve by DisplayId, recheck frame/scale, NSWorkspace setter
//! ```
//!
//! `NSScreen` instances, their arrays and `NSWorkspace` stay inside the
//! main-thread closure. Only [`DisplaySnapshot`] and [`DisplayId`] cross
//! threads; both are owned values with no pointer and no `unsafe impl
//! Send`/`Sync`.
//!
//! # Identity
//!
//! [`DisplayId`] is the `CGDirectDisplayID` returned by
//! `-[NSScreen CGDirectDisplayID]`. The installed `objc2-app-kit 0.3.2`
//! binding exposes it directly and returns `CGDirectDisplayID` (`u32`), with
//! `kCGNullDirectDisplay` (`0`) meaning "no display". It was chosen over the
//! `deviceDescription` `NSScreenNumber` because it is the public
//! CoreGraphics display identity itself, needs no `NSGraphics` feature, and is
//! available on every supported macOS release. This layer reports the raw
//! value; validation belongs to the caller.
//!
//! A snapshot is ordered exactly like `NSScreen.screens`: index 0 is
//! `screens[0]`, never the focus-dependent `mainScreen`. Frames are reported in
//! points and the scale is the raw `backingScaleFactor`; odd or non-finite
//! values are not dropped or sanitized here.
//!
//! # Threading
//!
//! [`NativeDesktop`] dispatches through an injected
//! [`MainThreadExecutor`]:
//!
//! - Desktop host: a worker dispatches the job to the main thread and waits for
//!   its result with a bounded `mpsc::recv_timeout` of
//!   [`DESKTOP_ACKNOWLEDGEMENT_BUDGET`]. A job whose waiter has already given up
//!   observes the shared abandoned state and issues no native call. A job that
//!   had already started when the wait expired reports
//!   [`DesktopError::Unknown`]; a job that never started reports
//!   [`DesktopError::TimedOut`].
//! - Standalone CLI: the caller is already the process main thread, so the
//!   executor runs the job inline with no dispatch pump, no Tauri, no window,
//!   no tray and no `NSApplication`. It therefore never waits on itself.
//! - Guarded operations add a check before enqueue and a final check on the
//!   main thread immediately before the native call. Their acknowledgement
//!   wait wakes on cancellation or its hop deadline, and the main-thread job
//!   owns the caller's keepalive resource until the native call completes.
//! - A [`NativeOperationTracker`] optionally observes admitted guarded jobs. A
//!   host that serializes native application waits for `unresolved == 0` before
//!   it issues a successor; an abandoned queued job releases that wait while
//!   staying owned until its closure returns. This is completion observation,
//!   not a claim that the OS call can be cancelled atomically.
//!
//! The ownership rule is explicit: the main thread may join processing workers
//! because those workers never wait on the main thread. Main-thread code never
//! waits on a worker, and every off-main wait is finite.
//!
//! # Compile-proof evidence
//!
//! Compile-proven against the installed crates and selected features:
//!
//! - `objc2 0.6.5`; `objc2-app-kit 0.3.2` with `NSScreen`,
//!   `NSWorkspace`, `objc2-core-foundation` and `objc2-core-graphics` (plus the
//!   pre-existing `NSApplication`, `NSResponder`, `NSRunningApplication`,
//!   `block2` and `std`). `NSScreen` is declared `MainThreadOnly`; the binding
//!   signature is `pub fn screens(mtm: MainThreadMarker) -> Retained<NSArray<NSScreen>>`
//!   and `CGDirectDisplayID` requires the `objc2-core-graphics` feature.
//! - `objc2-foundation 0.3.2` with `NSDictionary`, `NSError`, `NSObject`,
//!   `NSString` and `NSURL`, available transitively through the `NSScreen`
//!   feature and used directly for the URL, typed options dictionary and error
//!   text.
//!
//! The generated `setDesktopImageURL_forScreen_options_error` is `unsafe`
//! because the caller must ensure the options dictionary's generic key type is
//! exactly `NSWorkspaceDesktopImageOptionKey`. This adapter builds a statically
//! typed, empty `NSDictionary<NSWorkspaceDesktopImageOptionKey, AnyObject>` and
//! passes it to one isolated function with the `SAFETY` proof. The legacy
//! implementation passed `[NSDictionary dictionary]` too, so no option is
//! dropped or invented.
//!
//! The `desktop_main_thread` harness-less test binary runs `main` on the process
//! main thread and calls the real snapshot there without creating
//! `NSApplication`, a window, a tray or a dispatch pump. It passes under both
//! `cargo test` and `cargo nextest`, which is the standalone threading proof.
//! This automated proof never calls the setter.
//!
//! Under the `testing` feature the same binary also runs the real
//! resolve-and-recheck half of `set_image` through
//! `NativeDesktop::check_target_for_test`, which shares the private
//! `resolve_target` with the setter and has no path to it. That covers an
//! absent display id and a simulated geometry/scale change against real
//! `NSScreen` data. A physically connected or disconnected display is not
//! exercised: a simulated mismatched `expected` entry stands in for hotplug.
//!
//! An explicitly approved manual probe on November 5, 2026 changed one display
//! to magenta, removed its disposable source image and restored the original
//! URL/options. The user confirmed each visible result. Immediate URL readbacks
//! lagged both setters. This did not exercise actual cache clear or prove
//! long-term persistence. No automated test calls the setter. macOS Spaces
//! are unsupported; tiling's app-level virtual workspaces are unrelated.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSScreen, NSWorkspace, NSWorkspaceDesktopImageOptionKey};
use objc2_foundation::{NSDictionary, NSError, NSString, NSURL};
use stache_sdk::{CancelListener, CancellationToken, MainThreadExecutor};

#[cfg(any(test, feature = "testing"))]
pub mod testing;

/// How long one dispatched desktop operation may wait for its result.
///
/// This is a per-hop bound, not a shutdown or processing deadline. A screen
/// metadata read acknowledges in microseconds; a setter call copies the image
/// and can take longer, so the bound is wider than the application adapter's
/// two-second hop and still finite. Expiry reports a typed error instead of
/// blocking forever.
pub const DESKTOP_ACKNOWLEDGEMENT_BUDGET: Duration = Duration::from_secs(5);

const JOB_PENDING: u8 = 0;
const JOB_RUNNING: u8 = 1;
const JOB_ABANDONED: u8 = 2;
const JOB_ISSUED: u8 = 3;
const JOB_DONE: u8 = 4;

/// A monotonic time source for guarded desktop operations.
///
/// Production uses [`SystemDesktopClock`]. Tests inject a manual clock so a
/// caller deadline can expire without a real wait.
pub trait DesktopClock: Send + Sync {
    /// The current monotonic instant.
    fn now(&self) -> Instant;
}

/// The production [`DesktopClock`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemDesktopClock;

impl DesktopClock for SystemDesktopClock {
    fn now(&self) -> Instant { Instant::now() }
}

/// An owned resource that must outlive one guarded native call.
///
/// A caller passes, for example, its cache lease so that returning
/// [`DesktopError::Unknown`] cannot release the resource while the OS call that
/// already started still reads the file. The guarded job keeps the value alive
/// until the native call and its acknowledgement finish.
pub trait DesktopKeepalive: Send + Sync + 'static {}

impl<T: Send + Sync + 'static> DesktopKeepalive for T {}

/// The caller-owned guard for one guarded desktop operation.
///
/// The value is cheap to clone and safe to move between threads: every field is
/// an owned handle, a plain value or an `Arc`, and no native pointer or
/// `unsafe impl Send`/`Sync` is involved.
///
/// # Semantics
///
/// A guarded operation is admitted only while all of these hold:
///
/// - the caller's [`CancellationToken`] is not cancelled;
/// - the optional monotonic deadline, measured on the operation's injected [`DesktopClock`],
///   has not passed;
/// - the pure current-generation/revision predicate returns `true`. The
///   predicate must not take a blocking lock or perform I/O.
///
/// The check runs before the job is enqueued and again on the main thread
/// immediately before the native call, after the display geometry recheck. The
/// last check is the linearization point: it succeeds and the native call is
/// issued, or the operation stays inert. That linearization is about this
/// process's admission decision, not an atomic physical cancellation of an OS
/// call that has already begun. Cancellation and deadline expiry wake an
/// acknowledgement wait through the token's listener and a condition variable,
/// never by polling.
///
/// Cancellation never interrupts a native call that already started. Such a
/// call may report [`DesktopError::Unknown`] to its caller and still finish in
/// the main-thread job, retaining its keepalive resource until then.
#[derive(Clone)]
pub struct DesktopOperation {
    cancel: CancellationToken,
    deadline: Option<Instant>,
    clock: Arc<dyn DesktopClock>,
    current: Arc<dyn Fn() -> bool + Send + Sync>,
    keepalive: Option<Arc<dyn DesktopKeepalive>>,
    tracker: Option<NativeOperationTracker>,
}

impl DesktopOperation {
    /// Creates a guard with no deadline and the system clock.
    #[must_use]
    pub fn new(cancel: CancellationToken, current: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        Self {
            cancel,
            deadline: None,
            clock: Arc::new(SystemDesktopClock),
            current,
            keepalive: None,
            tracker: None,
        }
    }

    /// Sets an absolute monotonic deadline on the guard's clock.
    #[must_use]
    pub const fn with_deadline(mut self, deadline: Instant) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Replaces the clock used to read the deadline.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn DesktopClock>) -> Self {
        self.clock = clock;
        self
    }

    /// Attaches a resource the native job retains until it finishes.
    #[must_use]
    pub fn with_keepalive(mut self, keepalive: Arc<dyn DesktopKeepalive>) -> Self {
        self.keepalive = Some(keepalive);
        self
    }

    /// Attaches a tracker that observes whether this call may still issue or
    /// has finished its native step.
    ///
    /// A host that serializes native application uses one tracker and waits
    /// for no unresolved predecessor before it issues the next call. A guard
    /// without a tracker behaves exactly as before.
    #[must_use]
    pub fn with_tracker(mut self, tracker: NativeOperationTracker) -> Self {
        self.tracker = Some(tracker);
        self
    }

    /// Starts tracking this operation, if a tracker is attached.
    ///
    /// Backends call this once when they admit a guarded job. Releasing the
    /// returned account before issuance lets a successor proceed; finishing it
    /// records that the native step returned.
    #[must_use]
    pub fn track(&self) -> Option<NativeOperationAccount> {
        self.tracker.as_ref().map(NativeOperationTracker::begin)
    }

    /// Rechecks cancellation, deadline and currency once, without dispatching.
    ///
    /// Backend doubles call this so a scripted guarded call performs no work
    /// when the guard already refuses it. This is a read-only admission check;
    /// the authoritative linearization point stays inside the dispatched job.
    ///
    /// # Errors
    /// Returns [`DesktopError::Cancelled`], [`DesktopError::Expired`] or
    /// [`DesktopError::Stale`] for the respective refusal.
    pub fn check(&self, operation: &'static str) -> Result<(), DesktopError> {
        if self.cancel.is_cancelled() {
            return Err(DesktopError::Cancelled { operation });
        }
        if let Some(deadline) = self.deadline
            && self.clock.now() >= deadline
        {
            return Err(DesktopError::Expired { operation });
        }
        if !(self.current)() {
            return Err(DesktopError::Stale { operation });
        }
        Ok(())
    }

    fn not_issued(&self, operation: &'static str, hop_expired: bool) -> DesktopError {
        if self.cancel.is_cancelled() {
            return DesktopError::Cancelled { operation };
        }
        if let Some(deadline) = self.deadline
            && self.clock.now() >= deadline
        {
            return DesktopError::Expired { operation };
        }
        if !(self.current)() {
            return DesktopError::Stale { operation };
        }
        if hop_expired {
            DesktopError::TimedOut { operation }
        } else {
            DesktopError::Cancelled { operation }
        }
    }
}

impl fmt::Debug for DesktopOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DesktopOperation")
            .field("cancelled", &self.cancel.is_cancelled())
            .field("deadline", &self.deadline)
            .field("has_keepalive", &self.keepalive.is_some())
            .field("tracked", &self.tracker.is_some())
            .finish_non_exhaustive()
    }
}

/// The guarded extension of [`DesktopBackend`].
///
/// Backends implement it in addition to the unguarded [`DesktopBackend`]
/// methods, which remain available for callers that do not need cancellation,
/// deadlines or a currency predicate.
pub trait GuardedDesktopBackend: DesktopBackend {
    /// Reads the ordered display list unless the guard refuses the operation.
    ///
    /// # Errors
    /// Returns a typed guard refusal before any native read, or the same
    /// errors as [`DesktopBackend::snapshot`].
    fn snapshot_guarded(
        &self,
        operation: &DesktopOperation,
    ) -> Result<DisplaySnapshot, DesktopError>;

    /// Sets one display's image unless the guard refuses the operation.
    ///
    /// # Errors
    /// Returns a typed guard refusal before any native call, or the same
    /// errors as [`DesktopBackend::set_image`].
    fn set_image_guarded(
        &self,
        operation: &DesktopOperation,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &Path,
    ) -> Result<(), DesktopError>;
}

/// The stable native identity of one display.
///
/// The value is a plain `CGDirectDisplayID`; it carries no native pointer and
/// is safe to move between threads. `0` is `kCGNullDirectDisplay`, which this
/// layer reports rather than silently replacing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DisplayId(u32);

impl DisplayId {
    /// Wraps a raw `CGDirectDisplayID`.
    #[must_use]
    pub const fn new(raw: u32) -> Self { Self(raw) }

    /// Returns the raw `CGDirectDisplayID`.
    #[must_use]
    pub const fn raw(self) -> u32 { self.0 }
}

/// A display frame in points.
///
/// The values are exactly what `NSScreen.frame` reports. They are not rounded,
/// validated or converted to pixels here.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayFrame {
    /// Frame width in points.
    pub width: f64,
    /// Frame height in points.
    pub height: f64,
}

/// One display's raw identity, geometry and backing scale.
///
/// A later slice validates these values and computes target pixel dimensions;
/// this layer keeps every raw value it read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayEntry {
    /// Stable native display identity.
    pub id: DisplayId,
    /// Frame in points.
    pub frame: DisplayFrame,
    /// `NSScreen.backingScaleFactor`.
    pub scale: f64,
}

/// An ordered snapshot of the current screens.
///
/// Entry 0 is `NSScreen.screens[0]`; the order is the native screen order, not
/// a focus-dependent `mainScreen` reordering.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DisplaySnapshot {
    entries: Vec<DisplayEntry>,
}

impl DisplaySnapshot {
    /// Builds a snapshot from entries in native screen order.
    #[must_use]
    pub const fn new(entries: Vec<DisplayEntry>) -> Self { Self { entries } }

    /// Returns the entries in native screen order.
    #[must_use]
    pub fn entries(&self) -> &[DisplayEntry] { &self.entries }

    /// Returns the entry at `index`.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&DisplayEntry> { self.entries.get(index) }

    /// Iterates over entries in native screen order.
    pub fn iter(&self) -> std::slice::Iter<'_, DisplayEntry> { self.entries.iter() }

    /// Whether the snapshot has no displays.
    #[must_use]
    pub const fn is_empty(&self) -> bool { self.entries.is_empty() }

    /// The number of displays in the snapshot.
    #[must_use]
    pub const fn len(&self) -> usize { self.entries.len() }
}

impl<'a> IntoIterator for &'a DisplaySnapshot {
    type IntoIter = std::slice::Iter<'a, DisplayEntry>;
    type Item = &'a DisplayEntry;

    fn into_iter(self) -> Self::IntoIter { self.iter() }
}

/// A failed desktop-image operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DesktopError {
    /// A dispatched job did not start before the acknowledgement budget expired.
    /// No native call was made.
    TimedOut {
        /// The operation that timed out.
        operation: &'static str,
    },
    /// Cancellation was observed before the native call was issued. No native
    /// call was made.
    Cancelled {
        /// The operation that was cancelled.
        operation: &'static str,
    },
    /// The caller's monotonic deadline passed before the native call was
    /// issued. No native call was made.
    Expired {
        /// The operation whose deadline passed.
        operation: &'static str,
    },
    /// The caller's current-generation/revision predicate rejected the
    /// operation before the native call was issued. No native call was made.
    Stale {
        /// The operation that was no longer current.
        operation: &'static str,
    },
    /// A dispatched job had already started its native call when the
    /// acknowledgement expired, so its outcome is not known to the caller.
    Unknown {
        /// The operation whose outcome is unknown.
        operation: &'static str,
    },
    /// The host dispatcher rejected the job.
    Dispatch {
        /// The operation that could not be dispatched.
        operation: &'static str,
        /// The executor's error text.
        message: String,
    },
    /// Native screen work was invoked off the main thread.
    NotMainThread,
    /// The display is not in the current `NSScreen.screens` list.
    DisplayMissing(DisplayId),
    /// The display's frame or scale differs from the snapshot entry.
    DisplayChanged(DisplayId),
    /// The image path is not absolute.
    RelativePath(PathBuf),
    /// The image path is not valid UTF-8.
    NonUtf8Path(PathBuf),
    /// The native setter failed and returned `NSError` text.
    Native {
        /// The display whose set call failed.
        display: DisplayId,
        /// The localized `NSError` description.
        message: String,
    },
}

impl fmt::Display for DesktopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimedOut { operation } => {
                write!(f, "timed out waiting for the main thread to run {operation}")
            }
            Self::Cancelled { operation } => {
                write!(f, "cancelled {operation} before it reached the desktop")
            }
            Self::Expired { operation } => {
                write!(
                    f,
                    "the deadline for {operation} passed before it reached the desktop"
                )
            }
            Self::Stale { operation } => {
                write!(f, "{operation} is no longer current and was not issued")
            }
            Self::Unknown { operation } => {
                write!(f, "the outcome of {operation} is unknown after a timeout")
            }
            Self::Dispatch { operation, message } => {
                write!(f, "cannot dispatch {operation} to the main thread: {message}")
            }
            Self::NotMainThread => f.write_str("desktop image work requires the main thread"),
            Self::DisplayMissing(id) => {
                write!(f, "display {} is not present", id.raw())
            }
            Self::DisplayChanged(id) => {
                write!(f, "display {} changed since the snapshot", id.raw())
            }
            Self::RelativePath(path) => {
                write!(f, "image path is not absolute: {}", path.display())
            }
            Self::NonUtf8Path(path) => {
                write!(f, "image path is not valid UTF-8: {}", path.display())
            }
            Self::Native { display, message } => {
                write!(f, "cannot set display {} image: {message}", display.raw())
            }
        }
    }
}

impl std::error::Error for DesktopError {}

/// The safe per-display desktop-image boundary.
///
/// Implementations are `Send + Sync`. They take and return owned plain data and
/// may never leak a native object to the caller.
pub trait DesktopBackend: Send + Sync {
    /// Reads the ordered current display list.
    ///
    /// # Errors
    /// Returns [`DesktopError`] when the main-thread hop times out, the
    /// dispatcher rejects the job, or native screen access fails.
    fn snapshot(&self) -> Result<DisplaySnapshot, DesktopError>;

    /// Sets one display's desktop image.
    ///
    /// `expected` is the snapshot entry this operation was prepared for.
    /// Implementations re-resolve the display by [`DisplayId`] in the current
    /// screen list and recheck the frame and scale against `expected` before
    /// any native side effect. Resolution is never by list index, so a hotplug
    /// cannot retarget an image to another screen.
    ///
    /// # Errors
    /// Returns [`DesktopError::DisplayMissing`] or
    /// [`DesktopError::DisplayChanged`] before touching the desktop, and
    /// [`DesktopError::RelativePath`], [`DesktopError::NonUtf8Path`],
    /// [`DesktopError::TimedOut`], [`DesktopError::Unknown`] or
    /// [`DesktopError::Native`] for their respective failures.
    fn set_image(
        &self,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &Path,
    ) -> Result<(), DesktopError>;
}

/// The real main-thread desktop-image adapter.
pub struct NativeDesktop {
    executor: Arc<dyn MainThreadExecutor>,
    ops: Arc<dyn NativeDisplayOps>,
    budget: Duration,
}

impl NativeDesktop {
    /// Creates an adapter over the host's dispatcher.
    #[must_use]
    pub fn new(executor: Arc<dyn MainThreadExecutor>) -> Self {
        Self {
            executor,
            ops: Arc::new(SystemDisplayOps),
            budget: DESKTOP_ACKNOWLEDGEMENT_BUDGET,
        }
    }

    fn dispatch<T, F>(&self, operation: &'static str, job: F) -> Result<T, DesktopError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, DesktopError> + Send + 'static, {
        if self.executor.is_main_thread() {
            return job();
        }

        let state = Arc::new(AtomicU8::new(JOB_PENDING));
        let job_state = Arc::clone(&state);
        let (sender, receiver) = mpsc::channel();
        let dispatched = Box::new(move || {
            if job_state
                .compare_exchange(JOB_PENDING, JOB_RUNNING, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return;
            }
            let outcome = job();
            let _ = sender.send(outcome);
        });
        self.executor.execute(dispatched).map_err(|error| DesktopError::Dispatch {
            operation,
            message: error.to_string(),
        })?;

        match receiver.recv_timeout(self.budget) {
            Ok(outcome) => outcome,
            Err(RecvTimeoutError::Timeout) => {
                if state
                    .compare_exchange(
                        JOB_PENDING,
                        JOB_ABANDONED,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    )
                    .is_ok()
                {
                    Err(DesktopError::TimedOut { operation })
                } else {
                    Err(DesktopError::Unknown { operation })
                }
            }
            Err(RecvTimeoutError::Disconnected) => Err(DesktopError::Unknown { operation }),
        }
    }

    /// Re-resolves a display and compares it with the prepared entry, without
    /// touching the desktop.
    ///
    /// This is the read-only half of [`DesktopBackend::set_image`], exposed to
    /// the `harness = false` standalone test so the real `AppKit` resolution and
    /// recheck can run on the process main thread. It dispatches exactly like
    /// `set_image` but has no path to the native setter. Compiled only for the
    /// `testing` feature.
    ///
    /// # Errors
    /// Returns [`DesktopError::DisplayMissing`] when the id is absent from the
    /// current screen list and [`DesktopError::DisplayChanged`] when the current
    /// frame or scale differs from `expected`.
    #[cfg(feature = "testing")]
    pub fn check_target_for_test(
        &self,
        display: DisplayId,
        expected: &DisplayEntry,
    ) -> Result<(), DesktopError> {
        let expected = *expected;
        let ops = Arc::clone(&self.ops);
        self.dispatch("desktop target check", move || {
            ops.check_target(display, &expected)
        })
    }
}

impl DesktopBackend for NativeDesktop {
    fn snapshot(&self) -> Result<DisplaySnapshot, DesktopError> {
        let ops = Arc::clone(&self.ops);
        self.dispatch("display snapshot", move || ops.snapshot())
    }

    fn set_image(
        &self,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &Path,
    ) -> Result<(), DesktopError> {
        let image = validated_image_path(image)?;
        let expected = *expected;
        let ops = Arc::clone(&self.ops);
        self.dispatch("desktop image set", move || {
            ops.set_image(display, &expected, &image)
        })
    }
}

/// One queued guarded operation's shared admission state.
///
/// The state transitions are the linearization of the guard:
///
/// ```text
/// PENDING --(job runs on main)--> RUNNING --(last check passes)--> ISSUED
///    |                              |                                |
///    +--(waiter gives up)--> ABANDONED <--(waiter gives up)----------+
///                                   |
///                                   +--(job finishes)--> DONE
/// ```
///
/// Only compare-and-swap on the shared atomic decides races; no lock is held
/// while the native call runs.
struct Issuance {
    operation: &'static str,
    cancel: CancellationToken,
    deadline: Option<Instant>,
    clock: Arc<dyn DesktopClock>,
    current: Arc<dyn Fn() -> bool + Send + Sync>,
    state: Arc<AtomicU8>,
    account: Option<NativeOperationAccount>,
}

impl Issuance {
    fn check(&self) -> Result<(), DesktopError> {
        if self.cancel.is_cancelled() {
            return Err(DesktopError::Cancelled { operation: self.operation });
        }
        if let Some(deadline) = self.deadline
            && self.clock.now() >= deadline
        {
            return Err(DesktopError::Expired { operation: self.operation });
        }
        if !(self.current)() {
            return Err(DesktopError::Stale { operation: self.operation });
        }
        Ok(())
    }

    /// The final admission check and linearization point.
    ///
    /// Rechecks the guard and moves `RUNNING` to `ISSUED` atomically. Once this
    /// returns `Ok`, the native call follows; once it returns `Err`, no native
    /// call can follow for this job.
    fn issue(&self) -> Result<(), DesktopError> {
        self.check()?;
        self.state
            .compare_exchange(JOB_RUNNING, JOB_ISSUED, Ordering::SeqCst, Ordering::SeqCst)
            .map(|_| ())
            .map_err(|_| DesktopError::Cancelled { operation: self.operation })
    }

    /// Records that this issuance can no longer reach the native step.
    fn resolve_admission(&self) {
        if let Some(account) = &self.account {
            account.resolve_admission();
        }
    }
}

/// Tracks guarded native calls that may still run after their caller stopped
/// waiting.
///
/// A guard with a tracker creates one [`NativeOperationAccount`] per admitted
/// job. The account counts as *unresolved* from admission until the job either
/// can no longer issue its native step or returns from that step. A host that
/// serializes native application waits for `unresolved == 0` before issuing a
/// successor. It stays *owned* until the job closure actually returns, even
/// when the job was abandoned before it ran, so teardown can report an
/// unacknowledged native call instead of claiming completion.
///
/// The tracker is deliberately small: one mutex, one condition variable and no
/// thread of its own. Waits are lossless because the completion, abandonment
/// and cancellation paths notify the same condition variable.
#[derive(Clone, Debug, Default)]
pub struct NativeOperationTracker {
    inner: Arc<TrackerInner>,
}

#[derive(Debug, Default)]
struct TrackerInner {
    state: Mutex<TrackerState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct TrackerState {
    unresolved: usize,
    owned: usize,
}

impl NativeOperationTracker {
    /// Creates an empty tracker.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Registers one admitted guarded job.
    #[must_use]
    pub fn begin(&self) -> NativeOperationAccount {
        {
            let mut state = lock(&self.inner.state);
            state.unresolved += 1;
            state.owned += 1;
        }
        NativeOperationAccount {
            inner: Arc::new(AccountInner {
                tracker: Arc::clone(&self.inner),
                unresolved: AtomicBool::new(true),
                finished: AtomicBool::new(false),
            }),
        }
    }

    /// How many admitted jobs may still issue or are still issuing.
    #[must_use]
    pub fn unresolved(&self) -> usize { lock(&self.inner.state).unresolved }

    /// How many admitted jobs have not returned from their closure yet.
    #[must_use]
    pub fn owned(&self) -> usize { lock(&self.inner.state).owned }

    /// Whether every admitted job closure has returned.
    #[must_use]
    pub fn is_drained(&self) -> bool { lock(&self.inner.state).owned == 0 }

    /// Waits until no job may issue a native call.
    ///
    /// Returns `false` when the supplied token was cancelled or the deadline
    /// passed first; the caller must then not issue its own native call. A
    /// job abandoned before issuance releases this wait immediately, while its
    /// closure stays owned until it returns.
    #[must_use]
    pub fn wait_unresolved(
        &self,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
        clock: &dyn DesktopClock,
    ) -> bool {
        if self.unresolved() == 0 {
            return true;
        }
        let inner = Arc::clone(&self.inner);
        let _listener = cancel.on_cancel(Arc::new(move || {
            let _state = lock(&inner.state);
            inner.changed.notify_all();
        }));
        let mut state = lock(&self.inner.state);
        loop {
            if state.unresolved == 0 {
                return true;
            }
            if cancel.is_cancelled() {
                return false;
            }
            if let Some(deadline) = deadline {
                let now = clock.now();
                if now >= deadline {
                    return false;
                }
                let remaining = deadline.saturating_duration_since(now);
                let (next, _) = self
                    .inner
                    .changed
                    .wait_timeout(state, remaining)
                    .unwrap_or_else(PoisonError::into_inner);
                state = next;
            } else {
                state = self.inner.changed.wait(state).unwrap_or_else(PoisonError::into_inner);
            }
        }
    }
}

impl TrackerInner {
    fn release_unresolved(&self) {
        {
            let mut state = lock(&self.state);
            state.unresolved = state.unresolved.saturating_sub(1);
        }
        self.changed.notify_all();
    }

    fn release_owned(&self) {
        {
            let mut state = lock(&self.state);
            state.owned = state.owned.saturating_sub(1);
        }
        self.changed.notify_all();
    }
}

/// One admitted guarded job's admission and completion account.
#[derive(Clone, Debug)]
pub struct NativeOperationAccount {
    inner: Arc<AccountInner>,
}

#[derive(Debug)]
struct AccountInner {
    tracker: Arc<TrackerInner>,
    unresolved: AtomicBool,
    finished: AtomicBool,
}

impl NativeOperationAccount {
    /// Records that this job can no longer issue its native step.
    ///
    /// Called when the waiter abandons a pending or running job. The closure
    /// stays owned until it returns.
    pub fn resolve_admission(&self) {
        if self.inner.unresolved.swap(false, Ordering::SeqCst) {
            self.inner.tracker.release_unresolved();
        }
    }

    /// Records that the job closure returned, releasing admission if needed.
    pub fn finish(&self) {
        if self.inner.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        if self.inner.unresolved.swap(false, Ordering::SeqCst) {
            self.inner.tracker.release_unresolved();
        }
        self.inner.tracker.release_owned();
    }
}

enum GuardAckState<T> {
    Pending,
    Ready(T),
    Taken,
}

/// Acknowledgement of one guarded main-thread job, woken by completion or
/// cancellation.
struct GuardAck<T> {
    state: Mutex<GuardAckState<T>>,
    signal: Condvar,
}

impl<T> GuardAck<T> {
    const fn new() -> Self {
        Self {
            state: Mutex::new(GuardAckState::Pending),
            signal: Condvar::new(),
        }
    }

    fn complete(&self, value: T) {
        let mut state = lock(&self.state);
        if matches!(&*state, GuardAckState::Pending) {
            *state = GuardAckState::Ready(value);
            drop(state);
            self.signal.notify_all();
        }
    }

    fn wake(&self) {
        // Serialize notification with the waiter's check-to-wait transition.
        // Otherwise cancellation can be lost just before wait_timeout sleeps.
        let _state = lock(&self.state);
        self.signal.notify_all();
    }
}

impl NativeDesktop {
    /// Dispatches one guarded job and waits for its acknowledgement.
    ///
    /// The waiter is off-main only: an inline main-thread executor runs the job
    /// immediately and reads its ready acknowledgement without waiting. On the
    /// main thread the job rechecks the guard through `admit` and issues the
    /// native call only at that point. The keepalive resource is owned by the
    /// job closure, so an [`DesktopError::Unknown`] return cannot release it
    /// while the native call still runs.
    fn dispatch_guarded<T, F>(
        &self,
        operation: &'static str,
        guard: &DesktopOperation,
        job: F,
    ) -> Result<T, DesktopError>
    where
        T: Send + 'static,
        F: FnOnce(&Issuance) -> Result<T, DesktopError> + Send + 'static,
    {
        // Pre-enqueue admission: a refused operation never reaches the queue.
        guard.check(operation)?;
        let budget = guard.deadline.map_or(self.budget, |deadline| {
            deadline.saturating_duration_since(guard.clock.now()).min(self.budget)
        });
        if budget.is_zero() {
            return Err(DesktopError::Expired { operation });
        }

        let state = Arc::new(AtomicU8::new(JOB_PENDING));
        let account = guard.track();
        let issuance = Arc::new(Issuance {
            operation,
            cancel: guard.cancel.clone(),
            deadline: guard.deadline,
            clock: Arc::clone(&guard.clock),
            current: Arc::clone(&guard.current),
            state: Arc::clone(&state),
            account: account.clone(),
        });
        let ack = Arc::new(GuardAck::new());
        let keepalive = guard.keepalive.clone();
        let job_issuance = Arc::clone(&issuance);
        let job_ack = Arc::clone(&ack);
        let job_account = account.clone();
        let dispatched: Box<dyn FnOnce() + Send> = Box::new(move || {
            // Retained until the native call and the acknowledgement finish.
            let _keepalive = keepalive;
            if job_issuance
                .state
                .compare_exchange(JOB_PENDING, JOB_RUNNING, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                // The waiter gave up while this job was queued: stay inert.
                if let Some(account) = &job_account {
                    account.finish();
                }
                return;
            }
            let outcome = job(&job_issuance);
            job_ack.complete(outcome);
            // Publishing DONE after the result means a waiter that observes
            // DONE always finds the acknowledgement ready.
            job_issuance.state.store(JOB_DONE, Ordering::SeqCst);
            if let Some(account) = &job_account {
                account.finish();
            }
        });

        if self.executor.is_main_thread() {
            dispatched();
            return ack_take(&ack).unwrap_or_else(|| Err(DesktopError::Unknown { operation }));
        }

        if let Err(error) = self.executor.execute(dispatched) {
            if let Some(account) = &account {
                account.finish();
            }
            return Err(DesktopError::Dispatch {
                operation,
                message: error.to_string(),
            });
        }

        // Cancellation notifies this condition variable directly; the wait is
        // never a poll loop. The hop deadline uses the fixed per-hop budget
        // capped by the caller's remaining time.
        let _listener: CancelListener = guard.cancel.on_cancel(Arc::new({
            let ack = Arc::clone(&ack);
            move || ack.wake()
        }));
        let deadline = Instant::now().checked_add(budget);
        let mut state = lock(&ack.state);
        loop {
            match &*state {
                GuardAckState::Ready(_) => {
                    let GuardAckState::Ready(value) =
                        std::mem::replace(&mut *state, GuardAckState::Taken)
                    else {
                        unreachable!("matched Ready");
                    };
                    return value;
                }
                GuardAckState::Taken => {
                    drop(state);
                    return Err(DesktopError::Unknown { operation });
                }
                GuardAckState::Pending => {}
            }
            if guard.cancel.is_cancelled() {
                break;
            }
            let Some(remaining) =
                deadline.and_then(|deadline| deadline.checked_duration_since(Instant::now()))
            else {
                break;
            };
            if remaining.is_zero() {
                break;
            }
            let (next, _) = ack
                .signal
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            state = next;
        }
        drop(state);
        Err(abandon(&issuance, &ack, guard))
    }
}

impl GuardedDesktopBackend for NativeDesktop {
    fn snapshot_guarded(
        &self,
        operation: &DesktopOperation,
    ) -> Result<DisplaySnapshot, DesktopError> {
        let ops = Arc::clone(&self.ops);
        self.dispatch_guarded("display snapshot", operation, move |issuance| {
            ops.issue_snapshot(&|| issuance.issue())
        })
    }

    fn set_image_guarded(
        &self,
        operation: &DesktopOperation,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &Path,
    ) -> Result<(), DesktopError> {
        let image = validated_image_path(image)?;
        let expected = *expected;
        let ops = Arc::clone(&self.ops);
        self.dispatch_guarded("desktop image set", operation, move |issuance| {
            ops.issue_set(display, &expected, &image, &|| issuance.issue())
        })
    }
}

/// Reads a ready acknowledgement without waiting.
fn ack_take<T>(ack: &GuardAck<T>) -> Option<T> {
    let mut state = lock(&ack.state);
    match &*state {
        GuardAckState::Ready(_) => {
            let GuardAckState::Ready(value) = std::mem::replace(&mut *state, GuardAckState::Taken)
            else {
                unreachable!("matched Ready");
            };
            drop(state);
            Some(value)
        }
        GuardAckState::Taken | GuardAckState::Pending => None,
    }
}

/// Abandons a guarded job the waiter stopped waiting on.
///
/// Returns the not-issued reason when the job never reached its final check,
/// and [`DesktopError::Unknown`] when the native call was already issued (or may
/// have been) and will finish on the main thread.
fn abandon<T>(
    issuance: &Issuance,
    ack: &GuardAck<Result<T, DesktopError>>,
    guard: &DesktopOperation,
) -> DesktopError {
    let operation = issuance.operation;
    match issuance.state.compare_exchange(
        JOB_PENDING,
        JOB_ABANDONED,
        Ordering::SeqCst,
        Ordering::SeqCst,
    ) {
        Ok(_) => {
            issuance.resolve_admission();
            guard.not_issued(operation, true)
        }
        Err(observed) => match observed {
            JOB_RUNNING => {
                if issuance
                    .state
                    .compare_exchange(
                        JOB_RUNNING,
                        JOB_ABANDONED,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    )
                    .is_ok()
                {
                    // The job's final check can no longer pass.
                    issuance.resolve_admission();
                    guard.not_issued(operation, true)
                } else {
                    completed(ack, operation)
                }
            }
            JOB_ISSUED | JOB_DONE => completed(ack, operation),
            JOB_ABANDONED => {
                issuance.resolve_admission();
                guard.not_issued(operation, true)
            }
            _ => DesktopError::Unknown { operation },
        },
    }
}

fn completed<T>(ack: &GuardAck<Result<T, DesktopError>>, operation: &'static str) -> DesktopError {
    match ack_take(ack) {
        Some(Err(error)) => error,
        Some(Ok(_)) | None => DesktopError::Unknown { operation },
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The native main-thread step, injectable for tests without `AppKit`.
trait NativeDisplayOps: Send + Sync {
    fn snapshot(&self) -> Result<DisplaySnapshot, DesktopError>;

    fn set_image(
        &self,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &Path,
    ) -> Result<(), DesktopError>;

    /// Reads the screens after the guard's final check.
    fn issue_snapshot(
        &self,
        admit: &(dyn Fn() -> Result<(), DesktopError> + Send + Sync),
    ) -> Result<DisplaySnapshot, DesktopError> {
        admit()?;
        self.snapshot()
    }

    /// Rechecks the target, runs the guard's final check, then sets the image.
    ///
    /// The default keeps the guard before the whole step. [`SystemDisplayOps`]
    /// overrides it so the real geometry recheck happens first and the guard is
    /// checked immediately before the native setter.
    fn issue_set(
        &self,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &Path,
        admit: &(dyn Fn() -> Result<(), DesktopError> + Send + Sync),
    ) -> Result<(), DesktopError> {
        admit()?;
        self.set_image(display, expected, image)
    }

    #[cfg(feature = "testing")]
    fn check_target(&self, display: DisplayId, expected: &DisplayEntry)
    -> Result<(), DesktopError>;
}

struct SystemDisplayOps;

impl NativeDisplayOps for SystemDisplayOps {
    fn snapshot(&self) -> Result<DisplaySnapshot, DesktopError> {
        let main = require_main()?;
        Ok(snapshot_screens(main))
    }

    fn set_image(
        &self,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &Path,
    ) -> Result<(), DesktopError> {
        let main = require_main()?;
        let screen = resolve_target(main, display, expected)?;
        set_desktop_image(&screen, image)
            .map_err(|message| DesktopError::Native { display, message })
    }

    fn issue_set(
        &self,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &Path,
        admit: &(dyn Fn() -> Result<(), DesktopError> + Send + Sync),
    ) -> Result<(), DesktopError> {
        let main = require_main()?;
        let screen = resolve_target(main, display, expected)?;
        // The guard's linearization point is immediately before the native
        // call, after the geometry recheck above.
        admit()?;
        set_desktop_image(&screen, image)
            .map_err(|message| DesktopError::Native { display, message })
    }

    #[cfg(feature = "testing")]
    fn check_target(
        &self,
        display: DisplayId,
        expected: &DisplayEntry,
    ) -> Result<(), DesktopError> {
        let main = require_main()?;
        resolve_target(main, display, expected).map(|_| ())
    }
}

/// Finds a display by identity and rechecks its frame and scale.
///
/// Resolution is by [`DisplayId`], never by list index. This is shared by the
/// setter and the read-only test entry point so both run the same recheck.
fn resolve_target(
    main: MainThreadMarker,
    display: DisplayId,
    expected: &DisplayEntry,
) -> Result<Retained<NSScreen>, DesktopError> {
    let screens = NSScreen::screens(main);
    let target = screens
        .to_vec()
        .into_iter()
        .find(|screen| DisplayId(screen.CGDirectDisplayID()) == display);
    let Some(screen) = target else {
        return Err(DesktopError::DisplayMissing(display));
    };
    if entry_for(&screen) != *expected {
        return Err(DesktopError::DisplayChanged(display));
    }
    Ok(screen)
}

fn require_main() -> Result<MainThreadMarker, DesktopError> {
    MainThreadMarker::new().ok_or(DesktopError::NotMainThread)
}

fn snapshot_screens(main: MainThreadMarker) -> DisplaySnapshot {
    let screens = NSScreen::screens(main);
    DisplaySnapshot::new(screens.to_vec().iter().map(|screen| entry_for(screen)).collect())
}

fn entry_for(screen: &NSScreen) -> DisplayEntry {
    let frame = screen.frame();
    DisplayEntry {
        id: DisplayId(screen.CGDirectDisplayID()),
        frame: DisplayFrame {
            width: frame.size.width,
            height: frame.size.height,
        },
        scale: screen.backingScaleFactor(),
    }
}

fn validated_image_path(image: &Path) -> Result<PathBuf, DesktopError> {
    if !image.is_absolute() {
        return Err(DesktopError::RelativePath(image.to_path_buf()));
    }
    if image.to_str().is_none() {
        return Err(DesktopError::NonUtf8Path(image.to_path_buf()));
    }
    Ok(image.to_path_buf())
}

/// The one isolated `unsafe` call into the public per-display setter.
fn set_desktop_image(screen: &NSScreen, image: &Path) -> Result<(), String> {
    let Some(path) = image.to_str() else {
        return Err("image path is not valid UTF-8".to_string());
    };
    let url = NSURL::fileURLWithPath(&NSString::from_str(path));
    let options = NSDictionary::<NSWorkspaceDesktopImageOptionKey, AnyObject>::new();
    let workspace = NSWorkspace::sharedWorkspace();
    // SAFETY: the binding's only requirement is that the options dictionary's
    // generic key type is the typed `NSWorkspaceDesktopImageOptionKey`. The
    // dictionary above is statically typed as
    // `NSDictionary<NSWorkspaceDesktopImageOptionKey, AnyObject>` and is empty,
    // which satisfies that requirement vacuously and matches the legacy
    // empty-dictionary call. No other option value can reach the method.
    unsafe { workspace.setDesktopImageURL_forScreen_options_error(&url, screen, &options) }
        .map_err(|error| native_error_text(&error))
}

fn native_error_text(error: &NSError) -> String { error.localizedDescription().to_string() }

#[cfg(test)]
mod tests;
