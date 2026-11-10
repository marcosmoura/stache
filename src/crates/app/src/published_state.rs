//! App-boundary projection of SDK published state.
//!
//! The SDK keeps `u64` generation, revision and sequence values exact. Like the
//! module `revision` in [`crate::bindings`], they cross into the frontend as
//! exact decimal strings, because JavaScript numbers lose precision above 2^53.
//! The payload stays the serialized JSON the provider's registered type
//! produced; the app decodes it with the provider's own type
//! ([`stache_sdk::StateEnvelope::decode`]) where it needs more than a projection.

use serde::Serialize;
use serde_json::Value;
use stache_sdk::{StateEnvelope, StateReading, StateUnavailableReason};

/// One published envelope with its numbers as exact decimal strings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedEnvelopeView {
    /// The provider module identifier.
    pub provider: String,
    /// The runtime generation as an exact decimal string.
    pub generation: String,
    /// The accepted configuration revision as an exact decimal string.
    pub revision: String,
    /// The per-generation sequence as an exact decimal string.
    pub sequence: String,
    /// The registered schema name.
    pub schema: String,
    /// The registered schema version.
    pub schema_version: u32,
    /// The serialized payload of the provider's registered type.
    pub payload: Value,
}

impl From<&StateEnvelope> for PublishedEnvelopeView {
    fn from(envelope: &StateEnvelope) -> Self {
        Self {
            provider: envelope.provider.as_str().to_owned(),
            generation: envelope.generation.to_string(),
            revision: envelope.revision.to_string(),
            sequence: envelope.sequence.to_string(),
            schema: envelope.schema.name.to_owned(),
            schema_version: envelope.schema.version,
            payload: envelope.payload.as_ref().clone(),
        }
    }
}

/// What the frontend may be told about one provider's published state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum PublishedStateView {
    /// No provider or schema is registered under the identifier.
    Absent,
    /// The provider is known but has no active state.
    #[serde(rename_all = "camelCase")]
    Unavailable {
        /// A stable reason name.
        reason: String,
        /// The generation the reason refers to, as an exact decimal string.
        generation: Option<String>,
    },
    /// The latest value of the running generation.
    Available {
        /// The envelope.
        envelope: PublishedEnvelopeView,
    },
}

impl From<&StateReading> for PublishedStateView {
    fn from(reading: &StateReading) -> Self {
        match reading {
            StateReading::Absent => Self::Absent,
            StateReading::Unavailable(tombstone) => Self::Unavailable {
                reason: reason_name(tombstone.reason).to_owned(),
                generation: tombstone.generation.map(|generation| generation.to_string()),
            },
            StateReading::Available(envelope) => Self::Available { envelope: envelope.into() },
        }
    }
}

