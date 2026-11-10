//! Application shards: one AX owner thread per tracked application behind the
//! router, over the fake AX arena. No real AX is touched.

use std::collections::HashSet;

use super::*;

/// One adapter over a shared fake arena with `apps` applications ready to track.
pub(super) struct Rig {
    pub(super) adapter: Arc<WindowAdapter>,
    pub(super) ax: FakeWindowAx,
    pub(super) obs: FakeObservers,
    pub(super) queue: Arc<EventQueue>,
    pub(super) gate: Arc<GenerationCell>,
    pub(super) epoch: RuntimeEpoch,
    pub(super) prefix: String,
    pub(super) systems: Arc<AtomicUsize>,
    /// The application node of each application, in track order.
    pub(super) app_nodes: Vec<NodeId>,
}

pub(super) fn rig(windows_per_app: &[&[u32]], limit: Option<usize>) -> Rig {
    rig_with(windows_per_app, limit, &AdapterOptions::default())
}

pub(super) fn rig_with(
    windows_per_app: &[&[u32]],
    limit: Option<usize>,
    options: &AdapterOptions,
) -> Rig {
    let ax = FakeWindowAx::new();
    let mut app_nodes = Vec::new();
    for (index, cgs) in windows_per_app.iter().enumerate() {
        let nodes: Vec<NodeId> = cgs.iter().map(|cg| window_node(&ax, *cg)).collect();
        app_nodes.push(app_node(&ax, 900 + i32::try_from(index).expect("index"), nodes));
    }
    let (system, obs) = FakeWindowSystem::factory(ax.clone());
    let systems = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&systems);
    let prefix = unique_name();
    let mut adapter = WindowAdapter::spawn(
        &prefix,
        options,
        Arc::new(SystemWindowClock),
        move || {
            counter.fetch_add(1, Ordering::SeqCst);
            system()
        },
        RunLoopDriver::new,
    )
    .expect("spawn");
    if let Some(limit) = limit {
        adapter = adapter.with_dedicated_limit(limit);
    }
    let queue = EventQueue::new(1024);
    let gate = Arc::new(GenerationCell::new());
    gate.publish(1, 1);
    let epoch = adapter
        .install(
            InstallRequest {
                queue: Arc::clone(&queue),
                gate: Arc::clone(&gate),
            },
            None,
        )
        .expect("install");
    Rig {
        adapter: Arc::new(adapter),
        ax,
        obs,
        queue,
        gate,
        epoch,
        prefix,
        systems,
        app_nodes,
    }
}

impl Rig {
    pub(super) fn track(&self, index: usize) -> TrackOutcome {
        self.adapter.track_application(
            self.epoch,
            TrackRequest {
                application: app(1 + index as u64),
                pid: 900 + i32::try_from(index).expect("index"),
                cancel: CancellationToken::new(),
                deadline: None,
            },
            None,
        )
    }

    /// Drains until `count` enumerations completed; returns every event seen.
    fn enumerated(&self, count: usize) -> Vec<WindowEvent> {
        let mut seen = Vec::new();
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end
            && seen
                .iter()
                .filter(|e: &&WindowEvent| {
                    matches!(e.kind, WindowEventKind::EnumerationComplete { .. })
                })
                .count()
                < count
        {
            seen.extend(self.queue.drain().events);
            thread::sleep(Duration::from_millis(2));
        }
        seen
    }

    pub(super) fn tracked(&self, apps: usize) -> Vec<WindowEvent> {
        for index in 0..apps {
            assert_eq!(self.track(index), TrackOutcome::Tracking);
        }
        self.enumerated(apps)
    }

    fn request(window: WindowInstance, x: f64, deadline: Duration) -> ActionRequest {
        ActionRequest {
            issuance: WindowIssuance::new(window, 1, 1, 1, Instant::now() + deadline),
            action: frame(x),
        }
    }

    pub(super) fn stop(&self, deadline: Option<Instant>) -> StopOutcome {
        self.adapter.stop(self.epoch, &self.gate, deadline)
    }

