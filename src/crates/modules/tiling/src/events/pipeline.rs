//! Admission: classification, FIFO bounds, barriers and overflow scopes.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Instant;

use stache_macos::windows::{
    RuntimeEpoch, WindowChange, WindowEvent, WindowEventKind, WindowInstance,
};
use stache_macos::{ApplicationId, DisplayId};

use super::geometry::{GeometryCoalescer, Record};
use crate::drag::{DRAG_QUEUE_CAPACITY, DragSignal, LostCause};
use crate::state::{Cause, Input, MAX_UNCONFIRMED_APPS, Scope};

/// Semantic messages (create/destroy/focus/lifecycle/topology/config).
pub const SEMANTIC_CAPACITY: usize = 1024;
/// Effect completions; bounded by the effect runner's queue.
pub const COMPLETION_CAPACITY: usize = 256;
/// Start/fault/application-list control messages.
pub const CONTROL_CAPACITY: usize = 16;
/// Drag signals queued before the lane reports a lost observation.
const DRAG_CAPACITY: usize = DRAG_QUEUE_CAPACITY;

/// Explicit admission accounting for every bounded stage.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdmissionStats {
    /// Semantic messages admitted.
    pub semantic_accepted: u64,
    /// Semantic messages refused because the queue was full.
    pub semantic_rejected: u64,
    /// Highest semantic depth seen.
    pub semantic_high_water: usize,
    /// Current semantic depth.
    pub semantic_depth: usize,
    /// Geometry samples recorded as new tokens.
    pub geometry_recorded: u64,
    /// Geometry samples that superseded a pending one.
    pub geometry_coalesced: u64,
    /// Geometry samples refused by the cardinality bound.
    pub geometry_dropped: u64,
    /// Pending samples invalidated by a destroy barrier.
    pub geometry_invalidated: u64,
    /// Pending samples flushed ahead of a membership barrier.
    pub geometry_barrier_flushes: u64,
    /// Geometry batches flushed by a timer.
    pub geometry_timer_flushes: u64,
    /// Current pending geometry tokens.
    pub geometry_pending: usize,
    /// Completions admitted.
    pub completions_accepted: u64,
    /// Completions refused (unreachable while the runner queue is smaller).
    pub completions_rejected: u64,
    /// Control messages refused.
    pub control_rejected: u64,
    /// Display/config values that replaced the single latest slot.
    pub latest_replaced: u64,
    /// Backend sink drops reported.
    pub sink_dropped: u64,
    /// Events from a non-current epoch.
    pub stale_epoch_events: u64,
    /// Sequence gaps proven.
    pub sequence_gaps: u64,
    /// Submissions refused after the mailbox closed.
    pub mailbox_closed: u64,
    /// Drag signals admitted.
    pub drag_accepted: u64,
    /// Drag signals dropped because the bounded lane was full; each drop is
    /// reported to the actor as one lost observation.
    pub drag_dropped: u64,
}

#[derive(Default)]
struct Scopes {
    apps: BTreeMap<ApplicationId, Cause>,
    all: Option<Cause>,
}

impl Scopes {
    fn mark(&mut self, scope: Scope, cause: Cause) {
        match scope {
            Scope::All => self.all = Some(cause),
            Scope::App(app) => {
                if self.apps.len() >= MAX_UNCONFIRMED_APPS && !self.apps.contains_key(&app) {
                    self.all = Some(cause);
                } else {
                    self.apps.insert(app, cause);
                }
            }
        }
    }

    fn is_empty(&self) -> bool { self.apps.is_empty() && self.all.is_none() }

    fn take(&mut self) -> Vec<Input> {
        let mut out: Vec<Input> = std::mem::take(&mut self.apps)
            .into_iter()
            .map(|(app, cause)| Input::Unconfirmed(Scope::App(app), cause))
            .collect();
        if let Some(cause) = self.all.take() {
            out.insert(0, Input::Unconfirmed(Scope::All, cause));
        }
        out
    }
}

const fn event_application(kind: &WindowEventKind) -> Option<ApplicationId> {
    match kind {
        WindowEventKind::Snapshot { snapshot, .. } => Some(snapshot.window.application()),
        WindowEventKind::Window { window, .. } => Some(window.application()),
        WindowEventKind::Application { application, .. }
        | WindowEventKind::EnumerationComplete { application, .. }
        | WindowEventKind::EnumerationUnsettled { application, .. }
        | WindowEventKind::EnumerationAborted { application, .. }
        | WindowEventKind::ObserverOverflow { application } => Some(*application),
        WindowEventKind::CapabilityChanged { .. } => None,
    }
}

/// Which queue an input goes through.
enum Lane {
    Control,
    Completion,
    Latest,
    Semantic,
    Drag,
}

const fn lane(input: &Input) -> Lane {
    match input {
        Input::Start(_)
        | Input::Fault(_)
        | Input::Applications { .. }
        | Input::FrameWrites(_)
        | Input::Relayout => Lane::Control,
        Input::Completion { .. } => Lane::Completion,
        Input::Drag(_) => Lane::Drag,
        Input::Displays(_) | Input::Config { .. } | Input::Reservations(_) => Lane::Latest,
        _ => Lane::Semantic,
    }
}

