//! Slots: the tile a native tab group holds in a workspace.
//!
//! A slot has one **active** member (the tile) and any number of **suspended**
//! members: windows of the same application that left `AXWindows` while their
//! element stayed alive (macOS lists only the selected tab of a group, and a
//! window that is alone in its group has no tab bar to tell it from the next
//! one). A suspended member keeps its token, is never a tile and is never
//! written. Membership is the opaque `tab_group` id; nothing is ever matched by
//! frame, title or geometry.
//!
//! * A window that is suspended while it holds the tile keeps it (`slot_held`)
//!   until the listing settles, so the layout does not move while the window
//!   that replaces it is still being read.
//! * When exactly one such window and exactly one new tabbed window of the same
//!   application exist, the new one becomes the active member (single-tab
//!   handover). Any other combination releases the tile after the settle.
//! * A suspended member listed again becomes the active member of its slot,
//!   replacing the previous active one when that one was closed or suspended.

use stache_macos::ApplicationId;
use stache_macos::windows::WindowInstance;

use super::transition::{assign, destroy, untrack};
use super::{DesiredWindow, MAX_PENDING_HANDOVERS, TabNote, TilingState, frames};

fn note(state: &mut TilingState, note: TabNote) {
    if state.tab_outbox.notes.len() < MAX_PENDING_HANDOVERS {
        state.tab_outbox.notes.push(note);
    }
}

/// A fresh slot id.
pub(super) const fn mint_group(state: &mut TilingState) -> u64 {
    let id = state.next_tab_group;
    state.next_tab_group += 1;
    id
}

/// The owner reported `token` unlisted but alive.
pub(super) fn suspend(state: &mut TilingState, token: WindowInstance) {
    let Some(window) = state.windows.get(&token) else {
        return;
    };
    if window.suspended || window.departing.is_some() {
        return;
    }
    let held = window.desired.workspace.is_some_and(|id| {
        state
            .workspaces
            .iter()
            .any(|workspace| workspace.id == id && workspace.order.contains(&token))
    });
    let known = window.tab_group;
    let group = known.unwrap_or_else(|| mint_group(state));
    if let Some(window) = state.windows.get_mut(&token) {
        window.suspended = true;
        window.slot_held = held;
        window.returning = false;
        window.tab_group = Some(group);
    }
    state.diagnostics.tab_suspensions += 1;
    note(state, TabNote::Suspended(token));
    // The slot is decided once the listing settled.
    state.tab_outbox.settle.entry(token.application()).or_insert(false);
    pair(state, token.application());
}

/// Matches the one suspended slot holder of `application` with its one new
/// tabbed window. Returns whether a handover happened.
pub(super) fn pair(state: &mut TilingState, application: ApplicationId) -> bool {
    let mut held = state
        .windows
        .values()
        .filter(|window| window.application == application && window.suspended && window.slot_held)
        .map(|window| window.token);
    let (Some(old), None) = (held.next(), held.next()) else {
        return false;
    };
    drop(held);
    let mut arrivals = state
        .windows
        .values()
        .filter(|window| window.application == application && window.arrival && !window.suspended)
        .map(|window| window.token);
    let (Some(new), None) = (arrivals.next(), arrivals.next()) else {
        return false;
    };
    drop(arrivals);
    transplant(state, old, new);
    state.diagnostics.tab_single_handovers += 1;
    note(state, TabNote::SingleHandover(old, new));
    true
}

/// Moves the tile of `from` to `to`, a tracked window of the same application:
/// workspace, order position, floating and positioned flags, focus, focus
/// history and the frame record (so nothing is written unless `to` is off its
/// target). `to`'s own earlier place, if any, is dropped. `from` stays tracked
/// as a suspended member without a tile; the caller decides what else becomes of
/// it.
pub(super) fn transplant(state: &mut TilingState, from: WindowInstance, to: WindowInstance) {
    let Some(source) = state.windows.get(&from) else { return };
    let desired = source.desired;
    let group = source.tab_group;
    // `to` leaves the place it had.
    let own = state.windows.get(&to).and_then(|window| window.desired.workspace);
    if let Some(id) = own
        && let Some(workspace) = state.workspaces.iter_mut().find(|ws| ws.id == id)
    {
        workspace.order.retain(|held| *held != to);
        if workspace.focused_window == Some(to) {
            workspace.focused_window = state
                .focus_history
                .iter()
                .rev()
                .find(|held| **held != to && workspace.order.contains(held))
                .copied();
        }
    }
    state.focus_history.retain(|held| *held != to);
    // `to` takes the position of `from`.
    for workspace in &mut state.workspaces {
        for held in &mut workspace.order {
            if *held == from {
                *held = to;
            }
        }
        if workspace.focused_window == Some(from) {
            workspace.focused_window = Some(to);
        }
    }
    if state.focus.window == Some(from) {
        state.focus.window = Some(to);
    }
    for held in &mut state.focus_history {
        if *held == from {
            *held = to;
        }
    }
    frames::forget(state, to);
    frames::transfer(state, from, to);
    let group = group.unwrap_or_else(|| mint_group(state));
    if let Some(window) = state.windows.get_mut(&to) {
        window.desired = desired;
        window.suspended = false;
        window.slot_held = false;
        window.returning = false;
        window.arrival = false;
        window.tab_group = Some(group);
    }
    if let Some(window) = state.windows.get_mut(&from) {
        window.desired = DesiredWindow::default();
        window.suspended = true;
        window.slot_held = false;
        window.returning = false;
        window.arrival = false;
        window.tab_group = Some(group);
    }
}

