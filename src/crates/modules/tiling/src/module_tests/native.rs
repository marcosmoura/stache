//! TB-07-01/02/03: the lifecycle of the native borders through the real
//! registry, the real actor and the fakes. No window, overlay, display link,
//! observer or `SkyLight` connection exists; "main" is a [`ManualExecutor`] whose
//! queue the test runs (or a pump thread the test owns) and the two owners are
//! scriptable fakes.
//!
//! * TB-07-01 (start, reload, disabled): `disabled_borders_own_no_overlay_owner_or_hop`,
//!   `enabling_starts_the_owners_and_disabling_stops_them_before_the_release`,
//!   `appearance_and_ignore_reloads_keep_the_owners_and_release_nothing`.
//! * TB-07-02 (stop, ledger, retry): `a_normal_quit_with_borders_is_clean`,
//!   `a_quit_while_main_cannot_acknowledge_is_reported_not_owed`,
//!   `the_same_stall_during_a_runtime_stop_is_owed_quarantined_and_retried_once`,
//!   `a_mismatched_release_count_is_owed_even_at_exit_and_retried`,
//!   `a_stuck_live_owner_is_owed_then_retried`, `a_stuck_spaces_owner_is_owed_then_retried`,
//!   `pause_stops_the_owners_and_releases_like_a_stop`.
//! * TB-07-03 (health, panic): `an_engine_panic_turns_borders_off_and_keeps_tiling_running`,
//!   plus the engine and lifecycle tests for the busy main thread.

use std::sync::atomic::AtomicBool;

use stache_macos::ManualExecutor;
use stache_macos::border_overlay::testing::{FakeOverlayBackend, FakeSpaceEvents};
use stache_macos::border_overlay::{
    AppearanceChange, ApplyReport, BorderAppearance, BorderColor, BorderPaint, BorderPlacement,
    FakeLiveFollow, OverlayBackend, OverlayBatch, OverlayCapabilities, OverlayControl,
    OverlayHandle, OverlayKind, OverlayRect, OverlayWork, SpaceEvents, SpaceSink, StopOutcome,
};

use super::*;
use crate::actor::ENGINE_FAULT;

/// What the backend saw when `release_all` ran on "main".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReleaseProbe {
    spaces_stops: u32,
    live_stops: usize,
}

struct ProbeBackend {
    inner: FakeOverlayBackend,
    spaces: FakeSpaceEvents,
    live: FakeLiveFollow,
    releases: Mutex<Vec<ReleaseProbe>>,
}

impl OverlayBackend for ProbeBackend {
    fn apply(&self, work: &OverlayWork) -> ApplyReport { self.inner.apply(work) }

    fn release_all(&self) -> usize {
        lock(&self.releases).push(ReleaseProbe {
            spaces_stops: self.spaces.stops(),
            live_stops: self.live.stopped(),
        });
        self.inner.release_all()
    }
}

/// The fakes one borders host composes. Clones share state.
#[derive(Clone)]
pub(super) struct Fakes {
    exec: Arc<ManualExecutor>,
    control: Arc<OverlayControl>,
    backend: Arc<ProbeBackend>,
    spaces: FakeSpaceEvents,
    live: FakeLiveFollow,
    /// While set the Spaces source panics when started (the engine fault).
    panic_on_start: Arc<AtomicBool>,
}

