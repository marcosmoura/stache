//! Startup collision detection over the fake running-application query
//! (`RELEASE-04-03`, `RELEASE-04-04`, `RELEASE-04-05`). Nothing here queries or signals a real
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

/// Drives the host's startup step with the shipped policy, as `run_with` does
/// between the instance lock and the tray: `guard(..)?` and then the rest.
/// Returns the step's result, whether the rest ran, and the warnings.
fn startup(fake: &FakeRunningInstances) -> (Result<(), String>, bool, Vec<String>) {
    let logs = Captured::default();
    let mut rest_ran = false;
    let result = (|| -> Result<(), String> {
        collision::guard(fake, OWN_PID, &logs)?;
        rest_ran = true;
        Ok(())
    })();
    // The only interaction with the running application is the two queries.
    assert_eq!(fake.queries(), [IDENTITY, SIBLING_IDENTITY]);
    (result, rest_ran, logs.warnings())
}

#[test]
fn the_shipped_policy_refuses_the_same_identifier() {
    assert_eq!(collision::POLICY, CollisionPolicy::RefuseSameIdentity);
}

#[test]
fn a_running_sibling_warns_once_and_startup_continues() {
    let fake = FakeRunningInstances::with(vec![instance(
        SIBLING_IDENTITY,
        77,
        "/Applications/Sibling.app",
    )]);
    let (result, rest_ran, warnings) = startup(&fake);
    assert_eq!(result, Ok(()));
    assert!(rest_ran);
    assert_eq!(warnings, [format!(
        "another {SIBLING_IDENTITY} is running at /Applications/Sibling.app (pid 77)"
    )]);
}

#[test]
fn a_foreign_process_with_this_identifier_refuses_before_anything_else_runs() {
    let fake = FakeRunningInstances::with(vec![
        instance(IDENTITY, OWN_PID, "/Applications/Own.app"),
        instance(IDENTITY, 78, "/Applications/Other.app"),
    ]);
    let (result, rest_ran, warnings) = startup(&fake);
    assert_eq!(
        result,
        Err(format!(
            "cannot start: another {IDENTITY} is running at /Applications/Other.app (pid 78). \
             Quit that app from its menu bar icon, then start this one again"
        ))
    );
    assert!(!rest_ran, "nothing after the check runs on refusal");
    assert!(warnings.is_empty());
}

#[test]
fn only_this_process_running_is_silent() {
    let fake =
        FakeRunningInstances::with(vec![instance(IDENTITY, OWN_PID, "/Applications/Own.app")]);
    assert_eq!(startup(&fake), (Ok(()), true, Vec::new()));
}

#[test]
fn prefix_matches_are_ignored() {
    // The fake returns prefix matches like a careless query would.
    let fake = FakeRunningInstances::with(vec![
        instance("com.marcosmoura.stache.helper", 79, "/Applications/Helper.app"),
        instance(&format!("{IDENTITY}.helper"), 80, "/Applications/Helper2.app"),
    ]);
    assert_eq!(startup(&fake), (Ok(()), true, Vec::new()));
}

#[test]
fn a_failed_query_warns_for_each_identifier_and_startup_continues() {
    let fake = FakeRunningInstances::failing("unavailable");
    let (result, rest_ran, warnings) = startup(&fake);
    assert_eq!(result, Ok(()));
    assert!(rest_ran);
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

/// The host itself must propagate the refusal: `run_with` calls
/// `collision::guard(..)?` after the instance lock and before the Tauri
/// builder. A source check, since `run_with` needs a real Tauri runtime.
#[test]
fn run_with_propagates_the_guard_before_the_tray() {
    let source = include_str!("../src/lib.rs");
    let body = source.split("fn run_with(").nth(1).expect("run_with");
    let lock = body.find("InstanceLock::acquire(").expect("instance lock");
    let guard = body.find("collision::guard(").expect("guard call");
    let builder = body.find("tauri::Builder::default()").expect("builder");
    assert!(
        lock < guard && guard < builder,
        "lock {lock}, guard {guard}, builder {builder}"
    );
    let end = guard + body[guard..].find(';').expect("statement end");
    let statement = &body[guard..=end];
    assert!(
        statement.ends_with(")?;"),
        "the guard result is propagated with `?`: {statement}"
    );
}
