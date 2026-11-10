//! Key reader and location resolver tests. Temp directories and injected
//! home/config directories only; the location source is always a fake, and no
//! real location, prompt, process environment or network is touched.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use std::{fs, thread};

use stache_macos::{
    FakeLocationSource, LocationFix, LocationOutcome, LocationRequest, LocationSource,
    LocationUnavailable,
};
use stache_sdk::CancellationToken;
use stache_sdk::commands::Clock;

use crate::config::{BarConfig, WeatherConfig, WeatherProvider};
use crate::tests::{Harness, Native, builtin, config, lock};
use crate::weather::key::{ApiKeyProblem, MAX_ENV_FILE_BYTES};
use crate::weather::{
    ApiKeyRead, BarWeather, BarWeatherLocation, BarWeatherProvider, LocationDiagnostic,
    WeatherParts, WeatherRead, WeatherService, WeatherStatus, WeatherTiming, parse_api_key,
    read_api_key,
};
use crate::{BarPanelState, BarRuntimeState};

// ---------------------------------------------------------------- key reader

fn key_of(text: &str) -> Option<String> { parse_api_key(text.as_bytes()) }

#[test]
fn the_parser_keeps_the_legacy_dotenv_semantics() {
    let name = "VISUAL_CROSSING_API_KEY";
    assert_eq!(key_of(&format!("{name}=abc123")), Some("abc123".into()));
    assert_eq!(
        key_of(&format!("{name}=\"quoted value\"")),
        Some("quoted value".into())
    );
    assert_eq!(
        key_of(&format!("{name}='single quoted'")),
        Some("single quoted".into())
    );
    assert_eq!(
        key_of(&format!("# note\n\n{name}=k # trailing\n")),
        Some("k".into())
    );
    assert_eq!(
        key_of(&format!("export {name}=exported")),
        Some("exported".into())
    );
    assert_eq!(key_of(&format!("  {name}  =  spaced  ")), Some("spaced".into()));
    assert_eq!(key_of(&format!("{name}=a=b=c")), Some("a=b=c".into()));
    assert_eq!(
        key_of(&format!("{name}=\"  padded  \"")),
        Some("padded".into()),
        "trimmed"
    );
    assert_eq!(
        key_of(&format!("{name}=abc\r\nOTHER=1\r\n")),
        Some("abc".into()),
        "CRLF"
    );
}

#[test]
fn missing_empty_and_commented_keys_are_no_key() {
    assert_eq!(key_of(""), None);
    assert_eq!(key_of("OTHER=1\n"), None);
    assert_eq!(key_of("VISUAL_CROSSING_API_KEY=\n"), None, "empty value");
    assert_eq!(key_of("VISUAL_CROSSING_API_KEY=\"   \"\n"), None, "blank value");
    assert_eq!(key_of("# VISUAL_CROSSING_API_KEY=abc\n"), None, "commented out");
    assert_eq!(key_of("NOT_VISUAL_CROSSING_API_KEY=abc\n"), None, "exact name");
}

#[test]
fn malformed_lines_are_skipped_and_duplicates_resolve_to_the_last() {
    let text = "garbage line\n=novalue\nVISUAL_CROSSING_API_KEY=first\n9BAD NAME=1\nVISUAL_CROSSING_API_KEY=last\n";
    assert_eq!(key_of(text), Some("last".into()));
    // An unterminated quote swallows the rest of the file, as legacy did.
    assert_eq!(key_of("A=\"open\nVISUAL_CROSSING_API_KEY=lost\n"), None);
    // The key before a bad line survives.
    assert_eq!(
        key_of("VISUAL_CROSSING_API_KEY=ok\nA=\"open\n"),
        Some("ok".into())
    );
}

#[test]
fn substitution_comes_from_earlier_entries_and_a_single_quoted_dollar_is_literal() {
    assert_eq!(
        key_of("PART=abc\nVISUAL_CROSSING_API_KEY=${PART}def\n"),
        Some("abcdef".into())
    );
    assert_eq!(
        key_of("VISUAL_CROSSING_API_KEY='a$PART'\n"),
        Some("a$PART".into())
    );
}

#[test]
fn a_byte_order_mark_hides_the_first_entry_and_bad_utf8_is_skipped() {
    let mut bom = vec![0xEF, 0xBB, 0xBF];
    bom.extend_from_slice(b"VISUAL_CROSSING_API_KEY=hidden\n");
    assert_eq!(parse_api_key(&bom), None, "legacy never stripped the BOM");
    bom.extend_from_slice(b"VISUAL_CROSSING_API_KEY=second\n");
    assert_eq!(parse_api_key(&bom), Some("second".into()));
    let mut bad = b"X=\xff\xfe\n".to_vec();
    bad.extend_from_slice(b"VISUAL_CROSSING_API_KEY=fine\n");
    assert_eq!(parse_api_key(&bad), Some("fine".into()));
}

