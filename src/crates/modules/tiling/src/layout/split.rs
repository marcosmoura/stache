//! Split layouts: windows in columns or rows.
//!
//! Port of legacy `layout/split.rs`. Custom ratios are **cumulative
//! boundaries** and are only honoured when `count - 1` finite values are
//! strictly increasing inside `(0, 1)`; anything else behaves as an equal split.

use super::{Gaps, helpers};
use crate::config::LayoutType;
use crate::geometry::Rect;

/// Whether `layout` divides width. Only [`LayoutType::Split`] follows the area
/// orientation; the explicit variants never do.
#[must_use]
pub const fn is_horizontal(layout: LayoutType, is_landscape: bool) -> bool {
    match layout {
        LayoutType::SplitHorizontal => true,
        LayoutType::Split => is_landscape,
        _ => false,
    }
}

/// Whether `ratios` can describe a split of `window_count` windows.
#[must_use]
pub fn has_valid_cumulative_ratios(ratios: &[f64], window_count: usize) -> bool {
    window_count > 1
        && ratios.len() == window_count - 1
        && ratios
            .iter()
            .try_fold(0.0, |previous, &ratio| {
                (ratio.is_finite() && ratio > previous && ratio < 1.0).then_some(ratio)
            })
            .is_some()
}

/// Lays `ids` out for `layout` (one of the three split variants).
#[must_use]
pub fn layout<K: Copy>(
    layout: LayoutType,
    ids: &[K],
    usable: &Rect,
    gaps: &Gaps,
    ratios: &[f64],
) -> Vec<(K, Rect)> {
    if ids.is_empty() {
        return Vec::new();
    }
    let horizontal = is_horizontal(layout, helpers::is_landscape(usable));
    let (gap, extent, origin) = if horizontal {
        (gaps.inner_h, usable.width, usable.x)
    } else {
        (gaps.inner_v, usable.height, usable.y)
    };
    let count = ids.len();
    let available = extent - helpers::total_gap(gap, count);
    let custom = has_valid_cumulative_ratios(ratios, count);

    let mut previous = 0.0_f64;
    ids.iter()
        .enumerate()
        .map(|(i, id)| {
            let (start, size) = if custom {
                let next = ratios.get(i).copied().unwrap_or(1.0);
                let start = previous.mul_add(available, origin) + helpers::count_f64(i) * gap;
                let size = (next - previous) * available;
                previous = next;
                (start, size)
            } else {
                let size = available / helpers::count_f64(count);
                (helpers::count_f64(i).mul_add(size + gap, origin), size)
            };
            let frame = if horizontal {
                Rect::new(start, usable.y, size, usable.height)
            } else {
                Rect::new(usable.x, start, usable.width, size)
            };
            (*id, frame)
        })
        .collect()
}
