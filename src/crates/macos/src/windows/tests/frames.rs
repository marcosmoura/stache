//! The frame mailbox: stage costs, latest-frame-wins, sealing, stop and the
//! exactly-once reply guarantee, over the fake AX arena. No real AX.

use super::shards::{hold_writes_on, rig};
use super::*;
use crate::windows::frames::{self, FrameCallback, MAX_FRAMES_PER_TURN};

pub(super) type Replies = Arc<Mutex<Vec<FrameReply>>>;

pub(super) fn frame_request(
    window: WindowInstance,
    plan: u64,
    x: f64,
    stage: FrameStage,
) -> FrameRequest {
    FrameRequest {
        issuance: WindowIssuance::new(window, 1, 1, plan, Instant::now() + Duration::from_secs(30)),
        position: Some(AxPoint { x, y: 40.0 }),
        size: Some(AxSize { width: 320.0, height: 240.0 }),
        stage,
    }
}

pub(super) fn collector() -> (Replies, impl Fn() -> FrameCallback) {
    let replies: Replies = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&replies);
    let make = move || -> FrameCallback {
        let sink = Arc::clone(&sink);
        Box::new(move |reply| sink.lock().unwrap_or_else(PoisonError::into_inner).push(reply))
    };
    (replies, make)
}

pub(super) fn taken(replies: &Replies) -> Vec<FrameReply> {
    replies.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

fn listings(ax: &FakeWindowAx) -> usize {
    ax.calls()
        .iter()
        .filter(|call| matches!(call, Call::Windows(_, 0, limit) if *limit == MAX_WINDOWS_PER_APPLICATION))
        .count()
}

fn reads(ax: &FakeWindowAx) -> usize {
    ax.calls()
        .iter()
        .filter(|call| matches!(call, Call::Position(_) | Call::Size(_)))
        .count()
}

fn writes(ax: &FakeWindowAx) -> usize {
    ax.calls().iter().filter(|call| matches!(call, Call::SetPosition(..))).count()
}

fn applied(reply: &FrameReply) -> bool {
    matches!(
        reply,
        FrameReply::Done(FrameAck {
            outcome: WindowActionOutcome::Issued { kind: IssuedKind::Applied, .. },
            ..
        })
    )
}

fn not_issued(reply: &FrameReply, reason: NotIssuedReason) -> bool {
    matches!(reply, FrameReply::Done(FrameAck { outcome: WindowActionOutcome::NotIssued(why), sent_at: None })
        if *why == reason)
}

impl H {
    pub(super) fn mailbox(&self) -> Arc<Mutex<frames::FrameMailbox>> { self.core.frame_mailbox() }

    pub(super) fn put(&self, request: FrameRequest) -> Replies {
        let (replies, make) = collector();
        frames::submit(&self.mailbox(), vec![(request, make())]).expect("submit");
        replies
    }

    pub(super) fn tracked_app(&mut self, count: u32) -> (Vec<NodeId>, Vec<WindowInstance>) {
        let (_, nodes, tokens) = self.app_with_windows(app(1), 900, count);
        (nodes, tokens)
    }
}

#[test]
fn stages_list_and_read_exactly_what_they_promise() {
    let mut h = H::new();
    let (_, tokens) = h.tracked_app(1);
    let cases = [
        (FrameStage::Intermediate, 0, 0),
        (FrameStage::First, 1, 0),
        (FrameStage::Final, 1, 0),
        (FrameStage::Direct, 1, 2),
    ];
    for (plan, (stage, listed, read)) in (1..).zip(cases) {
        h.ax.clear_calls();
        let replies = h.put(frame_request(tokens[0], plan, 5.0, stage));
        h.turn();
        let got = taken(&replies);
        assert_eq!(got.len(), 1, "{stage:?}");
        assert!(applied(&got[0]), "{stage:?}: {got:?}");
        let FrameReply::Done(FrameAck { outcome, sent_at }) = got[0] else {
            panic!()
        };
        assert!(sent_at.is_some(), "{stage:?}");
        let WindowActionOutcome::Issued { observed, .. } = outcome else {
            panic!()
        };
        assert_eq!(observed.is_some(), stage == FrameStage::Direct, "{stage:?}");
        assert_eq!(listings(&h.ax), listed, "{stage:?} listings");
        assert_eq!(reads(&h.ax), read, "{stage:?} reads");
        assert_eq!(writes(&h.ax), 1, "{stage:?} writes");
    }
}

#[test]
fn the_same_plan_replaces_a_newer_plan_supersedes_and_an_older_one_is_refused() {
    let mut h = H::new();
    let (_, tokens) = h.tracked_app(1);
    h.ax.clear_calls();
    let first = h.put(frame_request(tokens[0], 5, 1.0, FrameStage::Intermediate));
    let second = h.put(frame_request(tokens[0], 5, 2.0, FrameStage::Intermediate));
    assert_eq!(taken(&first), [FrameReply::Replaced]);
    assert!(taken(&second).is_empty());
    let third = h.put(frame_request(tokens[0], 6, 3.0, FrameStage::Intermediate));
    let superseded = taken(&second);
    assert_eq!(superseded.len(), 1);
    assert!(not_issued(&superseded[0], NotIssuedReason::Superseded));
    let older = h.put(frame_request(tokens[0], 4, 4.0, FrameStage::Intermediate));
    let refused = taken(&older);
    assert_eq!(refused.len(), 1);
    assert!(not_issued(&refused[0], NotIssuedReason::Superseded));
    assert!(taken(&third).is_empty(), "the pending newer plan stays");
    h.turn();
    let done = taken(&third);
    assert_eq!(done.len(), 1);
    assert!(applied(&done[0]));
    assert_eq!(writes(&h.ax), 1, "only the latest frame was written");
    assert_eq!(taken(&first).len(), 1);
    assert_eq!(taken(&second).len(), 1);
    assert_eq!(taken(&older).len(), 1);
}

#[test]
fn an_intermediate_replacing_a_first_keeps_its_listing_and_envelope_size() {
    let mut h = H::new();
    let (_, tokens) = h.tracked_app(1);
    h.ax.clear_calls();
    let first = h.put(frame_request(tokens[0], 5, 1.0, FrameStage::First));
    let mut later = frame_request(tokens[0], 5, 2.0, FrameStage::Intermediate);
    later.size = None;
    let second = h.put(later);
    assert_eq!(taken(&first), [FrameReply::Replaced]);
    h.turn();
    assert!(applied(&taken(&second)[0]));
    assert_eq!(listings(&h.ax), 1, "membership is listed once");
    let sizes =
        h.ax.calls()
            .into_iter()
            .filter(|call| matches!(call, Call::SetSize(..)))
            .count();
    assert_eq!(sizes, 1, "the envelope size is still written");
    assert!(
        h.ax.calls().iter().any(
            |call| matches!(call, Call::SetPosition(_, p) if (p.x - 2.0).abs() < f64::EPSILON)
        ),
        "the newest position wins"
    );
}

#[test]
fn a_final_replacing_a_first_stays_final() {
    let mut h = H::new();
    let (_, tokens) = h.tracked_app(1);
    h.put(frame_request(tokens[0], 5, 1.0, FrameStage::First));
    let second = h.put(frame_request(tokens[0], 5, 2.0, FrameStage::Final));
    h.ax.clear_calls();
    h.turn();
    assert!(applied(&taken(&second)[0]));
    assert_eq!(listings(&h.ax), 1);
}

#[test]
fn a_full_mailbox_refuses_the_whole_batch_and_calls_nothing() {
    let h = H::new();
    let called = Arc::new(AtomicUsize::new(0));
    let window = |serial| WindowInstance::for_testing(h.adapter, h.epoch, app(1), serial);
    let batch = |range: std::ops::Range<u64>| -> Vec<(FrameRequest, FrameCallback)> {
        range
            .map(|serial| {
                let called = Arc::clone(&called);
                let callback: FrameCallback = Box::new(move |_| {
                    called.fetch_add(1, Ordering::SeqCst);
                });
                (
                    frame_request(window(serial), 1, 1.0, FrameStage::Intermediate),
                    callback,
                )
            })
            .collect()
    };
    let bound = FRAME_MAILBOX_BOUND as u64;
    assert_eq!(
        frames::submit(&h.mailbox(), batch(0..bound + 1)),
        Err(DispatchError::Full)
    );
    assert_eq!(called.load(Ordering::SeqCst), 0);
    assert_eq!(frames::submit(&h.mailbox(), batch(0..bound)), Ok(()));
    assert_eq!(
        frames::submit(&h.mailbox(), batch(bound..bound + 1)),
        Err(DispatchError::Full)
    );
    assert_eq!(
        called.load(Ordering::SeqCst),
        0,
        "nothing was queued or answered"
    );
    // A replacement of an already pending window still fits.
    assert_eq!(frames::submit(&h.mailbox(), batch(0..1)), Ok(()));
    assert_eq!(
        called.load(Ordering::SeqCst),
        1,
        "the replaced frame was answered"
    );
}

#[test]
fn a_window_destroyed_mid_sequence_ends_an_intermediate_frame_as_stale() {
    let mut h = H::new();
    let (nodes, tokens) = h.tracked_app(1);
    h.ax.edit(nodes[0], |node| {
        let _ = std::mem::replace(&mut node.set_position_result, Err(AxError::InvalidElement));
    });
    let replies = h.put(frame_request(tokens[0], 1, 1.0, FrameStage::Intermediate));
    h.turn();
    assert!(matches!(taken(&replies)[..], [FrameReply::Done(FrameAck {
        outcome: WindowActionOutcome::Stale(StaleReason::Destroyed),
        ..
    })]));
    // The same failure on a full-validation frame is a truthful rejection.
    let replies = h.put(frame_request(tokens[0], 2, 1.0, FrameStage::Final));
    h.turn();
    assert!(matches!(taken(&replies)[..], [FrameReply::Done(FrameAck {
        outcome: WindowActionOutcome::Issued { .. },
        ..
    })]));
}

#[test]
fn a_retired_token_is_stale_without_a_listing() {
    let mut h = H::new();
    let (_, tokens) = h.tracked_app(1);
    let gone = WindowInstance::for_testing(h.adapter, h.epoch, app(1), tokens[0].serial() + 50);
    h.ax.clear_calls();
    let replies = h.put(frame_request(gone, 1, 1.0, FrameStage::Intermediate));
    h.turn();
    assert!(matches!(taken(&replies)[..], [FrameReply::Done(FrameAck {
        outcome: WindowActionOutcome::Stale(StaleReason::UnknownWindow),
        sent_at: None
    })]));
    assert_eq!(listings(&h.ax), 0);
}

#[test]
fn stop_answers_every_pending_frame_as_sealed_exactly_once() {
    let mut h = H::new();
    // Frames run before commands, so only frames beyond the per-turn bound are
    // still pending when the stop runs.
    let (_, tokens) = h.tracked_app(u32::try_from(MAX_FRAMES_PER_TURN + 3).expect("small"));
    let (fillers, tokens) = tokens.split_at(MAX_FRAMES_PER_TURN);
    for token in fillers {
        h.put(frame_request(*token, 1, 1.0, FrameStage::First));
    }
    let pending: Vec<Replies> = tokens
        .iter()
        .map(|token| h.put(frame_request(*token, 1, 1.0, FrameStage::First)))
        .collect();
    let epoch = h.epoch;
    let stopped = h.send(|reply| Command::Stop { epoch, reply });
    h.turn();
    assert!(stopped.lock().unwrap_or_else(PoisonError::into_inner).is_some());
    for replies in &pending {
        let got = taken(replies);
        assert_eq!(got.len(), 1);
        assert!(not_issued(&got[0], NotIssuedReason::Sealed), "{got:?}");
    }
    assert_eq!(
        writes(&h.ax),
        MAX_FRAMES_PER_TURN,
        "only the bounded batch ran first"
    );
}

#[test]
fn untrack_answers_only_that_applications_pending_frames() {
    let mut h = H::new();
    let (_, tokens) = h.tracked_app(1);
    let other_nodes: Vec<_> = (0..=MAX_FRAMES_PER_TURN)
        .map(|n| window_node(&h.ax, 700 + u32::try_from(n).expect("small")))
        .collect();
    app_node(&h.ax, 901, other_nodes);
    assert_eq!(h.track(app(2), 901), TrackOutcome::Tracking);
    h.settle();
    let others: Vec<_> = snapshots(&h.events())
        .into_iter()
        .map(|snapshot| snapshot.window)
        .filter(|window| window.application() == app(2))
        .collect();
    // Frames run before commands: the bounded batch is the other
    // application's, and the untrack finds the rest still pending.
    let (fillers, rest) = others.split_at(MAX_FRAMES_PER_TURN);
    for token in fillers {
        h.put(frame_request(*token, 1, 1.0, FrameStage::First));
    }
    let doomed = h.put(frame_request(tokens[0], 1, 1.0, FrameStage::First));
    let kept = h.put(frame_request(rest[0], 1, 1.0, FrameStage::First));
    let epoch = h.epoch;
    let _ = h.send(|reply| Command::Untrack {
        epoch,
        application: app(1),
        reply,
    });
    h.turn();
    let got = taken(&doomed);
    assert!(matches!(got[..], [FrameReply::Done(FrameAck {
        outcome: WindowActionOutcome::Stale(StaleReason::ApplicationGone),
        sent_at: None
    })]));
    assert!(
        taken(&kept).is_empty(),
        "the other application's frame is still pending"
    );
    h.settle();
    let got = taken(&kept);
    assert_eq!(got.len(), 1);
    assert!(applied(&got[0]), "the other application's frame was written");
}

#[test]
fn a_frame_queued_before_an_untrack_in_the_same_turn_is_answered_once() {
    let mut h = H::new();
    let (_, tokens) = h.tracked_app(1);
    // The common case: one frame and then an untrack of its application in the
    // same turn. Frames run first, so the frame is written against
    // still-tracked membership; the untrack then finds nothing pending.
    let queued = h.put(frame_request(tokens[0], 1, 1.0, FrameStage::First));
    let epoch = h.epoch;
    let _ = h.send(|reply| Command::Untrack {
        epoch,
        application: app(1),
        reply,
    });
    h.turn();
    h.settle();
    let got = taken(&queued);
    assert_eq!(got.len(), 1, "answered exactly once: {got:?}");
    assert!(applied(&got[0]), "written before the untrack: {got:?}");
}

#[test]
fn terminate_and_drop_answer_pending_frames() {
    let mut h = H::new();
    let (_, tokens) = h.tracked_app(u32::try_from(MAX_FRAMES_PER_TURN + 2).expect("small"));
    for token in &tokens[..MAX_FRAMES_PER_TURN] {
        h.put(frame_request(*token, 1, 1.0, FrameStage::First));
    }
    let terminated = h.put(frame_request(
        tokens[MAX_FRAMES_PER_TURN],
        1,
        1.0,
        FrameStage::First,
    ));
    let _ = h.send(|reply| Command::Terminate { reply });
    assert!(h.turn().finished);
    let got = taken(&terminated);
    assert_eq!(got.len(), 1);
    assert!(not_issued(&got[0], NotIssuedReason::Sealed), "{got:?}");
    // A closed mailbox refuses later frames and calls nothing.
    let (late, make) = collector();
    let request = frame_request(tokens[MAX_FRAMES_PER_TURN + 1], 1, 1.0, FrameStage::First);
    assert_eq!(
        frames::submit(&h.mailbox(), vec![(request, make())]),
        Err(DispatchError::Closed)
    );
    assert!(taken(&late).is_empty());

    let mut h = H::new();
    let (_, tokens) = h.tracked_app(1);
    let dropped = h.put(frame_request(tokens[0], 1, 1.0, FrameStage::First));
    drop(h);
    let got = taken(&dropped);
    assert_eq!(got.len(), 1);
    assert!(not_issued(&got[0], NotIssuedReason::OwnerUnavailable), "{got:?}");
}

#[test]
fn a_panicking_write_answers_owner_unavailable_once_and_poisons_the_shard() {
    let mut h = H::new();
    let (_, tokens) = h.tracked_app(2);
    h.ax.on_write(|| panic!("injected"));
    let first = h.put(frame_request(tokens[0], 1, 1.0, FrameStage::First));
    let second = h.put(frame_request(tokens[1], 1, 1.0, FrameStage::First));
    h.turn();
    assert!(h.core.is_poisoned());
    for replies in [&first, &second] {
        let got = taken(replies);
        assert_eq!(got.len(), 1);
        assert!(not_issued(&got[0], NotIssuedReason::OwnerUnavailable), "{got:?}");
    }
    let later = h.put(frame_request(tokens[0], 2, 1.0, FrameStage::First));
    h.turn();
    assert_eq!(taken(&later).len(), 1);
}

#[test]
fn a_burst_of_frames_is_written_in_bounded_turns() {
    let mut h = H::new();
    let (_, tokens) = h.tracked_app(40);
    let pending: Vec<Replies> = tokens
        .iter()
        .map(|token| h.put(frame_request(*token, 1, 1.0, FrameStage::Intermediate)))
        .collect();
    assert!(h.turn().active, "leftovers re-arm the turn");
    let first_turn = pending.iter().filter(|replies| !taken(replies).is_empty()).count();
    assert!(first_turn < 40 && first_turn > 0, "{first_turn}");
    h.settle();
    assert!(pending.iter().all(|replies| taken(replies).len() == 1));
}

#[test]
fn a_queued_snapshot_observes_the_frame_pending_before_it() {
    let mut h = H::new();
    let (_, tokens) = h.tracked_app(1);
    let replies = h.put(frame_request(tokens[0], 1, 77.0, FrameStage::Intermediate));
    let window = tokens[0];
    let slot = h.send(|reply| Command::Snapshot { window, reply });
    h.turn();
    assert!(applied(&taken(&replies)[0]), "the frame was written first");
    let outcome = slot.lock().unwrap_or_else(PoisonError::into_inner).take().expect("answered");
    let SnapshotOutcome::Snapshot(snapshot) = outcome else {
        panic!("{outcome:?}")
    };
    assert!(
        format!("{:?}", snapshot.frame).contains("77.0"),
        "the snapshot saw the written position: {:?}",
        snapshot.frame
    );
}

#[test]
fn frames_reaching_a_stopped_shard_answer_sealed_not_wrong_epoch() {
    let mut h = H::new();
    let (_, tokens) = h.tracked_app(1);
    h.stop();
    let replies = h.put(frame_request(tokens[0], 1, 1.0, FrameStage::Intermediate));
    h.turn();
    let got = taken(&replies);
    assert_eq!(got.len(), 1);
    assert!(not_issued(&got[0], NotIssuedReason::Sealed), "{got:?}");
}

/// A clock that seals the gate on its next reading once armed: the seal lands
/// between `admit` and the lease.
struct SealingClock {
    inner: Arc<SteppingClock>,
    gate: Arc<GenerationCell>,
    armed: AtomicBool,
}

impl WindowClock for SealingClock {
    fn now(&self) -> Instant {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.gate.seal();
        }
        self.inner.now()
    }
}

