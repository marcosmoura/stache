//! The pure virtual-workspace model.
//!
//! Everything here is a function of values: display resolution, the workspaces a
//! configuration asks for, the per-application visibility requirement and the
//! accepted structural reload mapping. Nothing calls native code, takes a lock
//! or reads a clock; the actor is the only writer of the state these read.
//!
//! # Visibility requirement
//!
//! Application hide/unhide is global (decision 1): a visible workspace that
//! holds a window of an application needs that application visible, whatever
//! other workspaces want. [`visibility_requirement`] aggregates every visible
//! workspace on every display, so an application needed by any of them is never
//! [`AppVisibility::hide_wanted`]. Windows of that application on inactive
//! workspaces keep their logical assignment and stay on screen; the limit is
//! published as a [`VisibilityConflict`]. The result is a pure function of the
//! state, so it cannot flip-flop inside one transition. Slice B consumes it; this
//! slice issues no native hide or unhide.
//!
//! # Reload
//!
//! [`prepare_reload`] builds the complete [`Mapping`] for an accepted structural
//! configuration change before anything is committed. A rejected mapping leaves
//! the old state untouched.

use std::collections::{BTreeMap, BTreeSet};

use stache_macos::windows::WindowInstance;
use stache_macos::{ApplicationId, DisplayId};

use crate::config::{LayoutType, TilingConfig};
use crate::intent::ScreenTarget;
use crate::rules::{ManagementExclusion, PreparedRules};
use crate::state::{
    FocusState, ScreenState, TilingSnapshot, TilingState, WorkspaceId, WorkspaceState,
};

// ------------------------------------------------------------------ displays

/// The main display: the primary one, else the first.
#[must_use]
pub fn primary_screen(screens: &[ScreenState]) -> Option<DisplayId> {
    screens.iter().find(|s| s.is_primary).or_else(|| screens.first()).map(|s| s.id)
}

/// Resolves a configured screen string: `main`/`primary`, `secondary` (the first
/// non-main display), an exact display name, else a case-insensitive substring.
#[must_use]
pub fn resolve_screen(screens: &[ScreenState], name: &str) -> Option<DisplayId> {
    if name == "main" || name == "primary" {
        return primary_screen(screens);
    }
    if name == "secondary" {
        return first_secondary(screens);
    }
    if let Some(screen) = screens.iter().find(|s| s.name.as_deref() == Some(name)) {
        return Some(screen.id);
    }
    let lowered = name.to_lowercase();
    screens
        .iter()
        .find(|s| s.name.as_deref().is_some_and(|n| n.to_lowercase().contains(&lowered)))
        .map(|s| s.id)
}

fn first_secondary(screens: &[ScreenState]) -> Option<DisplayId> {
    let main = primary_screen(screens)?;
    screens.iter().find(|s| s.id != main).map(|s| s.id)
}

/// Resolves the display an intent names. `Secondary` is any display that is not
/// the main one (the first, in display order).
#[must_use]
pub fn resolve_target(screens: &[ScreenState], target: &ScreenTarget) -> Option<DisplayId> {
    match target {
        ScreenTarget::Main => primary_screen(screens),
        ScreenTarget::Secondary => first_secondary(screens),
        ScreenTarget::Named(name) => screens
            .iter()
            .find(|s| s.name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(name)))
            .map(|s| s.id),
    }
}

// ---------------------------------------------------------------- workspaces

/// A workspace a configuration asks for, resolved against the present displays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSpec {
    /// Name; the continuity key across reloads.
    pub name: String,
    /// The configured layout, if the workspace names one.
    pub configured_layout: Option<LayoutType>,
    /// The layout the configuration resolves to (explicit, else the default).
    pub layout: LayoutType,
    /// The configured screen string; `None` for generated defaults.
    pub configured_screen: Option<String>,
    /// The display it lands on.
    pub screen: DisplayId,
    /// The configured display is not present, so the main one stands in.
    pub displaced: bool,
}

