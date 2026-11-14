//! Two typed steps: prepare every output, then apply sequentially.
//!
//! [`prepare`] snapshots the displays once, resolves every selected target,
//! reads the source once and produces or reuses one validated output per
//! target. [`prepare_plan`] takes the caller's already-resolved per-display
//! plan, so a caller that snapshotted once can prepare mixed sources and sizes
//! without another snapshot. All outputs exist before [`Prepared::apply`] or
//! [`Prepared::apply_guarded`] makes its first native call, so a processing
//! failure changes no desktop. Both apply paths report the truth about partial
//! work: applied, failed, not attempted and unknown identities.
//!
//! The returned [`Prepared`] owns one shared cache lease, so the lease scope
//! from the first cache read through the last native acknowledgement is
//! enforced by the type. [`Prepared::apply_guarded`] additionally hands that
//! lease to every guarded native operation as a keepalive, so a late OS call
//! cannot outlive the cache file it reads. Preparation of several distinct
//! outputs runs on scoped processing threads that all join before it returns;
//! a plan with one distinct output runs on the calling thread. Apply spawns no
//! thread: a standalone caller runs it on its main thread, while a host caller
//! runs it on a coordinator worker. Workers never wait on the main thread.
//!
//! A host caller passes its per-process memo in [`PreparePlanDeps`]. Each
//! distinct source is then inspected once per plan (resolved, checked as a
//! regular non-link file within the byte limit); when its fingerprint matches
//! the one recorded after an earlier read, the recorded digest keys the cache
//! lookup and the source is read only if that output must be rebuilt. A cache
//! hit whose fingerprint matches the one recorded after a full validation
//! skips the PNG decode. Workers share the memo; its lock is never held
//! across file I/O.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use stache_macos::{
    DesktopBackend, DesktopClock, DesktopError, DesktopKeepalive, DesktopOperation, DisplayEntry,
    DisplayId, DisplaySnapshot, GuardedDesktopBackend, NativeOperationTracker,
};
use stache_sdk::{AppPaths, CacheLease, CacheLeaseError, CancellationToken, LeaseClock, LeaseWait};

use crate::cache::{Cache, CancelLeaseCommit, PublishCommit, PublishSeam, cache_key};
use crate::limits::{self, ResourceError};
use crate::memo::{FileFingerprint, PrepareMemo};
use crate::processing::{self, Effects, ProcessError};
use crate::selection::{
    ScreenTarget, SelectedTarget, SelectionError, TargetPixels, resolve_targets,
};
use crate::source;

/// How long one prepare waits for a shared cache lease.
///
/// This mirrors the existing five-second `cache clear` window so a busy cache
/// fails with a familiar finite wait. It is a coordination budget, not a
/// processing deadline.
pub const PREPARE_LEASE_BUDGET: Duration = Duration::from_secs(5);

/// The inputs [`prepare`] needs.
pub struct PrepareDeps<'a> {
    /// The desktop backend used for the one snapshot.
    pub desktop: &'a dyn DesktopBackend,
    /// The identity paths that own the cache and lease.
    pub paths: &'a AppPaths,
}

/// The inputs [`prepare_plan`] needs.
///
/// The caller already snapshotted the displays and resolved one
/// [`SelectedTarget`] per addressed display; no desktop backend is read here.
pub struct PreparePlanDeps<'a> {
    /// The identity paths that own the cache and lease.
    pub paths: &'a AppPaths,
    /// The host's per-process memo of validated outputs and source digests.
    /// Standalone callers pass `None` and every lookup decodes, every source
    /// is read.
    pub memo: Option<&'a PrepareMemo>,
}

/// One display's validated target and the one source it should use.
#[derive(Clone, Debug)]
pub struct PlannedTarget {
    /// The display and its validated target pixels.
    pub target: SelectedTarget,
    /// The one source image for this target.
    pub source: PathBuf,
    /// The effects applied to this target.
    pub effects: Effects,
}

/// One prepared output.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedTarget {
    /// The snapshot entry this output was prepared for.
    pub entry: DisplayEntry,
    /// The validated cache file.
    pub image: PathBuf,
}

