//! Pure transition tests: no threads, clock or backend.

use std::sync::Arc;

use stache_macos::applications::LifecycleKind;
use stache_macos::ax::AxReading;
use stache_macos::windows::{
    ApplicationChange, Capability, EnumerationAbort, SnapshotOutcome, StaleReason, TabInfo,
    TrackOutcome, WindowChange, WindowEventKind,
};

use super::*;
use crate::config::TilingConfig;
use crate::rules::ManagementExclusion;
use crate::test_support::{Feed, app, display, info, one_display, ready, run, snapshot, step};

fn fresh_state() -> (TilingState, Feed) {
    let mut feed = Feed::new(1);
    let state = ready("com.stache.app", &[info(1, "com.editor", "Editor")], &mut feed);
    (state, feed)
}

fn config(value: serde_json::Value) -> Arc<TilingConfig> {
    Arc::new(serde_json::from_value(value).expect("config"))
}

#[test]
fn startup_is_complete_only_after_enumeration_and_screens() {
    let mut feed = Feed::new(1);
    let state = TilingState::new("");
    assert_eq!(state.phase(), Phase::Initializing);
    let (state, effects) = step(state, Input::Start(feed.stamp));
    assert!(effects.contains(&Effect::ListApplications { stamp: feed.stamp }));
    let (state, effects) = step(state, Input::Applications {
        stamp: feed.stamp,
        apps: vec![info(1, "com.a", "A")],
    });
    // Observers first: the track request is issued, no per-window wait chain.
    assert!(effects.iter().any(|e| matches!(e, Effect::Track { .. })));
    assert_eq!(state.phase(), Phase::Initializing);
    let (state, _) = step(state, Input::Displays(Arc::new(one_display())));
    assert_eq!(state.phase(), Phase::Initializing, "enumeration still pending");
    let (state, _) = step(state, Input::Completion {
        stamp: feed.stamp,
        result: CompletionResult::Track(app(1), TrackOutcome::Tracking),
    });
    assert_eq!(state.phase(), Phase::Initializing);
    let (state, _) = step(state, feed.complete(app(1)));
    assert_eq!(state.phase(), Phase::Complete);
    assert!(state.snapshot().actions_allowed);
}

#[test]
fn tracking_is_chunked() {
    let feed = Feed::new(1);
    let apps: Vec<AppInfo> = (1..=20).map(|n| info(n, "com.x", "X")).collect();
    let (state, effects) = run(TilingState::new(""), vec![
        Input::Start(feed.stamp),
        Input::Applications { stamp: feed.stamp, apps },
    ]);
    let tracks = effects.iter().filter(|e| matches!(e, Effect::Track { .. })).count();
    assert_eq!(tracks, TRACK_CHUNK);
    assert_eq!(state.startup.pending.len(), 20 - TRACK_CHUNK);
    let (_, effects) = step(state, Input::Completion {
        stamp: feed.stamp,
        result: CompletionResult::Track(app(1), TrackOutcome::Tracking),
    });
    assert_eq!(
        effects.iter().filter(|e| matches!(e, Effect::Track { .. })).count(),
        1
    );
}

#[test]
fn create_adopts_and_assigns_after_the_focused_window() {
    let (state, mut feed) = fresh_state();
    let (a, b, c) = (feed.token(1, 1), feed.token(1, 2), feed.token(1, 3));
    let inputs = vec![
        feed.created(a, "a", 1),
        feed.created(b, "b", 2),
        feed.change(a, WindowChange::Focused),
        feed.created(c, "c", 3),
    ];
    let (state, _) = run(state, inputs);
    let order = &state.workspaces[0].order;
    // Non-Grid inserts after the focused window.
    assert_eq!(order, &vec![a, c, b]);
    assert_eq!(state.focus.window, Some(a));
}

#[test]
fn grid_appends_even_when_a_window_is_focused() {
    let (state, mut feed) = fresh_state();
    let cfg = config(serde_json::json!({"workspaces": [{"name": "g", "layout": "grid"}]}));
    let (state, _) = step(state, Input::Config { revision: 1, config: cfg });
    let (a, b, c) = (feed.token(1, 1), feed.token(1, 2), feed.token(1, 3));
    let (state, _) = run(state, vec![
        feed.created(a, "a", 1),
        feed.created(b, "b", 2),
        feed.change(a, WindowChange::Focused),
        feed.created(c, "c", 3),
    ]);
    assert_eq!(state.workspaces[0].order, vec![a, b, c]);
}

