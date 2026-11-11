//! Native tab groups through the whole actor over fakes.
//!
//! `AppKit` shows a tab group through ONE window at a time and swaps that window
//! (and its `CGWindowID`) on a new tab, a tab switch and a tab close, without a
//! creation notification when an existing tab is selected. The owner reports the
//! shared tab bar as an opaque id; the windows of one application that carry the
//! same id are one group and keep one slot. Nothing here is matched by frame,
//! title or `CGWindowID`.

use serde_json::json;
use stache_macos::ax::{AxPoint, AxReading, AxSize, FrameRead};
use stache_macos::windows::{TabBar, TabBarId, TabBarRead, WindowSnapshot};

use super::*;
use crate::actor::{MAX_TAB_SETTLE_RETRIES, TAB_SETTLE_DELAY};
use crate::config::TilingConfig;
use crate::geometry::Rect;
use crate::intent::Intent;
use crate::state::Input;

const FINDER: u64 = 1;
const OTHER: u64 = 2;

fn config() -> Arc<TilingConfig> {
    Arc::new(
        serde_json::from_value(json!({
            "enabled": true,
            "workspaces": [
                { "name": "main", "layout": "master", "screen": "main" },
                { "name": "files", "layout": "master", "screen": "main",
                  "rules": [{ "app_id": "com.apple.finder" }] },
            ],
        }))
        .expect("config"),
    )
}

pub(super) fn bar(id: u64, count: usize) -> TabBarRead {
    TabBarRead::Bar(TabBar {
        id: TabBarId::for_testing(id),
        count,
        selected: Some(0),
    })
}

/// A desktop with a Finder-like application and another application.
pub(super) struct Tabs {
    pub(super) rig: Rig,
    serial: u64,
}

impl Tabs {
    /// `finder_on_main`: Finder windows join the visible `main` workspace
    /// (otherwise a rule sends them to the hidden `files` workspace).
    pub(super) fn new() -> Self {
        let mut rig = Rig::new(
            "",
            vec![
                info(FINDER, "com.apple.finder", "Finder"),
                info(OTHER, "com.b", "B"),
            ],
            256,
            256,
        );
        rig.backend.script_echo_frames(true);
        rig.start();
        // Finder is not matched by the `files` rule here: drop it for the
        // visible-workspace tests (rules are exercised in one dedicated test).
        let config = serde_json::from_value(json!({
            "enabled": true,
            "workspaces": [{ "name": "main", "layout": "master", "screen": "main" }],
        }))
        .expect("config");
        rig.ingress
            .submit(Input::Config {
                revision: 1,
                config: Arc::new(config),
            })
            .expect("config");
        rig.gate.publish(rig.feed.stamp.registry, 1);
        rig.ingress.submit(Input::FrameWrites(true)).expect("writes");
        rig.pump();
        Self { rig, serial: 0 }
    }

    pub(super) fn token(&mut self, app: u64) -> WindowInstance {
        self.serial += 1;
        self.rig.token(app, self.serial)
    }

    pub(super) fn snapshot(
        token: WindowInstance,
        bar: TabBarRead,
        frame: Option<Rect>,
    ) -> WindowSnapshot {
        let mut snapshot = snapshot(token, "w", 100 + u32::try_from(token.serial()).unwrap_or(0));
        // Finder windows report AXDialog (kept: >= 400x300).
        snapshot.subrole = AxReading::Value("AXDialog".to_owned());
        snapshot.tab_bar = bar;
        if let Some(frame) = frame {
            snapshot.frame = FrameRead {
                position: AxReading::Value(AxPoint { x: frame.x, y: frame.y }),
                size: AxReading::Value(AxSize {
                    width: frame.width,
                    height: frame.height,
                }),
            };
        }
        snapshot
    }

    pub(super) fn enumerate(&mut self, token: WindowInstance, bar: TabBarRead) {
        let snapshot = Self::snapshot(token, bar, None);
        self.rig.push(WindowEventKind::Snapshot {
            snapshot,
            origin: SnapshotOrigin::Enumeration,
        });
        self.rig.pump();
    }

    /// Both applications enumerated their first scan.
    pub(super) fn finish_startup(&mut self) {
        self.rig.complete(app(FINDER));
        self.rig.complete(app(OTHER));
        self.rig.pump();
    }

    /// The window as laid out now: the frame a native tab swap keeps.
    pub(super) fn slot(&self, token: WindowInstance) -> Rect {
        self.rig
            .snap()
            .frames
            .iter()
            .find(|status| status.window == token)
            .and_then(|status| status.actual.or(status.desired))
            .expect("frame")
    }

    pub(super) fn order(&self) -> Vec<WindowInstance> {
        self.rig.snap().workspaces.iter().flat_map(|ws| ws.order.clone()).collect()
    }

