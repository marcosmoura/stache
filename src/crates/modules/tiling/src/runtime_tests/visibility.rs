//! Visibility runtime tests: the engine, runner, ledger, application backend
//! and window backend together. Fakes only: nothing is hidden, activated or
//! raised for real.

use stache_macos::applications::{ActivationOutcome, LifecycleKind};
use stache_macos::windows::{IssuedKind, NativeStatus};
use stache_macos::{FakeAppKey, VisibilityOperation};

use super::*;
use crate::intent::{FocusTarget, Intent};
use crate::state::AppInfo;
use crate::visibility::{
    ApplicationActionsSource, FocusStep, Ownership, UnhideEvidence, VisibilityControl,
};
use crate::visibility_tests::{FixtureSource, Native};

struct V {
    rig: Rig,
    native: Arc<Native>,
    ids: Vec<ApplicationId>,
    keys: Vec<FakeAppKey>,
    vis: Arc<VisibilityControl>,
}

fn config() -> Arc<crate::config::TilingConfig> {
    Arc::new(
        serde_json::from_value(serde_json::json!({"workspaces": [{"name": "a"}, {"name": "b"}]}))
            .expect("config"),
    )
}

fn v() -> V {
    let native = Arc::new(Native::new(Duration::from_millis(500)));
    // A windowless Finder hosts the desktop, so an empty workspace is
    // reachable (not inactive).
    let source = FixtureSource::new(&native, &["com.a", "com.b", "com.apple.finder"]);
    let actions = source.open(&CancellationToken::new()).expect("open");
    let (ids, keys) = (source.ids(), source.keys());
    let mut rig = Rig::new("", Vec::new(), 64, 64);
    *rig.directory.apps.lock().expect("apps") = ids
        .iter()
        .enumerate()
        .map(|(n, id)| AppInfo {
            application: *id,
            pid: 100 + i32::try_from(n).unwrap_or_default(),
            bundle_id: if n == 2 {
                "com.apple.finder".to_owned()
            } else {
                format!("com.fixture.{n}")
            },
            name: format!("Fixture {n}"),
        })
        .collect();
    let vis = Arc::new(VisibilityControl::new());
    let env = EffectEnv {
        backend: Arc::clone(&rig.backend) as Arc<dyn WindowBackend>,
        directory: Arc::clone(&rig.directory) as Arc<dyn crate::actor::AppDirectory>,
        ingress: Arc::clone(&rig.ingress),
        cancel: CancellationToken::new(),
        frames: Arc::clone(&rig.frames),
        visibility: Arc::clone(&vis),
        actions: Some(Arc::new(actions)),
        animation: Arc::clone(&rig.animation),
        borders: None,
    };
    let mut engine = Engine::new(
        TilingState::new(""),
        Arc::clone(&rig.ingress),
        Arc::clone(&rig.cell),
        Arc::new(InlineRunner::new(env)),
        Arc::clone(&rig.sink),
        Arc::clone(&rig.clock) as Arc<dyn crate::actor::Clock>,
    );
    engine.set_frame_control(Arc::clone(&rig.frames));
    engine.set_visibility_control(Arc::clone(&vis));
    rig.engine = engine;
    // Finder reads visible at startup: nothing is sent to it.
    rig.backend.script_hidden(stache_macos::windows::HiddenOutcome::Read(
        stache_macos::ax::AxReading::Value(false),
    ));
    rig.start();
    for id in &ids {
        rig.complete(*id);
    }
    rig.ingress
        .submit(Input::Config { revision: 1, config: config() })
        .expect("config");
    rig.pump();
    V { rig, native, ids, keys, vis }
}

impl V {
    fn window(&mut self, n: usize, serial: u64) -> WindowInstance {
        let token = WindowInstance::for_testing(
            self.rig.feed.adapter,
            self.rig.feed.epoch,
            self.ids[n],
            serial,
        );
        self.rig.created(token, "w", u32::try_from(n as u64 * 10 + serial).unwrap_or(1));
        self.rig.pump();
        token
    }

    fn authorize(&mut self) {
        self.rig.gate.publish(self.rig.feed.stamp.registry, 1);
        self.rig.ingress.submit(Input::FrameWrites(true)).expect("authorize");
        self.rig.pump();
    }

    fn intent(&mut self, intent: Intent) {
        self.rig.ingress.submit(Input::Intent(intent)).expect("intent");
        self.rig.pump();
    }

    fn lifecycle(&mut self, n: usize, kind: LifecycleKind) {
        let info = AppInfo {
            application: self.ids[n],
            pid: 100 + i32::try_from(n).unwrap_or_default(),
            bundle_id: format!("com.fixture.{n}"),
            name: format!("Fixture {n}"),
        };
        self.rig.ingress.submit(Input::Lifecycle { kind, info }).expect("lifecycle");
        self.rig.pump();
    }

    fn requests(&self) -> Vec<(FakeAppKey, VisibilityOperation)> {
        self.native.backend.visibility_requests()
    }

    fn owned(&self, n: usize) -> Option<Ownership> { self.vis.ownership(self.ids[n]) }
}

