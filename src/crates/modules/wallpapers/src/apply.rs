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
//! cannot outlive the cache file it reads. Neither function spawns a thread: a
//! standalone caller runs them on its main thread, while a host caller runs
//! them on a coordinator worker. Workers never wait on the main thread.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use stache_macos::{
    DesktopBackend, DesktopClock, DesktopError, DesktopKeepalive, DesktopOperation, DisplayEntry,
    DisplayId, DisplaySnapshot, GuardedDesktopBackend, NativeOperationTracker,
};
use stache_sdk::{AppPaths, CacheLease, CacheLeaseError, CancellationToken, LeaseClock, LeaseWait};

use crate::cache::{Cache, CancelLeaseCommit, PublishCommit, PublishSeam, cache_key};
use crate::limits::{self, ResourceError};
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
        &PreparePlanDeps { paths: deps.paths },
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
/// display identities, and prepares every output before returning. Adjacent
/// targets that share a source reuse that source's bounded bytes and decode;
/// a different source releases the previous decode before it is read.
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
    let commit = CancelLeaseCommit { cancel, lease: lease.as_ref() };
    let mut prepared = Vec::with_capacity(plan.len());
    let mut group: Option<SourceGroup> = None;
    for planned in plan {
        check_cancel(cancel)?;
        // Recompute the target instead of trusting the carried value.
        let pixels = crate::selection::target_pixels(&planned.target.entry)?;
        if group.as_ref().is_none_or(|group| group.path != planned.source) {
            // Release the previous bytes and decode before the next source is
            // read, so at most one source decode is resident.
            drop(group.take());
            let bytes = source::read_source(&planned.source)?;
            group = Some(SourceGroup {
                path: planned.source.clone(),
                bytes,
                decoded: None,
            });
        }
        let group = group.as_mut().expect("group was just established");
        let path = prepare_one(
            &cache,
            &group.bytes,
            pixels,
            planned.effects,
            &mut group.decoded,
            &lease,
            cancel,
            seam,
            &commit,
        )?;
        prepared.push(PreparedTarget {
            entry: planned.target.entry,
            image: path,
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

/// One source's bounded bytes and its optional single decode.
struct SourceGroup {
    path: PathBuf,
    bytes: source::SourceBytes,
    decoded: Option<image::DynamicImage>,
}

#[allow(clippy::too_many_arguments)]
fn prepare_one(
    cache: &Cache,
    source_bytes: &source::SourceBytes,
    pixels: TargetPixels,
    effects: Effects,
    decoded: &mut Option<image::DynamicImage>,
    lease: &CacheLease,
    cancel: &CancellationToken,
    seam: &dyn PublishSeam,
    commit: &dyn PublishCommit,
) -> Result<PathBuf, PrepareError> {
    let key = cache_key(&source_bytes.canonical, &source_bytes.digest, pixels, effects);
    if let Some(hit) = cache.lookup(&key, pixels)? {
        return Ok(hit);
    }
    check_cancel(cancel)?;
    if decoded.is_none() {
        *decoded = Some(processing::decode(&source_bytes.canonical, &source_bytes.bytes)?);
    }
    let Some(image) = decoded.as_ref() else {
        return Err(PrepareError::Process(ProcessError::Cancelled));
    };
    check_cancel(cancel)?;
    let resized = processing::cover_crop(image, pixels)?;
    check_cancel(cancel)?;
    let output = processing::apply_effects(resized, effects);
    check_cancel(cancel)?;
    // Codec work above and below is noninterruptible; cancellation is observed
    // between stages and again at the final publication point.
    lease.verify().map_err(PrepareError::Lease)?;
    Ok(cache.publish(&key, pixels, &output, seam, commit)?)
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
