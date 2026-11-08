//! The noTunes module lifecycle.
//!
//! One [`NoTunesModule`] owns at most one observation generation and one
//! SDK-owned policy worker. `start` installs observation, publishes the runtime
//! ownership, sweeps already-running Apple Music and iTunes instances and only
//! then spawns the worker, so callbacks that arrive during installation are
//! queued instead of lost.
//!
//! `reconfigure` accepts the local revision and publishes the accepted
//! configuration and token as one step under the snapshot lock, so queued work
//! always commits under a complete target and never spends stale retries
//! between the native acceptance and the publication. `stop` invalidates the
//! local runtime first, wakes the blocked worker and then removes the exact
//! observation; a removal failure keeps ownership so the SDK can retry it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use stache_macos::{ApplicationControl, ApplicationError, ApplicationObservation};
use stache_sdk::{
    CancellationToken, ErrorCategory, Module, ModuleContext, ModuleDescriptor, ModuleError,
    ModuleInvalidation, ModuleResult, Operation, ScopedLogger,
};

use crate::config::NoTunesConfig;
use crate::lock;
use crate::policy::{Inbox, PolicySnapshot, PolicyWorker, WorkerSetup, WorkerState, sweep};

/// The worker name visible in thread listings and diagnostics.
const WORKER_NAME: &str = "notunes-policy";

/// Automatic Apple Music and iTunes blocking.
pub struct NoTunesModule {
    control: ApplicationControl,
    config: Option<NoTunesConfig>,
    runtime: Option<Runtime>,
    next_generation: u64,
    #[cfg(test)]
    after_accept: Option<AcceptHook>,
    #[cfg(test)]
    after_spawn: Option<AfterSpawnHook>,
    #[cfg(test)]
    snapshot_probe: Option<Sender<Arc<Mutex<PolicySnapshot>>>>,
}

#[cfg(test)]
type AcceptHook = Arc<dyn Fn() + Send + Sync>;

#[cfg(test)]
type AfterSpawnHook = Arc<dyn Fn() + Send + Sync>;

struct Runtime {
    observation: Arc<ApplicationObservation>,
    snapshot: Arc<Mutex<PolicySnapshot>>,
    inbox: Sender<Inbox>,
    active: Arc<AtomicBool>,
    generation: u64,
}

impl NoTunesModule {
    /// Creates the module over one safe native application control.
    ///
    /// The production composition root builds the control from the host's
    /// main-thread executor and the system backend. The module never owns a
    /// cloned executor or a native object.
    #[must_use]
    pub const fn new(control: ApplicationControl) -> Self {
        Self {
            control,
            config: None,
            runtime: None,
            next_generation: 0,
            #[cfg(test)]
            after_accept: None,
            #[cfg(test)]
            after_spawn: None,
            #[cfg(test)]
            snapshot_probe: None,
        }
    }

    /// Pauses a start after the worker was spawned and before the bounded
    /// post-spawn recheck, so a test can prove that check never waits for a
    /// stalled native send.
    #[cfg(test)]
    pub(crate) fn with_after_spawn_hook(mut self, hook: AfterSpawnHook) -> Self {
        self.after_spawn = Some(hook);
        self
    }

    /// Pauses a reconfigure after native acceptance while the snapshot is
    /// published, so a test can prove no worker observes the old target.
    #[cfg(test)]
    pub(crate) fn with_after_accept_hook(mut self, hook: AcceptHook) -> Self {
        self.after_accept = Some(hook);
        self
    }

    /// Receives each started generation's policy snapshot for test inspection.
    #[cfg(test)]
    pub(crate) fn with_snapshot_probe(mut self, probe: Sender<Arc<Mutex<PolicySnapshot>>>) -> Self {
        self.snapshot_probe = Some(probe);
        self
    }

