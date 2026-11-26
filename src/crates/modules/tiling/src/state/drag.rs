//! The actor-side drag gesture: a pure state machine over [`DragSignal`]s and
//! window-frame snapshots.
//!
//! ```text
//!            Down                          Up (last button)
//!   Idle ───────────▶ Active ───────────────────────────▶ Settling
//!     ▲                 │ lost / timeout / epoch change       │ reads done
//!     │                 ▼                                     ▼
//!     └───────────── Cancelled                        commit (resize | swap |
//!                                                      snap-back | stale)
//! ```
//!
//! - **Geometry while held** (legacy `on_batched_geometry_updates`): a move or
//!   resize observation of a tiled window on a visible workspace while the
//!   button state shows a held button starts the gesture when none exists,
//!   capturing that window's workspace. The tap's down is not needed, so a
//!   dead or silent tap still never lets the planner fight the user.
//! - **Down** (left or right button, as legacy) captures the tiled,
//!   non-floating windows of the visible workspace on the display under the
//!   press (else the focused one) with their exact tokens and frames, in stable
//!   workspace order. While a gesture exists the planner writes
//!   nothing ([`DragState::suppresses`]), so the user is never fought and their
//!   change is never counted as an external change to re-attempt. Our own
//!   in-flight writes that complete meanwhile move the baseline
//!   ([`rebase`]), so an echo is not mistaken for the user.
//! - **Baselines** are each window's latest observed frame (where it really
//!   sits, even when that differs from what we asked: a refused minimum, a
//!   terminal grid, an unconfirmed write). The window whose observation started
//!   a gesture uses its frame before that observation, or the target of its
//!   in-flight write when the user grabbed it off that write's path. A
//!   sibling our animation was still moving ([`DragWindow::settling`]) may stop
//!   anywhere on its way when the gesture cancels it, so it is never taken for
//!   the resized or dragged window, only for a drop target.
//! - **Up** of the last held button, or the button state showing the release
//!   ([`DragSignal::Released`]) when the tap missed the up, asks for one fresh
//!   read per captured window
//!   (a [`Effect::Snapshot`] each) and waits for them, off the input callback.
//! - **Commit** revalidates the workspace, layout, revision and exact tile
//!   list. Then: a resize (legacy `find_resized_window`) moves the ratios; else
//!   the dragged window (greatest centre travel strictly above 50) swaps with the
//!   first original frame that inclusively contains its centre; else one
//!   snap-back plan. Any mismatch is stale: no swap, no ratio, a plain replan.
//! - A lost observation (overflow, discontinuity, permission loss, timeout,
//!   stop) or a down of an already held button cancels the gesture, never
//!   invents the missing up or replays a click, and reconciles the affected
//!   applications once.
//!
//! Floating windows are not captured: their movement stays the user's.

use std::collections::BTreeSet;
use std::time::Duration;

use stache_macos::windows::WindowInstance;

use super::{Cause, Effect, Scope, Stamp, TilingState, WorkspaceId, frames, intents, transition};
use crate::config::LayoutType;
use crate::drag::{
    DragPoint, DragSignal, LostCause, resized_window, select_dragged, select_target,
};
use crate::geometry::Rect;
use crate::layout::swap_windows;

/// One captured window: its exact token and its frame when the gesture began
/// (or when our own write last moved it).
#[derive(Clone, Debug, PartialEq)]
pub struct DragWindow {
    /// The exact window.
    pub token: WindowInstance,
    /// The baseline frame.
    pub original: Rect,
    /// Our animation was still moving it when the gesture began (and was
    /// cancelled there): where it stopped is not known for sure, so it is
    /// never taken for the resized or the dragged window, only a drop target.
    pub settling: bool,
}

