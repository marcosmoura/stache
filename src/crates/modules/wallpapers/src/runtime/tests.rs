//! Runtime tests with injected fake desktop, clock and random sources.
//!
//! No test calls the real setter, starts a real rotation timer with a real
//! sleep, or reads the user's home. Async work is observed through recorded
//! native calls, typed outcomes or `yield_now` barriers with a finite limit.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, thread};

use stache_macos::{
    DesktopBackend, DesktopError, DesktopOperation, DisplayEntry, DisplayId, DisplaySnapshot,
    FakeDesktop, GuardedDesktopBackend,
};
use stache_sdk::ipc::IpcHandler;
use stache_sdk::{
    AppPaths, CancellationToken, ErrorCategory, EventBus, LogRecord, Logger, MainThreadExecutor,
    ModuleHandle, Operation, Permission, PermissionError, PermissionProvider, PermissionStatus,
    Registry, RegistryError, ShutdownWait, ThreadError, UnavailableWindows,
};

use super::clock::{ManualClock, RuntimeClock};
use super::service::{ServiceConfig, WALLPAPERS_SERVER_BUDGET, WallpapersService};
use super::types::{
    WallpaperScreen, WallpaperSelection, WallpapersOutcomeCause, WallpapersOutcomeStatus,
    WallpapersSetOutcome, WallpapersSetParams, WallpapersTargetOutcome, WallpapersTargetResult,
};
use super::{Processor, RealProcessor};
use crate::apply::{PlannedTarget, PrepareError, Prepared};
use crate::config::{WallpaperMode, WallpapersConfig};
use crate::sources::SourceContext;
use crate::test_support::{TestEnv, entry, read_rgb8, snapshot, solid};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct InlineMain;

impl MainThreadExecutor for InlineMain {
    fn is_main_thread(&self) -> bool { true }

    fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
        job();
        Ok(())
    }
}

struct AllowAll;

impl PermissionProvider for AllowAll {
    fn status(&self, _: Permission) -> PermissionStatus { PermissionStatus::Granted }

    fn request(&self, _: Permission) -> Result<PermissionStatus, PermissionError> {
        Ok(PermissionStatus::Granted)
    }
}

#[derive(Default)]
struct CaptureLogger {
    records: Mutex<Vec<LogRecord>>,
}

impl Logger for CaptureLogger {
    fn log(&self, record: LogRecord) { lock(&self.records).push(record); }
}

impl CaptureLogger {
    fn contains(&self, needle: &str) -> bool {
        lock(&self.records).iter().any(|record| record.message.contains(needle))
    }
}

struct ScriptedRandom {
    draws: Mutex<std::collections::VecDeque<usize>>,
    fallback: usize,
}

impl ScriptedRandom {
    fn new(draws: Vec<usize>) -> Arc<Self> {
        Arc::new(Self {
            draws: Mutex::new(draws.into()),
            fallback: 0,
        })
    }
}

impl super::RandomSource for ScriptedRandom {
    fn index(&self, length: usize) -> usize {
        lock(&self.draws).pop_front().unwrap_or(self.fallback) % length.max(1)
    }
}

struct Harness {
    env: TestEnv,
    registry: Arc<Registry>,
    handle: ModuleHandle<WallpapersConfig>,
    service: Arc<WallpapersService>,
    desktop: FakeDesktop,
    clock: Arc<ManualClock>,
    random: Arc<ScriptedRandom>,
    logger: Arc<CaptureLogger>,
    config_dir: PathBuf,
}

impl Harness {
    fn new(config: WallpapersConfig, displays: DisplaySnapshot) -> Self {
        let env = TestEnv::new();
        let config_dir = env.path("config");
        fs::create_dir_all(&config_dir).expect("config directory");
        let desktop = FakeDesktop::with_snapshot(displays);
        let backend: Arc<dyn GuardedDesktopBackend> = Arc::new(desktop.clone());
        Self::build(config, env, config_dir, desktop, backend)
    }

    fn blocking(
        config: WallpapersConfig,
        displays: DisplaySnapshot,
    ) -> (Self, Sender<()>, Receiver<()>) {
        let env = TestEnv::new();
        let config_dir = env.path("config");
        fs::create_dir_all(&config_dir).expect("config directory");
        let desktop = FakeDesktop::with_snapshot(displays);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let backend: Arc<dyn GuardedDesktopBackend> = Arc::new(BlockingDesktop::new(
            desktop.clone(),
            Some(entered_tx),
            Some(release_rx),
        ));
        let harness = Self::build(config, env, config_dir, desktop, backend);
        (harness, release_tx, entered_rx)
    }

