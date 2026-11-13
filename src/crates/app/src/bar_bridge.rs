//! The host data bridge between the Bar module and its UI (overview
//! Decision 14).
//!
//! The SDK event bus is pull-only and lossy, so the host owns the delivery:
//! generated commands and one generated event carry concrete DTOs. This file
//! is the app-owned projection (`BarView`); the Bar crate publishes the data.
//!
//! # Sources: the Bar status cell plus one optional published-state projection
//!
//! The view comes from [`BarStatusSource::published`], the same published cell
//! [`ModuleFactory::project_status`](crate::host::ModuleFactory::project_status)
//! already reads. Bar's own data does not use the SDK `PublishedState`
//! channel: the bridge is host-to-UI for one module whose runtime already owns
//! a status cell that carries what the channel would add (runtime generation,
//! a monotonic sequence that never resets, the accepted config revision).
//! Reading takes the cell's short lock only: never a module, registry or
//! native lock, and never a native call.
//!
//! One additional source feeds [`BarView::keep_awake`]: Keep Awake, the first
//! production provider of the SDK published-state channel. Bar declares it in
//! `observes` (optional; its absence, disablement or failure never stops Bar)
//! but never imports its crate. The app-owned
//! [`KeepAwakeProjection`] observes the channel, validates schema, version,
//! generation and sequence, combines it with the registry's session facts and
//! yields the typed [`KeepAwakeIndicator`]. The view's `sequence` is Bar's own
//! sequence plus the projection's change counter, so a Keep Awake change is a
//! new, strictly greater sequence and emits [`BarChanged`] through the same
//! worker; both parts only grow, so the sum orders every view. Reads sample
//! in-memory cells only. There is no timer: the projection's watcher waits on the
//! published-state cell without a timeout and the host status sink calls
//! [`BarBridge::host_changed`] after every host lifecycle or session change.
//! A second projection, [`TilingProjection`], feeds [`BarView::tiling`] from
//! Tiling's `tilingState` the same way (see [`crate::bar_tiling`]); its change
//! counter is added to the sequence too, and `bar_tiling_action` runs a strip
//! click through Tiling's own typed focus path.
//! `bar_session_action` (generic host Pause/Resume, Bar limited to `keepAwake`)
//! is authorized like `bar_action` and executed by the host runner; see
//! [`crate::session_action`].
//!
//! # Ordering contract for the UI
//!
//! `generation` (the runtime generation of the latest start) and `sequence`
//! (+1 or more for every observable change, never reset, also across
//! generations) are exact `u64`s sent as decimal strings. Compare `sequence`
//! numerically (`BigInt`); it alone orders every view and event. The UI:
//!
//! 1. subscribes to [`BarChanged`], **then**
//! 2. calls `bar_snapshot`, and
//! 3. applies a snapshot or an event only if its `sequence` is greater than
//!    the one it holds, discarding anything older (a snapshot read before a
//!    newer event arrived is stale and is dropped by this rule).
//!
//! An event with `resnapshot: true` carries no view: changes were coalesced
//! beyond [`COALESCE_LIMIT`] or an earlier emission failed, so the UI calls
//! `bar_snapshot` again.
//!
//! # Delivery
//!
//! One bounded pull worker (`stache-bar-bridge`) turns the status cell's change
//! notification into [`BarChanged`] events. The notification is only a
//! "something changed" flag (coalescing: the worker always reads the newest
//! view, so a burst is one event). The worker exists only while a Bar run is
//! alive and exits once Bar is no longer running and nothing is pending;
//! disabled Bar owns none. Events go to the live Bar window label only, never a
//! broadcast.
//!
//! # Authorization
//!
//! Commands take the *actual* calling webview window (`tauri::WebviewWindow`,
//! injected by Tauri from the IPC origin), never a caller-supplied label or
//! surface. The label must be the live window `NativeWindows` holds for owner
//! `bar`. Otherwise:
//!
//! - not shaped `stache-bar-<n>` (test, access, shell, anything else):
//!   `unauthorized`;
//! - shaped like a Bar label but not live while Bar runs (a stale label after a
//!   panel reopen, or a forged one): `unauthorized`;
//! - shaped like a Bar label while Bar is not running: `unavailable` with the
//!   reason (the legitimate window may be racing its own teardown).
//!
//! `bar_snapshot`, `bar_action`, `bar_weather`, `bar_media_artwork`,
//! `bar_session_action` and `bar_tiling_action` are ACL-gated in the app
//! manifest (`build.rs`): only the `bar-window` capability
//! (`capabilities/bar.json`, `stache-bar-*`) grants them, together with
//! listening for the event. The label check above still applies. There is no
//! filesystem, shell, HTTP, launch or config write permission; the weather
//! command reads the env file and location natively inside the host.
//!
//! # Media artwork
//!
//! [`BarView::media`] carries the playing media with an `artworkId` (the
//! lowercase hex SHA-256 of the 128x128 PNG) and never the image: the view is
//! re-sent with every CPU tick, so the bytes would travel hundreds of times.
//! `bar_media_artwork(id)` answers [`BarArtworkOutcome`]: `ready` with the
//! base64 PNG text, `missing` (unknown, evicted, malformed or non-hex id; a bad
//! id never reaches the filesystem), `unauthorized` (label check, exactly like
//! `bar_weather`) or `unavailable`. The cache read runs on the blocking pool.
//!
//! # Weather: the secret and coordinate boundary
//!
//! `bar_weather` is the **only** place the Visual Crossing key and the device
//! coordinates exist outside the native adapters. It is an on-demand command
//! (one call per UI weather refresh: every 20 minutes and on reconnect); there
//! is no Rust weather timer or worker. Its response ([`BarWeatherOutcome`])
//! carries the key and coordinates for the authorized Bar window only. They
//! are never part of [`BarView`], [`BarChanged`], the Bar runtime status, the
//! host status or its `StatusChanged` event, logs, errors or `Debug` output.
//! What the host status gets is a typed diagnostic (an enum such as `denied`,
//! never a coordinate). It authorizes exactly like `bar_snapshot`, except that
//! a Bar that is running without a panel (zero size, no primary display, failed
//! panel) answers `unavailable` before any label check, and nothing is read. The
//! blocking work (a bounded location read and the env-file read) runs on the
//! blocking pool, never the main thread and never under a registry, module or
//! status lock, so a hung location read cannot delay `bar_snapshot` or
//! shutdown. A Bar stop, restart or configuration change during the read
//! answers `stale`; the UI refetches.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use specta::Type;
use stache_bar::{
    ArtworkLookup, ArtworkStore, BarConfig, BarFrameView, BarLayoutView, BarPanelState,
    BarPublished, BarRuntimeState, BarStatusSource, BarWeather, BatteryReading, CpuReading,
    MediaReading, MenuBarObservation, StatusSubscription, WeatherRead, WeatherService, WifiReading,
};

