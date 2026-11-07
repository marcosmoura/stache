//! Fake-backed tests for the shared input contract (KB-02-01..07).
//!
//! No test creates a real tap or hotkey.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex, PoisonError};
use std::time::Duration;

use super::testing::{CapacityWork, FakeBackend, FakeClock, FakeResponse};
use super::*;
use crate::module::ModuleId;
use crate::threading::CancellationToken;

const A: KeyCode = KeyCode(0);
const SHIFT_KEY: KeyCode = KeyCode(56);

struct Rig {
    clock: Arc<FakeClock>,
    backend: Arc<FakeBackend>,
    work: Arc<CapacityWork>,
    service: Arc<InputService>,
}

fn rig_with_work(capacity: usize) -> Rig {
    let clock = FakeClock::new();
    let backend = FakeBackend::new(&clock);
    let work = CapacityWork::new(capacity);
    let service = InputService::with_clock(
        Arc::clone(&backend) as Arc<dyn InputBackend>,
        Arc::clone(&work) as Arc<dyn WorkReservations>,
        Arc::clone(&clock) as Arc<dyn InputClock>,
    );
    Rig { clock, backend, work, service }
}

fn rig() -> Rig { rig_with_work(64) }

#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<(InputEventKind, bool)>>,
    contexts: Mutex<Vec<FrontmostState>>,
    suppress: AtomicBool,
    panic_on_next: AtomicBool,
    cancels: AtomicUsize,
    state_seen: Mutex<Vec<u64>>,
    held_work: Mutex<Vec<WorkReservation>>,
    take_work: AtomicBool,
    advance: Mutex<Option<(Arc<FakeClock>, Duration)>>,
}

impl Recorder {
    fn suppressing() -> Arc<Self> {
        let recorder = Self::default();
        recorder.suppress.store(true, Ordering::SeqCst);
        Arc::new(recorder)
    }

    fn passing() -> Arc<Self> { Arc::new(Self::default()) }

    fn count(&self) -> usize { self.calls.lock().unwrap_or_else(PoisonError::into_inner).len() }

    fn calls(&self) -> Vec<(InputEventKind, bool)> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl InputHandler for Recorder {
    fn on_event(&self, event: &InputEvent, scope: &mut CallbackScope) -> Disposition {
        assert!(
            !self.panic_on_next.swap(false, Ordering::SeqCst),
            "fake handler panic"
        );
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((event.kind(), event.repeat()));
        self.contexts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event.context().frontmost().clone());
        self.state_seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(scope.press_state());
        scope.set_press_state(scope.press_state() + 1);
        if self.take_work.load(Ordering::SeqCst)
            && let Some(work) = scope.take_work()
        {
            self.held_work.lock().unwrap_or_else(PoisonError::into_inner).push(work);
        }
        if let Some((clock, by)) =
            self.advance.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
        {
            clock.advance(*by);
        }
        if self.suppress.load(Ordering::SeqCst) {
            Disposition::Suppress
        } else {
            Disposition::Pass
        }
    }

    fn on_cancel(&self, _press: PressId) { self.cancels.fetch_add(1, Ordering::SeqCst); }
}

#[derive(Default)]
struct Watcher {
    seen: Mutex<Vec<(InputEventKind, ModifierFlags)>>,
    gaps: AtomicUsize,
}

impl InputObserver for Watcher {
    fn on_event(&self, event: &InputEvent) {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((event.kind(), event.flags()));
    }

    fn on_discontinuity(&self) { self.gaps.fetch_add(1, Ordering::SeqCst); }
}

fn raw(kind: InputEventKind, stream: PressStream, flags: ModifierFlags, repeat: bool) -> RawInput {
    RawInput {
        kind,
        stream,
        flags,
        repeat,
        timestamp: Duration::ZERO,
    }
}

fn down(code: KeyCode) -> RawInput {
    raw(
        InputEventKind::KeyDown,
        PressStream::Key(code),
        ModifierFlags::NONE,
        false,
    )
}

fn repeat(code: KeyCode) -> RawInput {
    raw(
        InputEventKind::KeyDown,
        PressStream::Key(code),
        ModifierFlags::NONE,
        true,
    )
}

fn up(code: KeyCode) -> RawInput {
    raw(
        InputEventKind::KeyUp,
        PressStream::Key(code),
        ModifierFlags::NONE,
        false,
    )
}

fn button(kind: InputEventKind, number: u32) -> RawInput {
    raw(
        kind,
        PressStream::Button(MouseButton(number)),
        ModifierFlags::NONE,
        false,
    )
}

fn key_kinds() -> KindSet { KindSet::of(InputEventKind::KeyDown).with(InputEventKind::KeyUp) }

fn spec(filter: InputFilter, level: u16) -> HandlerSpec {
    HandlerSpec {
        filter,
        priority: InputPriority { level, tie_break: 0 },
        work: WorkRequirement::None,
    }
}

fn any_key() -> InputFilter { InputFilter::new(key_kinds()) }

fn live() -> CancellationToken { CancellationToken::new() }

fn handler(rig: &Rig, recorder: &Arc<Recorder>, filter: InputFilter, level: u16) -> Subscription {
    rig.service
        .subscribe_handler(
            spec(filter, level),
            Arc::clone(recorder) as Arc<dyn InputHandler>,
            &live(),
        )
        .expect("subscribe handler")
}

fn observer(rig: &Rig, watcher: &Arc<Watcher>, filter: InputFilter) -> Subscription {
    rig.service
        .subscribe_observer(
            ObserverSpec { filter },
            Arc::clone(watcher) as Arc<dyn InputObserver>,
            &live(),
        )
        .expect("subscribe observer")
}

fn sup(rig: &Rig, raw: RawInput) -> Disposition { rig.service.dispatch(TapKind::Suppressing, raw) }

// ---------------------------------------------------------------- KB-02-01

#[test]
fn kb_02_01_two_services_are_independent_and_ids_do_not_cross() {
    let first = rig();
    let second = rig();
    let recorder = Recorder::suppressing();
    let sub = handler(&first, &recorder, any_key(), 1);
    assert_eq!(
        first.backend.count(TapKind::Suppressing, ControlAction::Install),
        1
    );
    assert_eq!(second.backend.submit_calls(), 0);
    assert_eq!(
        second.service.unsubscribe(sub.id()),
        Err(InputError::ForeignService)
    );
    assert_eq!(sup(&second, down(A)), Disposition::Pass);
    assert_eq!(sup(&first, down(A)), Disposition::Suppress);
}

#[test]
fn kb_02_01_lazy_tap_first_and_last_subscriber() {
    let rig = rig();
    assert_eq!(rig.backend.submit_calls(), 0, "no idle tap");
    assert_eq!(rig.service.tap_status(TapKind::Suppressing), TapStatus::Absent);
    let recorder = Recorder::passing();
    let first = handler(&rig, &recorder, any_key(), 1);
    let second = handler(&rig, &recorder, any_key(), 2);
    assert_eq!(
        rig.backend.count(TapKind::Suppressing, ControlAction::Install),
        1
    );
    assert_eq!(rig.backend.count(TapKind::Suppressing, ControlAction::Enable), 1);
    assert_eq!(rig.service.tap_status(TapKind::Suppressing), TapStatus::Ready);
    assert_eq!(first.unsubscribe().native, NativeCleanup::NotNeeded);
    assert!(rig.backend.is_live(TapKind::Suppressing));
    assert_eq!(second.unsubscribe().native, NativeCleanup::Closed);
    assert!(!rig.backend.is_live(TapKind::Suppressing));
    assert_eq!(rig.service.tap_status(TapKind::Suppressing), TapStatus::Absent);
}

#[test]
#[allow(clippy::needless_collect)]
fn kb_02_01_simultaneous_subscribe_installs_once_and_last_unsubscribe_removes_once() {
    let rig = rig();
    let barrier = Arc::new(Barrier::new(8));
    let subs: Vec<Subscription> = std::thread::scope(|scope| {
        let joins: Vec<_> = (0..8)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let service = Arc::clone(&rig.service);
                scope.spawn(move || {
                    barrier.wait();
                    service
                        .subscribe_handler(
                            spec(any_key(), 1),
                            Recorder::passing() as Arc<dyn InputHandler>,
                            &CancellationToken::new(),
                        )
                        .expect("subscribe")
                })
            })
            .collect();
        joins.into_iter().map(|j| j.join().expect("join")).collect()
    });
    assert_eq!(
        rig.backend.count(TapKind::Suppressing, ControlAction::Install),
        1
    );
    let barrier = Arc::new(Barrier::new(subs.len()));
    std::thread::scope(|scope| {
        for sub in subs {
            let barrier = Arc::clone(&barrier);
            scope.spawn(move || {
                barrier.wait();
                drop(sub);
            });
        }
    });
    assert_eq!(rig.backend.count(TapKind::Suppressing, ControlAction::Remove), 1);
    assert_eq!(rig.service.tap_status(TapKind::Suppressing), TapStatus::Absent);
}

#[test]
fn kb_02_01_startup_cancellation_never_installs_late() {
    let rig = rig();
    rig.backend
        .script(TapKind::Suppressing, ControlAction::Install, FakeResponse::Hold);
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    rig.clock.on_advance(move || trigger.cancel());
    let result = rig.service.subscribe_handler(
        spec(any_key(), 1),
        Recorder::passing() as Arc<dyn InputHandler>,
        &cancel,
    );
    assert_eq!(result.err(), Some(InputError::Cancelled));
    // The cleanup removal drained the queue; the cancelled install was skipped.
    assert_eq!(
        rig.backend.count(TapKind::Suppressing, ControlAction::Install),
        0
    );
    assert!(!rig.backend.is_live(TapKind::Suppressing));
    assert_eq!(rig.service.tap_status(TapKind::Suppressing), TapStatus::Absent);
    // A token cancelled before the call issues nothing at all.
    let before = rig.backend.submit_calls();
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let result = rig.service.subscribe_handler(
        spec(any_key(), 1),
        Recorder::passing() as Arc<dyn InputHandler>,
        &cancelled,
    );
    assert_eq!(result.err(), Some(InputError::Cancelled));
    assert_eq!(rig.backend.submit_calls(), before);
}

