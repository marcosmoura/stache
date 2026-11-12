//! TI-10-01 (10a): the `tilingState` projection, its publish gate and window
//! key resolution over hand-built snapshots.

use stache_macos::DisplayId;
use stache_macos::windows::{AdapterId, RuntimeEpoch, TabInfo, WindowInstance};
use stache_sdk::PublishedState;

use super::*;
use crate::config::LayoutType;
use crate::geometry::Rect;
use crate::state::{
    AppRecord, DesiredWindow, FocusState, ObservedWindow, ScreenState, WorkspaceId, WorkspaceState,
};
use crate::test_support::{app, info};

fn token(adapter: AdapterId, epoch: u64, app_n: u64, serial: u64) -> WindowInstance {
    WindowInstance::for_testing(adapter, RuntimeEpoch::for_testing(epoch), app(app_n), serial)
}

fn workspace(id: u32, name: &str, screen: u32, order: Vec<WindowInstance>) -> WorkspaceState {
    WorkspaceState {
        id: WorkspaceId(id),
        name: name.to_owned(),
        screen: DisplayId::new(screen),
        configured_screen: None,
        configured_layout: None,
        layout: LayoutType::Dwindle,
        layout_overridden: false,
        displaced: false,
        ratios: crate::minimum_size::RatioState::default(),
        visible: false,
        focused: false,
        order,
        focused_window: None,
    }
}

fn window(token: WindowInstance, workspace: Option<u32>, title: &str) -> WindowState {
    WindowState {
        token,
        application: token.application(),
        cg_window_id: None,
        desired: DesiredWindow {
            workspace: workspace.map(WorkspaceId),
            floating: false,
            positioned: false,
        },
        observed: ObservedWindow {
            title: title.to_owned(),
            role: None,
            subrole: None,
            frame: Some(Rect::new(0.0, 0.0, 100.0, 100.0)),
            fullscreen: false,
            minimized: false,
            hidden: false,
            tabs: TabInfo::None,
            observed: true,
        },
        adopted_seq: None,
        inactive_tab: false,
        tab_bar: None,
        departing: None,
        suspended: false,
        slot_held: false,
        tab_group: None,
        arrival: false,
        returning: false,
        fullscreen_rechecks: 0,
        relists_spent: false,
    }
}

fn record(n: u64, name: &str, hidden: bool) -> AppRecord {
    AppRecord {
        info: info(n, "com.example", name),
        hidden,
        skipped: false,
        tracked: true,
    }
}

fn screen(id: u32, name: &str) -> ScreenState {
    ScreenState {
        id: DisplayId::new(id),
        name: Some(name.to_owned()),
        is_primary: id == 1,
        refresh_hz: None,
        presence: 1,
    }
}

struct Fixture {
    snapshot: TilingSnapshot,
    a: WindowInstance,
    b: WindowInstance,
    c: WindowInstance,
}

/// Two displays, three workspaces, windows deliberately out of token order.
fn fixture() -> Fixture {
    let adapter = AdapterId::for_testing();
    let a = token(adapter, 3, 1, 1);
    let b = token(adapter, 3, 2, 2);
    let c = token(adapter, 3, 1, 3);
    let mut snapshot = TilingSnapshot::initial();
    snapshot.config_revision = 4;
    snapshot.screens = vec![screen(1, "Built-in"), screen(2, "Studio Display")];
    let mut coding = workspace(1, "coding", 1, vec![c, a]);
    coding.visible = true;
    coding.focused = true;
    let mut browser = workspace(2, "browser", 2, vec![b]);
    browser.visible = true;
    snapshot.workspaces = vec![coding, browser, workspace(3, "misc", 1, Vec::new())];
    snapshot.windows = vec![
        window(a, Some(1), "Alpha"),
        window(b, Some(2), "Bravo"),
        window(c, Some(1), "Charlie"),
    ];
    snapshot.apps = vec![record(1, "Zed", false), record(2, "Safari", false)];
    snapshot.focus = FocusState {
        window: Some(c),
        workspace: Some(WorkspaceId(1)),
        screen: Some(DisplayId::new(1)),
    };
    Fixture { snapshot, a, b, c }
}

#[test]
fn the_schema_is_tiling_state_v1() {
    assert_eq!(TilingPublishedState::SCHEMA.name, "tilingState");
    assert_eq!(TilingPublishedState::SCHEMA.version, 1);
}

#[test]
fn every_workspace_projects_in_order_with_screen_visibility_and_focus() {
    let state = project(&fixture().snapshot);
    let names: Vec<_> = state.workspaces.iter().map(|ws| ws.name.as_str()).collect();
    assert_eq!(names, ["coding", "browser", "misc"]);
    assert_eq!(state.workspaces[1], PublishedWorkspace {
        name: "browser".to_owned(),
        screen: "2".to_owned(),
        screen_name: Some("Studio Display".to_owned()),
        visible: true,
        focused: false,
        order: 1,
        inactive: false,
    });
    assert!(state.workspaces[0].focused && state.workspaces[0].visible);
    assert!(!state.workspaces[2].visible);
    assert_eq!(state.workspaces[2].order, 2);
}

