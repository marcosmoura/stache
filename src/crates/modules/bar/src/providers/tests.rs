//! Provider tests over fakes and a virtual clock: nothing here reads the
//! machine, sleeps or spawns a real process.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use stache_macos::system::testing::{FakeBattery, FakeCpuUsage, FakeSmc, FakeWifi, Script};
use stache_macos::system::{
    BatteryPowerState, BatteryReadout, InterfaceName, SystemReadError, WifiInterfaceReading,
    WifiPower, WifiSignalSink,
};
use stache_sdk::CancellationToken;
use stache_sdk::commands::{
    ChildProcess, Clock, CommandEnvironment, CommandExecutor, ProcessFactory, SpawnSpec,
};

use super::battery::{BatterySampler, percentage};
use super::cpu::CpuSampler;
use super::wifi::{
    SIGNALS_UNAVAILABLE, SignalState, WifiSampler, native_reading, parse_summary, sanitize_ssid,
};
use super::*;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ------------------------------------------------------------------ clock

type Event = (Duration, Box<dyn Fn() + Send + Sync>);

/// Virtual time: waiting advances it, up to the next scheduled event.
#[derive(Default)]
struct FakeClock {
    now: Mutex<Duration>,
    events: Mutex<Vec<Event>>,
}

impl FakeClock {
    fn at(&self, when: Duration, event: impl Fn() + Send + Sync + 'static) {
        lock(&self.events).push((when, Box::new(event)));
    }