#[test]
fn rules_assign_the_first_matching_workspace_else_the_focused_one() {
    let (state, mut feed) = fresh_state();
    let cfg = config(serde_json::json!({
        "workspaces": [
            {"name": "main"},
            {"name": "web", "rules": [{"appId": "com.editor", "title": "docs"}]}
        ]
    }));
    let (state, _) = step(state, Input::Config { revision: 1, config: cfg });
    let (hit, miss) = (feed.token(1, 1), feed.token(1, 2));
    let (state, _) = run(state, vec![
        feed.created(hit, "My Docs", 1),
        feed.created(miss, "other", 2),
    ]);
    let web = state.workspaces.iter().find(|w| w.name == "web").expect("web");
    let main = state.workspaces.iter().find(|w| w.name == "main").expect("main");
    assert_eq!(web.order, vec![hit]);
    assert_eq!(main.order, vec![miss], "no rule match: the focused workspace");
}

#[test]
fn windows_wait_unassigned_until_screens_exist() {
    let mut feed = Feed::new(1);
    let state = TilingState::new("");
    let (state, _) = run(state, vec![Input::Start(feed.stamp), Input::Applications {
        stamp: feed.stamp,
        apps: vec![info(1, "com.a", "A")],
    }]);
    let a = feed.token(1, 1);
    let (state, _) = step(state, feed.created(a, "a", 1));
    assert_eq!(state.windows[&a].desired.workspace, None);
    let (state, _) = step(state, Input::Displays(Arc::new(one_display())));
    assert!(state.windows[&a].desired.workspace.is_some());
    assert_eq!(state.workspaces[0].order, vec![a]);
}

#[test]
fn exclusions_apply_identically_at_startup_and_on_events() {
    let mut feed = Feed::new(1);
    let apps = [
        info(1, "com.editor", "Editor"),
        info(2, "com.apple.dock", "Dock"),
        info(3, "com.stache.app", "Stache"),
        info(4, "com.video", "Video"),
    ];
    let state = {
        let state = TilingState::new("com.stache.app");
        let (state, _) = step(state, Input::Config {
            revision: 1,
            config: config(serde_json::json!({"ignore": [{"title": "Private"}]})),
        });
        let (state, _) = run(state, vec![
            Input::Displays(Arc::new(one_display())),
            Input::Start(feed.stamp),
            Input::Applications {
                stamp: feed.stamp,
                apps: apps.to_vec(),
            },
        ]);
        state
    };
    // System components and Stache itself are never tracked.
    assert!(state.apps[&app(2)].skipped && state.apps[&app(3)].skipped);
    let tracked: Vec<_> = state.startup.awaiting.iter().copied().collect();
    assert!(tracked.contains(&app(1)) && tracked.contains(&app(4)));
    assert!(!tracked.contains(&app(2)) && !tracked.contains(&app(3)));

    let (ignored, pip, ok) = (feed.token(1, 1), feed.token(4, 1), feed.token(1, 2));
    let mut pip_snapshot = snapshot(pip, "pip", 2);
    pip_snapshot.subrole = AxReading::Value("AXFloatingWindow".to_owned());
    // The same path serves startup (Enumeration) and live events (Created).
    let (state, _) = run(state, vec![
        feed.enumerated(ignored, "Private notes", 1),
        feed.input(WindowEventKind::Snapshot {
            snapshot: pip_snapshot,
            origin: stache_macos::windows::SnapshotOrigin::Created,
        }),
        feed.created(ok, "Public", 3),
    ]);
    assert!(matches!(
        state.excluded[&ignored],
        ManagementExclusion::IgnoreRule { .. }
    ));
    assert_eq!(state.excluded[&pip], ManagementExclusion::PictureInPicture);
    assert!(state.windows.contains_key(&ok) && !state.windows.contains_key(&ignored));
}

