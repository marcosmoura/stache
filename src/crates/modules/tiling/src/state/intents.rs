//! Pure handlers for workspace and window intents.
//!
//! Legacy `actor/handlers/{workspace,focus,window_move,layout,preset,resize}.rs`
//! become the functions below. Each returns an [`IntentOutcome`]: `Applied`
//! changed desired state, `NoOp` was admitted silently (the user sees success)
//! and `Rejected` names a refusal. No handler calls native code; focus is a
//! typed [`Effect::Focus`] and frames stay with the planner.

use stache_macos::windows::WindowInstance;

use super::transition::on_focused;
use super::{
    Effect, FOCUS_HISTORY_CAPACITY, FocusState, TilingState, WORKSPACE_HISTORY_CAPACITY,
    WorkspaceId, frames,
};
use crate::config::LayoutType;
use crate::geometry::Rect;
use crate::intent::{
    Cycle, FocusDirection, FocusTarget, Intent, IntentNoOp, IntentOutcome, IntentRecord,
    IntentRejection, ResizeDimension, ScreenTarget, SwapDirection,
};
use crate::layout::{
    MAX_GRID_TILES, clamp_master_ratio, is_dwindle_split_horizontal, is_split_horizontal,
    swap_windows,
};
use crate::minimum_size::{
    Change, MinimumRequest, Size, default_ratios, guarded_ratio_change, resize_cumulative,
};
use crate::presets::{
    FloatingContext, FloatingTarget, FloatingTrigger, PositionDecision, PositionNoOp,
    PresetOutcome, PresetRequest, center_frame, choose_floating_target, default_position,
    resolve_preset,
};
use crate::workspaces::resolve_target;

/// Applies one intent and records what became of it.
pub(super) fn apply(state: &mut TilingState, intent: Intent, fx: &mut Vec<Effect>) {
    let outcome = dispatch(state, &intent, fx);
    match &outcome {
        IntentOutcome::Applied => state.diagnostics.intents_applied += 1,
        IntentOutcome::NoOp(_) => state.diagnostics.intents_noop += 1,
        IntentOutcome::Rejected(_) => state.diagnostics.intents_rejected += 1,
    }
    state.last_intent = Some(IntentRecord {
        sequence: state.sequence,
        intent,
        outcome,
    });
}

fn dispatch(state: &mut TilingState, intent: &Intent, fx: &mut Vec<Effect>) -> IntentOutcome {
    match intent {
        Intent::FocusWorkspace(name) => focus_workspace(state, name, fx),
        Intent::CycleWorkspace(cycle) => cycle_workspace(state, *cycle, fx),
        Intent::FocusWindow(target) => focus_window(state, target, fx),
        Intent::SwapWindow(direction) => swap(state, *direction),
        Intent::ToggleFloating => toggle_floating(state),
        Intent::Preset(name) => preset(state, name),
        Intent::Resize { dimension, amount } => resize(state, *dimension, *amount),
        Intent::Balance => balance(state),
        Intent::SetLayout(layout) => set_layout(state, *layout),
        Intent::CycleLayout => cycle_layout(state),
        Intent::SendWindowToWorkspace { window, workspace } => {
            send_window_to_workspace(state, *window, workspace, fx)
        }
        Intent::SendWindowToScreen(target) => send_window_to_screen(state, target, fx),
        Intent::SendWorkspaceToScreen(target) => send_workspace_to_screen(state, target, fx),
    }
}

// ------------------------------------------------------------------ helpers

const fn noop(why: IntentNoOp) -> IntentOutcome { IntentOutcome::NoOp(why) }

const fn reject(why: IntentRejection) -> IntentOutcome { IntentOutcome::Rejected(why) }

pub(super) fn focused_index(state: &TilingState) -> Option<usize> {
    state
        .focus
        .workspace
        .and_then(|id| state.workspaces.iter().position(|ws| ws.id == id))
        .or_else(|| state.workspaces.iter().position(|ws| ws.focused))
}

/// Windows that can take focus or be swapped: not an inactive tab, fullscreen,
/// minimized or hidden. Floating windows count.
fn focusable(state: &TilingState, index: usize) -> Vec<WindowInstance> {
    state.workspaces[index]
        .order
        .iter()
        .filter(|token| {
            state.windows.get(token).is_some_and(|w| {
                let f = state.flags_of(w);
                !(f.inactive_tab || f.fullscreen || f.minimized || f.hidden)
            })
        })
        .copied()
        .collect()
}

