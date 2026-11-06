use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use stache_macos::DisplayId;
use stache_sdk::CancellationToken;

use super::*;
use crate::selection::{SelectedTarget, target_pixels};
use crate::test_support::{TestEnv, entry, read_rgb8, solid};

fn target(id: u32, width: f64, height: f64, scale: f64) -> SelectedTarget {
    let entry = entry(id, width, height, scale);
    SelectedTarget {
        entry,
        pixels: target_pixels(&entry).expect("pixels"),
    }
}

#[test]
fn a_batch_prepares_every_source_and_target() {
    let env = TestEnv::new();
    let first = env.png("first.png", solid(40, 20, [1, 2, 3]));
    let second = env.png("second.png", solid(40, 20, [4, 5, 6]));
    let targets = vec![target(1, 640.0, 360.0, 1.0), target(2, 640.0, 360.0, 2.0)];
    let items = vec![
        BatchItem {
            source: first,
            targets: targets.clone(),
            effects: Effects::default(),
        },
        BatchItem {
            source: second,
            targets,
            effects: Effects::default(),
        },
    ];
    let report = prepare_batch(
        items,
        &BatchDeps { paths: env.paths() },
        &CancellationToken::new(),
    )
    .expect("batch");
    assert!(report.failures.is_empty());
    assert!(!report.cancelled);
    assert_eq!(report.outputs.len(), 4);
    for output in &report.outputs {
        assert!(output.image.is_file());
        let dimensions = read_rgb8(&output.image).dimensions();
        assert_eq!(dimensions, (output.pixels.width, output.pixels.height));
    }
    assert_eq!(
        report
            .outputs
            .iter()
            .filter(|output| output.display == DisplayId::new(1))
            .count(),
        2
    );
}

struct CountingItems {
    count: Arc<AtomicUsize>,
    limit: usize,
    sources: Vec<PathBuf>,
    targets: Vec<SelectedTarget>,
    cancel_after: Option<(usize, CancellationToken)>,
}

impl Iterator for CountingItems {
    type Item = BatchItem;

    fn next(&mut self) -> Option<Self::Item> {
        let index = self.count.fetch_add(1, Ordering::SeqCst);
        if index >= self.limit {
            return None;
        }
        if let Some((after, token)) = &self.cancel_after
            && index >= *after
        {
            token.cancel();
        }
        let source = self.sources[index % self.sources.len()].clone();
        Some(BatchItem {
            source,
            targets: self.targets.clone(),
            effects: Effects::default(),
        })
    }
}

#[test]
fn a_failure_stops_the_producer_from_pulling_more_work() {
    let env = TestEnv::new();
    let corrupt = env.path("broken.png");
    fs::write(&corrupt, b"not an image").expect("corrupt");
    let valid = env.png("valid.png", solid(8, 8, [7, 7, 7]));
    let pulled = Arc::new(AtomicUsize::new(0));
    let items = CountingItems {
        count: Arc::clone(&pulled),
        limit: 64,
        sources: vec![corrupt, valid],
        targets: vec![target(1, 320.0, 180.0, 1.0)],
        cancel_after: None,
    };
    let report = prepare_batch(
        items,
        &BatchDeps { paths: env.paths() },
        &CancellationToken::new(),
    )
    .expect("batch");
    assert!(!report.failures.is_empty());
    assert!(
        report.failures.iter().all(|failure| matches!(
            failure.error,
            ProcessError::Decode { .. } | ProcessError::Io { .. }
        )),
        "{:?}",
        report.failures
    );
    let pulled = pulled.load(Ordering::SeqCst);
    assert!(
        pulled < 64,
        "the producer kept pulling after a failure: {pulled}"
    );
}

#[test]
fn cancellation_stops_pulling_and_reports_cancelled() {
    let env = TestEnv::new();
    let valid = env.png("valid.png", solid(8, 8, [9, 9, 9]));
    let cancel = CancellationToken::new();
    let pulled = Arc::new(AtomicUsize::new(0));
    let items = CountingItems {
        count: Arc::clone(&pulled),
        limit: 64,
        sources: vec![valid],
        targets: vec![target(1, 320.0, 180.0, 1.0)],
        cancel_after: Some((1, cancel.clone())),
    };
    let report = prepare_batch(items, &BatchDeps { paths: env.paths() }, &cancel).expect("batch");
    assert!(report.cancelled);
    let pulled = pulled.load(Ordering::SeqCst);
    assert!(pulled <= 2, "cancellation pulled {pulled} items");
}

#[test]
fn cancellation_before_any_item_produces_no_output() {
    let env = TestEnv::new();
    let valid = env.png("valid.png", solid(8, 8, [10, 10, 10]));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let items = vec![BatchItem {
        source: valid,
        targets: vec![target(1, 320.0, 180.0, 1.0)],
        effects: Effects::default(),
    }];
    let error = prepare_batch(items, &BatchDeps { paths: env.paths() }, &cancel)
        .expect_err("cancelled before the lease");
    assert!(matches!(error, PrepareError::Cancelled), "{error:?}");
    assert!(!env.paths().cache_directory().exists());
    assert!(!env.paths().runtime_directory().exists());
}