#[test]
fn a_title_change_can_start_and_stop_an_exclusion() {
    let (state, mut feed) = fresh_state();
    let (state, _) = step(state, Input::Config {
        revision: 1,
        config: config(serde_json::json!({"ignore": [{"title": "secret"}]})),
    });
    let w = feed.token(1, 1);
    let (state, _) = step(state, feed.created(w, "plain", 1));
    assert!(state.windows.contains_key(&w));
    // TitleChanged asks for a read; the completion applies it.
    let (state, effects) = step(state, feed.change(w, WindowChange::TitleChanged));
    assert!(effects.contains(&Effect::Snapshot { stamp: feed.stamp, window: w }));
    let (state, _) = step(state, Input::Completion {
        stamp: feed.stamp,
        result: CompletionResult::Snapshot(
            w,
            SnapshotOutcome::Snapshot(Box::new(snapshot(w, "my secret", 1))),
        ),
    });
    assert!(!state.windows.contains_key(&w) && state.excluded.contains_key(&w));
    assert!(state.workspaces[0].order.is_empty());
    let (state, _) = step(state, Input::Completion {
        stamp: feed.stamp,
        result: CompletionResult::Snapshot(
            w,
            SnapshotOutcome::Snapshot(Box::new(snapshot(w, "plain again", 1))),
        ),
    });
    assert!(state.windows.contains_key(&w) && !state.excluded.contains_key(&w));
}

#[test]
fn fullscreen_minimized_hidden_and_tabbed_windows_stay_tracked_with_flags() {
    let (state, mut feed) = fresh_state();
    let w = feed.token(1, 1);
    let mut snap = snapshot(w, "w", 1);
    snap.fullscreen = AxReading::Value(true);
    snap.tabs = TabInfo::Tabs(3);
    let (state, _) = step(
        state,
        feed.input(WindowEventKind::Snapshot {
            snapshot: snap,
            origin: stache_macos::windows::SnapshotOrigin::Created,
        }),
    );
    let window = &state.windows[&w];
    assert!(window.flags().fullscreen);
    assert_eq!(window.observed.tabs, TabInfo::Tabs(3));
    assert_eq!(state.workspaces[0].order, vec![w], "still in the stable order");

    let (state, _) = step(state, feed.change(w, WindowChange::Miniaturized));
    assert!(state.windows[&w].flags().minimized);
    let (state, _) = step(state, feed.change(w, WindowChange::Deminiaturized));
    assert!(!state.windows[&w].flags().minimized);

    let (state, _) = step(
        state,
        feed.input(WindowEventKind::Application {
            application: app(1),
            change: ApplicationChange::Hidden,
        }),
    );
    assert!(state.windows[&w].flags().hidden && state.apps[&app(1)].hidden);
    assert!(
        state.ledger().eq(&VisibilityLedger::default()),
        "external hide is not runtime-owned"
    );
    let (state, _) = step(state, Input::Lifecycle {
        kind: LifecycleKind::Unhidden,
        info: info(1, "com.editor", "Editor"),
    });
    assert!(!state.windows[&w].flags().hidden);
}

#[test]
fn destroy_prunes_every_cache_and_a_late_snapshot_cannot_resurrect_it() {
    let (state, mut feed) = fresh_state();
    let (a, b) = (feed.token(1, 1), feed.token(1, 2));
    let (state, _) = run(state, vec![
        feed.created(a, "a", 7),
        feed.created(b, "b", 8),
        feed.change(a, WindowChange::Focused),
        feed.change(b, WindowChange::Focused),
        feed.change(b, WindowChange::Destroyed),
    ]);
    assert!(!state.windows.contains_key(&b));
    assert!(!state.workspaces[0].order.contains(&b));
    assert!(!state.focus_history.contains(&b));
    assert!(!state.cg_index.values().any(|t| *t == b));
    assert_ne!(state.focus.window, Some(b));
    assert_eq!(
        state.workspaces[0].focused_window,
        Some(a),
        "history picks the previous window"
    );
    // A late enumeration snapshot of the destroyed token is ignored.
    let (state, _) = step(state, feed.enumerated(b, "b", 8));
    assert!(!state.windows.contains_key(&b));
    assert_eq!(state.diagnostics.ignored_after_destroy, 1);
}

#[test]
fn same_app_cg_window_id_reuse_never_leaks_the_old_token() {
    let (state, mut feed) = fresh_state();
    let (old, new) = (feed.token(1, 1), feed.token(1, 2));
    let (state, _) = run(state, vec![
        feed.created(old, "old", 42),
        feed.change(old, WindowChange::Focused),
    ]);
    // The CGWindowID is reused by a new token before the destroy arrived.
    let (state, _) = step(state, feed.created(new, "new", 42));
    assert!(!state.windows.contains_key(&old), "stale owner evicted");
    assert!(state.is_tombstoned(old));
    assert_eq!(state.cg_index[&42], new);
    assert_eq!(state.workspaces[0].order, vec![new]);
    assert_eq!(state.diagnostics.cg_reuse_evictions, 1);
    // The delayed destroy for the old token cannot touch the new one.
    let (state, _) = step(state, feed.change(old, WindowChange::Destroyed));
    assert!(state.windows.contains_key(&new));
}

