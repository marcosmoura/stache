//! The Bar factory and the Bar-scoped screen-topology feed.
//!
//! # Ownership (overview Decision 18)
//!
//! Only the app applies a display topology to the registry, and only while Bar
//! runs. Tiling reads the registry's screen reservations and never applies
//! anything: with Bar off the topology is empty and Tiling sees "no host
//! topology" (zero host inset); with Bar running the topology lists every
//! attached display, so no attached display is `Absent`.
//!
//! The feed is a [`TopologyLease`] handed to the Bar module through the
//! `stache-bar` [`TopologySource`] seam, so `stache-bar` never depends on the
//! app. One lease lives exactly as long as one Bar runtime generation:
//!
//! - beginning it observes display reconfiguration, queries the layout once and
//!   applies the first topology;
//! - the observer callback only records a coalesced [`TopologyHint`] and wakes
//!   the worker; the worker re-queries (a bounded main-thread job, never from
//!   the callback), applies a new generation when the set of display IDs
//!   changed, and publishes the new reading Bar follows;
//! - ending it (explicitly, or by drop) applies an EMPTY topology first, before
//!   removing the observer or joining the worker.
//!
//! Every end of the generation reaches `end`: Bar's `stop` (disable, pause,
//! registry stop, permission revocation), a failed or partial start (Bar tears
//! its run down), and dropping the module. A stuck join or observer removal
//! never delays the empty topology and leaves the lease owned for an exact
//! retry.
//!
//! A topology generation is monotonic across leases (`generation` is shared by
//! the source), because the registry rejects a non-newer generation. A
//! numeric display ID that disappears and returns inside one coalesced
//! notification cannot be told apart from an unchanged display; macOS reports
//! such a change as a new notification, so the common unplug/replug sequence
//! applies an intermediate topology that omits the ID.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use stache_bar::{
    ArtworkStore, BarConfig, BarParts, BarRuntime, MediaParts, ProviderParts, TopologyLease,
    TopologyReading, TopologySource, TopologyWait, WeatherParts, WeatherService,
};
use stache_macos::{
    ApplicationControl, DisplayControl, DisplayObserver, MenuBarControl, TopologyHint,
};
use stache_sdk::{
    CancellationToken, DisplayId, MainThreadExecutor, ModuleId, Registry, ShutdownWait,
    TopologyGeneration,
};

use crate::access::SystemSettingsOpener;
use crate::bar_actions::BarActions;
use crate::bar_bridge::BarBridge;
use crate::bindings::{ModuleState, ModuleStatusView};
use crate::display::sdk_display_id;
use crate::host::{HostError, ModuleControl, ModuleFactory, register_module_observed};

/// The delay before the feed re-queries after a display query failed.
pub const QUERY_RETRY: Duration = Duration::from_secs(2);

/// The longest `end` waits for the display worker to finish.
///
/// The worker's own display query is bounded by the control's acknowledgement
/// budget; past this the lease reports "still owed" and stays owned for an
/// exact retry.
pub const FEED_JOIN: Duration = Duration::from_secs(2);

/// The registry the feed applies topologies to, bound by the host before any
/// module starts. A `Weak`, so the registry never owns itself through a module.
pub type RegistrySlot = Arc<OnceLock<Weak<Registry>>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Starts Bar-scoped topology leases over one display control.
pub struct HostTopologySource {
    control: DisplayControl,
    registry: RegistrySlot,
    /// The newest generation ever applied by any lease of this source.
    generation: Arc<AtomicU64>,
}

impl HostTopologySource {
    /// A source over `control`, applying to the registry in `registry`.
    #[must_use]
    pub fn new(control: DisplayControl, registry: RegistrySlot) -> Self {
        Self {
            control,
            registry,
            generation: Arc::new(AtomicU64::new(0)),
        }
    }
}

struct Applied {
    active: bool,
    displays: Vec<DisplayId>,
}

struct State {
    reading: TopologyReading,
    hint: Option<TopologyHint>,
    retry_at: Option<Instant>,
    stop: bool,
}