/// The workspaces `config` asks for. Empty configuration: one default workspace
/// per display. A configured display that is absent falls back to main.
#[must_use]
pub fn specs(screens: &[ScreenState], config: &TilingConfig) -> Vec<WorkspaceSpec> {
    if config.workspaces.is_empty() {
        return screens
            .iter()
            .enumerate()
            .map(|(index, screen)| WorkspaceSpec {
                name: format!("workspace-{}", index + 1),
                configured_layout: None,
                layout: config.default_layout,
                configured_screen: None,
                screen: screen.id,
                displaced: false,
            })
            .collect();
    }
    config
        .workspaces
        .iter()
        .filter_map(|ws| {
            let resolved = resolve_screen(screens, &ws.screen);
            let screen = resolved.or_else(|| primary_screen(screens))?;
            Some(WorkspaceSpec {
                name: ws.name.clone(),
                configured_layout: ws.layout,
                layout: ws.layout.unwrap_or(config.default_layout),
                configured_screen: Some(ws.screen.clone()),
                screen,
                displaced: resolved.is_none(),
            })
        })
        .collect()
}

/// The name of the workspace generated for a display no configured workspace
/// reached.
#[must_use]
pub fn default_name(screen: DisplayId) -> String { format!("default-{}", screen.raw()) }

fn is_generated_default(workspace: &WorkspaceState) -> bool {
    workspace.configured_screen.is_none() && workspace.name.starts_with("default-")
}

/// A fresh, empty workspace.
#[must_use]
pub fn new_workspace(id: WorkspaceId, spec: &WorkspaceSpec) -> WorkspaceState {
    WorkspaceState {
        id,
        name: spec.name.clone(),
        screen: spec.screen,
        configured_screen: spec.configured_screen.clone(),
        configured_layout: spec.configured_layout,
        layout: spec.layout,
        layout_overridden: false,
        displaced: spec.displaced,
        ratios: crate::minimum_size::RatioState::default(),
        visible: false,
        focused: false,
        order: Vec::new(),
        focused_window: None,
    }
}

/// Gives every display that has no workspace its generated default.
pub fn ensure_default_workspaces(
    screens: &[ScreenState],
    workspaces: &mut Vec<WorkspaceState>,
    next_id: &mut u32,
) {
    for screen in screens {
        if workspaces.iter().any(|ws| ws.screen == screen.id) {
            continue;
        }
        let spec = WorkspaceSpec {
            name: default_name(screen.id),
            configured_layout: None,
            layout: LayoutType::Dwindle,
            configured_screen: None,
            screen: screen.id,
            displaced: false,
        };
        workspaces.push(new_workspace(WorkspaceId(*next_id), &spec));
        *next_id += 1;
    }
}

/// Normalizes visibility and focus: exactly one visible workspace per present
/// display and one globally focused workspace. Existing choices are kept where
/// possible. Returns the focused workspace.
pub fn reconcile(
    screens: &[ScreenState],
    workspaces: &mut [WorkspaceState],
    focused_hint: Option<WorkspaceId>,
) -> Option<WorkspaceId> {
    let mut visible = BTreeSet::new();
    for screen in screens {
        let on: Vec<&WorkspaceState> =
            workspaces.iter().filter(|ws| ws.screen == screen.id).collect();
        let selected = on
            .iter()
            .find(|ws| Some(ws.id) == focused_hint && ws.visible)
            .or_else(|| on.iter().find(|ws| ws.visible))
            .or_else(|| on.first());
        if let Some(workspace) = selected {
            visible.insert(workspace.id);
        }
    }
    let primary = primary_screen(screens);
    let focus = focused_hint
        .filter(|id| visible.contains(id))
        .or_else(|| {
            workspaces
                .iter()
                .find(|ws| ws.focused && visible.contains(&ws.id))
                .map(|ws| ws.id)
        })
        .or_else(|| {
            workspaces
                .iter()
                .find(|ws| Some(ws.screen) == primary && visible.contains(&ws.id))
                .map(|ws| ws.id)
        })
        .or_else(|| visible.iter().next().copied());
    for workspace in workspaces.iter_mut() {
        workspace.visible = visible.contains(&workspace.id);
        workspace.focused = Some(workspace.id) == focus;
    }
    focus
}

