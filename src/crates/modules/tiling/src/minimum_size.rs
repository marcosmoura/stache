//! Minimum window sizes: bounded redistribution and honest refusal handling.
//!
//! Reported (`AXMinimumSize`) minimums win over **inferred** ones, which are
//! learned when a window refuses to shrink to a written frame. Everything here
//! is a bounded pure computation: at most [`MAX_PASSES`] adjustment passes and
//! no retry-to-convergence loop. When constraints cannot be met, the result says
//! so ([`MinimumStatus::Infeasible`] / [`MinimumStatus::Unresolved`]) and keeps
//! frames that make no claim of compliance. Nothing here assumes a native write
//! succeeded; observations are fed back through [`observe_write`].

use crate::config::LayoutType;
use crate::geometry::Rect;
use crate::layout::{
    Gaps, MAX_GRID_TILES, clamp_master_ratio, dwindle_layout, grid_layout,
    has_valid_cumulative_ratios, is_dwindle_split_horizontal, is_split_horizontal,
    regular_grid_dimensions, split_layout,
};

/// Adjustment passes the dwindle and master-stack solvers may run (legacy 3).
pub const MAX_PASSES: usize = 3;

/// A frame dimension counts as violated only when short by more than this
/// (legacy `would_violate_minimum_size`, 1 point).
pub const VIOLATION_TOLERANCE: f64 = 1.0;

/// A refused resize is inferred when the actual size exceeds the expected size
/// by more than this (legacy `detect_and_update_inferred_minimum`, 5 points).
pub const INFERENCE_TOLERANCE: f64 = 5.0;

/// A width and height in logical points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

impl Size {
    /// Creates a size.
    #[must_use]
    pub const fn new(width: f64, height: f64) -> Self { Self { width, height } }

    fn is_valid(self) -> bool {
        self.width.is_finite() && self.height.is_finite() && self.width >= 0.0 && self.height >= 0.0
    }
}

/// The minimum-size knowledge about one window.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MinimumConstraint {
    /// Size the application reports.
    pub reported: Option<Size>,
    /// Size inferred from a refused resize.
    pub inferred: Option<Size>,
}

impl MinimumConstraint {
    /// The effective minimum: reported, else inferred. Invalid (non-finite or
    /// negative) values are ignored.
    #[must_use]
    pub fn effective(&self) -> Option<Size> {
        self.reported
            .filter(|s| s.is_valid())
            .or_else(|| self.inferred.filter(|s| s.is_valid()))
    }

    /// Forgets the inferred minimum (workspace balance).
    pub const fn clear_inferred(&mut self) { self.inferred = None; }
}

/// A window shorter than its minimum on one or both axes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Violation<K> {
    /// The window.
    pub id: K,
    /// Its position in the frame list.
    pub index: usize,
    /// Width is below the minimum.
    pub width: bool,
    /// Height is below the minimum.
    pub height: bool,
}

/// Finds frames that violate `minimum_of`.
#[must_use]
pub fn find_violations<K: Copy>(
    frames: &[(K, Rect)],
    minimum_of: &dyn Fn(&K) -> Option<Size>,
) -> Vec<Violation<K>> {
    frames
        .iter()
        .enumerate()
        .filter_map(|(index, (id, frame))| {
            let minimum = minimum_of(id)?;
            let width = frame.width < minimum.width - VIOLATION_TOLERANCE;
            let height = frame.height < minimum.height - VIOLATION_TOLERANCE;
            (width || height).then_some(Violation { id: *id, index, width, height })
        })
        .collect()
}

fn deficit<K: Copy>(frames: &[(K, Rect)], minimum_of: &dyn Fn(&K) -> Option<Size>) -> f64 {
    frames
        .iter()
        .filter_map(|(id, frame)| {
            minimum_of(id)
                .map(|m| (m.width - frame.width).max(0.0) + (m.height - frame.height).max(0.0))
        })
        .sum()
}