struct Shared {
    control: DisplayControl,
    registry: RegistrySlot,
    generation: Arc<AtomicU64>,
    state: Mutex<State>,
    wake: Condvar,
    /// Serializes "apply to the registry" against `end`, so no apply can land
    /// after the empty topology.
    gate: Mutex<Applied>,
    /// Serializes "query, then apply" for the primed layout and every hinted
    /// one: whichever applies last also queried last.
    order: Mutex<()>,
}

enum Refresh {
    Applied,
    QueryFailed,
}

impl Shared {
    fn apply(&self, displays: &[DisplayId]) -> Result<TopologyGeneration, String> {
        let Some(registry) = self.registry.get().and_then(Weak::upgrade) else {
            return Err("the host registry is not available".to_owned());
        };
        let next = TopologyGeneration::new(self.generation.load(Ordering::SeqCst) + 1);
        registry
            .apply_screen_topology(next, displays)
            .map_err(|error| format!("the display topology was refused: {error}"))?;
        self.generation.store(next.get(), Ordering::SeqCst);
        Ok(next)
    }

    fn publish(&self, layout: Option<Arc<stache_macos::DisplayLayout>>, error: Option<String>) {
        let primary = layout
            .as_ref()
            .and_then(|layout| layout.primary().map(|display| sdk_display_id(display.id)));
        let generation = TopologyGeneration::new(self.generation.load(Ordering::SeqCst));
        let mut state = lock(&self.state);
        let same = state.reading.generation == generation
            && state.reading.primary == primary
            && state.reading.error == error
            && match (&state.reading.layout, &layout) {
                (Some(held), Some(next)) => **held == **next,
                (None, None) => true,
                _ => false,
            };
        if !same {
            state.reading = TopologyReading {
                revision: state.reading.revision + 1,
                generation,
                layout,
                primary,
                error,
            };
        }
        drop(state);
        self.wake.notify_all();
    }

    /// Queries the layout and applies it. A failed query publishes an explicit
    /// "unavailable" reading and keeps the last applied topology: nothing is
    /// guessed, and Bar clears its panel and reservation on that reading.
    fn refresh(&self) -> Refresh {
        let _ordered = lock(&self.order);
        let queried = self.control.query().map_err(|error| error.to_string());
        let mut gate = lock(&self.gate);
        if !gate.active {
            return Refresh::Applied;
        }
        match queried {
            Err(reason) => {
                drop(gate);
                self.publish(None, Some(format!("the displays could not be read: {reason}")));
                Refresh::QueryFailed
            }
            Ok(layout) => {
                let displays: Vec<DisplayId> =
                    layout.displays().iter().map(|display| sdk_display_id(display.id)).collect();
                if displays != gate.displays {
                    match self.apply(&displays) {
                        Ok(_) => gate.displays = displays,
                        Err(reason) => {
                            drop(gate);
                            self.publish(None, Some(reason));
                            return Refresh::QueryFailed;
                        }
                    }
                }
                drop(gate);
                self.publish(Some(Arc::new(layout)), None);
                Refresh::Applied
            }
        }
    }

    /// Applies the empty topology once. Always runs before any join.
    fn apply_empty(&self) {
        let mut gate = lock(&self.gate);
        if gate.active {
            gate.active = false;
            if !gate.displays.is_empty() {
                // A refused empty topology means the registry is gone: nothing
                // is left to clear.
                let _ = self.apply(&[]);
                gate.displays.clear();
            }
        }
    }
}

fn work(shared: &Shared) {
    loop {
        {
            let mut state = lock(&shared.state);
            loop {
                if state.stop {
                    return;
                }
                if state.hint.take().is_some() {
                    break;
                }
                match state.retry_at {
                    Some(at) if Instant::now() >= at => {
                        state.retry_at = None;
                        break;
                    }
                    Some(at) => {
                        state = shared
                            .wake
                            .wait_timeout(state, at.saturating_duration_since(Instant::now()))
                            .unwrap_or_else(PoisonError::into_inner)
                            .0;
                    }
                    None => {
                        state = shared.wake.wait(state).unwrap_or_else(PoisonError::into_inner);
                    }
                }
            }
        }
        if matches!(shared.refresh(), Refresh::QueryFailed) {
            lock(&shared.state).retry_at = Some(Instant::now() + QUERY_RETRY);
        }
    }
}

