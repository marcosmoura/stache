//! The `Send + Sync` control: one pending batch, latest-wins merge, tickets.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use stache_sdk::MainThreadExecutor;

use super::{
    ANIMATION_DIAGNOSTIC, AppearanceChange, ApplySummary, BorderAppearance, HideReason,
    MAX_OVERLAYS, OrderingMode, OverlayBackend, OverlayBatch, OverlayCapabilities, OverlayCommand,
    OverlayDiagnostics, OverlayHandle, OverlayKind, OverlayRect, OverlayWork, SpaceCapability,
    Transition,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Why an acknowledgement is not a result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AckError {
    /// The bounded wait elapsed.
    TimedOut,
    /// The caller is on the main thread and the result is not ready; main never waits.
    WouldBlockMain,
    /// The executor refused the job.
    DispatchFailed,
    /// The job was dropped without running.
    Abandoned,
    /// `release_all` discarded the batch.
    Superseded,
}

/// Result of an applied batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApplyAck {
    /// What the main thread did.
    pub summary: ApplySummary,
}

/// Result of `release_all`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReleaseAck {
    /// Overlays ordered out and closed by the backend: live plus pooled panels.
    /// Reported, never compared with a ledger (the pool is not in any ledger).
    pub closed: usize,
    /// Handles the control held when the release was requested: reserved
    /// (shown, hidden-and-kept or still pending) and not already being released.
    /// This is the number a handle ledger is compared with.
    pub handles: usize,
}

struct Slot<T> {
    value: Mutex<Option<Result<T, AckError>>>,
    ready: Condvar,
}

impl<T: Copy> Slot<T> {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            value: Mutex::new(None),
            ready: Condvar::new(),
        })
    }

    fn done(result: Result<T, AckError>) -> Arc<Self> {
        Arc::new(Self {
            value: Mutex::new(Some(result)),
            ready: Condvar::new(),
        })
    }

    fn fulfill(&self, result: Result<T, AckError>) {
        let mut value = lock(&self.value);
        if value.is_none() {
            *value = Some(result);
            drop(value);
            self.ready.notify_all();
        }
    }
}

/// Fulfills a slot with `Abandoned` if the job is dropped without finishing.
struct SlotGuard<T: Copy>(Arc<Slot<T>>);

impl<T: Copy> Drop for SlotGuard<T> {
    fn drop(&mut self) { self.0.fulfill(Err(AckError::Abandoned)); }
}

/// A handle on one batch's acknowledgement. Dropping it is harmless.
pub struct OverlayTicket<T: Copy> {
    slot: Arc<Slot<T>>,
    executor: Arc<dyn MainThreadExecutor>,
}

impl<T: Copy> Clone for OverlayTicket<T> {
    /// A second handle on the same acknowledgement (both see the same result).
    fn clone(&self) -> Self {
        Self {
            slot: Arc::clone(&self.slot),
            executor: Arc::clone(&self.executor),
        }
    }
}

impl<T: Copy> OverlayTicket<T> {
    /// The result if it is ready, without waiting.
    #[must_use]
    pub fn try_get(&self) -> Option<Result<T, AckError>> { *lock(&self.slot.value) }

    /// Waits at most `deadline` for the result.
    ///
    /// # Errors
    /// [`AckError::WouldBlockMain`] when called on the main thread with the
    /// result not ready (main never waits), [`AckError::TimedOut`] after the
    /// deadline, or the job's own failure.
    pub fn wait(&self, deadline: Duration) -> Result<T, AckError> {
        let mut value = lock(&self.slot.value);
        if let Some(result) = *value {
            return result;
        }
        if self.executor.is_main_thread() {
            return Err(AckError::WouldBlockMain);
        }
        let end = Instant::now() + deadline;
        loop {
            let now = Instant::now();
            if now >= end {
                return Err(AckError::TimedOut);
            }
            let (guard, _) = self
                .slot
                .ready
                .wait_timeout(value, end - now)
                .unwrap_or_else(PoisonError::into_inner);
            value = guard;
            if let Some(result) = *value {
                return result;
            }
        }
    }
}