// -------------------------------------------------------- visibility needs

/// What one application needs, aggregated over every visible workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppVisibility {
    /// The exact application instance.
    pub application: ApplicationId,
    /// Visible workspaces holding at least one tracked window of it.
    pub needed_by: Vec<WorkspaceId>,
    /// The displays of those workspaces.
    pub displays: Vec<DisplayId>,
    /// Non-visible workspaces holding its windows.
    pub inactive_in: Vec<WorkspaceId>,
    /// Hiding it is wanted: it has tracked windows and no visible workspace
    /// needs it. Never true while [`Self::needed_by`] is non-empty.
    pub hide_wanted: bool,
    /// The application is hidden by someone else. Never cleared by a refresh.
    pub externally_hidden: bool,
    /// Finder without a tracked window: kept shown as the desktop host,
    /// because macOS does not hide the last visible regular application.
    pub host: bool,
}

/// Why isolation is only partial.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictKind {
    /// A visible workspace needs the application while inactive workspaces
    /// also hold its windows: those windows stay visible.
    InactiveWindowsStayVisible,
    /// Visible workspaces on more than one display need the application.
    SharedAcrossDisplays,
}

/// A published partial-isolation limit. Never a promise to isolate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibilityConflict {
    /// The application.
    pub application: ApplicationId,
    /// The kind.
    pub kind: ConflictKind,
    /// The visible workspaces that win.
    pub needed_by: Vec<WorkspaceId>,
    /// The workspaces whose windows could not be isolated.
    pub affected: Vec<WorkspaceId>,
}

/// The application visibility requirement of a whole state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VisibilityRequirement {
    /// One entry per application with tracked windows, ordered by application.
    pub apps: Vec<AppVisibility>,
    /// Partial-isolation diagnostics, ordered by application.
    pub conflicts: Vec<VisibilityConflict>,
    /// Finder is wanted hidden because its tracked windows are all on hidden
    /// workspaces, so it cannot host the desktop (see [`AppVisibility::host`]).
    pub finder_held: bool,
    /// Startup has tried every listed application (`completed_once`).
    pub settled: bool,
    /// A running application without any window that hosts the desktop
    /// because Finder cannot: nothing visible is needed and something is
    /// wanted hidden. Its entry in [`Self::apps`] has [`AppVisibility::host`].
    pub desktop_host: Option<ApplicationId>,
    /// [`Self::desktop_host`] is the active application.
    pub host_active: bool,
    /// The frontmost application kept on screen: Finder is held, no
    /// windowless host exists and every other application is hidden or being
    /// hidden, so macOS would refuse its hide (and only deactivate it). Its
    /// entry is not wanted hidden.
    pub kept_last: Option<ApplicationId>,
}

impl VisibilityRequirement {
    /// The requirement of one application.
    #[must_use]
    pub fn app(&self, application: ApplicationId) -> Option<&AppVisibility> {
        self.apps.iter().find(|app| app.application == application)
    }

    /// Whether hiding `application` is wanted.
    #[must_use]
    pub fn hide_wanted(&self, application: ApplicationId) -> bool {
        self.app(application).is_some_and(|app| app.hide_wanted)
    }

    /// Whether some visible workspace needs `application`.
    #[must_use]
    pub fn needed(&self, application: ApplicationId) -> bool {
        self.app(application).is_some_and(|app| !app.needed_by.is_empty())
    }
}

