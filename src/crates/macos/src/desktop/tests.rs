use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use stache_sdk::CancellationToken;

use super::*;
use crate::applications::testing::ManualExecutor;

const TEST_BUDGET: Duration = Duration::from_millis(30);

struct CountingOps {
    snapshot: DisplaySnapshot,
    snapshots: AtomicUsize,
    sets: AtomicUsize,
    seen: Mutex<Option<(DisplayId, DisplayEntry, PathBuf)>>,
}

impl CountingOps {
    fn new(snapshot: DisplaySnapshot) -> Self {
        Self {
            snapshot,
            snapshots: AtomicUsize::new(0),
            sets: AtomicUsize::new(0),
            seen: Mutex::new(None),
        }
    }
}

impl NativeDisplayOps for CountingOps {
    fn snapshot(&self) -> Result<DisplaySnapshot, DesktopError> {
        self.snapshots.fetch_add(1, Ordering::SeqCst);
        Ok(self.snapshot.clone())
    }

    fn set_image(
        &self,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &Path,
    ) -> Result<(), DesktopError> {
        self.sets.fetch_add(1, Ordering::SeqCst);
        *self.seen.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((display, *expected, image.to_path_buf()));
        Ok(())
    }

    #[cfg(feature = "testing")]
    fn check_target(
        &self,
        _display: DisplayId,
        _expected: &DisplayEntry,
    ) -> Result<(), DesktopError> {
        Ok(())
    }
}

fn entry(id: u32) -> DisplayEntry {
    DisplayEntry {
        id: DisplayId::new(id),
        frame: DisplayFrame { width: 1920.0, height: 1080.0 },
        scale: 2.0,
    }
}

fn desktop(executor: Arc<ManualExecutor>, ops: Arc<CountingOps>) -> NativeDesktop {
    NativeDesktop {
        executor,
        ops,
        budget: TEST_BUDGET,
    }
}

#[test]
fn a_job_that_never_runs_times_out_without_hanging() {
    let executor = Arc::new(ManualExecutor::new(false));
    let ops = Arc::new(CountingOps::new(DisplaySnapshot::default()));
    let adapter = desktop(Arc::clone(&executor), Arc::clone(&ops));

    let started = Instant::now();
    let result = adapter.snapshot();
    let elapsed = started.elapsed();

    assert!(
        matches!(
            result,
            Err(DesktopError::TimedOut { operation: "display snapshot" })
        ),
        "unexpected result: {result:?}"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "wait was not bounded: {elapsed:?}"
    );
    assert_eq!(ops.snapshots.load(Ordering::SeqCst), 0);
    assert_eq!(executor.queued(), 1);
}

#[test]
fn an_abandoned_job_is_inert_when_it_runs_late() {
    let executor = Arc::new(ManualExecutor::new(false));
    let ops = Arc::new(CountingOps::new(DisplaySnapshot::default()));
    let adapter = desktop(Arc::clone(&executor), Arc::clone(&ops));

    assert!(matches!(adapter.snapshot(), Err(DesktopError::TimedOut { .. })));
    assert_eq!(executor.drain(), 1);

    assert_eq!(ops.snapshots.load(Ordering::SeqCst), 0);
}

#[test]
fn an_inline_executor_runs_on_main_without_dispatching() {
    let executor = Arc::new(ManualExecutor::new(true));
    let expected = DisplaySnapshot::new(vec![entry(7)]);
    let ops = Arc::new(CountingOps::new(expected.clone()));
    let adapter = desktop(Arc::clone(&executor), Arc::clone(&ops));

    let snapshot = adapter.snapshot().expect("inline snapshot");

    assert_eq!(snapshot, expected);
    assert_eq!(ops.snapshots.load(Ordering::SeqCst), 1);
    assert_eq!(executor.queued(), 0);
}

#[test]
fn set_image_rejects_a_relative_path_before_any_native_call() {
    let executor = Arc::new(ManualExecutor::new(true));
    let ops = Arc::new(CountingOps::new(DisplaySnapshot::default()));
    let adapter = desktop(Arc::clone(&executor), Arc::clone(&ops));

    let result = adapter.set_image(DisplayId::new(7), &entry(7), Path::new("image.png"));

    assert_eq!(
        result,
        Err(DesktopError::RelativePath(PathBuf::from("image.png")))
    );
    assert_eq!(ops.sets.load(Ordering::SeqCst), 0);
    assert_eq!(executor.queued(), 0);
}

