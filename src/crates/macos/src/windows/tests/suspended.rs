//! Suspended windows over the real owner core and the fake AX arena: a tracked
//! window that left `AXWindows` while its element is alive (an inactive native
//! tab, or the one window of an application whose first new tab replaced it) is
//! never retired. Dead elements are.

use super::*;

fn changes(events: &[WindowEvent], wanted: WindowChange) -> Vec<WindowInstance> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            WindowEventKind::Window { window, change } if *change == wanted => Some(*window),
            _ => None,
        })
        .collect()
}

#[test]
fn an_unlisted_window_with_a_live_element_is_suspended_not_retired() {
    let mut h = H::new();
    let (app_el, nodes, tokens) = h.app_with_windows(app(1), 500, 2);
    h.ax.edit(app_el, |n| n.windows = Ok(vec![nodes[0]]));
    let events = refresh(&mut h, app(1));
    assert_eq!(changes(&events, WindowChange::Suspended), vec![tokens[1]]);
    assert!(changes(&events, WindowChange::Destroyed).is_empty());
    assert_eq!(h.core.tracked_windows(), 2, "still tracked");
    assert!(
        h.obs.registrations(500).iter().any(|(node, _)| *node == nodes[1]),
        "its observer registrations stay"
    );

    // A second listing does not report it again.
    let events = refresh(&mut h, app(1));
    assert!(changes(&events, WindowChange::Suspended).is_empty());

    // It cannot act while unlisted, and the token stays valid.
    assert_eq!(
        h.act(tokens[1], WindowAction::Raise),
        WindowActionOutcome::Stale(StaleReason::Suspended)
    );
    assert_eq!(h.core.tracked_windows(), 2);

    // Listed again: the same token is reported.
    h.ax.edit(app_el, |n| n.windows = Ok(vec![nodes[1]]));
    let events = refresh(&mut h, app(1));
    let shots = snapshots(&events);
    assert_eq!(shots.iter().map(|s| s.window).collect::<Vec<_>>(), vec![
        tokens[1]
    ]);
    assert_eq!(changes(&events, WindowChange::Suspended), vec![tokens[0]]);

    // A listed window can act again.
    assert!(matches!(
        h.act(tokens[1], frame(5.0)),
        WindowActionOutcome::Issued { .. }
    ));
}

#[test]
fn a_suspended_window_is_retired_when_its_element_is_destroyed() {
    let mut h = H::new();
    let (app_el, nodes, tokens) = h.app_with_windows(app(1), 500, 2);
    h.ax.edit(app_el, |n| n.windows = Ok(vec![nodes[0]]));
    let _ = refresh(&mut h, app(1));

    h.obs.inject(500, AxNotification::ElementDestroyed, nodes[1]);
    h.turn();
    assert_eq!(changes(&h.events(), WindowChange::Destroyed), vec![tokens[1]]);
    assert_eq!(h.core.tracked_windows(), 1);
    assert!(h.obs.registrations(500).iter().all(|(node, _)| *node != nodes[1]));
}

#[test]
fn a_suspended_window_whose_element_died_is_retired_by_the_next_listing() {
    let mut h = H::new();
    let (app_el, nodes, tokens) = h.app_with_windows(app(1), 500, 2);
    h.ax.edit(app_el, |n| n.windows = Ok(vec![nodes[0]]));
    let _ = refresh(&mut h, app(1));
    h.ax.kill(nodes[1]);
    let events = refresh(&mut h, app(1));
    assert_eq!(changes(&events, WindowChange::Destroyed), vec![tokens[1]]);
    assert_eq!(h.core.tracked_windows(), 1);
}

#[test]
fn a_window_whose_id_changed_is_not_the_same_live_window() {
    let mut h = H::new();
    let (app_el, nodes, tokens) = h.app_with_windows(app(1), 500, 2);
    h.ax.edit(app_el, |n| n.windows = Ok(vec![nodes[0]]));
    // The element answers, but for another window id: it is not ours any more.
    h.ax.edit(nodes[1], |n| n.window_id = Ok(9999));
    let events = refresh(&mut h, app(1));
    assert_eq!(changes(&events, WindowChange::Destroyed), vec![tokens[1]]);
}

#[test]
fn the_window_that_left_the_list_is_suspended_before_the_one_that_replaces_it() {
    let mut h = H::with_options(256, false, true);
    let first = window_node(&h.ax, 1);
    let app_el = app_node(&h.ax, 9, vec![first]);
    h.track(app(1), 9);
    h.settle();
    let before = snapshots(&h.events());
    assert_eq!(before.len(), 1);

    // A new tab: the creation is announced, the old window is no longer listed.
    let fresh = window_node(&h.ax, 2);
    h.ax.edit(app_el, |n| n.windows = Ok(vec![fresh]));
    h.obs.inject(9, AxNotification::WindowCreated, fresh);
    h.settle();
    let kinds = kinds(&h.events());
    let suspended = kinds.iter().position(|kind| {
        matches!(kind, WindowEventKind::Window {
            window,
            change: WindowChange::Suspended,
        } if *window == before[0].window)
    });
    let created = kinds.iter().position(|kind| {
        matches!(kind, WindowEventKind::Snapshot {
            origin: SnapshotOrigin::Created,
            ..
        })
    });
    assert!(suspended.is_some() && created.is_some(), "{kinds:?}");
    assert!(suspended < created, "the departure comes first: {kinds:?}");
    assert!(
        !kinds.iter().any(|kind| matches!(kind, WindowEventKind::Window {
            change: WindowChange::Destroyed,
            ..
        })),
        "nothing is destroyed"
    );
}

