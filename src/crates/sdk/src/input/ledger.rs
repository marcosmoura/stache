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
}

impl PressLedger {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        // Preallocated so inserts below the bound never reallocate on the tap.
        Self {
            entries: HashMap::with_capacity(capacity),
            capacity,
        }
    }

    pub(crate) fn is_full(&self) -> bool { self.entries.len() >= self.capacity }

    pub(crate) fn len(&self) -> usize { self.entries.len() }

    pub(crate) fn get_mut(&mut self, stream: PressStream) -> Option<&mut PressEntry> {
        self.entries.get_mut(&stream)
    }

    pub(crate) fn remove(&mut self, stream: PressStream) -> Option<PressEntry> {
        self.entries.remove(&stream)
    }

    /// Inserts a new press. The caller checked [`Self::is_full`] first.
    pub(crate) fn insert(&mut self, entry: PressEntry) -> bool {
        if self.entries.len() >= self.capacity && !self.entries.contains_key(&entry.press.stream) {
            return false;
        }
        self.entries.insert(entry.press.stream, entry);
        true
    }

    pub(crate) fn drain(&mut self) -> Vec<PressEntry> {
        self.entries.drain().map(|(_, entry)| entry).collect()
    }
}