/// Windows the layout tiles: focusable and not explicitly floating, with the
/// Grid cap applied.
pub(super) fn tiled(state: &TilingState, index: usize) -> Vec<WindowInstance> {
    let mut tiles: Vec<WindowInstance> = focusable(state, index)
        .into_iter()
        .filter(|token| state.windows.get(token).is_some_and(|w| !w.desired.floating))
        .collect();
    if state.workspaces[index].layout == LayoutType::Grid {
        tiles.truncate(MAX_GRID_TILES);
    }
    tiles
}

fn push_workspace_history(state: &mut TilingState, id: WorkspaceId) {
    state.workspace_history.retain(|held| *held != id);
    state.workspace_history.push_back(id);
    while state.workspace_history.len() > WORKSPACE_HISTORY_CAPACITY {
        state.workspace_history.pop_front();
    }
}

/// Emits a focus request when the runtime may act. The target application must
/// be visible first: slice B's unhide precedes the native focus.
fn request_focus(state: &mut TilingState, window: WindowInstance, fx: &mut Vec<Effect>) {
    match state.stamp {
        Some(stamp) if state.frames.enabled => fx.push(Effect::Focus { stamp, window }),
        _ => state.diagnostics.focus_unauthorized += 1,
    }
}

/// Makes a workspace visible and focused on its display; the focused window is
/// cleared until the caller picks one.
fn show_workspace(state: &mut TilingState, id: WorkspaceId) {
    let Some(screen) = state.workspaces.iter().find(|ws| ws.id == id).map(|ws| ws.screen) else {
        return;
    };
    if let Some(previous) = state.focus.workspace.filter(|previous| *previous != id) {
        push_workspace_history(state, previous);
    }
    for workspace in &mut state.workspaces {
        if workspace.screen == screen {
            workspace.visible = workspace.id == id;
        }
        workspace.focused = workspace.id == id;
    }
    state.focus = FocusState {
        window: None,
        workspace: Some(id),
        screen: Some(screen),
    };
}

/// The window a workspace should focus on becoming current: its last focused
/// window, else the first focusable one (legacy).
fn workspace_target(state: &TilingState, index: usize) -> Option<WindowInstance> {
    let workspace = &state.workspaces[index];
    let candidates = focusable(state, index);
    workspace
        .focused_window
        .filter(|token| candidates.contains(token))
        .or_else(|| candidates.first().copied())
}

fn switch_to(state: &mut TilingState, id: WorkspaceId, fx: &mut Vec<Effect>) {
    show_workspace(state, id);
    let Some(index) = state.workspaces.iter().position(|ws| ws.id == id) else {
        return;
    };
    if let Some(token) = workspace_target(state, index) {
        on_focused(state, token);
        request_focus(state, token, fx);
    }
}

// ---------------------------------------------------------------- workspaces

fn focus_workspace(state: &mut TilingState, name: &str, fx: &mut Vec<Effect>) -> IntentOutcome {
    let Some(workspace) = state.workspaces.iter().find(|ws| ws.name == name) else {
        return reject(IntentRejection::UnknownWorkspace);
    };
    if workspace.visible && workspace.focused {
        return noop(IntentNoOp::Unchanged);
    }
    let id = workspace.id;
    switch_to(state, id, fx);
    IntentOutcome::Applied
}

fn cycle_workspace(state: &mut TilingState, cycle: Cycle, fx: &mut Vec<Effect>) -> IntentOutcome {
    let Some(index) = focused_index(state) else {
        return noop(IntentNoOp::NoFocusedWorkspace);
    };
    let screen = state.workspaces[index].screen;
    let on_screen: Vec<WorkspaceId> = state
        .workspaces
        .iter()
        .filter(|ws| ws.screen == screen)
        .map(|ws| ws.id)
        .collect();
    if on_screen.len() < 2 {
        return noop(IntentNoOp::TooFewWindows);
    }
    let current = on_screen.iter().position(|id| *id == state.workspaces[index].id).unwrap_or(0);
    let next = step(current, on_screen.len(), cycle);
    switch_to(state, on_screen[next], fx);
    IntentOutcome::Applied
}

