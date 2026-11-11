//! The pure transition function: `(state, input) -> (state', effects)`.
//!
//! Legacy handlers (`actor/handlers/{window,app,screen}.rs`) become the
//! functions below. Differences from legacy are deliberate and listed in
//! `docs/features/tiling.md`: exact tokens replace `(identity, window id)`
//! pairs, a missed destroy is repaired by a reconciliation instead of lingering,
//! and windows seen before any screen exists wait unassigned instead of
//! creating a throwaway workspace.

use std::collections::BTreeSet;

use stache_macos::applications::LifecycleKind;
use stache_macos::ax::{AxError, AxReading};
use stache_macos::windows::{
    ApplicationChange, Capability, DispatchError, FocusOutcome, SnapshotOrigin, SnapshotOutcome,
    StaleReason, TabBarId, TabBarRead, TrackOutcome, WindowChange, WindowEvent, WindowEventKind,
    WindowInstance, WindowSnapshot,
};
use stache_macos::{ApplicationId, DisplayId, DisplayLayout};

use super::{
    AppInfo, AppRecord, Cause, CompletionResult, DesiredWindow, Effect, FOCUS_HISTORY_CAPACITY,
    FocusState, Health, HealthFault, Input, MAX_FULLSCREEN_RECHECKS, MAX_TRACK_RETRIES,
    MAX_UNCONFIRMED_APPS, OVERLOAD_THRESHOLD, ObservedWindow, Scan, Scope, ScreenState, Stamp,
    TOMBSTONE_CAPACITY, TRACK_CHUNK, TilingState, WORKSPACE_HISTORY_CAPACITY, WindowState,
    WorkspaceId, drag, frames, intents, slots, tab_sync,
};
use crate::config::TilingConfig;
use crate::geometry::Rect;
use crate::layout::insert_window;
use crate::presets::FloatingTrigger;
use crate::rules::{
    ExclusionInput, WindowMetadata, find_matching_workspace_prepared, is_system_app_name,
    is_system_bundle, management_exclusion,
};
use crate::workspaces::{
    default_name, ensure_default_workspaces, focus_for, new_workspace, prepare_reload,
    primary_screen, reconcile, resolve_screen, specs,
};

/// Applies one input. Pure: no clock, lock or native call.
#[must_use]
pub fn transition(mut state: TilingState, input: Input) -> (TilingState, Vec<Effect>) {
    let mut effects = Vec::new();
    state.sequence += 1;
    apply(&mut state, input, &mut effects);
    settle(&mut state, &mut effects);
    effects.push(Effect::Publish);
    (state, effects)
}

fn apply(state: &mut TilingState, input: Input, fx: &mut Vec<Effect>) {
    match input {
        Input::Start(stamp) => start(state, stamp, fx),
        Input::Window(event) => window_event(state, event, fx),
        Input::Lifecycle { kind, info } => lifecycle(state, kind, info, fx),
        Input::Activated(application) => {
            state.active_application = Some(application);
            // An application the user brings forward gets another chance.
            retry_unsettled(state, application);
            // A missed Space change must not leave a returned window unlisted.
            relist_unlisted(state, application, false);
            if state.apps.contains_key(&application) {
                // Provisional: only a window already on a visible workspace. The
                // application's real focused window decides a workspace switch.
                focus_application(state, application);
                let tracked = state.apps.get(&application).is_some_and(|r| r.tracked);
                if let (true, Some(stamp)) = (tracked, state.stamp) {
                    state.pending_focus.inflight.insert(application);
                    fx.push(Effect::ReadFocus { stamp, application });
                }
            } else if state.stamp.is_some() && state.reconcile.probed.insert(application) {
                // Unknown to the registry (it was not listed: no pid yet, not
                // regular then, or launched before the watches): list again.
                if state.reconcile.probed.len() > MAX_PROBED_APPS {
                    state.reconcile.probed.clear();
                }
                request_listing(state, fx);
            }
        }
        Input::Relist => request_listing(state, fx),
        Input::Applications { stamp, apps } => {
            if state.stamp == Some(stamp) {
                applications(state, apps, fx);
            } else {
                state.diagnostics.stale_completions += 1;
            }
        }
        Input::Displays(layout) => apply_displays(state, &layout),
        Input::Config { revision, config } => apply_config(state, revision, config, fx),
        Input::Intent(intent) => intents::apply(state, intent, fx),
        Input::Drag(signal) => drag::signal(state, signal, fx),
        Input::SettleExpired(application) => settle_expired(state, application),
        Input::RetryTrack(application) => retry_transient(state, application),
        Input::GeometryBatch(tokens) => {
            let Some(stamp) = state.stamp else { return };
            for token in tokens {
                if state.windows.contains_key(&token) {
                    fx.push(Effect::Snapshot { stamp, window: token });
                }
            }
        }
        Input::Unconfirmed(scope, cause) => mark(state, scope, cause),
        Input::Completion { stamp, result } => {
            if state.stamp == Some(stamp) {
                completion(state, result);
            } else {
                state.diagnostics.stale_completions += 1;
            }
        }
        Input::Fault(fault) => state.health = Health::Failed(fault),
        Input::FrameWrites(enabled) => frames::set_enabled(state, enabled, fx),
        Input::Relayout => frames::relayout(state),
        Input::Reservations(host) => frames::apply_reservations(state, host),
        #[cfg(test)]
        Input::PanicForTest => panic!("injected transition panic"),
    }
}

// ---------------------------------------------------------------- registry

fn start(state: &mut TilingState, stamp: Stamp, fx: &mut Vec<Effect>) {
    if state.stamp == Some(stamp) {
        return;
    }
    if state.stamp.is_some() {
        reset_registry(state);
    }
    state.stamp = Some(stamp);
    state.reconcile.listing = true;
    fx.push(Effect::ListApplications { stamp });
}

/// The most unlisted applications remembered as already probed.
const MAX_PROBED_APPS: usize = 256;

/// Asks for the running-application list, coalescing with one in flight: a list
/// already running may predate the reason, so exactly one more follows it.
fn request_listing(state: &mut TilingState, fx: &mut Vec<Effect>) {
    let Some(stamp) = state.stamp else { return };
    if state.reconcile.listing {
        state.reconcile.relist = true;
    } else {
        state.reconcile.listing = true;
        fx.push(Effect::ListApplications { stamp });
    }
}

/// Drops everything bound to the previous registry; screens and workspaces stay.
fn reset_registry(state: &mut TilingState) {
    state.diagnostics.registry_restarts += 1;
    state.windows.clear();
    state.excluded.clear();
    state.cg_index.clear();
    state.apps.clear();
    state.active_application = None;
    state.pending_focus = super::PendingFocus::default();
    state.focus = FocusState::default();
    state.focus_history.clear();
    state.workspace_history.clear();
    state.tombstones.clear();
    state.tombstone_order.clear();
    state.reconcile = super::Reconcile::default();
    state.startup = super::Startup::default();
    state.last_event_sequence = 0;
    state.completed_once = false;
    state.accessibility = true;
    state.drag.gesture = None;
    frames::reset(state);
    state.tabs.reset();
    state.tab_outbox = super::TabOutbox::default();
    for workspace in &mut state.workspaces {
        workspace.order.clear();
        workspace.focused_window = None;
    }
}

// ------------------------------------------------------------ applications

fn app_skipped(state: &TilingState, info: &AppInfo) -> bool {
    let own = !state.self_bundle_id.is_empty()
        && info.bundle_id.to_lowercase() == state.self_bundle_id.to_lowercase();
    own || is_system_bundle(&info.bundle_id) || is_system_app_name(&info.name)
}

