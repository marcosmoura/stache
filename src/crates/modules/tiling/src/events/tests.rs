//! Pipeline, coalescer and mailbox tests (deterministic, no threads except where noted).
#![allow(clippy::cast_possible_truncation)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use stache_macos::DisplayId;
use stache_macos::windows::{WindowChange, WindowEventKind};

use super::*;
use crate::state::{Cause, Input, Scope};
use crate::test_support::{Feed, app, one_display};

fn started(semantic: usize, geometry: usize) -> (Pipeline, Feed) {
    let feed = Feed::new(1);
    let mut pipeline = Pipeline::with_bounds(
        semantic,
        COMPLETION_CAPACITY,
        GeometryCoalescer::with_capacity(geometry),
    );
    pipeline.admit_input(Input::Start(feed.stamp));
    // The Start control message is consumed by the actor before any event.
    assert!(matches!(pipeline.take(10).as_slice(), [Input::Start(_)]));
    (pipeline, feed)
}

fn ms(n: u64) -> Duration { Duration::from_millis(n) }

#[test]
fn production_bounds_are_the_specified_ones() {
    assert_eq!(SEMANTIC_CAPACITY, 1024);
    assert_eq!(MIN_REFRESH_HZ, 30.0);
    assert_eq!(MAX_REFRESH_HZ, 360.0);
}

#[test]
fn refresh_rate_clamp_rule_is_explicit() {
    // unknown / NaN / non-finite / non-positive -> 60
    for raw in [
        None,
        Some(f64::NAN),
        Some(f64::INFINITY),
        Some(f64::NEG_INFINITY),
        Some(0.0),
        Some(-30.0),
    ] {
        assert_eq!(clamp_refresh_hz(raw), 60.0, "{raw:?}");
    }
    // in-range stays; out-of-range finite positive values clamp to the edge
    assert_eq!(clamp_refresh_hz(Some(30.0)), 30.0);
    assert_eq!(clamp_refresh_hz(Some(60.0)), 60.0);
    assert_eq!(clamp_refresh_hz(Some(120.0)), 120.0);
    assert_eq!(clamp_refresh_hz(Some(360.0)), 360.0);
    assert_eq!(clamp_refresh_hz(Some(5.0)), 30.0);
    assert_eq!(clamp_refresh_hz(Some(1000.0)), 360.0);
    let period = |hz| flush_period(hz).as_secs_f64();
    assert!((period(Some(120.0)) - 1.0 / 120.0).abs() < 1e-9);
    assert!((period(Some(1000.0)) - 1.0 / 360.0).abs() < 1e-9);
    assert!((period(Some(f64::NAN)) - 1.0 / 60.0).abs() < 1e-9);
}

#[test]
fn geometry_coalesces_latest_per_exact_token_and_flushes_at_the_display_rate() {
    let feed = Feed::new(1);
    let (a, b) = (feed.token(1, 1), feed.token(1, 2));
    let d1 = DisplayId::new(1);
    let d2 = DisplayId::new(2);
    let mut geometry = GeometryCoalescer::default();
    let t0 = Instant::now();
    geometry.retime(
        BTreeMap::from([
            (d1, flush_period(Some(120.0))),
            (d2, flush_period(Some(30.0))),
        ]),
        |_| None,
        t0,
    );
    assert_eq!(geometry.record(a, Some(d1), t0), Record::New);
    for i in 1..100 {
        // A burst of samples for one token stays one entry.
        assert_eq!(
            geometry.record(a, Some(d1), t0 + Duration::from_micros(i)),
            Record::Coalesced
        );
    }
    assert_eq!(geometry.record(b, Some(d2), t0), Record::New);
    assert_eq!(geometry.len(), 2);
    assert!(geometry.due(t0).is_empty());
    // 120 Hz display flushes first (about 8.3 ms), 30 Hz one later (about 33.3 ms).
    let first = geometry.due(t0 + ms(9));
    assert_eq!(first, vec![vec![a]]);
    assert!(geometry.due(t0 + ms(20)).is_empty());
    assert_eq!(geometry.due(t0 + ms(34)), vec![vec![b]]);
    assert!(geometry.is_empty() && geometry.next_deadline().is_none());
}

#[test]
fn a_continuous_stream_still_flushes_every_period() {
    let feed = Feed::new(1);
    let a = feed.token(1, 1);
    let mut geometry = GeometryCoalescer::default();
    let t0 = Instant::now();
    let mut flushes = 0;
    for step in 0..200u64 {
        let now = t0 + Duration::from_millis(step);
        geometry.record(a, None, now);
        flushes += geometry.due(now).len();
    }
    // 200 ms at 60 Hz is about 12 flushes: never starved, never one per sample.
    assert!((10..=13).contains(&flushes), "{flushes}");
}

