//! Pure frame-effect tests: desired versus confirmed frames, plan sequences,
//! bounded attempts and host reservation normalization. No threads or backend.

use std::sync::Arc;

use stache_macos::applications::DisplayRect;
use stache_macos::ax::{AxError, AxPoint, AxReading, AxSize, FrameRead, WriteStatus};
use stache_macos::windows::{
    IssuedKind, NativeStatus, NotIssuedReason, StaleReason, TrackOutcome, WindowActionOutcome,
    WindowChange, WindowInstance,
};
use stache_macos::{DisplayId, DisplayInfo, DisplayLayout};
use stache_sdk::{DisplayId as SdkDisplayId, EdgeInsets, TopologyGeneration};

use super::*;
use crate::config::TilingConfig;
use crate::geometry::Rect;
use crate::outcome::{FrameComponents, NotAttemptedReason, OperationOutcome};
use crate::reservations::{DisplayProjection, HostReservations};
use crate::test_support::{Feed, app, info, run, snapshot, step};

fn screen(id: u32, primary: bool, x: f64, y: f64, w: f64, h: f64, native_top: f64) -> DisplayInfo {
    DisplayInfo {
        id: DisplayId::new(id),
        frame: DisplayRect::new(x, y, w, h),
        // AppKit space (y up): the menu bar removes `native_top` from the top.
        visible_frame: DisplayRect::new(x, y, w, h - native_top),
        scale: 2.0,
        refresh_hz: Some(60.0),
        name: Some(format!("Display {id}")),
        is_primary: primary,
        is_main: primary,
        is_builtin: primary,
    }
}

fn config(value: serde_json::Value) -> Arc<TilingConfig> {
    Arc::new(serde_json::from_value(value).expect("config"))
}

pub(super) fn gap_config(outer: u32) -> Arc<TilingConfig> {
    config(serde_json::json!({ "gaps": { "inner": 0, "outer": outer } }))
}

pub(super) struct Fixture {
    pub(super) state: TilingState,
    pub(super) feed: Feed,
    pub(super) windows: Vec<WindowInstance>,
}

impl Fixture {
    /// A started, enumerated state with `layout`, `config` and `count` windows
    /// whose frames are `(10, 10, 800, 600)`. Frame writes are still off.
    pub(super) fn new(layout: DisplayLayout, config: Arc<TilingConfig>, count: u32) -> Self {
        let mut feed = Feed::new(1);
        let state = TilingState::new("");
        let (mut state, _) = run(state, vec![
            Input::Displays(Arc::new(layout)),
            Input::Start(feed.stamp),
            Input::Applications {
                stamp: feed.stamp,
                apps: vec![info(1, "com.editor", "Editor")],
            },
            Input::Completion {
                stamp: feed.stamp,
                result: CompletionResult::Track(app(1), TrackOutcome::Tracking),
            },
        ]);
        let (next, _) = step(state, feed.complete(app(1)));
        state = next;
        let (next, _) = step(state, Input::Config { revision: 1, config });
        state = next;
        let mut windows = Vec::new();
        for serial in 1..=count {
            let token = feed.token(1, u64::from(serial));
            let input = feed.created(token, "w", serial);
            let (next, _) = step(state, input);
            state = next;
            windows.push(token);
        }
        assert_eq!(state.phase(), Phase::Complete);
        Self { state, feed, windows }
    }

    fn single() -> Self { Self::new(one_screen(), gap_config(0), 1) }

    pub(super) fn apply(&mut self, input: Input) -> Vec<Effect> {
        let state = std::mem::replace(&mut self.state, TilingState::new(""));
        let (next, effects) = step(state, input);
        self.state = next;
        effects
    }

    pub(super) fn enable(&mut self) -> Vec<Effect> { self.apply(Input::FrameWrites(true)) }

    pub(super) fn stamp(&self) -> Stamp { self.feed.stamp }

    pub(super) fn revision(&self) -> u64 { self.state.config_revision }

    pub(super) fn ack(
        &mut self,
        window: WindowInstance,
        plan: u64,
        outcome: WindowActionOutcome,
    ) -> Vec<Effect> {
        let (stamp, revision) = (self.stamp(), self.revision());
        self.apply(Input::Completion {
            stamp,
            result: CompletionResult::Frame {
                window,
                plan_sequence: plan,
                revision,
                outcome,
            },
        })
    }

    /// Feeds an observed frame for `window` as a snapshot completion.
    pub(super) fn observe(&mut self, window: WindowInstance, frame: Rect) -> Vec<Effect> {
        let mut snap = snapshot(window, "w", 1);
        snap.frame = FrameRead {
            position: AxReading::Value(AxPoint { x: frame.x, y: frame.y }),
            size: AxReading::Value(AxSize {
                width: frame.width,
                height: frame.height,
            }),
        };
        let stamp = self.stamp();
        self.apply(Input::Completion {
            stamp,
            result: CompletionResult::Snapshot(
                window,
                stache_macos::windows::SnapshotOutcome::Snapshot(Box::new(snap)),
            ),
        })
    }
}

