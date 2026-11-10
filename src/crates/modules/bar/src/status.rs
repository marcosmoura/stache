//! The Bar runtime observation.
//!
//! [`BarRuntimeStatus`] is sampled without a module, registry or lifecycle
//! lock and waits on no native work. It reports the truthful native phase of
//! the panel (queue acceptance is only `requested`), the zero-size and
//! no-primary states, the current reservation and frame, and any window whose
//! close was queued but is not acknowledged yet.

use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use specta::Type;
use stache_sdk::{AcceptedWatch, WindowPhase, WindowWatch};

use crate::config::BarConfig;
use crate::providers::{
    BatteryReading, CpuReading, MediaReading, MediaStatus, MediaUpdate, ProviderDiagnostic,
    ProviderKind, ProviderUpdate, WifiReading,
};
use crate::weather::WeatherStatus;

/// Where the Bar runtime is in its lifecycle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum BarRuntimeState {
    /// Nothing is installed, spawned or owned.
    #[default]
    Inactive,
    /// A start is in progress.
    Starting,
    /// The runtime is on.
    Running,
    /// A stop is in progress.
    Stopping,
    /// The last start failed; nothing is owned.
    Failed,
    /// Stopped, but a closed panel is not acknowledged or the topology feed
    /// still owes cleanup. The exact identities stay owned.
    CleanupPending,
}

/// What the panel is doing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum BarPanelState {
    /// No panel and no reason to have one (inactive).
    #[default]
    None,
    /// `height: 0` or a computed width `<= 0`: no panel, by design.
    NoPanelZeroSize,
    /// The host reports no primary display (or could not read the displays).
    NoPrimaryDisplay,
    /// The host queue accepted the open; no native window exists yet.
    Requested,
    /// The native window exists; its page has not loaded.
    Created,
    /// The page loaded.
    Loaded,
    /// Creation, page load or the wait failed; see `detail`.
    Failed,
}

/// The panel frame in logical points, top-left origin of the primary display.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BarFrameView {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

/// The screen reservation currently staged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BarReservationView {
    /// The reserved display (`CGDirectDisplayID`).
    pub display: u32,
    /// The constant top inset, `height + padding`, in points.
    pub top: u32,
}

/// The runtime observation published to the host.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BarRuntimeStatus {
    /// Lifecycle position.
    pub state: BarRuntimeState,
    /// Panel position.
    pub panel: BarPanelState,
    /// The accepted configuration revision as a decimal string.
    pub config_revision: String,
    /// The panel frame; present only while a panel exists.
    pub frame: Option<BarFrameView>,
    /// The staged reservation; present only while a panel exists.
    pub reservation: Option<BarReservationView>,
    /// The display topology generation the frame was computed against.
    pub topology_generation: Option<String>,
    /// Native window identities whose close is queued but not acknowledged.
    pub cleanup_pending: Vec<String>,
    /// The last failure or degraded condition.
    pub detail: Option<String>,
    /// Typed provider conditions (CPU, battery, Wi-Fi), outside the Bar
    /// visuals; deduplicated, one per provider at most.
    pub providers: Vec<ProviderDiagnostic>,
    /// The last weather location result and key-file condition (enums and a
    /// path only; never a coordinate or key). Changing it never changes the
    /// Bar view or its sequence.
    pub weather: WeatherStatus,
    /// The media adapter condition (`running`, `restarting`, `unavailable`, with
    /// a sanitized reason and stderr tail); `None` when no media worker runs.
    /// Never carries artwork bytes.
    pub media: Option<MediaStatus>,
}

/// Whether the system menu bar is on screen.
///
/// Legacy named the flag `menuHidden` but it was true when the menu bar was
/// VISIBLE (`Bar.state.ts:6-17`): a visible menu bar slides the Bar away
/// (`translateY(100%)`, `opacity: 0`). This type names what it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum MenuBarState {
    /// The menu bar is shown (always, or revealed by auto-hide).
    Visible,
    /// The menu bar is hidden (auto-hidden).
    Hidden,
    /// Nothing could be read, or the observation is not running. Legacy
    /// rendered this as "menu bar not visible" (the Bar shown); the UI decides.
    #[default]
    Unknown,
}

