//! Action adapter tests over fakes: the real `ApplicationControl` over the fake
//! application backend, and a scripted settings opener. Nothing launches an
//! application or opens a URL.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use stache_macos::{ApplicationBackend, FakeApplicationBackend, FakeBarrier};
use stache_sdk::{MainThreadExecutor, ThreadError};

use super::{
    ACTIVITY_MONITOR, ActionSource, BarAction, BarActionOutcome, BarActions, BarTarget, CALENDAR,
    EDGE_DEV, FEISHIN, SPOTIFY, TIDAL, WEATHER, media_player,
};
use crate::access::{PaneGuard, PaneOpen, SettingsOpener, SettingsPane};

const BUDGET: Duration = Duration::from_millis(150);
const WAIT: Duration = Duration::from_secs(5);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs every job on its own thread, like a main queue the caller can outwait.
struct Queue;

impl MainThreadExecutor for Queue {
    fn is_main_thread(&self) -> bool { false }

    fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
        thread::spawn(job);
        Ok(())
    }
}

#[derive(Default)]
struct Source {
    runtime: AtomicBool,
    caller: AtomicBool,
    media: Mutex<Option<String>>,
    wakes: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
}

impl Source {
    fn live() -> Arc<Self> {
        let source = Self::default();
        source.runtime.store(true, Ordering::SeqCst);
        source.caller.store(true, Ordering::SeqCst);
        Arc::new(source)
    }

    /// The Bar runtime stopped or restarted: notify like a status change.
    fn stop(&self) {
        self.runtime.store(false, Ordering::SeqCst);
        let wakes: Vec<_> = lock(&self.wakes).clone();
        for wake in wakes {
            wake();
        }
    }
}

impl ActionSource for Source {
    fn runtime_current(&self) -> bool { self.runtime.load(Ordering::SeqCst) }

    fn caller_current(&self) -> bool { self.caller.load(Ordering::SeqCst) }

    fn media_bundle(&self) -> Option<String> { lock(&self.media).clone() }

    fn watch(&self, wake: Arc<dyn Fn() + Send + Sync>) -> Box<dyn Send> {
        lock(&self.wakes).push(wake);
        Box::new(())
    }
}

fn dynamic(source: &Arc<Source>) -> Arc<dyn ActionSource> { Arc::clone(source) as _ }

struct Settings {
    result: Mutex<PaneOpen>,
    opened: Mutex<Vec<SettingsPane>>,
}

impl Settings {
    fn new(result: PaneOpen) -> Arc<Self> {
        Arc::new(Self {
            result: Mutex::new(result),
            opened: Mutex::new(Vec::new()),
        })
    }
}

impl SettingsOpener for Settings {
    fn open_accessibility_settings(&self) -> Result<(), String> { Err("not used".to_owned()) }

    fn open_pane(&self, pane: SettingsPane, current: PaneGuard) -> PaneOpen {
        if !current() {
            return PaneOpen::NotIssued;
        }
        let result = lock(&self.result).clone();
        if result == PaneOpen::Opened {
            lock(&self.opened).push(pane);
        }
        result
    }
}

struct Rig {
    backend: FakeApplicationBackend,
    settings: Arc<Settings>,
    actions: Arc<BarActions>,
}

impl Rig {
    fn new() -> Self {
        let backend = FakeApplicationBackend::new();
        backend.set_application_path(
            ACTIVITY_MONITOR,
            Some(PathBuf::from("/fake/Activity Monitor.app")),
        );
        backend.set_application_path(CALENDAR, Some(PathBuf::from("/fake/Calendar.app")));
        backend.set_application_path(WEATHER, Some(PathBuf::from("/fake/Weather.app")));
        let settings = Settings::new(PaneOpen::Opened);
        let control = stache_macos::ApplicationControl::with_acknowledgement_budget(
            Arc::new(Queue),
            Arc::new(backend.clone()) as Arc<dyn ApplicationBackend>,
            BUDGET,
        );
        let actions = Arc::new(BarActions::with_completion_wait(
            control,
            Arc::clone(&settings) as Arc<dyn SettingsOpener>,
            Duration::from_millis(250),
        ));
        Self { backend, settings, actions }
    }