/// The bounded queues. Everything here is plain data, so it is deterministic
/// and testable without threads or a clock.
pub struct Pipeline {
    semantic_capacity: usize,
    completion_capacity: usize,
    control: VecDeque<Input>,
    completions: VecDeque<Input>,
    semantic: VecDeque<Input>,
    /// Drag signals, in order. Bounded: a full lane is emptied and replaced by
    /// one lost observation, because a gesture with a missing signal is void.
    drag: VecDeque<Input>,
    drag_overflow: bool,
    latest_displays: Option<Input>,
    latest_config: Option<Input>,
    latest_reservations: Option<Input>,
    scopes: Scopes,
    geometry: GeometryCoalescer,
    stats: AdmissionStats,
    epoch: Option<RuntimeEpoch>,
    last_sequence: u64,
}

impl Default for Pipeline {
    fn default() -> Self { Self::new() }
}

impl Pipeline {
    /// A pipeline with the production bounds.
    #[must_use]
    pub fn new() -> Self {
        Self::with_bounds(
            SEMANTIC_CAPACITY,
            COMPLETION_CAPACITY,
            GeometryCoalescer::default(),
        )
    }

    /// A pipeline with explicit bounds (tests shrink them).
    #[must_use]
    pub fn with_bounds(
        semantic_capacity: usize,
        completion_capacity: usize,
        geometry: GeometryCoalescer,
    ) -> Self {
        Self {
            semantic_capacity: semantic_capacity.max(1),
            completion_capacity: completion_capacity.max(1),
            control: VecDeque::new(),
            completions: VecDeque::new(),
            semantic: VecDeque::new(),
            drag: VecDeque::new(),
            drag_overflow: false,
            latest_displays: None,
            latest_config: None,
            latest_reservations: None,
            scopes: Scopes::default(),
            geometry,
            stats: AdmissionStats::default(),
            epoch: None,
            last_sequence: 0,
        }
    }

    /// The geometry coalescer.
    pub const fn geometry_mut(&mut self) -> &mut GeometryCoalescer { &mut self.geometry }

    /// The geometry coalescer.
    #[must_use]
    pub const fn geometry(&self) -> &GeometryCoalescer { &self.geometry }

    /// Admission accounting with live depths.
    #[must_use]
    pub fn stats(&self) -> AdmissionStats {
        let mut stats = self.stats.clone();
        stats.semantic_depth = self.semantic.len();
        stats.geometry_pending = self.geometry.len();
        stats
    }

    /// Counts a submission refused because the mailbox closed.
    pub const fn note_closed(&mut self) { self.stats.mailbox_closed += 1; }

    /// Whether anything is waiting for the actor.
    #[must_use]
    pub fn has_work(&self) -> bool {
        !self.control.is_empty()
            || !self.scopes.is_empty()
            || !self.completions.is_empty()
            || !self.drag.is_empty()
            || self.drag_overflow
            || self.latest_displays.is_some()
            || self.latest_config.is_some()
            || self.latest_reservations.is_some()
            || !self.semantic.is_empty()
    }

    fn push_semantic(&mut self, input: Input, overflow: Option<Scope>, cause: Cause) -> bool {
        if self.semantic.len() >= self.semantic_capacity {
            self.stats.semantic_rejected += 1;
            if let Some(scope) = overflow {
                self.scopes.mark(scope, cause);
            }
            return false;
        }
        self.semantic.push_back(input);
        self.stats.semantic_accepted += 1;
        self.stats.semantic_high_water = self.stats.semantic_high_water.max(self.semantic.len());
        true
    }

    /// Admits a non-window input. Control, completion and latest-value lanes
    /// never grow past their bound; a refusal is counted and, where state could
    /// diverge, marks a scope unconfirmed.
    pub fn admit_input(&mut self, input: Input) {
        match lane(&input) {
            Lane::Control => {
                if let Input::Start(stamp) = &input {
                    self.epoch = Some(stamp.epoch);
                    self.last_sequence = 0;
                }
                if self.control.len() >= CONTROL_CAPACITY {
                    self.stats.control_rejected += 1;
                    self.scopes.mark(Scope::All, Cause::SemanticOverflow);
                } else {
                    self.control.push_back(input);
                }
            }
            Lane::Completion => {
                if self.completions.len() >= self.completion_capacity {
                    self.stats.completions_rejected += 1;
                    self.scopes.mark(Scope::All, Cause::EffectRejected);
                } else {
                    self.completions.push_back(input);
                    self.stats.completions_accepted += 1;
                }
            }
            Lane::Latest => {
                // Full-state values: when the FIFO is full the newest one
                // replaces a single slot, so nothing is lost semantically.
                if self.semantic.len() < self.semantic_capacity {
                    self.push_semantic(input, None, Cause::SemanticOverflow);
                } else {
                    let slot = match input {
                        Input::Displays(_) => &mut self.latest_displays,
                        Input::Reservations(_) => &mut self.latest_reservations,
                        _ => &mut self.latest_config,
                    };
                    if slot.replace(input).is_some() {
                        self.stats.latest_replaced += 1;
                    }
                }
            }
            Lane::Drag => self.admit_drag(input),
            Lane::Semantic => {
                let scope = match &input {
                    Input::Lifecycle { .. } => Some(Scope::All),
                    _ => None,
                };
                self.push_semantic(input, scope, Cause::SemanticOverflow);
            }
        }
    }

