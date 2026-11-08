//! The proxyAudio module runtime.
//!
//! One module owns at most one listener generation and one cancellable worker.
//! `start` applies the priority rules once, registers the listeners and spawns
//! the worker. `reconfigure` swaps the shared snapshot and re-evaluates once
//! without touching the listeners. `stop` drops the subscription, which removes
//! the exact addresses/callback/client data; the registry then cancels and
//! joins the worker.

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use stache_macos::audio::{AudioBackend, AudioChangeSubscription, AudioDirection, AudioWakeup};
use stache_sdk::{
    CancellationToken, ErrorCategory, Module, ModuleContext, ModuleDescriptor, ModuleError,
    ModuleResult, Operation, ScopedLogger,
};

use crate::config::ProxyAudioConfig;
use crate::device::{AudioDevice, devices_for};
use crate::priority::{resolve_input_device, resolve_output_device};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The proxyAudio module: ordered priorities applied on `CoreAudio` changes.
pub struct ProxyAudioModule {
    backend: Arc<dyn AudioBackend>,
    mirroring: Arc<dyn Fn() -> bool + Send + Sync>,
    config: Option<ProxyAudioConfig>,
    runtime: Option<Runtime>,
    next_generation: u64,
}

struct Runtime {
    subscription: Box<dyn AudioChangeSubscription>,
    /// Wakes the blocked worker after the listeners are removed on stop.
    wakeup: AudioWakeup,
    config: Arc<Mutex<ProxyAudioConfig>>,
    apply: Arc<Mutex<()>>,
    generation: u64,
}

impl ProxyAudioModule {
    /// Creates the module over one backend.
    ///
    /// The production composition root passes [`stache_macos::audio::CoreAudioBackend`];
    /// tests pass a fake.
    #[must_use]
    pub fn new(backend: Arc<dyn AudioBackend>) -> Self {
        Self {
            backend,
            mirroring: Arc::new(stache_macos::display::is_screen_mirroring_active),
            config: None,
            runtime: None,
            next_generation: 0,
        }
    }

    /// Replaces the screen-mirroring probe, which production reads from
    /// `CoreDisplay` and tests pin deterministically.
    #[must_use]
    pub fn with_mirroring(mut self, probe: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        self.mirroring = Arc::new(probe);
        self
    }

    /// Starts one generation.
    ///
    /// The spawner is injected so a worker-creation failure can be exercised
    /// without a real registry.
    fn start_generation<S>(
        &mut self,
        config: &ProxyAudioConfig,
        logger: &ScopedLogger,
        spawn: S,
    ) -> ModuleResult<()>
    where
        S: FnOnce(&str, Box<dyn FnOnce(CancellationToken) + Send>) -> ModuleResult<()>,
    {
        if self.runtime.is_some() {
            return Ok(());
        }
        let (wakeup, receiver) = AudioWakeup::channel();
        let subscription = self.backend.subscribe(wakeup.clone()).map_err(|error| {
            module_error(
                Operation::Start,
                ErrorCategory::Platform,
                format!("failed to register audio listeners: {error}"),
            )
        })?;

        let shared = Arc::new(Mutex::new(config.clone()));
        let apply = Arc::new(Mutex::new(()));
        apply_current(
            &apply,
            &shared,
            self.backend.as_ref(),
            self.mirroring.as_ref(),
            logger,
        );

        let backend = Arc::clone(&self.backend);
        let mirroring = Arc::clone(&self.mirroring);
        let worker_config = Arc::clone(&shared);
        let worker_apply = Arc::clone(&apply);
        let worker_logger = logger.clone();
        let job: Box<dyn FnOnce(CancellationToken) + Send> = Box::new(move |token| {
            run_worker(
                backend,
                worker_config,
                worker_apply,
                mirroring,
                worker_logger,
                receiver,
                token,
            );
        });
        spawn("audio", job)?;

        self.next_generation = self.next_generation.saturating_add(1);
        let generation = self.next_generation;
        logger.debug(format!("audio listener generation {generation} started"));
        self.runtime = Some(Runtime {
            subscription,
            wakeup,
            config: shared,
            apply,
            generation,
        });
        Ok(())
    }
}