/// Why constraints cannot all be satisfied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InfeasibleReason {
    /// The minimums along the controlling axis exceed the available space.
    RequiredExceedsAvailable {
        /// Points required.
        required: f64,
        /// Points available.
        available: f64,
    },
    /// Nothing usable remains after gaps.
    NoUsableSpace,
}

/// Outcome class of minimum-size enforcement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MinimumStatus {
    /// Every minimum already holds; frames are unchanged.
    Satisfied,
    /// Ratios were redistributed and every minimum now holds.
    Adjusted,
    /// Provably impossible; frames are the unmodified layout.
    Infeasible(InfeasibleReason),
    /// Best effort only: the least-violating frames are returned and this many
    /// windows still violate (the layout has no ratio that controls them, a
    /// ratio bound was hit or the pass limit ended).
    Unresolved {
        /// Windows still below their minimum.
        remaining: usize,
    },
}

/// Frames, the ratios that produced them and the status.
#[derive(Clone, Debug, PartialEq)]
pub struct MinimumResult<K> {
    /// Desired frames.
    pub frames: Vec<(K, Rect)>,
    /// Adjusted ratios when [`MinimumStatus::Adjusted`] (or best-effort).
    pub ratios: Option<Vec<f64>>,
    /// The status.
    pub status: MinimumStatus,
}

/// Inputs of minimum-size enforcement.
pub struct MinimumRequest<'a, K> {
    /// Split, Dwindle or Grid. Other layouts are returned unchanged.
    pub layout: LayoutType,
    /// Layoutable identities in order. For Grid, at most [`MAX_GRID_TILES`].
    pub ids: &'a [K],
    /// The usable area (outer gaps already applied).
    pub usable: Rect,
    /// Gaps.
    pub gaps: Gaps,
    /// The current runtime ratios.
    pub ratios: &'a [f64],
    /// Effective minimum of each window.
    pub minimum_of: &'a dyn Fn(&K) -> Option<Size>,
}

fn frames_for<K: Copy>(request: &MinimumRequest<'_, K>, ratios: &[f64]) -> Vec<(K, Rect)> {
    let (ids, usable, gaps) = (request.ids, &request.usable, &request.gaps);
    match request.layout {
        LayoutType::Dwindle => dwindle_layout(ids, usable, gaps, ratios),
        LayoutType::Grid => grid_layout(ids, usable, gaps, ratios),
        LayoutType::Split | LayoutType::SplitVertical | LayoutType::SplitHorizontal => {
            split_layout(request.layout, ids, usable, gaps, ratios)
        }
        _ => Vec::new(),
    }
}

/// Enforces minimums for Split, Dwindle and Grid. Other layouts never need it
/// and come back [`MinimumStatus::Satisfied`] with no frames.
#[must_use]
pub fn enforce<K: Copy>(request: &MinimumRequest<'_, K>) -> MinimumResult<K> {
    let ids = &request.ids[..request.ids.len().min(match request.layout {
        LayoutType::Grid => MAX_GRID_TILES,
        _ => usize::MAX,
    })];
    let request = MinimumRequest {
        layout: request.layout,
        ids,
        usable: request.usable,
        gaps: request.gaps,
        ratios: request.ratios,
        minimum_of: request.minimum_of,
    };
    let initial_ratios: &[f64] = request.ratios;
    let valid_inputs = request.usable.has_area()
        && request.gaps.validate().is_ok()
        && initial_ratios.iter().all(|r| r.is_finite());
    let unchanged = |status| MinimumResult {
        frames: if valid_inputs {
            frames_for(&request, initial_ratios)
        } else {
            Vec::new()
        },
        ratios: None,
        status,
    };
    if ids.len() < 2
        || !matches!(
            request.layout,
            LayoutType::Split
                | LayoutType::SplitVertical
                | LayoutType::SplitHorizontal
                | LayoutType::Dwindle
                | LayoutType::Grid
        )
    {
        return unchanged(MinimumStatus::Satisfied);
    }
    if !valid_inputs {
        return unchanged(MinimumStatus::Infeasible(InfeasibleReason::NoUsableSpace));
    }
    let initial = frames_for(&request, initial_ratios);
    if find_violations(&initial, request.minimum_of).is_empty() {
        return MinimumResult {
            frames: initial,
            ratios: None,
            status: MinimumStatus::Satisfied,
        };
    }
    match request.layout {
        LayoutType::Dwindle => enforce_dwindle(&request, initial),
        LayoutType::Grid => enforce_grid(&request, initial),
        _ => enforce_split(&request, initial),
    }
}

