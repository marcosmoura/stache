//! The effect runner under a slow or hung application: read effects run on
//! per-application lanes, so one application that does not answer delays only
//! its own reads. Driven over the real [`ThreadRunner`] with fake native edges
//! and backend holds; every wait is for a count or a hold, never for time.

use std::sync::Mutex;

use stache_macos::windows::FocusOutcome;
use stache_macos::{FakeAppKey, VisibilityOperation};

use super::*;
use crate::actor::{Clock, FrameControl, RUNNER_CAPACITY, RunnerError};
use crate::state::{AppInfo, CompletionResult, Effect, Stamp};
use crate::visibility::{Exec, Observation, Planned, VisibilityControl};
use crate::visibility_tests::{Native, entry, requirement};

const REGISTRY: u64 = 7;

/// Waits until `condition` holds, at most ten seconds; `false` on timeout.
fn reached(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    true
}

/// The real runner over fake native edges: the window backend, the
/// application backend (hide, unhide, activation) and a mailbox whose
/// completions the test reads.
struct Bench {
    native: Arc<Native>,
    windows: Arc<FakeWindowBackend>,
    ingress: Arc<Ingress>,
    vis: Arc<VisibilityControl>,
    actions: Arc<crate::visibility::ApplicationActions>,
    runner: ThreadRunner,
    cancel: CancellationToken,
    stamp: Stamp,
    ids: Vec<(ApplicationId, FakeAppKey)>,
    seen: Mutex<Vec<CompletionResult>>,
}

impl Bench {
    fn new(bundles: &[&str], capacity: usize) -> Self {
        let native = Arc::new(Native::new(Duration::from_millis(500)));
        let (actions, ids) = native.install(bundles);
        let actions = Arc::new(actions);
        let windows = Arc::new(FakeWindowBackend::default());
        let gate = Arc::new(GenerationCell::new());
        gate.publish(REGISTRY, 1);
        let epoch = windows.install(EventQueue::new(8), Arc::clone(&gate), None).expect("install");
        let ingress = Arc::new(Ingress::new(Pipeline::new()));
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let frames = Arc::new(FrameControl::new(gate, Arc::clone(&clock)));
        let cancel = CancellationToken::new();
        let animation = Arc::new(crate::AnimationControl::live(
            Arc::clone(&windows) as Arc<dyn WindowBackend>,
            Arc::clone(&ingress),
            Arc::clone(&frames),
            clock,
            Arc::new(stache_macos::FakeFrameClockFactory::failing("timer")),
            cancel.clone(),
        ));
        let vis = Arc::new(VisibilityControl::new());
        let env = EffectEnv {
            backend: Arc::clone(&windows) as Arc<dyn WindowBackend>,
            directory: Arc::new(FakeAppDirectory::default()),
            ingress: Arc::clone(&ingress),
            cancel: cancel.clone(),
            frames,
            visibility: Arc::clone(&vis),
            actions: Some(Arc::clone(&actions)),
            animation,
            borders: None,
        };
        let runner = ThreadRunner::spawn(env, capacity).expect("runner");
        Self {
            native,
            windows,
            ingress,
            vis,
            actions,
            runner,
            cancel,
            stamp: Stamp { epoch, registry: REGISTRY },
            ids,
            seen: Mutex::new(Vec::new()),
        }
    }

    fn id(&self, n: usize) -> ApplicationId { self.ids[n].0 }

    fn key(&self, n: usize) -> FakeAppKey { self.ids[n].1 }

    fn window(&self, n: usize, serial: u64) -> WindowInstance {
        WindowInstance::for_testing(self.windows.adapter(), self.stamp.epoch, self.id(n), serial)
    }

    fn submit(&self, effect: Effect) {
        if matches!(effect, Effect::Focus { .. }) {
            self.vis.note_focus_dispatched();
        }
        self.runner.submit(effect).expect("queued");
    }

    /// Every completion posted so far.
    fn completions(&self) -> Vec<CompletionResult> {
        let inputs = self.ingress.with(|pipeline| pipeline.take(usize::MAX));
        let mut seen = self.seen.lock().expect("seen");
        seen.extend(inputs.into_iter().filter_map(|input| match input {
            Input::Completion { result, .. } => Some(result),
            _ => None,
        }));
        seen.clone()
    }

