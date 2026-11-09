//! Frame-effect runtime tests: the engine, runner, gate and fake backend
//! together. Fakes only: no real AX, window, display or process.

use stache_macos::ax::{AxPoint, AxReading, AxSize, FrameRead, WriteStatus};
use stache_macos::windows::{IssuedKind, NativeStatus, NotIssuedReason, StaleReason};

use super::*;
use crate::actor::{AppDirectory, DrainOutcome, FrameControl, FrameJob};
use crate::config::TilingConfig;
use crate::geometry::Rect;
use crate::outcome::{FrameComponents, OperationOutcome};

const TARGET: Rect = Rect::new(0.0, 25.0, 1920.0, 1055.0);

const fn read(frame: Rect) -> FrameRead {
    FrameRead {
        position: AxReading::Value(AxPoint { x: frame.x, y: frame.y }),
        size: AxReading::Value(AxSize {
            width: frame.width,
            height: frame.height,
        }),
    }
}

fn issued(frame: Rect, size: WriteStatus, kind: IssuedKind) -> WindowActionOutcome {
    WindowActionOutcome::Issued {
        kind,
        native: NativeStatus::Frame {
            position: WriteStatus::Written,
            size,
        },
        observed: Some(read(frame)),
    }
}

fn applied(frame: Rect) -> WindowActionOutcome {
    issued(frame, WriteStatus::Written, IssuedKind::Applied)
}

fn default_config() -> Arc<TilingConfig> { Arc::new(TilingConfig::default()) }

// ---------------------------------------------------------------- engine rig

impl Rig {
    /// One started rig with one window, a config and everything enumerated.
    pub(super) fn framed() -> (Self, WindowInstance) {
        let mut rig = Self::new("", vec![info(1, "com.a", "A")], 64, 64);
        rig.start();
        rig.complete(app(1));
        let window = rig.token(1, 1);
        rig.created(window, "w", 1);
        rig.ingress
            .submit(Input::Config {
                revision: 1,
                config: default_config(),
            })
            .expect("config");
        rig.pump();
        assert_eq!(rig.snap().phase, Phase::Complete);
        (rig, window)
    }

    fn authorize(&mut self) {
        self.gate.publish(self.feed.stamp.registry, 1);
        self.ingress.submit(Input::FrameWrites(true)).expect("authorize");
        self.pump();
    }
}

#[test]
fn the_engine_writes_through_the_gate_and_settles_only_from_the_readback() {
    let (mut rig, window) = Rig::framed();
    assert_eq!(
        rig.backend.action_count(),
        0,
        "nothing moves before the authority"
    );
    rig.backend.script_action(applied(TARGET));
    rig.authorize();
    assert_eq!(rig.backend.action_count(), 1);
    let snap = rig.snap();
    let status = snap.frames.iter().find(|s| s.window == window).expect("frame status");
    assert!(
        matches!(status.outcome, Some(OperationOutcome::Applied { .. })),
        "{status:?}"
    );
    assert_eq!(status.actual, Some(TARGET));
    assert!(snap.plan_sequence >= 1);
    assert_eq!(rig.frames.outstanding(), 0);
}

#[test]
fn own_echoes_and_periodic_refreshes_do_not_start_another_attempt() {
    let (mut rig, window) = Rig::framed();
    rig.backend.script_action(issued(
        Rect::new(0.0, 25.0, 1000.0, 700.0),
        WriteStatus::Failed(stache_macos::ax::AxError::ActionUnsupported),
        IssuedKind::Partial,
    ));
    rig.authorize();
    assert_eq!(rig.backend.action_count(), 1);
    let snap = rig.snap();
    let status = snap.frames.iter().find(|s| s.window == window).expect("status");
    assert!(matches!(status.outcome, Some(OperationOutcome::Partial { .. })));
    assert_eq!(status.actual, Some(Rect::new(0.0, 25.0, 1000.0, 700.0)));

    // The window echoes the frame the readback already recorded, repeatedly.
    let mut echo = snapshot(window, "w", 1);
    echo.frame = FrameRead {
        position: AxReading::Value(AxPoint { x: 0.0, y: 25.0 }),
        size: AxReading::Value(AxSize { width: 1000.0, height: 700.0 }),
    };
    rig.backend.script_snapshot(window, SnapshotOutcome::Snapshot(Box::new(echo)));
    for _ in 0..4 {
        rig.push(WindowEventKind::Window {
            window,
            change: WindowChange::Resized,
        });
        rig.clock.advance(Duration::from_millis(40));
        rig.pump();
    }
    assert_eq!(
        rig.backend.action_count(),
        1,
        "no retry loop, no fighting the app"
    );
}