    /// Moves time forward, running every event that falls due on the way (a
    /// stop scheduled inside a long stall still fires).
    fn advance(&self, by: Duration) {
        let now = {
            let mut now = lock(&self.now);
            *now += by;
            *now
        };
        loop {
            let due = {
                let mut events = lock(&self.events);
                events
                    .iter()
                    .position(|(when, _)| *when <= now)
                    .map(|index| events.remove(index))
            };
            match due {
                Some((_, event)) => event(),
                None => break,
            }
        }
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Duration { *lock(&self.now) }

    fn wait(&self, cancel: Option<&CancellationToken>, quantum: Duration) -> bool {
        let start = self.now();
        let end = start + quantum;
        let next = {
            let events = lock(&self.events);
            events
                .iter()
                .enumerate()
                .filter(|(_, (when, _))| *when <= end)
                .min_by_key(|(_, (when, _))| *when)
                .map(|(index, (when, _))| (index, *when))
        };
        if let Some((index, when)) = next {
            *lock(&self.now) = when.max(start);
            let (_, event) = lock(&self.events).remove(index);
            event();
        } else {
            *lock(&self.now) = end;
        }
        cancel.is_some_and(CancellationToken::is_cancelled)
    }
}

// ---------------------------------------------------------------- process

#[derive(Clone, Default)]
struct Proc {
    stdout: Vec<u8>,
    exit_code: i32,
    /// Never exits on its own; exits when killed.
    stalls: bool,
}

struct FakeChild {
    proc: Proc,
    sent: bool,
    killed: bool,
    kills: Arc<AtomicUsize>,
}

impl ChildProcess for FakeChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.killed {
            return Ok(Some(ExitStatus::from_raw(9)));
        }
        if self.proc.stalls {
            return Ok(None);
        }
        Ok(Some(ExitStatus::from_raw(self.proc.exit_code << 8)))
    }

    fn kill(&mut self) -> io::Result<()> {
        self.killed = true;
        self.kills.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn read_stdout(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.proc.stalls && !self.killed {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if self.sent {
            return Ok(0);
        }
        let count = self.proc.stdout.len().min(buffer.len());
        buffer[..count].copy_from_slice(&self.proc.stdout[..count]);
        self.proc.stdout.drain(..count);
        self.sent = self.proc.stdout.is_empty();
        Ok(count)
    }

    fn read_stderr(&mut self, _: &mut [u8]) -> io::Result<usize> {
        if self.proc.stalls && !self.killed {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        Ok(0)
    }
}

/// Answers by executable file name.
#[derive(Default)]
struct FakeFactory {
    procs: Mutex<HashMap<String, Proc>>,
    specs: Mutex<Vec<SpawnSpec>>,
    kills: Arc<AtomicUsize>,
}

impl FakeFactory {
    fn answer(&self, name: &str, proc: Proc) { lock(&self.procs).insert(name.to_owned(), proc); }

    fn text(&self, name: &str, stdout: &str) {
        self.answer(name, Proc {
            stdout: stdout.as_bytes().to_vec(),
            ..Proc::default()
        });
    }

    fn specs(&self) -> Vec<SpawnSpec> { lock(&self.specs).clone() }

    fn kills(&self) -> usize { self.kills.load(Ordering::SeqCst) }
}

impl ProcessFactory for FakeFactory {
    fn spawn(&self, spec: &SpawnSpec) -> io::Result<Box<dyn ChildProcess>> {
        lock(&self.specs).push(spec.clone());
        let name = spec.program.file_name().and_then(|name| name.to_str()).unwrap_or_default();
        // `name last-argument` (one interface) before plain `name`.
        let specific = format!("{name} {}", spec.arguments.last().map_or("", String::as_str));
        let procs = lock(&self.procs);
        let proc = procs.get(&specific).or_else(|| procs.get(name)).cloned().unwrap_or_default();
        drop(procs);
        Ok(Box::new(FakeChild {
            proc,
            sent: false,
            killed: false,
            kills: Arc::clone(&self.kills),
        }))
    }
}

// -------------------------------------------------------------------- rig

struct Rig {
    parts: ProviderParts,
    clock: Arc<FakeClock>,
    factory: Arc<FakeFactory>,
    usage: Arc<Script<Result<f32, SystemReadError>>>,
    smc: Arc<Script<Result<Option<f64>, SystemReadError>>>,
    battery: Arc<Script<Result<Option<BatteryReadout>, SystemReadError>>>,
    wifi: Arc<Script<Result<Vec<WifiInterfaceReading>, SystemReadError>>>,
}

fn rig() -> Rig {
    let clock = Arc::new(FakeClock::default());
    let factory = Arc::new(FakeFactory::default());
    let fake_cpu = FakeCpuUsage::new(10.0);
    let usage = Arc::clone(&fake_cpu.0);
    let fake_smc = FakeSmc::new(None);
    let smc = Arc::clone(&fake_smc.0);
    let battery = Script::new(Ok(None));
    let wifi = Script::new(Ok(Vec::new()));
    let executor = Arc::new(CommandExecutor::with_parts(
        Arc::clone(&factory) as Arc<dyn ProcessFactory>,
        Arc::clone(&clock) as Arc<dyn Clock>,
        CommandEnvironment::default(),
    ));
    let parts = ProviderParts {
        cpu: Arc::new(fake_cpu),
        smc: Arc::new(fake_smc),
        battery: Arc::new(FakeBattery(Arc::clone(&battery))),
        wifi: Arc::new(FakeWifi(Arc::clone(&wifi))),
        executor,
        clock: Arc::clone(&clock) as Arc<dyn Clock>,
        ipconfig: PathBuf::from("/usr/sbin/ipconfig"),
        ssid: SsidGate::Off,
        signals: None,
        timing: ProviderTiming::default(),
        media: None,
    };
    Rig {
        parts,
        clock,
        factory,
        usage,
        smc,
        battery,
        wifi,
    }
}

fn done(sampled: Sampled) -> (Option<ProviderUpdate>, Option<String>) {
    match sampled {
        Sampled::Done { update, diagnostic } => (update, diagnostic),
        Sampled::Cancelled => panic!("the sample was cancelled"),
    }
}

fn cpu_of(update: Option<ProviderUpdate>) -> CpuReading {
    match update {
        Some(ProviderUpdate::Cpu(reading)) => reading,
        other => panic!("expected a CPU reading, got {other:?}"),
    }
}

// -------------------------------------------------------------------- CPU

fn cpu_sample(rig: &Rig) -> (Option<ProviderUpdate>, Option<String>) {
    let mut sampler = CpuSampler::new(&rig.parts);
    done(sampler.sample(&CancellationToken::new()))
}

#[test]
fn cpu_warms_up_before_the_first_sample_and_discards_the_priming_read() {
    let rig = rig();
    rig.usage.push(Ok(0.0)); // the priming measurement
    rig.usage.push(Ok(37.4));
    let mut sampler = CpuSampler::new(&rig.parts);
    assert_eq!(sampler.first_delay(), CPU_WARMUP);
    sampler.prime();
    assert_eq!(rig.usage.calls(), 1);
    let (update, _) = done(sampler.sample(&CancellationToken::new()));
    assert_eq!(cpu_of(update), CpuReading { usage: 37, temperature: None });
    assert_eq!(rig.usage.calls(), 2);
}

#[test]
fn cpu_usage_is_rounded_clamped_and_never_invented() {
    let rig = rig();
    for (raw, expected) in [(23.5_f32, 24), (23.49, 23), (-3.0, 0), (180.0, 100)] {
        rig.usage.set(Ok(raw));
        assert_eq!(cpu_of(cpu_sample(&rig).0).usage, expected, "{raw}");
    }
    for bad in [f32::NAN, f32::INFINITY] {
        rig.usage.set(Ok(bad));
        let (update, diagnostic) = cpu_sample(&rig);
        assert_eq!(update, None, "a non-finite usage publishes nothing");
        assert!(diagnostic.is_some());
    }
    rig.usage.set(Err(SystemReadError::new("boom")));
    assert_eq!(cpu_sample(&rig).0, None);
}

#[test]
fn the_temperature_is_rounded_and_unknown_stays_unknown() {
    let rig = rig();
    for (raw, expected) in [(Some(61.4), Some(61)), (Some(61.5), Some(62)), (None, None)] {
        rig.smc.set(Ok(raw));
        let (update, diagnostic) = cpu_sample(&rig);
        assert_eq!(cpu_of(update).temperature, expected, "{raw:?}");
        assert_eq!(diagnostic, None);
    }
    // Out-of-range or non-finite values never become a temperature.
    for bad in [0.0, -4.0, 150.0, 400.0, f64::NAN, f64::INFINITY] {
        rig.smc.set(Ok(Some(bad)));
        assert_eq!(cpu_of(cpu_sample(&rig).0).temperature, None, "{bad}");
    }
}

#[test]
fn an_smc_failure_keeps_the_usage_and_reports_a_condition_without_a_temperature() {
    let rig = rig();
    rig.smc.set(Err(SystemReadError::new("no smc")));
    let (update, diagnostic) = cpu_sample(&rig);
    assert_eq!(cpu_of(update), CpuReading { usage: 10, temperature: None });
    assert!(
        diagnostic.is_some_and(|note| !note.contains("no smc")),
        "no raw text leaks"
    );
}

#[test]
fn the_cpu_sample_never_spawns_a_process() {
    let rig = rig();
    rig.smc.set(Ok(Some(55.0)));
    let _ = cpu_sample(&rig);
    assert!(rig.factory.specs().is_empty());
}

#[test]
fn a_cancelled_token_publishes_no_cpu_sample() {
    let rig = rig();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut sampler = CpuSampler::new(&rig.parts);
    assert!(matches!(sampler.sample(&cancel), Sampled::Cancelled));
    assert_eq!(rig.smc.calls(), 0);
}

// ---------------------------------------------------------------- battery

fn battery_sample(rig: &Rig) -> (Option<ProviderUpdate>, Option<String>) {
    let mut sampler = BatterySampler::new(&rig.parts);
    done(sampler.sample(&CancellationToken::new()))
}

#[allow(clippy::unnecessary_wraps)] // the shape the fake script stores
fn readout(
    percent: f32,
    state: BatteryPowerState,
) -> Result<Option<BatteryReadout>, SystemReadError> {
    Ok(Some(BatteryReadout { percent, state }))
}

#[test]
fn battery_percentages_are_rounded_and_bounded() {
    assert_eq!(percentage(-10.0), Some(0));
    assert_eq!(percentage(49.4), Some(49));
    assert_eq!(percentage(49.5), Some(50));
    assert_eq!(percentage(150.0), Some(100));
    assert_eq!(percentage(f32::NAN), None);
    assert_eq!(percentage(f32::INFINITY), None);
}

#[test]
fn battery_maps_every_state_and_hides_when_there_is_none() {
    let rig = rig();
    for (state, expected) in [
        (BatteryPowerState::Unknown, BatteryChargeState::Unknown),
        (BatteryPowerState::Charging, BatteryChargeState::Charging),
        (BatteryPowerState::Discharging, BatteryChargeState::Discharging),
        (BatteryPowerState::Empty, BatteryChargeState::Empty),
        (BatteryPowerState::Full, BatteryChargeState::Full),
    ] {
        rig.battery.set(readout(80.0, state));
        assert_eq!(
            battery_sample(&rig).0,
            Some(ProviderUpdate::Battery(Some(BatteryReading {
                percentage: 80,
                state: expected
            })))
        );
    }
    rig.battery.set(Ok(None));
    assert_eq!(battery_sample(&rig), (Some(ProviderUpdate::Battery(None)), None));
}

#[test]
fn a_failed_battery_read_keeps_the_last_reading_and_records_a_condition() {
    let rig = rig();
    rig.battery.set(Err(SystemReadError::new("iokit")));
    let (update, diagnostic) = battery_sample(&rig);
    assert_eq!(update, None);
    assert!(diagnostic.is_some());
    rig.battery.set(readout(f32::NAN, BatteryPowerState::Full));
    assert_eq!(battery_sample(&rig).0, None);
}

#[test]
fn the_battery_period_follows_the_charging_state() {
    let rig = rig();
    let mut sampler = BatterySampler::new(&rig.parts);
    assert_eq!(sampler.period(), BATTERY_IDLE_PERIOD);
    rig.battery.set(readout(50.0, BatteryPowerState::Charging));
    let _ = sampler.sample(&CancellationToken::new());
    assert_eq!(sampler.period(), BATTERY_CHARGING_PERIOD);
    rig.battery.set(readout(50.0, BatteryPowerState::Discharging));
    let _ = sampler.sample(&CancellationToken::new());
    assert_eq!(sampler.period(), BATTERY_IDLE_PERIOD);
    rig.battery.set(readout(100.0, BatteryPowerState::Full));
    let _ = sampler.sample(&CancellationToken::new());
    assert_eq!(sampler.period(), BATTERY_IDLE_PERIOD);
}

// ------------------------------------------------------------------ Wi-Fi

fn iface(name: &str, power: WifiPower, rssi: Option<i32>) -> WifiInterfaceReading {
    WifiInterfaceReading {
        name: InterfaceName::parse(name).expect("name"),
        power,
        rssi,
    }
}

fn wifi_of(update: Option<ProviderUpdate>) -> WifiReading {
    match update {
        Some(ProviderUpdate::Wifi(reading)) => reading,
        other => panic!("expected a Wi-Fi reading, got {other:?}"),
    }
}

fn wifi_sample(rig: &Rig) -> (Option<ProviderUpdate>, Option<String>) {
    let mut sampler = WifiSampler::new(&rig.parts);
    done(sampler.sample(&CancellationToken::new()))
}

#[test]
fn no_wifi_interfaces_is_unknown() {
    let rig = rig();
    assert_eq!(wifi_of(wifi_sample(&rig).0), WifiReading::unknown());
    rig.wifi.set(Err(SystemReadError::new("corewlan")));
    let (update, diagnostic) = wifi_sample(&rig);
    assert_eq!(wifi_of(update), WifiReading::unknown());
    assert!(diagnostic.is_some());
}

#[test]
fn native_status_follows_the_legacy_precedence_across_interfaces() {
    let connected = native_reading(&[iface("en0", WifiPower::On, Some(-55))]);
    assert_eq!(connected.status, WifiStatus::Connected);
    assert_eq!(connected.signal_strength, Some(-55));
    assert_eq!(connected.network_name, None, "the name is never inferred");
    // Radio on, no association.
    assert_eq!(
        native_reading(&[iface("en0", WifiPower::On, None)]).status,
        WifiStatus::Disconnected
    );
    // All off.
    assert_eq!(
        native_reading(&[
            iface("en0", WifiPower::Off, None),
            iface("en1", WifiPower::Off, None)
        ])
        .status,
        WifiStatus::Off
    );
    // Power unknown: legacy's disconnected.
    assert_eq!(
        native_reading(&[iface("en0", WifiPower::Unknown, None)]).status,
        WifiStatus::Disconnected
    );
    // One off, one on and associated: the associated one wins.
    let mixed = native_reading(&[
        iface("en0", WifiPower::Off, None),
        iface("en1", WifiPower::On, Some(-71)),
    ]);
    assert_eq!(
        (mixed.status, mixed.signal_strength),
        (WifiStatus::Connected, Some(-71))
    );
    // One off, one on without association: on beats off.
    assert_eq!(
        native_reading(&[
            iface("en0", WifiPower::Off, None),
            iface("en1", WifiPower::On, None)
        ])
        .status,
        WifiStatus::Disconnected
    );
}

#[test]
fn by_default_exactly_one_bounded_ipconfig_runs_per_sample() {
    let mut rig = rig();
    rig.parts.ssid = SsidGate::from_value(None);
    rig.wifi.set(Ok(vec![iface("en0", WifiPower::On, Some(-60))]));
    rig.factory.text("ipconfig", "InterfaceType : WiFi\nSSID : Home\n");
    for sample in 1..=3 {
        let reading = wifi_of(wifi_sample(&rig).0);
        assert_eq!(reading.network_name.as_deref(), Some("Home"));
        let specs = rig.factory.specs();
        assert_eq!(specs.len(), sample, "one child per sample");
        assert_eq!(specs[sample - 1].program, PathBuf::from("/usr/sbin/ipconfig"));
        assert_eq!(specs[sample - 1].arguments, ["getsummary", "en0"]);
    }
}

#[test]
fn with_the_gate_off_no_ipconfig_child_is_ever_spawned() {
    let mut rig = rig();
    rig.parts.ssid = SsidGate::from_value(Some("0"));
    rig.wifi.set(Ok(vec![iface("en0", WifiPower::On, Some(-60))]));
    for _ in 0..3 {
        let reading = wifi_of(wifi_sample(&rig).0);
        assert_eq!(reading.network_name, None);
        assert_eq!(reading.status, WifiStatus::Connected);
    }
    assert!(rig.factory.specs().is_empty());
}

#[test]
fn the_ssid_gate_is_on_by_default_and_off_only_for_the_exact_value_zero() {
    assert_eq!(SsidGate::default(), SsidGate::On);
    assert_eq!(SsidGate::from_value(None), SsidGate::On);
    for value in ["", "1", "true", "no", "0 ", "00", "off"] {
        assert_eq!(SsidGate::from_value(Some(value)), SsidGate::On, "{value:?}");
    }
    assert_eq!(SsidGate::from_value(Some("0")), SsidGate::Off);
    assert_eq!(SSID_GATE_ENV, "STACHE_BAR_WIFI_SSID_SUMMARY");
}

fn gated(rig: &mut Rig) { rig.parts.ssid = SsidGate::On; }

#[test]
fn the_gated_summary_supplies_a_quoted_ssid_and_runs_a_fixed_vector() {
    let mut rig = rig();
    gated(&mut rig);
    rig.wifi.set(Ok(vec![iface("en0", WifiPower::On, Some(-82))]));
    rig.factory.text(
        "ipconfig",
        "<dictionary> {\n  InterfaceType : WiFi\n  SSID : \"My Home Net\"\n  RSSI : -64\n}\n",
    );
    let reading = wifi_of(wifi_sample(&rig).0);
    assert_eq!(reading, WifiReading {
        status: WifiStatus::Connected,
        network_name: Some("My Home Net".to_owned()),
        signal_strength: Some(-64),
    });
    let specs = rig.factory.specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].program, PathBuf::from("/usr/sbin/ipconfig"));
    assert_eq!(specs[0].arguments, ["getsummary", "en0"]);
}