    /// Cleans up a failed install and reports an actionable start error.
    fn fail_install(&self, error: ApplicationError, logger: &ScopedLogger) -> ModuleError {
        // A failed install already invalidated its generation and kept the
        // exact cleanup owned by the control; acknowledge it here because no
        // observation handle exists to stop.
        let cleanup = self.control.stop();
        let message = format!(
            "failed to install the application observer: {error}{}",
            cleanup_suffix(&cleanup)
        );
        logger.warn(&message);
        module_error(Operation::Start, ErrorCategory::Platform, message).with_source(error)
    }

    /// Cancels an accepted start whose observation was invalidated or whose
    /// lifecycle was cancelled before it could run.
    fn fail_start(
        &mut self,
        observation: &ApplicationObservation,
        reason: &str,
        category: ErrorCategory,
        logger: &ScopedLogger,
    ) -> ModuleError {
        if let Some(runtime) = self.runtime.as_ref() {
            runtime.active.store(false, Ordering::SeqCst);
            let _ = runtime.inbox.send(Inbox::Stop);
        }
        let cleanup = observation.stop();
        if cleanup.is_ok() {
            self.runtime = None;
        }
        let message = format!("{reason}{}", cleanup_suffix(&cleanup));
        logger.warn(&message);
        module_error(Operation::Start, category, message)
    }

    /// Cleans up a runtime whose initial policy could not be accepted.
    fn fail_accept(
        &mut self,
        observation: &ApplicationObservation,
        error: ApplicationError,
        logger: &ScopedLogger,
    ) -> ModuleError {
        let reason = format!("failed to accept the initial policy revision: {error}");
        self.fail_start(observation, &reason, ErrorCategory::Platform, logger)
            .with_source(error)
    }

    /// Cleans up a runtime whose worker could not be spawned.
    fn fail_spawn(
        &mut self,
        observation: &ApplicationObservation,
        inbox: &Sender<Inbox>,
        error: ModuleError,
        logger: &ScopedLogger,
    ) -> ModuleError {
        if let Some(runtime) = self.runtime.as_ref() {
            runtime.active.store(false, Ordering::SeqCst);
        }
        let _ = inbox.send(Inbox::Stop);
        let cleanup = observation.stop();
        if cleanup.is_ok() {
            self.runtime = None;
        }
        let message = format!(
            "failed to spawn the policy worker: {error}{}",
            cleanup_suffix(&cleanup)
        );
        logger.warn(&message);
        module_error(Operation::Start, ErrorCategory::Platform, message).with_source(error)
    }
}

