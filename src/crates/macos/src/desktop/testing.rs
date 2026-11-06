//! Deterministic desktop-image double.
//!
//! [`FakeDesktop`] records every snapshot read and set call without touching
//! `AppKit`. Snapshots can be replaced between calls, and each display can have
//! a scripted set outcome. Later slices use it as the default backend in
//! automated tests.

// The fake holds its in-memory state mutex while assembling one result. There
// is no contention to reduce, and splitting the guards would obscure the
// double.
#![allow(clippy::significant_drop_tightening)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::{
    DesktopBackend, DesktopError, DesktopOperation, DisplayEntry, DisplayId, DisplaySnapshot,
    GuardedDesktopBackend,
};

/// One recorded [`FakeDesktop::set_image`] call.
#[derive(Clone, Debug, PartialEq)]
pub struct FakeSetCall {
    /// The display the caller addressed.
    pub display: DisplayId,
    /// The snapshot entry the caller prepared for.
    pub expected: DisplayEntry,
    /// The image path the caller passed.
    pub image: PathBuf,
}

#[derive(Default)]
struct FakeDesktopState {
    snapshot: DisplaySnapshot,
    outcomes: BTreeMap<DisplayId, Result<(), DesktopError>>,
    calls: Vec<FakeSetCall>,
    snapshot_reads: usize,
    guarded_snapshot_reads: usize,
    guarded_calls: usize,
}

/// A scriptable [`DesktopBackend`] with no native dependency.
#[derive(Clone, Default)]
pub struct FakeDesktop {
    state: Arc<Mutex<FakeDesktopState>>,
}

impl FakeDesktop {
    /// Creates a fake with an empty snapshot and no scripted failures.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Creates a fake that reports `snapshot`.
    #[must_use]
    pub fn with_snapshot(snapshot: DisplaySnapshot) -> Self {
        let fake = Self::default();
        fake.set_snapshot(snapshot);
        fake
    }

    /// Replaces the snapshot returned by later calls.
    pub fn set_snapshot(&self, snapshot: DisplaySnapshot) { lock(&self.state).snapshot = snapshot; }

    /// Scripts the outcome of the next and later set calls for one display.
    pub fn set_outcome(&self, display: DisplayId, outcome: Result<(), DesktopError>) {
        lock(&self.state).outcomes.insert(display, outcome);
    }

    /// Removes a scripted outcome so the display succeeds by default.
    pub fn clear_outcome(&self, display: DisplayId) { lock(&self.state).outcomes.remove(&display); }

    /// Returns every recorded set call in order.
    #[must_use]
    pub fn calls(&self) -> Vec<FakeSetCall> { lock(&self.state).calls.clone() }

    /// Returns how many snapshots have been read.
    #[must_use]
    pub fn snapshot_reads(&self) -> usize { lock(&self.state).snapshot_reads }

    /// Returns how many guarded snapshots were admitted and read.
    #[must_use]
    pub fn guarded_snapshot_reads(&self) -> usize { lock(&self.state).guarded_snapshot_reads }

    /// Returns how many guarded set calls were admitted and recorded.
    #[must_use]
    pub fn guarded_calls(&self) -> usize { lock(&self.state).guarded_calls }
}

impl DesktopBackend for FakeDesktop {
    fn snapshot(&self) -> Result<DisplaySnapshot, DesktopError> {
        let mut state = lock(&self.state);
        state.snapshot_reads += 1;
        Ok(state.snapshot.clone())
    }

    fn set_image(
        &self,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &Path,
    ) -> Result<(), DesktopError> {
        let mut state = lock(&self.state);
        state.calls.push(FakeSetCall {
            display,
            expected: *expected,
            image: image.to_path_buf(),
        });
        state.outcomes.get(&display).cloned().unwrap_or(Ok(()))
    }
}

impl GuardedDesktopBackend for FakeDesktop {
    fn snapshot_guarded(
        &self,
        operation: &DesktopOperation,
    ) -> Result<DisplaySnapshot, DesktopError> {
        operation.check("display snapshot")?;
        let mut state = lock(&self.state);
        state.guarded_snapshot_reads += 1;
        Ok(state.snapshot.clone())
    }

    fn set_image_guarded(
        &self,
        operation: &DesktopOperation,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &Path,
    ) -> Result<(), DesktopError> {
        operation.check("desktop image set")?;
        let mut state = lock(&self.state);
        state.calls.push(FakeSetCall {
            display,
            expected: *expected,
            image: image.to_path_buf(),
        });
        state.guarded_calls += 1;
        state.outcomes.get(&display).cloned().unwrap_or(Ok(()))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