use crate::bar_actions::{ActionSource, BarAction, BarActionOutcome, BarActions};
use crate::bar_keep_awake::{KeepAwakeIndicator, KeepAwakeProjection, KeepAwakeView};
use crate::bar_tiling::{
    BarTilingOutcome, BarTilingRequest, BarTilingUnavailable, TilingProjection, TilingStripView,
    TilingView,
};
use crate::bindings::ModuleState;
use crate::host::HostHandle;
use crate::session_action::{
    SessionAction, SessionActionOutcome, SessionControl, SessionUnavailable, bar_may,
};
use crate::windows::NativeWindows;

/// The most changes one event may coalesce before the UI is told to take a
/// fresh snapshot instead of trusting the carried view.
pub const COALESCE_LIMIT: u64 = 64;

/// The longest the bridge worker is joined at shutdown.
pub const BRIDGE_JOIN: Duration = Duration::from_secs(2);

/// The prefix of every Bar panel label (`stache-<surface>-<n>`).
const BAR_LABEL_PREFIX: &str = "stache-bar-";

/// What the Bar UI renders from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BarView {
    /// The runtime generation as an exact decimal string.
    pub generation: String,
    /// The monotonic change sequence as an exact decimal string. Never parse
    /// it into a `number`.
    pub sequence: String,
    /// The accepted configuration revision as an exact decimal string; empty
    /// before the first acceptance.
    pub config_revision: String,
    /// What the panel is doing.
    pub panel: BarPanelState,
    /// Whether the system menu bar is on screen. `visible` slides the Bar
    /// away (legacy `menuHidden` is true here); see the adapter docs.
    pub menu: MenuBarObservation,
    /// The panel frame in logical points; present only while a panel exists.
    pub frame: Option<BarFrameView>,
    /// The accepted `height` in points.
    pub height: u32,
    /// The accepted `padding` in points.
    pub padding: u32,
    /// The accepted `[layout]` of `bar.toml`, normalized: every item has a
    /// unique `id`. Changes only with an accepted reload.
    pub layout: BarLayoutView,
    /// The last failure or degraded condition.
    pub detail: Option<String>,
    /// The CPU reading; absent until the first sample.
    pub cpu: Option<CpuReading>,
    /// The battery reading; absent without a battery or before the first
    /// sample (the component is hidden either way).
    pub battery: Option<BatteryReading>,
    /// The Wi-Fi reading; absent until the first sample.
    pub wifi: Option<WifiReading>,
    /// The playing media; absent when nothing plays, the adapter is
    /// unavailable or restarting. The artwork is an id, never bytes.
    pub media: Option<MediaReading>,
    /// A decimal-string counter that changes when a location consent decision
    /// arrives after a weather read timed out; the weather hook refetches on
    /// every change. No coordinate, key or decision is carried.
    pub weather_epoch: String,
    /// Keep Awake as the Bar renders it, or `null` until the first observation
    /// resolves (the legacy loading branch: nothing is drawn). `active` is true
    /// only for a confirmed adopted active assertion; every other state is
    /// `active: false`. `paused` and `enabled` are the host session facts for
    /// the click, never derived from the colour.
    pub keep_awake: Option<KeepAwakeIndicator>,
    /// The Tiling strip data, or `null` until the first observation resolves;
    /// once resolved it never returns to `null`. `absent`, `loading` or
    /// `available` (see [`crate::bar_tiling`]).
    pub tiling: Option<TilingStripView>,
}

/// Why the Bar is not serving data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum BarUnavailableReason {
    /// The module is disabled in `bar.toml`.
    Disabled,
    /// The module is paused for this session.
    Paused,
    /// The Accessibility launch gate (or another permission) holds it.
    WaitingForPermission,
    /// The module or its panel failed.
    Failed,
    /// A start is in progress.
    Starting,
    /// A stop is in progress.
    Stopping,
    /// Stopped, never started, or not registered.
    Stopped,
    /// A prerequisite is missing.
    Unavailable,
}