    /// Hides application `n` through the ledger and confirms it, as a
    /// finished earlier switch left it (outside the runner).
    fn hidden_and_confirmed(&self, n: usize) {
        let a = self.id(n);
        let planned = self.vis.plan(
            &requirement(vec![entry(a, false, false)]),
            &|_| true,
            true,
            Instant::now(),
        );
        assert_eq!(planned, vec![Planned::Hide(a)]);
        let exec = Exec {
            actions: Some(&self.actions),
            backend: &*self.windows,
            epoch: self.stamp.epoch,
            cancel: &self.cancel,
        };
        self.vis.execute_hide(&exec, a);
        self.vis.observe(a, Observation::Hidden);
    }

    fn requests(&self) -> Vec<(FakeAppKey, VisibilityOperation)> {
        self.native.backend.visibility_requests()
    }
}

fn raise_applied() -> WindowActionOutcome {
    WindowActionOutcome::Issued {
        kind: stache_macos::windows::IssuedKind::Applied,
        native: stache_macos::windows::NativeStatus::Action(Ok(())),
        observed: None,
    }
}

/// PF-08-01: every read of A is held inside the backend (A does not answer).
/// A workspace switch that hides A and focuses a window of B still completes:
/// the hide, the unhide, the activation and the raise go out and B's reads
/// answer.
#[test]
fn a_switch_completes_while_every_read_of_another_application_is_held() {
    let bench = Bench::new(&["com.a", "com.b"], RUNNER_CAPACITY);
    let (a, b) = (bench.id(0), bench.id(1));
    let stamp = bench.stamp;
    bench.hidden_and_confirmed(1);
    let held = bench.windows.hold_reads_of(a);
    bench.submit(Effect::Track {
        stamp,
        application: a,
        pid: 100,
    });
    bench.submit(Effect::Refresh { stamp, application: a });
    bench.submit(Effect::Snapshot {
        stamp,
        window: bench.window(0, 1),
    });
    assert!(
        held.wait_entered(1, Duration::from_secs(10)),
        "A's track is held"
    );

    // The switch: hide A, unhide B, focus B's window (activation, raise), and
    // the reads the switch causes for B.
    let planned = bench.vis.plan(
        &requirement(vec![entry(a, false, false), entry(b, true, true)]),
        &|_| true,
        true,
        Instant::now(),
    );
    assert!(planned.contains(&Planned::Hide(a)) && planned.contains(&Planned::Unhide(b)));
    for request in planned {
        bench.submit(match request {
            Planned::Hide(application) => Effect::HideApp { stamp, application },
            Planned::Unhide(application) => Effect::UnhideApp { stamp, application },
            Planned::Activate(application) => Effect::ActivateApp { stamp, application },
        });
    }
    let target = bench.window(1, 1);
    bench.windows.script_action(raise_applied());
    bench.windows.script_focused(b, target);
    bench.submit(Effect::Focus { stamp, window: target });
    bench.submit(Effect::Snapshot { stamp, window: target });
    bench.submit(Effect::ReadFocus { stamp, application: b });

    let answered = reached(|| {
        let done = bench.completions();
        done.iter()
            .any(|c| matches!(c, CompletionResult::Snapshot(w, _) if *w == target))
            && done.iter().any(|c| matches!(c, CompletionResult::Focus(app, _) if *app == b))
    });
    assert!(
        answered,
        "B's reads did not answer while A's are held: {:?}",
        bench.completions()
    );
    assert!(bench.requests().contains(&(bench.key(0), VisibilityOperation::Hide)));
    assert!(bench.requests().contains(&(bench.key(1), VisibilityOperation::Unhide)));
    assert_eq!(bench.native.backend.exact_activation_requests(), vec![
        bench.key(1)
    ]);
    assert_eq!(bench.windows.action_count(), 1, "the raise went out");
    assert!(bench.completions().iter().any(|c| matches!(
        c,
        CompletionResult::Focus(app, FocusOutcome::Resolved(w)) if *app == b && *w == target
    )));
    // A is still stuck in its track: its later reads have not started.
    assert!(!bench.windows.calls().contains(&FakeBackendCall::Refresh(a)));

    held.release();
    assert!(reached(|| bench
        .windows
        .calls()
        .contains(&FakeBackendCall::Snapshot(bench.window(0, 1)))));
    assert!(bench.runner.close_within(Duration::from_secs(5)));
}