#[test]
fn set_image_rejects_a_non_utf8_path_before_any_native_call() {
    let executor = Arc::new(ManualExecutor::new(false));
    let ops = Arc::new(CountingOps::new(DisplaySnapshot::default()));
    let adapter = desktop(Arc::clone(&executor), Arc::clone(&ops));
    let path = Path::new(OsStr::from_bytes(b"/tmp/\xff.png"));

    let result = adapter.set_image(DisplayId::new(7), &entry(7), path);

    assert_eq!(result, Err(DesktopError::NonUtf8Path(path.to_path_buf())));
    assert_eq!(ops.sets.load(Ordering::SeqCst), 0);
    assert_eq!(executor.queued(), 0);
}

#[test]
fn set_image_forwards_the_identity_entry_and_path() {
    let executor = Arc::new(ManualExecutor::new(true));
    let ops = Arc::new(CountingOps::new(DisplaySnapshot::default()));
    let adapter = desktop(Arc::clone(&executor), Arc::clone(&ops));
    let expected = entry(9);
    let path = PathBuf::from("/tmp/wallpaper.png");

    adapter.set_image(expected.id, &expected, &path).expect("inline set");

    assert_eq!(ops.sets.load(Ordering::SeqCst), 1);
    let seen = ops.seen.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(seen, Some((expected.id, expected, path)));
}

#[test]
fn the_fake_changes_snapshots_between_calls_and_records_outcomes() {
    let first = DisplaySnapshot::new(vec![entry(1)]);
    let second = DisplaySnapshot::new(vec![entry(2), entry(1)]);
    let fake = testing::FakeDesktop::with_snapshot(first.clone());

    assert_eq!(fake.snapshot().expect("first"), first);
    fake.set_snapshot(second.clone());
    assert_eq!(fake.snapshot().expect("second"), second);
    assert_eq!(fake.snapshot_reads(), 2);

    let failure = DesktopError::DisplayChanged(DisplayId::new(2));
    fake.set_outcome(DisplayId::new(2), Err(failure.clone()));
    let expected = entry(2);
    assert_eq!(
        fake.set_image(DisplayId::new(2), &expected, Path::new("/tmp/a.png")),
        Err(failure)
    );
    assert_eq!(
        fake.set_image(DisplayId::new(1), &entry(1), Path::new("/tmp/b.png")),
        Ok(())
    );
    assert_eq!(fake.calls().len(), 2);
    assert_eq!(fake.calls()[0].image, PathBuf::from("/tmp/a.png"));
}

// ---------------------------------------------------------------------------
// Guarded operations
// ---------------------------------------------------------------------------

/// A monotonic clock the test advances by hand.
struct ManualClock {
    now: Mutex<Instant>,
}

impl ManualClock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            now: Mutex::new(Instant::now()),
        })
    }

    fn advance(&self, duration: Duration) {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) += duration;
    }
}

impl DesktopClock for ManualClock {
    fn now(&self) -> Instant { *self.now.lock().unwrap_or_else(PoisonError::into_inner) }
}

/// Increments a counter exactly once when the last handle drops.
struct DropCounter(Arc<AtomicUsize>);

impl Drop for DropCounter {
    fn drop(&mut self) { self.0.fetch_add(1, Ordering::SeqCst); }
}

/// A native-step double that can hold either side of the guard's final check.
struct BlockingOps {
    snapshot: DisplaySnapshot,
    block_before_admit: bool,
    entered: Mutex<Sender<()>>,
    release: Mutex<Receiver<()>>,
    sets: AtomicUsize,
}

impl BlockingOps {
    fn new(
        snapshot: DisplaySnapshot,
        block_before_admit: bool,
    ) -> (Arc<Self>, Receiver<()>, Sender<()>) {
        let (entered, entered_receiver) = mpsc::channel();
        let (release, release_receiver) = mpsc::channel();
        (
            Arc::new(Self {
                snapshot,
                block_before_admit,
                entered: Mutex::new(entered),
                release: Mutex::new(release_receiver),
                sets: AtomicUsize::new(0),
            }),
            entered_receiver,
            release,
        )
    }
}

impl NativeDisplayOps for BlockingOps {
    fn snapshot(&self) -> Result<DisplaySnapshot, DesktopError> { Ok(self.snapshot.clone()) }

