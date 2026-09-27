//! Initialization and global state management for `tiling`.
//!
//! This module provides the entry point for initializing the tiling window manager
//! and accessing the global state actor handle.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────┐
//! │                         init()                                          │
//! │  1. Create StateActor + Handle                                          │
//! │  2. Create EventProcessor (with screen refresh rate batching)           │
//! │  3. Create EffectSubscriber + Executor                                  │
//! │  4. Wire everything together                                            │
//! │  5. Start event adapters (AX, App, Screen monitors)                     │
//! │  6. Detect screens and create initial workspaces                        │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use crate::tiling::init;
//!
//! // During app startup:
//! init::init(app_handle);
//!
//! // Later, to send commands:
//! if let Some(handle) = init::get_handle() {
//!     handle.send(StateMessage::SwitchWorkspace { name: "main".into() });
//! }
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tauri::Emitter;

use super::actor::{StateActor, StateActorHandle, StateMessage};
use super::borders;
use super::effects::subscriber::EffectSubscriberHandle;
use super::effects::{EffectExecutor, EffectSubscriber};
use super::events::{AXObserverAdapter, AppMonitorAdapter, EventProcessor, ScreenMonitorAdapter};
use crate::config::get_config;
use crate::modules::tiling::identity::{AppIdentity, WindowTarget};
use crate::modules::tiling::visibility::VisibilityRegistry;
use crate::{events, is_accessibility_granted};

// ============================================================================
// Global State
// ============================================================================

/// Serialized lifecycle state guarding concurrent start/pause requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecycleState {
    Stopped,
    Starting,
    Running,
    Stopping,
}

/// Named startup stages so a failure can be attributed and injected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InitStage {
    Actor,
    Processor,
    Subscriber,
    AppMonitor,
    ScreenMonitor,
    AxAdapter,
    StandaloneObservers,
    InitialState,
    MouseMonitor,
    Borders,
}

/// Owns every resource created so far during startup. Optional fields permit
/// rollback/quarantine after failure at any fatal `InitStage`.
#[allow(dead_code)] // adapter fields are consumed by the pause task (15D)
struct PartialRuntime {
    generation: u64,
    actor: Option<StateActorHandle>,
    actor_stopped: Option<CompletionLatch>,
    processor: Option<Arc<EventProcessor>>,
    subscriber: Option<EffectSubscriberHandle>,
    subscriber_stopped: Option<CompletionLatch>,
    app_monitor: Option<Arc<AppMonitorAdapter>>,
    screen_monitor: Option<Arc<ScreenMonitorAdapter>>,
    ax_adapter: Option<Arc<AXObserverAdapter>>,
    teardown: TeardownProgress,
}

/// Fully populated runtime required for a clean `Running` publish.
#[allow(dead_code)] // adapter fields are consumed by the pause task (15D)
struct TilingRuntime {
    generation: u64,
    actor: StateActorHandle,
    actor_stopped: CompletionLatch,
    processor: Arc<EventProcessor>,
    subscriber: EffectSubscriberHandle,
    subscriber_stopped: CompletionLatch,
    app_monitor: Arc<AppMonitorAdapter>,
    screen_monitor: Arc<ScreenMonitorAdapter>,
    ax_adapter: Arc<AXObserverAdapter>,
    teardown: TeardownProgress,
}

/// Per-stage teardown flags advanced by the runtime pause task.
#[allow(dead_code)] // advanced by the pause/retry task (15D)
#[allow(clippy::struct_excessive_bools)] // one flag per teardown stage by design
#[derive(Default)]
struct TeardownProgress {
    visibility_restored: bool,
    main_thread_sources_removed: bool,
    processor_stopped: bool,
    transient_services_paused: bool,
    subscriber_stopped: bool,
    actor_stopped: bool,
    caches_cleared: bool,
}

/// Restartable runtime holder. `Quarantined` is produced by the pause/retry
/// task; a running runtime is the only state published here.
enum RuntimeSlot {
    Empty,
    Running(TilingRuntime),
    #[allow(dead_code)] // produced by the 15D pause/retry task
    Quarantined(PartialRuntime),
}

impl TryFrom<PartialRuntime> for TilingRuntime {
    type Error = String;

    fn try_from(partial: PartialRuntime) -> Result<Self, String> {
        Ok(Self {
            generation: partial.generation,
            actor: partial.actor.ok_or("tiling: actor missing from runtime")?,
            actor_stopped: partial.actor_stopped.ok_or("tiling: actor completion latch missing")?,
            processor: partial.processor.ok_or("tiling: processor missing from runtime")?,
            subscriber: partial.subscriber.ok_or("tiling: subscriber missing from runtime")?,
            subscriber_stopped: partial
                .subscriber_stopped
                .ok_or("tiling: subscriber completion latch missing")?,
            app_monitor: partial.app_monitor.ok_or("tiling: app monitor missing from runtime")?,
            screen_monitor: partial
                .screen_monitor
                .ok_or("tiling: screen monitor missing from runtime")?,
            ax_adapter: partial.ax_adapter.ok_or("tiling: ax adapter missing from runtime")?,
            teardown: partial.teardown,
        })
    }
}

static LIFECYCLE: Mutex<LifecycleState> = Mutex::new(LifecycleState::Stopped);
static RUNTIME: Mutex<RuntimeSlot> = Mutex::new(RuntimeSlot::Empty);
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
const RUNTIME_STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// Test seam: overrides `RUNTIME_STOP_TIMEOUT` so timeout paths do not block
/// real seconds in unit tests.
static STOP_TIMEOUT_OVERRIDE: Mutex<Option<Duration>> = Mutex::new(None);

fn stop_timeout() -> Duration { (*STOP_TIMEOUT_OVERRIDE.lock()).unwrap_or(RUNTIME_STOP_TIMEOUT) }

#[cfg(test)]
fn set_stop_timeout(override_value: Option<Duration>) {
    *STOP_TIMEOUT_OVERRIDE.lock() = override_value;
}

/// Stored Tauri app handle for emitting events.
static APP_HANDLE: Mutex<Option<tauri::AppHandle>> = Mutex::new(None);

// ============================================================================
// Repeatable Completion
// ============================================================================

/// Repeatable completion state: unlike a consumed one-shot receiver, retries
/// can observe that a resource has already stopped. All clones share the same
/// flag; completion stays observable after a timeout or a successful wait.
#[derive(Clone, Debug)]
pub(crate) struct CompletionLatch(Arc<(parking_lot::Mutex<bool>, parking_lot::Condvar)>);

impl CompletionLatch {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self(Arc::new((
            parking_lot::Mutex::new(false),
            parking_lot::Condvar::new(),
        )))
    }

    /// Marks the resource as complete and wakes all waiters.
    pub(crate) fn mark_complete(&self) {
        let (lock, cvar) = &*self.0;
        *lock.lock() = true;
        cvar.notify_all();
    }

    /// Waits up to `timeout` for completion. Returns `true` when complete;
    /// completion remains observable on later calls after a timeout.
    #[must_use]
    #[allow(dead_code)] // production waiter added by the 15D pause teardown
    pub(crate) fn wait_timeout(&self, timeout: Duration) -> bool {
        let (lock, cvar) = &*self.0;
        let mut done = lock.lock();
        if !*done {
            cvar.wait_until(&mut done, Instant::now() + timeout);
        }
        *done
    }
}

// ============================================================================
// Public API
// ============================================================================

/// Gets the state actor handle for the current running runtime.
///
/// Returns `None` if the tiling runtime is not currently `Running`.
#[must_use]
pub fn get_handle() -> Option<StateActorHandle> {
    match &*RUNTIME.lock() {
        RuntimeSlot::Running(rt) => Some(rt.actor.clone()),
        RuntimeSlot::Empty | RuntimeSlot::Quarantined(_) => None,
    }
}

/// Gets the current event processor.
///
/// Returns `None` if the tiling runtime is not currently `Running`.
#[must_use]
pub fn get_processor() -> Option<Arc<EventProcessor>> {
    match &*RUNTIME.lock() {
        RuntimeSlot::Running(rt) => Some(Arc::clone(&rt.processor)),
        RuntimeSlot::Empty | RuntimeSlot::Quarantined(_) => None,
    }
}

/// Gets the current effect subscriber handle.
///
/// Returns `None` if the tiling runtime is not currently `Running`.
#[must_use]
pub fn get_subscriber_handle() -> Option<EffectSubscriberHandle> {
    match &*RUNTIME.lock() {
        RuntimeSlot::Running(rt) => Some(rt.subscriber.clone()),
        RuntimeSlot::Empty | RuntimeSlot::Quarantined(_) => None,
    }
}

/// Returns whether the tiling runtime is currently `Running`.
#[must_use]
pub fn is_initialized() -> bool { *LIFECYCLE.lock() == LifecycleState::Running }

/// Returns the current runtime generation counter value.
#[must_use]
pub fn current_generation() -> u64 { NEXT_GENERATION.load(Ordering::Relaxed) }

/// Returns whether tiling is enabled in config.
#[must_use]
pub fn is_enabled() -> bool { get_config().tiling.is_enabled() }