// ------------------------------------------------- the user's desktop at scale

/// The user's ten applications plus thirty more, in listing order.
fn forty_apps() -> Vec<AppInfo> {
    let named = super::scenarios::APPS
        .iter()
        .map(|(bundle, name)| ((*bundle).to_owned(), (*name).to_owned()));
    let more = (11..=40).map(|n| (format!("com.example.app{n}"), format!("App {n}")));
    named
        .chain(more)
        .enumerate()
        .map(|(index, (bundle, name))| info(index as u64 + 1, &bundle, &name))
        .collect()
}

/// Startup with forty applications, one of which (Code, listed second) does
/// not answer: its reads are held until its deadline answers `Unacknowledged`.
/// Every other application is tracked meanwhile; once the stuck one's deadline
/// passes, startup is complete, every other window is laid out, the stuck one
/// is listed as failed and retried on the backoff.
#[test]
#[allow(clippy::too_many_lines, reason = "one linear startup scenario")]
fn forty_applications_start_while_one_never_answers() {
    let backend = Arc::new(FakeWindowBackend::default());
    backend.script_echo_frames(true);
    let apps = forty_apps();
    let stuck = apps[1].application;
    for _ in 0..=crate::actor::TRACK_RETRY_BACKOFF.len() {
        backend.script_track_for(stuck, TrackOutcome::Unacknowledged);
    }
    let held = backend.hold_reads_of(stuck);
    let directory = Arc::new(FakeAppDirectory::default());
    *directory.apps.lock().expect("apps") = apps.clone();
    let runtime = TrackingRuntime::start(
        Arc::clone(&backend) as Arc<dyn WindowBackend>,
        Arc::clone(&directory) as Arc<dyn crate::actor::AppDirectory>,
        &TrackingOptions::new(""),
    )
    .expect("start");
    let mut feed = Feed::new(runtime.epoch().value());
    feed.adapter = backend.adapter();
    runtime.submit_displays(None, one_display()).expect("displays");
    runtime.submit_config(1, super::scenarios::user_config(false)).expect("config");
    assert!(runtime.authorize(1));
    assert!(
        held.wait_entered(1, Duration::from_secs(10)),
        "the stuck track is held"
    );

    let others_tracked = || {
        backend
            .calls()
            .iter()
            .filter(|call| matches!(call, FakeBackendCall::Track(app, _) if *app != stuck))
            .count()
    };
    let all_tracked = reached(|| others_tracked() == apps.len() - 1);
    assert!(
        all_tracked,
        "while the stuck application is held, {} of {} other applications were tracked",
        others_tracked(),
        apps.len() - 1
    );
    // Every application lists one ordinary window, as its shard does once
    // tracked (the enumeration starts with the track).
    let sink = backend.sink().expect("sink");
    let mut windows = Vec::new();
    for (index, info) in apps.iter().enumerate() {
        if info.application == stuck {
            continue;
        }
        let token = WindowInstance::for_testing(feed.adapter, feed.epoch, info.application, 1);
        windows.push(token);
        let cg = 1000 + u32::try_from(index).expect("small");
        assert!(sink.push(feed.event(WindowEventKind::Snapshot {
            snapshot: snapshot(token, &info.name, cg),
            origin: SnapshotOrigin::Enumeration,
        })));
        assert!(sink.push(feed.event(WindowEventKind::EnumerationComplete {
            application: info.application,
            windows: 1,
            truncated: false,
        })));
    }
    assert!(reached(|| windows
        .iter()
        .all(|w| runtime.snapshot().window(*w).is_some())));
    assert_eq!(
        runtime.snapshot().phase,
        Phase::Initializing,
        "the stuck one is still awaited"
    );

    // Its deadline passes: the backend answers `Unacknowledged`.
    held.release();
    assert!(
        reached(|| {
            let snap = runtime.snapshot();
            snap.phase == Phase::Complete && snap.failed_apps.iter().any(|f| f.application == stuck)
        }),
        "{:#?}",
        runtime.snapshot()
    );
    // Every other window is laid out: those on the visible workspace written.
    let snap = runtime.snapshot();
    let visible: Vec<_> = snap.workspaces.iter().filter(|ws| ws.visible).map(|ws| ws.id).collect();
    let shown: Vec<WindowInstance> = windows
        .iter()
        .copied()
        .filter(|w| {
            snap.window(*w)
                .and_then(|w| w.desired.workspace)
                .is_some_and(|id| visible.contains(&id))
        })
        .collect();
    assert!(
        windows
            .iter()
            .all(|w| snap.window(*w).is_some_and(|w| w.desired.workspace.is_some()))
    );
    assert!(!shown.is_empty());
    assert!(reached(|| {
        let written = backend.written_frames();
        shown.iter().all(|w| written.iter().any(|(token, _)| token == w))
    }));
    // Retried on the existing backoff.
    assert!(reached(|| {
        backend
            .calls()
            .iter()
            .filter(|c| matches!(c, FakeBackendCall::Track(app, _) if *app == stuck))
            .count()
            >= 2
    }));
    runtime.shutdown();
}

