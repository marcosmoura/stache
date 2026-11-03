//! Blocking policy, deduplication and the policy worker.
//!
//! Native callbacks never run policy. A will-launch notification and an open
//! completion only enqueue a small [`Inbox`] event; the worker is the single
//! place that classifies an instance, decides whether to terminate or open and
//! tracks pending work.
//!
//! The worker never holds the accepted-configuration lock while it calls native
//! code. Each step clones the current [`PolicySnapshot`] and releases the lock
//! before dispatching. The native issuance permit, not this module, decides
//! whether a request is still current, so a queued action always commits under
//! the configuration accepted at that moment.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use stache_macos::{
    AcceptedActionToken, ApplicationError, ApplicationId, ApplicationObservation, OpenCompletion,
    OpenCompletionContext, OpenRequest, TerminateOutcome,
};
use stache_sdk::{CancellationToken, ScopedLogger};

use crate::config::{NoTunesConfig, ReplacementTarget};
use crate::lock;

/// The exact bundle identifier of Apple Music.
pub const MUSIC_BUNDLE_ID: &str = "com.apple.Music";

/// The exact bundle identifier of iTunes.
pub const ITUNES_BUNDLE_ID: &str = "com.apple.iTunes";

const BLOCKED_BUNDLE_IDS: [&str; 2] = [MUSIC_BUNDLE_ID, ITUNES_BUNDLE_ID];

/// How many recent application identities are remembered after they exit.
const RETIRED_LIMIT: usize = 256;

/// How many times one notification re-reads the snapshot after a stale refusal.
const STALE_RETRIES: usize = 3;

/// The accepted configuration and the native action token that matches it.
pub struct PolicySnapshot {
    pub config: NoTunesConfig,
    pub token: Option<AcceptedActionToken>,
}

/// One event the worker owns.
pub enum Inbox {
    /// A will-launch notification for one native instance.
    Launch(ApplicationId),
    /// The result of one replacement-open request.
    Opened {
        /// The local reservation nonce of the request.
        nonce: u64,
        /// The native request context.
        context: OpenCompletionContext,
        /// Whether the control still considered the request newest at delivery.
        current: bool,
        /// The native result.
        result: Result<(), ApplicationError>,
    },
    /// The accepted configuration changed.
    ///
    /// This is a wake-up only: it carries no revision, because a queued event
    /// may describe an older acceptance than the snapshot the worker reads.
    ConfigChanged,
    /// Wakes the blocked worker so the registry can join it.
    Stop,
}

/// Per-instance decisions, keyed only by the opaque native identity.
#[derive(Default)]
pub struct WorkerState {
    live: BTreeMap<ApplicationId, InstanceRecord>,
    retired: VecDeque<ApplicationId>,
    pending: Option<PendingOpen>,
    next_nonce: u64,
}

impl WorkerState {
    /// Marks one exact instance as exited.
    ///
    /// This is event-driven: it runs after an inbox event, never from a timer.
    /// A live marker is never evicted because the retired cache is full, so a
    /// still-running instance can never allow a duplicate replacement.
    fn retire(&mut self, application: ApplicationId) {
        self.live.remove(&application);
        self.retired.push_back(application);
        if self.retired.len() > RETIRED_LIMIT {
            self.retired.pop_front();
        }
    }
}

#[derive(Default)]
struct InstanceRecord {
    termination_requested: bool,
    replacement_decided: bool,
}

struct PendingOpen {
    nonce: u64,
    token: AcceptedActionToken,
}

/// The single policy worker.
pub struct PolicyWorker {
    observation: Arc<ApplicationObservation>,
    snapshot: Arc<Mutex<PolicySnapshot>>,
    active: Arc<AtomicBool>,
    inbox: Sender<Inbox>,
    logger: ScopedLogger,
    cancellation: CancellationToken,
    state: WorkerState,
}

/// The shared handles one policy worker needs.
pub struct WorkerSetup {
    pub observation: Arc<ApplicationObservation>,
    pub snapshot: Arc<Mutex<PolicySnapshot>>,
    pub active: Arc<AtomicBool>,
    pub inbox: Sender<Inbox>,
    pub logger: ScopedLogger,
    pub cancellation: CancellationToken,
}