/// Initializes the `tiling` window manager.
///
/// Gates on config and accessibility, stores the app handle, then starts a
/// fresh runtime generation.
#[allow(clippy::needless_pass_by_value)] // AppHandle is intentionally passed by value for storage
pub fn init(app_handle: tauri::AppHandle) -> bool {
    if is_initialized() {
        tracing::warn!("tiling: already initialized");
        return false;
    }

    let config = get_config();
    tracing::debug!("tiling: enabled = {}", config.tiling.is_enabled());

    if !config.tiling.is_enabled() {
        tracing::info!("tiling: disabled in config (set enabled=true to enable)");
        return false;
    }

    tracing::debug!("tiling: accessibility_granted = {}", is_accessibility_granted());
    if !is_accessibility_granted() {
        tracing::warn!("tiling: accessibility permissions not granted");
        return false;
    }

    match start_runtime(app_handle.clone()) {
        Ok(()) => {
            tracing::info!("tiling: initialized successfully");
            if let Err(e) = app_handle.emit(
                events::tiling::INITIALIZED,
                serde_json::json!({ "enabled": true, "version": "v2" }),
            ) {
                tracing::warn!("tiling: failed to emit initialized event: {e}");
            }
            true
        }
        Err(e) => {
            tracing::error!("tiling: initialization failed: {e}");
            false
        }
    }
}

/// Shuts down the tiling system.
pub fn shutdown() {
    if let Err(e) = pause_runtime() {
        tracing::error!("tiling: shutdown failed: {e}");
    }
}

/// Resumes a paused tiling runtime via the same fresh-start path as `start`.
///
/// Never reuses retained resources from a prior generation.
///
/// # Errors
///
/// Returns an error when the lifecycle is not `Stopped` or any fatal startup
/// stage fails.
pub fn resume(app_handle: tauri::AppHandle) -> Result<(), String> { start_runtime(app_handle) }

impl From<TilingRuntime> for PartialRuntime {
    fn from(runtime: TilingRuntime) -> Self {
        Self {
            generation: runtime.generation,
            actor: Some(runtime.actor),
            actor_stopped: Some(runtime.actor_stopped),
            processor: Some(runtime.processor),
            subscriber: Some(runtime.subscriber),
            subscriber_stopped: Some(runtime.subscriber_stopped),
            app_monitor: Some(runtime.app_monitor),
            screen_monitor: Some(runtime.screen_monitor),
            ax_adapter: Some(runtime.ax_adapter),
            teardown: runtime.teardown,
        }
    }
}

use crate::modules::services::lifecycle::{LifecycleModule, ModuleStatus};

/// `LifecycleModule` adapter for the tiling window manager.
pub struct TilingLifecycle {
    app_handle: tauri::AppHandle,
}

impl TilingLifecycle {
    #[must_use]
    pub const fn new(app_handle: tauri::AppHandle) -> Self { Self { app_handle } }
}

impl LifecycleModule for TilingLifecycle {
    fn name(&self) -> &'static str { "Tiling Window Manager" }

    fn id(&self) -> &'static str { "tiling" }

    fn start(&self) -> Result<(), String> { start_runtime(self.app_handle.clone()) }

    fn pause(&self) -> Result<(), String> { pause_runtime() }

    fn resume(&self) -> Result<(), String> { start_runtime(self.app_handle.clone()) }

    fn status(&self) -> ModuleStatus {
        if !is_enabled() {
            return ModuleStatus::ConfiguredOff;
        }
        let state = *LIFECYCLE.lock();
        match state {
            LifecycleState::Running | LifecycleState::Starting => ModuleStatus::Running,
            LifecycleState::Stopped | LifecycleState::Stopping => ModuleStatus::Paused,
        }
    }
}

// ============================================================================
// Staged Startup Factory
// ============================================================================

/// Staged startup factory: each method maps to one `InitStage`, lets tests fail
/// a named stage, and routes AppKit/AX work through the synchronous
/// main-thread helper.
struct RuntimeFactory {
    fail_stage: Option<InitStage>,
    /// Fresh per-generation visibility registry, shared with the actor and
    /// its handle. Never reused across generations (a sealed registry stays
    /// sealed).
    registry: Arc<VisibilityRegistry>,
}

impl RuntimeFactory {
    fn new(fail_stage: Option<InitStage>) -> Self {
        Self {
            fail_stage,
            registry: Arc::new(VisibilityRegistry::default()),
        }
    }

    fn fail(&self, stage: InitStage) -> Result<(), String> {
        if self.fail_stage == Some(stage) {
            return Err(format!("tiling: injected startup failure at {stage:?}"));
        }
        Ok(())
    }

    fn create_actor(&self) -> Result<(StateActorHandle, CompletionLatch), String> {
        self.fail(InitStage::Actor)?;
        Ok(StateActor::spawn_with_registry(Arc::clone(&self.registry)))
    }

    fn create_processor(&self, actor: StateActorHandle) -> Result<Arc<EventProcessor>, String> {
        self.fail(InitStage::Processor)?;
        let processor = Arc::new(EventProcessor::new(actor));
        processor.start();
        Ok(processor)
    }

    fn create_subscriber(
        &self,
        actor: StateActorHandle,
    ) -> Result<(EffectSubscriberHandle, CompletionLatch), String> {
        self.fail(InitStage::Subscriber)?;
        let mut executor =
            get_app_handle().map_or_else(EffectExecutor::new, EffectExecutor::with_app_handle);
        if get_config().tiling.borders.is_enabled() {
            executor.set_borders_enabled(true);
        }
        let (subscriber, subscriber_handle, stopped) = EffectSubscriber::new(actor, executor);
        tauri::async_runtime::spawn(subscriber.run());
        Ok((subscriber_handle, stopped))
    }

    fn create_app_monitor(
        &self,
        processor: Arc<EventProcessor>,
    ) -> Result<Arc<AppMonitorAdapter>, String> {
        self.fail(InitStage::AppMonitor)?;
        Ok(crate::platform::thread::dispatch_on_main_sync(move || {
            let app_monitor = Arc::new(AppMonitorAdapter::new(processor));
            if !app_monitor.init() {
                tracing::warn!("tiling: app monitor initialization failed");
            }
            super::events::app_monitor::install_adapter(Arc::clone(&app_monitor));
            app_monitor
        }))
    }

    fn create_screen_monitor(
        &self,
        processor: Arc<EventProcessor>,
    ) -> Result<Arc<ScreenMonitorAdapter>, String> {
        self.fail(InitStage::ScreenMonitor)?;
        Ok(crate::platform::thread::dispatch_on_main_sync(move || {
            let screen_monitor = Arc::new(ScreenMonitorAdapter::new(processor));
            if !screen_monitor.init() {
                tracing::warn!("tiling: screen monitor initialization failed");
            }
            super::events::screen_monitor::install_adapter(Arc::clone(&screen_monitor));
            screen_monitor
        }))
    }

    fn create_ax_adapter(
        &self,
        processor: Arc<EventProcessor>,
    ) -> Result<Arc<AXObserverAdapter>, String> {
        self.fail(InitStage::AxAdapter)?;
        Ok(crate::platform::thread::dispatch_on_main_sync(move || {
            let ax_adapter = Arc::new(super::events::AXObserverAdapter::new(processor));
            super::events::ax_observer::install_adapter(Arc::clone(&ax_adapter));
            ax_adapter.activate();
            ax_adapter
        }))
    }

    fn setup_standalone_observers(&self) -> Result<(), String> {
        self.fail(InitStage::StandaloneObservers)?;
        crate::platform::thread::dispatch_on_main_sync(|| {
            if super::events::observer::init() {
                tracing::debug!("tiling: AXObserver initialized");
                Ok(())
            } else {
                Err("tiling: AXObserver initialization failed".to_string())
            }
        })
    }

    fn setup_mouse_monitor(&self) -> Result<(), String> {
        self.fail(InitStage::MouseMonitor)?;
        if super::events::mouse_monitor::init() {
            super::events::mouse_monitor::set_mouse_up_callback(on_mouse_up);
            Ok(())
        } else {
            Err("tiling: mouse monitor initialization failed".to_string())
        }
    }

    fn setup_borders(&self) -> Result<(), String> {
        self.fail(InitStage::Borders)?;
        if borders::init() {
            Ok(())
        } else {
            Err("tiling: borders initialization failed".to_string())
        }
    }

    fn enumerate_initial_state(
        &self,
        actor: StateActorHandle,
        processor: Arc<EventProcessor>,
        subscriber: EffectSubscriberHandle,
    ) -> Result<(), String> {
        self.fail(InitStage::InitialState)?;
        crate::platform::thread::dispatch_on_main_sync(move || {
            initialize_state(&actor, &processor, &subscriber);
        });
        Ok(())
    }
}

