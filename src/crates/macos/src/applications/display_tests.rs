//! Display-hint tests (HQ-03-04): pure coordinate normalization and screen
//! selection, plus the validated control path over fakes. No window list is read
//! from the system and no permission is requested.

use std::sync::Arc;
use std::time::Duration;

use stache_sdk::{CancellationToken, MainThreadExecutor};

use super::display::{
    DisplayHint, DisplayRect, DisplaySource, ScreenRecord, WindowRecord, appkit_rect_from_cg,
    select_display,
};
use super::testing::{FakeApplicationBackend, ManualExecutor};
use super::*;
use crate::desktop::DisplayId;

const PID: i32 = 4242;

fn rect(x: f64, y: f64, width: f64, height: f64) -> DisplayRect {
    DisplayRect::new(x, y, width, height)
}

fn screen(id: u32, x: f64, y: f64, width: f64, height: f64) -> ScreenRecord {
    ScreenRecord {
        id: DisplayId::new(id),
        frame: rect(x, y, width, height),
    }
}

fn window(pid: i32, layer: i32, bounds: DisplayRect) -> WindowRecord {
    WindowRecord {
        pid: Some(pid),
        layer: Some(layer),
        bounds: Some(bounds),
    }
}

/// Primary 1440x900 at the `AppKit` origin; a secondary to the right whose bottom
/// edge is lower (`AppKit` y is negative), and a third above the primary.
fn layout() -> Vec<ScreenRecord> {
    vec![
        screen(1, 0.0, 0.0, 1440.0, 900.0),
        screen(2, 1440.0, -180.0, 1920.0, 1080.0),
        screen(3, 0.0, 900.0, 1440.0, 900.0),
    ]
}

fn choose(windows: &[WindowRecord]) -> DisplayHint { select_display(Some(windows), PID, &layout()) }

#[test]
fn cg_top_left_converts_to_appkit_bottom_left() {
    // A 400x300 window 50 points below the top of a 900-point primary.
    let converted = appkit_rect_from_cg(rect(100.0, 50.0, 400.0, 300.0), 900.0);
    assert_eq!(converted, rect(100.0, 550.0, 400.0, 300.0));
    // The top-left origin maps to the primary's top edge.
    let top = appkit_rect_from_cg(rect(0.0, 0.0, 10.0, 10.0), 900.0);
    assert!((top.y + top.height - 900.0).abs() < f64::EPSILON);
    // Fractional (Retina point) coordinates are preserved exactly.
    let half = appkit_rect_from_cg(rect(0.5, 0.5, 100.5, 99.5), 900.0);
    assert_eq!(half, rect(0.5, 800.0, 100.5, 99.5));
}

#[test]
fn a_window_on_the_primary_screen_selects_it() {
    let hint = choose(&[window(PID, 0, rect(100.0, 100.0, 800.0, 600.0))]);
    assert_eq!(hint, DisplayHint {
        display: Some(DisplayId::new(1)),
        source: DisplaySource::WindowGeometry
    });
}

#[test]
fn origins_differ_between_a_secondary_to_the_right_and_one_above() {
    // Right: CG (1500, 100) -> AppKit y = 900 - 700 = 200, inside screen 2's
    // y range (-180..900).
    let right = choose(&[window(PID, 0, rect(1500.0, 100.0, 800.0, 600.0))]);
    assert_eq!(right.display, Some(DisplayId::new(2)));
    // Above: negative CG y is above the primary's top edge -> AppKit y >= 900.
    let above = choose(&[window(PID, 0, rect(100.0, -500.0, 400.0, 300.0))]);
    assert_eq!(above.display, Some(DisplayId::new(3)));
}

#[test]
fn the_largest_intersection_wins_and_the_first_screen_wins_a_tie() {
    // Straddles the primary/secondary seam: 100 points on screen 1, 700 on 2.
    let mostly_right = choose(&[window(PID, 0, rect(1340.0, 100.0, 800.0, 500.0))]);
    assert_eq!(mostly_right.display, Some(DisplayId::new(2)));
    let mostly_left = choose(&[window(PID, 0, rect(700.0, 100.0, 800.0, 500.0))]);
    assert_eq!(mostly_left.display, Some(DisplayId::new(1)));
    // Equal halves: the earlier screen in NSScreen order wins deterministically.
    let tie = choose(&[window(PID, 0, rect(1040.0, 100.0, 800.0, 500.0))]);
    assert_eq!(tie.display, Some(DisplayId::new(1)));
}

#[test]
fn the_front_most_layer_zero_window_of_the_process_decides() {
    let hint = choose(&[
        window(999, 0, rect(1500.0, 100.0, 800.0, 600.0)), // another process
        window(PID, 25, rect(1500.0, 100.0, 800.0, 600.0)), // not layer 0
        window(PID, 0, rect(100.0, 100.0, 300.0, 200.0)),  // front-most match
        window(PID, 0, rect(1500.0, 100.0, 1800.0, 800.0)), // larger, behind
    ]);
    assert_eq!(hint.display, Some(DisplayId::new(1)));
    assert_eq!(hint.source, DisplaySource::WindowGeometry);
}

#[test]
fn a_window_on_a_detached_display_has_no_intersection() {
    let hint = choose(&[window(PID, 0, rect(9000.0, 9000.0, 400.0, 300.0))]);
    assert_eq!(hint, DisplayHint::none(DisplaySource::NoIntersection));
}

