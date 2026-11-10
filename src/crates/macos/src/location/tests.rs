use std::sync::Arc;
use std::thread;
use std::time::Duration;

use super::testing::FakeLocationSource;
use super::*;

const LATITUDE: f64 = 41.123_456;
const LONGITUDE: f64 = -8.654_321;

fn fix(reduced: bool) -> LocationOutcome { fix_outcome(LATITUDE, LONGITUDE, 25.0, reduced) }

#[test]
fn debug_and_display_never_print_coordinates() {
    let outcome = fix(false);
    for text in [
        format!("{outcome:?}"),
        format!("{outcome}"),
        format!("{outcome:#?}"),
        format!("{:?}", LocationFix {
            latitude: LATITUDE,
            longitude: LONGITUDE,
            accuracy_meters: 1.0,
            reduced_accuracy: false,
        }),
    ] {
        assert!(!text.contains("41.12"), "{text}");
        assert!(!text.contains("8.654"), "{text}");
        assert!(text.contains("redacted") || text.contains(".."), "{text}");
    }
}

#[test]
fn authorization_statuses_map_to_actions_without_re_requesting() {
    use AuthorizationAction::{Fail, Proceed, Request, Wait};
    assert_eq!(authorization_action(status::NOT_DETERMINED, false), Request);
    assert_eq!(authorization_action(status::NOT_DETERMINED, true), Wait);
    for asked in [false, true] {
        assert_eq!(authorization_action(status::AUTHORIZED_ALWAYS, asked), Proceed);
        assert_eq!(
            authorization_action(status::AUTHORIZED_WHEN_IN_USE, asked),
            Proceed
        );
        assert_eq!(
            authorization_action(status::DENIED, asked),
            Fail(LocationUnavailable::Denied)
        );
        assert_eq!(
            authorization_action(status::RESTRICTED, asked),
            Fail(LocationUnavailable::Restricted)
        );
        assert_eq!(authorization_action(42, asked), Fail(LocationUnavailable::Error));
    }
}

#[test]
fn temporary_full_accuracy_needs_every_condition() {
    let declared = BundleKeys {
        usage_description: true,
        temporary_purpose: true,
    };
    let undeclared = BundleKeys {
        usage_description: true,
        temporary_purpose: false,
    };
    assert!(wants_temporary_full_accuracy(true, true, declared, false));
    assert!(!wants_temporary_full_accuracy(false, true, declared, false));
    assert!(!wants_temporary_full_accuracy(true, false, declared, false));
    assert!(!wants_temporary_full_accuracy(true, true, undeclared, false));
    assert!(!wants_temporary_full_accuracy(true, true, declared, true));
}

#[test]
fn error_codes_and_fixes_map_to_typed_outcomes() {
    assert_eq!(error_outcome(1), LocationUnavailable::Denied);
    assert_eq!(error_outcome(0), LocationUnavailable::Error);
    assert_eq!(error_outcome(-1), LocationUnavailable::Error);
    assert_eq!(
        fix(true),
        LocationOutcome::Located(LocationFix {
            latitude: LATITUDE,
            longitude: LONGITUDE,
            accuracy_meters: 25.0,
            reduced_accuracy: true,
        })
    );
    for (latitude, longitude, accuracy) in [
        (f64::NAN, 0.0, 1.0),
        (0.0, f64::INFINITY, 1.0),
        (91.0, 0.0, 1.0),
        (0.0, -181.0, 1.0),
        (0.0, 0.0, -1.0),
        (0.0, 0.0, f64::NAN),
    ] {
        assert_eq!(
            fix_outcome(latitude, longitude, accuracy, false),
            LocationOutcome::Unavailable(LocationUnavailable::Error)
        );
    }
}

#[test]
fn a_completion_keeps_the_first_outcome_and_drops_late_ones() {
    let completion = Completion::new(7);
    assert_eq!(completion.generation(), 7);
    assert!(!completion.is_complete());
    assert_eq!(completion.wait(Duration::from_millis(5)), None);
    let timeout = LocationOutcome::Unavailable(LocationUnavailable::Timeout);
    assert!(completion.complete(timeout));
    assert!(!completion.complete(fix(false)));
    assert_eq!(completion.wait(Duration::ZERO), Some(timeout));
}

#[test]
fn a_waiter_wakes_when_another_thread_completes() {
    let completion = Completion::new(1);
    let writer = Arc::clone(&completion);
    let handle = thread::spawn(move || {
        thread::sleep(Duration::from_millis(10));
        writer.complete(fix(false))
    });
    assert_eq!(completion.wait(Duration::from_secs(5)), Some(fix(false)));
    assert!(handle.join().expect("writer"));
}

#[test]
fn the_generation_gate_drops_late_and_stale_events() {
    let mut gate = GenerationGate::default();
    assert!(!gate.accepts(1));
    gate.begin(1);
    assert!(gate.accepts(1));
    assert!(!gate.accepts(2));
    assert!(gate.finish(1));
    assert!(!gate.accepts(1), "late callback after a finish");
    assert!(!gate.finish(1));
    gate.begin(2);
    gate.begin(3);
    assert!(!gate.accepts(2), "a newer read supersedes the older one");
    assert!(!gate.finish(2));
    assert!(gate.accepts(3));
}

