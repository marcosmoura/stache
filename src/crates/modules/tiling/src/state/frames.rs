//! Pure frame planning and the bookkeeping of frame writes.
//!
//! The actor keeps, per window, the **desired** frame (from the layout plan),
//! the **confirmed actual** frame (the readback of a write or an observed
//! snapshot) and at most one [`FrameAttempt`]. A monotonic plan sequence
//! changes whenever the desired frames change, so an A, B, A history yields
//! three distinct sequences and an old acknowledgement never matches a newer
//! attempt.
//!
//! Only changed, eligible frames are written. Per window and attempt each
//! component (position, size) is issued at most once. An attempt, once
//! settled, is not repeated: only a new target, a user command
//! ([`super::Input::Relayout`]), a display change, showing the window's
//! workspace or one meaningful external change creates another. The window's own echoes and periodic refreshes show the frame the
//! attempt already recorded, so they create nothing. There is no retry loop and
//! no fighting an application that refuses to resize.

use std::collections::{BTreeMap, BTreeSet};

use stache_macos::windows::{WindowActionOutcome, WindowInstance};
use stache_macos::{DisplayId, DisplayLayout};

use super::{Effect, ScreenState, TilingState, WorkspaceId};
use crate::config::TilingConfig;
use crate::geometry::{GeometryError, Insets, Rect, usable_area};
use crate::layout::{
    GapDisplay, Gaps, LayoutRequest, PlanStatus, WindowEntry, master_ratio, plan_layout,
};
use crate::minimum_size::{
    INFERENCE_TOLERANCE, MinimumConstraint, MinimumRequest, MinimumStatus, Size, default_ratios,
    enforce, infer_minimum,
};
use crate::outcome::{FrameComponents, NotAttemptedReason, OperationOutcome, classify_frame};
use crate::reservations::{HostLookup, HostReservations};

/// Frame writes in flight at once. More wait for a completion to free a slot.
pub const MAX_IN_FLIGHT_FRAMES: usize = 16;
/// External-change attempts per window until the target changes or the user
/// asks again.
pub const MAX_EXTERNAL_ATTEMPTS: u8 = 1;
/// External changes corrected per window after a display change, for free.
///
/// They do not spend [`MAX_EXTERNAL_ATTEMPTS`]: the system and the applications
/// move windows in more than one step while the displays are rearranged.
pub const DISPLAY_CHANGE_ATTEMPTS: u8 = 3;
/// Minimum-size inferences per window until the user asks again.
pub const MAX_INFERENCES: u8 = 2;
/// Own geometry echoes ignored per window after an animation settled. A read
/// taken during the motion can arrive after the completion; it is the animation
/// echoing, not the user. The budget is finite, so a real change always shows.
pub const MAX_SETTLE_ECHOES: u8 = 2;

/// Why a display produced no frame at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoFrameReason {
    /// Tiling's own native display state does not know the display.
    UnknownDisplay,
    /// The host topology exists and does not list the display, so a write
    /// cannot be authorized for it (it may be a reused native id).
    HostTopologyAbsent,
    /// The host still reports the presence that predates this native presence
    /// of the id, so a write could land on a reused display.
    StaleHostPresence,
    /// Invalid, non-finite or infeasible geometry.
    Geometry(GeometryError),
}

/// Where an attempt stands.
#[derive(Clone, Debug, PartialEq)]
pub enum AttemptStatus {
    /// Queued or running; no acknowledgement yet.
    InFlight,
    /// Acknowledged with this outcome.
    Settled(OperationOutcome),
}

/// One bounded frame write for one window and plan.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameAttempt {
    /// The plan sequence it belongs to.
    pub plan_sequence: u64,
    /// The accepted configuration revision it was planned under.
    pub revision: u64,
    /// The desired frame.
    pub target: Rect,
    /// The components that differed and were requested (each at most once).
    pub wanted: FrameComponents,
    /// The window's actual frame when the attempt was created.
    pub from: Rect,
    /// The plan asked for an animated transition. While it is in flight the
    /// window is in motion: its observed frame is not a baseline.
    pub animated: bool,
    /// The one size-only re-write that checks a wider readback after a write
    /// that moved and resized the window together.
    pub corrective: bool,
    /// Progress.
    pub status: AttemptStatus,
}

impl FrameAttempt {
    /// The window's actual frame as of the attempt: the readback when there is
    /// one, otherwise the frame before the write.
    #[must_use]
    pub fn actual(&self) -> Rect {
        match &self.status {
            AttemptStatus::Settled(outcome) => outcome.actual().unwrap_or(self.from),
            AttemptStatus::InFlight => self.from,
        }
    }

    /// Whether the attempt is waiting for an acknowledgement.
    #[must_use]
    pub const fn in_flight(&self) -> bool { matches!(self.status, AttemptStatus::InFlight) }
}

