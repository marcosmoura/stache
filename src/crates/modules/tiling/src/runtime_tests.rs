//! Fake-runtime tests: the actor, pipeline, runner and harness together, with a
//! fake backend, directory and clock. No real window, display or process.
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use stache_macos::ApplicationId;
use stache_macos::windows::{
    ActionRequest, AdapterId, CleanupReport, CloseOutcome, DispatchError, EventQueue,
    GenerationCell, InstallError, RuntimeEpoch, SnapshotOrigin, SnapshotOutcome, StopOutcome,
    TrackOutcome, WindowActionOutcome, WindowChange, WindowEventKind, WindowInstance,
};
use stache_sdk::CancellationToken;

use crate::actor::{
    EffectEnv, EffectRunner, Engine, InlineRunner, ManualClock, SnapshotCell, SystemClock,
    ThreadRunner,
};
use crate::backend::{FakeBackendCall, FakeWindowBackend, WindowBackend};
use crate::events::{GeometryCoalescer, Ingress, Pipeline};
use crate::state::{Health, HealthFault, Input, Phase, TilingSnapshot, TilingState};
use crate::test_support::{FakeAppDirectory, Feed, app, display, info, one_display, snapshot};
use crate::tracking::{TrackingOptions, TrackingRuntime};

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// A deterministic, single-threaded engine over the fake backend.
struct Rig {
    backend: Arc<FakeWindowBackend>,
    directory: Arc<FakeAppDirectory>,
    ingress: Arc<Ingress>,
    sink: Arc<EventQueue>,
    clock: Arc<ManualClock>,
    engine: Engine,
    cell: Arc<SnapshotCell>,
    feed: Feed,
    gate: Arc<GenerationCell>,
    frames: Arc<crate::FrameControl>,
    animation: Arc<crate::AnimationControl>,
}

impl Rig {
    fn new(
        self_bundle: &str,
        apps: Vec<crate::state::AppInfo>,
        semantic: usize,
        sink_capacity: usize,
    ) -> Self {
        let backend = Arc::new(FakeWindowBackend::default());
        let directory = Arc::new(FakeAppDirectory::default());
        *directory.apps.lock().expect("apps") = apps;
        let ingress = Arc::new(Ingress::new(Pipeline::with_bounds(
            semantic,
            crate::events::COMPLETION_CAPACITY,
            GeometryCoalescer::default(),
        )));
        let sink = EventQueue::new(sink_capacity);
        let gate = Arc::new(GenerationCell::new());
        let epoch = backend.install(Arc::clone(&sink), Arc::clone(&gate), None).expect("install");
        let mut feed = Feed::new(epoch.value());
        feed.adapter = backend.adapter();
        let clock = Arc::new(ManualClock::default());
        let frames = Arc::new(crate::FrameControl::new(
            Arc::clone(&gate),
            Arc::clone(&clock) as Arc<dyn crate::actor::Clock>,
        ));
        let cancel = CancellationToken::new();
        let animation = Arc::new(crate::AnimationControl::live(
            Arc::clone(&backend) as Arc<dyn WindowBackend>,
            Arc::clone(&ingress),
            Arc::clone(&frames),
            Arc::clone(&clock) as Arc<dyn crate::actor::Clock>,
            cancel.clone(),
        ));
        let env = EffectEnv {
            backend: Arc::clone(&backend) as Arc<dyn WindowBackend>,
            directory: Arc::clone(&directory) as Arc<dyn crate::actor::AppDirectory>,
            ingress: Arc::clone(&ingress),
            cancel,
            frames: Arc::clone(&frames),
            visibility: Arc::new(crate::VisibilityControl::new()),
            actions: None,
            animation: Arc::clone(&animation),
        };
        let cell = Arc::new(SnapshotCell::default());
        let engine = Engine::new(
            TilingState::new(self_bundle),
            Arc::clone(&ingress),
            Arc::clone(&cell),
            Arc::new(InlineRunner::new(env)),
            Arc::clone(&sink),
            Arc::clone(&clock) as Arc<dyn crate::actor::Clock>,
        );
        let mut engine = engine;
        engine.set_frame_control(Arc::clone(&frames));
        Self {
            backend,
            directory,
            ingress,
            sink,
            clock,
            engine,
            cell,
            feed,
            gate,
            frames,
            animation,
        }
    }

    fn start(&mut self) {
        self.ingress.submit(Input::Start(self.feed.stamp)).expect("start");
        self.ingress.submit(Input::Displays(Arc::new(one_display()))).expect("displays");
        self.pump();
    }

    /// Runs turns until quiet.
    fn pump(&mut self) {
        for _ in 0..200 {
            if self.engine.turn().processed == 0 {
                return;
            }
        }
        panic!("the engine never went quiet");
    }

    fn push(&mut self, kind: WindowEventKind) -> bool {
        let event = self.feed.event(kind);
        self.sink.push(event)
    }

    fn snap(&self) -> Arc<TilingSnapshot> { self.cell.load() }

    fn token(&self, app_n: u64, serial: u64) -> WindowInstance { self.feed.token(app_n, serial) }

    fn created(&mut self, token: WindowInstance, title: &str, cg: u32) {
        self.push(WindowEventKind::Snapshot {
            snapshot: snapshot(token, title, cg),
            origin: SnapshotOrigin::Created,
        });
    }

    fn complete(&mut self, application: ApplicationId) {
        self.push(WindowEventKind::EnumerationComplete {
            application,
            windows: 0,
            truncated: false,
        });
    }

    fn calls(&self) -> Vec<FakeBackendCall> { self.backend.calls() }
}

fn assert_no_native_mutation(calls: &[FakeBackendCall]) {
    assert!(
        calls.iter().all(|call| !matches!(call, FakeBackendCall::Action(_))),
        "the read-only harness must never act: {calls:?}"
    );
}