#[test]
fn a_user_command_is_the_only_thing_that_retries_a_refused_frame() {
    let (mut rig, _window) = Rig::framed();
    rig.backend
        .script_action(WindowActionOutcome::NotIssued(NotIssuedReason::Sealed));
    rig.authorize();
    assert_eq!(rig.backend.action_count(), 1);
    rig.pump();
    assert_eq!(rig.backend.action_count(), 1);
    rig.backend.script_action(applied(TARGET));
    rig.ingress.submit(Input::Relayout).expect("relayout");
    rig.pump();
    assert_eq!(rig.backend.action_count(), 2);
}

// ----------------------------------------------------------- frame control

fn control(
    backend: &FakeWindowBackend,
) -> (Arc<FrameControl>, Arc<GenerationCell>, WindowInstance) {
    let gate = Arc::new(GenerationCell::new());
    let sink = EventQueue::new(8);
    let epoch = backend.install(sink, Arc::clone(&gate), None).expect("install");
    let control = Arc::new(FrameControl::new(
        Arc::clone(&gate),
        Arc::new(ManualClock::default()) as Arc<dyn crate::actor::Clock>,
    ));
    let window = WindowInstance::for_testing(backend.adapter(), epoch, app(1), 1);
    (control, gate, window)
}

fn job(window: WindowInstance, plan: u64) -> FrameJob {
    FrameJob {
        generation: 1,
        revision: 1,
        plan_sequence: plan,
        window,
        target: TARGET,
        components: FrameComponents { position: true, size: true },
    }
}

#[test]
fn a_superseded_or_cancelled_queued_write_is_inert() {
    let backend = FakeWindowBackend::default();
    let (control, gate, window) = control(&backend);
    gate.publish(1, 1);
    let cancel = CancellationToken::new();
    control.note_dispatched(window, 1);
    control.note_dispatched(window, 2);
    // Plan one was replaced before it ran.
    assert_eq!(
        control.execute(&backend, &cancel, &job(window, 1)),
        WindowActionOutcome::NotIssued(NotIssuedReason::Superseded)
    );
    control.cancel(window);
    assert_eq!(
        control.execute(&backend, &cancel, &job(window, 2)),
        WindowActionOutcome::NotIssued(NotIssuedReason::Superseded)
    );
    assert_eq!(backend.action_count(), 0, "inert writes never reach the backend");
    control.finish(window, 1);
    control.finish(window, 2);
    assert_eq!(control.outstanding(), 0);
}

#[test]
fn the_gate_facts_are_checked_before_every_dispatch() {
    let backend = FakeWindowBackend::default();
    let (control, gate, window) = control(&backend);
    let cancel = CancellationToken::new();
    control.note_dispatched(window, 1);
    // Nothing published: not live.
    assert_eq!(
        control.execute(&backend, &cancel, &job(window, 1)),
        WindowActionOutcome::NotIssued(NotIssuedReason::GenerationInvalid)
    );
    gate.publish(2, 1);
    assert_eq!(
        control.execute(&backend, &cancel, &job(window, 1)),
        WindowActionOutcome::NotIssued(NotIssuedReason::GenerationInvalid),
        "another registry's generation"
    );
    gate.publish(1, 1);
    backend.script_action(applied(TARGET));
    assert!(matches!(
        control.execute(&backend, &cancel, &job(window, 1)),
        WindowActionOutcome::Issued { kind: IssuedKind::Applied, .. }
    ));
    // The cancelled caller token never sends.
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert_eq!(
        control.execute(&backend, &cancelled, &job(window, 1)),
        WindowActionOutcome::NotIssued(NotIssuedReason::Cancelled)
    );
    gate.seal();
    assert_eq!(
        control.execute(&backend, &cancel, &job(window, 1)),
        WindowActionOutcome::NotIssued(NotIssuedReason::Sealed)
    );
    assert_eq!(backend.action_count(), 1);
}