/// Why a command was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalReason {
    /// A new `Show` would exceed [`MAX_OVERLAYS`].
    CapReached,
    /// The command names a handle the control does not hold.
    UnknownHandle,
    /// The handle was released in this batch and is retired.
    ReleasedHandle,
}

/// One refused command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal {
    /// The handle.
    pub handle: OverlayHandle,
    /// Why.
    pub reason: RefusalReason,
}

/// What `apply` returns at once.
pub struct ApplyReceipt {
    /// Acknowledgement of the batch this call merged into.
    pub ticket: OverlayTicket<ApplyAck>,
    /// Commands refused while merging (counted, never silent).
    pub refused: Vec<Refusal>,
}

/// Monotonic counters, plain data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OverlayCounters {
    /// `Show` commands refused at the cap.
    pub refused_shows: u64,
    /// Other refused commands (unknown or retired handles).
    pub refused_other: u64,
    /// Commands replaced or absorbed by a later command.
    pub superseded: u64,
    /// Merged batches handed to the backend.
    pub batches: u64,
    /// `HideAll` requests accepted.
    pub hide_alls: u64,
    /// `Show` and `Order` commands the main thread dropped because a later
    /// `HideAll` (a newer gate epoch) had been applied or was in the same batch.
    pub stale_dropped: u64,
}

#[derive(Clone, Copy)]
enum Entry {
    Show {
        target: u32,
        frame: OverlayRect,
        appearance: BorderAppearance,
        change: AppearanceChange,
        /// The epoch the `Show` was computed under (kept when later `Move`s and
        /// `Order`s are absorbed: the oldest computation decides).
        epoch: u64,
    },
    Update {
        frame: Option<(OverlayRect, u64)>,
        order: Option<(u32, u64)>,
    },
    Hide,
    Release,
}

impl Entry {
    const fn from_kind(kind: OverlayKind, epoch: u64) -> Self {
        match kind {
            OverlayKind::Show {
                target_cg_id,
                frame,
                appearance,
                change,
            } => Self::Show {
                target: target_cg_id,
                frame,
                appearance,
                change,
                epoch,
            },
            OverlayKind::Move { frame } => Self::Update {
                frame: Some((frame, epoch)),
                order: None,
            },
            OverlayKind::Order { target_cg_id } => Self::Update {
                frame: None,
                order: Some((target_cg_id, epoch)),
            },
            OverlayKind::Hide => Self::Hide,
            OverlayKind::Release => Self::Release,
        }
    }

    /// Merges a later command into this one; false when it was absorbed.
    const fn merge(&mut self, kind: OverlayKind, epoch: u64) -> bool {
        match (&mut *self, kind) {
            (Self::Release, _) => false,
            (_, OverlayKind::Release | OverlayKind::Hide | OverlayKind::Show { .. }) => {
                *self = Self::from_kind(kind, epoch);
                true
            }
            (Self::Hide, _) => false,
            (Self::Show { frame, .. }, OverlayKind::Move { frame: next }) => {
                *frame = next;
                true
            }
            (Self::Show { target, .. }, OverlayKind::Order { target_cg_id }) => {
                *target = target_cg_id;
                true
            }
            (Self::Update { frame, .. }, OverlayKind::Move { frame: next }) => {
                *frame = Some((next, epoch));
                true
            }
            (Self::Update { order, .. }, OverlayKind::Order { target_cg_id }) => {
                *order = Some((target_cg_id, epoch));
                true
            }
        }
    }