/// The menu-bar observation published with the Bar view.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MenuBarObservation {
    /// The state.
    pub state: MenuBarState,
    /// Why the state is unknown or degraded (held from an earlier sample,
    /// estimated from the window list); `None` when read directly.
    pub reason: Option<String>,
}

impl MenuBarObservation {
    /// The observation while nothing observes the menu bar.
    #[must_use]
    pub fn not_observed() -> Self {
        Self {
            state: MenuBarState::Unknown,
            reason: Some("the menu bar is not being observed".to_owned()),
        }
    }
}

/// The read-only view the host bridge projects (overview Decision 14).
///
/// It comes from the same published status cell as [`BarRuntimeStatus`], so
/// reading it takes no module, registry or native lock and never calls native
/// code. `(generation, sequence)` is the exact provider identity:
/// `generation` is the runtime generation of the latest start and `sequence`
/// increases by at least one for every observable change and never resets, so
/// a consumer discards anything that is not newer than what it holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BarPublished {
    /// The runtime generation (the latest start; `0` before any).
    pub generation: u64,
    /// The monotonic change sequence.
    pub sequence: u64,
    /// The accepted configuration revision as a decimal string; empty before
    /// the first acceptance.
    pub config_revision: String,
    /// Lifecycle position.
    pub state: BarRuntimeState,
    /// Panel position.
    pub panel: BarPanelState,
    /// The menu-bar observation.
    pub menu: MenuBarObservation,
    /// The panel frame; present only while a panel exists.
    pub frame: Option<BarFrameView>,
    /// The accepted `height` in points.
    pub height: u16,
    /// The accepted `padding` in points.
    pub padding: u16,
    /// The last failure or degraded condition.
    pub detail: Option<String>,
    /// The CPU reading; `None` until the first sample.
    pub cpu: Option<CpuReading>,
    /// The battery reading; `None` without a battery or before the first
    /// sample.
    pub battery: Option<BatteryReading>,
    /// The Wi-Fi reading; `None` until the first sample.
    pub wifi: Option<WifiReading>,
    /// The playing media; `None` when nothing plays, the adapter is
    /// unavailable or restarting, or no media worker runs. The artwork is an id
    /// (fetched with `bar_media_artwork`), never bytes.
    pub media: Option<MediaReading>,
    /// Counts location consent decisions that arrived after a weather read
    /// timed out. A change tells the UI to refetch the weather now. It carries
    /// no coordinate, key or decision.
    pub weather_epoch: u64,
}

/// The provider readings of the live monitor.
#[derive(Clone, Default, PartialEq, Eq)]
struct Providers {
    /// The exact monitor whose samples are accepted.
    epoch: Option<u64>,
    cpu: Option<CpuReading>,
    battery: Option<BatteryReading>,
    wifi: Option<WifiReading>,
    media: Option<MediaReading>,
    media_status: Option<MediaStatus>,
    diagnostics: Vec<ProviderDiagnostic>,
}

/// The part of the cell a consumer can observe; a change bumps the sequence.
#[derive(Clone, PartialEq, Eq)]
struct Observable {
    state: BarRuntimeState,
    panel: BarPanelState,
    frame: Option<BarFrameView>,
    reservation: Option<BarReservationView>,
    topology: Option<u64>,
    detail: Option<String>,
    generation: u64,
    menu: MenuBarObservation,
    height: u16,
    padding: u16,
    feed_owed: bool,
    providers: Providers,
    weather_epoch: u64,
}