#[test]
fn only_requirement_driven_hides_and_unhides_with_no_oscillation() {
    let mut v = v();
    let a1 = v.window(0, 1);
    v.authorize();
    assert!(v.requests().is_empty(), "A is on the visible workspace");
    assert!(v.rig.snap().window(a1).is_some());

    v.intent(Intent::FocusWorkspace("b".into()));
    assert_eq!(v.requests(), vec![(v.keys[0], VisibilityOperation::Hide)]);
    assert_eq!(
        v.owned(0),
        Some(Ownership::PendingIssued),
        "Requested is not a confirmation"
    );
    assert_eq!(v.rig.snap().hidden_apps.len(), 1);
    v.lifecycle(0, LifecycleKind::Hidden);
    assert_eq!(v.owned(0), Some(Ownership::Confirmed));
    assert_eq!(v.rig.snap().hidden_apps[0].ownership, Ownership::Confirmed);

    let _b1 = v.window(1, 1);
    v.intent(Intent::FocusWorkspace("a".into()));
    assert_eq!(v.requests(), vec![
        (v.keys[0], VisibilityOperation::Hide),
        (v.keys[0], VisibilityOperation::Unhide),
        (v.keys[1], VisibilityOperation::Hide),
    ]);
    // Nothing happens again without a requirement change.
    v.rig.pump();
    v.lifecycle(0, LifecycleKind::Unhidden);
    assert_eq!(v.owned(0), None, "our unhide was observed: released");
    assert_eq!(v.requests().len(), 3);
}

#[test]
fn an_application_shared_by_two_workspaces_is_never_hidden_and_the_conflict_is_published() {
    let mut v = v();
    let _a1 = v.window(0, 1);
    v.authorize();
    v.intent(Intent::FocusWorkspace("b".into()));
    assert_eq!(v.requests().len(), 1, "A is hidden while only a hides it");
    v.lifecycle(0, LifecycleKind::Hidden);
    let _a2 = v.window(0, 2);
    assert_eq!(
        v.requests().last(),
        Some(&(v.keys[0], VisibilityOperation::Unhide))
    );
    let snap = v.rig.snap();
    assert!(
        snap.visibility.conflicts.iter().any(|c| c.application == v.ids[0]),
        "{:?}",
        snap.visibility
    );
}

#[test]
fn a_prehidden_application_is_never_hidden_owned_or_unhidden() {
    let mut v = v();
    let _a1 = v.window(0, 1);
    v.authorize();
    v.lifecycle(0, LifecycleKind::Hidden);
    v.intent(Intent::FocusWorkspace("b".into()));
    v.intent(Intent::FocusWorkspace("a".into()));
    assert!(v.requests().is_empty(), "{:?}", v.requests());
    assert_eq!(v.owned(0), None);
    assert!(v.rig.snap().hidden_apps.is_empty());
}

#[test]
fn an_external_show_relinquishes_and_the_user_is_not_fought() {
    let mut v = v();
    let _a1 = v.window(0, 1);
    v.authorize();
    v.intent(Intent::FocusWorkspace("b".into()));
    v.lifecycle(0, LifecycleKind::Hidden);
    v.lifecycle(0, LifecycleKind::Unhidden);
    assert_eq!(v.owned(0), None);
    v.rig.pump();
    assert_eq!(v.requests().len(), 1, "no second hide against the user's show");
}

#[test]
fn no_hide_is_issued_after_the_seal() {
    let mut v = v();
    let _a1 = v.window(0, 1);
    v.authorize();
    v.vis.seal();
    v.intent(Intent::FocusWorkspace("b".into()));
    assert!(v.requests().is_empty());
    assert!(v.rig.snap().hidden_apps.is_empty());
}

#[test]
fn a_terminated_instance_clears_its_record() {
    let mut v = v();
    let _a1 = v.window(0, 1);
    v.authorize();
    v.intent(Intent::FocusWorkspace("b".into()));
    v.lifecycle(0, LifecycleKind::Hidden);
    assert_eq!(v.owned(0), Some(Ownership::Confirmed));
    v.lifecycle(0, LifecycleKind::Terminated);
    assert_eq!(v.owned(0), None);
}

fn raised() -> WindowActionOutcome {
    WindowActionOutcome::Issued {
        kind: IssuedKind::Applied,
        native: NativeStatus::Action(Ok(())),
        observed: None,
    }
}

#[test]
fn focus_unhides_an_owned_application_first_then_activates_then_raises() {
    let mut v = v();
    let a1 = v.window(0, 1);
    v.authorize();
    v.intent(Intent::FocusWorkspace("b".into()));
    v.lifecycle(0, LifecycleKind::Hidden);
    v.rig.backend.script_action(raised());
    v.intent(Intent::FocusWindow(FocusTarget::Window(a1)));
    let report = v.vis.focus_reports().pop().expect("a focus ran");
    assert_eq!(report.window, a1);
    assert!(
        matches!(report.steps[0], FocusStep::Unhide(UnhideEvidence::Sent)),
        "{report:?}"
    );
    assert_eq!(
        report.steps[1],
        FocusStep::Activate(ActivationOutcome::Requested)
    );
    assert!(matches!(report.steps[2], FocusStep::Raise(_)));
    assert_eq!(
        v.requests(),
        vec![
            (v.keys[0], VisibilityOperation::Hide),
            (v.keys[0], VisibilityOperation::Unhide),
        ],
        "one unhide only; the requirement does not repeat it"
    );
    assert_eq!(v.native.backend.exact_activation_requests(), vec![v.keys[0]]);
    assert!(
        v.rig
            .calls()
            .iter()
            .any(|c| matches!(c, FakeBackendCall::Action(w) if *w == a1)),
        "the raise went through the backend gate"
    );
}

#[test]
fn a_focus_never_claims_native_success_from_the_enqueue() {
    let mut v = v();
    let a1 = v.window(0, 1);
    v.authorize();
    v.native.backend.refuse_exact_activation();
    let before = v.rig.backend.action_count();
    v.intent(Intent::FocusWindow(FocusTarget::Window(a1)));
    let report = v.vis.focus_reports().pop().expect("a focus ran");
    assert_eq!(report.steps[0], FocusStep::Activate(ActivationOutcome::Refused));
    assert!(matches!(report.steps[1], FocusStep::Skipped(_)));
    assert_eq!(
        v.rig.backend.action_count(),
        before,
        "a refused activation never raises"
    );
}