#[test]
fn an_empty_batch_creates_nothing() {
    let env = TestEnv::new();
    let report = prepare_batch(
        Vec::new(),
        &BatchDeps { paths: env.paths() },
        &CancellationToken::new(),
    )
    .expect("empty batch");
    assert!(report.outputs.is_empty());
    assert!(report.failures.is_empty());
    assert!(!env.paths().cache_directory().exists());
}

#[test]
fn the_worker_cap_is_the_smaller_of_parallelism_and_the_limit() {
    for parallelism in [1, 2, 4, 5, 8, 64] {
        assert_eq!(
            worker_count_for(parallelism),
            parallelism.min(MAX_CONCURRENT_WORKERS),
            "parallelism {parallelism}"
        );
    }
    assert_eq!(
        worker_count_for(0),
        1,
        "a zero parallelism still gets one worker"
    );
    assert!((1..=MAX_CONCURRENT_WORKERS).contains(&worker_count()));
}

/// Holds every worker inside `item_started` and records what the producer had
/// pulled when the last expected worker arrived.
struct ConcurrencyProbe {
    expected: usize,
    pulled: Arc<AtomicUsize>,
    pulled_at_full: AtomicUsize,
    peak: AtomicUsize,
    state: Mutex<ProbeState>,
    wake: Condvar,
}

struct ProbeState {
    in_flight: usize,
    released: bool,
}

/// How long the probe holds the last worker so a buggy extra worker could enter.
const EXTRA_WORKER_WINDOW: Duration = Duration::from_millis(100);
/// Bounded wait so a worker that never arrives fails the test instead of hanging.
const PROBE_BUDGET: Duration = Duration::from_secs(10);

impl ConcurrencyProbe {
    fn new(expected: usize, pulled: Arc<AtomicUsize>) -> Self {
        Self {
            expected,
            pulled,
            pulled_at_full: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            state: Mutex::new(ProbeState { in_flight: 0, released: false }),
            wake: Condvar::new(),
        }
    }
}

impl BatchObserver for ConcurrencyProbe {
    fn item_started(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.in_flight += 1;
        self.peak.fetch_max(state.in_flight, Ordering::SeqCst);
        self.wake.notify_all();
        if !state.released && state.in_flight == self.expected {
            self.pulled_at_full.store(self.pulled.load(Ordering::SeqCst), Ordering::SeqCst);
            // Let a regression that spawned an extra worker enter before release.
            let (guard, _) = self
                .wake
                .wait_timeout_while(state, EXTRA_WORKER_WINDOW, |state| !state.released)
                .unwrap_or_else(PoisonError::into_inner);
            state = guard;
            state.released = true;
            self.wake.notify_all();
        } else {
            let deadline = Instant::now() + PROBE_BUDGET;
            while !state.released {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(
                    !remaining.is_zero(),
                    "only {} of {} workers entered before the probe timed out",
                    state.in_flight,
                    self.expected
                );
                let (guard, _) = self
                    .wake
                    .wait_timeout(state, remaining)
                    .unwrap_or_else(PoisonError::into_inner);
                state = guard;
            }
        }
        drop(state);
    }

    fn item_finished(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.in_flight = state.in_flight.saturating_sub(1);
    }
}

#[test]
fn the_worker_cap_is_observed_and_the_producer_stays_bounded() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(16, 9, [12, 13, 14]));
    let workers = worker_count();
    let total = workers * 3 + 2;
    let pulled = Arc::new(AtomicUsize::new(0));
    let items = CountingItems {
        count: Arc::clone(&pulled),
        limit: total,
        sources: vec![source],
        targets: vec![target(1, 320.0, 180.0, 1.0)],
        cancel_after: None,
    };
    let probe = ConcurrencyProbe::new(workers, Arc::clone(&pulled));
    let report = prepare_batch_with(
        items,
        &BatchDeps { paths: env.paths() },
        &CancellationToken::new(),
        &probe,
    )
    .expect("batch");

    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(report.outputs.len(), total, "every item must complete");
    assert_eq!(
        probe.peak.load(Ordering::SeqCst),
        workers,
        "peak in-flight work must equal worker_count()"
    );
    // The producer may have pulled the workers' in-flight items, one full
    // channel (capacity == workers) and the item it is blocked sending.
    let bound = workers + workers + 1;
    assert!(
        probe.pulled_at_full.load(Ordering::SeqCst) <= bound,
        "the producer pulled {} items while {workers} workers were held; bound is {bound}",
        probe.pulled_at_full.load(Ordering::SeqCst)
    );
}