pub(super) fn one_screen() -> DisplayLayout {
    DisplayLayout::new(vec![screen(1, true, 0.0, 0.0, 1920.0, 1080.0, 25.0)])
}

#[derive(Clone, Copy, Debug)]
struct Write {
    window: WindowInstance,
    plan: u64,
    target: Rect,
    components: FrameComponents,
}

fn writes(effects: &[Effect]) -> Vec<Write> {
    effects
        .iter()
        .filter_map(|effect| match *effect {
            Effect::WriteFrame {
                window,
                plan_sequence,
                target,
                components,
                ..
            } => Some(Write {
                window,
                plan: plan_sequence,
                target,
                components,
            }),
            _ => None,
        })
        .collect()
}

fn cancels(effects: &[Effect]) -> Vec<WindowInstance> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::CancelFrame { window } => Some(*window),
            _ => None,
        })
        .collect()
}

const fn frame_read(x: f64, y: f64, w: f64, h: f64) -> FrameRead {
    FrameRead {
        position: AxReading::Value(AxPoint { x, y }),
        size: AxReading::Value(AxSize { width: w, height: h }),
    }
}

fn issued(
    kind: IssuedKind,
    position: WriteStatus,
    size: WriteStatus,
    observed: Option<FrameRead>,
) -> WindowActionOutcome {
    WindowActionOutcome::Issued {
        kind,
        native: NativeStatus::Frame { position, size },
        observed,
    }
}

fn applied_at(frame: Rect) -> WindowActionOutcome {
    issued(
        IssuedKind::Applied,
        WriteStatus::Written,
        WriteStatus::Written,
        Some(frame_read(frame.x, frame.y, frame.width, frame.height)),
    )
}

fn outcome_of(state: &TilingState, window: WindowInstance) -> Option<OperationOutcome> {
    state
        .frames
        .records
        .get(&window)
        .and_then(|record| record.attempt.as_ref())
        .and_then(|attempt| match &attempt.status {
            AttemptStatus::Settled(outcome) => Some(outcome.clone()),
            AttemptStatus::InFlight => None,
        })
}

// ------------------------------------------------------------------ basics

#[test]
fn nothing_is_written_until_the_runtime_publishes_its_authority() {
    let mut fixture = Fixture::single();
    // A refresh of the state alone plans nothing.
    let window = fixture.windows[0];
    let effects = fixture.observe(window, Rect::new(10.0, 10.0, 800.0, 600.0));
    assert!(writes(&effects).is_empty());
    let effects = fixture.enable();
    let writes = writes(&effects);
    assert_eq!(writes.len(), 1);
    // One window fills the usable area: the menu bar (25) is the native inset.
    assert_eq!(writes[0].target, Rect::new(0.0, 25.0, 1920.0, 1055.0));
    assert_eq!(writes[0].components, FrameComponents {
        position: true,
        size: true
    });
}

#[test]
fn revoking_the_authority_cancels_queued_writes_and_forgets_attempts() {
    let mut fixture = Fixture::single();
    let window = fixture.windows[0];
    assert_eq!(writes(&fixture.enable()).len(), 1);
    let effects = fixture.apply(Input::FrameWrites(false));
    assert_eq!(cancels(&effects), vec![window]);
    assert!(fixture.state.frames.records.is_empty());
    // A later acknowledgement of the revoked plan is stale.
    let plan = fixture.state.frames.plan_sequence;
    fixture.ack(window, plan, applied_at(Rect::new(0.0, 25.0, 1920.0, 1055.0)));
    assert_eq!(fixture.state.diagnostics.stale_frame_acks, 1);
    // Revoking again does nothing.
    assert!(cancels(&fixture.apply(Input::FrameWrites(false))).is_empty());
}

#[test]
fn only_changed_components_of_eligible_frames_are_sent() {
    let mut fixture = Fixture::single();
    let window = fixture.windows[0];
    // Position already right, size wrong.
    fixture.observe(window, Rect::new(0.0, 25.0, 800.0, 600.0));
    let writes = writes(&fixture.enable());
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].components, FrameComponents {
        position: false,
        size: true
    });

    // A window already at its target is not sent at all.
    let mut fixture = Fixture::single();
    let window = fixture.windows[0];
    fixture.observe(window, Rect::new(0.0, 25.0, 1920.0, 1055.0));
    assert!(writes_of(&fixture.enable()).is_empty());
}

