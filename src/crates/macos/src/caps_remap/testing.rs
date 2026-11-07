//! Fakes for the Caps remap boundaries. They never run `hidutil`, touch the
//! real mapping, or write real durable state.
//!
//! One [`World`] holds the fake global mapping, journal file and lease, and
//! counts every transition (each read, each durable journal step, each native
//! write). A test can crash the "process" before or after any numbered
//! transition: afterwards every call fails until [`World::restart`], exactly as
//! if the process had died, while the mapping and journal persist.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use stache_sdk::CancellationToken;

use super::engine::CapsRemap;
use super::hidutil::{Hidutil, HidutilError, HidutilErrorKind};
use super::mapping::{Mapping, render_openstep};
use super::storage::{CapsLeases, JournalStorage, LeaseFailure, LeaseGuard, StorageError};

/// Whether a crash happens before or after a transition's effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashWhen {
    /// The transition has no effect.
    Before,
    /// The transition took effect, but its acknowledgement was lost.
    After,
}

/// A scripted `hidutil set` behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetFault {
    /// Refused before any child ran.
    NotRun,
    /// The child exited unsuccessfully without changing anything.
    Exited,
    /// The child timed out after the mapping changed.
    TimedOutApplied {
        /// Whether the child could not be reaped.
        unconfirmed: bool,
    },
    /// The child timed out before the mapping changed.
    TimedOutUnchanged {
        /// Whether the child could not be reaped.
        unconfirmed: bool,
    },
    /// Another writer replaced the whole mapping instead of this write, and the
    /// call reported success.
    Raced(Vec<Mapping>),
}

