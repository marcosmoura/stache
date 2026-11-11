//! The main thread's record of which overlay panels the live-follow owner may
//! re-stack with the fast reorder (private order call on the owner thread).
//!
//! The native host writes it on the main thread: a panel is *eligible* right
//! after it was ordered `above` its target relative to that target (visible),
//! and stops being eligible **before** it is ordered out, closed or ordered in
//! any other way. The owner thread calls the private order only inside
//! [`with_eligible`], which holds the same lock across the call. So once the
//! main thread's [`mark_ineligible`] returned, no fast order is in progress and
//! none can start until the panel is ordered above again: a fast order can
//! never re-order in a panel the main thread ordered out (the private call with
//! the "above" mode would order a hidden window in). The main thread waits at
//! most for one private call already in progress.
//!
//! Keyed by window number (unique per process while the window exists).

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

static ELIGIBLE: Mutex<BTreeMap<u32, u32>> = Mutex::new(BTreeMap::new());

fn lock() -> MutexGuard<'static, BTreeMap<u32, u32>> {
    ELIGIBLE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Main thread: `panel` is visible and ordered directly above `target`.
pub fn mark_eligible(panel: u32, target: u32) { lock().insert(panel, target); }

/// Main thread, **before** ordering `panel` out, closing it or ordering it
/// any other way: no fast order may touch it any more.
pub fn mark_ineligible(panel: u32) { lock().remove(&panel); }

/// Owner thread: runs `call` (the private order) only while `panel` is
/// recorded as eligible above `target`, holding the lock across it. `None`
/// when it is not.
pub fn with_eligible<R>(panel: u32, target: u32, call: impl FnOnce() -> R) -> Option<R> {
    let guard = lock();
    let eligible = guard.get(&panel) == Some(&target);
    let result = eligible.then(call);
    drop(guard);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_panel_recorded_above_its_own_target_is_eligible() {
        // Numbers far from any real window number; the registry is process-wide.
        let (panel, target) = (4_000_000_001, 4_000_000_002);
        assert_eq!(with_eligible(panel, target, || 1), None, "never recorded");
        mark_eligible(panel, target);
        assert_eq!(with_eligible(panel, target, || 1), Some(1));
        assert_eq!(with_eligible(panel, 7, || 1), None, "another target");
        mark_ineligible(panel);
        assert_eq!(with_eligible(panel, target, || 1), None, "ordered out");
    }

    #[test]
    fn a_hide_waits_for_a_fast_order_in_progress_and_blocks_later_ones() {
        let (panel, target) = (4_000_000_011, 4_000_000_012);
        mark_eligible(panel, target);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let owner = std::thread::spawn(move || {
            with_eligible(panel, target, || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
        });
        entered_rx.recv().unwrap();
        let hider = std::thread::spawn(move || mark_ineligible(panel));
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(!hider.is_finished(), "the hide waits for the call in progress");
        release_tx.send(()).unwrap();
        assert_eq!(owner.join().unwrap(), Some(()));
        hider.join().unwrap();
        assert_eq!(with_eligible(panel, target, || ()), None);
    }
}
