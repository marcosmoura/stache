//! Fake-backed contract tests: no native work, no sleeps used as synchronization.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::*;
use crate::error::{ConfigError, ErrorCategory, ModuleError, ModuleResult, Operation};
use crate::events::EventBus;
use crate::logging::DiscardLogger;
use crate::module::{Module, ModuleConfig, ModuleDescriptor, ServiceId};
use crate::permissions::{Permission, PermissionError, PermissionProvider, PermissionStatus};
use crate::registry::{ModuleHandle, Registry};
use crate::status::ModuleStatus;
use crate::threading::MainThreadExecutor;
use crate::windows::UnavailableWindows;
use crate::{ModuleContext, RegistryError};

const PROVIDER: ModuleId = ModuleId::new("provider");
const CONSUMER: ModuleId = ModuleId::new("consumer");
const INTRUDER: ModuleId = ModuleId::new("intruder");
const GHOST: ModuleId = ModuleId::new("ghost");

static OBSERVES_PROVIDER: [ModuleId; 1] = [PROVIDER];
static OBSERVES_GHOST: [ModuleId; 1] = [GHOST];
static REQUIRES_NOTHING_REGISTERED: [ServiceId; 1] = [ServiceId::new("missing.service")];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Temp {
    level: u32,
}

impl PublishedState for Temp {
    const SCHEMA: StateSchema = StateSchema { name: "temp", version: 1 };
}

/// Same name, different version: a different contract.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct TempV2 {
    level: u32,
}

impl PublishedState for TempV2 {
    const SCHEMA: StateSchema = StateSchema { name: "temp", version: 2 };
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Other {
    name: String,
}

impl PublishedState for Other {
    const SCHEMA: StateSchema = StateSchema { name: "other", version: 1 };
}

#[derive(Clone, PartialEq)]
struct Cfg {
    enabled: bool,
    value: u32,
}

impl Cfg {
    const fn on(value: u32) -> Self { Self { enabled: true, value } }
}

impl ModuleConfig for Cfg {
    fn enabled(&self) -> bool { self.enabled }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.value == 1000 {
            Err(ConfigError::new("invalid"))
        } else {
            Ok(())
        }
    }
}

type Step = Arc<dyn Fn(&ModuleContext, &Cfg) -> ModuleResult<()> + Send + Sync>;

#[derive(Default)]
struct Script {
    start: Mutex<Option<Step>>,
    reconfigure: Mutex<Option<Step>>,
    stop: Mutex<Option<Step>>,
}

impl Script {
    fn on_start(
        &self,
        step: impl Fn(&ModuleContext, &Cfg) -> ModuleResult<()> + Send + Sync + 'static,
    ) {
        *lock(&self.start) = Some(Arc::new(step));
    }

    fn on_reconfigure(
        &self,
        step: impl Fn(&ModuleContext, &Cfg) -> ModuleResult<()> + Send + Sync + 'static,
    ) {
        *lock(&self.reconfigure) = Some(Arc::new(step));
    }

    fn on_stop(
        &self,
        step: impl Fn(&ModuleContext, &Cfg) -> ModuleResult<()> + Send + Sync + 'static,
    ) {
        *lock(&self.stop) = Some(Arc::new(step));
    }
}

fn run(slot: &Mutex<Option<Step>>, ctx: &ModuleContext, cfg: &Cfg) -> ModuleResult<()> {
    let step = lock(slot).clone();
    step.map_or(Ok(()), |step| step(ctx, cfg))
}

struct Fake {
    id: ModuleId,
    observes: &'static [ModuleId],
    requires: &'static [ServiceId],
    script: Arc<Script>,
    config: Cfg,
}

impl Fake {
    fn new(id: ModuleId) -> (Self, Arc<Script>) {
        let script = Arc::new(Script::default());
        (
            Self {
                id,
                observes: &[],
                requires: &[],
                script: Arc::clone(&script),
                config: Cfg::on(0),
            },
            script,
        )
    }

    const fn observing(mut self, observes: &'static [ModuleId]) -> Self {
        self.observes = observes;
        self
    }
}

fn failure(id: ModuleId, operation: Operation) -> ModuleError {
    ModuleError::new(id, operation, ErrorCategory::Platform, "scripted failure")
}

impl Module for Fake {
    type Config = Cfg;

    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: self.id,
            provides: &[],
            requires: self.requires,
            permissions: &[],
            executables: &[],
            surfaces: &[],
            observes: self.observes,
            reserves_screen: false,
        }
    }

    fn configure(&mut self, config: &Cfg, _: &ModuleContext) -> ModuleResult<()> {
        self.config = config.clone();
        Ok(())
    }

    fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        run(&self.script.start, ctx, &self.config)
    }

    fn reconfigure(&mut self, config: &Cfg, ctx: &ModuleContext) -> ModuleResult<()> {
        let result = run(&self.script.reconfigure, ctx, config);
        if result.is_ok() {
            self.config = config.clone();
        }
        result
    }

    fn stop(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        run(&self.script.stop, ctx, &self.config)
    }
}

struct Grant;

impl PermissionProvider for Grant {
    fn status(&self, _: Permission) -> PermissionStatus { PermissionStatus::Granted }

    fn request(&self, permission: Permission) -> Result<PermissionStatus, PermissionError> {
        Ok(self.status(permission))
    }
}

struct Inline;