#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent fault flags of one fake world"
)]
struct State {
    mappings: Vec<Mapping>,
    raw_get: Option<Vec<u8>>,
    journal: Option<Vec<u8>>,
    staged: Option<Vec<u8>>,
    /// Whether the last journal change lacks its directory sync.
    unsynced: bool,
    /// Journal bytes before that unsynced change.
    before_unsynced: Option<Vec<u8>>,
    lease_held: bool,
    lease_replaced: bool,
    step: usize,
    crash: Option<(usize, CrashWhen)>,
    crashed: bool,
    fail: Vec<(&'static str, usize)>,
    set_faults: VecDeque<SetFault>,
    unreaped: usize,
    log: Vec<&'static str>,
    sets: usize,
    gets: usize,
    acquisitions: usize,
}

/// The shared fake world.
#[derive(Clone, Default)]
pub struct World {
    state: Arc<Mutex<State>>,
}

enum Gate {
    Go { crash_after: bool },
    Refused(String),
}

impl World {
    /// A world whose global mapping is `initial`.
    #[must_use]
    pub fn new(initial: Vec<Mapping>) -> Self {
        let world = Self::default();
        world.state().mappings = initial;
        world
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn gate(&self, point: &'static str) -> Gate {
        let mut state = self.state();
        if state.crashed {
            return Gate::Refused(format!("the process crashed before {point}"));
        }
        state.step += 1;
        state.log.push(point);
        if let Some((step, when)) = state.crash
            && step == state.step
        {
            match when {
                CrashWhen::Before => {
                    state.crashed = true;
                    return Gate::Refused(format!("crashed before {point}"));
                }
                CrashWhen::After => return Gate::Go { crash_after: true },
            }
        }
        if let Some(index) = state.fail.iter().position(|(p, _)| *p == point) {
            if state.fail[index].1 <= 1 {
                state.fail.remove(index);
                return Gate::Refused(format!("injected failure at {point}"));
            }
            state.fail[index].1 -= 1;
        }
        drop(state);
        Gate::Go { crash_after: false }
    }

    fn after(&self, crash_after: bool, point: &'static str) -> Result<(), String> {
        if crash_after {
            self.state().crashed = true;
            return Err(format!("crashed after {point}"));
        }
        Ok(())
    }

    /// The fake `hidutil` over this world.
    #[must_use]
    pub fn hidutil(&self) -> Arc<dyn Hidutil> { Arc::new(FakeHidutil { world: self.clone() }) }

    /// The fake journal storage over this world.
    #[must_use]
    pub fn storage(&self) -> Arc<dyn JournalStorage> {
        Arc::new(FakeStorage { world: self.clone() })
    }

    /// The fake lease over this world.
    #[must_use]
    pub fn leases(&self) -> Arc<dyn CapsLeases> { Arc::new(FakeLeases { world: self.clone() }) }

    /// A new owner over this world.
    #[must_use]
    pub fn remap(&self) -> CapsRemap {
        CapsRemap::new(
            Arc::new(FakeHidutil { world: self.clone() }),
            Arc::new(FakeStorage { world: self.clone() }),
            Arc::new(FakeLeases { world: self.clone() }),
        )
    }

    /// Crashes the process at transition number `step` (1-based).
    pub fn crash_at(&self, step: usize, when: CrashWhen) {
        self.state().crash = Some((step, when));
    }

    /// Transitions counted so far.
    #[must_use]
    pub fn steps(&self) -> usize { self.state().step }

    /// Resets the step counter and the crash plan.
    pub fn reset_steps(&self) {
        let mut state = self.state();
        state.step = 0;
        state.crash = None;
    }

    /// Makes the next transition named `point` fail without effect.
    pub fn fail_next(&self, point: &'static str) { self.fail_nth(point, 1); }

    /// Makes the `nth` (1-based) future transition named `point` fail without
    /// effect.
    pub fn fail_nth(&self, point: &'static str, nth: usize) {
        self.state().fail.push((point, nth));
    }

    /// Scripts the next `hidutil set`.
    pub fn script_set(&self, fault: SetFault) { self.state().set_faults.push_back(fault); }

    /// Simulates a relaunch: the process is alive again, its lease is gone and
    /// a rename whose directory sync never happened is lost.
    pub fn restart(&self) {
        let mut state = self.state();
        state.crashed = false;
        state.crash = None;
        state.lease_held = false;
        state.staged = None;
        if std::mem::take(&mut state.unsynced) {
            state.journal = state.before_unsynced.take();
        }
    }

    /// Whether the simulated process is dead.
    #[must_use]
    pub fn crashed(&self) -> bool { self.state().crashed }

    /// Another cooperating process holds the lease.
    pub fn hold_lease(&self, held: bool) { self.state().lease_held = held; }

    /// The lock path or directory was replaced under a holder.
    pub fn replace_lease(&self, replaced: bool) { self.state().lease_replaced = replaced; }

    /// Makes `get` return exactly these bytes.
    pub fn raw_get(&self, bytes: Option<Vec<u8>>) { self.state().raw_get = bytes; }

    /// Earlier `hidutil` children not confirmed gone.
    pub fn set_unreaped(&self, count: usize) { self.state().unreaped = count; }

    /// An external writer replaces the whole mapping.
    pub fn external_mappings(&self, mappings: Vec<Mapping>) { self.state().mappings = mappings; }

    /// The current global mapping.
    #[must_use]
    pub fn mappings(&self) -> Vec<Mapping> { self.state().mappings.clone() }

    /// The durable journal bytes.
    #[must_use]
    pub fn journal(&self) -> Option<Vec<u8>> { self.state().journal.clone() }

    /// Plants journal bytes.
    pub fn plant_journal(&self, bytes: Option<Vec<u8>>) { self.state().journal = bytes; }

    /// Every transition name, in order.
    #[must_use]
    pub fn log(&self) -> Vec<&'static str> { self.state().log.clone() }

    /// Number of `hidutil set` calls that ran.
    #[must_use]
    pub fn sets(&self) -> usize { self.state().sets }

    /// Number of `hidutil get` calls that ran.
    #[must_use]
    pub fn gets(&self) -> usize { self.state().gets }

    /// Number of successful lease acquisitions.
    #[must_use]
    pub fn acquisitions(&self) -> usize { self.state().acquisitions }
}

struct FakeHidutil {
    world: World,
}

const fn io_error(message: String) -> HidutilError {
    HidutilError {
        kind: HidutilErrorKind::Io,
        message,
        child_unconfirmed: false,
    }
}

impl Hidutil for FakeHidutil {
    fn get(&self, _cancel: &CancellationToken) -> Result<Vec<u8>, HidutilError> {
        let crash_after = match self.world.gate("hidutil.get") {
            Gate::Go { crash_after } => crash_after,
            Gate::Refused(message) => return Err(io_error(message)),
        };
        let bytes = {
            let mut state = self.world.state();
            state.gets += 1;
            state
                .raw_get
                .clone()
                .unwrap_or_else(|| render_openstep(&state.mappings).into_bytes())
        };
        self.world.after(crash_after, "hidutil.get").map_err(io_error)?;
        Ok(bytes)
    }

    fn set(&self, payload: &str, _cancel: &CancellationToken) -> Result<(), HidutilError> {
        let crash_after = match self.world.gate("hidutil.set") {
            Gate::Go { crash_after } => crash_after,
            Gate::Refused(message) => return Err(io_error(message)),
        };
        let fault = self.world.state().set_faults.pop_front();
        let timed_out = |unconfirmed: bool| HidutilError {
            kind: HidutilErrorKind::TimedOut,
            message: "hidutil exceeded its 5 s deadline".to_owned(),
            child_unconfirmed: unconfirmed,
        };
        if let Some(SetFault::Raced(mappings)) = &fault {
            self.world.state().mappings.clone_from(mappings);
        }
        let apply = !matches!(
            fault,
            Some(
                SetFault::NotRun
                    | SetFault::Exited
                    | SetFault::TimedOutUnchanged { .. }
                    | SetFault::Raced(_)
            )
        );
        if apply {
            let parsed: serde_json::Value =
                serde_json::from_str(payload).map_err(|error| io_error(error.to_string()))?;
            let entries = parsed["UserKeyMapping"]
                .as_array()
                .ok_or_else(|| io_error("payload lacks UserKeyMapping".to_owned()))?;
            let mappings = entries
                .iter()
                .map(|entry| {
                    Some(Mapping {
                        src: entry["HIDKeyboardModifierMappingSrc"].as_u64()?,
                        dst: entry["HIDKeyboardModifierMappingDst"].as_u64()?,
                    })
                })
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| io_error("payload entry is malformed".to_owned()))?;
            let mut state = self.world.state();
            state.mappings = mappings;
            state.sets += 1;
        }
        self.world.after(crash_after, "hidutil.set").map_err(io_error)?;
        match fault {
            None | Some(SetFault::Raced(_)) => Ok(()),
            Some(SetFault::NotRun) => Err(HidutilError::not_run("hidutil did not run")),
            Some(SetFault::Exited) => Err(HidutilError {
                kind: HidutilErrorKind::Exited,
                message: "hidutil exited with status 1".to_owned(),
                child_unconfirmed: false,
            }),
            Some(
                SetFault::TimedOutApplied { unconfirmed }
                | SetFault::TimedOutUnchanged { unconfirmed },
            ) => Err(timed_out(unconfirmed)),
        }
    }

    fn unreaped(&self) -> usize { self.world.state().unreaped }
}

struct FakeStorage {
    world: World,
}

impl FakeStorage {
    fn step(&self, point: &'static str) -> Result<bool, StorageError> {
        match self.world.gate(point) {
            Gate::Go { crash_after } => Ok(crash_after),
            Gate::Refused(message) => Err(StorageError(message)),
        }
    }