/// Adds a running application. `launched` is true only for a launch event: a
/// failed application is given another attempt then (and on activation), never
/// merely because a routine list named it again.
fn add_app(state: &mut TilingState, info: AppInfo, launched: bool) {
    // macOS reports pid -1 (or 0) for an application that has not finished
    // launching; tracking it can only fail. It is added when a later listing or
    // its launch event carries a real pid.
    if info.pid <= 0 {
        return;
    }
    let application = info.application;
    if let Some(record) = state.apps.get_mut(&application) {
        record.info = info;
        if launched {
            retry_unsettled(state, application);
        }
        return;
    }
    let skipped = app_skipped(state, &info);
    if !skipped {
        // Tracking is chunked by `pump_tracks`; the queue holds at most one
        // entry per running application, so nothing is dropped.
        state.startup.pending.push_back(info.clone());
    }
    state.apps.insert(application, AppRecord {
        info,
        hidden: false,
        skipped,
        tracked: false,
    });
}

/// The user brought `application` forward: provisionally focus its most
/// recently focused window, else its first one, **among windows on a workspace
/// that is already visible**. This never switches a workspace: an application
/// whose windows are all on hidden workspaces keeps the focus as it is until its
/// real focused window is read (`Effect::ReadFocus`) and reported. An
/// application without a tracked window leaves the focus alone too; the first
/// window it adopts takes the focus (see `ingest`).
pub(super) fn focus_application(state: &mut TilingState, application: ApplicationId) {
    let visible: BTreeSet<WorkspaceId> =
        state.workspaces.iter().filter(|ws| ws.visible).map(|ws| ws.id).collect();
    let usable = |token: &WindowInstance| {
        state.windows.get(token).is_some_and(|window| {
            window.application == application
                && !window.observed.minimized
                && !window.is_inactive_tab()
                && !window.suspended
                && window.desired.workspace.is_some_and(|id| visible.contains(&id))
        })
    };
    let remembered = state.focus_history.iter().rev().find(|token| usable(token)).copied();
    let chosen = remembered.or_else(|| {
        state
            .workspaces
            .iter()
            .filter(|workspace| workspace.visible)
            .flat_map(|workspace| workspace.order.iter())
            .find(|token| usable(token))
            .copied()
    });
    if let Some(token) = chosen {
        on_focused(state, token);
    }
}

/// Gives a failed or truncated application another attempt, at most
/// [`MAX_TRACK_RETRIES`] times (triggered by activation or relaunch). Only that
/// application is affected; every other one keeps being laid out.
fn retry_unsettled(state: &mut TilingState, application: ApplicationId) {
    let failed = state.startup.failed.contains(&application);
    let truncated = state.startup.truncated.contains(&application);
    if !(failed || truncated) {
        return;
    }
    let spent = state.startup.retries.entry(application).or_insert(0);
    if *spent >= MAX_TRACK_RETRIES {
        return;
    }
    *spent += 1;
    requeue(state, application);
}

/// The time-based retry of a transient track failure. Bounded by the actor's
/// backoff schedule, so it does not spend [`MAX_TRACK_RETRIES`].
fn retry_transient(state: &mut TilingState, application: ApplicationId) {
    if !state.startup.transient.remove(&application) || !state.startup.failed.contains(&application)
    {
        return;
    }
    requeue(state, application);
}

/// Queues the tracking (or the refresh, once tracked) of a failed application.
fn requeue(state: &mut TilingState, application: ApplicationId) {
    let Some(record) = state.apps.get(&application) else {
        return;
    };
    if record.skipped {
        return;
    }
    if record.tracked {
        state.reconcile.unconfirmed.insert(application);
    } else if !state.startup.awaiting.contains(&application)
        && !state.startup.pending.iter().any(|info| info.application == application)
    {
        let info = record.info.clone();
        state.startup.pending.push_back(info);
    }
}

/// Adopts the authoritative list: new applications are added, vanished ones
/// terminated.
fn applications(state: &mut TilingState, apps: Vec<AppInfo>, fx: &mut Vec<Effect>) {
    state.reconcile.listing = false;
    state.startup.listed = true;
    let relist = std::mem::take(&mut state.reconcile.relist);
    let live: BTreeSet<ApplicationId> = apps.iter().map(|info| info.application).collect();
    let gone: Vec<ApplicationId> =
        state.apps.keys().filter(|app| !live.contains(app)).copied().collect();
    for application in gone {
        terminate_app(state, application, fx);
    }
    for info in apps {
        add_app(state, info, false);
    }
    if let (true, Some(stamp)) = (relist, state.stamp) {
        state.reconcile.listing = true;
        fx.push(Effect::ListApplications { stamp });
    }
}

fn lifecycle(state: &mut TilingState, kind: LifecycleKind, info: AppInfo, fx: &mut Vec<Effect>) {
    let application = info.application;
    match kind {
        LifecycleKind::Launched => add_app(state, info, true),
        LifecycleKind::Terminated => terminate_app(state, application, fx),
        LifecycleKind::TerminatedUnresolved => {
            // The termination named no known instance: the application list is
            // authoritative, so list it again instead of keeping a dead app.
            // The in-flight list may predate this termination: list once more
            // after it, however many arrive meanwhile.
            request_listing(state, fx);
        }
        LifecycleKind::Hidden => set_app_hidden(state, application, true),
        LifecycleKind::Unhidden => set_app_hidden(state, application, false),
        LifecycleKind::SpaceChanged => space_changed(state, fx),
    }
}

/// Whether `window` left `AXWindows` and released its slot with nothing taking
/// its place: suspended, no longer holding the tile, and no listed member of its
/// slot. An inactive native tab is not one: another member of its slot is
/// listed (or it is listed itself and only flagged inactive).
pub(super) fn unlisted_without_replacement(state: &TilingState, window: &WindowState) -> bool {
    window.suspended
        && !window.slot_held
        && window.departing.is_none()
        && !state.windows.values().any(|member| {
            member.token != window.token
                && member.tab_group.is_some()
                && member.tab_group == window.tab_group
                && !member.suspended
        })
}

/// Whether `window` starts (or continues) a re-list series: unlisted without
/// a replacement and not yet taken to be on another Space.
fn relist_candidate(state: &TilingState, window: &WindowState) -> bool {
    unlisted_without_replacement(state, window) && !window.relists_spent
}

/// Lists `application` again when it has a window that left `AXWindows`
/// without a replacement: a window that rejoined it (a native fullscreen exit)
/// is found by the listing and returns to its slot.
///
/// The rule (see `docs/features/tiling.md`): a window freshly unlisted starts a
/// bounded series of listings. Once one series ran out with the window still
/// missing (and not observed fullscreen) it is taken to be on another Space:
/// each later activation lists its application once, never a new series. A
/// Space change always starts a full series again (the window may rejoin
/// `AXWindows` only some time after the change, as Ghostty does on a native
/// fullscreen exit it never reported).
fn relist_unlisted(state: &mut TilingState, application: ApplicationId, space_change: bool) {
    if space_change {
        let spent: Vec<WindowInstance> = state
            .windows
            .values()
            .filter(|w| {
                w.application == application
                    && w.relists_spent
                    && unlisted_without_replacement(state, w)
            })
            .map(|w| w.token)
            .collect();
        for token in spent {
            if let Some(window) = state.windows.get_mut(&token) {
                window.relists_spent = false;
            }
        }
    }
    let mut candidate = false;
    let mut unlisted = false;
    for window in state.windows.values().filter(|w| w.application == application) {
        if unlisted_without_replacement(state, window) {
            unlisted = true;
            candidate |= relist_candidate(state, window);
        }
    }
    if candidate {
        start_relists(state, application);
    } else if unlisted && !state.unlisted_relists.contains_key(&application) {
        mark(state, Scope::App(application), Cause::Unlisted);
    }
}

/// Lists `application` now and starts following it: the window may rejoin
/// `AXWindows` only after the Space change completed, so the application is
/// listed again after the settle delay until it is back (bounded).
fn start_relists(state: &mut TilingState, application: ApplicationId) {
    state.unlisted_relists.insert(application, 0);
    mark(state, Scope::App(application), Cause::Unlisted);
}

