//! Pure tests of the Tiling projection's tracker, the strip filters and the
//! click request's strict wire form.

use std::sync::Arc;

use stache_sdk::{PublishedState, RuntimeEnd, SchemaIdentity, StateUnavailable};
use stache_tiling::{PublishedFocus, PublishedWindow, PublishedWorkspace};

use super::*;

const SERVING: HostFacts = HostFacts {
    running: true,
    enabled: true,
    paused: false,
    expects_start: false,
    healthy: true,
};

const WAITING: HostFacts = HostFacts {
    running: false,
    enabled: true,
    paused: false,
    expects_start: true,
    healthy: false,
};

fn workspace(name: &str, focused: bool, order: u32) -> PublishedWorkspace {
    PublishedWorkspace {
        name: name.to_owned(),
        screen: "1".to_owned(),
        screen_name: None,
        visible: focused,
        focused,
        order,
    }
}

fn window(key: &str, app: &str, title: &str, workspace: &str) -> PublishedWindow {
    PublishedWindow {
        key: key.to_owned(),
        app_name: app.to_owned(),
        title: title.to_owned(),
        workspace: Some(workspace.to_owned()),
        minimized: false,
        hidden: false,
        tab_active: true,
    }
}

/// `coding` focused with one visible editor, a minimized and a hidden window
/// and an inactive tab; `browser` holds the focused window.
fn state() -> TilingPublishedState {
    TilingPublishedState {
        workspaces: vec![
            workspace("terminal", false, 0),
            workspace("coding", true, 1),
            workspace("browser", false, 2),
        ],
        windows: vec![
            window("10", "Zed", "main.rs — stache", "coding"),
            PublishedWindow {
                minimized: true,
                ..window("11", "Notes", "minimized", "coding")
            },
            PublishedWindow {
                hidden: true,
                ..window("12", "Mail", "hidden", "coding")
            },
            PublishedWindow {
                tab_active: false,
                ..window("13", "Finder", "inactive tab", "coding")
            },
            window("14", "Finder", "active tab", "coding"),
            window("20", "Safari", "Docs", "browser"),
            PublishedWindow {
                workspace: None,
                ..window("30", "Loose", "no workspace", "coding")
            },
        ],
        focus: PublishedFocus {
            workspace: Some("coding".to_owned()),
            window: Some("20".to_owned()),
        },
    }
}

fn envelope_with(
    generation: u64,
    sequence: u64,
    version: u32,
    payload: serde_json::Value,
) -> StateReading {
    StateReading::Available(StateEnvelope {
        provider: TILING,
        generation,
        revision: 1,
        sequence,
        schema: SchemaIdentity {
            provider: TILING,
            name: TilingPublishedState::SCHEMA.name,
            version,
        },
        payload: Arc::new(payload),
    })
}

fn available(generation: u64, sequence: u64, state: &TilingPublishedState) -> StateReading {
    envelope_with(
        generation,
        sequence,
        TilingPublishedState::SCHEMA.version,
        serde_json::to_value(state).expect("encode"),
    )
}

fn tombstone(reason: StateUnavailableReason, generation: Option<u64>) -> StateReading {
    StateReading::Unavailable(StateUnavailable { reason, generation })
}

fn strip(view: &TilingView) -> &TilingStripView { view.strip.as_ref().expect("resolved") }

fn kind(view: &TilingView) -> &'static str {
    match strip(view) {
        TilingStripView::Absent => "absent",
        TilingStripView::Loading => "loading",
        TilingStripView::Available { .. } => "available",
    }
}

fn generation_and_sequence(view: &TilingView) -> (String, String) {
    match strip(view) {
        TilingStripView::Available { generation, sequence, .. } => {
            (generation.clone(), sequence.clone())
        }
        other => panic!("expected available, got {other:?}"),
    }
}