/// One gesture in progress.
#[derive(Clone, Debug, PartialEq)]
pub struct Gesture {
    /// Monotonic gesture identity.
    pub id: u64,
    /// The registry the gesture was captured under.
    pub stamp: Stamp,
    /// The workspace it belongs to.
    pub workspace: WorkspaceId,
    /// The layout when it began.
    pub layout: LayoutType,
    /// The accepted configuration revision when it began.
    pub revision: u64,
    /// The input epoch of the down; a different epoch on the up means events
    /// may have been missed. `None` while no down was seen (a gesture started
    /// from geometry).
    pub epoch: Option<u64>,
    /// Started by a window's move or resize while a button was held.
    pub from_geometry: bool,
    /// Buttons currently held.
    pub held: BTreeSet<u32>,
    /// The down's location and time.
    pub started: (DragPoint, Duration),
    /// The tiled windows, in stable workspace order.
    pub windows: Vec<DragWindow>,
    /// The up arrived: reads of these windows are outstanding.
    pub released: bool,
    /// Windows whose fresh read has not returned.
    pub pending: BTreeSet<WindowInstance>,
    /// Pointer travel from the down to the final up, once released.
    pub travel: f64,
    /// How long the buttons were held, once released.
    pub held_for: Duration,
}

/// How a gesture ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DragOutcome {
    /// The dragged window swapped with the target.
    Swapped {
        /// The window the user moved.
        dragged: WindowInstance,
        /// The window it was dropped onto.
        target: WindowInstance,
    },
    /// A tile was resized; `changed` tells whether a ratio moved.
    Resized {
        /// The resized window.
        window: WindowInstance,
        /// Whether a ratio moved.
        changed: bool,
    },
    /// No swap target: windows return to their tiles through one plan.
    SnappedBack,
    /// The captured tiles no longer matched: no swap, no ratio.
    Stale,
    /// Observation was lost; nothing was decided.
    Cancelled(LostCause),
}

/// The last finished gesture, for status and tests.
#[derive(Clone, Debug, PartialEq)]
pub struct DragRecord {
    /// The gesture identity.
    pub gesture: u64,
    /// How it ended.
    pub outcome: DragOutcome,
    /// Pointer travel between down and up (zero when cancelled).
    pub travel: f64,
    /// How long the buttons were held (zero when cancelled).
    pub held_for: Duration,
}

/// All drag bookkeeping of the actor.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DragState {
    pub(crate) gesture: Option<Gesture>,
    next_id: u64,
    /// The latest finished gesture.
    pub last: Option<DragRecord>,
    /// The passive observer is not running: user gestures are invisible, so
    /// no minimum size is inferred from a window growing on its own.
    pub degraded: bool,
    /// Whether a mouse button is physically held, read by the actor for the
    /// input being applied (geometry observations only); `None` when no
    /// button state source exists or the input needs none.
    pub(crate) button_held: Option<bool>,
}

impl DragState {
    /// Whether a gesture is in progress or settling. While it is, the planner
    /// writes nothing, so the user's move or resize is never fought and never
    /// counted as an external change to re-attempt. The gesture ends at its up,
    /// a lost observation or its timeout, and the plan then runs once.
    #[must_use]
    pub const fn suppresses(&self) -> bool { self.gesture.is_some() }

    /// The identity of the gesture in progress.
    #[must_use]
    pub fn gesture_id(&self) -> Option<u64> { self.gesture.as_ref().map(|g| g.id) }

    /// A gesture is held: no up (from the tap or the button state) yet.
    #[must_use]
    pub fn awaits_release(&self) -> bool { self.gesture.as_ref().is_some_and(|g| !g.released) }

    /// The captured windows whose baseline frame contains the point where the
    /// gesture's mouse went down, while the gesture lasts (mouse down until the
    /// settled snapshot decided it). Read only: the border fallback (part 06)
    /// reads it; `drag.rs` and this module stay the only drag decision.
    #[must_use]
    pub fn pressed(&self) -> Vec<WindowInstance> {
        let Some(gesture) = &self.gesture else {
            return Vec::new();
        };
        gesture
            .windows
            .iter()
            .filter(|window| crate::drag::contains_inclusive(&window.original, gesture.started.0))
            .map(|window| window.token)
            .collect()
    }
}

/// A read-only drag view for the snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DragReport {
    /// A gesture is in progress or settling.
    pub active: bool,
    /// The latest finished gesture.
    pub last: Option<DragRecord>,
    /// The passive observer is not running.
    pub degraded: bool,
}

pub(super) fn report(state: &TilingState) -> DragReport {
    DragReport {
        active: state.drag.suppresses(),
        last: state.drag.last.clone(),
        degraded: state.drag.degraded,
    }
}

