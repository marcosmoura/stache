//! Native tab groups through the pure transition: fake tab strips only, no
//! native code. One tile per group, no relayout for tab-only changes, exact
//! application scoping and explicit capability.

use stache_macos::windows::{
    SnapshotOrigin, TabInfo, TabMember, WindowChange, WindowEventKind, WindowInstance,
    WindowSnapshot,
};

use super::frame_tests::{Fixture, gap_config, one_screen};
use super::*;
use crate::geometry::Rect;
use crate::tabs::TabsStatus;
use crate::test_support::{Feed, app, info, ready, snapshot, step};

fn strip(token: WindowInstance, cg: u32, members: &[(u32, bool)]) -> WindowSnapshot {
    let mut snap = snapshot(token, "tab", cg);
    snap.tabs = TabInfo::Tabs(members.len());
    snap.tab_members = members
        .iter()
        .map(|(id, selected)| TabMember {
            cg_window_id: Some(*id),
            selected: Some(*selected),
        })
        .collect();
    snap
}

fn deliver(fx: &mut Fixture, snap: WindowSnapshot, origin: SnapshotOrigin) -> Vec<Effect> {
    let input = fx.feed.input(WindowEventKind::Snapshot { snapshot: snap, origin });
    fx.apply(input)
}

fn destroy(fx: &mut Fixture, token: WindowInstance) -> Vec<Effect> {
    let input = fx.feed.change(token, WindowChange::Destroyed);
    fx.apply(input)
}

fn writes(effects: &[Effect]) -> Vec<WindowInstance> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::WriteFrame { window, .. } => Some(*window),
            _ => None,
        })
        .collect()
}

/// Window serials: A (cg 1, the strip's reporter and selected tab), B (cg 2,
/// an inactive tab) and C (cg 3, an ordinary window). Frame writes are on.
struct Tabs {
    fx: Fixture,
    a: WindowInstance,
    b: WindowInstance,
    c: WindowInstance,
}

fn tabs() -> Tabs {
    let mut fx = Fixture::new(one_screen(), gap_config(0), 0);
    fx.state.set_tab_reads(true);
    let (a, b, c) = (fx.feed.token(1, 1), fx.feed.token(1, 2), fx.feed.token(1, 3));
    deliver(
        &mut fx,
        strip(a, 1, &[(1, true), (2, false)]),
        SnapshotOrigin::Created,
    );
    deliver(&mut fx, snapshot(b, "tab 2", 2), SnapshotOrigin::Created);
    deliver(&mut fx, snapshot(c, "plain", 3), SnapshotOrigin::Created);
    fx.enable();
    Tabs { fx, a, b, c }
}

impl Tabs {
    fn tiles(&self) -> Vec<WindowInstance> {
        self.fx.state.frames.desired.keys().copied().collect()
    }

    fn inactive(&self, token: WindowInstance) -> bool { self.fx.state.windows[&token].inactive_tab }
}

#[test]
fn a_multi_tab_app_has_one_visible_tile_and_the_inactive_tab_is_tracked() {
    let t = tabs();
    assert!(t.inactive(t.b) && !t.inactive(t.a) && !t.inactive(t.c));
    assert!(
        t.fx.state.windows.contains_key(&t.b),
        "inactive tabs are tracked"
    );
    let mut tiles = t.tiles();
    tiles.sort();
    let mut expected = vec![t.a, t.c];
    expected.sort();
    assert_eq!(tiles, expected, "one tile per group: the selected member");
    let workspace = &t.fx.state.workspaces[0];
    assert!(
        workspace.order.contains(&t.b),
        "the stable order still holds the tab"
    );
}

#[test]
fn a_tab_created_before_its_strip_is_read_is_held_back_once_the_strip_names_it() {
    let mut fx = Fixture::new(one_screen(), gap_config(0), 0);
    fx.state.set_tab_reads(true);
    let (a, b) = (fx.feed.token(1, 1), fx.feed.token(1, 2));
    deliver(&mut fx, snapshot(a, "a", 1), SnapshotOrigin::Created);
    deliver(&mut fx, snapshot(b, "b", 2), SnapshotOrigin::Created);
    fx.enable();
    assert_eq!(fx.state.frames.desired.len(), 2);
    // The strip is read afterwards (reconcile): B stops being a tile.
    deliver(
        &mut fx,
        strip(a, 1, &[(1, true), (2, false)]),
        SnapshotOrigin::Created,
    );
    assert_eq!(fx.state.frames.desired.keys().copied().collect::<Vec<_>>(), [a]);
    assert!(fx.state.windows[&b].inactive_tab);
}