/// A suspended window was listed again.
pub(super) fn mark_returning(state: &mut TilingState, token: WindowInstance) {
    if let Some(window) = state.windows.get_mut(&token)
        && window.suspended
    {
        window.returning = true;
    }
}

/// Resolves every returning window of `application`.
pub(super) fn resolve_returns(state: &mut TilingState, application: ApplicationId) {
    let returning: Vec<WindowInstance> = state
        .windows
        .values()
        .filter(|window| window.application == application && window.returning)
        .map(|window| window.token)
        .collect();
    for token in returning {
        resolve_return(state, token, false);
    }
}

/// A suspended window is listed again: it becomes the active member of its slot.
///
/// With `wait` set (a single read outside a listing) a slot whose active member
/// still looks alive is left alone until the listing settles: that member may be
/// about to report its close.
pub(super) fn resolve_return(state: &mut TilingState, token: WindowInstance, wait: bool) {
    let Some(window) = state.windows.get(&token) else {
        return;
    };
    if !window.suspended {
        if let Some(window) = state.windows.get_mut(&token) {
            window.returning = false;
        }
        return;
    }
    let application = window.application;
    let group = window.tab_group;
    // It still holds the tile: nothing else took the slot meanwhile.
    if window.slot_held {
        state.diagnostics.tab_returns += 1;
        if let Some(window) = state.windows.get_mut(&token) {
            window.suspended = false;
            window.slot_held = false;
            window.returning = false;
        }
        note(state, TabNote::Resumed(token));
        return;
    }
    let holder = group.and_then(|group| {
        state
            .windows
            .values()
            .find(|member| {
                member.token != token
                    && member.application == application
                    && member.tab_group == Some(group)
                    && member.holds_slot()
            })
            .map(|member| (member.token, member.departing.is_some() || member.suspended))
    });
    if wait && matches!(holder, Some((_, false))) {
        state.tab_outbox.settle.entry(application).or_insert(false);
        return;
    }
    state.diagnostics.tab_returns += 1;
    // The previous active member is closed (held back) or suspended: its tile
    // goes to the returning window.
    if let Some((holder, true)) = holder {
        transplant(state, holder, token);
        let gone = state.windows.get(&holder).is_some_and(|member| member.departing.is_some());
        if gone {
            destroy(state, holder);
        }
        note(state, TabNote::Returned(token, holder));
        return;
    }
    // No member holds the tile (it was released or its window closed), or one is
    // still active and alive: the window is an ordinary new tile.
    if let Some(window) = state.windows.get_mut(&token) {
        window.suspended = false;
        window.slot_held = false;
        window.returning = false;
        window.desired = DesiredWindow::default();
        if holder.is_some() {
            window.tab_group = None;
        }
    }
    assign(state, token);
    note(state, TabNote::Resumed(token));
}

/// The listing of `application` settled: suspended windows whose slot nobody
/// took over release their tile (the layout reflows) and the arrivals that found
/// no suspension are ordinary windows.
pub(super) fn settle(state: &mut TilingState, application: ApplicationId) {
    let released: Vec<WindowInstance> = state
        .windows
        .values()
        .filter(|window| window.application == application && window.suspended && window.slot_held)
        .map(|window| window.token)
        .collect();
    for token in released {
        release_slot(state, token);
    }
    for window in state.windows.values_mut().filter(|window| window.application == application) {
        window.arrival = false;
    }
}

/// Takes the tile of a suspended window away; the window stays tracked.
fn release_slot(state: &mut TilingState, token: WindowInstance) {
    let Some(mut kept) = state.windows.get(&token).cloned() else {
        return;
    };
    untrack(state, token);
    kept.desired = DesiredWindow::default();
    kept.slot_held = false;
    kept.returning = false;
    if let Some(cg) = kept.cg_window_id {
        state.cg_index.insert(cg, token);
    }
    state.windows.insert(token, kept);
    state.diagnostics.tab_slots_released += 1;
    note(state, TabNote::Released(token));
}