fn unresolved<K: Copy>(
    request: &MinimumRequest<'_, K>,
    frames: Vec<(K, Rect)>,
    ratios: Option<Vec<f64>>,
) -> MinimumResult<K> {
    let remaining = find_violations(&frames, request.minimum_of).len();
    MinimumResult {
        frames,
        ratios,
        status: MinimumStatus::Unresolved { remaining },
    }
}

fn adjusted_or_unresolved<K: Copy>(
    request: &MinimumRequest<'_, K>,
    frames: Vec<(K, Rect)>,
    ratios: Vec<f64>,
) -> MinimumResult<K> {
    if find_violations(&frames, request.minimum_of).is_empty() {
        MinimumResult {
            frames,
            ratios: Some(ratios),
            status: MinimumStatus::Adjusted,
        }
    } else {
        unresolved(request, frames, Some(ratios))
    }
}

// --- Split ---------------------------------------------------------------

fn enforce_split<K: Copy>(
    request: &MinimumRequest<'_, K>,
    initial: Vec<(K, Rect)>,
) -> MinimumResult<K> {
    let count = request.ids.len();
    let horizontal =
        is_split_horizontal(request.layout, request.usable.width >= request.usable.height);
    let (extent, gap) = if horizontal {
        (request.usable.width, request.gaps.inner_h)
    } else {
        (request.usable.height, request.gaps.inner_v)
    };
    #[allow(clippy::cast_precision_loss)] // window counts are tiny
    let available = gap.mul_add(-((count - 1) as f64), extent);
    if available <= 0.0 {
        return MinimumResult {
            frames: initial,
            ratios: None,
            status: MinimumStatus::Infeasible(InfeasibleReason::NoUsableSpace),
        };
    }
    let minimums: Vec<f64> = request
        .ids
        .iter()
        .map(|id| {
            (request.minimum_of)(id).map_or(0.0, |m| if horizontal { m.width } else { m.height })
        })
        .collect();
    let required: f64 = minimums.iter().sum();
    if required > available {
        return MinimumResult {
            frames: initial,
            ratios: None,
            status: MinimumStatus::Infeasible(InfeasibleReason::RequiredExceedsAvailable {
                required,
                available,
            }),
        };
    }
    let min_ratios: Vec<f64> = minimums.iter().map(|m| (m / available).min(1.0)).collect();
    let window_ratios = redistribute(request.ratios, &min_ratios, count);

    let mut position = if horizontal {
        request.usable.x
    } else {
        request.usable.y
    };
    let mut frames = Vec::with_capacity(count);
    let mut cumulative = Vec::with_capacity(count - 1);
    let mut running = 0.0;
    for (i, id) in request.ids.iter().enumerate() {
        let size = available * window_ratios[i];
        let frame = if horizontal {
            Rect::new(position, request.usable.y, size, request.usable.height)
        } else {
            Rect::new(request.usable.x, position, request.usable.width, size)
        };
        frames.push((*id, frame));
        position += size + gap;
        running += window_ratios[i];
        if i + 1 < count {
            cumulative.push(running);
        }
    }
    adjusted_or_unresolved(request, frames, cumulative)
}