impl PolicyWorker {
    /// Creates the worker over one live observation.
    pub fn new(setup: WorkerSetup, state: WorkerState) -> Self {
        Self {
            observation: setup.observation,
            snapshot: setup.snapshot,
            active: setup.active,
            inbox: setup.inbox,
            logger: setup.logger,
            cancellation: setup.cancellation,
            state,
        }
    }

    /// Handles one inbox event.
    pub(crate) fn handle(&mut self, event: Inbox) {
        let retire = match event {
            Inbox::Launch(application) => {
                self.handle_launch(application);
                true
            }
            Inbox::Opened {
                nonce,
                context,
                current,
                result,
            } => self.handle_opened(nonce, context, current, &result),
            Inbox::ConfigChanged => {
                self.handle_config();
                true
            }
            Inbox::Stop => false,
        };
        if retire && !self.stopped() {
            self.retire_exited();
        }
    }

    /// Retires markers for instances the native boundary reports as exited.
    ///
    /// The check between native queries makes retirement responsive to stop and
    /// cancellation even with a large live set: at most the one query already
    /// in flight can delay the worker from exiting. No lock is held while
    /// querying.
    pub(crate) fn retire_exited(&mut self) {
        let live: Vec<ApplicationId> = self.state.live.keys().copied().collect();
        for application in live {
            if self.stopped() {
                break;
            }
            if matches!(self.observation.is_running(application), Ok(false)) {
                self.state.retire(application);
            }
        }
    }

    fn handle_launch(&mut self, application: ApplicationId) {
        if self.stopped() {
            return;
        }
        if self.state.retired.contains(&application) {
            self.logger
                .debug("ignoring a launch notification for a retired application instance");
            return;
        }
        if self.current().is_none() {
            return;
        }
        let bundle_id = match self.observation.bundle_identifier(application) {
            Ok(Some(bundle_id)) => bundle_id,
            Ok(None) => {
                self.logger.debug("ignoring a launch notification without a bundle identifier");
                return;
            }
            Err(error) => {
                self.logger.warn(format!("cannot identify a launched application: {error}"));
                return;
            }
        };
        if !is_blocked(&bundle_id) {
            self.logger.debug(format!(
                "ignoring the launch of unrelated application {bundle_id}"
            ));
            return;
        }

        for attempt in 0..=STALE_RETRIES {
            let Some((_config, token)) = self.current() else {
                return;
            };
            self.reconcile_pending(token);
            if !self.termination_requested(application) {
                match self.observation.terminate(token, application) {
                    Ok(TerminateOutcome::Stale) => {
                        if attempt == STALE_RETRIES {
                            self.logger.warn(format!(
                                "stopped blocking {bundle_id}: its policy revision kept changing"
                            ));
                            return;
                        }
                        continue;
                    }
                    Ok(TerminateOutcome::Requested) => {
                        self.mark_termination(application);
                        self.logger.info(format!(
                            "requested termination of {bundle_id} (macOS accepted the request; exit is not confirmed)"
                        ));
                    }
                    Ok(TerminateOutcome::NotRunning) => {
                        self.mark_termination(application);
                        self.logger.debug(format!("{bundle_id} was already no longer running"));
                    }
                    Ok(TerminateOutcome::Refused) => {
                        self.mark_termination(application);
                        self.logger.warn(format!(
                            "macOS refused to terminate {bundle_id}; attempting the replacement anyway"
                        ));
                    }
                    Err(error) => {
                        self.mark_termination(application);
                        self.logger
                            .warn(format!("failed to request termination of {bundle_id}: {error}"));
                    }
                }
            }

            let Some((config, token)) = self.current() else {
                return;
            };
            self.reconcile_pending(token);
            if self.replacement_decided(application) {
                return;
            }
            let target = match config.target() {
                Ok(target) => target,
                Err(error) => {
                    // Validation rejects an invalid value before acceptance; a
                    // staged one can still reach a worker, so block instead of
                    // guessing a replacement.
                    self.mark_replacement(application);
                    self.logger.warn(format!("blocked {bundle_id}; {error}"));
                    return;
                }
            };
            let Some(target) = target else {
                self.mark_replacement(application);
                self.logger.info(format!(
                    "blocked {bundle_id}; no replacement application is configured"
                ));
                return;
            };
            match self.decide_replacement(application, &bundle_id, &target, token) {
                Decision::Done => return,
                Decision::Retry => {}
            }
        }
        self.logger.warn(format!(
            "gave up blocking {bundle_id} after repeated policy changes"
        ));
    }