/// Per-window frame knowledge.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameRecord {
    /// The latest attempt.
    pub attempt: Option<FrameAttempt>,
    /// Reported and inferred minimum size.
    pub minimum: MinimumConstraint,
    /// Inferences used.
    pub inferences: u8,
    /// External-change attempts used for the current target.
    pub externals: u8,
    /// External changes still corrected for free after a display change.
    pub settling: u8,
    /// A user command asked for a fresh attempt.
    pub forced: bool,
    /// Own geometry echoes of a settled animation still to ignore (bounded).
    pub settle_echoes: u8,
    /// A write that moved and resized the window read back larger than its
    /// target: one size-only re-write of the same target is owed before any
    /// minimum is learned (the readback may be a transient of the move).
    pub size_check: bool,
    /// The target the one corrective size-only write was already sent for:
    /// at most one per target, so a refused size is never fought.
    pub size_checked: Option<Rect>,
}

/// A native display presence a host reservation was last accepted for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    /// Tiling's native presence counter for the display.
    pub presence: u64,
    /// The host presence (`since`) that was accepted with it.
    pub since: u64,
}

/// All frame bookkeeping of the actor.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameBook {
    /// Frame writes may be planned (the runtime published its authority).
    pub enabled: bool,
    /// The monotonic layout plan sequence.
    pub plan_sequence: u64,
    /// The newest plan sequence an attempt was issued under. Every issuing round
    /// uses a sequence no earlier attempt has, so an old acknowledgement can
    /// never match a newer attempt.
    pub issued_plan: u64,
    /// The desired frames of the latest plan.
    pub desired: BTreeMap<WindowInstance, Rect>,
    /// Per-window records.
    pub records: BTreeMap<WindowInstance, FrameRecord>,
    /// Displays that produced no frame, with the reason (rebuilt each plan).
    pub no_frame: BTreeMap<DisplayId, NoFrameReason>,
    /// Accepted host presences by native display.
    pub bindings: BTreeMap<DisplayId, Binding>,
    /// One-shot frames for floating windows (an explicit preset or the default
    /// centering). A pin lives only until its attempt settles, so a floating
    /// window the user later moves is never pulled back.
    pub pinned: BTreeMap<WindowInstance, Rect>,
    /// The visible workspaces as of the last plan. A workspace that is visible
    /// now and was not then has just been revealed (switch, display return,
    /// startup).
    pub last_visible: BTreeSet<WorkspaceId>,
    /// Windows of a just-revealed workspace that have not had their first
    /// settled write yet. Their writes are direct, never animated: animation is
    /// only for layout changes inside an already visible workspace. A window
    /// leaves the set when it is written, found already at its target, or has
    /// no frame to write (a floating window).
    pub revealed: BTreeSet<WindowInstance>,
}

impl FrameBook {
    fn in_flight(&self) -> usize {
        self.records
            .values()
            .filter(|record| record.attempt.as_ref().is_some_and(FrameAttempt::in_flight))
            .count()
    }
}

fn near(a: f64, b: f64) -> bool { (a - b).abs() <= INFERENCE_TOLERANCE }

fn near_rect(a: &Rect, b: &Rect) -> bool {
    near(a.x, b.x) && near(a.y, b.y) && near(a.width, b.width) && near(a.height, b.height)
}

fn needed(actual: &Rect, target: &Rect) -> FrameComponents {
    FrameComponents {
        position: !(near(actual.x, target.x) && near(actual.y, target.y)),
        size: !(near(actual.width, target.width) && near(actual.height, target.height)),
    }
}

// ------------------------------------------------------------------ geometry

const fn rect_of(rect: stache_macos::applications::DisplayRect) -> Rect {
    Rect::new(rect.x, rect.y, rect.width, rect.height)
}

fn native_insets(bounds: &Rect, visible: &Rect) -> Result<Insets, GeometryError> {
    let insets = Insets::new(
        visible.y - bounds.y,
        bounds.max_x() - visible.max_x(),
        bounds.max_y() - visible.max_y(),
        visible.x - bounds.x,
    );
    if [insets.top, insets.right, insets.bottom, insets.left]
        .iter()
        .any(|v| !v.is_finite())
    {
        return Err(GeometryError::NonFinite { what: "native insets" });
    }
    // A visible frame can never exclude a negative amount.
    Ok(Insets::new(
        insets.top.max(0.0),
        insets.right.max(0.0),
        insets.bottom.max(0.0),
        insets.left.max(0.0),
    ))
}

/// Everything the geometry of one display needs.
struct DisplayInputs<'a> {
    layout: &'a DisplayLayout,
    screens: &'a [ScreenState],
    host: &'a HostReservations,
    config: &'a TilingConfig,
}

