use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use objc2_core_graphics::{CGEventFlags, CGEventType};
use stache_sdk::CancellationToken;
use stache_sdk::input::{
    InputEvent, InputEventKind, InputFilter, InputObserver, InputService, KeyCode, KindSet,
    ModifierFlags, MouseButton, NativeCleanup, NoWork, ObserverSpec, PressStream, RawInput,
    TapKind, TapStatus,
};

use super::MacInputBackend;
use super::convert::{Classified, EventFields, classify, event_mask, modifier_flags};
use super::owner::OWNER_THREAD_NAME;
use super::system::SystemInput;
use super::testing::{Call, FakeLog, fake_service};

const WAIT: Duration = Duration::from_secs(5);

struct Quiet;

impl InputObserver for Quiet {
    fn on_event(&self, _event: &InputEvent) {}
}

fn observer_spec() -> ObserverSpec {
    ObserverSpec {
        filter: InputFilter::new(KindSet::all()),
    }
}

// ---- confinement and composition ----

trait AmbiguousIfSend<A> {
    fn probe() {}
}
impl<T: ?Sized> AmbiguousIfSend<()> for T {}
impl<T: ?Sized + Send> AmbiguousIfSend<u8> for T {}

// Fails to compile (ambiguous impl) if the real native layer ever became `Send`.
const _: fn() = || {
    let _ = <SystemInput as AmbiguousIfSend<_>>::probe;
};

#[test]
fn constructing_the_real_backend_starts_no_thread_and_no_tap() {
    let (service, backend) = MacInputBackend::service(Arc::new(NoWork));
    assert!(service.is_available());
    assert!(
        !backend.owner.started(),
        "the owner thread starts on the first request"
    );
    assert_eq!(backend.owner.submitted(), 0);
    assert_eq!(service.tap_status(TapKind::Suppressing), TapStatus::Absent);
    assert_eq!(service.tap_status(TapKind::ListenOnly), TapStatus::Absent);
    // Never started: shutdown completes at once with no native object.
    let (sender, receiver) = mpsc::channel();
    backend.shutdown(move |result| sender.send(result).unwrap());
    assert_eq!(receiver.recv_timeout(WAIT).unwrap(), Ok(()));
    assert!(backend.join(WAIT));
}

#[test]
fn the_fake_backend_is_lazy_and_works_off_the_main_thread() {
    let log = FakeLog::new();
    let (service, backend) = fake_service(&log);
    assert!(!backend.owner.started());
    assert!(!log.created());

    let subscription = service
        .subscribe_observer(observer_spec(), Arc::new(Quiet), &CancellationToken::new())
        .unwrap();
    assert_eq!(log.calls(), vec![
        Call::Install(TapKind::ListenOnly),
        Call::Enable(TapKind::ListenOnly)
    ]);
    assert_eq!(service.tap_status(TapKind::ListenOnly), TapStatus::Ready);
    assert_eq!(service.tap_status(TapKind::Suppressing), TapStatus::Absent);
    // Every native call ran on the one named owner thread, never on the caller
    // (the test thread stands in for any caller, including the main thread).
    let names = log.thread_names();
    assert_eq!(names.len(), 1);
    assert!(names.contains(OWNER_THREAD_NAME));
    assert!(!log.thread_ids().contains(&thread::current().id()));

    let report = subscription.unsubscribe();
    assert_eq!(report.native, NativeCleanup::Closed);
    assert!(!log.is_live(TapKind::ListenOnly));
}

// ---- cancellation ----

