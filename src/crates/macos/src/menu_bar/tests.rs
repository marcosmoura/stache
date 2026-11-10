//! Menu-bar resolution, heuristic and observer tests. Fakes only: no `AppKit`
//! object is read and no observer is registered.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::testing::{FakeMenuBarBackend, hidden_sample, visible_sample};
use super::*;
use crate::applications::testing::ManualExecutor;

fn sample(
    menu: Option<bool>,
    pointer: Option<bool>,
    list: Option<Result<bool, String>>,
) -> MenuBarSample {
    MenuBarSample {
        menu_bar_visible: menu,
        pointer_in_row: pointer,
        window_list: list,
        auto_hide: Some(true),
    }
}

#[test]
fn either_signal_visible_wins_and_both_hidden_is_hidden() {
    for (menu, pointer, expected) in [
        (Some(true), Some(false), MenuBarVisibility::Visible),
        (Some(false), Some(true), MenuBarVisibility::Visible),
        (Some(true), None, MenuBarVisibility::Visible),
        (None, Some(true), MenuBarVisibility::Visible),
        (Some(false), Some(false), MenuBarVisibility::Hidden),
    ] {
        let resolved = sample(menu, pointer, None).resolve(None);
        assert_eq!(resolved.visibility, expected, "{menu:?}/{pointer:?}");
        assert_eq!(resolved.basis, MenuBarBasis::Signals);
    }
}

#[test]
fn an_undecided_sample_falls_back_to_the_window_list_then_the_prior_then_unknown() {
    let listed = sample(None, Some(false), Some(Ok(true))).resolve(Some(MenuBarVisibility::Hidden));
    assert_eq!(listed, MenuBarResolution {
        visibility: MenuBarVisibility::Visible,
        basis: MenuBarBasis::WindowList
    });
    let held = sample(Some(false), None, Some(Err("no list".to_owned())))
        .resolve(Some(MenuBarVisibility::Hidden));
    assert_eq!(held, MenuBarResolution {
        visibility: MenuBarVisibility::Hidden,
        basis: MenuBarBasis::Held
    });
    let unknown = sample(None, None, None).resolve(None);
    assert_eq!(unknown, MenuBarResolution {
        visibility: MenuBarVisibility::Unknown,
        basis: MenuBarBasis::Unresolved
    });
    // An unknown prior is not held.
    assert_eq!(
        sample(None, None, None).resolve(Some(MenuBarVisibility::Unknown)).visibility,
        MenuBarVisibility::Unknown
    );
}

#[test]
fn polling_is_needed_unless_auto_hide_is_known_off() {
    let mut value = hidden_sample();
    assert!(value.needs_poll());
    value.auto_hide = None;
    assert!(value.needs_poll(), "unknown preference keeps the poll");
    value.auto_hide = Some(false);
    assert!(!value.needs_poll());
}

#[test]
fn the_pointer_row_needs_the_right_screen_and_the_top_strip() {
    // A 1512x982 primary at the origin; the strip is [958, 982].
    let frame = (0.0, 0.0, 1512.0, 982.0);
    assert!(pointer_in_menu_bar_row((100.0, 982.0), frame, 24.0));
    assert!(pointer_in_menu_bar_row((100.0, 958.0), frame, 24.0));
    assert!(!pointer_in_menu_bar_row((100.0, 957.0), frame, 24.0));
    // Legacy compared only y: the same y on the external display to the left
    // (negative x) is not the primary's menu bar.
    assert!(!pointer_in_menu_bar_row((-1000.0, 970.0), frame, 24.0));
    assert!(!pointer_in_menu_bar_row((1512.0, 970.0), frame, 24.0));
}