fn weather(api_keys: &str) -> WeatherConfig {
    WeatherConfig {
        api_keys: api_keys.to_owned(),
        ..WeatherConfig::default()
    }
}

fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, text).expect("write fixture");
    path
}

fn problem(read: &ApiKeyRead) -> ApiKeyProblem {
    read.diagnostic.as_ref().expect("diagnostic").problem
}

#[test]
fn an_empty_api_keys_reads_no_file() {
    let dir = tempfile::tempdir().expect("dir");
    for blank in ["", "   "] {
        let read = read_api_key(&weather(blank), Some(dir.path()), dir.path());
        assert_eq!(read, ApiKeyRead { key: None, diagnostic: None });
    }
}

#[test]
fn relative_paths_resolve_against_the_config_dir_never_the_working_directory() {
    let config = tempfile::tempdir().expect("config");
    let other = tempfile::tempdir().expect("other");
    write(config.path(), "keys.env", "VISUAL_CROSSING_API_KEY=in-config\n");
    let read = read_api_key(&weather("keys.env"), None, config.path());
    assert_eq!(read.key.as_deref(), Some("in-config"));
    // The same relative value against another config dir does not find it,
    // and the diagnostic names the config-dir path (not a CWD path).
    let read = read_api_key(&weather("keys.env"), None, other.path());
    assert_eq!(read.key, None);
    assert_eq!(problem(&read), ApiKeyProblem::Missing);
    assert_eq!(
        read.diagnostic.and_then(|d| d.path),
        Some(other.path().join("keys.env").display().to_string())
    );
    // `./` and nested relative values resolve the same way.
    fs::create_dir(config.path().join("sub")).expect("sub");
    write(
        &config.path().join("sub"),
        "k.env",
        "VISUAL_CROSSING_API_KEY=nested\n",
    );
    assert_eq!(
        read_api_key(&weather(" ./sub/k.env "), None, config.path()).key.as_deref(),
        Some("nested")
    );
}

#[test]
fn absolute_and_tilde_paths_use_the_injected_home() {
    let config = tempfile::tempdir().expect("config");
    let home = tempfile::tempdir().expect("home");
    let absolute = write(home.path(), "abs.env", "VISUAL_CROSSING_API_KEY=absolute\n");
    write(home.path(), ".keys", "VISUAL_CROSSING_API_KEY=tilde\n");
    assert_eq!(
        read_api_key(&weather(absolute.to_str().expect("utf8")), None, config.path())
            .key
            .as_deref(),
        Some("absolute")
    );
    assert_eq!(
        read_api_key(&weather("~/.keys"), Some(home.path()), config.path())
            .key
            .as_deref(),
        Some("tilde")
    );
    let no_home = read_api_key(&weather("~/.keys"), None, config.path());
    assert_eq!(problem(&no_home), ApiKeyProblem::Unresolvable);
    assert_eq!(no_home.diagnostic.and_then(|d| d.path), None);
}

#[test]
fn missing_directory_oversized_and_unreadable_files_are_redacted_diagnostics() {
    let dir = tempfile::tempdir().expect("dir");
    let config = dir.path();
    assert_eq!(
        problem(&read_api_key(&weather("nope.env"), None, config)),
        ApiKeyProblem::Missing
    );

    fs::create_dir(config.join("folder")).expect("folder");
    assert_eq!(
        problem(&read_api_key(&weather("folder"), None, config)),
        ApiKeyProblem::NotAFile
    );

    let filler = "#".repeat(usize::try_from(MAX_ENV_FILE_BYTES).expect("small") + 10);
    write(
        config,
        "big.env",
        &format!("VISUAL_CROSSING_API_KEY=sentinel-big\n{filler}"),
    );
    let big = read_api_key(&weather("big.env"), None, config);
    assert_eq!(
        (big.key.as_deref(), problem(&big)),
        (None, ApiKeyProblem::TooLarge)
    );
    assert!(!format!("{big:?}").contains("sentinel-big"));

    write(config, "nokey.env", "OTHER=1\n");
    assert_eq!(
        problem(&read_api_key(&weather("nokey.env"), None, config)),
        ApiKeyProblem::KeyAbsent
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let locked = write(config, "locked.env", "VISUAL_CROSSING_API_KEY=sentinel-locked\n");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
        let read = read_api_key(&weather("locked.env"), None, config);
        // Root ignores the mode; everyone else must see `unreadable`.
        if read.key.is_none() {
            assert_eq!(problem(&read), ApiKeyProblem::Unreadable);
        }
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).expect("restore");
    }
}