    fn retry(&self) -> Option<CleanupReport> {
        self.adapter.retry_cleanup(&CancellationToken::new(), None)
    }
}

pub(super) fn window_of(events: &[WindowEvent], application: u64) -> WindowInstance {
    snapshots(events)
        .into_iter()
        .map(|snapshot| snapshot.window)
        .find(|window| window.application() == app(application))
        .expect("window")
}

/// Records the thread each native write ran on and holds writes on `held`.
pub(super) struct WriteHold {
    pub(super) entered: Receiver<String>,
    pub(super) release: SyncSender<()>,
    pub(super) threads: Arc<Mutex<Vec<String>>>,
}

pub(super) fn hold_writes_on(ax: &FakeWindowAx, held: Option<String>) -> WriteHold {
    let (entered_tx, entered) = mpsc::sync_channel::<String>(8);
    let (release, release_rx) = mpsc::sync_channel::<()>(8);
    let release_rx = Mutex::new(release_rx);
    let threads = Arc::new(Mutex::new(Vec::new()));
    let first = AtomicBool::new(true);
    let log = Arc::clone(&threads);
    ax.on_write(move || {
        let name = thread::current().name().unwrap_or_default().to_owned();
        log.lock().unwrap_or_else(PoisonError::into_inner).push(name.clone());
        if held.as_deref() == Some(name.as_str()) && first.swap(false, Ordering::SeqCst) {
            let _ = entered_tx.try_send(name);
            let _ = release_rx
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .recv_timeout(Duration::from_secs(30));
        }
    });
    WriteHold { entered, release, threads }
}

#[test]
fn creating_an_adapter_and_installing_an_epoch_starts_no_shard() {
    let rig = rig(&[&[300]], None);
    assert_eq!(rig.systems.load(Ordering::SeqCst), 0);
    assert_eq!(rig.adapter.shard_stats(), ShardStats::default());
    assert_eq!(rig.track(0), TrackOutcome::Tracking);
    assert_eq!(rig.systems.load(Ordering::SeqCst), 1);
    assert_eq!(rig.adapter.shard_stats().dedicated, 1);
}

#[test]
fn a_held_application_never_delays_another_applications_action_or_snapshot() {
    let rig = rig(&[&[300], &[310]], None);
    let gauge = hold_writes_on(&rig.ax, Some(format!("{}-0", rig.prefix)));
    let events = rig.tracked(2);
    let slow = window_of(&events, 1);
    let quick = window_of(&events, 2);

    let adapter = Arc::clone(&rig.adapter);
    let request = Rig::request(slow, 5.0, Duration::from_secs(30));
    let held = thread::spawn(move || adapter.act(request, &CancellationToken::new()));
    assert_eq!(
        gauge.entered.recv_timeout(Duration::from_secs(5)).expect("entered"),
        format!("{}-0", rig.prefix)
    );

    let started = Instant::now();
    let other = rig.adapter.act(
        Rig::request(quick, 7.0, Duration::from_secs(5)),
        &CancellationToken::new(),
    );
    let snapshot = rig.adapter.snapshot(quick, &CancellationToken::new(), None);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert!(matches!(other, WindowActionOutcome::Issued {
        kind: IssuedKind::Applied,
        ..
    }));
    assert!(matches!(snapshot, SnapshotOutcome::Snapshot(_)));

    gauge.release.send(()).expect("release");
    assert!(matches!(
        held.join().expect("held"),
        WindowActionOutcome::Issued { kind: IssuedKind::Applied, .. }
    ));
    // Each application's writes ran on its own shard thread.
    let threads = gauge.threads.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(threads.contains(&format!("{}-0", rig.prefix)));
    assert!(threads.contains(&format!("{}-1", rig.prefix)));
    assert_eq!(rig.adapter.shard_stats().dedicated, 2);
}

