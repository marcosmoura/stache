//! The `AXEnhancedUserInterface` lease over the fake AX arena (TA-04-01..04):
//! it opens before the first frame write of a burst, stays open for the idle
//! grace, restores only what the shard changed, and never fights a third party.
//! Nothing here touches real AX.

use super::frames::{Replies, frame_request, submit, taken, wait_for, windows_of};
use super::shards::{hold_writes_on, rig, rig_with};
use super::*;
use crate::ax::WindowAx;
use crate::windows::testing::FakeVoiceOver;

const GRACE: Duration = LEASE_IDLE_GRACE;

fn eui_notes(events: &[WindowEvent]) -> Vec<EnhancedUiNote> {
    events
        .iter()
        .filter_map(|event| match event.kind {
            WindowEventKind::EnhancedUi { note, .. } => Some(note),
            _ => None,
        })
        .collect()
}

fn eui_calls(ax: &FakeWindowAx) -> usize {
    count_calls(ax, |call| {
        matches!(call, Call::EnhancedUi(_) | Call::SetEnhancedUi(..))
    })
}

fn position_of(ax: &FakeWindowAx, wanted: &Call) -> usize {
    ax.calls().iter().position(|call| call == wanted).expect("call recorded")
}

fn attribute(ax: &FakeWindowAx, node: NodeId) -> Result<bool, AxError> {
    ax.enhanced_user_interface(&ax.element(node))
}

fn did_apply(replies: &Replies) -> bool {
    matches!(taken(replies)[..], [FrameReply::Done(FrameAck {
        outcome: WindowActionOutcome::Issued { kind: IssuedKind::Applied, .. },
        ..
    })])
}

struct Eui {
    h: H,
    node: NodeId,
    tokens: Vec<WindowInstance>,
    plan: u64,
}

impl Eui {
    /// One application with `windows` windows and the attribute `on`.
    fn new(windows: u32, on: Result<bool, AxError>) -> Self {
        let mut h = H::new();
        let (node, _, tokens) = h.app_with_windows(app(1), 900, windows);
        h.ax.edit(node, |app| app.enhanced_ui = on);
        h.ax.clear_calls();
        let _ = h.events();
        Self { h, node, tokens, plan: 0 }
    }

    /// Queues one frame of window `index` and runs one turn.
    fn write(&mut self, index: usize, stage: FrameStage) -> (Replies, Turn) {
        self.plan += 1;
        let request = frame_request(self.tokens[index], self.plan, 1.0, stage);
        let replies = self.h.put(request);
        let turn = self.h.turn();
        (replies, turn)
    }

    fn off_writes(&self) -> Vec<(NodeId, bool)> { self.h.ax.enhanced_ui_writes() }

    fn notes(&self) -> Vec<EnhancedUiNote> { eui_notes(&self.h.events()) }
}

#[test]
fn a_direct_write_runs_inside_a_lease_that_closes_after_the_grace() {
    let mut t = Eui::new(1, Ok(true));
    let (replies, turn) = t.write(0, FrameStage::Direct);
    assert!(did_apply(&replies));
    assert_eq!(t.off_writes(), [(t.node, false)]);
    let off = position_of(&t.h.ax, &Call::SetEnhancedUi(t.node, false));
    let first_write =
        t.h.ax
            .calls()
            .iter()
            .position(|call| matches!(call, Call::SetPosition(..)))
            .expect("written");
    assert!(off < first_write, "the attribute is off before the first write");
    assert_eq!(
        turn.deadline,
        Some(t.h.clock.peek() + GRACE),
        "the shard parks until the grace"
    );
    assert_eq!(attribute(&t.h.ax, t.node), Ok(false));

    t.h.clock.advance(Duration::from_millis(49));
    assert!(t.h.turn().deadline.is_some());
    assert_eq!(t.off_writes().len(), 1, "still inside the grace");
    t.h.clock.advance(Duration::from_millis(1));
    let turn = t.h.turn();
    assert_eq!(turn.deadline, None, "nothing is owed once restored");
    assert_eq!(t.off_writes(), [(t.node, false), (t.node, true)]);
    assert_eq!(attribute(&t.h.ax, t.node), Ok(true));
    assert_eq!(t.notes(), [EnhancedUiNote::Leased, EnhancedUiNote::Restored]);
}