type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct Inner {
    state: BarRuntimeState,
    panel: BarPanelState,
    window: Option<u64>,
    frame: Option<BarFrameView>,
    reservation: Option<BarReservationView>,
    topology: Option<u64>,
    detail: Option<String>,
    closing: Vec<WindowWatch>,
    feed_owed: bool,
    watch: Option<AcceptedWatch<BarConfig>>,
    generation: u64,
    sequence: u64,
    menu: MenuBarObservation,
    height: u16,
    padding: u16,
    providers: Providers,
    /// Outside [`Observable`]: a weather note never bumps the sequence.
    weather: WeatherStatus,
    /// Observable, and monotonic across runs: see [`BarPublished::weather_epoch`].
    weather_epoch: u64,
}

impl Inner {
    fn observable(&self) -> Observable {
        Observable {
            state: self.state,
            panel: self.panel.clone(),
            frame: self.frame,
            reservation: self.reservation,
            topology: self.topology,
            detail: self.detail.clone(),
            generation: self.generation,
            menu: self.menu.clone(),
            height: self.height,
            padding: self.padding,
            feed_owed: self.feed_owed,
            providers: self.providers.clone(),
            weather_epoch: self.weather_epoch,
        }
    }
}

/// The shared, cheaply cloned status cell.
#[derive(Clone, Default)]
pub struct BarStatusSource {
    inner: Arc<Mutex<Inner>>,
    subscribers: Arc<Mutex<Subscribers>>,
}

#[derive(Default)]
struct Subscribers {
    next: u64,
    wakes: Vec<(u64, Wake)>,
}

/// A registered change notification. Dropping it unregisters the callback.
pub struct StatusSubscription {
    id: u64,
    subscribers: std::sync::Weak<Mutex<Subscribers>>,
}

impl Drop for StatusSubscription {
    fn drop(&mut self) {
        if let Some(subscribers) = self.subscribers.upgrade() {
            subscribers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .wakes
                .retain(|(id, _)| *id != self.id);
        }
    }
}

impl BarStatusSource {
    /// Applies `update`; an observable change bumps the sequence and then
    /// wakes every subscriber after the cell lock is released.
    fn with<T>(&self, update: impl FnOnce(&mut Inner) -> T) -> T {
        let (value, changed) = {
            let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            let before = inner.observable();
            let value = update(&mut inner);
            let changed = inner.observable() != before;
            if changed {
                inner.sequence += 1;
            }
            drop(inner);
            (value, changed)
        };
        if changed {
            let wakes: Vec<Wake> = self
                .subscribers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .wakes
                .iter()
                .map(|(_, wake)| Arc::clone(wake))
                .collect();
            for wake in wakes {
                wake();
            }
        }
        value
    }

    /// Registers `wake`, called (outside the cell lock) after every observable
    /// change. It must only record and wake: no read of this cell's writers,
    /// no wait, no native call.
    #[must_use]
    pub fn subscribe(&self, wake: Wake) -> StatusSubscription {
        let mut subscribers = self.subscribers.lock().unwrap_or_else(PoisonError::into_inner);
        subscribers.next += 1;
        let id = subscribers.next;
        subscribers.wakes.push((id, wake));
        drop(subscribers);
        StatusSubscription {
            id,
            subscribers: Arc::downgrade(&self.subscribers),
        }
    }

    /// Reads the bridge view from the published cell. Never touches a module,
    /// registry or native lock.
    #[must_use]
    pub fn published(&self) -> BarPublished {
        let (mut view, watch) = {
            let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            let state = if inner.state == BarRuntimeState::Inactive
                && (inner.feed_owed
                    || inner.closing.iter().any(|w| w.phase().phase != WindowPhase::Closed))
            {
                BarRuntimeState::CleanupPending
            } else {
                inner.state
            };
            (
                BarPublished {
                    generation: inner.generation,
                    sequence: inner.sequence,
                    config_revision: String::new(),
                    state,
                    panel: inner.panel.clone(),
                    menu: inner.menu.clone(),
                    frame: inner.frame,
                    height: inner.height,
                    padding: inner.padding,
                    detail: inner.detail.clone(),
                    cpu: inner.providers.cpu,
                    battery: inner.providers.battery,
                    wifi: inner.providers.wifi.clone(),
                    media: inner.providers.media.clone(),
                    weather_epoch: inner.weather_epoch,
                },
                inner.watch.clone(),
            )
        };
        if let Some(accepted) = watch.and_then(|watch| watch.read()) {
            view.config_revision = accepted.revision.to_string();
        }
        view
    }

