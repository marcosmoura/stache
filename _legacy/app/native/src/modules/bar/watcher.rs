use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::time::Duration;

/// Starts a best-effort refresh watcher.
///
/// The `setup` closure can register observers or other event sources. If it
/// fails, the watcher still runs the fallback refresh loop.
pub fn start_best_effort_refresh_watcher<FSetup, FRefresh>(
    watcher_name: &str,
    fallback_poll_interval: Duration,
    setup: FSetup,
    mut refresh: FRefresh,
) where
    FSetup: FnOnce(Sender<()>) -> Result<(), String>,
    FRefresh: FnMut(),
{
    let (tx, rx) = channel::<()>();

    if let Err(err) = setup(tx) {
        tracing::warn!(watcher = watcher_name, error = %err, "failed to register refresh observers");
    }

    refresh();
    run_refresh_loop(&rx, fallback_poll_interval, &mut refresh);
}

fn run_refresh_loop<F>(
    rx: &std::sync::mpsc::Receiver<()>,
    fallback_poll_interval: Duration,
    mut refresh: F,
) where
    F: FnMut(),
{
    loop {
        match rx.recv_timeout(fallback_poll_interval) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => refresh(),
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::start_best_effort_refresh_watcher;

    #[test]
    fn runs_initial_refresh_even_if_setup_fails() {
        let refresh_count = Arc::new(AtomicUsize::new(0));
        let refresh_count_clone = Arc::clone(&refresh_count);

        let handle = std::thread::spawn(move || {
            start_best_effort_refresh_watcher(
                "test-watcher",
                std::time::Duration::from_millis(10),
                |_sender| Err("boom".to_string()),
                move || {
                    refresh_count_clone.fetch_add(1, Ordering::SeqCst);
                },
            );
        });

        handle.join().unwrap();
        assert_eq!(refresh_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn refreshes_on_signal_then_exits_after_disconnect() {
        let refresh_count = Arc::new(AtomicUsize::new(0));
        let refresh_count_clone = Arc::clone(&refresh_count);

        let handle = std::thread::spawn(move || {
            start_best_effort_refresh_watcher(
                "test-watcher",
                std::time::Duration::from_millis(100),
                |sender| {
                    std::thread::spawn(move || {
                        let _ = sender.send(());
                    });
                    Ok(())
                },
                move || {
                    refresh_count_clone.fetch_add(1, Ordering::SeqCst);
                },
            );
        });

        handle.join().unwrap();
        assert_eq!(refresh_count.load(Ordering::SeqCst), 2);
    }
}