#[test]
fn it_is_unresolved_until_the_first_observation_resolves() {
    let mut tracker = Tracker::default();
    assert_eq!(tracker.view(), TilingView { strip: None, version: 0 });
    let not_started = tombstone(StateUnavailableReason::NotStarted, None);
    assert_eq!(tracker.observe(&not_started, Some(WAITING)).strip, None);
    let starting = tombstone(StateUnavailableReason::Starting, Some(1));
    assert_eq!(tracker.observe(&starting, Some(WAITING)).strip, None);
    assert_eq!(tracker.view().version, 0);

    // Running, not published yet: loading.
    let unpublished = tombstone(StateUnavailableReason::NotPublished, Some(1));
    let view = tracker.observe(&unpublished, Some(SERVING));
    assert_eq!(kind(&view), "loading");
    assert_eq!(view.version, 1);

    let view = tracker.observe(&available(1, 1, &state()), Some(SERVING));
    assert_eq!(kind(&view), "available");
    assert_eq!(view.version, 2);

    // Once resolved it never returns to null, even while a restart waits.
    let view = tracker.observe(
        &tombstone(StateUnavailableReason::Starting, Some(2)),
        Some(WAITING),
    );
    assert_eq!(kind(&view), "absent");
    assert_eq!(view.version, 3);
}

#[test]
fn every_host_fact_that_is_not_serving_is_absent() {
    let reading = available(1, 1, &state());
    assert_eq!(
        kind(&Tracker::default().observe(&reading, Some(SERVING))),
        "available"
    );
    for host in [
        None,
        Some(HostFacts { running: false, ..SERVING }),
        Some(HostFacts { enabled: false, ..SERVING }),
        Some(HostFacts { paused: true, ..SERVING }),
        Some(HostFacts { healthy: false, ..SERVING }),
    ] {
        assert_eq!(
            kind(&Tracker::default().observe(&reading, host)),
            "absent",
            "{host:?}"
        );
    }
    // Loading needs a serving host too: a paused or unhealthy pending
    // generation is absent.
    let pending = tombstone(StateUnavailableReason::NotPublished, Some(1));
    for host in [
        Some(HostFacts { paused: true, ..SERVING }),
        Some(HostFacts { healthy: false, ..SERVING }),
        None,
    ] {
        assert_eq!(
            kind(&Tracker::default().observe(&pending, host)),
            "absent",
            "{host:?}"
        );
    }
    assert_eq!(
        kind(&Tracker::default().observe(&StateReading::Absent, None)),
        "absent"
    );
}

#[test]
fn every_end_tombstone_is_absent() {
    for end in [
        RuntimeEnd::Stopped,
        RuntimeEnd::Disabled,
        RuntimeEnd::Paused,
        RuntimeEnd::Failed,
        RuntimeEnd::PermissionBlocked,
        RuntimeEnd::DependencyUnavailable,
    ] {
        let mut tracker = Tracker::default();
        assert_eq!(
            kind(&tracker.observe(&available(1, 1, &state()), Some(SERVING))),
            "available"
        );
        let view = tracker.observe(
            &tombstone(StateUnavailableReason::Ended(end), Some(1)),
            Some(SERVING),
        );
        assert_eq!(kind(&view), "absent", "{end:?}");
    }
}

#[test]
fn a_schema_version_mismatch_or_malformed_payload_is_absent() {
    let good = serde_json::to_value(state()).expect("encode");
    let wrong_version = envelope_with(1, 1, 2, good);
    assert_eq!(
        kind(&Tracker::default().observe(&wrong_version, Some(SERVING))),
        "absent"
    );
    let StateReading::Available(mut foreign) = available(1, 1, &state()) else {
        unreachable!()
    };
    foreign.schema.name = "keepAwakeStatus";
    assert_eq!(
        kind(&Tracker::default().observe(&StateReading::Available(foreign), Some(SERVING))),
        "absent"
    );
    for payload in [
        serde_json::json!({ "workspaces": "nope" }),
        serde_json::json!(true),
        serde_json::json!({ "workspaces": [], "windows": [], "focus": { "window": 7 } }),
    ] {
        let malformed = envelope_with(1, 1, 1, payload);
        assert_eq!(
            kind(&Tracker::default().observe(&malformed, Some(SERVING))),
            "absent"
        );
    }
    // A later good write of the same generation recovers.
    let mut tracker = Tracker::default();
    let malformed = envelope_with(1, 1, 1, serde_json::json!(true));
    assert_eq!(kind(&tracker.observe(&malformed, Some(SERVING))), "absent");
    assert_eq!(
        kind(&tracker.observe(&available(1, 2, &state()), Some(SERVING))),
        "available"
    );
}