    fn run(&self, action: BarAction, source: &Arc<Source>) -> BarActionOutcome {
        self.actions.run(action, &dynamic(source))
    }

    fn spawn(
        &self,
        action: BarAction,
        source: &Arc<Source>,
    ) -> thread::JoinHandle<BarActionOutcome> {
        let actions = Arc::clone(&self.actions);
        let source = dynamic(source);
        thread::spawn(move || actions.run(action, &source))
    }
}

fn wait(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(2));
    }
}

fn entered(barrier: &FakeBarrier) {
    assert!(barrier.wait_entered(WAIT), "the fake never parked");
}

#[test]
fn every_action_maps_to_its_fixed_target() {
    assert_eq!(
        BarAction::Cpu.target(),
        Some(BarTarget::Application("com.apple.ActivityMonitor"))
    );
    assert_eq!(
        BarAction::Clock.target(),
        Some(BarTarget::Application("com.apple.iCal"))
    );
    assert_eq!(
        BarAction::Battery.target(),
        Some(BarTarget::Pane(SettingsPane::Battery))
    );
    assert_eq!(
        BarAction::Wifi.target(),
        Some(BarTarget::Pane(SettingsPane::Wifi))
    );
    assert_eq!(
        BarAction::Media.target(),
        None,
        "the media target depends on the published media"
    );
    assert_eq!(
        BarAction::Weather.target(),
        Some(BarTarget::Application("com.apple.weather"))
    );
}