    fn done(&self, crash_after: bool, point: &'static str) -> Result<(), StorageError> {
        self.world.after(crash_after, point).map_err(StorageError)
    }
}

impl JournalStorage for FakeStorage {
    fn read(&self) -> Result<Option<Vec<u8>>, StorageError> {
        let crash_after = self.step("journal.read")?;
        let bytes = self.world.state().journal.clone();
        self.done(crash_after, "journal.read")?;
        Ok(bytes)
    }

    fn write(&self, bytes: &[u8]) -> Result<(), StorageError> {
        let crash_after = self.step("journal.create")?;
        self.world.state().staged = Some(Vec::new());
        self.done(crash_after, "journal.create")?;
        let crash_after = self.step("journal.fsync")?;
        self.world.state().staged = Some(bytes.to_vec());
        self.done(crash_after, "journal.fsync")?;
        let crash_after = match self.step("journal.rename") {
            Ok(crash_after) => crash_after,
            Err(error) => {
                self.world.state().staged = None;
                return Err(error);
            }
        };
        {
            let mut guard = self.world.state();
            let state = &mut *guard;
            let staged = state.staged.take();
            state.before_unsynced.clone_from(&state.journal);
            state.journal = staged;
            state.unsynced = true;
            drop(guard);
        }
        self.done(crash_after, "journal.rename")?;
        let crash_after = self.step("journal.fsync_dir")?;
        self.world.state().unsynced = false;
        self.done(crash_after, "journal.fsync_dir")
    }

    fn remove(&self) -> Result<(), StorageError> {
        let crash_after = self.step("journal.unlink")?;
        {
            let mut state = self.world.state();
            state.before_unsynced = state.journal.take();
            state.unsynced = true;
        }
        self.done(crash_after, "journal.unlink")?;
        let crash_after = self.step("journal.fsync_dir")?;
        self.world.state().unsynced = false;
        self.done(crash_after, "journal.fsync_dir")
    }
}

struct FakeLeases {
    world: World,
}

struct FakeGuard {
    world: World,
}

impl Drop for FakeGuard {
    fn drop(&mut self) { self.world.state().lease_held = false; }
}

impl LeaseGuard for FakeGuard {
    fn verify(&self) -> Result<(), LeaseFailure> {
        if self.world.state().lease_replaced {
            return Err(LeaseFailure::Unsafe("the lock path was replaced".to_owned()));
        }
        Ok(())
    }
}

impl CapsLeases for FakeLeases {
    fn acquire(&self, cancel: &CancellationToken) -> Result<Box<dyn LeaseGuard>, LeaseFailure> {
        let crash_after = match self.world.gate("lease.acquire") {
            Gate::Go { crash_after } => crash_after,
            Gate::Refused(message) => return Err(LeaseFailure::Unsafe(message)),
        };
        if cancel.is_cancelled() {
            return Err(LeaseFailure::Cancelled);
        }
        {
            let mut state = self.world.state();
            if state.lease_held {
                return Err(LeaseFailure::Busy);
            }
            state.lease_held = true;
            state.acquisitions += 1;
        }
        if let Err(message) = self.world.after(crash_after, "lease.acquire") {
            return Err(LeaseFailure::Unsafe(message));
        }
        Ok(Box::new(FakeGuard { world: self.world.clone() }))
    }
}