fn usable_for(
    inputs: &DisplayInputs<'_>,
    bindings: &mut BTreeMap<DisplayId, Binding>,
    id: DisplayId,
) -> Result<(Rect, Gaps), NoFrameReason> {
    let info = inputs.layout.get(id).ok_or(NoFrameReason::UnknownDisplay)?;
    let screen = inputs
        .screens
        .iter()
        .find(|screen| screen.id == id)
        .ok_or(NoFrameReason::UnknownDisplay)?;
    let bounds = inputs.layout.ax_frame(info).map(rect_of).ok_or(NoFrameReason::UnknownDisplay)?;
    let visible = inputs
        .layout
        .ax_visible_frame(info)
        .map(rect_of)
        .ok_or(NoFrameReason::UnknownDisplay)?;
    let native = native_insets(&bounds, &visible).map_err(NoFrameReason::Geometry)?;
    let host = match inputs.host.lookup(id) {
        HostLookup::NoHostTopology => {
            bindings.remove(&id);
            Insets::ZERO
        }
        HostLookup::Absent => return Err(NoFrameReason::HostTopologyAbsent),
        HostLookup::Present(host) => {
            if bindings
                .get(&id)
                .is_some_and(|bound| bound.presence != screen.presence && bound.since == host.since)
            {
                return Err(NoFrameReason::StaleHostPresence);
            }
            bindings.insert(id, Binding {
                presence: screen.presence,
                since: host.since,
            });
            host.insets
        }
    };
    let gaps = Gaps::from_config(&inputs.config.gaps, GapDisplay {
        name: info.name.as_deref().unwrap_or(""),
        is_main: info.is_main,
    });
    let usable = usable_area(bounds, native, host, &gaps).map_err(NoFrameReason::Geometry)?;
    Ok((usable, gaps))
}

/// The host-resolution fingerprint of every screen, used to decide whether a
/// reservation hint changed anything Tiling uses.
fn resolutions(state: &TilingState) -> Vec<(DisplayId, HostLookup)> {
    state
        .screens
        .iter()
        .map(|screen| (screen.id, state.host.lookup(screen.id)))
        .collect()
}

/// Adopts a host reservation hint. A hint older than the one held (a stale
/// fetch) changes nothing.
pub(super) fn apply_reservations(state: &mut TilingState, host: HostReservations) {
    if host.revision < state.host.revision || host.topology < state.host.topology {
        state.diagnostics.stale_reservations += 1;
        return;
    }
    if host == state.host {
        return;
    }
    let before = resolutions(state);
    state.host = host;
    // The plan sequence moves when the plan re-evaluates in this same
    // transition; resolutions that changed also drop stale authority bindings.
    if before != resolutions(state) {
        let live: Vec<DisplayId> = state.screens.iter().map(|screen| screen.id).collect();
        state.frames.bindings.retain(|id, _| live.contains(id));
    }
}

fn desired_frames(state: &mut TilingState) -> BTreeMap<WindowInstance, Rect> {
    let config_arc = state.config.clone().unwrap_or_default();
    let config: &TilingConfig = &config_arc;
    let inputs = DisplayInputs {
        layout: &state.layout,
        screens: &state.screens,
        host: &state.host,
        config,
    };
    let mut usable: BTreeMap<DisplayId, Result<(Rect, Gaps), NoFrameReason>> = BTreeMap::new();
    let mut no_frame = BTreeMap::new();
    let mut desired = BTreeMap::new();
    for workspace in state.workspaces.iter().filter(|workspace| workspace.visible) {
        let entry = usable
            .entry(workspace.screen)
            .or_insert_with(|| usable_for(&inputs, &mut state.frames.bindings, workspace.screen));
        let (area, gaps) = match entry {
            Ok(found) => *found,
            Err(reason) => {
                no_frame.insert(workspace.screen, *reason);
                continue;
            }
        };
        let entries: Vec<WindowEntry<WindowInstance>> = workspace
            .order
            .iter()
            .filter_map(|token| state.windows.get(token))
            .map(|window| WindowEntry::with_flags(window.token, state.flags_of(window)))
            .collect();
        let tiled = tiled_count(workspace.layout, &entries);
        let request = LayoutRequest {
            layout: workspace.layout,
            windows: &entries,
            usable: area,
            gaps,
            master_ratio: master_ratio(&config.master, workspace.ratios.master_ratio),
            master_position: config.master.position,
            ratios: workspace.ratios.effective_split_ratios(workspace.layout, tiled),
        };
        let plan = plan_layout(&request);
        if let PlanStatus::Infeasible(error) = plan.status {
            no_frame.insert(workspace.screen, NoFrameReason::Geometry(error));
            continue;
        }
        let mut frames = plan.frames;
        // Grid cells stay fixed (an intentional deviation from legacy): an
        // inferred minimum never reshapes them; a refusing window overflows.
        let grid = workspace.layout == crate::config::LayoutType::Grid;
        let minimum_of = |token: &WindowInstance| {
            state.frames.records.get(token).and_then(|record| {
                if grid {
                    MinimumConstraint {
                        inferred: None,
                        ..record.minimum
                    }
                    .effective()
                } else {
                    record.minimum.effective()
                }
            })
        };
        if frames.iter().any(|(token, _)| minimum_of(token).is_some()) {
            let ids: Vec<WindowInstance> = frames.iter().map(|(token, _)| *token).collect();
            let defaults = default_ratios(workspace.layout, ids.len());
            let user = workspace.ratios.effective_split_ratios(workspace.layout, ids.len());
            let ratios = if !user.is_empty() && user.len() == defaults.len() {
                user.to_vec()
            } else {
                defaults
            };
            let result = enforce(&MinimumRequest {
                layout: workspace.layout,
                ids: &ids,
                usable: area,
                gaps,
                ratios: &ratios,
                minimum_of: &minimum_of,
            });
            if !matches!(result.status, MinimumStatus::Infeasible(_)) && !result.frames.is_empty() {
                frames = result.frames;
            }
        }
        desired.extend(frames);
        // One-shot floating frames: only for a window the plan left alone.
        for (token, _) in &plan.untouched {
            if let Some(pin) = state.frames.pinned.get(token) {
                desired.insert(*token, *pin);
            }
        }
    }
    state.frames.no_frame = no_frame;
    desired
}