    fn set_image(
        &self,
        _display: DisplayId,
        _expected: &DisplayEntry,
        _image: &Path,
    ) -> Result<(), DesktopError> {
        self.sets.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn issue_set(
        &self,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &Path,
        admit: &(dyn Fn() -> Result<(), DesktopError> + Send + Sync),
    ) -> Result<(), DesktopError> {
        if self.block_before_admit {
            self.entered
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .send(())
                .expect("test observes entry");
            let _ = self
                .release
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .recv_timeout(Duration::from_secs(10));
            admit()?;
            return self.set_image(display, expected, image);
        }
        admit()?;
        self.entered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send(())
            .expect("test observes issuance");
        let _ = self
            .release
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .recv_timeout(Duration::from_secs(10));
        self.set_image(display, expected, image)
    }

    #[cfg(feature = "testing")]
    fn check_target(
        &self,
        _display: DisplayId,
        _expected: &DisplayEntry,
    ) -> Result<(), DesktopError> {
        Ok(())
    }
}

fn guarded_desktop(executor: Arc<ManualExecutor>, ops: Arc<BlockingOps>) -> NativeDesktop {
    NativeDesktop {
        executor,
        ops,
        budget: Duration::from_secs(5),
    }
}

fn always_current() -> Arc<dyn Fn() -> bool + Send + Sync> { Arc::new(|| true) }

#[test]
fn the_guarded_boundary_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<DesktopOperation>();
    assert_send_sync::<NativeDesktop>();
    assert_send_sync::<Arc<dyn DesktopKeepalive>>();
}

#[test]
fn a_cancelled_guarded_snapshot_never_reaches_the_executor() {
    let executor = Arc::new(ManualExecutor::new(false));
    let ops = Arc::new(CountingOps::new(DisplaySnapshot::default()));
    let adapter = desktop(Arc::clone(&executor), Arc::clone(&ops));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let operation = DesktopOperation::new(cancel, always_current());

    let result = adapter.snapshot_guarded(&operation);

    assert_eq!(
        result,
        Err(DesktopError::Cancelled { operation: "display snapshot" })
    );
    assert_eq!(executor.queued(), 0);
    assert_eq!(ops.snapshots.load(Ordering::SeqCst), 0);
}

#[test]
fn an_expired_guarded_set_never_reaches_the_executor() {
    let executor = Arc::new(ManualExecutor::new(false));
    let ops = Arc::new(CountingOps::new(DisplaySnapshot::default()));
    let adapter = desktop(Arc::clone(&executor), Arc::clone(&ops));
    let clock = ManualClock::new();
    let operation = DesktopOperation::new(CancellationToken::new(), always_current())
        .with_clock(Arc::clone(&clock) as Arc<dyn DesktopClock>)
        .with_deadline(clock.now());
    let expected = entry(7);

    let result = adapter.set_image_guarded(
        &operation,
        expected.id,
        &expected,
        Path::new("/tmp/wallpaper.png"),
    );

    assert_eq!(
        result,
        Err(DesktopError::Expired { operation: "desktop image set" })
    );
    assert_eq!(executor.queued(), 0);
    assert_eq!(ops.sets.load(Ordering::SeqCst), 0);
}

#[test]
fn a_stale_guarded_set_never_reaches_the_executor() {
    let executor = Arc::new(ManualExecutor::new(false));
    let ops = Arc::new(CountingOps::new(DisplaySnapshot::default()));
    let adapter = desktop(Arc::clone(&executor), Arc::clone(&ops));
    let operation = DesktopOperation::new(CancellationToken::new(), Arc::new(|| false));
    let expected = entry(7);

    let result = adapter.set_image_guarded(
        &operation,
        expected.id,
        &expected,
        Path::new("/tmp/wallpaper.png"),
    );

    assert_eq!(
        result,
        Err(DesktopError::Stale { operation: "desktop image set" })
    );
    assert_eq!(executor.queued(), 0);
    assert_eq!(ops.sets.load(Ordering::SeqCst), 0);
}

