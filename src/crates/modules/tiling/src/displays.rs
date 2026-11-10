//! The native display feed: observes display reconfiguration and delivers
//! [`DisplayLayout`]s to the running actor.
//!
//! The observer callback runs on the main thread and only records a pure
//! [`TopologyHint`] (latest wins) and wakes a worker. The worker queries the
//! layout (a bounded main-thread job, never from the callback) and submits it
//! through [`TrackingRuntime::submit_displays`]. The module primes the feed
//! (initial query and submission) before the action authority is published, so
//! the first frame plan sees native topology. Tiling never applies a screen
//! topology to the host registry; the app's Bar-scoped topology feed owns that
//! (`stache-app` `bar` module), and only while Bar runs.

use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use stache_macos::{DisplayControl, DisplayObserver, TopologyHint};

use crate::status::TilingStatusSource;
use crate::tracking::TrackingRuntime;

/// The display worker thread name.
pub const DISPLAY_THREAD_NAME: &str = "stache-tiling-displays";

#[derive(Default)]
struct Pending {
    state: Mutex<PendingState>,
    wake: Condvar,
    /// Serializes "query the layout, then submit it" for the primed layout and
    /// every hinted one: whichever submits last also queried last, so a stale
    /// primed layout can never overwrite a newer hinted one.
    submit: Mutex<()>,
}