// ------------------------------------------------------------------ the lanes

impl Bench {
    fn window_of(&self, application: ApplicationId, serial: u64) -> WindowInstance {
        WindowInstance::for_testing(self.windows.adapter(), self.stamp.epoch, application, serial)
    }

    /// The backend reads of `application`, in the order they reached it.
    fn reads_of(&self, application: ApplicationId) -> Vec<FakeBackendCall> {
        self.windows
            .calls()
            .into_iter()
            .filter(|call| match call {
                FakeBackendCall::Track(a, _)
                | FakeBackendCall::Refresh(a)
                | FakeBackendCall::Untrack(a)
                | FakeBackendCall::FocusedWindow(a) => *a == application,
                FakeBackendCall::Snapshot(w) => w.application() == application,
                _ => false,
            })
            .collect()
    }

    fn focus_reads(&self, application: ApplicationId) -> usize {
        self.windows
            .calls()
            .iter()
            .filter(|call| **call == FakeBackendCall::FocusedWindow(application))
            .count()
    }

    fn answered_focus(&self, application: ApplicationId) -> bool {
        self.completions()
            .iter()
            .any(|c| matches!(c, CompletionResult::Focus(app, _) if *app == application))
    }

    /// Holds the effects thread inside the hide of application `n` (its
    /// public hide is refused, so the `AXHidden` fallback runs and is held).
    fn hold_effects_in_hide(&self, n: usize) -> Arc<crate::backend::SendHold> {
        let a = self.id(n);
        self.native.backend.refuse_visibility();
        let held = self.windows.hold_hidden_of(a);
        let planned = self.vis.plan(
            &requirement(vec![entry(a, false, false)]),
            &|_| true,
            true,
            Instant::now(),
        );
        assert_eq!(planned, vec![Planned::Hide(a)]);
        self.submit(Effect::HideApp {
            stamp: self.stamp,
            application: a,
        });
        assert!(held.wait_entered(1, Duration::from_secs(10)), "the hide is held");
        held
    }
}

/// PF-08-02: one application's track, refresh, three snapshots and untrack
/// reach the backend in submission order, one at a time, while the reads of
/// three other applications are in the backend at the same time.
#[test]
fn one_application_s_reads_keep_their_order_while_others_run() {
    let bench = Bench::new(&[], RUNNER_CAPACITY);
    let stamp = bench.stamp;
    let others: Vec<ApplicationId> = (2..=4).map(app).collect();
    let holds: Vec<_> = others.iter().map(|other| bench.windows.hold_reads_of(*other)).collect();
    for other in &others {
        bench.submit(Effect::Track {
            stamp,
            application: *other,
            pid: 1,
        });
        bench.submit(Effect::Snapshot {
            stamp,
            window: bench.window_of(*other, 1),
        });
    }
    for hold in &holds {
        assert!(hold.wait_entered(1, Duration::from_secs(10)));
    }
    let a = app(1);
    let windows: Vec<WindowInstance> = (1..=3).map(|serial| bench.window_of(a, serial)).collect();
    bench.submit(Effect::Track { stamp, application: a, pid: 1 });
    bench.submit(Effect::Refresh { stamp, application: a });
    for window in &windows {
        bench.submit(Effect::Snapshot { stamp, window: *window });
    }
    bench.submit(Effect::Untrack { stamp, application: a });
    let mut expected = vec![FakeBackendCall::Track(a, 1), FakeBackendCall::Refresh(a)];
    expected.extend(windows.iter().map(|w| FakeBackendCall::Snapshot(*w)));
    expected.push(FakeBackendCall::Untrack(a));
    assert!(
        reached(|| bench.reads_of(a).len() == expected.len()),
        "{:?}",
        bench.reads_of(a)
    );
    assert_eq!(bench.reads_of(a), expected);
    // The three others were in the backend the whole time.
    assert_eq!(bench.windows.reads_in_flight(), 3);
    assert_eq!(bench.windows.reads_high_water(), 4, "one of A's reads at a time");
    for hold in &holds {
        hold.release();
    }
    assert!(bench.runner.close_within(Duration::from_secs(5)));
    for other in &others {
        assert_eq!(
            bench.reads_of(*other).len(),
            2,
            "each other's track, then its snapshot"
        );
    }
}