#[test]
fn a_redacted_or_missing_ssid_stays_null_and_never_implies_disconnected() {
    let mut rig = rig();
    gated(&mut rig);
    rig.wifi.set(Ok(vec![iface("en0", WifiPower::On, Some(-58))]));
    for output in [
        "InterfaceType : WiFi\nSSID : <redacted>\n",
        "InterfaceType : WiFi\n",
        "",
    ] {
        rig.factory.text("ipconfig", output);
        let reading = wifi_of(wifi_sample(&rig).0);
        assert_eq!(reading.status, WifiStatus::Connected, "{output:?}");
        assert_eq!(reading.network_name, None);
        assert_eq!(reading.signal_strength, Some(-58));
    }
}

#[test]
fn the_first_interface_with_an_ssid_wins_across_several_interfaces() {
    let mut rig = rig();
    gated(&mut rig);
    rig.wifi.set(Ok(vec![
        iface("en0", WifiPower::On, None),
        iface("en7", WifiPower::On, Some(-70)),
    ]));
    rig.factory.text("ipconfig en0", "InterfaceType : WiFi\nSSID : <redacted>\n");
    rig.factory.text("ipconfig en7", "InterfaceType : WiFi\nSSID : Second\n");
    let reading = wifi_of(wifi_sample(&rig).0);
    assert_eq!(reading.network_name.as_deref(), Some("Second"));
    assert_eq!(
        reading.signal_strength,
        Some(-70),
        "the native RSSI of the chosen interface"
    );
    let vectors: Vec<_> = rig.factory.specs().iter().map(|spec| spec.arguments.clone()).collect();
    assert_eq!(vectors, [["getsummary", "en0"], ["getsummary", "en7"]]);
}

