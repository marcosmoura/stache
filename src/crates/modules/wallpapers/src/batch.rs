//! Bounded batch preparation for `screens × images` work.
//!
//! The producer pulls from the caller's iterator and blocks on a channel whose
//! capacity equals the worker count, so the Cartesian product is never
//! materialized as a queue. At most
//! `min(available_parallelism, MAX_CONCURRENT_WORKERS)` scoped standard-library
//! threads decode and encode. Workers never touch [`stache_macos::DesktopBackend`]
//! and never wait on the main thread. Cancellation is observed at queue,
//! decode, resize, effect, encode and publication boundaries; codec calls are
//! noninterruptible. The first failing item stops the producer from pulling
//! more work.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};

use stache_macos::DisplayId;
use stache_sdk::{AppPaths, CacheLease, CancellationToken, LeaseWait};

use crate::apply::{PREPARE_LEASE_BUDGET, PrepareError};
use crate::cache::{Cache, CancelLeaseCommit, NoFailures, cache_key};
use crate::limits::MAX_CONCURRENT_WORKERS;
use crate::processing::{self, Effects, ProcessError};
use crate::selection::{SelectedTarget, TargetPixels};
use crate::source;

/// Where one batch reads and publishes.
pub struct BatchDeps<'a> {
    /// The identity paths that own the cache and lease.
    pub paths: &'a AppPaths,
}

/// One source and every target it should be prepared for.
#[derive(Clone, Debug)]
pub struct BatchItem {
    /// The source image.
    pub source: PathBuf,
    /// The resolved targets for that source.
    pub targets: Vec<SelectedTarget>,
    /// The effects applied to every target.
    pub effects: Effects,
}

/// One successfully published output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchOutput {
    /// The source image.
    pub source: PathBuf,
    /// The display identity.
    pub display: DisplayId,
    /// The output size.
    pub pixels: TargetPixels,
    /// The validated cache file.
    pub image: PathBuf,
    /// Whether a validated cache entry already existed, so nothing was
    /// decoded or published for this output.
    pub cache_hit: bool,
}

/// One failed output.
#[derive(Debug)]
pub struct BatchFailure {
    /// The source image.
    pub source: PathBuf,
    /// The target, or `None` when the source itself failed before any target.
    pub display: Option<DisplayId>,
    /// The typed failure.
    pub error: ProcessError,
}

/// What a batch produced.
#[derive(Debug, Default)]
pub struct BatchReport {
    /// Every published or reused output.
    pub outputs: Vec<BatchOutput>,
    /// Every failed output.
    pub failures: Vec<BatchFailure>,
    /// Whether cancellation stopped the run.
    pub cancelled: bool,
}

/// Prepares every item with bounded backpressure and workers.
///
/// # Errors
/// Fails only for a lease or cache-directory failure that affects the whole
/// batch. Per-item failures are reported in [`BatchReport::failures`], and the
/// first one stops the producer from pulling more items.
pub fn prepare_batch<I>(
    items: I,
    deps: &BatchDeps<'_>,
    cancel: &CancellationToken,
) -> Result<BatchReport, PrepareError>
where
    I: IntoIterator<Item = BatchItem>,
{
    prepare_batch_with(items, deps, cancel, &NoObserver)
}

/// Observes work started and finished by batch workers.
///
/// Production passes [`NoObserver`]. Tests use it to hold every worker at a
/// known point without touching production behavior.
pub trait BatchObserver: Sync {
    /// Runs when a worker starts one item.
    fn item_started(&self) {}

    /// Runs when a worker finishes one item, successfully or not.
    fn item_finished(&self) {}
}

/// The production observer that ignores every event.
pub struct NoObserver;

impl BatchObserver for NoObserver {}