    /// The tiles: tracked windows that are not suspended members of a slot.
    pub(super) fn windows(&self) -> Vec<WindowInstance> {
        self.rig
            .snap()
            .windows
            .iter()
            .filter(|w| !w.suspended)
            .map(|w| w.token)
            .collect()
    }

    /// Every tracked window, suspended members included.
    pub(super) fn tracked(&self) -> Vec<WindowInstance> {
        self.rig.snap().windows.iter().map(|w| w.token).collect()
    }

    pub(super) fn writes(&self) -> usize { self.rig.backend.written_frames().len() }

    pub(super) fn refreshes(&self) -> usize {
        self.rig
            .calls()
            .iter()
            .filter(|call| matches!(call, FakeBackendCall::Refresh(a) if *a == app(FINDER)))
            .count()
    }

    /// Lets the actor clock pass `by` and runs what came due.
    pub(super) fn wait(&mut self, by: Duration) {
        self.rig.clock.advance(by);
        self.rig.engine.turn();
        self.rig.pump();
    }

    pub(super) fn settle_delay(&mut self) { self.wait(TAB_SETTLE_DELAY); }

    pub(super) fn focus(&mut self, token: WindowInstance) {
        self.rig.push(WindowEventKind::Window {
            window: token,
            change: WindowChange::Focused,
        });
        self.rig.pump();
    }

    pub(super) fn destroyed(&mut self, token: WindowInstance) {
        self.rig.push(WindowEventKind::Window {
            window: token,
            change: WindowChange::Destroyed,
        });
        self.rig.pump();
    }

    pub(super) fn listed(&mut self, token: WindowInstance, bar: TabBarRead, frame: Rect) {
        let snapshot = Self::snapshot(token, bar, Some(frame));
        self.rig.push(WindowEventKind::Snapshot {
            snapshot,
            origin: SnapshotOrigin::Enumeration,
        });
        self.rig.pump();
    }

    pub(super) fn created(&mut self, token: WindowInstance, bar: TabBarRead, frame: Rect) {
        let snapshot = Self::snapshot(token, bar, Some(frame));
        self.rig.push(WindowEventKind::Snapshot {
            snapshot,
            origin: SnapshotOrigin::Created,
        });
        self.rig.pump();
    }

    pub(super) fn end_listing(&mut self) {
        self.rig.complete(app(FINDER));
        self.rig.pump();
    }

    pub(super) fn unsettled_listing(&mut self) {
        self.rig.push(WindowEventKind::EnumerationUnsettled {
            application: app(FINDER),
            windows: 1,
        });
        self.end_listing();
    }

    /// Finder with a three-tab group in master next to another app's window.
    pub(super) fn finder_group() -> (Self, WindowInstance, WindowInstance) {
        let mut tabs = Self::new();
        let finder = tabs.token(FINDER);
        let other = tabs.token(OTHER);
        tabs.enumerate(finder, bar(7, 3));
        tabs.enumerate(other, TabBarRead::None);
        tabs.finish_startup();
        assert_eq!(tabs.windows().len(), 2);
        assert!(tabs.writes() >= 1, "the startup layout was written");
        (tabs, finder, other)
    }
}

#[test]
fn a_finder_dialog_window_is_tracked_and_a_small_one_is_not() {
    let mut tabs = Tabs::new();
    let big = tabs.token(FINDER);
    let small = tabs.token(FINDER);
    tabs.enumerate(big, TabBarRead::None);
    let tiny = Tabs::snapshot(small, TabBarRead::None, Some(Rect::new(0.0, 0.0, 300.0, 200.0)));
    tabs.rig.push(WindowEventKind::Snapshot {
        snapshot: tiny,
        origin: SnapshotOrigin::Enumeration,
    });
    tabs.finish_startup();
    assert_eq!(
        tabs.windows(),
        vec![big],
        "AXDialog >= 400x300 is a window; a sheet-sized one not"
    );
}

#[test]
fn switching_tabs_swaps_the_window_without_a_relayout_or_a_write() {
    let (mut tabs, finder, other) = Tabs::finder_group();
    tabs.focus(finder);
    let writes = tabs.writes();
    let slot = tabs.slot(finder);
    let order = tabs.order();

    // AppKit reports the main window changing; no window is created.
    tabs.rig.push(WindowEventKind::Application {
        application: app(FINDER),
        change: stache_macos::windows::ApplicationChange::MainWindowChanged,
    });
    tabs.rig.pump();
    assert_eq!(tabs.refreshes(), 0, "nothing is listed before AppKit settled");
    tabs.settle_delay();
    assert_eq!(tabs.refreshes(), 1);

    // The listing: the group is now represented by another window carrying the
    // same shared tab bar; the old one left the list.
    let selected = tabs.token(FINDER);
    tabs.listed(selected, bar(7, 3), slot);
    tabs.destroyed(finder);
    tabs.end_listing();

    let snap = tabs.rig.snap();
    assert_eq!(tabs.windows().len(), 2, "one tile per group");
    assert!(snap.window(finder).is_none() && snap.window(selected).is_some());
    assert_eq!(tabs.order().len(), order.len());
    assert_eq!(tabs.order(), vec![selected, other], "same slot, same position");
    assert_eq!(snap.focus.window, Some(selected), "focus follows the group");
    assert_eq!(
        tabs.writes(),
        writes,
        "nothing is written when the window is on its slot"
    );
    assert_eq!(snap.diagnostics.tab_handovers, 1);
    assert_eq!(
        snap.window(selected).expect("window").desired.workspace,
        snap.focus.workspace
    );
}