/// The typed answer of `bar_snapshot` (and the payload of [`BarChanged`]).
#[allow(clippy::large_enum_variant)] // one view per answer; boxing would change nothing on the wire
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BarSnapshotOutcome {
    /// The Bar is running; this is its view.
    Available {
        /// The view.
        view: BarView,
    },
    /// The Bar is not running; nothing stale is returned.
    #[serde(rename_all = "camelCase")]
    Unavailable {
        /// Why.
        reason: BarUnavailableReason,
        /// The failure detail, if any.
        detail: Option<String>,
        /// The runtime generation, as a decimal string.
        generation: String,
        /// The change sequence, as a decimal string.
        sequence: String,
    },
    /// The calling window may not read this feed.
    Unauthorized,
}

/// The typed answer of `bar_media_artwork`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BarArtworkOutcome {
    /// The artwork: base64 text of a 128x128 PNG (the UI builds the
    /// `data:image/png;base64,` URL).
    Ready {
        /// The base64 PNG.
        artwork: String,
    },
    /// No such artwork (unknown, evicted, or not a valid id).
    Missing,
    /// The calling window may not read artwork.
    Unauthorized,
    /// The Bar is not running, has no panel, or has no artwork store.
    #[serde(rename_all = "camelCase")]
    Unavailable {
        /// Why.
        reason: BarUnavailableReason,
        /// The failure detail, if any.
        detail: Option<String>,
    },
}

/// The typed answer of `bar_weather`. `ready` carries a secret and
/// coordinates: it goes to the authorized Bar window only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BarWeatherOutcome {
    /// The weather inputs for the currently accepted configuration.
    Ready {
        /// The key, location and configured provider.
        weather: BarWeather,
    },
    /// Bar is not serving weather; nothing was read.
    #[serde(rename_all = "camelCase")]
    Unavailable {
        /// Why.
        reason: BarUnavailableReason,
        /// The failure detail, if any.
        detail: Option<String>,
    },
    /// The calling window may not read weather.
    Unauthorized,
    /// Bar stopped, restarted or was reconfigured during the read; call again.
    Stale,
}

/// Emitted to the Bar window when the view changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type, tauri_specta::Event)]
#[serde(rename_all = "camelCase")]
pub struct BarChanged {
    /// The runtime generation, as a decimal string.
    pub generation: String,
    /// The change sequence, as a decimal string. Apply only if newer.
    pub sequence: String,
    /// Take a fresh `bar_snapshot`; `outcome` is absent.
    pub resnapshot: bool,
    /// The new state: `available` or `unavailable`, never `unauthorized`.
    pub outcome: Option<BarSnapshotOutcome>,
}

/// Delivers [`BarChanged`] to one window label.
pub trait BarEventSink: Send + Sync {
    /// Emits `event` to the window `label` only.
    ///
    /// # Errors
    /// Why the event could not be handed to the webview.
    fn emit(&self, label: &str, event: &BarChanged) -> Result<(), String>;
}

/// The published lifecycle state of modules, read without module locks.
pub trait ModuleStates: Send + Sync {
    /// The state of the module `id`, if it is registered.
    fn module_state(&self, id: &str) -> Option<ModuleState>;
}

impl ModuleStates for HostHandle {
    fn module_state(&self, id: &str) -> Option<ModuleState> {
        self.status()
            .modules
            .iter()
            .find(|module| module.id == id)
            .map(|module| module.state)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether `label` has the shape of a Bar panel label: `stache-bar-<digits>`.
#[must_use]
pub fn is_bar_label(label: &str) -> bool {
    label
        .strip_prefix(BAR_LABEL_PREFIX)
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit()))
}

/// The documented ordering rule the UI applies: `incoming` is applied only if
/// it is numerically greater than `held`. Both are exact decimal `u64`s; text
/// that does not parse is never newer.
#[must_use]
pub fn sequence_is_newer(held: &str, incoming: &str) -> bool {
    // `u64::from_str` accepts a leading `+`; the wire format is digits only.
    let exact = |text: &str| {
        if text.bytes().all(|byte| byte.is_ascii_digit()) {
            text.parse::<u64>()
        } else {
            "x".parse::<u64>()
        }
    };
    match (exact(held), exact(incoming)) {
        (Ok(held), Ok(incoming)) => incoming > held,
        (Err(_), Ok(_)) if held.is_empty() => true,
        _ => false,
    }
}

/// The optional projections sampled together for one view.
struct Projected {
    keep_awake: KeepAwakeView,
    tiling: TilingView,
}

/// The view's ordering number: Bar's own sequence plus the Keep Awake and
/// Tiling projections' change counters (each only grows).
const fn sequence_of(published: &BarPublished, projected: &Projected) -> u64 {
    published
        .sequence
        .saturating_add(projected.keep_awake.version)
        .saturating_add(projected.tiling.version)
}

fn view_of(published: &BarPublished, projected: &Projected) -> BarView {
    BarView {
        generation: published.generation.to_string(),
        sequence: sequence_of(published, projected).to_string(),
        config_revision: published.config_revision.clone(),
        panel: published.panel.clone(),
        menu: published.menu.clone(),
        frame: published.frame,
        height: u32::from(published.height),
        padding: u32::from(published.padding),
        layout: published.layout.clone(),
        detail: published.detail.clone(),
        cpu: published.cpu,
        battery: published.battery,
        wifi: published.wifi.clone(),
        media: published.media.clone(),
        weather_epoch: published.weather_epoch.to_string(),
        keep_awake: projected.keep_awake.indicator,
        tiling: projected.tiling.strip.clone(),
    }
}

