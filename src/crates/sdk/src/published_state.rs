//! Provider-published state and read-only observation.
//!
//! A provider module registers one typed schema contract with the host, then
//! publishes values of that type through a [`StatePublisher`] bound to one
//! runtime generation. The host keeps one authoritative latest-state cell per
//! provider; consumers read it through a [`StateObserver`] that cannot mutate
//! it. The SDK stays independent of every module crate: the cell stores the
//! payload as serialized JSON plus the schema identity, and the application
//! (which may depend on the provider crate) decodes it with the provider's type
//! through [`StateEnvelope::decode`].
//!
//! # Contract
//!
//! * Identity is provider [`ModuleId`] + schema name + version. A provider has
//!   at most one schema. Registering a second one, publishing a different type
//!   than the registered one, or publishing from a module that is not the
//!   registered provider is rejected with a deterministic [`StateError`].
//! * An envelope carries the provider, the runtime generation (new for every
//!   start), the accepted configuration revision that runtime is running with,
//!   a per-generation sequence that restarts at one, the schema identity and
//!   the payload. Payload-internal fields form one atomic snapshot. Host
//!   diagnostics sampled independently of the payload are not claimed atomic
//!   with it.
//! * The revision is provider-labelled: the provider passes the accepted
//!   revision its payload was derived from (typically
//!   [`crate::AcceptedConfig::revision`] read from its
//!   [`crate::AcceptedWatch`]) to [`StatePublisher::publish`], and the envelope
//!   carries exactly that. A revision below the one the generation started
//!   with or below the last one it committed is rejected as stale, so a
//!   payload from an older configuration never overwrites a newer one; a
//!   revision the registry has not accepted yet is rejected, so no payload can
//!   claim a configuration that does not exist. The registry advances the
//!   cell's accepted revision before the accepted-configuration cell shows it.
//!   A provider that only labels payloads with revisions it read from its
//!   [`crate::AcceptedWatch`] therefore never runs ahead of the watch, and no
//!   envelope can carry a revision that has not been accepted. There is no
//!   reconfigure hold: a failed reconfigure changes nothing here. Only
//!   publications made during `Module::start` are held (latest wins) and
//!   released just before the module reads as running; a start that fails
//!   never exposes an available state.
//! * A cell is updated before readers are woken. Notification is a coalesced
//!   hint: a reader that waited rereads the latest value, never a queue of
//!   samples. Readers take no registry, module or lifecycle lock and never run
//!   provider code.
//! * On stop, pause, disable or failure the registry invalidates the generation
//!   before any cleanup wait, replaces the value with an explicit unavailable
//!   tombstone and wakes readers. Writes from the ended generation's handles are
//!   rejected, and an old handle can never touch a successor generation.
//! * Observation is declared in [`crate::ModuleDescriptor::observes`], is not a
//!   hard `requires`, and never affects the observer's lifecycle. An observed
//!   provider that is unknown or has no schema reads [`StateReading::Absent`].
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//!
//! use serde::{Deserialize, Serialize};
//! use stache_sdk::{
//!     DiscardLogger, EventBus, MainThreadExecutor, ModuleId, PublishedState, Registry,
//!     StateReading, StateSchema, ThreadError, UnavailableWindows,
//! };
//!
//! // Lives in the provider's crate; consumers never import it.
//! #[derive(Serialize, Deserialize)]
//! struct Level {
//!     value: u32,
//! }
//!
//! impl PublishedState for Level {
//!     const SCHEMA: StateSchema = StateSchema { name: "level", version: 1 };
//! }
//!
//! # struct Main;
//! # impl MainThreadExecutor for Main {
//! #     fn is_main_thread(&self) -> bool { true }
//! #     fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
//! #         job();
//! #         Ok(())
//! #     }
//! # }
//! # struct Grant;
//! # impl stache_sdk::PermissionProvider for Grant {
//! #     fn status(&self, _: stache_sdk::Permission) -> stache_sdk::PermissionStatus {
//! #         stache_sdk::PermissionStatus::Granted
//! #     }
//! #     fn request(
//! #         &self,
//! #         permission: stache_sdk::Permission,
//! #     ) -> Result<stache_sdk::PermissionStatus, stache_sdk::PermissionError> {
//! #         Ok(self.status(permission))
//! #     }
//! # }
//! // The app registers the contract before any module registers.
//! let registry = Registry::new(
//!     Arc::new(DiscardLogger),
//!     Arc::new(EventBus::new()),
//!     Arc::new(Main),
//!     Arc::new(Grant),
//!     Arc::new(UnavailableWindows),
//! )
//! .with_state_schema::<Level>(ModuleId::new("meter"))
//! .expect("first schema for this provider");
//!
//! // A provider that is not running reads as absent until its module registers.
//! let observer = registry.observe_state(ModuleId::new("meter"));
//! assert_eq!(observer.read().state, StateReading::Absent);
//! // Inside `Module::start`, the provider calls `ctx.state_publisher::<Level>()?`
//! // and `publisher.publish(accepted_revision, &Level { value: 3 })?`; a consumer that lists
//! // `ModuleId::new("meter")` in `ModuleDescriptor::observes` calls
//! // `ctx.observe_state(ModuleId::new("meter"))?`.
//! ```