/// Every output for one plan, holding the lease through native application.
#[must_use]
pub struct Prepared {
    // Held for the whole prepare/apply scope. Guarded native calls also clone
    // this handle as their keepalive, so a late OS call keeps it alive.
    lease: Arc<CacheLease>,
    targets: Vec<PreparedTarget>,
}

impl Prepared {
    /// The prepared outputs in selection order.
    #[must_use]
    pub fn targets(&self) -> &[PreparedTarget] { &self.targets }

    /// Applies every prepared output in order through the unguarded boundary.
    ///
    /// Cancellation and `still_current` are checked before each call and can
    /// only suppress future calls. The first failure or unknown outcome stops
    /// the sequence; remaining identities become `not_attempted`.
    ///
    /// This is the Part 02-compatible path. Host callers use
    /// [`Prepared::apply_guarded`].
    ///
    /// # Errors
    /// Returns a lease error, before any native call, when the held lease can
    /// no longer be verified.
    pub fn apply(
        &self,
        backend: &dyn DesktopBackend,
        cancel: &CancellationToken,
        still_current: &dyn Fn() -> bool,
    ) -> Result<ApplyOutcome, CacheLeaseError> {
        self.lease.verify()?;
        let ids: Vec<DisplayId> = self.targets.iter().map(|target| target.entry.id).collect();
        let mut outcome = ApplyOutcome::default();
        for (index, target) in self.targets.iter().enumerate() {
            if cancel.is_cancelled() || !still_current() {
                outcome.not_attempted.extend(ids[index..].iter().copied());
                break;
            }
            match backend.set_image(target.entry.id, &target.entry, &target.image) {
                Ok(()) => outcome.applied.push(target.entry.id),
                Err(error @ DesktopError::Unknown { .. }) => {
                    outcome.unknown.push((target.entry.id, error));
                    outcome.not_attempted.extend(ids[index + 1..].iter().copied());
                    break;
                }
                Err(error) => {
                    outcome.failed.push((target.entry.id, error));
                    outcome.not_attempted.extend(ids[index + 1..].iter().copied());
                    break;
                }
            }
        }
        Ok(outcome)
    }

    /// Applies every prepared output in order through the guarded boundary.
    ///
    /// Each call carries the caller's cancellation token, deadline and pure
    /// current-generation/revision predicate, plus this value's cache lease as
    /// a keepalive. The lease therefore stays held until an issued native call
    /// finishes even when a cancellation race reports
    /// [`DesktopError::Unknown`].
    ///
    /// A cancelled, expired or stale refusal makes that display and the rest
    /// `not_attempted`; an unknown outcome makes the rest `not_attempted`.
    /// Already-issued calls may finish but cannot start another change.
    ///
    /// # Errors
    /// Returns a lease error, before any native call, when the held lease can
    /// no longer be verified.
    #[allow(clippy::needless_pass_by_value)]
    pub fn apply_guarded(
        &self,
        backend: &dyn GuardedDesktopBackend,
        cancel: &CancellationToken,
        still_current: Arc<dyn Fn() -> bool + Send + Sync>,
        deadline: Option<Instant>,
        clock: Arc<dyn DesktopClock>,
    ) -> Result<ApplyOutcome, CacheLeaseError> {
        self.apply_guarded_with_tracker(backend, cancel, still_current, deadline, clock, None)
    }