#[test]
fn retiming_cancels_old_timers_and_remaps_pending_tokens() {
    let feed = Feed::new(1);
    let (a, b, c) = (feed.token(1, 1), feed.token(1, 2), feed.token(1, 3));
    let (d1, d2) = (DisplayId::new(1), DisplayId::new(2));
    let mut geometry = GeometryCoalescer::default();
    let t0 = Instant::now();
    geometry.retime(
        BTreeMap::from([
            (d1, flush_period(Some(30.0))),
            (d2, flush_period(Some(30.0))),
        ]),
        |_| None,
        t0,
    );
    geometry.record(a, Some(d1), t0);
    geometry.record(b, Some(d1), t0);
    geometry.record(c, Some(d2), t0);
    // Hotplug: d2 disappears, the window moved to d1 (route by exact token); d1 is now 360 Hz.
    geometry.retime(
        BTreeMap::from([(d1, flush_period(Some(360.0)))]),
        |token| (token == c).then_some(d1),
        t0 + ms(1),
    );
    assert_eq!(geometry.len(), 3, "no sample lost across a retime");
    let due = geometry.due(t0 + ms(1) + ms(3));
    assert_eq!(due.len(), 1, "one bucket on the surviving display");
    assert_eq!(due[0], vec![a, b, c]);
}

#[test]
fn a_retime_never_postpones_a_pending_sample() {
    let feed = Feed::new(1);
    let a = feed.token(1, 1);
    let d1 = DisplayId::new(1);
    let mut geometry = GeometryCoalescer::default();
    let t0 = Instant::now();
    geometry.retime(BTreeMap::from([(d1, flush_period(Some(60.0)))]), |_| None, t0);
    geometry.record(a, Some(d1), t0);
    let before = geometry.next_deadline().expect("deadline");
    geometry.retime(
        BTreeMap::from([(d1, flush_period(Some(30.0)))]),
        |_| Some(d1),
        t0 + ms(5),
    );
    assert!(geometry.next_deadline().expect("deadline") <= before);
}

#[test]
fn geometry_cardinality_is_bounded_and_overflow_requests_a_refresh() {
    let (mut pipeline, mut feed) = started(16, 3);
    let now = Instant::now();
    for serial in 1..=5 {
        let token = feed.token(1, serial);
        let event = feed.event(WindowEventKind::Window {
            window: token,
            change: WindowChange::Moved,
        });
        pipeline.admit_event(event, None, now);
    }
    let stats = pipeline.stats();
    assert_eq!(stats.geometry_pending, 3);
    assert_eq!(stats.geometry_dropped, 2);
    let inputs = pipeline.take(100);
    assert!(inputs.iter().any(
        |i| matches!(i, Input::Unconfirmed(Scope::App(a), Cause::GeometryOverflow) if *a == app(1))
    ));
    // An already-pending token still coalesces at the bound.
    let token = feed.token(1, 1);
    let event = feed.event(WindowEventKind::Window {
        window: token,
        change: WindowChange::Resized,
    });
    pipeline.admit_event(event, None, now);
    assert_eq!(pipeline.stats().geometry_pending, 3);
}

#[test]
fn identity_barriers_invalidate_or_flush_older_geometry_by_exact_token() {
    let (mut pipeline, mut feed) = started(16, 16);
    let now = Instant::now();
    let (a, b, c) = (feed.token(1, 1), feed.token(1, 2), feed.token(1, 3));
    for token in [a, b, c] {
        let event = feed.event(WindowEventKind::Window {
            window: token,
            change: WindowChange::Moved,
        });
        pipeline.admit_event(event, None, now);
    }
    // Destroy invalidates a's older sample: it can never be applied afterwards.
    let destroy = feed.event(WindowEventKind::Window {
        window: a,
        change: WindowChange::Destroyed,
    });
    pipeline.admit_event(destroy, None, now);
    // A (re)snapshot of b is a membership barrier: the older sample is flushed first.
    let created = feed.created(b, "b", 2);
    let Input::Window(event) = created else { unreachable!() };
    pipeline.admit_event(event, None, now);
    let stats = pipeline.stats();
    assert_eq!(stats.geometry_invalidated, 1);
    assert_eq!(stats.geometry_barrier_flushes, 1);
    assert!(
        pipeline.geometry().contains(c)
            && !pipeline.geometry().contains(a)
            && !pipeline.geometry().contains(b)
    );

    let inputs = pipeline.take(100);
    let positions: Vec<&str> = inputs
        .iter()
        .filter_map(|input| match input {
            Input::GeometryBatch(tokens) if tokens == &vec![b] => Some("flush-b"),
            Input::Window(e) => match &e.kind {
                WindowEventKind::Window {
                    change: WindowChange::Destroyed,
                    ..
                } => Some("destroy-a"),
                WindowEventKind::Snapshot { .. } => Some("create-b"),
                _ => None,
            },
            _ => None,
        })
        .collect();
    // FIFO: destroy(a), then b's older geometry strictly before b's snapshot.
    assert_eq!(positions, vec!["destroy-a", "flush-b", "create-b"]);
}