    fn commands(self, handle: OverlayHandle) -> Vec<OverlayCommand> {
        let command = |kind, epoch| OverlayCommand { handle, kind, epoch };
        match self {
            Self::Show {
                target,
                frame,
                appearance,
                change,
                epoch,
            } => vec![command(
                OverlayKind::Show {
                    target_cg_id: target,
                    frame,
                    appearance,
                    change,
                },
                epoch,
            )],
            Self::Update { frame, order } => {
                let mut commands = Vec::new();
                if let Some((frame, epoch)) = frame {
                    commands.push(command(OverlayKind::Move { frame }, epoch));
                }
                if let Some((target_cg_id, epoch)) = order {
                    commands.push(command(OverlayKind::Order { target_cg_id }, epoch));
                }
                commands
            }
            Self::Hide => vec![command(OverlayKind::Hide, 0)],
            Self::Release => vec![command(OverlayKind::Release, 0)],
        }
    }
}

struct Pending {
    /// The reason and the highest gate epoch of the `HideAll`s merged so far.
    hide_all: Option<(HideReason, u64)>,
    entries: Vec<(OverlayHandle, Entry)>,
    slot: Arc<Slot<ApplyAck>>,
}

struct State {
    pending: Option<Pending>,
    /// Handles that count against [`MAX_OVERLAYS`]: shown or pending, not yet released.
    reserved: BTreeSet<OverlayHandle>,
    job_queued: bool,
    /// Bumped by `release_all` so an older queued job never takes newer work.
    epoch: u64,
    capabilities: OverlayCapabilities,
    counters: OverlayCounters,
    /// Engine-wide focus cross-fade; read when the job builds its work.
    transition: Option<Transition>,
    /// Whether the last batch that touched animations failed to describe one.
    animation_failed: bool,
    /// The panel window number of each live handle, as the main thread
    /// reported it after a `Show` (read by [`OverlayControl::window_number`]).
    windows: BTreeMap<OverlayHandle, u32>,
}

struct Shared {
    executor: Arc<dyn MainThreadExecutor>,
    backend: Arc<dyn OverlayBackend>,
    state: Mutex<State>,
}

/// The overlay control; clone-free, share it behind an `Arc`.
pub struct OverlayControl {
    shared: Arc<Shared>,
}