impl Module for NoTunesModule {
    type Config = NoTunesConfig;

    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: NoTunesConfig::ID,
            provides: &[],
            requires: &[],
            permissions: &[],
            executables: &[],
            surfaces: &[],
            observes: &[],
            reserves_screen: false,
        }
    }

    fn configure(&mut self, config: &NoTunesConfig, _ctx: &ModuleContext) -> ModuleResult<()> {
        self.config = Some(config.clone());
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // One ordered install, sweep and worker spawn.
    fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        if self.runtime.is_some() {
            return Ok(());
        }
        let Some(config) = self.config.clone() else {
            return Err(module_error(
                Operation::Start,
                ErrorCategory::Lifecycle,
                "no accepted configuration",
            ));
        };

        // The inbox exists before installation: a will-launch notification can
        // be delivered before `install` returns, and it must be queued rather
        // than dropped.
        let (sender, receiver) = mpsc::channel();
        let notify = sender.clone();
        let observation = match self.control.install(&ctx.cancellation(), move |application| {
            let _ = notify.send(Inbox::Launch(application));
        }) {
            Ok(observation) => observation,
            Err(error) => return Err(self.fail_install(error, ctx.logger())),
        };

        self.next_generation = self.next_generation.saturating_add(1);
        let generation = self.next_generation;
        let observation = Arc::new(observation);
        // Register before the first acceptance: a listener registered after the
        // registry already invalidated this module runs immediately, so a start
        // racing teardown is inert before it can issue anything.
        wire_invalidation(ctx.invalidation(), &observation);
        let snapshot = Arc::new(Mutex::new(PolicySnapshot { config, token: None }));
        let active = Arc::new(AtomicBool::new(true));
        self.runtime = Some(Runtime {
            observation: Arc::clone(&observation),
            snapshot: Arc::clone(&snapshot),
            inbox: sender.clone(),
            active: Arc::clone(&active),
            generation,
        });

        #[cfg(test)]
        if let Some(probe) = &self.snapshot_probe {
            let _ = probe.send(Arc::clone(&snapshot));
        }

        if ctx.cancellation().is_cancelled() {
            return Err(self.fail_start(
                observation.as_ref(),
                "start was cancelled before the initial policy was accepted",
                ErrorCategory::Lifecycle,
                ctx.logger(),
            ));
        }

        let token = match observation.accept() {
            Ok(token) => token,
            Err(error) => {
                return Err(self.fail_accept(observation.as_ref(), error, ctx.logger()));
            }
        };
        lock(&snapshot).token = Some(token);

        // The initial sweep runs before the worker exists, so a notification
        // that overlaps it is queued and deduplicated against these markers.
        let logger = ctx.logger().clone();
        let mut state = WorkerState::default();
        if sweep(
            observation.as_ref(),
            token,
            &mut state,
            &logger,
            &ctx.cancellation(),
            &active,
        ) {
            return Err(self.fail_start(
                observation.as_ref(),
                "start was interrupted during the initial application sweep",
                ErrorCategory::Lifecycle,
                ctx.logger(),
            ));
        }
        if !observation.is_current(token) {
            return Err(self.fail_start(
                observation.as_ref(),
                "the observation generation was invalidated during the initial application sweep",
                ErrorCategory::Platform,
                ctx.logger(),
            ));
        }

        let worker_observation = Arc::clone(&observation);
        let worker_snapshot = Arc::clone(&snapshot);
        let worker_active = Arc::clone(&active);
        let worker_inbox = sender.clone();
        let worker_logger = logger.clone();
        let worker_cancellation = ctx.cancellation();
        let job: Box<dyn FnOnce(CancellationToken) + Send> = Box::new(move |group_token| {
            let mut worker = PolicyWorker::new(
                WorkerSetup {
                    observation: worker_observation,
                    snapshot: worker_snapshot,
                    active: worker_active,
                    inbox: worker_inbox,
                    logger: worker_logger,
                    cancellation: worker_cancellation,
                },
                state,
            );
            // Block without polling: one event, a config change or the stop
            // wake unblocks the worker.
            while let Ok(event) = receiver.recv() {
                if matches!(event, Inbox::Stop) || group_token.is_cancelled() {
                    break;
                }
                worker.handle(event);
            }
        });
        if let Err(error) = ctx.spawn_worker(WORKER_NAME, job) {
            return Err(self.fail_spawn(observation.as_ref(), &sender, error, ctx.logger()));
        }
        #[cfg(test)]
        if let Some(hook) = self.after_spawn.as_ref() {
            hook();
        }
        // The bounded advisory read, not `is_current`, is used here: `is_current`
        // takes the commit gate and would wait for a stalled native send. A
        // stopped or superseded generation still fails the start.
        if ctx.cancellation().is_cancelled() || !observation.is_active() {
            return Err(self.fail_start(
                observation.as_ref(),
                "start was cancelled while the policy worker was starting",
                ErrorCategory::Lifecycle,
                ctx.logger(),
            ));
        }
        logger.debug(format!("noTunes policy generation {generation} started"));
        Ok(())
    }

    fn reconfigure(&mut self, config: &NoTunesConfig, ctx: &ModuleContext) -> ModuleResult<()> {
        if self.config.as_ref() == Some(config) {
            return Ok(());
        }
        if let Some(runtime) = self.runtime.as_ref() {
            // Acceptance and publication are one step under the snapshot lock:
            // the worker either sees the complete previous target or the
            // complete new one. It never observes a bumped native revision
            // paired with the previous snapshot, which would spend its stale
            // retries and lose the newest target.
            //
            // There is no lock inversion: native callbacks and open completions
            // only enqueue, the issuance permit never reads this snapshot, and
            // the worker releases it before any native call. Only the bounded
            // local acceptance gate can wait while the lock is held.
            let mut snapshot = lock(&runtime.snapshot);
            let token = runtime.observation.accept().map_err(|error| {
                module_error(
                    Operation::Reconfigure,
                    ErrorCategory::Platform,
                    format!("cannot accept the new policy revision: {error}"),
                )
                .with_source(error)
            })?;
            snapshot.config = config.clone();
            snapshot.token = Some(token);
            #[cfg(test)]
            if let Some(hook) = self.after_accept.as_ref() {
                hook();
            }
            drop(snapshot);
            let _ = runtime.inbox.send(Inbox::ConfigChanged);
            ctx.logger().debug(format!(
                "noTunes policy generation {} accepted a new target",
                runtime.generation
            ));
        }
        // A paused or stopped module only stages the next-start snapshot.
        self.config = Some(config.clone());
        Ok(())
    }

    fn stop(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        if let Some(runtime) = self.runtime.take() {
            // Local invalidation comes first: queued events and late
            // completions become inert before any native cleanup can wait.
            runtime.active.store(false, Ordering::SeqCst);
            let _ = runtime.inbox.send(Inbox::Stop);
            match runtime.observation.stop() {
                Ok(()) => {
                    ctx.logger().debug(format!(
                        "noTunes policy generation {} stopped",
                        runtime.generation
                    ));
                    Ok(())
                }
                Err(error) => {
                    // Keep the exact observation and runtime so the SDK's
                    // `needs_stop` retry can remove it; never claim success.
                    self.runtime = Some(runtime);
                    let message = format!("failed to remove the application observer: {error}");
                    ctx.logger().error(message.clone());
                    Err(module_error(Operation::Stop, ErrorCategory::Platform, message)
                        .with_source(error))
                }
            }
        } else {
            // A failed start never returned an observation handle. The control
            // still owns its partial generation; acknowledge that cleanup
            // before reporting success, so Retry starts from a clean slate.
            self.control.stop().map_err(|error| {
                let message = format!("failed to clean up the application observer: {error}");
                ctx.logger().error(message.clone());
                module_error(Operation::Stop, ErrorCategory::Platform, message).with_source(error)
            })
        }
    }
}