#[test]
fn startup_installs_observers_first_then_completes_asynchronously() {
    let apps = vec![
        info(1, "com.a", "A"),
        info(2, "com.b", "B"),
        info(3, "com.c", "C"),
    ];
    let mut rig = Rig::new("", apps, 64, 64);
    rig.start();
    let calls = rig.calls();
    assert_eq!(calls[0], FakeBackendCall::Install);
    let tracks = calls.iter().filter(|c| matches!(c, FakeBackendCall::Track(..))).count();
    assert_eq!(tracks, 3);
    // No per-window synchronous snapshot chain at startup.
    assert!(!calls.iter().any(|c| matches!(c, FakeBackendCall::Snapshot(_))));
    assert_eq!(rig.snap().phase, Phase::Initializing);
    for n in 1..=3 {
        let w = rig.token(n, 1);
        rig.push(WindowEventKind::Snapshot {
            snapshot: snapshot(w, "w", n as u32),
            origin: SnapshotOrigin::Enumeration,
        });
        rig.complete(app(n));
    }
    rig.pump();
    let snap = rig.snap();
    assert_eq!(snap.phase, Phase::Complete);
    assert_eq!(snap.windows.len(), 3);
    assert!(snap.actions_allowed);
}

#[test]
fn events_advance_while_enumeration_is_unfinished_and_a_create_is_not_lost() {
    let mut rig = Rig::new("", vec![info(1, "com.a", "A")], 64, 64);
    rig.start();
    // Enumeration has not completed; the stream keeps advancing.
    let (early, mid) = (rig.token(1, 1), rig.token(1, 2));
    rig.created(mid, "created while scanning", 2);
    rig.pump();
    assert_eq!(rig.snap().phase, Phase::Initializing);
    assert!(rig.snap().window(mid).is_some(), "applied before the scan ends");
    rig.push(WindowEventKind::Snapshot {
        snapshot: snapshot(early, "early", 1),
        origin: SnapshotOrigin::Enumeration,
    });
    rig.complete(app(1));
    rig.pump();
    let snap = rig.snap();
    assert_eq!(snap.phase, Phase::Complete);
    assert!(snap.window(early).is_some() && snap.window(mid).is_some());
}

#[test]
fn create_focus_destroy_through_the_whole_pipeline() {
    let mut rig = Rig::new("", vec![info(1, "com.a", "A")], 64, 64);
    rig.start();
    rig.complete(app(1));
    let w = rig.token(1, 1);
    rig.created(w, "w", 1);
    rig.push(WindowEventKind::Window {
        window: w,
        change: WindowChange::Focused,
    });
    rig.push(WindowEventKind::Window {
        window: w,
        change: WindowChange::Destroyed,
    });
    rig.pump();
    let snap = rig.snap();
    assert!(snap.windows.is_empty() && snap.focus.window.is_none());
    assert!(snap.workspaces.iter().all(|ws| ws.order.is_empty()));
    assert_no_native_mutation(&rig.calls());
}

#[test]
fn moved_bursts_coalesce_into_one_read_at_the_display_rate() {
    let mut rig = Rig::new("", vec![info(1, "com.a", "A")], 64, 4096);
    rig.start();
    rig.complete(app(1));
    let w = rig.token(1, 1);
    rig.created(w, "w", 1);
    rig.pump();
    let mut moved = snapshot(w, "w", 1);
    moved.frame.position =
        stache_macos::ax::AxReading::Value(stache_macos::ax::AxPoint { x: 500.0, y: 40.0 });
    rig.backend.script_snapshot(w, SnapshotOutcome::Snapshot(Box::new(moved)));
    for _ in 0..500 {
        rig.push(WindowEventKind::Window {
            window: w,
            change: WindowChange::Moved,
        });
    }
    rig.push(WindowEventKind::Window {
        window: w,
        change: WindowChange::Resized,
    });
    rig.pump();
    // Nothing flushed before the period elapses.
    assert!(!rig.calls().iter().any(|c| matches!(c, FakeBackendCall::Snapshot(_))));
    assert_eq!(rig.snap().admission.geometry_coalesced, 500);
    rig.clock.advance(Duration::from_millis(20));
    rig.pump();
    let reads = rig
        .calls()
        .iter()
        .filter(|c| matches!(c, FakeBackendCall::Snapshot(t) if *t == w))
        .count();
    assert_eq!(reads, 1, "501 samples, one authoritative read");
    assert_eq!(
        rig.snap().window(w).expect("window").observed.frame.expect("frame").x,
        500.0
    );
}

#[test]
fn a_destroy_invalidates_older_geometry_and_the_token_is_never_read() {
    let mut rig = Rig::new("", vec![info(1, "com.a", "A")], 64, 64);
    rig.start();
    rig.complete(app(1));
    let w = rig.token(1, 1);
    rig.created(w, "w", 1);
    rig.push(WindowEventKind::Window {
        window: w,
        change: WindowChange::Moved,
    });
    rig.push(WindowEventKind::Window {
        window: w,
        change: WindowChange::Destroyed,
    });
    rig.pump();
    rig.clock.advance(Duration::from_secs(1));
    rig.pump();
    assert!(!rig.calls().iter().any(|c| matches!(c, FakeBackendCall::Snapshot(_))));
    assert_eq!(rig.snap().admission.geometry_invalidated, 1);
}

#[test]
fn hotplug_retimes_and_keeps_pending_samples() {
    let mut rig = Rig::new("", vec![info(1, "com.a", "A")], 64, 64);
    rig.start();
    rig.complete(app(1));
    let w = rig.token(1, 1);
    rig.created(w, "w", 1);
    rig.push(WindowEventKind::Window {
        window: w,
        change: WindowChange::Moved,
    });
    rig.pump();
    assert_eq!(rig.snap().admission.geometry_pending, 1);
    let layout = stache_macos::DisplayLayout::new(vec![
        display(1, true, Some(360.0), 0.0),
        display(2, false, Some(30.0), 1920.0),
    ]);
    rig.ingress.submit(Input::Displays(Arc::new(layout))).expect("displays");
    rig.pump();
    assert_eq!(
        rig.snap().admission.geometry_pending,
        1,
        "remapped by exact token, not lost"
    );
    rig.clock.advance(Duration::from_millis(40));
    rig.pump();
    assert!(rig.calls().iter().any(|c| matches!(c, FakeBackendCall::Snapshot(t) if *t == w)));
}

