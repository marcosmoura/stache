//! The app projection of Tiling's published state into the Bar view, and the
//! Bar-only exact focus click.
//!
//! Tiling publishes its display DTO (`tilingState` v1, see
//! [`stache_tiling::TilingPublishedState`]) through the SDK published-state
//! channel. Bar declares Tiling in `observes` but never imports its crate, and
//! Tiling never imports Bar: this app file, which may depend on both, observes
//! the channel and projects one concrete [`TilingStripView`] into
//! [`BarView::tiling`](crate::bar_bridge::BarView::tiling).
//!
//! # Field semantics
//!
//! * `null` - unresolved: only until the first observation resolves. A Tiling
//!   that is enabled, not paused and not started yet is unresolved; once
//!   resolved it never returns to `null`.
//! * `{ kind: "loading" }` - the module is running, healthy and not paused,
//!   and its current generation has not published yet.
//! * `{ kind: "available", .. }` - a current, decodable `tilingState` v1 of the
//!   accepted generation while the registry reports the module running, not
//!   paused and enabled, and the actor reports healthy.
//! * `{ kind: "absent" }` - everything else: not registered, disabled, paused,
//!   stopping, failed, quarantined, unavailable, unhealthy (a caught actor
//!   panic), every tombstone, a schema or version mismatch and a malformed
//!   payload. A stale provider is never presented as available.
//!
//! `available` carries every workspace in published order (the UI sorts), the
//! apps of the globally focused workspace that are not minimized, not hidden
//! and the active tab of their group (published order), the focused window
//! looked up among all published windows, and the rendered generation and
//! sequence the click echoes back.
//!
//! # Observation
//!
//! Exactly like the Keep Awake projection: reads sample the lock-free state
//! cell, the registry snapshot and the actor's health cell, in memory only. A
//! late write or tombstone from an older generation and a backwards sequence
//! are ignored; a new generation resets the sequence. The projection owns a
//! change counter (`version`) that never resets, because the SDK sequence
//! restarts per generation; the bridge adds it to Bar's sequence.
//!
//! There is **no timer**. One worker, started when Bar first runs (a disabled
//! Bar owns none) and joined at shutdown, subscribes first and then waits for
//! a cell change with no timeout; a change, the registry closing or shutdown
//! cancellation are its only wake-ups. Registry lifecycle changes reach the
//! view through the host status sink ([`BarBridge::host_changed`]). A caught
//! actor panic does not move the cell; it is applied at the next re-sample of
//! the view (any Bar change, such as the next CPU sample).
//!
//! # The click
//!
//! [`BarBridge::tiling_action`] authorizes the actual calling window like every
//! Bar command, rejects malformed decimals, checks Tiling's availability
//! (never enabling or resuming it), revalidates the rendered generation and
//! sequence against the current published state, then hands the exact target
//! to Tiling's own typed focus path ([`TilingFocus`]), which resolves the
//! opaque key against the live runtime snapshot and revalidates at issuance.
//!
//! [`BarBridge::host_changed`]: crate::bar_bridge::BarBridge::host_changed
//! [`BarBridge::tiling_action`]: crate::bar_bridge::BarBridge::tiling_action

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use specta::Type;
use stache_sdk::{
    CancellationToken, ModuleId, ModuleSnapshot, ModuleStatus, Registry, StateEnvelope,
    StateObserver, StateReading, StateUnavailableReason, StateWait,
};
use stache_tiling::ipc::{ExactFocus, ExactFocusOutcome, FocusUnavailable, TilingFocus};
use stache_tiling::{TilingConfig, TilingPublishedState};

/// The observed provider.
pub const TILING: ModuleId = TilingConfig::ID;

/// The longest the worker is joined at shutdown.
pub const WATCH_JOIN: Duration = Duration::from_secs(2);

/// One workspace of the strip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct StripWorkspace {
    /// The configured name.
    pub name: String,
    /// The globally focused workspace.
    pub focused: bool,
    /// It has no window and Tiling cannot show it as an empty desktop, so a
    /// focus request is refused.
    pub inactive: bool,
}

/// The last focus request Tiling refused because its workspace was inactive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct StripRefusedFocus {
    /// The workspace name.
    pub workspace: String,
    /// Increases by one on every refusal of a Tiling generation.
    pub serial: u32,
}

/// One application window of the strip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct StripApp {
    /// The opaque per-generation window key (a decimal string); the click
    /// echoes it back.
    pub key: String,
    /// The owning application's name (empty when unknown).
    pub app_name: String,
    /// The window title, unmodified.
    pub title: String,
}

