//! Pure curves, durations and the animation stepper over a fake clock and a
//! fake native writer. Nothing here moves a window or starts a timer.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use stache_macos::ApplicationId;
use stache_macos::ax::{AxPoint, AxReading, AxSize, FrameRead, WriteStatus};
use stache_macos::windows::{
    AdapterId, IssuedKind, NativeStatus, NotIssuedReason, RuntimeEpoch, WindowActionOutcome,
    WindowInstance,
};
use stache_sdk::CancellationToken;

use super::easing::{ease_in, ease_in_out, ease_linear, ease_out, ease_out_expo, lerp};
use super::*;
use crate::actor::{Clock, SystemClock};
use crate::config::{AnimationConfig, EasingType};
use crate::geometry::Rect;
use crate::outcome::{FrameComponents, OperationOutcome, classify_frame};
use crate::state::Stamp;

fn near(a: f64, b: f64) -> bool { (a - b).abs() < 1e-9 }

fn window(serial: u64) -> WindowInstance {
    static ADAPTER: std::sync::OnceLock<AdapterId> = std::sync::OnceLock::new();
    WindowInstance::for_testing(
        *ADAPTER.get_or_init(AdapterId::for_testing),
        RuntimeEpoch::for_testing(1),
        ApplicationId::for_testing(1),
        serial,
    )
}

const BOTH: FrameComponents = FrameComponents { position: true, size: true };
const FROM: Rect = Rect::new(0.0, 0.0, 100.0, 100.0);
const TO: Rect = Rect::new(1000.0, 0.0, 100.0, 100.0);
const PERIOD: Duration = Duration::from_millis(16);

fn settings(easing: EasingType) -> AnimationSettings {
    AnimationSettings::from_config(&AnimationConfig {
        enabled: true,
        duration: 200,
        easing,
    })
}

fn job(plan: u64, from: Rect, target: Rect, easing: EasingType) -> AnimationJob {
    AnimationJob {
        stamp: Stamp {
            epoch: RuntimeEpoch::for_testing(1),
            registry: 1,
        },
        revision: 1,
        plan_sequence: plan,
        window: window(1),
        from,
        target,
        components: BOTH,
        period: PERIOD,
        settings: settings(easing),
    }
}

fn read(frame: Rect) -> FrameRead {
    FrameRead {
        position: AxReading::Value(AxPoint { x: frame.x, y: frame.y }),
        size: AxReading::Value(AxSize {
            width: frame.width,
            height: frame.height,
        }),
    }
}

fn applied(frame: Rect) -> WindowActionOutcome {
    WindowActionOutcome::Issued {
        kind: IssuedKind::Applied,
        native: NativeStatus::Frame {
            position: WriteStatus::Written,
            size: WriteStatus::Written,
        },
        observed: Some(read(frame)),
    }
}

/// A fake native writer: echoes every write as applied unless scripted.
struct FakeSink {
    writes: Mutex<Vec<(u64, Rect)>>,
    completed: Mutex<Vec<(u64, WindowActionOutcome)>>,
    settles: AtomicUsize,
    settle_frame: Mutex<Option<Rect>>,
    script: Mutex<VecDeque<Option<WindowActionOutcome>>>,
    sealed: AtomicBool,
    hold: Mutex<()>,
}

impl FakeSink {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            writes: Mutex::new(Vec::new()),
            completed: Mutex::new(Vec::new()),
            settles: AtomicUsize::new(0),
            settle_frame: Mutex::new(None),
            script: Mutex::new(VecDeque::new()),
            sealed: AtomicBool::new(false),
            hold: Mutex::new(()),
        })
    }

    fn writes(&self) -> Vec<(u64, Rect)> { self.writes.lock().expect("writes").clone() }

    fn completed(&self) -> Vec<(u64, WindowActionOutcome)> {
        self.completed.lock().expect("completed").clone()
    }
}