/// Aggregates the visibility requirement over every visible workspace.
#[must_use]
pub fn visibility_requirement(state: &TilingState) -> VisibilityRequirement {
    #[derive(Default)]
    struct Acc {
        needed_by: BTreeSet<WorkspaceId>,
        displays: BTreeSet<DisplayId>,
        inactive: BTreeSet<WorkspaceId>,
    }
    let mut acc: BTreeMap<ApplicationId, Acc> = BTreeMap::new();
    for workspace in &state.workspaces {
        for token in &workspace.order {
            let Some(window) = state.windows.get(token) else {
                continue;
            };
            let entry = acc.entry(window.application).or_default();
            if workspace.visible {
                entry.needed_by.insert(workspace.id);
                entry.displays.insert(workspace.screen);
            } else {
                entry.inactive.insert(workspace.id);
            }
        }
    }
    let mut out = VisibilityRequirement::default();
    for (application, entry) in acc {
        let needed = !entry.needed_by.is_empty();
        let needed_by: Vec<WorkspaceId> = entry.needed_by.into_iter().collect();
        let inactive: Vec<WorkspaceId> = entry.inactive.into_iter().collect();
        if needed && !inactive.is_empty() {
            out.conflicts.push(VisibilityConflict {
                application,
                kind: ConflictKind::InactiveWindowsStayVisible,
                needed_by: needed_by.clone(),
                affected: inactive.clone(),
            });
        }
        if entry.displays.len() > 1 {
            out.conflicts.push(VisibilityConflict {
                application,
                kind: ConflictKind::SharedAcrossDisplays,
                needed_by: needed_by.clone(),
                affected: needed_by.clone(),
            });
        }
        out.apps.push(AppVisibility {
            application,
            hide_wanted: !needed && !focus_undecided(state, application),
            displays: entry.displays.into_iter().collect(),
            externally_hidden: state.apps.get(&application).is_some_and(|app| app.hidden),
            needed_by,
            inactive_in: inactive,
            host: false,
        });
    }
    for application in finders(state) {
        match out.apps.iter().position(|app| app.application == application) {
            Some(index) => out.finder_held |= out.apps[index].hide_wanted,
            // Only once every listed application was tried: a Finder window
            // not listed yet must not be shown by mistake.
            None if !state.completed_once => {}
            None => out.apps.push(AppVisibility {
                application,
                needed_by: Vec::new(),
                displays: Vec::new(),
                inactive_in: Vec::new(),
                hide_wanted: false,
                externally_hidden: state.apps.get(&application).is_some_and(|app| app.hidden),
                host: true,
            }),
        }
    }
    out.settled = state.completed_once;
    if let Some(application) = windowless_host(state, &out) {
        out.desktop_host = Some(application);
        out.host_active = state.active_application == Some(application);
        out.apps.push(AppVisibility {
            application,
            needed_by: Vec::new(),
            displays: Vec::new(),
            inactive_in: Vec::new(),
            hide_wanted: false,
            externally_hidden: state.apps.get(&application).is_some_and(|app| app.hidden),
            host: true,
        });
    }
    if out.desktop_host.is_none() {
        out.kept_last = last_visible(state, &out);
        if let Some(index) = out.apps.iter().position(|app| Some(app.application) == out.kept_last)
        {
            out.apps[index].hide_wanted = false;
        }
    }
    out.apps.sort_by_key(|app| app.application);
    out
}

/// The frontmost application, when it is wanted hidden and would be the last
/// visible regular application: Finder is held, nothing is needed and every
/// other known application is hidden (observed or owned) or wanted hidden.
/// An application whose visibility is unknown (never reported hidden) keeps
/// the hide being sent.
fn last_visible(state: &TilingState, out: &VisibilityRequirement) -> Option<ApplicationId> {
    let front = state.active_application?;
    let blocked = !state.completed_once
        || !out.finder_held
        || out.apps.iter().any(|app| app.host || !app.needed_by.is_empty())
        || !out.hide_wanted(front);
    let others_hidden = state.apps.iter().all(|(application, record)| {
        *application == front
            || record.skipped
            || record.hidden
            || state.owned_hidden.contains(application)
            || out.hide_wanted(*application)
    });
    (!blocked && others_hidden).then_some(front)
}

/// Finder's bundle identifier: the desktop host.
const FINDER_BUNDLE_ID: &str = "com.apple.finder";

/// The running Finder instances Tiling knows.
fn finders(state: &TilingState) -> impl Iterator<Item = ApplicationId> + '_ {
    state
        .apps
        .iter()
        .filter(|(_, record)| record.info.bundle_id.eq_ignore_ascii_case(FINDER_BUNDLE_ID))
        .map(|(application, _)| *application)
}