#[test]
fn tab_only_creation_and_destruction_cause_no_relayout() {
    let mut t = tabs();
    let desired = t.fx.state.frames.desired.clone();
    let plan = t.fx.state.frames.plan_sequence;
    let d = t.fx.feed.token(1, 4);
    // The strip learns a third member, then that window appears: tab-only.
    let mut all = Vec::new();
    all.extend(deliver(
        &mut t.fx,
        strip(t.a, 1, &[(1, true), (2, false), (4, false)]),
        SnapshotOrigin::Created,
    ));
    all.extend(deliver(
        &mut t.fx,
        snapshot(d, "tab 3", 4),
        SnapshotOrigin::Created,
    ));
    assert!(t.inactive(d));
    // Destroying inactive tabs.
    all.extend(destroy(&mut t.fx, d));
    all.extend(destroy(&mut t.fx, t.b));
    assert_eq!(
        t.fx.state.frames.desired, desired,
        "desired frames are untouched"
    );
    assert_eq!(t.fx.state.frames.plan_sequence, plan, "no new plan was made");
    assert!(writes(&all).is_empty(), "no frame was written: {all:?}");
    assert!(!all.iter().any(|e| matches!(e, Effect::CancelFrame { .. })));
}

#[test]
fn switching_the_active_tab_moves_only_the_affected_geometry() {
    let mut t = tabs();
    let a_frame: Rect = t.fx.state.frames.desired[&t.a];
    let c_frame: Rect = t.fx.state.frames.desired[&t.c];
    let plan = t.fx.state.frames.plan_sequence;
    // B becomes the selected tab and reports the strip.
    let effects = deliver(
        &mut t.fx,
        strip(t.b, 2, &[(1, false), (2, true)]),
        SnapshotOrigin::Created,
    );
    assert!(t.inactive(t.a) && !t.inactive(t.b));
    assert_eq!(
        t.fx.state.frames.desired[&t.b], a_frame,
        "B takes the group's slot"
    );
    assert_eq!(
        t.fx.state.frames.desired[&t.c], c_frame,
        "the ordinary window is untouched"
    );
    assert!(!t.fx.state.frames.desired.contains_key(&t.a));
    assert_eq!(writes(&effects), [t.b], "only the new active member is written");
    assert!(t.fx.state.frames.plan_sequence > plan);
    // The stale reporter's group is gone: exactly one group.
    assert_eq!(t.fx.state.snapshot().tabs.groups, 1);
}

#[test]
fn deleting_the_group_makes_its_members_ordinary_windows() {
    let mut t = tabs();
    let mut none = snapshot(t.a, "a", 1);
    none.tabs = TabInfo::None;
    deliver(&mut t.fx, none, SnapshotOrigin::Created);
    assert!(!t.inactive(t.b));
    assert!(t.fx.state.frames.desired.contains_key(&t.b), "B is a tile again");
    assert_eq!(t.fx.state.snapshot().tabs.groups, 0);
}

#[test]
fn destroying_the_reporter_drops_its_group() {
    let mut t = tabs();
    destroy(&mut t.fx, t.a);
    assert!(!t.inactive(t.b), "an orphaned tab is never left invisible");
    assert_eq!(t.fx.state.snapshot().tabs.groups, 0);
}

#[test]
fn a_reused_cg_id_never_matches_a_stale_member() {
    let mut feed = Feed::new(1);
    let mut state = ready("", &[info(1, "com.a", "A")], &mut feed);
    state.set_tab_reads(true);
    let (a1, b1) = (feed.token(1, 1), feed.token(1, 2));
    for (snap, origin) in [
        (strip(a1, 1, &[(1, true), (2, false)]), SnapshotOrigin::Created),
        (snapshot(b1, "b", 2), SnapshotOrigin::Created),
    ] {
        state = step(
            state,
            feed.input(WindowEventKind::Snapshot { snapshot: snap, origin }),
        )
        .0;
    }
    assert!(state.windows[&b1].inactive_tab);
    // The tab closes; its CG id is reused by a new, ordinary window of the same
    // application before the strip is read again.
    state = step(state, feed.change(b1, WindowChange::Destroyed)).0;
    let e1 = feed.token(1, 5);
    state = step(
        state,
        feed.input(WindowEventKind::Snapshot {
            snapshot: snapshot(e1, "e", 2),
            origin: SnapshotOrigin::Created,
        }),
    )
    .0;
    assert!(
        !state.windows[&e1].inactive_tab,
        "a stale member does not match a reused id"
    );
    // Another group of the same application claiming the same id replaces the
    // stale reporter's group (the newest reporter wins).
    state = step(
        state,
        feed.input(WindowEventKind::Snapshot {
            snapshot: strip(e1, 2, &[(2, true), (1, false)]),
            origin: SnapshotOrigin::Created,
        }),
    )
    .0;
    assert!(state.windows[&a1].inactive_tab && !state.windows[&e1].inactive_tab);
    assert_eq!(state.snapshot().tabs.groups, 1);
}

