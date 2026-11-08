//! The rotation module, its one timer worker and the typed socket handler.
//!
//! `WallpapersModule::start` freezes one accepted snapshot, validates the pool
//! without decoding, creates one session with one wakeable timer when the
//! interval is positive, enqueues the initial job first and returns. The timer
//! and the coordinator are separate: `Module.stop` invalidates only the session
//! and its queued rotation work, while explicit requests remain served by the
//! host-lifetime service.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};

use serde_json::Value;
use stache_sdk::ipc::{HandlerError, HandlerResult, IpcHandler, decode_params, encode_response};
use stache_sdk::{
    ErrorCategory, Module, ModuleContext, ModuleDescriptor, ModuleError, ModuleResult, Operation,
    ScopedLogger,
};

use super::clock::WaitOutcome;
use super::service::{ServiceStatus, StartGate, WallpapersService};
use super::session::SessionControl;
use super::types::{WallpapersSetOutcome, WallpapersSetParams};
use crate::WallpapersConfig;
use crate::sources::{SourcePool, resolve_pool};

/// The typed socket handler for `wallpapers.set`.
pub struct WallpapersIpc {
    service: Arc<WallpapersService>,
}

impl WallpapersIpc {
    /// Binds the handler to one host-lifetime service.
    #[must_use]
    pub const fn new(service: Arc<WallpapersService>) -> Self { Self { service } }

    /// Handles one decoded `wallpapers.set` request.
    #[must_use]
    pub fn set(&self, params: &WallpapersSetParams) -> WallpapersSetOutcome {
        self.service.set(params)
    }
}

impl IpcHandler for WallpapersIpc {
    fn handle(&self, method: &str, params: Value) -> HandlerResult<Value> {
        match method {
            "set" => {
                let params: WallpapersSetParams = decode_params(params)?;
                encode_response(&self.service.set(&params))
            }
            _ => Err(HandlerError::unknown_method(method)),
        }
    }
}

/// One running rotation module.
pub struct WallpapersModule {
    service: Arc<WallpapersService>,
    session: Option<Arc<SessionControl>>,
    staged: Arc<Mutex<Option<StagedCandidate>>>,
}

#[derive(Clone)]
struct StagedCandidate {
    config: WallpapersConfig,
    pool: SourcePool,
}

impl WallpapersModule {
    /// Creates an inert module around the host-lifetime service.
    #[must_use]
    pub fn new(service: Arc<WallpapersService>) -> Self {
        Self {
            service,
            session: None,
            staged: Arc::new(Mutex::new(None)),
        }
    }

    /// The observable service state, including this module's session.
    #[must_use]
    pub fn status(&self) -> ServiceStatus { self.service.status() }

    fn spawn_timer(&self, ctx: &ModuleContext, session: &Arc<SessionControl>) -> ModuleResult<()> {
        let Some(timer_id) = session.try_claim_timer() else {
            return Ok(());
        };
        #[cfg(test)]
        if self.service.take_timer_spawn_failure() {
            session.release_timer(timer_id);
            return Err(ModuleError::new(
                WallpapersConfig::ID,
                ctx.logger().operation(),
                ErrorCategory::Platform,
                "injected rotation timer spawn failure",
            ));
        }
        let service = Arc::clone(&self.service);
        let worker_session = Arc::clone(session);
        let staged = Arc::clone(&self.staged);
        let logger = ctx.logger().clone();
        if let Err(error) = ctx.spawn_worker("wallpapers-rotation", move |token| {
            run_timer(service, worker_session, staged, logger, timer_id, token);
        }) {
            session.release_timer(timer_id);
            return Err(error);
        }
        Ok(())
    }
}