/// The desktop host when Finder cannot be one: needed only once startup has
/// tried every application, when Finder is held hidden, no visible workspace
/// needs an application and something is wanted hidden (a switch would leave
/// no visible regular application). A candidate is a tracked (listed, so
/// regular), non-skipped application whose window listing finished and
/// returned nothing: no tracked and no excluded window. The active one wins,
/// then an unhidden one, then the lowest application id.
///
/// Blind spot: the AX listing returns only windows of the current Space, so
/// an application whose windows are all on another Space or in a native
/// fullscreen Space (never listed or focused) looks windowless; activating it
/// can make macOS switch to that Space. A Space change does not re-list such
/// an application.
fn windowless_host(state: &TilingState, out: &VisibilityRequirement) -> Option<ApplicationId> {
    let needed = |app: &AppVisibility| app.host || !app.needed_by.is_empty();
    if !state.completed_once
        || !out.finder_held
        || out.apps.iter().any(needed)
        || !out.apps.iter().any(|app| app.hide_wanted)
    {
        return None;
    }
    windowless_candidate(state)
}

/// The windowless host candidate of [`windowless_host`], without the
/// planner's "a hide is wanted now" preconditions: whether some application
/// could host an empty desktop in this state.
fn windowless_candidate(state: &TilingState) -> Option<ApplicationId> {
    let startup = &state.startup;
    state
        .apps
        .iter()
        .filter(|(application, record)| {
            record.tracked
                && !record.skipped
                && !startup.failed.contains(application)
                && !startup.truncated.contains(application)
                && !startup.awaiting.contains(application)
                && !state.reconcile.inflight.contains(application)
                && !state.reconcile.unconfirmed.contains(application)
                && !state.reconcile.all
                && !state.windows.values().any(|window| window.application == **application)
                && !state.excluded.keys().any(|token| token.application() == **application)
        })
        .min_by_key(|(application, record)| {
            (
                state.active_application != Some(**application),
                record.hidden,
                **application,
            )
        })
        .map(|(application, _)| *application)
}

/// The workspaces Stache cannot show as an empty desktop (published as
/// `inactive`): no live window is assigned to it, it is not visible on its
/// display and it is not reachable. A workspace is reachable once startup has
/// tried every listed application and a workspace visible on another display
/// holds a window, Finder hosts the desktop (it runs without a tracked window)
/// or, while Finder is running, the windowless host selection finds a
/// candidate (the planner uses a windowless host only once Finder is held
/// hidden, so without a running Finder it would pick none). Before startup
/// completes only the other-display rule applies. `requirement` is
/// [`visibility_requirement`] of the same state: its Finder host entry is the
/// Finder rule.
#[must_use]
pub fn unreachable(state: &TilingState, requirement: &VisibilityRequirement) -> Vec<WorkspaceId> {
    let live = |id: WorkspaceId| {
        state.windows.values().any(|window| {
            window.departing.is_none()
                && (window.desired.workspace == Some(id)
                    || state
                        .workspaces
                        .iter()
                        .any(|ws| ws.id == id && ws.order.contains(&window.token)))
        })
    };
    // The planner picks a windowless host only while Finder is held, which
    // needs a running Finder: without one, rule 3 does not apply.
    let host = state.completed_once
        && (requirement.apps.iter().any(|app| app.host)
            || (finders(state).next().is_some() && windowless_candidate(state).is_some()));
    state
        .workspaces
        .iter()
        .filter(|workspace| !workspace.visible && !live(workspace.id) && !host)
        .filter(|workspace| {
            !state
                .workspaces
                .iter()
                .any(|other| other.visible && other.screen != workspace.screen && live(other.id))
        })
        .map(|workspace| workspace.id)
        .collect()
}