/// A listing of `application` completed: while one of its windows is still
/// unlisted without a replacement after a Space change or an activation, it is
/// listed again after the settle delay, at most [`MAX_FULLSCREEN_RECHECKS`]
/// times. A window still missing stays suspended.
fn follow_unlisted(state: &mut TilingState, application: ApplicationId) {
    let Some(&count) = state.unlisted_relists.get(&application) else {
        return;
    };
    let unlisted = state
        .windows
        .values()
        .any(|window| window.application == application && relist_candidate(state, window));
    if !unlisted || count >= MAX_FULLSCREEN_RECHECKS {
        state.unlisted_relists.remove(&application);
        if unlisted {
            // The series ran out: a window still missing that is not observed
            // fullscreen is on another Space; no new series for it.
            let spent: Vec<WindowInstance> = state
                .windows
                .values()
                .filter(|window| {
                    window.application == application
                        && !window.observed.fullscreen
                        && relist_candidate(state, window)
                })
                .map(|window| window.token)
                .collect();
            for token in spent {
                if let Some(window) = state.windows.get_mut(&token) {
                    window.relists_spent = true;
                }
            }
        }
        return;
    }
    state.unlisted_relists.insert(application, count + 1);
    state.tab_outbox.settle.entry(application).or_insert(false);
}

/// The active Space changed (native fullscreen always enters or leaves one):
/// list again every application with a window unlisted without a replacement,
/// and read again every window observed fullscreen, since nothing else reports
/// either change.
fn space_changed(state: &mut TilingState, fx: &mut Vec<Effect>) {
    let applications: BTreeSet<ApplicationId> = state
        .windows
        .values()
        .filter(|window| unlisted_without_replacement(state, window))
        .map(|window| window.application)
        .collect();
    for application in applications {
        relist_unlisted(state, application, true);
    }
    let Some(stamp) = state.stamp else { return };
    for window in state.windows.values() {
        if window.observed.fullscreen && !window.suspended {
            fx.push(Effect::Snapshot { stamp, window: window.token });
        }
    }
}

fn set_app_hidden(state: &mut TilingState, application: ApplicationId, hidden: bool) {
    // External hide/unhide is only observed; nothing here is runtime-owned.
    let Some(record) = state.apps.get_mut(&application) else {
        return;
    };
    record.hidden = hidden;
    for window in state.windows.values_mut().filter(|w| w.application == application) {
        window.observed.hidden = hidden;
    }
}

fn terminate_app(state: &mut TilingState, application: ApplicationId, fx: &mut Vec<Effect>) {
    let tokens: Vec<WindowInstance> = state
        .windows
        .values()
        .filter(|w| w.application == application)
        .map(|w| w.token)
        .collect();
    for token in tokens {
        destroy(state, token);
    }
    state.excluded.retain(|token, _| token.application() != application);
    state.tabs.forget_app(application);
    state.tab_outbox.settle.remove(&application);
    state.unlisted_relists.remove(&application);
    state.reconcile.unsettled.remove(&application);
    let tracked = state.apps.remove(&application).is_some_and(|record| record.tracked);
    state.enhanced_ui.remove(&application);
    if let (true, Some(stamp)) = (tracked, state.stamp) {
        fx.push(Effect::Untrack { stamp, application });
    }
    state.startup.pending.retain(|info| info.application != application);
    state.startup.awaiting.remove(&application);
    state.startup.failed.remove(&application);
    state.startup.transient.remove(&application);
    state.startup.failure_reasons.remove(&application);
    state.startup.retries.remove(&application);
    state.startup.truncated.remove(&application);
    state.reconcile.unconfirmed.remove(&application);
    state.reconcile.inflight.remove(&application);
    state.reconcile.scans.remove(&application);
    if state.active_application == Some(application) {
        state.active_application = None;
    }
}

// ----------------------------------------------------------------- windows

fn window_event(state: &mut TilingState, event: WindowEvent, fx: &mut Vec<Effect>) {
    let Some(stamp) = state.stamp else {
        state.diagnostics.stale_epoch_events += 1;
        return;
    };
    if event.epoch != stamp.epoch {
        state.diagnostics.stale_epoch_events += 1;
        return;
    }
    state.last_event_sequence = state.last_event_sequence.max(event.sequence);
    match event.kind {
        WindowEventKind::Snapshot { snapshot, origin } => {
            ingest(state, &snapshot, Some(origin), Some(event.sequence));
            if origin == SnapshotOrigin::Created {
                request_settle(state, snapshot.window.application());
            }
        }
        WindowEventKind::Window { window, change } => window_change(state, window, change, fx),
        WindowEventKind::Application { application, change } => match change {
            ApplicationChange::Hidden => set_app_hidden(state, application, true),
            ApplicationChange::Shown => set_app_hidden(state, application, false),
            // The window-level focus/main events carry the token; these carry
            // none, so the frontmost application's window is chosen from what
            // is known (a background application's own focus is not the user's).
            ApplicationChange::FocusedWindowChanged | ApplicationChange::MainWindowChanged => {
                request_settle(state, application);
                if state.active_application == Some(application) {
                    focus_application(state, application);
                    // The real focused window may be one not adopted yet: read it.
                    state.pending_focus.reads.insert(application);
                }
            }
        },
        WindowEventKind::EnumerationComplete { application, truncated, .. } => {
            finish_scan(state, application, truncated);
        }
        WindowEventKind::EnumerationUnsettled { application, .. } => {
            // Listed windows were still in transition: nothing is decided about
            // held-back windows until another listing, after a settle delay.
            state.reconcile.unsettled.insert(application);
            state.tab_outbox.settle.insert(application, true);
        }
        WindowEventKind::EnumerationAborted { application, reason } => {
            state.reconcile.inflight.remove(&application);
            state.reconcile.scans.remove(&application);
            state.reconcile.unsettled.remove(&application);
            state.unlisted_relists.remove(&application);
            slots::resolve_returns(state, application);
            close_departing(state, application, None);
            state.startup.failed.insert(application);
            state
                .startup
                .failure_reasons
                .insert(application, format!("enumeration aborted: {reason:?}"));
            state.diagnostics.enumeration_failures += 1;
        }
        WindowEventKind::ObserverOverflow { application } => {
            mark(state, Scope::App(application), Cause::ObserverOverflow);
        }
        WindowEventKind::CapabilityChanged {
            capability: Capability::Accessibility,
            available,
        } => {
            set_accessibility(state, available);
        }
        WindowEventKind::EnhancedUi { application, note } => {
            state.enhanced_ui.entry(application).or_default().record(note);
        }
    }
}

fn set_accessibility(state: &mut TilingState, available: bool) {
    let restored = available && !state.accessibility;
    state.accessibility = available;
    if !available {
        // Without Accessibility no frame can be read: the gesture cannot be
        // decided.
        drag::lost(state, crate::drag::LostCause::PermissionLost);
    }
    if restored {
        // Applications that failed for lack of permission are tracked again.
        let failed: Vec<ApplicationId> =
            std::mem::take(&mut state.startup.failed).into_iter().collect();
        for application in failed {
            if let Some(record) = state.apps.get_mut(&application) {
                record.tracked = false;
                let info = record.info.clone();
                state.startup.pending.push_back(info);
            }
        }
    }
}

fn window_change(
    state: &mut TilingState,
    token: WindowInstance,
    change: WindowChange,
    fx: &mut Vec<Effect>,
) {
    let Some(stamp) = state.stamp else { return };
    if matches!(change, WindowChange::Focused | WindowChange::Main) {
        request_settle(state, token.application());
    }
    match change {
        WindowChange::Destroyed => destroy_or_depart(state, token),
        WindowChange::Suspended => slots::suspend(state, token),
        WindowChange::Miniaturized | WindowChange::Deminiaturized => {
            let minimized = change == WindowChange::Miniaturized;
            if let Some(window) = state.windows.get_mut(&token) {
                window.observed.minimized = minimized;
            }
        }
        // A suspended window is not on screen: it takes no focus and no reads.
        WindowChange::Focused
        | WindowChange::TitleChanged
        | WindowChange::Moved
        | WindowChange::Resized
            if state.windows.get(&token).is_some_and(|window| window.suspended) => {}
        WindowChange::Focused => {
            if state.windows.contains_key(&token) {
                on_focused(state, token);
                reconcile_tabs(state, stamp, token, fx);
            } else if !state.excluded.contains_key(&token) && !state.tombstones.contains(&token) {
                // A focus for a window never seen: adopt it by reading it.
                state.diagnostics.unknown_focus += 1;
                buffer_focus(state, token);
                fx.push(Effect::Snapshot { stamp, window: token });
            }
        }
        WindowChange::TitleChanged => {
            if state.windows.contains_key(&token) || state.excluded.contains_key(&token) {
                fx.push(Effect::Snapshot { stamp, window: token });
            }
        }
        WindowChange::Moved | WindowChange::Resized => {
            if state.windows.contains_key(&token) {
                fx.push(Effect::Snapshot { stamp, window: token });
            }
        }
        // Main-window changes carry no state, but a tab switch changes the main
        // window: with tab reads on, re-read that one window.
        WindowChange::Main => reconcile_tabs(state, stamp, token, fx),
    }
}