#[test]
fn display_rates_drive_the_flush_period_with_the_explicit_clamp() {
    let mut rig = Rig::new("", Vec::new(), 64, 64);
    rig.start();
    let rates = [
        Some(30.0),
        Some(60.0),
        Some(120.0),
        Some(360.0),
        Some(1000.0),
        Some(f64::NAN),
        None,
    ];
    let displays: Vec<_> = rates
        .iter()
        .enumerate()
        .map(|(i, hz)| display(u32::try_from(i).unwrap() + 1, i == 0, *hz, 1920.0 * i as f64))
        .collect();
    rig.ingress
        .submit(Input::Displays(Arc::new(stache_macos::DisplayLayout::new(
            displays,
        ))))
        .expect("displays");
    rig.pump();
    let expected = [30.0, 60.0, 120.0, 360.0, 360.0, 60.0, 60.0];
    for (i, hz) in expected.iter().enumerate() {
        let id = stache_macos::DisplayId::new(u32::try_from(i).unwrap() + 1);
        let period = rig.ingress.with(|p| p.geometry().period_for(id));
        assert!(
            (period.as_secs_f64() - 1.0 / hz).abs() < 1e-9,
            "display {i}: {period:?}"
        );
    }
}

#[test]
fn a_full_semantic_queue_never_acts_on_stale_identity_and_reconciles_once() {
    let mut rig = Rig::new("", vec![info(1, "com.a", "A")], 8, 4096);
    rig.start();
    let kept: Vec<_> = (1..=4).map(|n| rig.token(1, n)).collect();
    for (i, token) in kept.iter().enumerate() {
        rig.created(*token, "w", i as u32 + 1);
    }
    rig.complete(app(1));
    rig.pump();
    assert_eq!(rig.snap().windows.len(), 4);
    assert_eq!(rig.snap().phase, Phase::Complete);

    // Eight unrelated events fill the queue in one burst; destroy(kept[0]) is lost.
    for n in 0..8 {
        let ghost = rig.token(7, 100 + n);
        rig.push(WindowEventKind::Window {
            window: ghost,
            change: WindowChange::Miniaturized,
        });
    }
    rig.push(WindowEventKind::Window {
        window: kept[0],
        change: WindowChange::Destroyed,
    });
    rig.engine.turn();
    let snap = rig.snap();
    assert!(snap.admission.semantic_rejected >= 1);
    assert!(
        snap.window(kept[0]).is_some(),
        "the lost destroy has not been applied..."
    );
    assert!(!snap.actions_allowed, "...so no action authority exists");
    assert_ne!(snap.phase, Phase::Complete);
    let refreshes = rig.calls().iter().filter(|c| matches!(c, FakeBackendCall::Refresh(_))).count();
    assert_eq!(refreshes, 1, "exactly one bounded reconciliation");

    // The authoritative enumeration does not list the destroyed window.
    for token in &kept[1..] {
        rig.push(WindowEventKind::Snapshot {
            snapshot: snapshot(*token, "w", 9),
            origin: SnapshotOrigin::Enumeration,
        });
    }
    rig.complete(app(1));
    rig.pump();
    let snap = rig.snap();
    assert!(snap.window(kept[0]).is_none(), "repaired by reconciliation");
    assert_eq!(snap.phase, Phase::Complete);
    assert!(snap.actions_allowed);
    assert_eq!(snap.diagnostics.missed_destroys_repaired, 1);
}

#[test]
fn a_backend_sink_overflow_triggers_reconciliation_of_every_tracked_app() {
    let mut rig = Rig::new("", vec![info(1, "com.a", "A"), info(2, "com.b", "B")], 64, 4);
    rig.start();
    rig.complete(app(1));
    rig.complete(app(2));
    rig.pump();
    assert_eq!(rig.snap().phase, Phase::Complete);
    for n in 0..10 {
        let ghost = rig.token(1, n);
        rig.push(WindowEventKind::Window {
            window: ghost,
            change: WindowChange::Miniaturized,
        });
    }
    rig.engine.turn();
    assert_eq!(rig.snap().admission.sink_dropped, 6);
    let calls = rig.calls();
    for application in [app(1), app(2)] {
        assert_eq!(
            calls.iter().filter(|c| **c == FakeBackendCall::Refresh(application)).count(),
            1
        );
    }
    assert_ne!(rig.snap().phase, Phase::Complete);
}

#[test]
fn an_observer_overflow_event_reconciles_that_application() {
    let mut rig = Rig::new("", vec![info(1, "com.a", "A")], 64, 64);
    rig.start();
    rig.complete(app(1));
    rig.pump();
    rig.push(WindowEventKind::ObserverOverflow { application: app(1) });
    rig.pump();
    assert!(rig.calls().contains(&FakeBackendCall::Refresh(app(1))));
}

#[test]
fn a_slow_actor_under_a_flood_stays_bounded_and_visibly_diagnosed() {
    let mut rig = Rig::new("", vec![info(1, "com.a", "A")], 64, 128);
    rig.start();
    rig.complete(app(1));
    rig.pump();
    // The actor does not run while 50k events and 5k lifecycle messages arrive.
    for n in 0..50_000u64 {
        let ghost = rig.token(1, n);
        rig.push(WindowEventKind::Window {
            window: ghost,
            change: WindowChange::Miniaturized,
        });
    }
    for _ in 0..5_000 {
        let _ = rig.ingress.submit(Input::Lifecycle {
            kind: stache_macos::applications::LifecycleKind::Hidden,
            info: info(1, "com.a", "A"),
        });
    }
    let stats = rig.ingress.stats();
    assert!(stats.semantic_depth <= 64);
    rig.pump();
    let snap = rig.snap();
    assert!(snap.admission.sink_dropped > 0 && snap.admission.semantic_rejected > 0);
    assert!(snap.admission.semantic_high_water <= 64);
    assert!(snap.diagnostics.unconfirmed_markings > 0);
}

