//! The Bar system providers: CPU, battery and Wi-Fi.
//!
//! Clock stays UI-local (a deterministic time-derived value in the UI), so it
//! has no provider here.
//!
//! # Ownership and lifecycle
//!
//! A [`ProviderMonitor`] is the providers' counterpart of the menu-bar
//! monitor: it exists only while one Bar run **has a panel** and owns exactly
//! one worker thread per provider (CPU, battery, Wi-Fi). Disabled, paused,
//! zero-size, no-primary-display and failed-panel runs own no provider thread,
//! no timer, no child process and no native handle. The runtime starts it where
//! it starts the menu monitor and ends it where it ends it (see `runtime.rs`).
//!
//! # Schedule
//!
//! Each provider has **one owned sample schedule** and its own thread, so a
//! slow or hung read of one provider never delays another provider, the host,
//! the main thread or a bridge read. A schedule is [`next_due`]: the next
//! sample starts one period after the previous one *started*, but never before
//! the previous one *finished*. A sample therefore never overlaps itself, a
//! slow sample delays only its own next one, and a long stall (sleep, a hung
//! read) produces one late sample, never a burst of catch-up samples.
//!
//! | Provider | Period                                              |
//! | -------- | --------------------------------------------------- |
//! | CPU      | 2 s, first sample after a 250 ms warm-up            |
//! | Battery  | 30 s while charging, 120 s otherwise                |
//! | Wi-Fi    | at most 5 s, earlier on a coalesced native signal   |
//!
//! A Wi-Fi change signal ([`WifiSignals`]) only sets one pending flag
//! ([`Wakeup`]); a storm is one extra sample, and consecutive samples are at
//! least [`ProviderTiming::wifi_min_gap`] apart.
//!
//! # Publication
//!
//! A sample publishes through the status cell under an exact monitor *epoch*:
//! ending a monitor clears the cell's epoch and readings atomically, so a late
//! completion of a stopped (or superseded) monitor can never republish. An
//! unchanged sample bumps no sequence. A failed read keeps the last good
//! reading (never invents one) and records a typed, deduplicated diagnostic.
//!
//! # Time
//!
//! Waiting and children use one injected [`Clock`], so schedules are tested on
//! a fake clock without real sleeping.

pub mod battery;
pub mod cpu;
pub mod media;
pub mod wifi;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub use battery::BatteryChargeState;
pub use media::{
    ArtworkLookup, ArtworkStore, MediaParts, MediaPaths, MediaReading, MediaResolver,
    MediaRunState, MediaStatus, MediaTiming, MediaUpdate, PublishMedia,
};
use serde::{Deserialize, Serialize};
use specta::Type;
use stache_macos::system::{
    BatteryBackend, CpuUsageBackend, SmcBackend, SmcCpuTemperature, SystemBattery, SystemCpuUsage,
    SystemWifi, WifiBackend,
};
use stache_sdk::CancellationToken;
use stache_sdk::commands::{
    CaptureLimits, Clock, CommandEnvironment, CommandExecutor, SystemClock,
};
pub use wifi::{SSID_GATE_ENV, SsidGate, WifiSignals, WifiStatus};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The CPU sample period.
pub const CPU_PERIOD: Duration = Duration::from_secs(2);
/// The delay before the first CPU sample, so the usage measurement has an
/// interval to measure (the OS needs at least 200 ms between two samples).
pub const CPU_WARMUP: Duration = Duration::from_millis(250);
/// The battery sample period while charging.
pub const BATTERY_CHARGING_PERIOD: Duration = Duration::from_secs(30);
/// The battery sample period otherwise.
pub const BATTERY_IDLE_PERIOD: Duration = Duration::from_secs(120);
/// The longest Wi-Fi sample period.
pub const WIFI_PERIOD: Duration = Duration::from_secs(5);
/// The least time between two Wi-Fi samples, however many signals arrive.
pub const WIFI_MIN_GAP: Duration = Duration::from_millis(500);
/// The deadline of the one child process (`ipconfig getsummary`).
pub const TOOL_DEADLINE: Duration = Duration::from_millis(1500);
/// The longest a monitor is joined at stop.
pub const PROVIDER_JOIN: Duration = Duration::from_secs(2);
/// The stdout cap of one child process.
pub const TOOL_STDOUT_CAP: usize = 256 * 1024;
/// The stderr cap of one child process.
pub const TOOL_STDERR_CAP: usize = 4 * 1024;