    /// Reads the point-in-time observation.
    ///
    /// Acknowledged closes are pruned here, so the list only names windows the
    /// host has not confirmed gone.
    #[must_use]
    pub fn read(&self) -> BarRuntimeStatus {
        let (mut status, watch) = self.with(|inner| {
            inner.closing.retain(|watch| watch.phase().phase != WindowPhase::Closed);
            let pending: Vec<String> =
                inner.closing.iter().map(|watch| watch.id().to_string()).collect();
            let state = if inner.state == BarRuntimeState::Inactive
                && (!pending.is_empty() || inner.feed_owed)
            {
                BarRuntimeState::CleanupPending
            } else {
                inner.state
            };
            (
                BarRuntimeStatus {
                    state,
                    panel: inner.panel.clone(),
                    config_revision: String::new(),
                    frame: inner.frame,
                    reservation: inner.reservation,
                    topology_generation: inner.topology.map(|generation| generation.to_string()),
                    cleanup_pending: pending,
                    detail: inner.detail.clone(),
                    providers: inner.providers.diagnostics.clone(),
                    weather: inner.weather.clone(),
                    media: inner.providers.media_status.clone(),
                },
                inner.watch.clone(),
            )
        });
        // The committed revision comes from the accepted-configuration watch,
        // never from a module-local counter.
        if let Some(accepted) = watch.and_then(|watch| watch.read()) {
            status.config_revision = accepted.revision.to_string();
        }
        status
    }

    pub(crate) fn install_watch(&self, watch: AcceptedWatch<BarConfig>) {
        self.with(|inner| inner.watch = Some(watch));
    }

    /// The accepted configuration and its revision, from the accepted watch;
    /// `None` before the first acceptance or without a watch.
    #[must_use]
    pub fn accepted_config(&self) -> Option<(u64, Arc<BarConfig>)> {
        let watch = self.inner.lock().unwrap_or_else(PoisonError::into_inner).watch.clone()?;
        watch.read().map(|accepted| (accepted.revision, accepted.config))
    }

    /// Records the weather conditions of runtime `generation`. `false` (and no
    /// change) when that run is no longer the live, running one. Not an
    /// observable change: no sequence bump, no bridge event.
    #[must_use]
    pub fn weather_note(&self, generation: u64, note: WeatherStatus) -> bool {
        self.with(|inner| {
            if inner.state == BarRuntimeState::Running && inner.generation == generation {
                inner.weather = note;
                true
            } else {
                false
            }
        })
    }

    /// A location consent decision arrived after a weather read timed out:
    /// bumps the weather epoch (an observable change, so the bridge emits) when
    /// runtime `generation` is still the live, running one. `false` (and no
    /// change) otherwise.
    #[must_use]
    pub fn weather_decided(&self, generation: u64) -> bool {
        self.with(|inner| {
            if inner.state == BarRuntimeState::Running && inner.generation == generation {
                inner.weather_epoch += 1;
                true
            } else {
                false
            }
        })
    }

    /// Whether runtime `generation` is still the live, running one.
    #[must_use]
    pub fn is_running(&self, generation: u64) -> bool {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.state == BarRuntimeState::Running && inner.generation == generation
    }

    pub(crate) fn starting(&self) {
        self.with(|inner| {
            inner.weather = WeatherStatus::default();
            inner.state = BarRuntimeState::Starting;
            inner.panel = BarPanelState::None;
            inner.detail = None;
            inner.menu = MenuBarObservation::not_observed();
            inner.providers = Providers::default();
        });
    }

