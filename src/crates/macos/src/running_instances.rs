//! A read-only query for running applications with one exact bundle identifier.
//!
//! The desktop host uses it at startup to notice a sibling or same-identity
//! bundle. It reports facts only: there is no terminate, signal or open
//! capability here, and none may be added.

use std::path::PathBuf;

use objc2_app_kit::NSRunningApplication;
use objc2_foundation::NSString;

/// One running application reported by [`RunningInstances::running`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunningInstance {
    /// The bundle identifier the system reports for the process, if any.
    pub bundle_identifier: Option<String>,
    /// The process identifier.
    pub pid: i32,
    /// The bundle location, when the application has a bundle.
    pub bundle_path: Option<PathBuf>,
}

/// Lists running applications by exact bundle identifier.
pub trait RunningInstances: Send + Sync {
    /// The applications the system reports for `bundle_identifier`.
    ///
    /// # Errors
    /// Returns a description when the query is unavailable.
    fn running(&self, bundle_identifier: &str) -> Result<Vec<RunningInstance>, String>;
}

/// The `NSRunningApplication` query.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRunningInstances;

impl RunningInstances for SystemRunningInstances {
    fn running(&self, bundle_identifier: &str) -> Result<Vec<RunningInstance>, String> {
        let identifier = NSString::from_str(bundle_identifier);
        let applications =
            NSRunningApplication::runningApplicationsWithBundleIdentifier(&identifier);
        Ok(applications
            .to_vec()
            .into_iter()
            .filter(|application| !application.isTerminated())
            .map(|application| RunningInstance {
                bundle_identifier: application.bundleIdentifier().map(|id| id.to_string()),
                pid: application.processIdentifier(),
                bundle_path: application
                    .bundleURL()
                    .and_then(|url| url.path())
                    .map(|path| PathBuf::from(path.to_string())),
            })
            .collect())
    }
}

/// Test doubles for [`RunningInstances`].
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use std::sync::Mutex;

    use super::{RunningInstance, RunningInstances};

    /// A fixed list of running applications, or a failing query.
    ///
    /// It returns every listed instance whose identifier starts with the
    /// requested one, like a careless system would, so callers must compare
    /// identifiers exactly. It records each query; it has no way to signal.
    #[derive(Debug, Default)]
    pub struct FakeRunningInstances {
        instances: Vec<RunningInstance>,
        failure: Option<String>,
        queries: Mutex<Vec<String>>,
    }

    impl FakeRunningInstances {
        /// A fake that reports `instances`.
        #[must_use]
        pub fn with(instances: Vec<RunningInstance>) -> Self {
            Self { instances, ..Self::default() }
        }

        /// A fake whose query always fails with `message`.
        #[must_use]
        pub fn failing(message: &str) -> Self {
            Self {
                failure: Some(message.to_owned()),
                ..Self::default()
            }
        }

        /// The identifiers queried so far, in order.
        ///
        /// # Panics
        /// Panics when the query log mutex is poisoned.
        #[must_use]
        pub fn queries(&self) -> Vec<String> { self.queries.lock().expect("queries").clone() }
    }

    impl RunningInstances for FakeRunningInstances {
        fn running(&self, bundle_identifier: &str) -> Result<Vec<RunningInstance>, String> {
            self.queries
                .lock()
                .map_err(|error| error.to_string())?
                .push(bundle_identifier.to_owned());
            if let Some(failure) = &self.failure {
                return Err(failure.clone());
            }
            Ok(self
                .instances
                .iter()
                .filter(|instance| {
                    instance
                        .bundle_identifier
                        .as_deref()
                        .is_some_and(|id| id.starts_with(bundle_identifier))
                })
                .cloned()
                .collect())
        }
    }
}