struct Attached {
    status: BarStatusSource,
    _subscription: StatusSubscription,
}

#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent worker flags guarded by one lock"
)]
struct Pump {
    /// A change was recorded and not yet handled.
    pending: bool,
    /// The worker thread is alive (or being spawned).
    alive: bool,
    /// Shutdown began: no new worker, the current one exits.
    closed: bool,
    handle: Option<JoinHandle<()>>,
    /// The sequence last emitted and the label it went to.
    last: Option<(String, u64)>,
    /// The last emission failed: the next event is a resnapshot hint.
    dirty: bool,
}

/// The bridge. Cheap to share; construction is inert.
pub struct BarBridge {
    windows: Arc<NativeWindows>,
    modules: Arc<dyn ModuleStates>,
    sink: Arc<dyn BarEventSink>,
    attached: Mutex<Option<Attached>>,
    pump: Mutex<Pump>,
    wake: Condvar,
    weak: Weak<Self>,
    actions: OnceLock<Arc<BarActions>>,
    weather: OnceLock<Arc<WeatherService>>,
    media: OnceLock<Arc<ArtworkStore>>,
    session: OnceLock<Arc<dyn SessionControl>>,
    keep_awake: Arc<KeepAwakeProjection>,
    tiling: Arc<TilingProjection>,
}

impl BarBridge {
    /// A bridge with no Bar attached: every Bar-shaped caller gets
    /// `unavailable` and no worker exists.
    #[must_use]
    pub fn new(
        windows: Arc<NativeWindows>,
        modules: Arc<dyn ModuleStates>,
        sink: Arc<dyn BarEventSink>,
    ) -> Arc<Self> {
        Arc::new_cyclic(|weak: &Weak<Self>| {
            let keep_awake = KeepAwakeProjection::new();
            let waker = weak.clone();
            keep_awake.set_waker(Arc::new(move || {
                if let Some(bridge) = waker.upgrade() {
                    bridge.poke_if_running();
                }
            }));
            let tiling = TilingProjection::new();
            let waker = weak.clone();
            tiling.set_waker(Arc::new(move || {
                if let Some(bridge) = waker.upgrade() {
                    bridge.poke_if_running();
                }
            }));
            Self {
                windows,
                modules,
                sink,
                attached: Mutex::new(None),
                pump: Mutex::new(Pump::default()),
                wake: Condvar::new(),
                weak: weak.clone(),
                actions: OnceLock::new(),
                weather: OnceLock::new(),
                media: OnceLock::new(),
                session: OnceLock::new(),
                keep_awake,
                tiling,
            }
        })
    }

    /// Binds the host registry the Keep Awake projection observes. Optional: without it the view stays unresolved
    /// (`keepAwake: null`).
    pub fn bind_keep_awake(&self, registry: &Arc<stache_sdk::Registry>) {
        self.keep_awake.bind(registry);
    }

    /// Binds the host registry the Tiling projection observes. Optional:
    /// without it the view stays unresolved (`tiling: null`).
    pub fn bind_tiling(&self, registry: &Arc<stache_sdk::Registry>) { self.tiling.bind(registry); }

    /// Installs Tiling's typed exact-focus path (its health feeds the
    /// projection and `bar_tiling_action` runs through it). Set once.
    pub fn set_tiling_focus(&self, focus: Arc<stache_tiling::ipc::TilingFocus>) {
        self.tiling.set_focus(focus);
    }

    /// How many times the Tiling watcher woke up (tests: zero while idle).
    #[must_use]
    pub fn tiling_wakeups(&self) -> u64 { self.tiling.wakeups() }

    /// Whether the Tiling watcher thread exists (tests).
    #[must_use]
    pub fn tiling_watcher_started(&self) -> bool { self.tiling.worker_started() }

    /// The current Tiling projection, for tests and status.
    #[must_use]
    pub fn tiling_view(&self) -> TilingView { self.tiling.view() }

    fn projected(&self) -> Projected {
        Projected {
            keep_awake: self.keep_awake.view(),
            tiling: self.tiling.view(),
        }
    }

    /// How many times the Keep Awake watcher woke up (tests: zero while idle).
    #[must_use]
    pub fn keep_awake_wakeups(&self) -> u64 { self.keep_awake.wakeups() }

    /// The current Keep Awake projection, for tests and status.
    #[must_use]
    pub fn keep_awake_view(&self) -> KeepAwakeView { self.keep_awake.view() }

    /// Called by the host status sink after every host status publication
    /// (start, pause, resume, enable, disable, failure). Signal only: it never
    /// samples the registry, takes no projection lock and does not block; the
    /// bridge worker re-samples the Keep Awake projection and emits only a
    /// real change. Without it the pause and enable flags of a provider that
    /// is not running would never reach the Bar (they do not move its cell).
    pub fn host_changed(&self) { self.poke_if_running(); }

    /// A Keep Awake change wakes the worker only while Bar is running.
    fn poke_if_running(&self) {
        if self
            .status()
            .is_some_and(|status| status.published().state == BarRuntimeState::Running)
        {
            self.poke();
        }
    }

    /// Attaches the Bar runtime's status cell. The change callback only
    /// records a flag and (re)starts the bounded worker.
    pub fn attach(&self, status: BarStatusSource) {
        let weak = self.weak.clone();
        let subscription = status.subscribe(Arc::new(move || {
            if let Some(bridge) = weak.upgrade() {
                bridge.poke();
            }
        }));
        *lock(&self.attached) = Some(Attached {
            status,
            _subscription: subscription,
        });
    }