// ------------------------------------------------------------------ signals

pub(super) fn signal(state: &mut TilingState, signal: DragSignal, fx: &mut Vec<Effect>) {
    match signal {
        DragSignal::Down { button, at, epoch, time } => {
            state.diagnostics.drag_downs += 1;
            down(state, (button, at, epoch, time), fx);
        }
        DragSignal::Up { button, at, epoch, time } => {
            state.diagnostics.drag_ups += 1;
            up(state, (button, at, epoch, time), fx);
        }
        DragSignal::Released => {
            if state.drag.awaits_release() {
                state.diagnostics.drag_released_by_state += 1;
                release(state, None, fx);
            }
        }
        DragSignal::Lost(cause) => lost(state, cause),
        // Not an observation loss (the observer never ran), so it is not
        // counted in `drag_lost_observations` unless a gesture is in progress,
        // which is voided as a stopped observation (none can exist in practice).
        DragSignal::Unavailable => {
            if state.drag.gesture.is_some() {
                lost(state, LostCause::Stopped);
            }
            state.drag.degraded = true;
        }
    }
}

fn down(
    state: &mut TilingState,
    (button, at, epoch, time): (u32, DragPoint, u64, Duration),
    fx: &mut Vec<Effect>,
) {
    if let Some(gesture) = state.drag.gesture.as_mut() {
        if gesture.released || gesture.held.contains(&button) {
            // A down for a button that is already held, or after the release,
            // proves an up was missed. Nothing is invented: the old gesture is
            // void and the new down starts a fresh one.
            lost(state, LostCause::Discontinuity);
        } else {
            // A second button, or the down of a gesture geometry already started.
            gesture.held.insert(button);
            gesture.epoch.get_or_insert(epoch);
            return;
        }
    }
    if state.stamp.is_none() {
        return;
    }
    if !state.frames.enabled || state.config.is_none() || state.no_screens {
        state.diagnostics.drag_ignored += 1;
        return;
    }
    let Some(index) = pressed_index(state, at).or_else(|| intents::focused_index(state)) else {
        state.diagnostics.drag_ignored += 1;
        return;
    };
    let start = Start {
        moved: None,
        epoch: Some(epoch),
        held: BTreeSet::from([button]),
        started: (at, time),
        from_geometry: false,
    };
    if !begin(state, index, start, fx) {
        state.diagnostics.drag_ignored += 1;
    }
}

/// How a gesture begins.
struct Start {
    /// The window whose geometry started the gesture and its baseline.
    moved: Option<(WindowInstance, Rect)>,
    epoch: Option<u64>,
    held: BTreeSet<u32>,
    started: (DragPoint, Duration),
    from_geometry: bool,
}