/// Builds a full runtime for one generation, assigning every fatal stage to
/// `PartialRuntime` as it succeeds. A failure at any fatal stage drops the
/// partial (rollback by drop: closed channels stop the actor and subscriber,
/// and `EventProcessor::drop` stops its timers) and returns the error.
fn build_runtime(factory: &RuntimeFactory, generation: u64) -> Result<TilingRuntime, String> {
    let mut partial = PartialRuntime {
        generation,
        actor: None,
        actor_stopped: None,
        processor: None,
        subscriber: None,
        subscriber_stopped: None,
        app_monitor: None,
        screen_monitor: None,
        ax_adapter: None,
        teardown: TeardownProgress::default(),
    };

    let (actor, actor_stopped) = factory.create_actor()?;
    partial.actor = Some(actor.clone());
    partial.actor_stopped = Some(actor_stopped);

    let processor = factory.create_processor(actor.clone())?;
    partial.processor = Some(Arc::clone(&processor));

    let (subscriber_handle, subscriber_stopped) = factory.create_subscriber(actor.clone())?;
    partial.subscriber = Some(subscriber_handle.clone());
    partial.subscriber_stopped = Some(subscriber_stopped);

    let app_monitor = factory.create_app_monitor(Arc::clone(&processor))?;
    partial.app_monitor = Some(app_monitor);

    let screen_monitor = factory.create_screen_monitor(Arc::clone(&processor))?;
    partial.screen_monitor = Some(screen_monitor);

    let ax_adapter = factory.create_ax_adapter(Arc::clone(&processor))?;
    partial.ax_adapter = Some(ax_adapter);

    factory.setup_standalone_observers()?;

    // Optional stages: failure logs degraded mode but still permits Running.
    if let Err(e) = factory.setup_mouse_monitor() {
        tracing::warn!("{e}");
    } else {
        super::events::mouse_monitor::set_active(true);
    }
    if let Err(e) = factory.setup_borders() {
        tracing::warn!("{e}");
    } else {
        super::borders::resume();
    }

    super::effects::resume_animation_worker();

    factory.enumerate_initial_state(actor, processor, subscriber_handle.clone())?;
    if !subscriber_handle.wait_until_ready(stop_timeout()) {
        return Err("tiling: effect subscriber did not become ready during startup".to_string());
    }

    TilingRuntime::try_from(partial)
}

/// Guards fresh starts against a quarantined runtime or an in-progress
/// transition. `resume` and `start` must reject these states.
fn ensure_can_start() -> Result<(), String> {
    let lifecycle = *LIFECYCLE.lock();
    if lifecycle != LifecycleState::Stopped {
        return Err(format!("tiling: cannot start while lifecycle is {lifecycle:?}"));
    }
    if matches!(&*RUNTIME.lock(), RuntimeSlot::Quarantined(_)) {
        return Err(
            "tiling: previous runtime is quarantined; only pause/shutdown may retry teardown"
                .to_string(),
        );
    }
    Ok(())
}

/// Starts a fresh runtime generation and publishes `RuntimeSlot::Running`.
///
/// The handle is stored for event emission (idempotent with `init`). No
/// lifecycle or runtime lock is held across the staged factory calls, which
/// dispatch to the main thread.
///
/// # Errors
///
/// Returns an error when the lifecycle is not `Stopped` or any fatal startup
/// stage fails.
pub fn start_runtime(app_handle: tauri::AppHandle) -> Result<(), String> {
    store_app_handle(app_handle);

    ensure_can_start()?;

    {
        let mut lifecycle = LIFECYCLE.lock();
        *lifecycle = LifecycleState::Starting;
    }

    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    let factory = RuntimeFactory::new(None);

    match build_runtime(&factory, generation) {
        Ok(runtime) => {
            *RUNTIME.lock() = RuntimeSlot::Running(runtime);
            *LIFECYCLE.lock() = LifecycleState::Running;
            tracing::info!("tiling: runtime {generation} started");
            Ok(())
        }
        Err(e) => {
            *LIFECYCLE.lock() = LifecycleState::Stopped;
            Err(e)
        }
    }
}

/// Restores Stache-hidden apps while the current generation's handle is
/// still published. Called before the runtime is taken from the slot so
/// `get_handle()` still resolves. Returns whether a restore was performed,
/// so the caller can mark the stage done on the runtime it takes.
fn restore_visibility_if_pending() -> bool {
    let pending = match &*RUNTIME.lock() {
        RuntimeSlot::Empty => false,
        RuntimeSlot::Running(rt) => !rt.teardown.visibility_restored,
        RuntimeSlot::Quarantined(partial) => !partial.teardown.visibility_restored,
    };
    if !pending {
        return false;
    }
    let summary = super::visibility::restore_stache_hidden_apps();
    tracing::info!(
        "tiling: restored {} of {} hidden apps",
        summary.restored,
        summary.attempted
    );
    true
}

/// Tears down the running tiling runtime in strict, idempotent order.
///
/// Stages:
/// 1. `restore_stache_hidden_apps()` before any actor teardown (idempotent;
///    returns an empty summary when no running handle exists).
/// 2. On the main thread, shutdown/gate/unregister app, screen, AX, and
///    standalone observers. All main-thread work completes before any wait.
/// 3. `EventProcessor::stop_and_wait` and routing-map clear.
/// 4. Cancel drag/animations; pause borders and the mouse tap.
/// 5. Subscriber shutdown + completion-latch wait.
/// 6. Actor shutdown + completion-latch wait.
/// 7. Drop all taken handles/Arcs.
/// 8. Clear tabs, AX caches, and transient animation/border state.
/// 9. Publish `RuntimeSlot::Empty` + `LifecycleState::Stopped`.
///
/// On timeout/failure the entire runtime (with per-stage progress) is
/// reinserted as `RuntimeSlot::Quarantined` and lifecycle stays `Stopping`;
/// a later call retries only the unfinished stages.
///
/// # Errors
///
/// Returns an error when the lifecycle is not running/stopping, or when a
/// teardown stage times out (the runtime is then quarantined for retry).
#[allow(clippy::too_many_lines)] // one block per ordered teardown stage, by design
pub fn pause_runtime() -> Result<(), String> {
    let retry = {
        let lifecycle = LIFECYCLE.lock();
        let state = *lifecycle;
        drop(lifecycle);
        match state {
            LifecycleState::Stopping => true,
            LifecycleState::Running => {
                *LIFECYCLE.lock() = LifecycleState::Stopping;
                false
            }
            LifecycleState::Stopped | LifecycleState::Starting => {
                return Err("tiling: pause called while not running".to_string());
            }
        }
    };

    let restored = restore_visibility_if_pending();

    let mut runtime = {
        let mut slot = RUNTIME.lock();
        let taken = std::mem::replace(&mut *slot, RuntimeSlot::Empty);
        drop(slot);
        match taken {
            RuntimeSlot::Empty if retry => {
                *LIFECYCLE.lock() = LifecycleState::Stopped;
                return Ok(());
            }
            RuntimeSlot::Empty => {
                return Err("tiling: no runtime to pause".to_string());
            }
            RuntimeSlot::Running(r) if !retry => PartialRuntime::from(r),
            RuntimeSlot::Quarantined(p) if retry => p,
            RuntimeSlot::Running(_) => {
                return Err("tiling: runtime is running during a teardown retry".to_string());
            }
            RuntimeSlot::Quarantined(_) => {
                return Err("tiling: runtime already quarantined".to_string());
            }
        }
    };

    if restored {
        runtime.teardown.visibility_restored = true;
    }

    if !runtime.teardown.main_thread_sources_removed {
        let app_monitor = runtime.app_monitor.clone();
        let screen_monitor = runtime.screen_monitor.clone();
        let ax_adapter = runtime.ax_adapter.clone();
        crate::platform::thread::dispatch_on_main_sync(move || {
            if let Some(m) = app_monitor {
                m.shutdown();
            }
            if let Some(s) = screen_monitor {
                s.shutdown();
            }
            if let Some(a) = ax_adapter {
                a.shutdown();
            }
            super::events::observer::shutdown();
        });
        runtime.teardown.main_thread_sources_removed = true;
    }

    if !runtime.teardown.processor_stopped {
        if let Some(processor) = &runtime.processor
            && !processor.stop_and_wait(stop_timeout())
        {
            *RUNTIME.lock() = RuntimeSlot::Quarantined(runtime);
            return Err("tiling: processor failed to stop within timeout".to_string());
        }
        runtime.teardown.processor_stopped = true;
    }

    if !runtime.teardown.transient_services_paused {
        super::events::drag_state::cancel_operation();
        super::events::mouse_monitor::set_active(false);
        if !super::effects::pause_animation_worker(stop_timeout()) {
            *RUNTIME.lock() = RuntimeSlot::Quarantined(runtime);
            return Err("tiling: animation worker did not stop within timeout".to_string());
        }
        super::effects::animation::reset_transient_state();
        super::borders::pause();
        runtime.teardown.transient_services_paused = true;
    }

    if !runtime.teardown.subscriber_stopped {
        if let Some(subscriber) = &runtime.subscriber {
            subscriber.shutdown();
        }
        if let Some(latch) = &runtime.subscriber_stopped
            && !latch.wait_timeout(stop_timeout())
        {
            *RUNTIME.lock() = RuntimeSlot::Quarantined(runtime);
            return Err("tiling: effect subscriber did not stop within timeout".to_string());
        }
        runtime.teardown.subscriber_stopped = true;
    }

    if !runtime.teardown.actor_stopped {
        if let Some(actor) = &runtime.actor {
            let _ = actor.send(StateMessage::Shutdown);
        }
        if let Some(latch) = &runtime.actor_stopped
            && !latch.wait_timeout(stop_timeout())
        {
            *RUNTIME.lock() = RuntimeSlot::Quarantined(runtime);
            return Err("tiling: state actor did not stop within timeout".to_string());
        }
        runtime.teardown.actor_stopped = true;
    }

    runtime.actor = None;
    runtime.actor_stopped = None;
    runtime.processor = None;
    runtime.subscriber = None;
    runtime.subscriber_stopped = None;
    runtime.app_monitor = None;
    runtime.screen_monitor = None;
    runtime.ax_adapter = None;

    if !runtime.teardown.caches_cleared {
        super::tabs::clear_all_tabs();
        super::effects::get_window_cache().clear();
        runtime.teardown.caches_cleared = true;
    }

    *LIFECYCLE.lock() = LifecycleState::Stopped;
    tracing::info!("tiling: runtime paused (generation {})", runtime.generation);
    Ok(())
}

