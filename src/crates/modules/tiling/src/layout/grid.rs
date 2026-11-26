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
//!
//! Every shape is a lattice of column and row lanes; the runtime ratios are its
//! cumulative boundaries (see [`resolved_ratios`]). A window spanning several
//! lanes follows every boundary on its border.

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

/// The ratio layout of a shape: a lattice of column and row lanes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// One or no window: no ratio.
    Single,
    /// Equal-cell shapes (2, 4, 6, 8, 9): column then row boundaries.
    Regular { rows: usize, cols: usize },
    /// The first window spans a whole lane (3, 5, 7, 10). The ratio vector
    /// holds the `major` boundaries first (the first one is the master share,
    /// as before), then the `minor` ones.
    Spanning {
        /// Lanes along the master share's axis, master lane included.
        major: usize,
        /// Lanes across it.
        minor: usize,
        /// Whether the major lanes are columns.
        major_columns: bool,
    },
}

const fn shape(count: usize, landscape: bool) -> Shape {
    if let Some((rows, cols)) = regular_dimensions(count, landscape) {
        return Shape::Regular { rows, cols };
    }
    match count {
        3 | 5 | 7 => {
            let rows = if count == 7 { 3 } else { 2 };
            let stack = (count - 1).div_ceil(rows);
            if landscape {
                Shape::Spanning {
                    major: 1 + stack,
                    minor: rows,
                    major_columns: true,
                }
            } else {
                Shape::Spanning {
                    major: 1 + rows,
                    minor: stack,
                    major_columns: false,
                }
            }
        }
        10 if landscape => Shape::Spanning {
            major: 4,
            minor: 3,
            major_columns: true,
        },
        10 => Shape::Spanning {
            major: 3,
            minor: 4,
            major_columns: true,
        },
        _ => Shape::Single,
    }
}

/// The runs of cumulative boundaries in a resolved Grid ratio vector, in
/// order, as `(length, lowest first value, highest first value)`. Every other
/// boundary stays inside `0.05..=0.95`. Used by mouse resize to keep a moved
/// boundary between its neighbours.
#[must_use]
pub fn ratio_axes(count: usize, landscape: bool) -> Vec<(usize, f64, f64)> {
    match shape(count.min(MAX_GRID_TILES), landscape) {
        Shape::Single => Vec::new(),
        Shape::Regular { rows, cols } => vec![(cols - 1, 0.05, 0.95), (rows - 1, 0.05, 0.95)],
        Shape::Spanning { major, minor, .. } => {
            vec![(major - 1, 0.1, 0.9), (minor - 1, 0.05, 0.95)]
        }
    }
}

/// The full ratio vector a Grid of `count` windows lays out with: every
/// column and row boundary, cumulative over its axis. Missing values take the
/// default (equal lanes; for the spanning shapes, equal stack lanes after the
/// master share), so a short vector such as a lone master share still works.
/// A spanning shape's boundaries that are not strictly increasing fall back to
/// equal lanes for that axis.
#[must_use]
pub fn resolved_ratios(count: usize, landscape: bool, ratios: &[f64]) -> Vec<f64> {
    let count = count.min(MAX_GRID_TILES);
    if count == 2 {
        return vec![clamp_ratio(
            ratios.first().copied().unwrap_or(0.5),
            0.05,
            0.95,
        )];
    }
    match shape(count, landscape) {
        Shape::Single => Vec::new(),
        Shape::Regular { rows, cols } => {
            let axis = |lanes: usize, offset: usize| {
                (1..lanes).map(move |index| {
                    ratios.get(offset + index - 1).map_or_else(
                        || helpers::count_f64(index) / helpers::count_f64(lanes),
                        |ratio| clamp_ratio(*ratio, 0.05, 0.95),
                    )
                })
            };
            axis(cols, 0).chain(axis(rows, cols - 1)).collect()
        }
        Shape::Spanning { major, minor, .. } => {
            let share = clamp_ratio(
                ratios.first().copied().unwrap_or_else(|| 1.0 / helpers::count_f64(major)),
                0.1,
                0.9,
            );
            let stack = helpers::count_f64(major - 1);
            let mut out = vec![share];
            out.extend(increasing(
                ratios.get(1..major - 1).unwrap_or_default(),
                major - 2,
                share,
                |i| (1.0 - share).mul_add(helpers::count_f64(i) / stack, share),
            ));
            let minor_values = ratios.get(major - 1..).unwrap_or_default();
            out.extend(increasing(minor_values, minor - 1, 0.0, |i| {
                helpers::count_f64(i) / helpers::count_f64(minor)
            }));
            out
        }
    }
}

