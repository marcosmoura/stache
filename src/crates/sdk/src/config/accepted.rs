//! Accepted configuration publication and bounded revision waits.
//!
//! Every registered module owns one accepted-configuration cell. The registry
//! publishes a coherent `(revision, configuration)` pair there only when a
//! configuration is accepted; module code never runs while the cell lock is
//! held, and a reader never takes a module, lifecycle or reload lock and never
//! reads disk.
//!
//! An [`AcceptedWatch<C>`] is a typed view over that cell. The configuration
//! type is fixed when the watch is created from its [`crate::ModuleHandle`], so
//! a watch cannot read a module's configuration as the wrong type.

use std::any::{Any, TypeId};
use std::fmt;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::lock;
use crate::module::ModuleConfig;
use crate::threading::CancellationToken;

/// A configuration revision and the exact value accepted at it.
///
/// Both fields come from one cell lock acquisition, so a reader can never
/// combine a revision with the configuration of another revision.
pub struct AcceptedConfig<C> {
    /// The registry revision the configuration was accepted at.
    pub revision: u64,
    /// The accepted configuration.
    pub config: Arc<C>,
}

impl<C> Clone for AcceptedConfig<C> {
    fn clone(&self) -> Self {
        Self {
            revision: self.revision,
            config: Arc::clone(&self.config),
        }
    }
}

impl<C> fmt::Debug for AcceptedConfig<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AcceptedConfig")
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

/// The outcome of [`AcceptedWatch::wait_for_change`].
pub enum AcceptedWait<C> {
    /// A revision other than the seen one was published.
    Changed(AcceptedConfig<C>),
    /// The timeout elapsed without a change, cancellation or close.
    TimedOut,
    /// The supplied cancellation token was cancelled.
    Cancelled,
    /// The module slot was invalidated.
    Closed,
}

impl<C> Clone for AcceptedWait<C> {
    fn clone(&self) -> Self {
        match self {
            Self::Changed(config) => Self::Changed(config.clone()),
            Self::TimedOut => Self::TimedOut,
            Self::Cancelled => Self::Cancelled,
            Self::Closed => Self::Closed,
        }
    }
}

impl<C> fmt::Debug for AcceptedWait<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Changed(config) => f.debug_tuple("Changed").field(&config.revision).finish(),
            Self::TimedOut => f.write_str("TimedOut"),
            Self::Cancelled => f.write_str("Cancelled"),
            Self::Closed => f.write_str("Closed"),
        }
    }
}

/// The registered and requested configuration types did not match.
pub(crate) struct TypeMismatch {
    pub(crate) expected: &'static str,
    pub(crate) registered: &'static str,
}

pub(crate) struct AcceptedCell {
    type_id: TypeId,
    type_name: &'static str,
    state: Mutex<CellState>,
    changed: Condvar,
    listeners: Mutex<Vec<ChangeEntry>>,
    next_listener: AtomicU64,
    #[cfg(test)]
    before_wait: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

struct CellState {
    current: Option<(u64, Arc<dyn Any + Send + Sync>)>,
    closed: bool,
}

struct ChangeEntry {
    id: u64,
    listener: Weak<dyn Fn() + Send + Sync>,
}

impl AcceptedCell {
    pub(crate) fn new(type_id: TypeId, type_name: &'static str) -> Self {
        Self {
            type_id,
            type_name,
            state: Mutex::new(CellState { current: None, closed: false }),
            changed: Condvar::new(),
            listeners: Mutex::new(Vec::new()),
            next_listener: AtomicU64::new(0),
            #[cfg(test)]
            before_wait: Mutex::new(None),
        }
    }

    /// Publishes a complete pair, then wakes waiters and change listeners.
    ///
    /// Listener callbacks run after both the state and listener locks have been
    /// released, so a listener may read the new pair or take another lock. A
    /// listener is stored weakly; dropping its [`AcceptedSubscription`] is
    /// enough to retire it.
    pub(crate) fn publish(&self, revision: u64, config: Arc<dyn Any + Send + Sync>) {
        let listeners = {
            let mut state = lock(&self.state);
            state.current = Some((revision, config));
            let mut listeners = lock(&self.listeners);
            listeners.retain(|entry| entry.listener.strong_count() > 0);
            listeners
                .iter()
                .filter_map(|entry| entry.listener.upgrade())
                .collect::<Vec<_>>()
        };
        self.changed.notify_all();
        notify_change_listeners(&listeners);
    }

    /// Marks the cell closed and wakes every waiter. Reads keep the last pair.
    pub(crate) fn close(&self) {
        let listeners = {
            let mut state = lock(&self.state);
            state.closed = true;
            let mut listeners = lock(&self.listeners);
            listeners.retain(|entry| entry.listener.strong_count() > 0);
            listeners
                .iter()
                .filter_map(|entry| entry.listener.upgrade())
                .collect::<Vec<_>>()
        };
        self.changed.notify_all();
        notify_change_listeners(&listeners);
    }

    pub(crate) fn watch<C: ModuleConfig>(
        cell: &Arc<Self>,
    ) -> Result<AcceptedWatch<C>, TypeMismatch> {
        if TypeId::of::<C>() != cell.type_id {
            return Err(TypeMismatch {
                expected: std::any::type_name::<C>(),
                registered: cell.type_name,
            });
        }
        Ok(AcceptedWatch {
            cell: Arc::clone(cell),
            marker: PhantomData,
        })
    }