/// Initializes the tiling state (screens, workspaces).
///
/// Detects screens on the main thread (where macOS APIs work) and sends
/// them to the actor via `SetScreens` message.
fn initialize_state(
    handle: &StateActorHandle,
    processor: &EventProcessor,
    subscriber: &EffectSubscriberHandle,
) {
    // Detect screens on the main thread (this is called during Tauri setup)
    // NSScreen APIs must be called from the main thread
    tracing::debug!("tiling: detecting screens on main thread...");
    let screens = super::actor::handlers::get_screens_from_macos();

    if screens.is_empty() {
        tracing::warn!("tiling: no screens detected during initialization");
        return;
    }

    tracing::debug!(
        "tiling: detected {} screen(s): {:?}",
        screens.len(),
        screens.iter().map(|s| &s.name).collect::<Vec<_>>()
    );

    processor.reconcile_screens(&screens);

    // Send pre-detected screens to the actor
    // This avoids calling macOS APIs from the async actor task
    if let Err(e) = handle.send(StateMessage::SetScreens { screens }) {
        tracing::error!("tiling: failed to send SetScreens message: {e}");
    }

    // Track existing windows
    track_existing_windows(handle, processor, subscriber);
}

/// Captures the exact application identity for a PID from one local
/// `NSRunningApplication` object inside an autorelease pool.
fn capture_identity_for_pid(pid: i32) -> Option<AppIdentity> {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    objc::rc::autoreleasepool(|| unsafe {
        let app: *mut Object = msg_send![
            class!(NSRunningApplication),
            runningApplicationWithProcessIdentifier: pid
        ];
        if app.is_null() {
            return None;
        }
        // The PID is authoritative here: it was resolved from window ownership,
        // so apps that report NSNotFound for `processIdentifier` still resolve.
        AppIdentity::from_ns_running_app_with_pid(app, pid)
    })
}

// ============================================================================
// Window Tracking
// ============================================================================

/// Tracks all existing windows at startup.
///
/// Enumerates all windows using the AX-first approach and sends
/// a batch `BatchWindowsCreated` message to the actor.
/// Also sends a `WindowFocused` message for the currently focused window,
/// and an `InitComplete` message to trigger initial layouts.
#[allow(clippy::too_many_lines)] // initial-scan enumeration, by design
fn track_existing_windows(
    handle: &StateActorHandle,
    processor: &EventProcessor,
    subscriber: &EffectSubscriberHandle,
) {
    use super::actor::WindowCreatedInfo;
    use super::rules::should_tile_window_with_rules;
    use super::window::{get_all_windows_including_hidden, get_focused_window_id};

    tracing::debug!("tiling: tracking existing windows...");

    // Get the currently focused window ID first (before enumeration)
    let focused_window_id = get_focused_window_id();
    tracing::trace!("tiling: system focused window id = {focused_window_id:?}");

    // Enumerate all windows including hidden ones
    let windows = get_all_windows_including_hidden();
    let ignore_rules = &get_config().tiling.ignore;

    tracing::debug!("Found {} windows from system", windows.len());
    for w in &windows {
        tracing::trace!(
            "  - id={}, pid={}, app='{}', title='{}', minimized={}, hidden={}",
            w.id,
            w.pid,
            w.app_name,
            w.title,
            w.is_minimized,
            w.is_hidden
        );
    }

    // Collect all trackable windows
    let mut window_infos: Vec<WindowCreatedInfo> = Vec::new();

    // Group windows by PID for tab detection
    // Collect unique PIDs and scan for tabs
    let mut pids_seen: std::collections::HashSet<i32> = std::collections::HashSet::new();
    for window in &windows {
        if should_tile_window_with_rules(
            &window.bundle_id,
            &window.app_name,
            &window.title,
            ignore_rules,
        ) {
            pids_seen.insert(window.pid);
        }
    }

    // Capture exact identities for every unique PID before building infos.
    let identities: std::collections::HashMap<i32, AppIdentity> = pids_seen
        .iter()
        .filter_map(|&pid| {
            let identity = capture_identity_for_pid(pid);
            identity.map(|identity| (pid, identity))
        })
        .collect();

    // Scan and register tabs for each app using the new TabRegistry approach
    for identity in identities.values() {
        crate::modules::tiling::tabs::scan_and_register_tabs_for_app(*identity);
    }

    for window in &windows {
        // Filter out system apps that shouldn't be tiled
        if !should_tile_window_with_rules(
            &window.bundle_id,
            &window.app_name,
            &window.title,
            ignore_rules,
        ) {
            tracing::trace!(
                "tiling: skipping system window '{}' from '{}'",
                window.title,
                window.app_name
            );
            continue;
        }

        let identity = identities.get(&window.pid).copied();

        // Check if this window is a tab (skip it - tabs are tracked separately)
        if identity.is_some_and(|identity| {
            crate::modules::tiling::tabs::is_tab_for_identity(window.id, identity)
        }) {
            continue;
        }

        // Create WindowCreatedInfo (tab detection is handled by the TabRegistry)
        let info = WindowCreatedInfo {
            window_id: window.id,
            pid: window.pid,
            identity,
            app_id: window.bundle_id.clone(),
            app_name: window.app_name.clone(),
            title: window.title.clone(),
            frame: window.frame,
            is_minimized: window.is_minimized,
            is_fullscreen: window.is_fullscreen,
            minimum_size: window.minimum_size,
            tab_group_id: None,
            is_active_tab: true,
        };

        window_infos.push(info);
    }

    let tracked_count = window_infos.len();

    // Also track these windows in the event processor for destroy detection
    // This is necessary because BatchWindowsCreated bypasses the processor
    let window_targets: Vec<(u32, AppIdentity, super::state::Rect)> = window_infos
        .iter()
        .filter_map(|w| w.identity.map(|identity| (w.window_id, identity, w.frame)))
        .collect();
    processor.track_windows_for_destroy_detection(&window_targets);

    // Resolve the initial focus identity BEFORE the batch consumes the infos.
    let focus_identity = focused_window_id.and_then(|window_id| {
        window_infos
            .iter()
            .find(|info| info.window_id == window_id)
            .and_then(|info| info.identity)
    });

    // Send batch message (no individual layout notifications)
    if !window_infos.is_empty()
        && let Err(e) = handle.send(StateMessage::BatchWindowsCreated(window_infos))
    {
        tracing::error!("tiling: failed to send BatchWindowsCreated: {e}");
    }

    tracing::debug!("tiling: tracked {tracked_count} windows");

    // Send focus event for the currently focused window (only with an exact
    // identity; otherwise fail closed and log).
    if let Some(window_id) = focused_window_id {
        if let Some(identity) = focus_identity {
            tracing::trace!("tiling: setting initial focus to window {window_id}");
            if let Err(e) = handle.send(StateMessage::WindowFocused { window_id, identity }) {
                tracing::error!("tiling: failed to send WindowFocused: {e}");
            }
        } else {
            tracing::warn!(
                "tiling: no identity for initially focused window {window_id}, skipping focus event"
            );
        }
    } else {
        tracing::trace!("tiling: no focused window detected at startup");
    }

    // Signal that initialization is complete - this triggers initial layouts
    tracing::trace!("tiling: sending InitComplete...");
    if let Err(e) = handle.send(StateMessage::InitComplete { subscriber: subscriber.clone() }) {
        tracing::error!("tiling: failed to send InitComplete: {e}");
    }
}

/// Adopts the windows an application already has when it launches.
///
/// A newly launched app can create its window before Stache attaches the app's
/// `AXObserver`, so no `AXWindowCreated` notification arrives. Scanning once at
/// launch closes that gap. Re-adopting an already-tracked window only refreshes
/// it, so this is safe to call alongside the observer.
pub fn adopt_existing_windows_for_instance(
    processor: &EventProcessor,
    identity: AppIdentity,
    pid: i32,
) {
    use super::actor::WindowCreatedInfo;
    use super::rules::should_tile_window_with_rules;
    use super::window::get_all_windows_including_hidden;

    let ignore_rules = &get_config().tiling.ignore;
    for window in get_all_windows_including_hidden() {
        if window.pid != pid {
            continue;
        }
        if !should_tile_window_with_rules(
            &window.bundle_id,
            &window.app_name,
            &window.title,
            ignore_rules,
        ) {
            continue;
        }
        if crate::modules::tiling::tabs::is_tab_for_identity(window.id, identity) {
            // Tabs are tracked by the tab registry, not as standalone windows.
            continue;
        }

        processor.on_window_created(WindowCreatedInfo {
            window_id: window.id,
            pid: window.pid,
            identity: Some(identity),
            app_id: window.bundle_id.clone(),
            app_name: window.app_name.clone(),
            title: window.title.clone(),
            frame: window.frame,
            is_minimized: window.is_minimized,
            is_fullscreen: window.is_fullscreen,
            minimum_size: window.minimum_size,
            tab_group_id: None,
            is_active_tab: true,
        });
    }
}

// ============================================================================
// App Handle Management
// ============================================================================

/// Stores the Tauri app handle for later use in event emission.
pub fn store_app_handle(handle: tauri::AppHandle) { *APP_HANDLE.lock() = Some(handle); }