/// How many windows the plan tiles (the Grid cap applies).
fn tiled_count(
    layout: crate::config::LayoutType,
    entries: &[WindowEntry<WindowInstance>],
) -> usize {
    let eligible = entries
        .iter()
        .filter(|e| {
            let f = e.flags;
            !(f.explicit_floating || f.inactive_tab || f.fullscreen || f.minimized || f.hidden)
        })
        .count();
    if layout == crate::config::LayoutType::Grid {
        eligible.min(crate::layout::MAX_GRID_TILES)
    } else {
        eligible
    }
}

/// The usable area and gaps of a display, or why there is none. Shared by
/// layout, presets, centering and resize so they all see one geometry.
pub(super) fn display_area(
    state: &mut TilingState,
    id: DisplayId,
) -> Result<(Rect, Gaps), NoFrameReason> {
    let config_arc = state.config.clone().unwrap_or_default();
    let inputs = DisplayInputs {
        layout: &state.layout,
        screens: &state.screens,
        host: &state.host,
        config: &config_arc,
    };
    usable_for(&inputs, &mut state.frames.bindings, id)
}

// ------------------------------------------------------------------ planning

enum Decision {
    Skip,
    Issue { external: bool },
}

/// `revealed`: the window's workspace was just shown. Showing a workspace lays
/// it out, so a settled attempt does not hold back a window left off its tile
/// (its external correction was used, or its last write ended unknown): it is
/// written once more. One write per showing, so nothing is fought; skipped, the
/// window would also stay revealed and its border hidden for good.
fn decide(
    record: &FrameRecord,
    target: &Rect,
    actual: &Rect,
    revision: u64,
    revealed: bool,
) -> Decision {
    let Some(attempt) = &record.attempt else {
        return Decision::Issue { external: false };
    };
    let same_target = near_rect(&attempt.target, target);
    match &attempt.status {
        AttemptStatus::InFlight => {
            if same_target && attempt.revision == revision && !record.forced {
                Decision::Skip
            } else {
                Decision::Issue { external: false }
            }
        }
        AttemptStatus::Settled(outcome) => {
            if record.forced || revealed || !same_target || record.size_check {
                return Decision::Issue { external: false };
            }
            // A proven-no-send caused by a plan or revision change never
            // consumed the write: it may be tried again under the new plan.
            let unspent = outcome.proves_no_send()
                && (matches!(outcome, OperationOutcome::NotAttempted {
                    reason: NotAttemptedReason::Superseded,
                    ..
                }) || attempt.revision != revision);
            if unspent && !matches!(outcome, OperationOutcome::Stale { .. }) {
                return Decision::Issue { external: false };
            }
            // The window refused this very target (a write that was really sent) with a size it re-asserts:
            // it overflows its cell and is not fought (no external retry).
            if record.minimum.inferred.is_some_and(|minimum| {
                target.width < minimum.width - INFERENCE_TOLERANCE
                    || target.height < minimum.height - INFERENCE_TOLERANCE
            }) {
                return Decision::Skip;
            }
            // No retry after an unknown result or a refusal.
            if matches!(outcome, OperationOutcome::Uncertain { .. })
                || near_rect(&attempt.actual(), actual)
            {
                return Decision::Skip;
            }
            if record.settling > 0 || record.externals < MAX_EXTERNAL_ATTEMPTS {
                Decision::Issue { external: true }
            } else {
                Decision::Skip
            }
        }
    }
}

/// Marks the windows of workspaces that became visible since the last plan as
/// just revealed, and forgets windows that are no longer on a visible workspace.
fn note_revealed(state: &mut TilingState) {
    let visible: BTreeSet<WorkspaceId> = state
        .workspaces
        .iter()
        .filter(|workspace| workspace.visible)
        .map(|workspace| workspace.id)
        .collect();
    for workspace in state
        .workspaces
        .iter()
        .filter(|workspace| workspace.visible && !state.frames.last_visible.contains(&workspace.id))
    {
        state.frames.revealed.extend(workspace.order.iter().copied());
    }
    let on_visible: BTreeSet<WindowInstance> = state
        .workspaces
        .iter()
        .filter(|workspace| workspace.visible)
        .flat_map(|workspace| workspace.order.iter().copied())
        .collect();
    state.frames.revealed.retain(|token| on_visible.contains(token));
    state.frames.last_visible = visible;
}