/// Whether `application` is the frontmost one and its focused window is not
/// decided yet: startup is still listing windows, or its focused-window read
/// has not answered. Hiding it now would make the system activate another
/// application, and the window the user was on would lose the focus for good
/// (an unhide never activates), although the answer may be about to make its
/// workspace the visible one.
fn focus_undecided(state: &TilingState, application: ApplicationId) -> bool {
    state.active_application == Some(application)
        && (!state.completed_once || state.pending_focus.inflight.contains(&application))
}

// -------------------------------------------------------------------- reload

/// Why a reload mapping was refused. The old state stays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReloadError {
    /// Two workspaces share a name, so the continuity key is ambiguous.
    DuplicateWorkspace(String),
    /// A workspace has no name.
    EmptyWorkspaceName,
    /// The mapping would break a workspace invariant.
    Inconsistent(&'static str),
}

impl std::fmt::Display for ReloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateWorkspace(name) => write!(f, "duplicate workspace name `{name}`"),
            Self::EmptyWorkspaceName => f.write_str("a workspace has an empty name"),
            Self::Inconsistent(why) => write!(f, "inconsistent workspace mapping: {why}"),
        }
    }
}

impl std::error::Error for ReloadError {}

/// The complete outcome of a structural reload, prepared before acceptance.
#[derive(Clone, Debug)]
pub struct Mapping {
    /// The new workspaces (identity, layout, ratios, order and focus carried).
    pub workspaces: Vec<WorkspaceState>,
    /// The next workspace id to allocate.
    pub next_workspace: u32,
    /// The workspace each moved window now belongs to.
    pub moved: BTreeMap<WindowInstance, WorkspaceId>,
    /// The focused workspace after the reload.
    pub focused: Option<WorkspaceId>,
    /// Windows the new `ignore` rules exclude: they release tiling and every
    /// visibility need.
    pub released: Vec<(WindowInstance, ManagementExclusion)>,
    /// Windows excluded by an `ignore` rule before; they are read again so the
    /// new rules decide.
    pub recheck: Vec<WindowInstance>,
    /// Workspace rules prepared from the new configuration.
    pub workspace_rules: Vec<(String, PreparedRules)>,
    /// The ignore rules prepared from the new configuration.
    pub ignore: PreparedRules,
}

fn validate_names(config: &TilingConfig) -> Result<(), ReloadError> {
    let mut seen = BTreeSet::new();
    for workspace in &config.workspaces {
        if workspace.name.is_empty() {
            return Err(ReloadError::EmptyWorkspaceName);
        }
        if !seen.insert(workspace.name.as_str()) {
            return Err(ReloadError::DuplicateWorkspace(workspace.name.clone()));
        }
    }
    Ok(())
}

/// Carries one existing workspace across a reload.
fn carry(
    old: &WorkspaceState,
    spec: &WorkspaceSpec,
    old_default: LayoutType,
    new_default: LayoutType,
) -> WorkspaceState {
    let mut next = old.clone();
    let old_effective = old.configured_layout.unwrap_or(old_default);
    let new_effective = spec.configured_layout.unwrap_or(new_default);
    // Screen assignment edits move the workspace; otherwise a user relocation
    // or a hotplug fallback is kept.
    if old.configured_screen != spec.configured_screen {
        next.screen = spec.screen;
        next.displaced = spec.displaced;
    }
    next.configured_screen.clone_from(&spec.configured_screen);
    next.configured_layout = spec.configured_layout;
    if new_effective != old_effective {
        let explicit_edit = old.configured_layout != spec.configured_layout;
        // A default edit reaches only workspaces that follow the default and
        // were not changed by the user; an explicit edit replaces the layout.
        if explicit_edit || !old.layout_overridden {
            let _ = next.ratios.set_layout(next.layout, new_effective);
            next.layout = new_effective;
            next.layout_overridden = false;
        }
    }
    next
}