/// What the Bar strip renders for Tiling.
#[allow(
    clippy::large_enum_variant,
    reason = "a view value built once per publication and serialized to the UI"
)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TilingStripView {
    /// No usable Tiling state: the empty slot.
    Absent,
    /// Tiling runs and has not published its first state yet.
    Loading,
    /// The current published state.
    #[serde(rename_all = "camelCase")]
    Available {
        /// The Tiling runtime generation, as a decimal string.
        generation: String,
        /// The published sequence within that generation, as a decimal
        /// string.
        sequence: String,
        /// Every workspace, in Tiling's order.
        workspaces: Vec<StripWorkspace>,
        /// The visible, active-tab windows of the focused workspace.
        apps: Vec<StripApp>,
        /// The focused window, from any workspace.
        focused_app: Option<StripApp>,
        /// The focused workspace name.
        focused_workspace: Option<String>,
        /// The last focus request refused because its workspace was inactive.
        refused_focus: Option<StripRefusedFocus>,
    },
}

/// The registry and runtime facts the projection combines with the published
/// state.
#[allow(
    clippy::struct_excessive_bools,
    reason = "five independent registry and runtime facts"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostFacts {
    /// The registry status is `Running`.
    pub running: bool,
    /// The accepted configuration enables the module.
    pub enabled: bool,
    /// The session pause flag.
    pub paused: bool,
    /// The module is enabled, not paused and not started yet.
    pub expects_start: bool,
    /// The live actor reports healthy.
    pub healthy: bool,
}

impl HostFacts {
    /// Reads the facts from one published registry snapshot and the actor's
    /// health.
    #[must_use]
    pub fn of(snapshot: &ModuleSnapshot, healthy: bool) -> Self {
        Self {
            running: snapshot.status == ModuleStatus::Running,
            enabled: snapshot.enabled,
            paused: snapshot.paused,
            expects_start: snapshot.enabled
                && !snapshot.paused
                && snapshot.status == ModuleStatus::Stopped,
            healthy,
        }
    }

    const fn serving(self) -> bool { self.running && self.enabled && !self.paused && self.healthy }
}

/// One coherent read of the projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TilingView {
    /// `None` until the first observation resolves.
    pub strip: Option<TilingStripView>,
    /// Grows by one for every change of `strip`; never resets.
    pub version: u64,
}

/// The last accepted reading of the current generation.
#[derive(Debug)]
enum Accepted {
    /// Nothing accepted, an absent provider or an end-of-generation tombstone.
    None,
    /// The generation has not published yet.
    Pending,
    /// A published envelope, decoded (`None` when it did not decode).
    Published {
        generation: u64,
        sequence: u64,
        state: Option<TilingPublishedState>,
    },
}

/// The validating state machine. Pure: it takes readings and host facts.
#[derive(Debug)]
pub struct Tracker {
    resolved: bool,
    /// The highest generation accepted.
    generation: u64,
    /// The last accepted sequence of that generation.
    sequence: u64,
    accepted: Accepted,
    strip: Option<TilingStripView>,
    version: u64,
}

impl Default for Tracker {
    fn default() -> Self {
        Self {
            resolved: false,
            generation: 0,
            sequence: 0,
            accepted: Accepted::None,
            strip: None,
            version: 0,
        }
    }
}

impl Tracker {
    /// Applies one reading with the host facts sampled with it and returns the
    /// resulting view.
    pub fn observe(&mut self, reading: &StateReading, host: Option<HostFacts>) -> TilingView {
        match reading {
            StateReading::Available(envelope) => self.accept_envelope(envelope),
            StateReading::Unavailable(tombstone) => match tombstone.generation {
                // A late tombstone of an older generation says nothing about
                // the one already accepted.
                Some(generation) if generation < self.generation => {}
                generation => {
                    if let Some(generation) = generation
                        && generation > self.generation
                    {
                        self.generation = generation;
                        self.sequence = 0;
                    }
                    self.accepted = match tombstone.reason {
                        StateUnavailableReason::NotStarted
                        | StateUnavailableReason::Starting
                        | StateUnavailableReason::NotPublished => Accepted::Pending,
                        StateUnavailableReason::Ended(_) => Accepted::None,
                    };
                }
            },
            StateReading::Absent => self.accepted = Accepted::None,
        }
        let waiting = matches!(
            reading,
            StateReading::Unavailable(tombstone)
                if matches!(
                    tombstone.reason,
                    StateUnavailableReason::NotStarted | StateUnavailableReason::Starting
                )
        ) && host.is_some_and(|host| host.expects_start);
        if !self.resolved && waiting {
            return self.view();
        }
        self.resolved = true;
        let next = match (host, &self.accepted) {
            (Some(host), Accepted::Pending) if host.serving() => TilingStripView::Loading,
            (
                Some(host),
                Accepted::Published {
                    generation,
                    sequence,
                    state: Some(state),
                },
            ) if host.serving() => strip_of(*generation, *sequence, state),
            _ => TilingStripView::Absent,
        };
        if self.strip.as_ref() != Some(&next) {
            self.strip = Some(next);
            self.version += 1;
        }
        self.view()
    }

