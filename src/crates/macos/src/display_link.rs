//! Per-display frame clocks for window animation pacing.
//!
//! A [`FrameClock`] reports the display's refresh ticks to a coordinator
//! without queueing them: [`FrameClock::take_tick`] returns the **latest** tick
//! since the last take, so a late coordinator never bursts to catch up.
//!
//! # System implementation
//!
//! [`SystemFrameClockFactory`] drives a clock with `CVDisplayLink`
//! (`CVDisplayLinkCreateWithCGDisplay`, hand-declared here; no crate). The API
//! is deprecated in recent SDKs; because it is declared by hand there is no
//! compiler deprecation warning to silence, and its availability and callback
//! cadence on macOS 26 are a **native-gate item** that no default test checks.
//! If creation fails the coordinator paces that display with its deadline
//! timer and reports the reason.
//!
//! # Callback rules
//!
//! The callback runs on `CoreVideo`'s thread. It only stores a timestamp into a
//! lock-free latest-wins slot and calls the `wake` closure: no AX, no
//! allocation, no lock held while calling out. The timestamp is `Instant::now()`
//! at callback entry, an approximation of the display's target timestamp
//! (mapping the `CoreVideo` host time to an `Instant` is not guaranteed to share
//! an epoch).
//!
//! # Stop safety
//!
//! `CoreVideo` never holds a pointer: the callback context is an integer key
//! into a process-wide registry of live clock cores. The callback looks its
//! core up, counts itself in flight and clones the `Arc` under the registry's
//! short lock (no call-out under it), then runs on its own reference. A key
//! that is gone finds nothing, so a late callback can never touch freed memory.
//! [`FrameClock::stop`] sets `stopped`, calls `CVDisplayLinkStop`, removes the
//! key, waits (bounded by the budget) until no callback is in flight, and only
//! then releases the link. A clock that cannot stop in time is returned as a
//! [`StuckClock`], which owns the link: it is never released while a callback
//! might run and never detached; the owner retries.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::DisplayId;

/// A request to be woken when a tick arrives. It runs on the clock's thread and
/// must neither block nor take a lock for long.
pub type WakeFn = Arc<dyn Fn() + Send + Sync>;

/// Why a frame clock could not be created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameClockError {
    /// The reason, for status diagnostics.
    pub reason: String,
}

impl fmt::Display for FrameClockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.reason) }
}

impl std::error::Error for FrameClockError {}

/// A clock that did not stop within its budget. It owns the native link and its
/// context: dropping it never frees them while a callback may run. Retry with
/// [`StuckClock::retry`].
pub struct StuckClock {
    clock: Box<dyn FrameClock>,
}

impl StuckClock {
    /// Wraps a clock that could not stop.
    #[must_use]
    pub fn new(clock: Box<dyn FrameClock>) -> Self { Self { clock } }

    /// Tries to stop the clock again within `budget`.
    ///
    /// # Errors
    ///
    /// The clock, still stuck.
    pub fn retry(self, budget: Duration) -> Result<(), Self> { self.clock.stop(budget) }
}

impl fmt::Debug for StuckClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("StuckClock") }
}

/// One display's frame clock.
pub trait FrameClock: Send {
    /// The latest tick since the last take, if any. Latest wins; nothing queues.
    fn take_tick(&self) -> Option<Instant>;

    /// Stops the clock. When this returns `Ok`, no callback runs afterwards and
    /// the native resources are freed.
    ///
    /// # Errors
    ///
    /// A callback was still running after `budget`; the clock is returned
    /// whole, owning its resources, to be retried.
    fn stop(self: Box<Self>, budget: Duration) -> Result<(), StuckClock>;
}

/// Creates frame clocks.
pub trait FrameClockFactory: Send + Sync {
    /// A clock for `display` that calls `wake` after each tick.
    ///
    /// # Errors
    ///
    /// The clock could not be created or started.
    fn create(
        &self,
        display: DisplayId,
        wake: WakeFn,
    ) -> Result<Box<dyn FrameClock>, FrameClockError>;
}

// --------------------------------------------------------------------- core

/// The state shared between a clock and its callback: the latest-wins slot, the
/// stop flag and the in-flight counter.
pub struct ClockCore {
    origin: Instant,
    /// Nanoseconds since `origin` plus one; zero is empty.
    slot: AtomicU64,
    stopped: AtomicBool,
    in_flight: AtomicUsize,
    wake: WakeFn,
}