impl AnimationSink for FakeSink {
    fn write(&self, job: &AnimationJob, frame: Rect) -> WindowActionOutcome {
        drop(self.hold.lock().expect("hold"));
        if self.sealed.load(Ordering::SeqCst) {
            return WindowActionOutcome::NotIssued(NotIssuedReason::Sealed);
        }
        self.writes.lock().expect("writes").push((job.plan_sequence, frame));
        self.script
            .lock()
            .expect("script")
            .pop_front()
            .flatten()
            .unwrap_or_else(|| applied(frame))
    }

    fn settle(&self, _job: &AnimationJob) -> Option<FrameRead> {
        self.settles.fetch_add(1, Ordering::SeqCst);
        self.settle_frame.lock().expect("settle").map(read)
    }

    fn complete(&self, job: &AnimationJob, outcome: WindowActionOutcome) {
        self.completed.lock().expect("completed").push((job.plan_sequence, outcome));
    }

    fn stop_reason(&self) -> NotIssuedReason {
        if self.sealed.load(Ordering::SeqCst) {
            NotIssuedReason::Sealed
        } else {
            NotIssuedReason::Cancelled
        }
    }
}

/// Advances `animator` in frame steps until it is idle, at most `limit` steps.
fn run(animator: &mut Animator, mut now: Instant, limit: usize) -> Instant {
    for _ in 0..limit {
        let Some(next) = animator.tick(now) else { return now };
        now = next.max(now);
    }
    now
}

// ------------------------------------------------------------------- curves

#[test]
fn easing_matches_the_legacy_formulas() {
    assert!(near(ease_linear(0.3), 0.3));
    assert!(near(ease_in(0.5), 0.125));
    assert!(near(ease_out(0.25), 0.578_125));
    assert!(near(ease_out(0.5), 0.875));
    assert!(near(ease_in_out(0.25), 0.0625));
    assert!(near(ease_in_out(0.5), 0.5));
    assert!(near(ease_in_out(0.75), 0.9375));
    assert!(near(ease_out_expo(0.5), 0.968_75));
    assert!(near(ease_out_expo(1.0), 1.0));
    assert!(near(lerp(50.0, 150.0, 0.25), 75.0));
    for easing in [
        EasingType::Linear,
        EasingType::EaseIn,
        EasingType::EaseOut,
        EasingType::EaseInOut,
        EasingType::EaseOutExpo,
        EasingType::Spring,
    ] {
        assert!(near(apply_easing(0.0, easing), 0.0) || easing == EasingType::EaseOutExpo);
        assert!(near(apply_easing(1.0, easing), 1.0), "{easing:?}");
    }
    assert!(
        near(apply_easing(0.5, EasingType::Spring), 0.5),
        "spring is simulated, not eased"
    );
}

#[test]
fn the_spring_matches_the_damped_oscillator_and_settles() {
    let spring = SpringState::new(Duration::from_millis(200));
    let (omega, zeta) = (5.5_f64 / 0.2, 0.85_f64);
    let omega_d = omega * (1.0 - zeta * zeta).sqrt();
    for t in [0.0_f64, 0.05, 0.1, 0.2, 0.5] {
        let expected = 1.0
            - (-zeta * omega * t).exp()
                * ((omega_d * t).cos() + zeta / (1.0 - zeta * zeta).sqrt() * (omega_d * t).sin());
        assert!(near(spring.calculate_position(t), expected), "t={t}");
    }
    assert!(near(spring.calculate_position(0.0), 0.0));
    let mut spring = SpringState::new(Duration::from_millis(100));
    let settled = (0..100).any(|_| spring.update(0.01).1);
    assert!(settled, "the spring converges");
    // Critically damped and overdamped branches stay finite and end at 1.
    assert!(near(SpringState::critically_damped_position(0.0, 33.0), 0.0));
    assert!((SpringState::critically_damped_position(5.0, 33.0) - 1.0).abs() < 1e-6);
    assert!((SpringState::overdamped_position(5.0, 33.0, 1.5) - 1.0).abs() < 1e-6);
    let params = SpringParams::from_duration(Duration::ZERO);
    assert!(params.omega_0.is_finite(), "a zero duration is floored");
}