/// The fixed `ipconfig` executable (a base-system tool).
pub const IPCONFIG_PATH: &str = "/usr/sbin/ipconfig";

/// A CPU reading, the shape the legacy UI rendered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CpuReading {
    /// The rounded usage, 0 to 100.
    pub usage: u8,
    /// The rounded temperature in degrees Celsius; `None` when no source could
    /// supply a valid one. Unknown stays unknown.
    pub temperature: Option<u8>,
}

/// A battery reading. There is no battery reading when the machine has no
/// battery; the UI then hides the component.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BatteryReading {
    /// The charge, 0 to 100.
    pub percentage: u8,
    /// The charge state.
    pub state: BatteryChargeState,
}

/// A Wi-Fi reading, the shape the legacy UI rendered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WifiReading {
    /// The status.
    pub status: WifiStatus,
    /// The network name; `None` unless the gated summary path supplied one.
    /// A missing name never implies disconnected.
    pub network_name: Option<String>,
    /// The RSSI in dBm, only for a connected interface.
    pub signal_strength: Option<i32>,
}

impl WifiReading {
    /// The "nothing known" reading.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            status: WifiStatus::Unknown,
            network_name: None,
            signal_strength: None,
        }
    }
}

/// Which provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ProviderKind {
    /// CPU usage and temperature.
    Cpu,
    /// The battery.
    Battery,
    /// Wi-Fi.
    Wifi,
}

impl ProviderKind {
    /// The worker thread name.
    #[must_use]
    pub const fn thread_name(self) -> &'static str {
        match self {
            Self::Cpu => "stache-bar-cpu",
            Self::Battery => "stache-bar-battery",
            Self::Wifi => "stache-bar-wifi",
        }
    }
}

/// A typed provider condition shown outside the Bar visuals (module status).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDiagnostic {
    /// The provider.
    pub provider: ProviderKind,
    /// What is degraded; free of untrusted tool output.
    pub message: String,
}

/// One provider's new reading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderUpdate {
    /// A CPU reading.
    Cpu(CpuReading),
    /// A battery reading, or `None` when the machine has no battery.
    Battery(Option<BatteryReading>),
    /// A Wi-Fi reading.
    Wifi(WifiReading),
}

/// The result of one sample.
pub enum Sampled {
    /// Cancellation interrupted the sample; nothing is published.
    Cancelled,
    /// The sample finished. `update` is `None` when nothing could be read (the
    /// last reading stays); `diagnostic` is the current degraded condition.
    Done {
        /// The new reading, if any.
        update: Option<ProviderUpdate>,
        /// The degraded condition, if any.
        diagnostic: Option<String>,
    },
}

/// One provider's sampling.
pub trait Sampler: Send {
    /// Which provider.
    fn kind(&self) -> ProviderKind;
    /// Runs once at worker start, before the first wait.
    fn prime(&mut self) {}
    /// The delay before the first sample.
    fn first_delay(&self) -> Duration { Duration::ZERO }
    /// Takes one sample. Must honor `cancel`.
    fn sample(&mut self, cancel: &CancellationToken) -> Sampled;
    /// The period that follows the sample just taken.
    fn period(&self) -> Duration;
}

/// When the next sample may start: one `period` after `started`, never before
/// `finished`. Never fires more than once for a long stall.
#[must_use]
pub fn next_due(started: Duration, finished: Duration, period: Duration) -> Duration {
    (started + period).max(finished)
}