    /// Names the runtime generation the current start belongs to.
    pub(crate) fn generation(&self, generation: u64) {
        self.with(|inner| inner.generation = generation);
    }

    /// Records the accepted `height`/`padding` the UI lays out with.
    pub(crate) fn accepted(&self, height: u16, padding: u16) {
        self.with(|inner| {
            inner.height = height;
            inner.padding = padding;
        });
    }

    /// Publishes the menu observation of runtime `generation`. A late publish
    /// of a stopped or superseded runtime changes nothing.
    pub(crate) fn menu(&self, generation: u64, menu: MenuBarObservation) {
        self.with(|inner| {
            let live = matches!(inner.state, BarRuntimeState::Starting | BarRuntimeState::Running);
            if inner.generation == generation && live {
                inner.menu = menu;
            }
        });
    }

    /// Nothing observes the menu bar (no panel, stopped).
    pub(crate) fn menu_off(&self) {
        self.with(|inner| inner.menu = MenuBarObservation::not_observed());
    }

    /// Accepts samples from the monitor `epoch` of the live runtime and clears
    /// every earlier reading. `false` when the runtime is no longer live.
    pub(crate) fn providers_on(&self, epoch: u64) -> bool {
        self.with(|inner| {
            let live = matches!(inner.state, BarRuntimeState::Starting | BarRuntimeState::Running);
            if live {
                inner.providers = Providers {
                    epoch: Some(epoch),
                    ..Providers::default()
                };
            }
            live
        })
    }

    /// Publishes one sample of the monitor `epoch`. A late publish of an ended
    /// or superseded monitor changes nothing. An unchanged sample bumps no
    /// sequence.
    pub(crate) fn provider_sample(
        &self,
        epoch: u64,
        kind: ProviderKind,
        update: Option<ProviderUpdate>,
        diagnostic: Option<String>,
    ) {
        self.with(|inner| {
            let providers = &mut inner.providers;
            if providers.epoch != Some(epoch) {
                return;
            }
            match update {
                Some(ProviderUpdate::Cpu(reading)) => providers.cpu = Some(reading),
                Some(ProviderUpdate::Battery(reading)) => providers.battery = reading,
                Some(ProviderUpdate::Wifi(reading)) => providers.wifi = Some(reading),
                None => {}
            }
            providers.diagnostics.retain(|note| note.provider != kind);
            if let Some(message) = diagnostic {
                providers.diagnostics.push(ProviderDiagnostic { provider: kind, message });
            }
            // A stable order, so an unchanged condition is no change.
            providers.diagnostics.sort_by_key(|note| note.provider as u8);
        });
    }

    /// Publishes the media worker's state of the monitor `epoch`. A late
    /// publish of an ended or superseded monitor changes nothing, and an
    /// unchanged state bumps no sequence.
    pub(crate) fn provider_media(&self, epoch: u64, update: MediaUpdate) {
        self.with(|inner| {
            let providers = &mut inner.providers;
            if providers.epoch != Some(epoch) {
                return;
            }
            providers.media = update.reading;
            providers.media_status = Some(update.status);
        });
    }

    /// The providers could not start: every provider reports why, and no
    /// sample is accepted.
    pub(crate) fn providers_failed(&self, reason: &str) {
        self.with(|inner| {
            inner.providers = Providers {
                epoch: None,
                diagnostics: [ProviderKind::Cpu, ProviderKind::Battery, ProviderKind::Wifi]
                    .into_iter()
                    .map(|provider| ProviderDiagnostic {
                        provider,
                        message: reason.to_owned(),
                    })
                    .collect(),
                ..Providers::default()
            };
        });
    }

    /// No provider runs: the readings and conditions are gone.
    pub(crate) fn providers_off(&self) {
        self.with(|inner| inner.providers = Providers::default());
    }

