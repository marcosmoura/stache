//! The safe window backend seam for the Tiling runtime (parts 04 and 05).
//!
//! Nothing here starts a thread or touches AX until a runtime installs an epoch:
//! the host composes [`LazySystemBackend`], which spawns the owner thread on
//! first use. The trait exposes pure values
//! only: opaque `Copy` window tokens, snapshots, events delivered through a
//! bounded sink and typed outcomes. No AX element, pointer or run-loop object
//! crosses it, and this crate keeps `forbid(unsafe_code)`.
//!
//! Focus is not a second activation path: a caller combines the applications
//! adapter's `ApplicationObservation::activate_exact` with
//! [`WindowAction::Raise`].
//!
//! [`FakeWindowBackend`] is the deterministic backend for the later runtime
//! harness; it records calls and replays scripted outcomes.

use std::sync::Arc;
use std::time::Instant;

use stache_macos::ApplicationId;
use stache_macos::windows::{
    ActionRequest, AdapterId, AdapterOptions, CleanupReport, CloseOutcome, DispatchError,
    EventQueue, FocusOutcome, GenerationCell, HiddenOutcome, HiddenRequest, InstallError,
    InstallRequest, NotIssuedReason, RuntimeEpoch, SnapshotOutcome, SpawnError, StopOutcome,
    TrackOutcome, TrackRequest, WindowActionOutcome, WindowAdapter, WindowInstance,
};
use stache_sdk::CancellationToken;

/// The Tiling AX owner thread name. `MenuAnywhere` keeps `stache-ax-owner`;
/// the adapter rejects that name, so the two never share a serialized AX thread.
pub const TILING_AX_OWNER_THREAD_NAME: &str = "stache-tiling-ax-owner";

/// Why a backend could not be created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendError {
    /// The AX owner thread could not start.
    Spawn(SpawnError),
}

/// What Tiling needs from native window ownership.
///
/// `Send + Sync`; blocking
/// methods wait at most one acknowledgement hop capped by `deadline` and must
/// never be called from the main thread. `submit_action` never blocks.
pub trait WindowBackend: Send + Sync {
    /// The identity tokens minted by this backend carry.
    fn adapter(&self) -> AdapterId;

    /// Whether snapshots carry native tab groups. `false` (the default) means
    /// tab reads are off and every window is an ordinary window.
    fn reads_tabs(&self) -> bool { false }

    /// Starts a runtime epoch whose events leave through `sink` and whose
    /// actions are checked against `gate`.
    ///
    /// # Errors
    ///
    /// [`InstallError`], including a refusal while cleanup is incomplete.
    fn install(
        &self,
        sink: Arc<EventQueue>,
        gate: Arc<GenerationCell>,
        deadline: Option<Instant>,
    ) -> Result<RuntimeEpoch, InstallError>;

    /// Installs observation and starts chunked enumeration for one exact
    /// application instance.
    fn track(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        pid: i32,
        cancel: CancellationToken,
        deadline: Option<Instant>,
    ) -> TrackOutcome;

    /// Re-enumerates a tracked application to reconcile after an overflow.
    fn refresh(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        cancel: CancellationToken,
        deadline: Option<Instant>,
    ) -> TrackOutcome;