#[test]
fn windows_follow_workspace_then_layout_order_and_focus_uses_keys() {
    let Fixture { snapshot, a, b, c } = fixture();
    let state = project(&snapshot);
    let keys: Vec<_> = state.windows.iter().map(|w| w.key.clone()).collect();
    assert_eq!(keys, [window_key(c), window_key(a), window_key(b)]);
    assert_eq!(state.windows[0].app_name, "Zed");
    assert_eq!(state.windows[0].workspace.as_deref(), Some("coding"));
    assert_eq!(state.windows[2].app_name, "Safari");
    assert_eq!(state.focus, PublishedFocus {
        workspace: Some("coding".to_owned()),
        window: Some(window_key(c)),
    });
}

#[test]
fn minimized_and_hidden_are_flagged_including_a_hidden_application() {
    let Fixture { mut snapshot, a, b, .. } = fixture();
    snapshot.windows[0].observed.minimized = true;
    snapshot.apps[1].hidden = true;
    let state = project(&snapshot);
    let find = |key: &str| state.windows.iter().find(|w| w.key == key).expect("window");
    assert!(find(&window_key(a)).minimized);
    assert!(!find(&window_key(a)).hidden);
    assert!(find(&window_key(b)).hidden);
}

#[test]
fn only_the_active_member_of_a_native_tab_group_is_tab_active() {
    let Fixture { mut snapshot, a, c, .. } = fixture();
    snapshot.windows[0].inactive_tab = true; // a
    snapshot.windows[2].suspended = true; // c, slot already taken over
    let state = project(&snapshot);
    let find = |key: &str| state.windows.iter().find(|w| w.key == key).expect("window");
    assert!(!find(&window_key(a)).tab_active);
    assert!(!find(&window_key(c)).tab_active);
    // A suspended window still holding its slot is the active tile.
    snapshot.windows[2].slot_held = true;
    assert!(
        project(&snapshot)
            .windows
            .iter()
            .any(|w| w.key == window_key(c) && w.tab_active)
    );
}

#[test]
fn a_departing_window_is_not_published_or_resolved() {
    let Fixture { mut snapshot, c, .. } = fixture();
    snapshot.windows[2].departing = Some(9);
    let state = project(&snapshot);
    assert!(state.windows.iter().all(|w| w.key != window_key(c)));
    assert_eq!(state.focus.window, None);
    assert_eq!(resolve_window_key(&snapshot, &window_key(c)), None);
}

#[test]
fn unicode_and_long_titles_pass_unmodified() {
    let Fixture { mut snapshot, .. } = fixture();
    let long = "Ω🙂 ‎WhatsApp — ".repeat(200);
    snapshot.windows[0].observed.title.clone_from(&long);
    let state = project(&snapshot);
    assert!(state.windows.iter().any(|w| w.title == long));
}

#[test]
fn no_workspaces_projects_empty_state() {
    let mut snapshot = TilingSnapshot::initial();
    snapshot.config_revision = 1;
    assert_eq!(project(&snapshot), TilingPublishedState::default());
}

#[test]
fn every_number_crosses_as_a_string_or_small_integer() {
    let state = project(&fixture().snapshot);
    let json = serde_json::to_value(&state).expect("encode");
    assert!(json["windows"][0]["key"].is_string());
    assert!(json["windows"][0]["tabActive"].is_boolean());
    assert!(json["workspaces"][0]["screen"].is_string());
    assert!(json["workspaces"][0]["screenName"].is_string());
    assert!(json["focus"]["window"].is_string());
    let back: TilingPublishedState = serde_json::from_value(json).expect("decode");
    assert_eq!(back, state);
}

#[test]
fn the_gate_publishes_only_projection_changes_after_a_configuration() {
    let Fixture { mut snapshot, .. } = fixture();
    let mut gate = PublishGate::default();
    let mut publications = 0;
    let mut step = |gate: &mut PublishGate, snapshot: &TilingSnapshot| {
        if let Some((revision, state)) = gate.next(snapshot) {
            assert_eq!(revision, snapshot.config_revision);
            gate.committed(state);
            publications += 1;
        }
        publications
    };

    // No configuration applied yet: nothing.
    let mut unconfigured = snapshot.clone();
    unconfigured.config_revision = 0;
    assert_eq!(step(&mut gate, &unconfigured), 0);
    assert_eq!(step(&mut gate, &snapshot), 1);
    // Admission, frame, sequence and drag churn is invisible.
    for n in 0..10_u32 {
        snapshot.sequence += 1;
        snapshot.plan_sequence += 1;
        snapshot.admission.geometry_coalesced += 1;
        snapshot.windows[0].observed.frame = Some(Rect::new(f64::from(n), 0.0, 10.0, 10.0));
        assert_eq!(step(&mut gate, &snapshot), 1);
    }
    snapshot.windows[0].observed.title = "renamed".to_owned();
    assert_eq!(step(&mut gate, &snapshot), 2);
    assert_eq!(step(&mut gate, &snapshot), 2);
}

