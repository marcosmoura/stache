//! Display layout, coordinate conversion and reconfiguration observer tests.
//! Fakes only: no `NSScreen` is read and no observer is registered.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::testing::FakeDisplayBackend;
use super::*;
use crate::applications::testing::ManualExecutor;

fn info(id: u32, frame: DisplayRect, visible: DisplayRect, primary: bool) -> DisplayInfo {
    DisplayInfo {
        id: DisplayId::new(id),
        frame,
        visible_frame: visible,
        scale: 2.0,
        refresh_hz: Some(60.0),
        name: Some(format!("Display {id}")),
        is_primary: primary,
        is_main: primary,
        is_builtin: primary,
    }
}

/// A 1440x900 primary, a 1920x1080 display to its left (bottom aligned with the
/// primary) and a 1280x720 display above it, in the `AppKit` space.
fn layout() -> DisplayLayout {
    DisplayLayout::new(vec![
        info(
            1,
            DisplayRect::new(0.0, 0.0, 1440.0, 900.0),
            DisplayRect::new(0.0, 80.0, 1440.0, 795.0),
            true,
        ),
        info(
            2,
            DisplayRect::new(-1920.0, 0.0, 1920.0, 1080.0),
            DisplayRect::new(-1920.0, 0.0, 1920.0, 1055.0),
            false,
        ),
        info(
            3,
            DisplayRect::new(0.0, 900.0, 1280.0, 720.0),
            DisplayRect::new(0.0, 900.0, 1280.0, 720.0),
            false,
        ),
    ])
}

#[test]
fn rect_conversion_round_trips_with_negative_origins() {
    let layout = layout();
    let height = layout.primary_height().expect("primary");
    assert_eq!(height, 900.0);
    for display in layout.displays() {
        let ax = layout.ax_frame(display).expect("ax");
        assert_eq!(
            appkit_rect_from_ax(ax, height),
            display.frame,
            "round trip {:?}",
            display.id
        );
    }
    // The display to the left keeps its negative x; its AX top is above the
    // primary's top because it is taller (1080 > 900).
    let left = layout.ax_frame(layout.get(DisplayId::new(2)).expect("left")).expect("ax");
    assert_eq!(left, DisplayRect::new(-1920.0, -180.0, 1920.0, 1080.0));
    // The display above the primary has a negative AX y.
    let above = layout.ax_frame(layout.get(DisplayId::new(3)).expect("above")).expect("ax");
    assert_eq!(above, DisplayRect::new(0.0, -720.0, 1280.0, 720.0));
    // The primary is the identity of the AX origin: its top-left is (0, 0).
    let primary = layout.ax_frame(layout.primary().expect("primary")).expect("ax");
    assert_eq!(primary, DisplayRect::new(0.0, 0.0, 1440.0, 900.0));
}

#[test]
fn visible_frames_convert_and_keep_the_menu_bar_and_dock_insets() {
    let layout = layout();
    let primary = layout.primary().expect("primary");
    // AppKit visible: dock 80 at the bottom, menu bar 25 at the top.
    let visible = layout.ax_visible_frame(primary).expect("ax");
    assert_eq!(visible, DisplayRect::new(0.0, 25.0, 1440.0, 795.0));
}

#[test]
fn point_conversion_is_an_involution() {
    let point = AppKitPoint { x: -300.0, y: 1000.0 };
    let ax = ax_point_from_appkit(point, 900.0);
    assert_eq!(ax, AxPoint { x: -300.0, y: -100.0 });
    assert_eq!(appkit_point_from_ax(ax, 900.0), point);
}

#[test]
fn display_for_ax_rect_picks_the_largest_intersection_and_handles_gaps() {
    let layout = layout();
    // A window straddling primary (mostly) and the left display.
    let straddle = DisplayRect::new(-200.0, 100.0, 900.0, 400.0);
    assert_eq!(layout.display_for_ax_rect(straddle), Some(DisplayId::new(1)));
    // Mostly on the left display, which has a negative origin.
    let mostly_left = DisplayRect::new(-1500.0, 0.0, 1600.0, 400.0);
    assert_eq!(layout.display_for_ax_rect(mostly_left), Some(DisplayId::new(2)));
    // Above the primary, in the AX space (negative y).
    assert_eq!(
        layout.display_for_ax_rect(DisplayRect::new(100.0, -600.0, 400.0, 400.0)),
        Some(DisplayId::new(3))
    );
    // Entirely off every display.
    assert_eq!(
        layout.display_for_ax_rect(DisplayRect::new(5000.0, 5000.0, 10.0, 10.0)),
        None
    );
    assert_eq!(DisplayLayout::default().display_for_ax_rect(straddle), None);
}

#[test]
fn an_empty_layout_has_no_primary_and_converts_nothing() {
    let empty = DisplayLayout::default();
    assert!(empty.primary().is_none());
    assert_eq!(empty.primary_height(), None);
    assert_eq!(empty.ax_frame(&layout().displays()[0]), None);
}

#[test]
fn refresh_rate_unknown_is_none_never_zero_hz() {
    assert_eq!(refresh_hz_from_native(0.0), None);
    assert_eq!(refresh_hz_from_native(-1.0), None);
    assert_eq!(refresh_hz_from_native(f64::NAN), None);
    assert_eq!(refresh_hz_from_native(f64::INFINITY), None);
    assert_eq!(refresh_hz_from_native(120.0), Some(120.0));
    assert_eq!(refresh_hz_from_native(59.94), Some(59.94));
}