#[test]
fn the_semantic_queue_is_fifo_bounded_and_overflow_marks_the_app() {
    let (mut pipeline, mut feed) = started(3, 16);
    let now = Instant::now();
    let tokens: Vec<_> = (1..=5).map(|n| feed.token(2, n)).collect();
    for token in &tokens {
        let event = feed.event(WindowEventKind::Window {
            window: *token,
            change: WindowChange::Destroyed,
        });
        pipeline.admit_event(event, None, now);
    }
    let stats = pipeline.stats();
    assert_eq!(stats.semantic_accepted, 3);
    assert_eq!(stats.semantic_rejected, 2);
    assert_eq!(stats.semantic_depth, 3);
    assert_eq!(stats.semantic_high_water, 3);
    let inputs = pipeline.take(100);
    // The lost destroys cannot be silently skipped: an unconfirmed scope comes first.
    assert!(
        matches!(inputs.first(), Some(Input::Unconfirmed(Scope::App(a), Cause::SemanticOverflow)) if *a == app(2))
    );
    let order: Vec<_> = inputs
        .iter()
        .filter_map(|i| match i {
            Input::Window(e) => match e.kind {
                WindowEventKind::Window { window, .. } => Some(window),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(order, tokens[..3].to_vec(), "accepted FIFO order preserved");
    // Exactly one scope entry no matter how many were lost: no overflow list.
    assert_eq!(
        inputs.iter().filter(|i| matches!(i, Input::Unconfirmed(..))).count(),
        1
    );
}

#[test]
fn a_sustained_flood_stays_bounded() {
    let (mut pipeline, mut feed) = started(SEMANTIC_CAPACITY, GEOMETRY_CAPACITY);
    let now = Instant::now();
    for n in 0..200_000u64 {
        let token = feed.token(1 + n % 50, n);
        let kind = if n % 2 == 0 {
            WindowChange::Destroyed
        } else {
            WindowChange::Moved
        };
        let event = feed.event(WindowEventKind::Window { window: token, change: kind });
        pipeline.admit_event(event, None, now);
    }
    let stats = pipeline.stats();
    assert!(stats.semantic_depth <= SEMANTIC_CAPACITY);
    assert!(stats.geometry_pending <= GEOMETRY_CAPACITY);
    assert!(
        stats.semantic_rejected > 0 && stats.geometry_dropped > 0,
        "visible accounting"
    );
    let inputs = pipeline.take(10_000);
    let scopes = inputs.iter().filter(|i| matches!(i, Input::Unconfirmed(..))).count();
    assert!(scopes <= crate::state::MAX_UNCONFIRMED_APPS + 1);
}

#[test]
fn sink_drops_gaps_and_stale_epochs_are_accounted() {
    let (mut pipeline, mut feed) = started(16, 16);
    let now = Instant::now();
    pipeline.admit_sink_drop(3);
    assert_eq!(pipeline.stats().sink_dropped, 3);
    let inputs = pipeline.take(10);
    assert!(matches!(
        inputs[0],
        Input::Unconfirmed(Scope::All, Cause::SinkOverflow)
    ));

    // A hole in the backend sequence proves a missed event.
    let first = feed.event(WindowEventKind::ObserverOverflow { application: app(1) });
    pipeline.admit_event(first, None, now);
    let _skipped = feed.event(WindowEventKind::ObserverOverflow { application: app(1) });
    let third = feed.event(WindowEventKind::ObserverOverflow { application: app(1) });
    pipeline.admit_event(third, None, now);
    assert_eq!(pipeline.stats().sequence_gaps, 1);

    // An old epoch's late event is dropped and counted.
    let mut old = Feed::new(99);
    let stale = old.event(WindowEventKind::ObserverOverflow { application: app(1) });
    pipeline.admit_event(stale, None, now);
    assert_eq!(pipeline.stats().stale_epoch_events, 1);
}

#[test]
fn display_and_config_values_replace_one_slot_when_the_fifo_is_full() {
    let (mut pipeline, mut feed) = started(2, 4);
    let now = Instant::now();
    for n in 1..=2 {
        let event = feed.event(WindowEventKind::Window {
            window: feed.token(1, n),
            change: WindowChange::Destroyed,
        });
        pipeline.admit_event(event, None, now);
    }
    let layout = Arc::new(one_display());
    pipeline.admit_input(Input::Displays(Arc::clone(&layout)));
    pipeline.admit_input(Input::Displays(Arc::clone(&layout)));
    pipeline.admit_input(Input::Displays(layout));
    assert_eq!(pipeline.stats().latest_replaced, 2);
    let inputs = pipeline.take(10);
    assert_eq!(
        inputs.iter().filter(|i| matches!(i, Input::Displays(_))).count(),
        1
    );
    assert!(
        !inputs.iter().any(|i| matches!(i, Input::Unconfirmed(..))),
        "nothing was lost"
    );
}

#[test]
fn lifecycle_overflow_widens_to_everything() {
    let (mut pipeline, mut feed) = started(1, 4);
    let now = Instant::now();
    let event = feed.event(WindowEventKind::Window {
        window: feed.token(1, 1),
        change: WindowChange::Destroyed,
    });
    pipeline.admit_event(event, None, now);
    pipeline.admit_input(Input::Lifecycle {
        kind: stache_macos::applications::LifecycleKind::Terminated,
        info: crate::test_support::info(1, "x", "x"),
    });
    let inputs = pipeline.take(10);
    assert!(inputs.iter().any(|i| matches!(i, Input::Unconfirmed(Scope::All, _))));
}

#[test]
fn completions_are_bounded() {
    let (mut pipeline, feed) = started(4, 4);
    for _ in 0..(COMPLETION_CAPACITY + 10) {
        pipeline.admit_input(Input::Completion {
            stamp: feed.stamp,
            result: crate::state::CompletionResult::Rejected(crate::state::Effect::Publish),
        });
    }
    let stats = pipeline.stats();
    assert_eq!(stats.completions_accepted as usize, COMPLETION_CAPACITY);
    assert_eq!(stats.completions_rejected, 10);
}

#[test]
fn ingress_refuses_after_shutdown_and_counts_it() {
    let ingress = Ingress::new(Pipeline::new());
    assert!(ingress.submit(Input::Activated(app(1))).is_ok());
    ingress.shutdown();
    assert_eq!(
        ingress.submit(Input::Activated(app(1))),
        Err(SubmitError::Closed)
    );
    assert_eq!(ingress.stats().mailbox_closed, 1);
}

#[test]
fn shutdown_before_waiting_never_sleeps_even_with_a_far_deadline() {
    let ingress = Ingress::new(Pipeline::new());
    ingress.shutdown();
    let started = Instant::now();
    assert_eq!(
        ingress.wait(Some(Instant::now() + Duration::from_secs(3600))),
        Wake::Shutdown
    );
    assert_eq!(ingress.wait(None), Wake::Shutdown);
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn cancelling_between_the_check_and_the_sleep_is_never_lost() {
    // The flag and the waiter's check share one lock, so whichever interleaving
    // occurs the waiter returns promptly. Hammer it across many thread races.
    for round in 0..200 {
        let ingress = Arc::new(Ingress::new(Pipeline::new()));
        let waiter = {
            let ingress = Arc::clone(&ingress);
            std::thread::spawn(move || ingress.wait(Some(Instant::now() + Duration::from_secs(30))))
        };
        if round % 2 == 0 {
            std::thread::yield_now();
        }
        ingress.shutdown();
        let started = Instant::now();
        assert_eq!(waiter.join().expect("waiter"), Wake::Shutdown);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}

#[test]
fn a_deadline_wakes_the_waiter() {
    let ingress = Ingress::new(Pipeline::new());
    let wake = ingress.wait(Some(Instant::now() + ms(20)));
    assert_eq!(wake, Wake::Deadline);
}

#[test]
fn arrival_wakes_the_waiter() {
    let ingress = Arc::new(Ingress::new(Pipeline::new()));
    let waiter = {
        let ingress = Arc::clone(&ingress);
        std::thread::spawn(move || ingress.wait(None))
    };
    std::thread::sleep(ms(20));
    ingress.submit(Input::Activated(app(1))).expect("submit");
    assert_eq!(waiter.join().expect("waiter"), Wake::Work);
}