use std::any::{TypeId, type_name};
use std::collections::HashMap;
use std::error::Error as StdError;
use std::fmt;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::generation::{GenerationScope, RuntimeEnd};
use crate::lock;
use crate::module::ModuleId;
use crate::threading::CancellationToken;

/// A schema name and version, declared by the Rust type that is published.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StateSchema {
    /// A stable schema name, unique within its provider.
    pub name: &'static str,
    /// The schema version. Incompatible payload changes bump it.
    pub version: u32,
}

/// A typed payload a provider publishes.
///
/// The type is the contract: it owns the schema identity and is the only type
/// the SDK will accept for its provider.
pub trait PublishedState: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// The schema identity of this payload type.
    const SCHEMA: StateSchema;
}

/// Provider + schema name + version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SchemaIdentity {
    /// The provider module.
    pub provider: ModuleId,
    /// The schema name.
    pub name: &'static str,
    /// The schema version.
    pub version: u32,
}

impl fmt::Display for SchemaIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}@{}", self.provider, self.name, self.version)
    }
}

/// One published value with the facts that make it interpretable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateEnvelope {
    /// The provider that published it.
    pub provider: ModuleId,
    /// The runtime generation, unique per start of one registry.
    pub generation: u64,
    /// The accepted configuration revision that runtime was running with.
    pub revision: u64,
    /// A sequence that starts at one for every generation and only grows.
    pub sequence: u64,
    /// The registered schema the payload was serialized with.
    pub schema: SchemaIdentity,
    /// The serialized payload.
    pub payload: Arc<Value>,
}

impl StateEnvelope {
    /// Decodes the payload with the provider's own type.
    ///
    /// # Errors
    /// Returns [`StateError::DecodeSchema`] when the envelope was not published
    /// by `provider` or `T` is not the schema it carries, or
    /// [`StateError::Decode`] when the payload does not deserialize.
    pub fn decode<T: PublishedState>(&self, provider: ModuleId) -> Result<T, StateError> {
        if self.schema.provider != provider
            || T::SCHEMA.name != self.schema.name
            || T::SCHEMA.version != self.schema.version
        {
            return Err(StateError::DecodeSchema {
                envelope: self.schema,
                requested: SchemaIdentity {
                    provider,
                    name: T::SCHEMA.name,
                    version: T::SCHEMA.version,
                },
            });
        }
        T::deserialize(self.payload.as_ref()).map_err(|error| StateError::Decode {
            schema: self.schema,
            message: error.to_string(),
        })
    }
}

/// Why a provider has no available state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StateUnavailableReason {
    /// The provider is registered and has not started yet.
    NotStarted,
    /// A runtime is starting; nothing is visible until the start succeeds.
    Starting,
    /// The runtime started and has not published yet.
    NotPublished,
    /// The runtime generation ended.
    Ended(RuntimeEnd),
}

/// An explicit tombstone: the provider is known but has no active state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StateUnavailable {
    /// Why there is no state.
    pub reason: StateUnavailableReason,
    /// The generation the reason refers to, when there was one.
    pub generation: Option<u64>,
}