    fn accept_envelope(&mut self, envelope: &StateEnvelope) {
        let older = envelope.generation < self.generation;
        let backwards = envelope.generation == self.generation && envelope.sequence < self.sequence;
        if older || backwards {
            return;
        }
        self.generation = envelope.generation;
        self.sequence = envelope.sequence;
        self.accepted = Accepted::Published {
            generation: envelope.generation,
            sequence: envelope.sequence,
            // A foreign provider, another schema or version, or a malformed
            // payload is rejected by the codec.
            state: envelope.decode::<TilingPublishedState>(TILING).ok(),
        };
    }

    /// The current view without a new reading.
    #[must_use]
    pub fn view(&self) -> TilingView {
        TilingView {
            strip: self.strip.clone(),
            version: self.version,
        }
    }
}

/// The legacy strip data of one published state.
#[must_use]
pub fn strip_of(generation: u64, sequence: u64, state: &TilingPublishedState) -> TilingStripView {
    let app = |window: &stache_tiling::PublishedWindow| StripApp {
        key: window.key.clone(),
        app_name: window.app_name.clone(),
        title: window.title.clone(),
    };
    let focused_workspace = state.focus.workspace.clone();
    let apps = focused_workspace.as_ref().map_or_else(Vec::new, |focused| {
        state
            .windows
            .iter()
            .filter(|window| {
                window.workspace.as_ref() == Some(focused)
                    && !window.minimized
                    && !window.hidden
                    && window.tab_active
            })
            .map(app)
            .collect()
    });
    let focused_app = state
        .focus
        .window
        .as_ref()
        .and_then(|key| state.windows.iter().find(|window| window.key == *key))
        .map(app);
    TilingStripView::Available {
        generation: generation.to_string(),
        sequence: sequence.to_string(),
        workspaces: state
            .workspaces
            .iter()
            .map(|workspace| StripWorkspace {
                name: workspace.name.clone(),
                focused: workspace.focused,
                inactive: workspace.inactive,
            })
            .collect(),
        apps,
        focused_app,
        focused_workspace,
        refused_focus: state.refused_focus.as_ref().map(|refused| StripRefusedFocus {
            workspace: refused.workspace.clone(),
            serial: refused.serial,
        }),
    }
}

// ------------------------------------------------------------------ click

/// What a click asks Tiling to focus. A closed enum: nothing else
/// deserializes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum BarTilingTarget {
    /// Focus the workspace with this exact name.
    FocusWorkspace {
        /// The workspace name as rendered.
        name: String,
    },
    /// Focus the window with this rendered opaque key.
    FocusWindow {
        /// The window key as rendered (a decimal string).
        window: String,
    },
}

/// One strip click: the target plus the Tiling generation and sequence the
/// strip rendered it from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BarTilingRequest {
    /// The rendered `tiling.generation`, an exact decimal string.
    pub generation: String,
    /// The rendered `tiling.sequence`, an exact decimal string.
    pub sequence: String,
    /// What to focus.
    pub action: BarTilingTarget,
}

/// Why a click was not attempted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum BarTilingUnavailable {
    /// The Bar module is not running.
    BarNotRunning,
    /// Tiling is not registered, not started or not running.
    NotRunning,
    /// Tiling is disabled.
    Disabled,
    /// Tiling is paused for this session.
    Paused,
    /// A Tiling stop or quarantined cleanup is pending.
    CleanupPending,
    /// Host shutdown began.
    ShuttingDown,
    /// Tiling runs but its actor reports failed health.
    Unhealthy,
}