/// One live feed for one Bar runtime generation.
pub struct HostTopologyLease {
    shared: Arc<Shared>,
    observer: Mutex<Option<DisplayObserver>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    ended: AtomicBool,
}

impl TopologySource for HostTopologySource {
    fn begin(&self) -> Result<Arc<dyn TopologyLease>, String> {
        let shared = Arc::new(Shared {
            control: self.control.clone(),
            registry: Arc::clone(&self.registry),
            generation: Arc::clone(&self.generation),
            state: Mutex::new(State {
                reading: TopologyReading {
                    revision: 0,
                    generation: TopologyGeneration::new(self.generation.load(Ordering::SeqCst)),
                    layout: None,
                    primary: None,
                    error: None,
                },
                hint: None,
                retry_at: None,
                stop: false,
            }),
            wake: Condvar::new(),
            gate: Mutex::new(Applied {
                active: true,
                displays: Vec::new(),
            }),
            order: Mutex::new(()),
        });
        let lease = Arc::new(HostTopologyLease {
            shared: Arc::clone(&shared),
            observer: Mutex::new(None),
            thread: Mutex::new(None),
            ended: AtomicBool::new(false),
        });
        // The callback only records a coalesced hint and wakes the worker.
        let sink = Arc::clone(&shared);
        let observer = self
            .control
            .observe_topology(move |hint| {
                let mut state = lock(&sink.state);
                if state.hint.is_none_or(|held| held.sequence < hint.sequence) {
                    state.hint = Some(hint);
                }
                drop(state);
                // `notify_all`: the condvar is shared with the lease's
                // `wait_change` waiters, so one wake could be taken by the
                // wrong waiter and the display worker would sleep through it.
                sink.wake.notify_all();
            })
            .map_err(|error| format!("the display observer could not start: {error}"))?;
        *lock(&lease.observer) = Some(observer);
        // Prime before the worker exists, so Bar's first reading is real.
        if matches!(shared.refresh(), Refresh::QueryFailed) {
            let reason = lock(&shared.state)
                .reading
                .error
                .clone()
                .unwrap_or_else(|| "the displays could not be read".to_owned());
            let _ = lease.end();
            return Err(reason);
        }
        let worker = Arc::clone(&shared);
        match std::thread::Builder::new()
            .name("stache-bar-displays".to_owned())
            .spawn(move || work(&worker))
        {
            Ok(handle) => *lock(&lease.thread) = Some(handle),
            Err(error) => {
                let _ = lease.end();
                return Err(format!("the display worker could not start: {error}"));
            }
        }
        Ok(lease)
    }
}

impl HostTopologyLease {
    /// Removes the display observer; it stays owned after a failed removal.
    #[allow(clippy::significant_drop_tightening)] // the guard owns the retry
    fn remove_observer(&self) -> bool {
        let mut observer = lock(&self.observer);
        match observer.as_ref() {
            Some(held) if held.stop().is_err() => false,
            Some(_) => {
                *observer = None;
                true
            }
            None => true,
        }
    }
}

impl TopologyLease for HostTopologyLease {
    fn read(&self) -> TopologyReading { lock(&self.shared.state).reading.clone() }