/// What a reader sees for one provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StateReading {
    /// No provider or no schema is registered under this identifier.
    Absent,
    /// The provider is known but not publishing; never stale active data.
    Unavailable(StateUnavailable),
    /// The latest value of the running generation.
    Available(StateEnvelope),
}

/// A reading with the cell version it was taken at.
///
/// `version` increases on every change of the cell, tombstones included. Pass it
/// to [`StateObserver::wait_for_change`] to wait for anything newer. It is a
/// per-provider counter, unrelated to the config revision or the sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateSnapshot {
    /// The cell version.
    pub version: u64,
    /// The state at that version.
    pub state: StateReading,
}

/// The outcome of [`StateObserver::wait_for_change`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StateWait {
    /// The cell moved past the seen version; this is the latest reading.
    Changed(StateSnapshot),
    /// The timeout elapsed without a change, cancellation or close.
    TimedOut,
    /// The supplied cancellation token was cancelled.
    Cancelled,
    /// The registry was invalidated for shutdown.
    Closed,
}

/// What a publication did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum PublishOutcome {
    /// The value became the latest state with this sequence.
    Published {
        /// The assigned per-generation sequence.
        sequence: u64,
    },
    /// `Module::start` is still running. The latest held value is released when
    /// the start succeeds, or dropped if the generation ends first.
    Held,
}

/// A deterministic published-state failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StateError {
    /// The module has no registered schema and none is registered for the type.
    NotRegistered {
        /// The module that asked.
        module: ModuleId,
        /// The Rust type it asked for.
        requested: &'static str,
    },
    /// The type is registered for a different provider.
    Unauthorized {
        /// The module that asked.
        module: ModuleId,
        /// The provider the type is registered for.
        provider: ModuleId,
    },
    /// The module's registered schema belongs to a different type.
    SchemaMismatch {
        /// The module that asked.
        module: ModuleId,
        /// The Rust type it asked for.
        requested: &'static str,
        /// The schema registered for the module.
        registered: SchemaIdentity,
    },
    /// The provider is not listed in the consumer's `observes`.
    NotObserved {
        /// The consumer.
        module: ModuleId,
        /// The provider it tried to observe.
        provider: ModuleId,
    },
    /// The generation that owns the handle has ended or been replaced.
    Stale {
        /// The provider.
        provider: ModuleId,
        /// The generation the handle was bound to.
        generation: u64,
    },
    /// The payload was derived from an older accepted revision than the
    /// generation started with or already committed.
    RevisionStale {
        /// The provider.
        provider: ModuleId,
        /// The generation of the handle.
        generation: u64,
        /// The revision the payload claimed.
        revision: u64,
        /// The lowest revision the generation still accepts.
        floor: u64,
    },
    /// The payload claimed a revision the registry has not accepted yet.
    RevisionNotAccepted {
        /// The provider.
        provider: ModuleId,
        /// The generation of the handle.
        generation: u64,
        /// The revision the payload claimed.
        revision: u64,
        /// The latest revision the registry accepted for the runtime.
        accepted: u64,
    },
    /// The value did not serialize.
    Serialize {
        /// The schema being published.
        schema: SchemaIdentity,
        /// The serializer's message.
        message: String,
    },
    /// The envelope carries a different schema than the requested type.
    DecodeSchema {
        /// The schema in the envelope.
        envelope: SchemaIdentity,
        /// The provider and schema the caller asked for.
        requested: SchemaIdentity,
    },
    /// The payload did not deserialize into the requested type.
    Decode {
        /// The envelope schema.
        schema: SchemaIdentity,
        /// The deserializer's message.
        message: String,
    },
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRegistered { module, requested } => {
                write!(
                    f,
                    "no state schema is registered for `{module}` and `{requested}`"
                )
            }
            Self::Unauthorized { module, provider } => write!(
                f,
                "module `{module}` may not publish a schema registered for `{provider}`"
            ),
            Self::SchemaMismatch { module, requested, registered } => write!(
                f,
                "module `{module}` registered schema {registered}, not `{requested}`"
            ),
            Self::NotObserved { module, provider } => {
                write!(
                    f,
                    "module `{module}` does not declare that it observes `{provider}`"
                )
            }
            Self::Stale { provider, generation } => write!(
                f,
                "generation {generation} of `{provider}` ended; its publisher is no longer valid"
            ),
            Self::Serialize { schema, message } => {
                write!(f, "state {schema} did not serialize: {message}")
            }
            Self::DecodeSchema { envelope, requested } => {
                write!(f, "envelope carries {envelope}, not {requested}")
            }
            Self::RevisionStale {
                provider,
                generation,
                revision,
                floor,
            } => write!(
                f,
                "`{provider}` generation {generation} rejected revision {revision}: \
                 revision {floor} was already reached"
            ),
            Self::RevisionNotAccepted {
                provider,
                generation,
                revision,
                accepted,
            } => write!(
                f,
                "`{provider}` generation {generation} rejected revision {revision}: \
                 only revision {accepted} is accepted"
            ),
            Self::Decode { schema, message } => {
                write!(f, "state {schema} did not decode: {message}")
            }
        }
    }
}