impl ClockCore {
    /// A core that calls `wake` after each stored tick.
    #[must_use]
    pub fn new(wake: WakeFn) -> Self {
        Self {
            origin: Instant::now(),
            slot: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
            in_flight: AtomicUsize::new(0),
            wake,
        }
    }

    /// The callback body: counts itself in flight, and unless the clock is
    /// stopped stores `at` (latest wins) and wakes. `mid` runs between entering
    /// and the stop check, so a test can hold a callback in flight.
    pub fn callback(&self, at: Instant, mid: impl FnOnce()) {
        self.enter();
        self.run_entered(at, mid);
    }

    /// Counts a callback in flight; [`Self::run_entered`] counts it out.
    fn enter(&self) { self.in_flight.fetch_add(1, Ordering::SeqCst); }

    /// The callback body after [`Self::enter`].
    fn run_entered(&self, at: Instant, mid: impl FnOnce()) {
        mid();
        if !self.stopped.load(Ordering::SeqCst) {
            let nanos = at.saturating_duration_since(self.origin).as_nanos();
            let nanos = u64::try_from(nanos).unwrap_or(u64::MAX - 1);
            self.slot.store(nanos.saturating_add(1), Ordering::SeqCst);
            // No lock is held here.
            (self.wake)();
        }
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }

    /// The latest tick since the last take.
    #[must_use]
    pub fn take(&self) -> Option<Instant> {
        match self.slot.swap(0, Ordering::SeqCst) {
            0 => None,
            stored => Some(self.origin + Duration::from_nanos(stored - 1)),
        }
    }

    /// Marks the core stopped: no callback stores or wakes afterwards.
    pub fn mark_stopped(&self) { self.stopped.store(true, Ordering::SeqCst); }