/// Gets the stored app handle.
#[must_use]
pub fn get_app_handle() -> Option<tauri::AppHandle> { APP_HANDLE.lock().clone() }

// ============================================================================
// Event Emission Helpers
// ============================================================================

/// Emits a workspace changed event to the frontend.
pub fn emit_workspace_changed(workspace: &str, screen: &str, previous_workspace: Option<&str>) {
    if let Some(handle) = get_app_handle() {
        let _ = handle.emit(
            events::tiling::WORKSPACE_CHANGED,
            serde_json::json!({
                "workspace": workspace,
                "screen": screen,
                "previousWorkspace": previous_workspace,
            }),
        );
    }
}

/// Emits a window focus changed event to the frontend.
pub fn emit_window_focus_changed(window_id: u32, workspace: &str) {
    if let Some(handle) = get_app_handle() {
        let _ = handle.emit(
            events::tiling::WINDOW_FOCUS_CHANGED,
            serde_json::json!({
                "windowId": window_id,
                "workspace": workspace,
            }),
        );
    }
}

/// Emits a window tracked event to the frontend.
pub fn emit_window_tracked(window_id: u32, workspace: &str) {
    if let Some(handle) = get_app_handle() {
        let _ = handle.emit(
            events::tiling::WINDOW_TRACKED,
            serde_json::json!({
                "windowId": window_id,
                "workspace": workspace,
            }),
        );
    }
}

/// Emits a window untracked event to the frontend.
pub fn emit_window_untracked(window_id: u32, workspace: &str) {
    if let Some(handle) = get_app_handle() {
        let _ = handle.emit(
            events::tiling::WINDOW_UNTRACKED,
            serde_json::json!({
                "windowId": window_id,
                "workspace": workspace,
            }),
        );
    }
}

/// Emits a layout applied event to the frontend.
pub fn emit_layout_applied(workspace: &str, layout: &str, window_count: usize) {
    if let Some(handle) = get_app_handle() {
        let _ = handle.emit(
            events::tiling::LAYOUT_CHANGED,
            serde_json::json!({
                "workspace": workspace,
                "layout": layout,
                "windowCount": window_count,
            }),
        );
    }
}

/// Emits a window title changed event to the frontend.
pub fn emit_window_title_changed(window_id: u32, title: &str) {
    if let Some(handle) = get_app_handle() {
        let _ = handle.emit(
            events::tiling::WINDOW_TITLE_CHANGED,
            serde_json::json!({
                "windowId": window_id,
                "title": title,
            }),
        );
    }
}

/// Emits a workspace windows changed event to the frontend.
///
/// This is called when windows in a workspace change (added, removed, minimized, etc.).
pub fn emit_workspace_windows_changed(workspace: &str, window_ids: &[u32]) {
    if let Some(handle) = get_app_handle() {
        let _ = handle.emit(
            events::tiling::WORKSPACE_WINDOWS_CHANGED,
            serde_json::json!({
                "workspace": workspace,
                "windows": window_ids,
            }),
        );
    }
}

// ============================================================================
// IPC Query Handler
// ============================================================================

use crate::platform::ipc_socket::{IpcQuery, IpcResponse};

/// Handles IPC queries for tiling v2.
///
/// This is called by the IPC socket server when v2 is enabled.
/// Handles both v2-specific queries and standard queries (Screens, Workspaces, Windows).
/// Returns None if the query should be handled by v1 handler.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn handle_ipc_query(query: &IpcQuery) -> Option<IpcResponse> {
    match query {
        // Ping is universal - respond to confirm app is running
        IpcQuery::Ping => Some(IpcResponse::success("pong")),

        // V2-specific enabled check
        IpcQuery::V2Enabled => Some(IpcResponse::success(is_initialized() && is_enabled())),

        // Standard queries - handle when v2 is enabled
        IpcQuery::Screens => handle_screens_query(),

        IpcQuery::Workspaces { screen, focused_screen } => {
            handle_workspaces_query(screen.as_deref(), *focused_screen)
        }

        IpcQuery::Windows {
            screen,
            workspace,
            focused_screen,
            focused_workspace,
            ..
        } => handle_windows_query(
            screen.as_deref(),
            workspace.as_deref(),
            *focused_screen,
            *focused_workspace,
        ),

        IpcQuery::Apps => handle_apps_query(),

        IpcQuery::V2State => {
            if !is_initialized() {
                return Some(IpcResponse::error("Tiling v2 not initialized"));
            }

            let handle = get_handle()?;

            // Use blocking query for IPC (runs in dedicated thread)
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;

            rt.block_on(async {
                // Get enabled state
                let enabled = handle
                    .query(super::actor::StateQuery::GetEnabled)
                    .await
                    .ok()
                    .and_then(super::actor::QueryResult::into_enabled)
                    .unwrap_or(false);

                // Get focus state
                let focus = handle
                    .query(super::actor::StateQuery::GetFocusState)
                    .await
                    .ok()
                    .and_then(super::actor::QueryResult::into_focus);

                // Get counts
                let screens = handle
                    .query(super::actor::StateQuery::GetAllScreens)
                    .await
                    .ok()
                    .and_then(super::actor::QueryResult::into_screens)
                    .unwrap_or_default();

                let workspaces = handle
                    .query(super::actor::StateQuery::GetAllWorkspaces)
                    .await
                    .ok()
                    .and_then(super::actor::QueryResult::into_workspaces)
                    .unwrap_or_default();

                let windows = handle
                    .query(super::actor::StateQuery::GetAllWindows)
                    .await
                    .ok()
                    .and_then(super::actor::QueryResult::into_windows)
                    .unwrap_or_default();

                let (focused_workspace_id, focused_window_id) =
                    focus.map_or((None, None), |f| (f.focused_workspace_id, f.focused_window_id));

                Some(IpcResponse::success(serde_json::json!({
                    "isEnabled": enabled,
                    "screenCount": screens.len(),
                    "workspaceCount": workspaces.len(),
                    "windowCount": windows.len(),
                    "focusedWorkspaceId": focused_workspace_id.map(|id| id.to_string()),
                    "focusedWindowId": focused_window_id,
                })))
            })
        }

        IpcQuery::V2Screens => {
            if !is_initialized() {
                return Some(IpcResponse::error("Tiling v2 not initialized"));
            }

            let handle = get_handle()?;

            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;

            rt.block_on(async {
                let screens = handle
                    .query(super::actor::StateQuery::GetAllScreens)
                    .await
                    .ok()
                    .and_then(super::actor::QueryResult::into_screens)
                    .unwrap_or_default();

                let screen_infos: Vec<_> = screens
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "id": s.id,
                            "name": s.name,
                            "isMain": s.is_main,
                            "frame": {
                                "x": s.frame.x,
                                "y": s.frame.y,
                                "width": s.frame.width,
                                "height": s.frame.height,
                            },
                            "visibleFrame": {
                                "x": s.visible_frame.x,
                                "y": s.visible_frame.y,
                                "width": s.visible_frame.width,
                                "height": s.visible_frame.height,
                            },
                        })
                    })
                    .collect();

                Some(IpcResponse::success(screen_infos))
            })
        }

        IpcQuery::V2Workspaces => {
            if !is_initialized() {
                return Some(IpcResponse::error("Tiling v2 not initialized"));
            }

            let handle = get_handle()?;

            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;

            rt.block_on(async {
                let workspaces = handle
                    .query(super::actor::StateQuery::GetAllWorkspaces)
                    .await
                    .ok()
                    .and_then(super::actor::QueryResult::into_workspaces)
                    .unwrap_or_default();

                let workspace_infos: Vec<_> = workspaces
                    .iter()
                    .map(|ws| {
                        let layout = ws.layout;
                        serde_json::json!({
                            "id": ws.id.to_string(),
                            "name": ws.name,
                            "screenId": ws.screen_id,
                            "layout": layout.as_str(),
                            "isVisible": ws.is_visible,
                            "isFocused": ws.is_focused,
                            "windowCount": ws.window_ids.len(),
                            "windowIds": ws.window_ids,
                        })
                    })
                    .collect();

                Some(IpcResponse::success(workspace_infos))
            })
        }

        IpcQuery::V2Windows { workspace_id } => {
            if !is_initialized() {
                return Some(IpcResponse::error("Tiling v2 not initialized"));
            }

            let handle = get_handle()?;
            let workspace_filter = workspace_id.clone();

            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;

            rt.block_on(async {
                // Get focus state for marking focused window
                let focus = handle
                    .query(super::actor::StateQuery::GetFocusState)
                    .await
                    .ok()
                    .and_then(super::actor::QueryResult::into_focus);
                let focused_window_id = focus.and_then(|f| f.focused_window_id);

                // Get all windows
                let windows = handle
                    .query(super::actor::StateQuery::GetAllWindows)
                    .await
                    .ok()
                    .and_then(super::actor::QueryResult::into_windows)
                    .unwrap_or_default();

                // Filter by workspace if specified
                let filtered_windows: Vec<_> = windows
                    .iter()
                    .filter(|w| {
                        workspace_filter.as_ref().is_none_or(|id| w.workspace_id.to_string() == *id)
                    })
                    .map(|w| {
                        serde_json::json!({
                            "id": w.id,
                            "pid": w.pid,
                            "appId": w.app_id,
                            "appName": w.app_name,
                            "title": w.title,
                            "workspaceId": w.workspace_id.to_string(),
                            "frame": {
                                "x": w.frame.x,
                                "y": w.frame.y,
                                "width": w.frame.width,
                                "height": w.frame.height,
                            },
                            "isMinimized": w.is_minimized,
                            "isFullscreen": w.is_fullscreen,
                            "isFloating": w.is_floating,
                            "isFocused": focused_window_id == Some(w.id),
                        })
                    })
                    .collect();

                Some(IpcResponse::success(filtered_windows))
            })
        }
    }
}