#[test]
fn create_focus_destroy_ordering_leaves_no_residue() {
    let (state, mut feed) = fresh_state();
    let w = feed.token(1, 1);
    let (state, _) = run(state, vec![
        feed.created(w, "w", 1),
        feed.change(w, WindowChange::Focused),
        feed.change(w, WindowChange::Destroyed),
    ]);
    assert!(state.windows.is_empty() && state.workspaces[0].order.is_empty());
    assert_eq!(state.focus.window, None);
    // Focus arriving after the destroy is ignored, not turned into a read.
    let (_, effects) = step(state, feed.change(w, WindowChange::Focused));
    assert!(!effects.iter().any(|e| matches!(e, Effect::Snapshot { .. })));
}

#[test]
fn focus_of_an_unknown_window_requests_a_read() {
    let (state, mut feed) = fresh_state();
    let w = feed.token(1, 9);
    let (state, effects) = step(state, feed.change(w, WindowChange::Focused));
    assert!(effects.contains(&Effect::Snapshot { stamp: feed.stamp, window: w }));
    assert_eq!(state.diagnostics.unknown_focus, 1);
}

#[test]
fn focus_switches_visible_workspace_on_its_screen() {
    let (state, mut feed) = fresh_state();
    let cfg = config(serde_json::json!({
        "workspaces": [{"name": "one"}, {"name": "two", "rules": [{"appName": "Editor"}]}]
    }));
    let (state, _) = step(state, Input::Config { revision: 1, config: cfg });
    let (a, b) = (feed.token(1, 1), feed.token(1, 2));
    let (state, _) = run(state, vec![feed.created(a, "a", 1), feed.created(b, "b", 2)]);
    // Rules route both to "one" (first matching is "two": Editor) -> both in two.
    let two = state.workspace_by_name("two");
    assert_eq!(two.order.len(), 2);
    let (state, _) = step(state, feed.change(a, WindowChange::Focused));
    assert!(state.workspace_by_name("two").visible && state.workspace_by_name("two").focused);
    assert!(!state.workspace_by_name("one").visible);
}

#[test]
fn the_destroy_of_a_missed_window_is_repaired_by_reconciliation_only() {
    let (state, mut feed) = fresh_state();
    let (keep, gone, late) = (feed.token(1, 1), feed.token(1, 2), feed.token(1, 3));
    let (state, _) = run(state, vec![
        feed.created(keep, "k", 1),
        feed.created(gone, "g", 2),
    ]);
    // Overflow dropped `gone`'s destroy: the app is marked and exactly one refresh issued.
    let (state, effects) = step(
        state,
        Input::Unconfirmed(Scope::App(app(1)), Cause::SemanticOverflow),
    );
    let refreshes = effects.iter().filter(|e| matches!(e, Effect::Refresh { .. })).count();
    assert_eq!(refreshes, 1);
    assert_eq!(state.phase(), Phase::Reconciling);
    assert!(
        !state.snapshot().actions_allowed,
        "no action authority while unconfirmed"
    );
    // Marking again during the scan does not issue a second concurrent refresh.
    let (state, effects) = step(
        state,
        Input::Unconfirmed(Scope::App(app(1)), Cause::SemanticOverflow),
    );
    assert!(!effects.iter().any(|e| matches!(e, Effect::Refresh { .. })));
    // A window created while scanning (after the floor) survives the scan.
    let (state, _) = run(state, vec![
        feed.enumerated(keep, "k", 1),
        feed.created(late, "late", 3),
    ]);
    let (state, effects) = step(state, feed.complete(app(1)));
    assert!(
        !state.windows.contains_key(&gone),
        "unseen pre-scan window repaired"
    );
    assert!(state.windows.contains_key(&keep) && state.windows.contains_key(&late));
    assert_eq!(state.diagnostics.missed_destroys_repaired, 1);
    // The re-mark that arrived mid-scan reruns exactly once after it.
    assert_eq!(
        effects.iter().filter(|e| matches!(e, Effect::Refresh { .. })).count(),
        1
    );
}