#[test]
fn kb_02_01_late_readiness_is_ignored_and_removal_is_not_lost() {
    let rig = rig();
    rig.backend
        .script(TapKind::Suppressing, ControlAction::Install, FakeResponse::Hold);
    rig.backend
        .script(TapKind::Suppressing, ControlAction::Remove, FakeResponse::Hold);
    let started = rig.clock.now();
    let result = rig.service.subscribe_handler(
        spec(any_key(), 1),
        Recorder::passing() as Arc<dyn InputHandler>,
        &live(),
    );
    assert!(matches!(
        result.err(),
        Some(InputError::Native {
            action: ControlAction::Install,
            cause: FailureCause::TimedOut,
            ..
        })
    ));
    assert!(rig.clock.elapsed_since(started) >= ACKNOWLEDGEMENT_BUDGET);
    // The removal also timed out: inert resources are retained, honestly reported.
    assert!(matches!(
        rig.service.tap_status(TapKind::Suppressing),
        TapStatus::Retained(Retained {
            action: ControlAction::Remove,
            cause: FailureCause::TimedOut
        })
    ));
    assert!(
        rig.service
            .diagnostics()
            .iter()
            .any(|d| matches!(d, InputDiagnostic::TapNotClosed { .. }))
    );
    // The owner finally runs: install was revoked, removal still runs.
    rig.backend.set_default(FakeResponse::Ack);
    assert_eq!(rig.service.retry_cleanup(TapKind::Suppressing), Ok(()));
    assert_eq!(
        rig.backend.count(TapKind::Suppressing, ControlAction::Install),
        0
    );
    assert!(rig.backend.count(TapKind::Suppressing, ControlAction::Remove) >= 1);
    assert_eq!(rig.service.tap_status(TapKind::Suppressing), TapStatus::Absent);
}

#[test]
fn kb_02_01_dropped_acknowledgement_times_out_without_false_success() {
    let rig = rig();
    rig.backend
        .script(TapKind::Suppressing, ControlAction::Install, FakeResponse::Drop);
    let result = rig.service.subscribe_handler(
        spec(any_key(), 1),
        Recorder::passing() as Arc<dyn InputHandler>,
        &live(),
    );
    assert!(matches!(
        result.err(),
        Some(InputError::Native {
            cause: FailureCause::TimedOut,
            ..
        })
    ));
    assert_eq!(rig.service.tap_status(TapKind::Suppressing), TapStatus::Absent);
    assert_eq!(
        rig.service.reservations().len(),
        0,
        "failed subscribe leaks no reservation"
    );
}

#[test]
fn kb_02_01_dispatch_rejection_is_counted_and_retains_nothing_for_install() {
    let rig = rig();
    rig.backend.script(
        TapKind::Suppressing,
        ControlAction::Install,
        FakeResponse::Reject(ControlRejected::QueueFull),
    );
    let result = rig.service.subscribe_handler(
        spec(any_key(), 1),
        Recorder::passing() as Arc<dyn InputHandler>,
        &live(),
    );
    assert!(matches!(
        result.err(),
        Some(InputError::Native {
            cause: FailureCause::Rejected(ControlRejected::QueueFull),
            ..
        })
    ));
    assert_eq!(rig.service.counters().control_rejected, 1);
    assert_eq!(rig.service.tap_status(TapKind::Suppressing), TapStatus::Absent);
}

#[test]
fn kb_02_01_removal_rejection_retains_ids_and_successor_gets_new_generation() {
    let rig = rig();
    let recorder = Recorder::suppressing();
    let sub = handler(&rig, &recorder, any_key(), 1);
    let old = sub.id();
    rig.backend.script(
        TapKind::Suppressing,
        ControlAction::Remove,
        FakeResponse::Fail("owner failure".to_owned()),
    );
    let report = sub.unsubscribe();
    assert!(matches!(
        report.native,
        NativeCleanup::Retained(Retained {
            action: ControlAction::Remove,
            cause: FailureCause::Failed(_)
        })
    ));
    // Logical invalidation did not wait for native cleanup.
    assert_eq!(sup(&rig, down(A)), Disposition::Pass);
    assert_eq!(recorder.count(), 0);
    // The next subscriber closes the retained tap first, then installs a successor.
    let successor = handler(&rig, &recorder, any_key(), 1);
    assert!(successor.id().generation > old.generation);
    assert_eq!(rig.service.unsubscribe(old), Err(InputError::StaleSubscription));
}

#[test]
fn kb_02_01_control_queue_has_exact_256_boundary() {
    let rig = rig();
    rig.backend.set_default(FakeResponse::Hold);
    let ticket = || super::control::Ticket::new(None);
    for _ in 0..CONTROL_QUEUE_CAPACITY {
        let request =
            ControlRequest::new(TapKind::Suppressing, ControlAction::Install, 1, ticket());
        assert_eq!(rig.backend.submit(request), Ok(()));
    }
    let request = ControlRequest::new(TapKind::Suppressing, ControlAction::Install, 1, ticket());
    assert_eq!(rig.backend.submit(request), Err(ControlRejected::QueueFull));
    assert_eq!(rig.backend.queued(), CONTROL_QUEUE_CAPACITY);
    assert!(rig.backend.process_next(&FakeResponse::Ack));
    let request = ControlRequest::new(TapKind::Suppressing, ControlAction::Install, 1, ticket());
    assert_eq!(rig.backend.submit(request), Ok(()));
}

#[test]
fn kb_02_01_unavailable_service_is_inert() {
    let service = InputService::unavailable();
    let result = service.subscribe_handler(
        spec(any_key(), 1),
        Recorder::passing() as Arc<dyn InputHandler>,
        &live(),
    );
    assert_eq!(result.err(), Some(InputError::Unavailable));
    assert!(!service.is_available());
    assert_eq!(
        service.dispatch(TapKind::Suppressing, down(A)),
        Disposition::Pass
    );
    let capability = InputCapability::new(
        ModuleId::new("t"),
        Arc::clone(&service),
        CancellationToken::new(),
    );
    assert!(!capability.is_available());
}

#[test]
fn kb_02_01_capability_invalidation_stops_callbacks_and_release_closes_tap() {
    let rig = rig();
    let capability = InputCapability::new(
        ModuleId::new("t"),
        Arc::clone(&rig.service),
        CancellationToken::new(),
    );
    let recorder = Recorder::suppressing();
    let sub = capability
        .subscribe_handler(
            spec(any_key(), 1),
            Arc::clone(&recorder) as Arc<dyn InputHandler>,
        )
        .expect("subscribe");
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
    capability.invalidate();
    assert_eq!(sub.inert_reason(), Some(InertReason::Invalidated));
    // Tombstone consumes the up without calling the module.
    assert_eq!(sup(&rig, up(A)), Disposition::Suppress);
    assert_eq!(recorder.count(), 1);
    assert_eq!(
        capability
            .subscribe_handler(spec(any_key(), 1), Recorder::passing() as Arc<dyn InputHandler>)
            .err(),
        Some(InputError::Invalidated)
    );
    capability.release_owner();
    assert_eq!(rig.service.tap_status(TapKind::Suppressing), TapStatus::Absent);
    drop(sub);
}

// ---------------------------------------------------------------- KB-02-02

#[test]
fn kb_02_02_modifier_release_flags_changed_reaches_named_key_filter() {
    let filter = InputFilter::new(KindSet::of(InputEventKind::FlagsChanged))
        .with_keys([SHIFT_KEY])
        .with_modifiers(ModifierMatch::Exact(ModifierFlags::SHIFT));
    let key = PressStream::Key(SHIFT_KEY);
    let kind = InputEventKind::FlagsChanged;
    assert!(filter.matches(kind, key, ModifierFlags::SHIFT));
    // Release: the resulting mask no longer matches, but the named key still passes.
    assert!(filter.matches(kind, key, ModifierFlags::NONE));
    assert!(!filter.matches(kind, PressStream::Key(KeyCode(55)), ModifierFlags::NONE));
    // Without a named key the mask comparison applies.
    let anonymous = InputFilter::new(KindSet::of(kind))
        .with_modifiers(ModifierMatch::Exact(ModifierFlags::SHIFT));
    assert!(!anonymous.matches(kind, key, ModifierFlags::NONE));
}

#[test]
fn kb_02_02_modifier_release_is_delivered_by_the_router() {
    let rig = rig();
    let recorder = Recorder::passing();
    let filter = InputFilter::new(KindSet::of(InputEventKind::FlagsChanged))
        .with_keys([SHIFT_KEY])
        .with_modifiers(ModifierMatch::Exact(ModifierFlags::SHIFT));
    let _sub = handler(&rig, &recorder, filter, 1);
    let flags = |f| {
        raw(
            InputEventKind::FlagsChanged,
            PressStream::Key(SHIFT_KEY),
            f,
            false,
        )
    };
    let _ = sup(&rig, flags(ModifierFlags::SHIFT));
    let _ = sup(&rig, flags(ModifierFlags::NONE));
    assert_eq!(recorder.count(), 2);
}