impl MainThreadExecutor for Inline {
    fn is_main_thread(&self) -> bool { true }

    fn execute(
        &self,
        job: Box<dyn FnOnce() + Send + 'static>,
    ) -> Result<(), crate::threading::ThreadError> {
        job();
        Ok(())
    }
}

fn bare_registry() -> Registry {
    Registry::new(
        Arc::new(DiscardLogger),
        Arc::new(EventBus::new()),
        Arc::new(Inline),
        Arc::new(Grant),
        Arc::new(UnavailableWindows),
    )
}

fn registry() -> Registry { bare_registry().with_state_schema::<Temp>(PROVIDER).expect("schema") }

/// A publisher captured by the provider's `start`.
type Slot = Arc<Mutex<Option<StatePublisher<Temp>>>>;

fn slot() -> Slot { Arc::new(Mutex::new(None)) }

fn captured(slot: &Slot) -> StatePublisher<Temp> {
    lock(slot).clone().expect("the provider captured a publisher")
}

/// A start that keeps its publisher and publishes `level = config value`.
fn capture_and_publish(script: &Script, slot: &Slot) {
    let slot = Arc::clone(slot);
    script.on_start(move |ctx, cfg| {
        let publisher = ctx.state_publisher::<Temp>().expect("registered publisher");
        let _ = publisher.publish(1, &Temp { level: cfg.value }).expect("publish");
        *lock(&slot) = Some(publisher);
        Ok(())
    });
}

fn provider_module(registry: &Registry, config: Cfg) -> (ModuleHandle<Cfg>, Arc<Script>, Slot) {
    let (module, script) = Fake::new(PROVIDER);
    let slot = slot();
    capture_and_publish(&script, &slot);
    let handle = registry.register(module, config).expect("register provider");
    (handle, script, slot)
}

fn available(reading: &StateReading) -> &StateEnvelope {
    match reading {
        StateReading::Available(envelope) => envelope,
        other => panic!("expected an available state, got {other:?}"),
    }
}

fn unavailable(reading: &StateReading) -> StateUnavailable {
    match reading {
        StateReading::Unavailable(tombstone) => *tombstone,
        other => panic!("expected an unavailable state, got {other:?}"),
    }
}

fn ended(end: RuntimeEnd) -> StateUnavailableReason { StateUnavailableReason::Ended(end) }

fn level(reading: &StateReading) -> u32 {
    available(reading).decode::<Temp>(PROVIDER).expect("decodes").level
}

// --- registration, identity and authorization --------------------------------

#[test]
fn independent_provider_and_consumer_share_one_registry() {
    let registry = registry();
    let (provider, _script, _slot) = provider_module(&registry, Cfg::on(3));
    let (consumer, consumer_script) = Fake::new(CONSUMER);
    let observed: Arc<Mutex<Option<StateObserver>>> = Arc::new(Mutex::new(None));
    {
        let observed = Arc::clone(&observed);
        consumer_script.on_start(move |ctx, _| {
            *lock(&observed) = Some(ctx.observe_state(PROVIDER).expect("declared"));
            Ok(())
        });
    }
    registry
        .register(consumer.observing(&OBSERVES_PROVIDER), Cfg::on(0))
        .expect("consumer");

    // Registered but not started: a known provider, not absent.
    let host = registry.observe_state(PROVIDER);
    assert_eq!(
        unavailable(&host.read().state).reason,
        StateUnavailableReason::NotStarted
    );

    registry.start_all().expect("start");
    let consumer_view = lock(&observed).clone().expect("consumer observer");
    let snapshot = consumer_view.read();
    let envelope = available(&snapshot.state);
    assert_eq!(envelope.provider, PROVIDER);
    assert_eq!(envelope.revision, 1);
    assert_eq!(envelope.sequence, 1);
    assert!(envelope.generation > 0);
    assert_eq!(envelope.schema, SchemaIdentity {
        provider: PROVIDER,
        name: "temp",
        version: 1
    });
    assert_eq!(envelope.schema.to_string(), "provider/temp@1");
    assert_eq!(envelope.decode::<Temp>(PROVIDER).expect("decode"), Temp {
        level: 3
    });
    assert_eq!(
        host.read(),
        snapshot,
        "app and consumer read the same authoritative cell"
    );
    assert_eq!(
        registry.snapshot(provider).expect("snapshot").status,
        ModuleStatus::Running
    );
    assert!(matches!(
        envelope.decode::<TempV2>(PROVIDER),
        Err(StateError::DecodeSchema { .. })
    ));
    assert!(
        matches!(
            envelope.decode::<Temp>(CONSUMER),
            Err(StateError::DecodeSchema { .. })
        ),
        "another provider with the same schema name and version cannot decode it"
    );
}

#[test]
fn an_unknown_provider_reads_absent_and_the_consumer_keeps_running() {
    let registry = registry();
    let (consumer, script) = Fake::new(CONSUMER);
    let observed: Arc<Mutex<Option<StateObserver>>> = Arc::new(Mutex::new(None));
    {
        let observed = Arc::clone(&observed);
        script.on_start(move |ctx, _| {
            *lock(&observed) = Some(ctx.observe_state(GHOST).expect("declared"));
            Ok(())
        });
    }
    let handle = registry
        .register(consumer.observing(&OBSERVES_GHOST), Cfg::on(0))
        .expect("an unknown observed provider is a valid registration");
    registry.start_all().expect("start");
    let ghost_view = lock(&observed).clone().expect("observer");
    assert_eq!(ghost_view.read(), StateSnapshot {
        version: 0,
        state: StateReading::Absent
    });
    assert_eq!(
        registry.snapshot(handle).expect("snapshot").status,
        ModuleStatus::Running
    );
}