#[test]
fn late_old_generation_writes_and_tombstones_are_ignored() {
    let mut tracker = Tracker::default();
    let view = tracker.observe(&available(5, 3, &state()), Some(SERVING));
    assert_eq!(generation_and_sequence(&view), ("5".to_owned(), "3".to_owned()));
    let version = view.version;

    let late_end = tombstone(StateUnavailableReason::Ended(RuntimeEnd::Paused), Some(4));
    assert_eq!(kind(&tracker.observe(&late_end, Some(SERVING))), "available");
    let mut other = state();
    other.focus.workspace = Some("terminal".to_owned());
    let late_write = available(4, 9, &other);
    let view = tracker.observe(&late_write, Some(SERVING));
    assert_eq!(generation_and_sequence(&view), ("5".to_owned(), "3".to_owned()));
    let backwards = available(5, 2, &other);
    let view = tracker.observe(&backwards, Some(SERVING));
    assert_eq!(generation_and_sequence(&view), ("5".to_owned(), "3".to_owned()));
    assert_eq!(
        tracker.view().version,
        version,
        "ignored readings change nothing"
    );

    // The current generation's own tombstone applies.
    let end = tombstone(StateUnavailableReason::Ended(RuntimeEnd::Disabled), Some(5));
    assert_eq!(kind(&tracker.observe(&end, Some(SERVING))), "absent");
    // And a write of the ended generation can no longer revive it after a
    // newer generation started.
    let next = tombstone(StateUnavailableReason::NotPublished, Some(6));
    assert_eq!(kind(&tracker.observe(&next, Some(SERVING))), "loading");
    assert_eq!(
        kind(&tracker.observe(&available(5, 4, &state()), Some(SERVING))),
        "loading"
    );
}

#[test]
fn a_new_generation_resets_the_sequence_while_the_version_keeps_growing() {
    let mut tracker = Tracker::default();
    let first = tracker.observe(&available(1, 40, &state()), Some(SERVING));
    let mut other = state();
    other.focus.window = Some("10".to_owned());
    let second = tracker.observe(&available(2, 1, &other), Some(SERVING));
    assert_eq!(
        generation_and_sequence(&second),
        ("2".to_owned(), "1".to_owned())
    );
    assert!(second.version > first.version);
    let third = tracker.observe(&available(2, 2, &state()), Some(SERVING));
    assert!(third.version > second.version);
    // Identical readings do not move the counter.
    assert_eq!(tracker.observe(&available(2, 2, &state()), Some(SERVING)), third);
}

#[test]
fn the_strip_keeps_legacy_filters_and_published_order() {
    let view = strip_of(7, 3, &state());
    let TilingStripView::Available {
        generation,
        sequence,
        workspaces,
        apps,
        focused_app,
        focused_workspace,
    } = view
    else {
        panic!("available")
    };
    assert_eq!((generation.as_str(), sequence.as_str()), ("7", "3"));
    assert_eq!(
        workspaces,
        vec![
            StripWorkspace {
                name: "terminal".to_owned(),
                focused: false
            },
            StripWorkspace {
                name: "coding".to_owned(),
                focused: true
            },
            StripWorkspace {
                name: "browser".to_owned(),
                focused: false
            },
        ],
        "every workspace, published order; the UI sorts"
    );
    let keys: Vec<&str> = apps.iter().map(|app| app.key.as_str()).collect();
    assert_eq!(
        keys,
        ["10", "14"],
        "minimized, hidden, inactive tabs and other workspaces are excluded"
    );
    assert_eq!(focused_workspace.as_deref(), Some("coding"));
    // The focused window comes from all windows, even another workspace.
    assert_eq!(
        focused_app,
        Some(StripApp {
            key: "20".to_owned(),
            app_name: "Safari".to_owned(),
            title: "Docs".to_owned()
        })
    );
}