/// Per-window shares (summing to 1) that honour `min_ratios`, preserving the
/// relative sizes of `cumulative` boundaries where possible (legacy
/// `compute_adjusted_ratios`). Work is O(n^2) in the window count.
#[must_use]
pub fn redistribute(cumulative: &[f64], min_ratios: &[f64], count: usize) -> Vec<f64> {
    if count == 0 {
        return Vec::new();
    }
    #[allow(clippy::cast_precision_loss)]
    let mut shares: Vec<f64> = if has_valid_cumulative_ratios(cumulative, count) {
        (0..count)
            .map(|i| {
                let start = if i == 0 { 0.0 } else { cumulative[i - 1] };
                cumulative.get(i).copied().unwrap_or(1.0) - start
            })
            .collect()
    } else {
        vec![1.0 / count as f64; count]
    };
    let minimum = |i: usize| {
        min_ratios
            .get(i)
            .copied()
            .filter(|r| r.is_finite())
            .unwrap_or_default()
            .clamp(0.0, 1.0)
    };
    for i in 0..count {
        if shares[i] < minimum(i) {
            let mut remaining = minimum(i) - shares[i];
            shares[i] = minimum(i);
            for (j, share) in shares.iter_mut().enumerate() {
                if j != i && remaining > 0.0 {
                    let available = *share - minimum(j);
                    if available > 0.0 {
                        let take = available.min(remaining);
                        *share -= take;
                        remaining -= take;
                    }
                }
            }
        }
    }
    let sum: f64 = shares.iter().sum();
    if sum.is_finite() && sum > f64::EPSILON && (sum - 1.0).abs() > 0.001 {
        for share in &mut shares {
            *share /= sum;
        }
    }
    shares
}

// --- Dwindle -------------------------------------------------------------

fn enforce_dwindle<K: Copy>(
    request: &MinimumRequest<'_, K>,
    initial: Vec<(K, Rect)>,
) -> MinimumResult<K> {
    let splits = request.ids.len() - 1;
    let mut ratios: Vec<f64> = request.ratios.to_vec();
    ratios.resize(splits.max(ratios.len()), 0.5);
    let landscape = request.usable.width >= request.usable.height;

    let mut best = initial;
    for _ in 0..MAX_PASSES {
        let violations = find_violations(&best, request.minimum_of);
        if violations.is_empty() {
            break;
        }
        let mut adjustments: Vec<(usize, f64)> = Vec::new();
        for violation in &violations {
            let index = violation.index;
            let Some(minimum) = (request.minimum_of)(&violation.id) else {
                continue;
            };
            let frame = best[index].1;
            // Window 0 is the first half of split 0; window `n` is the second
            // half of split `n - 1`.
            let split = index.saturating_sub(1);
            let horizontal = is_dwindle_split_horizontal(split, landscape);
            let (needed, total) = if horizontal {
                ((minimum.width - frame.width).max(0.0), request.usable.width)
            } else {
                ((minimum.height - frame.height).max(0.0), request.usable.height)
            };
            let adjustment = (needed / total).min(0.3);
            if adjustment > 0.01 {
                adjustments.push((split, if index == 0 { adjustment } else { -adjustment }));
            }
        }
        if adjustments.is_empty() {
            break;
        }
        let previous = ratios.clone();
        for (split, amount) in adjustments {
            ratios[split] = (ratios[split] + amount).clamp(0.1, 0.9);
        }
        if ratios == previous {
            break;
        }
        let candidate = frames_for(request, &ratios);
        let better =
            deficit(&candidate, request.minimum_of) < deficit(&best, request.minimum_of) - 1.0;
        let resolved = find_violations(&candidate, request.minimum_of).is_empty();
        if resolved || better {
            best = candidate;
        } else {
            break;
        }
    }
    adjusted_or_unresolved(request, best, ratios)
}

// --- Grid ----------------------------------------------------------------