#[test]
fn a_transition_measures_and_interpolates_like_legacy() {
    let t = Transition::new(FROM, Rect::new(500.0, 10.0, 110.0, 120.0));
    assert!(near(t.max_distance(), 500.0));
    assert!(t.involves_resize());
    assert!(!Transition::new(FROM, Rect::new(50.0, 50.0, 100.0, 100.0)).involves_resize());
    let half = Transition::new(FROM, Rect::new(100.0, 200.0, 200.0, 300.0)).interpolate(0.5);
    assert_eq!(half, Rect::new(50.0, 100.0, 150.0, 200.0));
    // The progress is clamped, so a spring overshoot never passes the target.
    assert_eq!(t.interpolate(1.4), t.to);
    assert_eq!(t.interpolate(-1.0), t.from);
}

#[test]
fn the_default_duration_is_200_ms_and_scales_below_500_px() {
    let defaults = AnimationConfig::default();
    assert!(!defaults.enabled, "animations stay off by default");
    assert_eq!(defaults.duration, 200);
    let settings = AnimationSettings::default();
    assert_eq!(settings.duration, Duration::from_millis(200));
    assert_eq!(settings.calculate_duration(500.0), Duration::from_millis(200));
    assert_eq!(settings.calculate_duration(5000.0), Duration::from_millis(200));
    assert_eq!(settings.calculate_duration(20.0), Duration::from_millis(50));
    assert_eq!(settings.calculate_duration(0.0), Duration::from_millis(50));
    // 50 + 150 * sqrt(100 / 500) = 117.08
    assert_eq!(settings.calculate_duration(100.0), Duration::from_millis(117));
    let mid = settings.calculate_duration(250.0);
    assert!(mid > Duration::from_millis(117) && mid < Duration::from_millis(200));
    let clamp = |duration| {
        AnimationSettings::from_config(&AnimationConfig {
            enabled: true,
            duration,
            easing: EasingType::EaseOut,
        })
        .duration
    };
    assert_eq!(clamp(10), Duration::from_millis(100));
    assert_eq!(clamp(60_000), Duration::from_millis(1000));
}

#[test]
fn only_an_enabled_meaningful_move_is_animated() {
    let on = AnimationConfig {
        enabled: true,
        ..AnimationConfig::default()
    };
    let off = AnimationConfig::default();
    assert!(!wants_animation(&off, &FROM, &TO), "off by default");
    assert!(wants_animation(&on, &FROM, &TO));
    assert!(!wants_animation(&on, &FROM, &Rect::new(4.0, 0.0, 100.0, 100.0)));
    assert!(wants_animation(&on, &FROM, &Rect::new(5.0, 0.0, 100.0, 100.0)));
    assert!(
        wants_animation(&on, &FROM, &Rect::new(0.0, 0.0, 100.0, 140.0)),
        "a resize counts"
    );
}

// ----------------------------------------------------------------- stepper

#[test]
fn an_animation_writes_frames_then_the_exact_target_and_reads_back_once() {
    let sink = FakeSink::new();
    *sink.settle_frame.lock().expect("settle") = Some(TO);
    let mut animator = Animator::new(Arc::clone(&sink) as Arc<dyn AnimationSink>);
    let start = Instant::now();
    animator.admit(job(7, FROM, TO, EasingType::Linear), start);
    run(&mut animator, start, 100);
    let writes = sink.writes();
    assert!(
        writes.len() >= 8,
        "intermediate frames were written: {}",
        writes.len()
    );
    assert!(writes.iter().all(|(plan, _)| *plan == 7));
    let xs: Vec<f64> = writes.iter().map(|(_, r)| r.x).collect();
    assert!(xs.windows(2).all(|w| w[0] <= w[1]), "monotonic travel {xs:?}");
    assert_eq!(
        writes.last().expect("last").1,
        TO,
        "the final write is the exact target"
    );
    assert_eq!(sink.settles.load(Ordering::SeqCst), 1, "one bounded readback");
    let done = sink.completed();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].0, 7);
    let outcome = classify_frame(window(1), 7, TO, BOTH, &done[0].1);
    assert!(
        matches!(outcome, OperationOutcome::Applied { .. }),
        "{outcome:?}"
    );
    assert_eq!(outcome.actual(), Some(TO), "the readback is reported");
    assert!(animator.is_empty());
}