impl StdError for StateError {}

/// The registered contract for one provider.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RegisteredSchema {
    provider: ModuleId,
    type_id: TypeId,
    schema: StateSchema,
}

impl RegisteredSchema {
    const fn of<T: PublishedState>(provider: ModuleId) -> Self {
        Self {
            provider,
            type_id: TypeId::of::<T>(),
            schema: T::SCHEMA,
        }
    }

    pub(crate) const fn provider(&self) -> ModuleId { self.provider }

    const fn identity(&self) -> SchemaIdentity {
        SchemaIdentity {
            provider: self.provider,
            name: self.schema.name,
            version: self.schema.version,
        }
    }
}

/// A set of provider schema contracts, applied to a registry before any module
/// is registered with [`crate::Registry::with_state_schemas`].
///
/// The application builds this from the owning crates' types; the SDK never
/// imports them. Duplicate providers are rejected when the set is applied.
#[derive(Clone, Debug, Default)]
pub struct StateSchemas {
    entries: Vec<RegisteredSchema>,
}

impl StateSchemas {
    /// Creates an empty set.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Adds the contract `T` for `provider`.
    #[must_use]
    pub fn with<T: PublishedState>(mut self, provider: ModuleId) -> Self {
        self.entries.push(RegisteredSchema::of::<T>(provider));
        self
    }

    /// The number of contracts.
    #[must_use]
    pub const fn len(&self) -> usize { self.entries.len() }

    /// Whether the set is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool { self.entries.is_empty() }

    pub(crate) fn entries(&self) -> &[RegisteredSchema] { &self.entries }
}

struct Active {
    generation: u64,
    /// The accepted revision the generation started with.
    start_revision: u64,
    /// The latest revision the registry accepted for this runtime.
    accepted: u64,
    /// The highest revision committed or staged by this generation.
    last_revision: u64,
    sequence: u64,
    /// Held only while `Module::start` runs.
    held: bool,
    staged: Option<(u64, Arc<Value>)>,
}

impl Active {
    fn floor(&self) -> u64 { self.start_revision.max(self.last_revision) }
}

struct CellInner {
    schema: Option<RegisteredSchema>,
    version: u64,
    view: StateReading,
    active: Option<Active>,
    closed: bool,
}

impl CellInner {
    fn bump(&mut self, view: StateReading) {
        self.view = view;
        self.version += 1;
    }

    /// Makes `payload` the latest state of the active generation.
    fn commit(&mut self, provider: ModuleId, revision: u64, payload: Arc<Value>) {
        let (Some(schema), Some(active)) = (self.schema, self.active.as_mut()) else {
            return;
        };
        active.sequence += 1;
        active.last_revision = active.last_revision.max(revision);
        let envelope = StateEnvelope {
            provider,
            generation: active.generation,
            revision,
            sequence: active.sequence,
            schema: schema.identity(),
            payload,
        };
        self.bump(StateReading::Available(envelope));
    }
}

#[cfg(test)]
type Hook = Arc<dyn Fn() + Send + Sync>;