#[derive(Default)]
struct PendingState {
    hint: Option<TopologyHint>,
    stop: bool,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One live display observation and its worker.
pub struct DisplayFeed {
    control: DisplayControl,
    runtime: Weak<TrackingRuntime>,
    status: TilingStatusSource,
    pending: Arc<Pending>,
    observer: Mutex<Option<DisplayObserver>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl DisplayFeed {
    /// Spawns the worker and registers the observer.
    ///
    /// # Errors
    ///
    /// Why the worker or the observer could not start. Nothing stays acquired.
    pub fn spawn(
        control: DisplayControl,
        runtime: &Arc<TrackingRuntime>,
        status: TilingStatusSource,
    ) -> Result<Arc<Self>, String> {
        let pending = Arc::new(Pending::default());
        let worker_pending = Arc::clone(&pending);
        let worker_control = control.clone();
        let worker_runtime = Arc::downgrade(runtime);
        let worker_status = status.clone();
        let thread = std::thread::Builder::new()
            .name(DISPLAY_THREAD_NAME.to_owned())
            .spawn(move || work(&worker_pending, &worker_control, &worker_runtime, &worker_status))
            .map_err(|error| format!("the display worker could not start: {error}"))?;
        let feed = Arc::new(Self {
            control,
            runtime: Arc::downgrade(runtime),
            status,
            pending: Arc::clone(&pending),
            observer: Mutex::new(None),
            thread: Mutex::new(Some(thread)),
        });
        let sink_pending = Arc::clone(&pending);
        // The sink only enqueues a pure hint and wakes the worker.
        let observer = feed.control.observe_topology(move |hint| {
            let mut state = lock(&sink_pending.state);
            if state.hint.is_none_or(|held| held.sequence < hint.sequence) {
                state.hint = Some(hint);
            }
            drop(state);
            sink_pending.wake.notify_one();
        });
        match observer {
            Ok(observer) => {
                *lock(&feed.observer) = Some(observer);
                Ok(feed)
            }
            Err(error) => {
                let stopped = feed.stop(Duration::from_secs(2));
                Err(format!(
                    "the display observer could not start: {error}{}",
                    if stopped {
                        ""
                    } else {
                        " (worker still ending)"
                    }
                ))
            }
        }
    }

    /// Queries the current layout and submits it (no hint): the initial
    /// topology, delivered before the action authority is published.
    ///
    /// # Errors
    ///
    /// The query failure or a closed actor.
    pub fn prime(&self) -> Result<(), String> {
        let _ordered = lock(&self.pending.submit);
        let layout = self.control.query().map_err(|error| error.to_string())?;
        let runtime = self.runtime.upgrade().ok_or("the runtime ended")?;
        runtime
            .submit_displays(None, layout)
            .map(|_| ())
            .map_err(|error| format!("the display layout could not be queued: {error:?}"))
    }

    /// Removes the observer and ends the worker, waiting at most `hop` for the
    /// worker. `false` when something is still owed: stop again to retry. The
    /// observer stays owned after a failed removal; the worker is never
    /// detached.
    pub fn stop(&self, hop: Duration) -> bool {
        {
            let mut state = lock(&self.pending.state);
            state.stop = true;
            drop(state);
            self.pending.wake.notify_all();
        }
        let mut done = true;
        {
            let mut observer = lock(&self.observer);
            if let Some(held) = observer.as_ref() {
                if held.stop().is_ok() {
                    *observer = None;
                } else {
                    done = false;
                }
            }
            drop(observer);
        }
        let mut thread = lock(&self.thread);
        if let Some(handle) = thread.as_ref() {
            let deadline = Instant::now() + hop;
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            if handle.is_finished() {
                if let Some(handle) = thread.take() {
                    let _ = handle.join();
                }
            } else {
                done = false;
            }
        }
        if done {
            self.status.display_failure(None);
        }
        done
    }
}

fn work(
    pending: &Pending,
    control: &DisplayControl,
    runtime: &Weak<TrackingRuntime>,
    status: &TilingStatusSource,
) {
    loop {
        let hint = {
            let mut state = lock(&pending.state);
            loop {
                if state.stop {
                    return;
                }
                if let Some(hint) = state.hint.take() {
                    break hint;
                }
                state = pending.wake.wait(state).unwrap_or_else(PoisonError::into_inner);
            }
        };
        let Some(runtime) = runtime.upgrade() else {
            return;
        };
        let _ordered = lock(&pending.submit);
        match control.query() {
            Ok(layout) => match runtime.submit_displays(Some(hint), layout) {
                Ok(_) => status.display_failure(None),
                Err(_) => return,
            },
            Err(error) => status.display_failure(Some(format!(
                "the display layout could not be read after a change: {error}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use stache_macos::{DisplayControl, FakeDisplayBackend};

    use super::*;
    use crate::backend::{FakeWindowBackend, WindowBackend};
    use crate::test_support::{FakeAppDirectory, display, one_display};
    use crate::tracking::{TrackingOptions, TrackingRuntime};

    struct Inline;

    impl stache_sdk::MainThreadExecutor for Inline {
        fn is_main_thread(&self) -> bool { false }

        fn execute(
            &self,
            job: Box<dyn FnOnce() + Send + 'static>,
        ) -> Result<(), stache_sdk::ThreadError> {
            job();
            Ok(())
        }
    }

    fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !condition() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn a_primed_layout_never_overwrites_a_newer_hinted_one() {
        let fake = FakeDisplayBackend::new(one_display());
        let runtime = Arc::new(
            TrackingRuntime::start(
                Arc::new(FakeWindowBackend::default()) as Arc<dyn WindowBackend>,
                Arc::new(FakeAppDirectory::default()),
                &TrackingOptions::new(""),
            )
            .expect("start"),
        );
        let feed = DisplayFeed::spawn(
            DisplayControl::new(Arc::new(Inline), Arc::new(fake.clone())),
            &runtime,
            TilingStatusSource::default(),
        )
        .expect("feed");
        // Hold the ordering lock as a worker mid-submit would. The prime waits
        // behind it and queries only afterwards, so it reads the newest layout.
        let ordered = lock(&feed.pending.submit);
        let cloned = Arc::clone(&feed);
        let handle = std::thread::spawn(move || cloned.prime());
        let reads = fake.layout_reads();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            fake.layout_reads(),
            reads,
            "the prime waits for the in-flight submit"
        );
        fake.set_layout(stache_macos::DisplayLayout::new(vec![
            display(1, true, Some(60.0), 0.0),
            display(2, false, Some(60.0), 1920.0),
        ]));
        assert_eq!(fake.reconfigure(), 1);
        drop(ordered);
        handle.join().expect("prime thread").expect("primed");
        wait_for("both displays", || runtime.snapshot().screens.len() == 2);
        // Whatever order the two submitters ran in, the newest layout is last.
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(runtime.snapshot().screens.len(), 2);
        assert!(feed.stop(Duration::from_secs(2)));
        assert!(fake.layout_reads() > reads);
    }
}