/// When a signalled sample may start: now, but at least `min_gap` after the
/// previous start.
#[must_use]
pub fn signalled_due(now: Duration, last_started: Option<Duration>, min_gap: Duration) -> Duration {
    last_started.map_or(now, |started| now.max(started + min_gap))
}

/// Time bounds, injectable so tests do not wait for production cadences.
#[derive(Clone, Copy, Debug)]
pub struct ProviderTiming {
    /// CPU period.
    pub cpu: Duration,
    /// Delay before the first CPU sample.
    pub cpu_warmup: Duration,
    /// Battery period while charging.
    pub battery_charging: Duration,
    /// Battery period otherwise.
    pub battery_idle: Duration,
    /// Longest Wi-Fi period.
    pub wifi: Duration,
    /// Least time between Wi-Fi samples.
    pub wifi_min_gap: Duration,
    /// Deadline of one child process.
    pub tool_deadline: Duration,
    /// Longest a monitor is joined at stop.
    pub join: Duration,
}

impl Default for ProviderTiming {
    fn default() -> Self {
        Self {
            cpu: CPU_PERIOD,
            cpu_warmup: CPU_WARMUP,
            battery_charging: BATTERY_CHARGING_PERIOD,
            battery_idle: BATTERY_IDLE_PERIOD,
            wifi: WIFI_PERIOD,
            wifi_min_gap: WIFI_MIN_GAP,
            tool_deadline: TOOL_DEADLINE,
            join: PROVIDER_JOIN,
        }
    }
}

/// Everything the providers need, injected by the host (fakes in tests).
#[derive(Clone)]
pub struct ProviderParts {
    /// CPU usage.
    pub cpu: Arc<dyn CpuUsageBackend>,
    /// The native SMC CPU temperature.
    pub smc: Arc<dyn SmcBackend>,
    /// The battery.
    pub battery: Arc<dyn BatteryBackend>,
    /// Wi-Fi interfaces.
    pub wifi: Arc<dyn WifiBackend>,
    /// Runs the gated `ipconfig` child. Reused from the SDK; no Bar-local
    /// wrapper.
    pub executor: Arc<CommandExecutor>,
    /// The one time source of the schedules and the executor.
    pub clock: Arc<dyn Clock>,
    /// The fixed `ipconfig` executable.
    pub ipconfig: std::path::PathBuf,
    /// Whether the SSID summary path may run. On by default.
    pub ssid: SsidGate,
    /// Native Wi-Fi change signals, if any.
    pub signals: Option<Arc<dyn WifiSignals>>,
    /// Time bounds.
    pub timing: ProviderTiming,
    /// The media adapter. `None` (the default) runs no media worker.
    pub media: Option<MediaParts>,
}

impl ProviderParts {
    /// The production parts: real readers, a real executor and clock, the SSID
    /// gate off, and no native Wi-Fi signal (the 5 s poll covers it).
    #[must_use]
    pub fn system() -> Self {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::default());
        let executor = Arc::new(CommandExecutor::with_parts(
            Arc::new(stache_sdk::commands::OsProcessFactory),
            Arc::clone(&clock),
            CommandEnvironment::from_env(),
        ));
        Self {
            cpu: Arc::new(SystemCpuUsage::new()),
            smc: Arc::new(SmcCpuTemperature::system()),
            battery: Arc::new(SystemBattery),
            wifi: Arc::new(SystemWifi),
            executor,
            clock,
            ipconfig: std::path::PathBuf::from(IPCONFIG_PATH),
            ssid: SsidGate::from_environment(),
            signals: None,
            timing: ProviderTiming::default(),
            media: None,
        }
    }

    /// The same parts also running the media provider over `media`.
    #[must_use]
    pub fn with_media(mut self, media: MediaParts) -> Self {
        self.media = Some(media);
        self
    }

    pub(crate) fn limits(&self) -> CaptureLimits {
        CaptureLimits::new(self.timing.tool_deadline, TOOL_STDOUT_CAP, TOOL_STDERR_CAP)
            .unwrap_or_else(|_| {
                CaptureLimits::new(TOOL_DEADLINE, TOOL_STDOUT_CAP, TOOL_STDERR_CAP)
                    .expect("the default tool limits are valid")
            })
    }
}