/// The authoritative latest-state cell of one provider.
pub(crate) struct StateCell {
    provider: ModuleId,
    inner: Mutex<CellInner>,
    changed: Condvar,
    #[cfg(test)]
    before_wait: Mutex<Option<Hook>>,
    #[cfg(test)]
    after_advance: Mutex<Option<Hook>>,
}

impl StateCell {
    const fn new(provider: ModuleId) -> Self {
        Self {
            provider,
            inner: Mutex::new(CellInner {
                schema: None,
                version: 0,
                view: StateReading::Absent,
                active: None,
                closed: false,
            }),
            changed: Condvar::new(),
            #[cfg(test)]
            before_wait: Mutex::new(None),
            #[cfg(test)]
            after_advance: Mutex::new(None),
        }
    }

    fn schema(&self) -> Option<RegisteredSchema> { lock(&self.inner).schema }

    /// The provider module was registered: a known provider with a schema reads
    /// unavailable instead of absent.
    pub(crate) fn mark_registered(&self) {
        {
            let mut inner = lock(&self.inner);
            if inner.schema.is_none() || inner.active.is_some() {
                return;
            }
            inner.bump(StateReading::Unavailable(StateUnavailable {
                reason: StateUnavailableReason::NotStarted,
                generation: None,
            }));
        }
        self.changed.notify_all();
    }

    /// Reflects the registry status of a provider that never started, so a
    /// disabled, paused or blocked provider does not read as merely not started.
    /// A provider that has had a generation keeps that generation's tombstone,
    /// and its reason follows later non-running registry statuses.
    pub(crate) fn note_status(&self, end: Option<RuntimeEnd>) {
        let reason = end.map_or(StateUnavailableReason::NotStarted, |end| {
            if end == RuntimeEnd::Stopped {
                StateUnavailableReason::NotStarted
            } else {
                StateUnavailableReason::Ended(end)
            }
        });
        {
            let mut inner = lock(&self.inner);
            if inner.schema.is_none() || inner.active.is_some() {
                return;
            }
            let StateReading::Unavailable(current) = &inner.view else {
                return;
            };
            let generation = current.generation;
            // A running or stopped status says nothing about an ended
            // generation, so its tombstone reason stands.
            if current.reason == reason || (generation.is_some() && end.is_none()) {
                return;
            }
            inner.bump(StateReading::Unavailable(StateUnavailable {
                reason,
                generation,
            }));
        }
        self.changed.notify_all();
    }

    /// Binds a new runtime generation and supersedes whatever came before.
    ///
    /// Publications are held until [`StateCell::release`].
    pub(crate) fn begin_generation(&self, generation: u64, revision: u64) {
        {
            let mut inner = lock(&self.inner);
            inner.active = Some(Active {
                generation,
                start_revision: revision,
                accepted: revision,
                last_revision: revision,
                sequence: 0,
                held: true,
                staged: None,
            });
            inner.bump(StateReading::Unavailable(StateUnavailable {
                reason: StateUnavailableReason::Starting,
                generation: Some(generation),
            }));
        }
        self.changed.notify_all();
    }

    /// Records that the registry accepted `revision` for the active runtime.
    ///
    /// The registry calls this BEFORE the accepted-configuration cell shows the
    /// revision. A publication at `revision` is therefore only possible for a
    /// provider that read it from the watch or otherwise learned it after
    /// acceptance. A provider that only labels payloads with revisions it read
    /// from the watch never runs ahead of it, and no envelope can carry a
    /// revision that has not been accepted. Between this call and the watch
    /// publication the cell may already accept a revision the watch does not
    /// show yet.
    pub(crate) fn advance_accepted(&self, revision: u64) {
        {
            let mut inner = lock(&self.inner);
            if let Some(active) = inner.active.as_mut() {
                active.accepted = active.accepted.max(revision);
            }
        }
        #[cfg(test)]
        {
            let hook = lock(&self.after_advance).take();
            if let Some(hook) = hook {
                hook();
            }
        }
    }

