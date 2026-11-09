//! Grid: a near-square arrangement of at most [`MAX_GRID_TILES`] windows.
//!
//! Port of legacy `layout/grid.rs` shapes for 1 through 10 windows. The legacy
//! cap of 12 with silently ignored extras is **not** ported: this crate caps at
//! ten tiles and reports every extra identity as overflow (see
//! [`crate::LayoutPlan`]).
//!
//! Shapes (landscape): 1 full; 2 side by side; 3 and 5 master column plus a
//! 2-row stack; 4 is 2x2; 6 is 2x3; 7 master plus 3-row stack; 8 is 2x4; 9 is
//! 3x3; 10 is a 3x4 grid whose first window spans the left column. Odd counts
//! make the first window larger.

use super::{Gaps, helpers};
use crate::geometry::Rect;

/// Largest number of tiled windows in a grid; further eligible windows overflow.
pub const MAX_GRID_TILES: usize = 10;

/// Rows and columns of the regular (equal-cell) shapes, for `count` windows.
///
/// Master-stack shapes (3, 5, 7, 10) return `None`.
#[must_use]
pub const fn regular_dimensions(count: usize, landscape: bool) -> Option<(usize, usize)> {
    match count {
        2 if landscape => Some((1, 2)),
        2 => Some((2, 1)),
        4 => Some((2, 2)),
        6 => Some((2, 3)),
        8 => Some((2, 4)),
        9 => Some((3, 3)),
        _ => None,
    }
}

const fn clamp_ratio(ratio: f64, low: f64, high: f64) -> f64 {
    if ratio.is_finite() {
        ratio.clamp(low, high)
    } else {
        f64::midpoint(low, high)
    }
}

/// Lays out at most [`MAX_GRID_TILES`] of `ids`; callers cap beforehand.
#[must_use]
pub fn layout<K: Copy>(ids: &[K], usable: &Rect, gaps: &Gaps, ratios: &[f64]) -> Vec<(K, Rect)> {
    let ids = &ids[..ids.len().min(MAX_GRID_TILES)];
    let landscape = helpers::is_landscape(usable);
    match ids.len() {
        0 => Vec::new(),
        1 => vec![(ids[0], *usable)],
        2 => two(ids, usable, gaps, landscape, ratios),
        3 | 5 => master_stack(ids, usable, gaps, 2, landscape, ratios),
        7 => master_stack(ids, usable, gaps, 3, landscape, ratios),
        4 => cells(ids, usable, gaps, 2, 2, ratios),
        6 => cells(ids, usable, gaps, 2, 3, ratios),
        8 => cells(ids, usable, gaps, 2, 4, ratios),
        9 => cells(ids, usable, gaps, 3, 3, ratios),
        _ => master_3x4(ids, usable, gaps, 3, landscape, ratios),
    }
}

fn two<K: Copy>(
    ids: &[K],
    usable: &Rect,
    gaps: &Gaps,
    landscape: bool,
    ratios: &[f64],
) -> Vec<(K, Rect)> {
    let ratio = clamp_ratio(ratios.first().copied().unwrap_or(0.5), 0.05, 0.95);
    if landscape {
        let gap = gaps.inner_h;
        let available = usable.width - gap;
        let first = available * ratio;
        let second = available * (1.0 - ratio);
        vec![
            (ids[0], Rect::new(usable.x, usable.y, first, usable.height)),
            (
                ids[1],
                Rect::new(usable.x + first + gap, usable.y, second, usable.height),
            ),
        ]
    } else {
        let gap = gaps.inner_v;
        let available = usable.height - gap;
        let first = available * ratio;
        let second = available * (1.0 - ratio);
        vec![
            (ids[0], Rect::new(usable.x, usable.y, usable.width, first)),
            (
                ids[1],
                Rect::new(usable.x, usable.y + first + gap, usable.width, second),
            ),
        ]
    }
}

/// Regular grids. The first `cols - 1` ratios are cumulative column boundaries
/// and the next `rows - 1` cumulative row boundaries; missing ones are equal.
fn cells<K: Copy>(
    ids: &[K],
    usable: &Rect,
    gaps: &Gaps,
    rows: usize,
    cols: usize,
    ratios: &[f64],
) -> Vec<(K, Rect)> {
    let available_width = usable.width - helpers::total_gap(gaps.inner_h, cols);
    let available_height = usable.height - helpers::total_gap(gaps.inner_v, rows);
    let column_boundaries = cols - 1;

    let boundary = |index: usize, count: usize, offset: usize, limit: usize| -> f64 {
        if index == 0 {
            0.0
        } else if offset + index - 1 < ratios.len() && index <= limit {
            clamp_ratio(ratios[offset + index - 1], 0.05, 0.95)
        } else {
            helpers::count_f64(index) / helpers::count_f64(count)
        }
    };
    let col_start: Vec<f64> = (0..cols).map(|c| boundary(c, cols, 0, column_boundaries)).collect();
    let row_start: Vec<f64> =
        (0..rows).map(|r| boundary(r, rows, column_boundaries, rows - 1)).collect();

    let mut out = Vec::with_capacity(ids.len());
    let mut next = 0;
    for row in 0..rows {
        let row_end = row_start.get(row + 1).copied().unwrap_or(1.0);
        let height = (row_end - row_start[row]) * available_height;
        let y = helpers::count_f64(row)
            .mul_add(gaps.inner_v, row_start[row].mul_add(available_height, usable.y));
        for col in 0..cols {
            let Some(id) = ids.get(next) else {
                return out;
            };
            let col_end = col_start.get(col + 1).copied().unwrap_or(1.0);
            let width = (col_end - col_start[col]) * available_width;
            let x = helpers::count_f64(col)
                .mul_add(gaps.inner_h, col_start[col].mul_add(available_width, usable.x));
            out.push((*id, Rect::new(x, y, width, height)));
            next += 1;
        }
    }
    out
}