fn enforce_grid<K: Copy>(
    request: &MinimumRequest<'_, K>,
    initial: Vec<(K, Rect)>,
) -> MinimumResult<K> {
    let count = request.ids.len();
    let landscape = request.usable.width >= request.usable.height;
    if let Some((rows, cols)) = regular_grid_dimensions(count, landscape) {
        return enforce_regular_grid(request, initial, rows, cols);
    }
    if !matches!(count, 3 | 5 | 7) {
        // The ten-window shape has no independent ratio for its cells; report
        // honestly rather than nudging an unrelated ratio.
        return unresolved(request, initial, None);
    }

    // Master-stack: only the first ratio (master share) is controllable.
    let mut ratios: Vec<f64> = if request.ratios.is_empty() {
        vec![0.5]
    } else {
        request.ratios.to_vec()
    };
    let mut best = initial;
    for _ in 0..MAX_PASSES {
        let violations = find_violations(&best, request.minimum_of);
        if violations.is_empty() {
            break;
        }
        let total = if landscape {
            request.usable.width
        } else {
            request.usable.height
        };
        let mut change = 0.0_f64;
        for violation in &violations {
            let Some(minimum) = (request.minimum_of)(&violation.id) else {
                continue;
            };
            let frame = best[violation.index].1;
            let (needed, violated) = if landscape {
                ((minimum.width - frame.width).max(0.0), violation.width)
            } else {
                ((minimum.height - frame.height).max(0.0), violation.height)
            };
            let adjustment = (needed / total).min(0.3);
            if !violated || adjustment < 0.01 {
                continue;
            }
            change += if violation.index == 0 {
                adjustment
            } else {
                -adjustment
            };
        }
        if change.abs() <= 0.01 {
            break;
        }
        let previous = ratios[0];
        ratios[0] = (ratios[0] + change).clamp(0.1, 0.9);
        if (ratios[0] - previous).abs() < f64::EPSILON {
            break;
        }
        let candidate = frames_for(request, &ratios);
        let resolved = find_violations(&candidate, request.minimum_of).is_empty();
        let better =
            deficit(&candidate, request.minimum_of) < deficit(&best, request.minimum_of) - 1.0;
        if resolved || better {
            best = candidate;
        } else {
            break;
        }
    }
    adjusted_or_unresolved(request, best, ratios)
}

/// Regular grids: a column needs its widest minimum and a row its tallest, so
/// solve each axis directly (legacy `enforce_regular_grid_minimums`).
fn enforce_regular_grid<K: Copy>(
    request: &MinimumRequest<'_, K>,
    initial: Vec<(K, Rect)>,
    rows: usize,
    cols: usize,
) -> MinimumResult<K> {
    #[allow(clippy::cast_precision_loss)]
    let available_width = request.gaps.inner_h.mul_add(-((cols - 1) as f64), request.usable.width);
    #[allow(clippy::cast_precision_loss)]
    let available_height =
        request.gaps.inner_v.mul_add(-((rows - 1) as f64), request.usable.height);
    if available_width <= 0.0 || available_height <= 0.0 {
        return MinimumResult {
            frames: initial,
            ratios: None,
            status: MinimumStatus::Infeasible(InfeasibleReason::NoUsableSpace),
        };
    }

    let mut min_widths = vec![0.0_f64; cols];
    let mut min_heights = vec![0.0_f64; rows];
    let mut cur_widths = vec![0.0_f64; cols];
    let mut cur_heights = vec![0.0_f64; rows];
    for (index, id) in request.ids.iter().enumerate() {
        let (row, col) = (index / cols, index % cols);
        if row >= rows {
            break;
        }
        if let Some(minimum) = (request.minimum_of)(id) {
            min_widths[col] = min_widths[col].max(minimum.width);
            min_heights[row] = min_heights[row].max(minimum.height);
        }
        if let Some((_, frame)) = initial.get(index) {
            cur_widths[col] = cur_widths[col].max(frame.width);
            cur_heights[row] = cur_heights[row].max(frame.height);
        }
    }

    let widths = distribute(&min_widths, &cur_widths, available_width);
    let heights = distribute(&min_heights, &cur_heights, available_height);
    let (widths, heights) = match (widths, heights) {
        (Ok(w), Ok(h)) => (w, h),
        (Err(reason), _) | (_, Err(reason)) => {
            return MinimumResult {
                frames: initial,
                ratios: None,
                status: MinimumStatus::Infeasible(reason),
            };
        }
    };
    let mut ratios = boundaries(&widths, available_width);
    ratios.extend(boundaries(&heights, available_height));
    let frames = frames_for(request, &ratios);
    adjusted_or_unresolved(request, frames, ratios)
}