#[test]
fn applied_only_follows_a_matching_readback_and_records_the_confirmed_frame() {
    let mut fixture = Fixture::single();
    let window = fixture.windows[0];
    let write = writes(&fixture.enable())[0];

    // Native acceptance without a readback is Uncertain, never Applied.
    let effects = fixture.ack(
        window,
        write.plan,
        issued(
            IssuedKind::Applied,
            WriteStatus::Written,
            WriteStatus::Written,
            None,
        ),
    );
    assert!(writes_of(&effects).is_empty());
    assert!(matches!(
        outcome_of(&fixture.state, window),
        Some(OperationOutcome::Uncertain { .. })
    ));
    assert_eq!(fixture.state.diagnostics.frames_applied, 0);
    assert_eq!(fixture.state.diagnostics.frames_uncertain, 1);

    // A fresh state: the matching readback is Applied and becomes the actual.
    let mut fixture = Fixture::single();
    let window = fixture.windows[0];
    let write = writes(&fixture.enable())[0];
    let effects = fixture.ack(window, write.plan, applied_at(write.target));
    assert!(writes_of(&effects).is_empty());
    assert!(matches!(
        outcome_of(&fixture.state, window),
        Some(OperationOutcome::Applied { .. })
    ));
    assert_eq!(fixture.state.diagnostics.frames_applied, 1);
    let snapshot = fixture.state.snapshot();
    let status = snapshot.frames.iter().find(|status| status.window == window).expect("status");
    assert_eq!(status.actual, Some(write.target));
    assert_eq!(status.desired, Some(write.target));
    assert!(!status.in_flight);
}

#[test]
fn partial_position_applied_size_refused_records_the_actual_and_never_retries() {
    let mut fixture = Fixture::single();
    let window = fixture.windows[0];
    let write = writes(&fixture.enable())[0];
    let stuck = Rect::new(0.0, 25.0, 640.0, 480.0);
    let effects = fixture.ack(
        window,
        write.plan,
        issued(
            IssuedKind::Partial,
            WriteStatus::Written,
            WriteStatus::Failed(AxError::ActionUnsupported),
            Some(frame_read(stuck.x, stuck.y, stuck.width, stuck.height)),
        ),
    );
    assert!(writes_of(&effects).is_empty(), "no immediate retry");
    let outcome = outcome_of(&fixture.state, window).expect("settled");
    assert!(
        matches!(outcome, OperationOutcome::Partial { .. }),
        "{outcome:?}"
    );
    assert_eq!(outcome.actual(), Some(stuck));
    assert_eq!(fixture.state.diagnostics.frames_partial, 1);

    // Own AX echoes, periodic refreshes and repeated snapshots of the same
    // frame create nothing: the app is not fought.
    for _ in 0..5 {
        assert!(writes_of(&fixture.observe(window, stuck)).is_empty());
    }
    assert_eq!(fixture.state.diagnostics.frame_attempts, 1);
}

#[test]
fn uncertain_results_are_never_retried_by_external_or_refresh_paths() {
    let mut fixture = Fixture::single();
    let window = fixture.windows[0];
    let write = writes(&fixture.enable())[0];
    fixture.ack(
        window,
        write.plan,
        issued(
            IssuedKind::Uncertain,
            WriteStatus::Failed(AxError::CannotComplete),
            WriteStatus::NotRequested,
            None,
        ),
    );
    // The window shows some other frame later: still no retry after unknown.
    assert!(writes_of(&fixture.observe(window, Rect::new(300.0, 300.0, 500.0, 500.0))).is_empty());
    assert!(writes_of(&fixture.observe(window, Rect::new(310.0, 310.0, 500.0, 500.0))).is_empty());
    assert_eq!(fixture.state.diagnostics.frame_attempts, 1);
    // Only an explicit user command attempts again.
    let again = writes(&fixture.apply(Input::Relayout));
    assert_eq!(again.len(), 1);
    assert!(again[0].plan > write.plan);
}

#[test]
fn proven_no_send_is_a_typed_reason_and_not_retried() {
    for (native, expected) in [
        (
            WindowActionOutcome::NotIssued(NotIssuedReason::Sealed),
            NotAttemptedReason::Sealed,
        ),
        (
            WindowActionOutcome::NotIssued(NotIssuedReason::GenerationInvalid),
            NotAttemptedReason::GenerationInvalid,
        ),
        (
            WindowActionOutcome::ExpiredBeforeSend,
            NotAttemptedReason::Expired,
        ),
    ] {
        let mut fixture = Fixture::single();
        let window = fixture.windows[0];
        let write = writes(&fixture.enable())[0];
        let effects = fixture.ack(window, write.plan, native);
        assert!(writes_of(&effects).is_empty(), "{native:?} must not retry");
        let outcome = outcome_of(&fixture.state, window).expect("settled");
        assert!(outcome.proves_no_send() && !outcome.was_issued());
        assert!(
            matches!(outcome, OperationOutcome::NotAttempted { reason, .. } if reason == expected)
        );
        assert_eq!(fixture.state.diagnostics.frames_not_attempted, 1);
        assert_eq!(fixture.state.diagnostics.frames_uncertain, 0);
    }
    let mut fixture = Fixture::single();
    let window = fixture.windows[0];
    let write = writes(&fixture.enable())[0];
    fixture.ack(
        window,
        write.plan,
        WindowActionOutcome::Stale(StaleReason::Destroyed),
    );
    assert!(matches!(
        outcome_of(&fixture.state, window),
        Some(OperationOutcome::Stale { .. })
    ));
}

