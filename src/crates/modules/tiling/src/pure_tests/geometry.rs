//! Usable area and gap resolution.

use super::assert_rect;
use crate::{
    GapDisplay, GapValue, Gaps, GapsConfig, GapsConfigValue, GeometryError, Insets, Rect,
    ScreenGapsConfig, usable_area,
};

const FULL: Rect = Rect::new(0.0, 0.0, 1920.0, 1080.0);

#[test]
fn top_exclusion_is_max_of_native_and_reservation_plus_own_gap_once() {
    // Spec example: top 0, native 24, reservation 40, own outer 6 -> 46, not 70.
    let usable = usable_area(
        FULL,
        Insets::new(24.0, 0.0, 0.0, 0.0),
        Insets::new(40.0, 0.0, 0.0, 0.0),
        &Gaps::uniform(0.0, 6.0),
    )
    .unwrap();
    assert_rect(usable, 6.0, 46.0, 1908.0, 1028.0);
}

#[test]
fn reservation_is_never_added_to_the_native_inset() {
    let g = Gaps::ZERO;
    let native = usable_area(FULL, Insets::new(24.0, 0.0, 0.0, 0.0), Insets::ZERO, &g).unwrap();
    assert_rect(native, 0.0, 24.0, 1920.0, 1056.0);
    let reserved = usable_area(FULL, Insets::ZERO, Insets::new(40.0, 0.0, 0.0, 0.0), &g).unwrap();
    assert_rect(reserved, 0.0, 40.0, 1920.0, 1040.0);
    let both = usable_area(
        FULL,
        Insets::new(24.0, 0.0, 0.0, 0.0),
        Insets::new(40.0, 0.0, 0.0, 0.0),
        &g,
    )
    .unwrap();
    assert_rect(both, 0.0, 40.0, 1920.0, 1040.0);
}

#[test]
fn all_four_edges_use_their_own_max_and_gap() {
    // top max(24,0)+6=30, right max(0,12)+6=18, bottom max(5,40)+6=46, left max(0,8)+6=14.
    let usable = usable_area(
        FULL,
        Insets::new(24.0, 0.0, 5.0, 0.0),
        Insets::new(0.0, 12.0, 40.0, 8.0),
        &Gaps::uniform(0.0, 6.0),
    )
    .unwrap();
    assert_rect(usable, 14.0, 30.0, 1888.0, 1004.0);
}

#[test]
fn per_side_outer_gaps_apply_per_edge() {
    let gaps = Gaps::new(0.0, 0.0, 10.0, 20.0, 30.0, 40.0);
    let usable = usable_area(FULL, Insets::ZERO, Insets::ZERO, &gaps).unwrap();
    assert_rect(usable, 40.0, 10.0, 1860.0, 1040.0);
}

#[test]
fn negative_origin_displays_keep_their_offset() {
    let left_of_main = Rect::new(-1920.0, -200.0, 1920.0, 1080.0);
    let usable = usable_area(
        left_of_main,
        Insets::new(24.0, 0.0, 0.0, 0.0),
        Insets::ZERO,
        &Gaps::ZERO,
    )
    .unwrap();
    assert_rect(usable, -1920.0, -176.0, 1920.0, 1056.0);
}

#[test]
fn portrait_display() {
    let portrait = Rect::new(0.0, 0.0, 1080.0, 1920.0);
    let usable = usable_area(
        portrait,
        Insets::new(24.0, 0.0, 0.0, 0.0),
        Insets::new(40.0, 0.0, 0.0, 0.0),
        &Gaps::uniform(0.0, 6.0),
    )
    .unwrap();
    assert_rect(usable, 6.0, 46.0, 1068.0, 1868.0);
}

#[test]
fn logical_points_are_scale_independent() {
    // Scale 1 and scale 2 share logical points; only the pixel count differs.
    // Geometry is linear: scaling every logical input by 2 scales the output by 2.
    let one = usable_area(
        Rect::new(-100.0, 50.0, 1512.0, 982.0),
        Insets::new(24.0, 0.0, 10.0, 0.0),
        Insets::new(40.0, 0.0, 0.0, 6.0),
        &Gaps::uniform(0.0, 6.0),
    )
    .unwrap();
    let two = usable_area(
        Rect::new(-200.0, 100.0, 3024.0, 1964.0),
        Insets::new(48.0, 0.0, 20.0, 0.0),
        Insets::new(80.0, 0.0, 0.0, 12.0),
        &Gaps::uniform(0.0, 12.0),
    )
    .unwrap();
    assert_rect(two, one.x * 2.0, one.y * 2.0, one.width * 2.0, one.height * 2.0);
}