#[test]
fn kb_02_02_button_two_is_exact_and_not_three_plus() {
    let two =
        InputFilter::new(KindSet::of(InputEventKind::MouseDown)).with_buttons([MouseButton(2)]);
    let stream = |n| PressStream::Button(MouseButton(n));
    let kind = InputEventKind::MouseDown;
    assert!(two.matches(kind, stream(2), ModifierFlags::NONE));
    for other in [0, 1, 3, 4, 31] {
        assert!(!two.matches(kind, stream(other), ModifierFlags::NONE));
    }
    assert!(!two.matches(InputEventKind::MouseUp, stream(2), ModifierFlags::NONE));
    let all = InputFilter::new(KindSet::of(kind));
    assert!(all.matches(kind, stream(7), ModifierFlags::NONE));
    // A key stream never matches a mouse kind.
    assert!(!all.matches(kind, PressStream::Key(A), ModifierFlags::NONE));
}

#[test]
fn kb_02_02_modifier_semantics_are_explicit() {
    let filter = |m| InputFilter::new(KindSet::of(InputEventKind::KeyDown)).with_modifiers(m);
    let key = PressStream::Key(A);
    let kind = InputEventKind::KeyDown;
    let cmd_shift = ModifierFlags::COMMAND | ModifierFlags::SHIFT;
    let exact = filter(ModifierMatch::Exact(ModifierFlags::COMMAND));
    assert!(exact.matches(kind, key, ModifierFlags::COMMAND));
    assert!(exact.matches(kind, key, ModifierFlags::COMMAND | ModifierFlags::CAPS_LOCK));
    assert!(!exact.matches(kind, key, cmd_shift));
    let all_of = filter(ModifierMatch::AllOf(ModifierFlags::COMMAND));
    assert!(all_of.matches(kind, key, cmd_shift));
    assert!(!all_of.matches(kind, key, ModifierFlags::SHIFT));
    assert!(filter(ModifierMatch::Any).matches(kind, key, ModifierFlags::NONE));
    assert_eq!(ModifierFlags::from_bits_truncate(u32::MAX).bits(), 0b11_1111);
}

#[test]
fn kb_02_02_filter_validation_and_overlap() {
    assert_eq!(
        InputFilter::new(KindSet::empty()).validate(),
        Err(FilterError::NoKinds)
    );
    assert_eq!(
        any_key().with_keys([]).validate(),
        Err(FilterError::EmptySelector)
    );
    let many =
        any_key().with_keys((0..=u16::try_from(MAX_FILTER_ENTRIES).expect("fits")).map(KeyCode));
    assert_eq!(many.validate(), Err(FilterError::TooManyEntries));
    assert_eq!(any_key().with_keys([A, A]).validate(), Ok(()));
    let a = any_key().with_keys([A]);
    let b = any_key().with_keys([KeyCode(1)]);
    assert!(!a.overlaps(&b));
    assert!(a.overlaps(&any_key()));
    let cmd = a.clone().with_modifiers(ModifierMatch::Exact(ModifierFlags::COMMAND));
    let shift = a.clone().with_modifiers(ModifierMatch::Exact(ModifierFlags::SHIFT));
    assert!(!cmd.overlaps(&shift));
    assert!(cmd.overlaps(&a.with_modifiers(ModifierMatch::AllOf(ModifierFlags::COMMAND))));
}

#[test]
fn kb_02_02_context_snapshots_are_versioned_with_unknown_and_stale_state() {
    let rig = rig();
    let recorder = Recorder::passing();
    let _sub = handler(&rig, &recorder, any_key(), 1);
    let publisher = rig.service.context_publisher();
    let _ = sup(&rig, down(A));
    let v1 = publisher.publish_frontmost(FrontmostApp {
        bundle_id: Arc::from("com.example.app"),
        exemptions: ExemptionFlags(4),
    });
    let _ = sup(&rig, down(KeyCode(1)));
    let v2 = publisher.mark_stale();
    let _ = sup(&rig, down(KeyCode(2)));
    assert!(v2 > v1);
    let seen = recorder.contexts.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(seen[0], FrontmostState::Unknown);
    assert!(
        matches!(&seen[1], FrontmostState::Known(app) if &*app.bundle_id == "com.example.app" && app.exemptions == ExemptionFlags(4))
    );
    assert_eq!(seen[2], FrontmostState::Stale);
}

// ---------------------------------------------------------------- KB-02-03

#[test]
fn kb_02_03_first_suppressor_wins_and_lower_never_sees_suppressed_event() {
    let rig = rig();
    let low = Recorder::passing();
    let high = Recorder::suppressing();
    let mid = Recorder::passing();
    let _low = handler(&rig, &low, any_key(), 1);
    let _high = handler(&rig, &high, any_key(), 9);
    let _mid = handler(&rig, &mid, any_key(), 5);
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
    assert_eq!(high.count(), 1);
    assert_eq!(mid.count() + low.count(), 0);
    // After the suppressor stops suppressing, lower ones run in order on new presses.
    assert_eq!(sup(&rig, up(A)), Disposition::Suppress);
    high.suppress.store(false, Ordering::SeqCst);
    assert_eq!(sup(&rig, down(A)), Disposition::Pass);
    assert_eq!((high.count(), mid.count(), low.count()), (3, 1, 1));
}

#[test]
fn kb_02_03_equal_priority_order_is_registration_order_and_diagnosed() {
    let rig = rig();
    let first = Recorder::suppressing();
    let second = Recorder::suppressing();
    let one = handler(&rig, &first, any_key(), 3);
    let two = handler(&rig, &second, any_key(), 3);
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
    assert_eq!((first.count(), second.count()), (1, 0));
    let diagnostics = rig.service.diagnostics();
    assert!(diagnostics.contains(&InputDiagnostic::Overlap {
        first: one.id(),
        second: two.id(),
        equal_priority: true,
    }));
    // A distinct priority is still an overlap, but not a tie.
    let three = handler(&rig, &Recorder::passing(), any_key(), 4);
    assert!(rig.service.diagnostics().contains(&InputDiagnostic::Overlap {
        first: one.id(),
        second: three.id(),
        equal_priority: false,
    }));
}

#[test]
fn kb_02_03_tie_break_orders_equal_levels() {
    let rig = rig();
    let low = Recorder::suppressing();
    let high = Recorder::suppressing();
    let mk = |tie| HandlerSpec {
        filter: any_key(),
        priority: InputPriority { level: 5, tie_break: tie },
        work: WorkRequirement::None,
    };
    let _a = rig
        .service
        .subscribe_handler(mk(1), Arc::clone(&low) as Arc<dyn InputHandler>, &live())
        .expect("a");
    let _b = rig
        .service
        .subscribe_handler(mk(2), Arc::clone(&high) as Arc<dyn InputHandler>, &live())
        .expect("b");
    let _ = sup(&rig, down(A));
    assert_eq!((high.count(), low.count()), (1, 0));
}

#[test]
fn kb_02_03_work_overflow_fails_open_for_whole_press_and_never_runs_late() {
    let rig = rig_with_work(1);
    let recorder = Recorder::suppressing();
    recorder.take_work.store(true, Ordering::SeqCst);
    let mut reserving = spec(any_key(), 1);
    reserving.work = WorkRequirement::Reserve;
    let _sub = rig
        .service
        .subscribe_handler(
            reserving,
            Arc::clone(&recorder) as Arc<dyn InputHandler>,
            &live(),
        )
        .expect("subscribe");
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
    assert_eq!(rig.work.used(), 1);
    // Capacity is spent: the whole second press passes, repeat/up included.
    assert_eq!(sup(&rig, down(KeyCode(1))), Disposition::Pass);
    assert_eq!(sup(&rig, repeat(KeyCode(1))), Disposition::Pass);
    assert_eq!(sup(&rig, up(KeyCode(1))), Disposition::Pass);
    assert_eq!(recorder.count(), 1, "rejected work never reaches the handler");
    assert_eq!(rig.service.counters().work_rejected, 1);
    // The suppressed press keeps repeat/up handling even though work cannot enqueue.
    assert_eq!(sup(&rig, repeat(A)), Disposition::Suppress);
    assert_eq!(sup(&rig, up(A)), Disposition::Suppress);
    assert_eq!(recorder.count(), 3);
    // Nothing executes later from the rejected press.
    recorder.held_work.lock().unwrap_or_else(PoisonError::into_inner).clear();
    assert_eq!(rig.work.used(), 0);
    assert_eq!(recorder.count(), 3);
}

#[test]
fn kb_02_03_empty_capture_needs_ledger_entry_but_no_work_slot() {
    let rig = rig_with_work(0);
    let recorder = Recorder::suppressing();
    let _sub = handler(&rig, &recorder, any_key(), 1);
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
    assert_eq!(rig.service.ledger_len(), 1);
    assert_eq!(rig.work.rejected(), 0);
    assert_eq!(sup(&rig, up(A)), Disposition::Suppress);
    assert_eq!(rig.service.ledger_len(), 0);
}