#[test]
fn a_symlink_to_a_regular_file_is_followed_and_the_read_exactly_at_the_cap_is_accepted() {
    let dir = tempfile::tempdir().expect("dir");
    let target = write(dir.path(), "real.env", "VISUAL_CROSSING_API_KEY=linked\n");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, dir.path().join("link.env")).expect("symlink");
        assert_eq!(
            read_api_key(&weather("link.env"), None, dir.path()).key.as_deref(),
            Some("linked")
        );
    }
    let head = "VISUAL_CROSSING_API_KEY=edge\n";
    let pad = "#".repeat(usize::try_from(MAX_ENV_FILE_BYTES).expect("small") - head.len());
    write(dir.path(), "edge.env", &format!("{head}{pad}"));
    assert_eq!(
        read_api_key(&weather("edge.env"), None, dir.path()).key.as_deref(),
        Some("edge")
    );
    drop(target);
}

#[test]
fn debug_output_redacts_the_key_and_the_coordinates() {
    let read = ApiKeyRead {
        key: Some("sentinel-key".into()),
        diagnostic: None,
    };
    let weather = BarWeather {
        config_revision: "1".into(),
        provider: BarWeatherProvider::Auto,
        api_key: Some("sentinel-key".into()),
        location: BarWeatherLocation::CoreLocation {
            latitude: 12.3456,
            longitude: 65.4321,
            reduced_accuracy: false,
        },
        refresh_interval_minutes: 20,
    };
    let default = BarWeatherLocation::DefaultLocation { query: "sentinel-city".into() };
    for text in [
        format!("{read:?}"),
        format!("{weather:?}"),
        format!("{default:?}"),
    ] {
        for secret in ["sentinel-key", "12.3456", "65.4321", "sentinel-city"] {
            assert!(!text.contains(secret), "{text}");
        }
    }
}

#[test]
fn the_dtos_have_exact_wire_forms() {
    let core = BarWeatherLocation::CoreLocation {
        latitude: 1.5,
        longitude: -2.5,
        reduced_accuracy: true,
    };
    assert_eq!(
        serde_json::to_string(&core).expect("json"),
        r#"{"source":"coreLocation","latitude":1.5,"longitude":-2.5,"reducedAccuracy":true}"#
    );
    assert_eq!(
        serde_json::to_string(&BarWeatherLocation::DefaultLocation { query: "Oslo".into() })
            .expect("json"),
        r#"{"source":"defaultLocation","query":"Oslo"}"#
    );
    assert_eq!(
        serde_json::to_string(&BarWeatherLocation::None).expect("json"),
        r#"{"source":"none"}"#
    );
    let weather = BarWeather {
        config_revision: "7".into(),
        provider: BarWeatherProvider::VisualCrossing,
        api_key: None,
        location: BarWeatherLocation::None,
        refresh_interval_minutes: 90,
    };
    let json = r#"{"configRevision":"7","provider":"visualCrossing","apiKey":null,"location":{"source":"none"},"refreshIntervalMinutes":90}"#;
    assert_eq!(serde_json::to_string(&weather).expect("json"), json);
    assert_eq!(
        serde_json::from_str::<BarWeather>(json).expect("round trip"),
        weather
    );
    for (provider, text) in [
        (BarWeatherProvider::Auto, "\"auto\""),
        (BarWeatherProvider::VisualCrossing, "\"visualCrossing\""),
        (BarWeatherProvider::OpenMeteo, "\"openMeteo\""),
    ] {
        assert_eq!(serde_json::to_string(&provider).expect("json"), text);
    }
    let status = WeatherStatus {
        location: Some(LocationDiagnostic::NotDeterminedTimeout),
        api_key: Some(crate::ApiKeyDiagnostic {
            problem: ApiKeyProblem::TooLarge,
            path: Some("/k".into()),
        }),
    };
    assert_eq!(
        serde_json::to_string(&status).expect("json"),
        r#"{"location":"notDeterminedTimeout","apiKey":{"problem":"tooLarge","path":"/k"}}"#
    );
}

// ------------------------------------------------------------------ resolver