#[test]
fn the_fake_records_requested_accuracy_and_scripts_every_outcome() {
    let source = FakeLocationSource::new(fix(false));
    let approximate = LocationRequest::new(false);
    let precise = LocationRequest {
        precise: true,
        timeout: Duration::from_secs(1),
    };
    for reason in [
        LocationUnavailable::Denied,
        LocationUnavailable::Restricted,
        LocationUnavailable::ServicesDisabled,
        LocationUnavailable::NotDeterminedTimeout,
        LocationUnavailable::Timeout,
        LocationUnavailable::Error,
        LocationUnavailable::NoUsageDescription,
        LocationUnavailable::Unavailable,
    ] {
        source.push(LocationOutcome::Unavailable(reason));
        assert_eq!(source.read(approximate), LocationOutcome::Unavailable(reason));
    }
    assert_eq!(source.read(precise), fix(false));
    source.set(fix(true));
    assert_eq!(source.read(precise), fix(true));
    let requests = source.requests();
    assert_eq!(requests.len(), 10);
    assert_eq!(requests[0], approximate);
    assert_eq!(requests[0].timeout, DEFAULT_TIMEOUT);
    assert!(requests[8].precise && requests[9].precise);
}

#[test]
fn an_off_main_read_without_a_bundle_never_reaches_corelocation() {
    // The test binary has no location usage description, so the real source
    // stops before constructing a CLLocationManager.
    let outcome = thread::spawn(|| SystemLocation.read(LocationRequest::new(false)))
        .join()
        .expect("reader");
    assert_eq!(
        outcome,
        LocationOutcome::Unavailable(LocationUnavailable::NoUsageDescription)
    );
}

#[test]
fn every_unavailable_reason_has_a_distinct_message() {
    let reasons = [
        LocationUnavailable::Denied,
        LocationUnavailable::Restricted,
        LocationUnavailable::ServicesDisabled,
        LocationUnavailable::NotDeterminedTimeout,
        LocationUnavailable::Timeout,
        LocationUnavailable::Error,
        LocationUnavailable::NoUsageDescription,
        LocationUnavailable::Unavailable,
    ];
    let messages: std::collections::HashSet<_> = reasons.iter().map(ToString::to_string).collect();
    assert_eq!(messages.len(), reasons.len());
}

#[test]
fn the_consent_watch_notifies_once_on_a_decision_only() {
    use WatchStep::{Decided, Dropped, Keep};
    let mut watch = ConsentWatch::default();
    assert!(!watch.is_pending());
    assert_eq!(
        watch.on_authorization(1, status::DENIED),
        Dropped,
        "nothing pending"
    );
    watch.begin(1);
    assert!(watch.is_pending());
    assert_eq!(watch.on_authorization(1, status::NOT_DETERMINED), Keep);
    assert_eq!(watch.on_authorization(1, status::NOT_DETERMINED), Keep);
    assert_eq!(watch.on_authorization(1, status::AUTHORIZED_ALWAYS), Decided);
    assert!(!watch.is_pending());
    assert_eq!(
        watch.on_authorization(1, status::AUTHORIZED_ALWAYS),
        Dropped,
        "once"
    );
    for decided in [
        status::DENIED,
        status::RESTRICTED,
        status::AUTHORIZED_WHEN_IN_USE,
    ] {
        watch.begin(2);
        assert_eq!(watch.on_authorization(2, decided), Decided);
    }
}

#[test]
fn the_consent_watch_drops_cancelled_and_superseded_decisions() {
    use WatchStep::{Decided, Dropped};
    let mut watch = ConsentWatch::default();
    watch.begin(1);
    assert!(watch.cancel());
    assert!(!watch.cancel());
    assert_eq!(
        watch.on_authorization(1, status::DENIED),
        Dropped,
        "late after cancel"
    );
    watch.begin(1);
    watch.begin(2); // a newer read replaces the one watcher
    assert_eq!(
        watch.on_authorization(1, status::DENIED),
        Dropped,
        "older generation"
    );
    assert_eq!(watch.on_authorization(2, status::DENIED), Decided);
}

#[test]
fn the_fake_keeps_one_watcher_and_notifies_once_after_the_timeout() {
    let source = FakeLocationSource::unavailable(LocationUnavailable::NotDeterminedTimeout);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let notifier = || -> ConsentNotifier {
        let calls = Arc::clone(&calls);
        Arc::new(move || {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
    };
    let request = LocationRequest::new(false);
    assert_eq!(
        source.read_watched(request, notifier()),
        LocationOutcome::Unavailable(LocationUnavailable::NotDeterminedTimeout)
    );
    assert!(source.watcher_pending());
    // A newer read replaces the watcher: still one manager.
    let _ = source.read_watched(request, notifier());
    assert_eq!(source.watchers_created(), 1);
    source.set(fix(false));
    assert!(source.decide_consent());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(!source.watcher_pending());
    assert!(!source.decide_consent(), "decided once");
    assert_eq!(source.notifications(), 1);
    assert_eq!(source.read(request), fix(false));
}

#[test]
fn a_cancelled_fake_watcher_never_notifies() {
    let source = FakeLocationSource::unavailable(LocationUnavailable::NotDeterminedTimeout);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let _ = source.read_watched(
        LocationRequest::new(false),
        Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }),
    );
    source.cancel_pending();
    source.cancel_pending();
    assert_eq!(source.watchers_cancelled(), 1);
    assert!(!source.decide_consent());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn a_timeout_that_was_not_awaiting_consent_leaves_no_watcher() {
    let source = FakeLocationSource::unavailable(LocationUnavailable::Timeout);
    let _ = source.read_watched(LocationRequest::new(false), Arc::new(|| {}));
    assert!(!source.watcher_pending());
    assert_eq!(source.watchers_created(), 0);
}