// ----------------------------------------------------------------- plans

#[test]
fn a_b_a_plans_get_distinct_sequences_and_old_acks_cannot_clean_a_newer_plan() {
    let mut fixture = Fixture::single();
    let w1 = fixture.windows[0];
    let a1 = writes(&fixture.enable())[0];
    assert_eq!(a1.target, Rect::new(0.0, 25.0, 1920.0, 1055.0));

    // B: a second window changes the first one's target; the queued A is cancelled.
    let w2 = fixture.feed.token(1, 2);
    let created = fixture.feed.created(w2, "w2", 2);
    let effects = fixture.apply(created);
    assert!(cancels(&effects).contains(&w1));
    let b = writes(&effects).into_iter().find(|w| w.window == w1).expect("w1 rewritten");
    assert!(b.plan > a1.plan);
    assert_ne!(b.target, a1.target);

    // A again: the second window closes.
    let destroyed = fixture.feed.change(w2, WindowChange::Destroyed);
    let effects = fixture.apply(destroyed);
    let a2 = writes(&effects).into_iter().find(|w| w.window == w1).expect("w1 back to A");
    assert!(a2.plan > b.plan && a2.plan > a1.plan);
    assert_eq!(a2.target, a1.target, "same target, distinct plan sequence");

    // Old acknowledgements (plans one and two) cannot settle plan three.
    for stale in [a1.plan, b.plan] {
        fixture.ack(w1, stale, applied_at(a1.target));
        assert!(
            outcome_of(&fixture.state, w1).is_none(),
            "plan {stale} must not clean"
        );
    }
    assert_eq!(fixture.state.diagnostics.stale_frame_acks, 2);
    fixture.ack(w1, a2.plan, applied_at(a2.target));
    assert!(matches!(
        outcome_of(&fixture.state, w1),
        Some(OperationOutcome::Applied { .. })
    ));
}

#[test]
fn an_acknowledgement_for_another_revision_or_window_is_stale() {
    let mut fixture = Fixture::single();
    let window = fixture.windows[0];
    let write = writes(&fixture.enable())[0];
    let stamp = fixture.stamp();
    fixture.apply(Input::Completion {
        stamp,
        result: CompletionResult::Frame {
            window,
            plan_sequence: write.plan,
            revision: fixture.revision() + 9,
            outcome: applied_at(write.target),
        },
    });
    let other = fixture.feed.token(1, 77);
    fixture.ack(other, write.plan, applied_at(write.target));
    assert_eq!(fixture.state.diagnostics.stale_frame_acks, 2);
    assert!(outcome_of(&fixture.state, window).is_none());
}

#[test]
fn a_new_revision_may_retry_a_write_that_was_refused_before_sending() {
    let mut fixture = Fixture::single();
    let window = fixture.windows[0];
    let write = writes(&fixture.enable())[0];
    fixture.ack(
        window,
        write.plan,
        WindowActionOutcome::NotIssued(NotIssuedReason::RevisionChanged),
    );
    // Same revision, nothing changed: no retry.
    assert!(writes_of(&fixture.observe(window, Rect::new(10.0, 10.0, 800.0, 600.0))).is_empty());
    // A newer accepted revision never saw this write: it is planned again, and
    // under a fresh plan sequence so the old plan cannot acknowledge it.
    let effects = fixture.apply(Input::Config {
        revision: 2,
        config: gap_config(0),
    });
    let retry = writes(&effects);
    assert_eq!(retry.len(), 1);
    assert!(retry[0].plan > write.plan);
    // An acknowledgement from the old revision is stale.
    fixture.ack(window, write.plan, applied_at(write.target));
    assert!(fixture.state.diagnostics.stale_frame_acks >= 1);
    assert!(outcome_of(&fixture.state, window).is_none());
}

#[test]
fn one_external_change_creates_one_attempt_then_the_budget_is_spent() {
    let mut fixture = Fixture::single();
    let window = fixture.windows[0];
    let write = writes(&fixture.enable())[0];
    fixture.ack(window, write.plan, applied_at(write.target));
    assert_eq!(fixture.state.diagnostics.frame_attempts, 1);

    // The user drags the window away: one meaningful external change.
    let dragged = Rect::new(400.0, 300.0, 900.0, 700.0);
    let first = writes(&fixture.observe(window, dragged));
    assert_eq!(first.len(), 1);
    assert!(first[0].plan >= write.plan);
    fixture.ack(window, first[0].plan, applied_at(write.target));
    // Dragging again in the same intent does not fight the user again.
    assert!(writes_of(&fixture.observe(window, Rect::new(500.0, 100.0, 700.0, 500.0))).is_empty());
    assert_eq!(fixture.state.diagnostics.frame_attempts, 2);
    // A user command starts over.
    assert_eq!(writes_of(&fixture.apply(Input::Relayout)).len(), 1);
}