    /// [`Prepared::apply_guarded`] with a shared native-operation tracker.
    ///
    /// Before each native call the loop waits until no predecessor may still
    /// issue or is still issuing a native change. That is the serialization
    /// point a host needs when an earlier call returned
    /// [`DesktopError::Unknown`] while its main-thread closure kept running: the
    /// successor is prepared, but it never issues until the predecessor
    /// resolves. If the wait is cancelled or its deadline passes, the current
    /// and remaining displays become `not_attempted` and no call is made.
    ///
    /// # Errors
    /// Returns a lease error, before any native call, when the held lease can
    /// no longer be verified.
    #[allow(clippy::needless_pass_by_value)]
    pub fn apply_guarded_with_tracker(
        &self,
        backend: &dyn GuardedDesktopBackend,
        cancel: &CancellationToken,
        still_current: Arc<dyn Fn() -> bool + Send + Sync>,
        deadline: Option<Instant>,
        clock: Arc<dyn DesktopClock>,
        tracker: Option<NativeOperationTracker>,
    ) -> Result<ApplyOutcome, CacheLeaseError> {
        self.lease.verify()?;
        let ids: Vec<DisplayId> = self.targets.iter().map(|target| target.entry.id).collect();
        let lease: Arc<CacheLease> = Arc::clone(&self.lease);
        let keepalive: Arc<dyn DesktopKeepalive> = lease;
        let mut outcome = ApplyOutcome::default();
        for (index, target) in self.targets.iter().enumerate() {
            if cancel.is_cancelled() || !(still_current)() {
                outcome.not_attempted.extend(ids[index..].iter().copied());
                break;
            }
            if let Some(tracker) = &tracker
                && !tracker.wait_unresolved(cancel, deadline, clock.as_ref())
            {
                outcome.not_attempted.extend(ids[index..].iter().copied());
                break;
            }
            let mut operation = DesktopOperation::new(cancel.clone(), Arc::clone(&still_current))
                .with_clock(Arc::clone(&clock))
                .with_keepalive(Arc::clone(&keepalive));
            if let Some(tracker) = &tracker {
                operation = operation.with_tracker(tracker.clone());
            }
            if let Some(deadline) = deadline {
                operation = operation.with_deadline(deadline);
            }
            match backend.set_image_guarded(
                &operation,
                target.entry.id,
                &target.entry,
                &target.image,
            ) {
                Ok(()) => outcome.applied.push(target.entry.id),
                Err(error @ DesktopError::Unknown { .. }) => {
                    outcome.unknown.push((target.entry.id, error));
                    outcome.not_attempted.extend(ids[index + 1..].iter().copied());
                    break;
                }
                Err(
                    DesktopError::Cancelled { .. }
                    | DesktopError::Expired { .. }
                    | DesktopError::Stale { .. },
                ) => {
                    outcome.not_attempted.extend(ids[index..].iter().copied());
                    break;
                }
                Err(error) => {
                    outcome.failed.push((target.entry.id, error));
                    outcome.not_attempted.extend(ids[index + 1..].iter().copied());
                    break;
                }
            }
        }
        Ok(outcome)
    }
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared")
            .field("targets", &self.targets)
            .finish_non_exhaustive()
    }
}

/// The observed result of one apply sequence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApplyOutcome {
    /// Displays whose native call returned success, in call order.
    pub applied: Vec<DisplayId>,
    /// Displays whose native call returned a known failure.
    pub failed: Vec<(DisplayId, DesktopError)>,
    /// Displays for which no call was made.
    pub not_attempted: Vec<DisplayId>,
    /// Displays whose native call outcome is not known to this process.
    pub unknown: Vec<(DisplayId, DesktopError)>,
}

impl ApplyOutcome {
    /// Whether every requested display was applied.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        !self.applied.is_empty()
            && self.failed.is_empty()
            && self.not_attempted.is_empty()
            && self.unknown.is_empty()
    }
}

/// Why preparation failed.
#[derive(Debug)]
pub enum PrepareError {
    /// The desktop snapshot could not be read.
    Snapshot(DesktopError),
    /// The selector or display geometry is unusable.
    Selection(SelectionError),
    /// Two planned targets address the same display.
    DuplicateDisplay(DisplayId),
    /// An effect exceeds an internal bound.
    Resource(ResourceError),
    /// The shared cache lease was busy, cancelled or unsafe.
    Lease(CacheLeaseError),
    /// Reading, processing or publishing one output failed.
    Process(ProcessError),
    /// Cancellation was requested before preparation completed.
    Cancelled,
}

impl std::fmt::Display for PrepareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Snapshot(error) => error.fmt(f),
            Self::Selection(error) => error.fmt(f),
            Self::DuplicateDisplay(display) => {
                write!(f, "display {} appears more than once in the plan", display.raw())
            }
            Self::Resource(error) => error.fmt(f),
            Self::Lease(error) => error.fmt(f),
            Self::Process(error) => error.fmt(f),
            Self::Cancelled => f.write_str("wallpaper preparation was cancelled"),
        }
    }
}