#[test]
fn an_animated_sequence_holds_one_lease_from_first_to_final() {
    let mut t = Eui::new(1, Ok(true));
    t.write(0, FrameStage::First);
    for _ in 0..5 {
        t.h.clock.advance(Duration::from_millis(16));
        t.write(0, FrameStage::Intermediate);
    }
    t.h.clock.advance(Duration::from_millis(16));
    t.write(0, FrameStage::Final);
    assert_eq!(
        t.off_writes(),
        [(t.node, false)],
        "one toggle for the whole sequence"
    );
    assert_eq!(attribute(&t.h.ax, t.node), Ok(false));
    t.h.clock.advance(GRACE);
    t.h.turn();
    assert_eq!(t.off_writes(), [(t.node, false), (t.node, true)]);
}

#[test]
fn chained_batches_inside_the_grace_toggle_once() {
    let mut t = Eui::new(2, Ok(true));
    t.write(0, FrameStage::Final);
    t.h.clock.advance(Duration::from_millis(20));
    t.write(1, FrameStage::Direct);
    t.h.clock.advance(Duration::from_millis(20));
    t.write(0, FrameStage::Direct);
    t.h.clock.advance(Duration::from_millis(40));
    t.h.turn();
    assert_eq!(t.off_writes(), [(t.node, false)], "A-B-A reuses the open lease");
    t.h.clock.advance(Duration::from_millis(10));
    t.h.turn();
    assert_eq!(t.off_writes(), [(t.node, false), (t.node, true)]);
    assert_eq!(t.notes(), [EnhancedUiNote::Leased, EnhancedUiNote::Restored]);
}

#[test]
fn an_application_that_is_already_off_is_never_written() {
    let mut t = Eui::new(1, Ok(false));
    for stage in [
        FrameStage::First,
        FrameStage::Intermediate,
        FrameStage::Final,
    ] {
        let (replies, turn) = t.write(0, stage);
        assert!(did_apply(&replies), "{stage:?}");
        assert_eq!(turn.deadline, None, "no lease, nothing to wait for");
    }
    t.h.clock.advance(GRACE * 10);
    t.h.turn();
    assert!(t.off_writes().is_empty());
    assert_eq!(
        count_calls(&t.h.ax, |call| matches!(call, Call::EnhancedUi(_))),
        1,
        "one read for the whole burst"
    );
    assert!(t.notes().is_empty());
}

#[test]
fn an_application_without_the_attribute_is_left_alone_and_silent() {
    let mut t = Eui::new(1, Err(AxError::AttributeUnsupported));
    let (replies, _) = t.write(0, FrameStage::Direct);
    assert!(did_apply(&replies));
    assert!(t.off_writes().is_empty());
    assert!(t.notes().is_empty());
}

#[test]
fn raise_and_hidden_reads_take_no_lease() {
    let mut t = Eui::new(1, Ok(true));
    let token = t.tokens[0];
    t.h.act(token, WindowAction::Raise);
    assert_eq!(eui_calls(&t.h.ax), 0);
}

