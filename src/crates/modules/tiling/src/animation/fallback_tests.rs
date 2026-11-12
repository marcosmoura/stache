//! TA-06-01 and TA-06-02: the metrics a finished animation leaves, the bounded
//! book, and the instant fallback policy, over a fake clock and a fake sink. A
//! slow application is modelled by answering its frames late or as replaced.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use stache_macos::DisplayId;
use stache_macos::windows::{FrameAck, FrameReply, FrameStage, WindowActionOutcome};

use super::coordinator_tests::{FakeSink, Rig, wait_for};
use super::fallback::{
    FIRST_COOLDOWN, FallbackEntry, FallbackEvent, FallbackReason, FallbackSet, MAX_COOLDOWN,
    MotionKind,
};
use super::metrics::{
    AnimationRecord, LATENCY_SAMPLES, LatencyRing, MAX_APPLICATIONS, MetricsBook, Trigger, WINDOW,
};
use super::tests::{FROM, PERIOD, TO, app, job, window_in, written};
use super::*;
use crate::config::{AnimationConfig, EasingType};

const D1: DisplayId = DisplayId::new(1);

fn ms(n: u64) -> Duration { Duration::from_millis(n) }

fn clocked_rig() -> Rig {
    let mut rig = Rig::new();
    rig.animator.set_clocked([D1].into());
    rig
}

/// A job of `application`'s window 1, on the display with a frame clock.
fn app_job(plan: u64, application: u64) -> AnimationJob {
    let mut j = job(plan, FROM, TO, EasingType::Linear);
    j.window = window_in(application, 1);
    j.display = Some(D1);
    j
}

fn done() -> FrameReply {
    FrameReply::Done(FrameAck {
        outcome: written(),
        sent_at: None,
    })
}

/// Runs one animation to its completion while answering the shard's frames like
/// a slow application: only every `keep`th non-final frame is written (the
/// others are replaced in its mailbox), and the final one is acknowledged
/// `final_delay` after it was sent. Ticks come from the frame clock (`clocked`)
/// or the timer.
fn run_animation(
    rig: &mut Rig,
    j: AnimationJob,
    clocked: bool,
    keep: usize,
    final_delay: Duration,
) {
    rig.sink.manual.store(true, Ordering::SeqCst);
    let before = rig.sink.completed().len();
    rig.admit(j);
    let mut n = 0;
    for _ in 0..400 {
        while rig.sink.held() > 0 {
            let reply = if rig.sink.held_stage(0) == FrameStage::Final {
                rig.advance(final_delay);
                done()
            } else {
                n += 1;
                if (n - 1) % keep == 0 {
                    done()
                } else {
                    FrameReply::Replaced
                }
            };
            rig.sink.answer(0, reply);
        }
        rig.pump();
        if rig.sink.completed().len() > before {
            break;
        }
        if clocked {
            rig.now += PERIOD;
            let at = rig.now;
            rig.ticks.insert(D1, at);
            rig.pump();
        } else if !rig.step() {
            rig.advance(PERIOD);
        }
    }
    rig.sink.manual.store(false, Ordering::SeqCst);
    assert!(rig.sink.completed().len() > before, "the animation completed");
}

fn healthy(rig: &mut Rig, plan: u64, application: u64) {
    run_animation(rig, app_job(plan, application), true, 1, Duration::ZERO);
}

fn slow(rig: &mut Rig, plan: u64, application: u64) {
    run_animation(rig, app_job(plan, application), true, 12, Duration::ZERO);
}

fn late(rig: &mut Rig, plan: u64, application: u64) {
    run_animation(rig, app_job(plan, application), true, 1, ms(300));
}

fn reason(rig: &Rig, application: u64) -> Option<FallbackReason> {
    rig.animator
        .shared()
        .fallback
        .entries()
        .into_iter()
        .find(|e| e.application == app(application))
        .map(|e| e.reason)
}

/// Each helper record is its own layout change (plan), so none fold together.
fn record(rate_hz: f64, lag_ms: u64) -> AnimationRecord {
    static PLAN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    AnimationRecord {
        plan_sequence: PLAN.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        kind: MotionKind::Resize,
        frames: FrameCounters {
            scheduled: 10,
            written: 8,
            dropped: 2,
        },
        rate_hz,
        write_p50: ms(2),
        write_p95: ms(4),
        end_lag: ms(lag_ms),
        period: PERIOD,
    }
}

// ----------------------------------------------------------------- metrics