    /// Registers a wake-only listener, or runs it once when the cell already
    /// moved past `seen_revision` or closed.
    fn subscribe(
        cell: &Arc<Self>,
        seen_revision: u64,
        listener: Arc<dyn Fn() + Send + Sync>,
    ) -> AcceptedSubscription {
        let id = cell.next_listener.fetch_add(1, Ordering::Relaxed);
        let run_now = {
            // The state lock orders this check with `publish`/`close`. A
            // publication either sees the listener already registered, or this
            // check sees the revision it published.
            let state = lock(&cell.state);
            if state.closed
                || state.current.as_ref().is_some_and(|(revision, _)| *revision != seen_revision)
            {
                true
            } else {
                let mut listeners = lock(&cell.listeners);
                listeners.push(ChangeEntry {
                    id,
                    listener: Arc::downgrade(&listener),
                });
                false
            }
        };
        if run_now {
            run_change_listener(&listener);
        }
        AcceptedSubscription {
            cell: Arc::clone(cell),
            id,
            listener,
        }
    }

    #[cfg(test)]
    pub(crate) fn set_before_wait(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *lock(&self.before_wait) = hook;
    }
}

/// A typed, clonable view over one module's accepted configuration.
///
/// A watch reads only the module's published configuration cell: it takes no
/// lifecycle lock, calls no module code and never reads disk. Clone the watch
/// freely; every clone observes the same cell.
///
/// The configuration type is taken from the [`crate::ModuleHandle`] used to
/// create it, so a watch cannot be used to read a different module's
/// configuration type:
///
/// ```compile_fail
/// use stache_sdk::{ConfigError, ModuleConfig, ModuleHandle, Registry};
///
/// #[derive(Clone, PartialEq)]
/// struct Audio;
///
/// impl ModuleConfig for Audio {
///     fn enabled(&self) -> bool { false }
///     fn validate(&self) -> Result<(), ConfigError> { Ok(()) }
/// }
///
/// #[derive(Clone, PartialEq)]
/// struct Video;
///
/// impl ModuleConfig for Video {
///     fn enabled(&self) -> bool { false }
///     fn validate(&self) -> Result<(), ConfigError> { Ok(()) }
/// }
///
/// fn wrong_type(registry: &Registry, handle: ModuleHandle<Audio>) {
///     let _: stache_sdk::AcceptedWatch<Video> = registry.accepted_watch(handle).unwrap();
/// }
/// ```
pub struct AcceptedWatch<C> {
    cell: Arc<AcceptedCell>,
    marker: PhantomData<fn() -> C>,
}

impl<C> Clone for AcceptedWatch<C> {
    fn clone(&self) -> Self {
        Self {
            cell: Arc::clone(&self.cell),
            marker: PhantomData,
        }
    }
}

impl<C> fmt::Debug for AcceptedWatch<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AcceptedWatch").finish_non_exhaustive()
    }
}

impl<C: ModuleConfig> AcceptedWatch<C> {
    /// Returns the last accepted pair, or `None` while nothing is accepted.
    ///
    /// The revision and the value come from one lock acquisition, so the pair
    /// is always coherent. After the cell is closed the last pair stays
    /// readable.
    #[must_use]
    pub fn read(&self) -> Option<AcceptedConfig<C>> {
        let state = lock(&self.cell.state);
        let (revision, value) = state.current.as_ref()?;
        let config = Arc::clone(value).downcast::<C>().ok()?;
        Some(AcceptedConfig { revision: *revision, config })
    }

    /// Waits for the accepted pair to move past `seen_revision`.
    ///
    /// An absent configuration is revision zero. The check and the sleep use
    /// the same cell lock, so a publication between them cannot be missed. A
    /// publication, cancellation and slot close each wake the wait; spurious
    /// wakes reread the authoritative pair.
    ///
    /// Precedence is cancellation, then close, then change, then timeout.
    /// A timeout that overflows [`Instant`] becomes an unbounded wait instead
    /// of a panic or a busy loop.
    #[must_use]
    pub fn wait_for_change(
        &self,
        seen_revision: u64,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> AcceptedWait<C> {
        let deadline = Instant::now().checked_add(timeout);
        let _wake = {
            let cell = Arc::clone(&self.cell);
            cancel.on_cancel(Arc::new(move || {
                let _state = lock(&cell.state);
                cell.changed.notify_all();
            }))
        };

        let mut state = lock(&self.cell.state);
        let mut timed_out = false;
        loop {
            if cancel.is_cancelled() {
                return AcceptedWait::Cancelled;
            }
            if state.closed {
                return AcceptedWait::Closed;
            }
            if let Some((revision, value)) = state.current.as_ref()
                && *revision != seen_revision
            {
                let config = Arc::clone(value).downcast::<C>().ok();
                debug_assert!(
                    config.is_some(),
                    "the accepted cell holds one configuration type"
                );
                if let Some(config) = config {
                    return AcceptedWait::Changed(AcceptedConfig { revision: *revision, config });
                }
            }
            if timed_out {
                return AcceptedWait::TimedOut;
            }
            #[cfg(test)]
            {
                let hook = lock(&self.cell.before_wait).take();
                if let Some(hook) = hook {
                    hook();
                }
            }
            match deadline {
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    let (next, result) = self
                        .cell
                        .changed
                        .wait_timeout(state, remaining)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state = next;
                    timed_out = result.timed_out();
                }
                None => {
                    state = self
                        .cell
                        .changed
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
            }
        }
    }

    /// Registers a wake-only callback for the next accepted publication.
    ///
    /// The callback runs after the publication lock is released. If the cell
    /// already moved past `seen_revision` (or is closed), it runs once before
    /// this call returns, so a caller cannot miss a change between its own read
    /// and this subscription.
    ///
    /// Keep the returned [`AcceptedSubscription`] alive for as long as the
    /// callback is wanted; dropping it retires the callback. The cell stores
    /// only a weak reference, so no callback outlives its subscription. The
    /// listener must be a pure wake: it may notify a condition variable but must
    /// not read disk, take a module lifecycle lock or run module code.
    pub fn on_change(
        &self,
        seen_revision: u64,
        listener: Arc<dyn Fn() + Send + Sync>,
    ) -> AcceptedSubscription {
        AcceptedCell::subscribe(&self.cell, seen_revision, listener)
    }
}

/// A live registration from [`AcceptedWatch::on_change`].
///
/// Dropping the value removes the callback from the cell. The strong listener
/// reference is held here, which is why the cell can keep only a weak one.
#[must_use = "dropping the subscription removes the change listener"]
pub struct AcceptedSubscription {
    cell: Arc<AcceptedCell>,
    id: u64,
    listener: Arc<dyn Fn() + Send + Sync>,
}

impl fmt::Debug for AcceptedSubscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AcceptedSubscription").finish_non_exhaustive()
    }
}

