//! The `CoreLocation` implementation of [`LocationSource`].
//!
//! A read is one hop on the main thread: [`SystemLocation::read`] runs on a
//! worker, dispatches `start` to the main queue and waits on a [`Completion`]
//! for at most the request timeout. On the main thread the manager and its
//! delegate live in a thread-local for exactly one read ("zero managers between
//! reads"). The delegate callbacks only translate the callback into an event and
//! dispatch it back to the main queue; all policy (authorization, accuracy,
//! `requestLocation`, teardown) runs in [`pump`] and is checked against the read
//! generation, so a late callback after a finish, a timeout or a newer read is
//! dropped. When a read times out while the consent prompt is still open, the
//! manager is **not** released: it becomes the single consent watcher (see
//! [`LocationSource`]), which only observes `locationManagerDidChangeAuthorization:`
//! until the status is decided, then releases the manager and calls the
//! notifier. Every callback body is wrapped in `catch_unwind` because the release
//! profile aborts on panic.

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class};
use objc2_core_location::{
    CLAccuracyAuthorization, CLLocation, CLLocationManager, CLLocationManagerDelegate,
    kCLLocationAccuracyBest, kCLLocationAccuracyReduced,
};
use objc2_foundation::{
    NSArray, NSBundle, NSDictionary, NSError, NSObject, NSObjectProtocol, NSString,
};

use super::{
    AuthorizationAction, BundleKeys, Completion, ConsentNotifier, ConsentWatch, GenerationGate,
    LocationOutcome, LocationRequest, LocationSource, LocationUnavailable, PURPOSE_KEY,
    TEMPORARY_DICTIONARY_KEY, USAGE_DESCRIPTION_KEY, WatchStep, authorization_action,
    error_outcome, fix_outcome, wants_temporary_full_accuracy,
};
use crate::main_thread::{is_main_thread, run_on_main};

#[link(name = "CoreLocation", kind = "framework")]
unsafe extern "C" {}

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
/// Set once `requestWhenInUseAuthorization` was called in this process.
static ASKED_AUTHORIZATION: AtomicBool = AtomicBool::new(false);
/// Set once the temporary full accuracy request was made in this process.
static ASKED_TEMPORARY: AtomicBool = AtomicBool::new(false);
/// Serializes reads so at most one manager exists at a time.
static READ_GATE: Mutex<()> = Mutex::new(());

/// The production `CoreLocation` source.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemLocation;

impl LocationSource for SystemLocation {
    fn read(&self, request: LocationRequest) -> LocationOutcome { read_once(request, None) }

    fn read_watched(
        &self,
        request: LocationRequest,
        on_decided: ConsentNotifier,
    ) -> LocationOutcome {
        read_once(request, Some(on_decided))
    }

    fn cancel_pending(&self) {
        // Enqueue only; never blocks the caller (which may be the main thread).
        let _ = run_on_main(Box::new(|| {
            STATE.with(|cell| {
                let mut state = cell.borrow_mut();
                if state.active.as_ref().is_some_and(|active| active.watching)
                    && let Some(active) = state.active.take()
                {
                    state.watch.cancel();
                    let _ = state.gate.finish(active.completion.generation());
                    teardown(&active);
                }
            });
        }));
    }
}

fn read_once(request: LocationRequest, on_decided: Option<ConsentNotifier>) -> LocationOutcome {
    // Waiting for a main-thread hop on the main thread would deadlock.
    if is_main_thread() {
        return LocationOutcome::Unavailable(LocationUnavailable::Unavailable);
    }
    let _one_at_a_time = READ_GATE.lock().unwrap_or_else(PoisonError::into_inner);
    let keys = bundle_keys();
    if !keys.usage_description {
        return LocationOutcome::Unavailable(LocationUnavailable::NoUsageDescription);
    }
    // SAFETY: `+[CLLocationManager locationServicesEnabled]` is a class
    // method with no arguments; Apple recommends calling it off the main
    // thread because it can block.
    if !unsafe { CLLocationManager::locationServicesEnabled_class() } {
        return LocationOutcome::Unavailable(LocationUnavailable::ServicesDisabled);
    }
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    let completion = Completion::new(generation);
    let awaiting = Arc::new(AtomicBool::new(false));
    let job = {
        let completion = Arc::clone(&completion);
        let awaiting = Arc::clone(&awaiting);
        Box::new(move || start(generation, request, keys, &completion, &awaiting, on_decided))
    };
    if run_on_main(job).is_err() {
        return LocationOutcome::Unavailable(LocationUnavailable::Unavailable);
    }
    if let Some(outcome) = completion.wait(request.timeout) {
        return outcome;
    }
    let reason = if awaiting.load(Ordering::SeqCst) {
        LocationUnavailable::NotDeterminedTimeout
    } else {
        LocationUnavailable::Timeout
    };
    let timed_out = LocationOutcome::Unavailable(reason);
    // The delegate may have won the race by a hair; first writer wins.
    let _ = completion.complete(timed_out);
    // Waiting on consent: keep the one manager as the consent watcher.
    // Otherwise release it; a callback arriving later is dropped.
    let event = if reason == LocationUnavailable::NotDeterminedTimeout {
        Event::Detach
    } else {
        Event::Cancel
    };
    let _ = run_on_main(Box::new(move || pump(generation, event)));
    completion.wait(Duration::ZERO).unwrap_or(timed_out)
}