/// Coalesces native change signals into one pending flag and wakes the
/// schedule's current wait.
#[derive(Default)]
pub struct Wakeup {
    state: Mutex<WakeState>,
}

#[derive(Default)]
struct WakeState {
    pending: bool,
    current: Option<CancellationToken>,
}

impl Wakeup {
    /// Records a signal and wakes the current wait. Cheap, never blocks on a
    /// sample: a burst is one pending flag.
    pub fn notify(&self) {
        let current = {
            let mut state = lock(&self.state);
            state.pending = true;
            state.current.take()
        };
        if let Some(token) = current {
            token.cancel();
        }
    }

    /// Registers `token` as the wait to wake. `true` when a signal was already
    /// pending (it is consumed and the wait must not start).
    fn arm(&self, token: &CancellationToken) -> bool {
        let mut state = lock(&self.state);
        if std::mem::take(&mut state.pending) {
            return true;
        }
        state.current = Some(token.clone());
        false
    }

    /// Unregisters the wait and consumes a pending signal.
    fn take(&self) -> bool {
        let mut state = lock(&self.state);
        state.current = None;
        std::mem::take(&mut state.pending)
    }
}

/// Receives each sample result. Must not block.
pub type PublishSample =
    Arc<dyn Fn(ProviderKind, Option<ProviderUpdate>, Option<String>) + Send + Sync>;

/// What one provider worker needs.
pub struct WorkerEnv {
    /// The schedule clock.
    pub clock: Arc<dyn Clock>,
    /// Cancelled when the monitor stops.
    pub stop: CancellationToken,
    /// The coalescing wake, for providers with change signals.
    pub wakeup: Option<Arc<Wakeup>>,
    /// Least time between two signalled samples.
    pub min_gap: Duration,
    /// Where results go.
    pub publish: PublishSample,
}

/// Runs one provider's schedule until `env.stop` is cancelled.
pub fn run_provider(mut sampler: Box<dyn Sampler>, env: &WorkerEnv) {
    let kind = sampler.kind();
    sampler.prime();
    let mut due = env.clock.now() + sampler.first_delay();
    let mut last_started: Option<Duration> = None;
    loop {
        // Wait for the schedule, a stop or a coalesced signal.
        loop {
            if env.stop.is_cancelled() {
                return;
            }
            let now = env.clock.now();
            if now >= due {
                break;
            }
            let wait = CancellationToken::new();
            let _from_stop = {
                let wait = wait.clone();
                env.stop.on_cancel(Arc::new(move || wait.cancel()))
            };
            if env.wakeup.as_ref().is_some_and(|wakeup| wakeup.arm(&wait)) {
                due = due.min(signalled_due(now, last_started, env.min_gap));
                continue;
            }
            env.clock.wait(Some(&wait), due.saturating_sub(now));
            if env.wakeup.as_ref().is_some_and(|wakeup| wakeup.take()) && !env.stop.is_cancelled() {
                due = due.min(signalled_due(env.clock.now(), last_started, env.min_gap));
            }
        }
        if env.stop.is_cancelled() {
            return;
        }
        let started = env.clock.now();
        last_started = Some(started);
        match sampler.sample(&env.stop) {
            Sampled::Cancelled => return,
            Sampled::Done { update, diagnostic } => {
                if env.stop.is_cancelled() {
                    return;
                }
                (env.publish)(kind, update, diagnostic);
            }
        }
        let finished = env.clock.now();
        due = next_due(started, finished, sampler.period());
    }
}

/// The running providers of one Bar run with a panel.
pub struct ProviderMonitor {
    stop: CancellationToken,
    threads: Mutex<Vec<JoinHandle<()>>>,
    /// The media executor: an adapter child it still retains (unreaped after a
    /// stop) keeps this monitor unsettled.
    media: Option<Arc<CommandExecutor>>,
}

