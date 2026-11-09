//! Monocle: every window receives the full usable frame.

use crate::geometry::Rect;

/// Lays `ids` out inside `usable`.
#[must_use]
pub fn layout<K: Copy>(ids: &[K], usable: &Rect) -> Vec<(K, Rect)> {
    ids.iter().map(|id| (*id, *usable)).collect()
}
