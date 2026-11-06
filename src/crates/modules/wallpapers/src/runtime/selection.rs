//! Stable pool identity and per-display sequential cursors.
//!
//! A pool entry is identified by its path plus the occurrence ordinal of that
//! path in the accepted order. Reordering therefore keeps a display's cursor on
//! the same file, while removing that entry falls back deterministically to
//! pool position zero. Duplicates remain distinct entries.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use stache_macos::DisplayId;

use crate::sources::SourcePool;

/// One distinct occurrence of a file in the accepted pool.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PoolIdentity {
    path: PathBuf,
    ordinal: u32,
}

/// One accepted pool position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PoolEntry {
    pub(crate) identity: PoolIdentity,
    pub(crate) path: PathBuf,
}

/// An owned, frozen copy of one accepted pool.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PoolSnapshot {
    entries: Vec<PoolEntry>,
}

impl PoolSnapshot {
    /// Freezes a resolved pool without reading disk again.
    #[must_use]
    pub(crate) fn from_pool(pool: &SourcePool) -> Self {
        let mut occurrences: BTreeMap<&Path, u32> = BTreeMap::new();
        let entries = pool
            .images()
            .iter()
            .map(|path| {
                let ordinal = occurrences.entry(path.as_path()).or_default();
                let identity = PoolIdentity {
                    path: path.clone(),
                    ordinal: *ordinal,
                };
                *ordinal += 1;
                PoolEntry { identity, path: path.clone() }
            })
            .collect();
        Self { entries }
    }

    /// Number of pool entries.
    #[must_use]
    pub(crate) const fn len(&self) -> usize { self.entries.len() }

    /// Whether the pool has no entry.
    #[must_use]
    pub(crate) const fn is_empty(&self) -> bool { self.entries.is_empty() }

    /// The entry at a pool position.
    #[must_use]
    pub(crate) fn entry(&self, index: usize) -> Option<&PoolEntry> { self.entries.get(index) }

    /// The position of one stable identity in the current order.
    #[must_use]
    pub(crate) fn position(&self, identity: &PoolIdentity) -> Option<usize> {
        self.entries.iter().position(|entry| &entry.identity == identity)
    }

    /// The next cursor after `index`, wrapping to zero.
    #[must_use]
    pub(crate) fn successor(&self, index: usize) -> Option<PoolIdentity> {
        if self.entries.is_empty() {
            return None;
        }
        self.entries
            .get((index + 1) % self.entries.len())
            .map(|entry| entry.identity.clone())
    }
}

/// One display's cursor and the successor committed after a successful display.
#[derive(Clone, Debug)]
pub(crate) struct PlannedSelection {
    pub(crate) path: PathBuf,
    pub(crate) successor: Option<PoolIdentity>,
}

/// Per-display sequential cursors.
#[derive(Default)]
pub(crate) struct SelectionState {
    next: BTreeMap<DisplayId, PoolIdentity>,
    accepted_revision: u64,
    session_epoch: u64,
    session_id: Option<u64>,
}

impl SelectionState {
    /// Updates the tag under the selection lock.
    pub(crate) const fn set_tag(&mut self, revision: u64, epoch: u64, session: Option<u64>) {
        self.accepted_revision = revision;
        self.session_epoch = epoch;
        self.session_id = session;
    }

    /// Whether the state still belongs to `revision`, `epoch` and `session`.
    #[must_use]
    pub(crate) fn tagged(&self, revision: u64, epoch: u64, session: Option<u64>) -> bool {
        self.accepted_revision == revision
            && self.session_epoch == epoch
            && self.session_id == session
    }

    /// Plans the next sequential entry for one display.
    ///
    /// A missing cursor starts at zero. A cursor whose identity is still in the
    /// pool keeps that exact position through reorder; a removed identity
    /// starts again at zero.
    #[must_use]
    pub(crate) fn plan_sequential(
        &self,
        display: DisplayId,
        pool: &PoolSnapshot,
    ) -> Option<PlannedSelection> {
        let index = self
            .next
            .get(&display)
            .and_then(|identity| pool.position(identity))
            .unwrap_or(0);
        let entry = pool.entry(index)?;
        Some(PlannedSelection {
            path: entry.path.clone(),
            successor: pool.successor(index),
        })
    }

    /// Plans one explicit collection position for every addressed display.
    #[must_use]
    pub(crate) fn plan_collection(index: usize, pool: &PoolSnapshot) -> Option<PlannedSelection> {
        let entry = pool.entry(index)?;
        Some(PlannedSelection {
            path: entry.path.clone(),
            successor: pool.successor(index),
        })
    }