#[test]
fn a_changed_config_or_topology_target_creates_a_new_attempt() {
    let mut fixture = Fixture::single();
    let window = fixture.windows[0];
    let write = writes(&fixture.enable())[0];
    fixture.ack(window, write.plan, applied_at(write.target));
    // Config: an outer gap moves the target.
    let effects = fixture.apply(Input::Config {
        revision: 2,
        config: gap_config(8),
    });
    let next = writes(&effects);
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].target, Rect::new(8.0, 33.0, 1904.0, 1039.0));
    fixture.ack(window, next[0].plan, applied_at(next[0].target));
    // Topology: a second display changes nothing for this window: no write.
    let layout = DisplayLayout::new(vec![
        screen(1, true, 0.0, 0.0, 1920.0, 1080.0, 25.0),
        screen(2, false, 1920.0, 0.0, 1280.0, 720.0, 25.0),
    ]);
    assert!(writes_of(&fixture.apply(Input::Displays(Arc::new(layout)))).is_empty());
    // A different primary size moves it.
    let layout = DisplayLayout::new(vec![screen(1, true, 0.0, 0.0, 1440.0, 900.0, 25.0)]);
    assert_eq!(
        writes_of(&fixture.apply(Input::Displays(Arc::new(layout)))).len(),
        1
    );
}

#[test]
fn in_flight_attempts_are_bounded() {
    let mut fixture = Fixture::new(
        one_screen(),
        config(serde_json::json!({ "defaultLayout": "split" })),
        40,
    );
    let first = writes(&fixture.enable());
    assert!(first.len() <= MAX_IN_FLIGHT_FRAMES);
    assert_eq!(first.len(), MAX_IN_FLIGHT_FRAMES);
    // Completing one frees a slot for the next, without any retry of the rest.
    let done = first[0];
    let effects = fixture.ack(done.window, done.plan, applied_at(done.target));
    assert_eq!(writes_of(&effects).len(), 1);
}

#[test]
fn a_window_without_a_known_frame_is_not_written() {
    let mut fixture = Fixture::single();
    let window = fixture.windows[0];
    if let Some(state) = fixture.state.windows.get_mut(&window) {
        state.observed.frame = None;
    }
    assert!(writes_of(&fixture.enable()).is_empty());
}

#[test]
fn floating_layout_writes_nothing() {
    let mut fixture = Fixture::new(
        one_screen(),
        // `center` (the default) centers a newly floating window once; keeping the
        // application frame is what this test isolates.
        config(serde_json::json!({
            "defaultLayout": "floating",
            "floating": { "defaultPosition": "default" }
        })),
        2,
    );
    assert!(writes_of(&fixture.enable()).is_empty());
}

#[test]
fn a_destroyed_window_forgets_its_attempt_and_its_late_ack_is_stale() {
    let mut fixture = Fixture::single();
    let window = fixture.windows[0];
    let write = writes(&fixture.enable())[0];
    let destroyed = fixture.feed.change(window, WindowChange::Destroyed);
    fixture.apply(destroyed);
    assert!(!fixture.state.frames.records.contains_key(&window));
    fixture.ack(window, write.plan, applied_at(write.target));
    assert_eq!(fixture.state.diagnostics.stale_frame_acks, 1);
}

#[test]
fn a_registry_restart_forgets_attempts_and_keeps_the_plan_sequence_monotonic() {
    let mut fixture = Fixture::single();
    let write = writes(&fixture.enable())[0];
    let next = Feed::new(2);
    let effects = fixture.apply(Input::Start(next.stamp));
    assert!(fixture.state.frames.records.is_empty());
    assert!(writes(&effects).is_empty());
    assert!(fixture.state.frames.plan_sequence >= write.plan);
}

#[test]
fn a_rejected_write_queue_is_a_typed_no_send_not_a_retry() {
    let mut fixture = Fixture::single();
    let window = fixture.windows[0];
    let effects = fixture.enable();
    let rejected = effects
        .iter()
        .find(|e| matches!(e, Effect::WriteFrame { .. }))
        .cloned()
        .unwrap();
    let stamp = fixture.stamp();
    let effects = fixture.apply(Input::Completion {
        stamp,
        result: CompletionResult::Rejected(rejected),
    });
    assert!(writes_of(&effects).is_empty());
    assert!(matches!(
        outcome_of(&fixture.state, window),
        Some(OperationOutcome::NotAttempted {
            reason: NotAttemptedReason::QueueFull,
            ..
        })
    ));
}