    fn status(&self) -> Option<BarStatusSource> {
        lock(&self.attached).as_ref().map(|attached| attached.status.clone())
    }

    /// The typed answer for the actual calling window `label`.
    #[must_use]
    pub fn snapshot(&self, label: &str) -> BarSnapshotOutcome {
        if !is_bar_label(label) {
            return BarSnapshotOutcome::Unauthorized;
        }
        let Some(status) = self.status() else {
            return Self::unattached();
        };
        let published = status.published();
        let live = self.windows.is_live_label(BarConfig::ID, label);
        if !live && published.state == BarRuntimeState::Running {
            // A stale label after a reopen, or a forged one.
            return BarSnapshotOutcome::Unauthorized;
        }
        self.outcome(&published, &self.projected())
    }

    fn unattached() -> BarSnapshotOutcome {
        BarSnapshotOutcome::Unavailable {
            reason: BarUnavailableReason::Stopped,
            detail: Some("the Bar module is not registered".to_owned()),
            generation: "0".to_owned(),
            sequence: "0".to_owned(),
        }
    }

    fn outcome(&self, published: &BarPublished, projected: &Projected) -> BarSnapshotOutcome {
        if published.state == BarRuntimeState::Running {
            return BarSnapshotOutcome::Available {
                view: view_of(published, projected),
            };
        }
        BarSnapshotOutcome::Unavailable {
            reason: self.reason(published),
            detail: published.detail.clone(),
            generation: published.generation.to_string(),
            sequence: sequence_of(published, projected).to_string(),
        }
    }

    /// Why a Bar that is not running serves nothing.
    fn reason(&self, published: &BarPublished) -> BarUnavailableReason {
        match self.modules.module_state(BarConfig::ID.as_str()) {
            Some(ModuleState::Disabled) => BarUnavailableReason::Disabled,
            Some(ModuleState::Paused) => BarUnavailableReason::Paused,
            Some(ModuleState::WaitingForPermission) => BarUnavailableReason::WaitingForPermission,
            Some(ModuleState::Failed) => BarUnavailableReason::Failed,
            Some(ModuleState::Unavailable) => BarUnavailableReason::Unavailable,
            Some(ModuleState::Running | ModuleState::Stopped) | None => match published.state {
                BarRuntimeState::Starting => BarUnavailableReason::Starting,
                BarRuntimeState::Stopping => BarUnavailableReason::Stopping,
                BarRuntimeState::Failed => BarUnavailableReason::Failed,
                BarRuntimeState::Inactive
                | BarRuntimeState::Running
                | BarRuntimeState::CleanupPending => BarUnavailableReason::Stopped,
            },
        }
    }

    /// Installs the click actions. Set once; construction stays inert.
    pub fn set_actions(&self, actions: Arc<BarActions>) { let _ = self.actions.set(actions); }

    /// Installs the weather service. Set once; construction stays inert.
    pub fn set_weather(&self, weather: Arc<WeatherService>) { let _ = self.weather.set(weather); }

    /// Installs the host session control behind `bar_session_action`. Set once.
    pub fn set_session(&self, session: Arc<dyn SessionControl>) {
        let _ = self.session.set(session);
    }

    /// Gives `bar_media_artwork` the store the media provider writes to.
    pub fn set_media(&self, store: Arc<ArtworkStore>) { let _ = self.media.set(store); }

    /// Answers `bar_media_artwork` for the calling window `label`. Blocks on a
    /// bounded cache read, so call it off the main thread.
    #[must_use]
    pub fn media_artwork(&self, label: &str, id: &str) -> BarArtworkOutcome {
        if !is_bar_label(label) {
            return BarArtworkOutcome::Unauthorized;
        }
        let Some(status) = self.status() else {
            return BarArtworkOutcome::Unavailable {
                reason: BarUnavailableReason::Stopped,
                detail: Some("the Bar module is not registered".to_owned()),
            };
        };
        let published = status.published();
        if published.state != BarRuntimeState::Running {
            return BarArtworkOutcome::Unavailable {
                reason: self.reason(&published),
                detail: published.detail,
            };
        }
        if !matches!(
            published.panel,
            BarPanelState::Requested | BarPanelState::Created | BarPanelState::Loaded
        ) {
            return BarArtworkOutcome::Unavailable {
                reason: if published.panel == BarPanelState::Failed {
                    BarUnavailableReason::Failed
                } else {
                    BarUnavailableReason::Unavailable
                },
                detail: published.detail.or_else(|| Some("the Bar has no panel".to_owned())),
            };
        }
        if !self.windows.is_live_label(BarConfig::ID, label) {
            return BarArtworkOutcome::Unauthorized;
        }
        let Some(store) = self.media.get() else {
            return BarArtworkOutcome::Unavailable {
                reason: BarUnavailableReason::Unavailable,
                detail: Some("Bar media is not available".to_owned()),
            };
        };
        match store.get(id) {
            // The window may have been replaced while the read was pending.
            ArtworkLookup::Found(_) if !self.windows.is_live_label(BarConfig::ID, label) => {
                BarArtworkOutcome::Unauthorized
            }
            ArtworkLookup::Found(artwork) => BarArtworkOutcome::Ready { artwork },
            ArtworkLookup::Missing => BarArtworkOutcome::Missing,
        }
    }