/// Captures workspace `index`. `false` when there is nothing to capture (a
/// Floating or empty workspace).
fn begin(state: &mut TilingState, index: usize, start: Start, fx: &mut Vec<Effect>) -> bool {
    let Some(stamp) = state.stamp else { return false };
    let layout = state.workspaces[index].layout;
    // Every window starts at its latest observed frame (where it really sits,
    // whatever we last asked of it); the window whose observation started the
    // gesture starts at the frame it had before the user moved it.
    let windows: Vec<DragWindow> = intents::tiled(state, index)
        .into_iter()
        .filter_map(|token| {
            let observed = state.windows.get(&token)?.observed.frame;
            let original = match start.moved {
                Some((moved, frame)) if moved == token => frame,
                _ => observed?,
            };
            let animating = state
                .frames
                .records
                .get(&token)
                .and_then(|record| record.attempt.as_ref())
                .filter(|attempt| attempt.animated && attempt.in_flight());
            // The window the user holds (the moved one, or the one under the
            // press) is the user's, never settling. Its observed frame lags
            // behind our animation, so the press is tested against the whole
            // span of the animation too.
            let held = start.moved.map_or_else(
                || {
                    let at = start.started.0;
                    crate::drag::contains_inclusive(&original, at)
                        || animating.is_some_and(|attempt| {
                            let (a, b) = (attempt.from, attempt.target);
                            let (x, y) = (a.x.min(b.x), a.y.min(b.y));
                            let span = Rect::new(
                                x,
                                y,
                                a.max_x().max(b.max_x()) - x,
                                a.max_y().max(b.max_y()) - y,
                            );
                            crate::drag::contains_inclusive(&span, at)
                        })
                },
                |(moved, _)| moved == token,
            );
            let settling = !held && animating.is_some();
            Some(DragWindow { token, original, settling })
        })
        .collect();
    if layout == LayoutType::Floating || windows.is_empty() {
        return false;
    }
    let captured: Vec<WindowInstance> = windows.iter().map(|w| w.token).collect();
    state.drag.next_id += 1;
    state.drag.gesture = Some(Gesture {
        id: state.drag.next_id,
        stamp,
        workspace: state.workspaces[index].id,
        layout,
        revision: state.config_revision,
        epoch: start.epoch,
        from_geometry: start.from_geometry,
        held: start.held,
        started: start.started,
        windows,
        released: false,
        pending: BTreeSet::new(),
        travel: 0.0,
        held_for: Duration::ZERO,
    });
    state.diagnostics.drag_gestures += 1;
    if start.from_geometry {
        state.diagnostics.drag_from_geometry += 1;
    }
    // From here every read of a captured window may show the user's frame,
    // never an echo of an earlier animation.
    frames::clear_settle_echoes(state, &captured);
    // The user's gesture owns the windows: a running animation stops at once,
    // and every queued or running write of a captured window, animated or not,
    // becomes inert. A cancelled write that already moved its window rebases
    // the baseline to its readback when it answers.
    fx.extend(
        state
            .frames
            .records
            .iter()
            .filter(|(token, record)| {
                record.attempt.as_ref().is_some_and(|attempt| {
                    attempt.in_flight() && (attempt.animated || captured.contains(token))
                })
            })
            .map(|(token, _)| Effect::CancelFrame { window: *token }),
    );
    true
}

/// A move or resize of `token` was observed (a notification, a window-server
/// frame, a snapshot or the readback of our own write), before it is stored.
/// With a button held and no gesture, a tiled window on a visible workspace
/// starts one (legacy: the first geometry update while the mouse is down);
/// with no button held, a gesture still waiting for its up ends here (the tap
/// missed the up). Without a button state, nothing.
///
/// `frame` is the observed frame when the observation carries one. Nothing
/// starts for a frame at the window's baseline (a click, a refresh), for a
/// late echo of our own settled animation, or for a frame our own in-flight
/// write explains (on its path from where it began to its target). A window
/// with a write in flight starts the gesture only from a frame off that path,
/// with the write's target as its baseline; otherwise the baseline is its
/// observed frame before this observation.
pub(super) fn observe(
    state: &mut TilingState,
    token: WindowInstance,
    frame: Option<Rect>,
    fx: &mut Vec<Effect>,
) {
    let Some(held) = state.drag.button_held else { return };
    if state.drag.gesture.is_some() {
        if !held && state.drag.awaits_release() {
            state.diagnostics.drag_released_by_state += 1;
            release(state, None, fx);
        }
        return;
    }
    if !held || !state.frames.enabled || state.config.is_none() || state.no_screens {
        return;
    }
    let Some(index) = state
        .workspaces
        .iter()
        .enumerate()
        .filter(|(_, ws)| ws.visible && ws.order.contains(&token))
        .map(|(index, _)| index)
        .find(|index| intents::tiled(state, *index).contains(&token))
    else {
        return;
    };
    if frames::is_settle_echo(state, token, frame) {
        return;
    }
    let attempt = state.frames.records.get(&token).and_then(|record| record.attempt.as_ref());
    let original = match attempt {
        Some(attempt) if attempt.in_flight() => {
            let Some(seen) = frame else { return };
            if frames::explained_by_travel(&seen, &attempt.from, &attempt.target) {
                return;
            }
            attempt.target
        }
        _ => {
            let Some(observed) = state.windows.get(&token).and_then(|w| w.observed.frame) else {
                return;
            };
            // Reads within the tolerance were stored as they came, so the
            // observed frame can creep with the user's first pixels: a window
            // still near our settled frame is measured from that frame.
            attempt
                .map(frames::FrameAttempt::actual)
                .filter(|settled| frames::near_rect(settled, &observed))
                .unwrap_or(observed)
        }
    };
    if frame.is_some_and(|frame| frames::near_rect(&frame, &original)) {
        return;
    }
    let start = Start {
        moved: Some((token, original)),
        epoch: None,
        held: BTreeSet::new(),
        started: (crate::drag::center(&original), Duration::ZERO),
        from_geometry: true,
    };
    begin(state, index, start, fx);
}