#[test]
fn kb_02_03_reservation_registry_yields_carbon_to_higher_priority_hid() {
    let rig = rig();
    let registry = rig.service.reservations();
    let owner = registry.new_owner();
    let chord = Chord::new(A, ModifierFlags::COMMAND | ModifierFlags::CAPS_LOCK);
    assert_eq!(chord.modifiers(), ModifierFlags::COMMAND);
    let carbon = registry
        .register(
            owner,
            ReservationBackend::Carbon,
            Claim::Chord(chord),
            InputPriority { level: 5, tie_break: 0 },
        )
        .expect("carbon");
    let overlapping = any_key()
        .with_keys([A])
        .with_modifiers(ModifierMatch::Exact(ModifierFlags::COMMAND));
    // Higher priority HID is accepted; the Carbon reservation yields.
    let high = handler(&rig, &Recorder::passing(), overlapping.clone(), 9);
    assert!(!registry.dispatchable(carbon));
    assert!(registry.conflicts().is_empty());
    assert!(rig.service.diagnostics().iter().any(
        |d| matches!(d, InputDiagnostic::Yielded { reservation, .. } if *reservation == carbon)
    ));
    drop(high);
    assert!(registry.dispatchable(carbon));
    // Equal priority is not a priority claim: it is recorded as a conflict.
    let sub = handler(&rig, &Recorder::passing(), overlapping, 5);
    assert_eq!(registry.conflicts().len(), 1);
    assert_eq!(registry.conflicts()[0].carbon, carbon);
    assert!(registry.dispatchable(carbon));
    // Raising the priority yields; moving off the chord restores and clears.
    sub.set_priority(InputPriority { level: 9, tie_break: 0 }).expect("raise");
    assert!(!registry.dispatchable(carbon));
    sub.set_filter(any_key().with_keys([KeyCode(1)])).expect("move filter");
    assert!(registry.conflicts().is_empty());
    assert!(registry.dispatchable(carbon));
    // A Carbon reservation arriving later records a conflict, never HID control.
    registry
        .register(
            owner,
            ReservationBackend::Carbon,
            Claim::Chord(Chord::new(KeyCode(1), ModifierFlags::NONE)),
            InputPriority::default(),
        )
        .expect("later carbon");
    assert!(registry.dispatchable(carbon));
    // Ownership cleanup is opaque and total.
    assert_eq!(registry.release_owner(owner), 2);
    assert!(registry.conflicts().is_empty());
    assert_eq!(registry.remove(carbon), Err(ReservationError::Stale));
    assert!(!registry.dispatchable(carbon));
    drop(sub);
    assert!(registry.is_empty());
}

fn prio(level: u16) -> InputPriority { InputPriority { level, tie_break: 0 } }

fn chord_of(key: KeyCode) -> Chord { Chord::new(key, ModifierFlags::COMMAND) }

fn hid_for(key: KeyCode) -> Claim {
    Claim::Filter(
        any_key()
            .with_keys([key])
            .with_modifiers(ModifierMatch::Exact(ModifierFlags::COMMAND)),
    )
}

const Q: KeyCode = KeyCode(12);

#[test]
fn kb_03_06_yield_restores_on_claimant_removal_pause_replace_and_owner_release() {
    let rig = rig();
    let r = rig.service.reservations();
    let owner = r.new_owner();
    let carbon = r
        .register(
            owner,
            ReservationBackend::Carbon,
            Claim::Chord(chord_of(Q)),
            prio(1),
        )
        .expect("c");
    let hid_owner = r.new_owner();
    let hid = r.register(hid_owner, ReservationBackend::Hid, hid_for(Q), prio(9)).expect("h");
    assert_eq!(r.yielded_to(carbon), Some(hid));
    assert!(!r.dispatchable(carbon));

    // Pause restores; resume yields again (the yield persists across re-claims).
    r.set_active(hid, false).expect("pause");
    assert!(r.dispatchable(carbon));
    r.set_active(hid, true).expect("resume");
    assert!(!r.dispatchable(carbon));

    // Stop / disable: the claimant's reservation is removed.
    r.remove(hid).expect("remove");
    assert!(r.dispatchable(carbon));

    // Reload: the claimant is replaced by a new reservation of a new generation.
    let first = r
        .register(hid_owner, ReservationBackend::Hid, hid_for(Q), prio(9))
        .expect("first");
    assert!(!r.dispatchable(carbon));
    let reloaded = r.update(first, hid_for(Q), prio(9)).expect("replace");
    assert_ne!(first, reloaded);
    assert_eq!(
        r.yielded_to(carbon),
        Some(reloaded),
        "yield follows the replaced claimant"
    );
    assert!(!r.dispatchable(carbon));
    // Owner release (module stop) restores.
    assert_eq!(r.release_owner(hid_owner), 1);
    assert!(r.dispatchable(carbon));
    // Claimant moved off the chord restores.
    let moving = r.register(hid_owner, ReservationBackend::Hid, hid_for(Q), prio(9)).expect("m");
    assert!(!r.dispatchable(carbon));
    r.update(moving, hid_for(KeyCode(1)), prio(9)).expect("move");
    assert!(r.dispatchable(carbon));
}

#[test]
fn kb_03_06_yield_survives_reclaim_of_the_carbon_reservation() {
    let rig = rig();
    let r = rig.service.reservations();
    let owner = r.new_owner();
    let hid_owner = r.new_owner();
    let hid = r.register(hid_owner, ReservationBackend::Hid, hid_for(Q), prio(9)).expect("h");
    // Carbon registering under a live higher-priority claimant starts yielded.
    let carbon = r
        .register(
            owner,
            ReservationBackend::Carbon,
            Claim::Chord(chord_of(Q)),
            prio(1),
        )
        .expect("c");
    assert_eq!(r.yielded_to(carbon), Some(hid));
    // Re-claiming the same chord (update) keeps it yielded under the new ID.
    let reclaimed = r.update(carbon, Claim::Chord(chord_of(Q)), prio(1)).expect("reclaim");
    assert!(!r.dispatchable(reclaimed));
    assert!(
        !r.dispatchable(carbon),
        "the old ID is stale and never dispatches"
    );
    // Removing and re-registering (layout refresh) under the claimant also yields.
    r.remove(reclaimed).expect("remove");
    let again = r
        .register(
            owner,
            ReservationBackend::Carbon,
            Claim::Chord(chord_of(Q)),
            prio(1),
        )
        .expect("again");
    assert!(!r.dispatchable(again));
    r.remove(hid).expect("release");
    assert!(r.dispatchable(again));
}

#[test]
fn kb_03_06_double_yield_is_idempotent_and_explicit_yield_is_validated() {
    let rig = rig();
    let r = rig.service.reservations();
    let owner = r.new_owner();
    let carbon = r
        .register(
            owner,
            ReservationBackend::Carbon,
            Claim::Chord(chord_of(Q)),
            prio(5),
        )
        .expect("c");
    let equal = r
        .register(r.new_owner(), ReservationBackend::Hid, hid_for(Q), prio(5))
        .expect("eq");
    // Equal priority cannot be arbitrated.
    assert_eq!(
        r.yield_to(carbon, equal),
        Err(ReservationError::UnsupportedOverlap { existing: carbon })
    );
    assert!(r.dispatchable(carbon));
    let high = r
        .register(r.new_owner(), ReservationBackend::Hid, hid_for(Q), prio(9))
        .expect("h");
    assert_eq!(r.yield_to(carbon, high), Ok(()));
    assert_eq!(r.yield_to(carbon, high), Ok(()));
    assert_eq!(r.yielded_to(carbon), Some(high));
    assert!(!r.dispatchable(carbon));
    // One claimant going away does not restore while another outranks.
    r.remove(high).expect("remove");
    assert!(r.dispatchable(carbon));
    // Wrong kinds and non-overlapping claims are not yieldable.
    let elsewhere = r
        .register(
            r.new_owner(),
            ReservationBackend::Hid,
            hid_for(KeyCode(1)),
            prio(9),
        )
        .expect("e");
    assert_eq!(
        r.yield_to(carbon, elsewhere),
        Err(ReservationError::NotYieldable)
    );
    assert_eq!(r.yield_to(equal, carbon), Err(ReservationError::NotYieldable));
    let paused = r
        .register(r.new_owner(), ReservationBackend::Hid, hid_for(Q), prio(9))
        .expect("p");
    r.set_active(paused, false).expect("pause");
    assert_eq!(r.yield_to(carbon, paused), Err(ReservationError::NotYieldable));
    assert!(r.dispatchable(carbon));
}

#[test]
fn kb_03_06_stale_or_nonexistent_yield_is_rejected() {
    let rig = rig();
    let r = rig.service.reservations();
    let owner = r.new_owner();
    let carbon = r
        .register(
            owner,
            ReservationBackend::Carbon,
            Claim::Chord(chord_of(Q)),
            prio(1),
        )
        .expect("c");
    let hid = r
        .register(r.new_owner(), ReservationBackend::Hid, hid_for(Q), prio(9))
        .expect("h");
    let foreign_rig = self::rig();
    let foreign_owner = foreign_rig.service.reservations().new_owner();
    let other = foreign_rig
        .service
        .reservations()
        .register(foreign_owner, ReservationBackend::Hid, hid_for(Q), prio(9))
        .expect("foreign");
    assert_eq!(r.yield_to(carbon, other), Err(ReservationError::ForeignRegistry));
    r.remove(hid).expect("remove");
    assert_eq!(r.yield_to(carbon, hid), Err(ReservationError::Stale));
    r.remove(carbon).expect("remove carbon");
    assert_eq!(r.yield_to(carbon, hid), Err(ReservationError::Stale));
    assert_eq!(r.set_active(hid, false), Err(ReservationError::Stale));
    assert!(!r.dispatchable(carbon), "a stale reservation never dispatches");
    assert_eq!(r.yielded_to(carbon), None);
}

#[test]
fn kb_03_06_unrelated_reservations_are_unaffected_and_one_press_dispatches_once() {
    let rig = rig();
    let r = rig.service.reservations();
    let owner = r.new_owner();
    let w = KeyCode(13);
    let quit = r
        .register(
            owner,
            ReservationBackend::Carbon,
            Claim::Chord(chord_of(Q)),
            prio(1),
        )
        .expect("q");
    let close = r
        .register(
            owner,
            ReservationBackend::Carbon,
            Claim::Chord(chord_of(w)),
            prio(1),
        )
        .expect("w");
    let elsewhere = r
        .register(
            owner,
            ReservationBackend::Carbon,
            Claim::Chord(Chord::new(Q, ModifierFlags::SHIFT)),
            prio(1),
        )
        .expect("s");
    let _hid = r
        .register(r.new_owner(), ReservationBackend::Hid, hid_for(Q), prio(9))
        .expect("h");

    // Model one physical press: every dispatchable Carbon reservation on the chord
    // runs, plus the single HID handler that owns the claim.
    let carbon_dispatches = |chord: Chord, ids: &[ReservationId]| {
        ids.iter()
            .zip([
                chord_of(Q),
                chord_of(w),
                Chord::new(Q, ModifierFlags::SHIFT),
            ])
            .filter(|(id, c)| *c == chord && r.dispatchable(**id))
            .count()
    };
    let ids = [quit, close, elsewhere];
    assert_eq!(carbon_dispatches(chord_of(Q), &ids), 0, "yielded: only HID runs");
    assert_eq!(
        carbon_dispatches(chord_of(w), &ids),
        1,
        "unrelated binding dispatches once"
    );
    assert_eq!(carbon_dispatches(Chord::new(Q, ModifierFlags::SHIFT), &ids), 1);
    assert!(r.dispatchable(close) && r.dispatchable(elsewhere));
    assert!(r.conflicts().is_empty());
}