#[test]
fn opening_a_tab_hands_the_slot_to_the_new_window_at_once() {
    let (mut tabs, finder, other) = Tabs::finder_group();
    let writes = tabs.writes();
    let slot = tabs.slot(finder);

    // `AXWindowCreated` for the new tab; the old window leaves without a destroy.
    let fresh = tabs.token(FINDER);
    tabs.created(fresh, bar(7, 4), slot);
    assert_eq!(tabs.order(), vec![fresh, other], "no extra tile, no jump");
    assert_eq!(tabs.windows().len(), 2);
    assert_eq!(tabs.writes(), writes);

    // The settled listing finds nothing new; the old token's late destroy is inert.
    tabs.settle_delay();
    tabs.destroyed(finder);
    tabs.end_listing();
    assert_eq!(tabs.order(), vec![fresh, other]);
    assert_eq!(tabs.writes(), writes);
    assert_eq!(tabs.rig.snap().diagnostics.tab_handovers, 1);
}

#[test]
fn closing_a_tab_keeps_the_slot_through_a_transient_listing() {
    let (mut tabs, finder, other) = Tabs::finder_group();
    let writes = tabs.writes();
    let slot = tabs.slot(finder);

    // The selected tab is closed: its window is destroyed. The tile stays where
    // it is (held), the other window is not relaid out.
    tabs.destroyed(finder);
    assert_eq!(
        tabs.order(),
        vec![finder, other],
        "the slot is held, not reflowed"
    );
    assert_eq!(tabs.writes(), writes);
    assert!(tabs.rig.snap().window(finder).expect("held").departing.is_some());

    // First listing: the remaining window is listed but unreadable yet.
    tabs.settle_delay();
    assert_eq!(tabs.refreshes(), 1);
    tabs.unsettled_listing();
    assert_eq!(
        tabs.order(),
        vec![finder, other],
        "a transient snapshot decides nothing"
    );
    assert_eq!(tabs.writes(), writes);

    // A retry, after the delay again; now the window reads.
    tabs.settle_delay();
    assert_eq!(tabs.refreshes(), 2);
    let remaining = tabs.token(FINDER);
    tabs.listed(remaining, bar(7, 2), slot);
    tabs.end_listing();
    assert_eq!(tabs.order(), vec![remaining, other]);
    assert_eq!(tabs.windows().len(), 2, "one tile per group");
    assert_eq!(tabs.writes(), writes, "no relayout and no write");
    assert!(tabs.rig.snap().window(finder).is_none());
    assert_eq!(tabs.rig.snap().diagnostics.tab_handovers, 1);
    assert_eq!(tabs.rig.snap().diagnostics.tab_departures_closed, 0);
}

#[test]
fn the_settle_retries_are_bounded_and_then_the_window_is_closed_for_real() {
    let (mut tabs, finder, other) = Tabs::finder_group();
    tabs.destroyed(finder);
    tabs.settle_delay();
    for _ in 0..=MAX_TAB_SETTLE_RETRIES {
        tabs.unsettled_listing();
        tabs.settle_delay();
    }
    let listings = tabs.refreshes();
    assert_eq!(
        listings,
        usize::from(MAX_TAB_SETTLE_RETRIES) + 1,
        "the first listing plus the bounded retries"
    );
    assert_eq!(
        tabs.order(),
        vec![other],
        "no successor ever settled: a normal close"
    );
    assert_eq!(tabs.rig.snap().diagnostics.tab_departures_closed, 1);
    // Nothing keeps asking.
    tabs.wait(Duration::from_secs(5));
    assert_eq!(tabs.refreshes(), listings);
}

#[test]
fn closing_the_last_tab_removes_the_tile_and_reflows() {
    let (mut tabs, finder, other) = Tabs::finder_group();
    let writes = tabs.writes();
    tabs.destroyed(finder);
    tabs.settle_delay();
    // The listing finds no window carrying the bar any more.
    tabs.end_listing();
    assert_eq!(tabs.order(), vec![other]);
    assert!(tabs.rig.snap().window(finder).is_none());
    assert!(
        tabs.writes() > writes,
        "the other window now takes the whole area"
    );
    assert_eq!(tabs.rig.snap().diagnostics.tab_departures_closed, 1);
}