const fn step(current: usize, len: usize, cycle: Cycle) -> usize {
    match cycle {
        Cycle::Next => (current + 1) % len,
        Cycle::Previous => {
            if current == 0 {
                len - 1
            } else {
                current - 1
            }
        }
    }
}

// ------------------------------------------------------------------- focus

fn focus_window(
    state: &mut TilingState,
    target: &FocusTarget,
    fx: &mut Vec<Effect>,
) -> IntentOutcome {
    match target {
        FocusTarget::Window(token) => focus_exact(state, *token, fx),
        FocusTarget::Direction(direction) => focus_direction(state, *direction, fx),
    }
}

fn focus_exact(
    state: &mut TilingState,
    token: WindowInstance,
    fx: &mut Vec<Effect>,
) -> IntentOutcome {
    let Some(workspace) = state.windows.get(&token).and_then(|w| w.desired.workspace) else {
        return reject(IntentRejection::UnknownWindow);
    };
    if state.focus.window == Some(token) {
        return noop(IntentNoOp::Unchanged);
    }
    let visible = state.workspaces.iter().any(|ws| ws.id == workspace && ws.visible);
    if !visible {
        show_workspace(state, workspace);
    }
    on_focused(state, token);
    request_focus(state, token, fx);
    IntentOutcome::Applied
}

fn focus_direction(
    state: &mut TilingState,
    direction: FocusDirection,
    fx: &mut Vec<Effect>,
) -> IntentOutcome {
    let Some(index) = focused_index(state) else {
        return noop(IntentNoOp::NoFocusedWorkspace);
    };
    let candidates = focusable(state, index);
    if candidates.is_empty() {
        return noop(IntentNoOp::TooFewWindows);
    }
    if candidates.len() == 1 && state.focus.window == Some(candidates[0]) {
        return noop(IntentNoOp::TooFewWindows);
    }
    let Some(target) = pick(state, &candidates, direction) else {
        return noop(if state.focus.window.is_none() {
            IntentNoOp::NoFocusedWindow
        } else {
            IntentNoOp::NoCandidate
        });
    };
    on_focused(state, target);
    request_focus(state, target, fx);
    IntentOutcome::Applied
}

/// The target of a direction or cycle step from the focused window.
fn pick(
    state: &TilingState,
    candidates: &[WindowInstance],
    direction: FocusDirection,
) -> Option<WindowInstance> {
    let current = state.focus.window.and_then(|token| candidates.iter().position(|c| *c == token));
    match direction {
        FocusDirection::Next => {
            Some(candidates[step(current.unwrap_or(0), candidates.len(), Cycle::Next)])
        }
        FocusDirection::Previous => {
            Some(candidates[step(current.unwrap_or(0), candidates.len(), Cycle::Previous)])
        }
        spatial => {
            let from = state.windows.get(&state.focus.window?)?.observed.frame?;
            spatial_target(state, &from, spatial, candidates)
        }
    }
}

/// The nearest window in a spatial direction, preferring aligned ones (legacy
/// `find_window_in_direction`).
fn spatial_target(
    state: &TilingState,
    from: &Rect,
    direction: FocusDirection,
    candidates: &[WindowInstance],
) -> Option<WindowInstance> {
    let (fx, fy) = (from.x + from.width / 2.0, from.y + from.height / 2.0);
    let mut best: Option<(WindowInstance, f64)> = None;
    for token in candidates {
        let Some(frame) = state.windows.get(token).and_then(|w| w.observed.frame) else {
            continue;
        };
        if (frame.x - from.x).abs() < 1.0
            && (frame.y - from.y).abs() < 1.0
            && (frame.width - from.width).abs() < 1.0
            && (frame.height - from.height).abs() < 1.0
        {
            continue;
        }
        let (cx, cy) = (frame.x + frame.width / 2.0, frame.y + frame.height / 2.0);
        let valid = match direction {
            FocusDirection::Up => cy < fy,
            FocusDirection::Down => cy > fy,
            FocusDirection::Left => cx < fx,
            FocusDirection::Right => cx > fx,
            _ => false,
        };
        if !valid {
            continue;
        }
        let (dx, dy) = (cx - fx, cy - fy);
        let distance = dy.mul_add(dy, dx * dx);
        let penalty = match direction {
            FocusDirection::Up | FocusDirection::Down => dx.abs() * 2.0,
            _ => dy.abs() * 2.0,
        };
        let weighted = penalty.mul_add(penalty, distance);
        if best.is_none_or(|(_, current)| weighted < current) {
            best = Some((*token, weighted));
        }
    }
    best.map(|(token, _)| token)
}