#[test]
fn a_finished_animation_records_its_frames_rate_latency_and_end_lag() {
    let mut rig = clocked_rig();
    run_animation(&mut rig, app_job(1, 1), true, 1, ms(7));
    let records = rig.animator.shared().book().records(app(1));
    assert_eq!(records.len(), 1);
    let r = records[0];
    // 13 frames at 16 ms ticks (the 13th is the final one), all written.
    assert_eq!(r.frames, FrameCounters {
        scheduled: 13,
        written: 13,
        dropped: 0
    });
    // From the first tick (16 ms) to the final acknowledgement (208 + 7 ms).
    let expected = 13.0 / 0.199;
    assert!(
        (r.rate_hz - expected).abs() < 0.01,
        "{} vs {expected}",
        r.rate_hz
    );
    assert_eq!(r.write_p50, Duration::ZERO);
    assert_eq!(r.write_p95, ms(7), "the final write waited 7 ms");
    // Scheduled end: the frame slot at or after 200 ms is 13 * 16 = 208 ms.
    assert_eq!(r.end_lag, ms(7));
    assert!(!r.rate_bad() && !r.lag_bad());
    assert!(rig.animator.shared().fallback.is_empty());
}

#[test]
fn dropped_frames_are_counted_and_lower_the_effective_rate() {
    let mut rig = clocked_rig();
    slow(&mut rig, 1, 1);
    let r = rig.animator.shared().book().records(app(1))[0];
    assert_eq!(r.frames.scheduled, 13);
    assert_eq!(r.frames.written, 2, "the first of 12, plus the final");
    assert_eq!(r.frames.dropped, 11);
    assert!(r.rate_hz < 15.0 && r.rate_bad(), "{}", r.rate_hz);
}

#[test]
fn the_latency_ring_is_fixed_and_keeps_the_newest_samples() {
    let mut ring = LatencyRing::new();
    assert_eq!(ring.percentiles(), (Duration::ZERO, Duration::ZERO));
    for n in 1..=100_u64 {
        ring.push(ms(n));
    }
    // The newest 64 are 37..=100: nearest rank 32 -> 68, 61 -> 97.
    assert_eq!(LATENCY_SAMPLES, 64);
    assert_eq!(ring.percentiles(), (ms(68), ms(97)));
    // Fixed: no heap, the buffer is inline.
    assert!(std::mem::size_of::<LatencyRing>() >= LATENCY_SAMPLES * 4);
}

#[test]
fn the_book_keeps_the_last_eight_animations_per_application() {
    let mut book = MetricsBook::default();
    for n in 0..12_u32 {
        book.record(app(1), record(f64::from(60 + n), 0));
    }
    let records = book.records(app(1));
    assert_eq!(records.len(), WINDOW);
    assert!(
        (records[0].rate_hz - 64.0).abs() < 1e-9,
        "the oldest four were dropped"
    );
    let summary = book.summaries();
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].1.animations, 8);
    assert_eq!(summary[0].1.dropped, 16);
    assert!((summary[0].1.rate_p50_hz - 67.0).abs() < 1e-9, "{summary:?}");
}

#[test]
fn the_book_holds_at_most_64_applications_and_evicts_the_least_recently_used() {
    let mut book = MetricsBook::default();
    for n in 1..=64_u64 {
        book.record(app(n), record(60.0, 0));
    }
    assert_eq!(book.len(), MAX_APPLICATIONS);
    // Application 1 is used again, so application 2 is the oldest.
    book.record(app(1), record(60.0, 0));
    book.record(app(65), record(60.0, 0));
    assert_eq!(book.len(), MAX_APPLICATIONS);
    assert!(!book.records(app(1)).is_empty());
    assert!(book.records(app(2)).is_empty(), "the least recently used went");
    assert!(!book.records(app(65)).is_empty());
    book.forget(app(65));
    assert!(book.records(app(65)).is_empty());
    book.clear();
    assert!(book.is_empty());
}

#[test]
fn the_policy_needs_two_bad_animations_of_the_last_three() {
    let mut book = MetricsBook::default();
    assert_eq!(book.record(app(1), record(10.0, 0)), None, "one bad animation");
    assert_eq!(book.record(app(1), record(60.0, 0)), None);
    assert_eq!(
        book.record(app(1), record(10.0, 0)),
        Some(Trigger::Rate),
        "2 of the last 3"
    );
    let mut book = MetricsBook::default();
    for _ in 0..3 {
        book.record(app(1), record(60.0, 0));
    }
    assert_eq!(book.record(app(1), record(60.0, 120)), None, "120 ms is healthy");
    assert_eq!(book.record(app(1), record(20.0, 120)), None, "20 Hz is healthy");
    let mut book = MetricsBook::default();
    for _ in 0..3 {
        book.record(app(1), record(60.0, 0));
    }
    assert_eq!(book.record(app(1), record(60.0, 300)), None, "1 of 3");
    assert_eq!(book.record(app(1), record(60.0, 300)), Some(Trigger::Lag));
    // A bad one that left the lookback no longer counts.
    let mut book = MetricsBook::default();
    book.record(app(1), record(10.0, 0));
    book.record(app(1), record(60.0, 0));
    book.record(app(1), record(60.0, 0));
    assert_eq!(book.record(app(1), record(10.0, 0)), None);
}

// ---------------------------------------------------------------- fallback