#[test]
fn missing_and_filtered_geometry_is_an_explicit_none_with_a_source() {
    assert_eq!(
        select_display(None, PID, &layout()),
        DisplayHint::none(DisplaySource::ListUnavailable)
    );
    assert_eq!(choose(&[]), DisplayHint::none(DisplaySource::NoWindow));
    // Other processes and non-zero layers only.
    assert_eq!(
        choose(&[
            window(1, 0, rect(0.0, 0.0, 10.0, 10.0)),
            window(PID, 3, rect(0.0, 0.0, 10.0, 10.0))
        ]),
        DisplayHint::none(DisplaySource::NoWindow)
    );
    // Permission-filtered or malformed records: owner/bounds absent.
    let filtered = [
        WindowRecord {
            pid: None,
            layer: Some(0),
            bounds: Some(rect(0.0, 0.0, 100.0, 100.0)),
        },
        WindowRecord {
            pid: Some(PID),
            layer: Some(0),
            bounds: None,
        },
        window(PID, 0, rect(0.0, 0.0, 0.0, 50.0)),
        window(PID, 0, rect(f64::NAN, 0.0, 50.0, 50.0)),
    ];
    assert_eq!(
        choose(&filtered),
        DisplayHint::none(DisplaySource::GeometryFiltered)
    );
    // A later usable window still resolves behind filtered ones.
    let mut recovered = filtered.to_vec();
    recovered.push(window(PID, 0, rect(10.0, 10.0, 100.0, 100.0)));
    assert_eq!(choose(&recovered).display, Some(DisplayId::new(1)));
    assert_eq!(
        select_display(Some(&[window(PID, 0, rect(0.0, 0.0, 10.0, 10.0))]), PID, &[]),
        DisplayHint::none(DisplaySource::NoScreens)
    );
}

#[test]
fn the_hint_feeds_the_hud_request_display_type() {
    let hint = choose(&[window(PID, 0, rect(100.0, 100.0, 800.0, 600.0))]);
    let display: Option<crate::desktop::DisplayId> = hint.display();
    assert_eq!(display, Some(DisplayId::new(1)));
}

// ----- control path over fakes ---------------------------------------------

struct Fixture {
    backend: Arc<FakeApplicationBackend>,
    observation: ApplicationObservation,
}

fn fixture() -> Fixture {
    let executor = Arc::new(ManualExecutor::new(true));
    let backend = Arc::new(FakeApplicationBackend::new());
    let control = ApplicationControl::with_acknowledgement_budget(
        executor as Arc<dyn MainThreadExecutor>,
        Arc::clone(&backend) as Arc<dyn ApplicationBackend>,
        Duration::from_millis(500),
    );
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    backend.set_screens(layout());
    Fixture { backend, observation }
}

#[test]
fn display_for_validates_then_reads_geometry() {
    let f = fixture();
    let key = f.backend.add_running_with_pid("com.example.a", PID);
    f.backend.set_frontmost(Some(key));
    f.backend
        .set_windows(Some(vec![window(PID, 0, rect(1500.0, 100.0, 800.0, 600.0))]));
    let id = f.observation.running_applications("com.example.a").expect("running")[0];
    let hint = f.observation.display_for(id).expect("hint");
    assert_eq!(hint, DisplayHint {
        display: Some(DisplayId::new(2)),
        source: DisplaySource::WindowGeometry
    });
    assert_eq!(f.backend.display_queries(), 1);
}

#[test]
fn display_for_reads_no_geometry_unless_exact_identity_is_frontmost() {
    let f = fixture();
    let a = f.backend.add_running_with_pid("com.example.a", PID);
    let b = f.backend.add_running_with_pid("com.example.b", PID + 1);
    let finder = f.backend.add_running_with_pid(FINDER_BUNDLE_IDENTIFIER, 1);
    f.backend
        .set_windows(Some(vec![window(PID, 0, rect(100.0, 100.0, 300.0, 200.0))]));
    let id = f.observation.running_applications("com.example.a").expect("running")[0];

    for front in [Some(b), Some(finder), None] {
        f.backend.set_frontmost(front);
        assert_eq!(
            f.observation.display_for(id).expect("hint"),
            DisplayHint::none(DisplaySource::NotValidated)
        );
    }
    f.backend.set_frontmost(Some(a));
    f.backend.terminate(a);
    assert_eq!(
        f.observation.display_for(id).expect("hint"),
        DisplayHint::none(DisplaySource::NotValidated)
    );
    assert_eq!(f.backend.display_queries(), 0);
}

#[test]
fn display_for_reports_unavailable_geometry_explicitly() {
    let f = fixture();
    let key = f.backend.add_running_with_pid("com.example.a", PID);
    f.backend.set_frontmost(Some(key));
    let id = f.observation.running_applications("com.example.a").expect("running")[0];
    f.backend.set_windows(None);
    assert_eq!(
        f.observation.display_for(id).expect("hint"),
        DisplayHint::none(DisplaySource::ListUnavailable)
    );
    f.backend.set_windows(Some(Vec::new()));
    assert_eq!(
        f.observation.display_for(id).expect("hint"),
        DisplayHint::none(DisplaySource::NoWindow)
    );
}

#[test]
fn display_for_after_stop_is_not_installed() {
    let f = fixture();
    let key = f.backend.add_running_with_pid("com.example.a", PID);
    f.backend.set_frontmost(Some(key));
    let id = f.observation.running_applications("com.example.a").expect("running")[0];
    f.observation.stop().expect("stop");
    assert_eq!(
        f.observation.display_for(id),
        Err(ApplicationError::NotInstalled)
    );
}

#[test]
fn the_system_display_hint_refuses_off_main_calls() {
    let backend = SystemApplicationBackend;
    let handle = BackendHandle::allocate();
    let id = ApplicationId {
        handle,
        generation: 1,
        token: 0,
    };
    assert_eq!(
        backend.display_hint(handle, id).expect_err("off-main"),
        ApplicationError::NotMainThread
    );
}