impl Drop for AcceptedSubscription {
    fn drop(&mut self) {
        lock(&self.cell.listeners).retain(|entry| entry.id != self.id);
        let _ = &self.listener;
    }
}

fn notify_change_listeners(listeners: &[Arc<dyn Fn() + Send + Sync>]) {
    for listener in listeners {
        run_change_listener(listener);
    }
}

fn run_change_listener(listener: &Arc<dyn Fn() + Send + Sync>) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener()));
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::error::{ConfigError, ErrorCategory, ModuleError, Operation};
    use crate::module::{Module, ModuleDescriptor, ModuleId};
    use crate::permissions::{Permission, PermissionError, PermissionProvider, PermissionStatus};
    use crate::threading::MainThreadExecutor;
    use crate::{
        ConfigCatalog, ConfigPaths, ConfigStore, DiscardLogger, EventBus, Initial, LogFilter,
        ModuleContext, ModuleHandle, ModuleResult, Registry, RegistryError, ThreadError,
        UnavailableWindows,
    };

    const CONFIG: ModuleId = ModuleId::new("accepted-test");

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
    #[serde(default, rename_all = "camelCase")]
    struct TestConfig {
        enabled: bool,
        value: u32,
    }

    impl TestConfig {
        fn on(value: u32) -> Self { Self { enabled: true, value } }

        fn off(value: u32) -> Self { Self { enabled: false, value } }

        fn invalid() -> Self { Self { enabled: true, value: u32::MAX } }
    }

    impl ModuleConfig for TestConfig {
        fn enabled(&self) -> bool { self.enabled }

        fn validate(&self) -> Result<(), ConfigError> {
            if self.value == u32::MAX {
                return Err(ConfigError::new("value is invalid"));
            }
            Ok(())
        }
    }

    /// A second registered configuration type for the wrong-type test.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    struct OtherConfig;

    impl ModuleConfig for OtherConfig {
        fn enabled(&self) -> bool { false }

        fn validate(&self) -> Result<(), ConfigError> { Ok(()) }
    }

    struct Gate {
        entered: Sender<()>,
        release: Mutex<Option<Receiver<()>>>,
    }

    impl Gate {
        fn new() -> (Arc<Self>, Receiver<()>, Sender<()>) {
            let (entered, entered_rx) = mpsc::channel();
            let (release, release_rx) = mpsc::channel();
            (
                Arc::new(Self {
                    entered,
                    release: Mutex::new(Some(release_rx)),
                }),
                entered_rx,
                release,
            )
        }

        fn pass(&self) {
            let receiver = {
                let mut release = lock(&self.release);
                let Some(receiver) = release.take() else {
                    return;
                };
                self.entered.send(()).expect("the test waits for the gate");
                receiver
            };
            receiver.recv().expect("the test releases the gate");
        }
    }

    #[derive(Default)]
    struct Hooks {
        start: Option<Arc<Gate>>,
        reconfigure: Option<Arc<Gate>>,
        stop: Option<Arc<Gate>>,
        fail_start: bool,
        fail_stop: bool,
    }

    struct TestModule {
        hooks: Arc<Hooks>,
    }

    impl Module for TestModule {
        type Config = TestConfig;

        fn descriptor(&self) -> ModuleDescriptor {
            ModuleDescriptor {
                id: CONFIG,
                provides: &[],
                requires: &[],
                permissions: &[],
                executables: &[],
                surfaces: &[],
            }
        }

        fn configure(&mut self, _: &TestConfig, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }

        fn start(&mut self, _: &ModuleContext) -> ModuleResult<()> {
            if let Some(gate) = &self.hooks.start {
                gate.pass();
            }
            if self.hooks.fail_start {
                return Err(ModuleError::new(
                    CONFIG,
                    Operation::Start,
                    ErrorCategory::Lifecycle,
                    "start failed by test",
                ));
            }
            Ok(())
        }

        fn reconfigure(&mut self, config: &TestConfig, _: &ModuleContext) -> ModuleResult<()> {
            if let Some(gate) = &self.hooks.reconfigure {
                gate.pass();
            }
            if config.value == 7 {
                return Err(ModuleError::new(
                    CONFIG,
                    Operation::Reconfigure,
                    ErrorCategory::Lifecycle,
                    "reconfigure failed by test",
                ));
            }
            Ok(())
        }

        fn stop(&mut self, _: &ModuleContext) -> ModuleResult<()> {
            if let Some(gate) = &self.hooks.stop {
                gate.pass();
            }
            if self.hooks.fail_stop {
                return Err(ModuleError::new(
                    CONFIG,
                    Operation::Stop,
                    ErrorCategory::Lifecycle,
                    "stop failed by test",
                ));
            }
            Ok(())
        }
    }

    struct InlineMain;

    impl MainThreadExecutor for InlineMain {
        fn is_main_thread(&self) -> bool { true }

        fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
            job();
            Ok(())
        }
    }

    struct AllowAll;

    impl PermissionProvider for AllowAll {
        fn status(&self, _: Permission) -> PermissionStatus { PermissionStatus::Granted }

        fn request(&self, _: Permission) -> Result<PermissionStatus, PermissionError> {
            Ok(PermissionStatus::Granted)
        }
    }

    fn registry() -> Arc<Registry> {
        Arc::new(Registry::new(
            Arc::new(DiscardLogger),
            Arc::new(EventBus::new()),
            Arc::new(InlineMain),
            Arc::new(AllowAll),
            Arc::new(UnavailableWindows),
        ))
    }

    fn register(
        registry: &Registry,
        hooks: Arc<Hooks>,
        config: TestConfig,
    ) -> ModuleHandle<TestConfig> {
        registry.register(TestModule { hooks }, config).expect("register")
    }

    fn within<T: Send + 'static>(
        label: &'static str,
        job: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(job());
        });
        receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| panic!("{label} waited for blocked module code"))
    }

    fn wait_entered(entered: &Receiver<()>) {
        entered.recv_timeout(Duration::from_secs(5)).expect("the gate was not entered");
    }

    fn read_watch<C: ModuleConfig>(watch: &AcceptedWatch<C>) -> Option<AcceptedConfig<C>> {
        within("accepted read", {
            let watch = watch.clone();
            move || watch.read()
        })
    }

    fn changed<C>(wait: AcceptedWait<C>) -> AcceptedConfig<C> {
        match wait {
            AcceptedWait::Changed(config) => config,
            other => panic!("expected a change, got {other:?}"),
        }
    }

    fn assert_send_sync<T: Send + Sync>(_: &T) {}

    #[test]
    fn a_valid_registration_publishes_revision_one() {
        let registry = registry();
        let handle = register(&registry, Arc::new(Hooks::default()), TestConfig::on(1));
        let watch = registry.accepted_watch(handle).expect("watch");
        let accepted = watch.read().expect("accepted");
        assert_eq!(accepted.revision, 1);
        assert_eq!(*accepted.config, TestConfig::on(1));
        let clone = watch.clone();
        assert_eq!(clone.read().expect("clone").revision, 1);
        assert_send_sync(&watch);
    }

    #[test]
    fn an_unchanged_reload_publishes_nothing() {
        let registry = registry();
        let handle = register(&registry, Arc::new(Hooks::default()), TestConfig::on(1));
        let watch = registry.accepted_watch(handle).expect("watch");
        let token = CancellationToken::new();
        let report = registry.reload(handle, TestConfig::on(1)).expect("unchanged");
        assert_eq!(report.status, crate::ReloadStatus::Unchanged);
        let accepted = watch.read().expect("accepted");
        assert_eq!(accepted.revision, 1);
        assert!(matches!(
            watch.wait_for_change(1, Duration::from_millis(20), &token),
            AcceptedWait::TimedOut
        ));
    }

    #[test]
    fn a_failed_pre_commit_quiesce_publishes_nothing() {
        let registry = registry();
        let hooks = Arc::new(Hooks {
            fail_stop: true,
            ..Hooks::default()
        });
        let handle = register(&registry, hooks, TestConfig::on(1));
        registry.start_all().expect("start");
        let watch = registry.accepted_watch(handle).expect("watch");
        assert!(
            registry.set_enabled(handle, false).is_err(),
            "stop failed by test"
        );

        let token = CancellationToken::new();
        assert!(
            registry.reload(handle, TestConfig::on(2)).is_err(),
            "the pre-commit quiesce failed"
        );
        let accepted = watch.read().expect("accepted");
        assert_eq!(accepted.revision, 1);
        assert_eq!(*accepted.config, TestConfig::on(1));
        assert!(matches!(
            watch.wait_for_change(1, Duration::from_millis(20), &token),
            AcceptedWait::TimedOut
        ));
    }

    #[test]
    fn shutdown_closes_the_cell_and_keeps_the_last_pair_readable() {
        let registry = registry();
        let handle = register(&registry, Arc::new(Hooks::default()), TestConfig::on(1));
        let watch = registry.accepted_watch(handle).expect("watch");
        registry.shutdown().expect("shutdown");
        let accepted = watch.read().expect("the last pair stays readable");
        assert_eq!(accepted.revision, 1);
        let token = CancellationToken::new();
        assert!(matches!(
            watch.wait_for_change(1, Duration::from_secs(u64::MAX), &token),
            AcceptedWait::Closed
        ));
    }

    #[test]
    fn a_never_existing_file_registers_accepted_defaults() {
        let (_root, registry, store) = store_setup();
        let initial: Initial<TestConfig> = store.initial(CONFIG).expect("initial");
        let Initial::Default(config) = initial else {
            panic!("a never-existing file uses the typed default");
        };
        let handle = register(&registry, Arc::new(Hooks::default()), config);
        store.bind(handle).expect("bind");
        let watch = registry.accepted_watch(handle).expect("watch");
        let accepted = watch.read().expect("defaults are accepted");
        assert_eq!(accepted.revision, 1);
        assert_eq!(*accepted.config, TestConfig::default());
    }

    #[test]
    fn an_invalid_loaded_file_publishes_nothing_through_registration() {
        use std::fs;

        let (_root, registry, store) = store_setup();
        fs::write(
            store.paths().module_file(CONFIG),
            format!("{{ \"enabled\": true, \"value\": {} }}", u32::MAX),
        )
        .expect("config");
        let initial: Initial<TestConfig> = store.initial(CONFIG).expect("initial");
        let Initial::Loaded(config) = initial else {
            panic!("an invalid value is still loaded for registration");
        };
        let handle = register(&registry, Arc::new(Hooks::default()), config);
        store.bind(handle).expect("bind");
        let watch = registry.accepted_watch(handle).expect("watch");
        assert!(
            watch.read().is_none(),
            "an invalid loaded value must not publish a pair"
        );
        assert_eq!(registry.snapshot(handle).expect("snapshot").revision, 0);
    }

    #[test]
    fn a_rejected_initial_config_is_absent_at_revision_zero() {
        let registry = registry();
        let handle = registry
            .register_rejected(
                TestModule {
                    hooks: Arc::new(Hooks::default()),
                },
                crate::ConfigDiagnostic::new("the file did not decode"),
            )
            .expect("register rejected");
        let watch = registry.accepted_watch(handle).expect("watch");
        assert!(watch.read().is_none());
        assert_eq!(registry.snapshot(handle).expect("snapshot").revision, 0);
    }

    #[test]
    fn an_invalid_initial_config_publishes_nothing() {
        let registry = registry();
        let handle = register(&registry, Arc::new(Hooks::default()), TestConfig::invalid());
        let watch = registry.accepted_watch(handle).expect("watch");
        assert!(
            watch.read().is_none(),
            "validation failure must not publish a pair"
        );
        let snapshot = registry.snapshot(handle).expect("snapshot");
        assert_eq!(snapshot.revision, 0);
        assert!(snapshot.diagnostic.is_some());
    }

    #[test]
    fn a_later_valid_reload_publishes_revision_one() {
        let registry = registry();
        let handle = registry
            .register_rejected(
                TestModule {
                    hooks: Arc::new(Hooks::default()),
                },
                crate::ConfigDiagnostic::new("the file did not decode"),
            )
            .expect("register rejected");
        let watch = registry.accepted_watch(handle).expect("watch");
        let report = registry.reload(handle, TestConfig::on(5)).expect("reload");
        assert_eq!(report.status, crate::ReloadStatus::Applied { revision: 1 });
        let accepted = watch.read().expect("accepted after recovery");
        assert_eq!(accepted.revision, 1);
        assert_eq!(*accepted.config, TestConfig::on(5));
    }

    #[test]
    fn a_handle_with_a_different_config_type_is_rejected() {
        let registry = registry();
        register(&registry, Arc::new(Hooks::default()), TestConfig::on(1));
        let wrong: ModuleHandle<OtherConfig> = ModuleHandle::for_test(CONFIG);
        let error = registry.accepted_watch(wrong).expect_err("wrong type");
        assert!(
            matches!(
                error,
                RegistryError::ConfigTypeMismatch { id, .. } if id == CONFIG
            ),
            "{error}"
        );
    }

    #[test]
    fn disabled_and_paused_reloads_publish_new_pairs() {
        let disabled_registry = registry();
        let handle = register(
            &disabled_registry,
            Arc::new(Hooks::default()),
            TestConfig::off(1),
        );
        disabled_registry.start_all().expect("start");
        let watch = disabled_registry.accepted_watch(handle).expect("watch");

        let report = disabled_registry.reload(handle, TestConfig::off(2)).expect("disabled reload");
        assert_eq!(report.status, crate::ReloadStatus::Applied { revision: 2 });
        let accepted = watch.read().expect("accepted");
        assert_eq!(accepted.revision, 2);
        assert_eq!(*accepted.config, TestConfig::off(2));
        assert_eq!(
            disabled_registry.snapshot(handle).expect("snapshot").status,
            crate::ModuleStatus::Disabled
        );

        let paused_registry = registry();
        let handle = register(&paused_registry, Arc::new(Hooks::default()), TestConfig::on(1));
        paused_registry.start_all().expect("start");
        paused_registry.pause(handle).expect("pause");
        let watch = paused_registry.accepted_watch(handle).expect("watch");
        let report = paused_registry.reload(handle, TestConfig::on(3)).expect("paused reload");
        assert_eq!(report.status, crate::ReloadStatus::Applied { revision: 2 });
        let accepted = watch.read().expect("accepted");
        assert_eq!(accepted.revision, 2);
        assert_eq!(*accepted.config, TestConfig::on(3));
        assert_eq!(
            paused_registry.snapshot(handle).expect("snapshot").status,
            crate::ModuleStatus::Paused
        );
    }

    #[test]
    fn a_rejected_reload_keeps_the_last_pair_and_does_not_notify() {
        let registry = registry();
        let handle = register(&registry, Arc::new(Hooks::default()), TestConfig::on(1));
        let watch = registry.accepted_watch(handle).expect("watch");
        let token = CancellationToken::new();
        assert!(registry.reload(handle, TestConfig::invalid()).is_err());

        let accepted = watch.read().expect("accepted");
        assert_eq!(accepted.revision, 1);
        assert_eq!(*accepted.config, TestConfig::on(1));
        assert!(matches!(
            watch.wait_for_change(1, Duration::from_millis(30), &token),
            AcceptedWait::TimedOut
        ));
    }

    #[test]
    fn a_failed_reconfigure_keeps_the_old_pair_and_does_not_notify() {
        let registry = registry();
        let handle = register(&registry, Arc::new(Hooks::default()), TestConfig::on(1));
        registry.start_all().expect("start");
        let watch = registry.accepted_watch(handle).expect("watch");
        let token = CancellationToken::new();
        assert!(registry.reload(handle, TestConfig::on(7)).is_err());

        let accepted = watch.read().expect("accepted");
        assert_eq!(accepted.revision, 1);
        assert!(matches!(
            watch.wait_for_change(1, Duration::from_millis(30), &token),
            AcceptedWait::TimedOut
        ));
    }

    #[test]
    fn a_committed_config_with_a_failed_start_is_visible() {
        let registry = registry();
        let hooks = Arc::new(Hooks {
            fail_start: true,
            ..Hooks::default()
        });
        let handle = register(&registry, hooks, TestConfig::off(1));
        registry.start_all().expect("start");
        let watch = registry.accepted_watch(handle).expect("watch");
        let report = registry.reload(handle, TestConfig::on(2)).expect("config commits");
        assert!(report.start_error.is_some());
        assert_eq!(report.status, crate::ReloadStatus::Applied { revision: 2 });
        let accepted = watch.read().expect("accepted despite start failure");
        assert_eq!(accepted.revision, 2);
        assert_eq!(*accepted.config, TestConfig::on(2));
    }

    #[test]
    fn a_blocked_reconfigure_keeps_the_old_pair_until_acceptance() {
        let registry = registry();
        let (gate, entered, release) = Gate::new();
        let hooks = Arc::new(Hooks {
            reconfigure: Some(gate),
            ..Hooks::default()
        });
        let handle = register(&registry, hooks, TestConfig::on(1));
        registry.start_all().expect("start");
        let watch = registry.accepted_watch(handle).expect("watch");

        let reload = {
            let registry = Arc::clone(&registry);
            thread::spawn(move || registry.reload(handle, TestConfig::on(2)))
        };
        wait_entered(&entered);

        let accepted = read_watch(&watch).expect("old pair while reconfigure blocks");
        assert_eq!(accepted.revision, 1);
        assert_eq!(*accepted.config, TestConfig::on(1));

        release.send(()).expect("release reconfigure");
        let report = reload.join().expect("reload thread").expect("reload");
        assert_eq!(report.status, crate::ReloadStatus::Applied { revision: 2 });
        let accepted = watch.read().expect("new pair after acceptance");
        assert_eq!(accepted.revision, 2);
        assert_eq!(*accepted.config, TestConfig::on(2));
    }

    #[test]
    fn reads_complete_while_start_and_stop_are_blocked() {
        let registry = registry();
        let (start_gate, start_entered, start_release) = Gate::new();
        let (stop_gate, stop_entered, stop_release) = Gate::new();
        let hooks = Arc::new(Hooks {
            start: Some(start_gate),
            stop: Some(stop_gate),
            ..Hooks::default()
        });
        let handle = register(&registry, hooks, TestConfig::on(1));
        let watch = registry.accepted_watch(handle).expect("watch");

        let start = {
            let registry = Arc::clone(&registry);
            thread::spawn(move || registry.start_all())
        };
        wait_entered(&start_entered);
        assert_eq!(read_watch(&watch).expect("pair during start").revision, 1);
        start_release.send(()).expect("release start");
        start.join().expect("start thread").expect("start");

        let stop = {
            let registry = Arc::clone(&registry);
            thread::spawn(move || registry.pause(handle))
        };
        wait_entered(&stop_entered);
        assert_eq!(read_watch(&watch).expect("pair during stop").revision, 1);
        stop_release.send(()).expect("release stop");
        stop.join().expect("stop thread").expect("stop");
    }

    #[test]
    fn pairs_are_atomic_under_concurrent_reloads() {
        let registry = registry();
        let handle = register(&registry, Arc::new(Hooks::default()), TestConfig::on(1));
        let watch = registry.accepted_watch(handle).expect("watch");
        let running = Arc::new(AtomicBool::new(true));
        let reader = {
            let running = Arc::clone(&running);
            thread::spawn(move || {
                while running.load(Ordering::SeqCst) {
                    if let Some(accepted) = watch.read() {
                        assert_eq!(
                            u64::from(accepted.config.value),
                            accepted.revision,
                            "a reader saw a revision/config pair from different publications"
                        );
                    }
                }
            })
        };
        for value in 2..=60 {
            registry.reload(handle, TestConfig::on(value)).expect("reload");
        }
        running.store(false, Ordering::SeqCst);
        reader.join().expect("reader thread");
    }

    #[test]
    fn rapid_a_b_a_updates_report_every_revision() {
        let registry = registry();
        let handle = register(&registry, Arc::new(Hooks::default()), TestConfig::on(1));
        let watch = registry.accepted_watch(handle).expect("watch");
        registry.reload(handle, TestConfig::on(2)).expect("A");
        let token = CancellationToken::new();

        let first = changed(watch.wait_for_change(1, Duration::from_secs(5), &token));
        assert_eq!(first.revision, 2);
        assert_eq!(*first.config, TestConfig::on(2));

        registry.reload(handle, TestConfig::on(3)).expect("B");
        let second = changed(watch.wait_for_change(2, Duration::from_secs(5), &token));
        assert_eq!(second.revision, 3);
        assert_eq!(*second.config, TestConfig::on(3));

        registry.reload(handle, TestConfig::on(2)).expect("A again");
        let third = changed(watch.wait_for_change(3, Duration::from_secs(5), &token));
        assert_eq!(third.revision, 4);
        assert_eq!(
            *third.config,
            TestConfig::on(2),
            "the same configuration is visible at a different revision"
        );
        assert_eq!(first.config, third.config);

        // Coalesced updates: a late waiter sees only the latest complete pair.
        registry.reload(handle, TestConfig::on(5)).expect("rapid 1");
        registry.reload(handle, TestConfig::on(6)).expect("rapid 2");
        registry.reload(handle, TestConfig::on(7)).expect("rapid 3");
        let latest = changed(watch.wait_for_change(4, Duration::from_secs(5), &token));
        assert_eq!(latest.revision, 7);
        assert_eq!(*latest.config, TestConfig::on(7));

        assert!(matches!(
            watch.wait_for_change(7, Duration::from_millis(20), &token),
            AcceptedWait::TimedOut
        ));
    }

    #[test]
    fn a_publication_between_the_check_and_the_wait_wakes_the_waiter() {
        let registry = registry();
        let handle = register(&registry, Arc::new(Hooks::default()), TestConfig::on(1));
        let watch = registry.accepted_watch(handle).expect("watch");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        watch.cell.set_before_wait(Some(Arc::new(move || {
            let _ = entered_tx.send(());
            let _ = lock(&release_rx).recv();
        })));

        let waiter = {
            thread::spawn(move || {
                let token = CancellationToken::new();
                watch.wait_for_change(1, Duration::from_secs(5), &token)
            })
        };
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the waiter reached the check/wait boundary");

        // The publisher blocks on the cell lock the waiter still holds. Once the
        // waiter releases it by sleeping, the publication and wake must arrive.
        let publisher = {
            let registry = Arc::clone(&registry);
            thread::spawn(move || registry.reload(handle, TestConfig::on(2)))
        };
        release_tx.send(()).expect("release the waiter");
        publisher.join().expect("publisher thread").expect("reload");
        let accepted = changed(waiter.join().expect("waiter thread"));
        assert_eq!(accepted.revision, 2);
        assert_eq!(*accepted.config, TestConfig::on(2));
    }

    #[test]
    fn a_very_long_timeout_wakes_on_cancellation() {
        let registry = registry();
        let handle = register(&registry, Arc::new(Hooks::default()), TestConfig::on(1));
        let watch = registry.accepted_watch(handle).expect("watch");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        watch.cell.set_before_wait(Some(Arc::new(move || {
            let _ = entered_tx.send(());
            let _ = lock(&release_rx).recv();
        })));
        let token = CancellationToken::new();
        let waiter = {
            let token = token.clone();
            thread::spawn(move || watch.wait_for_change(1, Duration::from_secs(u64::MAX), &token))
        };
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the waiter reached the check/wait boundary");
        let canceller = thread::spawn(move || token.cancel());
        release_tx.send(()).expect("release the waiter");
        let result = waiter.join().expect("waiter thread");
        canceller.join().expect("canceller thread");
        assert!(matches!(result, AcceptedWait::Cancelled), "{result:?}");
    }

    #[test]
    fn reads_after_close_return_the_last_pair() {
        let registry = registry();
        let handle = register(&registry, Arc::new(Hooks::default()), TestConfig::on(1));
        let watch = registry.accepted_watch(handle).expect("watch");
        registry.invalidate();
        let accepted = watch.read().expect("the last pair stays readable");
        assert_eq!(accepted.revision, 1);
        assert_eq!(*accepted.config, TestConfig::on(1));
    }

    #[test]
    fn waiting_treats_an_absent_config_as_revision_zero() {
        let registry = registry();
        let handle = registry
            .register_rejected(
                TestModule {
                    hooks: Arc::new(Hooks::default()),
                },
                crate::ConfigDiagnostic::new("absent"),
            )
            .expect("register rejected");
        let watch = registry.accepted_watch(handle).expect("watch");
        let token = CancellationToken::new();
        assert!(matches!(
            watch.wait_for_change(0, Duration::from_millis(20), &token),
            AcceptedWait::TimedOut
        ));
        registry.reload(handle, TestConfig::on(4)).expect("reload");
        let accepted = changed(watch.wait_for_change(0, Duration::from_secs(5), &token));
        assert_eq!(accepted.revision, 1);
    }

    #[test]
    fn cancellation_listeners_do_not_accumulate_across_waits() {
        let registry = registry();
        let handle = register(&registry, Arc::new(Hooks::default()), TestConfig::on(1));
        let watch = registry.accepted_watch(handle).expect("watch");
        let token = CancellationToken::new();
        for _ in 0..50 {
            let _ = watch.wait_for_change(1, Duration::from_millis(1), &token);
            assert_eq!(token.listener_count(), 0);
        }
        registry.reload(handle, TestConfig::on(2)).expect("reload");
        let _ = watch.wait_for_change(1, Duration::from_secs(5), &token);
        assert_eq!(token.listener_count(), 0);
    }

    #[test]
    fn a_change_listener_runs_after_publication_and_can_read_the_new_pair() {
        let registry = registry();
        let handle = register(&registry, Arc::new(Hooks::default()), TestConfig::on(1));
        let watch = registry.accepted_watch(handle).expect("watch");
        let observed = Arc::new(Mutex::new(None));
        let seen = Arc::clone(&observed);
        let callback_watch = watch.clone();
        let subscription = watch.on_change(
            1,
            Arc::new(move || {
                let pair = callback_watch.read().expect("the new pair is readable");
                *lock(&seen) = Some(pair.revision);
            }),
        );

        registry.reload(handle, TestConfig::on(2)).expect("publish");
        assert_eq!(
            *lock(&observed),
            Some(2),
            "the callback runs after the publication lock is released"
        );
        drop(subscription);
        registry.reload(handle, TestConfig::on(3)).expect("publish again");
        assert_eq!(*lock(&observed), Some(2), "a dropped subscription is retired");
    }

    #[test]
    fn a_late_subscription_runs_immediately_for_an_already_changed_revision() {
        let registry = registry();
        let handle = register(&registry, Arc::new(Hooks::default()), TestConfig::on(1));
        let watch = registry.accepted_watch(handle).expect("watch");
        registry.reload(handle, TestConfig::on(2)).expect("publish");
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&hits);
        let _subscription = watch.on_change(
            1,
            Arc::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_subscription_created_at_the_current_revision_waits_for_the_next_one() {
        let registry = registry();
        let handle = register(&registry, Arc::new(Hooks::default()), TestConfig::on(1));
        let watch = registry.accepted_watch(handle).expect("watch");
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&hits);
        let _subscription = watch.on_change(
            1,
            Arc::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        registry.reload(handle, TestConfig::on(2)).expect("publish");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn closing_the_cell_notifies_a_subscription() {
        let registry = registry();
        let handle = register(&registry, Arc::new(Hooks::default()), TestConfig::on(1));
        let watch = registry.accepted_watch(handle).expect("watch");
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&hits);
        let _subscription = watch.on_change(
            1,
            Arc::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        );
        registry.invalidate();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_panicking_change_listener_does_not_stop_publication() {
        let registry = registry();
        let handle = register(&registry, Arc::new(Hooks::default()), TestConfig::on(1));
        let watch = registry.accepted_watch(handle).expect("watch");
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&hits);
        let _first = watch.on_change(1, Arc::new(|| panic!("expected listener panic")));
        let _second = watch.on_change(
            1,
            Arc::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        );
        registry.reload(handle, TestConfig::on(2)).expect("publish");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(watch.read().expect("pair").revision, 2);
    }

    #[test]
    fn change_listeners_do_not_accumulate_across_subscriptions() {
        let registry = registry();
        let handle = register(&registry, Arc::new(Hooks::default()), TestConfig::on(1));
        let watch = registry.accepted_watch(handle).expect("watch");
        for _ in 0..20 {
            let subscription = watch.on_change(1, Arc::new(|| {}));
            drop(subscription);
        }
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&hits);
        let _subscription = watch.on_change(
            1,
            Arc::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        );
        registry.reload(handle, TestConfig::on(2)).expect("publish");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    fn store_setup() -> (tempfile::TempDir, Arc<Registry>, Arc<ConfigStore>) {
        use std::fs;

        let root = tempfile::tempdir().expect("root");
        let directory = root.path().join("config");
        fs::create_dir_all(&directory).expect("config directory");
        let registry = registry();
        let mut catalog = ConfigCatalog::new();
        catalog.register::<TestConfig>(CONFIG).expect("catalog");
        let store = Arc::new(ConfigStore::new(
            Arc::clone(&registry),
            catalog,
            ConfigPaths::new(&directory),
            Arc::new(LogFilter::default()),
        ));
        (root, registry, store)
    }

    #[test]
    fn a_malformed_initial_file_then_a_valid_fix_becomes_visible() {
        use std::fs;

        let (_root, registry, store) = store_setup();
        fs::write(store.paths().module_file(CONFIG), "{ \"value\": ").expect("malformed");
        let initial: Initial<TestConfig> = store.initial(CONFIG).expect("initial");
        let Initial::Invalid(diagnostic) = initial else {
            panic!("expected an invalid initial file");
        };
        let handle = registry
            .register_rejected(
                TestModule {
                    hooks: Arc::new(Hooks::default()),
                },
                crate::ConfigDiagnostic::new(diagnostic.message),
            )
            .expect("register rejected");
        store.bind(handle).expect("bind");
        let watch = registry.accepted_watch(handle).expect("watch");
        assert!(watch.read().is_none());

        fs::write(
            store.paths().module_file(CONFIG),
            "{ \"enabled\": true, \"value\": 2 }",
        )
        .expect("valid file");
        assert_eq!(
            store.reload_one(CONFIG).expect("reload"),
            crate::ReloadOutcome::Applied { revision: 1 }
        );
        let accepted = watch.read().expect("a later valid fix is visible");
        assert_eq!(accepted.revision, 1);
        assert_eq!(*accepted.config, TestConfig::on(2));
    }

    #[test]
    fn a_deleted_file_reload_keeps_the_last_pair_without_disk_access() {
        use std::fs;

        let (root, registry, store) = store_setup();
        fs::write(
            store.paths().module_file(CONFIG),
            "{ \"enabled\": false, \"value\": 1 }",
        )
        .expect("config");
        let initial: Initial<TestConfig> = store.initial(CONFIG).expect("initial");
        let Initial::Loaded(config) = initial else {
            panic!("expected a loaded config");
        };
        let handle = register(&registry, Arc::new(Hooks::default()), config);
        store.bind(handle).expect("bind");
        let watch = registry.accepted_watch(handle).expect("watch");
        assert_eq!(watch.read().expect("accepted").revision, 1);

        fs::remove_dir_all(root.path().join("config")).expect("remove the configuration directory");
        let outcome = store.reload_one(CONFIG).expect("reload");
        assert!(matches!(outcome, crate::ReloadOutcome::Rejected(_)));
        let accepted = watch.read().expect("the accessor does not read disk");
        assert_eq!(accepted.revision, 1);
        assert_eq!(*accepted.config, TestConfig::off(1));
    }

    #[test]
    fn a_close_wakes_a_wait_with_a_very_long_timeout() {
        let registry = registry();
        let handle = register(&registry, Arc::new(Hooks::default()), TestConfig::on(1));
        let watch = registry.accepted_watch(handle).expect("watch");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        watch.cell.set_before_wait(Some(Arc::new(move || {
            let _ = entered_tx.send(());
            let _ = lock(&release_rx).recv();
        })));
        let waiter = {
            thread::spawn(move || {
                let token = CancellationToken::new();
                watch.wait_for_change(1, Duration::from_secs(u64::MAX), &token)
            })
        };
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the waiter reached the check/wait boundary");
        let closing = {
            let registry = Arc::clone(&registry);
            thread::spawn(move || registry.invalidate())
        };
        release_tx.send(()).expect("release the waiter");
        let started = Instant::now();
        let result = waiter.join().expect("waiter thread");
        closing.join().expect("closing thread");
        assert!(matches!(result, AcceptedWait::Closed), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