#[test]
fn a_seal_between_admit_and_the_lease_makes_no_attribute_call() {
    let inner = SteppingClock::new(Duration::ZERO);
    let gate = Arc::new(GenerationCell::new());
    gate.publish(1, 1);
    let sealing = Arc::new(SealingClock {
        inner: Arc::clone(&inner),
        gate: Arc::clone(&gate),
        armed: AtomicBool::new(false),
    });
    let ax = FakeWindowAx::new();
    let (system, obs) = FakeWindowSystem::new(ax.clone());
    let adapter = AdapterId::allocate();
    let core = Core::new(system, adapter, sealing.clone(), false);
    let (tx, rx) = mpsc::sync_channel(64);
    let mut h = H {
        core,
        tx,
        rx,
        ax,
        obs,
        clock: inner,
        queue: EventQueue::new(1024),
        gate,
        adapter,
        epoch: RuntimeEpoch::new(0),
    };
    h.epoch = h.install().expect("install");
    let (node, _, tokens) = h.app_with_windows(app(1), 900, 1);
    h.ax.edit(node, |app| app.enhanced_ui = Ok(true));
    h.ax.clear_calls();
    let replies = h.put(frame_request(tokens[0], 1, 1.0, FrameStage::Intermediate));
    sealing.armed.store(true, Ordering::SeqCst);
    h.turn();
    let got = taken(&replies);
    assert_eq!(got.len(), 1);
    assert!(not_issued(&got[0], NotIssuedReason::Sealed), "{got:?}");
    assert!(h.gate.is_sealed());
    assert!(
        h.ax.calls()
            .iter()
            .all(|call| !matches!(call, Call::EnhancedUi(_) | Call::SetEnhancedUi(..))),
        "no attribute call after the seal"
    );
    assert_eq!(writes(&h.ax), 0);
}