#[test]
fn a_small_move_is_written_once_without_intermediate_frames() {
    let sink = FakeSink::new();
    let mut animator = Animator::new(Arc::clone(&sink) as Arc<dyn AnimationSink>);
    let start = Instant::now();
    let target = Rect::new(2.0, 0.0, 100.0, 100.0);
    animator.admit(job(1, FROM, target, EasingType::EaseOut), start);
    run(&mut animator, start, 20);
    assert_eq!(sink.writes(), vec![(1, target)]);
    assert_eq!(sink.completed().len(), 1);
}

#[test]
fn a_retarget_starts_from_the_confirmed_position_not_the_destination() {
    let sink = FakeSink::new();
    let mut animator = Animator::new(Arc::clone(&sink) as Arc<dyn AnimationSink>);
    let start = Instant::now();
    animator.admit(job(1, FROM, TO, EasingType::Linear), start);
    let mut now = start;
    for _ in 0..5 {
        now = animator.tick(now).expect("running").max(now);
    }
    let confirmed = animator.confirmed(window(1)).expect("a readback was seen");
    assert!(
        confirmed.x > 0.0 && confirmed.x < 1000.0,
        "mid-flight {confirmed:?}"
    );

    // The planner still believes the window is at FROM and wants it back there.
    animator.admit(
        job(2, FROM, Rect::new(0.0, 300.0, 100.0, 100.0), EasingType::Linear),
        now,
    );
    let done = sink.completed();
    assert_eq!(done.len(), 1, "the interrupted job is acknowledged once");
    assert_eq!(done[0].0, 1, "for its own plan");
    assert!(matches!(
        done[0].1,
        WindowActionOutcome::NotIssued(NotIssuedReason::Superseded)
    ));
    let before = sink.writes().len();
    now = animator.tick(now + PERIOD).expect("running").max(now);
    let first = sink.writes()[before].1;
    assert_eq!(sink.writes()[before].0, 2);
    assert!(
        first.x > confirmed.x * 0.5,
        "the new animation starts at the confirmed x ({}), not at the planner's 0: {first:?}",
        confirmed.x
    );
    run(&mut animator, now, 100);
    assert_eq!(
        sink.writes().last().expect("last").1,
        Rect::new(0.0, 300.0, 100.0, 100.0)
    );
}

#[test]
fn a_b_a_never_lets_an_old_job_mark_a_successor_applied() {
    let sink = FakeSink::new();
    let mut animator = Animator::new(Arc::clone(&sink) as Arc<dyn AnimationSink>);
    let start = Instant::now();
    let b = Rect::new(600.0, 0.0, 100.0, 100.0);
    animator.admit(job(1, FROM, b, EasingType::Linear), start);
    let mut now = animator.tick(start + PERIOD).expect("running");
    animator.admit(job(2, FROM, FROM, EasingType::Linear), now);
    now = animator.tick(now + PERIOD).expect("running");
    animator.admit(job(3, FROM, b, EasingType::Linear), now);
    run(&mut animator, now, 200);
    let done = sink.completed();
    let plans: Vec<u64> = done.iter().map(|(plan, _)| *plan).collect();
    assert_eq!(
        plans,
        vec![1, 2, 3],
        "each job acknowledges exactly its own plan, once"
    );
    for (plan, outcome) in &done[..2] {
        assert!(
            matches!(
                outcome,
                WindowActionOutcome::NotIssued(NotIssuedReason::Superseded)
            ),
            "plan {plan} is a typed no-send, never applied: {outcome:?}"
        );
    }
    assert!(matches!(
        classify_frame(window(1), 3, b, BOTH, &done[2].1),
        OperationOutcome::Applied { .. }
    ));
}