#[test]
fn a_third_party_re_enable_is_detected_counted_and_not_fought() {
    let mut t = Eui::new(1, Ok(true));
    let ax = t.h.ax.clone();
    t.h.ax.on_enhanced_ui_set(move |node, on| {
        if !on {
            ax.edit(node, |app| app.enhanced_ui = Ok(true));
        }
    });
    t.write(0, FrameStage::Final);
    assert_eq!(t.off_writes(), [(t.node, false)]);
    assert_eq!(attribute(&t.h.ax, t.node), Ok(true), "the third party won");
    assert_eq!(t.notes(), [EnhancedUiNote::Leased, EnhancedUiNote::ReEnabled {
        consecutive: 1
    }]);

    // The rest of the burst does not set it off again.
    t.write(0, FrameStage::Direct);
    assert_eq!(t.off_writes().len(), 1, "no fighting inside the burst");
    assert!(t.notes().is_empty());

    // Nothing is restored: the attribute is already on.
    t.h.clock.advance(GRACE * 10);
    let turn = t.h.turn();
    assert_eq!(turn.deadline, None);
    assert_eq!(t.off_writes().len(), 1);

    // The next batch takes a new lease once and meets the same fate.
    t.write(0, FrameStage::Final);
    assert_eq!(t.off_writes(), [(t.node, false), (t.node, false)]);
    assert_eq!(t.notes(), [EnhancedUiNote::Leased, EnhancedUiNote::ReEnabled {
        consecutive: 2
    }]);
    t.h.clock.advance(GRACE * 10);
    t.h.turn();
    assert!(
        t.off_writes().iter().all(|(_, on)| !on),
        "never restored over a re-enable"
    );
}

#[test]
fn a_re_enable_during_the_grace_counts_and_is_never_reported_as_restored() {
    let mut t = Eui::new(1, Ok(true));
    for expected in 1..=2 {
        t.write(0, FrameStage::Final);
        t.h.ax.edit(t.node, |app| app.enhanced_ui = Ok(true));
        t.h.clock.advance(GRACE);
        let turn = t.h.turn();
        assert_eq!(turn.deadline, None);
        assert_eq!(
            t.notes().last(),
            Some(&EnhancedUiNote::ReEnabled { consecutive: expected })
        );
        t.h.clock.advance(GRACE * 10);
    }
    assert_eq!(
        t.off_writes(),
        [(t.node, false), (t.node, false)],
        "no restore write over a re-enable"
    );
}

#[test]
fn a_clean_lease_resets_the_consecutive_re_enable_count() {
    let mut t = Eui::new(1, Ok(true));
    let ax = t.h.ax.clone();
    let flip = Arc::new(AtomicBool::new(true));
    let armed = Arc::clone(&flip);
    t.h.ax.on_enhanced_ui_set(move |node, on| {
        if !on && armed.load(Ordering::SeqCst) {
            ax.edit(node, |app| app.enhanced_ui = Ok(true));
        }
    });
    t.write(0, FrameStage::Final);
    assert_eq!(
        t.notes().last(),
        Some(&EnhancedUiNote::ReEnabled { consecutive: 1 })
    );
    flip.store(false, Ordering::SeqCst);
    t.h.clock.advance(GRACE * 10);
    t.write(0, FrameStage::Final);
    t.h.clock.advance(GRACE);
    t.h.turn();
    assert_eq!(t.notes(), [EnhancedUiNote::Leased, EnhancedUiNote::Restored]);
    flip.store(true, Ordering::SeqCst);
    t.h.clock.advance(GRACE * 10);
    t.write(0, FrameStage::Final);
    assert_eq!(
        t.notes().last(),
        Some(&EnhancedUiNote::ReEnabled { consecutive: 1 })
    );
}

#[test]
fn a_read_failure_writes_without_a_lease_and_says_so_once() {
    let mut t = Eui::new(1, Err(AxError::CannotComplete));
    let (replies, _) = t.write(0, FrameStage::Direct);
    assert!(did_apply(&replies));
    let (replies, _) = t.write(0, FrameStage::Direct);
    assert!(did_apply(&replies));
    assert!(t.off_writes().is_empty());
    assert_eq!(t.notes(), [EnhancedUiNote::ReadFailed(AxError::CannotComplete)]);
    assert_eq!(
        count_calls(&t.h.ax, |call| matches!(call, Call::EnhancedUi(_))),
        1,
        "a failing application is not read at frame rate"
    );
}