fn fix(latitude: f64, longitude: f64, reduced: bool) -> LocationOutcome {
    LocationOutcome::Located(LocationFix {
        latitude,
        longitude,
        accuracy_meters: 100.0,
        reduced_accuracy: reduced,
    })
}

#[derive(Default)]
struct TestClock(Mutex<Duration>);

impl TestClock {
    fn advance(&self, by: Duration) { *lock(&self.0) += by; }
}

impl Clock for TestClock {
    fn now(&self) -> Duration { *lock(&self.0) }

    fn wait(&self, _: Option<&CancellationToken>, _: Duration) -> bool { false }
}

struct Rig {
    h: Harness,
    config_dir: tempfile::TempDir,
    clock: Arc<TestClock>,
    service: Arc<WeatherService>,
}

fn bar_config(provider: WeatherProvider, precise: bool) -> BarConfig {
    BarConfig {
        weather: WeatherConfig {
            provider,
            api_keys: "keys.env".to_owned(),
            default_location: "  Lisbon  ".to_owned(),
            precise_location: precise,
            ..WeatherConfig::default()
        },
        ..config(28, 14)
    }
}

fn rig_with(
    config: BarConfig,
    source: Arc<dyn LocationSource>,
    timing: WeatherTiming,
    start: bool,
) -> Rig {
    let config_dir = tempfile::tempdir().expect("config dir");
    write(
        config_dir.path(),
        "keys.env",
        "VISUAL_CROSSING_API_KEY=  sentinel-key \n",
    );
    let h = Harness::new(Native::Auto, config, vec![builtin()]);
    if start {
        h.start();
        h.wait_until("running with a panel", |h| {
            let published = h.runtime.status().published();
            published.state == BarRuntimeState::Running && published.panel == BarPanelState::Loaded
        });
    }
    let clock = Arc::new(TestClock::default());
    let parts = WeatherParts::new(source, None, config_dir.path().to_path_buf())
        .with_clock(Arc::clone(&clock) as Arc<dyn Clock>)
        .with_timing(timing);
    let service = Arc::new(WeatherService::new(parts, h.runtime.status()));
    Rig { h, config_dir, clock, service }
}

fn rig(config: BarConfig, source: &Arc<FakeLocationSource>) -> Rig {
    rig_with(
        config,
        Arc::clone(source) as Arc<dyn LocationSource>,
        WeatherTiming::default(),
        true,
    )
}

fn ready(read: WeatherRead) -> BarWeather {
    match read {
        WeatherRead::Ready(weather) => weather,
        other => panic!("expected ready, got {other:?}"),
    }
}

#[test]
fn a_fix_becomes_core_location_with_the_trimmed_key_and_the_configured_provider() {
    let source = Arc::new(FakeLocationSource::new(fix(38.7, -9.1, false)));
    let r = rig(bar_config(WeatherProvider::Auto, false), &source);
    let weather = ready(r.service.read());
    assert_eq!(weather.config_revision, "1");
    assert_eq!(weather.provider, BarWeatherProvider::Auto);
    assert_eq!(weather.api_key.as_deref(), Some("sentinel-key"));
    assert_eq!(weather.location, BarWeatherLocation::CoreLocation {
        latitude: 38.7,
        longitude: -9.1,
        reduced_accuracy: false
    });
    assert_eq!(
        r.h.runtime.status().read().weather.location,
        Some(LocationDiagnostic::Located)
    );
}

#[test]
fn a_reduced_accuracy_fix_is_flagged() {
    let source = Arc::new(FakeLocationSource::new(fix(1.0, 2.0, true)));
    let r = rig(bar_config(WeatherProvider::OpenMeteo, false), &source);
    let weather = ready(r.service.read());
    assert_eq!(weather.location, BarWeatherLocation::CoreLocation {
        latitude: 1.0,
        longitude: 2.0,
        reduced_accuracy: true
    });
}

#[test]
fn every_unavailable_reason_falls_back_to_the_default_location_with_a_typed_diagnostic() {
    use LocationUnavailable as U;
    for (reason, expected) in [
        (U::Denied, LocationDiagnostic::Denied),
        (U::Restricted, LocationDiagnostic::Restricted),
        (U::ServicesDisabled, LocationDiagnostic::ServicesDisabled),
        (U::NotDeterminedTimeout, LocationDiagnostic::NotDeterminedTimeout),
        (U::Timeout, LocationDiagnostic::Timeout),
        (U::Error, LocationDiagnostic::Error),
        (U::NoUsageDescription, LocationDiagnostic::NoUsageDescription),
        (U::Unavailable, LocationDiagnostic::Unavailable),
    ] {
        let source = Arc::new(FakeLocationSource::unavailable(reason));
        let r = rig(bar_config(WeatherProvider::Auto, false), &source);
        let weather = ready(r.service.read());
        assert_eq!(
            weather.location,
            BarWeatherLocation::DefaultLocation { query: "Lisbon".into() },
            "{reason:?}"
        );
        assert_eq!(
            r.h.runtime.status().read().weather.location,
            Some(expected),
            "{reason:?}"
        );
    }
}