#[test]
fn closing_a_window_that_is_not_a_tab_group_is_immediate() {
    let mut tabs = Tabs::new();
    let (solo, other) = (tabs.token(FINDER), tabs.token(OTHER));
    tabs.enumerate(solo, bar(7, 1));
    tabs.enumerate(other, TabBarRead::None);
    tabs.finish_startup();
    tabs.destroyed(solo);
    assert_eq!(
        tabs.order(),
        vec![other],
        "a one-tab window has no group to wait for"
    );
    let plain = tabs.token(OTHER);
    tabs.rig.push(WindowEventKind::Snapshot {
        snapshot: snapshot(plain, "w", 900),
        origin: SnapshotOrigin::Created,
    });
    tabs.rig.pump();
    tabs.destroyed(plain);
    assert_eq!(tabs.order(), vec![other]);
    assert_eq!(tabs.refreshes(), 0);
}

#[test]
fn a_single_window_application_keeps_its_slot_across_a_new_tab_and_a_close() {
    // Ghostty-like: one window, one display.
    let mut tabs = Tabs::new();
    let first = tabs.token(FINDER);
    tabs.enumerate(first, bar(9, 2));
    tabs.finish_startup();
    let slot = tabs.slot(first);
    let writes = tabs.writes();

    let second = tabs.token(FINDER);
    tabs.created(second, bar(9, 3), slot);
    assert_eq!(tabs.order(), vec![second], "the new tab is not an extra tile");
    assert_eq!(tabs.writes(), writes, "no jump");

    // Close the tab: the remaining window comes back in the default frame and is
    // laid out into the slot with one write.
    tabs.destroyed(second);
    tabs.settle_delay();
    let third = tabs.token(FINDER);
    tabs.listed(third, bar(9, 2), Rect::new(300.0, 300.0, 700.0, 500.0));
    tabs.destroyed(second);
    tabs.end_listing();
    assert_eq!(tabs.order(), vec![third]);
    assert_eq!(tabs.windows().len(), 1);
    assert_eq!(
        tabs.writes(),
        writes + 1,
        "the remaining window is laid out once"
    );
}

#[test]
fn different_tab_groups_of_one_application_stay_separate() {
    let mut tabs = Tabs::new();
    let (first, second) = (tabs.token(FINDER), tabs.token(FINDER));
    tabs.enumerate(first, bar(7, 2));
    tabs.enumerate(second, bar(8, 2));
    tabs.finish_startup();
    let slot_second = tabs.slot(second);
    let swapped = tabs.token(FINDER);
    tabs.created(swapped, bar(7, 2), slot_second);
    assert_eq!(
        tabs.order(),
        vec![swapped, second],
        "only group 7 changed its window"
    );
    assert_eq!(tabs.windows().len(), 2);
}

#[test]
fn windows_that_merely_share_a_frame_are_never_merged() {
    let mut tabs = Tabs::new();
    let (first, second) = (tabs.token(FINDER), tabs.token(FINDER));
    tabs.enumerate(first, TabBarRead::None);
    tabs.enumerate(second, TabBarRead::None);
    tabs.finish_startup();
    let same = tabs.slot(first);
    let third = tabs.token(FINDER);
    tabs.created(third, TabBarRead::None, same);
    let fourth = tabs.token(FINDER);
    tabs.created(fourth, TabBarRead::NotRead, same);
    assert_eq!(tabs.windows().len(), 4, "no tab bar, no continuity");
    // A window whose bar is unreadable is no continuity either.
    let fifth = tabs.token(FINDER);
    tabs.created(fifth, TabBarRead::Unreadable, same);
    assert_eq!(tabs.windows().len(), 5);
    assert_eq!(tabs.rig.snap().diagnostics.tab_handovers, 0);
}

#[test]
fn a_new_bar_next_to_a_tracked_one_is_an_ordinary_new_window() {
    let (mut tabs, _finder, _other) = Tabs::finder_group();
    let fresh = tabs.token(FINDER);
    let slot = tabs.slot(tabs.windows()[0]);
    tabs.created(fresh, bar(99, 2), slot);
    assert_eq!(tabs.windows().len(), 3);
    assert_eq!(tabs.rig.snap().diagnostics.tab_handovers, 0);
}

#[test]
fn the_replacement_keeps_workspace_floating_flag_and_focus_history() {
    let mut tabs = Tabs::new();
    let (finder, other) = (tabs.token(FINDER), tabs.token(OTHER));
    tabs.enumerate(finder, bar(7, 2));
    tabs.enumerate(other, TabBarRead::None);
    tabs.finish_startup();
    tabs.focus(other);
    tabs.focus(finder);
    tabs.rig.ingress.submit(Input::Intent(Intent::ToggleFloating)).expect("intent");
    tabs.rig.pump();
    let before = tabs.rig.snap();
    let old = before.window(finder).expect("window").clone();
    assert!(old.desired.floating);

    let next = tabs.token(FINDER);
    tabs.created(next, bar(7, 3), tabs.slot(finder));
    let after = tabs.rig.snap();
    let new = after.window(next).expect("replacement");
    assert_eq!(new.desired.workspace, old.desired.workspace);
    assert!(new.desired.floating, "the explicit flag survives");
    assert_eq!(new.desired.positioned, old.desired.positioned);
    assert_eq!(after.focus.window, Some(next));
    assert_eq!(
        after
            .workspaces
            .iter()
            .find(|ws| Some(ws.id) == new.desired.workspace)
            .and_then(|ws| ws.focused_window),
        Some(next)
    );
}