#[test]
fn a_set_failure_writes_without_a_lease_and_restores_nothing() {
    let mut t = Eui::new(1, Ok(true));
    t.h.ax.edit(t.node, |app| {
        app.set_enhanced_ui_result = Err(AxError::CannotComplete);
    });
    let (replies, _) = t.write(0, FrameStage::Direct);
    assert!(did_apply(&replies));
    assert_eq!(t.notes(), [EnhancedUiNote::SetFailed(AxError::CannotComplete)]);
    t.h.clock.advance(GRACE * 10);
    let turn = t.h.turn();
    assert_eq!(turn.deadline, None);
    assert_eq!(
        t.off_writes(),
        [(t.node, false)],
        "the failed attempt only; no restore owed"
    );
}

#[test]
fn a_failed_restore_is_quarantined_and_retried_exactly_once() {
    let mut t = Eui::new(1, Ok(true));
    t.write(0, FrameStage::Final);
    t.h.ax.edit(t.node, |app| {
        app.set_enhanced_ui_result = Err(AxError::CannotComplete);
    });
    t.h.clock.advance(GRACE);
    let turn = t.h.turn();
    assert!(turn.deadline.is_some(), "the failed restore is retried later");
    assert_eq!(t.notes(), [
        EnhancedUiNote::Leased,
        EnhancedUiNote::RestoreFailed(AxError::CannotComplete)
    ]);

    // The stop retries once, fails again and keeps the application.
    let StopOutcome::Quarantined { applications, .. } = t.h.stop() else {
        panic!("expected a quarantine");
    };
    assert_eq!(applications, 1);
    let restore_attempts = |t: &Eui| t.off_writes().iter().filter(|(_, on)| *on).count();
    assert_eq!(restore_attempts(&t), 2, "one in the turn, one in the stop");
    assert_eq!(attribute(&t.h.ax, t.node), Ok(false));

    // The retry restores exactly once, and a second retry has nothing to do.
    t.h.ax.edit(t.node, |app| app.set_enhanced_ui_result = Ok(()));
    let report = t.h.run(|reply| Command::Retry { reply });
    assert_eq!(report.cleaned_applications, 1);
    assert_eq!(report.remaining_applications, 0);
    assert_eq!(restore_attempts(&t), 3);
    assert_eq!(attribute(&t.h.ax, t.node), Ok(true));
    let report = t.h.run(|reply| Command::Retry { reply });
    assert_eq!(report.cleaned_applications, 0);
    assert_eq!(restore_attempts(&t), 3, "never restored twice");
}

#[test]
fn stopping_restores_an_open_lease_without_waiting_for_the_grace() {
    let mut t = Eui::new(1, Ok(true));
    t.write(0, FrameStage::Final);
    assert_eq!(t.h.stop(), StopOutcome::Clean { applications: 1 });
    assert_eq!(t.off_writes(), [(t.node, false), (t.node, true)]);
    assert_eq!(t.notes(), [EnhancedUiNote::Leased, EnhancedUiNote::Restored]);
}

#[test]
fn an_application_terminated_mid_lease_drops_its_lease_with_no_write() {
    let mut t = Eui::new(1, Ok(true));
    t.write(0, FrameStage::Final);
    t.h.ax.edit(t.node, |app| {
        app.enhanced_ui = Err(AxError::InvalidElement);
        app.set_enhanced_ui_result = Err(AxError::InvalidElement);
    });
    let epoch = t.h.epoch;
    let outcome = t.h.run(|reply| Command::Untrack {
        epoch,
        application: app(1),
        reply,
    });
    assert_eq!(outcome, StopOutcome::Clean { applications: 1 });
    assert_eq!(
        t.off_writes(),
        [(t.node, false)],
        "no restore write to a dead application"
    );
    assert_eq!(t.notes(), [EnhancedUiNote::Leased]);
}