/// The tab group change hint: one single-window snapshot of the exact window,
/// only while tab reads are on. Never a whole-application refresh.
fn reconcile_tabs(state: &TilingState, stamp: Stamp, token: WindowInstance, fx: &mut Vec<Effect>) {
    if state.tabs.reading() && state.windows.contains_key(&token) {
        fx.push(Effect::Snapshot { stamp, window: token });
    }
}

fn observed_from(snapshot: &WindowSnapshot, hidden: bool) -> ObservedWindow {
    let text = |reading: &AxReading<String>| match reading {
        AxReading::Value(value) => Some(value.clone()),
        _ => None,
    };
    let flag = |reading: &AxReading<bool>| matches!(reading, AxReading::Value(true));
    ObservedWindow {
        title: text(&snapshot.title).unwrap_or_default(),
        role: text(&snapshot.role),
        subrole: text(&snapshot.subrole),
        frame: snapshot.frame.complete().map(|frame| {
            Rect::new(
                frame.origin.x,
                frame.origin.y,
                frame.size.width,
                frame.size.height,
            )
        }),
        fullscreen: flag(&snapshot.fullscreen),
        minimized: flag(&snapshot.minimized),
        hidden,
        tabs: snapshot.tabs,
        observed: snapshot.observed,
    }
}

fn exclusion_for(
    state: &TilingState,
    application: ApplicationId,
    observed: &ObservedWindow,
) -> Option<crate::rules::ManagementExclusion> {
    let info = &state.apps.get(&application)?.info;
    let input = ExclusionInput {
        metadata: WindowMetadata {
            app_id: &info.bundle_id,
            app_name: &info.name,
            title: &observed.title,
        },
        subrole: observed.subrole.as_deref(),
    };
    management_exclusion(&input, &state.self_bundle_id, &state.ignore)
}

/// Applies a snapshot. One path for startup, reconciliation and live events,
/// so exclusions behave identically everywhere.
#[allow(
    clippy::too_many_lines,
    reason = "one linear path: known window, exclusion, tab continuity, new window"
)]
fn ingest(
    state: &mut TilingState,
    snapshot: &WindowSnapshot,
    origin: Option<SnapshotOrigin>,
    adopted_seq: Option<u64>,
) {
    let token = snapshot.window;
    if state.tombstones.contains(&token) {
        state.diagnostics.ignored_after_destroy += 1;
        return;
    }
    if state.stamp.is_none_or(|stamp| stamp.epoch != token.epoch()) {
        state.diagnostics.stale_epoch_events += 1;
        return;
    }
    let application = token.application();
    let Some(record) = state.apps.get(&application) else {
        state.diagnostics.unknown_app_windows += 1;
        return;
    };
    let hidden = record.hidden;
    if origin == Some(SnapshotOrigin::Enumeration)
        && let Some(scan) = state.reconcile.scans.get_mut(&application)
    {
        scan.seen.insert(token);
    }
    let mut observed = observed_from(snapshot, hidden);
    // A read completing after an animation settled can show a position from the
    // motion; it is our own echo, so the confirmed frame is kept (bounded).
    if origin.is_none() && frames::absorb_echo(state, token, observed.frame) {
        observed.frame = state.windows.get(&token).and_then(|w| w.observed.frame);
    } else if origin.is_none() && !observed.fullscreen {
        // A native fullscreen frame is not a minimum the window re-asserts.
        frames::observe_growth(state, token, observed.frame);
    }
    let exclusion = exclusion_for(state, application, &observed);
    // A window adopted by a creation or a read, or by a listing once startup is
    // over (a creation held back as unsettled is adopted by the next listing),
    // may be the one that just took the focus.
    let live = origin != Some(SnapshotOrigin::Enumeration) || state.completed_once;

    if let Some(window) = state.windows.get_mut(&token) {
        let keep_title = matches!(snapshot.title, AxReading::Value(_));
        let title = std::mem::take(&mut window.observed.title);
        window.observed = observed;
        if !keep_title {
            window.observed.title = title;
        }
        window.cg_window_id = snapshot.cg_window_id.or(window.cg_window_id);
        window.tab_bar = merged_tab_bar(window.tab_bar, snapshot.tab_bar);
        if let Some(why) = exclusion {
            untrack(state, token);
            state.excluded.insert(token, why);
            state.diagnostics.excluded_windows += 1;
            return;
        }
        tab_sync::observe(state, snapshot, false);
        slots::recheck_fullscreen(state, token, false);
        // A suspended window listed again gets its slot back: at once for a
        // single read, after the whole listing otherwise (the window it replaces
        // may be reported unlisted later in the same listing).
        slots::mark_returning(state, token);
        if origin != Some(SnapshotOrigin::Enumeration) {
            slots::resolve_return(state, token, true);
        }
        return;
    }
    let exclusion = exclusion.or_else(|| {
        crate::rules::adoption_exclusion(
            observed.subrole.as_deref(),
            observed.frame.map(|frame| (frame.width, frame.height)),
        )
    });
    if let Some(why) = exclusion {
        if state.excluded.insert(token, why).is_none() {
            state.diagnostics.excluded_windows += 1;
        }
        return;
    }
    state.excluded.remove(&token);
    // A window carrying the tab bar of a tracked window of the same
    // application is that window's tab group seen through another window:
    // AppKit swaps the window that represents the group on a new tab, a tab
    // switch and a tab close. It takes the slot over instead of being a new tile.
    if let TabBarRead::Bar(bar) = snapshot.tab_bar
        && let Some(old) = tab_predecessor(state, application, bar.id, token)
    {
        hand_over(state, old, snapshot, observed, adopted_seq);
        if live {
            adopted_focus(state, token);
        }
        return;
    }
    if let Some(cg) = snapshot.cg_window_id {
        // A numeric CGWindowID is metadata: a different token owning it is stale.
        if let Some(old) = state.cg_index.insert(cg, token)
            && old != token
        {
            destroy(state, old);
            state.diagnostics.cg_reuse_evictions += 1;
        }
    }
    state.windows.insert(token, WindowState {
        token,
        application,
        cg_window_id: snapshot.cg_window_id,
        desired: DesiredWindow::default(),
        observed,
        adopted_seq,
        inactive_tab: false,
        tab_bar: merged_tab_bar(None, snapshot.tab_bar),
        departing: None,
        suspended: false,
        slot_held: false,
        tab_group: None,
        arrival: matches!(snapshot.tab_bar, TabBarRead::Bar(bar) if bar.count >= 2),
        returning: false,
        fullscreen_rechecks: 0,
        relists_spent: false,
    });
    assign(state, token);
    // A new tabbed window that replaces the one window of the application that
    // just left `AXWindows` alive takes that window's slot.
    if state.windows.get(&token).is_some_and(|window| window.arrival)
        && slots::pair(state, application)
    {
        tab_sync::observe(state, snapshot, false);
        if live {
            adopted_focus(state, token);
        }
        return;
    }
    // The application that was frontmost before any window was known takes the
    // focus with its first window (startup has no focus notification). During
    // the startup listing it also takes it from another application's window:
    // that focus can only be a default (first window of the default workspace),
    // and the frontmost application is the system's own answer.
    let held = state.focus.window.and_then(|held| state.windows.get(&held));
    let unclaimed = held.is_none_or(|held| !live && held.application != application);
    if unclaimed && state.active_application == Some(application) {
        on_focused(state, token);
    }
    if live {
        adopted_focus(state, token);
    }
    tab_sync::observe(state, snapshot, true);
    // Only a genuinely new window is a new-window event; adoption of an
    // existing one never positions anything.
    let trigger = if origin == Some(SnapshotOrigin::Created) {
        FloatingTrigger::NewWindow
    } else {
        FloatingTrigger::InitialAdoption
    };
    intents::position_new_floating(state, token, trigger);
}