impl std::error::Error for PrepareError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Snapshot(error) => Some(error),
            Self::Selection(error) => Some(error),
            Self::DuplicateDisplay(_) | Self::Cancelled => None,
            Self::Resource(error) => Some(error),
            Self::Lease(error) => Some(error),
            Self::Process(error) => Some(error),
        }
    }
}

impl From<SelectionError> for PrepareError {
    fn from(error: SelectionError) -> Self { Self::Selection(error) }
}

impl From<ResourceError> for PrepareError {
    fn from(error: ResourceError) -> Self { Self::Resource(error) }
}

impl From<CacheLeaseError> for PrepareError {
    fn from(error: CacheLeaseError) -> Self { Self::Lease(error) }
}

impl From<ProcessError> for PrepareError {
    fn from(error: ProcessError) -> Self { Self::Process(error) }
}

impl From<crate::cache::CacheError> for PrepareError {
    fn from(error: crate::cache::CacheError) -> Self { Self::Process(ProcessError::Cache(error)) }
}

/// Snapshots, resolves and prepares every selected output before any set call.
///
/// # Errors
/// Fails before processing for an invalid selector, missing display or
/// unusable geometry, and before any native call for any processing or cache
/// failure. The cache directory is not created when selection fails.
pub fn prepare(
    targets: ScreenTarget,
    source: &Path,
    effects: Effects,
    deps: &PrepareDeps<'_>,
    cancel: &CancellationToken,
) -> Result<Prepared, PrepareError> {
    prepare_with(
        targets,
        source,
        effects,
        deps,
        cancel,
        None,
        &crate::cache::NoFailures,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn prepare_with(
    targets: ScreenTarget,
    source: &Path,
    effects: Effects,
    deps: &PrepareDeps<'_>,
    cancel: &CancellationToken,
    clock: Option<Arc<dyn LeaseClock>>,
    seam: &dyn PublishSeam,
) -> Result<Prepared, PrepareError> {
    limits::check_blur(effects.blur)?;
    check_cancel(cancel)?;

    let snapshot: DisplaySnapshot = deps.desktop.snapshot().map_err(PrepareError::Snapshot)?;
    let selected = resolve_targets(&snapshot, &targets)?;
    let plan: Vec<PlannedTarget> = selected
        .into_iter()
        .map(|target| PlannedTarget {
            target,
            source: source.to_path_buf(),
            effects,
        })
        .collect();
    prepare_plan_with(
        &plan,
        &PreparePlanDeps { paths: deps.paths, memo: None },
        cancel,
        clock,
        seam,
    )
}

/// Prepares one caller-resolved display/source plan before any set call.
///
/// The caller snapshotted the displays once and resolved the targets; this
/// function never reads the desktop. It validates every display, target and
/// effect before reading a source or touching the cache, refuses duplicate
/// display identities, and prepares every output before returning. Each
/// distinct source is read and hashed once and decoded at most once, each
/// distinct output (same source, pixels and effects) is produced once, and
/// distinct outputs run on at most
/// `min(available_parallelism, MAX_CONCURRENT_WORKERS)` scoped threads. A
/// source's bytes and decode are released when its last output finishes, so
/// at most [`limits::MAX_CONCURRENT_WORKERS`] decoded sources are resident.
///
/// # Errors
/// Fails before processing for an empty plan, duplicate displays or unusable
/// geometry, and before any native call for any processing or cache failure.
/// The cache directory is not created when validation fails.
pub fn prepare_plan(
    plan: &[PlannedTarget],
    deps: &PreparePlanDeps<'_>,
    cancel: &CancellationToken,
) -> Result<Prepared, PrepareError> {
    prepare_plan_with(plan, deps, cancel, None, &crate::cache::NoFailures)
}

/// [`prepare_plan`] with an injected lease clock and publication seam.
#[allow(clippy::too_many_arguments)]
pub fn prepare_plan_with(
    plan: &[PlannedTarget],
    deps: &PreparePlanDeps<'_>,
    cancel: &CancellationToken,
    clock: Option<Arc<dyn LeaseClock>>,
    seam: &dyn PublishSeam,
) -> Result<Prepared, PrepareError> {
    prepare_plan_observed(
        plan,
        deps,
        cancel,
        clock,
        seam,
        &NoPlanObserver,
        crate::batch::worker_count(),
    )
}

/// Observes plan preparation at known points.
///
/// Production passes [`NoPlanObserver`]. Tests use it to hold workers at a
/// known point and to count reads, decodes and resident sources without
/// touching production behavior.
pub trait PlanObserver: Sync {
    /// Runs on the worker when it starts one distinct output, identified by the
    /// lowest plan index that uses it.
    fn output_started(&self, _plan_index: usize) {}

    /// Runs on the worker when that output finished, successfully or not.
    fn output_finished(&self, _plan_index: usize) {}

    /// Runs after a source was read and hashed.
    fn source_read(&self, _source: &Path) {}

    /// Runs after a source was decoded.
    fn source_decoded(&self, _source: &Path) {}

    /// Runs before a cache output is fully decoded for validation.
    fn output_decoded(&self, _output: &Path) {}

    /// Runs after a read source's bytes and decode were released.
    fn source_released(&self, _source: &Path) {}
}

/// The production plan observer that ignores every event.
pub struct NoPlanObserver;

impl PlanObserver for NoPlanObserver {}

/// [`prepare_plan_with`] with an injected observer and worker count.
///
/// The count is clamped to `1..=MAX_CONCURRENT_WORKERS` and to the number of
/// distinct outputs, so no caller can exceed the production bound.
#[allow(clippy::too_many_arguments)]
pub fn prepare_plan_observed(
    plan: &[PlannedTarget],
    deps: &PreparePlanDeps<'_>,
    cancel: &CancellationToken,
    clock: Option<Arc<dyn LeaseClock>>,
    seam: &dyn PublishSeam,
    observer: &dyn PlanObserver,
    workers: usize,
) -> Result<Prepared, PrepareError> {
    validate_plan(plan)?;
    check_cancel(cancel)?;

    let wait = clock.map_or_else(
        || LeaseWait::new(PREPARE_LEASE_BUDGET, cancel.clone()),
        |clock| LeaseWait::with_clock(PREPARE_LEASE_BUDGET, cancel.clone(), clock),
    );
    // The lease is acquired before the first cache read and held by the
    // returned value through native acknowledgement of the last set call.
    let lease = Arc::new(CacheLease::shared(deps.paths, &wait)?);
    check_cancel(cancel)?;

    let cache = Cache::open(deps.paths)?;
    let work = PlanWork::new(plan)?;
    let commit = CancelLeaseCommit { cancel, lease: lease.as_ref() };
    let run = PlanRun {
        work: &work,
        cache: &cache,
        lease: lease.as_ref(),
        cancel,
        seam,
        commit: &commit,
        observer,
        memo: deps.memo,
        stop: AtomicBool::new(false),
        schedule: Mutex::new(Schedule::new(&work)),
        results: Mutex::new((0..work.outputs.len()).map(|_| None).collect()),
    };
    let workers = workers.clamp(1, limits::MAX_CONCURRENT_WORKERS).min(work.outputs.len());
    if workers <= 1 {
        run.worker();
    } else {
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| run.worker());
            }
        });
    }

    let results = run.results.into_inner().unwrap_or_else(PoisonError::into_inner);
    // Outputs are ordered by source, so the error of the lowest plan index is
    // chosen explicitly rather than by completion or queue order.
    let failure = results
        .iter()
        .zip(&work.outputs)
        .filter_map(|(result, output)| match result {
            Some(Err(Halt::Failed(_))) => Some(output.plan_indices[0]),
            _ => None,
        })
        .min();
    if let Some(first) = failure {
        let index = work.output_of[first];
        if let Some(Some(Err(Halt::Failed(error)))) = results.into_iter().nth(index) {
            return Err(error);
        }
        return Err(PrepareError::Cancelled);
    }
    let mut prepared = Vec::with_capacity(plan.len());
    for (planned, output) in plan.iter().zip(&work.output_of) {
        let Some(Ok(image)) = &results[*output] else {
            return Err(PrepareError::Cancelled);
        };
        prepared.push(PreparedTarget {
            entry: planned.target.entry,
            image: image.clone(),
        });
    }
    Ok(Prepared { lease, targets: prepared })
}