    /// Waits until no callback is in flight, at most `budget`.
    #[must_use]
    pub fn wait_idle(&self, budget: Duration) -> bool {
        let deadline = Instant::now().checked_add(budget);
        loop {
            if self.in_flight.load(Ordering::SeqCst) == 0 {
                return true;
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Callbacks currently running.
    #[must_use]
    pub fn in_flight(&self) -> usize { self.in_flight.load(Ordering::SeqCst) }
}

// ------------------------------------------------------------------- system

/// Hand-declared `CoreVideo` display link API.
mod ffi {
    use std::ffi::c_void;

    /// `CVReturn`; zero is success.
    pub type CvReturn = i32;
    /// The output callback.
    pub type OutputCallback = extern "C" fn(
        link: *mut c_void,
        now: *const c_void,
        output: *const c_void,
        flags_in: u64,
        flags_out: *mut u64,
        context: *mut c_void,
    ) -> CvReturn;

    #[link(name = "CoreVideo", kind = "framework")]
    unsafe extern "C" {
        pub fn CVDisplayLinkCreateWithCGDisplay(display: u32, out: *mut *mut c_void) -> CvReturn;
        pub fn CVDisplayLinkSetOutputCallback(
            link: *mut c_void,
            callback: OutputCallback,
            context: *mut c_void,
        ) -> CvReturn;
        pub fn CVDisplayLinkStart(link: *mut c_void) -> CvReturn;
        pub fn CVDisplayLinkStop(link: *mut c_void) -> CvReturn;
        pub fn CVDisplayLinkRelease(link: *mut c_void);
    }
}

/// The live system clock cores, by the key `CoreVideo` passes as context.
fn registry() -> &'static Mutex<HashMap<usize, Arc<ClockCore>>> {
    static CORES: OnceLock<Mutex<HashMap<usize, Arc<ClockCore>>>> = OnceLock::new();
    CORES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_key() -> usize {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The `CoreVideo` callback. It runs on `CoreVideo`'s thread.
extern "C" fn output_callback(
    _link: *mut std::ffi::c_void,
    _now: *const std::ffi::c_void,
    _output: *const std::ffi::c_void,
    _flags_in: u64,
    _flags_out: *mut u64,
    context: *mut std::ffi::c_void,
) -> ffi::CvReturn {
    // The context is a registry key, never dereferenced.
    let key = context.addr();
    // Counted in flight under the lock: a stop that removed the key and then
    // saw no callback in flight can never be followed by this one.
    let found = registry().lock().unwrap_or_else(PoisonError::into_inner).get(&key).map(|core| {
        core.enter();
        Arc::clone(core)
    });
    let Some(core) = found else { return 0 };
    core.run_entered(Instant::now(), || {});
    0
}

/// A `CVDisplayLink` clock.
struct SystemClock {
    link: *mut std::ffi::c_void,
    /// The registry key `CoreVideo` passes as context.
    key: usize,
    core: Arc<ClockCore>,
    released: bool,
}

// SAFETY: the link is a `CoreVideo` object that may be stopped and released from
// any thread; everything else here is `Send`. Nothing is tied to the creating
// thread.
unsafe impl Send for SystemClock {}

impl SystemClock {
    /// Stops the link and frees the native resources once no callback runs.
    fn shutdown(&mut self, budget: Duration) -> bool {
        if self.released {
            return true;
        }
        self.core.mark_stopped();
        // SAFETY: `link` is a live link created by this value and not yet
        // released (`released` is false); `CVDisplayLinkStop` is idempotent.
        unsafe { ffi::CVDisplayLinkStop(self.link) };
        registry().lock().unwrap_or_else(PoisonError::into_inner).remove(&self.key);
        if !self.core.wait_idle(budget) {
            return false;
        }
        // SAFETY: the link is live, no callback is in flight and none can find
        // the core again (its key is gone); it is released exactly once.
        unsafe { ffi::CVDisplayLinkRelease(self.link) };
        self.released = true;
        true
    }
}

impl FrameClock for SystemClock {
    fn take_tick(&self) -> Option<Instant> { self.core.take() }

    fn stop(mut self: Box<Self>, budget: Duration) -> Result<(), StuckClock> {
        if self.shutdown(budget) {
            Ok(())
        } else {
            Err(StuckClock::new(self))
        }
    }
}

impl Drop for SystemClock {
    fn drop(&mut self) {
        // A clock dropped without `stop` still stops. One that cannot is leaked
        // on purpose: its resources are never freed under a running callback.
        let _ = self.shutdown(Duration::from_millis(200));
    }
}

/// Creates real `CVDisplayLink` clocks.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemFrameClockFactory;

impl FrameClockFactory for SystemFrameClockFactory {
    fn create(
        &self,
        display: DisplayId,
        wake: WakeFn,
    ) -> Result<Box<dyn FrameClock>, FrameClockError> {
        let fail = |what: &str, code: i32| FrameClockError {
            reason: format!("{what} failed with CVReturn {code}"),
        };
        let core = Arc::new(ClockCore::new(wake));
        let mut link: *mut std::ffi::c_void = std::ptr::null_mut();
        // SAFETY: `link` is a valid out pointer for the created link.
        let created =
            unsafe { ffi::CVDisplayLinkCreateWithCGDisplay(display.raw(), &raw mut link) };
        if created != 0 || link.is_null() {
            return Err(fail("CVDisplayLinkCreateWithCGDisplay", created));
        }
        let key = next_key();
        registry()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, Arc::clone(&core));
        // SAFETY: `link` is live and the callback is a valid `extern "C"`
        // function; the context is an integer key, never dereferenced.
        let attached = unsafe {
            ffi::CVDisplayLinkSetOutputCallback(
                link,
                output_callback,
                std::ptr::without_provenance_mut(key),
            )
        };
        let mut clock = SystemClock {
            link,
            key,
            core,
            released: false,
        };
        if attached != 0 {
            let _ = clock.shutdown(Duration::from_millis(200));
            return Err(fail("CVDisplayLinkSetOutputCallback", attached));
        }
        // SAFETY: `link` is live with its callback set.
        let started = unsafe { ffi::CVDisplayLinkStart(link) };
        if started != 0 {
            let _ = clock.shutdown(Duration::from_millis(200));
            return Err(fail("CVDisplayLinkStart", started));
        }
        Ok(Box::new(clock))
    }
}

// ------------------------------------------------------------------ testing

/// A manual fake of the frame clock, for default tests (never a real link).
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use super::{ClockCore, FrameClock, FrameClockError, FrameClockFactory, StuckClock, WakeFn};
    use crate::DisplayId;

    #[derive(Default)]
    struct State {
        fail: BTreeMap<DisplayId, String>,
        fail_all: Option<String>,
        stuck: BTreeSet<DisplayId>,
        cores: BTreeMap<DisplayId, Vec<Arc<ClockCore>>>,
        created: Vec<DisplayId>,
        attempts: Vec<DisplayId>,
        stopped: Vec<DisplayId>,
        stop_attempts: usize,
        live: usize,
    }

    /// A fake factory: manual ticks, creation failures, held callbacks and stop
    /// that can be stuck, with call logs. Clone it to keep a handle.
    #[derive(Clone, Default)]
    pub struct FakeFrameClockFactory {
        state: Arc<Mutex<State>>,
    }

    /// A callback held inside the clock (in flight) until released.
    pub struct HeldCallback {
        gate: Arc<(Mutex<(bool, bool)>, Condvar)>,
        handle: Option<JoinHandle<()>>,
    }

    impl HeldCallback {
        /// Waits until the callback is inside the clock.
        pub fn wait_entered(&self) {
            let (lock, cv) = &*self.gate;
            let state = lock.lock().unwrap_or_else(PoisonError::into_inner);
            drop(cv.wait_while(state, |state| !state.0).unwrap_or_else(PoisonError::into_inner));
        }

        /// Lets the callback finish and joins it.
        pub fn release(mut self) {
            {
                let (lock, cv) = &*self.gate;
                lock.lock().unwrap_or_else(PoisonError::into_inner).1 = true;
                cv.notify_all();
            }
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    impl FakeFrameClockFactory {
        /// A factory with no failures.
        #[must_use]
        pub fn new() -> Self { Self::default() }

        fn state(&self) -> MutexGuard<'_, State> {
            self.state.lock().unwrap_or_else(PoisonError::into_inner)
        }

        /// A factory whose every creation fails with `reason`.
        #[must_use]
        pub fn failing(reason: &str) -> Self {
            let factory = Self::default();
            factory.state().fail_all = Some(reason.to_owned());
            factory
        }

        /// Lets every creation succeed again (undoes [`Self::failing`]).
        pub fn allow_all(&self) { self.state().fail_all = None; }

        /// Makes creation for `display` fail with `reason`.
        pub fn fail_creation(&self, display: DisplayId, reason: &str) {
            self.state().fail.insert(display, reason.to_owned());
        }

        /// Lets creation for `display` succeed again.
        pub fn allow_creation(&self, display: DisplayId) { self.state().fail.remove(&display); }

        /// While set, stopping a clock of `display` reports it stuck.
        pub fn set_stuck(&self, display: DisplayId, stuck: bool) {
            let mut state = self.state();
            if stuck {
                state.stuck.insert(display);
            } else {
                state.stuck.remove(&display);
            }
        }

        /// A tick on every live clock of `display` at `at`. `false` when none.
        #[allow(clippy::must_use_candidate)] // a test may fire without checking a clock existed
        pub fn fire(&self, display: DisplayId, at: Instant) -> bool {
            let cores = self.state().cores.get(&display).cloned().unwrap_or_default();
            for core in &cores {
                core.callback(at, || {});
            }
            !cores.is_empty()
        }

        /// A tick on the newest clock of `display` that stays in flight inside
        /// the clock until the returned handle is released.
        #[must_use]
        pub fn fire_held(&self, display: DisplayId, at: Instant) -> Option<HeldCallback> {
            let core = self.state().cores.get(&display)?.last().cloned()?;
            let gate = Arc::new((Mutex::new((false, false)), Condvar::new()));
            let inside = Arc::clone(&gate);
            let handle = std::thread::spawn(move || {
                core.callback(at, || {
                    let (lock, cv) = &*inside;
                    {
                        lock.lock().unwrap_or_else(PoisonError::into_inner).0 = true;
                        cv.notify_all();
                    }
                    let state = lock.lock().unwrap_or_else(PoisonError::into_inner);
                    drop(
                        cv.wait_while(state, |state| !state.1)
                            .unwrap_or_else(PoisonError::into_inner),
                    );
                });
            });
            Some(HeldCallback { gate, handle: Some(handle) })
        }

        /// Every creation attempt, in order (including failures).
        #[must_use]
        pub fn attempts(&self) -> Vec<DisplayId> { self.state().attempts.clone() }

        /// Every successful creation, in order.
        #[must_use]
        pub fn created(&self) -> Vec<DisplayId> { self.state().created.clone() }

        /// Every clock that stopped cleanly, in order.
        #[must_use]
        pub fn stopped(&self) -> Vec<DisplayId> { self.state().stopped.clone() }

        /// How many stop calls ran (including stuck ones).
        #[must_use]
        pub fn stop_attempts(&self) -> usize { self.state().stop_attempts }

        /// Clocks created and not yet stopped (stuck ones still count).
        #[must_use]
        pub fn live(&self) -> usize { self.state().live }

        /// Whether a clock for `display` is live.
        #[must_use]
        pub fn is_live(&self, display: DisplayId) -> bool {
            self.state().cores.get(&display).is_some_and(|cores| !cores.is_empty())
        }
    }

    struct FakeClock {
        display: DisplayId,
        core: Arc<ClockCore>,
        state: Arc<Mutex<State>>,
    }

    impl FrameClock for FakeClock {
        fn take_tick(&self) -> Option<Instant> { self.core.take() }

        fn stop(self: Box<Self>, budget: Duration) -> Result<(), StuckClock> {
            let stuck = {
                let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                state.stop_attempts += 1;
                state.stuck.contains(&self.display)
            };
            self.core.mark_stopped();
            if stuck || !self.core.wait_idle(budget) {
                return Err(StuckClock::new(self));
            }
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(cores) = state.cores.get_mut(&self.display) {
                cores.retain(|core| !Arc::ptr_eq(core, &self.core));
            }
            state.stopped.push(self.display);
            state.live -= 1;
            drop(state);
            Ok(())
        }
    }

    impl FrameClockFactory for FakeFrameClockFactory {
        fn create(
            &self,
            display: DisplayId,
            wake: WakeFn,
        ) -> Result<Box<dyn FrameClock>, FrameClockError> {
            let mut state = self.state();
            state.attempts.push(display);
            if let Some(reason) = state.fail.get(&display).or(state.fail_all.as_ref()) {
                return Err(FrameClockError { reason: reason.clone() });
            }
            let core = Arc::new(ClockCore::new(wake));
            state.cores.entry(display).or_default().push(Arc::clone(&core));
            state.created.push(display);
            state.live += 1;
            drop(state);
            Ok(Box::new(FakeClock {
                display,
                core,
                state: Arc::clone(&self.state),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::testing::FakeFrameClockFactory;
    use super::*;

    fn wake() -> (WakeFn, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&count);
        (
            Arc::new(move || {
                seen.fetch_add(1, Ordering::SeqCst);
            }),
            count,
        )
    }

    #[test]
    fn ticks_are_latest_wins() {
        let factory = FakeFrameClockFactory::new();
        let (wake, count) = wake();
        let display = DisplayId::new(1);
        let clock = factory.create(display, wake).unwrap();
        let base = Instant::now();
        factory.fire(display, base + Duration::from_millis(5));
        factory.fire(display, base + Duration::from_millis(9));
        assert_eq!(count.load(Ordering::SeqCst), 2);
        let tick = clock.take_tick().unwrap();
        assert!(tick >= base + Duration::from_millis(8));
        assert_eq!(clock.take_tick(), None, "one take drains the slot");
    }

    #[test]
    fn stop_waits_for_a_held_callback_then_frees() {
        let factory = FakeFrameClockFactory::new();
        let (wake, count) = wake();
        let display = DisplayId::new(2);
        let clock = factory.create(display, wake).unwrap();
        let held = factory.fire_held(display, Instant::now()).unwrap();
        held.wait_entered();
        // Held in flight: a short budget cannot stop it, the clock stays owned.
        let stuck = clock.stop(Duration::from_millis(30)).unwrap_err();
        assert_eq!(factory.live(), 1);
        assert!(factory.stopped().is_empty());
        held.release();
        // The held callback saw `stopped`: no tick was stored and no wake ran.
        assert_eq!(count.load(Ordering::SeqCst), 0);
        stuck.retry(Duration::from_secs(1)).unwrap();
        assert_eq!(factory.live(), 0);
        assert_eq!(factory.stopped(), vec![display]);
        assert!(
            !factory.fire(display, Instant::now()),
            "no clock, no callback after release"
        );
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_stuck_stop_is_retried_until_it_works() {
        let factory = FakeFrameClockFactory::new();
        let (wake, _) = wake();
        let display = DisplayId::new(3);
        let clock = factory.create(display, wake).unwrap();
        factory.set_stuck(display, true);
        let stuck = clock.stop(Duration::from_millis(10)).unwrap_err();
        let stuck = stuck.retry(Duration::from_millis(10)).unwrap_err();
        assert_eq!(factory.live(), 1, "never freed while stuck");
        factory.set_stuck(display, false);
        stuck.retry(Duration::from_millis(10)).unwrap();
        assert_eq!(factory.live(), 0);
        assert_eq!(factory.stop_attempts(), 3);
    }

    #[test]
    fn creation_failure_is_reported() {
        let factory = FakeFrameClockFactory::new();
        let (wake, _) = wake();
        factory.fail_creation(DisplayId::new(4), "no link");
        let error = factory.create(DisplayId::new(4), wake).err().unwrap();
        assert_eq!(error.reason, "no link");
        assert_eq!(factory.live(), 0);
    }
}