#[test]
fn a_registered_provider_without_a_schema_reads_absent() {
    let registry = bare_registry();
    let (module, _) = Fake::new(PROVIDER);
    registry.register(module, Cfg::on(1)).expect("register");
    registry.start_all().expect("start");
    assert_eq!(
        registry.observe_state(PROVIDER).read().state,
        StateReading::Absent
    );
}

#[test]
fn observing_requires_a_declaration() {
    let registry = registry();
    let (consumer, script) = Fake::new(CONSUMER);
    let outcome: Arc<Mutex<Option<StateError>>> = Arc::new(Mutex::new(None));
    {
        let outcome = Arc::clone(&outcome);
        script.on_start(move |ctx, _| {
            *lock(&outcome) = ctx.observe_state(PROVIDER).err();
            Ok(())
        });
    }
    registry.register(consumer, Cfg::on(0)).expect("register");
    registry.start_all().expect("start");
    assert_eq!(
        lock(&outcome).clone(),
        Some(StateError::NotObserved {
            module: CONSUMER,
            provider: PROVIDER
        })
    );
}

#[test]
fn observes_never_satisfies_or_changes_hard_requires() {
    let registry = registry();
    let (provider, _script, _slot) = provider_module(&registry, Cfg::on(1));
    let _ = provider;
    let (mut consumer, _) = Fake::new(CONSUMER);
    consumer.requires = &REQUIRES_NOTHING_REGISTERED;
    registry
        .register(consumer.observing(&OBSERVES_PROVIDER), Cfg::on(0))
        .expect("register");
    let error = registry.start_all().expect_err("a missing hard requirement still fails");
    assert!(matches!(error, RegistryError::MissingProvider { module, .. } if module == CONSUMER));
}

#[test]
fn schema_registration_is_deterministic() {
    let duplicate = bare_registry()
        .with_state_schema::<Temp>(PROVIDER)
        .and_then(|registry| registry.with_state_schema::<TempV2>(PROVIDER));
    assert!(matches!(
        duplicate,
        Err(RegistryError::DuplicateStateSchema { provider }) if provider == PROVIDER
    ));
    let batch = bare_registry()
        .with_state_schemas(&StateSchemas::new().with::<Temp>(PROVIDER).with::<Other>(PROVIDER));
    assert!(matches!(batch, Err(RegistryError::DuplicateStateSchema { .. })));

    let late = bare_registry();
    let (module, _) = Fake::new(CONSUMER);
    late.register(module, Cfg::on(0)).expect("register");
    assert!(matches!(
        late.with_state_schema::<Temp>(PROVIDER),
        Err(RegistryError::StateSchemaAfterRegistration)
    ));

    let schemas = StateSchemas::new().with::<Temp>(PROVIDER);
    assert_eq!(schemas.len(), 1);
    assert!(!schemas.is_empty());
    assert!(StateSchemas::new().is_empty());
}

#[test]
fn publishers_are_authorized_and_typed() {
    let registry = registry();
    let (provider, provider_script, _slot) = provider_module(&registry, Cfg::on(1));
    let _ = provider;
    let errors: Arc<Mutex<Vec<(ModuleId, StateError)>>> = Arc::new(Mutex::new(Vec::new()));

    // The provider asks for another version of its schema.
    {
        let errors = Arc::clone(&errors);
        let original = lock(&provider_script.start).clone().expect("start step");
        provider_script.on_start(move |ctx, cfg| {
            if let Err(error) = ctx.state_publisher::<TempV2>() {
                lock(&errors).push((PROVIDER, error));
            }
            if let Err(error) = ctx.state_publisher::<Other>() {
                lock(&errors).push((PROVIDER, error));
            }
            original(ctx, cfg)
        });
    }
    // Another module asks for the provider's type, and a plain module for an
    // unregistered one.
    let (intruder, intruder_script) = Fake::new(INTRUDER);
    {
        let errors = Arc::clone(&errors);
        intruder_script.on_start(move |ctx, _| {
            if let Err(error) = ctx.state_publisher::<Temp>() {
                lock(&errors).push((INTRUDER, error));
            }
            Ok(())
        });
    }
    registry.register(intruder, Cfg::on(0)).expect("intruder");
    let (plain, plain_script) = Fake::new(CONSUMER);
    {
        let errors = Arc::clone(&errors);
        plain_script.on_start(move |ctx, _| {
            if let Err(error) = ctx.state_publisher::<Other>() {
                lock(&errors).push((CONSUMER, error));
            }
            Ok(())
        });
    }
    registry.register(plain, Cfg::on(0)).expect("plain");
    registry.start_all().expect("start");

    let errors = lock(&errors).clone();
    let registered = SchemaIdentity {
        provider: PROVIDER,
        name: "temp",
        version: 1,
    };
    assert!(errors.iter().any(|(module, error)| *module == PROVIDER
        && matches!(error, StateError::SchemaMismatch { registered: r, .. } if *r == registered)));
    assert_eq!(
        errors
            .iter()
            .filter(|(m, e)| *m == PROVIDER && matches!(e, StateError::SchemaMismatch { .. }))
            .count(),
        2,
        "both a different version and a different type are mismatches"
    );
    assert!(errors.iter().any(|(module, error)| *module == INTRUDER
        && *error
            == StateError::Unauthorized {
                module: INTRUDER,
                provider: PROVIDER
            }));
    assert!(
        errors.iter().any(|(module, error)| *module == CONSUMER
            && matches!(error, StateError::NotRegistered { .. }))
    );
    for (_, error) in &errors {
        assert!(!error.to_string().is_empty());
    }
}