/// PF-08-03: with every backend call held and eight applications to track,
/// four tracks are in the backend at once and never more.
#[test]
fn at_most_four_reads_are_in_the_backend_at_once() {
    let bench = Bench::new(&[], RUNNER_CAPACITY);
    let held = bench.windows.hold_reads();
    for n in 1..=8 {
        bench.submit(Effect::Track {
            stamp: bench.stamp,
            application: app(n),
            pid: 1,
        });
    }
    assert!(held.wait_entered(crate::actor::READ_WORKERS, Duration::from_secs(10)));
    assert_eq!(bench.runner.started_reads(), 4);
    assert_eq!(
        bench.runner.queued_reads(),
        4,
        "the other four wait on their lanes"
    );
    assert_eq!(bench.windows.reads_in_flight(), 4);
    held.release();
    assert!(reached(
        || bench.runner.started_reads() == 8 && bench.windows.reads_in_flight() == 0
    ));
    assert_eq!(bench.windows.reads_high_water(), 4, "never more than four");
    assert!(bench.runner.close_within(Duration::from_secs(5)));
}

/// PF-08-04: a hundred geometry batches for one window of a held application
/// lead to at most two backend reads of that window: the one in the backend
/// and one queued that answers for the rest.
#[test]
fn queued_geometry_reads_of_one_window_coalesce() {
    let bench = Bench::new(&[], RUNNER_CAPACITY);
    let a = app(1);
    let window = bench.window_of(a, 1);
    let frame = crate::geometry::Rect::new(0.0, 25.0, 800.0, 600.0);
    bench.windows.seed_window_frame(window, frame);
    bench.windows.script_bounds(42, window);
    let held = bench.windows.hold_reads_of(a);
    for batch in 0..100 {
        if batch == 1 {
            assert!(
                held.wait_entered(1, Duration::from_secs(10)),
                "the first read is held"
            );
        }
        bench.submit(Effect::ReadGeometry {
            stamp: bench.stamp,
            window,
            cg_window_id: 42,
            start: frame,
            target: frame,
        });
    }
    // A snapshot of the same window coalesces with snapshots only.
    for _ in 0..10 {
        bench.submit(Effect::Snapshot { stamp: bench.stamp, window });
    }
    assert_eq!(
        bench.runner.queued_reads(),
        2,
        "one geometry read and one snapshot queued"
    );
    held.release();
    assert!(bench.runner.close_within(Duration::from_secs(5)));
    let bounds = bench
        .windows
        .calls()
        .iter()
        .filter(|c| **c == FakeBackendCall::Bounds(42))
        .count();
    assert_eq!(bounds, 2);
    assert_eq!(bench.windows.snapshot_reads(window), 1);
    let geometry = bench
        .completions()
        .iter()
        .filter(|c| matches!(c, CompletionResult::Geometry(w, _) if *w == window))
        .count();
    assert_eq!(
        geometry, 2,
        "the queued read answers for the batches it absorbed"
    );
}