/// Plans the visible workspaces and issues the changed, eligible frames.
#[allow(clippy::too_many_lines)] // one linear planning pass over the desired frames
pub(super) fn plan_and_issue(state: &mut TilingState, fx: &mut Vec<Effect>) {
    state.frames.no_frame.clear();
    // A gesture is the user's (see `DragState::suppresses`): nothing is written.
    if !state.frames.enabled || state.drag.suppresses() {
        return;
    }
    let Some(stamp) = state.stamp else { return };
    if state.config.is_none() || !state.screens_known || state.no_screens || !state.planning_ready()
    {
        return;
    }
    // Windows whose own application is unconfirmed (failed, truncated, being
    // tracked or reconciled) are laid out around but never written.
    let unsettled = state.unsettled_apps();
    note_revealed(state);
    let desired = desired_frames(state);
    // A revealed window with no frame to write (a floating window, a Grid
    // extra) is never "written or found at its target": it waits for nothing.
    state.frames.revealed.retain(|token| desired.contains_key(token));
    let forced = state.frames.records.values().any(|record| record.forced);
    if desired != state.frames.desired || forced {
        state.frames.plan_sequence += 1;
    }
    state.frames.desired.clone_from(&desired);
    let revision = state.config_revision;
    let animations = state.config.as_ref().map(|config| config.animations.clone());

    // A queued write whose window no longer has the same target is cancelled
    // before it can run.
    let cancelled: Vec<WindowInstance> = state
        .frames
        .records
        .iter()
        .filter(|(token, record)| {
            record.attempt.as_ref().is_some_and(|attempt| {
                attempt.in_flight()
                    && desired.get(*token).is_none_or(|target| !near_rect(&attempt.target, target))
            })
        })
        .map(|(token, _)| *token)
        .collect();
    for window in cancelled {
        fx.push(Effect::CancelFrame { window });
    }

    let mut round: Option<u64> = None;
    for (token, target) in &desired {
        if state.frames.in_flight() >= MAX_IN_FLIGHT_FRAMES {
            break;
        }
        if unsettled.as_ref().is_none_or(|apps| apps.contains(&token.application())) {
            continue;
        }
        // The layout keeps the slot of a window whose application Stache hid
        // (it is being shown again), but nothing can be written to it until it
        // reads visible. Its siblings are already planned around its slot.
        if state.windows.get(token).is_some_and(|window| window.observed.hidden) {
            continue;
        }
        // A window whose destroy is held back (its native tab group is
        // settling) keeps its slot but is gone: its token can never be written.
        if state
            .windows
            .get(token)
            .is_some_and(|window| window.departing.is_some() || window.suspended)
        {
            continue;
        }
        let Some(actual) = state.windows.get(token).and_then(|window| window.observed.frame) else {
            continue;
        };
        let mut wanted = needed(&actual, target);
        // A window being animated is in motion: its observed frame is an
        // intermediate position, so what the running animation was moving is
        // still wanted (A, B, A must bring it back, not skip it).
        if let Some(attempt) = state
            .frames
            .records
            .get(token)
            .and_then(|record| record.attempt.as_ref())
            .filter(|attempt| attempt.animated && attempt.in_flight())
        {
            wanted.position |= attempt.wanted.position;
            wanted.size |= attempt.wanted.size;
        }
        if wanted.is_empty() {
            state.frames.revealed.remove(token);
            if let Some(record) = state.frames.records.get_mut(token) {
                record.forced = false;
            }
            // Already where the one-shot frame asks: nothing to write, pin spent.
            state.frames.pinned.remove(token);
            continue;
        }
        let revealed = state.frames.revealed.contains(token);
        let record = state.frames.records.entry(*token).or_default();
        let external = match decide(record, target, &actual, revision, revealed) {
            Decision::Skip => continue,
            Decision::Issue { external } => external,
        };
        let plan_sequence = *round.get_or_insert_with(|| {
            if state.frames.plan_sequence <= state.frames.issued_plan {
                state.frames.plan_sequence = state.frames.issued_plan + 1;
            }
            state.frames.issued_plan = state.frames.plan_sequence;
            state.frames.plan_sequence
        });
        let record = state.frames.records.entry(*token).or_default();
        let target_changed = record
            .attempt
            .as_ref()
            .is_none_or(|attempt| !near_rect(&attempt.target, target));
        if external && record.settling > 0 {
            record.settling -= 1;
        } else if external {
            record.externals += 1;
        } else if target_changed || record.forced || revealed {
            record.externals = 0;
        }
        record.forced = false;
        record.settle_echoes = 0;
        let corrective = std::mem::take(&mut record.size_check) && !target_changed;
        if corrective {
            // One direct size-only write: the position already landed, and an
            // animation's intermediate frames could never prove a minimum.
            wanted.position = false;
            wanted.size = true;
            record.size_checked = Some(*target);
        }
        record.attempt = Some(FrameAttempt {
            plan_sequence,
            revision,
            target: *target,
            wanted,
            from: actual,
            animated: false,
            corrective,
            status: AttemptStatus::InFlight,
        });
        // A just-revealed window is written directly, whatever the config says.
        // An application in instant fallback for this kind of write (a move,
        // or a size change) gets one direct write per plan.
        let kind = crate::animation::MotionKind::of(wanted.size);
        let animate = !corrective
            && !state.frames.revealed.remove(token)
            && !state.animation_fallback.contains(&(token.application(), kind))
            && animations
                .as_ref()
                .is_some_and(|config| crate::animation::wants_animation(config, &actual, target));
        // The move is paced by the display that will contain the target; it
        // crosses displays when that differs from the one the window is on.
        let start_display = animate.then(|| state.display_of(*token)).flatten();
        let display = animate.then(|| target_display(state, *token, target)).flatten();
        let cross_display = matches!((start_display, display), (Some(a), Some(b)) if a != b);
        let refresh_hz = display
            .or(start_display)
            .and_then(|id| state.screens.iter().find(|screen| screen.id == id))
            .and_then(|screen| screen.refresh_hz);
        if let Some(attempt) =
            state.frames.records.get_mut(token).and_then(|record| record.attempt.as_mut())
        {
            attempt.animated = animate;
        }
        state.diagnostics.frame_attempts += 1;
        fx.push(Effect::WriteFrame {
            stamp,
            revision,
            plan_sequence,
            window: *token,
            target: *target,
            components: wanted,
            from: actual,
            animate,
            refresh_hz,
            display,
            cross_display,
        });
    }
    // A one-shot frame whose attempt already settled is spent.
    let records = &state.frames.records;
    state.frames.pinned.retain(|token, pin| {
        records
            .get(token)
            .and_then(|record| record.attempt.as_ref())
            .is_none_or(|attempt| !near_rect(&attempt.target, pin) || attempt.in_flight())
    });
}