fn validate_plan(plan: &[PlannedTarget]) -> Result<(), PrepareError> {
    if plan.is_empty() {
        return Err(PrepareError::Selection(SelectionError::NoDisplays));
    }
    let mut seen = BTreeSet::new();
    for planned in plan {
        limits::check_blur(planned.effects.blur)?;
        if !seen.insert(planned.target.entry.id) {
            return Err(PrepareError::DuplicateDisplay(planned.target.entry.id));
        }
        // Geometry and target bounds fail before any read or cache write.
        crate::selection::target_pixels(&planned.target.entry)?;
    }
    Ok(())
}

/// The distinct sources and outputs of one plan.
///
/// Outputs are ordered so every output of one source is adjacent, sources in
/// order of first appearance. That order is what bounds resident decodes.
struct PlanWork<'p> {
    sources: Vec<PlanSource<'p>>,
    outputs: Vec<PlanOutput>,
    /// The output index for each plan index.
    output_of: Vec<usize>,
}

/// One distinct source path and its lazily read bytes and decode.
struct PlanSource<'p> {
    path: &'p Path,
    slot: Mutex<SourceSlot>,
}

/// One source's bounded bytes and its optional single decode.
#[derive(Default)]
struct SourceSlot {
    /// Whether the source was inspected against the memo in this plan.
    inspected: bool,
    /// The recorded canonical path and digest the inspection matched.
    recorded: Option<(PathBuf, [u8; 32])>,
    bytes: Option<Arc<source::SourceBytes>>,
    decoded: Option<Arc<image::DynamicImage>>,
}