#[test]
fn one_slow_animation_does_not_trigger_the_fallback_but_two_of_three_do() {
    let mut rig = clocked_rig();
    slow(&mut rig, 1, 1);
    assert!(rig.animator.shared().fallback.is_empty(), "1 of 1");
    healthy(&mut rig, 2, 1);
    assert!(rig.animator.shared().fallback.is_empty(), "1 of 2");
    slow(&mut rig, 3, 1);
    assert_eq!(reason(&rig, 1), Some(FallbackReason::BelowMinimumRate), "2 of 3");
    assert_eq!(
        FallbackReason::BelowMinimumRate.to_string(),
        "writes fewer than 15 frames per second in 2 of its last 3 layout changes"
    );
}

#[test]
fn an_end_lag_over_250_ms_triggers_the_fallback_in_two_of_three() {
    let mut rig = clocked_rig();
    late(&mut rig, 1, 1);
    assert!(rig.animator.shared().fallback.is_empty());
    late(&mut rig, 2, 1);
    assert_eq!(reason(&rig, 1), Some(FallbackReason::LateEnd));
    let records = rig.animator.shared().book().records(app(1));
    assert!(
        records.iter().all(|r| r.lag_bad() && !r.rate_bad()),
        "{records:?}"
    );
}

#[test]
fn an_overflow_application_that_cannot_keep_up_names_the_overflow_thread() {
    let mut rig = clocked_rig();
    rig.sink.set_overflow(app(1));
    slow(&mut rig, 1, 1);
    slow(&mut rig, 2, 1);
    assert_eq!(reason(&rig, 1), Some(FallbackReason::Overflow));
    assert_eq!(
        FallbackReason::Overflow.to_string(),
        "shares the overflow AX thread and cannot keep up"
    );
}

#[test]
fn a_display_paced_by_the_timer_that_cannot_keep_up_names_the_missing_frame_clock() {
    let mut rig = Rig::new();
    for plan in [1, 2] {
        let mut j = app_job(plan, 1);
        j.display = None;
        run_animation(&mut rig, j, false, 12, Duration::ZERO);
    }
    assert_eq!(reason(&rig, 1), Some(FallbackReason::NoFrameClock));
    assert_eq!(
        FallbackReason::NoFrameClock.to_string(),
        "its display has no frame clock and it cannot keep up"
    );
}

#[test]
fn a_healthy_application_on_the_timer_is_not_in_fallback() {
    let mut rig = Rig::new();
    for plan in 1..=4 {
        let mut j = app_job(plan, 1);
        j.display = None;
        run_animation(&mut rig, j, false, 1, Duration::ZERO);
    }
    assert!(rig.animator.shared().fallback.is_empty());
    assert_eq!(rig.animator.shared().book().records(app(1)).len(), 4);
}

#[test]
fn the_lease_reasons_read_as_specified() {
    assert_eq!(
        FallbackReason::ReEnablesEnhancedUi.to_string(),
        "turns Enhanced UI back on"
    );
    assert_eq!(
        FallbackReason::EnhancedUiStuck.to_string(),
        "Enhanced UI could not be turned off"
    );
    assert_eq!(
        FallbackReason::LateEnd.to_string(),
        "ends more than 250 ms late in 2 of its last 3 layout changes"
    );
}

#[test]
fn other_applications_are_unaffected_and_keep_animating() {
    let mut rig = clocked_rig();
    slow(&mut rig, 1, 1);
    slow(&mut rig, 2, 1);
    assert!(rig.animator.shared().fallback.contains(app(1), MotionKind::Resize));
    healthy(&mut rig, 3, 2);
    assert!(!rig.animator.shared().fallback.contains(app(2), MotionKind::Resize));
    let sent = rig.sink.sent();
    let of_two: Vec<_> = sent.iter().filter(|s| s.plan == 3).collect();
    assert!(of_two.len() >= 8, "application 2 animated: {}", of_two.len());
    assert_eq!(of_two[0].stage, FrameStage::First);
}

#[test]
fn an_application_entering_fallback_mid_animation_gets_one_final_write_and_no_more_frames() {
    let mut rig = clocked_rig();
    let t0 = rig.now;
    rig.admit(app_job(1, 1));
    for k in 1..=4_u32 {
        rig.now = t0 + PERIOD * k;
        let at = rig.now;
        rig.ticks.insert(D1, at);
        rig.pump();
    }
    assert_eq!(rig.sink.sent().len(), 4, "four frames so far");
    rig.animator.shared().fallback.insert(
        app(1),
        MotionKind::Resize,
        FallbackReason::BelowMinimumRate,
        rig.now,
    );
    // The next tick writes the exact target once.
    rig.now = t0 + PERIOD * 5;
    let at = rig.now;
    rig.ticks.insert(D1, at);
    rig.pump();
    let sent = rig.sink.sent();
    assert_eq!(sent.len(), 5);
    let last = sent.last().expect("last");
    assert_eq!((last.stage, last.frame), (FrameStage::Final, TO));
    // Nothing else is ever sent: no further intermediate frames, no fighting.
    for k in 6..=40_u32 {
        rig.now = t0 + PERIOD * k;
        let at = rig.now;
        rig.ticks.insert(D1, at);
        rig.pump();
    }
    rig.advance(Duration::from_secs(3));
    assert_eq!(rig.sink.sent().len(), 5);
    assert_eq!(
        rig.sink.sent().iter().filter(|s| s.stage == FrameStage::Final).count(),
        1,
        "exactly one final write"
    );
    assert_eq!(rig.sink.completed().len(), 1, "it settled as usual");
    assert!(matches!(
        rig.sink.completed()[0].1,
        WindowActionOutcome::Issued { .. }
    ));
    assert!(
        rig.animator.shared().book().records(app(1)).is_empty(),
        "a cut-short animation is not measured"
    );
}