/// Reserves every group's minimum, then shares the rest by current size.
fn distribute(
    minimums: &[f64],
    current: &[f64],
    available: f64,
) -> Result<Vec<f64>, InfeasibleReason> {
    let required: f64 = minimums.iter().sum();
    if required > available + VIOLATION_TOLERANCE {
        return Err(InfeasibleReason::RequiredExceedsAvailable { required, available });
    }
    let remaining = (available - required).max(0.0);
    let weights: Vec<f64> =
        current.iter().map(|s| if s.is_finite() { s.max(0.0) } else { 0.0 }).collect();
    let total: f64 = weights.iter().sum();
    #[allow(clippy::cast_precision_loss)]
    let equal = 1.0 / minimums.len() as f64;
    Ok(minimums
        .iter()
        .zip(&weights)
        .map(|(minimum, weight)| {
            minimum
                + remaining
                    * if total > f64::EPSILON {
                        weight / total
                    } else {
                        equal
                    }
        })
        .collect())
}

fn boundaries(sizes: &[f64], available: f64) -> Vec<f64> {
    let mut used = 0.0;
    sizes
        .iter()
        .take(sizes.len().saturating_sub(1))
        .map(|size| {
            used += size;
            (used / available).clamp(0.05, 0.95)
        })
        .collect()
}

// --- Refusal and capability ------------------------------------------------

/// What actually happened after a frame was written. Fed back by the caller;
/// this crate never performs or assumes the write.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FrameObservation {
    /// The window reached the expected frame within [`INFERENCE_TOLERANCE`].
    Honored,
    /// The frame moved toward the target but did not reach it.
    Partial {
        /// The frame the window actually has.
        actual: Rect,
    },
    /// The frame did not change at all.
    Refused {
        /// The frame the window actually has.
        actual: Rect,
    },
    /// The window reports it cannot be resized and the size differs.
    FixedSize {
        /// The frame the window actually has.
        actual: Rect,
    },
}

impl FrameObservation {
    /// The frame the window really has, when it is not the expected one.
    #[must_use]
    pub const fn actual(&self) -> Option<Rect> {
        match self {
            Self::Honored => None,
            Self::Partial { actual } | Self::Refused { actual } | Self::FixedSize { actual } => {
                Some(*actual)
            }
        }
    }
}

fn near(a: &Rect, b: &Rect) -> bool {
    (a.x - b.x).abs() <= INFERENCE_TOLERANCE
        && (a.y - b.y).abs() <= INFERENCE_TOLERANCE
        && (a.width - b.width).abs() <= INFERENCE_TOLERANCE
        && (a.height - b.height).abs() <= INFERENCE_TOLERANCE
}

/// Classifies a write from the frames before, wanted and observed.
///
/// `resizable` is the window's reported capability, if known.
/// Non-finite frames are treated as a refusal that preserves `previous`.
#[must_use]
pub fn observe_write(
    previous: &Rect,
    expected: &Rect,
    actual: &Rect,
    resizable: Option<bool>,
) -> FrameObservation {
    if !actual.is_finite() || !expected.is_finite() {
        return FrameObservation::Refused { actual: *previous };
    }
    if near(expected, actual) {
        return FrameObservation::Honored;
    }
    let size_differs = (expected.width - actual.width).abs() > INFERENCE_TOLERANCE
        || (expected.height - actual.height).abs() > INFERENCE_TOLERANCE;
    if resizable == Some(false) && size_differs {
        FrameObservation::FixedSize { actual: *actual }
    } else if near(previous, actual) {
        FrameObservation::Refused { actual: *actual }
    } else {
        FrameObservation::Partial { actual: *actual }
    }
}