// --- lifecycle, tombstones and generations -----------------------------------

#[test]
fn disabled_paused_failed_and_recovered_providers_never_read_stale_data() {
    let registry = registry();
    let (provider, script, slot) = provider_module(&registry, Cfg::on(5));
    let (consumer, consumer_script) = Fake::new(CONSUMER);
    let _ = consumer_script;
    let consumer = registry
        .register(consumer.observing(&OBSERVES_PROVIDER), Cfg::on(0))
        .expect("c");
    registry.start_all().expect("start");
    let observer = registry.observe_state(PROVIDER);
    let first = available(&observer.read().state).clone();

    registry.set_enabled(provider, false).expect("disable");
    let tombstone = unavailable(&observer.read().state);
    assert_eq!(tombstone.reason, ended(RuntimeEnd::Disabled));
    assert_eq!(tombstone.generation, Some(first.generation));
    assert_eq!(
        registry.snapshot(consumer).expect("snapshot").status,
        ModuleStatus::Running,
        "observation never affects the consumer's lifecycle"
    );

    registry.set_enabled(provider, true).expect("enable");
    let second = available(&observer.read().state).clone();
    assert!(
        second.generation > first.generation,
        "a restart is a new generation"
    );
    assert_eq!(second.sequence, 1, "the sequence resets with the generation");

    registry.pause(provider).expect("pause");
    assert_eq!(
        unavailable(&observer.read().state).reason,
        ended(RuntimeEnd::Paused)
    );
    registry.resume(provider).expect("resume");
    let third = available(&observer.read().state).clone();
    assert!(third.generation > second.generation);

    // A failing start never exposes the state it published while starting.
    script.on_start(|ctx, _| {
        let publisher = ctx.state_publisher::<Temp>().expect("publisher");
        assert_eq!(
            publisher.publish(1, &Temp { level: 9 }).expect("held"),
            PublishOutcome::Held
        );
        Err(failure(PROVIDER, Operation::Start))
    });
    registry.pause(provider).expect("pause");
    registry.resume(provider).expect_err("start fails");
    assert_eq!(
        unavailable(&observer.read().state).reason,
        ended(RuntimeEnd::Failed)
    );

    // Recovery publishes under a fresh generation again.
    capture_and_publish(&script, &slot);
    registry.set_enabled(provider, true).expect("retry");
    let recovered = available(&observer.read().state).clone();
    assert!(recovered.generation > third.generation);
    assert_eq!(level(&observer.read().state), 5);
    assert_eq!(
        registry.snapshot(consumer).expect("snapshot").status,
        ModuleStatus::Running
    );
}

#[test]
fn a_stopping_runtime_is_tombstoned_before_cleanup_waits() {
    let registry = registry();
    let (provider, script, slot) = provider_module(&registry, Cfg::on(7));
    let checks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let checks = Arc::clone(&checks);
        let slot = Arc::clone(&slot);
        let observer = registry.observe_state(PROVIDER);
        script.on_stop(move |_, _| {
            // Runs inside `Module::stop`, i.e. before workers join.
            let reading = observer.read().state;
            lock(&checks).push(format!("{:?}", unavailable(&reading).reason));
            let late = captured(&slot).publish(1, &Temp { level: 99 });
            lock(&checks).push(format!("{late:?}"));
            Ok(())
        });
    }
    registry.start_all().expect("start");
    registry.set_enabled(provider, false).expect("disable");
    let checks = lock(&checks).clone();
    assert_eq!(checks[0], format!("{:?}", ended(RuntimeEnd::Disabled)));
    assert!(checks[1].contains("Stale"), "{checks:?}");
}

#[test]
fn a_retained_old_handle_cannot_update_or_clear_the_successor() {
    let registry = registry();
    let (provider, _script, slot) = provider_module(&registry, Cfg::on(1));
    registry.start_all().expect("start");
    let old = captured(&slot);
    registry.set_enabled(provider, false).expect("disable");
    registry.set_enabled(provider, true).expect("enable");
    let observer = registry.observe_state(PROVIDER);
    let successor = available(&observer.read().state).clone();
    assert_ne!(old.generation(), successor.generation);

    let version = observer.read().version;
    assert!(matches!(
        old.publish(1, &Temp { level: 42 }),
        Err(StateError::Stale { provider, generation })
            if provider == PROVIDER && generation == old.generation()
    ));
    // The old generation's end arriving late must not clear the successor.
    registry
        .state_hub()
        .cell(PROVIDER)
        .end_generation(old.generation(), RuntimeEnd::Stopped);
    let after = observer.read();
    assert_eq!(
        after.version, version,
        "no write and no wake from the old generation"
    );
    assert_eq!(available(&after.state), &successor);

    // The successor's own handle still works.
    let current = captured(&slot);
    assert_eq!(current.generation(), successor.generation);
    assert_eq!(
        current.publish(1, &Temp { level: 2 }).expect("publish"),
        PublishOutcome::Published { sequence: 2 }
    );
}