#[test]
fn no_focus_means_no_apps_and_no_focused_app() {
    let mut unfocused = state();
    unfocused.focus = PublishedFocus::default();
    let TilingStripView::Available {
        apps,
        focused_app,
        focused_workspace,
        ..
    } = strip_of(1, 1, &unfocused)
    else {
        panic!("available")
    };
    assert!(apps.is_empty());
    assert_eq!(focused_app, None);
    assert_eq!(focused_workspace, None);
    // A focus key no published window carries is no focused app.
    let mut dangling = state();
    dangling.focus.window = Some("999".to_owned());
    let TilingStripView::Available { focused_app, .. } = strip_of(1, 1, &dangling) else {
        panic!("available")
    };
    assert_eq!(focused_app, None);
    // Zero workspaces is still available (the UI renders the empty slot).
    let empty = TilingPublishedState::default();
    let TilingStripView::Available { workspaces, apps, .. } = strip_of(1, 1, &empty) else {
        panic!("available")
    };
    assert!(workspaces.is_empty() && apps.is_empty());
}

#[test]
fn unicode_and_long_titles_are_carried_unchanged() {
    let long = "🙂 日本語 \u{200E}WhatsApp ".repeat(400);
    let mut unicode = state();
    unicode.windows[0].title.clone_from(&long);
    unicode.windows[0].app_name = "\u{200E}WhatsApp".to_owned();
    let TilingStripView::Available { apps, .. } = strip_of(1, 1, &unicode) else {
        panic!("available")
    };
    assert_eq!(apps[0].title, long);
    assert_eq!(apps[0].app_name, "\u{200E}WhatsApp");
    let json = serde_json::to_value(strip_of(1, 1, &unicode)).expect("encode");
    assert_eq!(json["apps"][0]["title"], long);
}

#[test]
fn the_wire_forms_are_tagged_camel_case_with_decimal_strings() {
    let json = serde_json::to_value(strip_of(u64::MAX, 2, &state())).expect("encode");
    assert_eq!(json["kind"], "available");
    assert_eq!(json["generation"], "18446744073709551615");
    assert_eq!(json["sequence"], "2");
    assert_eq!(json["focusedWorkspace"], "coding");
    assert_eq!(json["focusedApp"]["appName"], "Safari");
    assert_eq!(json["apps"][0]["appName"], "Zed");
    assert_eq!(
        serde_json::to_value(TilingStripView::Absent).expect("encode"),
        serde_json::json!({ "kind": "absent" })
    );
    assert_eq!(
        serde_json::to_value(TilingStripView::Loading).expect("encode"),
        serde_json::json!({ "kind": "loading" })
    );
    for (outcome, expected) in [
        (
            BarTilingOutcome::Applied,
            serde_json::json!({ "kind": "applied" }),
        ),
        (BarTilingOutcome::NoOp, serde_json::json!({ "kind": "noOp" })),
        (BarTilingOutcome::Stale, serde_json::json!({ "kind": "stale" })),
        (BarTilingOutcome::Busy, serde_json::json!({ "kind": "busy" })),
        (
            BarTilingOutcome::Uncertain,
            serde_json::json!({ "kind": "uncertain" }),
        ),
        (
            BarTilingOutcome::Refused,
            serde_json::json!({ "kind": "refused" }),
        ),
        (BarTilingOutcome::Failed, serde_json::json!({ "kind": "failed" })),
        (
            BarTilingOutcome::Invalid,
            serde_json::json!({ "kind": "invalid" }),
        ),
        (
            BarTilingOutcome::Unauthorized,
            serde_json::json!({ "kind": "unauthorized" }),
        ),
        (
            BarTilingOutcome::Unavailable {
                reason: BarTilingUnavailable::BarNotRunning,
            },
            serde_json::json!({ "kind": "unavailable", "reason": "barNotRunning" }),
        ),
        (
            BarTilingOutcome::Unavailable {
                reason: BarTilingUnavailable::CleanupPending,
            },
            serde_json::json!({ "kind": "unavailable", "reason": "cleanupPending" }),
        ),
    ] {
        assert_eq!(serde_json::to_value(outcome).expect("encode"), expected);
    }
}