#[test]
fn minimum_size_inference_is_bounded_and_reset_by_a_user_command() {
    let mut fixture = Fixture::new(one_screen(), gap_config(0), 2);
    let first = writes(&fixture.enable());
    let window = first[0].window;
    let mut rounds = 0;
    // An app that always comes out 200pt wider than asked: inference may
    // re-plan at most MAX_INFERENCES times, never a loop.
    for _ in 0..8 {
        let Some(attempt) = fixture
            .state
            .frames
            .records
            .get(&window)
            .and_then(|record| record.attempt.clone())
            .filter(FrameAttempt::in_flight)
        else {
            break;
        };
        rounds += 1;
        let wide = Rect::new(
            attempt.target.x,
            attempt.target.y,
            attempt.target.width + 200.0,
            attempt.target.height,
        );
        fixture.ack(
            window,
            attempt.plan_sequence,
            issued(
                IssuedKind::Applied,
                WriteStatus::Written,
                WriteStatus::Written,
                Some(frame_read(wide.x, wide.y, wide.width, wide.height)),
            ),
        );
    }
    let record = &fixture.state.frames.records[&window];
    assert!(record.inferences >= 1 && record.inferences <= MAX_INFERENCES);
    assert!(rounds <= 1 + usize::from(MAX_INFERENCES), "{rounds} rounds");
    assert!(record.minimum.inferred.is_some());
    fixture.apply(Input::Relayout);
    let record = &fixture.state.frames.records[&window];
    assert_eq!(record.inferences, 0);
    assert!(record.minimum.inferred.is_none());
}

fn writes_of(effects: &[Effect]) -> Vec<Write> { writes(effects) }

// ----------------------------------------------------------- reservations

fn projection() -> DisplayProjection {
    DisplayProjection::new(|native| SdkDisplayId::new(native.raw()))
}

fn edge(top: f64, left: f64, bottom: f64, right: f64) -> EdgeInsets {
    EdgeInsets::new(top, left, bottom, right).expect("insets")
}

fn host(revision: u64, topology: u64, displays: &[(u32, u64, EdgeInsets)]) -> HostReservations {
    use stache_sdk::{DisplayReservations, ScreenReservationSnapshot};
    HostReservations::from_snapshot(
        &ScreenReservationSnapshot {
            revision,
            topology: TopologyGeneration::new(topology),
            displays: displays
                .iter()
                .map(|(id, since, insets)| DisplayReservations {
                    display: SdkDisplayId::new(*id),
                    since: TopologyGeneration::new(*since),
                    insets: *insets,
                    contributions: Vec::new(),
                })
                .collect(),
        },
        &projection(),
    )
}

/// The single window's target on `layout` with `host` and outer gap `own`.
fn target_with(
    layout: DisplayLayout,
    host_state: Option<HostReservations>,
    own: u32,
) -> Option<Rect> {
    let mut fixture = Fixture::new(layout, gap_config(own), 1);
    if let Some(host_state) = host_state {
        fixture.apply(Input::Reservations(host_state));
    }
    writes(&fixture.enable()).first().map(|write| write.target)
}

#[test]
fn full_zero_native_24_host_40_own_6_reserves_46_not_70() {
    // Native 24, host 40, own gap 6 on the top edge: max(24, 40) + 6 = 46.
    let layout = DisplayLayout::new(vec![screen(1, true, 0.0, 0.0, 1000.0, 800.0, 24.0)]);
    let target = target_with(layout, Some(host(1, 1, &[(1, 1, edge(40.0, 0.0, 0.0, 0.0))])), 6)
        .expect("frame");
    assert_eq!(target, Rect::new(6.0, 46.0, 988.0, 800.0 - 46.0 - 6.0));
}

#[test]
fn every_edge_takes_max_of_native_and_host_then_adds_the_own_gap_once() {
    let layout = DisplayLayout::new(vec![screen(1, true, 0.0, 0.0, 1000.0, 800.0, 24.0)]);
    let reserved = edge(40.0, 50.0, 60.0, 70.0);
    let target = target_with(layout, Some(host(1, 1, &[(1, 1, reserved)])), 6).expect("frame");
    // top max(24, 40)+6, left 50+6, bottom 60+6, right 70+6.
    assert_eq!(
        target,
        Rect::new(56.0, 46.0, 1000.0 - 56.0 - 76.0, 800.0 - 46.0 - 66.0)
    );
}

#[test]
fn native_larger_than_host_wins() {
    let layout = DisplayLayout::new(vec![screen(1, true, 0.0, 0.0, 1000.0, 800.0, 24.0)]);
    let small = target_with(layout, Some(host(1, 1, &[(1, 1, edge(10.0, 0.0, 0.0, 0.0))])), 6);
    assert_eq!(small, Some(Rect::new(6.0, 30.0, 988.0, 800.0 - 30.0 - 6.0)));
}