impl Fakes {
    fn new() -> Self {
        let exec = Arc::new(ManualExecutor::new(false));
        let spaces = FakeSpaceEvents::new();
        let live = FakeLiveFollow::active();
        let backend = Arc::new(ProbeBackend {
            inner: FakeOverlayBackend::single_display(),
            spaces: spaces.clone(),
            live: live.clone(),
            releases: Mutex::new(Vec::new()),
        });
        let control = Arc::new(OverlayControl::new(
            Arc::clone(&exec) as Arc<dyn stache_sdk::MainThreadExecutor>,
            Arc::clone(&backend) as Arc<dyn OverlayBackend>,
            OverlayCapabilities::default(),
        ));
        Self {
            exec,
            control,
            backend,
            spaces,
            live,
            panic_on_start: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(super) fn install(&self, parts: &mut TilingParts) {
        parts.overlay = Some(Arc::clone(&self.control));
        let spaces = self.spaces.clone();
        let panic_on_start = Arc::clone(&self.panic_on_start);
        parts.space_events = Some(Arc::new(move || {
            Box::new(Scripted {
                inner: spaces.clone(),
                panic_on_start: Arc::clone(&panic_on_start),
            }) as Box<dyn SpaceEvents>
        }));
        let live = self.live.clone();
        parts.live_follow = Some(Arc::new(move || {
            Box::new(live.clone()) as Box<dyn stache_macos::border_overlay::LiveFollow>
        }));
    }

    fn releases(&self) -> Vec<ReleaseProbe> { lock(&self.backend.releases).clone() }

    /// Runs what "main" has queued.
    fn main_runs(&self) -> usize { self.exec.drain() }
}

/// The Spaces fake, optionally panicking at `start`.
struct Scripted {
    inner: FakeSpaceEvents,
    panic_on_start: Arc<AtomicBool>,
}

impl SpaceEvents for Scripted {
    fn start(&mut self, sink: SpaceSink) {
        assert!(
            !self.panic_on_start.load(Ordering::SeqCst),
            "scripted Spaces source failure"
        );
        self.inner.start(sink);
    }

    fn resync(&mut self) { self.inner.resync(); }

    fn stop(&mut self) -> StopOutcome { self.inner.stop() }
}

/// A thread the test owns that behaves like a healthy main thread: it runs the
/// executor's queue until dropped.
struct Pump {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Pump {
    fn start(fakes: &Fakes) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let exec = Arc::clone(&fakes.exec);
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("test-main-pump".to_owned())
            .spawn(move || {
                while !flag.load(Ordering::SeqCst) {
                    exec.drain();
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
            .expect("pump thread");
        Self { stop, thread: Some(thread) }
    }
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn host(fakes: &Fakes) -> Host {
    let mut setup = Setup {
        native: Some(fakes.clone()),
        ..Setup::default()
    };
    setup.timing.borders_release_wait = Duration::from_millis(60);
    Host::with(setup)
}

fn with_borders() -> TilingConfig {
    let mut config = enabled();
    config.borders.enabled = true;
    config
}

fn started_with_borders(fakes: &Fakes) -> Host {
    let host = host(fakes);
    host.reload(with_borders()).expect("config");
    host.registry.start_all().expect("start");
    wait_until("the owners", || {
        fakes.spaces.starts() == 1 && fakes.live.started() == 1
    });
    host
}

fn orphan(fakes: &Fakes, raw: u64) {
    let appearance = BorderAppearance {
        width: 4.0,
        hidpi: true,
        paint: BorderPaint::Solid(BorderColor { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }),
        corner_radius: None,
        motion: None,
        placement: BorderPlacement::Below,
    };
    let _ = fakes.control.apply(OverlayBatch::new().with(
        OverlayHandle::from_raw(raw),
        OverlayKind::Show {
            target_cg_id: 10,
            frame: OverlayRect::new(0.0, 0.0, 50.0, 50.0),
            appearance,
            change: AppearanceChange::Reload,
        },
    ));
    fakes.main_runs();
}

fn diagnostics(host: &Host) -> Vec<String> { host.runtime.status().read().diagnostics }

// ------------------------------------------------------------------ TB-07-01

#[test]
fn disabled_borders_own_no_overlay_owner_or_hop() {
    let fakes = Fakes::new();
    let host = host(&fakes);
    host.registry.start_all().expect("start");
    feed_window(&host);
    assert_eq!(
        (fakes.spaces.starts(), fakes.live.started(), fakes.exec.queued()),
        (0, 0, 0),
        "no owner, no main-thread job"
    );
    assert_eq!(fakes.control.counters().batches, 0);
    assert!(diagnostics(&host).iter().all(|d| !d.contains("borders")));
    host.disable();
    assert_eq!(host.state(), TilingRuntimeState::Inactive);
    assert_eq!(
        fakes.exec.queued(),
        0,
        "stopping disabled borders never hops to main"
    );
    assert!(fakes.releases().is_empty());
    assert_eq!(
        fakes.spaces.stops() + u32::try_from(fakes.live.stopped()).unwrap_or(9),
        0
    );
    assert!(host.runtime.status().read().detail.is_none());
}

#[test]
fn enabling_starts_the_owners_and_disabling_stops_them_before_the_release() {
    let fakes = Fakes::new();
    let host = started_with_borders(&fakes);
    assert_eq!(
        fakes.exec.queued(),
        0,
        "enabling queues nothing until a border shows"
    );

    host.reload(enabled()).expect("disable borders");
    wait_until("the owners stopped", || {
        fakes.spaces.stops() == 1 && fakes.live.stopped() == 1
    });
    wait_until("the release was queued", || fakes.exec.queued() == 1);
    assert_eq!(fakes.main_runs(), 1);
    assert_eq!(
        fakes.releases(),
        vec![ReleaseProbe { spaces_stops: 1, live_stops: 1 }],
        "both owners had stopped when main released the overlays"
    );
    // Tiling keeps running; the later stop has nothing left to release.
    assert_eq!(host.status(), ModuleStatus::Running);
    host.disable();
    assert_eq!(host.state(), TilingRuntimeState::Inactive);
    assert_eq!(
        fakes.releases().len(),
        1,
        "the stop did not release a second time"
    );
    assert!(host.runtime.status().read().detail.is_none());
}

#[test]
fn appearance_and_ignore_reloads_keep_the_owners_and_release_nothing() {
    let fakes = Fakes::new();
    let host = started_with_borders(&fakes);
    let mut config = with_borders();
    config.borders.hidpi = Some(false);
    host.reload(config.clone()).expect("appearance");
    config.borders.ignore = vec![crate::config::WindowRule {
        app_id: Some("com.example.ignored".to_owned()),
        title: None,
        app_name: None,
    }];
    host.reload(config).expect("ignore");
    // The actor applied both revisions before the next assertions.
    wait_until("both revisions applied", || {
        host.revision() >= 3 && host.runtime_revision() != "0"
    });
    std::thread::sleep(Duration::from_millis(40));
    assert_eq!(
        (fakes.spaces.starts(), fakes.live.started()),
        (1, 1),
        "no restart"
    );
    assert_eq!((fakes.spaces.stops(), fakes.live.stopped()), (0, 0));
    assert_eq!(fakes.exec.queued(), 0, "nothing released");
    host.disable();
    assert!(fakes.main_runs() <= 1);
}

// ------------------------------------------------------------------ TB-07-02

/// The quit path: the host announces the shutdown, then stops the module off the
/// main thread while main keeps running its queue.
#[test]
fn a_normal_quit_with_borders_is_clean() {
    let fakes = Fakes::new();
    let host = started_with_borders(&fakes);
    let _main = Pump::start(&fakes);
    host.runtime.begin_shutdown();
    // The host reads the module stop's result (`module:tiling` is failed when it
    // is an error) and then drains the factory (`factory:tiling` is
    // unacknowledged when it answers `false`): both must be clean.
    assert!(
        host.registry.set_enabled(host.handle, false).is_ok(),
        "the stop is Ok"
    );
    assert!(host.runtime.finish_shutdown(), "the factory drain answers");
    assert_eq!(host.state(), TilingRuntimeState::Inactive);
    assert!(host.trace().contains(&Teardown::Borders));
    assert_eq!(fakes.releases().len(), 1);
    assert_eq!(fakes.releases()[0], ReleaseProbe {
        spaces_stops: 1,
        live_stops: 1
    });
    let status = host.runtime.status().read();
    assert!(
        status.detail.is_none(),
        "a clean quit has nothing to report: {status:?}"
    );
}

/// Native run 2: borders were disabled by a reload (the owners stopped, the
/// ledger retired and the release acknowledged on an actor turn), then the
/// process quit. The stop settles with nothing owed, the module stop is `Ok` and
/// the factory drain answers, so the host records neither `module:tiling` as
/// failed nor `factory:tiling` as unacknowledged. (The run's remaining failure
/// was the restore of an application Tiling had hidden, see the part 07 notes.)
#[test]
fn a_quit_after_a_reload_disabled_the_borders_is_clean() {
    let fakes = Fakes::new();
    let host = started_with_borders(&fakes);
    let _main = Pump::start(&fakes);
    host.reload(enabled()).expect("disable borders");
    wait_until("the owners stopped and the release ran", || {
        fakes.spaces.stops() == 1 && fakes.live.stopped() == 1 && fakes.releases().len() == 1
    });
    // Let the actor read the acknowledgement of the reload-time release.
    std::thread::sleep(Duration::from_millis(100));
    host.runtime.begin_shutdown();
    assert!(
        host.registry.set_enabled(host.handle, false).is_ok(),
        "the stop is Ok"
    );
    assert!(host.runtime.finish_shutdown(), "the factory drain answers");
    assert_eq!(host.state(), TilingRuntimeState::Inactive);
    assert!(host.trace().contains(&Teardown::Borders));
    assert_eq!(fakes.releases().len(), 1, "no second release at the stop");
    let status = host.runtime.status().read();
    assert!(status.detail.is_none(), "nothing to report: {status:?}");
}

/// The defect of native run 1: a main thread that does not run the release job
/// within the bound must not turn the quit `partial`. At process exit the window
/// server removes the overlays, so the stage is a note, not an owed stage.
#[test]
fn a_quit_while_main_cannot_acknowledge_is_reported_not_owed() {
    let fakes = Fakes::new();
    let host = started_with_borders(&fakes);
    host.runtime.begin_shutdown();
    let started = Instant::now();
    host.disable();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(
        host.state(),
        TilingRuntimeState::Inactive,
        "{:?}",
        host.runtime.status().read()
    );
    assert!(host.trace().contains(&Teardown::Borders));
    let detail = host.runtime.status().read().detail.expect("reported");
    assert!(
        detail.contains("left to the window server at process exit")
            && detail.contains("did not acknowledge"),
        "{detail}"
    );
    assert_eq!(fakes.releases().len(), 0, "main never ran the job");
    assert_eq!(
        fakes.exec.queued(),
        1,
        "the job stays queued; it is not detached"
    );
}

#[test]
fn the_same_stall_during_a_runtime_stop_is_owed_quarantined_and_retried_once() {
    let fakes = Fakes::new();
    let host = started_with_borders(&fakes);
    host.disable();
    let status = host.runtime.status().read();
    assert_eq!(status.state, TilingRuntimeState::Quarantined, "{status:?}");
    assert!(
        status.owed.contains(&"borders overlays".to_owned()),
        "{status:?}"
    );
    assert!(!host.trace().contains(&Teardown::Borders));
    // Start is refused while the stage is owed.
    host.enable();
    assert_eq!(host.state(), TilingRuntimeState::Quarantined);
    // Main recovers; the retry waits for the very same job and releases nothing twice.
    assert_eq!(fakes.main_runs(), 1);
    host.enable();
    assert_eq!(
        host.status(),
        ModuleStatus::Running,
        "{:?}",
        host.runtime.status().read()
    );
    assert_eq!(fakes.releases().len(), 1, "exactly one release job ever ran");
    assert!(host.trace().contains(&Teardown::Borders));
}

#[test]
fn a_mismatched_release_count_is_owed_even_at_exit_and_retried() {
    let fakes = Fakes::new();
    let host = started_with_borders(&fakes);
    // A handle the engine's ledger does not know: the control holds one more.
    orphan(&fakes, 900);
    let _main = Pump::start(&fakes);
    host.runtime.begin_shutdown();
    host.disable();
    let status = host.runtime.status().read();
    assert_eq!(status.state, TilingRuntimeState::Quarantined, "{status:?}");
    assert!(
        status.owed.contains(&"borders overlays".to_owned()),
        "{status:?}"
    );
    assert!(
        format!("{status:?}").contains("acknowledged 1 handle(s) but the engine accounted for 0"),
        "{status:?}"
    );
    drop(status);
    // The retry releases again; the control now holds nothing, so it settles.
    host.enable();
    // The shutdown latch refuses the start; the stage itself was retried and settled.
    assert_eq!(
        host.state(),
        TilingRuntimeState::Inactive,
        "{:?}",
        host.runtime.status().read()
    );
    assert_eq!(fakes.releases().len(), 2, "the retry released once more");
}

#[test]
fn a_stuck_live_owner_is_owed_then_retried() {
    let fakes = Fakes::new();
    let host = started_with_borders(&fakes);
    let _main = Pump::start(&fakes);
    fakes.live.hold_stop(true);
    host.disable();
    let status = host.runtime.status().read();
    assert_eq!(status.state, TilingRuntimeState::Quarantined, "{status:?}");
    assert!(
        format!("{status:?}").contains("live follow owner did not stop"),
        "{status:?}"
    );
    drop(status);
    let attempts = fakes.live.stopped();
    fakes.live.hold_stop(false);
    host.enable();
    assert_eq!(
        host.status(),
        ModuleStatus::Running,
        "{:?}",
        host.runtime.status().read()
    );
    assert!(
        fakes.live.stopped() > attempts,
        "the retry reached exactly the stuck owner"
    );
    assert_eq!(fakes.spaces.stops(), 1, "the Spaces owner was not stopped again");
}

#[test]
fn a_stuck_spaces_owner_is_owed_then_retried() {
    let fakes = Fakes::new();
    let host = started_with_borders(&fakes);
    let _main = Pump::start(&fakes);
    fakes.spaces.hold_stop(true);
    host.runtime.begin_shutdown();
    host.disable();
    // A stuck owner is never settled by process exit.
    let status = host.runtime.status().read();
    assert_eq!(status.state, TilingRuntimeState::Quarantined, "{status:?}");
    assert!(
        format!("{status:?}").contains("spaces owner did not stop"),
        "{status:?}"
    );
    drop(status);
    fakes.spaces.hold_stop(false);
    host.enable();
    // `begin_shutdown` closed the module for starts; the stage itself settled.
    assert!(host.trace().contains(&Teardown::Borders));
    assert_eq!(fakes.live.stopped(), 1, "the live owner was not stopped again");
}

#[test]
fn pause_stops_the_owners_and_releases_like_a_stop() {
    let fakes = Fakes::new();
    let host = started_with_borders(&fakes);
    let _main = Pump::start(&fakes);
    host.registry.pause(host.handle).expect("pause");
    assert_eq!((fakes.spaces.stops(), fakes.live.stopped()), (1, 1));
    assert!(host.trace().contains(&Teardown::Borders));
    host.registry.resume(host.handle).expect("resume");
    wait_until("fresh owners", || {
        fakes.spaces.starts() == 2 && fakes.live.started() == 2
    });
}

// ------------------------------------------------------------------ TB-07-03

#[test]
fn an_engine_panic_turns_borders_off_and_keeps_tiling_running() {
    let fakes = Fakes::new();
    let host = host(&fakes);
    host.registry.start_all().expect("start");
    fakes.panic_on_start.store(true, Ordering::SeqCst);
    host.reload(with_borders()).expect("enable borders");
    wait_until("the fault", || {
        diagnostics(&host).iter().any(|d| d == ENGINE_FAULT)
    });
    let status = host.runtime.status().read();
    assert_eq!(status.state, TilingRuntimeState::Running);
    assert!(
        status.healthy,
        "borders never change Tiling's health: {status:?}"
    );
    drop(status);
    // The overlays are released on the next main-thread turn.
    wait_until("the release was queued", || fakes.exec.queued() == 1);
    assert_eq!(fakes.main_runs(), 1);
    assert_eq!(host.status(), ModuleStatus::Running);
    // Tiling still serves its snapshot and stops cleanly.
    feed_window(&host);
    let _main = Pump::start(&fakes);
    host.disable();
    assert_eq!(
        host.state(),
        TilingRuntimeState::Inactive,
        "{:?}",
        host.runtime.status().read()
    );
}

/// The wording status shows for the reload-time diagnostics: a stuck owner is
/// named and retried by the next stop, a main thread that does not run the
/// release within the ack wait is reported busy, and the note clears once main
/// catches up and the actor looks again.
#[test]
fn reload_diagnostics_reach_the_status_surface() {
    let fakes = Fakes::new();
    let host = started_with_borders(&fakes);
    fakes.live.hold_stop(true);
    host.reload(enabled()).expect("disable borders");
    wait_until("the owner note", || {
        diagnostics(&host).contains(
            &"borders: the live follow owner did not stop; it is retried by the next stop"
                .to_owned(),
        )
    });
    wait_until("the busy note", || {
        diagnostics(&host).contains(&"borders: main thread busy".to_owned())
    });
    assert_eq!(host.status(), ModuleStatus::Running, "borders never fail Tiling");
    assert!(host.runtime.status().read().healthy);
    assert_eq!(fakes.main_runs(), 1);
    wait_until("the busy note clears", || {
        !diagnostics(&host).contains(&"borders: main thread busy".to_owned())
    });
    fakes.live.hold_stop(false);
    let _main = Pump::start(&fakes);
    host.disable();
    assert_eq!(
        host.state(),
        TilingRuntimeState::Inactive,
        "{:?}",
        host.runtime.status().read()
    );
}