#[test]
fn sequences_are_monotonic_within_a_generation() {
    let registry = registry();
    let (_provider, _script, slot) = provider_module(&registry, Cfg::on(0));
    registry.start_all().expect("start");
    let publisher = captured(&slot);
    let observer = registry.observe_state(PROVIDER);
    let mut last = available(&observer.read().state).sequence;
    for level in 1..=5 {
        let outcome = publisher.publish(1, &Temp { level }).expect("publish");
        let PublishOutcome::Published { sequence } = outcome else {
            panic!("a running generation publishes immediately");
        };
        assert!(sequence > last);
        assert_eq!(available(&observer.read().state).sequence, sequence);
        last = sequence;
    }
}

// --- accepted revisions -------------------------------------------------------

/// Publishes `level` labelled with `revision` through the captured publisher.
fn publish_at(slot: &Slot, revision: u64, level: u32) -> Result<PublishOutcome, StateError> {
    captured(slot).publish(revision, &Temp { level })
}

#[test]
fn a_worker_labels_payloads_with_the_revision_it_computed_them_from() {
    let registry = Arc::new(registry());
    let (provider, script, slot) = provider_module(&registry, Cfg::on(1));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    script.on_reconfigure(move |_, _| {
        entered_tx.send(()).expect("test listens");
        lock(&release_rx).recv().expect("test releases");
        Ok(())
    });
    registry.start_all().expect("start");
    let watch = registry.accepted_watch(provider).expect("watch");
    let observer = registry.observe_state(PROVIDER);

    // The worker reads config 1 from the watch and keeps computing with it.
    let worked_from = watch.read().expect("accepted");
    assert_eq!(worked_from.revision, 1);

    let reloading = {
        let registry = Arc::clone(&registry);
        thread::spawn(move || registry.reload(provider, Cfg::on(2)))
    };
    entered_rx.recv_timeout(Duration::from_secs(5)).expect("reconfigure entered");
    // Mid-reconfigure the worker still runs config 1: its payload is
    // published at once, labelled 1, and nothing claims the candidate.
    assert_eq!(
        publish_at(&slot, worked_from.revision, 10).expect("publish"),
        PublishOutcome::Published { sequence: 2 }
    );
    assert_eq!(available(&observer.read().state).revision, 1);
    assert!(
        publish_at(&slot, 2, 11).is_err(),
        "revision 2 is not accepted yet"
    );

    release_tx.send(()).expect("release");
    reloading.join().expect("join").expect("reload");

    // Still computing with config 1 after acceptance: still labelled 1.
    assert_eq!(available(&observer.read().state).revision, 1);
    let _ = publish_at(&slot, worked_from.revision, 12).expect("old config still publishes");
    assert_eq!(available(&observer.read().state).revision, 1);
    assert_eq!(level(&observer.read().state), 12);

    // Only after reading config 2 from the watch does it label payloads 2.
    let adopted = watch.read().expect("accepted");
    assert_eq!(adopted.revision, 2);
    let _ = publish_at(&slot, adopted.revision, 20).expect("publish");
    let envelope = available(&observer.read().state).clone();
    assert_eq!(envelope.revision, 2);
    assert!(watch.read().expect("accepted").revision >= envelope.revision);
    assert_eq!(level(&observer.read().state), 20);
}

#[test]
fn publishing_an_unaccepted_revision_is_rejected() {
    let registry = registry();
    let (provider, _script, slot) = provider_module(&registry, Cfg::on(1));
    registry.start_all().expect("start");
    let observer = registry.observe_state(PROVIDER);
    let before = observer.read();

    let error = publish_at(&slot, 2, 5).expect_err("revision 2 is not accepted");
    assert!(matches!(error, StateError::RevisionNotAccepted {
        revision: 2,
        accepted: 1,
        ..
    }));
    assert!(error.to_string().contains("revision 2"));
    assert!(publish_at(&slot, u64::MAX, 5).is_err());
    assert_eq!(observer.read(), before, "a rejected publication changes nothing");

    registry.reload(provider, Cfg::on(2)).expect("accept");
    let _ = publish_at(&slot, 2, 5).expect("accepted now");
    assert_eq!(available(&observer.read().state).revision, 2);
}

#[test]
fn an_older_revision_never_overwrites_a_newer_one() {
    let registry = registry();
    let (provider, script, slot) = provider_module(&registry, Cfg::on(1));
    registry.start_all().expect("start");
    registry.reload(provider, Cfg::on(2)).expect("accept");
    let observer = registry.observe_state(PROVIDER);

    // The generation started at revision 1, so 1 is allowed until 2 commits.
    let _ = publish_at(&slot, 1, 1).expect("older but not yet superseded");
    let _ = publish_at(&slot, 2, 2).expect("newer");
    let before = observer.read();
    let error = publish_at(&slot, 1, 3).expect_err("stale revision");
    assert!(matches!(error, StateError::RevisionStale {
        revision: 1,
        floor: 2,
        ..
    }));
    assert!(!error.to_string().is_empty());
    assert_eq!(observer.read(), before);
    let _ = publish_at(&slot, 2, 4).expect("the same revision keeps publishing");

    // A restart at revision 2 cannot publish below its start revision.
    {
        let slot = Arc::clone(&slot);
        script.on_start(move |ctx, _| {
            let publisher = ctx.state_publisher::<Temp>().expect("registered publisher");
            let _ = publisher.publish(2, &Temp { level: 0 }).expect("publish");
            *lock(&slot) = Some(publisher);
            Ok(())
        });
    }
    registry.set_enabled(provider, false).expect("disable");
    registry.set_enabled(provider, true).expect("enable");
    assert_eq!(available(&observer.read().state).revision, 2);
    assert!(matches!(
        publish_at(&slot, 1, 9),
        Err(StateError::RevisionStale { revision: 1, floor: 2, .. })
    ));
}