#[test]
fn a_permanently_overloaded_stream_stays_ineligible() {
    let mut rig = Rig::new("", vec![info(1, "com.a", "A")], 4, 4096);
    rig.start();
    rig.complete(app(1));
    rig.pump();
    for _round in 0..6 {
        for n in 0..20u64 {
            let ghost = rig.token(1, 1000 + n);
            rig.push(WindowEventKind::Window {
                window: ghost,
                change: WindowChange::Miniaturized,
            });
        }
        rig.engine.turn();
    }
    let snap = rig.snap();
    assert!(snap.diagnostics.overloaded);
    assert_eq!(snap.phase, Phase::Incomplete);
    assert!(!snap.actions_allowed);
}

#[test]
fn late_completions_from_an_old_registry_are_dropped_with_a_diagnostic() {
    let mut rig = Rig::new("", vec![info(1, "com.a", "A")], 64, 64);
    rig.start();
    let old = Feed::new(77).stamp;
    rig.ingress
        .submit(Input::Completion {
            stamp: old,
            result: crate::state::CompletionResult::Track(app(1), TrackOutcome::OwnerUnavailable),
        })
        .expect("submit");
    rig.pump();
    let snap = rig.snap();
    assert_eq!(snap.diagnostics.stale_completions, 1);
    assert_eq!(
        snap.health,
        Health::Healthy,
        "the old registry cannot poison the new one"
    );
}

#[test]
fn a_closed_runner_is_a_disconnect_health_fact() {
    let mut rig = Rig::new("", Vec::new(), 64, 64);
    rig.start();
    let env = EffectEnv {
        backend: Arc::clone(&rig.backend) as Arc<dyn WindowBackend>,
        directory: Arc::clone(&rig.directory) as Arc<dyn crate::actor::AppDirectory>,
        ingress: Arc::clone(&rig.ingress),
        cancel: CancellationToken::new(),
        frames: Arc::clone(&rig.frames),
        visibility: Arc::new(crate::VisibilityControl::new()),
        actions: None,
        animation: Arc::clone(&rig.animation),
    };
    let runner = Arc::new(ThreadRunner::spawn(env, 4).expect("runner"));
    runner.close();
    rig.engine = Engine::new(
        TilingState::new(""),
        Arc::clone(&rig.ingress),
        Arc::clone(&rig.cell),
        runner,
        Arc::clone(&rig.sink),
        Arc::new(SystemClock),
    );
    rig.ingress.submit(Input::Start(rig.feed.stamp)).expect("start");
    rig.pump();
    assert_eq!(rig.snap().health, Health::Failed(HealthFault::Disconnected));
    assert!(!rig.snap().actions_allowed);
}

#[cfg(panic = "unwind")]
#[test]
fn unwind_only_a_panicking_transition_keeps_the_last_safe_snapshot_and_requires_restart() {
    let mut rig = Rig::new("", vec![info(1, "com.a", "A")], 64, 64);
    rig.start();
    let w = rig.token(1, 1);
    rig.created(w, "w", 1);
    rig.complete(app(1));
    rig.pump();
    let before = rig.snap();
    assert!(before.window(w).is_some());
    rig.ingress.submit(Input::PanicForTest).expect("submit");
    let turn = rig.engine.turn();
    assert!(turn.halted);
    let after = rig.snap();
    assert_eq!(after.health, Health::Failed(HealthFault::Panicked));
    assert!(!after.actions_allowed);
    assert!(
        after.window(w).is_some(),
        "the last published safe state is preserved"
    );
    // The actor does not continue with possibly corrupted state.
    assert!(rig.ingress.submit(Input::Activated(app(1))).is_err());
    assert!(rig.engine.turn().halted);
}

#[test]
fn the_read_only_harness_performs_zero_native_effects() {
    let mut rig = Rig::new(
        "com.stache",
        vec![info(1, "com.a", "A"), info(2, "com.stache", "Stache")],
        8,
        8,
    );
    rig.start();
    rig.complete(app(1));
    let w = rig.token(1, 1);
    rig.created(w, "w", 1);
    rig.push(WindowEventKind::Window {
        window: w,
        change: WindowChange::Focused,
    });
    rig.push(WindowEventKind::Window {
        window: w,
        change: WindowChange::Moved,
    });
    rig.push(WindowEventKind::Window {
        window: w,
        change: WindowChange::TitleChanged,
    });
    for n in 0..30 {
        let ghost = rig.token(1, 100 + n);
        rig.push(WindowEventKind::Window {
            window: ghost,
            change: WindowChange::Miniaturized,
        });
    }
    rig.pump();
    rig.clock.advance(Duration::from_secs(1));
    rig.pump();
    rig.ingress
        .submit(Input::Displays(Arc::new(stache_macos::DisplayLayout::default())))
        .expect("displays");
    rig.pump();
    let calls = rig.calls();
    assert_no_native_mutation(&calls);
    assert!(calls.iter().all(|call| matches!(
        call,
        FakeBackendCall::Install
            | FakeBackendCall::Track(..)
            | FakeBackendCall::Refresh(_)
            | FakeBackendCall::Untrack(_)
            | FakeBackendCall::Snapshot(_)
    )));
    // Stache itself is never even observed.
    assert!(!calls.iter().any(|c| matches!(c, FakeBackendCall::Track(a, _) if *a == app(2))));
    assert_eq!(rig.snap().epoch, Some(rig.feed.epoch.value()));
}

