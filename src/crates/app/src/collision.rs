//! Startup detection of another running Stache bundle.
//!
//! The desktop host calls [`check`] after claiming the instance lock and before
//! creating the tray, modules or socket. The command line never calls it. It
//! only reads the running applications for two exact bundle identifiers and
//! logs; it never signals, terminates or opens anything.

use std::path::PathBuf;

use stache_macos::{RunningInstance, RunningInstances};
use stache_sdk::{IDENTITY, LogLevel, LogRecord, Logger, ModuleId, Operation, SIBLING_IDENTITY};

/// How a match on this process's own identifier is treated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollisionPolicy {
    /// Stage 1: every match only warns.
    WarnOnly,
    /// From the rename on: a foreign process with this identifier refuses.
    RefuseSameIdentity,
}

/// The policy this build ships with.
pub const POLICY: CollisionPolicy = CollisionPolicy::WarnOnly;

/// What one running application means for startup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Log one line and continue.
    Warn(Collision),
    /// Report and exit before tray, modules or socket.
    Refuse(Collision),
    /// Not a collision.
    Ignore,
}

/// The facts a collision diagnostic names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collision {
    /// The exact bundle identifier.
    pub identifier: String,
    /// The bundle location, if known.
    pub bundle_path: Option<PathBuf>,
    /// The process identifier.
    pub pid: i32,
}

impl Collision {
    /// The one diagnostic line.
    #[must_use]
    pub fn message(&self) -> String {
        let path = self.bundle_path.as_ref().map_or_else(
            || "unknown bundle path".to_owned(),
            |path| path.display().to_string(),
        );
        format!(
            "another {} is running at {path} (pid {})",
            self.identifier, self.pid
        )
    }
}

/// Classifies one running application. Only exact identifiers count, and this
/// process's own PID is never a collision.
#[must_use]
pub fn decide(
    instance: &RunningInstance,
    own_pid: i32,
    own_identifier: &str,
    sibling_identifier: &str,
    policy: CollisionPolicy,
) -> Decision {
    if instance.pid == own_pid {
        return Decision::Ignore;
    }
    let Some(identifier) = instance.bundle_identifier.as_deref() else {
        return Decision::Ignore;
    };
    let collision = || Collision {
        identifier: identifier.to_owned(),
        bundle_path: instance.bundle_path.clone(),
        pid: instance.pid,
    };
    if identifier == own_identifier {
        match policy {
            CollisionPolicy::WarnOnly => Decision::Warn(collision()),
            CollisionPolicy::RefuseSameIdentity => Decision::Refuse(collision()),
        }
    } else if identifier == sibling_identifier {
        Decision::Warn(collision())
    } else {
        Decision::Ignore
    }
}

/// Queries this identifier and the sibling and logs one warning per match, or
/// one warning per failed query. Returns the refusals, which are always empty
/// under [`CollisionPolicy::WarnOnly`].
pub fn check(
    query: &dyn RunningInstances,
    own_pid: i32,
    policy: CollisionPolicy,
    logger: &dyn Logger,
) -> Vec<Collision> {
    let mut refusals = Vec::new();
    // A process reported by both queries is still one match.
    let mut seen = Vec::new();
    for identifier in [IDENTITY, SIBLING_IDENTITY] {
        let instances = match query.running(identifier) {
            Ok(instances) => instances,
            Err(error) => {
                warn(
                    logger,
                    format!("could not check for a running {identifier}: {error}"),
                );
                continue;
            }
        };
        for instance in &instances {
            if seen.contains(&instance.pid) {
                continue;
            }
            seen.push(instance.pid);
            match decide(instance, own_pid, IDENTITY, SIBLING_IDENTITY, policy) {
                Decision::Warn(collision) => warn(logger, collision.message()),
                Decision::Refuse(collision) => refusals.push(collision),
                Decision::Ignore => {}
            }
        }
    }
    refusals
}

fn warn(logger: &dyn Logger, message: String) {
    logger.log(LogRecord {
        level: LogLevel::Warn,
        module: ModuleId::new("host"),
        operation: Operation::Start,
        message,
    });
}