// ------------------------------------------------------------ pending focus

/// Remembers a focus notification for a window that is not tracked yet, so the
/// exact window takes the focus when it is adopted. One per application, latest
/// wins, bounded and short-lived.
fn buffer_focus(state: &mut TilingState, token: WindowInstance) {
    if state.pending_focus.buffered.len() >= super::MAX_PENDING_FOCUS {
        state.pending_focus.buffered.clear();
    }
    state
        .pending_focus
        .buffered
        .insert(token.application(), (token, state.sequence));
}

/// A window was adopted (or took over a slot): it takes the focus when its
/// notification was buffered, else the frontmost application's real focused
/// window is read. Identity only; a guess never switches a workspace.
pub(super) fn adopted_focus(state: &mut TilingState, token: WindowInstance) {
    let application = token.application();
    let buffered = state.pending_focus.buffered.remove(&application);
    let fresh = buffered.is_some_and(|(held, at)| {
        held == token && state.sequence.saturating_sub(at) <= super::PENDING_FOCUS_LIFE
    });
    if fresh {
        on_focused(state, token);
    } else if state.active_application == Some(application) {
        state.pending_focus.reads.insert(application);
    }
}

/// Emits the focus reads requested during this transition.
fn issue_focus_reads(state: &mut TilingState, fx: &mut Vec<Effect>) {
    let reads = std::mem::take(&mut state.pending_focus.reads);
    let Some(stamp) = state.stamp else { return };
    for application in reads {
        if state.apps.get(&application).is_some_and(|record| record.tracked) {
            state.pending_focus.inflight.insert(application);
            fx.push(Effect::ReadFocus { stamp, application });
        }
    }
}

// ---------------------------------------------------------------- tab groups

/// The tab bar a window carries after a snapshot: a bar read replaces it, "no
/// bar" clears it, anything unknown (not read, unreadable) keeps what was known.
const fn merged_tab_bar(
    known: Option<stache_macos::windows::TabBar>,
    read: TabBarRead,
) -> Option<stache_macos::windows::TabBar> {
    match read {
        TabBarRead::Bar(bar) => Some(bar),
        TabBarRead::None => None,
        TabBarRead::NotRead | TabBarRead::Unreadable => known,
    }
}

/// The one tracked window of `application` that carries tab bar `id`, when
/// there is exactly one: nothing else (frame, title, `CGWindowID`) is compared,
/// and an ambiguous match is no match.
fn tab_predecessor(
    state: &TilingState,
    application: ApplicationId,
    id: TabBarId,
    token: WindowInstance,
) -> Option<WindowInstance> {
    let mut carriers = state.windows.values().filter(|window| {
        window.application == application
            && window.token != token
            && window.holds_slot()
            && window.tab_bar.is_some_and(|bar| bar.id == id)
    });
    let found = carriers.next()?.token;
    carriers.next().is_none().then_some(found)
}

/// Asks the shell for one settled listing of an application with a native tab
/// group: `AppKit` swaps the window that represents the group without telling.
fn request_settle(state: &mut TilingState, application: ApplicationId) {
    let tabbed = state
        .windows
        .values()
        .any(|window| window.application == application && window.tab_bar.is_some());
    if tabbed {
        state.tab_outbox.settle.entry(application).or_insert(false);
    }
}

/// Destroys a window, unless it belongs to a tab group with other tabs: then its
/// slot is held until the listing settles and either another window takes the
/// tab bar over (a close of one tab) or no window does (the last tab closed).
fn destroy_or_depart(state: &mut TilingState, token: WindowInstance) {
    let sequence = state.last_event_sequence;
    let Some(window) = state.windows.get(&token) else {
        destroy(state, token);
        return;
    };
    if window.departing.is_some() {
        return;
    }
    // A suspended member without the tile closes silently: it held nothing.
    let silent = window.suspended && !window.slot_held;
    // A slot with suspended members hands its tile to the member that is listed
    // next, so the closing active member is held back like a tab of a bar.
    let mates = window.tab_group.is_some_and(|group| {
        state.windows.values().any(|member| {
            member.token != token && member.tab_group == Some(group) && member.suspended
        })
    });
    let tabbed = window.tab_bar.is_some_and(|bar| bar.count >= 2);
    if !silent && (mates || tabbed) {
        if let Some(window) = state.windows.get_mut(&token) {
            window.departing = Some(sequence);
        }
        state.diagnostics.tab_departures_held += 1;
        state.tab_outbox.settle.entry(token.application()).or_insert(false);
    } else {
        destroy(state, token);
    }
}

/// Closes held-back windows of `application` for real. With a `floor`, only the
/// ones held back at or before it (a listing requested later cannot have seen a
/// successor of a window that left after it was requested).
fn close_departing(state: &mut TilingState, application: ApplicationId, floor: Option<u64>) {
    let closing: Vec<WindowInstance> = state
        .windows
        .values()
        .filter(|window| {
            window.application == application
                && window.departing.is_some_and(|held| floor.is_none_or(|floor| held <= floor))
        })
        .map(|window| window.token)
        .collect();
    for token in closing {
        destroy(state, token);
        state.diagnostics.tab_departures_closed += 1;
    }
}

fn settle_expired(state: &mut TilingState, application: ApplicationId) {
    state.reconcile.unsettled.remove(&application);
    slots::resolve_returns(state, application);
    close_departing(state, application, None);
    slots::settle(state, application);
}

/// `new` carries the tab bar of the tracked window `old`: it takes over the
/// group's slot. Same workspace, same position in the workspace order, same
/// floating and positioned flags, same focus and focus-history entries and the
/// same frame record (so nothing is written unless the new window is off its
/// target).
///
/// `old` is dead when its destroy was held back: it is tombstoned and never
/// authorizes again. Otherwise it merely left `AXWindows` (another tab of the
/// group is selected now): it stays tracked as a suspended member of the slot,
/// so selecting it again finds it by element identity.
fn hand_over(
    state: &mut TilingState,
    old: WindowInstance,
    snapshot: &WindowSnapshot,
    observed: ObservedWindow,
    adopted_seq: Option<u64>,
) {
    let new = snapshot.window;
    let Some(previous) = state.windows.get(&old) else {
        return;
    };
    let dead = previous.departing.is_some();
    let group = match previous.tab_group {
        Some(group) => Some(group),
        None if dead => None,
        None => Some(slots::mint_group(state)),
    };
    let Some(previous) = state.windows.get(&old).cloned() else {
        return;
    };
    state.windows.insert(new, WindowState {
        token: new,
        application: previous.application,
        cg_window_id: snapshot.cg_window_id,
        desired: previous.desired,
        observed,
        adopted_seq,
        inactive_tab: false,
        tab_bar: merged_tab_bar(previous.tab_bar, snapshot.tab_bar),
        departing: None,
        suspended: false,
        slot_held: false,
        tab_group: group,
        arrival: false,
        returning: false,
        fullscreen_rechecks: 0,
        relists_spent: false,
    });
    for workspace in &mut state.workspaces {
        for held in &mut workspace.order {
            if *held == old {
                *held = new;
            }
        }
        if workspace.focused_window == Some(old) {
            workspace.focused_window = Some(new);
        }
    }
    if state.focus.window == Some(old) {
        state.focus.window = Some(new);
    }
    for held in &mut state.focus_history {
        if *held == old {
            *held = new;
        }
    }
    // The history keeps one entry per window: the replacement can already be
    // there only if it was focused before it was seen, which cannot happen for a
    // fresh token, but keep the invariant cheap.
    let mut seen = BTreeSet::new();
    state.focus_history.retain(|held| seen.insert(*held));
    frames::transfer(state, old, new);
    if dead {
        state.windows.remove(&old);
        state.cg_index.retain(|_, held| *held != old);
        for scan in state.reconcile.scans.values_mut() {
            scan.seen.remove(&old);
        }
        tab_sync::forget_window(state, old, previous.cg_window_id);
        state.excluded.remove(&old);
        tombstone(state, old);
    } else if let Some(window) = state.windows.get_mut(&old) {
        window.desired = DesiredWindow::default();
        window.suspended = true;
        window.relists_spent = false;
        window.slot_held = false;
        window.returning = false;
        window.arrival = false;
        window.tab_group = group;
    }
    if let Some(cg) = snapshot.cg_window_id {
        state.cg_index.insert(cg, new);
    }
    state.diagnostics.tab_handovers += 1;
    if state.tab_outbox.handovers.len() < super::MAX_PENDING_HANDOVERS {
        state.tab_outbox.handovers.push((old, new));
    }
    tab_sync::observe(state, snapshot, false);
}