#[test]
fn an_unreliable_strip_excludes_nothing() {
    let mut t = tabs();
    let a = t.a;
    // Two selected members.
    deliver(
        &mut t.fx,
        strip(a, 1, &[(1, true), (2, true)]),
        SnapshotOrigin::Created,
    );
    assert!(!t.inactive(t.b));
    // No selected member.
    deliver(
        &mut t.fx,
        strip(a, 1, &[(1, false), (2, false)]),
        SnapshotOrigin::Created,
    );
    assert!(!t.inactive(t.b) && !t.inactive(t.a));
    // The selected member is not a tracked window of this application.
    deliver(
        &mut t.fx,
        strip(a, 1, &[(77, true), (2, false)]),
        SnapshotOrigin::Created,
    );
    assert!(!t.inactive(t.b));
    // An unknown flag is not evidence.
    let mut snap = strip(a, 1, &[(1, true), (2, false)]);
    snap.tab_members[1].selected = None;
    deliver(&mut t.fx, snap, SnapshotOrigin::Created);
    assert!(!t.inactive(t.b));
}

#[test]
fn a_failed_read_is_explicit_and_leaves_ordinary_windows_alone() {
    let mut t = tabs();
    let mut failed = snapshot(t.a, "a", 1);
    failed.tabs = TabInfo::Failed;
    deliver(&mut t.fx, failed, SnapshotOrigin::Created);
    assert!(
        !t.inactive(t.b),
        "a failed read drops the group instead of guessing"
    );
    let status = t.fx.state.snapshot().tabs;
    assert_eq!(status.unavailable_apps, 1);
    let diagnostics = crate::status::tab_diagnostics(&status);
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].contains("unavailable"));
    // Recovery clears the marker.
    deliver(
        &mut t.fx,
        strip(t.a, 1, &[(1, true), (2, false)]),
        SnapshotOrigin::Created,
    );
    assert_eq!(t.fx.state.snapshot().tabs.unavailable_apps, 0);
    assert!(t.inactive(t.b));
}

#[test]
fn with_reads_off_strips_are_ignored_and_the_diagnostic_is_explicit() {
    let mut t = tabs();
    t.fx.state.set_tab_reads(false);
    deliver(
        &mut t.fx,
        strip(t.a, 1, &[(1, true), (2, false)]),
        SnapshotOrigin::Created,
    );
    assert!(!t.inactive(t.b), "nothing is excluded without reads");
    let status = t.fx.state.snapshot().tabs;
    assert!(!status.reading);
    assert!(crate::status::tab_diagnostics(&status)[0].contains("not read"));
    assert!(crate::status::tab_diagnostics(&TabsStatus { reading: true, ..status }).is_empty());
}

#[test]
fn the_initial_scan_runs_once_per_app_and_changes_reconcile_one_window() {
    let mut feed = Feed::new(1);
    let mut state = ready("", &[info(1, "com.a", "A"), info(2, "com.b", "B")], &mut feed);
    state.set_tab_reads(true);
    // `ready` completed one enumeration per app before reads were declared on.
    let before = state.snapshot().tabs.stats.initial_scans;
    assert_eq!(before, 0, "reads were off during the fixture's scans");
    let complete = |feed: &mut Feed, n: u64| feed.complete(app(n));
    state = step(state, complete(&mut feed, 1)).0;
    state = step(state, complete(&mut feed, 1)).0;
    state = step(state, complete(&mut feed, 2)).0;
    assert_eq!(
        state.snapshot().tabs.stats.initial_scans,
        2,
        "once for each application"
    );
    // A later change re-reads exactly the one window, never the application.
    let w = feed.token(1, 1);
    state = step(
        state,
        feed.input(WindowEventKind::Snapshot {
            snapshot: strip(w, 1, &[(1, true), (9, false)]),
            origin: SnapshotOrigin::Created,
        }),
    )
    .0;
    let (state, effects) = step(state, feed.change(w, WindowChange::Main));
    let reads = effects
        .iter()
        .filter(|e| matches!(e, Effect::Snapshot { window, .. } if *window == w))
        .count();
    assert_eq!(reads, 1);
    assert!(!effects.iter().any(|e| matches!(e, Effect::Refresh { .. })));
    // With reads off the same hint reads nothing.
    let mut off = state;
    off.set_tab_reads(false);
    let (_, effects) = step(off, feed.change(w, WindowChange::Main));
    assert!(!effects.iter().any(|e| matches!(e, Effect::Snapshot { .. })));
}