#[test]
fn a_refused_target_stops_animating_with_no_final_snap() {
    let sink = FakeSink::new();
    sink.script.lock().expect("script").extend([
        None,
        Some(WindowActionOutcome::Issued {
            kind: IssuedKind::Rejected,
            native: NativeStatus::Frame {
                position: WriteStatus::Failed(stache_macos::ax::AxError::CannotComplete),
                size: WriteStatus::NotRequested,
            },
            observed: None,
        }),
    ]);
    let mut animator = Animator::new(Arc::clone(&sink) as Arc<dyn AnimationSink>);
    let start = Instant::now();
    animator.admit(job(1, FROM, TO, EasingType::Linear), start);
    run(&mut animator, start, 100);
    assert_eq!(sink.writes().len(), 2, "no retry and no final snap loop");
    let done = sink.completed();
    assert_eq!(done.len(), 1);
    assert!(matches!(done[0].1, WindowActionOutcome::Issued {
        kind: IssuedKind::Rejected,
        ..
    }));
    assert!(animator.is_empty());
    assert_eq!(animator.tick(start + Duration::from_secs(5)), None);
    assert_eq!(sink.writes().len(), 2);
}

#[test]
fn an_unresizable_window_stops_at_the_first_frame_it_cannot_follow() {
    let sink = FakeSink::new();
    // The application keeps its size: the readback never shows the asked size.
    sink.script
        .lock()
        .expect("script")
        .push_back(Some(applied(Rect::new(10.0, 0.0, 300.0, 300.0))));
    let mut animator = Animator::new(Arc::clone(&sink) as Arc<dyn AnimationSink>);
    let start = Instant::now();
    animator.admit(
        job(1, FROM, Rect::new(800.0, 0.0, 400.0, 400.0), EasingType::Linear),
        start,
    );
    run(&mut animator, start, 100);
    assert_eq!(sink.writes().len(), 1, "stopped, no retry fight");
    assert_eq!(sink.completed().len(), 1);
    assert!(animator.is_empty());
}

#[test]
fn a_seal_mid_animation_writes_nothing_more() {
    let sink = FakeSink::new();
    let mut animator = Animator::new(Arc::clone(&sink) as Arc<dyn AnimationSink>);
    let start = Instant::now();
    animator.admit(job(1, FROM, TO, EasingType::EaseOut), start);
    let now = animator.tick(start + PERIOD).expect("running");
    let written = sink.writes().len();
    sink.sealed.store(true, Ordering::SeqCst);
    animator.tick(now + PERIOD);
    assert_eq!(
        sink.writes().len(),
        written,
        "the sealed write was refused, nothing sent"
    );
    assert!(matches!(
        sink.completed()[0].1,
        WindowActionOutcome::NotIssued(NotIssuedReason::Sealed)
    ));
    assert!(animator.is_empty());
}

#[test]
fn an_abort_acknowledges_every_job_with_the_stop_reason_and_writes_nothing() {
    let sink = FakeSink::new();
    let mut animator = Animator::new(Arc::clone(&sink) as Arc<dyn AnimationSink>);
    let start = Instant::now();
    animator.admit(job(1, FROM, TO, EasingType::Spring), start);
    let mut other = job(2, FROM, TO, EasingType::Spring);
    other.window = window(2);
    animator.admit(other, start);
    sink.sealed.store(true, Ordering::SeqCst);
    animator.abort_all();
    assert!(sink.writes().is_empty());
    let done = sink.completed();
    assert_eq!(done.len(), 2);
    assert!(
        done.iter()
            .all(|(_, o)| matches!(o, WindowActionOutcome::NotIssued(NotIssuedReason::Sealed)))
    );
    assert!(animator.is_empty());
}

#[test]
fn a_spring_animation_ends_on_the_exact_target() {
    let sink = FakeSink::new();
    let mut animator = Animator::new(Arc::clone(&sink) as Arc<dyn AnimationSink>);
    let start = Instant::now();
    animator.admit(job(1, FROM, TO, EasingType::Spring), start);
    run(&mut animator, start, 200);
    assert_eq!(sink.writes().last().expect("last").1, TO);
    assert!(
        sink.writes().iter().all(|(_, r)| r.x <= TO.x),
        "no overshoot past the target"
    );
    assert_eq!(sink.completed().len(), 1);
}

// ------------------------------------------------------------------ worker

fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn spawn(sink: &Arc<FakeSink>, cancel: &CancellationToken) -> AnimationWorker {
    AnimationWorker::spawn(
        Arc::clone(sink) as Arc<dyn AnimationSink>,
        Arc::new(SystemClock) as Arc<dyn Clock>,
        cancel,
    )
    .expect("worker")
}