// ----------------------------------------------------------------- inputs

/// Turns frame planning on or off. Turning it off forgets every attempt and
/// cancels the queued writes.
pub(super) fn set_enabled(state: &mut TilingState, enabled: bool, fx: &mut Vec<Effect>) {
    if state.frames.enabled == enabled {
        return;
    }
    state.frames.enabled = enabled;
    if !enabled {
        fx.extend(
            state
                .frames
                .records
                .keys()
                .map(|window| Effect::CancelFrame { window: *window }),
        );
        state.frames.records.clear();
        state.frames.desired.clear();
        state.frames.no_frame.clear();
        state.frames.revealed.clear();
        state.frames.last_visible.clear();
    }
}

/// A user command: every window may be attempted again, and inferred minimums
/// are forgotten (balance).
pub(super) fn relayout(state: &mut TilingState) {
    let tokens: Vec<WindowInstance> = state.windows.keys().copied().collect();
    for token in tokens {
        let record = state.frames.records.entry(token).or_default();
        record.forced = true;
        record.externals = 0;
        record.inferences = 0;
        record.minimum.clear_inferred();
    }
}

/// The displays changed: what an earlier attempt proved says nothing about the
/// new arrangement, so every window may be attempted again, and the next
/// [`DISPLAY_CHANGE_ATTEMPTS`] external changes of each are corrected too (the
/// system keeps moving windows after the change is reported).
pub(super) fn displays_changed(state: &mut TilingState) {
    let tokens: Vec<WindowInstance> = state.windows.keys().copied().collect();
    for token in tokens {
        let record = state.frames.records.entry(token).or_default();
        record.forced = true;
        record.externals = 0;
        record.settling = DISPLAY_CHANGE_ATTEMPTS;
    }
}

/// After a drag: windows whose frame no longer matches their planned frame may
/// be attempted again, once. Other windows are left alone, so a plain click
/// starts no plan.
pub(super) fn force_deviating(state: &mut TilingState, tokens: &[WindowInstance]) {
    for token in tokens {
        let deviates =
            state.windows.get(token).and_then(|w| w.observed.frame).is_some_and(|actual| {
                state
                    .frames
                    .desired
                    .get(token)
                    .is_some_and(|target| !near_rect(&actual, target))
            });
        if deviates {
            let record = state.frames.records.entry(*token).or_default();
            record.forced = true;
            record.externals = 0;
        }
    }
}

/// Gives each window one fresh external-change attempt (a lost observation
/// cannot tell what the user did).
pub(super) fn reset_externals(state: &mut TilingState, tokens: &[WindowInstance]) {
    for token in tokens {
        if let Some(record) = state.frames.records.get_mut(token) {
            record.externals = 0;
        }
    }
}

/// Forgets a window.
pub(super) fn forget(state: &mut TilingState, token: WindowInstance) {
    state.frames.records.remove(&token);
    state.frames.revealed.remove(&token);
    state.frames.desired.remove(&token);
    state.frames.pinned.remove(&token);
}