/// A geometry read absorbed by one already queued for the same window replaces
/// it in place: the queued read has not started, so it is judged against the
/// newest attempt's range. Here the first queued range explains the window
/// server's frame and the second does not, so the queued read must fall back
/// to the snapshot.
#[test]
fn a_coalesced_geometry_read_runs_with_the_newest_range() {
    let bench = Bench::new(&[], RUNNER_CAPACITY);
    let a = app(1);
    let window = bench.window_of(a, 1);
    let frame = crate::geometry::Rect::new(0.0, 25.0, 800.0, 600.0);
    let elsewhere = crate::geometry::Rect::new(2000.0, 900.0, 400.0, 300.0);
    bench.windows.seed_window_frame(window, frame);
    bench.windows.script_bounds(42, window);
    let held = bench.windows.hold_reads_of(a);
    let read = |range: crate::geometry::Rect| Effect::ReadGeometry {
        stamp: bench.stamp,
        window,
        cg_window_id: 42,
        start: range,
        target: range,
    };
    bench.submit(read(frame));
    assert!(
        held.wait_entered(1, Duration::from_secs(10)),
        "the first read is held"
    );
    bench.submit(read(frame));
    bench.submit(read(elsewhere));
    assert_eq!(bench.runner.queued_reads(), 1, "one queued geometry read");
    held.release();
    assert!(bench.runner.close_within(Duration::from_secs(5)));
    assert_eq!(
        bench.windows.snapshot_reads(window),
        1,
        "the queued read used the newest range, which does not explain the frame"
    );
    let geometry = bench
        .completions()
        .iter()
        .filter(|c| matches!(c, CompletionResult::Geometry(w, _) if *w == window))
        .count();
    assert_eq!(geometry, 1, "only the first read was explained");
}

/// A read queued for an earlier registry completes with that stamp and is
/// dropped as stale, so it must not absorb a read of the current registry.
#[test]
fn a_queued_read_of_an_earlier_registry_does_not_absorb_a_current_one() {
    let bench = Bench::new(&[], RUNNER_CAPACITY);
    let a = app(1);
    let window = bench.window_of(a, 1);
    let held = bench.windows.hold_reads_of(a);
    bench.submit(Effect::Snapshot { stamp: bench.stamp, window });
    assert!(
        held.wait_entered(1, Duration::from_secs(10)),
        "the first read is held"
    );
    let restarted = Stamp {
        registry: bench.stamp.registry + 1,
        ..bench.stamp
    };
    for stamp in [bench.stamp, restarted, bench.stamp, restarted] {
        bench.submit(Effect::Snapshot { stamp, window });
    }
    assert_eq!(bench.runner.queued_reads(), 2, "one queued read per registry");
    held.release();
    assert!(bench.runner.close_within(Duration::from_secs(5)));
}

/// PF-08-05: with the read queue full the next read is refused (the actor
/// records the refusal as before; see
/// `mailbox_saturation_is_a_visible_failure_not_success`), while a frame write
/// and a hide are still accepted by the effects thread.
#[test]
fn a_full_read_queue_refuses_reads_but_never_a_frame_write_or_a_hide() {
    use crate::outcome::FrameComponents;
    let bench = Bench::new(&["com.a"], RUNNER_CAPACITY);
    let stamp = bench.stamp;
    let a = app(9);
    let held = bench.windows.hold_reads_of(a);
    bench.submit(Effect::Track { stamp, application: a, pid: 1 });
    assert!(held.wait_entered(1, Duration::from_secs(10)));
    for serial in 1..=RUNNER_CAPACITY as u64 {
        bench.submit(Effect::Snapshot {
            stamp,
            window: bench.window_of(a, serial),
        });
    }
    assert_eq!(bench.runner.queued_reads(), RUNNER_CAPACITY);
    let refused = bench.runner.submit(Effect::Snapshot {
        stamp,
        window: bench.window_of(a, 999),
    });
    assert_eq!(refused, Err(RunnerError::Full));
    assert_eq!(
        bench.runner.submit(Effect::Refresh { stamp, application: app(10) }),
        Err(RunnerError::Full),
        "the bound is over every lane"
    );
    let window = bench.window_of(a, 1);
    let target = crate::geometry::Rect::new(0.0, 25.0, 800.0, 600.0);
    let write = Effect::WriteFrame {
        stamp,
        revision: 1,
        plan_sequence: 1,
        window,
        cg_window_id: None,
        target,
        components: FrameComponents { position: true, size: true },
        from: target,
        animate: false,
        refresh_hz: Some(60.0),
        display: None,
        cross_display: false,
    };
    assert_eq!(bench.runner.submit(write), Ok(()));
    let hidden = bench.id(0);
    let planned = bench.vis.plan(
        &requirement(vec![entry(hidden, false, false)]),
        &|_| true,
        true,
        Instant::now(),
    );
    assert_eq!(planned, vec![Planned::Hide(hidden)]);
    assert_eq!(
        bench.runner.submit(Effect::HideApp { stamp, application: hidden }),
        Ok(())
    );
    assert!(reached(|| bench
        .requests()
        .contains(&(bench.key(0), VisibilityOperation::Hide))));
    held.release();
    assert!(bench.runner.close_within(Duration::from_secs(5)));
}