impl Module for ProxyAudioModule {
    type Config = ProxyAudioConfig;

    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: ProxyAudioConfig::ID,
            provides: &[],
            requires: &[],
            permissions: &[],
            executables: &[],
            surfaces: &[],
            observes: &[],
            reserves_screen: false,
        }
    }

    fn configure(&mut self, config: &ProxyAudioConfig, _ctx: &ModuleContext) -> ModuleResult<()> {
        self.config = Some(config.clone());
        Ok(())
    }

    fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        let Some(config) = self.config.clone() else {
            return Err(module_error(
                Operation::Start,
                ErrorCategory::Lifecycle,
                "no accepted configuration",
            ));
        };
        self.start_generation(&config, ctx.logger(), |name, job| ctx.spawn_worker(name, job))
    }

    fn reconfigure(&mut self, config: &ProxyAudioConfig, ctx: &ModuleContext) -> ModuleResult<()> {
        self.config = Some(config.clone());
        if let Some(runtime) = self.runtime.as_ref() {
            *lock(&runtime.config) = config.clone();
            apply_current(
                &runtime.apply,
                &runtime.config,
                self.backend.as_ref(),
                self.mirroring.as_ref(),
                &ctx.logger().with_operation(Operation::Reconfigure),
            );
        }
        Ok(())
    }

    fn stop(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        // Dropping the subscription removes the exact listener addresses,
        // callback and client data. The registry has already cancelled the
        // worker token, so one more signal wakes the blocked worker, which
        // sees the cancellation and exits before the registry joins it.
        if let Some(Runtime {
            subscription,
            wakeup,
            generation,
            ..
        }) = self.runtime.take()
        {
            drop(subscription);
            wakeup.signal();
            ctx.logger().debug(format!("audio listener generation {generation} stopped"));
        }
        Ok(())
    }
}

#[allow(clippy::needless_pass_by_value)] // Every argument is moved into the worker closure once.
fn run_worker(
    backend: Arc<dyn AudioBackend>,
    config: Arc<Mutex<ProxyAudioConfig>>,
    apply: Arc<Mutex<()>>,
    mirroring: Arc<dyn Fn() -> bool + Send + Sync>,
    logger: ScopedLogger,
    receiver: Receiver<()>,
    token: CancellationToken,
) {
    // Block without polling: a `CoreAudio` change or `stop` sends the signal.
    while receiver.recv().is_ok() && !token.is_cancelled() {
        apply_current(&apply, &config, backend.as_ref(), mirroring.as_ref(), &logger);
    }
}

/// Applies the current shared snapshot under the runtime apply lock.
///
/// The snapshot is read while the apply lock is held, so a concurrent
/// reconfigure can never leave an older snapshot applied last.
fn apply_current(
    apply: &Mutex<()>,
    config: &Mutex<ProxyAudioConfig>,
    backend: &dyn AudioBackend,
    mirroring: &(dyn Fn() -> bool + Send + Sync),
    logger: &ScopedLogger,
) {
    let _guard = lock(apply);
    let config = lock(config).clone();
    let mirroring_active = mirroring();
    apply_direction(
        backend,
        AudioDirection::Output,
        &config,
        mirroring_active,
        logger,
    );
    apply_direction(backend, AudioDirection::Input, &config, mirroring_active, logger);
}

fn apply_direction(
    backend: &dyn AudioBackend,
    direction: AudioDirection,
    config: &ProxyAudioConfig,
    mirroring_active: bool,
    logger: &ScopedLogger,
) {
    let Some(current) = backend.default_device(direction) else {
        return;
    };
    let devices = devices_for(backend, direction);
    let current = AudioDevice::from_native(&current, backend);
    let target = match direction {
        AudioDirection::Output => {
            resolve_output_device(&current, &devices, config, mirroring_active)
        }
        AudioDirection::Input => resolve_input_device(&current, &devices, config, mirroring_active),
    };
    let Some(target) = target else {
        return;
    };
    if target.id == current.id {
        return;
    }
    match backend.set_default_device(direction, target.id) {
        Ok(()) => logger.info(format!(
            "default {} device changed to {}",
            direction.as_str(),
            target.name
        )),
        Err(error) => logger.error(format!(
            "failed to set default {} device: {error}",
            direction.as_str()
        )),
    }
}

