use super::*;
use crate::test_support::{entry, snapshot};

#[test]
fn main_is_the_first_snapshot_entry_not_a_focused_screen() {
    let snapshot = snapshot(vec![
        entry(10, 1280.0, 720.0, 2.0),
        entry(20, 1920.0, 1080.0, 1.0),
    ]);
    let selected = resolve_targets(&snapshot, &ScreenTarget::Main).expect("main");
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].id(), DisplayId::new(10));
}

#[test]
fn all_preserves_native_order_and_validates_every_display() {
    let snapshot = snapshot(vec![
        entry(30, 1280.0, 720.0, 1.0),
        entry(10, 2560.0, 1440.0, 2.0),
    ]);
    let selected = resolve_targets(&snapshot, &ScreenTarget::All).expect("all");
    assert_eq!(selected.iter().map(|target| target.id()).collect::<Vec<_>>(), [
        DisplayId::new(30),
        DisplayId::new(10),
    ]);
    assert_eq!(selected[0].pixels, TargetPixels { width: 1280, height: 720 });
    assert_eq!(selected[1].pixels, TargetPixels { width: 5120, height: 2880 });
}

#[test]
fn numeric_selection_is_one_based_and_zero_aliases_the_primary() {
    let snapshot = snapshot(vec![
        entry(1, 100.0, 100.0, 1.0),
        entry(2, 200.0, 200.0, 1.0),
        entry(3, 300.0, 300.0, 1.0),
    ]);
    assert_eq!(
        resolve_targets(&snapshot, &ScreenTarget::Index(0)).expect("zero")[0].id(),
        DisplayId::new(1)
    );
    assert_eq!(
        resolve_targets(&snapshot, &ScreenTarget::Index(1)).expect("one")[0].id(),
        DisplayId::new(1)
    );
    assert_eq!(
        resolve_targets(&snapshot, &ScreenTarget::Index(2)).expect("two")[0].id(),
        DisplayId::new(2)
    );
    assert_eq!(
        resolve_targets(&snapshot, &ScreenTarget::Index(3)).expect("three")[0].id(),
        DisplayId::new(3)
    );
    let error = resolve_targets(&snapshot, &ScreenTarget::Index(4)).expect_err("four");
    assert!(matches!(error, SelectionError::OutOfRange {
        requested: 4,
        available: 3
    }));
}

#[test]
fn parsing_accepts_only_the_documented_selectors() {
    assert_eq!(ScreenTarget::parse("all").expect("all"), ScreenTarget::All);
    assert_eq!(ScreenTarget::parse(" main ").expect("main"), ScreenTarget::Main);
    assert_eq!(ScreenTarget::parse("0").expect("zero"), ScreenTarget::Index(0));
    assert_eq!(
        ScreenTarget::parse("12").expect("numeric"),
        ScreenTarget::Index(12)
    );
    let error = ScreenTarget::parse("left").expect_err("invalid");
    assert!(matches!(error, SelectionError::InvalidSelector { .. }));
    let error = ScreenTarget::parse("-1").expect_err("negative");
    assert!(matches!(error, SelectionError::InvalidSelector { .. }));
}

#[test]
fn an_empty_snapshot_has_no_targets_and_invents_no_fallback() {
    let error =
        resolve_targets(&DisplaySnapshot::default(), &ScreenTarget::All).expect_err("empty");
    assert_eq!(error, SelectionError::NoDisplays);
    let error =
        resolve_targets(&DisplaySnapshot::default(), &ScreenTarget::Main).expect_err("empty main");
    assert_eq!(error, SelectionError::NoDisplays);
}

#[test]
fn mixed_backing_scales_produce_exact_pixels() {
    let one = entry(1, 1280.0, 720.0, 1.0);
    let two = entry(2, 1280.0, 720.0, 2.0);
    assert_eq!(target_pixels(&one).expect("1x"), TargetPixels {
        width: 1280,
        height: 720
    });
    assert_eq!(target_pixels(&two).expect("2x"), TargetPixels {
        width: 2560,
        height: 1440
    });
}

#[test]
fn fractional_pixels_truncate_like_the_legacy_adapter() {
    let display = entry(1, 100.9, 50.9, 1.0);
    assert_eq!(target_pixels(&display).expect("truncated"), TargetPixels {
        width: 100,
        height: 50
    });
}

#[test]
fn non_finite_non_positive_and_overflowing_geometry_is_rejected() {
    for (display, problem) in [
        (entry(1, 0.0, 100.0, 1.0), GeometryProblem::NonPositive),
        (entry(2, -1.0, 100.0, 1.0), GeometryProblem::NonPositive),
        (entry(3, 100.0, 100.0, 0.0), GeometryProblem::NonPositive),
        (entry(4, f64::NAN, 100.0, 1.0), GeometryProblem::NonFinite),
        (entry(5, 100.0, 100.0, f64::INFINITY), GeometryProblem::NonFinite),
        (entry(6, 1.0e300, 100.0, 1.0), GeometryProblem::Overflow),
        (entry(7, 20_000.0, 1_000.0, 1.0), GeometryProblem::TooLarge),
    ] {
        let error = target_pixels(&display).expect_err("invalid geometry");
        assert_eq!(
            error,
            SelectionError::Geometry { display: display.id, problem },
            "{display:?}"
        );
    }
}

#[test]
fn one_invalid_display_fails_the_whole_all_selection() {
    let snapshot = snapshot(vec![entry(1, 100.0, 100.0, 1.0), entry(2, f64::NAN, 100.0, 1.0)]);
    assert!(matches!(
        resolve_targets(&snapshot, &ScreenTarget::All),
        Err(SelectionError::Geometry { display, .. }) if display == DisplayId::new(2)
    ));
}