// ---- threaded shards -------------------------------------------------------

pub(super) fn windows_of(events: &[WindowEvent], application: u64) -> Vec<WindowInstance> {
    snapshots(events)
        .into_iter()
        .map(|snapshot| snapshot.window)
        .filter(|window| window.application() == app(application))
        .collect()
}

pub(super) fn submit(
    adapter: &WindowAdapter,
    application: u64,
    frames: Vec<FrameRequest>,
) -> (Replies, Result<(), DispatchError>) {
    let (replies, make) = collector();
    let batch = frames.into_iter().map(|request| (request, make())).collect();
    let result = adapter.submit_frames(app(application), batch);
    (replies, result)
}

pub(super) fn wait_for(replies: &Replies, count: usize) -> Vec<FrameReply> {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end && taken(replies).len() < count {
        thread::sleep(Duration::from_millis(2));
    }
    taken(replies)
}

#[test]
fn a_held_application_never_delays_another_and_its_pending_frame_is_replaced() {
    let rig = rig(&[&[300, 301], &[310]], None);
    let gauge = hold_writes_on(&rig.ax, Some(format!("{}-0", rig.prefix)));
    let events = rig.tracked(2);
    let slow = windows_of(&events, 1);
    let quick = windows_of(&events, 2)[0];

    let (held, result) = submit(&rig.adapter, 1, vec![frame_request(
        slow[0],
        1,
        1.0,
        FrameStage::Intermediate,
    )]);
    result.expect("held");
    gauge.entered.recv_timeout(Duration::from_secs(5)).expect("entered");

    let (a, result) = submit(&rig.adapter, 1, vec![frame_request(
        slow[1],
        1,
        2.0,
        FrameStage::Intermediate,
    )]);
    result.expect("a");
    let (b, result) = submit(&rig.adapter, 1, vec![frame_request(
        slow[1],
        1,
        3.0,
        FrameStage::Intermediate,
    )]);
    result.expect("b");
    assert_eq!(taken(&a), [FrameReply::Replaced], "dropped, never written");

    let started = Instant::now();
    let (other, result) = submit(&rig.adapter, 2, vec![frame_request(
        quick,
        1,
        7.0,
        FrameStage::Intermediate,
    )]);
    result.expect("other");
    let other = wait_for(&other, 1);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert!(applied(&other[0]));
    assert!(taken(&held).is_empty());

    gauge.release.send(()).expect("release");
    assert!(applied(&wait_for(&held, 1)[0]));
    assert!(applied(&wait_for(&b, 1)[0]));
    assert_eq!(taken(&a).len(), 1);
    assert_eq!(taken(&b).len(), 1);
}