#[test]
fn a_job_waiting_for_its_retarget_read_also_switches_to_one_final_write() {
    let mut rig = clocked_rig();
    rig.sink.manual_reads.store(true, Ordering::SeqCst);
    rig.admit(app_job(1, 1));
    rig.admit(app_job(2, 1));
    assert!(rig.sink.sent().is_empty() || rig.sink.sent().iter().all(|s| s.plan == 1));
    let before = rig.sink.sent().len();
    rig.animator.shared().fallback.insert(
        app(1),
        MotionKind::Resize,
        FallbackReason::LateEnd,
        rig.now,
    );
    rig.now += PERIOD;
    let at = rig.now;
    rig.ticks.insert(D1, at);
    rig.pump();
    let sent = rig.sink.sent();
    let plan2: Vec<_> = sent[before..].iter().filter(|s| s.plan == 2).collect();
    assert_eq!(plan2.len(), 1);
    assert_eq!((plan2[0].stage, plan2[0].frame), (FrameStage::Final, TO));
}

#[test]
fn a_completed_plan_is_never_written_again() {
    let mut rig = clocked_rig();
    healthy(&mut rig, 1, 1);
    let sent = rig.sink.sent().len();
    let reads = rig.sink.settle_reads();
    assert_eq!(reads, 1);
    for _ in 0..50 {
        rig.now += PERIOD;
        let at = rig.now;
        rig.ticks.insert(D1, at);
        rig.pump();
    }
    rig.advance(Duration::from_secs(5));
    assert_eq!(rig.sink.sent().len(), sent, "no write after the settle readback");
    assert_eq!(rig.sink.settle_reads(), 1, "no second readback");
}

// ----------------------------------------------------------- the set itself

#[test]
fn the_set_is_sticky_per_instance_and_keeps_the_first_reason() {
    let set = FallbackSet::default();
    let now = std::time::Instant::now();
    assert!(set.insert(app(1), MotionKind::Resize, FallbackReason::LateEnd, now));
    assert!(!set.insert(app(1), MotionKind::Resize, FallbackReason::Overflow, now));
    assert_eq!(set.entries(), vec![FallbackEntry {
        application: app(1),
        kind: MotionKind::Resize,
        reason: FallbackReason::LateEnd,
        until: Some(now + FIRST_COOLDOWN),
    }]);
    // A relaunch is a new instance: not in the set.
    assert!(!set.contains(app(2), MotionKind::Resize));
    set.forget(app(1));
    assert!(set.is_empty());
}

fn control(sink: &Arc<FakeSink>) -> AnimationControl {
    AnimationControl::new(
        Arc::clone(sink) as Arc<dyn AnimationSink>,
        Arc::new(crate::actor::SystemClock),
        Arc::new(stache_macos::FakeFrameClockFactory::failing("timer")),
        stache_sdk::CancellationToken::new(),
    )
}

fn enabled(on: bool) -> AnimationConfig {
    AnimationConfig {
        enabled: on,
        duration: 200,
        easing: EasingType::Linear,
    }
}

#[test]
fn a_reload_that_turns_animations_off_and_on_clears_the_fallback_and_the_history() {
    let sink = FakeSink::new();
    let control = control(&sink);
    assert!(
        !control.enter_fallback(app(1), FallbackReason::LateEnd, Instant::now()),
        "nothing falls back while animations are off"
    );
    control.configure(&enabled(true)).expect("on");
    assert!(control.enter_fallback(app(1), FallbackReason::LateEnd, Instant::now()));
    control.shared().book().record(app(1), record(10.0, 0));
    assert_eq!(
        control.fallback_apps(),
        [(app(1), MotionKind::Move), (app(1), MotionKind::Resize)].into()
    );
    assert_eq!(control.report().metrics.len(), 1);
    // A reload that keeps animations on keeps the fallback.
    control
        .configure(&AnimationConfig { duration: 300, ..enabled(true) })
        .expect("on");
    assert!(control.fallback_apps().contains(&(app(1), MotionKind::Resize)));
    // Off clears; on again starts fresh.
    control.configure(&enabled(false)).expect("off");
    assert!(control.fallback_apps().is_empty());
    assert!(control.report().metrics.is_empty());
    control.configure(&enabled(true)).expect("on");
    assert!(control.fallback_apps().is_empty());
    assert_eq!(control.report(), AnimationReport::default());
    control.stop();
    assert!(control.join_within(Duration::from_secs(5)));
}

