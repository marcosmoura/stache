//! Keeps the windows' inactive-tab flags in step with the tab registry.
//!
//! Layout is a pure function of the windows, their order and their flags, so
//! changing a flag is the whole invalidation: a window that becomes an inactive
//! tab leaves the desired frames, the window that becomes the group's visible
//! member takes the slot the previous one held, and nothing else moves. Tab-only
//! creation or destruction (inactive members) changes no flag of a tile and so
//! produces no new plan.

use std::collections::BTreeSet;

use stache_macos::ApplicationId;
use stache_macos::windows::{WindowInstance, WindowSnapshot};

use super::TilingState;

/// Applies a snapshot's tab information and re-derives the flags of its
/// application. `fresh` is a window adopted by this very snapshot.
pub(super) fn observe(state: &mut TilingState, snapshot: &WindowSnapshot, fresh: bool) {
    state.tabs.observe(snapshot);
    // Recomputing is idempotent and bounded by the application's windows; a
    // `CGWindowID` that arrives late can complete a member match.
    if state.tabs.reading() {
        recompute(
            state,
            snapshot.window.application(),
            fresh.then_some(snapshot.window),
        );
    }
}

/// Drops a gone window from the registry and re-derives its application.
pub(super) fn forget_window(
    state: &mut TilingState,
    token: WindowInstance,
    cg_window_id: Option<u32>,
) {
    if state.tabs.forget_window(token, cg_window_id) {
        recompute(state, token.application(), None);
    }
}

/// Re-derives the inactive flags of one application's windows and gives the
/// newly visible member of each group the slot of the one it replaced.
pub(super) fn recompute(
    state: &mut TilingState,
    application: ApplicationId,
    fresh: Option<WindowInstance>,
) {
    let inactive = state.tabs.inactive(application, state.windows.values());
    let mut became_inactive: BTreeSet<WindowInstance> = BTreeSet::new();
    let mut became_active: BTreeSet<WindowInstance> = BTreeSet::new();
    for window in state.windows.values_mut().filter(|w| w.application == application) {
        let now = inactive.contains(&window.token);
        if now && !window.inactive_tab {
            became_inactive.insert(window.token);
        } else if !now && window.inactive_tab {
            became_active.insert(window.token);
        }
        window.inactive_tab = now;
    }
    if became_inactive.is_empty() {
        return;
    }
    for group in state.tabs.resolve(application, state.windows.values()) {
        let losers: Vec<WindowInstance> =
            group.inactive.iter().copied().filter(|t| became_inactive.contains(t)).collect();
        let winner = (became_active.contains(&group.active) || fresh == Some(group.active))
            .then_some(group.active);
        if let ([loser], Some(winner)) = (losers.as_slice(), winner) {
            swap_slots(state, *loser, winner);
        }
    }
}

/// Exchanges the stable order positions of two windows of one workspace so the
/// replacement lands where the replaced window was.
fn swap_slots(state: &mut TilingState, loser: WindowInstance, winner: WindowInstance) {
    let workspace = |token: WindowInstance| {
        state.windows.get(&token).and_then(|window| window.desired.workspace)
    };
    let (Some(a), Some(b)) = (workspace(loser), workspace(winner)) else {
        return;
    };
    if a != b {
        return;
    }
    let Some(ws) = state.workspaces.iter_mut().find(|ws| ws.id == a) else {
        return;
    };
    let positions = (
        ws.order.iter().position(|t| *t == loser),
        ws.order.iter().position(|t| *t == winner),
    );
    if let (Some(i), Some(j)) = positions {
        ws.order.swap(i, j);
    }
}