fn module_error(
    operation: Operation,
    category: ErrorCategory,
    message: impl Into<String>,
) -> ModuleError {
    ModuleError::new(ProxyAudioConfig::ID, operation, category, message)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    use stache_sdk::{DiscardLogger, Logger, ModuleHandle, ModuleStatus, Registry, RegistryError};

    use super::*;
    use crate::test_support::{
        CaptureLogger, FakeBackend, config, input_device, invalid_regex_config, output_device,
        registry,
    };

    fn module(backend: &FakeBackend) -> ProxyAudioModule {
        ProxyAudioModule::new(Arc::new(backend.clone())).with_mirroring(|| false)
    }

    fn start(
        backend: &FakeBackend,
        config: ProxyAudioConfig,
        logger: Arc<dyn Logger>,
    ) -> (Registry, ModuleHandle<ProxyAudioConfig>) {
        let registry = registry(logger);
        let handle = registry.register(module(backend), config).expect("register");
        registry.start_all().expect("start");
        (registry, handle)
    }

    fn capture() -> (Arc<CaptureLogger>, Arc<dyn Logger>) {
        let capture = Arc::new(CaptureLogger::default());
        (Arc::clone(&capture), capture)
    }

    #[test]
    fn start_applies_once_and_owns_one_generation() {
        let backend = FakeBackend::new(vec![
            output_device(1, "MacBook Pro Speakers"),
            output_device(2, "AirPods Pro"),
            input_device(3, "MacBook Pro Microphone"),
        ]);
        backend.seed_default(AudioDirection::Output, 1);
        backend.seed_default(AudioDirection::Input, 3);

        let (registry, _handle) = start(
            &backend,
            config(&["AirPods Pro"], &["AT2020USB"]),
            Arc::new(DiscardLogger),
        );

        assert_eq!(backend.sets(), vec![(AudioDirection::Output, 2)]);
        assert_eq!(backend.default_id(AudioDirection::Output), Some(2));
        assert_eq!(backend.live(), 1);
        assert_eq!(backend.generations(), 1);
        registry.shutdown().expect("shutdown");
        assert_eq!(backend.live(), 0);
    }

    #[test]
    fn hdmi_transport_promotes_an_output_whose_name_hides_it() {
        let backend = FakeBackend::new(vec![
            output_device(1, "MacBook Pro Speakers"),
            output_device(2, "Studio Display"),
        ]);
        backend.seed_default(AudioDirection::Output, 1);
        backend.mark_hdmi(2);
        let (registry, _handle) = start(&backend, config(&[], &[]), Arc::new(DiscardLogger));
        assert_eq!(backend.sets(), vec![(AudioDirection::Output, 2)]);
        registry.shutdown().expect("shutdown");
    }

    #[test]
    fn a_failed_subscription_reports_a_failed_start_without_a_runtime() {
        let backend = FakeBackend::new(vec![output_device(1, "MacBook Pro Speakers")]);
        backend.fail_subscribe();
        let registry = registry(Arc::new(DiscardLogger));
        let handle = registry
            .register(module(&backend), config(&["AirPods Pro"], &[]))
            .expect("register");

        let error = registry.start_all().expect_err("listener failure must fail the start");
        assert!(matches!(error, RegistryError::Module(_)), "{error:?}");
        let snapshot = registry.snapshot(handle).expect("snapshot");
        assert!(matches!(snapshot.status, ModuleStatus::Failed(_)));
        assert_eq!(backend.subscribe_calls(), 1);
        assert_eq!(backend.live(), 0);
        registry.shutdown().expect("shutdown is safe after a failed start");
    }

    #[test]
    fn a_worker_spawn_failure_drops_the_subscription_and_keeps_no_runtime() {
        let backend = FakeBackend::new(vec![output_device(1, "MacBook Pro Speakers")]);
        let config = config(&["AirPods Pro"], &[]);
        let logger =
            ScopedLogger::new(Arc::new(DiscardLogger), ProxyAudioConfig::ID, Operation::Start);
        let mut module = module(&backend);
        module.config = Some(config.clone());

        let error = module
            .start_generation(&config, &logger, |_name, job| {
                drop(job);
                Err(module_error(
                    Operation::Start,
                    ErrorCategory::Platform,
                    "failed to spawn worker",
                ))
            })
            .expect_err("spawn failure must fail the start");

        assert!(error.message().contains("failed to spawn worker"));
        assert_eq!(backend.subscribe_calls(), 1);
        assert_eq!(backend.live(), 0);
        assert_eq!(backend.drops(), 1, "the subscription is rolled back");
        assert!(module.runtime.is_none());
    }

    #[test]
    fn a_failed_set_keeps_running_and_logs_the_error() {
        let backend = FakeBackend::new(vec![
            output_device(1, "MacBook Pro Speakers"),
            output_device(2, "AirPods Pro"),
        ]);
        backend.seed_default(AudioDirection::Output, 1);
        backend.fail_set();
        let (captured, logger) = capture();
        let (registry, handle) = start(&backend, config(&["AirPods Pro"], &[]), logger);

        assert_eq!(
            registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Running
        );
        assert!(backend.sets().is_empty());
        assert!(
            captured
                .messages()
                .iter()
                .any(|message| message.contains("failed to set default output device")),
            "{:?}",
            captured.messages()
        );
        registry.shutdown().expect("shutdown");
    }

    #[test]
    fn pause_unregisters_and_resume_registers_a_new_generation() {
        let backend = FakeBackend::new(vec![
            output_device(1, "MacBook Pro Speakers"),
            output_device(2, "AirPods Pro"),
        ]);
        backend.seed_default(AudioDirection::Output, 1);
        let (registry, handle) =
            start(&backend, config(&["AirPods Pro"], &[]), Arc::new(DiscardLogger));
        assert_eq!(backend.live(), 1);

        registry.pause(handle).expect("pause");
        assert_eq!(
            registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Paused
        );
        assert_eq!(backend.live(), 0);
        assert_eq!(backend.drops(), 1);

        registry.resume(handle).expect("resume");
        assert_eq!(backend.live(), 1);
        assert_eq!(backend.generations(), 2);
        assert_eq!(
            registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Running
        );
        registry.shutdown().expect("shutdown");
    }

    #[test]
    fn disabling_and_enabling_follow_the_shared_lifecycle() {
        let backend = FakeBackend::new(vec![
            output_device(1, "MacBook Pro Speakers"),
            output_device(2, "AirPods Pro"),
        ]);
        backend.seed_default(AudioDirection::Output, 1);
        let (registry, handle) =
            start(&backend, config(&["AirPods Pro"], &[]), Arc::new(DiscardLogger));

        registry.set_enabled(handle, false).expect("disable");
        assert_eq!(backend.live(), 0);
        assert_eq!(
            registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Disabled
        );

        registry.set_enabled(handle, true).expect("enable");
        assert_eq!(backend.live(), 1);
        assert_eq!(backend.generations(), 2);
        registry.shutdown().expect("shutdown");
    }

    #[test]
    fn reconfigure_applies_new_priorities_without_duplicating_listeners() {
        let backend = FakeBackend::new(vec![
            output_device(1, "MacBook Pro Speakers"),
            output_device(2, "AirPods Pro"),
            output_device(3, "External Speakers"),
        ]);
        backend.seed_default(AudioDirection::Output, 1);
        let (registry, handle) = start(
            &backend,
            config(&["External Speakers"], &[]),
            Arc::new(DiscardLogger),
        );
        assert_eq!(backend.sets(), vec![(AudioDirection::Output, 3)]);

        registry.reload(handle, config(&["AirPods Pro"], &[])).expect("reload");
        assert_eq!(backend.sets(), vec![
            (AudioDirection::Output, 3),
            (AudioDirection::Output, 2)
        ]);
        assert_eq!(backend.live(), 1);
        assert_eq!(backend.generations(), 1);
        registry.shutdown().expect("shutdown");
    }

    #[test]
    fn reconfigure_does_not_set_a_default_that_already_matches() {
        let backend = FakeBackend::new(vec![
            output_device(1, "MacBook Pro Speakers"),
            output_device(2, "AirPods Pro"),
        ]);
        backend.seed_default(AudioDirection::Output, 1);
        let (registry, handle) =
            start(&backend, config(&["AirPods Pro"], &[]), Arc::new(DiscardLogger));
        assert_eq!(backend.sets().len(), 1);

        registry.reload(handle, config(&["AirPods"], &[])).expect("reload");
        assert_eq!(backend.sets().len(), 1, "a matching target is left alone");
        registry.shutdown().expect("shutdown");
    }

    #[test]
    fn repeated_reconfigure_never_leaves_duplicate_generations() {
        let backend = FakeBackend::new(vec![
            output_device(1, "MacBook Pro Speakers"),
            output_device(2, "AirPods Pro"),
            output_device(3, "External Speakers"),
        ]);
        backend.seed_default(AudioDirection::Output, 1);
        let (registry, handle) = start(
            &backend,
            config(&["External Speakers"], &[]),
            Arc::new(DiscardLogger),
        );

        for index in 0..20 {
            let target = if index % 2 == 0 {
                "AirPods Pro"
            } else {
                "External Speakers"
            };
            registry.reload(handle, config(&[target], &[])).expect("reload");
            assert_eq!(backend.live(), 1, "iteration {index} leaked a generation");
            assert_eq!(
                backend.generations(),
                1,
                "iteration {index} registered a second listener generation"
            );
        }
        registry.shutdown().expect("shutdown");
    }

    #[test]
    fn an_invalid_reload_keeps_running_state_and_the_last_revision() {
        let backend = FakeBackend::new(vec![
            output_device(1, "MacBook Pro Speakers"),
            output_device(2, "AirPods Pro"),
        ]);
        backend.seed_default(AudioDirection::Output, 1);
        let (registry, handle) =
            start(&backend, config(&["AirPods Pro"], &[]), Arc::new(DiscardLogger));
        let before = registry.snapshot(handle).expect("snapshot");

        let error = registry
            .reload(handle, invalid_regex_config())
            .expect_err("invalid regex must be rejected");
        assert!(matches!(error, RegistryError::InvalidConfig { .. }), "{error:?}");

        let after = registry.snapshot(handle).expect("snapshot");
        assert_eq!(after.status, ModuleStatus::Running);
        assert_eq!(after.revision, before.revision);
        assert!(
            after.diagnostic.is_some(),
            "the rejection is reported separately"
        );
        assert_eq!(backend.sets().len(), 1);
        assert_eq!(backend.live(), 1);
        registry.shutdown().expect("shutdown");
    }

    #[test]
    fn a_paused_reload_updates_the_next_start_snapshot() {
        let backend = FakeBackend::new(vec![
            output_device(1, "MacBook Pro Speakers"),
            output_device(2, "AirPods Pro"),
            output_device(3, "External Speakers"),
        ]);
        backend.seed_default(AudioDirection::Output, 1);
        let (registry, handle) = start(
            &backend,
            config(&["External Speakers"], &[]),
            Arc::new(DiscardLogger),
        );
        registry.pause(handle).expect("pause");
        registry.reload(handle, config(&["AirPods Pro"], &[])).expect("paused reload");
        assert_eq!(
            registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Paused
        );
        assert_eq!(backend.live(), 0, "a paused reload must not start the module");
        assert_eq!(backend.sets().len(), 1);

        registry.resume(handle).expect("resume");
        assert_eq!(backend.default_id(AudioDirection::Output), Some(2));
        assert_eq!(backend.generations(), 2);
        registry.shutdown().expect("shutdown");
    }

    #[test]
    fn a_callback_during_stop_is_harmless_and_restart_has_one_generation() {
        let backend = FakeBackend::new(vec![
            output_device(1, "MacBook Pro Speakers"),
            output_device(2, "AirPods Pro"),
        ]);
        backend.seed_default(AudioDirection::Output, 1);
        let (registry, handle) =
            start(&backend, config(&["AirPods Pro"], &[]), Arc::new(DiscardLogger));

        backend.wakeup().signal();
        backend.wake_on_drop();
        registry.pause(handle).expect("pause with a pending callback");
        assert_eq!(backend.live(), 0);

        registry.resume(handle).expect("resume");
        assert_eq!(backend.live(), 1);
        assert_eq!(backend.generations(), 2);
        registry.shutdown().expect("shutdown");
    }

    #[test]
    fn a_callback_racing_pause_leaves_no_generation() {
        let backend = FakeBackend::new(vec![
            output_device(1, "MacBook Pro Speakers"),
            output_device(2, "AirPods Pro"),
        ]);
        backend.seed_default(AudioDirection::Output, 1);
        let (registry, handle) =
            start(&backend, config(&["AirPods Pro"], &[]), Arc::new(DiscardLogger));

        let wakeup = backend.wakeup();
        let barrier = Arc::new(Barrier::new(2));
        let worker_barrier = Arc::clone(&barrier);
        let signalling = thread::spawn(move || {
            worker_barrier.wait();
            wakeup.signal();
        });
        barrier.wait();
        registry.pause(handle).expect("pause racing a callback");
        signalling.join().expect("signalling thread");
        assert_eq!(backend.live(), 0);
        assert_eq!(backend.drops(), 1);
        registry.shutdown().expect("shutdown");
    }

    #[test]
    fn repeated_pause_and_resume_keeps_one_generation() {
        let backend = FakeBackend::new(vec![
            output_device(1, "MacBook Pro Speakers"),
            output_device(2, "AirPods Pro"),
        ]);
        backend.seed_default(AudioDirection::Output, 1);
        let (registry, handle) =
            start(&backend, config(&["AirPods Pro"], &[]), Arc::new(DiscardLogger));

        for index in 0..20 {
            registry.pause(handle).expect("pause");
            assert_eq!(backend.live(), 0, "iteration {index}");
            registry.resume(handle).expect("resume");
            assert_eq!(backend.live(), 1, "iteration {index}");
        }
        registry.shutdown().expect("shutdown");
        assert_eq!(backend.live(), 0);
    }

    #[test]
    fn a_second_start_all_does_not_duplicate_resources() {
        let backend = FakeBackend::new(vec![output_device(1, "MacBook Pro Speakers")]);
        backend.seed_default(AudioDirection::Output, 1);
        let registry = registry(Arc::new(DiscardLogger));
        let _handle = registry
            .register(module(&backend), config(&["MacBook Pro"], &[]))
            .expect("register");
        registry.start_all().expect("start");
        registry.start_all().expect("second start");
        assert_eq!(backend.generations(), 1);
        assert_eq!(backend.live(), 1);
        registry.shutdown().expect("shutdown");
    }

    #[test]
    fn a_stop_that_triggers_a_callback_never_panics() {
        let backend = FakeBackend::new(vec![output_device(1, "MacBook Pro Speakers")]);
        backend.wake_on_drop();
        let (registry, handle) =
            start(&backend, config(&["MacBook Pro"], &[]), Arc::new(DiscardLogger));
        let delivered = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&delivered);
        let wakeup = backend.wakeup();
        let signalling = thread::spawn(move || {
            wakeup.signal();
            observed.store(true, Ordering::SeqCst);
        });
        registry.pause(handle).expect("pause");
        signalling.join().expect("signalling thread");
        assert!(delivered.load(Ordering::SeqCst));
        assert_eq!(backend.live(), 0);
        registry.shutdown().expect("shutdown");
    }
}