#[test]
fn a_window_created_while_the_initial_scan_runs_is_kept_even_if_the_scan_missed_it() {
    let mut feed = Feed::new(1);
    let state = TilingState::new("");
    let (state, _) = run(state, vec![
        Input::Displays(Arc::new(one_display())),
        Input::Start(feed.stamp),
        Input::Applications {
            stamp: feed.stamp,
            apps: vec![info(1, "com.a", "A")],
        },
    ]);
    let (early, mid) = (feed.token(1, 1), feed.token(1, 2));
    // The Created event precedes the track completion in the actor's order.
    let (state, _) = run(state, vec![
        feed.created(mid, "mid", 2),
        Input::Completion {
            stamp: feed.stamp,
            result: CompletionResult::Track(app(1), TrackOutcome::Tracking),
        },
        feed.enumerated(early, "early", 1),
        feed.complete(app(1)),
    ]);
    assert!(state.windows.contains_key(&mid) && state.windows.contains_key(&early));
    assert_eq!(state.phase(), Phase::Complete);
}

#[test]
fn a_destroy_during_the_scan_is_not_resurrected_by_its_late_snapshot() {
    let mut feed = Feed::new(1);
    let state = TilingState::new("");
    let (state, _) = run(state, vec![
        Input::Displays(Arc::new(one_display())),
        Input::Start(feed.stamp),
        Input::Applications {
            stamp: feed.stamp,
            apps: vec![info(1, "com.a", "A")],
        },
        Input::Completion {
            stamp: feed.stamp,
            result: CompletionResult::Track(app(1), TrackOutcome::Tracking),
        },
    ]);
    let w = feed.token(1, 1);
    let (state, _) = run(state, vec![
        feed.change(w, WindowChange::Destroyed),
        feed.enumerated(w, "w", 1),
        feed.complete(app(1)),
    ]);
    assert!(state.windows.is_empty());
}

#[test]
fn permanent_overflow_is_visibly_overloaded_and_bounded() {
    let (mut state, _feed) = fresh_state();
    for _ in 0..(OVERLOAD_THRESHOLD + 2) {
        let (next, _) = step(state, Input::Unconfirmed(Scope::All, Cause::SemanticOverflow));
        state = next;
    }
    assert!(state.diagnostics.overloaded);
    assert_eq!(state.phase(), Phase::Incomplete);
    assert!(!state.snapshot().actions_allowed);
    assert!(state.reconcile.unconfirmed.len() <= MAX_UNCONFIRMED_APPS);
    // Overload stays visible while markings keep arriving, even after a scan finishes.
}

#[test]
fn unconfirmed_app_scope_widens_to_all_beyond_the_bound() {
    let (mut state, _) = fresh_state();
    for n in 0..(MAX_UNCONFIRMED_APPS as u64 + 5) {
        state = step(
            state,
            Input::Unconfirmed(Scope::App(app(1000 + n)), Cause::SemanticOverflow),
        )
        .0;
    }
    assert!(state.reconcile.unconfirmed.len() <= MAX_UNCONFIRMED_APPS);
}

#[test]
fn observer_overflow_and_sequence_gap_reconcile() {
    let (state, mut feed) = fresh_state();
    let (state, effects) = step(
        state,
        feed.input(WindowEventKind::ObserverOverflow { application: app(1) }),
    );
    assert_eq!(
        effects.iter().filter(|e| matches!(e, Effect::Refresh { .. })).count(),
        1
    );
    let (_, effects) = step(state, Input::Unconfirmed(Scope::All, Cause::SequenceGap));
    assert!(effects.iter().any(|e| matches!(e, Effect::ListApplications { .. })));
}

#[test]
fn enumeration_failures_are_incomplete_not_complete() {
    let (state, mut feed) = fresh_state();
    let (state, _) = step(
        state,
        feed.input(WindowEventKind::EnumerationAborted {
            application: app(1),
            reason: EnumerationAbort::DeadlineExceeded,
        }),
    );
    assert_eq!(state.phase(), Phase::Incomplete);
    let (state, _) = step(
        state,
        feed.input(WindowEventKind::EnumerationComplete {
            application: app(1),
            windows: 0,
            truncated: true,
        }),
    );
    assert_eq!(state.phase(), Phase::Incomplete, "truncated is never complete");
}

#[test]
fn track_failures_and_a_dead_backend_are_health_facts() {
    let feed = Feed::new(1);
    let (state, _) = run(TilingState::new(""), vec![
        Input::Displays(Arc::new(one_display())),
        Input::Start(feed.stamp),
        Input::Applications {
            stamp: feed.stamp,
            apps: vec![info(1, "com.a", "A")],
        },
    ]);
    let (state, _) = step(state, Input::Completion {
        stamp: feed.stamp,
        result: CompletionResult::Track(app(1), TrackOutcome::OwnerUnavailable),
    });
    assert_eq!(state.health, Health::Failed(HealthFault::BackendUnavailable));
    assert_eq!(state.phase(), Phase::Incomplete);
}