#[test]
fn an_empty_default_location_is_none() {
    let source = Arc::new(FakeLocationSource::unavailable(LocationUnavailable::Denied));
    let mut config = bar_config(WeatherProvider::Auto, false);
    config.weather.default_location = "   ".to_owned();
    let r = rig(config, &source);
    assert_eq!(ready(r.service.read()).location, BarWeatherLocation::None);
}

#[test]
fn open_meteo_reads_no_key_and_a_bad_key_file_is_a_status_diagnostic() {
    let source = Arc::new(FakeLocationSource::unavailable(LocationUnavailable::Denied));
    let r = rig(bar_config(WeatherProvider::OpenMeteo, false), &source);
    assert_eq!(ready(r.service.read()).api_key, None);
    assert_eq!(r.h.runtime.status().read().weather.api_key, None);

    let r = rig(bar_config(WeatherProvider::Auto, false), &source);
    fs::remove_file(r.config_dir.path().join("keys.env")).expect("remove");
    let weather = ready(r.service.read());
    assert_eq!(weather.api_key, None);
    let note = r.h.runtime.status().read().weather.api_key.expect("diagnostic");
    assert_eq!(note.problem, ApiKeyProblem::Missing);
    assert_eq!(
        note.path,
        Some(r.config_dir.path().join("keys.env").display().to_string())
    );
}

#[test]
fn the_key_file_is_read_on_every_call() {
    let source = Arc::new(FakeLocationSource::unavailable(LocationUnavailable::Denied));
    let r = rig(bar_config(WeatherProvider::Auto, false), &source);
    assert_eq!(ready(r.service.read()).api_key.as_deref(), Some("sentinel-key"));
    write(
        r.config_dir.path(),
        "keys.env",
        "VISUAL_CROSSING_API_KEY=rotated\n",
    );
    assert_eq!(ready(r.service.read()).api_key.as_deref(), Some("rotated"));
}

#[test]
fn precise_location_reaches_the_request_and_a_reconfigure_applies_at_the_next_read() {
    let source = Arc::new(FakeLocationSource::new(fix(1.0, 2.0, false)));
    let r = rig(bar_config(WeatherProvider::Auto, false), &source);
    let _ = ready(r.service.read());
    r.h.reload(bar_config(WeatherProvider::Auto, true));
    let weather = ready(r.service.read());
    assert_eq!(weather.config_revision, "2");
    r.h.reload(bar_config(WeatherProvider::Auto, false));
    let _ = ready(r.service.read());
    let requests = source.requests();
    assert_eq!(
        requests.iter().map(|request| request.precise).collect::<Vec<_>>(),
        [false, true, false]
    );
    assert!(
        requests
            .iter()
            .all(|request| { request.timeout == LocationRequest::new(false).timeout })
    );
}

#[test]
fn the_configured_refresh_interval_is_returned_and_a_reconfigure_changes_it() {
    let source = Arc::new(FakeLocationSource::new(fix(1.0, 2.0, false)));
    let r = rig(bar_config(WeatherProvider::Auto, false), &source);
    assert_eq!(ready(r.service.read()).refresh_interval_minutes, 20);
    let mut config = bar_config(WeatherProvider::Auto, false);
    config.weather.refresh_interval = 45;
    r.h.reload(config);
    let weather = ready(r.service.read());
    assert_eq!(weather.config_revision, "2");
    assert_eq!(weather.refresh_interval_minutes, 45);
}

#[test]
fn an_outcome_is_reused_for_sixty_seconds_for_the_same_run_and_revision() {
    let source = Arc::new(FakeLocationSource::new(fix(1.0, 2.0, false)));
    let r = rig(bar_config(WeatherProvider::Auto, false), &source);
    let _ = ready(r.service.read());
    r.clock.advance(Duration::from_secs(59));
    let _ = ready(r.service.read());
    assert_eq!(source.requests().len(), 1, "double mount reads once");
    r.clock.advance(Duration::from_secs(1));
    let _ = ready(r.service.read());
    assert_eq!(source.requests().len(), 2, "reads again at the 60 s gap");
}