#[test]
fn infeasible_and_invalid_inputs_are_explicit_errors() {
    let g = Gaps::ZERO;
    let none = Insets::ZERO;
    assert_eq!(
        usable_area(Rect::new(0.0, 0.0, 0.0, 100.0), none, none, &g),
        Err(GeometryError::NoUsableArea)
    );
    // Excessive outer gaps: 600 + 600 on a 1000 wide display.
    assert_eq!(
        usable_area(
            Rect::new(0.0, 0.0, 1000.0, 800.0),
            none,
            none,
            &Gaps::uniform(0.0, 600.0)
        ),
        Err(GeometryError::NoUsableArea)
    );
    // Exactly zero remaining is also not usable.
    assert_eq!(
        usable_area(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            none,
            none,
            &Gaps::uniform(0.0, 50.0)
        ),
        Err(GeometryError::NoUsableArea)
    );
    assert!(matches!(
        usable_area(Rect::new(f64::NAN, 0.0, 10.0, 10.0), none, none, &g),
        Err(GeometryError::NonFinite { .. })
    ));
    assert!(matches!(
        usable_area(FULL, Insets::new(f64::INFINITY, 0.0, 0.0, 0.0), none, &g),
        Err(GeometryError::NonFinite { .. })
    ));
    assert!(matches!(
        usable_area(FULL, none, Insets::new(0.0, -1.0, 0.0, 0.0), &g),
        Err(GeometryError::Negative { .. })
    ));
    assert!(matches!(
        usable_area(FULL, none, none, &Gaps::uniform(f64::NAN, 0.0)),
        Err(GeometryError::NonFinite { .. })
    ));
    assert!(matches!(
        usable_area(FULL, none, none, &Gaps::uniform(0.0, -2.0)),
        Err(GeometryError::Negative { .. })
    ));
}

fn screen(name: &str, inner: u32) -> ScreenGapsConfig {
    ScreenGapsConfig {
        screen: name.to_owned(),
        inner: GapValue::Uniform(inner),
        outer: GapValue::Uniform(inner + 1),
    }
}

fn resolved(entries: Vec<ScreenGapsConfig>, name: &str, is_main: bool) -> Gaps {
    Gaps::from_config(&GapsConfigValue::PerScreen(entries), GapDisplay {
        name,
        is_main,
    })
}

#[test]
fn earlier_matching_alias_beats_a_later_exact_name() {
    // Legacy layout/gaps.rs:142-169: one `find` over name OR alias, first wins.
    let gaps = resolved(vec![screen("main", 5), screen("Built-in", 9)], "Built-in", true);
    assert_eq!(gaps.inner_h, 5.0);
    assert_eq!(gaps.outer_top, 6.0);
    // Reversed order: the exact name is first.
    let gaps = resolved(vec![screen("Built-in", 9), screen("main", 5)], "Built-in", true);
    assert_eq!(gaps.inner_h, 9.0);
}

#[test]
fn name_and_aliases_match_case_insensitively() {
    assert_eq!(
        resolved(vec![screen("BUILT-IN", 4)], "built-in", false).inner_h,
        4.0
    );
    assert_eq!(
        resolved(vec![screen("PRIMARY", 3), screen("x", 8)], "z", true).inner_h,
        3.0
    );
    assert_eq!(
        resolved(vec![screen("x", 8), screen("Secondary", 2)], "z", false).inner_h,
        2.0
    );
}

#[test]
fn unmatched_display_uses_first_entry_then_zero() {
    // `main` does not apply to a non-main display, so nothing matches: first entry.
    assert_eq!(
        resolved(vec![screen("Y", 3), screen("main", 7)], "X", false).inner_h,
        3.0
    );
    // `secondary` does not apply to the main display.
    assert_eq!(
        resolved(vec![screen("Y", 3), screen("secondary", 7)], "X", true).inner_h,
        3.0
    );
    assert_eq!(resolved(Vec::new(), "X", true), Gaps::ZERO);
}

#[test]
fn global_gaps_and_value_shapes() {
    let per_side_inner = GapValue::PerSide {
        top: 10,
        right: 4,
        bottom: 20,
        left: 8,
    };
    let config = GapsConfigValue::Global(GapsConfig {
        inner: per_side_inner,
        outer: GapValue::PerAxis { horizontal: 7, vertical: 3 },
    });
    let display = GapDisplay { name: "any", is_main: true };
    let gaps = Gaps::from_config(&config, display);
    // Inner per side averages opposite sides: (8+4)/2 horizontal, (10+20)/2 vertical.
    assert_eq!((gaps.inner_h, gaps.inner_v), (6.0, 15.0));
    // Per-axis outer: vertical -> top/bottom, horizontal -> right/left.
    assert_eq!(
        (
            gaps.outer_top,
            gaps.outer_right,
            gaps.outer_bottom,
            gaps.outer_left
        ),
        (3.0, 7.0, 3.0, 7.0)
    );
    // Integer division as legacy `(left + right) / 2`.
    let odd = GapsConfig {
        inner: GapValue::PerSide {
            top: 1,
            right: 4,
            bottom: 0,
            left: 3,
        },
        outer: GapValue::PerSide {
            top: 1,
            right: 2,
            bottom: 3,
            left: 4,
        },
    };
    let gaps = Gaps::from_gaps_config(&odd);
    assert_eq!((gaps.inner_h, gaps.inner_v), (3.0, 0.0));
    assert_eq!(
        (
            gaps.outer_top,
            gaps.outer_right,
            gaps.outer_bottom,
            gaps.outer_left
        ),
        (1.0, 2.0, 3.0, 4.0)
    );
    // The maximum cannot overflow.
    let max = GapsConfig {
        inner: GapValue::PerSide {
            top: u32::MAX,
            right: 0,
            bottom: u32::MAX,
            left: u32::MAX,
        },
        outer: GapValue::Uniform(0),
    };
    assert!(Gaps::from_gaps_config(&max).validate().is_ok());
}