#[test]
fn kb_02_03_reservation_ids_do_not_cross_registries() {
    let one = rig();
    let two = rig();
    let owner = one.service.reservations().new_owner();
    let id = one
        .service
        .reservations()
        .register(
            owner,
            ReservationBackend::Carbon,
            Claim::Chord(Chord::new(A, ModifierFlags::NONE)),
            InputPriority::default(),
        )
        .expect("register");
    assert_eq!(
        two.service.reservations().remove(id),
        Err(ReservationError::ForeignRegistry)
    );
    let updated = one
        .service
        .reservations()
        .update(
            id,
            Claim::Chord(Chord::new(KeyCode(1), ModifierFlags::NONE)),
            InputPriority::default(),
        )
        .expect("update");
    assert_eq!(
        one.service.reservations().remove(id),
        Err(ReservationError::Stale)
    );
    assert_eq!(one.service.reservations().remove(updated), Ok(()));
}

#[test]
fn kb_02_03_handlers_cannot_run_for_unrelated_events() {
    let rig = rig();
    let recorder = Recorder::suppressing();
    let _sub = handler(&rig, &recorder, any_key().with_keys([A]), 1);
    assert_eq!(sup(&rig, down(KeyCode(1))), Disposition::Pass);
    assert_eq!(
        rig.service.ledger_len(),
        0,
        "an unmatched down needs no ledger entry"
    );
    let malformed = raw(
        InputEventKind::MouseDown,
        PressStream::Key(A),
        ModifierFlags::NONE,
        false,
    );
    assert_eq!(sup(&rig, malformed), Disposition::Pass);
    assert_eq!(rig.service.counters().malformed, 1);
}

// ---------------------------------------------------------------- KB-02-04

#[test]
fn kb_02_04_suppressed_down_then_logical_stop_then_up_is_consumed_by_tombstone() {
    let rig = rig();
    let recorder = Recorder::suppressing();
    let sub = handler(&rig, &recorder, any_key(), 1);
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
    let report = sub.unsubscribe();
    assert_eq!(report.native, NativeCleanup::Deferred);
    assert_eq!(sup(&rig, repeat(A)), Disposition::Suppress);
    assert_eq!(sup(&rig, up(A)), Disposition::Suppress);
    assert_eq!(recorder.count(), 1, "no callback after invalidation");
    assert_eq!(rig.service.ledger_len(), 0);
    settle_removed(&rig);
    // The orphan release after the tombstone is gone passes.
    assert_eq!(sup(&rig, up(A)), Disposition::Pass);
    assert_eq!(rig.service.counters().orphan_events, 1);
}

#[test]
fn kb_02_04_passed_down_never_acquires_a_suppressor_mid_press() {
    let rig = rig();
    let passing = Recorder::passing();
    let _low = handler(&rig, &passing, any_key(), 1);
    assert_eq!(sup(&rig, down(A)), Disposition::Pass);
    let late = Recorder::suppressing();
    let late_sub = handler(&rig, &late, any_key(), 9);
    assert_eq!(sup(&rig, repeat(A)), Disposition::Pass);
    // A priority change mid-press changes nothing for this press either.
    late_sub
        .set_priority(InputPriority { level: 20, tie_break: 0 })
        .expect("priority");
    assert_eq!(sup(&rig, up(A)), Disposition::Pass);
    assert_eq!(late.count(), 0);
    assert_eq!(rig.service.ledger_len(), 0);
    // The next press uses the new order.
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
}

#[test]
fn kb_02_04_priority_change_keeps_original_owner_tombstone_until_up() {
    let rig = rig();
    let recorder = Recorder::suppressing();
    let sub = handler(&rig, &recorder, any_key(), 1);
    let old = sub.id();
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
    sub.set_priority(InputPriority { level: 7, tie_break: 0 }).expect("priority");
    assert_ne!(sub.id(), old);
    assert_eq!(rig.service.unsubscribe(old), Err(InputError::StaleSubscription));
    assert_eq!(sup(&rig, up(A)), Disposition::Suppress);
    assert_eq!(
        recorder.count(),
        1,
        "the replaced entry is inert; the new one never saw the down"
    );
    assert_eq!(rig.service.ledger_len(), 0);
}

#[test]
fn kb_02_04_repeat_and_up_follow_the_ledger_with_per_press_state() {
    let rig = rig();
    let recorder = Recorder::suppressing();
    // Only the key A is matched; repeat/up are not re-filtered.
    let _sub = handler(
        &rig,
        &recorder,
        any_key().with_modifiers(ModifierMatch::Exact(ModifierFlags::COMMAND)),
        1,
    );
    let cmd = |mut r: RawInput| {
        r.flags = ModifierFlags::COMMAND;
        r
    };
    assert_eq!(sup(&rig, cmd(down(A))), Disposition::Suppress);
    // Command was released before the repeat/up: the ledger still routes them.
    assert_eq!(sup(&rig, repeat(A)), Disposition::Suppress);
    assert_eq!(sup(&rig, repeat(A)), Disposition::Suppress);
    assert_eq!(sup(&rig, up(A)), Disposition::Suppress);
    assert_eq!(recorder.calls(), vec![
        (InputEventKind::KeyDown, false),
        (InputEventKind::KeyDown, true),
        (InputEventKind::KeyDown, true),
        (InputEventKind::KeyUp, false),
    ]);
    assert_eq!(
        *recorder.state_seen.lock().unwrap_or_else(PoisonError::into_inner),
        vec![0, 1, 2, 3]
    );
    // Orphan repeat/up pass.
    assert_eq!(sup(&rig, repeat(A)), Disposition::Pass);
    assert_eq!(sup(&rig, up(A)), Disposition::Pass);
}

#[test]
fn kb_02_04_mouse_buttons_are_ledgered_per_button() {
    let rig = rig();
    let recorder = Recorder::suppressing();
    let filter =
        InputFilter::new(KindSet::of(InputEventKind::MouseDown).with(InputEventKind::MouseUp))
            .with_buttons([MouseButton(2)]);
    let _sub = handler(&rig, &recorder, filter, 1);
    assert_eq!(
        sup(&rig, button(InputEventKind::MouseDown, 3)),
        Disposition::Pass
    );
    assert_eq!(
        sup(&rig, button(InputEventKind::MouseDown, 2)),
        Disposition::Suppress
    );
    assert_eq!(sup(&rig, button(InputEventKind::MouseUp, 3)), Disposition::Pass);
    assert_eq!(
        sup(&rig, button(InputEventKind::MouseUp, 2)),
        Disposition::Suppress
    );
    assert_eq!(recorder.count(), 2);
}

#[test]
fn kb_02_04_ledger_exhaustion_boundary_is_exactly_1024_and_fails_open() {
    let rig = rig();
    let recorder = Recorder::suppressing();
    let _sub = handler(&rig, &recorder, any_key(), 1);
    for code in 0..PRESS_LEDGER_CAPACITY {
        let code = KeyCode(u16::try_from(code).expect("fits"));
        assert_eq!(sup(&rig, down(code)), Disposition::Suppress, "entry {code:?}");
    }
    assert_eq!(rig.service.ledger_len(), PRESS_LEDGER_CAPACITY);
    let overflow = KeyCode(5_000);
    assert_eq!(sup(&rig, down(overflow)), Disposition::Pass);
    assert_eq!(rig.service.counters().ledger_exhausted, 1);
    assert_eq!(
        recorder.count(),
        PRESS_LEDGER_CAPACITY,
        "exhausted press never reaches the handler"
    );
    // No eviction: the overflow press's up passes and an existing up still matches.
    assert_eq!(sup(&rig, up(overflow)), Disposition::Pass);
    assert_eq!(sup(&rig, up(KeyCode(0))), Disposition::Suppress);
    assert_eq!(rig.service.ledger_len(), PRESS_LEDGER_CAPACITY - 1);
    // The freed slot is usable again.
    assert_eq!(sup(&rig, down(overflow)), Disposition::Suppress);
}

#[test]
fn kb_02_04_tap_disabled_cancels_presses_advances_epoch_and_never_synthesizes_releases() {
    let rig = rig();
    let recorder = Recorder::suppressing();
    let _sub = handler(&rig, &recorder, any_key(), 1);
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
    assert_eq!(sup(&rig, down(KeyCode(1))), Disposition::Suppress);
    let before = rig.service.epoch();
    rig.service.tap_disabled(TapKind::Suppressing);
    assert_eq!(recorder.cancels.load(Ordering::SeqCst), 2);
    assert!(rig.service.epoch() > before);
    assert_eq!(rig.service.ledger_len(), 0);
    assert_eq!(recorder.count(), 2, "no synthetic release reached the handler");
    assert_eq!(
        sup(&rig, up(A)),
        Disposition::Pass,
        "the discontinuity makes the up an orphan"
    );
    assert_eq!(rig.service.counters().discontinuities, 1);
    assert!(rig.service.diagnostics().iter().any(|d| matches!(
        d,
        InputDiagnostic::Discontinuity { tap: TapKind::Suppressing, .. }
    )));
    assert_eq!(rig.service.tap_status(TapKind::Suppressing), TapStatus::Ready);
    // New presses carry the new epoch.
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
}