const fn reason_name(reason: StateUnavailableReason) -> &'static str {
    use stache_sdk::RuntimeEnd;

    match reason {
        StateUnavailableReason::NotStarted => "notStarted",
        StateUnavailableReason::Starting => "starting",
        StateUnavailableReason::NotPublished => "notPublished",
        StateUnavailableReason::Ended(RuntimeEnd::Stopped) => "stopped",
        StateUnavailableReason::Ended(RuntimeEnd::Disabled) => "disabled",
        StateUnavailableReason::Ended(RuntimeEnd::Paused) => "paused",
        StateUnavailableReason::Ended(RuntimeEnd::Failed) => "failed",
        StateUnavailableReason::Ended(RuntimeEnd::PermissionBlocked) => "permissionBlocked",
        StateUnavailableReason::Ended(RuntimeEnd::DependencyUnavailable) => "dependencyUnavailable",
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use serde::Deserialize;
    use stache_sdk::{
        ConfigError, ConfigPaths, ConfigStore, DiscardLogger, EventBus, LogFilter,
        MainThreadExecutor, Module, ModuleConfig, ModuleContext, ModuleDescriptor, ModuleId,
        ModuleResult, Permission, PermissionError, PermissionProvider, PermissionStatus,
        PublishedState, Registry, StatePublisher, StateSchema, StateSchemas, ThreadError,
        UnavailableWindows,
    };

    use super::*;
    use crate::host::FactoryContext;

    const FAKE_PROVIDER: ModuleId = ModuleId::new("fakeProvider");

    /// Stands in for a provider crate's DTO; the app imports the original type.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct FakeStatus {
        active: bool,
    }

    impl PublishedState for FakeStatus {
        const SCHEMA: StateSchema = StateSchema { name: "fakeStatus", version: 1 };
    }

    #[derive(Clone, PartialEq)]
    struct FakeConfig;

    impl ModuleConfig for FakeConfig {
        fn enabled(&self) -> bool { true }

        fn validate(&self) -> Result<(), ConfigError> { Ok(()) }
    }

    struct FakeProvider {
        publisher: Arc<Mutex<Option<StatePublisher<FakeStatus>>>>,
    }

    impl Module for FakeProvider {
        type Config = FakeConfig;

        fn descriptor(&self) -> ModuleDescriptor {
            ModuleDescriptor {
                id: FAKE_PROVIDER,
                provides: &[],
                requires: &[],
                permissions: &[],
                executables: &[],
                surfaces: &[],
                observes: &[],
                reserves_screen: false,
            }
        }

        fn configure(&mut self, _: &FakeConfig, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }

        fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
            let publisher = ctx.state_publisher::<FakeStatus>().expect("registered");
            let _ = publisher.publish(1, &FakeStatus { active: true }).expect("publish");
            *self.publisher.lock().expect("lock") = Some(publisher);
            Ok(())
        }

        fn reconfigure(&mut self, _: &FakeConfig, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }

        fn stop(&mut self, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }
    }

    struct Grant;

    impl PermissionProvider for Grant {
        fn status(&self, _: Permission) -> PermissionStatus { PermissionStatus::Granted }

        fn request(&self, _: Permission) -> Result<PermissionStatus, PermissionError> {
            Ok(PermissionStatus::Granted)
        }
    }

    struct Inline;

    impl MainThreadExecutor for Inline {
        fn is_main_thread(&self) -> bool { true }

        fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
            job();
            Ok(())
        }
    }

    fn context(root: &Path, registry: Arc<Registry>) -> FactoryContext {
        let store = Arc::new(ConfigStore::new(
            Arc::clone(&registry),
            crate::catalog::catalog().expect("catalog"),
            ConfigPaths::new(root.join("config")),
            Arc::new(LogFilter::default()),
        ));
        FactoryContext::new(registry, store)
    }

    #[test]
    fn the_app_registers_a_codec_and_factory_context_reads_the_state() {
        let root = tempfile::tempdir().expect("root");
        let registry = Arc::new(
            Registry::new(
                Arc::new(DiscardLogger),
                Arc::new(EventBus::new()),
                Arc::new(Inline),
                Arc::new(Grant),
                Arc::new(UnavailableWindows),
            )
            .with_state_schemas(&StateSchemas::new().with::<FakeStatus>(FAKE_PROVIDER))
            .expect("schemas register before modules"),
        );
        let context = context(root.path(), Arc::clone(&registry));
        let observer = context.observe_state(FAKE_PROVIDER);
        assert_eq!(
            PublishedStateView::from(&observer.read().state),
            PublishedStateView::Absent
        );

        let publisher = Arc::new(Mutex::new(None));
        registry
            .register(
                FakeProvider {
                    publisher: Arc::clone(&publisher),
                },
                FakeConfig,
            )
            .expect("register");
        assert_eq!(
            PublishedStateView::from(&observer.read().state),
            PublishedStateView::Unavailable {
                reason: "notStarted".to_owned(),
                generation: None
            }
        );
        registry.start_all().expect("start");

        let reading = observer.read().state;
        let StateReading::Available(envelope) = &reading else {
            panic!("expected an available state");
        };
        // The app decodes with the provider's own type and its registered codec.
        assert_eq!(
            envelope.decode::<FakeStatus>(FAKE_PROVIDER).expect("decode"),
            FakeStatus { active: true }
        );

        // The frontend projection carries exact decimal strings.
        let view = PublishedStateView::from(&reading);
        let json = serde_json::to_value(&view).expect("serializes");
        assert_eq!(json["state"], "available");
        assert_eq!(json["envelope"]["revision"], "1");
        assert_eq!(json["envelope"]["sequence"], "1");
        assert_eq!(json["envelope"]["generation"], envelope.generation.to_string());
        assert_eq!(json["envelope"]["schema"], "fakeStatus");
        assert_eq!(json["envelope"]["schemaVersion"], 1);
        assert_eq!(json["envelope"]["payload"]["active"], true);
    }

    #[test]
    fn numbers_above_two_to_the_fifty_third_stay_exact() {
        let envelope = StateEnvelope {
            provider: FAKE_PROVIDER,
            generation: u64::MAX,
            revision: (1 << 53) + 1,
            sequence: u64::MAX - 1,
            schema: stache_sdk::SchemaIdentity {
                provider: FAKE_PROVIDER,
                name: "fakeStatus",
                version: 1,
            },
            payload: Arc::new(serde_json::json!({ "active": false })),
        };
        let view = PublishedEnvelopeView::from(&envelope);
        assert_eq!(view.generation, "18446744073709551615");
        assert_eq!(view.revision, "9007199254740993");
        assert_eq!(view.sequence, "18446744073709551614");
    }

    #[test]
    fn tombstones_project_a_stable_reason_and_generation() {
        let tombstone = StateReading::Unavailable(stache_sdk::StateUnavailable {
            reason: StateUnavailableReason::Ended(stache_sdk::RuntimeEnd::Paused),
            generation: Some(7),
        });
        assert_eq!(
            PublishedStateView::from(&tombstone),
            PublishedStateView::Unavailable {
                reason: "paused".to_owned(),
                generation: Some("7".to_owned()),
            }
        );
    }
}
