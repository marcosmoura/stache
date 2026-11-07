//! The service-owned physical-press ledger.

use std::collections::HashMap;
use std::sync::Arc;

use super::event::{PressId, PressStream};
use super::subscribers::HandlerEntry;

/// Hard bound on tracked presses. Exhaustion fails open; nothing is evicted.
pub const PRESS_LEDGER_CAPACITY: usize = 1_024;

/// Who owns a press after its first down.
pub(crate) enum Owned {
    /// The down passed; it can never acquire a suppressor mid-press.
    Passed,
    /// The down was suppressed by this handler. If the handler goes inert the
    /// entry stays as a tombstone that still consumes repeat and up.
    Suppressed(Arc<HandlerEntry>),
}

pub(crate) struct PressEntry {
    pub(crate) press: PressId,
    pub(crate) owner: Owned,
    pub(crate) state: u64,
}

pub(crate) struct PressLedger {
    entries: HashMap<PressStream, PressEntry>,
    capacity: usize,
    /// Entries owned by [`Owned::Suppressed`]; kept in step on every mutation.
    suppressed: usize,
}

impl PressLedger {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        // Preallocated so inserts below the bound never reallocate on the tap.
        Self {
            entries: HashMap::with_capacity(capacity),
            capacity,
            suppressed: 0,
        }
    }

    /// Entries whose press was swallowed and still owes a repeat or up.
    pub(crate) const fn suppressed(&self) -> usize { self.suppressed }

    pub(crate) fn is_full(&self) -> bool { self.entries.len() >= self.capacity }

    pub(crate) fn len(&self) -> usize { self.entries.len() }

    pub(crate) fn get_mut(&mut self, stream: PressStream) -> Option<&mut PressEntry> {
        self.entries.get_mut(&stream)
    }

    pub(crate) fn remove(&mut self, stream: PressStream) -> Option<PressEntry> {
        let removed = self.entries.remove(&stream);
        if matches!(
            removed,
            Some(PressEntry {
                owner: Owned::Suppressed(_),
                ..
            })
        ) {
            self.suppressed -= 1;
        }
        removed
    }

    /// Inserts a new press. The caller checked [`Self::is_full`] first.
    pub(crate) fn insert(&mut self, entry: PressEntry) -> bool {
        if self.entries.len() >= self.capacity && !self.entries.contains_key(&entry.press.stream) {
            return false;
        }
        if matches!(entry.owner, Owned::Suppressed(_)) {
            self.suppressed += 1;
        }
        if let Some(old) = self.entries.insert(entry.press.stream, entry)
            && matches!(old.owner, Owned::Suppressed(_))
        {
            self.suppressed -= 1;
        }
        true
    }

    pub(crate) fn drain(&mut self) -> Vec<PressEntry> {
        self.suppressed = 0;
        self.entries.drain().map(|(_, entry)| entry).collect()
    }
}