/// Moves the frame knowledge of `old` to `new`, the window that took its tab
/// group slot over: the learned minimum, the one-shot frame and the revealed
/// mark. An attempt still in flight stays with the dead token (its
/// acknowledgement is dropped as stale), so `new` is judged only by where it
/// actually is: nothing is written when it already sits on its target.
pub(super) fn transfer(state: &mut TilingState, old: WindowInstance, new: WindowInstance) {
    if let Some(mut record) = state.frames.records.remove(&old) {
        if record.attempt.as_ref().is_some_and(FrameAttempt::in_flight) {
            record.attempt = None;
        }
        state.frames.records.insert(new, record);
    }
    if let Some(pin) = state.frames.pinned.remove(&old) {
        state.frames.pinned.insert(new, pin);
    }
    if state.frames.revealed.remove(&old) {
        state.frames.revealed.insert(new);
    }
    if let Some(target) = state.frames.desired.remove(&old) {
        state.frames.desired.insert(new, target);
    }
}

/// Balance: the given windows may be attempted again and forget inferred
/// minimums.
pub(super) fn relayout_windows(state: &mut TilingState, tokens: &[WindowInstance]) {
    for token in tokens {
        let record = state.frames.records.entry(*token).or_default();
        record.forced = true;
        record.externals = 0;
        record.inferences = 0;
        record.minimum.clear_inferred();
    }
}

/// Forgets everything bound to the previous registry.
pub(super) fn reset(state: &mut TilingState) {
    state.frames.records.clear();
    state.frames.pinned.clear();
    state.frames.revealed.clear();
    state.frames.last_visible.clear();
    state.frames.desired.clear();
    state.frames.no_frame.clear();
}

/// Records the acknowledgement of a frame write. Only the attempt of exactly
/// this window, plan and revision can be settled.
pub(super) fn complete(
    state: &mut TilingState,
    window: WindowInstance,
    plan_sequence: u64,
    revision: u64,
    outcome: &WindowActionOutcome,
) {
    let Some(record) = state.frames.records.get_mut(&window) else {
        state.diagnostics.stale_frame_acks += 1;
        return;
    };
    let Some(attempt) = record.attempt.as_mut() else {
        state.diagnostics.stale_frame_acks += 1;
        return;
    };
    if attempt.plan_sequence != plan_sequence
        || attempt.revision != revision
        || !attempt.in_flight()
    {
        state.diagnostics.stale_frame_acks += 1;
        return;
    }
    let result = classify_frame(window, plan_sequence, attempt.target, attempt.wanted, outcome);
    match &result {
        OperationOutcome::Applied { .. } => state.diagnostics.frames_applied += 1,
        OperationOutcome::Partial { .. } => state.diagnostics.frames_partial += 1,
        OperationOutcome::Uncertain { .. } => state.diagnostics.frames_uncertain += 1,
        OperationOutcome::Failed { .. } => state.diagnostics.frames_failed += 1,
        OperationOutcome::NotAttempted { .. } | OperationOutcome::Stale { .. } => {
            state.diagnostics.frames_not_attempted += 1;
        }
        OperationOutcome::Enqueued => {}
    }
    let actual = result.actual();
    if attempt.wanted.size
        && let Some(actual) = actual
        && let Some(inferred) = infer_minimum(
            &attempt.target,
            &actual,
            record.minimum.reported,
            record.minimum.inferred,
        )
    {
        // A window moved and resized in one write, or animated through
        // intermediate frames, can read back a transient size. Only a size
        // the window still refuses on a direct size-only write is its
        // minimum; the corrective check is owed at most once per target.
        if attempt.corrective || (!attempt.wanted.position && !attempt.animated) {
            learn_minimum(&mut record.minimum, &mut record.inferences, inferred);
        } else if record.size_checked.is_none_or(|checked| !near_rect(&checked, &attempt.target)) {
            record.size_check = true;
        }
    }
    attempt.status = AttemptStatus::Settled(result);
    if attempt.animated {
        record.settle_echoes = MAX_SETTLE_ECHOES;
    }
    // A one-shot floating frame is spent once its attempt settled.
    if state
        .frames
        .pinned
        .get(&window)
        .is_some_and(|pin| near_rect(pin, &attempt.target))
    {
        state.frames.pinned.remove(&window);
    }
    if let Some(actual) = actual
        && let Some(observed) = state.windows.get_mut(&window)
    {
        // The readback is the confirmed actual frame, whatever was asked for.
        observed.observed.frame = Some(actual);
        // Our own write is the new baseline of a gesture in progress, so its
        // echo can never look like the user moving the window.
        super::drag::rebase(&mut state.drag, window, actual);
    }
}

fn learn_minimum(minimum: &mut MinimumConstraint, inferences: &mut u8, inferred: Size) {
    if *inferences < MAX_INFERENCES && minimum.inferred != Some(inferred) {
        minimum.inferred = Some(inferred);
        *inferences += 1;
    }
}