/// After cancellation the queued reads are discarded, never run.
#[test]
fn cancellation_discards_queued_reads() {
    let bench = Bench::new(&[], RUNNER_CAPACITY);
    let a = app(1);
    let held = bench.windows.hold_reads_of(a);
    bench.submit(Effect::Track {
        stamp: bench.stamp,
        application: a,
        pid: 1,
    });
    assert!(held.wait_entered(1, Duration::from_secs(10)));
    bench.submit(Effect::Snapshot {
        stamp: bench.stamp,
        window: bench.window_of(a, 1),
    });
    bench.submit(Effect::Untrack {
        stamp: bench.stamp,
        application: a,
    });
    bench.cancel.cancel();
    held.release();
    assert!(bench.runner.close_within(Duration::from_secs(5)));
    assert_eq!(bench.reads_of(a), vec![FakeBackendCall::Track(a, 1)]);
    assert_eq!(bench.runner.lane_entries(), (0, 0));
}

/// PF-08-06: a stop joins every runner thread within its bound; with one read
/// worker held in a backend call the runner is reported owed (its handle
/// kept), and a later stop joins it.
#[test]
fn a_stop_with_a_held_read_worker_leaves_the_runner_owed_until_a_later_stop() {
    let backend = Arc::new(FakeWindowBackend::default());
    let held = backend.hold_reads_of(app(1));
    let directory = Arc::new(FakeAppDirectory::default());
    *directory.apps.lock().expect("apps") = vec![info(1, "com.a", "A"), info(2, "com.b", "B")];
    let runtime = TrackingRuntime::start(
        Arc::clone(&backend) as Arc<dyn WindowBackend>,
        directory as Arc<dyn crate::actor::AppDirectory>,
        &TrackingOptions::new(""),
    )
    .expect("start");
    assert!(held.wait_entered(1, Duration::from_secs(10)));
    assert!(reached(|| backend
        .calls()
        .iter()
        .any(|c| matches!(c, FakeBackendCall::Track(a, _) if *a == app(2)))));
    runtime.quiesce();
    assert!(runtime.join_actor(Duration::from_secs(2)));
    assert!(
        !runtime.close_runner(Duration::from_millis(50)),
        "the held read worker is owed"
    );
    held.release();
    assert!(
        runtime.close_runner(Duration::from_secs(2)),
        "a later stop joins it"
    );
    let _ = runtime.stop_epoch(Duration::from_secs(2));
}

/// Every runner thread ends on an idle stop.
#[test]
fn an_idle_stop_joins_every_runner_thread() {
    let bench = Bench::new(&[], RUNNER_CAPACITY);
    bench.submit(Effect::Track {
        stamp: bench.stamp,
        application: app(1),
        pid: 1,
    });
    assert!(reached(|| !bench.reads_of(app(1)).is_empty()));
    assert!(bench.runner.close_within(Duration::from_secs(2)));
    assert_eq!(bench.runner.submit(Effect::Publish), Err(RunnerError::Closed));
    assert_eq!(
        bench.runner.submit(Effect::Refresh {
            stamp: bench.stamp,
            application: app(1)
        }),
        Err(RunnerError::Closed)
    );
}

// ------------------------------------------ focus reads after Stache's own focus