#[test]
fn a_returning_window_found_by_a_creation_notification_is_reported() {
    let mut h = H::with_options(256, false, true);
    let first = window_node(&h.ax, 1);
    let app_el = app_node(&h.ax, 9, vec![first]);
    h.track(app(1), 9);
    h.settle();
    let token = snapshots(&h.events())[0].window;

    let fresh = window_node(&h.ax, 2);
    h.ax.edit(app_el, |n| n.windows = Ok(vec![fresh]));
    h.obs.inject(9, AxNotification::WindowCreated, fresh);
    h.settle();
    let _ = h.events();

    // The new window closes and the old one is listed again, announced by a
    // creation: the same token comes back as a snapshot.
    h.ax.kill(fresh);
    h.ax.edit(app_el, |n| n.windows = Ok(vec![first]));
    h.obs.inject(9, AxNotification::WindowCreated, first);
    h.settle();
    let events = h.events();
    let shots = snapshots(&events);
    assert_eq!(shots.iter().map(|s| s.window).collect::<Vec<_>>(), vec![token]);
}

#[test]
fn only_a_bounded_number_of_windows_stay_suspended() {
    let mut h = H::new();
    let count = u32::try_from(MAX_SUSPENDED_PER_APPLICATION).expect("small") + 8;
    let (app_el, _nodes, tokens) = h.app_with_windows(app(1), 500, count);
    h.ax.edit(app_el, |n| n.windows = Ok(vec![]));
    let events = refresh(&mut h, app(1));
    let suspended = changes(&events, WindowChange::Suspended);
    let retired = changes(&events, WindowChange::Destroyed);
    assert_eq!(suspended.len(), tokens.len());
    assert_eq!(retired.len(), 8, "the oldest beyond the bound are retired");
    assert_eq!(h.core.tracked_windows(), MAX_SUSPENDED_PER_APPLICATION);
    assert!(
        retired.iter().all(|token| tokens[..8].contains(token)),
        "oldest first"
    );
}

#[test]
fn an_aborted_listing_suspends_and_retires_nothing() {
    let mut h = H::new();
    let (app_el, nodes, _) = h.app_with_windows(app(1), 500, 2);
    h.ax.edit(app_el, |n| n.windows = Ok(vec![nodes[0]]));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let epoch = h.epoch;
    let _ = h.run(|reply| Command::Refresh {
        epoch,
        application: app(1),
        cancel,
        deadline: None,
        reply,
    });
    h.settle();
    let events = h.events();
    assert!(changes(&events, WindowChange::Suspended).is_empty());
    assert!(changes(&events, WindowChange::Destroyed).is_empty());
}

#[test]
fn suspended_windows_release_their_elements_with_the_application() {
    let mut h = H::new();
    let (app_el, nodes, _) = h.app_with_windows(app(1), 500, 3);
    h.ax.edit(app_el, |n| n.windows = Ok(vec![nodes[0]]));
    let _ = refresh(&mut h, app(1));
    assert_eq!(h.core.tracked_windows(), 3);
    let epoch = h.epoch;
    let outcome = h.run(|reply| Command::Untrack {
        epoch,
        application: app(1),
        reply,
    });
    assert!(matches!(outcome, StopOutcome::Clean { .. }), "{outcome:?}");
    assert_eq!(h.ax.live_elements(), 0, "nothing leaked");
    assert!(h.obs.registrations(500).is_empty());
}

#[test]
fn suspended_windows_are_released_when_the_runtime_stops() {
    let mut h = H::new();
    let (app_el, nodes, _) = h.app_with_windows(app(1), 500, 3);
    h.ax.edit(app_el, |n| n.windows = Ok(vec![nodes[0]]));
    let _ = refresh(&mut h, app(1));
    assert!(matches!(h.stop(), StopOutcome::Clean { .. }));
    assert_eq!(h.ax.live_elements(), 0);
}

/// Native run 4: a window that entered native fullscreen left `AXWindows`
/// (suspended) and was listed again after the exit. The consumer learned that
/// from a single-window snapshot, so that read must also lift the owner's own
/// suspension: otherwise every intermediate animation frame answers
/// `Stale(Suspended)` and a later unlisting is never reported again.
#[test]
fn a_suspended_window_read_while_listed_again_is_no_longer_suspended() {
    use super::frames::{frame_request, taken};
    let mut h = H::new();
    let (app_el, nodes, tokens) = h.app_with_windows(app(1), 500, 1);
    h.ax.edit(app_el, |n| n.windows = Ok(Vec::new()));
    let events = refresh(&mut h, app(1));
    assert_eq!(changes(&events, WindowChange::Suspended), vec![tokens[0]]);

    // Listed again; only a single-window read observes it.
    h.ax.edit(app_el, |n| n.windows = Ok(vec![nodes[0]]));
    let outcome = h.run(|reply| Command::Snapshot { window: tokens[0], reply });
    assert!(matches!(outcome, SnapshotOutcome::Snapshot(_)), "{outcome:?}");

    let replies = h.put(frame_request(tokens[0], 1, 5.0, FrameStage::Intermediate));
    h.turn();
    let got = taken(&replies);
    assert!(
        matches!(got.as_slice(), [FrameReply::Done(FrameAck {
            outcome: WindowActionOutcome::Issued { .. },
            ..
        })]),
        "{got:?}"
    );

    // Unlisted again: the suspension is reported again.
    h.ax.edit(app_el, |n| n.windows = Ok(Vec::new()));
    let events = refresh(&mut h, app(1));
    assert_eq!(changes(&events, WindowChange::Suspended), vec![tokens[0]]);
}