    pub(crate) fn running(&self) { self.with(|inner| inner.state = BarRuntimeState::Running); }

    /// A stop after a failed start keeps the failure visible until the next
    /// start clears it.
    pub(crate) fn stopping(&self) {
        self.with(|inner| {
            if inner.state != BarRuntimeState::Failed {
                inner.state = BarRuntimeState::Stopping;
            }
        });
    }

    /// Nothing is owned. `feed_owed` keeps `cleanupPending` truthful.
    pub(crate) fn stopped(&self, feed_owed: bool) {
        self.with(|inner| {
            if inner.state != BarRuntimeState::Failed {
                inner.state = BarRuntimeState::Inactive;
                inner.panel = BarPanelState::None;
                inner.detail = None;
            }
            inner.window = None;
            inner.frame = None;
            inner.reservation = None;
            inner.topology = None;
            inner.feed_owed = feed_owed;
            inner.menu = MenuBarObservation::not_observed();
            inner.providers = Providers::default();
            inner.weather = WeatherStatus::default();
        });
    }

    /// A start failed: nothing is owned and the reason stays visible.
    pub(crate) fn start_failed(&self, reason: String, feed_owed: bool) {
        self.with(|inner| {
            inner.state = BarRuntimeState::Failed;
            inner.panel = BarPanelState::Failed;
            inner.window = None;
            inner.frame = None;
            inner.reservation = None;
            inner.topology = None;
            inner.detail = Some(reason);
            inner.feed_owed = feed_owed;
            inner.menu = MenuBarObservation::not_observed();
            inner.providers = Providers::default();
            inner.weather = WeatherStatus::default();
        });
    }

    pub(crate) fn no_panel(&self, panel: BarPanelState, detail: Option<String>, topology: u64) {
        self.with(|inner| {
            inner.panel = panel;
            inner.window = None;
            inner.frame = None;
            inner.reservation = None;
            inner.topology = Some(topology);
            inner.detail = detail;
        });
    }

    pub(crate) fn panel_placed(&self, window: u64, frame: BarFrameView, topology: u64) {
        self.with(|inner| {
            if inner.window != Some(window) {
                inner.panel = BarPanelState::Requested;
            }
            inner.window = Some(window);
            inner.frame = Some(frame);
            inner.topology = Some(topology);
            inner.detail = None;
        });
    }

    pub(crate) fn reservation(&self, reservation: Option<BarReservationView>) {
        self.with(|inner| inner.reservation = reservation);
    }

    /// Records the native phase of `window` unless it is no longer the panel.
    pub(crate) fn phase(&self, window: u64, phase: &WindowPhase) {
        self.with(|inner| {
            if inner.window != Some(window) {
                return;
            }
            inner.panel = match phase {
                WindowPhase::Requested => BarPanelState::Requested,
                WindowPhase::Created => BarPanelState::Created,
                WindowPhase::Loaded => BarPanelState::Loaded,
                // Terminal phases are published together with their typed
                // reason by the runtime (`panel_failed`), never bare.
                WindowPhase::Failed(_) | WindowPhase::Closing | WindowPhase::Closed => return,
            };
        });
    }

    /// The panel failed after it existed: it is gone and the reason is typed.
    pub(crate) fn panel_failed(&self, reason: String) {
        self.with(|inner| {
            inner.panel = BarPanelState::Failed;
            inner.window = None;
            inner.frame = None;
            inner.reservation = None;
            inner.detail = Some(reason);
        });
    }

    pub(crate) fn detail(&self, detail: Option<String>) {
        self.with(|inner| inner.detail = detail);
    }

    pub(crate) fn closing(&self, watch: WindowWatch) {
        self.with(|inner| inner.closing.push(watch));
    }

    pub(crate) fn feed_owed(&self, owed: bool) { self.with(|inner| inner.feed_owed = owed); }
}