#[test]
fn a_rejected_reconfigure_changes_nothing_and_keeps_unrelated_publications() {
    let registry = registry();
    let (provider, script, slot) = provider_module(&registry, Cfg::on(1));
    {
        let slot = Arc::clone(&slot);
        script.on_reconfigure(move |_, cfg| {
            // Unrelated state computed from the still-accepted config 1.
            let outcome = publish_at(&slot, 1, 77).expect("publish");
            assert!(matches!(outcome, PublishOutcome::Published { .. }));
            if cfg.value == 99 {
                Err(failure(PROVIDER, Operation::Reconfigure))
            } else {
                Ok(())
            }
        });
    }
    registry.start_all().expect("start");
    let observer = registry.observe_state(PROVIDER);
    let watch = registry.accepted_watch(provider).expect("watch");

    registry.reload(provider, Cfg::on(99)).expect_err("reconfigure fails");
    assert_eq!(
        watch.read().expect("accepted").revision,
        1,
        "the old revision stays accepted"
    );
    let after = observer.read();
    assert_eq!(available(&after.state).revision, 1);
    assert_eq!(level(&after.state), 77, "the unrelated publication was kept");
    assert!(
        publish_at(&slot, 2, 1).is_err(),
        "the rejected candidate was never accepted"
    );

    // An invalid candidate never reaches the module at all.
    let version = observer.read().version;
    registry.reload(provider, Cfg::on(1000)).expect_err("invalid");
    assert_eq!(observer.read().version, version);

    registry.reload(provider, Cfg::on(2)).expect("accepted");
    let _ = publish_at(&slot, 2, 5).expect("publish");
    assert_eq!(available(&observer.read().state).revision, 2);
}

#[test]
fn the_cell_learns_a_revision_before_the_watch_shows_it() {
    let registry = registry();
    let (provider, _script, slot) = provider_module(&registry, Cfg::on(1));
    registry.start_all().expect("start");
    let watch = registry.accepted_watch(provider).expect("watch");
    let publisher = captured(&slot);
    let observed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let observed = Arc::clone(&observed);
        let watch = watch.clone();
        registry.state_hub().cell(PROVIDER).set_after_advance(Some(Arc::new(move || {
            // Between the cell advancing and the watch publishing.
            let shown = watch.read().expect("accepted").revision;
            let first = publisher.publish(2, &Temp { level: 2 });
            let beyond = publisher.publish(3, &Temp { level: 3 });
            lock(&observed).push(format!("{shown} {} {}", first.is_ok(), beyond.is_err()));
        })));
    }
    registry.reload(provider, Cfg::on(2)).expect("accept");
    assert_eq!(*lock(&observed), vec!["1 true true".to_owned()]);
    assert_eq!(watch.read().expect("accepted").revision, 2);
}

#[test]
fn a_b_a_transitions_carry_strictly_increasing_revisions() {
    let registry = registry();
    let (provider, _script, slot) = provider_module(&registry, Cfg::on(1));
    registry.start_all().expect("start");
    let watch = registry.accepted_watch(provider).expect("watch");
    let observer = registry.observe_state(PROVIDER);
    let mut seen = vec![available(&observer.read().state).revision];
    for value in [2, 1] {
        registry.reload(provider, Cfg::on(value)).expect("reload");
        let _ =
            publish_at(&slot, watch.read().expect("accepted").revision, value).expect("publish");
        seen.push(available(&observer.read().state).revision);
        assert_eq!(level(&observer.read().state), value);
    }
    assert_eq!(
        seen,
        vec![1, 2, 3],
        "A-B-A is three accepted revisions, never a repeat"
    );
}

#[test]
fn a_provider_that_never_started_reads_the_registry_status() {
    let registry = registry();
    // Registered disabled: reads as disabled, not as merely not started.
    let (provider, _script, _slot) = provider_module(&registry, Cfg { enabled: false, value: 1 });
    let observer = registry.observe_state(PROVIDER);
    let reason = || unavailable(&observer.read().state);
    assert_eq!(reason().reason, ended(RuntimeEnd::Disabled));
    assert_eq!(reason().generation, None);

    registry.start_all().expect("start");
    assert_eq!(reason().reason, ended(RuntimeEnd::Disabled));
    registry.set_enabled(provider, true).expect("enable");
    assert!(matches!(observer.read().state, StateReading::Available(_)));
}

#[test]
fn a_paused_or_blocked_never_started_provider_reads_why() {
    let registry = registry();
    let (provider, _script, _slot) = provider_module(&registry, Cfg::on(1));
    let observer = registry.observe_state(PROVIDER);
    assert_eq!(
        unavailable(&observer.read().state).reason,
        StateUnavailableReason::NotStarted
    );
    registry.pause(provider).expect("pause");
    assert_eq!(
        unavailable(&observer.read().state).reason,
        ended(RuntimeEnd::Paused)
    );
    registry.resume(provider).expect("resume");
    // The runtime is up and has since ended: the tombstone keeps its generation.
    let running = available(&observer.read().state).generation;
    registry.pause(provider).expect("pause again");
    let tombstone = unavailable(&observer.read().state);
    assert_eq!(tombstone.generation, Some(running));
    assert_eq!(tombstone.reason, ended(RuntimeEnd::Paused));
}