#[test]
fn summary_parsing_sanitizes_untrusted_text() {
    assert_eq!(sanitize_ssid("\"Cafe\""), Some("Cafe".to_owned()));
    assert_eq!(sanitize_ssid("  \"\"  "), None);
    assert_eq!(sanitize_ssid("<REDACTED>"), None);
    assert_eq!(sanitize_ssid("a\u{7}b\u{1b}[31mc\n"), Some("ab[31mc".to_owned()));
    let long = "é".repeat(100);
    let clean = sanitize_ssid(&long).expect("bounded");
    assert!(clean.len() <= 64 && clean.chars().all(|ch| ch == 'é'));
    let summary = parse_summary("InterfaceType : IEEE80211\nSSID_STR : x\nlink RSSI : -61 dBm\n");
    assert_eq!(
        (summary.is_wifi, summary.ssid.as_deref(), summary.signal_strength),
        (true, Some("x"), Some(-61))
    );
    // An RSSI outside the plausible range is dropped.
    assert_eq!(parse_summary("RSSI : 40\n").signal_strength, None);
    assert_eq!(parse_summary("RSSI : -200\n").signal_strength, None);
    assert!(!parse_summary("InterfaceType : Ethernet\n").is_wifi);
    // Bounded work: a hostile many-line summary still parses.
    let hostile = "k : v\n".repeat(100_000);
    assert_eq!(parse_summary(&hostile), IpconfigSummaryDefault::default());
}