#[test]
fn a_guarded_job_cancelled_while_queued_never_issues_native_work() {
    let executor = Arc::new(ManualExecutor::new(false));
    let ops = Arc::new(CountingOps::new(DisplaySnapshot::default()));
    let adapter = desktop(Arc::clone(&executor), Arc::clone(&ops));
    let cancel = CancellationToken::new();
    let operation = DesktopOperation::new(cancel.clone(), always_current());
    let expected = entry(7);

    let (result_sender, result_receiver) = mpsc::channel();
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            let result = adapter.set_image_guarded(
                &operation,
                expected.id,
                &expected,
                Path::new("/tmp/wallpaper.png"),
            );
            let _ = result_sender.send(result);
        });
        while executor.queued() == 0 {
            std::thread::yield_now();
        }
        let started = Instant::now();
        cancel.cancel();
        let result = result_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("cancellation must wake the wait promptly");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the wait was not woken by cancellation"
        );
        assert_eq!(
            result,
            Err(DesktopError::Cancelled { operation: "desktop image set" })
        );
        waiter.join().expect("waiter thread");
    });

    assert_eq!(executor.drain(), 1);
    assert_eq!(ops.sets.load(Ordering::SeqCst), 0);
}

#[test]
fn a_guarded_job_that_expires_while_queued_is_inert() {
    let executor = Arc::new(ManualExecutor::new(false));
    let (ops, _entered, _release) = BlockingOps::new(DisplaySnapshot::default(), false);
    let adapter = guarded_desktop(Arc::clone(&executor), Arc::clone(&ops));
    let clock = ManualClock::new();
    let operation = DesktopOperation::new(CancellationToken::new(), always_current())
        .with_clock(Arc::clone(&clock) as Arc<dyn DesktopClock>)
        .with_deadline(clock.now() + Duration::from_secs(2));
    let expected = entry(7);

    std::thread::scope(|scope| {
        let handle = scope.spawn(|| {
            adapter.set_image_guarded(
                &operation,
                expected.id,
                &expected,
                Path::new("/tmp/wallpaper.png"),
            )
        });
        while executor.queued() == 0 {
            std::thread::yield_now();
        }
        clock.advance(Duration::from_secs(3));
        assert_eq!(executor.drain(), 1);
        let result = handle.join().expect("waiter thread");
        assert_eq!(
            result,
            Err(DesktopError::Expired { operation: "desktop image set" })
        );
    });
    assert_eq!(ops.sets.load(Ordering::SeqCst), 0);
}

#[test]
fn a_running_job_abandoned_before_issuance_never_calls_the_native_step() {
    let executor = Arc::new(ManualExecutor::new(false));
    let (ops, entered, release) = BlockingOps::new(DisplaySnapshot::default(), true);
    let adapter = guarded_desktop(Arc::clone(&executor), Arc::clone(&ops));
    let cancel = CancellationToken::new();
    let operation = DesktopOperation::new(cancel.clone(), always_current());
    let expected = entry(7);

    let (sender, receiver) = mpsc::channel();
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            let result = adapter.set_image_guarded(
                &operation,
                expected.id,
                &expected,
                Path::new("/tmp/wallpaper.png"),
            );
            let _ = sender.send(result);
        });
        while executor.queued() == 0 {
            std::thread::yield_now();
        }
        let drain = scope.spawn(|| executor.drain());
        entered.recv_timeout(Duration::from_secs(5)).expect("job entered");
        cancel.cancel();
        let result = receiver.recv_timeout(Duration::from_secs(1)).expect("waiter returns");
        release.send(()).expect("release the job");
        assert_eq!(drain.join().expect("drain thread"), 1);
        waiter.join().expect("waiter thread");
        assert_eq!(
            result,
            Err(DesktopError::Cancelled { operation: "desktop image set" })
        );
    });
    assert_eq!(ops.sets.load(Ordering::SeqCst), 0);
}