#[test]
fn the_replacement_stays_on_its_workspace_even_when_a_rule_would_pick_another() {
    let mut tabs = Tabs::new();
    tabs.rig
        .ingress
        .submit(Input::Config { revision: 2, config: config() })
        .expect("config");
    tabs.rig.pump();
    let (finder, other) = (tabs.token(FINDER), tabs.token(OTHER));
    tabs.enumerate(finder, bar(7, 2));
    tabs.enumerate(other, TabBarRead::None);
    tabs.finish_startup();
    // The user sent the Finder window to the main workspace.
    tabs.rig
        .ingress
        .submit(Input::Intent(Intent::SendWindowToWorkspace {
            window: Some(finder),
            workspace: "main".to_owned(),
        }))
        .expect("intent");
    tabs.rig.pump();
    let before = tabs.rig.snap().window(finder).expect("window").desired.workspace;
    let next = tabs.token(FINDER);
    tabs.created(next, bar(7, 3), tabs.slot(finder));
    let snap = tabs.rig.snap();
    assert_eq!(snap.window(next).expect("window").desired.workspace, before);
    let main = snap.workspace("main").expect("main");
    assert!(main.order.contains(&next));
}

#[test]
fn the_old_token_stays_dead_after_a_handover() {
    let (mut tabs, finder, _other) = Tabs::finder_group();
    let next = tabs.token(FINDER);
    tabs.created(next, bar(7, 3), tabs.slot(finder));
    // The old window stays a suspended member until its element is destroyed.
    assert!(tabs.rig.snap().window(finder).is_some_and(|w| w.suspended));
    tabs.destroyed(finder);
    let ignored = tabs.rig.snap().diagnostics.ignored_after_destroy;
    // A late read of the destroyed token cannot resurrect it as a tile.
    tabs.listed(finder, bar(7, 3), tabs.slot(next));
    let snap = tabs.rig.snap();
    assert!(snap.window(finder).is_none());
    assert_eq!(snap.diagnostics.ignored_after_destroy, ignored + 1);
    assert_eq!(tabs.windows().len(), 2);
}

#[test]
fn a_burst_of_group_events_is_one_coalesced_listing() {
    let (mut tabs, finder, _other) = Tabs::finder_group();
    for change in [
        WindowChange::Focused,
        WindowChange::Main,
        WindowChange::Focused,
    ] {
        tabs.rig.push(WindowEventKind::Window { window: finder, change });
    }
    tabs.rig.push(WindowEventKind::Application {
        application: app(FINDER),
        change: stache_macos::windows::ApplicationChange::FocusedWindowChanged,
    });
    tabs.rig.pump();
    tabs.wait(Duration::from_millis(60));
    tabs.rig.push(WindowEventKind::Window {
        window: finder,
        change: WindowChange::Main,
    });
    tabs.rig.pump();
    assert_eq!(tabs.refreshes(), 0);
    tabs.wait(Duration::from_millis(100));
    assert_eq!(tabs.refreshes(), 1, "one listing for the whole burst");
    // The listing still names the group's window.
    let slot = tabs.slot(finder);
    tabs.listed(finder, bar(7, 3), slot);
    tabs.end_listing();
    tabs.wait(Duration::from_secs(5));
    assert_eq!(tabs.refreshes(), 1, "nothing is rescheduled by a clean listing");
}

#[test]
fn applications_without_a_tab_bar_are_never_listed_for_focus_changes() {
    let (mut tabs, _finder, other) = Tabs::finder_group();
    tabs.rig.push(WindowEventKind::Window {
        window: other,
        change: WindowChange::Focused,
    });
    tabs.rig.push(WindowEventKind::Application {
        application: app(OTHER),
        change: stache_macos::windows::ApplicationChange::MainWindowChanged,
    });
    tabs.rig.pump();
    tabs.wait(Duration::from_secs(1));
    let others = tabs
        .rig
        .calls()
        .iter()
        .filter(|call| matches!(call, FakeBackendCall::Refresh(a) if *a == app(OTHER)))
        .count();
    assert_eq!(others, 0);
}

// ---------------------------------------------------------------- suspended

impl Tabs {
    pub(super) fn suspend(&mut self, token: WindowInstance) {
        self.rig.push(WindowEventKind::Window {
            window: token,
            change: WindowChange::Suspended,
        });
        self.rig.pump();
    }

