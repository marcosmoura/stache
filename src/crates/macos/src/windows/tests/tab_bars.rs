//! Tab bar identity over the real owner core and the fake AX arena: `AppKit`'s one
//! shared tab bar moves into the newly selected window, so two windows carry the
//! same bar exactly when their `AXTabGroup` elements are the same object.

use super::*;
use crate::ax::WindowIdError;

fn tab_group(ax: &FakeWindowAx, tabs: usize, selected: usize) -> NodeId {
    let buttons: Vec<NodeId> = (0..tabs)
        .map(|index| {
            ax.add_node(FakeNode {
                selected: Ok(index == selected),
                ..FakeNode::default()
            })
        })
        .collect();
    let group = ax.add_node(FakeNode {
        children: Ok(vec![]),
        tabs: Ok(buttons),
        ..FakeNode::default()
    });
    ax.edit(group, |node| {
        node.strings.insert(WindowString::Role, Ok("AXTabGroup".to_owned()));
    });
    group
}

fn carry(ax: &FakeWindowAx, window: NodeId, group: NodeId) {
    ax.edit(window, |node| node.children = Ok(vec![group]));
}

fn bar(snapshot: &WindowSnapshot) -> TabBar {
    match snapshot.tab_bar {
        TabBarRead::Bar(bar) => bar,
        other => panic!("expected a tab bar, got {other:?}"),
    }
}

fn gone(events: &[WindowEvent]) -> Vec<WindowInstance> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            WindowEventKind::Window {
                window,
                change: WindowChange::Destroyed,
            } => Some(*window),
            _ => None,
        })
        .collect()
}

fn suspended(events: &[WindowEvent]) -> Vec<WindowInstance> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            WindowEventKind::Window {
                window,
                change: WindowChange::Suspended,
            } => Some(*window),
            _ => None,
        })
        .collect()
}

fn unsettled(events: &[WindowEvent]) -> usize {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            WindowEventKind::EnumerationUnsettled { windows, .. } => Some(*windows),
            _ => None,
        })
        .sum()
}

#[test]
fn tab_bars_are_not_read_unless_enabled() {
    let mut h = H::new();
    let window = window_node(&h.ax, 1);
    let group = tab_group(&h.ax, 2, 0);
    carry(&h.ax, window, group);
    app_node(&h.ax, 9, vec![window]);
    h.track(app(1), 9);
    h.settle();
    assert_eq!(snapshots(&h.events())[0].tab_bar, TabBarRead::NotRead);
    assert!(!h.ax.calls().iter().any(|call| matches!(call, Call::Children(..))));
}

#[test]
fn the_selected_tab_switching_windows_keeps_the_bar_id() {
    let mut h = H::with_options(256, false, true);
    let first = window_node(&h.ax, 1);
    let group = tab_group(&h.ax, 3, 0);
    carry(&h.ax, first, group);
    let app_el = app_node(&h.ax, 9, vec![first]);
    h.track(app(1), 9);
    h.settle();
    let before = snapshots(&h.events());
    assert_eq!(before.len(), 1);
    let first_bar = bar(&before[0]);
    assert_eq!((first_bar.count, first_bar.selected), (3, Some(0)));

    // Another tab became selected: AppKit lists a different window for the group
    // (no creation notification), the shared tab bar moved into it.
    let second = window_node(&h.ax, 2);
    h.ax.edit(first, |node| node.children = Ok(vec![]));
    carry(&h.ax, second, group);
    h.ax.edit(app_el, |node| node.windows = Ok(vec![second]));
    let events = refresh(&mut h, app(1));
    let after = snapshots(&events);
    assert_eq!(after.len(), 1);
    assert_ne!(after[0].window, before[0].window, "a new exact token");
    assert_eq!(bar(&after[0]).id, first_bar.id, "the same shared tab bar");
    assert!(
        gone(&events).is_empty(),
        "the old window is alive, so it is not retired"
    );
    assert_eq!(
        suspended(&events),
        vec![before[0].window],
        "the old window left the list and is suspended"
    );
}