type IpconfigSummaryDefault = super::wifi::IpconfigSummary;

#[test]
fn a_stalled_ipconfig_is_killed_on_cancel_and_publishes_nothing() {
    let mut rig = rig();
    gated(&mut rig);
    rig.wifi.set(Ok(vec![iface("en0", WifiPower::On, Some(-60))]));
    rig.factory.answer("ipconfig", Proc {
        stalls: true,
        ..Proc::default()
    });
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    rig.clock.at(Duration::from_millis(200), move || token.cancel());
    let mut sampler = WifiSampler::new(&rig.parts);
    assert!(matches!(sampler.sample(&cancel), Sampled::Cancelled));
    assert_eq!(rig.factory.kills(), 1);
}

#[test]
fn a_timed_out_ipconfig_falls_back_to_the_native_reading_with_a_condition() {
    let mut rig = rig();
    gated(&mut rig);
    rig.wifi.set(Ok(vec![iface("en0", WifiPower::On, Some(-60))]));
    rig.factory.answer("ipconfig", Proc {
        stalls: true,
        ..Proc::default()
    });
    let (update, diagnostic) = wifi_sample(&rig);
    assert_eq!(wifi_of(update).status, WifiStatus::Connected);
    assert!(diagnostic.is_some_and(|text| text.contains("timed out")));
}

// -------------------------------------------------------------- schedules

/// Records when each sample starts, in virtual time.
struct Recording {
    inner: Box<dyn Sampler>,
    clock: Arc<FakeClock>,
    starts: Arc<Mutex<Vec<Duration>>>,
    /// Virtual time one sample takes.
    takes: Duration,
    /// Runs inside every sample.
    during: Option<Box<dyn Fn() + Send>>,
}

impl Sampler for Recording {
    fn kind(&self) -> ProviderKind { self.inner.kind() }

    fn prime(&mut self) { self.inner.prime(); }

    fn first_delay(&self) -> Duration { self.inner.first_delay() }

    fn sample(&mut self, cancel: &CancellationToken) -> Sampled {
        self.sample_for(Trigger::periodic(), cancel)
    }

    fn sample_for(&mut self, trigger: Trigger, cancel: &CancellationToken) -> Sampled {
        lock(&self.starts).push(self.clock.now());
        if let Some(during) = &self.during {
            during();
        }
        self.clock.advance(self.takes);
        self.inner.sample_for(trigger, cancel)
    }

    fn period(&self) -> Duration { self.inner.period() }
}

fn secs(value: f64) -> Duration { Duration::from_secs_f64(value) }

/// Runs `sampler` for `window` of virtual time and returns the sample starts
/// and the published count.
fn schedule(
    rig: &Rig,
    sampler: Box<dyn Sampler>,
    takes: Duration,
    window: Duration,
    wakeup: Option<Arc<Wakeup>>,
    during: Option<Box<dyn Fn() + Send>>,
) -> (Vec<Duration>, usize) {
    let starts = Arc::new(Mutex::new(Vec::new()));
    let published = Arc::new(AtomicUsize::new(0));
    let stop = CancellationToken::new();
    {
        let stop = stop.clone();
        rig.clock.at(window, move || stop.cancel());
    }
    let count = Arc::clone(&published);
    let env = WorkerEnv {
        clock: Arc::clone(&rig.parts.clock),
        stop,
        wakeup,
        min_gap: WIFI_MIN_GAP,
        quality_gap: WIFI_PERIOD,
        publish: Arc::new(move |_, _, _| {
            count.fetch_add(1, Ordering::SeqCst);
        }),
    };
    let recording = Recording {
        inner: sampler,
        clock: Arc::clone(&rig.clock),
        starts: Arc::clone(&starts),
        takes,
        during,
    };
    run_provider(Box::new(recording), &env);
    let starts = lock(&starts).clone();
    (starts, published.load(Ordering::SeqCst))
}

#[test]
fn the_cpu_schedule_is_two_seconds_after_a_warmup_with_exact_counts() {
    let rig = rig();
    let sampler = Box::new(CpuSampler::new(&rig.parts));
    let (starts, published) = schedule(&rig, sampler, Duration::ZERO, secs(10.0), None, None);
    assert_eq!(starts, [
        secs(0.25),
        secs(2.25),
        secs(4.25),
        secs(6.25),
        secs(8.25)
    ]);
    assert_eq!(published, 5);
}

#[test]
fn the_battery_schedule_is_30_seconds_charging_and_120_seconds_otherwise() {
    let rig = rig();
    rig.battery.set(readout(50.0, BatteryPowerState::Charging));
    let sampler = Box::new(BatterySampler::new(&rig.parts));
    let (starts, _) = schedule(&rig, sampler, Duration::ZERO, secs(120.0), None, None);
    assert_eq!(starts, [secs(0.0), secs(30.0), secs(60.0), secs(90.0)]);

    let rig = self::rig();
    rig.battery.set(readout(50.0, BatteryPowerState::Discharging));
    let sampler = Box::new(BatterySampler::new(&rig.parts));
    let (starts, _) = schedule(&rig, sampler, Duration::ZERO, secs(240.0), None, None);
    assert_eq!(starts, [secs(0.0), secs(120.0)]);
}