    fn admit_drag(&mut self, input: Input) {
        if self.drag.len() >= DRAG_CAPACITY {
            // Drop the new signal and void everything queued: a down or up is
            // missing, so none of it can be trusted. The actor sees one lost
            // observation. Nothing is invented or replayed.
            self.stats.drag_dropped += 1;
            self.drag.clear();
            self.drag_overflow = true;
        } else {
            self.drag.push_back(input);
            self.stats.drag_accepted += 1;
        }
    }

    /// Reports events the backend sink dropped.
    pub fn admit_sink_drop(&mut self, dropped: u64) {
        if dropped > 0 {
            self.stats.sink_dropped += dropped;
            self.scopes.mark(Scope::All, Cause::SinkOverflow);
        }
    }

    /// Admits one backend event. `route` is the display the actor currently
    /// places the window on (geometry only).
    pub fn admit_event(&mut self, event: WindowEvent, route: Option<DisplayId>, now: Instant) {
        if self.epoch != Some(event.epoch) {
            self.stats.stale_epoch_events += 1;
            return;
        }
        if event.sequence != self.last_sequence + 1 {
            // A hole proves the backend dropped something between the two.
            self.stats.sequence_gaps += 1;
            self.scopes.mark(Scope::All, Cause::SequenceGap);
        }
        self.last_sequence = self.last_sequence.max(event.sequence);
        let application = event_application(&event.kind);
        let scope = application.map(Scope::App);
        match &event.kind {
            WindowEventKind::Window {
                window,
                change: WindowChange::Moved | WindowChange::Resized,
            } => {
                match self.geometry.record(*window, route, now) {
                    Record::New => self.stats.geometry_recorded += 1,
                    Record::Coalesced => self.stats.geometry_coalesced += 1,
                    Record::Dropped => {
                        // Superseded samples may go; the actor is told to refresh.
                        self.stats.geometry_dropped += 1;
                        self.scopes.mark(Scope::App(window.application()), Cause::GeometryOverflow);
                    }
                }
                return;
            }
            WindowEventKind::Window {
                window,
                change: WindowChange::Destroyed | WindowChange::Suspended,
            } => {
                if self.geometry.remove(*window) {
                    self.stats.geometry_invalidated += 1;
                }
            }
            WindowEventKind::Window {
                window,
                change: WindowChange::Miniaturized | WindowChange::Deminiaturized,
            } => self.barrier_flush(*window),
            WindowEventKind::Snapshot { snapshot, .. } => self.barrier_flush(snapshot.window),
            _ => {}
        }
        self.push_semantic(Input::Window(event), scope, Cause::SemanticOverflow);
    }

    /// Flushes older coalesced geometry for a token ahead of a membership
    /// barrier, preserving accepted order.
    fn barrier_flush(&mut self, token: WindowInstance) {
        if self.geometry.remove(token) {
            self.stats.geometry_barrier_flushes += 1;
            let scope = Some(Scope::App(token.application()));
            self.push_semantic(Input::GeometryBatch(vec![token]), scope, Cause::GeometryOverflow);
        }
    }

    /// Geometry batches whose display timer came due.
    pub fn due_geometry(&mut self, now: Instant) -> Vec<Input> {
        let batches = self.geometry.due(now);
        self.stats.geometry_timer_flushes += batches.len() as u64;
        batches.into_iter().map(Input::GeometryBatch).collect()
    }

    /// Takes up to `limit` inputs: control, overflow scopes, completions, latest
    /// values, then the FIFO.
    pub fn take(&mut self, limit: usize) -> Vec<Input> {
        let mut out: Vec<Input> = Vec::new();
        out.extend(self.control.drain(..));
        out.extend(self.scopes.take());
        if std::mem::take(&mut self.drag_overflow) {
            out.push(Input::Drag(DragSignal::Lost(LostCause::Overflow)));
        }
        out.extend(self.drag.drain(..));
        out.extend(self.completions.drain(..));
        out.extend(self.latest_displays.take());
        out.extend(self.latest_config.take());
        out.extend(self.latest_reservations.take());
        let room = limit.saturating_sub(out.len());
        let count = room.min(self.semantic.len());
        out.extend(self.semantic.drain(..count));
        out
    }

    /// Tokens currently pending (diagnostic).
    #[must_use]
    pub fn pending_tokens(&self) -> BTreeSet<WindowInstance> {
        let mut set = BTreeSet::new();
        for item in &self.semantic {
            if let Input::GeometryBatch(tokens) = item {
                set.extend(tokens.iter().copied());
            }
        }
        set
    }
}