#[test]
fn kb_02_04_reenable_failure_is_visible_and_retry_is_explicit_and_bounded() {
    let rig = rig();
    let recorder = Recorder::suppressing();
    let _sub = handler(&rig, &recorder, any_key(), 1);
    rig.backend.set_default(FakeResponse::Fail("denied".to_owned()));
    rig.service.tap_disabled(TapKind::Suppressing);
    assert!(matches!(
        rig.service.tap_status(TapKind::Suppressing),
        TapStatus::ReenableFailed {
            attempts: 1,
            cause: FailureCause::Failed(_)
        }
    ));
    // Subscribing now would be dishonest.
    assert!(matches!(
        rig.service
            .subscribe_handler(
                spec(any_key(), 1),
                Recorder::passing() as Arc<dyn InputHandler>,
                &live()
            )
            .err(),
        Some(InputError::TapNotReady { .. })
    ));
    for _ in 0..MAX_RETRY_ATTEMPTS - 1 {
        assert!(matches!(
            rig.service.retry_reenable(TapKind::Suppressing, &live()),
            Err(InputError::Native {
                action: ControlAction::Enable,
                ..
            })
        ));
    }
    assert_eq!(
        rig.service.retry_reenable(TapKind::Suppressing, &live()),
        Err(InputError::RetryExhausted { tap: TapKind::Suppressing })
    );
}

#[test]
fn kb_02_04_reenable_retry_can_recover() {
    let rig = rig();
    let _sub = handler(&rig, &Recorder::suppressing(), any_key(), 1);
    rig.backend.script(
        TapKind::Suppressing,
        ControlAction::Enable,
        FakeResponse::Fail("x".to_owned()),
    );
    rig.service.tap_disabled(TapKind::Suppressing);
    assert!(matches!(
        rig.service.tap_status(TapKind::Suppressing),
        TapStatus::ReenableFailed { .. }
    ));
    assert_eq!(
        rig.service.retry_reenable(TapKind::Suppressing, &live()),
        Ok(TapStatus::Ready)
    );
    assert_eq!(rig.service.tap_status(TapKind::Suppressing), TapStatus::Ready);
}

#[test]
fn kb_02_04_pending_reenable_stays_disabled_until_acknowledged() {
    let rig = rig();
    let _sub = handler(&rig, &Recorder::suppressing(), any_key(), 1);
    rig.backend
        .script(TapKind::Suppressing, ControlAction::Enable, FakeResponse::Hold);
    rig.service.tap_disabled(TapKind::Suppressing);
    assert_eq!(rig.service.tap_status(TapKind::Suppressing), TapStatus::Disabled);
    assert!(rig.backend.process_next(&FakeResponse::Ack));
    assert_eq!(rig.service.tap_status(TapKind::Suppressing), TapStatus::Ready);
}

#[test]
fn kb_02_04_disabled_tap_races_dispatch_without_inconsistency() {
    let rig = rig();
    let recorder = Recorder::suppressing();
    let _sub = handler(&rig, &recorder, any_key(), 1);
    let barrier = Barrier::new(2);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            barrier.wait();
            for n in 0..300u16 {
                let code = KeyCode(n % 8);
                let _ = sup(&rig, down(code));
                let _ = sup(&rig, up(code));
            }
        });
        barrier.wait();
        for _ in 0..30 {
            rig.service.tap_disabled(TapKind::Suppressing);
        }
    });
    rig.service.tap_disabled(TapKind::Suppressing);
    assert_eq!(rig.service.ledger_len(), 0);
    assert_eq!(rig.service.tap_status(TapKind::Suppressing), TapStatus::Ready);
}

// ---------------------------------------------------------------- KB-02-05

#[test]
fn kb_02_05_panic_is_contained_fails_open_and_makes_subscriber_inert() {
    let rig = rig();
    let panicking = Recorder::suppressing();
    let sub = handler(&rig, &panicking, any_key(), 9);
    let fallback = Recorder::passing();
    let _fallback = handler(&rig, &fallback, any_key(), 1);
    panicking.panic_on_next.store(true, Ordering::SeqCst);
    assert_eq!(sup(&rig, down(A)), Disposition::Pass);
    assert_eq!(sub.inert_reason(), Some(InertReason::Panicked));
    assert_eq!(rig.service.counters().panics, 1);
    assert_eq!(
        fallback.count(),
        1,
        "lower handlers still see the failed-open press"
    );
    assert_eq!(sup(&rig, down(KeyCode(1))), Disposition::Pass);
    assert_eq!(panicking.count(), 0);
}

#[test]
fn kb_02_05_panic_on_release_keeps_ledger_cleanup() {
    let rig = rig();
    let recorder = Recorder::suppressing();
    let _sub = handler(&rig, &recorder, any_key(), 1);
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
    recorder.panic_on_next.store(true, Ordering::SeqCst);
    assert_eq!(sup(&rig, up(A)), Disposition::Suppress);
    assert_eq!(rig.service.ledger_len(), 0);
    assert!(
        rig.service
            .diagnostics()
            .iter()
            .any(|d| matches!(d, InputDiagnostic::CallbackPanicked { .. }))
    );
}

#[test]
fn kb_02_05_slow_returned_callback_inhibits_future_delivery() {
    let rig = rig();
    let recorder = Recorder::suppressing();
    *recorder.advance.lock().unwrap_or_else(PoisonError::into_inner) = Some((
        Arc::clone(&rig.clock),
        CALLBACK_SOFT_TARGET + Duration::from_micros(1),
    ));
    let sub = handler(&rig, &recorder, any_key(), 1);
    // The returned decision is honored; delivery stops afterwards.
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
    assert_eq!(sub.inert_reason(), Some(InertReason::Slow));
    assert_eq!(rig.service.counters().slow_callbacks, 1);
    assert!(rig.service.diagnostics().iter().any(|d| matches!(d, InputDiagnostic::CallbackSlow { elapsed, .. } if *elapsed > CALLBACK_SOFT_TARGET)));
    assert_eq!(sup(&rig, up(A)), Disposition::Suppress);
    assert_eq!(sup(&rig, down(KeyCode(1))), Disposition::Pass);
    assert_eq!(recorder.count(), 1);
}

#[test]
fn kb_02_05_callback_at_exactly_the_target_is_not_slow() {
    let rig = rig();
    let recorder = Recorder::passing();
    *recorder.advance.lock().unwrap_or_else(PoisonError::into_inner) =
        Some((Arc::clone(&rig.clock), CALLBACK_SOFT_TARGET));
    let sub = handler(&rig, &recorder, any_key(), 1);
    let _ = sup(&rig, down(A));
    assert_eq!(sub.inert_reason(), None);
}

#[test]
fn kb_02_05_acknowledgement_uses_two_seconds_and_ten_millisecond_quanta() {
    assert_eq!(ACKNOWLEDGEMENT_BUDGET, Duration::from_secs(2));
    assert_eq!(CANCELLATION_QUANTUM, Duration::from_millis(10));
    // Acknowledged inside the budget.
    let rig = rig();
    rig.backend.script(
        TapKind::Suppressing,
        ControlAction::Install,
        FakeResponse::AckAfter(Duration::from_millis(1500)),
    );
    let started = rig.clock.now();
    let sub = handler(&rig, &Recorder::passing(), any_key(), 1);
    let elapsed = rig.clock.elapsed_since(started);
    assert!(
        elapsed >= Duration::from_millis(1500) && elapsed < Duration::from_millis(1520),
        "{elapsed:?}"
    );
    assert!(rig.clock.waits().iter().all(|w| *w <= CANCELLATION_QUANTUM));
    drop(sub);
    // Too late: times out at the budget, within one quantum.
    let rig = rig_for_late();
    let started = rig.clock.now();
    let result = rig.service.subscribe_handler(
        spec(any_key(), 1),
        Recorder::passing() as Arc<dyn InputHandler>,
        &live(),
    );
    assert!(matches!(
        result.err(),
        Some(InputError::Native {
            cause: FailureCause::TimedOut,
            ..
        })
    ));
    let elapsed = rig.clock.elapsed_since(started);
    assert!(
        elapsed >= ACKNOWLEDGEMENT_BUDGET
            && elapsed <= ACKNOWLEDGEMENT_BUDGET + CANCELLATION_QUANTUM,
        "{elapsed:?}"
    );
    assert!(rig.clock.waits().iter().all(|w| *w <= CANCELLATION_QUANTUM));
    // The late acknowledgement never installs.
    rig.clock.advance(Duration::from_secs(5));
    assert_eq!(
        rig.backend.count(TapKind::Suppressing, ControlAction::Install),
        0
    );
}

fn rig_for_late() -> Rig {
    let rig = rig();
    rig.backend.script(
        TapKind::Suppressing,
        ControlAction::Install,
        FakeResponse::AckAfter(Duration::from_millis(2500)),
    );
    rig
}

#[test]
fn kb_02_05_unsubscribe_invalidates_before_slow_native_cleanup() {
    let rig = rig();
    let recorder = Recorder::suppressing();
    let sub = handler(&rig, &recorder, any_key(), 1);
    rig.backend
        .script(TapKind::Suppressing, ControlAction::Remove, FakeResponse::Hold);
    let report = sub.unsubscribe();
    assert!(matches!(
        report.native,
        NativeCleanup::Retained(Retained {
            cause: FailureCause::TimedOut,
            ..
        })
    ));
    assert_eq!(sup(&rig, down(A)), Disposition::Pass);
    assert_eq!(recorder.count(), 0);
    assert!(
        rig.backend.is_live(TapKind::Suppressing),
        "resources are retained, not freed early"
    );
}

