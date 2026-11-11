//! Tiling's published state: the minimal display DTO other modules observe.
//!
//! The actor projects every published [`TilingSnapshot`] into a
//! [`TilingPublishedState`] and forwards it to the generation's SDK
//! [`StatePublisher`] only when the projection differs from the last value it
//! published. The DTO carries no frames, admission counters or other churn, so
//! moves, resizes, drags and admission accounting never cause a publication,
//! and nothing here owns a timer, a poll or a thread.
//!
//! The payload is labelled with the snapshot's own accepted configuration
//! revision (the revision the actor applied), so the payload and its revision
//! label are always one coherent pair. The registry ends the generation (an
//! exact-generation tombstone) on stop, pause, disable and failure; a write
//! after that is rejected by the SDK and ignored here, so a late writer can
//! never reach a successor. After a caught actor panic nothing is published:
//! the last value stands until the registry ends the generation.
//!
//! # Window keys
//!
//! A window key is an opaque decimal string. It is the 128-bit number
//! `(backend epoch << 64) | serial`: the window adapter allocates serials
//! monotonically per backend epoch and never reuses one, and every Tiling
//! start opens a new epoch, so a key never names another window within one
//! generation and a key from a previous generation never resolves in a later
//! one. [`resolve_window_key`] maps a key back to the exact current
//! [`WindowInstance`]. No native pointer or token internal beyond that number
//! is published.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use stache_macos::windows::WindowInstance;
use stache_sdk::{PublishedState, StatePublisher, StateSchema};

use crate::state::{TilingSnapshot, WindowState};

/// Tiling's published state, schema `tilingState` v1.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TilingPublishedState {
    /// Every workspace, in Tiling's own workspace order.
    pub workspaces: Vec<PublishedWorkspace>,
    /// Every managed window, grouped by workspace in Tiling's stable layout
    /// order (workspaces in workspace order, windows without a workspace last).
    pub windows: Vec<PublishedWindow>,
    /// The focused workspace and window.
    pub focus: PublishedFocus,
}

/// One workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedWorkspace {
    /// The configured name.
    pub name: String,
    /// The display it is on: the `CGDirectDisplayID` as a decimal string.
    pub screen: String,
    /// The display name, when known.
    pub screen_name: Option<String>,
    /// Visible on its display.
    pub visible: bool,
    /// The globally focused workspace.
    pub focused: bool,
    /// Its position in Tiling's workspace order, from zero.
    pub order: u32,
}

/// One managed window.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedWindow {
    /// The opaque per-generation key (see [`resolve_window_key`]).
    pub key: String,
    /// The owning application's localized name (empty when unknown).
    pub app_name: String,
    /// The window title, unmodified.
    pub title: String,
    /// The workspace it belongs to, when assigned.
    pub workspace: Option<String>,
    /// Minimized.
    pub minimized: bool,
    /// The window or its application is hidden.
    pub hidden: bool,
    /// `false` for an inactive member of a native tab group: a tab group shows
    /// only its active tab. `true` for every window outside a tab group.
    pub tab_active: bool,
}

/// The focus.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedFocus {
    /// The focused workspace name.
    pub workspace: Option<String>,
    /// The focused window's key.
    pub window: Option<String>,
}

/// Tiling owns the published-state contract of its display state. The app
/// registers this original type as the codec for `TilingConfig::ID`.
/// Incompatible payload changes bump the version.
impl PublishedState for TilingPublishedState {
    const SCHEMA: StateSchema = StateSchema {
        name: "tilingState",
        version: 1,
    };
}

/// The opaque key of `window`.
#[must_use]
pub fn window_key(window: WindowInstance) -> String {
    ((u128::from(window.epoch().value()) << 64) | u128::from(window.serial())).to_string()
}

/// Whether the window is gone natively (its destroy is only held back for the
/// tab group slot) and so is not published.
const fn is_departing(window: &WindowState) -> bool { window.departing.is_some() }

/// The exact current window `key` names in `snapshot`.
///
/// `None` for a malformed or non-canonical key, a key no current window carries
/// (unknown, destroyed or from another generation) and a key two windows would
/// claim (ambiguity resolves to nothing rather than a guess).
#[must_use]
pub fn resolve_window_key(snapshot: &TilingSnapshot, key: &str) -> Option<WindowInstance> {
    let parsed: u128 = key.parse().ok()?;
    if parsed.to_string() != key {
        return None;
    }
    let mut matches = snapshot
        .windows
        .iter()
        .filter(|window| !is_departing(window) && window_key(window.token) == key);
    let first = matches.next()?;
    matches.next().is_none().then_some(first.token)
}