/// The visible workspace on the display containing the down's location (AX
/// global space, top-left origin, the same space as `CGEvent` locations and
/// window frames). `None` when no display contains the point.
fn pressed_index(state: &TilingState, at: DragPoint) -> Option<usize> {
    let display = state.layout.displays().iter().find(|display| {
        state.layout.ax_frame(display).is_some_and(|frame| {
            at.x >= frame.x
                && at.x <= frame.x + frame.width
                && at.y >= frame.y
                && at.y <= frame.y + frame.height
        })
    })?;
    state.workspaces.iter().position(|ws| ws.visible && ws.screen == display.id)
}

fn up(
    state: &mut TilingState,
    (button, at, epoch, time): (u32, DragPoint, u64, Duration),
    fx: &mut Vec<Effect>,
) {
    let Some(gesture) = state.drag.gesture.as_mut() else {
        return;
    };
    if gesture.released {
        return;
    }
    if gesture.epoch.is_some_and(|down| down != epoch) {
        lost(state, LostCause::Discontinuity);
        return;
    }
    let removed = gesture.held.remove(&button);
    // A gesture geometry started has seen no down: any up ends it.
    if !gesture.held.is_empty() || (!removed && gesture.epoch.is_some()) {
        return;
    }
    release(state, Some((at, time)), fx);
}

/// The last button is up (`Some` with the up's location and time) or the
/// button state shows the release (`None`): one fresh read per captured
/// window, then the commit.
fn release(state: &mut TilingState, up: Option<(DragPoint, Duration)>, fx: &mut Vec<Effect>) {
    let Some(gesture) = state.drag.gesture.as_mut() else {
        return;
    };
    gesture.released = true;
    gesture.held.clear();
    if let Some((at, time)) = up {
        gesture.travel = (at.x - gesture.started.0.x).hypot(at.y - gesture.started.0.y);
        gesture.held_for = time.saturating_sub(gesture.started.1);
    }
    let stamp = gesture.stamp;
    let tokens: Vec<WindowInstance> = gesture
        .windows
        .iter()
        .map(|w| w.token)
        .filter(|token| state.windows.contains_key(token))
        .collect();
    // The final frames are read off the input callback, one fresh read each.
    if let Some(gesture) = state.drag.gesture.as_mut() {
        gesture.pending = tokens.iter().copied().collect();
    }
    fx.extend(tokens.into_iter().map(|window| Effect::Snapshot { stamp, window }));
    if state.drag.gesture.as_ref().is_some_and(|g| g.pending.is_empty()) {
        commit(state);
    }
}

/// A read of `token` returned (any outcome) or could not be queued.
pub(super) fn on_read(state: &mut TilingState, token: WindowInstance) {
    let Some(gesture) = state.drag.gesture.as_mut() else {
        return;
    };
    if !gesture.released || !gesture.pending.remove(&token) {
        return;
    }
    if gesture.pending.is_empty() {
        commit(state);
    }
}

/// Our own write completed: its readback is the window's new baseline.
pub(super) fn rebase(drag: &mut DragState, token: WindowInstance, frame: Rect) {
    if let Some(window) = drag
        .gesture
        .as_mut()
        .and_then(|g| g.windows.iter_mut().find(|w| w.token == token))
    {
        window.original = frame;
    }
}

// ------------------------------------------------------------------- cancel