/// Builds the complete mapping for accepting `config` over `old`.
///
/// Pure: nothing in `old` changes. The workspace *name* is the continuity key:
/// a matching workspace keeps identity, user layout and ratios (while the
/// configured layout is unchanged), floating state, order and focus. Added
/// workspaces use the configured layout. Windows of a removed workspace move,
/// in their stable order, to the visible (else first) workspace of the same
/// display, with the main display as fallback. Ignore edits apply eligibility
/// immediately. When the workspace definitions change, the commit step then
/// re-evaluates every managed window with the startup assignment.
///
/// # Errors
///
/// [`ReloadError`] when the candidate is ambiguous or would break an invariant;
/// the caller keeps the old state.
pub fn prepare_reload(old: &TilingState, config: &TilingConfig) -> Result<Mapping, ReloadError> {
    validate_names(config)?;
    let old_default = old.config.as_deref().map_or_else(LayoutType::default, |c| c.default_layout);
    let workspace_rules: Vec<(String, PreparedRules)> = config
        .workspaces
        .iter()
        .map(|ws| (ws.name.clone(), PreparedRules::new(&ws.rules)))
        .collect();
    let ignore = PreparedRules::new(&config.ignore);
    let (released, recheck) = ignore_changes(old, &ignore);

    let mut next_workspace = old.next_workspace;
    let mut workspaces: Vec<WorkspaceState>;
    let mut moved = BTreeMap::new();
    if old.screens.is_empty() {
        // No displays: nothing can be placed. Keep what exists untouched.
        workspaces = old.workspaces.clone();
    } else {
        let wanted = specs(&old.screens, config);
        workspaces = Vec::with_capacity(wanted.len());
        for spec in &wanted {
            if let Some(existing) = old.workspaces.iter().find(|ws| ws.name == spec.name) {
                workspaces.push(carry(existing, spec, old_default, config.default_layout));
            } else {
                workspaces.push(new_workspace(WorkspaceId(next_workspace), spec));
                next_workspace += 1;
            }
        }
        // A generated default stays while its display would otherwise be empty.
        let unlisted: Vec<&WorkspaceState> = old
            .workspaces
            .iter()
            .filter(|ws| !workspaces.iter().any(|n| n.name == ws.name))
            .collect();
        for gone in unlisted {
            if is_generated_default(gone) && !workspaces.iter().any(|n| n.screen == gone.screen) {
                workspaces.push(gone.clone());
            }
        }
        ensure_default_workspaces(&old.screens, &mut workspaces, &mut next_workspace);
        let focused = reconcile(&old.screens, &mut workspaces, old.focus.workspace);
        let focused = focus_heir(old, &mut workspaces).or(focused);
        // Windows of removed workspaces move in stable order.
        let main = primary_screen(&old.screens);
        let removed: Vec<&WorkspaceState> = old
            .workspaces
            .iter()
            .filter(|ws| !workspaces.iter().any(|n| n.id == ws.id))
            .collect();
        for gone in removed {
            let destination = |screen: DisplayId| -> Option<usize> {
                let on = |s: DisplayId| {
                    workspaces
                        .iter()
                        .position(|n| n.screen == s && n.visible)
                        .or_else(|| workspaces.iter().position(|n| n.screen == s))
                };
                on(screen).or_else(|| main.and_then(on))
            };
            let Some(index) = destination(gone.screen) else {
                continue;
            };
            let target = workspaces[index].id;
            for token in &gone.order {
                if !workspaces[index].order.contains(token) {
                    workspaces[index].order.push(*token);
                }
                moved.insert(*token, target);
            }
        }
        let mapping = Mapping {
            workspaces,
            next_workspace,
            moved,
            focused,
            released,
            recheck,
            workspace_rules,
            ignore,
        };
        mapping.check(&old.screens)?;
        return Ok(mapping);
    }
    let focused = old.focus.workspace;
    Ok(Mapping {
        workspaces,
        next_workspace,
        moved,
        focused,
        released,
        recheck,
        workspace_rules,
        ignore,
    })
}

type IgnoreChanges = (Vec<(WindowInstance, ManagementExclusion)>, Vec<WindowInstance>);