/// [`prepare_batch`] with an injected observer.
pub fn prepare_batch_with<I>(
    items: I,
    deps: &BatchDeps<'_>,
    cancel: &CancellationToken,
    observer: &(dyn BatchObserver + '_),
) -> Result<BatchReport, PrepareError>
where
    I: IntoIterator<Item = BatchItem>,
{
    let mut items = items.into_iter();
    let Some(first) = items.next() else {
        return Ok(BatchReport::default());
    };
    if cancel.is_cancelled() {
        return Err(PrepareError::Cancelled);
    }
    let wait = LeaseWait::new(PREPARE_LEASE_BUDGET, cancel.clone());
    let lease = CacheLease::shared(deps.paths, &wait)?;
    let cache = Cache::open(deps.paths)?;
    let workers = worker_count();
    let (sender, receiver) = mpsc::sync_channel::<BatchItem>(workers);
    let receiver = Arc::new(Mutex::new(receiver));
    let stop = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::new(AtomicBool::new(false));
    let results: Arc<Mutex<Vec<Result<BatchOutput, BatchFailure>>>> =
        Arc::new(Mutex::new(Vec::new()));

    std::thread::scope(|scope| {
        for _ in 0..workers {
            let receiver = Arc::clone(&receiver);
            let stop = Arc::clone(&stop);
            let cancelled = Arc::clone(&cancelled);
            let results = Arc::clone(&results);
            let cache = &cache;
            let lease = &lease;
            scope.spawn(move || {
                worker(
                    &receiver, &stop, &cancelled, &results, cache, lease, cancel, observer,
                );
            });
        }
        // Drop the last non-worker receiver handle so a producer blocked on a
        // full channel wakes when every worker has stopped.
        drop(receiver);

        let pending = std::iter::once(first).chain(items);
        for item in pending {
            if cancel.is_cancelled() {
                cancelled.store(true, Ordering::SeqCst);
                break;
            }
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if sender.send(item).is_err() {
                break;
            }
        }
        drop(sender);
    });

    let collected = std::mem::take(&mut *lock(&results));
    let mut outputs = Vec::new();
    let mut failures = Vec::new();
    for result in collected {
        match result {
            Ok(output) => outputs.push(output),
            Err(failure) => failures.push(failure),
        }
    }
    outputs.sort_by(|left, right| {
        left.source
            .cmp(&right.source)
            .then_with(|| left.display.raw().cmp(&right.display.raw()))
    });
    failures.sort_by(|left, right| {
        left.source
            .cmp(&right.source)
            .then_with(|| left.display.map(DisplayId::raw).cmp(&right.display.map(DisplayId::raw)))
    });
    Ok(BatchReport {
        outputs,
        failures,
        cancelled: cancelled.load(Ordering::SeqCst),
    })
}

#[allow(clippy::too_many_arguments)]
fn worker(
    receiver: &Arc<Mutex<mpsc::Receiver<BatchItem>>>,
    stop: &Arc<AtomicBool>,
    cancelled: &Arc<AtomicBool>,
    results: &Arc<Mutex<Vec<Result<BatchOutput, BatchFailure>>>>,
    cache: &Cache,
    lease: &CacheLease,
    cancel: &CancellationToken,
    observer: &dyn BatchObserver,
) {
    loop {
        if cancel.is_cancelled() {
            cancelled.store(true, Ordering::SeqCst);
            break;
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let item = {
            let receiver = lock(receiver);
            receiver.recv()
        };
        let Ok(item) = item else { break };
        observer.item_started();
        let outcome = process_item(cache, lease, &item, cancel);
        observer.item_finished();
        match outcome {
            Ok(outputs) => lock(results).extend(outputs.into_iter().map(Ok)),
            Err((_, ProcessError::Cancelled)) => {
                cancelled.store(true, Ordering::SeqCst);
                stop.store(true, Ordering::SeqCst);
                break;
            }
            Err((display, error)) => {
                lock(results).push(Err(BatchFailure {
                    source: item.source,
                    display,
                    error,
                }));
                stop.store(true, Ordering::SeqCst);
                break;
            }
        }
    }
}

fn process_item(
    cache: &Cache,
    lease: &CacheLease,
    item: &BatchItem,
    cancel: &CancellationToken,
) -> Result<Vec<BatchOutput>, (Option<DisplayId>, ProcessError)> {
    if cancel.is_cancelled() {
        return Err((None, ProcessError::Cancelled));
    }
    let source_bytes = source::read_source(&item.source).map_err(|error| (None, error))?;
    let mut decoded: Option<image::DynamicImage> = None;
    let mut outputs = Vec::with_capacity(item.targets.len());
    for target in &item.targets {
        if cancel.is_cancelled() {
            return Err((Some(target.entry.id), ProcessError::Cancelled));
        }
        let key = cache_key(
            &source_bytes.canonical,
            &source_bytes.digest,
            target.pixels,
            item.effects,
        );
        let mut cache_hit = false;
        let image = match cache.lookup(&key, target.pixels) {
            Ok(Some(hit)) => {
                cache_hit = true;
                hit
            }
            Ok(None) => {
                if decoded.is_none() {
                    decoded = Some(
                        processing::decode(&source_bytes.canonical, &source_bytes.bytes)
                            .map_err(|error| (Some(target.entry.id), error))?,
                    );
                }
                let Some(image) = decoded.as_ref() else {
                    return Err((Some(target.entry.id), ProcessError::Cancelled));
                };
                if cancel.is_cancelled() {
                    return Err((Some(target.entry.id), ProcessError::Cancelled));
                }
                let resized = processing::cover_crop(image, target.pixels)
                    .map_err(|error| (Some(target.entry.id), error))?;
                if cancel.is_cancelled() {
                    return Err((Some(target.entry.id), ProcessError::Cancelled));
                }
                let output = processing::apply_effects(resized, item.effects);
                if cancel.is_cancelled() {
                    return Err((Some(target.entry.id), ProcessError::Cancelled));
                }
                lease
                    .verify()
                    .map_err(|error| (Some(target.entry.id), ProcessError::Lease(error)))?;
                cache
                    .publish(&key, target.pixels, &output, &NoFailures, &CancelLeaseCommit {
                        cancel,
                        lease,
                    })
                    .map_err(|error| (Some(target.entry.id), error))?
            }
            Err(error) => return Err((Some(target.entry.id), ProcessError::Cache(error))),
        };
        outputs.push(BatchOutput {
            source: item.source.clone(),
            display: target.entry.id,
            pixels: target.pixels,
            image,
            cache_hit,
        });
    }
    Ok(outputs)
}

fn worker_count() -> usize {
    worker_count_for(std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get))
}

fn worker_count_for(parallelism: usize) -> usize { parallelism.clamp(1, MAX_CONCURRENT_WORKERS) }

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