/// Observation was lost. A gesture in progress is void: nothing is decided, the
/// affected applications are reconciled once and their windows get one fresh
/// external-change attempt. With no gesture only the loss is counted.
pub(super) fn lost(state: &mut TilingState, cause: LostCause) {
    state.diagnostics.drag_lost_observations += 1;
    let Some(gesture) = state.drag.gesture.take() else {
        return;
    };
    state.diagnostics.drag_cancelled += 1;
    let tokens: Vec<WindowInstance> = gesture.windows.iter().map(|w| w.token).collect();
    let applications: BTreeSet<_> = tokens.iter().map(WindowInstance::application).collect();
    for application in applications {
        transition::mark(state, Scope::App(application), Cause::DragLost);
    }
    frames::reset_externals(state, &tokens);
    note_spans(state, &gesture);
    state.drag.last = Some(DragRecord {
        gesture: gesture.id,
        outcome: DragOutcome::Cancelled(cause),
        travel: 0.0,
        held_for: Duration::ZERO,
    });
}

// ------------------------------------------------------------------- commit

fn commit(state: &mut TilingState) {
    let Some(gesture) = state.drag.gesture.take() else {
        return;
    };
    let tokens: Vec<WindowInstance> = gesture.windows.iter().map(|w| w.token).collect();
    let outcome = decide(state, &gesture).unwrap_or(DragOutcome::Stale);
    match &outcome {
        DragOutcome::Swapped { .. } => state.diagnostics.drag_swaps += 1,
        DragOutcome::Resized { .. } => state.diagnostics.drag_resizes += 1,
        DragOutcome::SnappedBack => state.diagnostics.drag_snap_backs += 1,
        DragOutcome::Stale => state.diagnostics.drag_stale += 1,
        DragOutcome::Cancelled(_) => {}
    }
    // One plan follows: windows that left their frame may be attempted again,
    // and a read of the user's frame arriving after this commit (a read that
    // was already running at the up answered first) still gets one attempt.
    frames::reset_externals(state, &tokens);
    frames::force_deviating(state, &tokens);
    note_spans(state, &gesture);
    state.drag.last = Some(DragRecord {
        gesture: gesture.id,
        outcome,
        travel: gesture.travel,
        held_for: gesture.held_for,
    });
}

/// The user's geometry of every captured window: from its baseline to its
/// frame now. Late reads inside it never infer a minimum.
fn note_spans(state: &mut TilingState, gesture: &Gesture) {
    let spans: Vec<(WindowInstance, Rect, Rect)> = gesture
        .windows
        .iter()
        .filter_map(|w| Some((w.token, w.original, state.windows.get(&w.token)?.observed.frame?)))
        .collect();
    frames::note_user_span(state, &spans);
}

/// Revalidates the gesture and decides its outcome; `None` when stale.
fn decide(state: &mut TilingState, gesture: &Gesture) -> Option<DragOutcome> {
    if state.stamp != Some(gesture.stamp) || state.config_revision != gesture.revision {
        return None;
    }
    let index = state.workspaces.iter().position(|ws| ws.id == gesture.workspace)?;
    if state.workspaces[index].layout != gesture.layout {
        return None;
    }
    let ids = intents::tiled(state, index);
    let same_tiles = ids.len() == gesture.windows.len()
        && ids.iter().zip(&gesture.windows).all(|(id, window)| *id == window.token);
    if !same_tiles {
        return None;
    }
    let originals: Vec<(WindowInstance, Rect)> =
        gesture.windows.iter().map(|w| (w.token, w.original)).collect();
    // A window our cancelled animation left somewhere on its way has no
    // current frame for the decision: it can only be dropped onto.
    let currents: Vec<(WindowInstance, Rect)> = gesture
        .windows
        .iter()
        .filter(|w| !w.settling)
        .filter_map(|w| Some((w.token, state.windows.get(&w.token)?.observed.frame?)))
        .collect();

    if let Some((window, old, new)) = resized_window(&originals, &currents) {
        let changed = intents::apply_user_resize(state, index, &ids, window, (&old, &new))
            == intents::UserResize::Changed;
        return Some(DragOutcome::Resized { window, changed });
    }
    if let Some((dragged, frame)) = select_dragged(&originals, &currents)
        && let Some(target) = select_target(&originals, dragged, crate::drag::center(&frame))
        && swap_windows(&mut state.workspaces[index].order, &dragged, &target)
    {
        return Some(DragOutcome::Swapped { dragged, target });
    }
    Some(DragOutcome::SnappedBack)
}