    /// The start succeeded: releases the held publication.
    ///
    /// A generation that published nothing reads
    /// [`StateUnavailableReason::NotPublished`] from then on.
    pub(crate) fn release(&self, generation: u64) {
        let changed = {
            let mut inner = lock(&self.inner);
            let provider = self.provider;
            let Some(active) = inner.active.as_mut().filter(|a| a.generation == generation) else {
                return;
            };
            active.held = false;
            if let Some((revision, payload)) = active.staged.take() {
                inner.commit(provider, revision, payload);
            } else {
                inner.bump(StateReading::Unavailable(StateUnavailable {
                    reason: StateUnavailableReason::NotPublished,
                    generation: Some(generation),
                }));
            }
            true
        };
        if changed {
            self.changed.notify_all();
        }
    }

    /// Publishes a payload derived from accepted `revision` for `generation`.
    ///
    /// The cell is updated under its lock before any reader is woken.
    pub(crate) fn publish(
        &self,
        generation: u64,
        revision: u64,
        payload: Arc<Value>,
    ) -> Result<PublishOutcome, StateError> {
        let outcome = {
            let mut inner = lock(&self.inner);
            let provider = self.provider;
            let Some(active) = inner.active.as_mut().filter(|a| a.generation == generation) else {
                return Err(StateError::Stale { provider, generation });
            };
            if revision < active.floor() {
                return Err(StateError::RevisionStale {
                    provider,
                    generation,
                    revision,
                    floor: active.floor(),
                });
            }
            if revision > active.accepted {
                return Err(StateError::RevisionNotAccepted {
                    provider,
                    generation,
                    revision,
                    accepted: active.accepted,
                });
            }
            if active.held {
                active.last_revision = revision;
                active.staged = Some((revision, payload));
                return Ok(PublishOutcome::Held);
            }
            inner.commit(provider, revision, payload);
            PublishOutcome::Published {
                sequence: inner.active.as_ref().map_or(0, |active| active.sequence),
            }
        };
        self.changed.notify_all();
        Ok(outcome)
    }

    /// Replaces the generation's state with a tombstone. A generation that is
    /// not the active one is ignored, so an old handle's end can never clear a
    /// successor.
    pub(crate) fn end_generation(&self, generation: u64, end: RuntimeEnd) {
        {
            let mut inner = lock(&self.inner);
            if inner.active.as_ref().is_none_or(|a| a.generation != generation) {
                return;
            }
            inner.active = None;
            inner.bump(StateReading::Unavailable(StateUnavailable {
                reason: StateUnavailableReason::Ended(end),
                generation: Some(generation),
            }));
        }
        self.changed.notify_all();
    }

    /// Wakes every waiter with [`StateWait::Closed`]. Reads keep the last value.
    pub(crate) fn close(&self) {
        lock(&self.inner).closed = true;
        self.changed.notify_all();
    }