    fn decide_replacement(
        &mut self,
        application: ApplicationId,
        blocked: &str,
        target: &ReplacementTarget,
        token: AcceptedActionToken,
    ) -> Decision {
        // Resolution and the running check happen before the accepted revision
        // is re-validated, and no decision is recorded for the instance until
        // that re-validation passes, so obsolete policy can never consume its
        // current-target event.
        let bundle_id = match self.target_bundle_id(application, target, token) {
            Ok(bundle_id) => bundle_id,
            Err(decision) => return decision,
        };
        let running = match self.observation.running_applications(&bundle_id) {
            Ok(running) => running,
            Err(error) => {
                self.logger.warn(format!(
                    "cannot check whether replacement {bundle_id} is running: {error}"
                ));
                return Decision::Done;
            }
        };
        let Some((config, current)) = self.current() else {
            return Decision::Done;
        };
        if current != token || config.target().ok().flatten().as_ref() != Some(target) {
            return Decision::Retry;
        }
        self.reconcile_pending(current);
        if !running.is_empty() {
            self.mark_replacement(application);
            self.logger.info(format!(
                "blocked {blocked}; replacement {bundle_id} is already running"
            ));
            return Decision::Done;
        }
        let path = match self.target_open_path(application, target, token) {
            Ok(path) => path,
            Err(decision) => return decision,
        };
        if self.state.pending.is_some() {
            // Joining a pending replacement is a complete decision for this
            // instance: a duplicate launch after the completion must not open
            // a second player.
            self.mark_replacement(application);
            self.logger
                .debug("a replacement open is already pending; not requesting another");
            return Decision::Done;
        }

        // The reservation is visible before the native call because an open
        // completion may be delivered synchronously from inside it.
        self.state.next_nonce = self.state.next_nonce.wrapping_add(1);
        let nonce = self.state.next_nonce;
        self.state.pending = Some(PendingOpen { nonce, token });
        let inbox = self.inbox.clone();
        let completion = move |completion: OpenCompletion| {
            let (current, context, result) = match completion {
                OpenCompletion::Current(context, result) => (true, context, result),
                OpenCompletion::Stale(context, result) => (false, context, result),
            };
            let _ = inbox.send(Inbox::Opened {
                nonce,
                context,
                current,
                result,
            });
        };
        let display = path.display();
        match self.observation.open(token, &path, completion) {
            Ok(OpenRequest::Issued) => {
                self.mark_replacement(application);
                self.logger
                    .info(format!("requested opening replacement {bundle_id} at {display}"));
                Decision::Done
            }
            Ok(OpenRequest::Stale) => {
                self.clear_pending(nonce);
                self.logger
                    .debug("reconsidering the replacement under the current policy revision");
                Decision::Retry
            }
            Err(ApplicationError::TimedOut) => {
                // The request may have reached macOS before the timeout. It
                // cannot be cancelled, so it must not be retried blindly.
                self.mark_replacement(application);
                self.logger.warn(format!(
                    "opening replacement {bundle_id} timed out; the request may still complete"
                ));
                Decision::Done
            }
            Err(error) => {
                self.clear_pending(nonce);
                self.mark_replacement(application);
                self.logger.warn(format!("failed to open replacement {bundle_id}: {error}"));
                Decision::Done
            }
        }
    }