#[test]
fn cancellation_wakes_a_sleeping_worker_promptly() {
    let sink = FakeSink::new();
    let cancel = CancellationToken::new();
    let worker = spawn(&sink, &cancel);
    let mut slow = job(1, FROM, TO, EasingType::EaseOut);
    slow.period = Duration::from_secs(30);
    assert!(worker.submit(slow).is_ok());
    wait_for("the job was admitted", || worker.pending() == 0);
    let begun = Instant::now();
    cancel.cancel();
    assert!(worker.join_within(Duration::from_secs(5)), "the worker ended");
    assert!(begun.elapsed() < Duration::from_secs(2), "woken, not slept out");
    let done = sink.completed();
    assert_eq!(done.len(), 1);
    assert!(matches!(
        done[0].1,
        WindowActionOutcome::NotIssued(NotIssuedReason::Cancelled)
    ));
    assert!(sink.writes().is_empty());
    assert!(
        worker.submit(job(2, FROM, TO, EasingType::Linear)).is_err(),
        "closed"
    );
}

#[test]
fn the_mailbox_keeps_only_the_newest_plan_per_window() {
    let sink = FakeSink::new();
    let cancel = CancellationToken::new();
    let worker = spawn(&sink, &cancel);
    // Hold the thread inside a write so later submissions stay pending.
    let hold = sink.hold.lock().expect("hold");
    let mut first = job(1, FROM, TO, EasingType::Linear);
    first.period = Duration::from_millis(1);
    assert!(worker.submit(first).is_ok());
    wait_for("the worker blocked in a write", || {
        worker.pending() == 0 && sink.completed().is_empty()
    });
    std::thread::sleep(Duration::from_millis(30));
    assert!(worker.submit(job(2, FROM, TO, EasingType::Linear)).is_ok());
    assert!(worker.submit(job(3, FROM, TO, EasingType::Linear)).is_ok());
    assert_eq!(worker.pending(), 1, "the newest plan replaced the older one");
    assert_eq!(
        sink.completed().iter().map(|(plan, _)| *plan).collect::<Vec<_>>(),
        vec![2],
        "the replaced plan was acknowledged as superseded"
    );
    drop(hold);
    cancel.cancel();
    assert!(worker.join_within(Duration::from_secs(5)));
    let plans: Vec<u64> = sink.completed().iter().map(|(plan, _)| *plan).collect();
    assert!(
        plans.contains(&1) && plans.contains(&2) && plans.contains(&3),
        "{plans:?}"
    );
    assert_eq!(plans.len(), 3, "each plan acknowledged exactly once");
}

#[test]
fn the_mailbox_is_bounded() {
    let sink = FakeSink::new();
    let cancel = CancellationToken::new();
    let worker = spawn(&sink, &cancel);
    let hold = sink.hold.lock().expect("hold");
    let mut first = job(1, FROM, TO, EasingType::Linear);
    first.period = Duration::from_millis(1);
    assert!(worker.submit(first).is_ok());
    wait_for("the worker blocked in a write", || worker.pending() == 0);
    std::thread::sleep(Duration::from_millis(30));
    // One window runs; the rest of the cap is pending.
    for n in 0..(MAX_ANIMATED as u64 - 1) {
        let mut j = job(10 + n, FROM, TO, EasingType::Linear);
        j.window = window(100 + n);
        assert!(worker.submit(j).is_ok());
    }
    let mut extra = job(99, FROM, TO, EasingType::Linear);
    extra.window = window(999);
    let refused = worker.submit(extra).expect_err("full: the caller writes directly");
    assert_eq!(
        refused.plan_sequence, 99,
        "the overflow job comes back untouched"
    );
    assert!(
        sink.completed().iter().all(|(plan, _)| *plan != 99),
        "a refused job is not acknowledged by the worker: the caller owns its write"
    );
    // A newer plan for a window the worker already holds is still accepted.
    let mut again = job(100, FROM, TO, EasingType::Linear);
    again.window = window(100);
    assert!(worker.submit(again).is_ok());
    drop(hold);
    cancel.cancel();
    assert!(worker.join_within(Duration::from_secs(5)));
}