/// The typed answer of `bar_tiling_action`. No outcome produces UI; the strip
/// changes only through the next `BarChanged`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BarTilingOutcome {
    /// Tiling admitted the focus and changed its desired state.
    Applied,
    /// Admitted; the target was already focused.
    NoOp,
    /// The rendered generation, sequence, window or workspace is no longer
    /// current. Nothing was sent (or Tiling refused it as unknown).
    Stale,
    /// Nothing was attempted.
    Unavailable {
        /// Why.
        reason: BarTilingUnavailable,
    },
    /// Another Tiling action is in flight, or its queue is full. Nothing was
    /// sent; a click never queues.
    Busy,
    /// Sent, but the wait for Tiling's record ended first (deadline, actor
    /// failure or shutdown). Never retried.
    Uncertain,
    /// Tiling refused the focus for another reason.
    Refused,
    /// Tiling's mailbox is closed or the admission budget was spent.
    Failed,
    /// The generation or sequence is not an exact decimal `u64`.
    Invalid,
    /// The calling window may not click.
    Unauthorized,
}

impl From<FocusUnavailable> for BarTilingUnavailable {
    fn from(reason: FocusUnavailable) -> Self {
        match reason {
            FocusUnavailable::NotRunning => Self::NotRunning,
            FocusUnavailable::Disabled => Self::Disabled,
            FocusUnavailable::Paused => Self::Paused,
            FocusUnavailable::CleanupPending => Self::CleanupPending,
            FocusUnavailable::ShuttingDown => Self::ShuttingDown,
            FocusUnavailable::Unhealthy => Self::Unhealthy,
        }
    }
}

impl From<ExactFocusOutcome> for BarTilingOutcome {
    fn from(outcome: ExactFocusOutcome) -> Self {
        match outcome {
            ExactFocusOutcome::Applied => Self::Applied,
            ExactFocusOutcome::NoOp => Self::NoOp,
            ExactFocusOutcome::Stale => Self::Stale,
            ExactFocusOutcome::Unavailable(reason) => Self::Unavailable { reason: reason.into() },
            ExactFocusOutcome::Busy => Self::Busy,
            ExactFocusOutcome::Uncertain => Self::Uncertain,
            ExactFocusOutcome::Refused => Self::Refused,
            ExactFocusOutcome::Failed => Self::Failed,
        }
    }
}

/// Parses an exact decimal `u64`: ASCII digits only (no sign, space or
/// exponent), canonical (no leading zero) and in range.
#[must_use]
pub fn exact_decimal(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value = text.parse::<u64>().ok()?;
    (value.to_string() == text).then_some(value)
}

/// Whether the rendered `generation`/`sequence` still describe the current
/// published state: the generation must be the current one and the rendered
/// sequence not ahead of it.
#[must_use]
pub fn rendered_is_current(reading: &StateReading, generation: u64, sequence: u64) -> bool {
    matches!(
        reading,
        StateReading::Available(envelope)
            if envelope.provider == TILING
                && envelope.generation == generation
                && sequence <= envelope.sequence
    )
}

// ------------------------------------------------------------- projection

struct Source {
    observer: StateObserver,
    registry: Weak<Registry>,
}

type Waker = Arc<dyn Fn() + Send + Sync>;