/// Reads the Info.plist facts from the main bundle. Safe off the main thread.
fn bundle_keys() -> BundleKeys {
    let info = NSBundle::mainBundle().infoDictionary();
    let Some(info) = info else {
        return BundleKeys {
            usage_description: false,
            temporary_purpose: false,
        };
    };
    let usage_description = info
        .objectForKey(&NSString::from_str(USAGE_DESCRIPTION_KEY))
        .and_then(|value| value.downcast_ref::<NSString>().map(ToString::to_string))
        .is_some_and(|text| !text.trim().is_empty());
    let temporary_purpose = info
        .objectForKey(&NSString::from_str(TEMPORARY_DICTIONARY_KEY))
        .is_some_and(|value| {
            value.downcast_ref::<NSDictionary>().is_some_and(|dictionary| {
                dictionary.objectForKey(&NSString::from_str(PURPOSE_KEY)).is_some()
            })
        });
    BundleKeys {
        usage_description,
        temporary_purpose,
    }
}

// ---------------------------------------------------------------------------
// Main-thread state.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Authorization,
    TemporaryAccuracy,
    Locating,
}

struct Active {
    manager: Retained<CLLocationManager>,
    // Keeps the delegate alive: `CLLocationManager.delegate` is weak.
    _delegate: Retained<LocationDelegate>,
    request: LocationRequest,
    keys: BundleKeys,
    completion: Arc<Completion>,
    awaiting: Arc<AtomicBool>,
    phase: Phase,
    /// The read is over; this manager only observes the consent decision.
    watching: bool,
    on_decided: Option<ConsentNotifier>,
}

#[derive(Default)]
struct MainState {
    gate: GenerationGate,
    watch: ConsentWatch,
    active: Option<Active>,
}

thread_local! {
    static STATE: RefCell<MainState> = RefCell::new(MainState::default());
}

#[derive(Clone, Copy, Debug)]
struct RawFix {
    latitude: f64,
    longitude: f64,
    accuracy: f64,
}

#[derive(Clone, Copy, Debug)]
enum Event {
    AuthorizationChanged,
    TemporaryDone,
    Fix(RawFix),
    Failed(isize),
    Cancel,
    /// The read timed out: become the consent watcher if still waiting.
    Detach,
}

#[derive(Clone, Copy)]
enum Step {
    Continue,
    Finish(Option<LocationOutcome>),
}

/// Queues `event` for the main thread; the only thing a callback does.
fn dispatch(generation: u64, event: Event) {
    let _ = run_on_main(Box::new(move || pump(generation, event)));
}

fn start(
    generation: u64,
    request: LocationRequest,
    keys: BundleKeys,
    completion: &Arc<Completion>,
    awaiting: &Arc<AtomicBool>,
    on_decided: Option<ConsentNotifier>,
) {
    let Some(mtm) = MainThreadMarker::new() else {
        let _ = completion.complete(LocationOutcome::Unavailable(LocationUnavailable::Unavailable));
        return;
    };
    STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        // A previous read is always finished or cancelled first; drop any
        // leftover so two managers never coexist.
        if let Some(previous) = state.active.take() {
            state.watch.cancel();
            teardown(&previous);
        }
        state.gate.begin(generation);
        // SAFETY: `new` is NSObject's allocate-and-init; CLLocationManager
        // is created on the main thread, so its callbacks run on the main run
        // loop.
        let manager: Retained<CLLocationManager> = unsafe { CLLocationManager::new() };
        let delegate = LocationDelegate::new(mtm, generation);
        // SAFETY: the delegate conforms to CLLocationManagerDelegate and is kept
        // alive by `Active` until `teardown` clears it (the property is weak).
        unsafe { manager.setDelegate(Some(ProtocolObject::from_ref(&*delegate))) };
        let mut active = Active {
            manager,
            _delegate: delegate,
            request,
            keys,
            completion: Arc::clone(completion),
            awaiting: Arc::clone(awaiting),
            phase: Phase::Authorization,
            watching: false,
            on_decided,
        };
        let step = authorize(&mut active);
        finish_or_keep(&mut state, generation, active, step);
    });
}