#[test]
fn the_wire_forms_are_exact() {
    for (action, text) in [
        (BarAction::Cpu, "\"cpu\""),
        (BarAction::Clock, "\"clock\""),
        (BarAction::Battery, "\"battery\""),
        (BarAction::Wifi, "\"wifi\""),
        (BarAction::Weather, "\"weather\""),
        (BarAction::Media, "\"media\""),
    ] {
        assert_eq!(serde_json::to_string(&action).expect("serialize"), text);
        assert_eq!(
            serde_json::from_str::<BarAction>(text).expect("round trip"),
            action
        );
    }
    // Anything but the closed enum is refused at the boundary.
    for bad in [
        "\"open\"",
        "{\"media\":\"com.spotify.client\"}",
        "\"com.spotify.client\"",
        "\"/Applications/X.app\"",
        "{\"url\":\"x\"}",
        "1",
    ] {
        assert!(serde_json::from_str::<BarAction>(bad).is_err(), "{bad}");
    }
    for (outcome, json) in [
        (BarActionOutcome::Completed, r#"{"kind":"completed"}"#),
        (BarActionOutcome::Stale, r#"{"kind":"stale"}"#),
        (BarActionOutcome::NotIssued, r#"{"kind":"notIssued"}"#),
        (BarActionOutcome::NotAttempted, r#"{"kind":"notAttempted"}"#),
        (BarActionOutcome::NotInstalled, r#"{"kind":"notInstalled"}"#),
        (
            BarActionOutcome::TimedOutBeforeIssuance,
            r#"{"kind":"timedOutBeforeIssuance"}"#,
        ),
        (
            BarActionOutcome::UncertainAfterIssuance,
            r#"{"kind":"uncertainAfterIssuance"}"#,
        ),
        (BarActionOutcome::Busy, r#"{"kind":"busy"}"#),
        (BarActionOutcome::Unauthorized, r#"{"kind":"unauthorized"}"#),
        (
            BarActionOutcome::Refused { detail: "x".to_owned() },
            r#"{"kind":"refused","detail":"x"}"#,
        ),
        (
            BarActionOutcome::Unavailable {
                reason: crate::bar_bridge::BarUnavailableReason::Paused,
                detail: None,
            },
            r#"{"kind":"unavailable","reason":"paused","detail":null}"#,
        ),
    ] {
        assert_eq!(serde_json::to_string(&outcome).expect("serialize"), json);
        assert_eq!(
            serde_json::from_str::<BarActionOutcome>(json).expect("round trip"),
            outcome
        );
    }
}

#[test]
fn the_application_clicks_open_the_resolved_fixed_paths_with_inline_completion() {
    let rig = Rig::new();
    rig.backend.complete_opens_synchronously();
    let source = Source::live();
    assert_eq!(rig.run(BarAction::Cpu, &source), BarActionOutcome::Completed);
    assert_eq!(rig.run(BarAction::Clock, &source), BarActionOutcome::Completed);
    assert_eq!(rig.run(BarAction::Weather, &source), BarActionOutcome::Completed);
    assert_eq!(rig.backend.open_requests(), [
        PathBuf::from("/fake/Activity Monitor.app"),
        PathBuf::from("/fake/Calendar.app"),
        PathBuf::from("/fake/Weather.app"),
    ]);
    assert_eq!(
        rig.backend.observations(),
        0,
        "the click's observation is removed"
    );
}

#[test]
fn the_settings_clicks_reach_the_opener_with_the_fixed_pane() {
    let rig = Rig::new();
    let source = Source::live();
    assert_eq!(rig.run(BarAction::Battery, &source), BarActionOutcome::Completed);
    assert_eq!(rig.run(BarAction::Wifi, &source), BarActionOutcome::Completed);
    assert_eq!(*lock(&rig.settings.opened), [
        SettingsPane::Battery,
        SettingsPane::Wifi
    ]);
    assert!(
        rig.backend.open_requests().is_empty(),
        "no application was opened"
    );
}

#[test]
fn a_deferred_completion_after_issuance_completes_the_click() {
    let rig = Rig::new();
    let source = Source::live();
    let click = rig.spawn(BarAction::Cpu, &source);
    wait("the issued open", || rig.backend.pending_opens() == 1);
    assert!(rig.backend.complete_open(0, Ok(())));
    assert_eq!(click.join().expect("click"), BarActionOutcome::Completed);
}

#[test]
fn a_native_completion_failure_is_refused() {
    let rig = Rig::new();
    let source = Source::live();
    let click = rig.spawn(BarAction::Clock, &source);
    wait("the issued open", || rig.backend.pending_opens() == 1);
    assert!(rig.backend.complete_open(0, Err("launch failed".to_owned())));
    assert!(matches!(
        click.join().expect("click"),
        BarActionOutcome::Refused { detail } if detail.contains("launch failed")
    ));
}

#[test]
fn a_missing_application_is_not_installed_and_opens_nothing() {
    let rig = Rig::new();
    rig.backend.set_application_path(ACTIVITY_MONITOR, None);
    assert_eq!(
        rig.run(BarAction::Cpu, &Source::live()),
        BarActionOutcome::NotInstalled
    );
    assert!(rig.backend.open_requests().is_empty());
    assert_eq!(rig.backend.observations(), 0);
}

#[test]
fn a_rejected_url_and_a_failed_open_are_refused() {
    let rig = Rig::new();
    *lock(&rig.settings.result) =
        PaneOpen::Refused("macOS did not open System Settings".to_owned());
    assert!(matches!(
        rig.run(BarAction::Battery, &Source::live()),
        BarActionOutcome::Refused { detail } if detail.contains("did not open")
    ));
    rig.backend.fail_next_open();
    assert!(matches!(
        rig.run(BarAction::Cpu, &Source::live()),
        BarActionOutcome::Refused { .. }
    ));
    assert!(rig.backend.open_requests().is_empty());
}

#[test]
fn pane_timeouts_keep_their_before_and_after_issuance_meaning() {
    let rig = Rig::new();
    *lock(&rig.settings.result) = PaneOpen::TimedOutBeforeIssuance;
    assert_eq!(
        rig.run(BarAction::Wifi, &Source::live()),
        BarActionOutcome::TimedOutBeforeIssuance
    );
    *lock(&rig.settings.result) = PaneOpen::UncertainAfterIssuance;
    assert_eq!(
        rig.run(BarAction::Wifi, &Source::live()),
        BarActionOutcome::UncertainAfterIssuance
    );
}

#[test]
fn an_issued_open_without_a_completion_is_uncertain_and_never_retried() {
    let rig = Rig::new();
    let source = Source::live();
    assert_eq!(
        rig.run(BarAction::Cpu, &source),
        BarActionOutcome::UncertainAfterIssuance
    );
    assert_eq!(
        rig.backend.open_requests().len(),
        1,
        "issued exactly once, no retry"
    );
    assert_eq!(rig.backend.pending_opens(), 1);
}

#[test]
fn an_ack_timeout_before_issuance_sends_nothing_even_when_the_job_wakes_late() {
    let rig = Rig::new();
    let barrier = rig.backend.block_next_open();
    let source = Source::live();
    let click = rig.spawn(BarAction::Cpu, &source);
    entered(&barrier);
    assert_eq!(
        click.join().expect("click"),
        BarActionOutcome::TimedOutBeforeIssuance
    );
    assert!(barrier.release());
    thread::sleep(Duration::from_millis(60));
    assert!(
        rig.backend.open_requests().is_empty(),
        "the abandoned job never sends"
    );
}

#[test]
fn an_ack_timeout_after_issuance_is_uncertain() {
    let rig = Rig::new();
    let barrier = rig.backend.block_next_open_after_issue();
    let source = Source::live();
    let click = rig.spawn(BarAction::Cpu, &source);
    entered(&barrier);
    assert_eq!(
        click.join().expect("click"),
        BarActionOutcome::UncertainAfterIssuance
    );
    assert!(barrier.release());
    assert_eq!(
        rig.backend.open_requests().len(),
        1,
        "the request had issued; no retry"
    );
    thread::sleep(Duration::from_millis(60));
    assert_eq!(rig.backend.open_requests().len(), 1);
}

#[test]
fn stopping_during_preflight_makes_the_click_stale_and_sends_nothing() {
    let rig = Rig::new();
    let barrier = rig.backend.block_next_application_path();
    let source = Source::live();
    let click = rig.spawn(BarAction::Cpu, &source);
    entered(&barrier);
    source.stop();
    assert!(barrier.release());
    let outcome = click.join().expect("click");
    assert!(
        matches!(
            outcome,
            BarActionOutcome::Stale | BarActionOutcome::TimedOutBeforeIssuance
        ),
        "{outcome:?}"
    );
    assert!(rig.backend.open_requests().is_empty());
}

#[test]
fn stopping_before_the_native_send_makes_the_permit_refuse_it() {
    // The budget is long enough that the ack does not time out first.
    let backend = FakeApplicationBackend::new();
    backend.set_application_path(ACTIVITY_MONITOR, Some(PathBuf::from("/fake/AM.app")));
    let control = stache_macos::ApplicationControl::with_acknowledgement_budget(
        Arc::new(Queue),
        Arc::new(backend.clone()) as Arc<dyn ApplicationBackend>,
        Duration::from_secs(3),
    );
    let actions = Arc::new(BarActions::new(control, Settings::new(PaneOpen::Opened)));
    let barrier = backend.block_next_open();
    let source = Source::live();
    let click = {
        let actions = Arc::clone(&actions);
        let source = dynamic(&source);
        thread::spawn(move || actions.run(BarAction::Cpu, &source))
    };
    entered(&barrier);
    source.stop();
    assert!(barrier.release());
    assert_eq!(click.join().expect("click"), BarActionOutcome::Stale);
    assert!(backend.open_requests().is_empty(), "the permit refused the send");
    // No successor effects: a new run for a new generation works on its own.
    backend.complete_opens_synchronously();
    assert_eq!(
        actions.run(BarAction::Cpu, &dynamic(&Source::live())),
        BarActionOutcome::Completed
    );
    assert_eq!(backend.open_requests().len(), 1);
}

#[test]
fn a_pane_guard_that_fails_at_the_send_is_stale_not_sent() {
    let rig = Rig::new();
    let source = Source::live();
    source.caller.store(false, Ordering::SeqCst);
    assert_eq!(rig.run(BarAction::Battery, &source), BarActionOutcome::Stale);
    assert!(lock(&rig.settings.opened).is_empty());
}

#[test]
fn a_click_while_one_is_in_flight_is_busy_and_does_no_work() {
    let rig = Rig::new();
    let barrier = rig.backend.block_next_application_path();
    let source = Source::live();
    let first = rig.spawn(BarAction::Cpu, &source);
    entered(&barrier);
    let busy = Arc::new(AtomicUsize::new(0));
    for action in [
        BarAction::Cpu,
        BarAction::Clock,
        BarAction::Battery,
        BarAction::Wifi,
        BarAction::Weather,
    ]
    .into_iter()
    .cycle()
    .take(100)
    {
        if rig.run(action, &source) == BarActionOutcome::Busy {
            busy.fetch_add(1, Ordering::SeqCst);
        }
    }
    assert_eq!(
        busy.load(Ordering::SeqCst),
        100,
        "every rapid click is dropped, none queued"
    );
    assert!(lock(&rig.settings.opened).is_empty());
    assert!(barrier.release());
    let _ = first.join().expect("first click");
    // The slot is free again once the click ends.
    assert_eq!(rig.run(BarAction::Battery, &source), BarActionOutcome::Completed);
}

#[test]
fn only_the_four_known_players_map_to_a_fixed_application() {
    assert_eq!(media_player("com.spotify.client"), Some("com.spotify.client"));
    assert_eq!(
        media_player("com.microsoft.edgemac.Dev"),
        Some("com.microsoft.edgemac.Dev")
    );
    assert_eq!(media_player("com.tidal.desktop"), Some("com.tidal.desktop"));
    assert_eq!(media_player("org.jeffvli.feishin"), Some("org.jeffvli.feishin"));
    for other in [
        "",
        "com.microsoft.edgemac",
        "com.microsoft.edgemac.Beta",
        "com.microsoft.edgemac.dev",
        "COM.SPOTIFY.CLIENT",
        "com.spotify.client ",
        "com.spotify.client/../../x",
        "/Applications/Spotify.app",
        "com.apple.Music",
    ] {
        assert_eq!(media_player(other), None, "{other:?}");
    }
}

#[test]
fn a_media_click_opens_the_fixed_application_of_each_known_player() {
    for (bundle, path) in [
        (SPOTIFY, "/fake/Spotify.app"),
        (EDGE_DEV, "/fake/Microsoft Edge Dev.app"),
        (TIDAL, "/fake/TIDAL.app"),
        (FEISHIN, "/fake/Feishin.app"),
    ] {
        let rig = Rig::new();
        rig.backend.complete_opens_synchronously();
        rig.backend.set_application_path(bundle, Some(PathBuf::from(path)));
        let source = Source::live();
        *lock(&source.media) = Some(bundle.to_owned());
        assert_eq!(
            rig.run(BarAction::Media, &source),
            BarActionOutcome::Completed,
            "{bundle}"
        );
        assert_eq!(rig.backend.open_requests(), [PathBuf::from(path)], "{bundle}");
    }
}

#[test]
fn a_media_click_with_an_unknown_or_absent_player_launches_nothing() {
    for media in [
        None,
        Some(String::new()),
        Some("com.apple.Music".to_owned()),
        Some("com.microsoft.edgemac".to_owned()),
    ] {
        let rig = Rig::new();
        rig.backend.complete_opens_synchronously();
        // Even an installed look-alike is never opened.
        rig.backend
            .set_application_path("com.apple.Music", Some(PathBuf::from("/fake/Music.app")));
        let source = Source::live();
        *lock(&source.media) = media.clone();
        assert_eq!(
            rig.run(BarAction::Media, &source),
            BarActionOutcome::NotAttempted,
            "{media:?}"
        );
        assert!(rig.backend.open_requests().is_empty(), "{media:?}");
        assert_eq!(rig.backend.observations(), 0, "no observation was installed");
    }
}

#[test]
fn a_media_click_for_a_known_player_that_is_not_installed_is_typed() {
    let rig = Rig::new();
    let source = Source::live();
    *lock(&source.media) = Some(SPOTIFY.to_owned());
    assert_eq!(
        rig.run(BarAction::Media, &source),
        BarActionOutcome::NotInstalled
    );
    assert!(rig.backend.open_requests().is_empty());
}

#[test]
fn a_stale_runtime_beats_the_media_mapping() {
    let rig = Rig::new();
    let source = Source::live();
    *lock(&source.media) = Some(SPOTIFY.to_owned());
    source.stop();
    assert_eq!(rig.run(BarAction::Media, &source), BarActionOutcome::Stale);
}