#[test]
fn cancelling_an_issued_call_reports_unknown_and_keeps_the_keepalive() {
    let executor = Arc::new(ManualExecutor::new(false));
    let (ops, entered, release) = BlockingOps::new(DisplaySnapshot::default(), false);
    let adapter = guarded_desktop(Arc::clone(&executor), Arc::clone(&ops));
    let cancel = CancellationToken::new();
    let dropped = Arc::new(AtomicUsize::new(0));
    let keepalive: Arc<dyn DesktopKeepalive> = Arc::new(DropCounter(Arc::clone(&dropped)));
    let operation =
        DesktopOperation::new(cancel.clone(), always_current()).with_keepalive(keepalive);
    let expected = entry(7);

    let (sender, receiver) = mpsc::channel();
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            let result = adapter.set_image_guarded(
                &operation,
                expected.id,
                &expected,
                Path::new("/tmp/wallpaper.png"),
            );
            let _ = sender.send(result);
        });
        while executor.queued() == 0 {
            std::thread::yield_now();
        }
        let drain = scope.spawn(|| executor.drain());
        entered.recv_timeout(Duration::from_secs(5)).expect("native call entered");
        cancel.cancel();
        let result = receiver.recv_timeout(Duration::from_secs(1)).expect("waiter returns");
        assert_eq!(
            result,
            Err(DesktopError::Unknown { operation: "desktop image set" })
        );
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            0,
            "the issued call must retain the keepalive"
        );
        release.send(()).expect("release the native call");
        assert_eq!(drain.join().expect("drain thread"), 1);
        waiter.join().expect("waiter thread");
    });
    drop(operation);
    assert_eq!(ops.sets.load(Ordering::SeqCst), 1);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn a_guarded_call_completes_normally_inline_on_main() {
    let executor = Arc::new(ManualExecutor::new(true));
    let ops = Arc::new(CountingOps::new(DisplaySnapshot::default()));
    let adapter = desktop(Arc::clone(&executor), Arc::clone(&ops));
    let operation = DesktopOperation::new(CancellationToken::new(), always_current());
    let expected = entry(7);

    adapter
        .set_image_guarded(
            &operation,
            expected.id,
            &expected,
            Path::new("/tmp/wallpaper.png"),
        )
        .expect("inline guarded set");

    assert_eq!(ops.sets.load(Ordering::SeqCst), 1);
    assert_eq!(executor.queued(), 0);
}

#[test]
fn the_fake_honours_the_guard_for_scripted_callers() {
    let fake = testing::FakeDesktop::with_snapshot(DisplaySnapshot::new(vec![entry(3)]));
    let expected = entry(3);

    let current = DesktopOperation::new(CancellationToken::new(), always_current());
    fake.set_image_guarded(&current, expected.id, &expected, Path::new("/tmp/current.png"))
        .expect("current guard");
    assert_eq!(fake.guarded_calls(), 1);

    let cancel = CancellationToken::new();
    cancel.cancel();
    let cancelled = DesktopOperation::new(cancel, always_current());
    let result = fake.set_image_guarded(
        &cancelled,
        expected.id,
        &expected,
        Path::new("/tmp/cancelled.png"),
    );
    assert_eq!(
        result,
        Err(DesktopError::Cancelled { operation: "desktop image set" })
    );
    assert_eq!(fake.guarded_calls(), 1);
    assert_eq!(fake.calls().len(), 1);

    let clock = ManualClock::new();
    let expired = DesktopOperation::new(CancellationToken::new(), always_current())
        .with_clock(Arc::clone(&clock) as Arc<dyn DesktopClock>)
        .with_deadline(clock.now());
    assert!(matches!(
        fake.snapshot_guarded(&expired),
        Err(DesktopError::Expired { operation: "display snapshot" })
    ));
    assert_eq!(fake.guarded_snapshot_reads(), 0);
}

#[test]
fn a_tracked_late_issued_call_stays_unresolved_until_the_native_step_finishes() {
    let executor = Arc::new(ManualExecutor::new(false));
    let (ops, entered, release) = BlockingOps::new(DisplaySnapshot::default(), false);
    let adapter = guarded_desktop(Arc::clone(&executor), Arc::clone(&ops));
    let tracker = NativeOperationTracker::new();
    let cancel = CancellationToken::new();
    let operation =
        DesktopOperation::new(cancel.clone(), always_current()).with_tracker(tracker.clone());
    let expected = entry(7);

    let (sender, receiver) = mpsc::channel();
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            let result = adapter.set_image_guarded(
                &operation,
                expected.id,
                &expected,
                Path::new("/tmp/wallpaper.png"),
            );
            let _ = sender.send(result);
        });
        while executor.queued() == 0 {
            std::thread::yield_now();
        }
        let drain = scope.spawn(|| executor.drain());
        entered.recv_timeout(Duration::from_secs(5)).expect("native call entered");
        assert_eq!(tracker.unresolved(), 1);
        assert_eq!(tracker.owned(), 1);
        cancel.cancel();
        let result = receiver.recv_timeout(Duration::from_secs(1)).expect("waiter returns");
        assert_eq!(
            result,
            Err(DesktopError::Unknown { operation: "desktop image set" })
        );
        assert_eq!(
            tracker.unresolved(),
            1,
            "an issued native call stays unresolved for a successor"
        );
        assert!(!tracker.is_drained());
        release.send(()).expect("release the native call");
        assert_eq!(drain.join().expect("drain thread"), 1);
        waiter.join().expect("waiter thread");
    });
    assert_eq!(tracker.unresolved(), 0);
    assert_eq!(tracker.owned(), 0);
    assert!(tracker.is_drained());
}