impl OverlayControl {
    /// Creates a control. It owns no panel until the first `Show`.
    #[must_use]
    pub fn new(
        executor: Arc<dyn MainThreadExecutor>,
        backend: Arc<dyn OverlayBackend>,
        capabilities: OverlayCapabilities,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                executor,
                backend,
                state: Mutex::new(State {
                    pending: None,
                    reserved: BTreeSet::new(),
                    job_queued: false,
                    epoch: 0,
                    capabilities,
                    counters: OverlayCounters::default(),
                    transition: None,
                    animation_failed: false,
                    windows: BTreeMap::new(),
                }),
            }),
        }
    }

    /// Sets the ordering mode (the native gate's verdict).
    pub fn set_ordering_mode(&self, mode: OrderingMode) {
        lock(&self.shared.state).capabilities.ordering = mode;
    }

    /// Sets the corner radius capability (learned when the lazy resolver resolves).
    pub fn set_radius_capability(&self, capability: super::RadiusCapability) {
        lock(&self.shared.state).capabilities.radius = capability;
    }

    /// Sets the Space capability.
    pub fn set_space_capability(&self, capability: SpaceCapability) {
        lock(&self.shared.state).capabilities.space = capability;
    }

    /// Sets the engine-wide focus cross-fade (`None` or 0 ms is instant).
    ///
    /// Applies to batches that run after this call; it never dispatches work
    /// and never touches a running fade.
    pub fn set_transition(&self, transition: Option<Transition>) {
        lock(&self.shared.state).transition = transition;
    }

    /// The engine-wide cross-fade.
    #[must_use]
    pub fn transition(&self) -> Option<Transition> { lock(&self.shared.state).transition }

    /// The capability facts.
    #[must_use]
    pub fn capabilities(&self) -> OverlayCapabilities { lock(&self.shared.state).capabilities }

    /// One diagnostic per capability gap.
    #[must_use]
    pub fn diagnostics(&self) -> OverlayDiagnostics {
        let state = lock(&self.shared.state);
        OverlayDiagnostics {
            animation: state.animation_failed.then_some(ANIMATION_DIAGNOSTIC),
            ..OverlayDiagnostics::from_capabilities(&state.capabilities)
        }
    }

    /// The counters.
    #[must_use]
    pub fn counters(&self) -> OverlayCounters { lock(&self.shared.state).counters }

    /// The window number of `handle`'s panel, recorded after the main thread
    /// applied its `Show`; `None` before, after its release, or when the host
    /// has none. Never waits for the main thread.
    #[must_use]
    pub fn window_number(&self, handle: OverlayHandle) -> Option<u32> {
        lock(&self.shared.state).windows.get(&handle).copied()
    }

    /// Merges a batch into the pending batch and returns at once.
    ///
    /// Never blocks and never runs native code on the calling thread unless
    /// the executor itself is on the main thread (then the job runs inline).
    #[must_use]
    #[allow(clippy::needless_pass_by_value)] // the batch is consumed by the caller
    pub fn apply(&self, batch: OverlayBatch) -> ApplyReceipt {
        let (slot, refused, dispatch) = {
            let mut state = lock(&self.shared.state);
            let mut refused = Vec::new();
            let mut touched = batch.hide_all.is_some();
            if let Some(reason) = batch.hide_all {
                let pending = pending_mut(&mut state);
                // Epochs only grow; the newest reason is the one reported.
                let epoch = pending.hide_all.map_or(batch.epoch, |(_, kept)| kept.max(batch.epoch));
                pending.hide_all = Some((reason, epoch));
                state.counters.hide_alls += 1;
            }
            for command in &batch.commands {
                touched |= merge_command(&mut state, command, &mut refused);
            }
            let slot = match (&state.pending, touched) {
                (Some(pending), true) => Arc::clone(&pending.slot),
                _ => Slot::done(Ok(ApplyAck {
                    summary: ApplySummary::default(),
                })),
            };
            let dispatch = if state.pending.is_some() && !state.job_queued {
                state.job_queued = true;
                Some(state.epoch)
            } else {
                None
            };
            (slot, refused, dispatch)
        };
        if let Some(epoch) = dispatch {
            self.dispatch(epoch);
        }
        ApplyReceipt {
            ticket: OverlayTicket {
                slot,
                executor: Arc::clone(&self.shared.executor),
            },
            refused,
        }
    }

    fn dispatch(&self, epoch: u64) {
        let shared = Arc::clone(&self.shared);
        let job = Box::new(move || run_job(&shared, epoch));
        if self.shared.executor.execute(job).is_err() {
            let mut state = lock(&self.shared.state);
            if state.epoch == epoch {
                state.job_queued = false;
                if let Some(pending) = state.pending.as_mut() {
                    pending.slot.fulfill(Err(AckError::DispatchFailed));
                    pending.slot = Slot::new();
                }
            }
        }
    }

    /// Discards pending work, then orders out and closes every overlay.
    ///
    /// The only teardown path. The acknowledgement carries how many overlays
    /// were closed.
    #[must_use]
    pub fn release_all(&self) -> OverlayTicket<ReleaseAck> {
        let (handles, superseded) = {
            let mut state = lock(&self.shared.state);
            let releasing = state.pending.as_ref().map_or(0, |pending| {
                pending
                    .entries
                    .iter()
                    .filter(|(handle, entry)| {
                        matches!(entry, Entry::Release) && state.reserved.contains(handle)
                    })
                    .count()
            });
            let handles = state.reserved.len().saturating_sub(releasing);
            state.epoch += 1;
            state.job_queued = false;
            state.reserved.clear();
            state.windows.clear();
            (handles, state.pending.take())
        };
        if let Some(pending) = superseded {
            pending.slot.fulfill(Err(AckError::Superseded));
        }
        let slot = Slot::<ReleaseAck>::new();
        let guard = SlotGuard(Arc::clone(&slot));
        let backend = Arc::clone(&self.shared.backend);
        let job = Box::new(move || {
            let closed = backend.release_all();
            guard.0.fulfill(Ok(ReleaseAck { closed, handles }));
        });
        if self.shared.executor.execute(job).is_err() {
            slot.fulfill(Err(AckError::DispatchFailed));
        }
        OverlayTicket {
            slot,
            executor: Arc::clone(&self.shared.executor),
        }
    }
}