    /// Reads the application's real focused window, bounded by the AX element
    /// timeout on the owner thread, and answers which tracked window it is.
    fn focused_window(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> FocusOutcome;

    /// Stops observing one application.
    fn untrack(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> StopOutcome;

    /// Reads a fresh snapshot of a live window. The wait observes `cancel`.
    fn snapshot(
        &self,
        window: WindowInstance,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> SnapshotOutcome;

    /// Resolves a `CGWindowID` to a currently live token, or nothing.
    fn resolve_window_id(
        &self,
        cg_window_id: u32,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> Option<WindowInstance>;

    /// Queues an action without waiting; `reply` runs on the backend's own
    /// thread and must not block.
    ///
    /// # Errors
    ///
    /// [`DispatchError`]: nothing was queued, so nothing can be sent later.
    fn submit_action(
        &self,
        request: ActionRequest,
        reply: Box<dyn FnOnce(WindowActionOutcome) + Send>,
    ) -> Result<(), DispatchError>;

    /// Queues an action and waits (blocking).
    fn act(&self, request: ActionRequest, cancel: &CancellationToken) -> WindowActionOutcome;

    /// Reads or writes the `AXHidden` of one tracked application on the owner
    /// thread: the fallback when the public hide or unhide returned `false`.
    /// A write is authorized by the request's own one-shot issuance, not the
    /// frame gate, so a restore can still use it after stop sealed the gate.
    fn application_hidden(
        &self,
        request: HiddenRequest,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> HiddenOutcome;

    /// Seals `gate` immediately, then stops the epoch with acknowledgement.
    fn stop(
        &self,
        epoch: RuntimeEpoch,
        gate: &GenerationCell,
        deadline: Option<Instant>,
    ) -> StopOutcome;

    /// Retries quarantined removals exactly once each.
    fn retry_cleanup(
        &self,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> Option<CleanupReport>;

    /// Stops and joins the owner thread (blocking, bounded).
    fn close(&self) -> CloseOutcome;
}

/// The production backend: a dedicated `stache-tiling-ax-owner` thread.
pub struct SystemWindowBackend {
    adapter: WindowAdapter,
    read_tabs: bool,
}

impl SystemWindowBackend {
    /// Starts the owner thread. Nothing calls this yet; no AX message is sent
    /// until a runtime installs an epoch and tracks an application.
    ///
    /// # Errors
    ///
    /// [`BackendError::Spawn`] when the thread cannot start.
    pub fn new(options: &AdapterOptions) -> Result<Self, BackendError> {
        WindowAdapter::system(TILING_AX_OWNER_THREAD_NAME, options)
            .map(|adapter| Self {
                adapter,
                read_tabs: options.read_tabs,
            })
            .map_err(BackendError::Spawn)
    }
}

#[cfg(any(test, feature = "testing"))]
impl SystemWindowBackend {
    /// Wraps an adapter built over a fake system, so tests drive the real
    /// owner `Core` through the production backend path.
    #[must_use]
    pub const fn from_adapter(adapter: WindowAdapter, read_tabs: bool) -> Self {
        Self { adapter, read_tabs }
    }
}

impl WindowBackend for SystemWindowBackend {
    fn adapter(&self) -> AdapterId { self.adapter.id() }

    fn reads_tabs(&self) -> bool { self.read_tabs }

    fn install(
        &self,
        sink: Arc<EventQueue>,
        gate: Arc<GenerationCell>,
        deadline: Option<Instant>,
    ) -> Result<RuntimeEpoch, InstallError> {
        self.adapter.install(InstallRequest { queue: sink, gate }, deadline)
    }

    fn track(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        pid: i32,
        cancel: CancellationToken,
        deadline: Option<Instant>,
    ) -> TrackOutcome {
        self.adapter.track_application(
            epoch,
            TrackRequest {
                application,
                pid,
                cancel,
                deadline,
            },
            deadline,
        )
    }

    fn refresh(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        cancel: CancellationToken,
        deadline: Option<Instant>,
    ) -> TrackOutcome {
        self.adapter.refresh_application(epoch, application, cancel, deadline)
    }

    fn focused_window(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> FocusOutcome {
        self.adapter.focused_window(epoch, application, cancel, deadline)
    }

    fn untrack(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> StopOutcome {
        self.adapter.untrack_application(epoch, application, cancel, deadline)
    }

    fn snapshot(
        &self,
        window: WindowInstance,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> SnapshotOutcome {
        self.adapter.snapshot(window, cancel, deadline)
    }

    fn resolve_window_id(
        &self,
        cg_window_id: u32,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> Option<WindowInstance> {
        self.adapter.resolve_window_id(cg_window_id, cancel, deadline)
    }

    fn submit_action(
        &self,
        request: ActionRequest,
        reply: Box<dyn FnOnce(WindowActionOutcome) + Send>,
    ) -> Result<(), DispatchError> {
        self.adapter.submit_action(request, reply)
    }

    fn act(&self, request: ActionRequest, cancel: &CancellationToken) -> WindowActionOutcome {
        self.adapter.act(request, cancel)
    }

    fn application_hidden(
        &self,
        request: HiddenRequest,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> HiddenOutcome {
        self.adapter.application_hidden(request, cancel, deadline)
    }

    fn stop(
        &self,
        epoch: RuntimeEpoch,
        gate: &GenerationCell,
        deadline: Option<Instant>,
    ) -> StopOutcome {
        self.adapter.stop(epoch, gate, deadline)
    }

    fn retry_cleanup(
        &self,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> Option<CleanupReport> {
        self.adapter.retry_cleanup(cancel, deadline)
    }

    fn close(&self) -> CloseOutcome { self.adapter.close() }
}

/// The production backend, started on first use.
///
/// Constructing it spawns nothing, so a host that registers a disabled Tiling
/// keeps no `stache-tiling-ax-owner` thread. The owner thread starts the first
/// time a runtime installs an epoch; every later call reaches the same
/// [`SystemWindowBackend`]. A refused thread is reported as an unavailable
/// owner, the same truth a dead owner reports.
pub struct LazySystemBackend {
    options: AdapterOptions,
    state: std::sync::Mutex<LazyState>,
}

enum LazyState {
    Unstarted,
    Started(Arc<SystemWindowBackend>),
    Failed(AdapterId),
    Closed(AdapterId),
}

impl LazySystemBackend {
    /// An inert backend that starts its owner thread on first use.
    #[must_use]
    pub const fn new(options: AdapterOptions) -> Self {
        Self {
            options,
            state: std::sync::Mutex::new(LazyState::Unstarted),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LazyState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether the owner thread was started (and not yet closed).
    #[must_use]
    pub fn started(&self) -> bool { matches!(*self.lock(), LazyState::Started(_)) }

    /// The started backend, starting it when this is the first use.
    fn backend(&self) -> Option<Arc<SystemWindowBackend>> {
        let mut state = self.lock();
        if let LazyState::Started(backend) = &*state {
            return Some(Arc::clone(backend));
        }
        if !matches!(*state, LazyState::Unstarted) {
            return None;
        }
        if let Ok(backend) = SystemWindowBackend::new(&self.options) {
            let backend = Arc::new(backend);
            *state = LazyState::Started(Arc::clone(&backend));
            Some(backend)
        } else {
            *state = LazyState::Failed(AdapterId::detached());
            None
        }
    }

    /// The backend only when it already started: cleanup never starts a thread.
    fn existing(&self) -> Option<Arc<SystemWindowBackend>> {
        match &*self.lock() {
            LazyState::Started(backend) => Some(Arc::clone(backend)),
            _ => None,
        }
    }
}

impl WindowBackend for LazySystemBackend {
    fn reads_tabs(&self) -> bool { self.options.read_tabs }

    fn adapter(&self) -> AdapterId {
        if let Some(backend) = self.backend() {
            return backend.adapter();
        }
        match &*self.lock() {
            LazyState::Failed(id) | LazyState::Closed(id) => *id,
            _ => AdapterId::detached(),
        }
    }

    fn install(
        &self,
        sink: Arc<EventQueue>,
        gate: Arc<GenerationCell>,
        deadline: Option<Instant>,
    ) -> Result<RuntimeEpoch, InstallError> {
        self.backend().map_or(Err(InstallError::OwnerUnavailable), |b| {
            b.install(sink, gate, deadline)
        })
    }

    fn track(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        pid: i32,
        cancel: CancellationToken,
        deadline: Option<Instant>,
    ) -> TrackOutcome {
        self.existing().map_or(TrackOutcome::OwnerUnavailable, |b| {
            b.track(epoch, application, pid, cancel, deadline)
        })
    }

    fn refresh(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        cancel: CancellationToken,
        deadline: Option<Instant>,
    ) -> TrackOutcome {
        self.existing().map_or(TrackOutcome::OwnerUnavailable, |b| {
            b.refresh(epoch, application, cancel, deadline)
        })
    }

    fn focused_window(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> FocusOutcome {
        self.existing().map_or(FocusOutcome::OwnerUnavailable, |b| {
            b.focused_window(epoch, application, cancel, deadline)
        })
    }

    fn untrack(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> StopOutcome {
        self.existing().map_or(StopOutcome::OwnerUnavailable, |b| {
            b.untrack(epoch, application, cancel, deadline)
        })
    }

    fn snapshot(
        &self,
        window: WindowInstance,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> SnapshotOutcome {
        self.existing().map_or(SnapshotOutcome::OwnerUnavailable, |b| {
            b.snapshot(window, cancel, deadline)
        })
    }

    fn resolve_window_id(
        &self,
        cg_window_id: u32,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> Option<WindowInstance> {
        self.existing()?.resolve_window_id(cg_window_id, cancel, deadline)
    }

    fn submit_action(
        &self,
        request: ActionRequest,
        reply: Box<dyn FnOnce(WindowActionOutcome) + Send>,
    ) -> Result<(), DispatchError> {
        self.existing()
            .map_or(Err(DispatchError::Closed), |b| b.submit_action(request, reply))
    }

    fn act(&self, request: ActionRequest, cancel: &CancellationToken) -> WindowActionOutcome {
        self.existing().map_or(
            WindowActionOutcome::NotIssued(NotIssuedReason::OwnerUnavailable),
            |b| b.act(request, cancel),
        )
    }

    fn application_hidden(
        &self,
        request: HiddenRequest,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> HiddenOutcome {
        self.existing().map_or(
            HiddenOutcome::NotIssued(NotIssuedReason::OwnerUnavailable),
            |b| b.application_hidden(request, cancel, deadline),
        )
    }

    fn stop(
        &self,
        epoch: RuntimeEpoch,
        gate: &GenerationCell,
        deadline: Option<Instant>,
    ) -> StopOutcome {
        gate.seal();
        self.existing()
            .map_or(StopOutcome::NotCurrent, |b| b.stop(epoch, gate, deadline))
    }

    fn retry_cleanup(
        &self,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> Option<CleanupReport> {
        self.existing().map_or(
            Some(CleanupReport {
                cleaned_applications: 0,
                remaining_applications: 0,
                remaining_registrations: 0,
            }),
            |b| b.retry_cleanup(cancel, deadline),
        )
    }

    fn close(&self) -> CloseOutcome {
        let started = {
            let mut state = self.lock();
            match &*state {
                LazyState::Started(backend) => Some(Arc::clone(backend)),
                LazyState::Unstarted | LazyState::Failed(_) => {
                    *state = LazyState::Closed(AdapterId::detached());
                    None
                }
                LazyState::Closed(_) => return CloseOutcome::AlreadyClosed,
            }
        };
        let Some(backend) = started else {
            return CloseOutcome::Closed;
        };
        let outcome = backend.close();
        if outcome == CloseOutcome::Closed {
            *self.lock() = LazyState::Closed(backend.adapter());
        }
        outcome
    }
}

/// One call a [`FakeWindowBackend`] recorded.
#[cfg(any(test, feature = "testing"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FakeBackendCall {
    /// `install`.
    Install,
    /// `track` for an application.
    Track(ApplicationId, i32),
    /// `refresh` for an application.
    Refresh(ApplicationId),
    /// `untrack` for an application.
    Untrack(ApplicationId),
    /// `focused_window` for an application.
    FocusedWindow(ApplicationId),
    /// `snapshot` of a window.
    Snapshot(WindowInstance),
    /// `resolve_window_id`.
    Resolve(u32),
    /// An action request (queued or blocking).
    Action(WindowInstance),
    /// `application_hidden` for an application with its operation.
    Hidden(ApplicationId, stache_macos::windows::HiddenOperation),
    /// `stop`.
    Stop(RuntimeEpoch),
    /// `retry_cleanup`.
    Retry,
    /// `close`.
    Close,
}

#[cfg(any(test, feature = "testing"))]
mod fake {
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;

    use stache_macos::ax::{AxReading, FrameRead, WriteStatus};
    use stache_macos::windows::{FocusOutcome, IssuedKind, NativeStatus, WindowAction};

    use super::{
        ActionRequest, AdapterId, ApplicationId, Arc, CancellationToken, CleanupReport,
        CloseOutcome, DispatchError, EventQueue, FakeBackendCall, GenerationCell, HiddenOutcome,
        HiddenRequest, InstallError, Instant, RuntimeEpoch, SnapshotOutcome, StopOutcome,
        TrackOutcome, WindowActionOutcome, WindowBackend, WindowInstance,
    };
    use crate::geometry::Rect;

    #[derive(Default)]
    struct State {
        calls: Vec<FakeBackendCall>,
        sink: Option<Arc<EventQueue>>,
        epochs: u64,
        track_outcome: Option<TrackOutcome>,
        track_outcomes: HashMap<ApplicationId, VecDeque<TrackOutcome>>,
        action_outcomes: VecDeque<WindowActionOutcome>,
        hidden_outcomes: VecDeque<HiddenOutcome>,
        dispatch_error: Option<DispatchError>,
        snapshots: HashMap<WindowInstance, SnapshotOutcome>,
        focused: HashMap<ApplicationId, WindowInstance>,
        resolve: HashMap<u32, WindowInstance>,
        stop_outcome: Option<StopOutcome>,
        retry_reports: VecDeque<Option<CleanupReport>>,
        install_error: Option<InstallError>,
        gate: Option<Arc<GenerationCell>>,
        hold: Option<Arc<SendHold>>,
        reads_tabs: bool,
        echo_frames: bool,
        written: Vec<(WindowInstance, Rect)>,
    }

    #[derive(Default)]
    struct HoldState {
        entered: usize,
        released: bool,
    }

    /// Holds fake native sends until a test releases them, like a stalled
    /// owner. Logical invalidation (`GenerationCell::seal`) never waits on it.
    #[derive(Default)]
    pub struct SendHold {
        state: Mutex<HoldState>,
        changed: Condvar,
    }

    // The guard is deliberately held across the condvar wait; this is test-only.
    #[allow(clippy::significant_drop_tightening)]
    impl SendHold {
        fn guard(&self) -> std::sync::MutexGuard<'_, HoldState> {
            self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
        }

        fn pass(&self) {
            let mut state = self.guard();
            state.entered += 1;
            self.changed.notify_all();
            let released = self
                .changed
                .wait_while(state, |state| !state.released)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            drop(released);
        }

        /// Waits until `count` sends are held. `false` on timeout.
        #[must_use]
        pub fn wait_entered(&self, count: usize, timeout: Duration) -> bool {
            let state = self.guard();
            let (state, _) = self
                .changed
                .wait_timeout_while(state, timeout, |state| state.entered < count)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let reached = state.entered >= count;
            drop(state);
            reached
        }

        /// Lets every held and future send return.
        pub fn release(&self) {
            self.guard().released = true;
            self.changed.notify_all();
        }
    }

    /// A deterministic in-crate backend. It never starts a thread or touches
    /// native code; actions complete inline with scripted outcomes.
    pub struct FakeWindowBackend {
        adapter: AdapterId,
        state: Mutex<State>,
    }

    impl FakeWindowBackend {
        fn state(&self) -> std::sync::MutexGuard<'_, State> {
            self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
        }

        /// Every call so far, in order.
        pub fn calls(&self) -> Vec<FakeBackendCall> { self.state().calls.clone() }

        /// The sink the last install registered.
        pub fn sink(&self) -> Option<Arc<EventQueue>> { self.state().sink.clone() }

        /// Scripts the next `track` outcome (default `Tracking`).
        pub fn script_track(&self, outcome: TrackOutcome) {
            self.state().track_outcome = Some(outcome);
        }

        /// Queues a `track` outcome for one application only; once the queue is
        /// empty that application tracks normally again.
        pub fn script_track_for(&self, application: ApplicationId, outcome: TrackOutcome) {
            self.state().track_outcomes.entry(application).or_default().push_back(outcome);
        }

        /// Declares that this backend reads tab groups (default off).
        pub fn script_reads_tabs(&self, reads: bool) { self.state().reads_tabs = reads; }

        /// Scripts the next install failure.
        pub fn script_install_error(&self, error: InstallError) {
            self.state().install_error = Some(error);
        }

        /// Queues an action outcome (default `Stale(UnknownWindow)`).
        pub fn script_action(&self, outcome: WindowActionOutcome) {
            self.state().action_outcomes.push_back(outcome);
        }

        /// Makes an unscripted frame write answer `Applied` with a readback of
        /// exactly the frame it was asked for (default off: unscripted writes
        /// are `Stale`). Writing both halves is assumed.
        pub fn script_echo_frames(&self, echo: bool) { self.state().echo_frames = echo; }

        /// Every frame written so far (position and size both requested), in order.
        pub fn written_frames(&self) -> Vec<(WindowInstance, Rect)> { self.state().written.clone() }

        /// Queues an `AXHidden` outcome (default: not tracked).
        pub fn script_hidden(&self, outcome: HiddenOutcome) {
            self.state().hidden_outcomes.push_back(outcome);
        }

        /// Makes the next dispatch fail.
        pub fn script_dispatch_error(&self, error: DispatchError) {
            self.state().dispatch_error = Some(error);
        }

        /// Scripts a snapshot answer.
        pub fn script_snapshot(&self, window: WindowInstance, outcome: SnapshotOutcome) {
            self.state().snapshots.insert(window, outcome);
        }

        /// Scripts the window an application reports as its focused one.
        pub fn script_focused(&self, application: ApplicationId, window: WindowInstance) {
            self.state().focused.insert(application, window);
        }

        /// The application reports no focused window any more.
        pub fn script_unfocused(&self, application: ApplicationId) {
            self.state().focused.remove(&application);
        }

        /// Scripts a `CGWindowID` resolution.
        pub fn script_resolve(&self, cg_window_id: u32, window: WindowInstance) {
            self.state().resolve.insert(cg_window_id, window);
        }

        /// Holds every later `act`/`submit_action` after its gate check and
        /// outcome selection, until the returned hold is released.
        pub fn hold_sends(&self) -> Arc<SendHold> {
            let hold = Arc::new(SendHold::default());
            self.state().hold = Some(Arc::clone(&hold));
            hold
        }

        /// How many action requests were recorded.
        pub fn action_count(&self) -> usize {
            self.state()
                .calls
                .iter()
                .filter(|call| matches!(call, FakeBackendCall::Action(_)))
                .count()
        }

        /// The scripted outcome, else (when echoing) a faithful readback of the
        /// requested frame, else a stale token.
        fn select_outcome(state: &mut State, request: &ActionRequest) -> WindowActionOutcome {
            if let Some(scripted) = state.action_outcomes.pop_front() {
                return scripted;
            }
            match (state.echo_frames, &request.action) {
                (
                    true,
                    WindowAction::SetFrame {
                        position: Some(p),
                        size: Some(z),
                    },
                ) => {
                    let frame = Rect::new(p.x, p.y, z.width, z.height);
                    state.written.push((request.issuance.window(), frame));
                    WindowActionOutcome::Issued {
                        kind: IssuedKind::Applied,
                        native: NativeStatus::Frame {
                            position: WriteStatus::Written,
                            size: WriteStatus::Written,
                        },
                        observed: Some(FrameRead {
                            position: AxReading::Value(*p),
                            size: AxReading::Value(*z),
                        }),
                    }
                }
                _ => WindowActionOutcome::Stale(stache_macos::windows::StaleReason::UnknownWindow),
            }
        }

        /// Models the owner's runtime-wide check just before the native send.
        fn gate_refusal(state: &State, request: &ActionRequest) -> Option<WindowActionOutcome> {
            let gate = state.gate.as_ref()?;
            if gate.is_sealed() {
                return Some(WindowActionOutcome::NotIssued(
                    stache_macos::windows::NotIssuedReason::Sealed,
                ));
            }
            let live = gate.live_generation();
            (live == 0 || live != request.issuance.generation()).then_some(
                WindowActionOutcome::NotIssued(
                    stache_macos::windows::NotIssuedReason::GenerationInvalid,
                ),
            )
        }

        /// Queues the next `retry_cleanup` answer (default: nothing remains).
        pub fn script_retry(&self, report: Option<CleanupReport>) {
            self.state().retry_reports.push_back(report);
        }

        /// Scripts the next stop outcome (default `Clean`).
        pub fn script_stop(&self, outcome: StopOutcome) {
            self.state().stop_outcome = Some(outcome);
        }
    }

    impl Default for FakeWindowBackend {
        fn default() -> Self {
            Self {
                adapter: AdapterId::for_testing(),
                state: Mutex::new(State::default()),
            }
        }
    }

    impl WindowBackend for FakeWindowBackend {
        fn adapter(&self) -> AdapterId { self.adapter }

        fn reads_tabs(&self) -> bool { self.state().reads_tabs }

        fn install(
            &self,
            sink: Arc<EventQueue>,
            gate: Arc<GenerationCell>,
            _deadline: Option<Instant>,
        ) -> Result<RuntimeEpoch, InstallError> {
            let mut state = self.state();
            state.calls.push(FakeBackendCall::Install);
            state.gate = Some(gate);
            if let Some(error) = state.install_error.take() {
                return Err(error);
            }
            state.epochs += 1;
            state.sink = Some(sink);
            Ok(RuntimeEpoch::for_testing(state.epochs))
        }

        fn track(
            &self,
            _epoch: RuntimeEpoch,
            application: ApplicationId,
            pid: i32,
            _cancel: CancellationToken,
            _deadline: Option<Instant>,
        ) -> TrackOutcome {
            let mut state = self.state();
            state.calls.push(FakeBackendCall::Track(application, pid));
            if let Some(outcome) =
                state.track_outcomes.get_mut(&application).and_then(VecDeque::pop_front)
            {
                return outcome;
            }
            state.track_outcome.take().unwrap_or(TrackOutcome::Tracking)
        }

        fn refresh(
            &self,
            _epoch: RuntimeEpoch,
            application: ApplicationId,
            _cancel: CancellationToken,
            _deadline: Option<Instant>,
        ) -> TrackOutcome {
            let mut state = self.state();
            state.calls.push(FakeBackendCall::Refresh(application));
            state.track_outcome.take().unwrap_or(TrackOutcome::Tracking)
        }

        fn focused_window(
            &self,
            _epoch: RuntimeEpoch,
            application: ApplicationId,
            _cancel: &CancellationToken,
            _deadline: Option<Instant>,
        ) -> FocusOutcome {
            let mut state = self.state();
            state.calls.push(FakeBackendCall::FocusedWindow(application));
            state
                .focused
                .get(&application)
                .copied()
                .map_or(FocusOutcome::Unresolved, FocusOutcome::Resolved)
        }

        fn untrack(
            &self,
            _epoch: RuntimeEpoch,
            application: ApplicationId,
            _cancel: &CancellationToken,
            _deadline: Option<Instant>,
        ) -> StopOutcome {
            self.state().calls.push(FakeBackendCall::Untrack(application));
            StopOutcome::Clean { applications: 1 }
        }

        fn snapshot(
            &self,
            window: WindowInstance,
            _cancel: &CancellationToken,
            _deadline: Option<Instant>,
        ) -> SnapshotOutcome {
            let mut state = self.state();
            state.calls.push(FakeBackendCall::Snapshot(window));
            state.snapshots.get(&window).cloned().unwrap_or(SnapshotOutcome::Stale(
                stache_macos::windows::StaleReason::UnknownWindow,
            ))
        }

        fn resolve_window_id(
            &self,
            cg_window_id: u32,
            _cancel: &CancellationToken,
            _deadline: Option<Instant>,
        ) -> Option<WindowInstance> {
            let mut state = self.state();
            state.calls.push(FakeBackendCall::Resolve(cg_window_id));
            state.resolve.get(&cg_window_id).copied()
        }

        fn submit_action(
            &self,
            request: ActionRequest,
            reply: Box<dyn FnOnce(WindowActionOutcome) + Send>,
        ) -> Result<(), DispatchError> {
            let (outcome, hold) = {
                let mut state = self.state();
                state.calls.push(FakeBackendCall::Action(request.issuance.window()));
                if let Some(error) = state.dispatch_error.take() {
                    return Err(error);
                }
                let outcome = Self::gate_refusal(&state, &request)
                    .unwrap_or_else(|| Self::select_outcome(&mut state, &request));
                (outcome, state.hold.clone())
            };
            if let Some(hold) = hold {
                hold.pass();
            }
            reply(outcome);
            Ok(())
        }

        fn act(&self, request: ActionRequest, _cancel: &CancellationToken) -> WindowActionOutcome {
            let (outcome, hold) = {
                let mut state = self.state();
                state.calls.push(FakeBackendCall::Action(request.issuance.window()));
                if let Some(error) = state.dispatch_error.take() {
                    return WindowActionOutcome::NotIssued(
                        stache_macos::windows::NotIssuedReason::Dispatch(error),
                    );
                }
                let outcome = Self::gate_refusal(&state, &request)
                    .unwrap_or_else(|| Self::select_outcome(&mut state, &request));
                (outcome, state.hold.clone())
            };
            if let Some(hold) = hold {
                hold.pass();
            }
            outcome
        }

        fn application_hidden(
            &self,
            request: HiddenRequest,
            _cancel: &CancellationToken,
            _deadline: Option<Instant>,
        ) -> HiddenOutcome {
            let mut state = self.state();
            state
                .calls
                .push(FakeBackendCall::Hidden(request.application, request.operation));
            let scripted = state.hidden_outcomes.pop_front();
            drop(state);
            // A scripted write models the owner committing the one-shot send.
            let Some(outcome) = scripted else {
                return HiddenOutcome::Stale(stache_macos::windows::StaleReason::ApplicationGone);
            };
            if matches!(
                outcome,
                HiddenOutcome::Written { .. } | HiddenOutcome::Unobserved
            ) && !request
                .issuance
                .as_ref()
                .is_some_and(|issuance| issuance.commit_send(Instant::now()))
            {
                return HiddenOutcome::NotIssued(stache_macos::windows::NotIssuedReason::Cancelled);
            }
            outcome
        }

        fn stop(
            &self,
            epoch: RuntimeEpoch,
            gate: &GenerationCell,
            _deadline: Option<Instant>,
        ) -> StopOutcome {
            gate.seal();
            let mut state = self.state();
            state.calls.push(FakeBackendCall::Stop(epoch));
            state.stop_outcome.take().unwrap_or(StopOutcome::Clean { applications: 0 })
        }

        fn retry_cleanup(
            &self,
            _cancel: &CancellationToken,
            _deadline: Option<Instant>,
        ) -> Option<CleanupReport> {
            let mut state = self.state();
            state.calls.push(FakeBackendCall::Retry);
            state.retry_reports.pop_front().unwrap_or(Some(CleanupReport {
                cleaned_applications: 0,
                remaining_applications: 0,
                remaining_registrations: 0,
            }))
        }

        fn close(&self) -> CloseOutcome {
            self.state().calls.push(FakeBackendCall::Close);
            CloseOutcome::Closed
        }
    }
}

#[cfg(any(test, feature = "testing"))]
pub use fake::{FakeWindowBackend, SendHold};

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use stache_macos::windows::{
        IssuedKind, NativeStatus, NotIssuedReason, StaleReason, WindowAction, WindowIssuance,
    };

    use super::*;

    const fn assert_send_sync<T: Send + Sync>() {}
    const _: () = {
        assert_send_sync::<SystemWindowBackend>();
        assert_send_sync::<FakeWindowBackend>();
        assert_send_sync::<Arc<dyn WindowBackend>>();
    };

    #[test]
    fn the_tiling_owner_name_is_distinct_from_menu_anywheres() {
        assert_eq!(TILING_AX_OWNER_THREAD_NAME, "stache-tiling-ax-owner");
        assert_ne!(TILING_AX_OWNER_THREAD_NAME, "stache-ax-owner");
    }

    #[test]
    fn the_fake_installs_tracks_acts_and_stops_through_the_trait() {
        let backend: Arc<dyn WindowBackend> = Arc::new(FakeWindowBackend::default());
        let sink = EventQueue::new(8);
        let gate = Arc::new(GenerationCell::new());
        gate.publish(1, 1);
        let epoch = backend.install(sink, Arc::clone(&gate), None).expect("install");
        let application = ApplicationId::for_testing(1);
        assert_eq!(
            backend.track(epoch, application, 10, CancellationToken::new(), None),
            TrackOutcome::Tracking
        );
        let window = WindowInstance::for_testing(backend.adapter(), epoch, application, 1);
        let request = ActionRequest {
            issuance: WindowIssuance::new(window, 1, 1, 1, Instant::now() + Duration::from_secs(5)),
            action: WindowAction::Raise,
        };
        // Unscripted: a truthful no-send, never a made-up success.
        assert_eq!(
            backend.act(request.clone(), &CancellationToken::new()),
            WindowActionOutcome::Stale(StaleReason::UnknownWindow)
        );
        let fake = FakeWindowBackend::default();
        fake.script_action(WindowActionOutcome::Issued {
            kind: IssuedKind::Applied,
            native: NativeStatus::Action(Ok(())),
            observed: None,
        });
        fake.script_dispatch_error(DispatchError::Full);
        assert_eq!(
            fake.act(request.clone(), &CancellationToken::new()),
            WindowActionOutcome::NotIssued(NotIssuedReason::Dispatch(DispatchError::Full))
        );
        assert!(matches!(
            fake.act(request, &CancellationToken::new()),
            WindowActionOutcome::Issued { kind: IssuedKind::Applied, .. }
        ));
        assert_eq!(backend.stop(epoch, &gate, None), StopOutcome::Clean {
            applications: 0
        });
        assert!(gate.is_sealed());
    }
}