/// One distinct output: same source, pixels and effects.
struct PlanOutput {
    source: usize,
    pixels: TargetPixels,
    effects: Effects,
    /// Every plan index that uses this output, ascending.
    plan_indices: Vec<usize>,
}

impl<'p> PlanWork<'p> {
    fn new(plan: &'p [PlannedTarget]) -> Result<Self, PrepareError> {
        let mut sources: Vec<PlanSource<'p>> = Vec::new();
        let mut outputs: Vec<PlanOutput> = Vec::new();
        for (plan_index, planned) in plan.iter().enumerate() {
            // Recompute the target instead of trusting the carried value.
            let pixels = crate::selection::target_pixels(&planned.target.entry)?;
            let source = sources
                .iter()
                .position(|source| source.path == planned.source)
                .unwrap_or_else(|| {
                    sources.push(PlanSource {
                        path: &planned.source,
                        slot: Mutex::new(SourceSlot::default()),
                    });
                    sources.len() - 1
                });
            match outputs.iter_mut().find(|output| {
                output.source == source
                    && output.pixels == pixels
                    && output.effects == planned.effects
            }) {
                Some(output) => output.plan_indices.push(plan_index),
                None => outputs.push(PlanOutput {
                    source,
                    pixels,
                    effects: planned.effects,
                    plan_indices: vec![plan_index],
                }),
            }
        }
        // Stable: within a source, outputs keep plan order.
        outputs.sort_by_key(|output| output.source);
        let mut output_of = vec![0; plan.len()];
        for (index, output) in outputs.iter().enumerate() {
            for plan_index in &output.plan_indices {
                output_of[*plan_index] = index;
            }
        }
        Ok(Self { sources, outputs, output_of })
    }
}

/// Which outputs are taken and how many of each source are in flight.
struct Schedule {
    next: usize,
    untaken: Vec<usize>,
    in_flight: Vec<usize>,
}

impl Schedule {
    fn new(work: &PlanWork<'_>) -> Self {
        let mut untaken = vec![0; work.sources.len()];
        for output in &work.outputs {
            untaken[output.source] += 1;
        }
        Self {
            next: 0,
            untaken,
            in_flight: vec![0; work.sources.len()],
        }
    }
}

/// Why one output did not produce a file.
enum Halt {
    /// This output failed or observed cancellation.
    Failed(PrepareError),
    /// Another output failed first; this one stopped at a boundary.
    Stopped,
}

