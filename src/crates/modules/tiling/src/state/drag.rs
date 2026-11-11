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
//! - **Down** (left or right button, as legacy) captures the focused
//!   workspace's tiled, non-floating windows with their exact tokens and frames,
//!   in stable workspace order. While a gesture exists the planner writes
//!   nothing ([`DragState::suppresses`]), so the user is never fought and their
//!   change is never counted as an external change to re-attempt. Our own
//!   in-flight writes that complete meanwhile move the baseline
//!   ([`rebase`]), so an echo is not mistaken for the user.
//! - **Up** of the last held button asks for one fresh read per captured window
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
    /// may have been missed.
    pub epoch: u64,
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
}

pub(super) fn report(state: &TilingState) -> DragReport {
    DragReport {
        active: state.drag.suppresses(),
        last: state.drag.last.clone(),
    }
}

// ------------------------------------------------------------------ signals

pub(super) fn signal(state: &mut TilingState, signal: DragSignal, fx: &mut Vec<Effect>) {
    match signal {
        DragSignal::Down { button, at, epoch, time } => {
            down(state, (button, at, epoch, time), fx);
        }
        DragSignal::Up { button, at, epoch, time } => up(state, (button, at, epoch, time), fx),
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
            gesture.held.insert(button);
            return;
        }
    }
    let Some(stamp) = state.stamp else { return };
    if !state.frames.enabled || state.config.is_none() || state.no_screens {
        state.diagnostics.drag_ignored += 1;
        return;
    }
    let Some(index) = intents::focused_index(state) else {
        state.diagnostics.drag_ignored += 1;
        return;
    };
    let layout = state.workspaces[index].layout;
    let windows: Vec<DragWindow> = intents::tiled(state, index)
        .into_iter()
        .filter_map(|token| {
            let original = state.windows.get(&token)?.observed.frame?;
            Some(DragWindow { token, original })
        })
        .collect();
    if layout == LayoutType::Floating || windows.is_empty() {
        state.diagnostics.drag_ignored += 1;
        return;
    }
    state.drag.next_id += 1;
    state.drag.gesture = Some(Gesture {
        id: state.drag.next_id,
        stamp,
        workspace: state.workspaces[index].id,
        layout,
        revision: state.config_revision,
        epoch,
        held: BTreeSet::from([button]),
        started: (at, time),
        windows,
        released: false,
        pending: BTreeSet::new(),
        travel: 0.0,
        held_for: Duration::ZERO,
    });
    state.diagnostics.drag_gestures += 1;
    // The user's gesture owns the window: a running animation stops at once and
    // writes nothing further (its queued frames become inert).
    fx.extend(
        state
            .frames
            .records
            .iter()
            .filter(|(_, record)| {
                record
                    .attempt
                    .as_ref()
                    .is_some_and(|attempt| attempt.animated && attempt.in_flight())
            })
            .map(|(token, _)| Effect::CancelFrame { window: *token }),
    );
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
    if gesture.epoch != epoch {
        lost(state, LostCause::Discontinuity);
        return;
    }
    if !gesture.held.remove(&button) || !gesture.held.is_empty() {
        return;
    }
    gesture.released = true;
    gesture.travel = (at.x - gesture.started.0.x).hypot(at.y - gesture.started.0.y);
    gesture.held_for = time.saturating_sub(gesture.started.1);
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
    // One plan follows: windows that left their frame may be attempted again.
    frames::force_deviating(state, &tokens);
    state.drag.last = Some(DragRecord {
        gesture: gesture.id,
        outcome,
        travel: gesture.travel,
        held_for: gesture.held_for,
    });
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
    let currents: Vec<(WindowInstance, Rect)> = gesture
        .windows
        .iter()
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