/// A window that took its planned size and later reads larger on its own (no
/// gesture in progress) is re-asserting a minimum it does not report (legacy
/// `detect_and_update_inferred_minimum` on resize events). It is learned, so
/// the next plan lays out around it instead of writing the rejected size back.
pub(super) fn observe_growth(state: &mut TilingState, token: WindowInstance, frame: Option<Rect>) {
    let Some(frame) = frame else { return };
    // A gesture in progress, or no passive observer at all (a user resize
    // would be invisible), means the growth may be the user's: learn nothing.
    if state.drag.gesture.is_some() || state.drag.degraded {
        return;
    }
    // The usable area of the window's display: a window at least that large
    // in a dimension is zoomed or maximized, not re-asserting a minimum.
    let usable = state
        .display_of(token)
        .and_then(|display| display_area(state, display).ok())
        .map(|(usable, _)| usable);
    let Some(record) = state.frames.records.get_mut(&token) else {
        return;
    };
    let Some(attempt) = record.attempt.as_ref() else {
        return;
    };
    let AttemptStatus::Settled(OperationOutcome::Applied { .. }) = &attempt.status else {
        return;
    };
    if !attempt.wanted.size || record.size_check {
        return;
    }
    if let Some(inferred) = infer_minimum(
        &attempt.target,
        &frame,
        record.minimum.reported,
        record.minimum.inferred,
    ) && !usable.is_some_and(|usable| {
        inferred.width >= usable.width - INFERENCE_TOLERANCE
            || inferred.height >= usable.height - INFERENCE_TOLERANCE
    }) {
        learn_minimum(&mut record.minimum, &mut record.inferences, inferred);
    }
}

/// Whether a read of `token` is a leftover echo of an animation that settled:
/// its frame differs from the settled one while the bounded echo budget lasts.
/// The caller then keeps the confirmed frame instead of the intermediate one.
pub(super) fn absorb_echo(
    state: &mut TilingState,
    token: WindowInstance,
    frame: Option<Rect>,
) -> bool {
    let Some(frame) = frame else { return false };
    let Some(record) = state.frames.records.get_mut(&token) else {
        return false;
    };
    let Some(attempt) = record.attempt.as_ref() else {
        return false;
    };
    if record.settle_echoes == 0 || attempt.in_flight() || near_rect(&frame, &attempt.actual()) {
        return false;
    }
    record.settle_echoes -= 1;
    state.diagnostics.animation_echoes_ignored += 1;
    true
}

/// A frame write that was never queued (the effect queue was full).
pub(super) fn rejected(
    state: &mut TilingState,
    window: WindowInstance,
    plan_sequence: u64,
    reason: NotAttemptedReason,
) {
    let Some(attempt) = state
        .frames
        .records
        .get_mut(&window)
        .and_then(|record| record.attempt.as_mut())
        .filter(|attempt| attempt.plan_sequence == plan_sequence && attempt.in_flight())
    else {
        return;
    };
    attempt.status =
        AttemptStatus::Settled(OperationOutcome::NotAttempted { reason, window, plan_sequence });
    state.diagnostics.frames_not_attempted += 1;
}

/// A read-only view of one window's frame state for snapshots.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameStatus {
    /// The window.
    pub window: WindowInstance,
    /// The desired frame of the latest plan.
    pub desired: Option<Rect>,
    /// The confirmed actual frame (readback, else the last observation).
    pub actual: Option<Rect>,
    /// The plan sequence of the latest attempt.
    pub plan_sequence: Option<u64>,
    /// An attempt is waiting for its acknowledgement.
    pub in_flight: bool,
    /// The outcome of the latest settled attempt.
    pub outcome: Option<OperationOutcome>,
    /// The effective minimum size.
    pub minimum: Option<Size>,
}

pub(super) fn statuses(state: &TilingState) -> Vec<FrameStatus> {
    state
        .frames
        .records
        .iter()
        .map(|(token, record)| FrameStatus {
            window: *token,
            desired: state.frames.desired.get(token).copied(),
            actual: state.windows.get(token).and_then(|window| window.observed.frame),
            plan_sequence: record.attempt.as_ref().map(|attempt| attempt.plan_sequence),
            in_flight: record.attempt.as_ref().is_some_and(FrameAttempt::in_flight),
            outcome: record.attempt.as_ref().and_then(|attempt| match &attempt.status {
                AttemptStatus::Settled(outcome) => Some(outcome.clone()),
                AttemptStatus::InFlight => None,
            }),
            minimum: record.minimum.effective(),
        })
        .collect()
}

/// The display containing `target`, else the window's workspace screen.
fn target_display(
    state: &TilingState,
    token: stache_macos::windows::WindowInstance,
    target: &Rect,
) -> Option<stache_macos::DisplayId> {
    state
        .layout
        .display_for_ax_rect(stache_macos::applications::DisplayRect::new(
            target.x,
            target.y,
            target.width,
            target.height,
        ))
        .or_else(|| {
            let id = state.windows.get(&token)?.desired.workspace?;
            state.workspaces.iter().find(|ws| ws.id == id).map(|ws| ws.screen)
        })
}