    fn read(&self) -> StateSnapshot {
        let inner = lock(&self.inner);
        StateSnapshot {
            version: inner.version,
            state: inner.view.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_before_wait(&self, hook: Option<Hook>) { *lock(&self.before_wait) = hook; }

    #[cfg(test)]
    pub(crate) fn set_after_advance(&self, hook: Option<Hook>) {
        *lock(&self.after_advance) = hook;
    }
}

/// The host's registry of provider cells.
#[derive(Default)]
pub(crate) struct StateHub {
    cells: Mutex<HashMap<ModuleId, Arc<StateCell>>>,
    /// Set by `close_all`; cells created afterwards start closed.
    closed: AtomicBool,
}

impl StateHub {
    /// The provider's cell, created absent on first use.
    pub(crate) fn cell(&self, provider: ModuleId) -> Arc<StateCell> {
        let mut cells = lock(&self.cells);
        Arc::clone(cells.entry(provider).or_insert_with(|| {
            let cell = Arc::new(StateCell::new(provider));
            // Read under the map lock, which `close_all` also holds while it
            // sets the flag, so a cell is either closed here or by `close_all`.
            if self.closed.load(Ordering::SeqCst) {
                cell.close();
            }
            cell
        }))
    }

    /// The cell of a provider that has a registered schema.
    pub(crate) fn schema_cell(&self, provider: ModuleId) -> Option<Arc<StateCell>> {
        let cell = lock(&self.cells).get(&provider).cloned()?;
        cell.schema().map(|_| cell)
    }

    /// Registers a contract, or returns the already registered one.
    pub(crate) fn register(&self, schema: RegisteredSchema) -> Result<(), RegisteredSchema> {
        let cell = self.cell(schema.provider);
        let mut inner = lock(&cell.inner);
        if let Some(existing) = inner.schema {
            return Err(existing);
        }
        inner.schema = Some(schema);
        Ok(())
    }

    pub(crate) fn close_all(&self) {
        let cells: Vec<_> = {
            let cells = lock(&self.cells);
            self.closed.store(true, Ordering::SeqCst);
            cells.values().cloned().collect()
        };
        for cell in cells {
            cell.close();
        }
    }

    /// A typed publisher for `module`, bound to `scope`'s generation.
    pub(crate) fn publisher<T: PublishedState>(
        &self,
        module: ModuleId,
        scope: &GenerationScope,
    ) -> Result<StatePublisher<T>, StateError> {
        let requested = type_name::<T>();
        let own = lock(&self.cells).get(&module).cloned().and_then(|cell| {
            let schema = cell.schema()?;
            Some((cell, schema))
        });
        if let Some((cell, schema)) = own {
            if schema.type_id != TypeId::of::<T>() {
                return Err(StateError::SchemaMismatch {
                    module,
                    requested,
                    registered: schema.identity(),
                });
            }
            return Ok(StatePublisher {
                cell,
                generation: scope.id(),
                marker: PhantomData,
            });
        }
        let owner = lock(&self.cells)
            .values()
            .filter_map(|cell| cell.schema())
            .find(|schema| schema.type_id == TypeId::of::<T>())
            .map(|schema| schema.provider);
        Err(
            owner.map_or(StateError::NotRegistered { module, requested }, |provider| {
                StateError::Unauthorized { module, provider }
            }),
        )
    }
}

/// The write handle for one provider, bound to one runtime generation.
///
/// Only `T`, the type registered for the provider, can be published:
///
/// ```compile_fail
/// use stache_sdk::{PublishedState, StatePublisher};
///
/// #[derive(serde::Serialize, serde::Deserialize)]
/// struct Audio;
/// impl PublishedState for Audio {
///     const SCHEMA: stache_sdk::StateSchema =
///         stache_sdk::StateSchema { name: "audio", version: 1 };
/// }
///
/// #[derive(serde::Serialize, serde::Deserialize)]
/// struct Video;
/// impl PublishedState for Video {
///     const SCHEMA: stache_sdk::StateSchema =
///         stache_sdk::StateSchema { name: "video", version: 1 };
/// }
///
/// fn wrong_type(publisher: &StatePublisher<Audio>) {
///     let _ = publisher.publish(1, &Video);
/// }
/// ```
///
/// The handle is invalidated the moment its generation ends; later writes
/// fail with [`StateError::Stale`] and can never reach a successor generation.
#[must_use = "a publisher does nothing until it publishes"]
pub struct StatePublisher<T> {
    cell: Arc<StateCell>,
    generation: u64,
    marker: PhantomData<fn(&T)>,
}

impl<T> Clone for StatePublisher<T> {
    fn clone(&self) -> Self {
        Self {
            cell: Arc::clone(&self.cell),
            generation: self.generation,
            marker: PhantomData,
        }
    }
}

impl<T> fmt::Debug for StatePublisher<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StatePublisher")
            .field("provider", &self.cell.provider)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl<T: PublishedState> StatePublisher<T> {
    /// The runtime generation this handle is bound to.
    #[must_use]
    pub const fn generation(&self) -> u64 { self.generation }

    /// Serializes `value` and publishes it as the provider's latest state,
    /// labelled with `revision`: the accepted configuration revision the value
    /// was derived from (typically `AcceptedConfig::revision` from the
    /// worker's `AcceptedWatch`).
    ///
    /// # Errors
    /// Returns [`StateError::Serialize`] when the value does not serialize,
    /// [`StateError::Stale`] when the generation ended or was replaced,
    /// [`StateError::RevisionStale`] when `revision` is older than the one the
    /// generation started with or already committed, and
    /// [`StateError::RevisionNotAccepted`] when the registry has not accepted
    /// `revision` yet.
    pub fn publish(&self, revision: u64, value: &T) -> Result<PublishOutcome, StateError> {
        let payload = serde_json::to_value(value).map_err(|error| StateError::Serialize {
            schema: SchemaIdentity {
                provider: self.cell.provider,
                name: T::SCHEMA.name,
                version: T::SCHEMA.version,
            },
            message: error.to_string(),
        })?;
        self.cell.publish(self.generation, revision, Arc::new(payload))
    }
}

/// A read-only view of one provider's latest state.
///
/// Clones share the same cell. The observer exposes no mutation and takes no
/// registry, module or lifecycle lock.
#[derive(Clone)]
pub struct StateObserver {
    cell: Arc<StateCell>,
}

impl fmt::Debug for StateObserver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StateObserver")
            .field("provider", &self.cell.provider)
            .finish_non_exhaustive()
    }
}