#[test]
fn a_terminated_application_is_forgotten() {
    let sink = FakeSink::new();
    let control = control(&sink);
    control.configure(&enabled(true)).expect("on");
    control.enter_fallback(app(1), FallbackReason::EnhancedUiStuck, Instant::now());
    control.enter_fallback(app(2), FallbackReason::EnhancedUiStuck, Instant::now());
    control.shared().book().record(app(1), record(60.0, 0));
    control.forget_application(app(1));
    assert_eq!(
        control.fallback_apps(),
        [(app(2), MotionKind::Move), (app(2), MotionKind::Resize)].into()
    );
    assert!(control.report().metrics.is_empty());
    control.stop();
    assert!(control.join_within(Duration::from_secs(5)));
}

#[test]
fn a_refusal_because_the_coordinator_is_full_is_not_sticky() {
    let sink = FakeSink::new();
    sink.manual.store(true, Ordering::SeqCst);
    let control = control(&sink);
    control.configure(&enabled(true)).expect("on");
    for n in 0..MAX_ANIMATED as u64 {
        let mut j = job(n + 1, FROM, TO, EasingType::Linear);
        j.window = window_in(1, n + 1);
        assert!(control.submit(j).is_ok());
    }
    let mut extra = job(99, FROM, TO, EasingType::Linear);
    extra.window = window_in(1, 99);
    let refused = control.submit(extra);
    assert!(refused.is_err(), "full: the caller writes directly");
    assert!(
        control.fallback_apps().is_empty(),
        "a full coordinator is per job"
    );
    assert!(control.report().fallbacks.is_empty());
    // Nothing was remembered about the refusal; the jobs are released.
    control.stop();
    drop(refused);
    while sink.held() > 0 {
        sink.answer(0, FrameReply::Replaced);
    }
    wait_for("the jobs to end", || {
        control.join_within(Duration::from_millis(50))
    });
}

// --------------------------------------------- end lag is not tick phase

/// Runs `count` animations of one application with the display's ticks arriving
/// every `period` at `phase` after admission and a healthy shard acknowledging
/// the final frame `latency` after it was sent.
fn run_phased(
    rig: &mut Rig,
    application: u64,
    count: u64,
    period: Duration,
    phase: Duration,
    latency: Duration,
) {
    rig.sink.manual.store(true, Ordering::SeqCst);
    for plan in 1..=count {
        let before = rig.sink.completed().len();
        let mut j = app_job(plan, application);
        j.period = period;
        rig.admit(j);
        rig.advance(phase);
        for _ in 0..400 {
            rig.now += period;
            let at = rig.now;
            rig.ticks.insert(D1, at);
            rig.pump();
            while rig.sink.held() > 0 {
                if rig.sink.held_stage(0) == FrameStage::Final {
                    rig.advance(latency);
                }
                rig.sink.answer(0, done());
            }
            rig.pump();
            if rig.sink.completed().len() > before {
                break;
            }
        }
        assert!(rig.sink.completed().len() > before, "animation {plan} completed");
    }
    rig.sink.manual.store(false, Ordering::SeqCst);
}

#[test]
fn a_healthy_application_never_falls_back_whatever_the_tick_phase() {
    for period_ms in [16_u64, 8] {
        let period = ms(period_ms);
        for phase_ms in 0..period_ms {
            let mut rig = clocked_rig();
            run_phased(&mut rig, 1, 6, period, ms(phase_ms), ms(2));
            assert!(
                rig.animator.shared().fallback.is_empty(),
                "period {period:?}, phase {phase_ms} ms: {:?}",
                rig.animator.shared().book().records(app(1))
            );
        }
    }
}

#[test]
fn the_worst_tick_phase_measures_only_the_write_latency() {
    let mut rig = clocked_rig();
    run_phased(&mut rig, 1, 1, ms(16), ms(15), ms(3));
    let records = rig.animator.shared().book().records(app(1));
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].end_lag, ms(3), "{records:?}");
}

#[test]
fn a_slow_application_still_falls_back_on_lag_at_both_refresh_rates() {
    for (period, latency) in [(ms(16), ms(300)), (ms(8), ms(300))] {
        let mut rig = clocked_rig();
        run_phased(&mut rig, 1, 3, period, ms(3), latency);
        assert_eq!(reason(&rig, 1), Some(FallbackReason::LateEnd), "{period:?}");
    }
}