/// The Tiling projection owned by the Bar bridge. Construction is inert.
pub struct TilingProjection {
    tracker: Mutex<Tracker>,
    source: OnceLock<Source>,
    focus: OnceLock<Arc<TilingFocus>>,
    waker: Mutex<Option<Waker>>,
    cancel: CancellationToken,
    worker: Mutex<Option<JoinHandle<()>>>,
    /// How many times the worker woke up. An idle Bar leaves it unchanged.
    wakeups: AtomicU64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl TilingProjection {
    /// An unbound projection: its view is unresolved (`null`).
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            tracker: Mutex::new(Tracker::default()),
            source: OnceLock::new(),
            focus: OnceLock::new(),
            waker: Mutex::new(None),
            cancel: CancellationToken::new(),
            worker: Mutex::new(None),
            wakeups: AtomicU64::new(0),
        })
    }

    /// Sets the callback that tells the bridge the view changed.
    pub fn set_waker(&self, waker: Waker) { *lock(&self.waker) = Some(waker); }

    /// Binds the host registry. Later calls are ignored. Starts nothing.
    pub fn bind(&self, registry: &Arc<Registry>) {
        let _ = self.source.set(Source {
            observer: registry.observe_state(TILING),
            registry: Arc::downgrade(registry),
        });
    }

    /// Installs Tiling's typed focus path (health and clicks). Set once.
    pub fn set_focus(&self, focus: Arc<TilingFocus>) { let _ = self.focus.set(focus); }

    /// Tiling's focus path, when composed.
    #[must_use]
    pub fn focus(&self) -> Option<Arc<TilingFocus>> { self.focus.get().cloned() }

    /// The current published reading (`Absent` while unbound).
    #[must_use]
    pub fn reading(&self) -> StateReading {
        self.source
            .get()
            .map_or(StateReading::Absent, |source| source.observer.read().state)
    }

    fn host(&self, source: &Source) -> Option<HostFacts> {
        let registry = source.registry.upgrade()?;
        let healthy = self.focus.get().is_some_and(|focus| focus.healthy());
        registry
            .snapshots()
            .iter()
            .find(|snapshot| snapshot.id == TILING)
            .map(|snapshot| HostFacts::of(snapshot, healthy))
    }

    /// Starts the single push worker if a source is bound and none ran yet. A
    /// disabled Bar never calls this, so it owns no thread.
    pub fn ensure_worker(self: &Arc<Self>) {
        if self.source.get().is_none() || self.cancel.is_cancelled() {
            return;
        }
        let mut worker = lock(&self.worker);
        if worker.is_some() {
            return;
        }
        let projection = Arc::clone(self);
        // On a spawn failure reads still sample; only the push wake-up is
        // missing, and the next Bar change or snapshot carries the newest view.
        *worker = std::thread::Builder::new()
            .name("stache-bar-tiling".to_owned())
            .spawn(move || projection.watch())
            .ok();
    }

    /// Re-samples the sources and returns the coherent view.
    #[must_use]
    pub fn view(&self) -> TilingView {
        let mut tracker = lock(&self.tracker);
        let Some(source) = self.source.get() else {
            return tracker.view();
        };
        let reading = source.observer.read();
        let host = self.host(source);
        tracker.observe(&reading.state, host)
    }

    /// How many times the worker woke up since it started.
    #[must_use]
    pub fn wakeups(&self) -> u64 { self.wakeups.load(Ordering::SeqCst) }

    /// Whether the worker thread exists (tests).
    #[must_use]
    pub fn worker_started(&self) -> bool { lock(&self.worker).is_some() }

    fn watch(&self) {
        let Some(source) = self.source.get() else { return };
        // Subscribe first: any change after this version wakes the wait.
        let mut seen = source.observer.read().version;
        let mut notified = self.view().version;
        loop {
            // No timeout: an overflowing deadline is an unbounded wait that
            // only a change, the registry closing or cancellation ends.
            match source.observer.wait_for_change(seen, Duration::MAX, &self.cancel) {
                StateWait::Changed(snapshot) => seen = snapshot.version,
                StateWait::TimedOut => continue,
                StateWait::Cancelled | StateWait::Closed => return,
            }
            self.wakeups.fetch_add(1, Ordering::SeqCst);
            let view = self.view();
            if view.version != notified {
                notified = view.version;
                let waker = lock(&self.waker).clone();
                if let Some(waker) = waker {
                    waker();
                }
            }
        }
    }

    /// Runs one click after the bridge authorized the caller: availability,
    /// then the rendered generation and sequence, then Tiling's own exact
    /// focus (which resolves the key and revalidates at issuance). Blocks for
    /// at most Tiling's bounded wait.
    #[must_use]
    pub fn click(&self, request: &BarTilingRequest) -> BarTilingOutcome {
        let (Some(generation), Some(sequence)) = (
            exact_decimal(&request.generation),
            exact_decimal(&request.sequence),
        ) else {
            return BarTilingOutcome::Invalid;
        };
        let Some(focus) = self.focus() else {
            return BarTilingOutcome::Unavailable {
                reason: BarTilingUnavailable::NotRunning,
            };
        };
        if let Some(reason) = focus.unavailable() {
            return BarTilingOutcome::Unavailable { reason: reason.into() };
        }
        if !rendered_is_current(&self.reading(), generation, sequence) {
            return BarTilingOutcome::Stale;
        }
        let target = match &request.action {
            BarTilingTarget::FocusWorkspace { name } => ExactFocus::Workspace(name.clone()),
            BarTilingTarget::FocusWindow { window } => ExactFocus::Window(window.clone()),
        };
        focus.focus(&target).into()
    }

    /// Stage 1: wakes the worker so it exits. Never joins.
    pub fn begin_shutdown(&self) { self.cancel.cancel(); }

    /// Stage 3: joins the worker within a bound. `false` keeps it owned.
    #[must_use]
    pub fn finish_shutdown(&self) -> bool {
        self.begin_shutdown();
        let Some(handle) = lock(&self.worker).take() else {
            return true;
        };
        let deadline = Instant::now() + WATCH_JOIN;
        while !handle.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        if handle.is_finished() {
            let _ = handle.join();
            true
        } else {
            *lock(&self.worker) = Some(handle);
            false
        }
    }
}

#[cfg(test)]
#[path = "bar_tiling_tests.rs"]
mod tests;