// -------------------------------------------------------------- assignment

fn choose_workspace(state: &TilingState, token: WindowInstance) -> Option<WorkspaceId> {
    let window = state.windows.get(&token)?;
    let info = &state.apps.get(&window.application)?.info;
    let metadata = WindowMetadata {
        app_id: &info.bundle_id,
        app_name: &info.name,
        title: &window.observed.title,
    };
    // 1. The first matching configured workspace rule.
    let rules = state.workspace_rules.iter().map(|(_, rules)| rules);
    if let Some(found) = find_matching_workspace_prepared(&metadata, rules)
        && let Some(workspace) = state
            .workspaces
            .iter()
            .find(|ws| ws.name == state.workspace_rules[found.workspace_index].0)
    {
        return Some(workspace.id);
    }
    // 2. The focused, else visible, workspace on the window's own display, then
    //    that display's default (its first workspace).
    let own = window.observed.frame.and_then(|frame| {
        state.layout.display_for_ax_rect(stache_macos::applications::DisplayRect::new(
            frame.x,
            frame.y,
            frame.width,
            frame.height,
        ))
    });
    if let Some(display) = own {
        let on: Vec<_> = state.workspaces.iter().filter(|ws| ws.screen == display).collect();
        let chosen = on
            .iter()
            .find(|ws| Some(ws.id) == state.focus.workspace)
            .or_else(|| on.iter().find(|ws| ws.visible))
            .or_else(|| on.first());
        if let Some(workspace) = chosen {
            return Some(workspace.id);
        }
    }
    // 3. No known display: focused workspace, first visible, any (legacy).
    state
        .focus
        .workspace
        .filter(|id| state.workspaces.iter().any(|ws| ws.id == *id))
        .or_else(|| state.workspaces.iter().find(|ws| ws.visible).map(|ws| ws.id))
        .or_else(|| state.workspaces.first().map(|ws| ws.id))
}

pub(super) fn assign(state: &mut TilingState, token: WindowInstance) {
    let Some(id) = choose_workspace(state, token) else {
        return;
    };
    let focused = state.focus.window.filter(|focused| {
        state.windows.get(focused).is_some_and(|w| w.desired.workspace == Some(id))
    });
    let Some(workspace) = state.workspaces.iter_mut().find(|ws| ws.id == id) else {
        return;
    };
    insert_window(&mut workspace.order, token, focused.as_ref(), workspace.layout);
    if let Some(window) = state.windows.get_mut(&token) {
        window.desired.workspace = Some(id);
    }
}

fn assign_unassigned(state: &mut TilingState) {
    let tokens: Vec<WindowInstance> = state
        .windows
        .values()
        .filter(|w| w.desired.workspace.is_none() && !w.suspended)
        .map(|w| w.token)
        .collect();
    for token in tokens {
        assign(state, token);
    }
}

// ------------------------------------------------------------------ removal

fn tombstone(state: &mut TilingState, token: WindowInstance) {
    if state.tombstones.insert(token) {
        state.tombstone_order.push_back(token);
        while state.tombstone_order.len() > TOMBSTONE_CAPACITY {
            if let Some(old) = state.tombstone_order.pop_front() {
                state.tombstones.remove(&old);
            }
        }
    }
}

/// Removes a token from every cache, order and history; the token is then dead.
pub(super) fn destroy(state: &mut TilingState, token: WindowInstance) {
    untrack(state, token);
    state.excluded.remove(&token);
    tombstone(state, token);
}

/// Removes a tracked window from the state (tokens may later reappear only via
/// a fresh snapshot, e.g. after a title rule stops excluding it).
pub(super) fn untrack(state: &mut TilingState, token: WindowInstance) {
    let Some(window) = state.windows.remove(&token) else {
        return;
    };
    frames::forget(state, token);
    tab_sync::forget_window(state, token, window.cg_window_id);
    state.focus_history.retain(|held| *held != token);
    state.cg_index.retain(|_, held| *held != token);
    for scan in state.reconcile.scans.values_mut() {
        scan.seen.remove(&token);
    }
    if let Some(id) = window.desired.workspace
        && let Some(workspace) = state.workspaces.iter_mut().find(|ws| ws.id == id)
    {
        workspace.order.retain(|held| *held != token);
        if workspace.focused_window == Some(token) {
            workspace.focused_window = state
                .focus_history
                .iter()
                .rev()
                .find(|held| workspace.order.contains(held))
                .copied();
        }
    }
    if state.focus.window == Some(token) {
        // The focused workspace stays; its next remembered window takes over.
        state.focus.window = state
            .focus
            .workspace
            .and_then(|id| state.workspaces.iter().find(|ws| ws.id == id))
            .and_then(|ws| ws.focused_window);
    }
}

// -------------------------------------------------------------------- focus

pub(super) fn on_focused(state: &mut TilingState, token: WindowInstance) {
    let Some(workspace_id) = state.windows.get(&token).and_then(|w| w.desired.workspace) else {
        state.focus.window = Some(token);
        return;
    };
    if state.focus.window == Some(token) && state.focus.workspace == Some(workspace_id) {
        return;
    }
    let Some(screen) = state.workspaces.iter().find(|ws| ws.id == workspace_id).map(|ws| ws.screen)
    else {
        return;
    };
    if let Some(previous) = state.focus.workspace.filter(|previous| *previous != workspace_id) {
        state.workspace_history.retain(|held| *held != previous);
        state.workspace_history.push_back(previous);
        while state.workspace_history.len() > WORKSPACE_HISTORY_CAPACITY {
            state.workspace_history.pop_front();
        }
    }
    state.focus = FocusState {
        window: Some(token),
        workspace: Some(workspace_id),
        screen: Some(screen),
    };
    state.focus_history.retain(|held| *held != token);
    state.focus_history.push_back(token);
    while state.focus_history.len() > FOCUS_HISTORY_CAPACITY {
        state.focus_history.pop_front();
    }
    for workspace in &mut state.workspaces {
        let target = workspace.id == workspace_id;
        if target {
            workspace.focused_window = Some(token);
        }
        // Only the focused workspace is visible on its screen; other screens
        // keep their own visible workspace.
        if workspace.screen == screen {
            workspace.visible = target;
        }
        workspace.focused = target;
    }
}

// --------------------------------------------------------------- enumeration