    /// A one-window application (no tab bar, like a single Ghostty window) next
    /// to another application's window.
    pub(super) fn single() -> (Self, WindowInstance, WindowInstance) {
        let mut tabs = Self::new();
        let single = tabs.token(FINDER);
        let other = tabs.token(OTHER);
        tabs.enumerate(single, TabBarRead::None);
        tabs.enumerate(other, TabBarRead::None);
        tabs.finish_startup();
        assert_eq!(tabs.windows().len(), 2);
        assert!(tabs.writes() >= 1, "the startup layout was written");
        (tabs, single, other)
    }

    pub(super) fn suspended_of(&self, token: WindowInstance) -> bool {
        self.rig.snap().window(token).is_some_and(|window| window.suspended)
    }

    /// One settled listing that names exactly `listed` (each on its slot).
    pub(super) fn settled_listing(&mut self, listed: &[(WindowInstance, TabBarRead)]) {
        self.settle_delay();
        for (token, bar) in listed {
            let frame = self.slot(*token);
            self.listed(*token, *bar, frame);
        }
        self.end_listing();
    }
}

#[test]
fn a_new_tab_on_a_single_window_takes_its_slot_without_a_reflow_or_a_write() {
    let (mut tabs, single, other) = Tabs::single();
    tabs.focus(single);
    let writes = tabs.writes();
    let slot = tabs.slot(single);
    let order = tabs.order();

    // Command-T: the owner reports the old window unlisted but alive, then the
    // new tabbed window.
    tabs.suspend(single);
    assert_eq!(tabs.order(), order, "the slot is held while the listing settles");
    let fresh = tabs.token(FINDER);
    tabs.created(fresh, bar(7, 2), slot);
    assert_eq!(tabs.order(), vec![fresh, other], "no extra tile, same position");
    assert_eq!(tabs.windows(), vec![fresh, other]);
    assert!(tabs.suspended_of(single), "alive and tracked, never a tile");
    assert_eq!(tabs.writes(), writes, "no write to a window already on its slot");
    let snap = tabs.rig.snap();
    assert_eq!(snap.focus.window, Some(fresh));
    assert_eq!(snap.diagnostics.tab_single_handovers, 1);
    assert_eq!(snap.diagnostics.tab_suspensions, 1);

    // The settled listing finds nothing else to decide.
    tabs.settled_listing(&[(fresh, bar(7, 2))]);
    assert_eq!(tabs.order(), vec![fresh, other]);
    assert_eq!(tabs.writes(), writes);
    assert_eq!(tabs.rig.snap().diagnostics.tab_slots_released, 0);
    assert!(tabs.suspended_of(single));
}

#[test]
fn a_new_tab_reported_before_the_suspension_is_still_one_slot() {
    let (mut tabs, single, other) = Tabs::single();
    let slot = tabs.slot(single);
    let fresh = tabs.token(FINDER);
    // The new window arrives first (an ordinary window for a moment), then the
    // owner reports the old one unlisted.
    tabs.created(fresh, bar(7, 2), slot);
    assert_eq!(tabs.windows().len(), 3);
    tabs.suspend(single);
    assert_eq!(tabs.order(), vec![fresh, other], "the arrival took the slot");
    assert_eq!(tabs.windows(), vec![fresh, other]);
    assert_eq!(tabs.rig.snap().diagnostics.tab_single_handovers, 1);
}

#[test]
fn closing_the_new_tab_gives_the_slot_back_to_the_same_window_three_times() {
    let (mut tabs, single, other) = Tabs::single();
    tabs.focus(single);
    let writes = tabs.writes();
    let slot = tabs.slot(single);
    for round in 1..=3_u64 {
        // Command-T.
        tabs.suspend(single);
        let fresh = tabs.token(FINDER);
        tabs.created(fresh, bar(7, 2), slot);
        assert_eq!(tabs.order(), vec![fresh, other]);
        tabs.settled_listing(&[(fresh, bar(7, 2))]);

        // Command-W: the new window is destroyed and the same window is listed.
        tabs.destroyed(fresh);
        assert_eq!(
            tabs.order(),
            vec![fresh, other],
            "held until the listing settles"
        );
        tabs.settle_delay();
        tabs.listed(single, TabBarRead::None, slot);
        tabs.end_listing();
        assert_eq!(
            tabs.order(),
            vec![single, other],
            "round {round}: same window, same slot"
        );
        assert_eq!(tabs.windows(), vec![single, other]);
        assert!(!tabs.suspended_of(single));
        assert!(tabs.rig.snap().window(fresh).is_none());
        assert_eq!(tabs.rig.snap().focus.window, Some(single));
        assert_eq!(tabs.writes(), writes, "round {round}: no reflow, no write");
    }
    let diagnostics = tabs.rig.snap().diagnostics.clone();
    assert_eq!((diagnostics.tab_suspensions, diagnostics.tab_returns), (3, 3));
    assert_eq!(diagnostics.tab_single_handovers, 3);
}