#[test]
fn permission_loss_is_incomplete_and_restoration_tracks_again() {
    let (state, mut feed) = fresh_state();
    let (state, _) = step(
        state,
        feed.input(WindowEventKind::CapabilityChanged {
            capability: Capability::Accessibility,
            available: false,
        }),
    );
    assert_eq!(state.phase(), Phase::Incomplete);
    let (state, _) = step(
        state,
        feed.input(WindowEventKind::CapabilityChanged {
            capability: Capability::Accessibility,
            available: true,
        }),
    );
    assert_eq!(state.phase(), Phase::Complete);
}

#[test]
fn rejected_effects_are_failures_never_success_and_never_retry_hot() {
    let feed = Feed::new(1);
    let (state, effects) = run(TilingState::new(""), vec![
        Input::Displays(Arc::new(one_display())),
        Input::Start(feed.stamp),
        Input::Applications {
            stamp: feed.stamp,
            apps: vec![info(1, "com.a", "A")],
        },
    ]);
    let track = effects.into_iter().find(|e| matches!(e, Effect::Track { .. })).expect("track");
    let (state, effects) = step(state, Input::Completion {
        stamp: feed.stamp,
        result: CompletionResult::Rejected(track),
    });
    assert_eq!(state.phase(), Phase::Incomplete);
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::Track { .. } | Effect::Refresh { .. }))
    );
}

#[test]
fn terminate_removes_windows_and_untracks() {
    let (state, mut feed) = fresh_state();
    let w = feed.token(1, 1);
    let (state, _) = step(state, feed.created(w, "w", 1));
    let (state, effects) = step(state, Input::Lifecycle {
        kind: LifecycleKind::Terminated,
        info: info(1, "com.editor", "Editor"),
    });
    assert!(state.windows.is_empty() && state.apps.is_empty());
    assert!(state.workspaces[0].order.is_empty());
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::Untrack { application, .. } if *application == app(1)))
    );
}

#[test]
fn an_unresolved_terminate_relists_applications_once() {
    let (state, _) = fresh_state();
    let lifecycle = || Input::Lifecycle {
        kind: LifecycleKind::TerminatedUnresolved,
        info: info(9, "", ""),
    };
    let (state, effects) = step(state, lifecycle());
    assert!(
        state.apps.contains_key(&app(1)),
        "nothing is dropped without a list"
    );
    assert_eq!(
        effects.iter().filter(|e| matches!(e, Effect::ListApplications { .. })).count(),
        1
    );
    // A listing already in flight is not duplicated.
    let (_, effects) = step(state, lifecycle());
    assert!(!effects.iter().any(|e| matches!(e, Effect::ListApplications { .. })));
}

#[test]
fn an_unresolved_terminate_during_an_inflight_listing_relists_exactly_once_after_it() {
    let feed = Feed::new(1);
    let (state, _) = step(TilingState::new(""), Input::Start(feed.stamp));
    assert!(state.reconcile.listing);
    let unresolved = || Input::Lifecycle {
        kind: LifecycleKind::TerminatedUnresolved,
        info: info(9, "", ""),
    };
    // Two arrive while the first listing is in flight: coalesced, none issued.
    let relists = |effects: &[Effect]| {
        effects.iter().filter(|e| matches!(e, Effect::ListApplications { .. })).count()
    };
    let (state, effects) = step(state, unresolved());
    assert_eq!(relists(&effects), 0);
    let (state, effects) = step(state, unresolved());
    assert_eq!(relists(&effects), 0);
    // The in-flight list completes: exactly one follow-up list is requested.
    let (state, effects) = step(state, Input::Applications {
        stamp: feed.stamp,
        apps: vec![info(1, "com.editor", "Editor")],
    });
    assert_eq!(relists(&effects), 1);
    assert!(state.reconcile.listing);
    // The follow-up completes without asking again.
    let (state, effects) = step(state, Input::Applications {
        stamp: feed.stamp,
        apps: vec![],
    });
    assert_eq!(relists(&effects), 0);
    assert!(!state.apps.contains_key(&app(1)), "the dead app is dropped");
}