fn swap(state: &mut TilingState, direction: SwapDirection) -> IntentOutcome {
    let Some(index) = focused_index(state) else {
        return noop(IntentNoOp::NoFocusedWorkspace);
    };
    let Some(focused) = state.focus.window else {
        return noop(IntentNoOp::NoFocusedWindow);
    };
    let candidates = focusable(state, index);
    if candidates.len() < 2 || !candidates.contains(&focused) {
        return noop(IntentNoOp::TooFewWindows);
    }
    let Some(target) = pick(state, &candidates, direction.as_focus()).filter(|t| *t != focused)
    else {
        return noop(IntentNoOp::NoCandidate);
    };
    if swap_windows(&mut state.workspaces[index].order, &focused, &target) {
        // Focus stays on the window that was focused.
        state.workspaces[index].focused_window = Some(focused);
        IntentOutcome::Applied
    } else {
        noop(IntentNoOp::Unchanged)
    }
}

// ----------------------------------------------------------------- floating

/// The index of `token` among the workspace's eligible windows.
fn eligible_index(state: &TilingState, index: usize, token: WindowInstance) -> Option<usize> {
    state.workspaces[index]
        .order
        .iter()
        .filter(|held| {
            state.windows.get(held).is_some_and(|w| {
                let f = state.flags_of(w);
                !(f.explicit_floating || f.inactive_tab || f.fullscreen || f.minimized || f.hidden)
            })
        })
        .position(|held| *held == token)
}

/// Pins the centered frame of `token` on its own display, once.
pub(super) fn center_once(state: &mut TilingState, token: WindowInstance) {
    let Some(observed) = state.windows.get(&token).and_then(|w| w.observed.frame) else {
        return;
    };
    let Some(display) = state.display_of(token) else { return };
    let Ok((usable, _)) = frames::display_area(state, display) else {
        return;
    };
    if let Ok(frame) = center_frame(&observed, &usable) {
        state.frames.pinned.insert(token, frame);
        if let Some(window) = state.windows.get_mut(&token) {
            window.desired.positioned = true;
        }
    }
}

/// Default positioning for a window that just became floating by creation or
/// an explicit toggle. Grid overflow never gets it.
pub(super) fn position_new_floating(
    state: &mut TilingState,
    token: WindowInstance,
    trigger: FloatingTrigger,
) {
    let Some(window) = state.windows.get(&token) else {
        return;
    };
    let Some(index) = window
        .desired
        .workspace
        .and_then(|id| state.workspaces.iter().position(|ws| ws.id == id))
    else {
        return;
    };
    let layout = state.workspaces[index].layout;
    let overflow = trigger == FloatingTrigger::NewWindow
        && layout == LayoutType::Grid
        && eligible_index(state, index, token).is_some_and(|i| i >= MAX_GRID_TILES);
    let context = FloatingContext {
        trigger,
        layout,
        explicit_floating: window.desired.floating,
        grid_overflow: overflow,
        already_positioned: window.desired.positioned,
    };
    let setting = state
        .config
        .as_deref()
        .map_or_else(Default::default, |c| c.floating.default_position);
    if default_position(setting, &context) == PositionDecision::Center {
        center_once(state, token);
    }
}

fn toggle_floating(state: &mut TilingState) -> IntentOutcome {
    let Some(index) = focused_index(state) else {
        return noop(IntentNoOp::NoFocusedWorkspace);
    };
    let Some(token) = state.focus.window.filter(|t| state.workspaces[index].order.contains(t))
    else {
        return noop(IntentNoOp::NoFocusedWindow);
    };
    let Some(window) = state.windows.get_mut(&token) else {
        return reject(IntentRejection::UnknownWindow);
    };
    window.desired.floating = !window.desired.floating;
    if window.desired.floating {
        position_new_floating(state, token, FloatingTrigger::ExplicitFloatToggle);
    } else {
        // Tiled again: a later float is a fresh explicit event.
        window.desired.positioned = false;
        state.frames.pinned.remove(&token);
    }
    IntentOutcome::Applied
}