#[test]
fn drain_has_a_finite_timeout_and_wakes_on_cancellation() {
    let backend = FakeWindowBackend::default();
    let (control, _gate, window) = control(&backend);
    assert_eq!(
        control.drain(Duration::from_millis(5), &CancellationToken::new()),
        DrainOutcome::Drained
    );
    control.note_dispatched(window, 1);
    assert_eq!(
        control.drain(Duration::from_millis(30), &CancellationToken::new()),
        DrainOutcome::TimedOut { outstanding: 1 }
    );
    // A long drain is cut short by cancellation, not by its timeout.
    let cancel = CancellationToken::new();
    let waker = cancel.clone();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        waker.cancel();
    });
    let started = Instant::now();
    assert_eq!(
        control.drain(Duration::from_secs(30), &cancel),
        DrainOutcome::Cancelled { outstanding: 1 }
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    thread.join().expect("waker");
    // Finishing the write completes a drain that waits on another thread.
    let finisher = Arc::clone(&control);
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        finisher.finish(window, 1);
    });
    assert_eq!(
        control.drain(Duration::from_secs(30), &CancellationToken::new()),
        DrainOutcome::Drained
    );
    thread.join().expect("finisher");
    // A write that never got queued does not stay outstanding.
    control.note_dispatched(window, 3);
    control.note_not_queued(window, 3);
    assert_eq!(control.outstanding(), 0);
}

// ----------------------------------------------------------- real threads

struct Live {
    runtime: TrackingRuntime,
    backend: Arc<FakeWindowBackend>,
    windows: Vec<WindowInstance>,
}

fn live(count: u32) -> Live {
    let backend = Arc::new(FakeWindowBackend::default());
    let directory = Arc::new(FakeAppDirectory::default());
    *directory.apps.lock().expect("apps") = vec![info(1, "com.a", "A")];
    let runtime = TrackingRuntime::start(
        Arc::clone(&backend) as Arc<dyn WindowBackend>,
        directory as Arc<dyn AppDirectory>,
        &TrackingOptions::new(""),
    )
    .expect("start");
    let mut feed = Feed::new(runtime.epoch().value());
    feed.adapter = backend.adapter();
    runtime.submit_displays(None, one_display()).expect("displays");
    wait_until("tracked", || {
        backend.calls().iter().any(|call| matches!(call, FakeBackendCall::Track(..)))
    });
    let sink = backend.sink().expect("sink");
    let mut windows = Vec::new();
    for serial in 1..=count {
        let token = feed.token(1, u64::from(serial));
        assert!(sink.push(feed.event(WindowEventKind::Snapshot {
            snapshot: snapshot(token, "w", serial),
            origin: SnapshotOrigin::Created,
        })));
        windows.push(token);
    }
    assert!(sink.push(feed.event(WindowEventKind::EnumerationComplete {
        application: app(1),
        windows: count as usize,
        truncated: false,
    })));
    wait_until("complete", || {
        let snap = runtime.snapshot();
        snap.phase == Phase::Complete && snap.windows.len() == count as usize
    });
    runtime.submit_config(1, default_config()).expect("config");
    wait_until("config", || runtime.snapshot().config_revision == 1);
    Live { runtime, backend, windows }
}