fn finish_scan(state: &mut TilingState, application: ApplicationId, truncated: bool) {
    state.reconcile.inflight.remove(&application);
    state.startup.failed.remove(&application);
    state.startup.transient.remove(&application);
    state.startup.failure_reasons.remove(&application);
    let unsettled = state.reconcile.unsettled.remove(&application);
    // Windows listed again take their slots before held-back windows close.
    slots::resolve_returns(state, application);
    if truncated {
        state.startup.truncated.insert(application);
        state.diagnostics.enumeration_failures += 1;
    } else {
        state.startup.retries.remove(&application);
        state.startup.truncated.remove(&application);
        // The initial tab scan of an application is its first complete
        // enumeration; later changes reconcile through single-window reads.
        state.tabs.scan_complete(application);
    }
    if let Some(scan) = state.reconcile.scans.remove(&application)
        && !truncated
    {
        // A listing that skipped windows in transition proves nothing about the
        // windows held back: another listing follows after a settle delay.
        if !unsettled {
            close_departing(state, application, Some(scan.floor));
            slots::settle(state, application);
        }
        // A window adopted before the scan began and absent from it is gone: its
        // destroy was missed. Windows adopted after the floor are kept.
        let missing: Vec<WindowInstance> = state
            .windows
            .values()
            .filter(|w| {
                w.application == application
                    && w.departing.is_none()
                    && !w.suspended
                    && w.adopted_seq.is_some_and(|seq| seq <= scan.floor)
                    && !scan.seen.contains(&w.token)
            })
            .map(|w| w.token)
            .collect();
        for token in missing {
            destroy_or_depart(state, token);
            if !state.windows.contains_key(&token) {
                state.diagnostics.missed_destroys_repaired += 1;
            }
        }
    }
    state.diagnostics.reconciliations_completed += 1;
    if state.reconcile.unconfirmed.is_empty() && !state.reconcile.all {
        state.reconcile.consecutive_overflows = 0;
    }
    follow_unlisted(state, application);
}

pub(super) fn mark(state: &mut TilingState, scope: Scope, cause: Cause) {
    state.diagnostics.unconfirmed_markings += 1;
    if matches!(
        cause,
        Cause::SemanticOverflow | Cause::SinkOverflow | Cause::GeometryOverflow
    ) {
        state.reconcile.consecutive_overflows += 1;
    }
    match scope {
        Scope::All => state.reconcile.all = true,
        Scope::App(application) => {
            if state.reconcile.unconfirmed.len() >= MAX_UNCONFIRMED_APPS {
                state.reconcile.all = true;
            } else {
                state.reconcile.unconfirmed.insert(application);
            }
        }
    }
}

/// Issues exactly one refresh per unconfirmed application that has none in
/// flight; a marking that arrives during a scan reruns once after it.
fn schedule_reconcile(state: &mut TilingState, fx: &mut Vec<Effect>) {
    let Some(stamp) = state.stamp else { return };
    if state.reconcile.all {
        state.reconcile.all = false;
        let tracked: Vec<ApplicationId> =
            state.apps.iter().filter(|(_, r)| r.tracked).map(|(app, _)| *app).collect();
        state.reconcile.unconfirmed.extend(tracked);
        if !state.reconcile.listing {
            state.reconcile.listing = true;
            fx.push(Effect::ListApplications { stamp });
        }
    }
    let ready: Vec<ApplicationId> = state
        .reconcile
        .unconfirmed
        .iter()
        .filter(|app| !state.reconcile.inflight.contains(app))
        .copied()
        .collect();
    for application in ready {
        state.reconcile.unconfirmed.remove(&application);
        if !state.apps.get(&application).is_some_and(|record| record.tracked) {
            continue;
        }
        state.reconcile.inflight.insert(application);
        state.reconcile.scans.insert(application, Scan {
            floor: state.last_event_sequence,
            seen: BTreeSet::new(),
        });
        state.diagnostics.reconciliations_started += 1;
        fx.push(Effect::Refresh { stamp, application });
    }
}

fn pump_tracks(state: &mut TilingState, fx: &mut Vec<Effect>) {
    let Some(stamp) = state.stamp else { return };
    while state.startup.awaiting.len() < TRACK_CHUNK {
        let Some(info) = state.startup.pending.pop_front() else {
            break;
        };
        if !state.apps.contains_key(&info.application) {
            continue;
        }
        state.startup.awaiting.insert(info.application);
        // The scan starts when the request is issued, not when it is acknowledged:
        // the enumeration's events may overtake the acknowledgement.
        state.reconcile.inflight.insert(info.application);
        state.reconcile.scans.insert(info.application, Scan {
            floor: state.last_event_sequence,
            seen: BTreeSet::new(),
        });
        fx.push(Effect::Track {
            stamp,
            application: info.application,
            pid: info.pid,
        });
    }
}

fn settle(state: &mut TilingState, fx: &mut Vec<Effect>) {
    issue_focus_reads(state, fx);
    pump_tracks(state, fx);
    schedule_reconcile(state, fx);
    state.diagnostics.overloaded = state.reconcile.consecutive_overflows >= OVERLOAD_THRESHOLD;
    if state.phase() == super::Phase::Complete && !state.completed_once {
        state.completed_once = true;
        // Startup guessed the focus from the listing order. Every window is
        // known now, so the frontmost application's real focused window decides
        // (and its workspace becomes the visible one) before anything is hidden.
        if let Some(application) = state.active_application {
            state.pending_focus.reads.insert(application);
            issue_focus_reads(state, fx);
        }
    }
    frames::plan_and_issue(state, fx);
}

// -------------------------------------------------------------- completions

/// A failure an application may recover from by itself: its accessibility
/// interface is not ready yet (just launched), busy, or did not answer in time.
/// Missing permission, a missing owner or a wrong epoch never are.
const fn is_transient(outcome: TrackOutcome) -> bool {
    matches!(
        outcome,
        TrackOutcome::Failed(AxError::CannotComplete | AxError::Other(_))
            | TrackOutcome::Unacknowledged
            | TrackOutcome::Dispatch(DispatchError::Full)
    )
}

fn track_failed(state: &mut TilingState, application: ApplicationId, outcome: TrackOutcome) {
    state.startup.awaiting.remove(&application);
    state.reconcile.inflight.remove(&application);
    state.reconcile.scans.remove(&application);
    state.startup.failed.insert(application);
    if is_transient(outcome) {
        state.startup.transient.insert(application);
    } else {
        state.startup.transient.remove(&application);
    }
    state.startup.failure_reasons.insert(application, format!("{outcome:?}"));
    state.diagnostics.enumeration_failures += 1;
    match outcome {
        TrackOutcome::PermissionDenied => state.accessibility = false,
        TrackOutcome::OwnerUnavailable => {
            state.health = Health::Failed(HealthFault::BackendUnavailable);
        }
        _ => {}
    }
}

fn completion(state: &mut TilingState, result: CompletionResult) {
    match result {
        CompletionResult::Track(application, outcome) => match outcome {
            TrackOutcome::Tracking => {
                state.startup.awaiting.remove(&application);
                if let Some(record) = state.apps.get_mut(&application) {
                    record.tracked = true;
                }
            }
            TrackOutcome::AlreadyTracked => {
                // No enumeration will follow this request: drop its scan, refresh.
                state.startup.awaiting.remove(&application);
                state.reconcile.inflight.remove(&application);
                state.reconcile.scans.remove(&application);
                if let Some(record) = state.apps.get_mut(&application) {
                    record.tracked = true;
                    state.reconcile.unconfirmed.insert(application);
                }
            }
            other => track_failed(state, application, other),
        },
        CompletionResult::Refresh(application, outcome) => {
            if !matches!(outcome, TrackOutcome::Tracking | TrackOutcome::AlreadyTracked) {
                track_failed(state, application, outcome);
            }
        }
        // The application's real focused window is the only thing that may
        // switch a workspace on activation. Ignored once another application is
        // frontmost, or when the answer is unknown (never a guess).
        CompletionResult::Focus(application, outcome) => {
            state.pending_focus.inflight.remove(&application);
            if let FocusOutcome::Resolved(token) = outcome
                && state.active_application == Some(application)
                && state.windows.get(&token).is_some_and(|w| {
                    w.application == application
                        && !w.observed.minimized
                        && !w.is_inactive_tab()
                        && !w.suspended
                })
            {
                on_focused(state, token);
            }
        }
        CompletionResult::Snapshot(token, outcome) => {
            match outcome {
                SnapshotOutcome::Snapshot(snapshot) => {
                    // Stamped with the latest event sequence so a later scan can
                    // repair this window if its destroy was missed.
                    let sequence = state.last_event_sequence;
                    ingest(state, &snapshot, None, Some(sequence));
                }
                // Alive but unlisted: the owner reports the suspension itself.
                SnapshotOutcome::Stale(StaleReason::Suspended) => {}
                SnapshotOutcome::Stale(_) => {
                    if !state.windows.get(&token).is_some_and(|window| window.suspended) {
                        destroy_or_depart(state, token);
                    }
                }
                _ => state.diagnostics.snapshot_refusals += 1,
            }
            // A drag waits for one fresh read per captured window, whatever it said.
            drag::on_read(state, token);
        }
        CompletionResult::Frame {
            window,
            plan_sequence,
            revision,
            outcome,
        } => {
            frames::complete(state, window, plan_sequence, revision, &outcome);
        }
        // The result lives in the shared visibility control; the actor shell
        // folds it. The completion only wakes the turn.
        CompletionResult::Visibility(_) => {}
        CompletionResult::Rejected(effect) => rejected(state, &effect),
    }
}