/// Infers a minimum from a window that came out larger than expected.
///
/// Port of legacy `detect_and_update_inferred_minimum`. A reported minimum makes inference
/// unnecessary. The returned size keeps any previously inferred axis the
/// current observation did not exceed.
#[must_use]
pub fn infer_minimum(
    expected: &Rect,
    actual: &Rect,
    reported: Option<Size>,
    existing: Option<Size>,
) -> Option<Size> {
    if reported.is_some() || !expected.is_finite() || !actual.is_finite() {
        return None;
    }
    let wider = actual.width > expected.width + INFERENCE_TOLERANCE;
    let taller = actual.height > expected.height + INFERENCE_TOLERANCE;
    (wider || taller).then(|| {
        Size::new(
            if wider {
                actual.width
            } else {
                existing.map_or(0.0, |s| s.width)
            },
            if taller {
                actual.height
            } else {
                existing.map_or(0.0, |s| s.height)
            },
        )
    })
}

// --- Runtime ratio state ---------------------------------------------------

/// Per-workspace runtime ratio overrides and their reset semantics.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RatioState {
    /// Cumulative (Split), per-node (Dwindle) or layout-specific (Grid) ratios.
    pub split_ratios: Vec<f64>,
    /// User master-ratio override; `None` uses the configured percentage.
    pub master_ratio: Option<f64>,
}

/// Result of a state mutation that may legitimately do nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// State changed; the caller should re-plan.
    Changed,
    /// Nothing changed; do not re-plan or clear anything else.
    Unchanged,
}

impl RatioState {
    /// Switches layout. Re-selecting the active layout is a no-op that keeps
    /// ratios; a real switch clears every runtime override.
    pub fn set_layout(&mut self, current: LayoutType, next: LayoutType) -> Change {
        if current == next {
            return Change::Unchanged;
        }
        self.split_ratios.clear();
        self.master_ratio = None;
        Change::Changed
    }

    /// Balances the workspace: clears every ratio override. It is a no-op only
    /// when there is nothing to clear **and** no window has an inferred minimum
    /// (`any_inferred`); the caller clears inferred minimums on `Changed`.
    pub fn balance(&mut self, any_inferred: bool) -> Change {
        if self.split_ratios.is_empty() && self.master_ratio.is_none() && !any_inferred {
            return Change::Unchanged;
        }
        self.split_ratios.clear();
        self.master_ratio = None;
        Change::Changed
    }

    /// The ratios to lay out `count` windows with. For the split layouts, stale
    /// cumulative boundaries (wrong length after a membership change, or not
    /// strictly increasing) are ignored in favour of an equal split without
    /// mutating the stored value.
    #[must_use]
    pub fn effective_split_ratios(&self, layout: LayoutType, count: usize) -> &[f64] {
        let split = matches!(
            layout,
            LayoutType::Split | LayoutType::SplitHorizontal | LayoutType::SplitVertical
        );
        if split && !has_valid_cumulative_ratios(&self.split_ratios, count) {
            &[]
        } else {
            &self.split_ratios
        }
    }

    /// Adds `delta` to the master ratio (override, else `configured`), clamped
    /// to `0.1..=0.9`. Unchanged when the clamp leaves it where it was or the
    /// delta is not finite.
    pub fn resize_master(&mut self, configured: f64, delta: f64) -> Change {
        let current = self.master_ratio.unwrap_or(configured);
        if !current.is_finite() || !delta.is_finite() {
            return Change::Unchanged;
        }
        let next = clamp_master_ratio(current + delta);
        if (next - current).abs() < f64::EPSILON {
            return Change::Unchanged;
        }
        self.master_ratio = Some(next);
        Change::Changed
    }
}