impl ProviderMonitor {
    /// Starts the three provider workers. Returns at once: every read happens
    /// on a worker thread.
    ///
    /// # Errors
    /// A worker could not be spawned; nothing is left running.
    pub fn start(
        parts: &ProviderParts,
        publish: &PublishSample,
        publish_media: &PublishMedia,
    ) -> Result<Self, String> {
        let monitor = Self {
            stop: CancellationToken::new(),
            threads: Mutex::new(Vec::new()),
            media: parts.media.as_ref().map(|media| Arc::clone(&media.executor)),
        };
        let wakeup = Arc::new(Wakeup::default());
        let samplers: [Box<dyn Sampler>; 3] = [
            Box::new(cpu::CpuSampler::new(parts)),
            Box::new(battery::BatterySampler::new(parts)),
            Box::new(wifi::WifiSampler::new(parts)),
        ];
        for sampler in samplers {
            let kind = sampler.kind();
            let env = WorkerEnv {
                clock: Arc::clone(&parts.clock),
                stop: monitor.stop.clone(),
                wakeup: (kind == ProviderKind::Wifi).then(|| Arc::clone(&wakeup)),
                min_gap: parts.timing.wifi_min_gap,
                publish: Arc::clone(publish),
            };
            let signals = (kind == ProviderKind::Wifi).then(|| parts.signals.clone()).flatten();
            let spawned =
                std::thread::Builder::new().name(kind.thread_name().to_owned()).spawn(move || {
                    // Native signals are subscribed on the worker, off the
                    // caller's thread, and dropped when it exits.
                    let _subscription = signals.and_then(|signals| {
                        let wake = Arc::clone(env.wakeup.as_ref()?);
                        signals.watch(Arc::new(move || wake.notify())).ok()
                    });
                    run_provider(sampler, &env);
                });
            match spawned {
                Ok(handle) => lock(&monitor.threads).push(handle),
                Err(error) => {
                    let _ = monitor.end(Duration::from_secs(1));
                    return Err(format!("the {kind:?} provider could not start: {error}"));
                }
            }
        }
        if let Some(media) = parts.media.clone() {
            let stop = monitor.stop.clone();
            let publish = Arc::clone(publish_media);
            let spawned = std::thread::Builder::new()
                .name("stache-bar-media".to_owned())
                .spawn(move || media::owner::run(&media, &stop, publish));
            match spawned {
                Ok(handle) => lock(&monitor.threads).push(handle),
                Err(error) => {
                    let _ = monitor.end(Duration::from_secs(1));
                    return Err(format!("the media provider could not start: {error}"));
                }
            }
        }
        Ok(monitor)
    }

    /// Asks every worker to stop: wakes waits and kills owned children. Never
    /// blocks.
    pub fn request_stop(&self) { self.stop.cancel(); }

    /// Whether every worker finished.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        lock(&self.threads).iter().all(JoinHandle::is_finished)
            && self.media.as_ref().is_none_or(|executor| executor.sweep().is_empty())
    }

    /// Joins settled workers (instant). `true` once nothing remains.
    #[must_use]
    pub fn reap(&self) -> bool {
        if !lock(&self.threads).iter().all(JoinHandle::is_finished) {
            return false;
        }
        let handles: Vec<JoinHandle<()>> = lock(&self.threads).drain(..).collect();
        for handle in handles {
            let _ = handle.join();
        }
        // A retained (unreaped) adapter child is rechecked, never forgotten.
        self.media.as_ref().is_none_or(|executor| executor.sweep().is_empty())
    }

    /// Stops the workers and joins them within `join`. `false` keeps whatever
    /// has not finished owned for another `end`.
    #[must_use]
    pub fn end(&self, join: Duration) -> bool {
        self.request_stop();
        let deadline = Instant::now() + join;
        while !self.is_settled() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        self.reap()
    }
}

impl Drop for ProviderMonitor {
    fn drop(&mut self) {
        // A dropped monitor never leaves a worker running: ask, do not wait.
        self.request_stop();
    }
}

#[cfg(test)]
mod tests;