#[test]
fn a_runtime_writes_nothing_until_authorized_and_never_after_the_seal() {
    let live = live(1);
    assert!(!live.runtime.is_authorized());
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(live.backend.action_count(), 0);
    live.backend.script_action(applied(TARGET));
    assert!(live.runtime.authorize(1));
    assert!(live.runtime.is_authorized());
    wait_until("one write", || live.backend.action_count() == 1);
    wait_until("settled", || {
        live.runtime
            .snapshot()
            .frames
            .iter()
            .any(|f| matches!(f.outcome, Some(OperationOutcome::Applied { .. })))
    });
    live.runtime.seal();
    assert!(!live.runtime.is_authorized());
    assert!(!live.runtime.authorize(1), "a sealed runtime never re-publishes");
    live.runtime.seal();
    live.runtime.shutdown();
}

#[test]
fn invalidation_completes_while_a_send_is_held_and_queued_writes_become_inert() {
    let live = live(2);
    let hold = live.backend.hold_sends();
    live.backend.script_action(applied(TARGET));
    live.backend.script_action(applied(TARGET));
    assert!(live.runtime.authorize(1));
    // One write is stalled inside the (fake) native send, the other queued behind it.
    assert!(hold.wait_entered(1, Duration::from_secs(10)), "a send is held");
    wait_until("both outstanding", || live.runtime.outstanding_frames() == 2);

    // Logical invalidation is one atomic store: it returns at once.
    let started = Instant::now();
    live.runtime.seal();
    assert!(started.elapsed() < Duration::from_millis(500));
    assert!(!live.runtime.is_authorized());

    // The drain is finite and reports the truth while the send is stalled.
    assert_eq!(
        live.runtime.drain_frames(Duration::from_millis(60), &CancellationToken::new()),
        DrainOutcome::TimedOut { outstanding: 2 }
    );
    hold.release();
    assert_eq!(
        live.runtime.drain_frames(Duration::from_secs(10), &CancellationToken::new()),
        DrainOutcome::Drained
    );
    assert_eq!(
        live.backend.action_count(),
        1,
        "the queued write never reached the backend"
    );
    live.runtime.shutdown();
    assert_eq!(live.windows.len(), 2);
}

#[test]
fn shutdown_seals_before_it_waits_on_a_stalled_send() {
    let live = live(1);
    let hold = live.backend.hold_sends();
    live.backend.script_action(applied(TARGET));
    assert!(live.runtime.authorize(1));
    assert!(hold.wait_entered(1, Duration::from_secs(10)));
    let runtime = Arc::new(live.runtime);
    let stopper = Arc::clone(&runtime);
    let thread = std::thread::spawn(move || stopper.shutdown());
    // The shutdown cannot finish (the worker join waits on the held send), yet
    // the seal already landed.
    wait_until("sealed first", || !runtime.is_authorized());
    assert!(!thread.is_finished());
    hold.release();
    thread.join().expect("shutdown");
    assert_eq!(live.backend.action_count(), 1);
}

#[test]
fn a_cancelled_drain_reports_what_is_still_outstanding() {
    let live = live(1);
    let hold = live.backend.hold_sends();
    live.backend.script_action(applied(TARGET));
    assert!(live.runtime.authorize(1));
    assert!(hold.wait_entered(1, Duration::from_secs(10)));
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        live.runtime.drain_frames(Duration::from_secs(30), &cancel),
        DrainOutcome::Cancelled { outstanding: 1 }
    );
    live.runtime.seal();
    hold.release();
    live.runtime.shutdown();
}

#[test]
fn stale_and_unscripted_outcomes_are_typed_not_applied() {
    let live = live(1);
    // Unscripted: the fake answers Stale(UnknownWindow), a proven no-send.
    assert!(live.runtime.authorize(1));
    wait_until("typed stale", || {
        live.runtime.snapshot().frames.iter().any(|f| {
            matches!(
                f.outcome,
                Some(OperationOutcome::Stale {
                    cause: crate::outcome::StaleCause::Window(StaleReason::UnknownWindow),
                    ..
                })
            )
        })
    });
    assert_eq!(live.runtime.snapshot().diagnostics.frames_applied, 0);
    live.runtime.shutdown();
}