/// Default ratios used when the first resize happens (legacy
/// `initialize_default_ratios`).
#[must_use]
pub fn default_ratios(layout: LayoutType, count: usize) -> Vec<f64> {
    #[allow(clippy::cast_precision_loss)]
    match layout {
        LayoutType::Split | LayoutType::SplitHorizontal | LayoutType::SplitVertical => {
            (1..count).map(|i| i as f64 / count as f64).collect()
        }
        LayoutType::Dwindle => vec![0.5; count.saturating_sub(1)],
        LayoutType::Grid => match count {
            5 | 7 => vec![1.0 / 3.0],
            10 | 11 => vec![0.25],
            _ => vec![0.5],
        },
        LayoutType::Master | LayoutType::Floating | LayoutType::Monocle => Vec::new(),
    }
}

/// Moves cumulative boundary `index` by `delta` within minimum shares.
///
/// Port of legacy `apply_split_resize_with_minimums`. /// Shrunk windows give up space starting next to the boundary; if all are at
/// their minimum nothing changes. Boundaries stay in `0.05..=0.95` and strictly
/// increasing.
pub fn resize_cumulative(ratios: &mut [f64], index: usize, delta: f64, min_ratios: &[f64]) {
    if index >= ratios.len() || !delta.is_finite() {
        return;
    }
    let mut sizes = Vec::with_capacity(ratios.len() + 1);
    let mut previous = 0.0;
    for ratio in ratios.iter() {
        sizes.push(ratio - previous);
        previous = *ratio;
    }
    sizes.push(1.0 - previous);
    let minimum = |i: usize| min_ratios.get(i).copied().unwrap_or(0.05);

    if delta < 0.0 {
        let needed = -delta;
        let mut taken = 0.0;
        for i in (0..=index).rev() {
            let available = (sizes[i] - minimum(i)).max(0.0);
            if available > 0.0 {
                let take = (needed - taken).min(available);
                sizes[i] -= take;
                taken += take;
                if (taken - needed).abs() < 0.001 {
                    break;
                }
            }
        }
        sizes[index + 1] += taken;
    } else {
        let available = (sizes[index + 1] - minimum(index + 1)).max(0.0);
        let actual = delta.min(available);
        if actual > 0.0 {
            sizes[index + 1] -= actual;
            sizes[index] += actual;
        }
    }

    let mut cumulative = 0.0;
    for (i, ratio) in ratios.iter_mut().enumerate() {
        cumulative += sizes[i];
        *ratio = cumulative.clamp(0.05, 0.95);
    }
    for i in 1..ratios.len() {
        if ratios[i] <= ratios[i - 1] {
            ratios[i] = ratios[i - 1] + 0.05;
        }
    }
}

/// Proposes `ratios[index] + delta` for Dwindle or Grid, if it is safe.
///
/// The value is clamped to `0.1..=0.9` and accepted only if no window would violate its minimum (legacy
/// `apply_dwindle_resize_with_minimums` / grid variant). Returns the new ratios
/// or `None` when blocked or unchanged.
#[must_use]
pub fn guarded_ratio_change<K: Copy>(
    request: &MinimumRequest<'_, K>,
    index: usize,
    delta: f64,
) -> Option<Vec<f64>> {
    if index >= request.ratios.len()
        || !delta.is_finite()
        || !matches!(request.layout, LayoutType::Dwindle | LayoutType::Grid)
    {
        return None;
    }
    let mut proposed = request.ratios.to_vec();
    proposed[index] = (proposed[index] + delta).clamp(0.1, 0.9);
    if proposed == request.ratios {
        return None;
    }
    let frames = frames_for(request, &proposed);
    find_violations(&frames, request.minimum_of).is_empty().then_some(proposed)
}