#[test]
fn an_abandoned_queued_tracked_call_releases_admission_but_stays_owned() {
    let executor = Arc::new(ManualExecutor::new(false));
    let ops = Arc::new(CountingOps::new(DisplaySnapshot::default()));
    let adapter = desktop(Arc::clone(&executor), Arc::clone(&ops));
    let tracker = NativeOperationTracker::new();
    let operation = DesktopOperation::new(CancellationToken::new(), always_current())
        .with_tracker(tracker.clone());

    let result = adapter.snapshot_guarded(&operation);
    assert!(matches!(result, Err(DesktopError::TimedOut { .. })));
    assert_eq!(
        tracker.unresolved(),
        0,
        "a job that never ran cannot issue a native call"
    );
    assert_eq!(tracker.owned(), 1, "the queued closure is still owned");

    assert_eq!(executor.drain(), 1);
    assert_eq!(tracker.owned(), 0);
    assert!(tracker.is_drained());
    assert_eq!(ops.snapshots.load(Ordering::SeqCst), 0);
}

#[test]
fn a_successor_wait_is_released_only_when_the_predecessor_finishes() {
    let tracker = NativeOperationTracker::new();
    let clock = ManualClock::new();
    let account = tracker.begin();
    assert_eq!(tracker.unresolved(), 1);

    std::thread::scope(|scope| {
        let tracker = tracker.clone();
        let (entered, entered_rx) = mpsc::channel();
        let waiter = scope.spawn(move || {
            let _ = entered.send(());
            tracker.wait_unresolved(&CancellationToken::new(), None, clock.as_ref())
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).expect("wait started");
        for _ in 0..20 {
            std::thread::yield_now();
        }
        // The waiter cannot return while the predecessor is unresolved.
        assert!(!waiter.is_finished());
        account.finish();
        assert!(waiter.join().expect("successor wait"));
    });
    assert_eq!(tracker.unresolved(), 0);
    assert_eq!(tracker.owned(), 0);
}

#[test]
fn cancelling_a_successor_wait_returns_without_issuing() {
    let tracker = NativeOperationTracker::new();
    let clock = ManualClock::new();
    let _account = tracker.begin();
    let cancel = CancellationToken::new();
    let waiter = {
        let cancel = cancel.clone();
        std::thread::spawn(move || tracker.wait_unresolved(&cancel, None, clock.as_ref()))
    };
    cancel.cancel();
    assert!(!waiter.join().expect("cancelled successor wait"));
}

#[test]
fn a_cancellation_arriving_near_the_wait_is_never_lost() {
    for _ in 0..50 {
        let executor = Arc::new(ManualExecutor::new(false));
        let ops = Arc::new(CountingOps::new(DisplaySnapshot::default()));
        let adapter = desktop(Arc::clone(&executor), Arc::clone(&ops));
        let cancel = CancellationToken::new();
        let operation = DesktopOperation::new(cancel.clone(), always_current());
        let expected = entry(7);

        let (sender, receiver) = mpsc::channel();
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                let result = adapter.set_image_guarded(
                    &operation,
                    expected.id,
                    &expected,
                    Path::new("/tmp/wallpaper.png"),
                );
                let _ = sender.send(result);
            });
            while executor.queued() == 0 {
                std::thread::yield_now();
            }
            cancel.cancel();
            let result = receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("cancellation must wake the wait");
            assert_eq!(
                result,
                Err(DesktopError::Cancelled { operation: "desktop image set" })
            );
            assert_eq!(executor.drain(), 1);
            waiter.join().expect("waiter thread");
        });
        assert_eq!(ops.sets.load(Ordering::SeqCst), 0);
    }
}