/// Projects `snapshot` into the published DTO.
#[must_use]
pub fn project(snapshot: &TilingSnapshot) -> TilingPublishedState {
    let workspaces = snapshot
        .workspaces
        .iter()
        .enumerate()
        .map(|(index, workspace)| PublishedWorkspace {
            name: workspace.name.clone(),
            screen: workspace.screen.raw().to_string(),
            screen_name: snapshot
                .screens
                .iter()
                .find(|screen| screen.id == workspace.screen)
                .and_then(|screen| screen.name.clone()),
            visible: workspace.visible,
            focused: workspace.focused,
            order: u32::try_from(index).unwrap_or(u32::MAX),
        })
        .collect();

    let live: Vec<&WindowState> =
        snapshot.windows.iter().filter(|window| !is_departing(window)).collect();
    let mut placed = BTreeSet::new();
    let mut ordered: Vec<&WindowState> = Vec::with_capacity(live.len());
    for workspace in &snapshot.workspaces {
        for token in &workspace.order {
            if let Some(window) = live.iter().find(|window| window.token == *token)
                && window.desired.workspace == Some(workspace.id)
                && placed.insert(window.token)
            {
                ordered.push(window);
            }
        }
        for window in &live {
            if window.desired.workspace == Some(workspace.id) && placed.insert(window.token) {
                ordered.push(window);
            }
        }
    }
    for window in &live {
        if placed.insert(window.token) {
            ordered.push(window);
        }
    }

    let windows = ordered
        .into_iter()
        .map(|window| {
            let app = snapshot.apps.iter().find(|app| app.info.application == window.application);
            PublishedWindow {
                key: window_key(window.token),
                app_name: app.map(|app| app.info.name.clone()).unwrap_or_default(),
                title: window.observed.title.clone(),
                workspace: window.desired.workspace.and_then(|id| workspace_name(snapshot, id)),
                minimized: window.observed.minimized,
                hidden: window.observed.hidden || app.is_some_and(|app| app.hidden),
                tab_active: !window.is_inactive_tab(),
            }
        })
        .collect();

    let focus = PublishedFocus {
        workspace: snapshot.focus.workspace.and_then(|id| workspace_name(snapshot, id)),
        window: snapshot
            .focus
            .window
            .filter(|token| live.iter().any(|window| window.token == *token))
            .map(window_key),
    };

    TilingPublishedState { workspaces, windows, focus }
}

fn workspace_name(snapshot: &TilingSnapshot, id: crate::state::WorkspaceId) -> Option<String> {
    snapshot.workspaces.iter().find(|ws| ws.id == id).map(|ws| ws.name.clone())
}

/// Decides when a projection is published: only once a configuration was
/// applied and only when it differs from the last committed one.
#[derive(Debug, Default)]
pub struct PublishGate {
    last: Option<TilingPublishedState>,
}

impl PublishGate {
    /// The revision and DTO to publish for `snapshot`, or `None` when nothing
    /// changed or no configuration was applied yet.
    pub fn next(&self, snapshot: &TilingSnapshot) -> Option<(u64, TilingPublishedState)> {
        if snapshot.config_revision == 0 {
            return None;
        }
        let state = project(snapshot);
        (self.last.as_ref() != Some(&state)).then_some((snapshot.config_revision, state))
    }

    /// Records a value the SDK accepted.
    pub fn committed(&mut self, state: TilingPublishedState) { self.last = Some(state); }
}

/// One generation's publication point, owned by the actor.
pub struct Publication {
    publisher: StatePublisher<TilingPublishedState>,
    gate: PublishGate,
}

impl Publication {
    pub fn new(publisher: StatePublisher<TilingPublishedState>) -> Self {
        Self {
            publisher,
            gate: PublishGate::default(),
        }
    }

    /// Publishes the projection of `snapshot` when it changed. A rejected write
    /// (the generation ended, or a revision the cell refuses) is not recorded,
    /// so nothing is fabricated and the next change retries.
    pub fn observe(&mut self, snapshot: &TilingSnapshot) {
        let Some((revision, state)) = self.gate.next(snapshot) else {
            return;
        };
        if self.publisher.publish(revision, &state).is_ok() {
            self.gate.committed(state);
        }
    }
}

#[cfg(test)]
#[path = "published_tests.rs"]
mod tests;