/// A read-only effect that could not be queued is a visible failure, never a
/// success and never an immediate retry (which could spin while saturated).
fn rejected(state: &mut TilingState, effect: &Effect) {
    match *effect {
        Effect::Track { application, .. } | Effect::Refresh { application, .. } => {
            track_failed(state, application, TrackOutcome::Unacknowledged);
        }
        Effect::ListApplications { .. } => {
            state.reconcile.listing = false;
            state.reconcile.relist = false;
            state.diagnostics.snapshot_refusals += 1;
        }
        Effect::WriteFrame { window, plan_sequence, .. } => {
            frames::rejected(
                state,
                window,
                plan_sequence,
                crate::outcome::NotAttemptedReason::QueueFull,
            );
        }
        Effect::ReadFocus { application, .. } => {
            state.pending_focus.inflight.remove(&application);
            state.diagnostics.snapshot_refusals += 1;
        }
        Effect::Snapshot { window, .. } => {
            state.diagnostics.snapshot_refusals += 1;
            // The read will never come: the gesture decides with what it knows.
            drag::on_read(state, window);
        }
        Effect::Untrack { .. }
        | Effect::CancelFrame { .. }
        | Effect::HideApp { .. }
        | Effect::UnhideApp { .. }
        | Effect::Focus { .. }
        | Effect::Publish => {
            state.diagnostics.snapshot_refusals += 1;
        }
    }
}

// ---------------------------------------------------------------- displays

/// Creates the workspaces the configuration asks for that do not exist yet.
fn create_missing(state: &mut TilingState) {
    if state.screens.is_empty() {
        return;
    }
    let default = TilingConfig::default();
    let config = state.config.as_deref().unwrap_or(&default);
    for spec in specs(&state.screens, config) {
        if !state.workspaces.iter().any(|ws| ws.name == spec.name) {
            let id = WorkspaceId(state.next_workspace);
            state.next_workspace += 1;
            state.workspaces.push(new_workspace(id, &spec));
        }
    }
}

/// The display a displaced workspace belongs on, when it is present again.
fn home_screen(state: &TilingState, index: usize) -> Option<DisplayId> {
    let workspace = &state.workspaces[index];
    if let Some(configured) = &workspace.configured_screen {
        return resolve_screen(&state.screens, configured);
    }
    let default = TilingConfig::default();
    let config = state.config.as_deref().unwrap_or(&default);
    if let Some(spec) = specs(&state.screens, config).into_iter().find(|s| s.name == workspace.name)
    {
        return Some(spec.screen);
    }
    // A generated per-display default returns to the display it is named after.
    state
        .screens
        .iter()
        .find(|screen| default_name(screen.id) == workspace.name)
        .map(|screen| screen.id)
}

fn apply_displays(state: &mut TilingState, layout: &DisplayLayout) {
    if layout.displays().is_empty() {
        // Keep the last known screens and workspaces: do not tear state down for
        // a transient empty topology, but never claim completeness.
        state.no_screens = true;
        return;
    }
    state.no_screens = false;
    state.layout = layout.clone();
    let previous: std::collections::BTreeMap<DisplayId, u64> =
        state.screens.iter().map(|screen| (screen.id, screen.presence)).collect();
    let mut next_presence = state.next_presence;
    state.screens = layout
        .displays()
        .iter()
        .map(|display| ScreenState {
            id: display.id,
            name: display.name.clone(),
            is_primary: display.is_primary,
            refresh_hz: display.refresh_hz,
            // A display that was not in the previous layout is a new presence.
            presence: previous.get(&display.id).copied().unwrap_or_else(|| {
                next_presence += 1;
                next_presence
            }),
        })
        .collect();
    state.next_presence = next_presence;
    state.screens_known = true;
    let present: BTreeSet<DisplayId> = state.screens.iter().map(|s| s.id).collect();
    // A workspace whose display vanished falls back to main and is remembered
    // as displaced.
    if let Some(primary) = primary_screen(&state.screens) {
        for workspace in &mut state.workspaces {
            if !present.contains(&workspace.screen) {
                workspace.screen = primary;
                workspace.displaced = true;
            }
        }
    }
    create_missing(state);
    // ...and returns when its display is present again.
    let restores: Vec<(usize, DisplayId)> = (0..state.workspaces.len())
        .filter(|index| state.workspaces[*index].displaced)
        .filter_map(|index| {
            let target = home_screen(state, index)?;
            (target != state.workspaces[index].screen).then_some((index, target))
        })
        .collect();
    for (index, target) in restores {
        state.workspaces[index].screen = target;
        state.workspaces[index].displaced = false;
    }
    normalize(state);
    assign_unassigned(state);
}

/// Defaults for empty displays, one visible workspace per display, one focus.
fn normalize(state: &mut TilingState) {
    let mut next = state.next_workspace;
    ensure_default_workspaces(&state.screens, &mut state.workspaces, &mut next);
    state.next_workspace = next;
    let focused = reconcile(&state.screens, &mut state.workspaces, state.focus.workspace);
    state.focus = match focused {
        Some(id) => focus_for(&state.workspaces, id, &state.focus),
        None => FocusState::default(),
    };
}

fn apply_config(
    state: &mut TilingState,
    revision: u64,
    config: std::sync::Arc<TilingConfig>,
    fx: &mut Vec<Effect>,
) {
    if state.config.is_some() && revision <= state.config_revision {
        state.diagnostics.stale_configs += 1;
        return;
    }
    // Prepare the complete mapping before anything is committed: a refused
    // candidate leaves the old configuration, rules and workspaces in place.
    let mapping = match prepare_reload(state, &config) {
        Ok(mapping) => mapping,
        Err(error) => {
            state.diagnostics.rejected_reloads += 1;
            state.last_reload_error = Some(error.to_string());
            return;
        }
    };
    state.last_reload_error = None;
    state.config_revision = revision;
    state.workspace_rules.clone_from(&mapping.workspace_rules);
    state.ignore = mapping.ignore.clone();
    let changed = state.config.as_deref() != Some(&*config);
    state.config = Some(config);
    if !(changed && state.screens_known) {
        return;
    }
    state.workspaces = mapping.workspaces;
    state.next_workspace = mapping.next_workspace;
    for (token, id) in &mapping.moved {
        if let Some(window) = state.windows.get_mut(token) {
            window.desired.workspace = Some(*id);
        }
    }
    // Newly ignored windows release tiling and every visibility need.
    for (token, why) in mapping.released {
        untrack(state, token);
        state.excluded.insert(token, why);
        state.diagnostics.excluded_windows += 1;
    }
    // Windows an ignore rule excluded are read again so the new rules decide.
    if let Some(stamp) = state.stamp {
        fx.extend(mapping.recheck.into_iter().map(|window| Effect::Snapshot { stamp, window }));
    }
    let existing: BTreeSet<WorkspaceId> = state.workspaces.iter().map(|ws| ws.id).collect();
    for window in state.windows.values_mut() {
        if window.desired.workspace.is_some_and(|id| !existing.contains(&id)) {
            window.desired.workspace = None;
        }
    }
    state.focus = match mapping.focused {
        Some(id) => focus_for(&state.workspaces, id, &state.focus),
        None => FocusState::default(),
    };
    assign_unassigned(state);
}