/// 3, 5 and 7 windows: the first spans a full column (landscape) or row
/// (portrait); the rest fill the remaining cells. The first ratio is the master
/// share, defaulting to one equal column or row.
fn master_stack<K: Copy>(
    ids: &[K],
    usable: &Rect,
    gaps: &Gaps,
    rows: usize,
    landscape: bool,
    ratios: &[f64],
) -> Vec<(K, Rect)> {
    let count = ids.len();
    let stack_cols = (count - 1).div_ceil(rows);
    let mut out = Vec::with_capacity(count);

    if landscape {
        let total_cols = 1 + stack_cols;
        let ratio = clamp_ratio(
            ratios.first().copied().unwrap_or_else(|| 1.0 / helpers::count_f64(total_cols)),
            0.1,
            0.9,
        );
        let available_width = usable.width - helpers::total_gap(gaps.inner_h, total_cols);
        let available_height = usable.height - helpers::total_gap(gaps.inner_v, rows);
        let master_width = available_width * ratio;
        let col_width = available_width * (1.0 - ratio) / helpers::count_f64(stack_cols);
        let cell_height = available_height / helpers::count_f64(rows);
        let master_height = gaps.inner_v.mul_add(
            helpers::count_f64(rows - 1),
            cell_height * helpers::count_f64(rows),
        );
        out.push((
            ids[0],
            Rect::new(usable.x, usable.y, master_width, master_height),
        ));

        let mut next = 1;
        for row in 0..rows {
            let y = helpers::count_f64(row).mul_add(cell_height + gaps.inner_v, usable.y);
            for col in 0..stack_cols {
                let Some(id) = ids.get(next) else { break };
                let x = helpers::count_f64(col)
                    .mul_add(col_width + gaps.inner_h, usable.x + master_width + gaps.inner_h);
                out.push((*id, Rect::new(x, y, col_width, cell_height)));
                next += 1;
            }
        }
    } else {
        let total_rows = 1 + rows;
        let ratio = clamp_ratio(
            ratios.first().copied().unwrap_or_else(|| 1.0 / helpers::count_f64(total_rows)),
            0.1,
            0.9,
        );
        let available_width = usable.width - helpers::total_gap(gaps.inner_h, stack_cols);
        let available_height = usable.height - helpers::total_gap(gaps.inner_v, total_rows);
        let master_height = available_height * ratio;
        let row_height = available_height * (1.0 - ratio) / helpers::count_f64(rows);
        let cell_width = available_width / helpers::count_f64(stack_cols);
        let master_width = gaps.inner_h.mul_add(
            helpers::count_f64(stack_cols - 1),
            cell_width * helpers::count_f64(stack_cols),
        );
        out.push((
            ids[0],
            Rect::new(usable.x, usable.y, master_width, master_height),
        ));

        let mut next = 1;
        for row in 0..rows {
            let y = helpers::count_f64(row).mul_add(
                row_height + gaps.inner_v,
                usable.y + master_height + gaps.inner_v,
            );
            for col in 0..stack_cols {
                let Some(id) = ids.get(next) else { break };
                let x = helpers::count_f64(col).mul_add(cell_width + gaps.inner_h, usable.x);
                out.push((*id, Rect::new(x, y, cell_width, row_height)));
                next += 1;
            }
        }
    }
    out
}

/// Ten windows: a 3x4 (landscape) or 4x3 (portrait) grid whose first window
/// spans `master_span` rows of the first column.
fn master_3x4<K: Copy>(
    ids: &[K],
    usable: &Rect,
    gaps: &Gaps,
    master_span: usize,
    landscape: bool,
    ratios: &[f64],
) -> Vec<(K, Rect)> {
    let (rows, cols) = if landscape { (3, 4) } else { (4, 3) };
    let ratio = clamp_ratio(
        ratios.first().copied().unwrap_or_else(|| 1.0 / helpers::count_f64(cols)),
        0.1,
        0.9,
    );
    let available_width = usable.width - helpers::total_gap(gaps.inner_h, cols);
    let available_height = usable.height - helpers::total_gap(gaps.inner_v, rows);
    let master_width = available_width * ratio;
    let stack_cols = cols - 1;
    let col_width = available_width * (1.0 - ratio) / helpers::count_f64(stack_cols);
    let cell_height = available_height / helpers::count_f64(rows);

    let mut out = Vec::with_capacity(ids.len());
    let master_height = gaps.inner_v.mul_add(
        helpers::count_f64(master_span - 1),
        cell_height * helpers::count_f64(master_span),
    );
    out.push((
        ids[0],
        Rect::new(usable.x, usable.y, master_width, master_height),
    ));

    let mut next = 1;
    for row in 0..rows {
        let y = helpers::count_f64(row).mul_add(cell_height + gaps.inner_v, usable.y);
        let start_col = usize::from(row < master_span);
        for col in start_col..cols {
            let Some(id) = ids.get(next) else { break };
            let (x, width) = if col == 0 {
                (usable.x, master_width)
            } else {
                let x = helpers::count_f64(col - 1)
                    .mul_add(col_width + gaps.inner_h, usable.x + master_width + gaps.inner_h);
                (x, col_width)
            };
            out.push((*id, Rect::new(x, y, width, cell_height)));
            next += 1;
        }
    }
    out
}
