//! Pure workspace ordering: where new windows go and explicit swaps.
//!
//! The actor keeps the ordered list; these functions only compute changes. Focus
//! may influence where a **new** window is inserted (non-Grid layouts) but never
//! reorders existing windows, so it can never displace a Grid tile.

use crate::config::LayoutType;

/// Where a new window goes in `order`.
///
/// Non-Grid layouts insert after the focused window when it is in `order`
/// (legacy `actor/handlers/window.rs:147-164`) and otherwise append. Grid always
/// appends so an existing set of ten tiles is not displaced.
#[must_use]
pub fn insertion_index<K: PartialEq>(
    order: &[K],
    focused: Option<&K>,
    layout: LayoutType,
) -> usize {
    if layout == LayoutType::Grid {
        return order.len();
    }
    focused
        .and_then(|focused| order.iter().position(|id| id == focused))
        .map_or(order.len(), |index| index + 1)
}

/// Inserts `id` into `order`; returns its index, or `None` when already present.
pub fn insert_window<K: PartialEq>(
    order: &mut Vec<K>,
    id: K,
    focused: Option<&K>,
    layout: LayoutType,
) -> Option<usize> {
    if order.contains(&id) {
        return None;
    }
    let index = insertion_index(order, focused, layout);
    order.insert(index, id);
    Some(index)
}

/// Explicitly swaps two windows; returns whether anything changed.
pub fn swap_windows<K: PartialEq>(order: &mut [K], a: &K, b: &K) -> bool {
    if a == b {
        return false;
    }
    match (
        order.iter().position(|id| id == a),
        order.iter().position(|id| id == b),
    ) {
        (Some(first), Some(second)) => {
            order.swap(first, second);
            true
        }
        _ => false,
    }
}