#[test]
fn a_created_tab_window_carries_the_existing_bar_id() {
    let mut h = H::with_options(256, false, true);
    let first = window_node(&h.ax, 1);
    let group = tab_group(&h.ax, 2, 0);
    carry(&h.ax, first, group);
    let app_el = app_node(&h.ax, 9, vec![first]);
    h.track(app(1), 9);
    h.settle();
    let first_bar = bar(&snapshots(&h.events())[0]);

    // A new tab: `AXWindowCreated`, the old window leaves without a destroy.
    let fresh = window_node(&h.ax, 2);
    carry(&h.ax, fresh, group);
    h.ax.edit(app_el, |node| node.windows = Ok(vec![fresh]));
    h.obs.inject(9, AxNotification::WindowCreated, fresh);
    h.settle();
    let events = h.events();
    let created: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            WindowEventKind::Snapshot {
                snapshot,
                origin: SnapshotOrigin::Created,
            } => Some(snapshot.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(created.len(), 1);
    assert_eq!(bar(&created[0]).id, first_bar.id);
    assert!(
        gone(&events).is_empty(),
        "no destroy for a window that merely left the list"
    );
}

#[test]
fn different_bars_and_plain_windows_get_distinct_identities() {
    let mut h = H::with_options(256, false, true);
    let (a, b, plain) = (
        window_node(&h.ax, 1),
        window_node(&h.ax, 2),
        window_node(&h.ax, 3),
    );
    let (group_a, group_b) = (tab_group(&h.ax, 2, 0), tab_group(&h.ax, 2, 1));
    carry(&h.ax, a, group_a);
    carry(&h.ax, b, group_b);
    h.ax.edit(plain, |node| node.children = Ok(vec![]));
    app_node(&h.ax, 9, vec![a, b, plain]);
    h.track(app(1), 9);
    h.settle();
    let shots = snapshots(&h.events());
    assert_ne!(bar(&shots[0]).id, bar(&shots[1]).id);
    assert_eq!(bar(&shots[1]).selected, Some(1));
    assert_eq!(shots[2].tab_bar, TabBarRead::None);
}

#[test]
fn a_tab_group_nested_in_the_window_is_found_within_the_bounds() {
    let mut h = H::with_options(256, false, true);
    let window = window_node(&h.ax, 1);
    let group = tab_group(&h.ax, 2, 0);
    let inner = h.ax.add_node(FakeNode {
        children: Ok(vec![group]),
        ..FakeNode::default()
    });
    let outer = h.ax.add_node(FakeNode {
        children: Ok(vec![inner]),
        ..FakeNode::default()
    });
    h.ax.edit(window, |node| node.children = Ok(vec![outer]));
    // A group deeper than the bound is not found, and says so as "none".
    let deep = (0..8).fold(group, |child, _| {
        h.ax.add_node(FakeNode {
            children: Ok(vec![child]),
            ..FakeNode::default()
        })
    });
    let far = window_node(&h.ax, 2);
    h.ax.edit(far, |node| node.children = Ok(vec![deep]));
    app_node(&h.ax, 9, vec![window, far]);
    h.track(app(1), 9);
    h.settle();
    let shots = snapshots(&h.events());
    assert!(matches!(shots[0].tab_bar, TabBarRead::Bar(_)));
    assert_eq!(shots[1].tab_bar, TabBarRead::None);
}

#[test]
fn a_window_in_transition_is_neither_adopted_nor_destroyed_and_settles_later() {
    let mut h = H::with_options(256, false, true);
    let first = window_node(&h.ax, 1);
    let group = tab_group(&h.ax, 3, 0);
    carry(&h.ax, first, group);
    let app_el = app_node(&h.ax, 9, vec![first]);
    h.track(app(1), 9);
    h.settle();
    let before = snapshots(&h.events());
    let first_bar = bar(&before[0]);

    // A tab closed: the remaining window is listed, but its id and its tab bar
    // cannot be read yet.
    let remaining = window_node(&h.ax, 2);
    h.ax.edit(remaining, |node| {
        node.window_id = Err(WindowIdError::NoWindow);
        node.children = Err(AxError::CannotComplete);
    });
    h.ax.edit(app_el, |node| node.windows = Ok(vec![remaining]));
    h.ax.kill(first);
    let events = refresh(&mut h, app(1));
    assert!(
        snapshots(&events).is_empty(),
        "the transient window is not adopted"
    );
    assert_eq!(unsettled(&events), 1);
    assert_eq!(
        gone(&events),
        vec![before[0].window],
        "only the unlisted window left"
    );
    assert_eq!(h.core.tracked_windows(), 0, "no token was minted for it");
    assert!(
        h.obs.registrations(9).iter().all(|(node, _)| *node != remaining),
        "its registrations did not stay behind"
    );

    // It settles: the next listing adopts it, with the bar it always carried.
    h.ax.edit(remaining, |node| {
        node.window_id = Ok(2);
        node.children = Ok(vec![group]);
    });
    let events = refresh(&mut h, app(1));
    let after = snapshots(&events);
    assert_eq!(after.len(), 1);
    assert_eq!(bar(&after[0]).id, first_bar.id);
    assert_eq!(unsettled(&events), 0);
    assert_eq!(
        after[0].window.serial(),
        before[0].window.serial() + 1,
        "the failed attempt consumed no serial"
    );
}

#[test]
fn a_creation_in_transition_asks_for_a_later_listing() {
    let mut h = H::with_options(256, false, true);
    let first = window_node(&h.ax, 1);
    let app_el = app_node(&h.ax, 9, vec![first]);
    h.track(app(1), 9);
    h.settle();
    let _ = h.events();
    let fresh = window_node(&h.ax, 2);
    h.ax.edit(fresh, |node| {
        node.window_id = Err(WindowIdError::NoWindow);
        node.children = Err(AxError::CannotComplete);
    });
    h.ax.edit(app_el, |node| node.windows = Ok(vec![first, fresh]));
    h.obs.inject(9, AxNotification::WindowCreated, fresh);
    h.settle();
    let events = h.events();
    assert_eq!(unsettled(&events), 1);
    assert!(snapshots(&events).is_empty());
    assert_eq!(h.core.tracked_windows(), 1);
}

#[test]
fn a_window_without_an_id_but_with_a_readable_tree_is_still_adopted() {
    let mut h = H::with_options(256, false, true);
    let window = window_node(&h.ax, 1);
    h.ax.edit(window, |node| {
        node.window_id = Err(WindowIdError::NoWindow);
        node.children = Ok(vec![]);
    });
    app_node(&h.ax, 9, vec![window]);
    h.track(app(1), 9);
    h.settle();
    let shots = snapshots(&h.events());
    assert_eq!(
        shots.len(),
        1,
        "only an unreadable tab bar makes a window transient"
    );
    assert_eq!(shots[0].tab_bar, TabBarRead::None);
}

#[test]
fn geometry_reads_reuse_the_cached_bar_without_searching_again() {
    let mut h = H::with_options(256, false, true);
    let window = window_node(&h.ax, 1);
    let group = tab_group(&h.ax, 2, 1);
    carry(&h.ax, window, group);
    app_node(&h.ax, 9, vec![window]);
    h.track(app(1), 9);
    h.settle();
    let token = snapshots(&h.events())[0].window;
    h.ax.clear_calls();
    let outcome = h.run(|reply| Command::Snapshot { window: token, reply });
    let SnapshotOutcome::Snapshot(snapshot) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(bar(&snapshot).selected, Some(1));
    assert!(
        !h.ax
            .calls()
            .iter()
            .any(|call| matches!(call, Call::Children(..) | Call::Tabs(..))),
        "no tree search for a position read"
    );
}

#[test]
fn bars_nobody_carries_age_out_but_survive_a_close_listing_or_two() {
    let mut h = H::with_options(256, false, true);
    let first = window_node(&h.ax, 1);
    let group = tab_group(&h.ax, 2, 0);
    carry(&h.ax, first, group);
    let app_el = app_node(&h.ax, 9, vec![first]);
    h.track(app(1), 9);
    h.settle();
    let first_bar = bar(&snapshots(&h.events())[0]);

    // The group's window closes; unrelated plain windows are listed meanwhile.
    let plain = window_node(&h.ax, 5);
    h.ax.edit(plain, |node| node.children = Ok(vec![]));
    h.ax.edit(app_el, |node| node.windows = Ok(vec![plain]));
    h.ax.kill(first);
    let _ = refresh(&mut h, app(1));
    let _ = refresh(&mut h, app(1));
    let successor = window_node(&h.ax, 6);
    carry(&h.ax, successor, group);
    h.ax.edit(app_el, |node| node.windows = Ok(vec![plain, successor]));
    let shots = snapshots(&refresh(&mut h, app(1)));
    let adopted = shots.iter().find(|s| matches!(s.tab_bar, TabBarRead::Bar(_))).expect("bar");
    assert_eq!(
        bar(adopted).id,
        first_bar.id,
        "two listings later it is still known"
    );

    // Without a carrier for several settled listings the entry is released.
    h.ax.edit(app_el, |node| node.windows = Ok(vec![plain]));
    h.ax.kill(successor);
    for _ in 0..4 {
        let _ = refresh(&mut h, app(1));
    }
    let late = window_node(&h.ax, 7);
    carry(&h.ax, late, group);
    h.ax.edit(app_el, |node| node.windows = Ok(vec![plain, late]));
    let shots = snapshots(&refresh(&mut h, app(1)));
    let adopted = shots.iter().find(|s| matches!(s.tab_bar, TabBarRead::Bar(_))).expect("bar");
    assert_ne!(
        bar(adopted).id,
        first_bar.id,
        "an old orphan is not matched any more"
    );
}

#[test]
fn the_retained_tab_group_is_released_with_the_application() {
    let mut h = H::with_options(256, false, true);
    let window = window_node(&h.ax, 1);
    let group = tab_group(&h.ax, 2, 0);
    carry(&h.ax, window, group);
    app_node(&h.ax, 9, vec![window]);
    h.track(app(1), 9);
    h.settle();
    assert!(
        h.ax.live_elements() > 0,
        "the owner holds the window and the tab group"
    );
    let epoch = h.epoch;
    let outcome = h.run(|reply| Command::Untrack {
        epoch,
        application: app(1),
        reply,
    });
    assert!(matches!(outcome, StopOutcome::Clean { .. }), "{outcome:?}");
    assert_eq!(h.ax.live_elements(), 0, "nothing leaked");
}

#[test]
fn the_retained_tab_group_is_released_when_the_runtime_stops() {
    let mut h = H::with_options(256, false, true);
    let window = window_node(&h.ax, 1);
    let group = tab_group(&h.ax, 2, 0);
    carry(&h.ax, window, group);
    app_node(&h.ax, 9, vec![window]);
    h.track(app(1), 9);
    h.settle();
    assert!(matches!(h.stop(), StopOutcome::Clean { .. }));
    assert_eq!(h.ax.live_elements(), 0);
}