impl Module for WallpapersModule {
    type Config = WallpapersConfig;

    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: WallpapersConfig::ID,
            provides: &[],
            requires: &[],
            permissions: &[],
            executables: &[],
            surfaces: &[],
            observes: &[],
            reserves_screen: false,
        }
    }

    fn configure(&mut self, _: &WallpapersConfig, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }

    fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        *lock(&self.staged) = None;
        let Some(pair) = self.service.accepted_pair() else {
            return Err(ModuleError::new(
                WallpapersConfig::ID,
                Operation::Start,
                ErrorCategory::Lifecycle,
                "no accepted Wallpapers configuration is available; fix wallpapers.jsonc and retry",
            ));
        };
        let pool = resolve_pool(&pair.config, self.service.source_context()).map_err(|error| {
            ModuleError::new(
                WallpapersConfig::ID,
                Operation::Start,
                ErrorCategory::Config,
                error.to_string(),
            )
        })?;
        if pool.is_empty() {
            return Err(ModuleError::new(
                WallpapersConfig::ID,
                Operation::Start,
                ErrorCategory::Config,
                "wallpapers is enabled but no usable image source resolved; configure path or list and retry",
            ));
        }
        // The initial job is queued first, but it cannot reach a native call
        // until this startup stage has established every owned resource. A
        // positive-interval timer spawn failure therefore rolls back without
        // ever setting a wallpaper.
        let gate = Arc::new(StartGate::new());
        let session = self.service.begin_session(&pair, pool, self.service.now());
        self.session = Some(Arc::clone(&session));
        if !self.service.submit_initial(
            &session,
            pair.revision,
            ctx.logger().clone(),
            Arc::clone(&gate),
        ) {
            self.session = None;
            self.service.end_session(session.id());
            return Err(ModuleError::new(
                WallpapersConfig::ID,
                Operation::Start,
                ErrorCategory::Lifecycle,
                "wallpaper start could not reserve the rotation queue; retry the start",
            ));
        }
        if pair.config.interval > 0 {
            if let Err(error) = self.spawn_timer(ctx, &session) {
                gate.fail();
                self.session = None;
                self.service.end_session(session.id());
                return Err(error);
            }
            if !session.timer_spawned() {
                gate.fail();
                self.session = None;
                self.service.end_session(session.id());
                return Err(ModuleError::new(
                    WallpapersConfig::ID,
                    Operation::Start,
                    ErrorCategory::Lifecycle,
                    "wallpaper start could not establish its rotation timer; retry the start",
                ));
            }
        }
        gate.open();
        Ok(())
    }

    fn reconfigure(&mut self, config: &WallpapersConfig, ctx: &ModuleContext) -> ModuleResult<()> {
        let pool = resolve_pool(config, self.service.source_context()).map_err(|error| {
            ModuleError::new(
                WallpapersConfig::ID,
                Operation::Reconfigure,
                ErrorCategory::Config,
                error.to_string(),
            )
        })?;
        if config.enabled && pool.is_empty() {
            return Err(ModuleError::new(
                WallpapersConfig::ID,
                Operation::Reconfigure,
                ErrorCategory::Config,
                "wallpapers is enabled but no usable image source resolved; fix path or list and retry",
            ));
        }
        *lock(&self.staged) = Some(StagedCandidate { config: config.clone(), pool });

        let Some(session) = self.session.clone() else {
            return Ok(());
        };
        let previous_pending = session.pending_positive_interval();
        session.set_pending_positive_interval(config.interval > 0);
        if config.interval > 0
            && !session.timer_spawned()
            && let Err(error) = self.spawn_timer(ctx, &session)
        {
            session.set_pending_positive_interval(previous_pending);
            return Err(error);
        }
        session.signal().notify();
        Ok(())
    }

    fn stop(&mut self, _: &ModuleContext) -> ModuleResult<()> {
        if let Some(session) = self.session.take() {
            self.service.end_session(session.id());
        }
        *lock(&self.staged) = None;
        Ok(())
    }
}