#[test]
fn a_new_revision_or_a_restart_is_read_again() {
    let source = Arc::new(FakeLocationSource::new(fix(1.0, 2.0, false)));
    let r = rig(bar_config(WeatherProvider::Auto, false), &source);
    let _ = ready(r.service.read());
    // An identical reload is not a new revision and reads nothing.
    r.h.reload(bar_config(WeatherProvider::Auto, false));
    let _ = ready(r.service.read());
    assert_eq!(source.requests().len(), 1, "same revision");
    r.h.reload(bar_config(WeatherProvider::OpenMeteo, false));
    let _ = ready(r.service.read());
    assert_eq!(source.requests().len(), 2, "new revision");
    r.h.registry.pause(r.h.handle).expect("pause");
    assert!(matches!(r.service.read(), WeatherRead::NotRunning(_)));
    r.h.registry.resume(r.h.handle).expect("resume");
    r.h.wait_until("running again", |h| {
        let published = h.runtime.status().published();
        published.state == BarRuntimeState::Running && published.panel == BarPanelState::Loaded
    });
    let _ = ready(r.service.read());
    assert_eq!(source.requests().len(), 3, "new generation");
}

#[test]
fn disabled_paused_failed_and_zero_size_runs_read_nothing() {
    // Disabled: never started.
    let source = Arc::new(FakeLocationSource::new(fix(1.0, 2.0, false)));
    let mut off = bar_config(WeatherProvider::Auto, false);
    off.enabled = false;
    let r = rig_with(
        off,
        Arc::clone(&source) as Arc<dyn LocationSource>,
        WeatherTiming::default(),
        false,
    );
    r.h.start();
    assert!(matches!(r.service.read(), WeatherRead::NotRunning(_)));

    // Zero size: running with no panel.
    let mut zero = bar_config(WeatherProvider::Auto, false);
    zero.height = 0;
    let r = rig_with(
        zero,
        Arc::clone(&source) as Arc<dyn LocationSource>,
        WeatherTiming::default(),
        false,
    );
    r.h.start();
    r.h.wait_until("no panel", |h| {
        h.runtime.status().published().panel == BarPanelState::NoPanelZeroSize
    });
    assert!(matches!(r.service.read(), WeatherRead::NotRunning(_)));

    // Paused.
    let r = rig(bar_config(WeatherProvider::Auto, false), &source);
    let before = source.requests().len();
    r.h.registry.pause(r.h.handle).expect("pause");
    assert!(matches!(r.service.read(), WeatherRead::NotRunning(_)));
    assert_eq!(source.requests().len(), before);

    // A failed panel (Manual mode, native create failure).
    let h = Harness::new(Native::Manual, bar_config(WeatherProvider::Auto, false), vec![
        builtin(),
    ]);
    h.start();
    h.windows.wait_opened(1);
    h.windows.failed(h.windows.last(), "no native window");
    h.wait_until("failed", |h| {
        h.runtime.status().published().panel == BarPanelState::Failed
    });
    let dir = tempfile::tempdir().expect("dir");
    let parts = WeatherParts::new(
        Arc::clone(&source) as Arc<dyn LocationSource>,
        None,
        dir.path().to_path_buf(),
    );
    let service = WeatherService::new(parts, h.runtime.status());
    assert!(matches!(service.read(), WeatherRead::NotRunning(_)));
    assert_eq!(
        source.requests().len(),
        before,
        "zero reads in every non-serving state"
    );
}

/// A source that parks until released.
struct Gated {
    reads: AtomicUsize,
    entered: Mutex<bool>,
    entered_cv: Condvar,
    released: AtomicBool,
    release_cv: Condvar,
    release_lock: Mutex<()>,
    outcome: LocationOutcome,
}

impl Gated {
    fn new(outcome: LocationOutcome) -> Arc<Self> {
        Arc::new(Self {
            reads: AtomicUsize::new(0),
            entered: Mutex::new(false),
            entered_cv: Condvar::new(),
            released: AtomicBool::new(false),
            release_cv: Condvar::new(),
            release_lock: Mutex::new(()),
            outcome,
        })
    }

    #[allow(clippy::significant_drop_tightening)] // the guard is the wait state
    fn wait_entered(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut entered = lock(&self.entered);
        while !*entered {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "the read never started");
            entered = self.entered_cv.wait_timeout(entered, left).expect("wait").0;
        }
    }

    fn release(&self) {
        let _guard = lock(&self.release_lock);
        self.released.store(true, Ordering::SeqCst);
        self.release_cv.notify_all();
    }
}