// ---------------------------------------------------------------- KB-02-06

#[test]
fn kb_02_06_passive_only_creates_exactly_one_listen_only_tap() {
    let rig = rig();
    let first = Arc::new(Watcher::default());
    let second = Arc::new(Watcher::default());
    let one = observer(&rig, &first, any_key());
    let two = observer(&rig, &second, any_key());
    assert_eq!(rig.backend.count(TapKind::ListenOnly, ControlAction::Install), 1);
    assert_eq!(
        rig.backend.count(TapKind::Suppressing, ControlAction::Install),
        0
    );
    assert_eq!(rig.backend.count(TapKind::Suppressing, ControlAction::Enable), 0);
    assert!(rig.backend.is_live(TapKind::ListenOnly));
    assert!(!rig.backend.is_live(TapKind::Suppressing));
    drop(one);
    assert!(rig.backend.is_live(TapKind::ListenOnly));
    assert_eq!(two.unsubscribe().native, NativeCleanup::Closed);
    assert!(!rig.backend.is_live(TapKind::ListenOnly));
    assert_eq!(rig.backend.count(TapKind::Suppressing, ControlAction::Remove), 0);
}

#[test]
fn kb_02_06_listen_only_closes_while_suppressing_stays_live() {
    let rig = rig();
    let watcher = Arc::new(Watcher::default());
    let recorder = Recorder::suppressing();
    let passive = observer(&rig, &watcher, any_key());
    let _active = handler(&rig, &recorder, any_key(), 1);
    assert!(rig.backend.is_live(TapKind::ListenOnly) && rig.backend.is_live(TapKind::Suppressing));
    assert_eq!(passive.unsubscribe().native, NativeCleanup::Closed);
    assert!(!rig.backend.is_live(TapKind::ListenOnly));
    assert!(rig.backend.is_live(TapKind::Suppressing));
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
}

#[test]
fn kb_02_06_passive_cannot_suppress_reorder_or_be_called_by_the_suppressing_tap() {
    let rig = rig();
    let watcher = Arc::new(Watcher::default());
    let recorder = Recorder::suppressing();
    let _passive = observer(&rig, &watcher, any_key());
    let _active = handler(&rig, &recorder, any_key(), 1);
    // The listen-only entry point always passes and never reaches handlers.
    assert_eq!(
        rig.service.dispatch(TapKind::ListenOnly, down(A)),
        Disposition::Pass
    );
    assert_eq!(recorder.count(), 0);
    assert_eq!(
        rig.service.ledger_len(),
        0,
        "passive delivery creates no ledger entry"
    );
    assert_eq!(
        watcher.seen.lock().unwrap_or_else(PoisonError::into_inner).len(),
        1
    );
    // The suppressing tap never reaches observers.
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
    assert_eq!(
        watcher.seen.lock().unwrap_or_else(PoisonError::into_inner).len(),
        1
    );
    // Observers register no HID claim.
    assert_eq!(rig.service.reservations().len(), 1);
}

#[test]
fn kb_02_06_observer_filters_modifier_release_and_discontinuity() {
    let rig = rig();
    let watcher = Arc::new(Watcher::default());
    let filter = InputFilter::new(KindSet::of(InputEventKind::FlagsChanged)).with_keys([SHIFT_KEY]);
    let _sub = observer(&rig, &watcher, filter);
    let flags = |f| {
        raw(
            InputEventKind::FlagsChanged,
            PressStream::Key(SHIFT_KEY),
            f,
            false,
        )
    };
    let _ = rig.service.dispatch(TapKind::ListenOnly, flags(ModifierFlags::SHIFT));
    let _ = rig.service.dispatch(TapKind::ListenOnly, flags(ModifierFlags::NONE));
    let _ = rig.service.dispatch(TapKind::ListenOnly, down(A));
    assert_eq!(
        watcher.seen.lock().unwrap_or_else(PoisonError::into_inner).len(),
        2
    );
    rig.service.tap_disabled(TapKind::ListenOnly);
    assert_eq!(watcher.gaps.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------- KB-02-07

#[test]
fn kb_02_07_work_fake_rejects_on_full_without_blocking() {
    let fake = CapacityWork::new(2);
    let owner: Arc<dyn WorkReservations> = Arc::clone(&fake) as Arc<dyn WorkReservations>;
    let a = WorkReservation::try_new(&owner).expect("first");
    let _b = WorkReservation::try_new(&owner).expect("second");
    assert_eq!(WorkReservation::try_new(&owner).err(), Some(WorkRejected));
    assert_eq!(fake.rejected(), 1);
    drop(a);
    assert!(WorkReservation::try_new(&owner).is_ok());
    let none: Arc<dyn WorkReservations> = Arc::new(NoWork);
    assert!(WorkReservation::try_new(&none).is_err());
}

#[test]
fn kb_05_02_redeem_consumes_without_release_and_refuses_a_foreign_owner() {
    let fake = CapacityWork::new(2);
    let owner: Arc<dyn WorkReservations> = Arc::clone(&fake) as Arc<dyn WorkReservations>;
    let other: Arc<dyn WorkReservations> = CapacityWork::new(1);
    let reservation = WorkReservation::try_new(&owner).expect("reserve");
    assert!(reservation.is_owned_by(&owner));
    assert!(!reservation.is_owned_by(&other));
    // A foreign owner gets the reservation back, still holding its slot.
    let reservation = reservation.redeem(&other).expect_err("foreign");
    assert_eq!(fake.used(), 1);
    // Redeeming hands the slot to the owner's own accounting: no release runs.
    reservation.redeem(&owner).expect("owned");
    assert_eq!(fake.used(), 1, "redeem must not release");
}

#[test]
fn kb_02_07_work_fake_never_exceeds_capacity_under_contention() {
    let fake = CapacityWork::new(4);
    let owner: Arc<dyn WorkReservations> = Arc::clone(&fake) as Arc<dyn WorkReservations>;
    let peak = AtomicU64::new(0);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..200 {
                    if let Ok(reservation) = WorkReservation::try_new(&owner) {
                        peak.fetch_max(fake.used() as u64, Ordering::SeqCst);
                        drop(reservation);
                    }
                }
            });
        }
    });
    assert!(peak.load(Ordering::SeqCst) <= 4);
    assert_eq!(fake.used(), 0);
}

// ---------------------------------------------------------------- HQ-04-09