// ============================================================================
// Standard Query Handlers (v1 API compatibility)
// ============================================================================

/// Handles the standard `screens` query using v2 state.
fn handle_screens_query() -> Option<IpcResponse> {
    if !is_initialized() {
        return Some(IpcResponse::error("Tiling v2 not initialized"));
    }

    let handle = get_handle()?;
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;

    rt.block_on(async {
        let screens = handle
            .query(super::actor::StateQuery::GetAllScreens)
            .await
            .ok()
            .and_then(super::actor::QueryResult::into_screens)
            .unwrap_or_default();

        // Format response to match v1 API format
        let screen_infos: Vec<_> = screens
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "name": s.name,
                    "isMain": s.is_main,
                    "isBuiltin": s.is_builtin,
                    "scaleFactor": s.scale_factor,
                    "frame": {
                        "x": s.frame.x,
                        "y": s.frame.y,
                        "width": s.frame.width,
                        "height": s.frame.height,
                    },
                    "visibleFrame": {
                        "x": s.visible_frame.x,
                        "y": s.visible_frame.y,
                        "width": s.visible_frame.width,
                        "height": s.visible_frame.height,
                    },
                })
            })
            .collect();

        Some(IpcResponse::success(screen_infos))
    })
}

/// Handles the standard `workspaces` query using v2 state.
#[allow(clippy::too_many_lines)]
fn handle_workspaces_query(screen: Option<&str>, focused_screen: bool) -> Option<IpcResponse> {
    if !is_initialized() {
        return Some(IpcResponse::error("Tiling v2 not initialized"));
    }

    let handle = get_handle()?;
    let screen_filter = screen.map(ToString::to_string);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;

    rt.block_on(async {
        // Get workspaces
        let workspaces = handle
            .query(super::actor::StateQuery::GetAllWorkspaces)
            .await
            .ok()
            .and_then(super::actor::QueryResult::into_workspaces)
            .unwrap_or_default();

        // Get focus state for focused_screen filter
        let focus = handle
            .query(super::actor::StateQuery::GetFocusState)
            .await
            .ok()
            .and_then(super::actor::QueryResult::into_focus);

        // Get all screens to map screen_id to screen name
        let screens = handle
            .query(super::actor::StateQuery::GetAllScreens)
            .await
            .ok()
            .and_then(super::actor::QueryResult::into_screens)
            .unwrap_or_default();

        // Determine focused screen ID
        let focused_screen_id = if focused_screen {
            focus.and_then(|f| {
                f.focused_workspace_id.and_then(|ws_id| {
                    workspaces.iter().find(|ws| ws.id == ws_id).map(|ws| ws.screen_id)
                })
            })
        } else {
            None
        };

        // Filter workspaces
        let filtered_workspaces: Vec<_> = workspaces
            .iter()
            .filter(|ws| {
                // Filter by screen name if provided
                if let Some(ref filter) = screen_filter {
                    let screen = screens.iter().find(|s| s.id == ws.screen_id);

                    // Handle special screen names
                    let matches = match filter.as_str() {
                        "main" | "primary" => screen.is_some_and(|s| s.is_main),
                        "secondary" => screen.is_some_and(|s| !s.is_main),
                        _ => screen.is_some_and(|s| s.name == *filter),
                    };

                    if !matches {
                        return false;
                    }
                }
                // Filter by focused screen if requested
                if focused_screen {
                    if let Some(focused_id) = focused_screen_id {
                        return ws.screen_id == focused_id;
                    }
                    return false;
                }
                true
            })
            .map(|ws| {
                let screen_id = ws.screen_id;
                let screen_name = screens
                    .iter()
                    .find(|s| s.id == ws.screen_id)
                    .map_or_else(|| format!("screen-{screen_id}"), |s| s.name.clone());

                let layout = ws.layout;
                serde_json::json!({
                    "id": ws.id.to_string(),
                    "name": ws.name,
                    "screenName": screen_name,
                    "layout": layout.as_str(),
                    "isVisible": ws.is_visible,
                    "isFocused": ws.is_focused,
                    "windowCount": ws.window_ids.len(),
                })
            })
            .collect();

        Some(IpcResponse::success(filtered_workspaces))
    })
}