#[test]
fn primary_and_main_are_distinct_flags() {
    let mut layout = layout();
    let displays: Vec<DisplayInfo> = layout
        .displays()
        .iter()
        .cloned()
        .map(|mut display| {
            display.is_main = display.id == DisplayId::new(2);
            display
        })
        .collect();
    layout = DisplayLayout::new(displays);
    assert_eq!(layout.primary().expect("primary").id, DisplayId::new(1));
    assert!(layout.get(DisplayId::new(2)).expect("left").is_main);
    assert!(!layout.get(DisplayId::new(2)).expect("left").is_primary);
}

#[test]
fn the_existing_desktop_snapshot_types_are_unchanged() {
    // `DisplayFrame` stays width/height only; `DisplayInfo` is additive.
    let frame = crate::desktop::DisplayFrame { width: 1.0, height: 2.0 };
    let entry = crate::desktop::DisplayEntry {
        id: DisplayId::new(1),
        frame,
        scale: 2.0,
    };
    assert_eq!(crate::desktop::DisplaySnapshot::new(vec![entry]).len(), 1);
}

struct Fixture {
    backend: FakeDisplayBackend,
    executor: Arc<ManualExecutor>,
    control: DisplayControl,
}

fn fixture(on_main: bool, budget: Duration) -> Fixture {
    let backend = FakeDisplayBackend::new(layout());
    let executor = Arc::new(ManualExecutor::new(on_main));
    let control = DisplayControl::with_budget(
        Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
        Arc::new(backend.clone()),
        budget,
    );
    Fixture { backend, executor, control }
}

#[test]
fn query_runs_inline_on_main_and_returns_the_layout() {
    let f = fixture(true, Duration::from_millis(200));
    assert_eq!(f.control.query().expect("layout"), layout());
    assert_eq!(f.backend.layout_reads(), 1);
}

#[test]
fn query_off_main_times_out_when_the_main_queue_never_drains() {
    let f = fixture(false, Duration::from_millis(30));
    assert_eq!(f.control.query(), Err(DisplayError::TimedOut));
    f.executor.reject(true);
    assert!(matches!(f.control.query(), Err(DisplayError::Dispatch(_))));
}

#[test]
fn the_observer_delivers_pure_ordered_hints_and_stops_inertly() {
    let f = fixture(true, Duration::from_millis(200));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink_seen = Arc::clone(&seen);
    let observer = f
        .control
        .observe_topology(move |hint| sink_seen.lock().expect("seen").push(hint))
        .expect("observe");
    assert_eq!(f.backend.reconfigure(), 1);
    assert_eq!(f.backend.reconfigure(), 1);
    assert_eq!(*seen.lock().expect("seen"), [
        TopologyHint { sequence: 1 },
        TopologyHint { sequence: 2 }
    ]);
    assert_eq!(
        f.backend.layout_reads(),
        0,
        "the notification path never queries"
    );

    observer.stop().expect("stop");
    assert_eq!(f.backend.observers(), 0);
    assert_eq!(f.backend.removals(), 1);
    assert_eq!(f.backend.reconfigure(), 0);
    observer.stop().expect("idempotent");
    assert_eq!(f.backend.removals(), 1);
}

#[test]
fn a_late_notification_after_stop_is_inert_even_if_removal_failed() {
    let f = fixture(true, Duration::from_millis(200));
    let count = Arc::new(AtomicUsize::new(0));
    let sink_count = Arc::clone(&count);
    let observer = f
        .control
        .observe_topology(move |_| {
            sink_count.fetch_add(1, Ordering::SeqCst);
        })
        .expect("observe");
    f.backend.fail_unobserves(1);
    assert!(matches!(observer.stop(), Err(DisplayError::RemovalFailed(_))));
    assert_eq!(
        f.backend.observers(),
        1,
        "failed removal stays registered and owned"
    );
    // The registered native block still fires, but the sink is inert.
    assert_eq!(f.backend.reconfigure(), 1);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    // Retry removes exactly once; dropping afterwards does not remove again.
    observer.stop().expect("retry");
    assert_eq!(f.backend.removals(), 1);
    drop(observer);
    assert_eq!(f.backend.removals(), 1);
}

#[test]
fn dropping_the_observer_removes_it() {
    let f = fixture(true, Duration::from_millis(200));
    let observer = f.control.observe_topology(|_| {}).expect("observe");
    assert_eq!(f.backend.observers(), 1);
    drop(observer);
    assert_eq!(f.backend.observers(), 0);
}

#[test]
fn a_failed_registration_leaves_nothing_registered() {
    let f = fixture(true, Duration::from_millis(200));
    f.backend.fail_next_observe();
    assert!(matches!(
        f.control.observe_topology(|_| {}),
        Err(DisplayError::ObservationFailed(_))
    ));
    assert_eq!(f.backend.observers(), 0);
}

#[test]
fn a_timed_out_registration_is_removed_by_the_ordered_cleanup_job() {
    let f = fixture(false, Duration::from_millis(30));
    let count = Arc::new(AtomicUsize::new(0));
    let sink_count = Arc::clone(&count);
    let result = f.control.observe_topology(move |_| {
        sink_count.fetch_add(1, Ordering::SeqCst);
    });
    assert!(matches!(result, Err(DisplayError::TimedOut)));
    // The observe job and the ordered cleanup job both wait in the queue.
    assert_eq!(f.executor.queued(), 2);
    f.executor.set_on_main(true);
    assert_eq!(f.executor.drain(), 2);
    assert_eq!(f.backend.observers(), 0, "the late registration was removed");
    assert_eq!(f.backend.reconfigure(), 0);
    assert_eq!(count.load(Ordering::SeqCst), 0);
}