/// Waits for the helper thread's acknowledged removal. The callback only flags
/// it, so completion is asynchronous.
fn settle_removed(rig: &Rig) {
    for _ in 0..5_000 {
        if rig.service.tap_status(TapKind::Suppressing) == TapStatus::Absent {
            assert!(!rig.backend.is_live(TapKind::Suppressing));
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("deferred removal never completed");
}

/// Gives a wrongly scheduled removal time to show up before asserting it did not.
fn assert_still_installed(rig: &Rig) {
    std::thread::sleep(Duration::from_millis(30));
    assert!(rig.backend.is_live(TapKind::Suppressing));
    assert_eq!(rig.service.tap_status(TapKind::Suppressing), TapStatus::Ready);
    assert_eq!(rig.backend.count(TapKind::Suppressing, ControlAction::Remove), 0);
}

#[test]
fn hq_04_09_last_handler_mid_press_keeps_tap_until_up_then_removes_acknowledged() {
    let rig = rig();
    let recorder = Recorder::suppressing();
    let sub = handler(&rig, &recorder, any_key(), 1);
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
    assert_eq!(sub.unsubscribe().native, NativeCleanup::Deferred);
    assert_eq!(sup(&rig, repeat(A)), Disposition::Suppress);
    assert_eq!(sup(&rig, repeat(A)), Disposition::Suppress);
    assert_still_installed(&rig);
    assert_eq!(sup(&rig, up(A)), Disposition::Suppress);
    settle_removed(&rig);
    assert_eq!(rig.backend.count(TapKind::Suppressing, ControlAction::Remove), 1);
    assert_eq!(recorder.count(), 1, "the inert handler never sees the rest");
}

#[test]
fn hq_04_09_removal_waits_for_every_tombstone() {
    let rig = rig();
    let sub = handler(&rig, &Recorder::suppressing(), any_key(), 1);
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
    assert_eq!(sup(&rig, down(SHIFT_KEY)), Disposition::Suppress);
    assert_eq!(sub.unsubscribe().native, NativeCleanup::Deferred);
    assert_eq!(sup(&rig, up(A)), Disposition::Suppress);
    assert_still_installed(&rig);
    assert_eq!(sup(&rig, up(SHIFT_KEY)), Disposition::Suppress);
    settle_removed(&rig);
}

#[test]
fn hq_04_09_new_first_down_passes_while_deferred() {
    let rig = rig();
    let recorder = Recorder::suppressing();
    let sub = handler(&rig, &recorder, any_key(), 1);
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
    assert_eq!(sub.unsubscribe().native, NativeCleanup::Deferred);
    assert_eq!(sup(&rig, down(SHIFT_KEY)), Disposition::Pass);
    assert_eq!(sup(&rig, up(SHIFT_KEY)), Disposition::Pass);
    assert_eq!(recorder.count(), 1);
    assert_still_installed(&rig);
    assert_eq!(sup(&rig, up(A)), Disposition::Suppress);
    settle_removed(&rig);
}

#[test]
fn hq_04_09_tap_disable_drains_ledger_and_removes_without_reenable() {
    // The helper is held so the deferral is still pending when the disable checks.
    let (rig, hold) = deferred_rig();
    rig.service.tap_disabled(TapKind::Suppressing);
    hold.release();
    settle_removed(&rig);
    assert_eq!(rig.service.ledger_len(), 0);
    // Orphans pass after the drain, and no re-enable raced the removal.
    assert_eq!(sup(&rig, up(A)), Disposition::Pass);
    assert_eq!(rig.backend.count(TapKind::Suppressing, ControlAction::Enable), 1);
}

#[test]
fn hq_04_09_owner_shutdown_hands_removal_to_the_owner() {
    let rig = rig();
    let sub = handler(&rig, &Recorder::suppressing(), any_key(), 1);
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
    assert_eq!(sub.unsubscribe().native, NativeCleanup::Deferred);
    assert_still_installed(&rig);
    rig.service.begin_owner_shutdown();
    assert!(!rig.backend.is_live(TapKind::Suppressing));
    assert_eq!(rig.backend.count(TapKind::Suppressing, ControlAction::Remove), 1);
    assert_eq!(rig.service.tap_status(TapKind::Suppressing), TapStatus::Absent);
    // Idempotent, and the helper never removes a second time.
    rig.service.begin_owner_shutdown();
    assert_eq!(sup(&rig, up(A)), Disposition::Suppress);
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(rig.backend.count(TapKind::Suppressing, ControlAction::Remove), 1);
}

#[test]
fn hq_04_09_carbon_yield_is_restored_immediately_on_unsubscribe() {
    let rig = rig();
    let registry = rig.service.reservations();
    let owner = registry.new_owner();
    let chord = Chord::new(A, ModifierFlags::COMMAND);
    let carbon = registry
        .register(
            owner,
            ReservationBackend::Carbon,
            Claim::Chord(chord),
            InputPriority { level: 5, tie_break: 0 },
        )
        .expect("carbon");
    let filter = any_key()
        .with_keys([A])
        .with_modifiers(ModifierMatch::Exact(ModifierFlags::COMMAND));
    let sub = handler(&rig, &Recorder::suppressing(), filter, 9);
    assert!(!registry.dispatchable(carbon));
    let command = raw(
        InputEventKind::KeyDown,
        PressStream::Key(A),
        ModifierFlags::COMMAND,
        false,
    );
    assert_eq!(sup(&rig, command), Disposition::Suppress);
    assert_eq!(sub.unsubscribe().native, NativeCleanup::Deferred);
    assert!(
        registry.dispatchable(carbon),
        "yield restored while the tap is deferred"
    );
    assert_eq!(sup(&rig, up(A)), Disposition::Suppress);
    settle_removed(&rig);
}

#[test]
fn hq_04_09_resubscribe_during_deferral_reuses_the_tap() {
    let rig = rig();
    let first = Recorder::suppressing();
    let sub = handler(&rig, &first, any_key(), 1);
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
    assert_eq!(sub.unsubscribe().native, NativeCleanup::Deferred);
    let second = Recorder::suppressing();
    let again = handler(&rig, &second, any_key(), 1);
    assert_eq!(
        rig.backend.count(TapKind::Suppressing, ControlAction::Install),
        1
    );
    assert_eq!(sup(&rig, repeat(A)), Disposition::Suppress);
    assert_eq!(sup(&rig, up(A)), Disposition::Suppress);
    assert_eq!(second.count(), 0, "the old press stays with its tombstone");
    assert_still_installed(&rig);
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
    assert_eq!(second.count(), 1);
    // The cancelled deferral stays cancelled: a later last unsubscribe is plain.
    assert_eq!(sup(&rig, up(A)), Disposition::Suppress);
    assert_eq!(again.unsubscribe().native, NativeCleanup::Closed);
    assert_eq!(rig.backend.count(TapKind::Suppressing, ControlAction::Remove), 1);
}

/// A rig whose last suppressing handler left while a press is still held, with
/// the deferral helper parked before it takes the control lock.
fn deferred_rig() -> (Rig, crate::input::testing::DeferralHold) {
    let rig = rig();
    let hold = rig.service.hold_deferral_helper();
    let sub = handler(&rig, &Recorder::suppressing(), any_key(), 1);
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
    assert_eq!(sub.unsubscribe().native, NativeCleanup::Deferred);
    (rig, hold)
}

fn subscribe_again(rig: &Rig) -> Result<Subscription, InputError> {
    rig.service.subscribe_handler(
        spec(any_key(), 1),
        Recorder::suppressing() as Arc<dyn InputHandler>,
        &live(),
    )
}

#[test]
fn hq_04_09_subscribe_after_disable_during_deferral_reenables_the_reused_tap() {
    let (rig, hold) = deferred_rig();
    rig.service.tap_disabled(TapKind::Suppressing);
    assert_eq!(
        rig.service.tap_status(TapKind::Suppressing),
        TapStatus::Disabled,
        "a skipped re-enable must not leave the status Ready"
    );
    assert_eq!(rig.backend.count(TapKind::Suppressing, ControlAction::Enable), 1);
    let again = subscribe_again(&rig).expect("revive");
    assert_eq!(rig.backend.count(TapKind::Suppressing, ControlAction::Enable), 2);
    assert_eq!(rig.backend.count(TapKind::Suppressing, ControlAction::Remove), 0);
    assert_eq!(rig.service.tap_status(TapKind::Suppressing), TapStatus::Ready);
    // The cancelled deferral makes the released helper a no-op.
    hold.release();
    assert_still_installed(&rig);
    assert_eq!(rig.backend.count(TapKind::Suppressing, ControlAction::Enable), 2);
    drop(again);
}

#[test]
fn hq_04_09_subscribe_after_the_helper_removed_the_disabled_tap_installs_afresh() {
    let rig = rig();
    let sub = handler(&rig, &Recorder::suppressing(), any_key(), 1);
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
    assert_eq!(sub.unsubscribe().native, NativeCleanup::Deferred);
    // The helper wins: the press ends, the tap is removed, then the OS disable lands.
    assert_eq!(sup(&rig, up(A)), Disposition::Suppress);
    settle_removed(&rig);
    rig.service.tap_disabled(TapKind::Suppressing);
    assert_eq!(rig.service.tap_status(TapKind::Suppressing), TapStatus::Absent);
    let again = subscribe_again(&rig).expect("fresh install");
    assert_eq!(
        rig.backend.count(TapKind::Suppressing, ControlAction::Install),
        2
    );
    assert!(rig.backend.is_live(TapKind::Suppressing));
    assert_eq!(rig.service.tap_status(TapKind::Suppressing), TapStatus::Ready);
    drop(again);
}

#[test]
fn hq_04_09_failed_revive_fails_the_subscribe_and_the_helper_still_removes_the_tap() {
    let (rig, hold) = deferred_rig();
    rig.service.tap_disabled(TapKind::Suppressing);
    rig.backend.script(
        TapKind::Suppressing,
        ControlAction::Enable,
        FakeResponse::Fail("denied".into()),
    );
    let error = subscribe_again(&rig).map(drop).expect_err("revive fails");
    assert!(matches!(error, InputError::Native {
        action: ControlAction::Enable,
        ..
    }));
    assert!(matches!(
        rig.service.tap_status(TapKind::Suppressing),
        TapStatus::ReenableFailed { attempts: 1, .. }
    ));
    hold.release();
    settle_removed(&rig);
    assert_eq!(rig.backend.count(TapKind::Suppressing, ControlAction::Remove), 1);
    assert!(!rig.backend.is_live(TapKind::Suppressing));
}

#[test]
fn hq_04_09_disable_during_the_revive_enable_is_not_lost() {
    let (rig, hold) = deferred_rig();
    rig.service.tap_disabled(TapKind::Suppressing);
    let service = Arc::clone(&rig.service);
    rig.backend.on_next_enable(move || service.tap_disabled(TapKind::Suppressing));
    let again = subscribe_again(&rig).expect("revive");
    // install + revive + the re-enable the intervening disable submitted.
    assert_eq!(rig.backend.count(TapKind::Suppressing, ControlAction::Enable), 3);
    assert_eq!(rig.service.tap_status(TapKind::Suppressing), TapStatus::Ready);
    hold.release();
    assert_still_installed(&rig);
    drop(again);
}

#[test]
fn hq_04_09_unacknowledged_disable_during_the_revive_enable_keeps_the_status_honest() {
    let (rig, hold) = deferred_rig();
    rig.service.tap_disabled(TapKind::Suppressing);
    let service = Arc::clone(&rig.service);
    let backend = Arc::clone(&rig.backend);
    rig.backend.on_next_enable(move || {
        backend.script(TapKind::Suppressing, ControlAction::Enable, FakeResponse::Hold);
        service.tap_disabled(TapKind::Suppressing);
    });
    let error = subscribe_again(&rig).map(drop).expect_err("tap is disabled again");
    assert!(matches!(error, InputError::TapNotReady { .. }));
    assert_eq!(
        rig.service.tap_status(TapKind::Suppressing),
        TapStatus::Disabled,
        "revive must not overwrite the newer disable with Ready"
    );
    assert_eq!(rig.backend.queued(), 1, "the disable's own re-enable is pending");
    assert!(rig.backend.process_next(&FakeResponse::Ack));
    assert_eq!(rig.service.tap_status(TapKind::Suppressing), TapStatus::Ready);
    hold.release();
    settle_removed(&rig);
}

#[test]
fn hq_04_09_without_tombstones_removal_is_immediate() {
    let passing_rig = rig();
    let rig = rig();
    let sub = handler(&rig, &Recorder::suppressing(), any_key(), 1);
    assert_eq!(sup(&rig, down(A)), Disposition::Suppress);
    assert_eq!(sup(&rig, up(A)), Disposition::Suppress);
    assert_eq!(sub.unsubscribe().native, NativeCleanup::Closed);
    assert!(!rig.backend.is_live(TapKind::Suppressing));
    // A passed press leaves no suppressed tombstone either.
    let sub = handler(&passing_rig, &Recorder::passing(), any_key(), 1);
    assert_eq!(sup(&passing_rig, down(A)), Disposition::Pass);
    assert_eq!(sub.unsubscribe().native, NativeCleanup::Closed);
}