#[test]
fn the_wifi_schedule_is_at_most_five_seconds() {
    let rig = rig();
    let sampler = Box::new(WifiSampler::new(&rig.parts));
    let (starts, _) = schedule(&rig, sampler, Duration::ZERO, secs(30.0), None, None);
    assert_eq!(starts, [0.0, 5.0, 10.0, 15.0, 20.0, 25.0].map(secs));
}

#[test]
fn a_slow_sample_never_overlaps_and_a_long_stall_causes_no_catch_up_storm() {
    let rig = rig();
    let sampler = Box::new(CpuSampler::new(&rig.parts));
    // Each sample takes 7 s of a 2 s schedule.
    let (starts, _) = schedule(&rig, sampler, secs(7.0), secs(20.0), None, None);
    assert_eq!(starts, [secs(0.25), secs(7.25), secs(14.25)]);
    assert!(
        starts.windows(2).all(|pair| pair[1].saturating_sub(pair[0]) >= secs(7.0)),
        "no overlap"
    );

    // A 10-minute stall inside one sample yields one late sample, not 300.
    let rig = self::rig();
    let sampler = Box::new(CpuSampler::new(&rig.parts));
    let (starts, _) = schedule(&rig, sampler, secs(600.0), secs(1300.0), None, None);
    assert_eq!(starts.len(), 3, "{starts:?}");
}

#[test]
fn a_wifi_signal_storm_coalesces_into_one_extra_sample_after_the_minimum_gap() {
    let rig = rig();
    let wakeup = Arc::new(Wakeup::default());
    let storm = Arc::clone(&wakeup);
    let sampler = Box::new(WifiSampler::new(&rig.parts));
    // A thousand signals arrive while the first sample runs.
    let during: Box<dyn Fn() + Send> = Box::new(move || {
        for _ in 0..1000 {
            storm.notify(WifiSignalKind::Identity);
        }
    });
    let (starts, _) = schedule(
        &rig,
        sampler,
        Duration::ZERO,
        secs(9.0),
        Some(wakeup),
        Some(during),
    );
    // The storm fires inside every sample: each is followed by exactly one
    // gap-spaced extra sample, never more.
    assert_eq!(starts[0], secs(0.0));
    assert_eq!(
        starts[1],
        secs(0.5),
        "one coalesced sample after the minimum gap"
    );
    assert!(starts.windows(2).all(|pair| pair[1].saturating_sub(pair[0]) >= WIFI_MIN_GAP));
    assert!(
        starts.len() <= 20,
        "bounded by the gap, not by the signal count: {}",
        starts.len()
    );
}

#[test]
fn a_signal_during_the_wait_wakes_the_schedule_early_once() {
    let rig = rig();
    let wakeup = Arc::new(Wakeup::default());
    let signal = Arc::clone(&wakeup);
    rig.clock.at(secs(2.0), move || {
        for _ in 0..50 {
            signal.notify(WifiSignalKind::Identity);
        }
    });
    let sampler = Box::new(WifiSampler::new(&rig.parts));
    let (starts, _) = schedule(&rig, sampler, Duration::ZERO, secs(9.0), Some(wakeup), None);
    assert_eq!(starts, [secs(0.0), secs(2.0), secs(7.0)]);
}

#[test]
fn stop_cancels_the_wait_and_publishes_nothing_more() {
    let rig = rig();
    let sampler = Box::new(BatterySampler::new(&rig.parts));
    let (starts, published) = schedule(&rig, sampler, Duration::ZERO, secs(1.0), None, None);
    assert_eq!(starts, [Duration::ZERO]);
    assert_eq!(published, 1);
}

#[test]
fn schedule_math_is_pure() {
    assert_eq!(next_due(secs(10.0), secs(10.5), secs(2.0)), secs(12.0));
    assert_eq!(next_due(secs(10.0), secs(30.0), secs(2.0)), secs(30.0));
    assert_eq!(signalled_due(secs(3.0), None, WIFI_MIN_GAP), secs(3.0));
    assert_eq!(
        signalled_due(secs(3.0), Some(secs(2.9)), WIFI_MIN_GAP),
        secs(3.4)
    );
    assert_eq!(
        signalled_due(secs(3.0), Some(secs(1.0)), WIFI_MIN_GAP),
        secs(3.0)
    );
}

// ------------------------------------------------------- Wi-Fi signals

/// A scripted change-signal source: keeps the subscriber so tests can fire
/// signals, and records when its guard is dropped.
#[derive(Default)]
struct FakeSignals {
    fail: bool,
    sink: Mutex<Option<WifiSignalSink>>,
    watches: AtomicUsize,
    dropped: Arc<AtomicBool>,
}

struct FakeGuard(Arc<AtomicBool>);

impl Drop for FakeGuard {
    fn drop(&mut self) { self.0.store(true, Ordering::SeqCst); }
}

impl WifiSignals for FakeSignals {
    fn watch(&self, notify: WifiSignalSink) -> Result<Box<dyn Send>, String> {
        self.watches.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err("monitoring refused".to_owned());
        }
        *lock(&self.sink) = Some(notify);
        Ok(Box::new(FakeGuard(Arc::clone(&self.dropped))))
    }
}

impl FakeSignals {
    fn fire(&self, kind: WifiSignalKind) {
        let sink = lock(&self.sink).clone();
        if let Some(sink) = sink {
            sink(kind);
        }
    }

    /// Fires `count` signals of `kind` at `when`.
    fn at(self: &Arc<Self>, rig: &Rig, when: Duration, kind: WifiSignalKind, count: usize) {
        let signals = Arc::clone(self);
        rig.clock.at(when, move || {
            for _ in 0..count {
                signals.fire(kind);
            }
        });
    }
}