fn preset(state: &mut TilingState, name: &str) -> IntentOutcome {
    let Some(index) = focused_index(state) else {
        return noop(IntentNoOp::NoFocusedWorkspace);
    };
    let workspace = &state.workspaces[index];
    let (layout, screen) = (workspace.layout, workspace.screen);
    let token = state.focus.window.filter(|t| workspace.order.contains(t));
    let eligible = token.is_some_and(|t| {
        state.windows.get(&t).is_some_and(|w| {
            let f = state.flags_of(w);
            !(f.inactive_tab || f.fullscreen || f.minimized || f.hidden)
        })
    });
    let area = frames::display_area(state, screen);
    let presets = state.config.as_deref().map(|c| c.floating.presets.clone()).unwrap_or_default();
    let (usable, gaps) = area.unwrap_or((Rect::new(0.0, 0.0, 0.0, 0.0), crate::layout::Gaps::ZERO));
    let outcome = resolve_preset(&PresetRequest {
        presets: &presets,
        name,
        layout,
        has_focused_window: token.is_some(),
        window_eligible: eligible,
        usable,
        inner_h: gaps.inner_h,
        inner_v: gaps.inner_v,
    });
    match outcome {
        PresetOutcome::NoOp(why) => noop(IntentNoOp::Preset(why)),
        PresetOutcome::Infeasible(error) => reject(IntentRejection::Geometry(error)),
        PresetOutcome::Target(_) if area.is_err() => reject(IntentRejection::NoDisplayArea),
        PresetOutcome::Target(_) => {
            // An eligible explicit preset outranks default positioning.
            let chosen = choose_floating_target(
                Some(outcome),
                PositionDecision::NoOp(PositionNoOp::AlreadyPositioned),
            );
            match (chosen, token) {
                (FloatingTarget::Preset(frame), Some(token)) => {
                    state.frames.pinned.insert(token, frame);
                    if let Some(window) = state.windows.get_mut(&token) {
                        window.desired.positioned = true;
                    }
                    IntentOutcome::Applied
                }
                _ => noop(IntentNoOp::Unchanged),
            }
        }
    }
}

// ------------------------------------------------------------------- layout

fn set_layout(state: &mut TilingState, layout: LayoutType) -> IntentOutcome {
    let Some(index) = focused_index(state) else {
        return noop(IntentNoOp::NoFocusedWorkspace);
    };
    apply_layout(state, index, layout)
}

fn apply_layout(state: &mut TilingState, index: usize, layout: LayoutType) -> IntentOutcome {
    let default = state.config.as_deref().map_or_else(LayoutType::default, |c| c.default_layout);
    let workspace = &mut state.workspaces[index];
    if workspace.ratios.set_layout(workspace.layout, layout) == Change::Unchanged {
        return noop(IntentNoOp::Unchanged);
    }
    workspace.layout = layout;
    // Back at the configured layout the user override is gone.
    workspace.layout_overridden = layout != workspace.configured_layout.unwrap_or(default);
    IntentOutcome::Applied
}

fn cycle_layout(state: &mut TilingState) -> IntentOutcome {
    let Some(index) = focused_index(state) else {
        return noop(IntentNoOp::NoFocusedWorkspace);
    };
    let next = match state.workspaces[index].layout {
        LayoutType::Floating => LayoutType::Dwindle,
        LayoutType::Dwindle => LayoutType::Monocle,
        LayoutType::Monocle => LayoutType::Master,
        LayoutType::Master => LayoutType::Split,
        LayoutType::Split => LayoutType::SplitVertical,
        LayoutType::SplitVertical => LayoutType::SplitHorizontal,
        LayoutType::SplitHorizontal => LayoutType::Grid,
        LayoutType::Grid => LayoutType::Floating,
    };
    apply_layout(state, index, next)
}

fn balance(state: &mut TilingState) -> IntentOutcome {
    let Some(index) = focused_index(state) else {
        return noop(IntentNoOp::NoFocusedWorkspace);
    };
    let tokens: Vec<WindowInstance> = state.workspaces[index].order.clone();
    let any_inferred = tokens
        .iter()
        .any(|t| state.frames.records.get(t).is_some_and(|r| r.minimum.inferred.is_some()));
    if state.workspaces[index].ratios.balance(any_inferred) == Change::Unchanged {
        return noop(IntentNoOp::Unchanged);
    }
    frames::relayout_windows(state, &tokens);
    IntentOutcome::Applied
}