#[test]
fn seventeen_windows_all_reach_their_exact_target() {
    let sink = FakeSink::new();
    let cancel = CancellationToken::new();
    let worker = spawn(&sink, &cancel);
    let mut direct = Vec::new();
    for n in 0..=(MAX_ANIMATED as u64) {
        let mut j = job(10 + n, FROM, TO, EasingType::Linear);
        j.window = window(100 + n);
        j.period = Duration::from_millis(2);
        if let Err(back) = worker.submit(j) {
            // The caller (`EffectEnv::run`) writes the exact target itself.
            direct.push(back.plan_sequence);
        }
    }
    assert_eq!(direct.len(), 1, "only the 17th window is refused");
    // While 16 are running or settling a second plan for one of them is accepted.
    let mut second = job(500, FROM, Rect::new(5.0, 5.0, 100.0, 100.0), EasingType::Linear);
    second.window = window(100);
    second.period = Duration::from_millis(2);
    assert!(
        worker.submit(second).is_ok(),
        "a held window is retargeted, never dropped"
    );
    wait_for("every accepted plan acknowledged", || {
        sink.completed().len() > MAX_ANIMATED
    });
    let completed = sink.completed();
    let writes = sink.writes();
    for n in 1..(MAX_ANIMATED as u64) {
        let plan = 10 + n;
        assert!(
            completed.iter().any(|(p, _)| *p == plan),
            "plan {plan} acknowledged"
        );
        assert_eq!(
            writes.iter().rev().find(|(p, _)| *p == plan).map(|(_, f)| *f),
            Some(TO),
            "plan {plan} ended on its exact target"
        );
    }
    cancel.cancel();
    assert!(worker.join_within(Duration::from_secs(5)));
}

#[test]
fn an_animator_at_capacity_writes_the_exact_target_directly() {
    let sink = FakeSink::new();
    let mut animator = Animator::new(Arc::clone(&sink) as Arc<dyn AnimationSink>);
    let now = Instant::now();
    for n in 0..=(MAX_ANIMATED as u64) {
        let mut j = job(10 + n, FROM, TO, EasingType::Linear);
        j.window = window(100 + n);
        animator.admit(j, now);
    }
    assert_eq!(animator.len(), MAX_ANIMATED);
    let overflow = MAX_ANIMATED as u64 + 10;
    assert_eq!(
        sink.writes(),
        vec![(overflow, TO)],
        "the overflow window got its final target at once"
    );
    let completed = sink.completed();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].0, overflow);
    assert!(
        matches!(completed[0].1, WindowActionOutcome::Issued { .. }),
        "not NotIssued(Full)"
    );
}

#[test]
fn a_disabled_control_owns_no_worker_and_follows_the_configuration() {
    let sink = FakeSink::new();
    let cancel = CancellationToken::new();
    let control = AnimationControl::new(
        Arc::clone(&sink) as Arc<dyn AnimationSink>,
        Arc::new(SystemClock),
        cancel,
    );
    assert!(!control.is_running(), "construction is inert");
    control.configure(&AnimationConfig::default()).expect("disabled");
    assert!(!control.is_running(), "default off spawns nothing");
    assert!(control.submit(job(1, FROM, TO, EasingType::Linear)).is_err());

    let on = AnimationConfig {
        enabled: true,
        duration: 300,
        easing: EasingType::Linear,
    };
    control.configure(&on).expect("enable");
    assert!(control.is_running());
    control.configure(&AnimationConfig::default()).expect("disable");
    assert!(
        !control.is_running(),
        "a reload that disables retires the worker"
    );
    assert!(control.submit(job(2, FROM, TO, EasingType::Linear)).is_err());
    assert!(control.join_within(Duration::from_secs(5)));
    assert_eq!(control.retired(), 0);

    control.configure(&on).expect("enable again");
    assert!(control.is_running());
    control.stop();
    assert!(control.join_within(Duration::from_secs(5)));
    control.configure(&on).expect("ignored after a stop");
    assert!(!control.is_running(), "a stopped control never spawns again");
}