#[test]
fn serials_are_unique_across_shards_and_tokens_name_one_adapter_epoch() {
    let rig = rig(&[&[300, 301], &[310, 311], &[320]], None);
    let events = rig.tracked(3);
    let windows: Vec<_> = snapshots(&events).iter().map(|s| s.window).collect();
    assert_eq!(windows.len(), 5);
    let serials: HashSet<u64> = windows.iter().map(WindowInstance::serial).collect();
    assert_eq!(serials.len(), 5, "every shard draws from one allocator");
    assert!(
        windows
            .iter()
            .all(|w| w.adapter() == rig.adapter.id() && w.epoch() == rig.epoch)
    );
    // Event sequences are monotonic per application and unique per epoch.
    let sequences: HashSet<u64> = events.iter().map(|e| e.sequence).collect();
    assert_eq!(sequences.len(), events.len());
}

#[test]
fn a_cg_window_id_claimed_on_two_shards_resolves_to_nothing() {
    let rig = rig(&[&[300, 301], &[300, 311]], None);
    let events = rig.tracked(2);
    let cancel = CancellationToken::new();
    assert_eq!(
        rig.adapter.resolve_window_id(300, &cancel, None),
        None,
        "ambiguous"
    );
    let unique = rig.adapter.resolve_window_id(311, &cancel, None).expect("unique");
    assert_eq!(
        unique,
        snapshots(&events)
            .iter()
            .find(|s| s.cg_window_id == Some(311))
            .expect("311")
            .window
    );
    assert_eq!(rig.adapter.resolve_window_id(999, &cancel, None), None);
}

#[test]
fn applications_past_the_cap_share_the_overflow_shard() {
    let rig = rig(&[&[300], &[310], &[320], &[330]], Some(2));
    let gauge = hold_writes_on(&rig.ax, None);
    let events = rig.tracked(4);
    let stats = rig.adapter.shard_stats();
    assert_eq!((stats.dedicated, stats.overflow_applications), (2, 2));
    for application in 1..=4 {
        let outcome = rig.adapter.act(
            Rig::request(window_of(&events, application), 9.0, Duration::from_secs(5)),
            &CancellationToken::new(),
        );
        assert!(matches!(outcome, WindowActionOutcome::Issued {
            kind: IssuedKind::Applied,
            ..
        }));
    }
    let threads: HashSet<String> = gauge
        .threads
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .cloned()
        .collect();
    let expected: HashSet<String> = [
        format!("{}-0", rig.prefix),
        format!("{}-1", rig.prefix),
        format!("{}-overflow", rig.prefix),
    ]
    .into_iter()
    .collect();
    assert_eq!(threads, expected);
    assert!(!rig.adapter.is_overflow(app(1)), "a dedicated shard");
    assert!(rig.adapter.is_overflow(app(3)), "past the cap");
    assert!(!rig.adapter.is_overflow(app(99)), "untracked");

    // Untracking an overflow application leaves the other one working.
    let cancel = CancellationToken::new();
    assert_eq!(
        rig.adapter.untrack_application(rig.epoch, app(3), &cancel, None),
        StopOutcome::Clean { applications: 1 }
    );
    assert_eq!(rig.adapter.shard_stats().overflow_applications, 1);
    assert!(matches!(
        rig.adapter.snapshot(window_of(&events, 4), &cancel, None),
        SnapshotOutcome::Snapshot(_)
    ));
    // The last overflow application takes the overflow shard with it.
    assert_eq!(
        rig.adapter.untrack_application(rig.epoch, app(4), &cancel, None),
        StopOutcome::Clean { applications: 1 }
    );
    assert_eq!(rig.adapter.shard_stats(), ShardStats {
        dedicated: 2,
        ..ShardStats::default()
    });
    assert_eq!(rig.stop(None), StopOutcome::Clean { applications: 2 });
    assert_eq!(rig.adapter.close(), CloseOutcome::Closed);
    assert_eq!(rig.ax.live_elements(), 0);
}