impl<E: Into<PrepareError>> From<E> for Halt {
    fn from(error: E) -> Self { Self::Failed(error.into()) }
}

/// The shared state of one plan run, borrowed by every worker.
struct PlanRun<'a, 'p> {
    work: &'a PlanWork<'p>,
    cache: &'a Cache,
    lease: &'a CacheLease,
    cancel: &'a CancellationToken,
    seam: &'a dyn PublishSeam,
    commit: &'a dyn PublishCommit,
    observer: &'a dyn PlanObserver,
    memo: Option<&'a PrepareMemo>,
    stop: AtomicBool,
    schedule: Mutex<Schedule>,
    results: Mutex<Vec<Option<Result<PathBuf, Halt>>>>,
}

impl PlanRun<'_, '_> {
    fn worker(&self) {
        let mut finished = None;
        loop {
            let (taken, release) = self.finish_and_take(finished.take());
            if let Some(source) = release {
                self.release(source);
            }
            let Some(index) = taken else { break };
            let first = self.work.outputs[index].plan_indices[0];
            self.observer.output_started(first);
            let result = self.produce(index);
            if result.is_err() {
                // Set before the finish event so every other worker sees the
                // stop at its next boundary.
                self.stop.store(true, Ordering::SeqCst);
            }
            self.observer.output_finished(first);
            lock(&self.results)[index] = Some(result);
            finished = Some(index);
        }
    }

    /// Finishes the previous output and takes the next one in one critical
    /// section, so a source with untaken outputs is never left without a
    /// worker while the run continues.
    ///
    /// A source's bytes and decode are released once nothing of it is in
    /// flight and it has no untaken output, or the run stopped. Every resident
    /// source therefore has an output in flight, so at most one per worker.
    fn finish_and_take(&self, finished: Option<usize>) -> (Option<usize>, Option<usize>) {
        let mut schedule = lock(&self.schedule);
        let stopped = self.stop.load(Ordering::SeqCst) || self.cancel.is_cancelled();
        let taken = if !stopped && schedule.next < self.work.outputs.len() {
            let index = schedule.next;
            let source = self.work.outputs[index].source;
            schedule.next += 1;
            schedule.untaken[source] -= 1;
            schedule.in_flight[source] += 1;
            Some(index)
        } else {
            None
        };
        let release = finished.map(|index| self.work.outputs[index].source).filter(|&source| {
            schedule.in_flight[source] -= 1;
            schedule.in_flight[source] == 0 && (schedule.untaken[source] == 0 || stopped)
        });
        drop(schedule);
        (taken, release)
    }

    fn release(&self, source: usize) {
        let plan_source = &self.work.sources[source];
        let released = std::mem::take(&mut *lock(&plan_source.slot));
        if released.bytes.is_some() {
            drop(released);
            self.observer.source_released(plan_source.path);
        }
    }

    /// One stage boundary: cancellation wins, then another output's failure.
    fn boundary(&self) -> Result<(), Halt> {
        if self.cancel.is_cancelled() {
            return Err(Halt::Failed(PrepareError::Cancelled));
        }
        if self.stop.load(Ordering::SeqCst) {
            return Err(Halt::Stopped);
        }
        Ok(())
    }

    fn produce(&self, index: usize) -> Result<PathBuf, Halt> {
        let output = &self.work.outputs[index];
        let plan_source = &self.work.sources[output.source];
        self.boundary()?;
        // With a memo, a source whose fingerprint is unchanged keys the lookup
        // with its recorded digest and is read only if the output is rebuilt.
        let recorded = self.recorded_digest(plan_source)?.map(|(canonical, digest)| {
            cache_key(&canonical, &digest, output.pixels, output.effects)
        });
        if let Some(key) = &recorded
            && let Some(hit) = self.lookup(key, output.pixels)?
        {
            return Ok(hit);
        }
        self.boundary()?;
        let bytes = self.source_bytes(plan_source)?;
        let key = cache_key(&bytes.canonical, &bytes.digest, output.pixels, output.effects);
        if recorded.as_ref() != Some(&key)
            && let Some(hit) = self.lookup(&key, output.pixels)?
        {
            return Ok(hit);
        }
        self.boundary()?;
        let image = self.decoded(plan_source, &bytes)?;
        self.boundary()?;
        let resized = processing::cover_crop(&image, output.pixels)?;
        drop(image);
        self.boundary()?;
        let pixels = processing::apply_effects(resized, output.effects);
        self.boundary()?;
        // Codec work above and below is noninterruptible; cancellation is
        // observed between stages and again at the final publication point.
        self.lease.verify()?;
        Ok(self.cache.publish_with(
            &key,
            output.pixels,
            &pixels,
            self.seam,
            self.commit,
            self.memo,
        )?)
    }