#[test]
fn applications_list_is_authoritative_for_missed_lifecycle() {
    let (state, feed) = fresh_state();
    let (state, _) = step(state, Input::Applications {
        stamp: feed.stamp,
        apps: vec![info(2, "com.new", "New")],
    });
    assert!(!state.apps.contains_key(&app(1)) && state.apps.contains_key(&app(2)));
}

#[test]
fn activation_is_informational() {
    let (state, _) = fresh_state();
    let (state, _) = step(state, Input::Activated(app(1)));
    assert_eq!(state.active_application, Some(app(1)));
}

#[test]
fn no_screens_is_incomplete_and_keeps_workspaces_hotplug_moves_and_restores() {
    let (state, mut feed) = fresh_state();
    let cfg = config(serde_json::json!({
        "workspaces": [
            {"name": "m", "screen": "main"},
            {"name": "s", "screen": "secondary"}
        ]
    }));
    let two = Arc::new(stache_macos::DisplayLayout::new(vec![
        display(1, true, Some(60.0), 0.0),
        display(2, false, Some(120.0), 1920.0),
    ]));
    let (state, _) = run(state, vec![
        Input::Config { revision: 1, config: cfg },
        Input::Displays(Arc::clone(&two)),
    ]);
    assert_eq!(
        state.workspace_by_name("s").screen,
        stache_macos::DisplayId::new(2)
    );
    let w = feed.token(1, 1);
    let (state, _) = step(state, feed.created(w, "w", 1));

    // Unplug the secondary: its workspace moves to main.
    let (state, _) = step(state, Input::Displays(Arc::new(one_display())));
    assert_eq!(
        state.workspace_by_name("s").screen,
        stache_macos::DisplayId::new(1)
    );
    // Plug it back: restored to its configured screen.
    let (state, _) = step(state, Input::Displays(two));
    assert_eq!(
        state.workspace_by_name("s").screen,
        stache_macos::DisplayId::new(2)
    );

    // No screens at all: tracking continues, nothing is torn down, never Complete.
    let (state, _) = step(
        state,
        Input::Displays(Arc::new(stache_macos::DisplayLayout::new(Vec::new()))),
    );
    assert!(state.no_screens && state.windows.contains_key(&w));
    assert_eq!(state.phase(), Phase::Incomplete);
    let (state, _) = step(state, Input::Displays(Arc::new(one_display())));
    assert!(!state.no_screens);
    assert_eq!(state.phase(), Phase::Complete);
}

#[test]
fn every_screen_gets_a_workspace_and_exactly_one_is_visible() {
    let (state, _) = fresh_state();
    let two = Arc::new(stache_macos::DisplayLayout::new(vec![
        display(1, true, None, 0.0),
        display(2, false, Some(144.0), 1920.0),
    ]));
    let (state, _) = step(state, Input::Displays(two));
    for screen in &state.screens {
        let on: Vec<_> = state.workspaces.iter().filter(|w| w.screen == screen.id).collect();
        assert!(!on.is_empty());
        assert_eq!(on.iter().filter(|w| w.visible).count(), 1);
    }
    assert_eq!(state.workspaces.iter().filter(|w| w.focused).count(), 1);
}

#[test]
fn stale_config_revisions_are_dropped() {
    let (state, _) = fresh_state();
    let (state, _) = step(state, Input::Config {
        revision: 5,
        config: config(serde_json::json!({})),
    });
    let (state, _) = step(state, Input::Config {
        revision: 4,
        config: config(serde_json::json!({"workspaces": [{"name": "x"}]})),
    });
    assert_eq!(state.config_revision, 5);
    assert_eq!(state.diagnostics.stale_configs, 1);
}

#[test]
fn config_change_syncs_workspaces_and_reassigns_displaced_windows() {
    let (state, mut feed) = fresh_state();
    let (state, _) = step(state, Input::Config {
        revision: 1,
        config: config(serde_json::json!({"workspaces": [{"name": "a"}, {"name": "b"}]})),
    });
    let w = feed.token(1, 1);
    let (state, _) = step(state, feed.created(w, "w", 1));
    let (state, _) = step(state, Input::Config {
        revision: 2,
        config: config(serde_json::json!({"workspaces": [{"name": "c"}]})),
    });
    assert!(
        state
            .workspaces
            .iter()
            .all(|ws| ws.name == "c" || ws.name.starts_with("default-"))
    );
    assert_eq!(
        state.windows[&w].desired.workspace,
        Some(state.workspace_by_name("c").id)
    );
    assert_eq!(state.workspace_by_name("c").order, vec![w]);
}