// ------------------------------------------------------------------- resize

fn resize(state: &mut TilingState, dimension: ResizeDimension, amount: i32) -> IntentOutcome {
    if amount == 0 {
        return noop(IntentNoOp::Unchanged);
    }
    let Some(index) = focused_index(state) else {
        return noop(IntentNoOp::NoFocusedWorkspace);
    };
    let layout = state.workspaces[index].layout;
    if matches!(layout, LayoutType::Floating | LayoutType::Monocle) {
        return noop(IntentNoOp::LayoutIgnoresCommand);
    }
    let ids = tiled(state, index);
    if ids.len() < 2 {
        return noop(IntentNoOp::TooFewWindows);
    }
    let Some(position) = state.focus.window.and_then(|t| ids.iter().position(|id| *id == t)) else {
        return noop(IntentNoOp::NoFocusedWindow);
    };
    let screen = state.workspaces[index].screen;
    let Ok((area, gaps)) = frames::display_area(state, screen) else {
        return reject(IntentRejection::NoDisplayArea);
    };
    let landscape = area.width >= area.height;
    let delta = f64::from(amount)
        / match dimension {
            ResizeDimension::Width => area.width,
            ResizeDimension::Height => area.height,
        };
    let last = ids.len() - 1;
    // The ratio index to move and the signed delta, per layout (legacy).
    let target = match layout {
        LayoutType::Master => {
            return resize_master(state, index, dimension, position, delta, landscape);
        }
        LayoutType::Dwindle => {
            if position >= last {
                Some((position - 1, -delta))
            } else {
                let horizontal = is_dwindle_split_horizontal(position, landscape);
                matches!(
                    (dimension, horizontal),
                    (ResizeDimension::Width, true) | (ResizeDimension::Height, false)
                )
                .then_some((position, delta))
            }
        }
        LayoutType::Grid => (dimension == ResizeDimension::Width).then_some((0, delta)),
        _ => {
            let horizontal = is_split_horizontal(layout, landscape);
            if matches!(
                (dimension, horizontal),
                (ResizeDimension::Width, true) | (ResizeDimension::Height, false)
            ) {
                Some(if position >= last {
                    (position - 1, -delta)
                } else {
                    (position, delta)
                })
            } else {
                None
            }
        }
    };
    let Some((ratio_index, signed)) = target else {
        return noop(IntentNoOp::LayoutIgnoresCommand);
    };
    apply_ratio(state, index, &ids, (area, gaps), (ratio_index, signed))
}

/// Moves one ratio of the workspace within every window's minimum size.
fn apply_ratio(
    state: &mut TilingState,
    index: usize,
    ids: &[WindowInstance],
    (area, gaps): (Rect, crate::layout::Gaps),
    (ratio_index, signed): (usize, f64),
) -> IntentOutcome {
    let layout = state.workspaces[index].layout;
    let landscape = area.width >= area.height;
    let defaults = default_ratios(layout, ids.len());
    let stored = &state.workspaces[index].ratios.split_ratios;
    let ratios = if stored.len() == defaults.len() {
        stored.clone()
    } else {
        defaults
    };
    let minimum_of = |token: &WindowInstance| -> Option<Size> {
        state.frames.records.get(token).and_then(|r| r.minimum.effective())
    };
    let next = if matches!(layout, LayoutType::Dwindle | LayoutType::Grid) {
        guarded_ratio_change(
            &MinimumRequest {
                layout,
                ids,
                usable: area,
                gaps,
                ratios: &ratios,
                minimum_of: &minimum_of,
            },
            ratio_index,
            signed,
        )
    } else {
        let total = if is_split_horizontal(layout, landscape) {
            area.width
        } else {
            area.height
        };
        let minimums: Vec<f64> = ids
            .iter()
            .map(|t| {
                minimum_of(t).map_or(0.05, |size| {
                    let points = if is_split_horizontal(layout, landscape) {
                        size.width
                    } else {
                        size.height
                    };
                    (points / total).max(0.05)
                })
            })
            .collect();
        let mut proposed = ratios.clone();
        resize_cumulative(&mut proposed, ratio_index, signed, &minimums);
        (proposed != ratios).then_some(proposed)
    };
    match next {
        Some(next) => {
            state.workspaces[index].ratios.split_ratios = next;
            IntentOutcome::Applied
        }
        None => noop(IntentNoOp::Unchanged),
    }
}