#[test]
fn a_shard_panic_restores_its_open_leases() {
    let mut t = Eui::new(1, Ok(true));
    t.h.ax.on_write(|| panic!("injected"));
    let (replies, _) = t.write(0, FrameStage::Direct);
    assert!(t.h.core.is_poisoned());
    assert!(!did_apply(&replies));
    assert_eq!(t.off_writes(), [(t.node, false), (t.node, true)]);
    assert_eq!(attribute(&t.h.ax, t.node), Ok(true));
}

// ---- threaded shards ---------------------------------------------------------

#[test]
fn a_real_shard_restores_after_the_grace_without_being_woken() {
    let rig = rig(&[&[300]], None);
    let node = rig.app_nodes[0];
    rig.ax.edit(node, |app| app.enhanced_ui = Ok(true));
    let events = rig.tracked(1);
    let window = windows_of(&events, 1)[0];
    let (replies, result) = submit(&rig.adapter, 1, vec![frame_request(
        window,
        1,
        1.0,
        FrameStage::Direct,
    )]);
    result.expect("submitted");
    assert_eq!(wait_for(&replies, 1).len(), 1);
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end && attribute(&rig.ax, node) != Ok(true) {
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        attribute(&rig.ax, node),
        Ok(true),
        "the parked shard woke at the deadline"
    );
    assert_eq!(rig.ax.enhanced_ui_writes(), [(node, false), (node, true)]);
    assert_eq!(eui_notes(&rig.queue.drain().events), [
        EnhancedUiNote::Leased,
        EnhancedUiNote::Restored
    ]);
}

#[test]
fn a_restore_after_stop_is_owed_while_a_retired_shard_holds_a_lease() {
    let rig = rig(&[&[300]], None);
    let node = rig.app_nodes[0];
    rig.ax.edit(node, |app| app.enhanced_ui = Ok(true));
    let events = rig.tracked(1);
    let window = windows_of(&events, 1)[0];
    let (replies, result) = submit(&rig.adapter, 1, vec![frame_request(
        window,
        1,
        1.0,
        FrameStage::Final,
    )]);
    result.expect("submitted");
    assert_eq!(wait_for(&replies, 1).len(), 1);
    rig.ax.edit(node, |app| {
        app.set_enhanced_ui_result = Err(AxError::CannotComplete);
    });
    assert!(matches!(rig.stop(None), StopOutcome::Quarantined { .. }));

    let owed = rig.adapter.restore_enhanced_ui(None);
    assert!(
        matches!(&owed, LeaseRestoreOutcome::Owed { applications } if applications == &[app(1)]),
        "{owed:?}"
    );

    rig.ax.edit(node, |app| app.set_enhanced_ui_result = Ok(()));
    let report = rig.adapter.retry_cleanup(&CancellationToken::new(), None).expect("report");
    assert_eq!(report.remaining_applications, 0);
    assert_eq!(attribute(&rig.ax, node), Ok(true));
    assert_eq!(
        rig.adapter.restore_enhanced_ui(None),
        LeaseRestoreOutcome::Clean { restored: 0 }
    );
    assert_eq!(rig.adapter.close(), CloseOutcome::Closed);
}