#[test]
fn two_registries_drop_late_completions_events_and_state() {
    let (state, mut feed) = fresh_state();
    let w = feed.token(1, 1);
    let (state, _) = step(state, feed.created(w, "w", 1));
    let old_stamp = feed.stamp;
    let mut next = Feed::new(2);
    // A restart: the new epoch/registry starts clean (screens stay).
    let (state, effects) = step(state, Input::Start(next.stamp));
    assert!(effects.contains(&Effect::ListApplications { stamp: next.stamp }));
    assert!(state.windows.is_empty() && state.workspaces[0].order.is_empty());
    assert_eq!(state.diagnostics.registry_restarts, 1);
    // Late completions and events from the old registry are dropped, with a diagnostic.
    let (state, _) = step(state, Input::Completion {
        stamp: old_stamp,
        result: CompletionResult::Snapshot(
            w,
            SnapshotOutcome::Snapshot(Box::new(snapshot(w, "w", 1))),
        ),
    });
    let (state, _) = step(state, Input::Applications {
        stamp: old_stamp,
        apps: vec![info(1, "x", "x")],
    });
    let (state, _) = step(state, feed.created(w, "old event", 1));
    assert!(state.windows.is_empty() && state.apps.is_empty());
    assert_eq!(state.diagnostics.stale_completions, 2);
    assert_eq!(state.diagnostics.stale_epoch_events, 1);
    // The same app instance reappearing in the new registry works.
    let (state, _) = step(state, Input::Applications {
        stamp: next.stamp,
        apps: vec![info(1, "com.editor", "Editor")],
    });
    let n = next.token(1, 1);
    let (state, _) = run(state, vec![
        Input::Completion {
            stamp: next.stamp,
            result: CompletionResult::Track(app(1), TrackOutcome::Tracking),
        },
        next.created(n, "fresh", 9),
    ]);
    assert!(state.windows.contains_key(&n));
}

#[test]
fn stale_snapshot_reads_remove_the_window() {
    let (state, mut feed) = fresh_state();
    let w = feed.token(1, 1);
    let (state, _) = step(state, feed.created(w, "w", 1));
    let (state, _) = step(state, Input::Completion {
        stamp: feed.stamp,
        result: CompletionResult::Snapshot(w, SnapshotOutcome::Stale(StaleReason::Destroyed)),
    });
    assert!(state.windows.is_empty() && state.is_tombstoned(w));
}

#[test]
fn geometry_batches_request_reads_only_for_tracked_windows() {
    let (state, mut feed) = fresh_state();
    let (a, gone) = (feed.token(1, 1), feed.token(1, 2));
    let (state, _) = step(state, feed.created(a, "a", 1));
    let (_, effects) = step(state, Input::GeometryBatch(vec![a, gone]));
    let reads = effects.iter().filter(|e| matches!(e, Effect::Snapshot { .. })).count();
    assert_eq!(reads, 1);
}

#[test]
fn tombstones_are_bounded() {
    let (mut state, feed) = fresh_state();
    for serial in 0..(TOMBSTONE_CAPACITY as u64 + 50) {
        let token = feed.token(1, serial + 1);
        let mut f = Feed::new(1);
        state = step(state, f.change(token, WindowChange::Destroyed)).0;
    }
    assert!(state.tombstones.len() <= TOMBSTONE_CAPACITY);
    assert_eq!(state.tombstones.len(), state.tombstone_order.len());
}

#[test]
fn transitions_emit_only_read_only_effects() {
    let (state, mut feed) = fresh_state();
    let w = feed.token(1, 1);
    let (_, effects) = run(state, vec![
        feed.created(w, "w", 1),
        feed.change(w, WindowChange::Focused),
        feed.change(w, WindowChange::TitleChanged),
        feed.change(w, WindowChange::Moved),
        Input::Unconfirmed(Scope::All, Cause::SinkOverflow),
        feed.change(w, WindowChange::Destroyed),
    ]);
    assert!(effects.iter().all(|e| matches!(
        e,
        Effect::Track { .. }
            | Effect::Refresh { .. }
            | Effect::Snapshot { .. }
            | Effect::Untrack { .. }
            | Effect::ListApplications { .. }
            | Effect::Publish
    )));
}

impl TilingState {
    fn workspace_by_name(&self, name: &str) -> &WorkspaceState {
        self.workspaces
            .iter()
            .find(|w| w.name == name)
            .unwrap_or_else(|| panic!("workspace {name}"))
    }
}