impl LocationSource for Gated {
    #[allow(clippy::significant_drop_tightening)] // the guard is the wait state
    fn read(&self, _: LocationRequest) -> LocationOutcome {
        self.reads.fetch_add(1, Ordering::SeqCst);
        *lock(&self.entered) = true;
        self.entered_cv.notify_all();
        let mut guard = lock(&self.release_lock);
        while !self.released.load(Ordering::SeqCst) {
            guard = self.release_cv.wait(guard).expect("wait");
        }
        self.outcome
    }
}

#[test]
fn concurrent_calls_share_one_read() {
    let gate = Gated::new(fix(3.0, 4.0, false));
    let r = rig_with(
        bar_config(WeatherProvider::Auto, false),
        Arc::clone(&gate) as Arc<dyn LocationSource>,
        WeatherTiming::default(),
        true,
    );
    let calls: Vec<_> = (0..4)
        .map(|_| {
            let service = Arc::clone(&r.service);
            thread::spawn(move || service.read())
        })
        .collect();
    gate.wait_entered();
    thread::sleep(Duration::from_millis(100));
    gate.release();
    for call in calls {
        let weather = ready(call.join().expect("call"));
        assert_eq!(weather.location, BarWeatherLocation::CoreLocation {
            latitude: 3.0,
            longitude: 4.0,
            reduced_accuracy: false
        });
    }
    assert_eq!(gate.reads.load(Ordering::SeqCst), 1, "one read for four calls");
}

#[test]
fn a_hung_read_times_out_joiners_without_starting_a_second_read() {
    let gate = Gated::new(fix(3.0, 4.0, false));
    let timing = WeatherTiming {
        read_timeout: Duration::from_millis(50),
        join_margin: Duration::from_millis(50),
        ..WeatherTiming::default()
    };
    let r = rig_with(
        bar_config(WeatherProvider::Auto, false),
        Arc::clone(&gate) as Arc<dyn LocationSource>,
        timing,
        true,
    );
    let leader = {
        let service = Arc::clone(&r.service);
        thread::spawn(move || service.read())
    };
    gate.wait_entered();
    let started = Instant::now();
    let joined = ready(r.service.read());
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(joined.location, BarWeatherLocation::DefaultLocation {
        query: "Lisbon".into()
    });
    assert_eq!(
        gate.reads.load(Ordering::SeqCst),
        1,
        "no second read while one hangs"
    );
    gate.release();
    let _ = ready(leader.join().expect("leader"));
}

#[test]
fn a_stop_or_reconfigure_during_the_read_is_stale_and_records_nothing() {
    let gate = Gated::new(fix(3.0, 4.0, false));
    let r = rig_with(
        bar_config(WeatherProvider::Auto, false),
        Arc::clone(&gate) as Arc<dyn LocationSource>,
        WeatherTiming::default(),
        true,
    );
    let call = {
        let service = Arc::clone(&r.service);
        thread::spawn(move || service.read())
    };
    gate.wait_entered();
    r.h.reload(bar_config(WeatherProvider::Auto, true));
    gate.release();
    assert!(
        matches!(call.join().expect("call"), WeatherRead::Stale),
        "revision changed"
    );
    assert_eq!(r.h.runtime.status().read().weather, WeatherStatus::default());

    // Stop during a read.
    let gate = Gated::new(fix(3.0, 4.0, false));
    let r = rig_with(
        bar_config(WeatherProvider::Auto, false),
        Arc::clone(&gate) as Arc<dyn LocationSource>,
        WeatherTiming::default(),
        true,
    );
    let call = {
        let service = Arc::clone(&r.service);
        thread::spawn(move || service.read())
    };
    gate.wait_entered();
    r.h.registry.pause(r.h.handle).expect("pause");
    gate.release();
    assert!(
        matches!(call.join().expect("call"), WeatherRead::Stale),
        "stopped"
    );
    assert_eq!(r.h.runtime.status().read().weather, WeatherStatus::default());
}

#[test]
fn a_weather_note_changes_no_sequence_and_a_stop_clears_it() {
    let source = Arc::new(FakeLocationSource::unavailable(LocationUnavailable::Denied));
    let r = rig(bar_config(WeatherProvider::Auto, false), &source);
    let before = r.h.runtime.status().published();
    let _ = ready(r.service.read());
    let after = r.h.runtime.status().published();
    assert_eq!(
        before.sequence, after.sequence,
        "the visuals and their sequence are unchanged"
    );
    assert_eq!(
        r.h.runtime.status().read().weather.location,
        Some(LocationDiagnostic::Denied)
    );
    r.h.registry.pause(r.h.handle).expect("pause");
    assert_eq!(r.h.runtime.status().read().weather, WeatherStatus::default());
    // A note for a finished run is refused.
    assert!(!r.h.runtime.status().weather_note(1, WeatherStatus::default()));
}