#[test]
fn a_healthy_timer_paced_application_never_falls_back_whatever_the_phase() {
    for period_ms in [16_u64, 8] {
        let period = ms(period_ms);
        for phase_ms in 0..period_ms {
            let mut rig = Rig::new();
            rig.sink.manual.store(true, Ordering::SeqCst);
            rig.advance(ms(phase_ms));
            for plan in 1..=6 {
                let before = rig.sink.completed().len();
                let mut j = app_job(plan, 1);
                j.display = None;
                j.period = period;
                rig.admit(j);
                for _ in 0..400 {
                    while rig.sink.held() > 0 {
                        if rig.sink.held_stage(0) == FrameStage::Final {
                            rig.advance(ms(2));
                        }
                        rig.sink.answer(0, done());
                    }
                    rig.pump();
                    if rig.sink.completed().len() > before || !rig.step() {
                        break;
                    }
                }
                assert!(rig.sink.completed().len() > before);
            }
            assert!(
                rig.animator.shared().fallback.is_empty(),
                "period {period:?}, phase {phase_ms} ms: {:?}",
                rig.animator.shared().book().records(app(1))
            );
        }
    }
}

#[test]
fn a_stalled_clock_job_that_cannot_keep_up_is_attributed_to_the_timer() {
    let mut rig = clocked_rig();
    // The display is clocked but never ticks: the job is timer-paced once stalled.
    for plan in 1..=2 {
        run_animation(&mut rig, app_job(plan, 1), false, 12, Duration::ZERO);
    }
    assert_eq!(reason(&rig, 1), Some(FallbackReason::NoFrameClock));
}

// ------------------------------------------- one record per layout change

/// How one window of a plan answers: `written` of every `of` non-final frames
/// are written (the others are replaced), the final one `final_delay` late.
#[derive(Clone, Copy)]
struct Behaviour {
    written: usize,
    of: usize,
    final_delay: Duration,
}

/// The real 120 Hz log: about 70 written frames per second, 14 ms late.
const AT_70_HZ: Behaviour = Behaviour {
    written: 3,
    of: 5,
    final_delay: Duration::from_millis(14),
};
/// One window in ten written: 12 Hz at 120 Hz ticks.
const AT_12_HZ: Behaviour = Behaviour {
    written: 1,
    of: 10,
    final_delay: Duration::ZERO,
};

/// A healthy multi-window application: one in six written (20 Hz), 120 ms late.
const AT_20_HZ: Behaviour = Behaviour {
    written: 1,
    of: 6,
    final_delay: Duration::from_millis(120),
};

const PERIOD_120_HZ: Duration = Duration::from_nanos(8_333_333);

/// Runs one layout change of `application` with one job per entry of
/// `windows` (all one plan), ticking at `PERIOD_120_HZ`, until all complete.
fn run_plan(rig: &mut Rig, plan: u64, application: u64, windows: &[Behaviour]) {
    rig.sink.manual.store(true, Ordering::SeqCst);
    let before = rig.sink.completed().len();
    let instance = |i: usize| window_in(application, i as u64 + 1);
    for index in 0..windows.len() {
        let mut j = app_job(plan, application);
        j.window = instance(index);
        j.period = PERIOD_120_HZ;
        rig.admit(j);
    }
    let mut counters = vec![0_usize; windows.len()];
    for _ in 0..800 {
        let mut late = false;
        while rig.sink.held() > 0 {
            let window = rig.sink.held_window(0);
            let slot = (0..windows.len()).find(|i| instance(*i) == window).unwrap_or(0);
            let b = windows[slot];
            let reply = if rig.sink.held_stage(0) == FrameStage::Final {
                if !late {
                    // The windows' finals are acknowledged together.
                    rig.advance(b.final_delay);
                    late = true;
                }
                done()
            } else {
                counters[slot] += 1;
                let n = counters[slot];
                if (n * b.written) / b.of > ((n - 1) * b.written) / b.of {
                    done()
                } else {
                    FrameReply::Replaced
                }
            };
            rig.sink.answer(0, reply);
        }
        rig.pump();
        if rig.sink.completed().len() >= before + windows.len() {
            break;
        }
        rig.now += PERIOD_120_HZ;
        let at = rig.now;
        rig.ticks.insert(D1, at);
        rig.pump();
    }
    rig.sink.manual.store(false, Ordering::SeqCst);
    assert!(
        rig.sink.completed().len() >= before + windows.len(),
        "plan {plan} completed"
    );
}

/// The real 120 Hz log: three windows of one application, each about 70 Hz
/// written and 14 ms late. Six layout changes never put it in fallback, and the
/// book keeps one folded record per layout change.
#[test]
fn three_windows_at_70_hz_and_14_ms_never_fall_back_and_fold_into_one_record_per_plan() {
    let mut rig = clocked_rig();
    for plan in 1..=6 {
        run_plan(&mut rig, plan, 1, &[AT_70_HZ; 3]);
        assert_eq!(reason(&rig, 1), None, "plan {plan}");
    }
    let records = rig.animator.shared().book().records(app(1));
    assert_eq!(records.len(), 6, "one record per plan: {records:?}");
    for (index, record) in records.iter().enumerate() {
        assert_eq!(record.plan_sequence, index as u64 + 1);
        assert!(
            record.rate_hz > 60.0 && record.rate_hz < 80.0,
            "about 70 Hz: {record:?}"
        );
        assert!(!record.rate_bad() && !record.lag_bad(), "{record:?}");
        assert!(
            record.end_lag >= ms(13) && record.end_lag <= ms(30),
            "{record:?}"
        );
        assert_eq!(
            record.frames.written + record.frames.dropped,
            record.frames.scheduled
        );
        assert!(
            record.frames.written > 30 && record.frames.dropped > 20,
            "the frames of the three windows are summed: {record:?}"
        );
    }
}