/// What a user drag-resize did to the workspace ratios.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UserResize {
    /// A ratio moved.
    Changed,
    /// The layout has a ratio for it but the guards (minimum sizes, clamps)
    /// left it where it was.
    Unchanged,
    /// This layout or geometry has no ratio to move; the caller replans only.
    NoRatio,
}

/// Moves the ratio a user's drag-resize of `token` implies (legacy
/// `on_user_resize_completed`). `ids` are the workspace's tiles in order.
pub(super) fn apply_user_resize(
    state: &mut TilingState,
    index: usize,
    ids: &[WindowInstance],
    token: WindowInstance,
    (old, new): (&Rect, &Rect),
) -> UserResize {
    use crate::config::MasterPosition;
    use crate::drag::{RatioMove, ResizeContext, ratio_move};
    let layout = state.workspaces[index].layout;
    let Some(position) = ids.iter().position(|id| *id == token) else {
        return UserResize::NoRatio;
    };
    let screen = state.workspaces[index].screen;
    let Ok((area, gaps)) = frames::display_area(state, screen) else {
        return UserResize::NoRatio;
    };
    let landscape = area.width >= area.height;
    let master = state.config.as_deref().map(|c| c.master.position).unwrap_or_default();
    let resolved = match master {
        MasterPosition::Auto if landscape => MasterPosition::Left,
        MasterPosition::Auto => MasterPosition::Top,
        other => other,
    };
    let context = ResizeContext {
        layout,
        position,
        count: ids.len(),
        width: area.width,
        height: area.height,
        master_uses_width: matches!(resolved, MasterPosition::Left | MasterPosition::Right),
    };
    let outcome = match ratio_move(&context, old, new) {
        RatioMove::Master { dimension, delta } => {
            resize_master(state, index, dimension, position, delta, landscape)
        }
        RatioMove::Split { index: ratio_index, delta } => {
            apply_ratio(state, index, ids, (area, gaps), (ratio_index, delta))
        }
        RatioMove::Relayout | RatioMove::Nothing => return UserResize::NoRatio,
    };
    if outcome == IntentOutcome::Applied {
        UserResize::Changed
    } else {
        UserResize::Unchanged
    }
}

fn resize_master(
    state: &mut TilingState,
    index: usize,
    dimension: ResizeDimension,
    position: usize,
    delta: f64,
    landscape: bool,
) -> IntentOutcome {
    use crate::config::MasterPosition;
    let master = state.config.as_deref().map(|c| c.master.clone()).unwrap_or_default();
    let resolved = match master.position {
        MasterPosition::Auto if landscape => MasterPosition::Left,
        MasterPosition::Auto => MasterPosition::Top,
        other => other,
    };
    let uses_width = matches!(resolved, MasterPosition::Left | MasterPosition::Right);
    if !matches!(
        (dimension, uses_width),
        (ResizeDimension::Width, true) | (ResizeDimension::Height, false)
    ) {
        return noop(IntentNoOp::LayoutIgnoresCommand);
    }
    // Growing the master window grows the master share; a stack window shrinks it.
    let signed = if position == 0 { delta } else { -delta };
    let configured = clamp_master_ratio(f64::from(master.ratio) / 100.0);
    match state.workspaces[index].ratios.resize_master(configured, signed) {
        Change::Changed => IntentOutcome::Applied,
        Change::Unchanged => noop(IntentNoOp::Unchanged),
    }
}

// --------------------------------------------------------------------- send