/// Windows the new ignore rules newly exclude, and windows to read again.
fn ignore_changes(old: &TilingState, ignore: &PreparedRules) -> IgnoreChanges {
    let mut released = Vec::new();
    for window in old.windows.values() {
        let Some(info) = old.apps.get(&window.application).map(|app| &app.info) else {
            continue;
        };
        let input = crate::rules::ExclusionInput {
            metadata: crate::rules::WindowMetadata {
                app_id: &info.bundle_id,
                app_name: &info.name,
                title: &window.observed.title,
            },
            subrole: window.observed.subrole.as_deref(),
        };
        if let Some(why) = crate::rules::management_exclusion(&input, &old.self_bundle_id, ignore) {
            released.push((window.token, why));
        }
    }
    let recheck = old
        .excluded
        .iter()
        .filter(|(_, why)| matches!(why, ManagementExclusion::IgnoreRule { .. }))
        .map(|(token, _)| *token)
        .collect();
    (released, recheck)
}

impl Mapping {
    /// Verifies the invariants a committed mapping must hold.
    ///
    /// # Errors
    ///
    /// [`ReloadError::Inconsistent`] naming the broken invariant.
    pub fn check(&self, screens: &[ScreenState]) -> Result<(), ReloadError> {
        for workspace in &self.workspaces {
            if !screens.iter().any(|s| s.id == workspace.screen) {
                return Err(ReloadError::Inconsistent("workspace on an absent display"));
            }
        }
        for screen in screens {
            let visible =
                self.workspaces.iter().filter(|w| w.screen == screen.id && w.visible).count();
            if visible != 1 {
                return Err(ReloadError::Inconsistent(
                    "a display needs exactly one visible workspace",
                ));
            }
        }
        if self.workspaces.iter().filter(|w| w.focused).count() > 1 {
            return Err(ReloadError::Inconsistent("more than one focused workspace"));
        }
        let mut seen = BTreeSet::new();
        for token in self.workspaces.iter().flat_map(|w| w.order.iter()) {
            if !seen.insert(*token) {
                return Err(ReloadError::Inconsistent("a window is in two workspaces"));
            }
        }
        if self.moved.values().any(|id| !self.workspaces.iter().any(|w| w.id == *id)) {
            return Err(ReloadError::Inconsistent(
                "a window moves to a missing workspace",
            ));
        }
        Ok(())
    }
}

/// A removed focused workspace hands the focus to the workspace now visible on
/// its display, where the native focus still is. `None` when the focused
/// workspace survives or its display shows none.
fn focus_heir(old: &TilingState, workspaces: &mut [WorkspaceState]) -> Option<WorkspaceId> {
    let screen = old
        .focus
        .workspace
        .filter(|id| !workspaces.iter().any(|n| n.id == *id))
        .and_then(|id| old.workspaces.iter().find(|ws| ws.id == id))?
        .screen;
    let heir = workspaces.iter().find(|n| n.screen == screen && n.visible)?.id;
    for workspace in workspaces.iter_mut() {
        workspace.focused = workspace.id == heir;
    }
    Some(heir)
}

/// A focus snapshot for a mapping (used by the commit step).
#[must_use]
pub fn focus_for(
    workspaces: &[WorkspaceState],
    id: WorkspaceId,
    previous: &FocusState,
) -> FocusState {
    let Some(workspace) = workspaces.iter().find(|w| w.id == id) else {
        return FocusState::default();
    };
    let window = previous
        .window
        .filter(|token| workspace.order.contains(token))
        .or(workspace.focused_window)
        .or_else(|| workspace.order.first().copied());
    FocusState {
        window,
        workspace: Some(id),
        screen: Some(workspace.screen),
    }
}

/// Prepares a reload against the published snapshot (the module's staging hook).
///
/// The snapshot lacks application metadata, so only the workspace structure is
/// validated here; the actor re-prepares against its full state and a rejection
/// there leaves its state unchanged.
///
/// # Errors
///
/// [`ReloadError`] when the structural mapping is refused.
pub fn prepare_reload_from_snapshot(
    snapshot: &TilingSnapshot,
    config: &TilingConfig,
) -> Result<(), ReloadError> {
    prepare_reload(&TilingState::replica(snapshot), config).map(|_| ())
}
