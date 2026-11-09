//! Fixtures for the pure geometry, layout, rule, preset and minimum-size code.
//!
//! Golden values are derived by hand from the legacy algorithms (cited per
//! test), never by running this crate and pasting its output.

#![allow(
    clippy::float_cmp,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::many_single_char_names
)]

mod geometry;
mod grid;
mod layouts;
mod minimum;
mod presets;
mod rules;

use crate::Rect;

/// Asserts a rectangle equals `(x, y, w, h)` within a tiny tolerance.
#[track_caller]
fn assert_rect(actual: Rect, x: f64, y: f64, w: f64, h: f64) {
    let close = |a: f64, b: f64| (a - b).abs() < 1e-6;
    assert!(
        close(actual.x, x)
            && close(actual.y, y)
            && close(actual.width, w)
            && close(actual.height, h),
        "expected ({x}, {y}, {w}, {h}), got {actual:?}"
    );
}

/// The frame of `id` in `frames`.
#[track_caller]
fn frame<K: PartialEq + std::fmt::Debug>(frames: &[(K, Rect)], id: K) -> Rect {
    frames
        .iter()
        .find(|(candidate, _)| *candidate == id)
        .unwrap_or_else(|| panic!("no frame for {id:?}"))
        .1
}