fn pump(generation: u64, event: Event) {
    let notify = STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        if !state.gate.accepts(generation) {
            return None; // late callback after a finish, a timeout or a newer read
        }
        let mut active = state.active.take()?;
        if active.watching {
            return watch(&mut state, generation, active, event);
        }
        if matches!(event, Event::Detach) {
            return detach(&mut state, generation, active);
        }
        let step = handle(&mut active, event);
        finish_or_keep(&mut state, generation, active, step);
        None
    });
    // Outside the state borrow, and contained: the release profile aborts on
    // panic.
    if let Some(notify) = notify {
        let _ = catch_unwind(AssertUnwindSafe(|| notify()));
    }
}

/// The read timed out. Keep the manager as the consent watcher when it is
/// still waiting for the decision; if the decision already arrived, end now.
fn detach(state: &mut MainState, generation: u64, mut active: Active) -> Option<ConsentNotifier> {
    if active.phase != Phase::Authorization || !active.awaiting.load(Ordering::SeqCst) {
        let _ = state.gate.finish(generation);
        teardown(&active);
        return None;
    }
    active.watching = true;
    state.watch.begin(generation);
    watch(state, generation, active, Event::AuthorizationChanged)
}

/// A consent watcher only reacts to authorization changes; everything else is
/// ignored. A decision tears it down and yields the notifier.
fn watch(
    state: &mut MainState,
    generation: u64,
    active: Active,
    event: Event,
) -> Option<ConsentNotifier> {
    match event {
        Event::AuthorizationChanged => {
            // SAFETY: plain property read on the live manager.
            let status = unsafe { active.manager.authorizationStatus() }.0;
            match state.watch.on_authorization(generation, status) {
                WatchStep::Keep => {
                    state.active = Some(active);
                    None
                }
                WatchStep::Decided => {
                    let _ = state.gate.finish(generation);
                    teardown(&active);
                    active.on_decided
                }
                WatchStep::Dropped => {
                    let _ = state.gate.finish(generation);
                    teardown(&active);
                    None
                }
            }
        }
        Event::Cancel => {
            state.watch.cancel();
            let _ = state.gate.finish(generation);
            teardown(&active);
            None
        }
        _ => {
            state.active = Some(active);
            None
        }
    }
}

fn finish_or_keep(state: &mut MainState, generation: u64, active: Active, step: Step) {
    match step {
        Step::Continue => state.active = Some(active),
        Step::Finish(outcome) => {
            if let Some(outcome) = outcome {
                let _ = active.completion.complete(outcome);
            }
            let _ = state.gate.finish(generation);
            teardown(&active);
        }
    }
}

fn teardown(active: &Active) {
    active.awaiting.store(false, Ordering::SeqCst);
    // SAFETY: the manager is the one created for this read, on the main thread.
    unsafe {
        active.manager.setDelegate(None);
        active.manager.stopUpdatingLocation();
    }
}

fn handle(active: &mut Active, event: Event) -> Step {
    match event {
        Event::Cancel => Step::Finish(None),
        Event::AuthorizationChanged if active.phase == Phase::Authorization => authorize(active),
        Event::TemporaryDone if active.phase == Phase::TemporaryAccuracy => locate(active),
        Event::Fix(raw) if active.phase == Phase::Locating => {
            // SAFETY: plain property read on the live manager.
            let reduced = unsafe { active.manager.accuracyAuthorization() }
                == CLAccuracyAuthorization::ReducedAccuracy;
            Step::Finish(Some(fix_outcome(
                raw.latitude,
                raw.longitude,
                raw.accuracy,
                reduced,
            )))
        }
        Event::Failed(code) => {
            Step::Finish(Some(LocationOutcome::Unavailable(error_outcome(code))))
        }
        _ => Step::Continue,
    }
}