#[allow(clippy::needless_pass_by_value)]
#[allow(clippy::too_many_arguments)]
fn run_timer(
    service: Arc<WallpapersService>,
    session: Arc<SessionControl>,
    staged: Arc<Mutex<Option<StagedCandidate>>>,
    logger: ScopedLogger,
    mut timer_id: u64,
    token: stache_sdk::CancellationToken,
) {
    loop {
        if token.is_cancelled() || !session.is_live() {
            break;
        }
        // Snapshot the wake generation before reading or adopting the state it
        // guards. A publication before the snapshot is then visible to `adopt`;
        // one after it changes the generation and wakes the wait.
        let seen = session.signal().generation();
        adopt(&service, &session, &staged, &logger);
        let interval = session.interval();
        if interval == 0 {
            if !session.pending_positive_interval() {
                session.release_timer(timer_id);
                if !session.pending_positive_interval() {
                    break;
                }
                let Some(next_id) = session.try_claim_timer() else {
                    break;
                };
                timer_id = next_id;
                continue;
            }
            match session.signal().wait_after(seen, None, service.clock_ref()) {
                WaitOutcome::Stopped => break,
                WaitOutcome::Wake | WaitOutcome::Deadline => continue,
            }
        }
        let Some(deadline) = session.deadline() else {
            let revision = session.revision();
            session.note_attempt_finished_if(service.now(), revision, &|| {
                accepted_revision(&service, revision)
            });
            continue;
        };
        match session.signal().wait_after(seen, Some(deadline), service.clock_ref()) {
            WaitOutcome::Stopped => break,
            WaitOutcome::Wake => continue,
            WaitOutcome::Deadline => {}
        }

        let revision = session.revision();
        let (reply, receiver) = mpsc::channel();
        if !service.submit_rotation(&session, revision, logger.clone(), Some(reply)) {
            logger.warn("wallpaper rotation skipped: the apply queue is full");
            session.note_attempt_finished_if(service.now(), revision, &|| {
                accepted_revision(&service, revision)
            });
            continue;
        }

        // Wait losslessly for the job's completion. An accepted publication may
        // arrive while a noninterruptible decode keeps the job running; the
        // generation snapshot plus a nonblocking reply check means neither a
        // publication nor a completion can be lost between check and wait.
        loop {
            if token.is_cancelled() || !session.is_live() {
                break;
            }
            let seen = session.signal().generation();
            match receiver.try_recv() {
                Ok(report) => {
                    if !report.full_success {
                        logger.debug("wallpaper rotation attempt was not a full success");
                    }
                    break;
                }
                Err(mpsc::TryRecvError::Disconnected) => break,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            adopt(&service, &session, &staged, &logger);
            if !session.is_live() {
                break;
            }
            // An accepted interval of zero removes this timer resource; the
            // queued job remains owned by the coordinator, and its reply
            // channel is simply dropped rather than polled.
            if session.interval() == 0 && !session.pending_positive_interval() {
                break;
            }
            match session.signal().wait_after(seen, None, service.clock_ref()) {
                WaitOutcome::Stopped => break,
                WaitOutcome::Wake | WaitOutcome::Deadline => {}
            }
        }
        if token.is_cancelled() || !session.is_live() {
            break;
        }
        adopt(&service, &session, &staged, &logger);
        if session.interval() == 0 && !session.pending_positive_interval() {
            continue;
        }
        // Only reschedule for this job's revision; a publication that landed
        // while it ran is adopted on the next iteration instead.
        if session.revision() == revision
            && session.deadline().is_some_and(|deadline| deadline <= service.now())
        {
            session.note_attempt_finished_if(service.now(), revision, &|| {
                accepted_revision(&service, revision)
            });
        }
    }
    session.release_timer(timer_id);
}

fn accepted_revision(service: &WallpapersService, revision: u64) -> bool {
    service.accepted_pair().is_some_and(|pair| pair.revision == revision)
}

fn adopt(
    service: &WallpapersService,
    session: &SessionControl,
    staged: &Mutex<Option<StagedCandidate>>,
    logger: &ScopedLogger,
) {
    if !session.is_live() {
        return;
    }
    let Some(pair) = service.accepted_pair() else {
        return;
    };
    if pair.revision == session.revision() {
        return;
    }
    let candidate = lock(staged)
        .as_ref()
        .filter(|candidate| candidate.config == *pair.config)
        .cloned();
    let pool = if let Some(candidate) = candidate {
        candidate.pool
    } else {
        match resolve_pool(&pair.config, service.source_context()) {
            Ok(pool) => pool,
            Err(error) => {
                logger.warn(format!(
                    "wallpaper configuration revision {} could not be adopted: {error}",
                    pair.revision
                ));
                return;
            }
        }
    };
    let Some(latest) = service.accepted_pair() else {
        return;
    };
    if latest.revision != pair.revision {
        // A newer publication superseded this pair while the pool was resolved;
        // the next wake rereads the current cell instead of adopting behind it.
        return;
    }
    *lock(staged) = None;
    let config = (*latest.config).clone();
    let mode = config.mode.clone();
    session.adopt(&latest, pool, service.now());
    logger.info(format!(
        "adopted wallpaper revision {} (interval {}, mode {mode:?})",
        pair.revision, config.interval
    ));
    if config.interval == 0 {
        session.set_pending_positive_interval(false);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