impl StateObserver {
    pub(crate) const fn new(cell: Arc<StateCell>) -> Self { Self { cell } }

    /// The observed provider.
    #[must_use]
    pub fn provider(&self) -> ModuleId { self.cell.provider }

    /// The latest state, coherent with its version.
    #[must_use]
    pub fn read(&self) -> StateSnapshot { self.cell.read() }

    /// Waits for the cell to move past `seen_version` (zero for "anything").
    ///
    /// The check and the sleep use the same cell lock, so a publication between
    /// them cannot be missed. Publications, tombstones, cancellation and
    /// registry shutdown wake the wait; the result is always the latest value,
    /// so a slow reader sees coalesced state rather than every sample.
    ///
    /// Precedence is cancellation, then close, then change, then timeout. A
    /// timeout that overflows [`Instant`] becomes an unbounded wait.
    #[must_use]
    pub fn wait_for_change(
        &self,
        seen_version: u64,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> StateWait {
        let cell = &self.cell;
        let deadline = Instant::now().checked_add(timeout);
        let _wake = {
            let cell = Arc::clone(cell);
            cancel.on_cancel(Arc::new(move || {
                let _inner = lock(&cell.inner);
                cell.changed.notify_all();
            }))
        };

        let mut inner = lock(&cell.inner);
        let mut timed_out = false;
        loop {
            if cancel.is_cancelled() {
                return StateWait::Cancelled;
            }
            if inner.closed {
                return StateWait::Closed;
            }
            if inner.version != seen_version {
                return StateWait::Changed(StateSnapshot {
                    version: inner.version,
                    state: inner.view.clone(),
                });
            }
            if timed_out {
                return StateWait::TimedOut;
            }
            #[cfg(test)]
            {
                let hook = lock(&cell.before_wait).take();
                if let Some(hook) = hook {
                    hook();
                }
            }
            match deadline {
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    let (next, result) = cell
                        .changed
                        .wait_timeout(inner, remaining)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    inner = next;
                    timed_out = result.timed_out();
                }
                None => {
                    inner =
                        cell.changed.wait(inner).unwrap_or_else(std::sync::PoisonError::into_inner);
                }
            }
        }
    }
}

/// A runtime's binding to its provider cell, held by the registry.
#[derive(Clone)]
pub(crate) struct GenerationBinding {
    cell: Arc<StateCell>,
    generation: u64,
}

impl GenerationBinding {
    /// Binds `scope`'s generation to the cell and tombstones it when the scope
    /// ends.
    pub(crate) fn begin(cell: &Arc<StateCell>, scope: &GenerationScope, revision: u64) -> Self {
        cell.begin_generation(scope.id(), revision);
        let ended = Arc::clone(cell);
        let generation = scope.id();
        scope.on_end(move |end| ended.end_generation(generation, end));
        Self {
            cell: Arc::clone(cell),
            generation,
        }
    }

    /// The start succeeded: release the held publication.
    pub(crate) fn activate(&self) { self.cell.release(self.generation); }
}

#[cfg(test)]
mod tests;