/// One publication of the Wi-Fi worker.
#[derive(Debug)]
struct Published {
    at: Duration,
    reading: WifiReading,
    diagnostic: Option<String>,
    /// `ipconfig` children spawned so far.
    children: usize,
}

/// A gated rig connected to network `A` on `en0`.
fn connected() -> Rig {
    let mut rig = rig();
    gated(&mut rig);
    rig.wifi.set(Ok(vec![iface("en0", WifiPower::On, Some(-60))]));
    rig.factory.text("ipconfig", "InterfaceType : WiFi\nSSID : A\n");
    rig
}

/// Runs the Wi-Fi worker (subscription included) on the fake clock until
/// `window`, as `ProviderMonitor` runs it.
fn wifi_run(rig: &Rig, signals: Option<&Arc<FakeSignals>>, window: Duration) -> Vec<Published> {
    let published = Arc::new(Mutex::new(Vec::new()));
    let stop = CancellationToken::new();
    {
        let stop = stop.clone();
        rig.clock.at(window, move || stop.cancel());
    }
    let env = WorkerEnv {
        clock: Arc::clone(&rig.parts.clock),
        stop,
        wakeup: Some(Arc::new(Wakeup::default())),
        min_gap: WIFI_MIN_GAP,
        quality_gap: WIFI_PERIOD,
        publish: {
            let published = Arc::clone(&published);
            let clock = Arc::clone(&rig.clock);
            let factory = Arc::clone(&rig.factory);
            Arc::new(move |_, update, diagnostic| {
                lock(&published).push(Published {
                    at: clock.now(),
                    reading: wifi_of(update),
                    diagnostic,
                    children: factory.specs().len(),
                });
            })
        },
    };
    let signals = signals.map(|signals| Arc::clone(signals) as Arc<dyn WifiSignals>);
    run_wifi_worker(WifiSampler::new(&rig.parts), signals.as_ref(), &env);
    std::mem::take(&mut *lock(&published))
}

fn times(published: &[Published]) -> Vec<Duration> {
    published.iter().map(|publication| publication.at).collect()
}

#[test]
fn with_signals_and_none_delivered_ipconfig_runs_once_per_minute() {
    let rig = connected();
    let signals = Arc::new(FakeSignals::default());
    let published = wifi_run(&rig, Some(&signals), secs(600.0));
    let expected: Vec<Duration> = (0..10).map(|minute| secs(f64::from(minute) * 60.0)).collect();
    assert_eq!(times(&published), expected);
    assert_eq!(
        rig.factory.specs().len(),
        10,
        "one ipconfig at start and per 60 s"
    );
    assert!(published.iter().all(|publication| {
        publication.reading.network_name.as_deref() == Some("A") && publication.diagnostic.is_none()
    }));
    assert_eq!(signals.watches.load(Ordering::SeqCst), 1);
}

#[test]
fn without_a_signal_source_ipconfig_runs_every_five_seconds_as_before() {
    // The baseline the signals are measured against: 12 children per minute.
    let rig = connected();
    let published = wifi_run(&rig, None, secs(600.0));
    assert_eq!(published.len(), 120);
    assert_eq!(rig.factory.specs().len(), 120);
    assert!(published.iter().all(|publication| publication.diagnostic.is_none()));
}

#[test]
fn an_identity_signal_runs_ipconfig_once_within_the_minimum_gap_and_a_storm_once() {
    let rig = connected();
    let signals = Arc::new(FakeSignals::default());
    signals.at(&rig, secs(10.0), WifiSignalKind::Identity, 1);
    signals.at(&rig, secs(20.0), WifiSignalKind::Identity, 1000);
    // Inside the minimum gap of the storm's sample: waits for the gap.
    signals.at(&rig, secs(20.1), WifiSignalKind::Identity, 1);
    let published = wifi_run(&rig, Some(&signals), secs(30.0));
    assert_eq!(times(&published), [0.0, 10.0, 20.0, 20.5].map(secs));
    let children: Vec<usize> = published.iter().map(|publication| publication.children).collect();
    assert_eq!(children, [1, 2, 3, 4], "one ipconfig per identity sample");
}

#[test]
fn a_quality_storm_for_a_minute_samples_natively_at_most_every_five_seconds() {
    let rig = connected();
    let signals = Arc::new(FakeSignals::default());
    for tenth in 10..590 {
        signals.at(&rig, secs(f64::from(tenth) / 10.0), WifiSignalKind::Quality, 1);
    }
    let wifi = Arc::clone(&rig.wifi);
    rig.clock.at(secs(30.0), move || {
        wifi.set(Ok(vec![iface("en0", WifiPower::On, Some(-70))]));
    });
    let published = wifi_run(&rig, Some(&signals), secs(59.5));
    let at = times(&published);
    assert_eq!(at[0], Duration::ZERO);
    assert!(
        at.windows(2).all(|pair| pair[1].saturating_sub(pair[0]) >= WIFI_PERIOD),
        "{at:?}"
    );
    assert!(at.len() <= 12, "{at:?}");
    assert_eq!(
        rig.factory.specs().len(),
        1,
        "only the first sample runs ipconfig"
    );
    let last = published.last().expect("samples");
    assert_eq!(last.reading, WifiReading {
        status: WifiStatus::Connected,
        network_name: Some("A".to_owned()),
        signal_strength: Some(-70),
    });
}