/// PF-08-09: a focus read of B waits for Stache's own focus of B submitted
/// before it (held between its activation and its raise), then reports the
/// raised window.
#[test]
fn a_focus_read_waits_for_the_raise_of_an_earlier_focus() {
    let bench = Bench::new(&["com.a", "com.b"], RUNNER_CAPACITY);
    let (a, b) = (bench.id(0), bench.id(1));
    let (before, raised) = (bench.window(1, 1), bench.window(1, 2));
    bench.windows.script_focused(b, before);
    bench.windows.script_raise_focuses(true);
    bench.windows.script_action(raise_applied());
    let raise = bench.windows.hold_sends();
    bench.submit(Effect::Focus {
        stamp: bench.stamp,
        window: raised,
    });
    assert!(
        raise.wait_entered(1, Duration::from_secs(10)),
        "held after the activation"
    );
    assert_eq!(bench.native.backend.exact_activation_requests(), vec![
        bench.key(1)
    ]);
    bench.submit(Effect::ReadFocus {
        stamp: bench.stamp,
        application: b,
    });
    // Another application's focus read runs meanwhile; B's has not started.
    bench.submit(Effect::ReadFocus {
        stamp: bench.stamp,
        application: a,
    });
    assert!(reached(|| bench.answered_focus(a)));
    assert_eq!(bench.focus_reads(b), 0, "B's focus read waits for the raise");
    raise.release();
    assert!(reached(|| bench.answered_focus(b)));
    assert!(bench.completions().iter().any(|c| matches!(
        c,
        CompletionResult::Focus(app, FocusOutcome::Resolved(w)) if *app == b && *w == raised
    )));
    // A focus submitted after the read does not hold it.
    let raise = bench.windows.hold_sends();
    bench.windows.script_action(raise_applied());
    bench.submit(Effect::ReadFocus {
        stamp: bench.stamp,
        application: a,
    });
    bench.submit(Effect::Focus {
        stamp: bench.stamp,
        window: bench.window(0, 1),
    });
    assert!(raise.wait_entered(1, Duration::from_secs(10)));
    assert!(reached(|| bench.focus_reads(a) == 2));
    raise.release();
    assert!(bench.runner.close_within(Duration::from_secs(5)));
    assert_eq!(bench.runner.lane_entries(), (0, 0), "settled counts are dropped");
}

/// PF-08-09: a focus read with no earlier focus of its application pending runs
/// at once, also while the effects thread is held inside another
/// application's hide.
#[test]
fn a_focus_read_without_a_pending_focus_runs_while_the_effects_thread_is_busy() {
    let bench = Bench::new(&["com.a", "com.b"], RUNNER_CAPACITY);
    let b = bench.id(1);
    bench.windows.script_focused(b, bench.window(1, 1));
    let hide = bench.hold_effects_in_hide(0);
    bench.submit(Effect::ReadFocus {
        stamp: bench.stamp,
        application: b,
    });
    assert!(reached(|| bench.answered_focus(b)));
    hide.release();
    assert!(bench.runner.close_within(Duration::from_secs(5)));
}

/// PF-08-09: a focus refused by a full effects queue or discarded on
/// cancellation never leaves a focus read waiting.
#[test]
fn a_refused_or_discarded_focus_releases_the_focus_read() {
    // Refused: the effects queue (capacity one) is full behind a held hide.
    let bench = Bench::new(&["com.a", "com.b"], 1);
    let b = bench.id(1);
    let hide = bench.hold_effects_in_hide(0);
    bench.runner.submit(Effect::Publish).expect("fills the queue");
    let focus = Effect::Focus {
        stamp: bench.stamp,
        window: bench.window(1, 1),
    };
    assert_eq!(bench.runner.submit(focus), Err(RunnerError::Full));
    bench.submit(Effect::ReadFocus {
        stamp: bench.stamp,
        application: b,
    });
    assert!(
        reached(|| bench.answered_focus(b)),
        "not held by the refused focus"
    );
    hide.release();
    assert!(bench.runner.close_within(Duration::from_secs(5)));
    assert_eq!(bench.runner.lane_entries(), (0, 0));

    // Discarded: the focus is queued behind the held hide, the read waits for
    // it, then the run is cancelled.
    let bench = Bench::new(&["com.a", "com.b"], RUNNER_CAPACITY);
    let b = bench.id(1);
    let hide = bench.hold_effects_in_hide(0);
    bench.submit(Effect::Focus {
        stamp: bench.stamp,
        window: bench.window(1, 1),
    });
    bench.submit(Effect::ReadFocus {
        stamp: bench.stamp,
        application: b,
    });
    bench.submit(Effect::ReadFocus {
        stamp: bench.stamp,
        application: bench.id(0),
    });
    assert!(reached(|| bench.answered_focus(bench.id(0))));
    assert_eq!(
        bench.runner.queued_reads(),
        1,
        "B's focus read waits for B's focus"
    );
    bench.cancel.cancel();
    hide.release();
    assert!(bench.runner.close_within(Duration::from_secs(5)));
    assert_eq!(bench.focus_reads(b), 0, "discarded, never run");
    assert_eq!(bench.runner.lane_entries(), (0, 0));
}