    /// Resolves the weather inputs for the actual calling window `label`.
    ///
    /// Authorized like [`snapshot`](Self::snapshot) (a non-Bar, stale or forged
    /// label is `unauthorized`), but a Bar that is not running, or is running
    /// without a panel, is `unavailable` before the label is checked and before
    /// anything is read. Blocks for at most the location timeout plus a small
    /// margin and does file I/O, so it must run off the main thread (the
    /// command spawns it) and never holds a lock the read path takes.
    #[must_use]
    pub fn weather(&self, label: &str) -> BarWeatherOutcome {
        if !is_bar_label(label) {
            return BarWeatherOutcome::Unauthorized;
        }
        let Some(status) = self.status() else {
            return BarWeatherOutcome::Unavailable {
                reason: BarUnavailableReason::Stopped,
                detail: Some("the Bar module is not registered".to_owned()),
            };
        };
        let published = status.published();
        if published.state != BarRuntimeState::Running {
            return BarWeatherOutcome::Unavailable {
                reason: self.reason(&published),
                detail: published.detail,
            };
        }
        if !matches!(
            published.panel,
            BarPanelState::Requested | BarPanelState::Created | BarPanelState::Loaded
        ) {
            return BarWeatherOutcome::Unavailable {
                reason: if published.panel == BarPanelState::Failed {
                    BarUnavailableReason::Failed
                } else {
                    BarUnavailableReason::Unavailable
                },
                detail: published.detail.or_else(|| Some("the Bar has no panel".to_owned())),
            };
        }
        if !self.windows.is_live_label(BarConfig::ID, label) {
            return BarWeatherOutcome::Unauthorized;
        }
        let Some(weather) = self.weather.get() else {
            return BarWeatherOutcome::Unavailable {
                reason: BarUnavailableReason::Unavailable,
                detail: Some("Bar weather is not available".to_owned()),
            };
        };
        match weather.read() {
            // The window may have been replaced while the read was pending.
            WeatherRead::Ready(_) if !self.windows.is_live_label(BarConfig::ID, label) => {
                BarWeatherOutcome::Stale
            }
            WeatherRead::Ready(weather) => BarWeatherOutcome::Ready { weather },
            WeatherRead::NotRunning(published) => BarWeatherOutcome::Unavailable {
                reason: self.reason(&published),
                detail: published.detail,
            },
            WeatherRead::Stale => BarWeatherOutcome::Stale,
        }
    }

    /// Runs one fixed click action for the actual calling window `label`.
    ///
    /// Authorized exactly like [`snapshot`](Self::snapshot): a non-Bar or stale
    /// or forged label is `unauthorized`; a Bar-shaped label while Bar is not
    /// running is `unavailable`. Blocks for at most the adapters' bounds and
    /// never takes a lock the read path takes, so it must run off the main
    /// thread (the command spawns it) and never delays `snapshot`.
    #[must_use]
    pub fn action(&self, label: &str, action: BarAction) -> BarActionOutcome {
        if !is_bar_label(label) {
            return BarActionOutcome::Unauthorized;
        }
        let Some(status) = self.status() else {
            return BarActionOutcome::Unavailable {
                reason: BarUnavailableReason::Stopped,
                detail: Some("the Bar module is not registered".to_owned()),
            };
        };
        let published = status.published();
        let live = self.windows.is_live_label(BarConfig::ID, label);
        if !live && published.state == BarRuntimeState::Running {
            return BarActionOutcome::Unauthorized;
        }
        if published.state != BarRuntimeState::Running {
            return BarActionOutcome::Unavailable {
                reason: self.reason(&published),
                detail: published.detail,
            };
        }
        let Some(actions) = self.actions.get() else {
            return BarActionOutcome::Unavailable {
                reason: BarUnavailableReason::Unavailable,
                detail: Some("Bar actions are not available".to_owned()),
            };
        };
        let source: Arc<dyn ActionSource> = Arc::new(LiveAction {
            status,
            generation: published.generation,
            windows: Arc::clone(&self.windows),
            label: label.to_owned(),
        });
        actions.run(action, &source)
    }

    /// Runs one generic host session action for the actual calling window
    /// `label`.
    ///
    /// Authorized like [`action`](Self::action): a non-Bar, stale or forged
    /// label is `unauthorized`, and so is every module or action the Bar
    /// capability does not hold ([`bar_may`]: Pause/Resume of `keepAwake` only),
    /// before anything reaches the host. A Bar-shaped label while Bar is not
    /// running is `unavailable`. The host decides and executes on its own
    /// runner (see [`crate::session_action`]) and the actual result is
    /// returned; blocks for at most the host bound, so it runs off the main
    /// thread.
    #[must_use]
    pub fn session_action(
        &self,
        label: &str,
        module: &str,
        action: SessionAction,
    ) -> SessionActionOutcome {
        let not_running = SessionActionOutcome::Unavailable {
            reason: SessionUnavailable::NotRunning,
        };
        if !is_bar_label(label) || !bar_may(module, action) {
            return SessionActionOutcome::Unauthorized;
        }
        let Some(status) = self.status() else {
            return not_running;
        };
        let running = status.published().state == BarRuntimeState::Running;
        if running && !self.windows.is_live_label(BarConfig::ID, label) {
            return SessionActionOutcome::Unauthorized;
        }
        let Some(session) = self.session.get().filter(|_| running) else {
            return not_running;
        };
        session.session_action(module, action)
    }