#[test]
fn losing_the_association_without_a_signal_runs_ipconfig_and_clears_the_name() {
    let rig = connected();
    let mut sampler = WifiSampler::new(&rig.parts);
    sampler.set_signals(SignalState::Installed);
    let cancel = CancellationToken::new();
    let quality = Trigger {
        quality: true,
        ..Trigger::default()
    };
    let first = wifi_of(done(sampler.sample_for(Trigger::periodic(), &cancel)).0);
    assert_eq!(first.network_name.as_deref(), Some("A"));
    // Same native state, no identity signal: no child.
    for trigger in [quality, Trigger::default()] {
        let again = wifi_of(done(sampler.sample_for(trigger, &cancel)).0);
        assert_eq!(again, first);
    }
    assert_eq!(rig.factory.specs().len(), 1);
    // Associated to not associated, with no signal.
    rig.wifi.set(Ok(vec![iface("en0", WifiPower::On, None)]));
    rig.factory.text("ipconfig", "InterfaceType : WiFi\n");
    let lost = wifi_of(done(sampler.sample_for(Trigger::default(), &cancel)).0);
    assert_eq!(rig.factory.specs().len(), 2, "the state change runs ipconfig");
    assert_eq!(lost, WifiReading {
        status: WifiStatus::Disconnected,
        network_name: None,
        signal_strength: None,
    });
}

#[test]
fn an_unavailable_source_keeps_the_five_second_cadence_and_reports_it_once() {
    let rig = connected();
    let signals = Arc::new(FakeSignals {
        fail: true,
        ..FakeSignals::default()
    });
    let published = wifi_run(&rig, Some(&signals), secs(30.0));
    assert_eq!(times(&published), [0.0, 5.0, 10.0, 15.0, 20.0, 25.0].map(secs));
    assert_eq!(
        rig.factory.specs().len(),
        6,
        "one ipconfig per 5 s, as without signals"
    );
    for publication in &published {
        assert_eq!(publication.reading.network_name.as_deref(), Some("A"));
        let diagnostic = publication.diagnostic.as_deref().unwrap_or_default();
        assert_eq!(
            diagnostic.matches(SIGNALS_UNAVAILABLE).count(),
            1,
            "{diagnostic:?}"
        );
    }
}

#[test]
fn a_stopped_worker_samples_nothing_more_and_drops_the_subscription() {
    let rig = connected();
    let signals = Arc::new(FakeSignals::default());
    // A quality signal at 1 s is due at 5 s; the stop at 3 s comes first.
    signals.at(&rig, secs(1.0), WifiSignalKind::Quality, 1);
    let published = wifi_run(&rig, Some(&signals), secs(3.0));
    assert_eq!(times(&published), [Duration::ZERO]);
    assert!(
        signals.dropped.load(Ordering::SeqCst),
        "the guard ends with the worker"
    );
    // Signals after the stop reach nobody: no sample, no child.
    signals.fire(WifiSignalKind::Identity);
    rig.clock.advance(secs(120.0));
    assert_eq!(rig.factory.specs().len(), 1);
}

#[test]
fn the_bar_follows_a_network_switch_and_the_radio_turning_off() {
    let rig = connected();
    let signals = Arc::new(FakeSignals::default());
    {
        let factory = Arc::clone(&rig.factory);
        let signals = Arc::clone(&signals);
        rig.clock.at(secs(10.0), move || {
            factory.text("ipconfig", "InterfaceType : WiFi\nSSID : B\n");
            signals.fire(WifiSignalKind::Identity);
        });
    }
    {
        let factory = Arc::clone(&rig.factory);
        let wifi = Arc::clone(&rig.wifi);
        let signals = Arc::clone(&signals);
        rig.clock.at(secs(20.0), move || {
            wifi.set(Ok(vec![iface("en0", WifiPower::Off, None)]));
            factory.text("ipconfig", "InterfaceType : WiFi\n");
            signals.fire(WifiSignalKind::Identity);
        });
    }
    let published = wifi_run(&rig, Some(&signals), secs(30.0));
    let readings: Vec<(Duration, WifiStatus, Option<&str>)> = published
        .iter()
        .map(|publication| {
            (
                publication.at,
                publication.reading.status,
                publication.reading.network_name.as_deref(),
            )
        })
        .collect();
    assert_eq!(readings, [
        (secs(0.0), WifiStatus::Connected, Some("A")),
        (secs(10.0), WifiStatus::Connected, Some("B")),
        (secs(20.0), WifiStatus::Off, None),
    ]);
}

#[test]
fn a_lost_source_samples_at_once_then_polls_every_five_seconds_with_the_condition() {
    let rig = connected();
    let signals = Arc::new(FakeSignals::default());
    signals.at(&rig, secs(0.2), WifiSignalKind::Lost, 1);
    // Later signals only follow the existing gap rules.
    signals.at(&rig, secs(7.0), WifiSignalKind::Quality, 50);
    signals.at(&rig, secs(12.0), WifiSignalKind::Identity, 50);
    let published = wifi_run(&rig, Some(&signals), secs(30.0));
    assert_eq!(
        times(&published),
        [0.0, 0.5, 5.5, 10.5, 12.0, 17.0, 22.0, 27.0].map(secs)
    );
    let children: Vec<usize> = published.iter().map(|publication| publication.children).collect();
    assert_eq!(
        children,
        (1..=8).collect::<Vec<_>>(),
        "every sample after the loss runs ipconfig"
    );
    assert_eq!(published[0].diagnostic, None);
    for publication in &published[1..] {
        let diagnostic = publication.diagnostic.as_deref().unwrap_or_default();
        assert_eq!(
            diagnostic.matches(SIGNALS_UNAVAILABLE).count(),
            1,
            "{diagnostic:?}"
        );
        assert_eq!(publication.reading.network_name.as_deref(), Some("A"));
    }
}

#[allow(dead_code)]
fn unused(_: VecDeque<()>) {}