#[test]
fn a_cancelled_queued_install_never_mutates_native_state() {
    let log = FakeLog::new();
    // The owner is held inside its factory, so the install stays queued.
    log.close_factory();
    let (service, backend) = fake_service(&log);
    let token = CancellationToken::new();
    let worker = {
        let service = Arc::clone(&service);
        let token = token.clone();
        thread::spawn(move || service.subscribe_observer(observer_spec(), Arc::new(Quiet), &token))
    };
    let deadline = std::time::Instant::now() + WAIT;
    while backend.owner.submitted() == 0 {
        assert!(std::time::Instant::now() < deadline, "install was never queued");
        thread::sleep(Duration::from_millis(1));
    }
    token.cancel();
    // Release the owner only after the caller gave up; the late install must
    // recheck cancellation and skip the mutation.
    thread::sleep(Duration::from_millis(50));
    log.open_factory();
    assert!(worker.join().unwrap().is_err());
    assert!(log.wait_until(|calls| calls.contains(&Call::Remove(TapKind::ListenOnly))));
    let calls = log.calls();
    assert!(
        !calls.iter().any(|call| matches!(call, Call::Install(_) | Call::Enable(_))),
        "a cancelled install must not mutate: {calls:?}"
    );
    assert!(!log.is_live(TapKind::ListenOnly));
}

#[test]
fn a_partial_install_is_cleaned_by_remove() {
    let log = FakeLog::new();
    log.fail_install_partially();
    let (service, _backend) = fake_service(&log);
    let error = service
        .subscribe_observer(observer_spec(), Arc::new(Quiet), &CancellationToken::new())
        .unwrap_err();
    assert!(
        error.to_string().contains("injected partial install failure"),
        "{error}"
    );
    assert!(log.wait_until(|calls| calls.contains(&Call::Remove(TapKind::ListenOnly))));
    assert!(
        !log.has_partial(TapKind::ListenOnly),
        "remove must clean the partial tap"
    );
    assert!(!log.is_live(TapKind::ListenOnly));
}

#[test]
fn a_creation_failure_is_reported_with_its_message_and_nothing_remains() {
    let log = FakeLog::new();
    log.fail_install_cleanly();
    let (service, _backend) = fake_service(&log);
    let error = service
        .subscribe_observer(observer_spec(), Arc::new(Quiet), &CancellationToken::new())
        .unwrap_err();
    assert!(error.to_string().contains("injected create failure"), "{error}");
    assert!(!log.is_live(TapKind::ListenOnly));
}

#[test]
fn a_removal_failure_is_reported_and_retried() {
    let log = FakeLog::new();
    let (service, _backend) = fake_service(&log);
    let subscription = service
        .subscribe_observer(observer_spec(), Arc::new(Quiet), &CancellationToken::new())
        .unwrap();
    log.fail_next_removals(1);
    let report = subscription.unsubscribe();
    assert!(matches!(report.native, NativeCleanup::Retained(_)), "{report:?}");
    assert!(matches!(
        service.tap_status(TapKind::ListenOnly),
        TapStatus::Retained(_)
    ));
    assert!(
        log.is_live(TapKind::ListenOnly),
        "the failed removal left the tap owned"
    );
    service.retry_cleanup(TapKind::ListenOnly).unwrap();
    assert!(!log.is_live(TapKind::ListenOnly));
    assert_eq!(service.tap_status(TapKind::ListenOnly), TapStatus::Absent);
}