#[test]
fn the_click_request_deserializes_strictly() {
    let good = serde_json::json!({
        "generation": "3",
        "sequence": "12",
        "action": { "kind": "focusWindow", "window": "18446744073709551617" },
    });
    let request: BarTilingRequest = serde_json::from_value(good).expect("decode");
    assert_eq!(request.action, BarTilingTarget::FocusWindow {
        window: "18446744073709551617".to_owned()
    });
    let workspace: BarTilingRequest = serde_json::from_value(serde_json::json!({
        "generation": "3",
        "sequence": "12",
        "action": { "kind": "focusWorkspace", "name": "coding" },
    }))
    .expect("decode");
    assert_eq!(workspace.action, BarTilingTarget::FocusWorkspace {
        name: "coding".to_owned()
    });
    for bad in [
        // Unknown action kinds, extra fields and missing fields.
        serde_json::json!({ "generation": "3", "sequence": "1", "action": { "kind": "pause" } }),
        serde_json::json!({ "generation": "3", "sequence": "1", "action": { "kind": "focusWorkspace", "name": "a", "screen": 1 } }),
        serde_json::json!({ "generation": "3", "sequence": "1", "action": { "kind": "focusWindow", "window": 7 } }),
        serde_json::json!({ "generation": "3", "sequence": "1", "action": { "kind": "focusWindow" } }),
        serde_json::json!({ "generation": "3", "sequence": "1", "action": { "kind": "focusWorkspace", "name": "a" }, "module": "tiling" }),
        serde_json::json!({ "generation": 3, "sequence": "1", "action": { "kind": "focusWorkspace", "name": "a" } }),
        serde_json::json!({ "sequence": "1", "action": { "kind": "focusWorkspace", "name": "a" } }),
        serde_json::json!({ "generation": "3", "sequence": "1", "action": "focusWorkspace" }),
    ] {
        assert!(
            serde_json::from_value::<BarTilingRequest>(bad.clone()).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn decimals_are_exact_and_rendered_state_is_revalidated() {
    assert_eq!(exact_decimal("0"), Some(0));
    assert_eq!(exact_decimal("18446744073709551615"), Some(u64::MAX));
    for bad in [
        "",
        "+1",
        "-1",
        "01",
        " 1",
        "1 ",
        "1e3",
        "0x1",
        "18446744073709551616",
        "１",
    ] {
        assert_eq!(exact_decimal(bad), None, "{bad:?}");
    }
    let reading = available(4, 10, &state());
    assert!(rendered_is_current(&reading, 4, 10));
    assert!(
        rendered_is_current(&reading, 4, 1),
        "an older sequence of the same generation is current"
    );
    assert!(
        !rendered_is_current(&reading, 4, 11),
        "a sequence ahead of the published one is stale"
    );
    assert!(
        !rendered_is_current(&reading, 3, 10),
        "an older generation is stale"
    );
    assert!(
        !rendered_is_current(&reading, 5, 1),
        "a future generation is stale"
    );
    assert!(!rendered_is_current(
        &tombstone(StateUnavailableReason::Ended(RuntimeEnd::Stopped), Some(4)),
        4,
        10
    ));
    assert!(!rendered_is_current(&StateReading::Absent, 4, 10));
}