#[test]
fn accepted_config_followed_by_a_start_failure_exposes_no_available_state() {
    let registry = registry();
    let (provider, script, _slot) = provider_module(&registry, Cfg { enabled: false, value: 1 });
    let observer = registry.observe_state(PROVIDER);
    registry.start_all().expect("start");
    script.on_start(|ctx, _| {
        let publisher = ctx.state_publisher::<Temp>().expect("publisher");
        let _ = publisher.publish(2, &Temp { level: 13 }).expect("held");
        Err(failure(PROVIDER, Operation::Start))
    });
    let report = registry.reload(provider, Cfg::on(13)).expect("accepted");
    assert!(report.start_error.is_some());
    assert_eq!(
        registry.accepted_watch(provider).expect("watch").read().expect("c").revision,
        2
    );
    assert_eq!(
        unavailable(&observer.read().state).reason,
        ended(RuntimeEnd::Failed)
    );
}

// --- notification, waiting and prompt reads -----------------------------------

fn one_generation_cell() -> (Arc<StateCell>, StateObserver, StatePublisher<Temp>) {
    let hub = StateHub::default();
    hub.register(RegisteredSchema::of::<Temp>(PROVIDER)).expect("register");
    let cell = hub.cell(PROVIDER);
    cell.mark_registered();
    let scope = GenerationScope::new(1);
    let binding = GenerationBinding::begin(&cell, &scope, 1);
    binding.activate();
    let publisher = hub.publisher::<Temp>(PROVIDER, &scope).expect("publisher");
    let observer = StateObserver::new(Arc::clone(&cell));
    (cell, observer, publisher)
}

#[test]
fn a_publication_between_the_check_and_the_sleep_is_not_lost() {
    let (cell, observer, publisher) = one_generation_cell();
    let seen = observer.read().version;
    let (entered_tx, entered_rx) = mpsc::channel();
    // The hook runs after the check and before the sleep, with the cell lock
    // held; a publisher started now can only proceed once the wait released it.
    cell.set_before_wait(Some(Arc::new(move || {
        let _ = entered_tx.send(());
    })));
    let waiter = thread::spawn(move || {
        observer.wait_for_change(seen, Duration::from_secs(30), &CancellationToken::new())
    });
    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the wait reached the boundary");
    thread::spawn(move || publisher.publish(1, &Temp { level: 4 }).map(|_| ()));
    // A lost wake would block for the whole timeout and then report TimedOut.
    let StateWait::Changed(snapshot) = waiter.join().expect("join") else {
        panic!("expected the publication");
    };
    assert_eq!(level(&snapshot.state), 4);
}

#[test]
fn an_overflowing_timeout_waits_unbounded_instead_of_panicking() {
    let (cell, observer, publisher) = one_generation_cell();
    let seen = observer.read().version;
    let (entered_tx, entered_rx) = mpsc::channel();
    cell.set_before_wait(Some(Arc::new(move || {
        let _ = entered_tx.send(());
    })));
    let waiter = thread::spawn(move || {
        observer.wait_for_change(seen, Duration::MAX, &CancellationToken::new())
    });
    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the wait reached the boundary");
    thread::spawn(move || publisher.publish(1, &Temp { level: 1 }).map(|_| ()));
    assert!(matches!(waiter.join().expect("join"), StateWait::Changed(_)));
}

#[test]
fn cancellation_wakes_a_wait_in_progress() {
    let (cell, observer, _publisher) = one_generation_cell();
    let seen = observer.read().version;
    let (entered_tx, entered_rx) = mpsc::channel();
    cell.set_before_wait(Some(Arc::new(move || {
        let _ = entered_tx.send(());
    })));
    let token = CancellationToken::new();
    let waiter = {
        let token = token.clone();
        thread::spawn(move || observer.wait_for_change(seen, Duration::MAX, &token))
    };
    entered_rx.recv_timeout(Duration::from_secs(5)).expect("the wait started");
    token.cancel();
    assert!(matches!(waiter.join().expect("join"), StateWait::Cancelled));
}

#[test]
fn wait_precedence_is_cancel_then_close_then_change_then_timeout() {
    let (cell, observer, publisher) = one_generation_cell();
    let seen = observer.read().version;
    let _ = publisher.publish(1, &Temp { level: 1 }).expect("publish");
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        observer.wait_for_change(seen, Duration::from_secs(5), &cancelled),
        StateWait::Cancelled
    ));
    assert!(matches!(
        observer.wait_for_change(seen, Duration::from_secs(5), &CancellationToken::new()),
        StateWait::Changed(_)
    ));
    // Nothing newer than the latest version: the bounded wait times out.
    let latest = observer.read().version;
    assert!(matches!(
        observer.wait_for_change(latest, Duration::from_millis(5), &CancellationToken::new()),
        StateWait::TimedOut
    ));
    cell.close();
    assert!(matches!(
        observer.wait_for_change(seen, Duration::from_secs(5), &CancellationToken::new()),
        StateWait::Closed
    ));
}