/// `len` boundaries after `after`: the given values when they are finite and
/// strictly increasing below 1, otherwise `default(1..=len)` for all of them.
fn increasing(values: &[f64], len: usize, after: f64, default: impl Fn(usize) -> f64) -> Vec<f64> {
    let mut previous = after;
    let mut out = Vec::with_capacity(len);
    for index in 0..len {
        let value = values.get(index).copied().unwrap_or_else(|| default(index + 1));
        if !(value.is_finite() && value > previous && value < 1.0) {
            return (1..=len).map(default).collect();
        }
        previous = value;
        out.push(value);
    }
    out
}

/// Lays out at most [`MAX_GRID_TILES`] of `ids`; callers cap beforehand.
#[must_use]
pub fn layout<K: Copy>(ids: &[K], usable: &Rect, gaps: &Gaps, ratios: &[f64]) -> Vec<(K, Rect)> {
    let ids = &ids[..ids.len().min(MAX_GRID_TILES)];
    let landscape = helpers::is_landscape(usable);
    let ratios = &resolved_ratios(ids.len(), landscape, ratios);
    match ids.len() {
        0 => Vec::new(),
        1 => vec![(ids[0], *usable)],
        2 => two(ids, usable, gaps, landscape, ratios),
        4 => cells(ids, usable, gaps, 2, 2, ratios),
        6 => cells(ids, usable, gaps, 2, 3, ratios),
        8 => cells(ids, usable, gaps, 2, 4, ratios),
        9 => cells(ids, usable, gaps, 3, 3, ratios),
        _ => spanning(ids, usable, gaps, landscape, ratios),
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

/// 3, 5, 7 and 10 windows on the lattice of [`resolved_ratios`]. 3, 5 and 7:
/// the first window spans the first column (landscape) or row (portrait) and
/// the rest fill the stack cells row by row. 10: a 3x4 (landscape) or 4x3
/// (portrait) lattice whose first window spans three rows of the first column;
/// every other window takes one cell, row by row.
fn spanning<K: Copy>(
    ids: &[K],
    usable: &Rect,
    gaps: &Gaps,
    landscape: bool,
    ratios: &[f64],
) -> Vec<(K, Rect)> {
    let Shape::Spanning { major, major_columns, .. } = shape(ids.len(), landscape) else {
        return Vec::new();
    };
    let edges = |values: &[f64]| {
        let mut out = vec![0.0];
        out.extend_from_slice(values);
        out.push(1.0);
        out
    };
    let major_edges = edges(&ratios[..major - 1]);
    let minor_edges = edges(&ratios[major - 1..]);
    let (col_edges, row_edges) = if major_columns {
        (major_edges, minor_edges)
    } else {
        (minor_edges, major_edges)
    };
    let (cols, rows) = (col_edges.len() - 1, row_edges.len() - 1);
    let available_width = usable.width - helpers::total_gap(gaps.inner_h, cols);
    let available_height = usable.height - helpers::total_gap(gaps.inner_v, rows);
    // The extent of lanes `from..to` on one axis.
    let extent = |edges: &[f64], origin: f64, available: f64, gap: f64, from: usize, to: usize| {
        let start = helpers::count_f64(from).mul_add(gap, edges[from].mul_add(available, origin));
        let end = helpers::count_f64(to - 1).mul_add(gap, edges[to].mul_add(available, origin));
        (start, end - start)
    };
    let cell = |(c0, c1): (usize, usize), (r0, r1): (usize, usize)| {
        let (x, width) = extent(&col_edges, usable.x, available_width, gaps.inner_h, c0, c1);
        let (y, height) = extent(&row_edges, usable.y, available_height, gaps.inner_v, r0, r1);
        Rect::new(x, y, width, height)
    };

    let mut slots = Vec::with_capacity(ids.len());
    if ids.len() == 10 {
        let span = 3;
        slots.push(((0, 1), (0, span)));
        for row in 0..rows {
            for col in usize::from(row < span)..cols {
                slots.push(((col, col + 1), (row, row + 1)));
            }
        }
    } else if major_columns {
        slots.push(((0, 1), (0, rows)));
        for row in 0..rows {
            for col in 1..cols {
                slots.push(((col, col + 1), (row, row + 1)));
            }
        }
    } else {
        slots.push(((0, cols), (0, 1)));
        for row in 1..rows {
            for col in 0..cols {
                slots.push(((col, col + 1), (row, row + 1)));
            }
        }
    }
    ids.iter().zip(slots).map(|(id, (c, r))| (*id, cell(c, r))).collect()
}