impl Drop for NoTunesModule {
    /// Best-effort cleanup when the module is dropped without a lifecycle stop.
    ///
    /// The registry always stops the module first. This only prevents a leaked
    /// observer if a host violates that contract.
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.active.store(false, Ordering::SeqCst);
            let _ = runtime.inbox.send(Inbox::Stop);
            let _ = runtime.observation.stop();
        }
    }
}

fn cleanup_suffix(cleanup: &Result<(), ApplicationError>) -> String {
    match cleanup {
        Ok(()) => String::new(),
        Err(error) => format!("; observation cleanup also failed: {error}"),
    }
}

/// Connects the registry's stage-1 invalidation to one observation generation.
///
/// The listener only invalidates logically; it removes nothing. A listener
/// registered after the handle was already invalidated runs immediately, so a
/// start that races teardown cannot issue native work. Listeners run at most
/// once and the handle belongs to this module slot, so there is nothing to
/// unregister.
pub fn wire_invalidation(
    invalidation: &ModuleInvalidation,
    observation: &Arc<ApplicationObservation>,
) {
    let observation = Arc::clone(observation);
    invalidation.on_invalidate(Arc::new(move || observation.invalidate()));
}

fn module_error(
    operation: Operation,
    category: ErrorCategory,
    message: impl Into<String>,
) -> ModuleError {
    ModuleError::new(NoTunesConfig::ID, operation, category, message)
}
