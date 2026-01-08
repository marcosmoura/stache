use std::thread;

pub fn spawn_named_thread<F>(name: &str, task: F)
where F: FnOnce() + Send + 'static {
    let thread_name = format!("barba-{name}");

    if let Err(err) = thread::Builder::new().name(thread_name.clone()).spawn(task) {
        eprintln!("Failed to spawn {thread_name}: {err}");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::*;

    #[test]
    fn spawn_named_thread_executes_task() {
        let executed = Arc::new(AtomicBool::new(false));
        let executed_clone = Arc::clone(&executed);

        spawn_named_thread("test-task", move || {
            executed_clone.store(true, Ordering::SeqCst);
        });

        // Give the thread time to execute
        thread::sleep(Duration::from_millis(100));

        assert!(executed.load(Ordering::SeqCst));
    }

    #[test]
    fn spawn_named_thread_uses_correct_prefix() {
        use std::sync::mpsc::channel;

        let (tx, rx) = channel();

        spawn_named_thread("name-test", move || {
            let current_thread = thread::current();
            let name = current_thread.name().unwrap_or("").to_string();
            tx.send(name).unwrap();
        });

        let thread_name = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(thread_name, "barba-name-test");
    }

    #[test]
    fn spawn_named_thread_handles_empty_name() {
        use std::sync::mpsc::channel;

        let (tx, rx) = channel();

        spawn_named_thread("", move || {
            let current_thread = thread::current();
            let name = current_thread.name().unwrap_or("").to_string();
            tx.send(name).unwrap();
        });

        let thread_name = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(thread_name, "barba-");
    }

    #[test]
    fn spawn_named_thread_handles_special_characters_in_name() {
        use std::sync::mpsc::channel;

        let (tx, rx) = channel();

        spawn_named_thread("test-with-dashes_and_underscores", move || {
            let current_thread = thread::current();
            let name = current_thread.name().unwrap_or("").to_string();
            tx.send(name).unwrap();
        });

        let thread_name = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(thread_name, "barba-test-with-dashes_and_underscores");
    }

    #[test]
    fn spawn_named_thread_runs_concurrently() {
        use std::sync::mpsc::channel;

        let (tx1, rx1) = channel();
        let (tx2, rx2) = channel();

        spawn_named_thread("concurrent-1", move || {
            thread::sleep(Duration::from_millis(50));
            tx1.send(1).unwrap();
        });

        spawn_named_thread("concurrent-2", move || {
            thread::sleep(Duration::from_millis(50));
            tx2.send(2).unwrap();
        });

        let result1 = rx1.recv_timeout(Duration::from_secs(1)).unwrap();
        let result2 = rx2.recv_timeout(Duration::from_secs(1)).unwrap();

        assert_eq!(result1, 1);
        assert_eq!(result2, 2);
    }
}