#[test]
fn an_explicit_restore_after_a_held_write_runs_only_after_the_drain() {
    let rig = rig(&[&[300]], None);
    let node = rig.app_nodes[0];
    rig.ax.edit(node, |app| app.enhanced_ui = Ok(true));
    let gauge = hold_writes_on(&rig.ax, Some(format!("{}-0", rig.prefix)));
    let events = rig.tracked(1);
    let window = windows_of(&events, 1)[0];
    let (replies, result) = submit(&rig.adapter, 1, vec![frame_request(
        window,
        1,
        1.0,
        FrameStage::Final,
    )]);
    result.expect("submitted");
    gauge.entered.recv_timeout(Duration::from_secs(5)).expect("the write is held");
    rig.gate.seal();

    let adapter = Arc::clone(&rig.adapter);
    let restore = thread::spawn(move || adapter.restore_enhanced_ui(None));
    thread::sleep(Duration::from_millis(20));
    assert_eq!(
        rig.ax.enhanced_ui_writes(),
        [(node, false)],
        "nothing restored under a held write"
    );

    gauge.release.send(()).expect("release");
    let outcome = restore.join().expect("restore");
    assert!(
        matches!(outcome, LeaseRestoreOutcome::Clean { restored } if restored <= 1),
        "{outcome:?}"
    );
    assert_eq!(wait_for(&replies, 1).len(), 1);
    assert_eq!(rig.ax.enhanced_ui_writes(), [(node, false), (node, true)]);
    let calls = rig.ax.calls();
    let write = calls
        .iter()
        .position(|call| matches!(call, Call::SetPosition(..)))
        .expect("write");
    assert!(
        write < position_of(&rig.ax, &Call::SetEnhancedUi(node, true)),
        "the restore follows the held write"
    );
    assert_eq!(attribute(&rig.ax, node), Ok(true));
    assert!(matches!(rig.stop(None), StopOutcome::Clean { .. }));
}

#[test]
fn an_explicit_restore_reports_the_applications_that_still_owe() {
    let rig = rig(&[&[300]], None);
    let node = rig.app_nodes[0];
    rig.ax.edit(node, |app| app.enhanced_ui = Ok(true));
    let events = rig.tracked(1);
    let window = windows_of(&events, 1)[0];
    let (replies, result) = submit(&rig.adapter, 1, vec![frame_request(
        window,
        1,
        1.0,
        FrameStage::Final,
    )]);
    result.expect("submitted");
    wait_for(&replies, 1);
    rig.ax.edit(node, |app| {
        app.set_enhanced_ui_result = Err(AxError::CannotComplete);
    });
    assert_eq!(
        rig.adapter.restore_enhanced_ui(None),
        LeaseRestoreOutcome::Owed { applications: vec![app(1)] }
    );
    rig.ax.edit(node, |app| app.set_enhanced_ui_result = Ok(()));
    assert_eq!(
        rig.adapter.restore_enhanced_ui(None),
        LeaseRestoreOutcome::Clean { restored: 1 }
    );
    assert_eq!(attribute(&rig.ax, node), Ok(true));
}

#[test]
fn while_voiceover_runs_no_attribute_is_touched() {
    let probe = Arc::new(FakeVoiceOver::default());
    probe.set(true);
    let options = AdapterOptions {
        voice_over: Some(probe.clone()),
        ..AdapterOptions::default()
    };
    let rig = rig_with(&[&[300]], None, &options);
    let node = rig.app_nodes[0];
    rig.ax.edit(node, |app| app.enhanced_ui = Ok(true));
    let events = rig.tracked(1);
    let window = windows_of(&events, 1)[0];
    for plan in 1..=2 {
        let (replies, result) = submit(&rig.adapter, 1, vec![frame_request(
            window,
            plan,
            1.0,
            FrameStage::Final,
        )]);
        result.expect("submitted");
        assert_eq!(wait_for(&replies, 1).len(), 1);
        assert!(did_apply(&replies));
    }
    assert_eq!(eui_calls(&rig.ax), 0, "no read and no write under VoiceOver");
    assert_eq!(
        eui_notes(&rig.queue.drain().events),
        [EnhancedUiNote::VoiceOverSkipped],
        "reported once per application"
    );
    assert_eq!(
        rig.adapter.restore_enhanced_ui(None),
        LeaseRestoreOutcome::Clean { restored: 0 }
    );

    // Once VoiceOver is off the next batch takes a lease again.
    probe.set(false);
    thread::sleep(GRACE * 3);
    let (replies, result) = submit(&rig.adapter, 1, vec![frame_request(
        window,
        3,
        3.0,
        FrameStage::Final,
    )]);
    result.expect("submitted");
    wait_for(&replies, 1);
    assert_eq!(rig.ax.enhanced_ui_writes().first(), Some(&(node, false)));
}