#[test]
fn an_empty_snapshot_is_zero_host_inset_and_native_topology_still_gates_writes() {
    let layout = DisplayLayout::new(vec![screen(1, true, 0.0, 0.0, 1000.0, 800.0, 24.0)]);
    let without = target_with(layout.clone(), None, 6);
    let empty = target_with(layout.clone(), Some(host(1, 0, &[])), 6);
    let zero = target_with(layout, Some(host(1, 1, &[(1, 1, EdgeInsets::ZERO)])), 6);
    // Native exclusion (24) plus the own gap (6) survives a missing or zero reservation.
    let expected = Some(Rect::new(6.0, 30.0, 988.0, 764.0));
    assert_eq!(without, expected);
    assert_eq!(empty, expected);
    assert_eq!(zero, expected);

    // Native topology gates: losing every display plans nothing.
    let mut fixture = Fixture::single();
    let window = fixture.windows[0];
    let write = writes(&fixture.enable())[0];
    fixture.ack(window, write.plan, applied_at(write.target));
    let effects = fixture.apply(Input::Displays(Arc::new(DisplayLayout::new(Vec::new()))));
    assert!(fixture.state.no_screens && writes_of(&effects).is_empty());
    assert!(writes_of(&fixture.apply(Input::Relayout)).is_empty());
}

#[test]
fn negative_origins_and_scales_use_one_logical_space() {
    for scale in [1.0, 2.0] {
        // A lone display left of and below the AppKit origin: its AX frame has
        // a negative x and y.
        let mut only = screen(2, false, -1280.0, 200.0, 1280.0, 720.0, 24.0);
        only.scale = scale;
        let mut fixture = Fixture::new(DisplayLayout::new(vec![only]), gap_config(4), 1);
        fixture.apply(Input::Reservations(host(1, 1, &[(
            2,
            1,
            edge(40.0, 0.0, 0.0, 0.0),
        )])));
        let target = writes(&fixture.enable()).first().map(|write| write.target);
        // AX space: height 720, y = 720 - (200 + 720) = -200; top = max(24, 40) + 4.
        assert_eq!(
            target,
            Some(Rect::new(-1276.0, -200.0 + 44.0, 1272.0, 720.0 - 44.0 - 4.0)),
            "scale {scale}"
        );
    }
}

#[test]
fn infeasible_reservations_are_an_explicit_no_frame_diagnostic() {
    let layout = DisplayLayout::new(vec![screen(1, true, 0.0, 0.0, 1000.0, 800.0, 24.0)]);
    let mut fixture = Fixture::new(layout, gap_config(6), 1);
    fixture.apply(Input::Reservations(host(1, 1, &[(
        1,
        1,
        edge(500.0, 0.0, 400.0, 0.0),
    )])));
    assert!(writes_of(&fixture.enable()).is_empty());
    let snapshot = fixture.state.snapshot();
    assert_eq!(snapshot.no_frame, vec![(
        DisplayId::new(1),
        NoFrameReason::Geometry(crate::geometry::GeometryError::NoUsableArea)
    )]);
}

#[test]
fn nonfinite_native_geometry_is_an_explicit_no_frame_diagnostic() {
    let mut bad = screen(1, true, 0.0, 0.0, 1000.0, 800.0, 24.0);
    bad.visible_frame = DisplayRect::new(0.0, 0.0, f64::NAN, 776.0);
    let mut fixture = Fixture::new(DisplayLayout::new(vec![bad]), gap_config(0), 1);
    assert!(writes_of(&fixture.enable()).is_empty());
    assert!(matches!(
        fixture.state.snapshot().no_frame.first(),
        Some((
            _,
            NoFrameReason::Geometry(crate::geometry::GeometryError::NonFinite { .. })
        ))
    ));
}

#[test]
fn a_display_absent_from_a_present_host_topology_cannot_authorize_a_write() {
    let layout = DisplayLayout::new(vec![screen(1, true, 0.0, 0.0, 1000.0, 800.0, 24.0)]);
    let mut fixture = Fixture::new(layout, gap_config(0), 1);
    // The host lists only another display: zero inset, but no write.
    fixture.apply(Input::Reservations(host(1, 1, &[(
        7,
        1,
        edge(40.0, 0.0, 0.0, 0.0),
    )])));
    assert!(writes_of(&fixture.enable()).is_empty());
    assert_eq!(fixture.state.snapshot().no_frame, vec![(
        DisplayId::new(1),
        NoFrameReason::HostTopologyAbsent
    )]);
    // The host then lists it: authorized with its inset.
    let effects = fixture.apply(Input::Reservations(host(2, 2, &[(
        1,
        2,
        edge(40.0, 0.0, 0.0, 0.0),
    )])));
    assert_eq!(
        writes(&effects).first().map(|w| w.target),
        Some(Rect::new(0.0, 40.0, 1000.0, 760.0))
    );
}