#[test]
fn switching_between_three_tabs_suspends_and_returns_windows_by_identity() {
    let (mut tabs, a, other) = Tabs::finder_group();
    tabs.focus(a);
    let writes = tabs.writes();
    let slot = tabs.slot(a);

    // Tab B is selected: its window is listed, A leaves the list alive.
    let b = tabs.token(FINDER);
    tabs.listed(b, bar(7, 3), slot);
    tabs.suspend(a);
    tabs.end_listing();
    assert_eq!(tabs.order(), vec![b, other]);
    assert!(tabs.suspended_of(a));

    // Back to A: it is listed again (the same token), B leaves alive.
    tabs.settle_delay();
    tabs.listed(a, bar(7, 3), slot);
    tabs.suspend(b);
    tabs.end_listing();
    assert_eq!(tabs.order(), vec![a, other]);
    assert!(!tabs.suspended_of(a) && tabs.suspended_of(b));
    assert_eq!(tabs.windows(), vec![a, other], "no extra tiles");

    // And to a third tab C, then back to B.
    let c = tabs.token(FINDER);
    tabs.listed(c, bar(7, 3), slot);
    tabs.suspend(a);
    tabs.end_listing();
    assert_eq!(tabs.order(), vec![c, other]);
    tabs.listed(b, bar(7, 3), slot);
    tabs.suspend(c);
    tabs.end_listing();
    assert_eq!(tabs.order(), vec![b, other]);
    assert_eq!(tabs.windows(), vec![b, other]);
    assert_eq!(
        tabs.tracked().len(),
        4,
        "A, B, C and the other application's window"
    );
    assert_eq!(tabs.writes(), writes, "nothing was written");
    assert_eq!(tabs.rig.snap().focus.window, Some(b));
}

#[test]
fn closing_an_inactive_tab_changes_no_layout() {
    let (mut tabs, a, other) = Tabs::finder_group();
    let slot = tabs.slot(a);
    let b = tabs.token(FINDER);
    tabs.listed(b, bar(7, 3), slot);
    tabs.suspend(a);
    tabs.end_listing();
    let writes = tabs.writes();
    let refreshes = tabs.refreshes();

    // The inactive tab A is closed.
    tabs.destroyed(a);
    assert!(tabs.rig.snap().window(a).is_none());
    assert_eq!(tabs.order(), vec![b, other]);
    assert!(tabs.rig.snap().window(b).is_some_and(|w| w.departing.is_none()));
    tabs.wait(Duration::from_secs(1));
    assert_eq!(tabs.writes(), writes);
    assert_eq!(tabs.refreshes(), refreshes);
    assert_eq!(tabs.rig.snap().diagnostics.tab_departures_held, 0);
}

#[test]
fn closing_the_active_tab_hands_the_slot_to_the_listed_suspended_member() {
    let (mut tabs, a, other) = Tabs::finder_group();
    let slot = tabs.slot(a);
    let b = tabs.token(FINDER);
    tabs.listed(b, bar(7, 3), slot);
    tabs.suspend(a);
    tabs.end_listing();
    let writes = tabs.writes();

    // B (active) is closed: A, suspended, is listed next.
    tabs.destroyed(b);
    assert_eq!(tabs.order(), vec![b, other], "held, not reflowed");
    tabs.settle_delay();
    tabs.listed(a, bar(7, 2), slot);
    tabs.end_listing();
    assert_eq!(tabs.order(), vec![a, other]);
    assert!(tabs.rig.snap().window(b).is_none());
    assert_eq!(tabs.windows(), vec![a, other]);
    assert_eq!(tabs.writes(), writes);
}

#[test]
fn closing_the_last_window_reflows() {
    let (mut tabs, single, other) = Tabs::single();
    let writes = tabs.writes();
    tabs.destroyed(single);
    assert_eq!(tabs.order(), vec![other], "an ordinary close is not held");
    assert_eq!(tabs.windows(), vec![other]);
    assert!(tabs.writes() > writes, "the remaining window is laid out");
}

#[test]
fn two_suspensions_and_one_tabbed_arrival_are_not_matched_and_reflow_after_the_settle() {
    let mut tabs = Tabs::new();
    let (first, second, other) = (tabs.token(FINDER), tabs.token(FINDER), tabs.token(OTHER));
    tabs.enumerate(first, TabBarRead::None);
    tabs.enumerate(second, TabBarRead::None);
    tabs.enumerate(other, TabBarRead::None);
    tabs.finish_startup();
    assert_eq!(tabs.windows().len(), 3);
    let slot = tabs.slot(first);

    tabs.suspend(first);
    tabs.suspend(second);
    let fresh = tabs.token(FINDER);
    tabs.created(fresh, bar(7, 2), slot);
    assert_eq!(
        tabs.rig.snap().diagnostics.tab_single_handovers,
        0,
        "ambiguous: no handover"
    );
    assert_eq!(
        tabs.order().len(),
        4,
        "both slots are still held, plus the new window"
    );

    // After the settle the two unmatched slots are released.
    tabs.settled_listing(&[(fresh, bar(7, 2))]);
    let (mut tiles, mut expected) = (tabs.windows(), vec![fresh, other]);
    tiles.sort();
    expected.sort();
    assert_eq!(tiles, expected);
    assert_eq!(tabs.order().len(), 2, "the layout reflowed to two tiles");
    assert!(tabs.order().contains(&fresh) && tabs.order().contains(&other));
    assert!(tabs.suspended_of(first) && tabs.suspended_of(second));
    assert_eq!(tabs.rig.snap().diagnostics.tab_slots_released, 2);
}