fn authorize(active: &mut Active) -> Step {
    // SAFETY: plain property read on the live manager.
    let status = unsafe { active.manager.authorizationStatus() }.0;
    match authorization_action(status, ASKED_AUTHORIZATION.load(Ordering::SeqCst)) {
        AuthorizationAction::Request => {
            ASKED_AUTHORIZATION.store(true, Ordering::SeqCst);
            active.awaiting.store(true, Ordering::SeqCst);
            // SAFETY: documented instance method (macOS 10.15+); only called
            // while the status is notDetermined, at most once per process.
            unsafe { active.manager.requestWhenInUseAuthorization() };
            Step::Continue
        }
        AuthorizationAction::Wait => {
            active.awaiting.store(true, Ordering::SeqCst);
            Step::Continue
        }
        AuthorizationAction::Proceed => {
            active.awaiting.store(false, Ordering::SeqCst);
            accuracy(active)
        }
        AuthorizationAction::Fail(reason) => {
            Step::Finish(Some(LocationOutcome::Unavailable(reason)))
        }
    }
}

fn accuracy(active: &mut Active) -> Step {
    // SAFETY: plain property read on the live manager.
    let reduced = unsafe { active.manager.accuracyAuthorization() }
        == CLAccuracyAuthorization::ReducedAccuracy;
    let asked = ASKED_TEMPORARY.load(Ordering::SeqCst);
    if wants_temporary_full_accuracy(active.request.precise, reduced, active.keys, asked) {
        ASKED_TEMPORARY.store(true, Ordering::SeqCst);
        active.phase = Phase::TemporaryAccuracy;
        let generation = active.completion.generation();
        let done = RcBlock::new(move |_error: *mut NSError| {
            let _ = catch_unwind(AssertUnwindSafe(|| dispatch(generation, Event::TemporaryDone)));
        });
        // SAFETY: the purpose key is declared in the bundle (checked in
        // `wants_temporary_full_accuracy`); the block only dispatches an event
        // and is copied by CoreLocation.
        unsafe {
            active
                .manager
                .requestTemporaryFullAccuracyAuthorizationWithPurposeKey_completion(
                    &NSString::from_str(PURPOSE_KEY),
                    Some(&done),
                );
        }
        Step::Continue
    } else {
        locate(active)
    }
}

fn locate(active: &mut Active) -> Step {
    active.phase = Phase::Locating;
    // SAFETY: reads the exported accuracy constants and uses the documented
    // one-shot `requestLocation` on the live manager.
    unsafe {
        let desired = if active.request.precise {
            kCLLocationAccuracyBest
        } else {
            kCLLocationAccuracyReduced
        };
        active.manager.setDesiredAccuracy(desired);
        active.manager.requestLocation();
    }
    Step::Continue
}

// ---------------------------------------------------------------------------
// Delegate.

struct DelegateIvars {
    generation: u64,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. The class owns only a
    // generation number; CoreLocation calls the selectors with valid arguments
    // on the thread that created the manager. It is `MainThreadOnly`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "StacheLocationDelegate"]
    #[ivars = DelegateIvars]
    struct LocationDelegate;

    unsafe impl NSObjectProtocol for LocationDelegate {}

    unsafe impl CLLocationManagerDelegate for LocationDelegate {
        #[unsafe(method(locationManagerDidChangeAuthorization:))]
        fn did_change_authorization(&self, _manager: &CLLocationManager) {
            let generation = self.ivars().generation;
            let _ = catch_unwind(AssertUnwindSafe(|| {
                dispatch(generation, Event::AuthorizationChanged);
            }));
        }

        #[unsafe(method(locationManager:didUpdateLocations:))]
        fn did_update_locations(
            &self,
            _manager: &CLLocationManager,
            locations: &NSArray<CLLocation>,
        ) {
            let generation = self.ivars().generation;
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let event = locations.lastObject().map_or(Event::Failed(-1), |location| {
                    // SAFETY: plain property reads on a live CLLocation.
                    let (coordinate, accuracy) =
                        unsafe { (location.coordinate(), location.horizontalAccuracy()) };
                    Event::Fix(RawFix {
                        latitude: coordinate.latitude,
                        longitude: coordinate.longitude,
                        accuracy,
                    })
                });
                dispatch(generation, event);
            }));
        }

        #[unsafe(method(locationManager:didFailWithError:))]
        fn did_fail_with_error(&self, _manager: &CLLocationManager, error: &NSError) {
            let generation = self.ivars().generation;
            let _ = catch_unwind(AssertUnwindSafe(|| {
                dispatch(generation, Event::Failed(error.code()));
            }));
        }
    }
);

impl LocationDelegate {
    fn new(mtm: MainThreadMarker, generation: u64) -> Retained<Self> {
        let allocated = Self::alloc(mtm).set_ivars(DelegateIvars { generation });
        // SAFETY: `alloc` returns an uninitialized instance of this exact
        // subclass and `init` is NSObject's designated initializer; the class
        // has no `Drop`.
        unsafe { objc2::msg_send![super(allocated), init] }
    }
}