/// Three windows in one plan, only one of them slow: that plan counts once.
/// A second slow plan triggers the minimum-rate fallback.
#[test]
fn one_slow_window_in_a_plan_counts_once_and_a_second_slow_plan_triggers() {
    let mut rig = clocked_rig();
    let mixed = [AT_70_HZ, AT_12_HZ, AT_70_HZ];
    run_plan(&mut rig, 1, 1, &mixed);
    assert_eq!(
        reason(&rig, 1),
        None,
        "three slow-ish windows are still one record"
    );
    let records = rig.animator.shared().book().records(app(1));
    assert_eq!(records.len(), 1, "{records:?}");
    assert!(records[0].rate_hz < 15.0 && records[0].rate_bad(), "{records:?}");
    run_plan(&mut rig, 2, 1, &[AT_70_HZ; 3]);
    assert_eq!(reason(&rig, 1), None, "1 of 2");
    run_plan(&mut rig, 3, 1, &mixed);
    assert_eq!(reason(&rig, 1), Some(FallbackReason::BelowMinimumRate), "2 of 3");
    assert_eq!(rig.animator.shared().book().records(app(1)).len(), 3);
}

/// A healthy application that writes about 20 frames per second per window and
/// ends about 120 ms late must keep animating: the fallback is a last resort.
#[test]
fn twenty_hz_and_120_ms_of_end_lag_never_trigger_the_fallback() {
    let mut rig = clocked_rig();
    for plan in 1..=6 {
        run_plan(&mut rig, plan, 1, &[AT_20_HZ; 3]);
        assert_eq!(reason(&rig, 1), None, "plan {plan}");
    }
    let records = rig.animator.shared().book().records(app(1));
    assert!(
        records.iter().all(|r| !r.rate_bad() && !r.lag_bad()),
        "{records:?}"
    );
}

// ------------------------------------------------- recovery and the two kinds

#[test]
fn a_rate_or_lag_fallback_cools_down_doubling_to_ten_minutes_and_a_healthy_record_resets_it() {
    let set = FallbackSet::default();
    let t0 = Instant::now();
    let kind = MotionKind::Resize;
    let mut now = t0;
    let mut expected = FIRST_COOLDOWN;
    for _ in 0..6 {
        assert!(set.insert(app(1), kind, FallbackReason::BelowMinimumRate, now));
        assert_eq!(set.entries()[0].until, Some(now + expected));
        assert!(
            set.expire(now + expected.saturating_sub(ms(1))).is_empty(),
            "not before"
        );
        let left = set.expire(now + expected);
        assert_eq!(left.len(), 1);
        assert!(!set.contains(app(1), kind));
        now += expected;
        expected = (expected * 2).min(MAX_COOLDOWN);
    }
    assert_eq!(expected, MAX_COOLDOWN, "60, 120, 240, 480, 600, 600 s");
    // A healthy animation in between: back to 60 s.
    set.healthy(app(1), kind);
    assert!(set.insert(app(1), kind, FallbackReason::LateEnd, now));
    assert_eq!(set.entries()[0].until, Some(now + FIRST_COOLDOWN));
    // The other kind keeps its own cool-down.
    assert!(set.insert(app(1), MotionKind::Move, FallbackReason::Overflow, now));
    assert_eq!(set.entries()[0].until, Some(now + FIRST_COOLDOWN));
}

#[test]
fn an_enhanced_ui_fallback_never_cools_down_and_replaces_a_recoverable_one() {
    let set = FallbackSet::default();
    let now = Instant::now();
    set.insert(app(1), MotionKind::Move, FallbackReason::NoFrameClock, now);
    set.insert(
        app(1),
        MotionKind::Move,
        FallbackReason::ReEnablesEnhancedUi,
        now,
    );
    assert_eq!(set.entries()[0].reason, FallbackReason::ReEnablesEnhancedUi);
    // The replacement is noted: the log says it is now sticky.
    let events = set.take_events();
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(
        events[1],
        FallbackEvent::Entered(FallbackEntry {
            application: app(1),
            kind: MotionKind::Move,
            reason: FallbackReason::ReEnablesEnhancedUi,
            until: None,
        })
    );
    assert_eq!(set.entries()[0].until, None);
    assert!(set.expire(now + Duration::from_hours(24)).is_empty());
    assert!(set.contains(app(1), MotionKind::Move));
}

