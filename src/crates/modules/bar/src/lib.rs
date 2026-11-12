//! Stache Bar.
//!
//! [`BarConfig`] is the single authoritative source for `bar.toml`, its
//! schema, template and legacy migration. The crate also owns the Bar backend
//! runtime: [`BarRuntime`] / [`BarModule`] open one non-activating panel on the
//! primary display (`window`), publish a constant top screen reservation and
//! follow the host's Bar-scoped display feed (`topology`), reporting a typed
//! [`BarRuntimeStatus`].
//!
//! The CPU, battery, Wi-Fi and (when injected) media data providers (`providers`) run only while a run
//! has a panel, one owned schedule and worker thread each, and publish into the
//! same status cell and sequence as the panel state.
//!
//! Weather (`weather`) has no worker: the UI asks per refresh and
//! [`WeatherService`] resolves the key file and one location read on demand.
//!
//! The production factory is chained in the host (part 04) and default
//! disabled, so `enabled = true` in `bar.toml` starts Bar and `enabled = false`
//! (the default) starts nothing. The panel loads the
//! `bar` surface through the SDK window capability; this crate imports no other
//! module crate and does not depend on the app.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod config;
mod layout;
mod menubar;
mod module;
pub mod providers;
mod runtime;
mod status;
mod topology;
mod weather;
mod window;

#[cfg(test)]
mod tests;

pub use config::{BarConfig, WeatherConfig, WeatherProvider};
pub use layout::{
    BarEntryFields, BarLayoutConfig, BarLayoutEntry, BarLayoutEntryTable, BarLayoutItem,
    BarLayoutView, BarWidgetKind, ClockOptions, CpuOptions, HourCycle, NoOptions,
};
pub use menubar::{MENU_JOIN, MENU_POLL, MENU_RETRY, MenuMonitor, Schedule, next_wake};
pub use module::{BarModule, BarRuntime, KEEP_AWAKE_PROVIDER, TILING_PROVIDER};
pub use providers::{
    ArtworkLookup, ArtworkStore, BatteryChargeState, BatteryReading, CpuReading, MediaParts,
    MediaPaths, MediaReading, MediaResolver, MediaRunState, MediaStatus, MediaTiming, MediaUpdate,
    ProviderDiagnostic, ProviderKind, ProviderMonitor, ProviderParts, ProviderTiming, PublishMedia,
    SSID_GATE_ENV, SsidGate, WifiReading, WifiSignals, WifiStatus,
};
pub use runtime::{BAR_SURFACE, BarParts, BarTiming, PANEL_LOAD_DEADLINE};
pub use status::{
    BarFrameView, BarPanelState, BarPublished, BarReservationView, BarRuntimeState,
    BarRuntimeStatus, BarStatusSource, MenuBarObservation, MenuBarState, StatusSubscription,
};
pub use topology::{TopologyLease, TopologyReading, TopologySource, TopologyWait};
pub use weather::{
    ApiKeyDiagnostic, ApiKeyProblem, ApiKeyRead, BarWeather, BarWeatherLocation,
    BarWeatherProvider, JOIN_MARGIN, KEY_NAME, LocationDiagnostic, MAX_ENV_FILE_BYTES,
    REUSE_WINDOW, WeatherParts, WeatherRead, WeatherService, WeatherStatus, WeatherTiming,
    parse_api_key, read_api_key,
};
pub use window::{Desired, PanelFrame, plan};