    fn lookup(&self, key: &str, pixels: TargetPixels) -> Result<Option<PathBuf>, Halt> {
        Ok(self.cache.lookup_with(key, pixels, self.memo, &|output| {
            self.observer.output_decoded(output);
        })?)
    }

    /// Inspects a source once per plan and returns its recorded digest when
    /// the memo holds one for exactly its fresh fingerprint.
    ///
    /// The inspection runs the same checks and fails with the same errors as
    /// the read, so the memo never accepts a missing, linked, non-regular or
    /// oversized source. Without a memo nothing is inspected here.
    fn recorded_digest(
        &self,
        plan_source: &PlanSource<'_>,
    ) -> Result<Option<(PathBuf, [u8; 32])>, Halt> {
        let Some(memo) = self.memo else { return Ok(None) };
        let mut slot = lock(&plan_source.slot);
        if slot.inspected {
            return Ok(slot.recorded.clone());
        }
        self.boundary()?;
        let (canonical, metadata) = match source::inspect_source(plan_source.path) {
            Ok(inspected) => inspected,
            Err(error) => {
                // Set while the slot is still locked, so no waiter inspects again.
                self.stop.store(true, Ordering::SeqCst);
                return Err(error.into());
            }
        };
        let recorded = memo
            .source_digest(&canonical, &FileFingerprint::of(&metadata))
            .map(|digest| (canonical, digest));
        slot.inspected = true;
        slot.recorded.clone_from(&recorded);
        drop(slot);
        Ok(recorded)
    }

    /// Reads and hashes a source once; later outputs of it reuse the bytes.
    fn source_bytes(&self, plan_source: &PlanSource<'_>) -> Result<Arc<source::SourceBytes>, Halt> {
        let mut slot = lock(&plan_source.slot);
        if let Some(bytes) = &slot.bytes {
            return Ok(Arc::clone(bytes));
        }
        // A waiter whose reader failed sees the stop here and never reads.
        self.boundary()?;
        let bytes = match source::read_source(plan_source.path) {
            Ok(bytes) => Arc::new(bytes),
            Err(error) => {
                // Set while the slot is still locked, so no waiter reads again.
                self.stop.store(true, Ordering::SeqCst);
                return Err(error.into());
            }
        };
        self.observer.source_read(plan_source.path);
        if let (Some(memo), Some(fingerprint)) = (self.memo, bytes.fingerprint) {
            memo.record_source(&bytes.canonical, fingerprint, bytes.digest);
        }
        slot.bytes = Some(Arc::clone(&bytes));
        drop(slot);
        Ok(bytes)
    }

    /// Decodes a source at most once; later outputs of it reuse the decode.
    fn decoded(
        &self,
        plan_source: &PlanSource<'_>,
        bytes: &source::SourceBytes,
    ) -> Result<Arc<image::DynamicImage>, Halt> {
        let mut slot = lock(&plan_source.slot);
        if let Some(decoded) = &slot.decoded {
            return Ok(Arc::clone(decoded));
        }
        self.boundary()?;
        let decoded = match processing::decode(&bytes.canonical, &bytes.bytes) {
            Ok(decoded) => Arc::new(decoded),
            Err(error) => {
                // Set while the slot is still locked, so no waiter decodes again.
                self.stop.store(true, Ordering::SeqCst);
                return Err(error.into());
            }
        };
        self.observer.source_decoded(plan_source.path);
        slot.decoded = Some(Arc::clone(&decoded));
        drop(slot);
        Ok(decoded)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn check_cancel(cancel: &CancellationToken) -> Result<(), PrepareError> {
    if cancel.is_cancelled() {
        Err(PrepareError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