#[test]
fn the_layer_25_heuristic_matches_legacy() {
    let window = |owner: Option<&str>, layer, name: Option<&str>, bounds| ListedWindow {
        owner: owner.map(str::to_owned),
        layer,
        name: name.map(str::to_owned),
        has_bounds: bounds,
    };
    assert!(window_list_has_menu_bar(&[window(
        Some("WindowServer"),
        Some(25),
        None,
        false
    )]));
    assert!(window_list_has_menu_bar(&[window(
        Some("Window Server"),
        Some(25),
        None,
        false
    )]));
    assert!(window_list_has_menu_bar(&[window(
        Some("Other"),
        Some(25),
        None,
        true
    )]));
    assert!(!window_list_has_menu_bar(&[window(
        Some("Other"),
        Some(25),
        None,
        false
    )]));
    assert!(window_list_has_menu_bar(&[window(
        Some("Control Center"),
        Some(24),
        Some("Menu Bar"),
        false
    )]));
    assert!(!window_list_has_menu_bar(&[window(
        Some("Control Center"),
        Some(27),
        Some("Menu Bar"),
        true
    )]));
    assert!(!window_list_has_menu_bar(&[window(None, Some(25), None, true)]));
    assert!(!window_list_has_menu_bar(&[window(
        Some("App"),
        None,
        None,
        true
    )]));
    assert!(!window_list_has_menu_bar(&[]));
}

struct Fixture {
    backend: FakeMenuBarBackend,
    executor: Arc<ManualExecutor>,
    control: MenuBarControl,
}

fn fixture(on_main: bool, budget: Duration) -> Fixture {
    let backend = FakeMenuBarBackend::new(hidden_sample());
    let executor = Arc::new(ManualExecutor::new(on_main));
    let control = MenuBarControl::with_budget(
        Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
        Arc::new(backend.clone()),
        budget,
    );
    Fixture { backend, executor, control }
}

#[test]
fn sample_runs_inline_on_main_and_times_out_when_the_queue_never_drains() {
    let f = fixture(true, Duration::from_millis(200));
    assert_eq!(f.control.sample().expect("sample"), hidden_sample());
    assert_eq!(f.backend.samples(), 1);
    f.backend.set_sample(visible_sample());
    assert_eq!(f.control.sample().expect("sample"), visible_sample());
    let stalled = fixture(false, Duration::from_millis(30));
    assert_eq!(stalled.control.sample(), Err(MenuBarError::TimedOut));
    stalled.executor.reject(true);
    assert!(matches!(
        stalled.control.sample(),
        Err(MenuBarError::Dispatch(_))
    ));
}

#[test]
fn the_observer_delivers_pure_ordered_hints_and_stops_inertly() {
    let f = fixture(true, Duration::from_millis(200));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink_seen = Arc::clone(&seen);
    let observer = f
        .control
        .observe(move |hint| sink_seen.lock().expect("seen").push(hint))
        .expect("observe");
    assert_eq!(f.backend.signal(), 1);
    assert_eq!(f.backend.signal(), 1);
    assert_eq!(*seen.lock().expect("seen"), [
        MenuBarHint { sequence: 1 },
        MenuBarHint { sequence: 2 }
    ]);
    assert_eq!(f.backend.samples(), 0, "the notification path never samples");
    observer.stop().expect("stop");
    assert_eq!((f.backend.observers(), f.backend.removals()), (0, 1));
    assert_eq!(f.backend.signal(), 0);
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
        .observe(move |_| {
            sink_count.fetch_add(1, Ordering::SeqCst);
        })
        .expect("observe");
    f.backend.fail_unobserves(1);
    assert!(matches!(observer.stop(), Err(MenuBarError::RemovalFailed(_))));
    assert_eq!(f.backend.observers(), 1, "a failed removal stays owned");
    assert_eq!(f.backend.signal(), 1);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    observer.stop().expect("retry");
    assert_eq!(f.backend.removals(), 1);
    drop(observer);
    assert_eq!(f.backend.removals(), 1);
}

#[test]
fn dropping_removes_and_a_failed_registration_leaves_nothing() {
    let f = fixture(true, Duration::from_millis(200));
    drop(f.control.observe(|_| {}).expect("observe"));
    assert_eq!(f.backend.observers(), 0);
    f.backend.fail_next_observe();
    assert!(matches!(
        f.control.observe(|_| {}),
        Err(MenuBarError::ObservationFailed(_))
    ));
    assert_eq!(f.backend.observers(), 0);
}