#[test]
fn keys_round_trip_and_reject_unknown_malformed_and_stale() {
    let Fixture { mut snapshot, a, b, .. } = fixture();
    assert_eq!(resolve_window_key(&snapshot, &window_key(a)), Some(a));
    assert_eq!(resolve_window_key(&snapshot, &window_key(b)), Some(b));
    for malformed in [
        "",
        "x",
        "-1",
        "+1",
        " 1",
        "01",
        "1.0",
        "999999999999999999999999999999999999999999",
    ] {
        assert_eq!(resolve_window_key(&snapshot, malformed), None, "{malformed:?}");
    }
    // Unknown serial in the current epoch.
    let unknown = token(a.adapter(), 3, 1, 77);
    assert_eq!(resolve_window_key(&snapshot, &window_key(unknown)), None);
    // A window from a previous generation (older epoch, same serial).
    let previous = token(a.adapter(), 2, 1, a.serial());
    assert_ne!(window_key(previous), window_key(a));
    assert_eq!(resolve_window_key(&snapshot, &window_key(previous)), None);
    // Destroyed: the key no longer resolves.
    let key = window_key(a);
    snapshot.windows.retain(|w| w.token != a);
    assert_eq!(resolve_window_key(&snapshot, &key), None);
}

#[test]
fn an_ambiguous_key_resolves_to_nothing() {
    let Fixture { mut snapshot, a, .. } = fixture();
    // Another adapter minting the same epoch and serial is not guessed.
    let twin = token(AdapterId::for_testing(), 3, 9, a.serial());
    snapshot.windows.push(window(twin, Some(1), "Twin"));
    assert_eq!(resolve_window_key(&snapshot, &window_key(a)), None);
}

#[test]
fn inactive_and_the_refused_focus_are_copied_from_the_snapshot() {
    let Fixture { mut snapshot, .. } = fixture();
    snapshot.unreachable = vec![WorkspaceId(3)];
    snapshot.refused_focus = Some(crate::state::RefusedFocus {
        workspace: WorkspaceId(3),
        serial: 7,
    });
    let state = project(&snapshot);
    let flags: Vec<_> = state.workspaces.iter().map(|ws| ws.inactive).collect();
    assert_eq!(flags, [false, false, true]);
    assert_eq!(
        state.refused_focus,
        Some(PublishedRefusedFocus {
            workspace: "misc".to_owned(),
            serial: 7,
        })
    );
    let json = serde_json::to_value(&state).expect("encode");
    assert_eq!(json["workspaces"][2]["inactive"], true);
    assert_eq!(json["refusedFocus"]["workspace"], "misc");
    assert_eq!(json["refusedFocus"]["serial"], 7);
    // A workspace that no longer exists publishes no refusal.
    snapshot.workspaces.pop();
    assert_eq!(project(&snapshot).refused_focus, None);
}

#[test]
fn a_payload_without_the_new_fields_decodes_with_their_defaults() {
    let mut json = serde_json::to_value(project(&fixture().snapshot)).expect("encode");
    json.as_object_mut().expect("object").remove("refusedFocus");
    for workspace in json["workspaces"].as_array_mut().expect("array") {
        workspace.as_object_mut().expect("object").remove("inactive");
    }
    let back: TilingPublishedState = serde_json::from_value(json).expect("decode");
    assert_eq!(back.refused_focus, None);
    assert!(back.workspaces.iter().all(|ws| !ws.inactive));
}

#[test]
fn each_refusal_publishes_once() {
    let Fixture { mut snapshot, .. } = fixture();
    snapshot.unreachable = vec![WorkspaceId(3)];
    let mut gate = PublishGate::default();
    let mut publications = 0;
    let mut step = |gate: &mut PublishGate, snapshot: &TilingSnapshot| {
        if let Some((_, state)) = gate.next(snapshot) {
            gate.committed(state);
            publications += 1;
        }
        publications
    };
    assert_eq!(step(&mut gate, &snapshot), 1);
    for serial in 1..=3 {
        snapshot.refused_focus = Some(crate::state::RefusedFocus {
            workspace: WorkspaceId(3),
            serial,
        });
        snapshot.sequence += 1;
        assert_eq!(step(&mut gate, &snapshot), 1 + serial);
        assert_eq!(step(&mut gate, &snapshot), 1 + serial, "the same refusal again");
    }
    snapshot.unreachable.clear();
    assert_eq!(step(&mut gate, &snapshot), 5, "the flag flips");
}