    fn build(
        config: WallpapersConfig,
        env: TestEnv,
        config_dir: PathBuf,
        desktop: FakeDesktop,
        backend: Arc<dyn GuardedDesktopBackend>,
    ) -> Self {
        let clock = ManualClock::new();
        let random = ScriptedRandom::new(Vec::new());
        Self::build_with(
            config,
            env,
            config_dir,
            desktop,
            backend,
            clock,
            random,
            Arc::new(RealProcessor),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_with(
        config: WallpapersConfig,
        env: TestEnv,
        config_dir: PathBuf,
        desktop: FakeDesktop,
        backend: Arc<dyn GuardedDesktopBackend>,
        clock: Arc<ManualClock>,
        random: Arc<ScriptedRandom>,
        processor: Arc<dyn Processor>,
    ) -> Self {
        let source = SourceContext::new(config_dir.clone(), Some(env.root().to_path_buf()));
        let service = WallpapersService::new(ServiceConfig {
            paths: env.paths().clone(),
            source,
            backend,
            clock: clock.clone(),
            random: random.clone(),
            processor,
        })
        .expect("service");
        let logger = Arc::new(CaptureLogger::default());
        let registry = Arc::new(Registry::new(
            logger.clone(),
            Arc::new(EventBus::new()),
            Arc::new(InlineMain),
            Arc::new(AllowAll),
            Arc::new(UnavailableWindows),
        ));
        let handle =
            registry.register(service.module(), config).expect("register wallpapers module");
        let watch = registry.accepted_watch(handle).expect("accepted watch");
        service.install_accepted_watch(watch);
        Self {
            env,
            registry,
            handle,
            service,
            desktop,
            clock,
            random,
            logger,
            config_dir,
        }
    }

    fn png(&self, name: &str, color: [u8; 3]) -> PathBuf { self.env.png(name, solid(8, 8, color)) }

    fn params(
        &self,
        selection: WallpaperSelection,
        screen: WallpaperScreen,
    ) -> WallpapersSetParams {
        WallpapersSetParams {
            config_directory: self.config_dir.display().to_string(),
            selection,
            screen,
        }
    }

    fn start(&self) { self.registry.start_all().expect("start"); }

    fn reload(&self, config: WallpapersConfig) {
        self.registry.reload(self.handle, config).expect("reload");
    }

    fn status(&self) -> super::ServiceStatus { self.service.status() }

    fn session(&self) -> Arc<super::session::SessionControl> {
        self.service.session_handle().expect("session")
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.service.begin_shutdown();
        let _ = self.registry.shutdown();
        let _ = self.service.finish_shutdown(&TestWait::new());
    }
}

struct TestWait {
    deadline: Instant,
}

impl TestWait {
    fn new() -> Self {
        Self {
            deadline: Instant::now() + Duration::from_secs(2),
        }
    }
}

impl ShutdownWait for TestWait {
    fn now(&self) -> Instant { Instant::now() }

    fn deadline(&self) -> Instant { self.deadline }

    fn wait(&self, timeout: Duration) { thread::sleep(timeout.min(Duration::from_millis(5))); }
}

struct BlockingDesktop {
    inner: FakeDesktop,
    entered: Arc<Mutex<Option<Sender<()>>>>,
    release: Arc<Mutex<Option<Receiver<()>>>>,
}

impl BlockingDesktop {
    fn new(inner: FakeDesktop, entered: Option<Sender<()>>, release: Option<Receiver<()>>) -> Self {
        Self {
            inner,
            entered: Arc::new(Mutex::new(entered)),
            release: Arc::new(Mutex::new(release)),
        }
    }

    fn block(&self) {
        let entered = lock(&self.entered).take();
        if let Some(entered) = entered {
            let _ = entered.send(());
        }
        let release = lock(&self.release).take();
        if let Some(release) = release {
            let _ = release.recv();
        }
    }
}

impl DesktopBackend for BlockingDesktop {
    fn snapshot(&self) -> Result<DisplaySnapshot, DesktopError> { self.inner.snapshot() }

    fn set_image(
        &self,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &Path,
    ) -> Result<(), DesktopError> {
        self.inner.set_image(display, expected, image)
    }
}

impl GuardedDesktopBackend for BlockingDesktop {
    fn snapshot_guarded(
        &self,
        operation: &DesktopOperation,
    ) -> Result<DisplaySnapshot, DesktopError> {
        operation.check("display snapshot")?;
        self.block();
        self.inner.snapshot_guarded(operation)
    }

    fn set_image_guarded(
        &self,
        operation: &DesktopOperation,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &Path,
    ) -> Result<(), DesktopError> {
        self.inner.set_image_guarded(operation, display, expected, image)
    }
}

struct SlowDesktop {
    inner: FakeDesktop,
    clock: Arc<ManualClock>,
    snapshot_advance: Duration,
    set_advance: Duration,
}

impl DesktopBackend for SlowDesktop {
    fn snapshot(&self) -> Result<DisplaySnapshot, DesktopError> { self.inner.snapshot() }

    fn set_image(
        &self,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &Path,
    ) -> Result<(), DesktopError> {
        self.inner.set_image(display, expected, image)
    }
}

impl GuardedDesktopBackend for SlowDesktop {
    fn snapshot_guarded(
        &self,
        operation: &DesktopOperation,
    ) -> Result<DisplaySnapshot, DesktopError> {
        self.clock.advance(self.snapshot_advance);
        self.inner.snapshot_guarded(operation)
    }

    fn set_image_guarded(
        &self,
        operation: &DesktopOperation,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &Path,
    ) -> Result<(), DesktopError> {
        self.clock.advance(self.set_advance);
        self.inner.set_image_guarded(operation, display, expected, image)
    }
}

fn single_display() -> DisplaySnapshot { snapshot(vec![entry(1, 16.0, 16.0, 1.0)]) }

fn dual_display() -> DisplaySnapshot {
    snapshot(vec![entry(1, 16.0, 16.0, 1.0), entry(2, 16.0, 16.0, 1.0)])
}

fn enabled_with_list(list: &[&str], interval: u64) -> WallpapersConfig {
    WallpapersConfig {
        enabled: true,
        list: list.iter().map(|name| (*name).to_owned()).collect(),
        interval,
        mode: WallpaperMode::Sequential,
        ..WallpapersConfig::default()
    }
}

fn wait_until(mut predicate: impl FnMut() -> bool) {
    let started = Instant::now();
    while !predicate() {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "condition was not reached"
        );
        thread::yield_now();
    }
}

fn wait_idle(harness: &Harness) {
    wait_until(|| {
        let status = harness.status();
        !status.executing && !status.queued
    });
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------------------
// Manual requests
// ---------------------------------------------------------------------------

#[test]
fn a_disabled_explicit_file_set_succeeds_without_a_runtime_session() {
    let harness = Harness::new(WallpapersConfig::default(), single_display());
    let file = harness.env.png("external.png", solid(8, 8, [10, 20, 30]));
    let outcome = harness.service.set(&harness.params(
        WallpaperSelection::File {
            path: file.display().to_string(),
        },
        WallpaperScreen::Main,
    ));
    assert!(outcome.is_full_success(), "{outcome:?}");
    assert_eq!(outcome.applied, vec![1]);
    let calls = harness.desktop.calls();
    assert_eq!(calls.len(), 1);
    let pixels = read_rgb8(&calls[0].image);
    assert_eq!(pixels.get_pixel(0, 0).0, [10, 20, 30]);
    assert_eq!(harness.status().session, None, "no rotation session was created");
}

#[test]
fn a_collection_index_is_zero_based_and_selects_that_pool_entry() {
    let harness = Harness::new(
        WallpapersConfig {
            list: vec!["a.png".to_owned(), "b.png".to_owned()],
            ..WallpapersConfig::default()
        },
        single_display(),
    );
    harness.png("config/a.png", [255, 0, 0]);
    harness.png("config/b.png", [0, 255, 0]);
    let outcome = harness.service.set(&harness.params(
        WallpaperSelection::Collection { index: 1 },
        WallpaperScreen::Main,
    ));
    assert!(outcome.is_full_success(), "{outcome:?}");
    let call = &harness.desktop.calls()[0];
    assert_eq!(read_rgb8(&call.image).get_pixel(0, 0).0, [0, 255, 0]);
}

#[test]
fn a_name_selects_the_first_matching_accepted_pool_entry_like_a_collection() {
    let harness = Harness::new(
        WallpapersConfig {
            list: vec![
                "a.png".to_owned(),
                "dup/B.png".to_owned(),
                "b.PNG".to_owned(),
            ],
            ..WallpapersConfig::default()
        },
        single_display(),
    );
    fs::create_dir_all(harness.config_dir.join("dup")).expect("dup directory");
    harness.png("config/a.png", [255, 0, 0]);
    harness.png("config/dup/B.png", [0, 255, 0]);
    harness.png("config/b.PNG", [0, 0, 255]);
    // Stem, case-insensitive; the earlier of two matching entries wins.
    let outcome = harness.service.set(&harness.params(
        WallpaperSelection::Name { name: "b".to_owned() },
        WallpaperScreen::Main,
    ));
    assert!(outcome.is_full_success(), "{outcome:?}");
    let call = &harness.desktop.calls()[0];
    assert_eq!(read_rgb8(&call.image).get_pixel(0, 0).0, [0, 255, 0]);
    // The full file name matches too, and the committed cursor follows the
    // matched entry exactly like `Collection`.
    let outcome = harness.service.set(&harness.params(
        WallpaperSelection::Name { name: "A.PNG".to_owned() },
        WallpaperScreen::Main,
    ));
    assert!(outcome.is_full_success(), "{outcome:?}");
}

#[test]
fn an_unknown_name_is_a_typed_invalid_selection_before_any_snapshot() {
    let harness = Harness::new(
        WallpapersConfig {
            list: vec!["a.png".to_owned()],
            ..WallpapersConfig::default()
        },
        single_display(),
    );
    harness.png("config/a.png", [1, 2, 3]);
    for name in ["missing", " a ", ""] {
        let outcome = harness.service.set(&harness.params(
            WallpaperSelection::Name { name: name.to_owned() },
            WallpaperScreen::Main,
        ));
        assert_eq!(outcome.status, WallpapersOutcomeStatus::Failure, "{name:?}");
        assert!(
            matches!(&outcome.cause, Some(WallpapersOutcomeCause::InvalidSelection { message })
                if message.contains("no wallpaper named")),
            "{outcome:?}"
        );
    }
    assert_eq!(harness.desktop.guarded_snapshot_reads(), 0);
    assert!(harness.desktop.calls().is_empty());
}

#[test]
fn an_out_of_range_collection_index_fails_before_any_snapshot() {
    let harness = Harness::new(
        WallpapersConfig {
            list: vec!["a.png".to_owned()],
            ..WallpapersConfig::default()
        },
        single_display(),
    );
    harness.png("config/a.png", [1, 2, 3]);
    let outcome = harness.service.set(&harness.params(
        WallpaperSelection::Collection { index: 4 },
        WallpaperScreen::Main,
    ));
    assert_eq!(outcome.status, WallpapersOutcomeStatus::Failure);
    assert!(matches!(
        outcome.cause,
        Some(WallpapersOutcomeCause::InvalidSelection { .. })
    ));
    assert_eq!(harness.desktop.guarded_snapshot_reads(), 0);
    assert!(harness.desktop.calls().is_empty());
}

#[test]
fn a_different_config_directory_is_refused_before_any_work() {
    let harness = Harness::new(WallpapersConfig::default(), single_display());
    let other = harness.env.path("other");
    fs::create_dir_all(&other).expect("other dir");
    let params = WallpapersSetParams {
        config_directory: other.display().to_string(),
        selection: WallpaperSelection::Random,
        screen: WallpaperScreen::All,
    };
    let outcome = harness.service.set(&params);
    assert!(matches!(
        outcome.cause,
        Some(WallpapersOutcomeCause::DifferentConfig { .. })
    ));
    assert_eq!(harness.desktop.guarded_snapshot_reads(), 0);
    assert!(harness.desktop.calls().is_empty());
}

#[test]
fn an_explicit_file_is_independent_of_an_invalid_configured_pool() {
    let harness = Harness::new(
        WallpapersConfig {
            path: "missing-directory".to_owned(),
            ..WallpapersConfig::default()
        },
        single_display(),
    );
    let file = harness.png("external.png", [7, 8, 9]);
    let outcome = harness.service.set(&harness.params(
        WallpaperSelection::File {
            path: file.display().to_string(),
        },
        WallpaperScreen::Main,
    ));
    assert!(outcome.is_full_success(), "{outcome:?}");
    assert_eq!(harness.desktop.calls().len(), 1);
}

#[test]
fn an_empty_pool_random_request_is_a_source_failure() {
    let harness = Harness::new(WallpapersConfig::default(), single_display());
    let outcome = harness
        .service
        .set(&harness.params(WallpaperSelection::Random, WallpaperScreen::Main));
    assert_eq!(outcome.status, WallpapersOutcomeStatus::Failure);
    assert!(matches!(
        outcome.cause,
        Some(WallpapersOutcomeCause::Source { .. })
    ));
    assert_eq!(harness.desktop.guarded_snapshot_reads(), 0);
}

#[test]
fn a_service_without_a_typed_watch_reports_no_accepted_configuration() {
    let env = TestEnv::new();
    let config_dir = env.path("config");
    fs::create_dir_all(&config_dir).expect("config directory");
    let desktop = FakeDesktop::with_snapshot(single_display());
    let service = WallpapersService::new(ServiceConfig {
        paths: env.paths().clone(),
        source: SourceContext::new(config_dir.clone(), Some(env.root().to_path_buf())),
        backend: Arc::new(desktop),
        clock: ManualClock::new(),
        random: ScriptedRandom::new(Vec::new()),
        processor: Arc::new(RealProcessor),
    })
    .expect("service");
    let outcome = service.set(&WallpapersSetParams {
        config_directory: config_dir.display().to_string(),
        selection: WallpaperSelection::Random,
        screen: WallpaperScreen::Main,
    });
    assert!(matches!(
        outcome.cause,
        Some(WallpapersOutcomeCause::NoAcceptedConfig)
    ));
}

#[test]
fn random_all_draws_independently_for_each_display() {
    let harness = Harness::new(
        WallpapersConfig {
            list: vec!["a.png".to_owned(), "b.png".to_owned()],
            ..WallpapersConfig::default()
        },
        dual_display(),
    );
    harness.png("config/a.png", [255, 0, 0]);
    harness.png("config/b.png", [0, 255, 0]);
    *lock(&harness.random.draws) = [0, 1].into();
    let outcome = harness
        .service
        .set(&harness.params(WallpaperSelection::Random, WallpaperScreen::All));
    assert!(outcome.is_full_success(), "{outcome:?}");
    let calls = harness.desktop.calls();
    assert_eq!(calls.len(), 2);
    let colors: Vec<[u8; 3]> =
        calls.iter().map(|call| read_rgb8(&call.image).get_pixel(0, 0).0).collect();
    assert_eq!(colors, vec![[255, 0, 0], [0, 255, 0]]);
}

#[test]
fn a_partial_native_failure_reports_each_display_truthfully() {
    let harness = Harness::new(
        WallpapersConfig {
            list: vec!["a.png".to_owned()],
            ..WallpapersConfig::default()
        },
        dual_display(),
    );
    harness.png("config/a.png", [1, 2, 3]);
    harness.desktop.set_outcome(
        DisplayId::new(2),
        Err(DesktopError::Native {
            display: DisplayId::new(2),
            message: "denied by test".to_owned(),
        }),
    );
    let outcome = harness
        .service
        .set(&harness.params(WallpaperSelection::Collection { index: 0 }, WallpaperScreen::All));
    assert_eq!(outcome.status, WallpapersOutcomeStatus::Partial);
    assert_eq!(outcome.applied, vec![1]);
    assert_eq!(outcome.failed, vec![2]);
    assert!(outcome.not_attempted.is_empty());
    assert!(outcome.unknown.is_empty());
    assert_eq!(outcome.targets.len(), 2);
    assert!(matches!(
        outcome.targets[0].target,
        WallpapersTargetResult::Applied
    ));
    assert!(matches!(
        outcome.targets[1].target,
        WallpapersTargetResult::Failed { .. }
    ));
}

#[test]
fn a_display_lost_before_its_set_fails_it_and_attempts_nothing_after_it() {
    let harness = Harness::new(
        WallpapersConfig {
            list: vec!["a.png".to_owned()],
            ..WallpapersConfig::default()
        },
        dual_display(),
    );
    harness.png("config/a.png", [1, 2, 3]);
    harness.desktop.set_outcome(
        DisplayId::new(1),
        Err(DesktopError::DisplayMissing(DisplayId::new(1))),
    );
    let outcome = harness
        .service
        .set(&harness.params(WallpaperSelection::Collection { index: 0 }, WallpaperScreen::All));
    assert_eq!(outcome.status, WallpapersOutcomeStatus::Failure);
    assert!(outcome.applied.is_empty());
    assert_eq!(outcome.failed, vec![1]);
    assert_eq!(outcome.not_attempted, vec![2]);
    assert!(outcome.unknown.is_empty());
}

// ---------------------------------------------------------------------------
// Queue, deadline and shutdown
// ---------------------------------------------------------------------------

#[test]
fn queue_admission_is_bounded_and_deadline_expiry_never_reaches_the_desktop() {
    let harness = Harness::blocking(
        WallpapersConfig {
            enabled: true,
            list: vec!["a.png".to_owned()],
            interval: 1,
            ..WallpapersConfig::default()
        },
        single_display(),
    );
    let (harness, release, entered) = harness;
    harness.png("config/a.png", [1, 2, 3]);
    harness.start();
    entered
        .recv_timeout(Duration::from_secs(5))
        .expect("the initial snapshot is blocked");

    // Let the timer enqueue one rotation behind the executing initial job.
    let session = harness.session();
    harness.clock.advance(Duration::from_secs(1));
    session.signal().notify();
    wait_until(|| harness.status().queued);

    let full = harness.service.set_with_deadline(
        &harness.params(
            WallpaperSelection::Collection { index: 0 },
            WallpaperScreen::Main,
        ),
        None,
    );
    assert_eq!(full.status, WallpapersOutcomeStatus::Failure);
    assert_eq!(full.cause, Some(WallpapersOutcomeCause::QueueFull));

    let past = harness
        .clock
        .now()
        .checked_sub(Duration::from_secs(1))
        .expect("clock is past the Unix epoch");
    let expired = harness.service.set_with_deadline(
        &harness.params(
            WallpaperSelection::Collection { index: 0 },
            WallpaperScreen::Main,
        ),
        Some(past),
    );
    assert_eq!(expired.status, WallpapersOutcomeStatus::Cancelled);
    assert_eq!(expired.cause, Some(WallpapersOutcomeCause::DeadlineExpired));
    assert!(
        harness.desktop.calls().is_empty(),
        "no queued job issued a native call"
    );

    release.send(()).expect("release initial snapshot");
    wait_until(|| harness.desktop.calls().len() >= 2);
}

#[test]
fn begin_shutdown_wakes_a_blocked_manual_request_and_retains_owned_cleanup() {
    let harness = Harness::blocking(WallpapersConfig::default(), single_display());
    let (harness, release, entered) = harness;
    let file = harness.png("external.png", [4, 5, 6]);
    let service = Arc::clone(&harness.service);
    let params = harness.params(
        WallpaperSelection::File {
            path: file.display().to_string(),
        },
        WallpaperScreen::Main,
    );
    let call = thread::spawn(move || service.set(&params));
    entered
        .recv_timeout(Duration::from_secs(5))
        .expect("the manual snapshot is blocked");

    harness.service.begin_shutdown();
    let outcome = call.join().expect("manual request returned");
    assert_eq!(outcome.status, WallpapersOutcomeStatus::Cancelled);
    assert!(harness.desktop.calls().is_empty());

    release.send(()).expect("release the noninterruptible snapshot");
    assert!(
        harness.service.finish_shutdown(&TestWait::new()),
        "the coordinator must finish once its owned stage returns"
    );
}

#[test]
fn a_publication_between_snapshot_and_native_suppresses_the_stale_apply() {
    let harness = Harness::blocking(WallpapersConfig::default(), single_display());
    let (harness, release, entered) = harness;
    let file = harness.png("external.png", [11, 12, 13]);
    let service = Arc::clone(&harness.service);
    let params = harness.params(
        WallpaperSelection::File {
            path: file.display().to_string(),
        },
        WallpaperScreen::Main,
    );
    let call = thread::spawn(move || service.set(&params));
    entered
        .recv_timeout(Duration::from_secs(5))
        .expect("the manual snapshot is blocked");

    harness.reload(WallpapersConfig {
        radius: 2,
        ..WallpapersConfig::default()
    });
    release.send(()).expect("release the snapshot");
    let outcome = call.join().expect("manual request returned");
    assert!(
        !outcome.is_full_success(),
        "a stale accepted pair must not produce a native success"
    );
    assert!(harness.desktop.calls().is_empty());

    let second = harness.service.set(&harness.params(
        WallpaperSelection::File {
            path: file.display().to_string(),
        },
        WallpaperScreen::Main,
    ));
    assert!(
        second.is_full_success(),
        "the next request uses the new accepted pair"
    );
    assert_eq!(harness.desktop.calls().len(), 1);
}

// ---------------------------------------------------------------------------
// Module lifecycle and timer
// ---------------------------------------------------------------------------

#[test]
fn start_with_interval_zero_runs_one_initial_job_and_creates_no_timer() {
    let harness = Harness::new(enabled_with_list(&["a.png"], 0), single_display());
    harness.png("config/a.png", [9, 9, 9]);
    harness.start();
    wait_until(|| harness.desktop.calls().len() == 1);
    wait_until(|| {
        let status = harness.status();
        !status.executing && !status.queued
    });
    let status = harness.status();
    let session = status.session.expect("running session");
    assert_eq!(session.interval, 0);
    assert!(!session.timer_spawned);
    assert!(!session.deadline_present);
    assert_eq!(harness.desktop.calls().len(), 1);

    harness.registry.pause(harness.handle).expect("pause");
    assert_eq!(harness.status().session, None);
    harness.registry.resume(harness.handle).expect("resume");
    wait_until(|| harness.desktop.calls().len() == 2);
    assert_eq!(harness.status().session.expect("session").interval, 0);
}

#[test]
fn an_empty_enabled_pool_fails_start_and_a_later_fix_retries() {
    let harness = Harness::new(enabled_with_list(&[], 0), single_display());
    let error = harness.registry.start_all().expect_err("empty pool");
    match error {
        RegistryError::Module(error) => {
            assert_eq!(error.operation(), Operation::Start);
            assert_eq!(error.category(), ErrorCategory::Config);
        }
        other => panic!("expected a module start error, got {other:?}"),
    }
    assert_eq!(harness.status().session, None);
    assert!(harness.desktop.calls().is_empty());

    harness.png("config/a.png", [1, 1, 1]);
    harness.reload(enabled_with_list(&["a.png"], 0));
    wait_until(|| harness.desktop.calls().len() == 1);
}

#[test]
fn an_initial_processing_failure_keeps_the_module_running_without_a_retry_loop() {
    let harness = Harness::new(enabled_with_list(&["broken.png"], 0), single_display());
    fs::write(harness.env.path("config/broken.png"), b"not an image").expect("broken source");
    harness.start();
    wait_until(|| harness.logger.contains("rotation failed"));
    let snapshot = harness.registry.snapshot(harness.handle).expect("snapshot");
    assert_eq!(snapshot.status, stache_sdk::ModuleStatus::Running);
    let session = harness.status().session.expect("session");
    assert_eq!(session.interval, 0);
    assert!(!session.timer_spawned);
    assert_eq!(session.full_success_resets, 0);
    assert!(harness.desktop.calls().is_empty());
}

#[test]
fn start_returns_while_the_initial_job_is_blocked_in_the_native_boundary() {
    let harness = Harness::blocking(enabled_with_list(&["a.png"], 60), single_display());
    let (harness, release, entered) = harness;
    harness.png("config/a.png", [2, 2, 2]);
    harness.start();
    entered
        .recv_timeout(Duration::from_secs(5))
        .expect("the initial job reached the blocked snapshot");
    assert_eq!(
        harness.registry.snapshot(harness.handle).expect("snapshot").status,
        stache_sdk::ModuleStatus::Running,
        "start must not wait for image processing or native effects"
    );
    assert!(harness.status().executing);
    release.send(()).expect("release the snapshot");
    wait_until(|| harness.desktop.calls().len() == 1);
}

#[test]
fn a_positive_interval_runs_one_wakeable_timer_and_sequential_rotation() {
    let harness = Harness::new(enabled_with_list(&["a.png", "b.png"], 60), single_display());
    harness.png("config/a.png", [255, 0, 0]);
    harness.png("config/b.png", [0, 255, 0]);
    harness.start();
    wait_until(|| harness.desktop.calls().len() == 1);
    wait_idle(&harness);
    let session = harness.session();
    assert!(session.timer_spawned());
    assert!(session.deadline().is_some());

    harness.clock.advance(Duration::from_secs(60));
    session.signal().notify();
    wait_until(|| harness.desktop.calls().len() == 2);
    let calls = harness.desktop.calls();
    assert_eq!(
        read_rgb8(&calls[0].image).get_pixel(0, 0).0,
        [255, 0, 0],
        "initial sequential choice starts at pool position zero"
    );
    assert_eq!(
        read_rgb8(&calls[1].image).get_pixel(0, 0).0,
        [0, 255, 0],
        "the next tick advances the display's pool cursor"
    );
    assert!(session.timer_spawned(), "exactly one timer remains");
}

#[test]
fn interval_changes_preserve_then_reset_the_deadline_and_zero_removes_the_timer() {
    let harness = Harness::new(enabled_with_list(&["a.png"], 60), single_display());
    harness.png("config/a.png", [3, 3, 3]);
    harness.start();
    wait_until(|| harness.desktop.calls().len() == 1);
    wait_idle(&harness);
    let session = harness.session();
    let first_deadline = session.deadline().expect("initial deadline");

    // Same interval, different effects: a later revision must keep the deadline
    // and must not apply an image.
    harness.reload(WallpapersConfig {
        radius: 4,
        ..enabled_with_list(&["a.png"], 60)
    });
    wait_until(|| session.revision() == 2);
    assert_eq!(session.deadline(), Some(first_deadline));
    assert_eq!(harness.desktop.calls().len(), 1);

    // Interval change resets only the deadline; no image is applied.
    harness.reload(enabled_with_list(&["a.png"], 120));
    wait_until(|| session.revision() == 3);
    let second_deadline = session.deadline().expect("new deadline");
    assert!(second_deadline > first_deadline);
    assert_eq!(harness.desktop.calls().len(), 1);

    // Zero removes the timer without applying an image.
    harness.reload(enabled_with_list(&["a.png"], 0));
    wait_until(|| !session.timer_spawned());
    assert!(session.deadline().is_none());
    assert_eq!(harness.desktop.calls().len(), 1);
}

#[test]
fn a_rejected_reload_preserves_the_last_session_and_deadline() {
    let harness = Harness::new(enabled_with_list(&["a.png"], 60), single_display());
    harness.png("config/a.png", [5, 5, 5]);
    harness.start();
    wait_until(|| harness.desktop.calls().len() == 1);
    wait_idle(&harness);
    let session = harness.session();
    let deadline = session.deadline().expect("deadline");
    let revision = session.revision();

    // A structurally invalid source path is rejected by reconfigure preflight.
    let rejected = WallpapersConfig {
        path: "missing".to_owned(),
        list: Vec::new(),
        ..enabled_with_list(&["a.png"], 120)
    };
    harness
        .registry
        .reload(harness.handle, rejected)
        .expect_err("preflight rejects the candidate");
    assert_eq!(session.revision(), revision);
    assert_eq!(session.deadline(), Some(deadline));
    assert_eq!(harness.status().session.expect("session").interval, 60);
}

#[test]
fn pause_and_stop_clear_the_session_without_touching_explicit_requests() {
    let harness = Harness::new(enabled_with_list(&["a.png"], 60), single_display());
    let file = harness.png("external.png", [6, 6, 6]);
    harness.png("config/a.png", [8, 8, 8]);
    harness.start();
    wait_until(|| harness.desktop.calls().len() == 1);
    harness.registry.pause(harness.handle).expect("pause");
    assert_eq!(harness.status().session, None);

    let outcome = harness.service.set(&harness.params(
        WallpaperSelection::File {
            path: file.display().to_string(),
        },
        WallpaperScreen::Main,
    ));
    assert!(
        outcome.is_full_success(),
        "an explicit request works while paused"
    );

    harness.registry.set_enabled(harness.handle, false).expect("disable");
    let outcome = harness.service.set(&harness.params(
        WallpaperSelection::File {
            path: file.display().to_string(),
        },
        WallpaperScreen::Main,
    ));
    assert!(
        outcome.is_full_success(),
        "a stopped module keeps the action service"
    );
}

// ---------------------------------------------------------------------------
// DTO and handler
// ---------------------------------------------------------------------------

#[test]
fn every_selection_and_outcome_roundtrips_through_serde() {
    let selections = [
        WallpaperSelection::File { path: "/tmp/a.png".to_owned() },
        WallpaperSelection::Collection { index: 3 },
        WallpaperSelection::Name { name: "Sunset".to_owned() },
        WallpaperSelection::Random,
    ];
    for selection in selections {
        let params = WallpapersSetParams {
            config_directory: "/tmp/config".to_owned(),
            selection,
            screen: WallpaperScreen::Index { index: 2 },
        };
        let text = serde_json::to_string(&params).expect("serialize params");
        let back: WallpapersSetParams = serde_json::from_str(&text).expect("deserialize params");
        assert_eq!(back, params);
    }

    let outcomes = [
        WallpapersSetOutcome::refused(
            WallpapersOutcomeStatus::Failure,
            WallpapersOutcomeCause::NoAcceptedConfig,
        ),
        WallpapersSetOutcome::cancelled(WallpapersOutcomeCause::DeadlineExpired),
        WallpapersSetOutcome {
            status: WallpapersOutcomeStatus::Partial,
            cause: Some(WallpapersOutcomeCause::Native {
                message: "one display failed".to_owned(),
            }),
            applied: vec![1],
            failed: vec![2],
            not_attempted: vec![3],
            unknown: vec![4],
            targets: vec![
                WallpapersTargetOutcome {
                    display: 1,
                    target: WallpapersTargetResult::Applied,
                },
                WallpapersTargetOutcome {
                    display: 2,
                    target: WallpapersTargetResult::Failed {
                        reason: super::types::WallpapersTargetFailure::Native,
                        message: "denied".to_owned(),
                    },
                },
                WallpapersTargetOutcome {
                    display: 3,
                    target: WallpapersTargetResult::NotAttempted,
                },
                WallpapersTargetOutcome {
                    display: 4,
                    target: WallpapersTargetResult::Unknown {
                        reason: super::types::WallpapersTargetFailure::Unknown,
                        message: "unknown".to_owned(),
                    },
                },
            ],
        },
    ];
    for outcome in outcomes {
        let text = serde_json::to_string(&outcome).expect("serialize outcome");
        let back: WallpapersSetOutcome = serde_json::from_str(&text).expect("deserialize outcome");
        assert_eq!(back, outcome);
    }
}

#[test]
fn the_set_handler_decodes_params_and_returns_a_typed_outcome() {
    use serde_json::json;

    let harness = Harness::new(WallpapersConfig::default(), single_display());
    let file = harness.png("external.png", [12, 13, 14]);
    let handler = harness.service.handler();
    let params = harness.params(
        WallpaperSelection::File {
            path: file.display().to_string(),
        },
        WallpaperScreen::Main,
    );
    let value = serde_json::to_value(&params).expect("params value");
    let response = handler.handle("set", value).expect("handled");
    let outcome: WallpapersSetOutcome = serde_json::from_value(response).expect("typed outcome");
    assert!(outcome.is_full_success(), "{outcome:?}");

    let error = handler.handle("unknown", json!({})).expect_err("unknown method");
    assert_eq!(error.kind(), stache_sdk::ipc::HandlerErrorKind::UnknownMethod);
}

#[test]
fn the_server_budget_leaves_a_delivery_margin_under_the_client_budget() {
    assert_eq!(WALLPAPERS_SERVER_BUDGET, Duration::from_secs(115));
    assert!(super::WALLPAPERS_CLIENT_BUDGET > WALLPAPERS_SERVER_BUDGET);
    assert_eq!(
        super::WALLPAPERS_CLIENT_BUDGET
            .checked_sub(WALLPAPERS_SERVER_BUDGET)
            .expect("client budget is larger"),
        Duration::from_secs(5)
    );
}

#[test]
fn an_inert_service_has_no_coordinator_thread_or_session() {
    let harness = Harness::new(WallpapersConfig::default(), single_display());
    let status = harness.status();
    assert!(!status.coordinator_running);
    assert!(!status.executing);
    assert!(!status.queued);
    assert_eq!(status.session, None);
}

#[test]
fn an_invalid_explicit_relative_path_is_refused_before_work() {
    let harness = Harness::new(WallpapersConfig::default(), single_display());
    let outcome = harness.service.set(&harness.params(
        WallpaperSelection::File {
            path: "relative.png".to_owned(),
        },
        WallpaperScreen::Main,
    ));
    assert!(matches!(
        outcome.cause,
        Some(WallpapersOutcomeCause::InvalidSelection { .. })
    ));
    assert!(harness.desktop.calls().is_empty());
}

#[test]
fn the_service_budget_is_the_production_request_budget() {
    let harness = Harness::new(WallpapersConfig::default(), single_display());
    let file = harness.png("external.png", [1, 1, 2]);
    let params = harness.params(
        WallpaperSelection::File {
            path: file.display().to_string(),
        },
        WallpaperScreen::Main,
    );
    let outcome = harness.service.set(&params);
    assert!(outcome.is_full_success());
}

#[test]
fn the_service_rejects_a_changed_config_directory_after_canonicalization() {
    let harness = Harness::new(WallpapersConfig::default(), single_display());
    let alias = harness.env.path("config-alias");
    std::os::unix::fs::symlink(&harness.config_dir, &alias).expect("alias");
    let params = WallpapersSetParams {
        config_directory: alias.display().to_string(),
        selection: WallpaperSelection::Random,
        screen: WallpaperScreen::Main,
    };
    let outcome = harness.service.set(&params);
    assert!(
        !matches!(
            outcome.cause,
            Some(WallpapersOutcomeCause::DifferentConfig { .. })
        ),
        "a symlink alias of the same directory is accepted"
    );
}

#[test]
fn every_manual_outcome_has_an_operation_order_target_list() {
    let harness = Harness::new(
        WallpapersConfig {
            list: vec!["a.png".to_owned()],
            ..WallpapersConfig::default()
        },
        dual_display(),
    );
    harness.png("config/a.png", [1, 1, 1]);
    harness.desktop.set_outcome(
        DisplayId::new(2),
        Err(DesktopError::Native {
            display: DisplayId::new(2),
            message: "denied".to_owned(),
        }),
    );
    let outcome = harness
        .service
        .set(&harness.params(WallpaperSelection::Collection { index: 0 }, WallpaperScreen::All));
    assert_eq!(
        outcome.targets.iter().map(|target| target.display).collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[test]
fn concurrent_manual_requests_are_serialized_by_the_one_coordinator() {
    let harness = Harness::new(WallpapersConfig::default(), dual_display());
    let file = harness.png("external.png", [2, 3, 4]);
    let service = Arc::clone(&harness.service);
    let params = harness.params(
        WallpaperSelection::File {
            path: file.display().to_string(),
        },
        WallpaperScreen::All,
    );
    let first = thread::spawn({
        let service = Arc::clone(&service);
        let params = params.clone();
        move || service.set(&params)
    });
    let second = thread::spawn(move || service.set(&params));
    let first = first.join().expect("first request");
    let second = second.join().expect("second request");
    assert!(first.is_full_success(), "{first:?}");
    assert!(second.is_full_success(), "{second:?}");
    assert_eq!(harness.desktop.calls().len(), 4);
}

#[test]
fn a_collection_selection_advances_only_applied_display_cursors() {
    let harness = Harness::new(
        WallpapersConfig {
            list: vec!["a.png".to_owned(), "b.png".to_owned()],
            ..WallpapersConfig::default()
        },
        dual_display(),
    );
    harness.png("config/a.png", [255, 0, 0]);
    harness.png("config/b.png", [0, 255, 0]);
    harness.desktop.set_outcome(
        DisplayId::new(2),
        Err(DesktopError::Native {
            display: DisplayId::new(2),
            message: "denied".to_owned(),
        }),
    );
    let outcome = harness
        .service
        .set(&harness.params(WallpaperSelection::Collection { index: 0 }, WallpaperScreen::All));
    assert_eq!(outcome.applied, vec![1]);
    assert_eq!(outcome.failed, vec![2]);
}

#[test]
fn a_missing_explicit_file_is_a_source_failure_before_the_snapshot() {
    let harness = Harness::new(WallpapersConfig::default(), single_display());
    let outcome = harness.service.set(&harness.params(
        WallpaperSelection::File {
            path: harness.env.path("missing.png").display().to_string(),
        },
        WallpaperScreen::Main,
    ));
    assert!(matches!(
        outcome.cause,
        Some(WallpapersOutcomeCause::Source { .. })
    ));
    assert_eq!(harness.desktop.guarded_snapshot_reads(), 0);
}

#[test]
fn a_config_validation_error_keeps_the_module_at_the_previous_accepted_revision() {
    let harness = Harness::new(enabled_with_list(&["a.png"], 0), single_display());
    harness.png("config/a.png", [3, 3, 3]);
    harness.start();
    wait_until(|| harness.desktop.calls().len() == 1);
    let revision = harness.status().session.expect("session").revision;
    let error = harness
        .registry
        .reload(harness.handle, WallpapersConfig {
            list: vec!["nul\0name.png".to_owned()],
            ..enabled_with_list(&["a.png"], 0)
        })
        .expect_err("NUL list entry is invalid");
    assert!(matches!(error, RegistryError::InvalidConfig { .. }));
    assert_eq!(harness.status().session.expect("session").revision, revision);
}

// ---------------------------------------------------------------------------
// Additional timing, publication and cursor cases
// ---------------------------------------------------------------------------

fn slow_harness(
    config: WallpapersConfig,
    displays: DisplaySnapshot,
    snapshot_advance: Duration,
    set_advance: Duration,
) -> (Harness, Arc<ManualClock>) {
    let env = TestEnv::new();
    let config_dir = env.path("config");
    fs::create_dir_all(&config_dir).expect("config directory");
    let desktop = FakeDesktop::with_snapshot(displays);
    let clock = ManualClock::new();
    let backend: Arc<dyn GuardedDesktopBackend> = Arc::new(SlowDesktop {
        inner: desktop.clone(),
        clock: Arc::clone(&clock),
        snapshot_advance,
        set_advance,
    });
    let harness = Harness::build_with(
        config,
        env,
        config_dir,
        desktop,
        backend,
        Arc::clone(&clock),
        ScriptedRandom::new(Vec::new()),
        Arc::new(RealProcessor),
    );
    (harness, clock)
}

#[test]
fn a_legitimate_cold_job_beyond_five_seconds_completes_inside_the_server_budget() {
    let (harness, clock) = slow_harness(
        WallpapersConfig::default(),
        single_display(),
        Duration::from_millis(3_500),
        Duration::from_millis(2_500),
    );
    let file = harness.png("external.png", [20, 21, 22]);
    let started = clock.now();
    let outcome = harness.service.set_with_budget(
        &harness.params(
            WallpaperSelection::File {
                path: file.display().to_string(),
            },
            WallpaperScreen::Main,
        ),
        WALLPAPERS_SERVER_BUDGET,
    );
    assert!(outcome.is_full_success(), "{outcome:?}");
    assert!(
        clock.now().duration_since(started) > Duration::from_secs(5),
        "the fake job took longer than the old five-second client default"
    );
}

#[test]
fn a_deadline_that_expires_during_snapshot_issues_no_native_change() {
    let (harness, _clock) = slow_harness(
        WallpapersConfig::default(),
        single_display(),
        Duration::from_secs(120),
        Duration::ZERO,
    );
    let file = harness.png("external.png", [23, 24, 25]);
    let outcome = harness.service.set_with_budget(
        &harness.params(
            WallpaperSelection::File {
                path: file.display().to_string(),
            },
            WallpaperScreen::Main,
        ),
        WALLPAPERS_SERVER_BUDGET,
    );
    assert_eq!(outcome.status, WallpapersOutcomeStatus::Cancelled);
    assert_eq!(outcome.cause, Some(WallpapersOutcomeCause::DeadlineExpired));
    assert!(harness.desktop.calls().is_empty());
}

#[test]
fn an_unknown_native_result_is_reported_as_uncertain_not_retried() {
    let harness = Harness::new(
        WallpapersConfig {
            list: vec!["a.png".to_owned()],
            ..WallpapersConfig::default()
        },
        single_display(),
    );
    harness.png("config/a.png", [30, 31, 32]);
    harness.desktop.set_outcome(
        DisplayId::new(1),
        Err(DesktopError::Unknown { operation: "desktop image set" }),
    );
    let outcome = harness.service.set(&harness.params(
        WallpaperSelection::Collection { index: 0 },
        WallpaperScreen::Main,
    ));
    assert_eq!(outcome.status, WallpapersOutcomeStatus::Uncertain);
    assert!(outcome.applied.is_empty());
    assert_eq!(outcome.unknown, vec![1]);
    assert_eq!(harness.desktop.calls().len(), 1, "no automatic retry");
}

#[test]
fn an_explicit_file_resets_the_next_sequential_choice_to_zero() {
    let harness = Harness::new(enabled_with_list(&["a.png", "b.png"], 0), single_display());
    harness.png("config/a.png", [255, 0, 0]);
    harness.png("config/b.png", [0, 255, 0]);
    let external = harness.png("external.png", [1, 1, 1]);
    harness.start();
    wait_until(|| harness.desktop.calls().len() == 1);
    wait_idle(&harness);

    let outcome = harness.service.set(&harness.params(
        WallpaperSelection::File {
            path: external.display().to_string(),
        },
        WallpaperScreen::Main,
    ));
    assert!(outcome.is_full_success());
    harness.registry.pause(harness.handle).expect("pause");
    harness.registry.resume(harness.handle).expect("resume");
    wait_until(|| harness.desktop.calls().len() == 3);
    let calls = harness.desktop.calls();
    assert_eq!(
        read_rgb8(&calls[2].image).get_pixel(0, 0).0,
        [255, 0, 0],
        "sequential starts at zero after an external file"
    );
}

#[test]
fn a_coalesced_a_b_a_publication_keeps_the_same_config_and_deadline() {
    let harness = Harness::new(enabled_with_list(&["a.png"], 60), single_display());
    harness.png("config/a.png", [4, 4, 4]);
    harness.start();
    wait_until(|| harness.desktop.calls().len() == 1);
    wait_idle(&harness);
    let session = harness.session();
    let deadline = session.deadline().expect("deadline");

    harness.reload(WallpapersConfig {
        radius: 3,
        ..enabled_with_list(&["a.png"], 60)
    });
    wait_until(|| session.revision() == 2);
    assert_eq!(session.deadline(), Some(deadline));

    harness.reload(enabled_with_list(&["a.png"], 60));
    wait_until(|| session.revision() == 3);
    assert_eq!(
        session.deadline(),
        Some(deadline),
        "A-B-A with the same interval keeps the original deadline"
    );
    assert_eq!(harness.desktop.calls().len(), 1);
}

#[test]
fn a_partial_rotation_tick_does_not_count_as_a_full_success_reset() {
    let harness = Harness::new(enabled_with_list(&["a.png"], 60), dual_display());
    harness.png("config/a.png", [50, 51, 52]);
    harness.desktop.set_outcome(
        DisplayId::new(2),
        Err(DesktopError::Native {
            display: DisplayId::new(2),
            message: "denied".to_owned(),
        }),
    );
    harness.start();
    wait_until(|| harness.desktop.calls().len() >= 2);
    wait_idle(&harness);
    let session = harness.status().session.expect("session");
    assert_eq!(session.full_success_resets, 0);
    assert!(session.timer_spawned);
    assert!(session.deadline_present);
}

#[test]
fn a_disabled_reload_updates_the_accepted_pool_used_by_explicit_requests() {
    let harness = Harness::new(
        WallpapersConfig {
            list: vec!["a.png".to_owned()],
            ..WallpapersConfig::default()
        },
        single_display(),
    );
    harness.png("config/a.png", [255, 0, 0]);
    harness.png("config/b.png", [0, 255, 0]);
    harness.reload(WallpapersConfig {
        list: vec!["b.png".to_owned()],
        ..WallpapersConfig::default()
    });
    let outcome = harness.service.set(&harness.params(
        WallpaperSelection::Collection { index: 0 },
        WallpaperScreen::Main,
    ));
    assert!(outcome.is_full_success(), "{outcome:?}");
    assert_eq!(
        read_rgb8(&harness.desktop.calls()[0].image).get_pixel(0, 0).0,
        [0, 255, 0],
        "the disabled module used the newly accepted pool without a session"
    );
    assert_eq!(harness.status().session, None);
}

#[test]
fn a_paused_reload_updates_the_accepted_pool_used_by_explicit_requests() {
    let harness = Harness::new(enabled_with_list(&["a.png"], 60), single_display());
    harness.png("config/a.png", [255, 0, 0]);
    harness.png("config/b.png", [0, 255, 0]);
    harness.start();
    wait_until(|| harness.desktop.calls().len() == 1);
    harness.registry.pause(harness.handle).expect("pause");
    harness.reload(WallpapersConfig {
        list: vec!["b.png".to_owned()],
        ..enabled_with_list(&["a.png"], 60)
    });
    let outcome = harness.service.set(&harness.params(
        WallpaperSelection::Collection { index: 0 },
        WallpaperScreen::Main,
    ));
    assert!(outcome.is_full_success(), "{outcome:?}");
    let calls = harness.desktop.calls();
    assert_eq!(read_rgb8(&calls[calls.len() - 1].image).get_pixel(0, 0).0, [
        0, 255, 0
    ]);
    assert_eq!(harness.status().session, None);
}

#[test]
fn a_malformed_initial_file_reports_no_accepted_config_until_a_later_fix() {
    use stache_sdk::ConfigDiagnostic;

    let env = TestEnv::new();
    let config_dir = env.path("config");
    fs::create_dir_all(&config_dir).expect("config directory");
    let desktop = FakeDesktop::with_snapshot(single_display());
    let service = WallpapersService::new(ServiceConfig {
        paths: env.paths().clone(),
        source: SourceContext::new(config_dir.clone(), Some(env.root().to_path_buf())),
        backend: Arc::new(desktop.clone()),
        clock: ManualClock::new(),
        random: ScriptedRandom::new(Vec::new()),
        processor: Arc::new(RealProcessor),
    })
    .expect("service");
    let registry = Arc::new(Registry::new(
        Arc::new(CaptureLogger::default()),
        Arc::new(EventBus::new()),
        Arc::new(InlineMain),
        Arc::new(AllowAll),
        Arc::new(UnavailableWindows),
    ));
    let handle = registry
        .register_rejected(
            service.module(),
            ConfigDiagnostic::new("wallpapers.toml did not decode"),
        )
        .expect("register rejected");
    let watch = registry.accepted_watch(handle).expect("watch");
    service.install_accepted_watch(watch);

    let params = WallpapersSetParams {
        config_directory: config_dir.display().to_string(),
        selection: WallpaperSelection::Collection { index: 0 },
        screen: WallpaperScreen::Main,
    };
    let before = service.set(&params);
    assert!(matches!(
        before.cause,
        Some(WallpapersOutcomeCause::NoAcceptedConfig)
    ));
    assert!(desktop.calls().is_empty());

    env.png("config/a.png", solid(8, 8, [9, 8, 7]));
    registry
        .reload(handle, WallpapersConfig {
            list: vec!["a.png".to_owned()],
            ..WallpapersConfig::default()
        })
        .expect("fixed reload");
    let after = service.set(&params);
    assert!(after.is_full_success(), "{after:?}");
    assert_eq!(desktop.calls().len(), 1);

    service.begin_shutdown();
    let _ = registry.shutdown();
    assert!(service.finish_shutdown(&TestWait::new()));
}

// ---------------------------------------------------------------------------
// Native serialization, startup, processing and check-to-write barriers
// ---------------------------------------------------------------------------

impl TestWait {
    fn with_budget(budget: Duration) -> Self {
        Self {
            deadline: Instant::now() + budget,
        }
    }
}

/// A processor that can pause one call at a barrier and then run the real
/// pipeline. It proves orchestration against actual decode/resize/cache work
/// rather than only a backend double.
struct BlockingProcessor {
    armed: std::sync::atomic::AtomicBool,
    entered: Mutex<Option<Sender<()>>>,
    release: Mutex<Option<Receiver<()>>>,
    clock: Option<Arc<ManualClock>>,
    advance: Duration,
}

impl BlockingProcessor {
    fn gated() -> (Arc<Self>, Receiver<()>, Sender<()>) {
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        (
            Arc::new(Self {
                armed: std::sync::atomic::AtomicBool::new(false),
                entered: Mutex::new(Some(entered)),
                release: Mutex::new(Some(release_rx)),
                clock: None,
                advance: Duration::ZERO,
            }),
            entered_rx,
            release,
        )
    }

    fn armed() -> (Arc<Self>, Receiver<()>, Sender<()>) {
        let (processor, entered, release) = Self::gated();
        processor.arm();
        (processor, entered, release)
    }

    fn advancing(clock: Arc<ManualClock>, advance: Duration) -> Arc<Self> {
        Arc::new(Self {
            armed: std::sync::atomic::AtomicBool::new(false),
            entered: Mutex::new(None),
            release: Mutex::new(None),
            clock: Some(clock),
            advance,
        })
    }

    fn arm(&self) { self.armed.store(true, std::sync::atomic::Ordering::SeqCst); }
}

impl Processor for BlockingProcessor {
    fn prepare(
        &self,
        plan: &[PlannedTarget],
        paths: &AppPaths,
        cancel: &CancellationToken,
    ) -> Result<Prepared, PrepareError> {
        if self.armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
            let entered = self.entered.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(entered) = entered {
                let _ = entered.send(());
            }
            let release = self.release.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(release) = release {
                let _ = release.recv();
            }
        }
        if let Some(clock) = &self.clock {
            clock.advance(self.advance);
        }
        RealProcessor.prepare(plan, paths, cancel)
    }
}

/// A guarded backend that returns `Unknown` for its first set while a late
/// native closure holds the tracked operation until the test releases it.
struct LateDesktop {
    inner: FakeDesktop,
    late: std::sync::atomic::AtomicBool,
    entered: Mutex<Option<Sender<()>>>,
    release: Mutex<Option<Receiver<()>>>,
    completed: Mutex<Option<Sender<()>>>,
    held: Mutex<Option<thread::JoinHandle<()>>>,
}

impl LateDesktop {
    fn new(inner: FakeDesktop) -> (Arc<Self>, Receiver<()>, Sender<()>, Receiver<()>) {
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let (completed, completed_rx) = mpsc::channel();
        (
            Arc::new(Self {
                inner,
                late: std::sync::atomic::AtomicBool::new(true),
                entered: Mutex::new(Some(entered)),
                release: Mutex::new(Some(release_rx)),
                completed: Mutex::new(Some(completed)),
                held: Mutex::new(None),
            }),
            entered_rx,
            release,
            completed_rx,
        )
    }

    fn join(&self) {
        let handle = self.held.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(handle) = handle {
            handle.join().expect("late native closure");
        }
    }
}

impl DesktopBackend for LateDesktop {
    fn snapshot(&self) -> Result<DisplaySnapshot, DesktopError> { self.inner.snapshot() }

    fn set_image(
        &self,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &Path,
    ) -> Result<(), DesktopError> {
        self.inner.set_image(display, expected, image)
    }
}

impl GuardedDesktopBackend for LateDesktop {
    fn snapshot_guarded(
        &self,
        operation: &DesktopOperation,
    ) -> Result<DisplaySnapshot, DesktopError> {
        self.inner.snapshot_guarded(operation)
    }

    fn set_image_guarded(
        &self,
        operation: &DesktopOperation,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &Path,
    ) -> Result<(), DesktopError> {
        operation.check("desktop image set")?;
        let account = operation.track().expect("the service attaches a tracker");
        if self.late.swap(false, std::sync::atomic::Ordering::SeqCst) {
            let inner = self.inner.clone();
            let expected = *expected;
            let image = image.to_path_buf();
            let entered = self.entered.lock().unwrap_or_else(PoisonError::into_inner).take();
            let release = self.release.lock().unwrap_or_else(PoisonError::into_inner).take();
            let completed = self.completed.lock().unwrap_or_else(PoisonError::into_inner).take();
            let handle = thread::spawn(move || {
                if let Some(entered) = entered {
                    let _ = entered.send(());
                }
                if let Some(release) = release {
                    let _ = release.recv();
                }
                let _ = inner.set_image(display, &expected, &image);
                account.finish();
                if let Some(completed) = completed {
                    let _ = completed.send(());
                }
            });
            *self.held.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle);
            return Err(DesktopError::Unknown { operation: "desktop image set" });
        }
        let result = self.inner.set_image_guarded(operation, display, expected, image);
        account.finish();
        result
    }
}

fn late_harness(
    config: WallpapersConfig,
    displays: DisplaySnapshot,
) -> (Harness, Arc<LateDesktop>, Receiver<()>, Sender<()>, Receiver<()>) {
    let env = TestEnv::new();
    let config_dir = env.path("config");
    fs::create_dir_all(&config_dir).expect("config directory");
    let desktop = FakeDesktop::with_snapshot(displays);
    let (late, entered, release, completed) = LateDesktop::new(desktop.clone());
    let backend: Arc<dyn GuardedDesktopBackend> = late.clone();
    let harness = Harness::build(config, env, config_dir, desktop, backend);
    (harness, late, entered, release, completed)
}

#[test]
fn a_late_issued_native_call_serializes_the_successor_native_issuance() {
    let (harness, late, entered, release, completed) =
        late_harness(WallpapersConfig::default(), single_display());
    let file = harness.png("external.png", [40, 41, 42]);
    let params = harness.params(
        WallpaperSelection::File {
            path: file.display().to_string(),
        },
        WallpaperScreen::Main,
    );
    let service = Arc::clone(&harness.service);
    let first_params = params.clone();
    let first = thread::spawn(move || service.set(&first_params));
    entered.recv_timeout(Duration::from_secs(5)).expect("late native started");

    let outcome = first.join().expect("first request returned");
    assert_eq!(outcome.status, WallpapersOutcomeStatus::Uncertain, "{outcome:?}");
    assert_eq!(outcome.unknown, vec![1]);
    assert!(
        harness.desktop.calls().is_empty(),
        "the late call did not return yet"
    );
    assert_eq!(harness.service.native_tracker().unresolved(), 1);
    assert_eq!(harness.service.native_tracker().owned(), 1);

    let service = Arc::clone(&harness.service);
    let (second_tx, second_rx) = mpsc::channel();
    let second = thread::spawn(move || {
        let _ = second_tx.send(service.set(&params));
    });
    wait_until(|| harness.status().executing);
    for _ in 0..20 {
        thread::yield_now();
    }
    assert!(
        harness.desktop.calls().is_empty(),
        "a successor must not issue while the predecessor is unresolved"
    );
    assert!(
        second_rx.try_recv().is_err(),
        "the successor must still be waiting"
    );

    release.send(()).expect("release the late native call");
    completed.recv_timeout(Duration::from_secs(5)).expect("late native finished");
    late.join();
    let second_outcome =
        second_rx.recv_timeout(Duration::from_secs(5)).expect("successor completed");
    second.join().expect("successor thread");
    assert!(second_outcome.is_full_success(), "{second_outcome:?}");
    assert_eq!(harness.desktop.calls().len(), 2);
    assert_eq!(harness.service.native_tracker().unresolved(), 0);
    assert!(harness.service.native_tracker().is_drained());
}

#[test]
fn shutdown_reports_an_unresolved_late_native_call_and_drains_after_it_finishes() {
    let (harness, late, entered, release, completed) =
        late_harness(WallpapersConfig::default(), single_display());
    let file = harness.png("external.png", [43, 44, 45]);
    let params = harness.params(
        WallpaperSelection::File {
            path: file.display().to_string(),
        },
        WallpaperScreen::Main,
    );
    let service = Arc::clone(&harness.service);
    let first = thread::spawn(move || service.set(&params));
    entered.recv_timeout(Duration::from_secs(5)).expect("late native started");
    let outcome = first.join().expect("first request returned");
    assert_eq!(outcome.status, WallpapersOutcomeStatus::Uncertain);

    harness.service.begin_shutdown();
    assert!(
        !harness
            .service
            .finish_shutdown(&TestWait::with_budget(Duration::from_millis(100))),
        "an issued native closure must keep teardown unacknowledged"
    );

    release.send(()).expect("release the late native call");
    completed.recv_timeout(Duration::from_secs(5)).expect("late native finished");
    late.join();
    assert!(
        harness.service.finish_shutdown(&TestWait::new()),
        "teardown finishes once the native closure returns"
    );
}

#[test]
fn publishing_between_completion_and_commit_keeps_the_successor_cursor_and_deadline() {
    let harness = Harness::new(enabled_with_list(&["a.png", "b.png"], 60), single_display());
    harness.png("config/a.png", [255, 0, 0]);
    harness.png("config/b.png", [0, 255, 0]);
    harness.start();
    wait_until(|| harness.desktop.calls().len() == 1);
    wait_idle(&harness);
    let session = harness.session();
    let deadline = session.deadline().expect("initial deadline");
    assert_eq!(session.revision(), 1);

    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    harness.service.set_commit_hook(Some(Arc::new(move || {
        let _ = entered_tx.send(());
        let _ = release_rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
    })));

    let service = Arc::clone(&harness.service);
    let params = harness.params(
        WallpaperSelection::Collection { index: 1 },
        WallpaperScreen::Main,
    );
    let call = thread::spawn(move || service.set(&params));
    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the manual job reached the commit gate");

    harness.reload(WallpapersConfig {
        radius: 4,
        ..enabled_with_list(&["a.png", "b.png"], 60)
    });
    wait_until(|| session.revision() == 2);
    assert_eq!(
        session.deadline(),
        Some(deadline),
        "an unchanged interval preserves the deadline while publishing"
    );
    release_tx.send(()).expect("release the commit");
    let outcome = call.join().expect("manual request returned");
    assert!(outcome.is_full_success(), "{outcome:?}");
    assert_eq!(
        session.deadline(),
        Some(deadline),
        "a stale completion must not reset the successor deadline"
    );

    // The initial job left the cursor on b.png. The rejected stale completion
    // selected b.png with successor a.png; if it had committed, the next tick
    // would apply a.png rather than b.png.
    let calls = harness.desktop.calls();
    assert_eq!(calls.len(), 2, "the manual call reached the desktop");
    harness.clock.advance(Duration::from_secs(60));
    session.signal().notify();
    wait_until(|| harness.desktop.calls().len() == 3);
    assert_eq!(
        read_rgb8(&harness.desktop.calls()[2].image).get_pixel(8, 8).0,
        [0, 255, 0],
        "the successor cursor still points at b.png"
    );
}

#[test]
fn a_running_empty_pool_reconfigure_is_rejected_and_preserves_state() {
    let harness = Harness::new(enabled_with_list(&["a.png"], 60), single_display());
    harness.png("config/a.png", [51, 52, 53]);
    harness.start();
    wait_until(|| harness.desktop.calls().len() == 1);
    wait_idle(&harness);
    let session = harness.session();
    let deadline = session.deadline().expect("deadline");

    let error = harness
        .registry
        .reload(harness.handle, enabled_with_list(&[], 120))
        .expect_err("an enabled empty pool is rejected while running");
    match error {
        RegistryError::Module(error) => {
            assert_eq!(error.operation(), Operation::Reconfigure);
            assert_eq!(error.category(), ErrorCategory::Config);
        }
        other => panic!("expected a module reconfigure error, got {other:?}"),
    }
    assert_eq!(session.revision(), 1);
    assert_eq!(session.deadline(), Some(deadline));
    assert_eq!(session.interval(), 60);
    assert_eq!(harness.desktop.calls().len(), 1);
}

#[test]
fn a_zero_to_positive_reconfigure_starts_the_timer_without_applying_an_image() {
    let harness = Harness::new(enabled_with_list(&["a.png"], 0), single_display());
    harness.png("config/a.png", [54, 55, 56]);
    harness.start();
    wait_until(|| harness.desktop.calls().len() == 1);
    wait_idle(&harness);
    let session = harness.session();
    assert!(!session.timer_spawned());
    assert!(session.deadline().is_none());

    harness.reload(enabled_with_list(&["a.png"], 60));
    wait_until(|| session.revision() == 2);
    assert!(
        session.timer_spawned(),
        "the staged positive interval starts a timer"
    );
    assert!(session.deadline().is_some());
    assert_eq!(harness.desktop.calls().len(), 1, "no image is applied on save");
}

#[test]
fn an_accepted_zero_during_a_blocked_rotation_removes_the_timer_without_polling_its_reply() {
    let env = TestEnv::new();
    let config_dir = env.path("config");
    fs::create_dir_all(&config_dir).expect("config directory");
    let desktop = FakeDesktop::with_snapshot(single_display());
    let (processor, entered, release) = BlockingProcessor::gated();
    let harness = Harness::build_with(
        enabled_with_list(&["a.png"], 60),
        env,
        config_dir,
        desktop.clone(),
        Arc::new(desktop),
        ManualClock::new(),
        ScriptedRandom::new(Vec::new()),
        Arc::clone(&processor) as Arc<dyn Processor>,
    );
    harness.png("config/a.png", [57, 58, 59]);
    harness.start();
    wait_until(|| harness.desktop.calls().len() == 1);
    wait_idle(&harness);
    // The initial job already ran; arm the next rotation's processing.
    processor.arm();
    let session = harness.session();
    harness.clock.advance(Duration::from_secs(60));
    session.signal().notify();
    entered
        .recv_timeout(Duration::from_secs(5))
        .expect("rotation processing entered");

    harness.reload(enabled_with_list(&["a.png"], 0));
    wait_until(|| session.revision() == 2);
    wait_until(|| !session.timer_spawned());
    assert!(
        harness.desktop.calls().len() == 1,
        "the blocked job has not applied anything yet"
    );

    release.send(()).expect("release processing");
    wait_idle(&harness);
    assert_eq!(
        harness.desktop.calls().len(),
        1,
        "the obsolete rotation must not issue a native call"
    );
}

#[test]
fn pause_proceeds_while_real_processing_is_blocked() {
    let env = TestEnv::new();
    let config_dir = env.path("config");
    fs::create_dir_all(&config_dir).expect("config directory");
    let desktop = FakeDesktop::with_snapshot(single_display());
    let (processor, entered, release) = BlockingProcessor::armed();
    let harness = Harness::build_with(
        enabled_with_list(&["a.png"], 60),
        env,
        config_dir,
        desktop.clone(),
        Arc::new(desktop),
        ManualClock::new(),
        ScriptedRandom::new(Vec::new()),
        Arc::clone(&processor) as Arc<dyn Processor>,
    );
    harness.png("config/a.png", [60, 61, 62]);
    harness.start();
    entered
        .recv_timeout(Duration::from_secs(5))
        .expect("initial processing entered");
    assert_eq!(
        harness.registry.snapshot(harness.handle).expect("snapshot").status,
        stache_sdk::ModuleStatus::Running
    );

    harness.registry.pause(harness.handle).expect("pause during processing");
    assert_eq!(harness.status().session, None);
    release.send(()).expect("release processing");
    wait_until(|| {
        let status = harness.status();
        !status.executing && !status.queued
    });
    assert!(harness.desktop.calls().is_empty());
}

#[test]
fn shutdown_owns_a_blocked_real_processing_job_until_it_returns() {
    let env = TestEnv::new();
    let config_dir = env.path("config");
    fs::create_dir_all(&config_dir).expect("config directory");
    let desktop = FakeDesktop::with_snapshot(single_display());
    let (processor, entered, release) = BlockingProcessor::armed();
    let harness = Harness::build_with(
        WallpapersConfig::default(),
        env,
        config_dir,
        desktop.clone(),
        Arc::new(desktop),
        ManualClock::new(),
        ScriptedRandom::new(Vec::new()),
        Arc::clone(&processor) as Arc<dyn Processor>,
    );
    let file = harness.png("external.png", [63, 64, 65]);
    let service = Arc::clone(&harness.service);
    let params = harness.params(
        WallpaperSelection::File {
            path: file.display().to_string(),
        },
        WallpaperScreen::Main,
    );
    let call = thread::spawn(move || service.set(&params));
    entered.recv_timeout(Duration::from_secs(5)).expect("processing entered");

    harness.service.begin_shutdown();
    let outcome = call.join().expect("manual request returned");
    assert_eq!(outcome.status, WallpapersOutcomeStatus::Cancelled);
    assert!(
        !harness
            .service
            .finish_shutdown(&TestWait::with_budget(Duration::from_millis(100))),
        "noninterruptible processing stays owned during teardown"
    );

    release.send(()).expect("release processing");
    assert!(
        harness.service.finish_shutdown(&TestWait::new()),
        "teardown finishes once the owned processing returns"
    );
    assert!(harness.desktop.calls().is_empty());
}

#[test]
fn a_deadline_that_expires_during_real_processing_issues_no_native_change() {
    let env = TestEnv::new();
    let config_dir = env.path("config");
    fs::create_dir_all(&config_dir).expect("config directory");
    let desktop = FakeDesktop::with_snapshot(single_display());
    let clock = ManualClock::new();
    let processor = BlockingProcessor::advancing(Arc::clone(&clock), Duration::from_secs(120));
    let harness = Harness::build_with(
        WallpapersConfig::default(),
        env,
        config_dir,
        desktop.clone(),
        Arc::new(desktop),
        Arc::clone(&clock),
        ScriptedRandom::new(Vec::new()),
        processor as Arc<dyn Processor>,
    );
    let file = harness.png("external.png", [66, 67, 68]);
    let outcome = harness.service.set_with_budget(
        &harness.params(
            WallpaperSelection::File {
                path: file.display().to_string(),
            },
            WallpaperScreen::Main,
        ),
        Duration::from_secs(1),
    );
    assert_eq!(outcome.status, WallpapersOutcomeStatus::Cancelled);
    assert_eq!(outcome.cause, Some(WallpapersOutcomeCause::DeadlineExpired));
    assert!(harness.desktop.calls().is_empty());
}

#[test]
fn a_slow_real_processing_job_beyond_five_seconds_completes_inside_the_budget() {
    let env = TestEnv::new();
    let config_dir = env.path("config");
    fs::create_dir_all(&config_dir).expect("config directory");
    let desktop = FakeDesktop::with_snapshot(single_display());
    let clock = ManualClock::new();
    let processor = BlockingProcessor::advancing(Arc::clone(&clock), Duration::from_secs(6));
    let harness = Harness::build_with(
        WallpapersConfig::default(),
        env,
        config_dir,
        desktop.clone(),
        Arc::new(desktop),
        Arc::clone(&clock),
        ScriptedRandom::new(Vec::new()),
        processor as Arc<dyn Processor>,
    );
    let file = harness.png("external.png", [69, 70, 71]);
    let started = clock.now();
    let outcome = harness.service.set_with_budget(
        &harness.params(
            WallpaperSelection::File {
                path: file.display().to_string(),
            },
            WallpaperScreen::Main,
        ),
        WALLPAPERS_SERVER_BUDGET,
    );
    assert!(outcome.is_full_success(), "{outcome:?}");
    assert!(
        clock.now().duration_since(started) > Duration::from_secs(5),
        "the fake processing time exceeded the old five-second default"
    );
    assert_eq!(harness.desktop.calls().len(), 1);
}

#[test]
fn a_timer_spawn_failure_rolls_back_without_a_native_call_and_start_retries() {
    let harness = Harness::new(enabled_with_list(&["a.png"], 60), single_display());
    harness.png("config/a.png", [72, 73, 74]);
    harness.service.fail_next_timer_spawn();
    let error = harness.registry.start_all().expect_err("timer spawn failure");
    match error {
        RegistryError::Module(error) => {
            assert_eq!(error.operation(), Operation::Start);
            assert_eq!(error.category(), ErrorCategory::Platform);
        }
        other => panic!("expected a module start error, got {other:?}"),
    }
    wait_until(|| {
        let status = harness.status();
        !status.executing && !status.queued
    });
    assert_eq!(harness.status().session, None);
    assert!(
        harness.desktop.calls().is_empty(),
        "the latched initial job must not issue after a failed startup"
    );

    harness.registry.start_all().expect("retry start");
    wait_until(|| harness.desktop.calls().len() == 1);
    assert!(harness.status().session.is_some());
}

#[test]
fn start_fails_actionably_when_the_rotation_queue_cannot_take_the_initial_job() {
    let harness = Harness::blocking(
        WallpapersConfig {
            enabled: false,
            list: vec!["a.png".to_owned()],
            interval: 0,
            ..WallpapersConfig::default()
        },
        single_display(),
    );
    let (harness, release, entered) = harness;
    harness.png("config/a.png", [75, 76, 77]);
    let file = harness.png("external.png", [78, 79, 80]);
    harness.start();
    let service = Arc::clone(&harness.service);
    let params = harness.params(
        WallpaperSelection::File {
            path: file.display().to_string(),
        },
        WallpaperScreen::Main,
    );
    let first_params = params.clone();
    let first = thread::spawn(move || service.set(&first_params));
    entered.recv_timeout(Duration::from_secs(5)).expect("manual snapshot blocked");
    let service = Arc::clone(&harness.service);
    let (second_tx, _second_rx) = mpsc::channel();
    let second = thread::spawn(move || {
        let _ = second_tx.send(service.set(&params));
    });
    wait_until(|| harness.status().queued);

    let error = harness
        .registry
        .set_enabled(harness.handle, true)
        .expect_err("the initial job cannot be silently skipped");
    match error {
        RegistryError::Module(error) => assert_eq!(error.operation(), Operation::Start),
        other => panic!("expected a module start error, got {other:?}"),
    }
    assert_eq!(harness.status().session, None);
    assert!(harness.desktop.calls().is_empty());

    release.send(()).expect("release the blocked manual snapshot");
    let _ = first.join().expect("first manual request");
    second.join().expect("second manual request");
    wait_idle(&harness);

    harness.registry.set_enabled(harness.handle, true).expect("retry start");
    wait_until(|| harness.desktop.calls().len() == 1);
    assert!(harness.status().session.is_some());
}

#[test]
fn a_reconfigure_timer_spawn_failure_restores_the_old_staged_state() {
    let harness = Harness::new(enabled_with_list(&["a.png"], 0), single_display());
    harness.png("config/a.png", [81, 82, 83]);
    harness.start();
    wait_until(|| harness.desktop.calls().len() == 1);
    wait_idle(&harness);
    let session = harness.session();
    assert!(!session.timer_spawned());
    assert!(session.deadline().is_none());
    assert!(!session.pending_positive_interval());

    harness.service.fail_next_timer_spawn();
    let error = harness
        .registry
        .reload(harness.handle, enabled_with_list(&["a.png"], 60))
        .expect_err("timer spawn failure rejects the candidate");
    match error {
        RegistryError::Module(error) => assert_eq!(error.operation(), Operation::Reconfigure),
        other => panic!("expected a module reconfigure error, got {other:?}"),
    }
    assert_eq!(session.revision(), 1, "the failed candidate is not published");
    assert!(!session.timer_spawned());
    assert!(!session.pending_positive_interval());
    assert!(session.deadline().is_none());

    harness.reload(enabled_with_list(&["a.png"], 60));
    wait_until(|| session.revision() == 2);
    assert!(session.timer_spawned());
    assert!(session.deadline().is_some());
    assert_eq!(harness.desktop.calls().len(), 1);
}

#[test]
fn disable_proceeds_while_real_processing_is_blocked() {
    let env = TestEnv::new();
    let config_dir = env.path("config");
    fs::create_dir_all(&config_dir).expect("config directory");
    let desktop = FakeDesktop::with_snapshot(single_display());
    let (processor, entered, release) = BlockingProcessor::armed();
    let harness = Harness::build_with(
        enabled_with_list(&["a.png"], 60),
        env,
        config_dir,
        desktop.clone(),
        Arc::new(desktop),
        ManualClock::new(),
        ScriptedRandom::new(Vec::new()),
        Arc::clone(&processor) as Arc<dyn Processor>,
    );
    harness.png("config/a.png", [84, 85, 86]);
    harness.start();
    entered
        .recv_timeout(Duration::from_secs(5))
        .expect("initial processing entered");

    harness
        .registry
        .set_enabled(harness.handle, false)
        .expect("disable during processing");
    assert_eq!(harness.status().session, None);
    release.send(()).expect("release processing");
    wait_until(|| {
        let status = harness.status();
        !status.executing && !status.queued
    });
    assert!(harness.desktop.calls().is_empty());
}

#[test]
fn a_manual_started_disabled_cannot_commit_into_a_later_session() {
    let harness = Harness::new(
        WallpapersConfig {
            enabled: false,
            list: vec!["a.png".to_owned(), "b.png".to_owned(), "c.png".to_owned()],
            interval: 60,
            ..WallpapersConfig::default()
        },
        single_display(),
    );
    harness.png("config/a.png", [255, 0, 0]);
    harness.png("config/b.png", [0, 255, 0]);
    harness.png("config/c.png", [0, 0, 255]);
    harness.start();
    assert_eq!(harness.status().session, None);

    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    harness.service.set_commit_hook(Some(Arc::new(move || {
        let _ = entered_tx.send(());
        let _ = release_rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
    })));
    let service = Arc::clone(&harness.service);
    let params = harness.params(
        WallpaperSelection::Collection { index: 1 },
        WallpaperScreen::Main,
    );
    let call = thread::spawn(move || service.set(&params));
    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the manual job reached the commit gate");

    harness
        .registry
        .set_enabled(harness.handle, true)
        .expect("start a later session");
    wait_until(|| harness.status().session.is_some());
    release_tx.send(()).expect("release the manual commit");
    let outcome = call.join().expect("manual request returned");
    assert!(outcome.is_full_success(), "{outcome:?}");

    wait_until(|| harness.desktop.calls().len() == 2);
    wait_idle(&harness);
    assert_eq!(
        read_rgb8(&harness.desktop.calls()[1].image).get_pixel(8, 8).0,
        [255, 0, 0],
        "the new session starts its sequential cursor at zero, not at the stale manual successor"
    );
    assert_eq!(
        harness.session().full_success_resets(),
        1,
        "only the new session's initial job may reset the interval"
    );
}