// ----------------------------------------------------------------- threaded

/// Delegates to a fake but lets a test hold `track` open and release it.
struct GateBackend {
    inner: Arc<FakeWindowBackend>,
    open: AtomicBool,
    entered: AtomicUsize,
    /// Parks `snapshot` until its cancellation token fires.
    block_snapshots: AtomicBool,
    snapshots_entered: AtomicUsize,
}

impl GateBackend {
    fn new(open: bool) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(FakeWindowBackend::default()),
            open: AtomicBool::new(open),
            entered: AtomicUsize::new(0),
            block_snapshots: AtomicBool::new(false),
            snapshots_entered: AtomicUsize::new(0),
        })
    }
}

impl WindowBackend for GateBackend {
    fn adapter(&self) -> AdapterId { self.inner.adapter() }

    fn install(
        &self,
        sink: Arc<EventQueue>,
        gate: Arc<GenerationCell>,
        deadline: Option<Instant>,
    ) -> Result<RuntimeEpoch, InstallError> {
        self.inner.install(sink, gate, deadline)
    }

    fn track(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        pid: i32,
        cancel: CancellationToken,
        deadline: Option<Instant>,
    ) -> TrackOutcome {
        self.entered.fetch_add(1, Ordering::SeqCst);
        while !self.open.load(Ordering::SeqCst) {
            if cancel.wait(Duration::from_millis(5)) {
                return TrackOutcome::Unacknowledged;
            }
        }
        self.inner.track(epoch, application, pid, cancel, deadline)
    }

    fn refresh(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        cancel: CancellationToken,
        deadline: Option<Instant>,
    ) -> TrackOutcome {
        self.inner.refresh(epoch, application, cancel, deadline)
    }

