//! Master: one master window beside (or above) a stack.
//!
//! Port of legacy `layout/master.rs`. The ratio is clamped to `0.1..=0.9`.
//! `Auto` resolves to left on landscape and top on portrait areas.

use super::split::has_valid_cumulative_ratios;
use super::{Gaps, helpers};
use crate::config::MasterPosition;
use crate::geometry::Rect;

/// Resolves `Auto` against the usable area orientation.
#[must_use]
pub const fn resolve_position(position: MasterPosition, landscape: bool) -> MasterPosition {
    match position {
        MasterPosition::Auto if landscape => MasterPosition::Left,
        MasterPosition::Auto => MasterPosition::Top,
        other => other,
    }
}

/// Clamps a master ratio to the layout's `0.1..=0.9` range.
#[must_use]
pub const fn clamp_ratio(ratio: f64) -> f64 { ratio.clamp(0.1, 0.9) }

/// Where stack window `index` starts along the stack axis and how long it is.
/// `weights` are cumulative boundaries between the stack windows, relative to
/// the stack; anything else gives equal windows.
fn stack_lane(
    weights: &[f64],
    count: usize,
    index: usize,
    origin: f64,
    extent: f64,
    gap: f64,
) -> (f64, f64) {
    let available = extent - helpers::total_gap(gap, count);
    let (start, end) = if has_valid_cumulative_ratios(weights, count) {
        let start = if index == 0 { 0.0 } else { weights[index - 1] };
        (start, weights.get(index).copied().unwrap_or(1.0))
    } else {
        let size = 1.0 / helpers::count_f64(count);
        (
            helpers::count_f64(index) * size,
            helpers::count_f64(index + 1) * size,
        )
    };
    (
        helpers::count_f64(index).mul_add(gap, start.mul_add(available, origin)),
        (end - start) * available,
    )
}

/// Lays `ids` out; the first identity is the master. `weights` divide the
/// stack (see [`stack_lane`]).
#[must_use]
pub fn layout<K: Copy>(
    ids: &[K],
    usable: &Rect,
    (ratio, weights): (f64, &[f64]),
    gaps: &Gaps,
    position: MasterPosition,
) -> Vec<(K, Rect)> {
    let Some(master) = ids.first() else {
        return Vec::new();
    };
    if ids.len() == 1 {
        return vec![(*master, *usable)];
    }
    let ratio = clamp_ratio(ratio);
    let stack = &ids[1..];
    let count = stack.len();

    let mut out = Vec::with_capacity(ids.len());
    match resolve_position(position, helpers::is_landscape(usable)) {
        MasterPosition::Left | MasterPosition::Auto => {
            let available = usable.width - gaps.inner_h;
            let master_width = available * ratio;
            let stack_width = available - master_width;
            let stack_x = usable.x + master_width + gaps.inner_h;
            out.push((
                *master,
                Rect::new(usable.x, usable.y, master_width, usable.height),
            ));
            for (i, id) in stack.iter().enumerate() {
                let (y, height) =
                    stack_lane(weights, count, i, usable.y, usable.height, gaps.inner_v);
                out.push((*id, Rect::new(stack_x, y, stack_width, height)));
            }
        }
        MasterPosition::Right => {
            let available = usable.width - gaps.inner_h;
            let stack_width = available * (1.0 - ratio);
            let master_width = available * ratio;
            let master_x = usable.x + stack_width + gaps.inner_h;
            out.push((
                *master,
                Rect::new(master_x, usable.y, master_width, usable.height),
            ));
            for (i, id) in stack.iter().enumerate() {
                let (y, height) =
                    stack_lane(weights, count, i, usable.y, usable.height, gaps.inner_v);
                out.push((*id, Rect::new(usable.x, y, stack_width, height)));
            }
        }
        MasterPosition::Top => {
            let available = usable.height - gaps.inner_v;
            let master_height = available * ratio;
            let stack_height = available - master_height;
            let stack_y = usable.y + master_height + gaps.inner_v;
            out.push((
                *master,
                Rect::new(usable.x, usable.y, usable.width, master_height),
            ));
            for (i, id) in stack.iter().enumerate() {
                let (x, width) =
                    stack_lane(weights, count, i, usable.x, usable.width, gaps.inner_h);
                out.push((*id, Rect::new(x, stack_y, width, stack_height)));
            }
        }
        MasterPosition::Bottom => {
            let available = usable.height - gaps.inner_v;
            let stack_height = available * (1.0 - ratio);
            let master_height = available * ratio;
            let master_y = usable.y + stack_height + gaps.inner_v;
            out.push((
                *master,
                Rect::new(usable.x, master_y, usable.width, master_height),
            ));
            for (i, id) in stack.iter().enumerate() {
                let (x, width) =
                    stack_lane(weights, count, i, usable.x, usable.width, gaps.inner_h);
                out.push((*id, Rect::new(x, usable.y, width, stack_height)));
            }
        }
    }
    out
}