#[test]
fn a_refused_shard_is_a_typed_outcome_with_nothing_left_running() {
    let rig = rig(&[&[300]], None);
    // Another live adapter owns the name this adapter's first shard needs.
    let squatter = WindowAdapter::spawn(
        &format!("{}-0", rig.prefix),
        &AdapterOptions::default(),
        Arc::new(SystemWindowClock),
        || FakeWindowSystem::new(FakeWindowAx::new()).0,
        RunLoopDriver::new,
    )
    .expect("squatter");
    assert_eq!(rig.track(0), TrackOutcome::OwnerUnavailable);
    assert_eq!(rig.systems.load(Ordering::SeqCst), 0, "no thread ran");
    assert_eq!(rig.adapter.shard_stats(), ShardStats::default());
    assert_eq!(squatter.close(), CloseOutcome::Closed);
    // The slot was released, so the same application tracks once the name is free.
    assert_eq!(rig.track(0), TrackOutcome::Tracking);
    assert_eq!(rig.adapter.shard_stats().dedicated, 1);
}

#[test]
fn untracking_during_enumeration_ends_the_shard_and_retires_its_tokens() {
    let rig = rig(&[&[300, 301, 302, 303], &[310]], None);
    assert_eq!(rig.track(0), TrackOutcome::Tracking);
    let cancel = CancellationToken::new();
    // No wait: enumeration may still be running on the shard.
    assert_eq!(
        rig.adapter.untrack_application(rig.epoch, app(1), &cancel, None),
        StopOutcome::Clean { applications: 1 }
    );
    assert_eq!(rig.adapter.shard_stats(), ShardStats::default());
    let after = rig.adapter.snapshot(
        WindowInstance::for_testing(rig.adapter.id(), rig.epoch, app(1), 1),
        &cancel,
        None,
    );
    assert_eq!(after, SnapshotOutcome::Stale(StaleReason::ApplicationGone));
    // The other application is unaffected and the freed name is reusable.
    assert_eq!(rig.track(1), TrackOutcome::Tracking);
    let mut events = Vec::new();
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end
        && !events.iter().any(|e: &WindowEvent| {
            matches!(e.kind, WindowEventKind::EnumerationComplete { application, .. } if application == app(2))
        })
    {
        events.extend(rig.queue.drain().events);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(matches!(
        rig.adapter.snapshot(window_of(&events, 2), &cancel, None),
        SnapshotOutcome::Snapshot(_)
    ));
    assert_eq!(rig.stop(None), StopOutcome::Clean { applications: 1 });
    assert_eq!(rig.adapter.close(), CloseOutcome::Closed);
    assert_eq!(rig.ax.live_elements(), 0);
}

#[test]
fn a_stuck_shard_is_kept_by_stop_and_cleaned_by_retry() {
    let rig = rig(&[&[300], &[310]], None);
    let gauge = hold_writes_on(&rig.ax, Some(format!("{}-0", rig.prefix)));
    let events = rig.tracked(2);
    let adapter = Arc::clone(&rig.adapter);
    let request = Rig::request(window_of(&events, 1), 5.0, Duration::from_secs(30));
    let held = thread::spawn(move || adapter.act(request, &CancellationToken::new()));
    gauge.entered.recv_timeout(Duration::from_secs(5)).expect("entered");

    let outcome = rig.stop(Some(Instant::now() + Duration::from_millis(300)));
    assert_eq!(outcome, StopOutcome::Unacknowledged, "one stuck shard decides");
    assert!(rig.adapter.cleanup_unconfirmed());
    assert_eq!(
        rig.adapter.shard_stats().quarantined,
        1,
        "the handle is kept, not detached"
    );
    let refused = rig.adapter.install(
        InstallRequest {
            queue: Arc::clone(&rig.queue),
            gate: Arc::new(GenerationCell::new()),
        },
        None,
    );
    assert!(matches!(refused, Err(InstallError::Quarantined { .. })));

    gauge.release.send(()).expect("release");
    let _ = held.join().expect("held");
    let report = rig.retry().expect("report");
    assert_eq!(report.remaining_applications, 0);
    assert!(!rig.adapter.cleanup_unconfirmed());
    assert_eq!(rig.adapter.shard_stats(), ShardStats::default());
    assert_eq!(rig.ax.live_elements(), 0);
    let next = rig
        .adapter
        .install(
            InstallRequest {
                queue: Arc::clone(&rig.queue),
                gate: Arc::new(GenerationCell::new()),
            },
            None,
        )
        .expect("install after retry");
    assert!(next > rig.epoch);
    assert_eq!(rig.adapter.close(), CloseOutcome::Closed);
}

#[test]
fn a_quarantined_shard_blocks_install_until_a_retry_and_stop_sums_the_counts() {
    let rig = rig(&[&[300], &[310]], None);
    let _events = rig.tracked(2);
    rig.obs.set_remove_error(900, Some(AxError::CannotComplete));
    let outcome = rig.stop(None);
    assert!(
        matches!(outcome, StopOutcome::Quarantined { applications: 1, registrations } if registrations > 0),
        "{outcome:?}"
    );
    assert_eq!(rig.adapter.shard_stats().quarantined, 1);
    rig.obs.set_remove_error(900, None);
    let report = rig.retry().expect("report");
    assert_eq!(report.cleaned_applications, 1);
    assert_eq!(report.remaining_applications, 0);
    assert_eq!(rig.adapter.shard_stats(), ShardStats::default());
    assert_eq!(rig.adapter.close(), CloseOutcome::Closed);
    assert_eq!(rig.ax.live_elements(), 0);
}

/// An untracked application, a stale epoch and a foreign adapter answer exactly
/// what the single owner core answered.
#[allow(
    clippy::too_many_lines,
    reason = "one table of parity cases against the single core"
)]
#[test]
fn unrouted_commands_answer_what_the_single_core_answered() {
    let mut core = H::new();
    let rig = rig(&[&[300]], None);
    let cancel = CancellationToken::new();
    let ghost = app(77);
    let wrong_epoch = RuntimeEpoch::new(9);

    let core_snapshot = |h: &mut H, adapter, epoch, application| {
        let window = WindowInstance::for_testing(adapter, epoch, application, 1);
        h.run(|reply| Command::Snapshot { window, reply })
    };
    for (core_epoch, router_epoch, foreign) in [
        (core.epoch, rig.epoch, false),
        (wrong_epoch, wrong_epoch, false),
        (core.epoch, rig.epoch, true),
    ] {
        let core_adapter = if foreign {
            AdapterId::for_testing()
        } else {
            core.adapter
        };
        let router_adapter = if foreign {
            AdapterId::for_testing()
        } else {
            rig.adapter.id()
        };
        assert_eq!(
            core_snapshot(&mut core, core_adapter, core_epoch, ghost),
            rig.adapter.snapshot(
                WindowInstance::for_testing(router_adapter, router_epoch, ghost, 1),
                &cancel,
                None
            ),
        );
    }
    // Actions: the gate, the empty request and the deadline come first.
    let core_window = WindowInstance::for_testing(core.adapter, core.epoch, ghost, 1);
    let router_window = WindowInstance::for_testing(rig.adapter.id(), rig.epoch, ghost, 1);
    for action in [frame(5.0), WindowAction::SetFrame {
        position: None,
        size: None,
    }] {
        let core_request = ActionRequest {
            issuance: WindowIssuance::new(
                core_window,
                1,
                1,
                1,
                Instant::now() + Duration::from_secs(30),
            ),
            action,
        };
        let router_request = ActionRequest {
            issuance: WindowIssuance::new(
                router_window,
                1,
                1,
                1,
                Instant::now() + Duration::from_secs(30),
            ),
            action,
        };
        assert_eq!(
            core.run(|reply| Command::Act { request: core_request, reply }),
            rig.adapter.act(router_request, &cancel),
        );
    }
    let stale_generation = ActionRequest {
        issuance: WindowIssuance::new(
            router_window,
            5,
            1,
            1,
            Instant::now() + Duration::from_secs(30),
        ),
        action: frame(5.0),
    };
    assert_eq!(
        rig.adapter.act(stale_generation, &cancel),
        WindowActionOutcome::NotIssued(NotIssuedReason::GenerationInvalid)
    );
    // Hidden, focus, refresh and untrack of an unknown application.
    let hidden = |epoch| HiddenRequest {
        epoch,
        application: ghost,
        operation: HiddenOperation::Read,
        issuance: None,
    };
    for (core_epoch, router_epoch) in [(core.epoch, rig.epoch), (wrong_epoch, wrong_epoch)] {
        let request = hidden(core_epoch);
        assert_eq!(
            core.run(|reply| Command::Hidden { request, reply }),
            rig.adapter.application_hidden(hidden(router_epoch), &cancel, None),
        );
        assert_eq!(
            core.run(|reply| Command::FocusedWindow {
                epoch: core_epoch,
                application: ghost,
                reply
            }),
            rig.adapter.focused_window(router_epoch, ghost, &cancel, None),
        );
        assert_eq!(
            core.run(|reply| Command::Refresh {
                epoch: core_epoch,
                application: ghost,
                cancel: CancellationToken::new(),
                deadline: None,
                reply,
            }),
            rig.adapter
                .refresh_application(router_epoch, ghost, CancellationToken::new(), None),
        );
        assert_eq!(
            core.run(|reply| Command::Untrack {
                epoch: core_epoch,
                application: ghost,
                reply
            }),
            rig.adapter.untrack_application(router_epoch, ghost, &cancel, None),
        );
    }
    // Before any epoch is installed (after a stop), everything is a wrong epoch.
    assert_eq!(rig.stop(None), StopOutcome::Clean { applications: 0 });
    assert_eq!(
        rig.adapter.snapshot(router_window, &cancel, None),
        SnapshotOutcome::Stale(StaleReason::WrongEpoch)
    );
    assert_eq!(rig.adapter.close(), CloseOutcome::Closed);
}