    fn untrack(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> StopOutcome {
        self.inner.untrack(epoch, application, cancel, deadline)
    }

    fn snapshot(
        &self,
        window: WindowInstance,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> SnapshotOutcome {
        if self.block_snapshots.load(Ordering::SeqCst) {
            self.snapshots_entered.fetch_add(1, Ordering::SeqCst);
            while !cancel.wait(Duration::from_millis(5)) {}
            return SnapshotOutcome::Unacknowledged;
        }
        self.inner.snapshot(window, cancel, deadline)
    }

    fn resolve_window_id(
        &self,
        id: u32,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> Option<WindowInstance> {
        self.inner.resolve_window_id(id, cancel, deadline)
    }

    fn submit_action(
        &self,
        request: ActionRequest,
        reply: Box<dyn FnOnce(WindowActionOutcome) + Send>,
    ) -> Result<(), DispatchError> {
        self.inner.submit_action(request, reply)
    }

    fn act(&self, request: ActionRequest, cancel: &CancellationToken) -> WindowActionOutcome {
        self.inner.act(request, cancel)
    }

    fn application_hidden(
        &self,
        request: stache_macos::windows::HiddenRequest,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> stache_macos::windows::HiddenOutcome {
        self.inner.application_hidden(request, cancel, deadline)
    }

    fn stop(
        &self,
        epoch: RuntimeEpoch,
        gate: &GenerationCell,
        deadline: Option<Instant>,
    ) -> StopOutcome {
        self.inner.stop(epoch, gate, deadline)
    }

    fn retry_cleanup(
        &self,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> Option<CleanupReport> {
        self.inner.retry_cleanup(cancel, deadline)
    }

    fn close(&self) -> CloseOutcome { self.inner.close() }
}

fn launch(
    backend: &Arc<GateBackend>,
    apps: Vec<crate::state::AppInfo>,
    options: &TrackingOptions,
) -> (TrackingRuntime, Feed, Arc<FakeAppDirectory>) {
    let directory = Arc::new(FakeAppDirectory::default());
    *directory.apps.lock().expect("apps") = apps;
    let runtime = TrackingRuntime::start(
        Arc::clone(backend) as Arc<dyn WindowBackend>,
        Arc::clone(&directory) as Arc<dyn crate::actor::AppDirectory>,
        options,
    )
    .expect("start");
    let mut feed = Feed::new(runtime.epoch().value());
    feed.adapter = backend.adapter();
    runtime.submit_displays(None, one_display()).expect("displays");
    (runtime, feed, directory)
}

fn push(backend: &GateBackend, feed: &mut Feed, kind: WindowEventKind) {
    let sink = backend.inner.sink().expect("sink");
    assert!(sink.push(feed.event(kind)));
}

#[test]
fn enumeration_blocked_in_the_backend_does_not_block_the_event_stream() {
    let backend = GateBackend::new(false);
    let (runtime, mut feed, _) =
        launch(&backend, vec![info(1, "com.a", "A")], &TrackingOptions::new(""));
    wait_until("track entered", || backend.entered.load(Ordering::SeqCst) == 1);
    // The track request is parked inside the backend; the actor still answers.
    let w = feed.token(1, 1);
    push(&backend, &mut feed, WindowEventKind::Snapshot {
        snapshot: snapshot(w, "w", 1),
        origin: SnapshotOrigin::Created,
    });
    wait_until("window visible while enumeration is blocked", || {
        runtime.snapshot().window(w).is_some()
    });
    assert_eq!(runtime.snapshot().phase, Phase::Initializing);
    push(&backend, &mut feed, WindowEventKind::Window {
        window: w,
        change: WindowChange::Focused,
    });
    wait_until("focus applied", || runtime.snapshot().focus.window == Some(w));

    backend.open.store(true, Ordering::SeqCst);
    push(&backend, &mut feed, WindowEventKind::EnumerationComplete {
        application: app(1),
        windows: 1,
        truncated: false,
    });
    wait_until("complete", || runtime.snapshot().phase == Phase::Complete);
    let calls = backend.inner.calls();
    assert_eq!(
        calls[0],
        FakeBackendCall::Install,
        "observers installed before anything else"
    );
    runtime.shutdown();
    assert_no_native_mutation(&backend.inner.calls());
}

#[test]
fn shutdown_cancels_a_blocked_enumeration_and_joins_promptly() {
    let backend = GateBackend::new(false);
    let (runtime, mut feed, _) =
        launch(&backend, vec![info(1, "com.a", "A")], &TrackingOptions::new(""));
    wait_until("track entered", || backend.entered.load(Ordering::SeqCst) == 1);
    // A pending geometry deadline must not delay cancellation either.
    let w = feed.token(1, 1);
    push(&backend, &mut feed, WindowEventKind::Window {
        window: w,
        change: WindowChange::Moved,
    });
    let started = Instant::now();
    runtime.shutdown();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(runtime.ingress().is_shutdown());
}

#[test]
fn shutdown_cancels_a_blocked_backend_snapshot_within_the_quantum() {
    let backend = GateBackend::new(true);
    backend.block_snapshots.store(true, Ordering::SeqCst);
    let (runtime, mut feed, _) =
        launch(&backend, vec![info(1, "com.a", "A")], &TrackingOptions::new(""));
    wait_until("tracking", || {
        backend.inner.calls().iter().any(|c| matches!(c, FakeBackendCall::Track(..)))
    });
    // A focus for a window never seen makes the actor read it: that read parks
    // in the backend until the shutdown cancellation reaches it.
    let w = feed.token(1, 1);
    push(&backend, &mut feed, WindowEventKind::Window {
        window: w,
        change: WindowChange::Focused,
    });
    wait_until("snapshot parked in the backend", || {
        backend.snapshots_entered.load(Ordering::SeqCst) == 1
    });
    let started = Instant::now();
    runtime.shutdown();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_dead_backend_owner_is_a_health_fact_not_a_silent_success() {
    let backend = GateBackend::new(true);
    backend.inner.script_track(TrackOutcome::OwnerUnavailable);
    let (runtime, _feed, _) =
        launch(&backend, vec![info(1, "com.a", "A")], &TrackingOptions::new(""));
    wait_until("failed health", || {
        runtime.health() == Health::Failed(HealthFault::BackendUnavailable)
    });
    let snap = runtime.snapshot();
    assert_eq!(snap.phase, Phase::Incomplete);
    assert!(!snap.actions_allowed);
    runtime.shutdown();
}

#[test]
fn mailbox_saturation_is_a_visible_failure_not_success() {
    let backend = GateBackend::new(false);
    let mut options = TrackingOptions::new("");
    options.runner_capacity = 2;
    let apps: Vec<_> = (1..=30).map(|n| info(n, "com.x", "X")).collect();
    let (runtime, _feed, _) = launch(&backend, apps, &options);
    // One track is parked in the worker, two are queued; the rest are refused.
    wait_until("refusals recorded", || {
        let snap = runtime.snapshot();
        snap.phase == Phase::Incomplete && snap.diagnostics.enumeration_failures >= 20
    });
    assert_ne!(runtime.snapshot().phase, Phase::Complete);
    assert!(backend.entered.load(Ordering::SeqCst) <= 3);
    runtime.shutdown();
}

#[test]
fn every_observed_snapshot_is_internally_consistent_under_concurrent_reads() {
    let backend = GateBackend::new(true);
    let (runtime, mut feed, _) =
        launch(&backend, vec![info(1, "com.a", "A")], &TrackingOptions::new(""));
    wait_until("tracking", || {
        backend.inner.calls().iter().any(|c| matches!(c, FakeBackendCall::Track(..)))
    });
    let stop = Arc::new(AtomicBool::new(false));
    let violations = Arc::new(AtomicUsize::new(0));
    let runtime = Arc::new(runtime);
    // Collected on purpose: the iterator is lazy and the readers must start now.
    #[allow(clippy::needless_collect)]
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let (runtime, stop, violations) =
                (Arc::clone(&runtime), Arc::clone(&stop), Arc::clone(&violations));
            std::thread::spawn(move || {
                let mut last = 0;
                let mut reads = 0u64;
                while !stop.load(Ordering::SeqCst) {
                    let snap = runtime.snapshot();
                    reads += 1;
                    let ordered: usize = snap.workspaces.iter().map(|ws| ws.order.len()).sum();
                    let consistent = ordered == snap.windows.len()
                        && snap.windows.iter().all(|w| {
                            w.desired.workspace.is_some_and(|id| {
                                snap.workspaces
                                    .iter()
                                    .any(|ws| ws.id == id && ws.order.contains(&w.token))
                            })
                        })
                        && snap.focus.window.is_none_or(|t| snap.window(t).is_some())
                        && snap.sequence >= last;
                    if !consistent {
                        violations.fetch_add(1, Ordering::SeqCst);
                    }
                    last = snap.sequence;
                }
                reads
            })
        })
        .collect();
    for round in 0..300u64 {
        let w = feed.token(1, round + 1);
        push(&backend, &mut feed, WindowEventKind::Snapshot {
            snapshot: snapshot(w, "w", round as u32 + 1),
            origin: SnapshotOrigin::Created,
        });
        push(&backend, &mut feed, WindowEventKind::Window {
            window: w,
            change: WindowChange::Focused,
        });
        if round % 3 == 0 {
            push(&backend, &mut feed, WindowEventKind::Window {
                window: w,
                change: WindowChange::Destroyed,
            });
        }
        if round % 50 == 0 {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    wait_until("drained", || runtime.snapshot().windows.len() == 200);
    stop.store(true, Ordering::SeqCst);
    let reads: u64 = readers.into_iter().map(|r| r.join().expect("reader")).sum();
    assert!(reads > 100);
    assert_eq!(violations.load(Ordering::SeqCst), 0);
    runtime.shutdown();
}

#[test]
fn stale_topology_hints_are_dropped() {
    let backend = GateBackend::new(true);
    let (runtime, _feed, _) = launch(&backend, Vec::new(), &TrackingOptions::new(""));
    let hint = |sequence| Some(stache_macos::TopologyHint { sequence });
    assert_eq!(runtime.submit_displays(hint(5), one_display()), Ok(true));
    assert_eq!(runtime.submit_displays(hint(3), one_display()), Ok(false));
    assert_eq!(runtime.submit_displays(hint(5), one_display()), Ok(false));
    assert_eq!(runtime.submit_displays(hint(6), one_display()), Ok(true));
    runtime.shutdown();
    assert!(runtime.submit_displays(hint(9), one_display()).is_err());
}

#[test]
fn the_lifecycle_sink_resolves_metadata_and_feeds_the_actor() {
    let backend = GateBackend::new(true);
    let (runtime, mut feed, directory) =
        launch(&backend, vec![info(1, "com.a", "A")], &TrackingOptions::new(""));
    wait_until("tracking", || runtime.snapshot().epoch.is_some());
    directory.apps.lock().expect("apps").push(info(2, "com.b", "B"));
    let sink = runtime.lifecycle_sink();
    sink(stache_macos::applications::LifecycleEvent {
        kind: stache_macos::applications::LifecycleKind::Launched,
        application: app(2),
        pid: Some(102),
    });
    wait_until("launched app tracked", || {
        backend
            .inner
            .calls()
            .iter()
            .any(|c| matches!(c, FakeBackendCall::Track(a, _) if *a == app(2)))
    });
    let w = feed.token(2, 1);
    push(&backend, &mut feed, WindowEventKind::Snapshot {
        snapshot: snapshot(w, "w", 1),
        origin: SnapshotOrigin::Created,
    });
    wait_until("window", || runtime.snapshot().window(w).is_some());
    runtime.shutdown();
}

#[test]
fn registries_are_independent_two_runtimes_two_epochs() {
    let first = GateBackend::new(true);
    let second = GateBackend::new(true);
    let (a, mut fa, _) = launch(&first, vec![info(1, "com.a", "A")], &TrackingOptions::new(""));
    let mut options = TrackingOptions::new("");
    options.registry = 2;
    let (b, mut fb, _) = launch(&second, vec![info(1, "com.a", "A")], &options);
    for backend in [&first, &second] {
        wait_until("tracking", || {
            backend.inner.calls().iter().any(|c| matches!(c, FakeBackendCall::Track(..)))
        });
    }
    let wa = fa.token(1, 1);
    push(&first, &mut fa, WindowEventKind::Snapshot {
        snapshot: snapshot(wa, "a", 1),
        origin: SnapshotOrigin::Created,
    });
    wait_until("a", || a.snapshot().window(wa).is_some());
    // The other registry never sees the first one's window.
    let wb = fb.token(1, 1);
    push(&second, &mut fb, WindowEventKind::Snapshot {
        snapshot: snapshot(wb, "b", 2),
        origin: SnapshotOrigin::Created,
    });
    wait_until("b", || b.snapshot().window(wb).is_some());
    assert!(b.snapshot().window(wa).is_none() || wa == wb);
    assert_eq!(a.snapshot().registry, Some(1));
    assert_eq!(b.snapshot().registry, Some(2));
    a.shutdown();
    b.shutdown();
}

// ------------------------------------------------------- real owner core

mod real_core {
    use std::sync::atomic::AtomicUsize;

    use stache_macos::ax::testing::{FakeNode, FakeWindowAx, NodeId};
    use stache_macos::ax::{AxNotification, AxPoint, AxSize, BoolAttribute, WindowString};
    use stache_macos::windows::testing::FakeWindowSystem;
    use stache_macos::windows::{AdapterOptions, RunLoopDriver, SystemWindowClock, WindowAdapter};

    use super::*;
    use crate::backend::SystemWindowBackend;
    use crate::state::{Cause, Scope};

    static NAMES: AtomicUsize = AtomicUsize::new(0);

    fn window_node(ax: &FakeWindowAx, cg: u32) -> NodeId {
        let mut node = FakeNode {
            position: Ok(AxPoint { x: 10.0, y: 20.0 }),
            size: Ok(AxSize { width: 300.0, height: 200.0 }),
            position_settable: Ok(true),
            size_settable: Ok(true),
            window_id: Ok(cg),
            ..FakeNode::default()
        };
        node.strings.insert(WindowString::Title, Ok(format!("window {cg}")));
        node.strings.insert(WindowString::Role, Ok("AXWindow".to_owned()));
        node.strings.insert(WindowString::Subrole, Ok("AXStandardWindow".to_owned()));
        node.bools.insert(BoolAttribute::Minimized, Ok(false));
        node.bools.insert(BoolAttribute::FullScreen, Ok(false));
        ax.add_node(node)
    }

    fn tokens(runtime: &TrackingRuntime) -> Vec<WindowInstance> {
        let mut tokens: Vec<_> = runtime.snapshot().windows.iter().map(|w| w.token).collect();
        tokens.sort();
        tokens
    }

    /// The real owner `Core` over a fake AX arena and fake observers, driven
    /// through Tiling's production tracking path: initial track, a created and
    /// focused window, then a refresh after a window closed.
    #[test]
    fn the_real_core_keeps_live_windows_across_a_refresh_and_destroys_a_closed_one() {
        let ax = FakeWindowAx::new();
        let nodes: Vec<NodeId> = (0..3).map(|n| window_node(&ax, 100 + n)).collect();
        let app_el = ax.add_node(FakeNode {
            windows: Ok(nodes.clone()),
            ..FakeNode::default()
        });
        let listed = info(1, "com.a", "A");
        ax.set_application(listed.pid, app_el);
        let (system, obs) = FakeWindowSystem::new(ax.clone());
        let adapter = WindowAdapter::spawn(
            &format!(
                "stache-test-tiling-real-core-{}",
                NAMES.fetch_add(1, Ordering::SeqCst)
            ),
            &AdapterOptions::default(),
            Arc::new(SystemWindowClock),
            move || system,
            RunLoopDriver::new,
        )
        .expect("spawn");
        let backend = Arc::new(SystemWindowBackend::from_adapter(adapter, false));
        let directory = Arc::new(FakeAppDirectory::default());
        *directory.apps.lock().expect("apps") = vec![listed.clone()];
        let runtime = TrackingRuntime::start(
            backend as Arc<dyn WindowBackend>,
            directory as Arc<dyn crate::actor::AppDirectory>,
            &TrackingOptions::new(""),
        )
        .expect("start");
        runtime.submit_displays(None, one_display()).expect("displays");

        // Initial track.
        wait_until("initial enumeration", || {
            let snap = runtime.snapshot();
            snap.phase == Phase::Complete && snap.windows.len() == 3
        });
        let initial = tokens(&runtime);

        // A new window is created and focused in one observer drain.
        let fresh = window_node(&ax, 999);
        let mut all = nodes.clone();
        all.push(fresh);
        ax.edit(app_el, |node| node.windows = Ok(all.clone()));
        obs.inject(listed.pid, AxNotification::FocusedWindowChanged, fresh);
        obs.inject(listed.pid, AxNotification::WindowCreated, fresh);
        // Any command wakes the owner; an unconfirmed marking also refreshes.
        runtime
            .ingress()
            .submit(Input::Unconfirmed(Scope::App(app(1)), Cause::ObserverOverflow))
            .expect("submit");
        wait_until("created window focused", || {
            let snap = runtime.snapshot();
            snap.windows.len() == 4 && snap.focus.window.is_some()
        });
        let created = *tokens(&runtime)
            .iter()
            .find(|token| !initial.contains(token))
            .expect("new token");
        assert_eq!(runtime.snapshot().focus.window, Some(created));
        assert_eq!(tokens(&runtime).len(), 4, "the refresh deleted nothing");

        // A window closes without a destroy notification; a refresh finds it.
        let kept = vec![nodes[0], nodes[1], fresh];
        ax.edit(app_el, |node| node.windows = Ok(kept.clone()));
        runtime
            .ingress()
            .submit(Input::Unconfirmed(Scope::App(app(1)), Cause::SinkOverflow))
            .expect("submit");
        wait_until("closed window destroyed", || {
            runtime.snapshot().windows.len() == 3
        });
        let remaining = tokens(&runtime);
        assert!(remaining.contains(&initial[0]) && remaining.contains(&initial[1]));
        assert!(remaining.contains(&created));
        assert!(!remaining.contains(&initial[2]));
        let snap = runtime.snapshot();
        assert_eq!(
            snap.diagnostics.missed_destroys_repaired, 0,
            "the owner reported the destroy; Tiling's repair was not needed"
        );
        assert_eq!(snap.health, Health::Healthy);
        runtime.shutdown();
    }

    /// Tab reads on, over the real owner `Core`: two windows of one app, the
    /// first with a tab strip naming both, only the selected one is a tile.
    #[test]
    fn the_real_core_with_tab_reads_holds_back_the_inactive_tab() {
        let ax = FakeWindowAx::new();
        let front = window_node(&ax, 100);
        let back = window_node(&ax, 101);
        let plain = window_node(&ax, 102);
        let tab_front = ax.add_node(FakeNode {
            window_id: Ok(100),
            selected: Ok(true),
            ..FakeNode::default()
        });
        let tab_back = ax.add_node(FakeNode {
            window_id: Ok(101),
            selected: Ok(false),
            ..FakeNode::default()
        });
        let group = ax.add_node(FakeNode {
            children: Ok(vec![]),
            tabs: Ok(vec![tab_front, tab_back]),
            ..FakeNode::default()
        });
        ax.edit(group, |n| {
            n.strings.insert(WindowString::Role, Ok("AXTabGroup".to_owned()));
        });
        ax.edit(front, |n| n.children = Ok(vec![group]));
        let nodes = vec![front, back, plain];
        let app_el = ax.add_node(FakeNode {
            windows: Ok(nodes),
            ..FakeNode::default()
        });
        let listed = info(1, "com.a", "A");
        ax.set_application(listed.pid, app_el);
        let (system, _obs) = FakeWindowSystem::new(ax);
        let adapter = WindowAdapter::spawn(
            &format!(
                "stache-test-tiling-real-tabs-{}",
                NAMES.fetch_add(1, Ordering::SeqCst)
            ),
            &AdapterOptions { read_tabs: true },
            Arc::new(SystemWindowClock),
            move || system,
            RunLoopDriver::new,
        )
        .expect("spawn");
        let backend = Arc::new(SystemWindowBackend::from_adapter(adapter, true));
        let directory = Arc::new(FakeAppDirectory::default());
        *directory.apps.lock().expect("apps") = vec![listed];
        let runtime = TrackingRuntime::start(
            backend as Arc<dyn WindowBackend>,
            directory as Arc<dyn crate::actor::AppDirectory>,
            &TrackingOptions::new(""),
        )
        .expect("start");
        runtime.submit_displays(None, one_display()).expect("displays");
        wait_until("enumeration with tabs", || {
            let snap = runtime.snapshot();
            snap.phase == Phase::Complete && snap.windows.len() == 3
        });
        let snap = runtime.snapshot();
        let inactive: Vec<u32> = snap
            .windows
            .iter()
            .filter(|w| w.inactive_tab)
            .filter_map(|w| w.cg_window_id)
            .collect();
        assert_eq!(inactive, [101], "only the unselected tab is held back");
        assert!(snap.tabs.reading);
        assert_eq!((snap.tabs.groups, snap.tabs.inactive_windows), (1, 1));
        assert_eq!(snap.tabs.stats.initial_scans, 1);
        runtime.shutdown();
    }
}

mod drag;
mod frames;
mod visibility;