#[test]
fn sealing_during_a_held_write_leaves_queued_frames_inert() {
    let rig = rig(&[&[300, 301, 302]], None);
    let gauge = hold_writes_on(&rig.ax, Some(format!("{}-0", rig.prefix)));
    let events = rig.tracked(1);
    let windows = windows_of(&events, 1);
    let (held, result) = submit(&rig.adapter, 1, vec![frame_request(
        windows[0],
        1,
        1.0,
        FrameStage::First,
    )]);
    result.expect("held");
    gauge.entered.recv_timeout(Duration::from_secs(5)).expect("entered");
    let (queued, result) = submit(&rig.adapter, 1, vec![
        frame_request(windows[1], 1, 2.0, FrameStage::Intermediate),
        frame_request(windows[2], 1, 3.0, FrameStage::Final),
    ]);
    result.expect("queued");

    rig.gate.seal();
    gauge.release.send(()).expect("release");

    let truth = wait_for(&held, 1);
    assert!(
        matches!(truth[..], [FrameReply::Done(FrameAck {
            outcome: WindowActionOutcome::Issued { .. },
            ..
        })]),
        "the committed write answers truthfully: {truth:?}"
    );
    let inert = wait_for(&queued, 2);
    assert_eq!(inert.len(), 2);
    assert!(
        inert.iter().all(|reply| not_issued(reply, NotIssuedReason::Sealed)),
        "{inert:?}"
    );
    thread::sleep(Duration::from_millis(50));
    assert_eq!(taken(&queued).len(), 2);
    assert_eq!(taken(&held).len(), 1);
    assert_eq!(writes(&rig.ax), 1, "no queued frame reached the native send");
}