    /// Runs one Tiling strip click for the actual calling window `label`.
    ///
    /// Authorized like [`session_action`](Self::session_action): a non-Bar,
    /// stale or forged label is `unauthorized` before anything reaches Tiling,
    /// and a Bar-shaped label while Bar is not running is
    /// `unavailable{barNotRunning}`. Then the decimals are checked, Tiling's
    /// availability (a click never enables, resumes or reconfigures it), the
    /// rendered generation and sequence, and finally Tiling's own exact focus
    /// path resolves the key and revalidates at issuance. Blocks for at most
    /// Tiling's bounded wait, so it runs off the main thread; it takes no lock
    /// the read path takes, so it never delays `bar_snapshot` or `BarChanged`.
    #[must_use]
    pub fn tiling_action(&self, label: &str, request: &BarTilingRequest) -> BarTilingOutcome {
        let not_running = BarTilingOutcome::Unavailable {
            reason: BarTilingUnavailable::BarNotRunning,
        };
        if !is_bar_label(label) {
            return BarTilingOutcome::Unauthorized;
        }
        let Some(status) = self.status() else {
            return not_running;
        };
        let running = status.published().state == BarRuntimeState::Running;
        if running && !self.windows.is_live_label(BarConfig::ID, label) {
            return BarTilingOutcome::Unauthorized;
        }
        if !running {
            return not_running;
        }
        self.tiling.click(request)
    }

    /// Records a change and makes sure a worker will handle it. Cheap and
    /// non-blocking: it is called from the status mutators' threads.
    fn poke(&self) {
        let mut pump = lock(&self.pump);
        if pump.closed {
            return;
        }
        pump.pending = true;
        if !pump.alive {
            let Some(bridge) = self.weak.upgrade() else { return };
            match std::thread::Builder::new()
                .name("stache-bar-bridge".to_owned())
                .spawn(move || bridge.pump_loop())
            {
                Ok(handle) => {
                    pump.alive = true;
                    // A previous, exited worker's handle is already finished.
                    if let Some(old) = pump.handle.replace(handle) {
                        let _ = old.join();
                    }
                }
                // No worker: the next event is a resnapshot hint.
                Err(_) => pump.dirty = true,
            }
        }
        drop(pump);
        self.wake.notify_all();
    }

    fn pump_loop(&self) {
        loop {
            {
                let mut pump = lock(&self.pump);
                while !pump.pending && !pump.closed {
                    pump = self.wake.wait(pump).unwrap_or_else(PoisonError::into_inner);
                }
                if pump.closed {
                    pump.alive = false;
                    return;
                }
                pump.pending = false;
            }
            let running = self.pump_once();
            let mut pump = lock(&self.pump);
            if !pump.pending && !running {
                // Idle and Bar is down: no worker until the next change.
                pump.alive = false;
                return;
            }
            drop(pump);
        }
    }

    /// Reads the newest view and emits it to the live Bar window. Returns
    /// whether Bar is running.
    fn pump_once(&self) -> bool {
        let Some(status) = self.status() else { return false };
        let published = status.published();
        let running = published.state == BarRuntimeState::Running;
        if running {
            self.keep_awake.ensure_worker();
            self.tiling.ensure_worker();
        }
        let projected = self.projected();
        let sequence = sequence_of(&published, &projected);
        let Some(label) = self.windows.live_label(BarConfig::ID) else {
            return running;
        };
        let (resnapshot, skip) = {
            let pump = lock(&self.pump);
            let gap = match &pump.last {
                Some((last_label, last)) if *last_label == label => sequence.saturating_sub(*last),
                _ => 0,
            };
            let same = pump
                .last
                .as_ref()
                .is_some_and(|(last_label, last)| *last_label == label && *last == sequence);
            (pump.dirty || gap > COALESCE_LIMIT, same && !pump.dirty)
        };
        if skip {
            return running;
        }
        let event = BarChanged {
            generation: published.generation.to_string(),
            sequence: sequence.to_string(),
            resnapshot,
            outcome: (!resnapshot).then(|| self.outcome(&published, &projected)),
        };
        let delivered = self.sink.emit(&label, &event).is_ok();
        let mut pump = lock(&self.pump);
        pump.dirty = !delivered;
        if delivered {
            pump.last = Some((label, sequence));
        }
        running
    }

    /// Stage 1: no new worker; the current one exits. Never joins.
    pub fn begin_shutdown(&self) {
        lock(&self.pump).closed = true;
        self.keep_awake.begin_shutdown();
        self.tiling.begin_shutdown();
        self.wake.notify_all();
    }

    /// Stage 3: joins the worker within a bound. `false` keeps it owned.
    #[must_use]
    pub fn finish_shutdown(&self) -> bool {
        self.begin_shutdown();
        let keep_awake = self.keep_awake.finish_shutdown();
        let watcher = self.tiling.finish_shutdown() && keep_awake;
        // The worker takes the pump lock to exit, so it is never held while
        // waiting for it.
        let Some(handle) = lock(&self.pump).handle.take() else {
            return watcher;
        };
        let deadline = Instant::now() + BRIDGE_JOIN;
        while !handle.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        if handle.is_finished() {
            let _ = handle.join();
            watcher
        } else {
            lock(&self.pump).handle = Some(handle);
            false
        }
    }

    /// Whether the worker thread is alive, for tests and status.
    #[must_use]
    pub fn worker_alive(&self) -> bool { lock(&self.pump).alive }
}

/// The live authorization of one click: the Bar run that authorized it and the
/// window that sent it.
struct LiveAction {
    status: BarStatusSource,
    generation: u64,
    windows: Arc<NativeWindows>,
    label: String,
}

