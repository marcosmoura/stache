//! Dwindle: binary space partitioning in a spiral.
//!
//! Port of legacy `layout/dwindle.rs`. Each new window splits the previous
//! window's space. The first split divides width on landscape areas
//! (`width >= height`) and height on portrait areas, then alternates. Ratios are
//! per-node values (not cumulative) clamped to `0.05..=0.95`, default `0.5`.

use super::{Gaps, helpers};
use crate::geometry::Rect;

/// Whether the zero-based dwindle split `split_index` divides width.
///
/// Shared with minimum-size solving so both agree on the axis.
#[must_use]
pub const fn is_split_horizontal(split_index: usize, is_landscape: bool) -> bool {
    if is_landscape {
        split_index.is_multiple_of(2)
    } else {
        !split_index.is_multiple_of(2)
    }
}

/// The per-node ratio for split `index`, defaulting to `0.5`.
fn node_ratio(ratios: &[f64], index: usize) -> f64 {
    ratios
        .get(index)
        .copied()
        .filter(|ratio| ratio.is_finite())
        .map_or(0.5, |ratio| ratio.clamp(0.05, 0.95))
}

/// Lays `ids` out inside `usable`.
#[must_use]
pub fn layout<K: Copy>(ids: &[K], usable: &Rect, gaps: &Gaps, ratios: &[f64]) -> Vec<(K, Rect)> {
    let Some(first) = ids.first() else {
        return Vec::new();
    };
    if ids.len() == 1 {
        return vec![(*first, *usable)];
    }

    let landscape = helpers::is_landscape(usable);
    let mut frames: Vec<Rect> = Vec::with_capacity(ids.len());
    frames.push(*usable);

    for i in 1..ids.len() {
        let parent = frames[i - 1];
        let ratio = node_ratio(ratios, i - 1);
        let (first_half, second_half) = if is_split_horizontal(i - 1, landscape) {
            helpers::split_horizontal(&parent, ratio, gaps.inner_h)
        } else {
            helpers::split_vertical(&parent, ratio, gaps.inner_v)
        };
        frames[i - 1] = first_half;
        frames.push(second_half);
    }

    ids.iter().copied().zip(frames).collect()
}