#[test]
fn an_unrouted_application_is_answered_inline_with_typed_outcomes() {
    let rig = rig(&[&[300]], None);
    let window = WindowInstance::for_testing(rig.adapter.id(), rig.epoch, app(1), 1);
    let (replies, result) = submit(&rig.adapter, 1, vec![
        frame_request(window, 1, 1.0, FrameStage::First),
        FrameRequest {
            position: None,
            size: None,
            ..frame_request(window, 2, 1.0, FrameStage::First)
        },
    ]);
    result.expect("inline");
    let got = taken(&replies);
    assert_eq!(got.len(), 2, "answered before submit_frames returned");
    assert!(matches!(
        got[0],
        FrameReply::Done(FrameAck {
            outcome: WindowActionOutcome::Stale(StaleReason::ApplicationGone),
            sent_at: None
        })
    ));
    assert!(not_issued(&got[1], NotIssuedReason::EmptyRequest));
    assert_eq!(rig.systems.load(Ordering::SeqCst), 0, "no shard was started");
}

#[test]
fn a_closed_adapter_refuses_frames_and_snapshots_without_calling_back() {
    let rig = rig(&[&[300]], None);
    let events = rig.tracked(1);
    let window = windows_of(&events, 1)[0];
    assert_eq!(rig.adapter.close(), CloseOutcome::Closed);
    let (replies, result) = submit(&rig.adapter, 1, vec![frame_request(
        window,
        1,
        1.0,
        FrameStage::First,
    )]);
    assert_eq!(result, Err(DispatchError::Closed));
    assert!(taken(&replies).is_empty());
    let called = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&called);
    let result = rig.adapter.submit_snapshot(window, move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(result, Err(DispatchError::Closed));
    assert_eq!(called.load(Ordering::SeqCst), 0);
}

#[test]
fn submit_snapshot_is_the_non_blocking_snapshot() {
    let rig = rig(&[&[300]], None);
    let unrouted = WindowInstance::for_testing(rig.adapter.id(), rig.epoch, app(1), 1);
    let (tx, rx) = mpsc::channel();
    let inline = tx.clone();
    rig.adapter
        .submit_snapshot(unrouted, move |outcome| inline.send(outcome).expect("send"))
        .expect("inline");
    assert_eq!(
        rx.try_recv().expect("inline reply"),
        SnapshotOutcome::Stale(StaleReason::ApplicationGone)
    );

    let events = rig.tracked(1);
    let window = windows_of(&events, 1)[0];
    rig.adapter
        .submit_snapshot(window, move |outcome| tx.send(outcome).expect("send"))
        .expect("routed");
    let outcome = rx.recv_timeout(Duration::from_secs(5)).expect("shard reply");
    assert_eq!(
        outcome,
        rig.adapter.snapshot(window, &CancellationToken::new(), None),
        "the same answer the blocking form gives"
    );
    assert!(matches!(outcome, SnapshotOutcome::Snapshot(_)));
}