#[test]
fn entries_and_exits_are_noted_once_for_the_log() {
    let set = FallbackSet::default();
    let now = Instant::now();
    set.insert(app(1), MotionKind::Resize, FallbackReason::LateEnd, now);
    set.insert(app(1), MotionKind::Resize, FallbackReason::LateEnd, now);
    let entry = set.entries()[0];
    assert_eq!(set.take_events(), [FallbackEvent::Entered(entry)]);
    set.expire(now + FIRST_COOLDOWN);
    assert_eq!(set.take_events(), [FallbackEvent::Left(entry)]);
    assert!(set.take_events().is_empty());
}

#[test]
fn leaving_the_fallback_clears_that_kinds_history_so_two_new_bad_records_are_needed() {
    let sink = FakeSink::new();
    let control = control(&sink);
    control.configure(&enabled(true)).expect("on");
    let book = || control.shared().book().records(app(1));
    let mut moved = record(60.0, 0);
    moved.kind = MotionKind::Move;
    control.shared().book().record(app(1), moved);
    control.shared().book().record(app(1), record(10.0, 0));
    assert_eq!(
        control.shared().book().record(app(1), record(10.0, 0)),
        Some(Trigger::Rate)
    );
    let t0 = Instant::now();
    control.shared().fallback.insert(
        app(1),
        MotionKind::Resize,
        FallbackReason::BelowMinimumRate,
        t0,
    );
    assert!(control.expire_fallbacks(t0 + ms(59_999)).is_empty());
    assert_eq!(control.expire_fallbacks(t0 + FIRST_COOLDOWN).len(), 1);
    assert!(control.fallback_apps().is_empty());
    assert_eq!(
        book(),
        vec![moved],
        "the resize history is cleared, the move one kept"
    );
    assert_eq!(
        control.shared().book().record(app(1), record(10.0, 0)),
        None,
        "1 of 1"
    );
    assert_eq!(
        control.shared().book().record(app(1), record(10.0, 0)),
        Some(Trigger::Rate),
        "2 of 2"
    );
    control.stop();
    assert!(control.join_within(Duration::from_secs(5)));
}

#[test]
fn the_policy_counts_each_kind_separately() {
    let mut book = MetricsBook::default();
    let moved = |rate| {
        let mut r = record(rate, 0);
        r.kind = MotionKind::Move;
        r
    };
    assert_eq!(book.record(app(1), record(10.0, 0)), None);
    assert_eq!(
        book.record(app(1), moved(10.0)),
        None,
        "one bad move, one bad resize"
    );
    assert_eq!(book.record(app(1), moved(60.0)), None);
    assert_eq!(
        book.record(app(1), record(10.0, 0)),
        Some(Trigger::Rate),
        "two bad resizes"
    );
    assert_eq!(
        book.record(app(1), moved(10.0)),
        Some(Trigger::Rate),
        "two bad moves of 3"
    );
}

/// A slow application that falls back for resizes still animates a move, and
/// its resize records do not count against its moves.
#[test]
fn an_application_that_cannot_animate_resizes_still_animates_moves() {
    let mut rig = clocked_rig();
    slow(&mut rig, 1, 1);
    slow(&mut rig, 2, 1);
    assert!(rig.animator.shared().fallback.contains(app(1), MotionKind::Resize));
    assert!(!rig.animator.shared().fallback.contains(app(1), MotionKind::Move));
    let mut moving = app_job(3, 1);
    moving.components = crate::outcome::FrameComponents { position: true, size: false };
    run_animation(&mut rig, moving, true, 1, Duration::ZERO);
    let of_move: Vec<_> = rig.sink.sent().into_iter().filter(|s| s.plan == 3).collect();
    assert!(of_move.len() >= 8, "the move animated: {}", of_move.len());
    assert_eq!(of_move[0].stage, FrameStage::First);
}

/// A resize running when its kind falls back is cut to one final write; a
/// move of the same application is not.
#[test]
fn a_mid_animation_fallback_only_cuts_short_the_kind_that_fell_back() {
    let mut rig = clocked_rig();
    let t0 = rig.now;
    let mut moving = app_job(1, 1);
    moving.components = crate::outcome::FrameComponents { position: true, size: false };
    rig.admit(moving);
    for k in 1..=3_u32 {
        rig.now = t0 + PERIOD * k;
        let at = rig.now;
        rig.ticks.insert(D1, at);
        rig.pump();
    }
    rig.animator.shared().fallback.insert(
        app(1),
        MotionKind::Resize,
        FallbackReason::BelowMinimumRate,
        rig.now,
    );
    rig.now = t0 + PERIOD * 4;
    let at = rig.now;
    rig.ticks.insert(D1, at);
    rig.pump();
    let last = *rig.sink.sent().last().expect("a frame");
    assert_eq!(last.stage, FrameStage::Intermediate, "the move keeps animating");
}