    /// Records the successor of an applied sequential/collection display.
    pub(crate) fn commit(&mut self, display: DisplayId, successor: Option<PoolIdentity>) {
        match successor {
            Some(successor) => {
                self.next.insert(display, successor);
            }
            None => {
                self.next.remove(&display);
            }
        }
    }

    /// An external file is not a pool entry; the next sequential choice starts
    /// at zero.
    pub(crate) fn note_external(&mut self, display: DisplayId) { self.next.remove(&display); }

    /// Drops state for displays that are no longer connected.
    pub(crate) fn prune(&mut self, active: &[DisplayId]) {
        self.next.retain(|display, _| active.contains(display));
    }

    /// Test-only inspection of one display's cursor.
    #[cfg(test)]
    pub(crate) fn cursor(&self, display: DisplayId) -> Option<&PoolIdentity> {
        self.next.get(&display)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(names: &[&str]) -> PoolSnapshot {
        PoolSnapshot::from_pool(&SourcePool::from_images_for_test(
            names.iter().map(PathBuf::from).collect(),
        ))
    }

    #[test]
    fn a_new_display_starts_at_zero_and_then_advances() {
        let pool = pool(&["a.png", "b.png", "c.png"]);
        let display = DisplayId::new(1);
        let state = SelectionState::default();
        let first = state.plan_sequential(display, &pool).expect("first");
        assert_eq!(first.path, PathBuf::from("a.png"));
        let mut state = state;
        state.commit(display, first.successor);
        let second = state.plan_sequential(display, &pool).expect("second");
        assert_eq!(second.path, PathBuf::from("b.png"));
        state.commit(display, second.successor);
        let third = state.plan_sequential(display, &pool).expect("third");
        assert_eq!(third.path, PathBuf::from("c.png"));
        state.commit(display, third.successor);
        let wrapped = state.plan_sequential(display, &pool).expect("wrap");
        assert_eq!(wrapped.path, PathBuf::from("a.png"));
    }

    #[test]
    fn duplicate_occurrences_are_distinct_cursors() {
        let pool = pool(&["a.png", "a.png", "b.png"]);
        let display = DisplayId::new(1);
        let mut state = SelectionState::default();
        let first = state.plan_sequential(display, &pool).expect("first");
        assert_eq!(first.path, PathBuf::from("a.png"));
        state.commit(display, first.successor.clone());
        let second = state.plan_sequential(display, &pool).expect("second");
        assert_eq!(second.path, PathBuf::from("a.png"));
        assert_ne!(first.successor, second.successor);
    }

    #[test]
    fn a_reorder_keeps_the_cursor_on_the_same_identity() {
        let original = pool(&["a.png", "b.png", "c.png"]);
        let display = DisplayId::new(1);
        let mut state = SelectionState::default();
        let first = state.plan_sequential(display, &original).expect("first");
        state.commit(display, first.successor);
        let reordered = pool(&["c.png", "a.png", "b.png"]);
        let next = state.plan_sequential(display, &reordered).expect("next");
        assert_eq!(next.path, PathBuf::from("b.png"), "the cursor follows b.png");
    }

    #[test]
    fn a_removed_entry_starts_again_at_zero() {
        let original = pool(&["a.png", "b.png"]);
        let display = DisplayId::new(1);
        let mut state = SelectionState::default();
        let first = state.plan_sequential(display, &original).expect("first");
        state.commit(display, first.successor);
        let shortened = pool(&["a.png"]);
        let next = state.plan_sequential(display, &shortened).expect("next");
        assert_eq!(next.path, PathBuf::from("a.png"));
    }

    #[test]
    fn an_external_file_resets_the_next_sequential_choice() {
        let pool = pool(&["a.png", "b.png"]);
        let display = DisplayId::new(1);
        let mut state = SelectionState::default();
        let first = state.plan_sequential(display, &pool).expect("first");
        state.commit(display, first.successor);
        state.note_external(display);
        let next = state.plan_sequential(display, &pool).expect("next");
        assert_eq!(next.path, PathBuf::from("a.png"));
    }

    #[test]
    fn disconnected_display_state_is_pruned() {
        let pool = pool(&["a.png"]);
        let display = DisplayId::new(1);
        let mut state = SelectionState::default();
        let first = state.plan_sequential(display, &pool).expect("first");
        state.commit(display, first.successor);
        state.prune(&[DisplayId::new(2)]);
        assert!(state.cursor(display).is_none());
    }
}