#[test]
fn a_tombstoned_display_cancels_inflight_work_and_zero_insets_without_authorizing() {
    let layout = DisplayLayout::new(vec![screen(1, true, 0.0, 0.0, 1000.0, 800.0, 24.0)]);
    let mut fixture = Fixture::new(layout, gap_config(0), 1);
    let window = fixture.windows[0];
    fixture.apply(Input::Reservations(host(1, 1, &[(
        1,
        1,
        edge(40.0, 0.0, 0.0, 0.0),
    )])));
    let write = writes(&fixture.enable())[0];
    // The host drops the display (tombstone) and lists another: invalidated
    // atomically in the same transition, nothing is planned for it.
    let effects = fixture.apply(Input::Reservations(host(2, 2, &[(5, 2, EdgeInsets::ZERO)])));
    assert_eq!(cancels(&effects), vec![window]);
    assert!(writes_of(&effects).is_empty());
    assert_eq!(fixture.state.snapshot().no_frame, vec![(
        DisplayId::new(1),
        NoFrameReason::HostTopologyAbsent
    )]);
    // The in-flight acknowledgement of the old plan is still recorded truthfully
    // (it was issued), but nothing new is written.
    fixture.ack(window, write.plan, applied_at(write.target));
    assert!(writes_of(&fixture.apply(Input::Relayout)).is_empty());
}

#[test]
fn a_reservation_change_replaces_the_plan_and_a_stale_fetch_cannot_overwrite_a_successor() {
    let layout = DisplayLayout::new(vec![screen(1, true, 0.0, 0.0, 1000.0, 800.0, 24.0)]);
    let mut fixture = Fixture::new(layout, gap_config(0), 1);
    let window = fixture.windows[0];
    let first = writes(&fixture.enable())[0];
    // New reservation: the plan is replaced in the same transition.
    let effects = fixture.apply(Input::Reservations(host(5, 3, &[(
        1,
        1,
        edge(60.0, 0.0, 0.0, 0.0),
    )])));
    assert_eq!(cancels(&effects), vec![window]);
    let second = writes(&effects).first().copied().expect("replanned");
    assert!(second.plan > first.plan);
    assert_eq!(second.target, Rect::new(0.0, 60.0, 1000.0, 740.0));
    // An older fetch (lower revision) and an older topology are dropped.
    for stale in [
        host(4, 3, &[(1, 1, edge(0.0, 0.0, 0.0, 0.0))]),
        host(6, 2, &[(1, 1, edge(0.0, 0.0, 0.0, 0.0))]),
    ] {
        let effects = fixture.apply(Input::Reservations(stale));
        assert!(writes_of(&effects).is_empty() && cancels(&effects).is_empty());
    }
    assert_eq!(fixture.state.diagnostics.stale_reservations, 2);
    assert_eq!(fixture.state.host.revision, 5);
    // An unrelated revision bump that changes nothing for this display plans nothing.
    let effects = fixture.apply(Input::Reservations(host(7, 3, &[
        (1, 1, edge(60.0, 0.0, 0.0, 0.0)),
        (8, 1, edge(5.0, 0.0, 0.0, 0.0)),
    ])));
    assert!(writes_of(&effects).is_empty() && cancels(&effects).is_empty());
}

#[test]
fn a_reused_native_display_id_cannot_inherit_the_old_host_presence() {
    let both = DisplayLayout::new(vec![
        screen(1, true, 0.0, 0.0, 1000.0, 800.0, 24.0),
        screen(2, false, 1000.0, 0.0, 1000.0, 800.0, 24.0),
    ]);
    let only_first = DisplayLayout::new(vec![screen(1, true, 0.0, 0.0, 1000.0, 800.0, 24.0)]);
    let mut fixture = Fixture::new(both.clone(), gap_config(0), 2);
    let presences: Vec<u64> = fixture.state.screens.iter().map(|s| s.presence).collect();
    fixture.apply(Input::Reservations(host(1, 1, &[
        (1, 1, EdgeInsets::ZERO),
        (2, 1, edge(40.0, 0.0, 0.0, 0.0)),
    ])));
    let first = writes(&fixture.enable());
    assert!(!first.is_empty());
    // Display 2 is detached, then a (possibly different) display reuses id 2.
    fixture.apply(Input::Displays(Arc::new(only_first)));
    fixture.apply(Input::Displays(Arc::new(both)));
    let after: Vec<u64> = fixture.state.screens.iter().map(|s| s.presence).collect();
    assert_eq!(after[0], presences[0], "display 1 never left");
    assert!(after[1] > presences[1], "id 2 is a new native presence");
    // The host still reports the old presence (since 1): no write may land on it.
    let effects = fixture.apply(Input::Relayout);
    assert_eq!(fixture.state.snapshot().no_frame, vec![(
        DisplayId::new(2),
        NoFrameReason::StaleHostPresence
    )]);
    let _ = effects;
    // The host observes the reuse (presence since 2): authorized again.
    fixture.apply(Input::Reservations(host(2, 2, &[
        (1, 1, EdgeInsets::ZERO),
        (2, 2, edge(40.0, 0.0, 0.0, 0.0)),
    ])));
    assert!(fixture.state.snapshot().no_frame.is_empty());
}