    fn wait_change(
        &self,
        after: u64,
        timeout: Duration,
        cancel: &[&CancellationToken],
    ) -> TopologyWait {
        let shared = &self.shared;
        let wake: Arc<dyn Fn() + Send + Sync> = {
            let shared = Arc::clone(shared);
            Arc::new(move || {
                // Taking the lock orders the notification after a waiter that
                // already checked the token and is about to sleep.
                drop(lock(&shared.state));
                shared.wake.notify_all();
            })
        };
        let _guards: Vec<_> =
            cancel.iter().map(|token| token.on_cancel(Arc::clone(&wake))).collect();
        let deadline = Instant::now() + timeout;
        let mut state = lock(&shared.state);
        loop {
            if cancel.iter().any(|token| token.is_cancelled()) {
                return TopologyWait::Cancelled;
            }
            if state.reading.revision != after {
                return TopologyWait::Changed(state.reading.clone());
            }
            let now = Instant::now();
            if now >= deadline {
                return TopologyWait::TimedOut;
            }
            state = shared
                .wake
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn end(&self) -> bool {
        // 1. The empty topology, before any removal or join can wait.
        self.shared.apply_empty();
        // 2. Stop and wake the worker.
        {
            lock(&self.shared.state).stop = true;
            self.shared.wake.notify_all();
        }
        self.ended.store(true, Ordering::SeqCst);
        // 3. The observer: stays owned after a failed removal.
        let mut done = self.remove_observer();
        // 4. The worker, bounded; never detached by a timeout.
        let mut thread = lock(&self.thread);
        if let Some(handle) = thread.as_ref() {
            let deadline = Instant::now() + FEED_JOIN;
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            if handle.is_finished() {
                if let Some(handle) = thread.take() {
                    let _ = handle.join();
                }
            } else {
                done = false;
            }
        }
        done
    }
}

impl Drop for HostTopologyLease {
    fn drop(&mut self) {
        if !self.ended.load(Ordering::SeqCst) {
            let _ = TopologyLease::end(self);
        }
    }
}

/// Registers the Bar module over the host-lifetime runtime service.
///
/// Construction is inert: nothing native is installed, no thread is spawned,
/// no window is opened and no topology is applied until an enabled module
/// starts. Registration installs the typed accepted-configuration watch the
/// status reads its committed revision from.
///
/// The production composition (`lib.rs`) appends [`BarFactory::production`],
/// default disabled, with the data bridge attached; the module's own `enabled`
/// setting starts it.
pub struct BarFactory {
    runtime: BarRuntime,
    registry: RegistrySlot,
    bridge: OnceLock<Arc<BarBridge>>,
    actions: Option<Arc<BarActions>>,
    weather: Option<Arc<WeatherService>>,
    media: Option<Arc<ArtworkStore>>,
}

impl BarFactory {
    /// A factory over a display control (a system control in production, a fake
    /// backend in tests). Bar runs with the standard time bounds.
    #[must_use]
    pub fn composed(control: DisplayControl) -> Self { Self::with_parts(control, |parts| parts) }

    /// Like [`composed`](Self::composed) with the parts adjusted (tests shorten
    /// the load deadline).
    #[must_use]
    pub fn with_parts(control: DisplayControl, adjust: impl FnOnce(BarParts) -> BarParts) -> Self {
        let registry: RegistrySlot = Arc::new(OnceLock::new());
        let source = HostTopologySource::new(control, Arc::clone(&registry));
        let parts = adjust(BarParts::new(Arc::new(source)));
        let media = parts
            .providers
            .as_ref()
            .and_then(|providers| providers.media.as_ref())
            .map(|media| Arc::clone(&media.store));
        Self {
            runtime: BarRuntime::new(parts),
            registry,
            bridge: OnceLock::new(),
            actions: None,
            weather: None,
            media,
        }
    }

    /// Gives the bridge its fixed click actions when it is attached.
    #[must_use]
    pub fn with_actions(mut self, actions: Arc<BarActions>) -> Self {
        self.actions = Some(actions);
        self
    }

    /// Gives the bridge its on-demand weather service over `parts` (the
    /// location source and the home/config directories the key file resolves
    /// against). Inert: no worker, timer or native handle exists until the Bar
    /// UI asks for weather.
    #[must_use]
    pub fn with_weather(mut self, parts: WeatherParts) -> Self {
        self.weather = Some(Arc::new(WeatherService::new(parts, self.runtime.status())));
        self
    }

    /// Attaches the host data bridge to this factory's status cell and ties its
    /// worker to the host shutdown stages.
    pub fn attach_bridge(&self, bridge: &Arc<BarBridge>) {
        if self.bridge.set(Arc::clone(bridge)).is_ok() {
            if let Some(actions) = &self.actions {
                bridge.set_actions(Arc::clone(actions));
            }
            if let Some(weather) = &self.weather {
                bridge.set_weather(Arc::clone(weather));
            }
            if let Some(media) = &self.media {
                bridge.set_media(Arc::clone(media));
            }
            bridge.attach(self.runtime.status());
        }
    }

    /// The production composition: a system display control over the host's
    /// main-thread executor. Inert until an enabled module starts.
    #[must_use]
    pub fn production(main_thread: Arc<dyn MainThreadExecutor>) -> Self {
        Self::production_with_media(main_thread, None)
    }

    /// Like [`production`](Self::production) with the media provider over
    /// `media` (`None` runs no media worker).
    #[must_use]
    pub fn production_with_media(
        main_thread: Arc<dyn MainThreadExecutor>,
        media: Option<MediaParts>,
    ) -> Self {
        let menu = MenuBarControl::system(Arc::clone(&main_thread));
        // Clicks reuse the shared application adapter and a fixed System
        // Settings opener; both are inert until a click arrives.
        let actions = Arc::new(BarActions::new(
            ApplicationControl::system(Arc::clone(&main_thread)),
            Arc::new(SystemSettingsOpener::new(Arc::clone(&main_thread))),
        ));
        Self::with_parts(DisplayControl::system(main_thread), |parts| {
            let mut providers = ProviderParts::system();
            providers.media = media;
            parts.with_menu(menu).with_providers(providers)
        })
        .with_actions(actions)
    }

    /// The runtime observation.
    #[must_use]
    pub fn status(&self) -> stache_bar::BarStatusSource { self.runtime.status() }
}

impl ModuleFactory for BarFactory {
    fn id(&self) -> ModuleId { BarConfig::ID }

    fn bind_registry(&self, registry: &Arc<Registry>) {
        let _ = self.registry.set(Arc::downgrade(registry));
        // The optional Keep Awake and Tiling projections observe the same
        // registry.
        if let Some(bridge) = self.bridge.get() {
            bridge.bind_keep_awake(registry);
            bridge.bind_tiling(registry);
        }
    }

    fn register(
        &self,
        registry: &Registry,
        store: &stache_sdk::ConfigStore,
    ) -> Result<Arc<dyn ModuleControl>, HostError> {
        let runtime = self.runtime.clone();
        register_module_observed(registry, store, runtime.module(), move |registry, handle| {
            runtime.install_accepted_watch(registry.accepted_watch(handle)?);
            Ok(())
        })
    }

    /// A running module whose panel failed after start is shown `unavailable`
    /// with the reason; the registry lifecycle itself is never rewritten.
    fn project_status(&self, view: &mut ModuleStatusView) -> bool {
        if view.id != BarConfig::ID.as_str() {
            return false;
        }
        let status = self.runtime.status().read();
        if view.state == ModuleState::Running && status.panel == stache_bar::BarPanelState::Failed {
            view.state = ModuleState::Unavailable;
            view.detail.clone_from(&status.detail);
        }
        view.bar = Some(status);
        true
    }

    fn begin_shutdown(&self) {
        if let Some(bridge) = self.bridge.get() {
            bridge.begin_shutdown();
        }
        self.runtime.begin_shutdown();
    }

    fn finish_shutdown(&self, _wait: &dyn ShutdownWait) -> Result<bool, String> {
        let bridge = self.bridge.get().is_none_or(|bridge| bridge.finish_shutdown());
        Ok(self.runtime.finish_shutdown() && bridge)
    }
}

#[cfg(test)]
#[path = "bar_tests.rs"]
mod tests;