fn pending_mut(state: &mut State) -> &mut Pending {
    state.pending.get_or_insert_with(|| Pending {
        hide_all: None,
        entries: Vec::new(),
        slot: Slot::new(),
    })
}

/// Merges one command; returns whether it entered the pending batch.
fn merge_command(state: &mut State, command: &OverlayCommand, refused: &mut Vec<Refusal>) -> bool {
    let handle = command.handle;
    let is_show = matches!(command.kind, OverlayKind::Show { .. });
    let existing = state.pending.as_ref().and_then(|pending| {
        pending.entries.iter().position(|(entry_handle, _)| *entry_handle == handle)
    });
    if let Some(index) = existing {
        let entry = &mut pending_mut(state).entries[index].1;
        if matches!(entry, Entry::Release) {
            state.counters.refused_other += 1;
            refused.push(Refusal {
                handle,
                reason: RefusalReason::ReleasedHandle,
            });
            return false;
        }
        entry.merge(command.kind, command.epoch);
        state.counters.superseded += 1;
        return true;
    }
    if !state.reserved.contains(&handle) {
        if !is_show {
            state.counters.refused_other += 1;
            refused.push(Refusal {
                handle,
                reason: RefusalReason::UnknownHandle,
            });
            return false;
        }
        if state.reserved.len() >= MAX_OVERLAYS {
            state.counters.refused_shows += 1;
            refused.push(Refusal {
                handle,
                reason: RefusalReason::CapReached,
            });
            return false;
        }
        state.reserved.extend([handle]);
    }
    pending_mut(state)
        .entries
        .push((handle, Entry::from_kind(command.kind, command.epoch)));
    true
}

fn run_job(shared: &Shared, epoch: u64) {
    let (work, slot) = {
        let mut state = lock(&shared.state);
        if state.epoch != epoch {
            return;
        }
        state.job_queued = false;
        let Some(pending) = state.pending.take() else { return };
        let ordering = state.capabilities.ordering;
        let transition = state.transition;
        let mut commands = Vec::new();
        for (handle, entry) in pending.entries {
            if matches!(entry, Entry::Release) {
                state.reserved.remove(&handle);
                state.windows.remove(&handle);
            }
            commands.extend(entry.commands(handle));
        }
        state.counters.batches += 1;
        drop(state);
        (
            OverlayWork {
                ordering,
                transition,
                hide_all: pending.hide_all.map(|(reason, _)| reason),
                hide_epoch: pending.hide_all.map_or(0, |(_, epoch)| epoch),
                commands,
            },
            pending.slot,
        )
    };
    let guard = SlotGuard(slot);
    let report = shared.backend.apply(&work);
    if !report.windows.is_empty() {
        let mut state = lock(&shared.state);
        if state.epoch == epoch {
            for (handle, number) in &report.windows {
                if state.reserved.contains(handle) {
                    state.windows.insert(*handle, *number);
                }
            }
        }
    }
    if !report.dropped.is_empty() {
        let mut state = lock(&shared.state);
        for handle in &report.dropped {
            let queued = state
                .pending
                .as_ref()
                .is_some_and(|pending| pending.entries.iter().any(|(entry, _)| entry == handle));
            if !queued {
                state.reserved.remove(handle);
                state.windows.remove(handle);
            }
        }
    }
    if report.summary.stale_dropped > 0 {
        lock(&shared.state).counters.stale_dropped += u64::from(report.summary.stale_dropped);
    }
    if report.summary.animation_failures > 0 {
        lock(&shared.state).animation_failed = true;
    } else if report.summary.animations_installed > 0 {
        lock(&shared.state).animation_failed = false;
    }
    guard.0.fulfill(Ok(ApplyAck { summary: report.summary }));
}