// ------------------------------------------------------------ late consent

fn undecided() -> Arc<FakeLocationSource> {
    Arc::new(FakeLocationSource::unavailable(
        LocationUnavailable::NotDeterminedTimeout,
    ))
}

fn epoch(r: &Rig) -> u64 { r.h.runtime.status().published().weather_epoch }

#[test]
fn a_decision_after_the_consent_timeout_drops_the_cache_and_bumps_the_epoch_once() {
    let source = undecided();
    let r = rig(bar_config(WeatherProvider::Auto, false), &source);
    let first = ready(r.service.read());
    assert_eq!(first.location, BarWeatherLocation::DefaultLocation {
        query: "Lisbon".into()
    });
    assert!(source.watcher_pending());
    // Inside the reuse window the undecided answer is still reused.
    let _ = ready(r.service.read());
    assert_eq!(source.requests().len(), 1);
    let (sequence, before) = {
        let published = r.h.runtime.status().published();
        (published.sequence, published.weather_epoch)
    };
    source.set(fix(38.7, -9.1, false));
    assert!(source.decide_consent());
    let published = r.h.runtime.status().published();
    assert_eq!(published.weather_epoch, before + 1, "bumped exactly once");
    assert!(published.sequence > sequence, "an observable change");
    assert!(!source.decide_consent(), "a decision is delivered once");
    assert_eq!(epoch(&r), before + 1);
    // The cache was dropped: the next read locates.
    let located = ready(r.service.read());
    assert!(matches!(
        located.location,
        BarWeatherLocation::CoreLocation { .. }
    ));
    assert_eq!(source.requests().len(), 2);
}

#[test]
fn a_decision_after_bar_stops_cancels_the_watcher_and_bumps_nothing() {
    let source = undecided();
    let r = rig(bar_config(WeatherProvider::Auto, false), &source);
    let _ = ready(r.service.read());
    assert!(source.watcher_pending());
    let before = epoch(&r);
    r.h.registry.pause(r.h.handle).expect("pause");
    assert!(!source.watcher_pending(), "the manager is released on stop");
    assert_eq!(source.watchers_cancelled(), 1);
    assert!(!source.decide_consent());
    assert_eq!(epoch(&r), before);
    assert_eq!(source.notifications(), 0);
}

#[test]
fn a_decision_of_a_superseded_run_never_bumps_the_new_run() {
    let source = undecided();
    let r = rig(bar_config(WeatherProvider::Auto, false), &source);
    let _ = ready(r.service.read());
    // Capture the notifier of generation 1 by deciding after a restart: the
    // stop cancels the watcher, so the late decision is dropped.
    r.h.registry.pause(r.h.handle).expect("pause");
    r.h.registry.resume(r.h.handle).expect("resume");
    r.h.wait_until("running again", |h| {
        let published = h.runtime.status().published();
        published.state == BarRuntimeState::Running && published.panel == BarPanelState::Loaded
    });
    let before = epoch(&r);
    assert!(!source.decide_consent());
    assert_eq!(epoch(&r), before);
}

#[test]
fn a_newer_read_replaces_the_watcher_and_never_adds_a_second() {
    let source = undecided();
    let r = rig(bar_config(WeatherProvider::Auto, false), &source);
    let _ = ready(r.service.read());
    r.clock.advance(Duration::from_secs(60));
    let _ = ready(r.service.read());
    assert_eq!(source.requests().len(), 2);
    assert_eq!(source.watchers_created(), 1, "one manager at a time");
    let before = epoch(&r);
    assert!(source.decide_consent());
    assert_eq!(epoch(&r), before + 1, "one bump for one decision");
}

#[test]
fn other_outcomes_leave_no_watcher_and_no_epoch_change() {
    for reason in [LocationUnavailable::Denied, LocationUnavailable::Timeout] {
        let source = Arc::new(FakeLocationSource::unavailable(reason));
        let r = rig(bar_config(WeatherProvider::Auto, false), &source);
        let before = epoch(&r);
        let _ = ready(r.service.read());
        assert!(!source.watcher_pending());
        assert!(!source.decide_consent());
        assert_eq!(epoch(&r), before);
    }
}