#[test]
fn a_slow_consumer_sees_the_coalesced_latest_state() {
    let (_cell, observer, publisher) = one_generation_cell();
    let seen = observer.read().version;
    for level in 1..=50 {
        let _ = publisher.publish(1, &Temp { level }).expect("publish");
    }
    let StateWait::Changed(snapshot) =
        observer.wait_for_change(seen, Duration::from_secs(5), &CancellationToken::new())
    else {
        panic!("expected a change");
    };
    assert_eq!(
        level(&snapshot.state),
        50,
        "one wake, the latest value, no per-sample queue"
    );
    assert_eq!(
        available(&snapshot.state).sequence,
        50,
        "activation published nothing"
    );
}

#[test]
fn shutdown_wakes_waiters_with_closed_and_reads_keep_the_last_value() {
    let registry = Arc::new(registry());
    let (_provider, _script, _slot) = provider_module(&registry, Cfg::on(8));
    registry.start_all().expect("start");
    let observer = registry.observe_state(PROVIDER);
    let seen = observer.read().version;
    let (entered_tx, entered_rx) = mpsc::channel();
    registry.state_hub().cell(PROVIDER).set_before_wait(Some(Arc::new(move || {
        let _ = entered_tx.send(());
    })));
    let waiter = {
        let observer = observer.clone();
        thread::spawn(move || {
            observer.wait_for_change(seen, Duration::MAX, &CancellationToken::new())
        })
    };
    entered_rx.recv_timeout(Duration::from_secs(5)).expect("the wait started");
    registry.invalidate();
    assert!(matches!(waiter.join().expect("join"), StateWait::Closed));
    assert_eq!(level(&observer.read().state), 8);
}

#[test]
fn reads_stay_prompt_while_the_provider_is_stalled_in_module_code() {
    let registry = Arc::new(registry());
    let (provider, script, _slot) = provider_module(&registry, Cfg::on(1));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    script.on_reconfigure(move |_, _| {
        entered_tx.send(()).expect("test listens");
        lock(&release_rx).recv().expect("test releases");
        Ok(())
    });
    registry.start_all().expect("start");
    let reloading = {
        let registry = Arc::clone(&registry);
        thread::spawn(move || registry.reload(provider, Cfg::on(2)))
    };
    entered_rx.recv_timeout(Duration::from_secs(5)).expect("stalled in reconfigure");

    let (done_tx, done_rx) = mpsc::channel();
    {
        let registry = Arc::clone(&registry);
        thread::spawn(move || {
            let observer = registry.observe_state(PROVIDER);
            let reading = observer.read();
            let _ = done_tx.send(level(&reading.state));
        });
    }
    assert_eq!(
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("reads do not wait on module code"),
        1
    );
    release_tx.send(()).expect("release");
    reloading.join().expect("join").expect("reload");
}

#[test]
fn the_observer_is_read_only_and_the_publisher_is_typed() {
    // `StateObserver` exposes only `provider`, `read` and `wait_for_change`; the
    // compile-time half of the contract is the `compile_fail` doctest on
    // `StatePublisher`. This checks that observers are cheap shared views.
    let (_cell, observer, publisher) = one_generation_cell();
    let clone = observer.clone();
    let _ = publisher.publish(1, &Temp { level: 6 }).expect("publish");
    assert_eq!(observer.read(), clone.read());
    assert_eq!(observer.provider(), PROVIDER);
}

#[test]
fn a_tombstone_keeps_its_generation_while_its_reason_follows_the_status() {
    let registry = registry();
    let (provider, script, _slot) = provider_module(&registry, Cfg::on(5));
    registry.start_all().expect("start");
    let observer = registry.observe_state(PROVIDER);
    let generation = available(&observer.read().state).generation;

    script.on_start(|_, _| Err(failure(PROVIDER, Operation::Start)));
    registry.pause(provider).expect("pause");
    let paused = observer.read();
    registry.resume(provider).expect_err("start fails");
    let failed = observer.read();
    assert_eq!(unavailable(&failed.state).reason, ended(RuntimeEnd::Failed));
    // Exactly Starting, then Ended(Failed): the failed start's cleanup never
    // republishes the pre-start Paused status as the new generation's reason.
    assert_eq!(failed.version - paused.version, 2);
    let failed_generation = unavailable(&failed.state).generation;
    assert!(failed_generation.is_some_and(|g| g > generation));

    registry.set_enabled(provider, false).expect("disable");
    let disabled = observer.read();
    let tombstone = unavailable(&disabled.state);
    assert_eq!(tombstone.reason, ended(RuntimeEnd::Disabled));
    assert!(tombstone.generation.is_some());
    assert!(disabled.version > failed.version);
    assert_ne!(tombstone.reason, StateUnavailableReason::NotStarted);

    // Re-publishing the same status changes nothing.
    registry.state_hub().cell(PROVIDER).note_status(Some(RuntimeEnd::Disabled));
    assert_eq!(observer.read().version, disabled.version);
}

#[test]
fn a_cell_created_after_shutdown_wakes_waiters_with_closed() {
    let registry = registry();
    registry.invalidate();
    let observer = registry.observe_state(GHOST);
    let seen = observer.read().version;
    let wait = observer.wait_for_change(seen, Duration::from_secs(30), &CancellationToken::new());
    assert!(matches!(wait, StateWait::Closed));
}