#[test]
fn a_suspension_nobody_replaces_releases_its_tile_after_the_settle() {
    let (mut tabs, single, other) = Tabs::single();
    tabs.suspend(single);
    assert_eq!(tabs.order().len(), 2, "held while the listing settles");
    tabs.settled_listing(&[]);
    assert_eq!(tabs.order(), vec![other]);
    assert!(tabs.suspended_of(single));
    assert_eq!(tabs.rig.snap().diagnostics.tab_slots_released, 1);

    // It is listed again later: an ordinary tile again.
    tabs.settle_delay();
    tabs.listed(single, TabBarRead::None, Rect::new(0.0, 0.0, 800.0, 600.0));
    tabs.end_listing();
    assert!(!tabs.suspended_of(single));
    assert_eq!(tabs.windows().len(), 2);
    assert!(tabs.order().contains(&single));
}

#[test]
fn suspended_windows_are_not_listed_as_windows_in_the_status() {
    let (mut tabs, single, _other) = Tabs::single();
    let slot = tabs.slot(single);
    tabs.suspend(single);
    let fresh = tabs.token(FINDER);
    tabs.created(fresh, bar(7, 2), slot);
    let status = crate::ipc::status(&tabs.rig.snap());
    assert_eq!(status.suspended_windows, 1);
    assert_eq!(status.windows, 2);
    assert_eq!(status.tracked_windows.len(), 2);
}

#[test]
fn a_suspended_window_gets_no_snapshot_reads_and_a_stale_answer_does_not_destroy_it() {
    let (mut tabs, single, other) = Tabs::single();
    let slot = tabs.slot(single);
    tabs.suspend(single);
    let fresh = tabs.token(FINDER);
    tabs.created(fresh, bar(7, 2), slot);
    tabs.focus(fresh);
    tabs.rig.push(WindowEventKind::Window {
        window: single,
        change: WindowChange::Moved,
    });
    tabs.rig.pump();
    tabs.rig.push(WindowEventKind::Window {
        window: single,
        change: WindowChange::Focused,
    });
    tabs.rig.pump();
    assert!(tabs.suspended_of(single));
    assert_eq!(
        tabs.rig.snap().focus.window,
        Some(fresh),
        "a suspended window takes no focus"
    );
    assert_eq!(tabs.order(), vec![fresh, other]);
}

#[test]
fn a_return_announced_before_the_close_waits_for_the_settle_instead_of_adding_a_tile() {
    let (mut tabs, single, other) = Tabs::single();
    let slot = tabs.slot(single);
    tabs.suspend(single);
    let fresh = tabs.token(FINDER);
    tabs.created(fresh, bar(7, 2), slot);
    tabs.settled_listing(&[(fresh, bar(7, 2))]);
    let writes = tabs.writes();

    // The old window is announced again while the new one has not reported its
    // close yet: no second tile.
    tabs.created(single, TabBarRead::None, slot);
    assert_eq!(tabs.order(), vec![fresh, other]);
    assert!(tabs.suspended_of(single));
    tabs.destroyed(fresh);
    tabs.settle_delay();
    tabs.listed(single, TabBarRead::None, slot);
    tabs.end_listing();
    assert_eq!(tabs.order(), vec![single, other]);
    assert_eq!(tabs.writes(), writes);
}

#[test]
fn a_window_back_from_native_fullscreen_is_laid_out_again() {
    // Native run 4: Ghostty enters native fullscreen, leaves `AXWindows` alive
    // (suspended), no window takes its place, the slot is released; after the
    // exit it is listed again off its tile and must be a tile again.
    let (mut tabs, single, other) = Tabs::single();
    tabs.suspend(single);
    tabs.settled_listing(&[(other, TabBarRead::None)]);
    assert_eq!(tabs.order(), vec![other], "the released slot reflows");
    assert_eq!(tabs.rig.snap().diagnostics.tab_slots_released, 1);
    let writes = tabs.writes();

    let off = Rect::new(162.0, 137.0, 700.0, 500.0);
    tabs.created(single, TabBarRead::None, off);
    assert!(!tabs.suspended_of(single));
    assert!(tabs.order().contains(&single), "a tile again");
    assert!(tabs.writes() > writes, "laid out again");
    assert_ne!(tabs.slot(single), off);
}