#[test]
fn a_disabled_tap_is_re_enabled_by_the_owner_without_blocking_the_caller() {
    let log = FakeLog::new();
    let (service, _backend) = fake_service(&log);
    let _subscription = service
        .subscribe_observer(observer_spec(), Arc::new(Quiet), &CancellationToken::new())
        .unwrap();
    service.tap_disabled(TapKind::ListenOnly);
    assert!(log.wait_until(|calls| {
        calls.iter().filter(|call| **call == Call::Enable(TapKind::ListenOnly)).count() == 2
    }));
    let deadline = std::time::Instant::now() + WAIT;
    while service.tap_status(TapKind::ListenOnly) != TapStatus::Ready {
        assert!(
            std::time::Instant::now() < deadline,
            "tap never became ready again"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

// ---- shutdown ----

#[test]
fn shutdown_removes_every_tap_destroys_native_state_and_joins_in_bounds() {
    let log = FakeLog::new();
    let (service, backend) = fake_service(&log);
    let _subscription = service
        .subscribe_observer(observer_spec(), Arc::new(Quiet), &CancellationToken::new())
        .unwrap();
    assert!(log.is_live(TapKind::ListenOnly));

    let (sender, receiver) = mpsc::channel();
    backend.shutdown(move |result| sender.send(result).unwrap());
    assert_eq!(receiver.recv_timeout(WAIT).unwrap(), Ok(()));
    assert!(log.dropped(), "native state is destroyed before the answer");
    assert!(!log.is_live(TapKind::ListenOnly));
    assert!(backend.join(WAIT), "the owner thread joins inside the bound");

    // A second shutdown answers immediately.
    let (again, receiver) = mpsc::channel();
    backend.shutdown(move |result| again.send(result).unwrap());
    assert_eq!(receiver.recv_timeout(WAIT).unwrap(), Ok(()));
}

#[test]
fn a_request_after_shutdown_is_refused() {
    let log = FakeLog::new();
    let (service, backend) = fake_service(&log);
    let (sender, receiver) = mpsc::channel();
    backend.shutdown(move |result| sender.send(result).unwrap());
    assert_eq!(receiver.recv_timeout(WAIT).unwrap(), Ok(()));
    let error = service
        .subscribe_observer(observer_spec(), Arc::new(Quiet), &CancellationToken::new())
        .unwrap_err();
    assert!(error.to_string().contains("Closed"), "{error}");
    assert!(!log.created(), "a closed owner never starts native state");
    assert_eq!(backend.owner.submitted(), 0);
}

#[test]
fn shutdown_reports_a_removal_failure_instead_of_claiming_success() {
    let log = FakeLog::new();
    let (service, backend) = fake_service(&log);
    let _subscription = service
        .subscribe_observer(observer_spec(), Arc::new(Quiet), &CancellationToken::new())
        .unwrap();
    log.fail_next_removals(2);
    let (sender, receiver) = mpsc::channel();
    backend.shutdown(move |result| sender.send(result).unwrap());
    let result = receiver.recv_timeout(WAIT).unwrap();
    assert!(result.unwrap_err().contains("injected removal failure"));
    assert!(backend.join(WAIT));
}

#[test]
fn dropping_the_service_and_backend_stops_the_owner() {
    let log = FakeLog::new();
    let (service, backend) = fake_service(&log);
    drop(
        service
            .subscribe_observer(observer_spec(), Arc::new(Quiet), &CancellationToken::new())
            .unwrap(),
    );
    drop(service);
    drop(backend);
    assert!(
        log.dropped(),
        "dropping the last owner reference joins the thread"
    );
    assert!(!log.is_live(TapKind::ListenOnly));
}

// ---- event conversion ----

fn fields(event_type: CGEventType) -> EventFields {
    EventFields {
        event_type: event_type.0,
        key_code: 0,
        autorepeat: 0,
        button: 0,
        flags: 0,
        timestamp_ns: 0,
    }
}

fn input(fields: EventFields) -> RawInput {
    match classify(fields) {
        Classified::Input(raw) => raw,
        other => panic!("expected input, got {other:?}"),
    }
}

#[test]
fn key_down_up_and_repeat_convert() {
    let down = input(EventFields {
        key_code: 12,
        flags: CGEventFlags::MaskCommand.0 | CGEventFlags::MaskShift.0,
        timestamp_ns: 1_500,
        ..fields(CGEventType::KeyDown)
    });
    assert_eq!(down.kind, InputEventKind::KeyDown);
    assert_eq!(down.stream, PressStream::Key(KeyCode(12)));
    assert_eq!(down.flags, ModifierFlags::COMMAND | ModifierFlags::SHIFT);
    assert!(!down.repeat);
    assert_eq!(down.timestamp, Duration::from_nanos(1_500));

    let repeat = input(EventFields {
        key_code: 12,
        autorepeat: 1,
        ..fields(CGEventType::KeyDown)
    });
    assert!(repeat.repeat);

    let up = input(EventFields {
        key_code: 12,
        autorepeat: 1,
        ..fields(CGEventType::KeyUp)
    });
    assert_eq!(up.kind, InputEventKind::KeyUp);
    assert!(!up.repeat, "only a key down can be a repeat");
}

#[test]
fn flags_changed_keeps_the_resulting_mask_for_modifier_release() {
    let press = input(EventFields {
        key_code: 55,
        flags: CGEventFlags::MaskCommand.0,
        ..fields(CGEventType::FlagsChanged)
    });
    assert_eq!(press.kind, InputEventKind::FlagsChanged);
    assert_eq!(press.stream, PressStream::Key(KeyCode(55)));
    assert_eq!(press.flags, ModifierFlags::COMMAND);
    let release = input(EventFields {
        key_code: 55,
        ..fields(CGEventType::FlagsChanged)
    });
    assert_eq!(release.flags, ModifierFlags::NONE);
}

#[test]
fn mouse_buttons_zero_one_two_and_three_stay_exact() {
    let cases = [
        (CGEventType::LeftMouseDown, CGEventType::LeftMouseUp, 0),
        (CGEventType::RightMouseDown, CGEventType::RightMouseUp, 1),
        (CGEventType::OtherMouseDown, CGEventType::OtherMouseUp, 2),
        (CGEventType::OtherMouseDown, CGEventType::OtherMouseUp, 3),
    ];
    for (down, up, button) in cases {
        let raw = input(EventFields { button, ..fields(down) });
        assert_eq!(raw.kind, InputEventKind::MouseDown);
        assert_eq!(
            raw.stream,
            PressStream::Button(MouseButton(button.try_into().unwrap()))
        );
        let raw = input(EventFields { button, ..fields(up) });
        assert_eq!(raw.kind, InputEventKind::MouseUp);
        assert_eq!(
            raw.stream,
            PressStream::Button(MouseButton(button.try_into().unwrap()))
        );
    }
}

#[test]
fn tap_disabled_unknown_and_out_of_range_values_never_become_input() {
    assert_eq!(
        classify(fields(CGEventType::TapDisabledByTimeout)),
        Classified::TapDisabled
    );
    assert_eq!(
        classify(fields(CGEventType::TapDisabledByUserInput)),
        Classified::TapDisabled
    );
    assert_eq!(classify(fields(CGEventType::MouseMoved)), Classified::Ignore);
    assert_eq!(
        classify(EventFields {
            key_code: -1,
            ..fields(CGEventType::KeyDown)
        }),
        Classified::Ignore
    );
    assert_eq!(
        classify(EventFields {
            key_code: 70_000,
            ..fields(CGEventType::KeyUp)
        }),
        Classified::Ignore
    );
    assert_eq!(
        classify(EventFields {
            button: -1,
            ..fields(CGEventType::OtherMouseDown)
        }),
        Classified::Ignore
    );
}

#[test]
fn modifier_flags_map_every_named_bit_and_drop_the_rest() {
    let all = CGEventFlags::MaskAlphaShift.0
        | CGEventFlags::MaskShift.0
        | CGEventFlags::MaskControl.0
        | CGEventFlags::MaskAlternate.0
        | CGEventFlags::MaskCommand.0
        | CGEventFlags::MaskSecondaryFn.0;
    let mapped = modifier_flags(all | CGEventFlags::MaskNumericPad.0 | 0xFF);
    for flag in [
        ModifierFlags::CAPS_LOCK,
        ModifierFlags::SHIFT,
        ModifierFlags::CONTROL,
        ModifierFlags::OPTION,
        ModifierFlags::COMMAND,
        ModifierFlags::FUNCTION,
    ] {
        assert!(mapped.contains(flag));
    }
    assert_eq!(modifier_flags(0), ModifierFlags::NONE);
}

#[test]
fn the_event_mask_covers_keys_flags_and_all_mouse_buttons_only() {
    let mask = event_mask();
    for kind in [
        CGEventType::KeyDown,
        CGEventType::KeyUp,
        CGEventType::FlagsChanged,
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGEventType::OtherMouseDown,
        CGEventType::OtherMouseUp,
    ] {
        assert_ne!(mask & (1 << kind.0), 0, "{kind:?}");
    }
    assert_eq!(mask.count_ones(), 9);
    assert_eq!(mask & (1 << CGEventType::MouseMoved.0), 0);
}

#[allow(dead_code)]
fn service_type_is_shareable(_: &InputService) {}