    /// The bundle identifier `target` resolves to for this instance.
    ///
    /// A path target asks the native boundary; a bundle-identifier target is
    /// already one. A path with no application bundle is a permanent answer for
    /// the accepted policy, but the instance is only consumed after the
    /// re-validation that [`PolicyWorker::decide_replacement`] performs, so a
    /// superseded decision returns a retry instead.
    fn target_bundle_id(
        &mut self,
        application: ApplicationId,
        target: &ReplacementTarget,
        token: AcceptedActionToken,
    ) -> Result<String, Decision> {
        match target {
            ReplacementTarget::BundleId(bundle_id) => Ok(bundle_id.clone()),
            ReplacementTarget::Path(path) => {
                match self.observation.bundle_identifier_at_path(path) {
                    Ok(Some(bundle_id)) => Ok(bundle_id),
                    Ok(None) => {
                        if !self.is_accepted(token, target) {
                            return Err(Decision::Retry);
                        }
                        self.mark_replacement(application);
                        self.logger.warn(format!(
                            "no application bundle at {}; not launching a replacement",
                            path.display()
                        ));
                        Err(Decision::Done)
                    }
                    Err(error) => {
                        // A failed query is never a decision: a later
                        // notification may retry it.
                        self.logger.warn(format!(
                            "cannot read the bundle identifier at {}: {error}",
                            path.display()
                        ));
                        Err(Decision::Done)
                    }
                }
            }
        }
    }

    /// The path to open for `target`.
    ///
    /// A path target opens exactly that path; a bundle-identifier target asks
    /// the native boundary for the installed path.
    ///
    /// A missing installed application is a permanent answer for the accepted
    /// policy, but the instance is only consumed after the re-validation that
    /// [`PolicyWorker::decide_replacement`] performs. A reconfigure that lands
    /// between that re-validation and this query therefore returns a retry
    /// instead of marking the instance decided under a superseded target.
    fn target_open_path(
        &mut self,
        application: ApplicationId,
        target: &ReplacementTarget,
        token: AcceptedActionToken,
    ) -> Result<PathBuf, Decision> {
        match target {
            ReplacementTarget::Path(path) => Ok(path.clone()),
            ReplacementTarget::BundleId(bundle_id) => {
                match self.observation.application_path(bundle_id) {
                    Ok(Some(path)) => Ok(path),
                    Ok(None) => {
                        if !self.is_accepted(token, target) {
                            return Err(Decision::Retry);
                        }
                        self.mark_replacement(application);
                        self.logger.warn(format!(
                            "{bundle_id} is not installed; not launching a replacement"
                        ));
                        Err(Decision::Done)
                    }
                    Err(error) => {
                        self.logger.warn(format!(
                            "cannot resolve the application path for {bundle_id}: {error}"
                        ));
                        Err(Decision::Done)
                    }
                }
            }
        }
    }

    /// Whether the accepted revision still names `target`.
    ///
    /// This reads the snapshot and releases it; it is never held across a native
    /// call.
    fn is_accepted(&self, token: AcceptedActionToken, target: &ReplacementTarget) -> bool {
        self.current().is_some_and(|(config, current)| {
            current == token && config.target().ok().flatten().as_ref() == Some(target)
        })
    }

    fn handle_opened(
        &mut self,
        nonce: u64,
        context: OpenCompletionContext,
        current: bool,
        result: &Result<(), ApplicationError>,
    ) -> bool {
        match result {
            Ok(()) => self
                .logger
                .info(format!("replacement open request {} succeeded", context.request)),
            Err(error) => self.logger.warn(format!(
                "replacement open request {} failed: {error}",
                context.request
            )),
        }
        if !current {
            self.logger.debug(format!(
                "ignoring the stale result of open request {}",
                context.request
            ));
            return false;
        }
        if self.stopped() {
            self.logger.debug("ignoring an open completion after the runtime stopped");
            return false;
        }
        // The completion was current when the callback was delivered, but it may
        // have been queued across a reconfigure. Only a reservation made by this
        // exact request under the still-accepted revision may be cleared.
        let Some((_, token)) = self.current() else {
            return false;
        };
        if self
            .state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.nonce == nonce && pending.token == token)
        {
            self.state.pending = None;
            true
        } else {
            self.logger.debug(format!(
                "ignoring the superseded completion of open request {}",
                context.request
            ));
            false
        }
    }

    fn handle_config(&mut self) {
        let Some((_, token)) = self.current() else {
            return;
        };
        self.reconcile_pending(token);
    }

    /// Drops a reservation that belongs to a superseded accepted revision.
    ///
    /// Both an explicit config event and the next queued action run this, so a
    /// lost or reordered notification cannot pin an old-revision reservation
    /// forever. A stale completion never clears a reservation.
    fn reconcile_pending(&mut self, token: AcceptedActionToken) {
        if self.state.pending.as_ref().is_some_and(|pending| pending.token != token) {
            self.state.pending = None;
            self.logger.debug(
                "discarding a pending replacement reserved under a superseded policy revision",
            );
        }
    }

    fn current(&self) -> Option<(NoTunesConfig, AcceptedActionToken)> {
        if self.stopped() {
            return None;
        }
        let snapshot = lock(&self.snapshot);
        let token = snapshot.token?;
        Some((snapshot.config.clone(), token))
    }

    fn stopped(&self) -> bool {
        !self.active.load(Ordering::SeqCst) || self.cancellation.is_cancelled()
    }

    fn termination_requested(&self, application: ApplicationId) -> bool {
        self.state
            .live
            .get(&application)
            .is_some_and(|record| record.termination_requested)
    }

    fn mark_termination(&mut self, application: ApplicationId) {
        self.state.live.entry(application).or_default().termination_requested = true;
    }

    fn replacement_decided(&self, application: ApplicationId) -> bool {
        self.state
            .live
            .get(&application)
            .is_some_and(|record| record.replacement_decided)
    }

    fn mark_replacement(&mut self, application: ApplicationId) {
        self.state.live.entry(application).or_default().replacement_decided = true;
    }

    fn clear_pending(&mut self, nonce: u64) {
        if self.state.pending.as_ref().is_some_and(|pending| pending.nonce == nonce) {
            self.state.pending = None;
        }
    }
}