/// Handles the standard `windows` query using v2 state.
#[allow(clippy::too_many_lines)]
fn handle_windows_query(
    screen: Option<&str>,
    workspace: Option<&str>,
    focused_screen: bool,
    focused_workspace: bool,
) -> Option<IpcResponse> {
    if !is_initialized() {
        return Some(IpcResponse::error("Tiling v2 not initialized"));
    }

    let handle = get_handle()?;
    let screen_filter = screen.map(ToString::to_string);
    let workspace_filter = workspace.map(ToString::to_string);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;

    rt.block_on(async {
        // Get all windows
        let windows = handle
            .query(super::actor::StateQuery::GetAllWindows)
            .await
            .ok()
            .and_then(super::actor::QueryResult::into_windows)
            .unwrap_or_default();

        // Get workspaces
        let workspaces = handle
            .query(super::actor::StateQuery::GetAllWorkspaces)
            .await
            .ok()
            .and_then(super::actor::QueryResult::into_workspaces)
            .unwrap_or_default();

        // Get screens
        let screens = handle
            .query(super::actor::StateQuery::GetAllScreens)
            .await
            .ok()
            .and_then(super::actor::QueryResult::into_screens)
            .unwrap_or_default();

        // Get focus state
        let focus = handle
            .query(super::actor::StateQuery::GetFocusState)
            .await
            .ok()
            .and_then(super::actor::QueryResult::into_focus);

        let focused_window_id = focus.as_ref().and_then(|f| f.focused_window_id);
        let focused_workspace_id = focus.as_ref().and_then(|f| f.focused_workspace_id);

        // Determine focused screen ID
        let focused_screen_id = focused_workspace_id
            .and_then(|ws_id| workspaces.iter().find(|ws| ws.id == ws_id).map(|ws| ws.screen_id));

        // Filter windows
        let filtered_windows: Vec<_> = windows
            .iter()
            .filter(|w| {
                // Get window's workspace
                let ws = workspaces.iter().find(|ws| ws.id == w.workspace_id);

                // Filter by workspace name
                if let Some(ref filter) = workspace_filter {
                    if let Some(ws) = ws {
                        if ws.name != *filter {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }

                // Filter by focused workspace
                if focused_workspace {
                    if let Some(focused_id) = focused_workspace_id {
                        if w.workspace_id != focused_id {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }

                // Filter by screen name
                if let Some(ref filter) = screen_filter {
                    if let Some(ws) = ws {
                        let screen = screens.iter().find(|s| s.id == ws.screen_id);

                        // Handle special screen names
                        let matches = match filter.as_str() {
                            "main" | "primary" => screen.is_some_and(|s| s.is_main),
                            "secondary" => screen.is_some_and(|s| !s.is_main),
                            _ => screen.is_some_and(|s| s.name == *filter),
                        };

                        if !matches {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }

                // Filter by focused screen
                if focused_screen
                    && let Some(ws) = ws
                    && let Some(focused_id) = focused_screen_id
                {
                    return ws.screen_id == focused_id;
                } else if focused_screen {
                    return false;
                }

                true
            })
            .map(|w| {
                let workspace_name = workspaces
                    .iter()
                    .find(|ws| ws.id == w.workspace_id)
                    .map(|ws| ws.name.clone())
                    .unwrap_or_default();

                serde_json::json!({
                    "id": w.id,
                    "pid": w.pid,
                    "appId": w.app_id,
                    "appName": w.app_name,
                    "title": w.title,
                    "workspace": workspace_name,
                    "frame": {
                        "x": w.frame.x,
                        "y": w.frame.y,
                        "width": w.frame.width,
                        "height": w.frame.height,
                    },
                    "isMinimized": w.is_minimized,
                    "isFullscreen": w.is_fullscreen,
                    "isFloating": w.is_floating,
                    "isFocused": focused_window_id == Some(w.id),
                })
            })
            .collect();

        Some(IpcResponse::success(filtered_windows))
    })
}

/// Handles the `apps` query - returns all running applications (excluding ignored apps).
#[allow(clippy::unnecessary_wraps)] // Matches other handler signatures
fn handle_apps_query() -> Option<IpcResponse> {
    use super::rules::should_tile_window;
    use super::window::get_running_apps;

    // Get all running apps
    let apps = get_running_apps();

    // Filter out apps that match ignore rules and format response
    let app_infos: Vec<_> = apps
        .iter()
        .filter(|app| should_tile_window(&app.bundle_id, &app.name))
        .map(|app| {
            serde_json::json!({
                "pid": app.pid,
                "name": app.name,
                "bundleId": app.bundle_id,
                "isHidden": app.is_hidden,
            })
        })
        .collect();

    Some(IpcResponse::success(app_infos))
}

// ============================================================================
// Mouse Up Callback (Drag Completion)
// ============================================================================

/// Called when the mouse button is released after a drag/resize operation.
///
/// This is registered as a callback with the mouse monitor and runs on
/// the mouse monitor thread (not the async runtime).
fn on_mouse_up() {
    use super::events::drag_state::{self, DragOperation};

    // Finish any ongoing operation
    let Some(info) = drag_state::finish_operation() else {
        return;
    };

    // Get the actor handle to send messages
    let Some(handle) = get_handle() else {
        return;
    };

    // Process the completed operation
    match info.operation {
        DragOperation::Move => handle_move_finished(&info, &handle),
        DragOperation::Resize => handle_resize_finished(&info, &handle),
    }
}

/// Handles the completion of a move operation.
///
/// For tiled windows:
/// - If dropped on another tiled window, swap them
/// - Otherwise, reapply the layout to snap back to position
///
/// For floating windows: leave them where they are.
fn handle_move_finished(info: &super::events::drag_state::DragInfo, handle: &StateActorHandle) {
    if !info.has_tiled_windows() {
        // All floating windows - nothing to snap back
        return;
    }

    // Get current window frames by querying the AX system directly
    let current_frames = get_current_frames_for_snapshots(&info.window_snapshots);

    // Check if a window was dragged onto another window for swapping
    if let Some((dragged_target, target_target)) =
        find_drag_swap_target(&info.window_snapshots, &current_frames)
    {
        // Send swap command (exact targets)
        let _ = handle.send(StateMessage::SwapWindows {
            target_a: dragged_target,
            target_b: target_target,
        });
        return;
    }

    // No swap target - send message to actor to trigger layout refresh
    // (going through actor ensures reliable processing from the mouse monitor thread)
    let _ = handle.send(StateMessage::UserMoveCompleted {
        workspace_id: info.workspace_id,
    });
}

/// Handles the completion of a resize operation.
///
/// For tiled windows: calculate new split ratios based on how the windows were resized.
/// For floating windows: just accept their new positions.
fn handle_resize_finished(info: &super::events::drag_state::DragInfo, handle: &StateActorHandle) {
    if !info.has_tiled_windows() {
        // All floating windows - nothing to do
        return;
    }

    // Get current window frames by querying the AX system directly
    let current_frames = get_current_frames_for_snapshots(&info.window_snapshots);

    // Find which window was resized
    let resized_info = find_resized_window(&info.window_snapshots, &current_frames);

    if let Some((target, old_frame, new_frame)) = resized_info {
        // Send the resize completion message with the exact target
        let _ = handle.send(StateMessage::UserResizeCompleted {
            workspace_id: info.workspace_id,
            target,
            old_frame,
            new_frame,
        });
    } else {
        // No significant resize detected - send message to actor to trigger layout refresh
        let _ = handle.send(StateMessage::UserMoveCompleted {
            workspace_id: info.workspace_id,
        });
    }
}

/// Gets current window frames by querying the AX system for each window in the snapshots.
///
/// This is used during mouse-up handling when we need current positions.
fn get_current_frames_for_snapshots(
    snapshots: &[super::events::drag_state::WindowSnapshot],
) -> Vec<(WindowTarget, super::state::Rect)> {
    use super::effects::window_ops;

    let mut frames = Vec::with_capacity(snapshots.len());

    for snapshot in snapshots {
        if let Some(frame) = window_ops::get_window_frame(snapshot.target) {
            frames.push((snapshot.target, frame));
        }
    }

    frames
}

/// Finds if a dragged window should be swapped with another window.
///
/// Returns `Some((dragged_target, target_target))` if a swap should occur.
fn find_drag_swap_target(
    snapshots: &[super::events::drag_state::WindowSnapshot],
    current_frames: &[(WindowTarget, super::state::Rect)],
) -> Option<(WindowTarget, WindowTarget)> {
    const MIN_DRAG_DISTANCE: f64 = 50.0;

    // Find which window was dragged (moved significantly from original position)
    let mut dragged: Option<(WindowTarget, super::state::Rect)> = None;
    let mut max_distance = 0.0f64;

    for snapshot in snapshots {
        if snapshot.is_floating {
            continue;
        }

        let Some((_, current_frame)) =
            current_frames.iter().find(|(t, _)| t.window_id == snapshot.target.window_id)
        else {
            continue;
        };

        let orig_center_x = snapshot.original_frame.x + snapshot.original_frame.width / 2.0;
        let orig_center_y = snapshot.original_frame.y + snapshot.original_frame.height / 2.0;
        let curr_center_x = current_frame.x + current_frame.width / 2.0;
        let curr_center_y = current_frame.y + current_frame.height / 2.0;

        let dx = curr_center_x - orig_center_x;
        let dy = curr_center_y - orig_center_y;
        let distance = dx.hypot(dy);

        if distance > max_distance && distance > MIN_DRAG_DISTANCE {
            max_distance = distance;
            dragged = Some((snapshot.target, *current_frame));
        }
    }

    let (dragged_target, dragged_frame) = dragged?;

    let dragged_center_x = dragged_frame.x + dragged_frame.width / 2.0;
    let dragged_center_y = dragged_frame.y + dragged_frame.height / 2.0;

    for snapshot in snapshots {
        if snapshot.target.window_id == dragged_target.window_id || snapshot.is_floating {
            continue;
        }

        let orig = &snapshot.original_frame;

        if dragged_center_x >= orig.x
            && dragged_center_x <= orig.x + orig.width
            && dragged_center_y >= orig.y
            && dragged_center_y <= orig.y + orig.height
        {
            return Some((dragged_target, snapshot.target));
        }
    }

    None
}

/// Finds which window was resized by comparing snapshots to current frames.
fn find_resized_window(
    snapshots: &[super::events::drag_state::WindowSnapshot],
    current_frames: &[(WindowTarget, super::state::Rect)],
) -> Option<(WindowTarget, super::state::Rect, super::state::Rect)> {
    let mut max_diff = 0.0f64;
    let mut resized: Option<(WindowTarget, super::state::Rect, super::state::Rect)> = None;

    for snapshot in snapshots {
        if snapshot.is_floating {
            continue;
        }

        let Some((_, current_frame)) =
            current_frames.iter().find(|(t, _)| t.window_id == snapshot.target.window_id)
        else {
            continue;
        };

        let width_diff = (current_frame.width - snapshot.original_frame.width).abs();
        let height_diff = (current_frame.height - snapshot.original_frame.height).abs();
        let size_diff = width_diff + height_diff;

        if size_diff > max_diff {
            max_diff = size_diff;
            resized = Some((snapshot.target, snapshot.original_frame, *current_frame));
        }
    }

    // Only return if there was a significant change (more than 5 pixels)
    if max_diff > 5.0 { resized } else { None }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_initialized_default() {
        // In a fresh test environment, this just verifies the function doesn't panic
        let _ = is_initialized();
    }

    #[test]
    fn test_is_enabled_reads_config() {
        // This just verifies the function doesn't panic
        let _ = is_enabled();
    }

    #[test]
    fn test_get_app_handle_without_store() {
        // Without storing, should return None
        // Note: this may be affected by other tests
        let _ = get_app_handle();
    }

    #[test]
    fn completion_latch_unmarked_times_out() {
        let latch = CompletionLatch::new();
        assert!(!latch.wait_timeout(Duration::from_millis(20)));
    }

    #[test]
    fn completion_latch_mark_complete_is_observable() {
        let latch = CompletionLatch::new();
        latch.mark_complete();
        assert!(latch.wait_timeout(Duration::from_millis(20)));
    }

    #[test]
    fn completion_latch_completion_persists_after_wait() {
        let latch = CompletionLatch::new();
        latch.mark_complete();
        assert!(latch.wait_timeout(Duration::from_millis(20)));
        assert!(latch.wait_timeout(Duration::from_millis(20)));
    }

    #[test]
    fn completion_latch_mark_complete_wakes_waiters() {
        let latch = CompletionLatch::new();
        let latch_for_thread = latch.clone();
        let thread = std::thread::spawn(move || {
            let _ = latch_for_thread.wait_timeout(Duration::from_secs(1));
        });
        std::thread::sleep(Duration::from_millis(10));
        latch.mark_complete();
        thread.join().unwrap();
    }

    #[test]
    fn completion_latch_repeatable_across_clones() {
        let latch = CompletionLatch::new();
        let clone = latch.clone();
        latch.mark_complete();
        assert!(clone.wait_timeout(Duration::from_millis(20)));
    }

    /// Serializes tests that mutate the process-global lifecycle statics.
    static TEST_LIFECYCLE_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    #[test]
    fn runtime_slot_is_empty_by_default() {
        let _guard = TEST_LIFECYCLE_LOCK.lock();
        assert!(matches!(&*RUNTIME.lock(), RuntimeSlot::Empty));
    }

    #[test]
    fn lifecycle_starts_stopped() {
        let _guard = TEST_LIFECYCLE_LOCK.lock();
        assert_eq!(*LIFECYCLE.lock(), LifecycleState::Stopped);
    }

    #[test]
    fn getters_return_none_before_start() {
        let _guard = TEST_LIFECYCLE_LOCK.lock();
        assert!(get_handle().is_none());
        assert!(get_processor().is_none());
        assert!(get_subscriber_handle().is_none());
        assert!(!is_initialized());
    }

    #[test]
    fn tiling_runtime_from_partial_requires_mandatory_fields() {
        let partial = PartialRuntime {
            generation: 1,
            actor: None,
            actor_stopped: None,
            processor: None,
            subscriber: None,
            subscriber_stopped: None,
            app_monitor: None,
            screen_monitor: None,
            ax_adapter: None,
            teardown: TeardownProgress::default(),
        };
        assert!(TilingRuntime::try_from(partial).is_err());
    }

    #[tokio::test]
    async fn runtime_factory_fails_named_stage_and_actor_exits_on_drop() {
        let failing = RuntimeFactory::new(Some(InitStage::Actor));
        assert!(failing.create_actor().is_err());

        let passing = RuntimeFactory::new(None);
        let (actor, stopped) = passing.create_actor().expect("actor stage must not fail");
        drop(actor);
        assert!(
            stopped.wait_timeout(Duration::from_secs(2)),
            "actor must stop after the last handle is dropped"
        );
    }

    #[test]
    fn ensure_can_start_rejects_running_and_quarantined() {
        let _guard = TEST_LIFECYCLE_LOCK.lock();
        assert!(ensure_can_start().is_ok());

        *LIFECYCLE.lock() = LifecycleState::Starting;
        assert!(ensure_can_start().is_err());
        *LIFECYCLE.lock() = LifecycleState::Stopped;

        *RUNTIME.lock() = RuntimeSlot::Quarantined(PartialRuntime {
            generation: 1,
            actor: None,
            actor_stopped: None,
            processor: None,
            subscriber: None,
            subscriber_stopped: None,
            app_monitor: None,
            screen_monitor: None,
            ax_adapter: None,
            teardown: TeardownProgress::default(),
        });
        assert!(ensure_can_start().is_err());
        *RUNTIME.lock() = RuntimeSlot::Empty;
    }

    #[test]
    fn pause_runtime_when_stopped_errors() {
        let _guard = TEST_LIFECYCLE_LOCK.lock();
        *LIFECYCLE.lock() = LifecycleState::Stopped;
        *RUNTIME.lock() = RuntimeSlot::Empty;
        assert!(pause_runtime().is_err(), "pause with no runtime must error");
    }

    #[test]
    fn pause_from_quarantined_retries_unfinished_stages_then_empties() {
        let _guard = TEST_LIFECYCLE_LOCK.lock();
        set_stop_timeout(Some(Duration::from_millis(30)));

        // Quarantined partial: only the subscriber latch is unfinished and it
        // never completes on the first attempt -> stays quarantined.
        let unfinished = CompletionLatch::new();
        let partial = PartialRuntime {
            generation: 7,
            actor: None,
            actor_stopped: None,
            processor: None,
            subscriber: None,
            subscriber_stopped: Some(unfinished.clone()),
            app_monitor: None,
            screen_monitor: None,
            ax_adapter: None,
            teardown: TeardownProgress {
                visibility_restored: true,
                main_thread_sources_removed: true,
                processor_stopped: true,
                transient_services_paused: true,
                ..TeardownProgress::default()
            },
        };
        *RUNTIME.lock() = RuntimeSlot::Quarantined(partial);
        *LIFECYCLE.lock() = LifecycleState::Stopping;

        let err = pause_runtime().unwrap_err();
        assert!(
            err.contains("subscriber"),
            "first attempt must time out on the subscriber latch: {err}"
        );
        assert!(
            matches!(&*RUNTIME.lock(), RuntimeSlot::Quarantined(_)),
            "unfinished runtime must stay quarantined"
        );
        assert_eq!(*LIFECYCLE.lock(), LifecycleState::Stopping);

        // Complete the latch and retry: teardown finishes and the slot empties.
        unfinished.mark_complete();
        pause_runtime().unwrap();
        assert!(matches!(&*RUNTIME.lock(), RuntimeSlot::Empty));
        assert_eq!(*LIFECYCLE.lock(), LifecycleState::Stopped);

        set_stop_timeout(None);
    }

    #[test]
    fn pause_from_quarantined_with_restore_pending_runs_restore_first() {
        let _guard = TEST_LIFECYCLE_LOCK.lock();
        set_stop_timeout(Some(Duration::from_millis(30)));

        // All stages except visibility_restored are marked done; the restore
        // call runs with no running handle and is a harmless empty no-op that
        // still advances the stage flag (idempotency with terminal cleanup).
        let partial = PartialRuntime {
            generation: 8,
            actor: None,
            actor_stopped: None,
            processor: None,
            subscriber: None,
            subscriber_stopped: Some(CompletionLatch::new()),
            app_monitor: None,
            screen_monitor: None,
            ax_adapter: None,
            teardown: TeardownProgress {
                main_thread_sources_removed: true,
                processor_stopped: true,
                transient_services_paused: true,
                ..TeardownProgress::default()
            },
        };
        *RUNTIME.lock() = RuntimeSlot::Quarantined(partial);
        *LIFECYCLE.lock() = LifecycleState::Stopping;

        // The subscriber latch below never completes; the error proves we got
        // past the restore + transient stages, and the quarantine is retained.
        let err = pause_runtime().unwrap_err();
        assert!(err.contains("subscriber"), "{err}");
        assert!(matches!(&*RUNTIME.lock(), RuntimeSlot::Quarantined(_)));
        assert_eq!(*LIFECYCLE.lock(), LifecycleState::Stopping);

        // Leave the globals clean so serialized siblings see Empty/Stopped.
        *RUNTIME.lock() = RuntimeSlot::Empty;
        *LIFECYCLE.lock() = LifecycleState::Stopped;

        set_stop_timeout(None);
    }

    #[test]
    fn pause_restores_then_teardown_is_idempotent_and_terminal_cleanup_empty() {
        let _guard = TEST_LIFECYCLE_LOCK.lock();
        set_stop_timeout(Some(Duration::from_millis(30)));

        // A quarantined generation with restore pending: the first pause runs
        // the public restore (empty summary — no running handle) and advances
        // the stage flag; teardown then times out on the unfinished subscriber
        // latch and retains the quarantine.
        let unfinished = CompletionLatch::new();
        let partial = PartialRuntime {
            generation: 9,
            actor: None,
            actor_stopped: None,
            processor: None,
            subscriber: None,
            subscriber_stopped: Some(unfinished.clone()),
            app_monitor: None,
            screen_monitor: None,
            ax_adapter: None,
            teardown: TeardownProgress {
                main_thread_sources_removed: true,
                processor_stopped: true,
                transient_services_paused: true,
                ..TeardownProgress::default()
            },
        };
        *RUNTIME.lock() = RuntimeSlot::Quarantined(partial);
        *LIFECYCLE.lock() = LifecycleState::Stopping;

        let err = pause_runtime().unwrap_err();
        assert!(err.contains("subscriber"), "{err}");
        assert!(matches!(&*RUNTIME.lock(), RuntimeSlot::Quarantined(_)));

        // Retry after completing the latch: teardown finishes, restore stage is
        // already marked done so nothing is restored a second time.
        unfinished.mark_complete();
        pause_runtime().unwrap();
        assert!(matches!(&*RUNTIME.lock(), RuntimeSlot::Empty));
        assert_eq!(*LIFECYCLE.lock(), LifecycleState::Stopped);

        // Terminal cleanup: a further pause on the stopped runtime errors and
        // performs no restore.
        assert!(pause_runtime().is_err());
        assert!(matches!(&*RUNTIME.lock(), RuntimeSlot::Empty));

        set_stop_timeout(None);
    }

    #[test]
    fn pause_restores_via_published_handle_before_teardown() {
        let _guard = TEST_LIFECYCLE_LOCK.lock();
        set_stop_timeout(Some(Duration::from_millis(30)));

        let identity = AppIdentity {
            pid: 42,
            launch_date: crate::modules::tiling::identity::LaunchDateBits::
                from_time_interval_since_reference_date(1.0)
                .unwrap(),
        };
        let registry = Arc::new(VisibilityRegistry::default());
        assert!(registry.insert(identity));

        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let handle = StateActorHandle::new_with_registry(tx, Arc::clone(&registry));
        let processor = Arc::new(EventProcessor::new(handle.clone()));
        let (_, subscriber, _subscriber_stopped) =
            EffectSubscriber::new(handle.clone(), EffectExecutor::new());
        let runtime = TilingRuntime {
            generation: 7,
            actor: handle,
            actor_stopped: CompletionLatch::new(),
            processor: Arc::clone(&processor),
            subscriber,
            subscriber_stopped: CompletionLatch::new(),
            app_monitor: Arc::new(AppMonitorAdapter::new(Arc::clone(&processor))),
            screen_monitor: Arc::new(ScreenMonitorAdapter::new(Arc::clone(&processor))),
            ax_adapter: Arc::new(AXObserverAdapter::new(Arc::clone(&processor))),
            teardown: TeardownProgress {
                main_thread_sources_removed: true,
                processor_stopped: true,
                transient_services_paused: true,
                subscriber_stopped: true,
                actor_stopped: true,
                caches_cleared: true,
                ..TeardownProgress::default()
            },
        };
        *RUNTIME.lock() = RuntimeSlot::Running(runtime);
        *LIFECYCLE.lock() = LifecycleState::Running;

        pause_runtime().unwrap();
        assert!(matches!(&*RUNTIME.lock(), RuntimeSlot::Empty));
        assert_eq!(*LIFECYCLE.lock(), LifecycleState::Stopped);

        // The published handle sealed and drained the populated registry.
        // This fails if pause restores after taking the runtime (get_handle
        // → None → empty summary → registry never sealed).
        assert!(registry.sealed());
        assert!(!registry.contains(&identity));
        assert!(registry.is_empty());

        set_stop_timeout(None);
    }
}