/// Moves `token` to `destination` (appended, legacy) and repairs focus.
fn move_window(
    state: &mut TilingState,
    token: WindowInstance,
    destination: WorkspaceId,
    fx: &mut Vec<Effect>,
) {
    let Some(source) = state.windows.get(&token).and_then(|w| w.desired.workspace) else {
        return;
    };
    let was_focused = state.focus.window == Some(token);
    if let Some(workspace) = state.workspaces.iter_mut().find(|ws| ws.id == source) {
        workspace.order.retain(|held| *held != token);
        if workspace.focused_window == Some(token) {
            workspace.focused_window = state
                .focus_history
                .iter()
                .rev()
                .find(|held| **held != token && workspace.order.contains(held))
                .copied();
        }
    }
    if let Some(workspace) = state.workspaces.iter_mut().find(|ws| ws.id == destination) {
        workspace.order.push(token);
    }
    if let Some(window) = state.windows.get_mut(&token) {
        window.desired.workspace = Some(destination);
    }
    state.frames.pinned.remove(&token);
    if !was_focused {
        return;
    }
    // Focus stays in the workspace it was in, on its next window.
    let replacement = state
        .workspaces
        .iter()
        .find(|ws| ws.id == source)
        .and_then(|ws| ws.focused_window.or_else(|| ws.order.first().copied()));
    state.focus.window = replacement;
    if let Some(next) = replacement {
        state.focus_history.retain(|held| *held != next);
        state.focus_history.push_back(next);
        while state.focus_history.len() > FOCUS_HISTORY_CAPACITY {
            state.focus_history.pop_front();
        }
        if let Some(ws) = state.workspaces.iter_mut().find(|ws| ws.id == source) {
            ws.focused_window = Some(next);
        }
        request_focus(state, next, fx);
    }
}

fn send_window_to_workspace(
    state: &mut TilingState,
    window: Option<WindowInstance>,
    name: &str,
    fx: &mut Vec<Effect>,
) -> IntentOutcome {
    let Some(destination) = state.workspaces.iter().find(|ws| ws.name == name).map(|ws| ws.id)
    else {
        return reject(IntentRejection::UnknownWorkspace);
    };
    let Some(token) = window.or(state.focus.window) else {
        return noop(IntentNoOp::NoFocusedWindow);
    };
    let Some(current) = state.windows.get(&token).map(|w| w.desired.workspace) else {
        return reject(IntentRejection::UnknownWindow);
    };
    if current == Some(destination) {
        return noop(IntentNoOp::Unchanged);
    }
    move_window(state, token, destination, fx);
    IntentOutcome::Applied
}

fn send_window_to_screen(
    state: &mut TilingState,
    target: &ScreenTarget,
    fx: &mut Vec<Effect>,
) -> IntentOutcome {
    let Some(screen) = resolve_target(&state.screens, target) else {
        return reject(IntentRejection::UnknownScreen);
    };
    let Some(token) = state.focus.window else {
        return noop(IntentNoOp::NoFocusedWindow);
    };
    let Some(current) = state.windows.get(&token).and_then(|w| w.desired.workspace) else {
        return reject(IntentRejection::UnknownWindow);
    };
    if state.workspaces.iter().any(|ws| ws.id == current && ws.screen == screen) {
        return noop(IntentNoOp::Unchanged);
    }
    let Some(destination) = state
        .workspaces
        .iter()
        .find(|ws| ws.screen == screen && ws.visible)
        .map(|ws| ws.id)
    else {
        return reject(IntentRejection::NoVisibleWorkspace);
    };
    move_window(state, token, destination, fx);
    IntentOutcome::Applied
}

fn send_workspace_to_screen(
    state: &mut TilingState,
    target: &ScreenTarget,
    fx: &mut Vec<Effect>,
) -> IntentOutcome {
    let Some(screen) = resolve_target(&state.screens, target) else {
        return reject(IntentRejection::UnknownScreen);
    };
    let Some(index) = focused_index(state) else {
        return noop(IntentNoOp::NoFocusedWorkspace);
    };
    let (id, source) = (state.workspaces[index].id, state.workspaces[index].screen);
    if source == screen {
        return noop(IntentNoOp::Unchanged);
    }
    // The workspace leaves its display: another one there becomes visible.
    let fallback = state
        .workspaces
        .iter()
        .find(|ws| ws.screen == source && ws.id != id)
        .map(|ws| ws.id);
    for workspace in &mut state.workspaces {
        if workspace.id == id {
            workspace.screen = screen;
            workspace.displaced = false;
        } else if Some(workspace.id) == fallback {
            workspace.visible = true;
        }
    }
    if fallback.is_none() {
        // The source display must keep a workspace: generate it on demand.
        let mut next = state.next_workspace;
        crate::workspaces::ensure_default_workspaces(
            &state.screens,
            &mut state.workspaces,
            &mut next,
        );
        state.next_workspace = next;
    }
    switch_to(state, id, fx);
    // `switch_to` shows exactly one workspace per display; re-normalize the rest.
    let hint = state.focus.workspace;
    crate::workspaces::reconcile(&state.screens, &mut state.workspaces, hint);
    IntentOutcome::Applied
}
