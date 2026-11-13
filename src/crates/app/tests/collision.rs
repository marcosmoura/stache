//! Startup collision detection over the fake running-application query
//! (`RELEASE-04-03`, `RELEASE-04-05`). Nothing here queries or signals a real
//! application: the fake has no signal capability at all.

use std::path::PathBuf;
use std::sync::Mutex;

use stache_app::collision::{self, Collision, CollisionPolicy, Decision};
use stache_macos::{FakeRunningInstances, RunningInstance};
use stache_sdk::{
    DEVELOPMENT_IDENTITY, IDENTITY, LogLevel, LogRecord, Logger, RELEASE_IDENTITY, SIBLING_IDENTITY,
};

const OWN_PID: i32 = 4242;

#[derive(Default)]
struct Captured(Mutex<Vec<LogRecord>>);

impl Logger for Captured {
    fn log(&self, record: LogRecord) { self.0.lock().expect("logs").push(record); }
}

impl Captured {
    fn warnings(&self) -> Vec<String> {
        let records = self.0.lock().expect("logs");
        assert!(records.iter().all(|record| record.level == LogLevel::Warn));
        records.iter().map(|record| record.message.clone()).collect()
    }
}

fn instance(identifier: &str, pid: i32, path: &str) -> RunningInstance {
    RunningInstance {
        bundle_identifier: Some(identifier.to_owned()),
        pid,
        bundle_path: Some(PathBuf::from(path)),
    }
}

/// Drives the host's startup seam with the shipped policy.
fn startup(fake: &FakeRunningInstances) -> (Vec<Collision>, Vec<String>) {
    let logs = Captured::default();
    let refusals = collision::check(fake, OWN_PID, collision::POLICY, &logs);
    (refusals, logs.warnings())
}

#[test]
fn the_shipped_policy_only_warns() {
    assert_eq!(collision::POLICY, CollisionPolicy::WarnOnly);
}

#[test]
fn a_running_sibling_warns_once_and_startup_continues() {
    let fake = FakeRunningInstances::with(vec![instance(
        SIBLING_IDENTITY,
        77,
        "/Applications/Sibling.app",
    )]);
    let (refusals, warnings) = startup(&fake);
    assert!(refusals.is_empty());
    assert_eq!(warnings, [format!(
        "another {SIBLING_IDENTITY} is running at /Applications/Sibling.app (pid 77)"
    )]);
    assert_eq!(fake.queries(), [IDENTITY, SIBLING_IDENTITY]);
}

#[test]
fn a_foreign_process_with_this_identifier_warns_once_and_startup_continues() {
    let fake = FakeRunningInstances::with(vec![
        instance(IDENTITY, OWN_PID, "/Applications/Own.app"),
        instance(IDENTITY, 78, "/Applications/Other.app"),
    ]);
    let (refusals, warnings) = startup(&fake);
    assert!(refusals.is_empty());
    assert_eq!(warnings, [format!(
        "another {IDENTITY} is running at /Applications/Other.app (pid 78)"
    )]);
}

#[test]
fn only_this_process_running_is_silent() {
    let fake =
        FakeRunningInstances::with(vec![instance(IDENTITY, OWN_PID, "/Applications/Own.app")]);
    assert_eq!(startup(&fake), (Vec::new(), Vec::new()));
}

#[test]
fn prefix_matches_are_ignored() {
    // The fake returns prefix matches like a careless query would.
    let fake = FakeRunningInstances::with(vec![
        instance("com.marcosmoura.stache.helper", 79, "/Applications/Helper.app"),
        instance(&format!("{IDENTITY}.helper"), 80, "/Applications/Helper2.app"),
    ]);
    assert_eq!(startup(&fake), (Vec::new(), Vec::new()));
}

#[test]
fn a_failed_query_warns_for_each_identifier_and_startup_continues() {
    let fake = FakeRunningInstances::failing("unavailable");
    let (refusals, warnings) = startup(&fake);
    assert!(refusals.is_empty());
    assert_eq!(warnings, [
        format!("could not check for a running {IDENTITY}: unavailable"),
        format!("could not check for a running {SIBLING_IDENTITY}: unavailable"),
    ]);
}

#[test]
fn the_sibling_warns_under_both_build_identities() {
    for (own, sibling) in [
        (DEVELOPMENT_IDENTITY, RELEASE_IDENTITY),
        (RELEASE_IDENTITY, DEVELOPMENT_IDENTITY),
    ] {
        for policy in [
            CollisionPolicy::WarnOnly,
            CollisionPolicy::RefuseSameIdentity,
        ] {
            let running = instance(sibling, 81, "/Applications/Sibling.app");
            assert!(
                matches!(
                    collision::decide(&running, OWN_PID, own, sibling, policy),
                    Decision::Warn(_)
                ),
                "{own} {policy:?}"
            );
        }
    }
}

#[test]
fn the_later_policy_refuses_only_a_foreign_same_identifier() {
    let policy = CollisionPolicy::RefuseSameIdentity;
    let foreign = instance(IDENTITY, 82, "/Applications/Other.app");
    assert_eq!(
        collision::decide(&foreign, OWN_PID, IDENTITY, SIBLING_IDENTITY, policy),
        Decision::Refuse(Collision {
            identifier: IDENTITY.to_owned(),
            bundle_path: Some(PathBuf::from("/Applications/Other.app")),
            pid: 82,
        })
    );
    let own = instance(IDENTITY, OWN_PID, "/Applications/Own.app");
    assert_eq!(
        collision::decide(&own, OWN_PID, IDENTITY, SIBLING_IDENTITY, policy),
        Decision::Ignore
    );
    let unbundled = RunningInstance {
        bundle_identifier: None,
        pid: 83,
        bundle_path: None,
    };
    assert_eq!(
        collision::decide(&unbundled, OWN_PID, IDENTITY, SIBLING_IDENTITY, policy),
        Decision::Ignore
    );
}