enum Decision {
    Done,
    Retry,
}

/// Terminates every instance of both blocked applications once.
///
/// The sweep runs before the worker is spawned and never opens a replacement.
/// Its markers are visible to the queued launch notifications, so an overlap
/// cannot duplicate a termination or miss the single replacement decision.
///
/// Cancellation and observation invalidation are rechecked between native
/// operations, so a stop that lands during the sweep stops issuing new work.
/// Returns `true` when the sweep did not finish for that reason.
pub fn sweep(
    observation: &ApplicationObservation,
    token: AcceptedActionToken,
    state: &mut WorkerState,
    logger: &ScopedLogger,
    cancellation: &CancellationToken,
    active: &AtomicBool,
) -> bool {
    'sweep: for bundle_id in BLOCKED_BUNDLE_IDS {
        if sweep_interrupted(cancellation, active, observation, token) {
            return true;
        }
        let applications = match observation.running_applications(bundle_id) {
            Ok(applications) => applications,
            Err(error) => {
                logger.warn(format!("failed to list running {bundle_id} instances: {error}"));
                continue;
            }
        };
        for application in applications {
            if sweep_interrupted(cancellation, active, observation, token) {
                return true;
            }
            if state.retired.contains(&application) {
                continue;
            }
            let requested =
                state.live.get(&application).is_some_and(|record| record.termination_requested);
            if requested {
                continue;
            }
            match observation.terminate(token, application) {
                Ok(TerminateOutcome::Stale) => {
                    logger.warn(
                        "the startup sweep stopped because its policy revision is no longer current",
                    );
                    break 'sweep;
                }
                outcome => {
                    state.live.entry(application).or_default().termination_requested = true;
                    match outcome {
                        Ok(TerminateOutcome::Requested) => logger.info(format!(
                            "requested termination of {bundle_id} during the startup sweep (exit is not confirmed)"
                        )),
                        Ok(TerminateOutcome::NotRunning) => logger.debug(format!(
                            "{bundle_id} was already no longer running during the startup sweep"
                        )),
                        Ok(TerminateOutcome::Refused) => logger.warn(format!(
                            "macOS refused to terminate {bundle_id} during the startup sweep"
                        )),
                        Err(error) => logger.warn(format!(
                            "failed to request termination of {bundle_id} during the startup sweep: {error}"
                        )),
                        Ok(TerminateOutcome::Stale) => {}
                    }
                }
            }
        }
    }
    false
}

fn sweep_interrupted(
    cancellation: &CancellationToken,
    active: &AtomicBool,
    observation: &ApplicationObservation,
    token: AcceptedActionToken,
) -> bool {
    cancellation.is_cancelled() || !active.load(Ordering::SeqCst) || !observation.is_current(token)
}

fn is_blocked(bundle_id: &str) -> bool { BLOCKED_BUNDLE_IDS.contains(&bundle_id) }