impl ActionSource for LiveAction {
    fn runtime_current(&self) -> bool {
        let published = self.status.published();
        published.state == BarRuntimeState::Running && published.generation == self.generation
    }

    fn caller_current(&self) -> bool { self.windows.is_live_label(BarConfig::ID, &self.label) }

    fn media_bundle(&self) -> Option<String> {
        self.status.published().media.map(|media| media.bundle_identifier)
    }

    fn watch(&self, wake: Arc<dyn Fn() + Send + Sync>) -> Box<dyn Send> {
        Box::new(self.status.subscribe(wake))
    }
}

/// Tauri-managed access to the bridge.
#[derive(Clone)]
pub struct BarBridgeState(pub Arc<BarBridge>);

/// Returns the Bar view to the Bar window that called.
///
/// `window` is the actual calling webview window injected by Tauri; the
/// bridge authorizes its label against the live Bar panel, never an argument.
/// Reads published cells only and never blocks.
#[tauri::command]
#[specta::specta]
#[allow(clippy::needless_pass_by_value)] // Tauri injects these by value.
#[must_use]
pub fn bar_snapshot(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, BarBridgeState>,
) -> BarSnapshotOutcome {
    state.0.snapshot(window.label())
}

/// Runs one fixed Bar click action for the Bar window that called.
///
/// `action` is a closed enum; the host maps it to a fixed target and accepts no
/// name, path, URL or argument. `window` is the actual calling webview window
/// injected by Tauri; the bridge authorizes its label against the live Bar
/// panel. The blocking native sequence runs on the blocking pool (never the
/// main thread) and is bounded; a click while one is in flight is `busy`.
#[tauri::command]
#[specta::specta]
pub async fn bar_action(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    action: BarAction,
) -> BarActionOutcome {
    use tauri::Manager as _;

    let bridge = Arc::clone(&app.state::<BarBridgeState>().0);
    let label = window.label().to_owned();
    tauri::async_runtime::spawn_blocking(move || bridge.action(&label, action))
        .await
        .unwrap_or_else(|error| BarActionOutcome::Unavailable {
            reason: BarUnavailableReason::Failed,
            detail: Some(format!("the action worker failed: {error}")),
        })
}

/// Pauses or resumes one module for this session, for the Bar window that
/// called.
///
/// `module` is validated strictly (only `keepAwake` is authorized for the Bar
/// capability) and `action` is a closed enum (`pause` | `resume`), so no other
/// action can even deserialize. The host decides from the current session
/// metadata at execution time and returns what actually happened; see
/// [`crate::session_action`]. The wait runs on the blocking pool, bounded.
#[tauri::command]
#[specta::specta]
pub async fn bar_session_action(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    module: String,
    action: SessionAction,
) -> SessionActionOutcome {
    use tauri::Manager as _;

    let bridge = Arc::clone(&app.state::<BarBridgeState>().0);
    let label = window.label().to_owned();
    tauri::async_runtime::spawn_blocking(move || bridge.session_action(&label, &module, action))
        .await
        .unwrap_or(SessionActionOutcome::Uncertain)
}

/// Focuses a Tiling workspace or window for the Bar window that called.
///
/// `request` is strict: a closed `action` enum (`focusWorkspace{name}` |
/// `focusWindow{window}`), the rendered Tiling `generation` and `sequence` as
/// exact decimal strings, and no other field. `window` is the actual calling
/// webview window injected by Tauri; the bridge authorizes its label against
/// the live Bar panel. The bounded wait runs on the blocking pool. No outcome
/// produces UI; the strip changes only through the next `BarChanged`.
#[tauri::command]
#[specta::specta]
pub async fn bar_tiling_action(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    request: BarTilingRequest,
) -> BarTilingOutcome {
    use tauri::Manager as _;

    let bridge = Arc::clone(&app.state::<BarBridgeState>().0);
    let label = window.label().to_owned();
    tauri::async_runtime::spawn_blocking(move || bridge.tiling_action(&label, &request))
        .await
        .unwrap_or(BarTilingOutcome::Uncertain)
}

/// Resolves the weather inputs (provider, key, location) for the Bar window
/// that called.
///
/// `window` is the actual calling webview window injected by Tauri; the bridge
/// authorizes its label against the live Bar panel. The response carries the
/// API key and coordinates and exists nowhere else. The location read and the
/// env-file read run on the blocking pool (never the main thread), bounded.
#[tauri::command]
#[specta::specta]
pub async fn bar_weather(window: tauri::WebviewWindow, app: tauri::AppHandle) -> BarWeatherOutcome {
    use tauri::Manager as _;

    let bridge = Arc::clone(&app.state::<BarBridgeState>().0);
    let label = window.label().to_owned();
    tauri::async_runtime::spawn_blocking(move || bridge.weather(&label))
        .await
        .unwrap_or_else(|error| BarWeatherOutcome::Unavailable {
            reason: BarUnavailableReason::Failed,
            detail: Some(format!("the weather worker failed: {error}")),
        })
}

#[tauri::command]
#[specta::specta]
pub async fn bar_media_artwork(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    id: String,
) -> BarArtworkOutcome {
    use tauri::Manager as _;

    let bridge = Arc::clone(&app.state::<BarBridgeState>().0);
    let label = window.label().to_owned();
    tauri::async_runtime::spawn_blocking(move || bridge.media_artwork(&label, &id))
        .await
        .unwrap_or_else(|error| BarArtworkOutcome::Unavailable {
            reason: BarUnavailableReason::Failed,
            detail: Some(format!("the artwork worker failed: {error}")),
        })
}