#[test]
fn shard_threads_never_take_menu_anywheres_name() {
    // A prefix that would derive the reserved name is itself the reserved name.
    let attempt = WindowAdapter::spawn(
        "stache-ax-owner",
        &AdapterOptions::default(),
        Arc::new(SystemWindowClock),
        || FakeWindowSystem::new(FakeWindowAx::new()).0,
        RunLoopDriver::new,
    );
    assert_eq!(attempt.err(), Some(SpawnError::ReservedThreadName));
    assert_eq!(MAX_APPLICATION_SHARDS, 32);
}

// ---- join state --------------------------------------------------------------

fn joinable() -> (super::adapter::JoinSlot, SyncSender<()>) {
    use super::adapter::{JoinSlot, Thread};
    let (end_tx, end_rx) = mpsc::sync_channel::<()>(1);
    let (done_tx, done_rx) = mpsc::sync_channel::<()>(1);
    let join = thread::spawn(move || {
        let _ = end_rx.recv();
        let _ = done_tx.send(());
    });
    (JoinSlot::new(Thread::new(done_rx, join)), end_tx)
}

#[test]
fn a_join_timeout_keeps_the_thread_and_a_later_join_after_it_ended_succeeds() {
    let (slot, end) = joinable();
    assert!(
        !slot.join(Instant::now() + Duration::from_millis(20)),
        "still running"
    );
    assert!(!slot.join(Instant::now()), "still running, still not joined");
    end.send(()).expect("end");
    assert!(slot.join(Instant::now() + Duration::from_secs(5)));
    assert!(slot.join(Instant::now()), "joined stays joined");
}

#[test]
fn a_concurrent_joiner_never_reports_a_thread_that_is_still_alive_as_joined() {
    let (slot, end) = joinable();
    let slot = Arc::new(slot);
    let waiting = Arc::clone(&slot);
    let first = thread::spawn(move || waiting.join(Instant::now() + Duration::from_secs(10)));
    thread::sleep(Duration::from_millis(100));
    assert!(
        !slot.join(Instant::now() + Duration::from_millis(10)),
        "another caller is joining a live thread"
    );
    end.send(()).expect("end");
    assert!(first.join().expect("first joiner"));
    assert!(slot.join(Instant::now()));
}
