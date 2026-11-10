//! Line-stream tests over fake children; the few real processes are harmless
//! `/bin/sh` one-liners with no descendants.

use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use super::super::{ChildProcess, CommandEnvironment, ProcessFactory, SpawnSpec};
use super::*;
use crate::lock;

// ---------------------------------------------------------------------------
// Assembler
// ---------------------------------------------------------------------------

fn assemble(cap: usize, chunks: &[&[u8]], finish: bool) -> Vec<StreamEvent> {
    let mut assembler = LineAssembler::new(cap);
    let mut out = VecDeque::new();
    for chunk in chunks {
        assembler.push(chunk, &mut out);
    }
    if finish {
        assembler.finish(&mut out);
    }
    out.into_iter().map(StreamEvent::from).collect()
}

fn line(text: &str) -> StreamEvent { StreamEvent::Line(text.to_owned()) }

#[test]
fn utf8_split_across_reads_decodes_once_the_line_is_complete() {
    let bytes = "caf\u{e9} \u{1f3b5}\n".as_bytes();
    for split in 1..bytes.len() {
        let events = assemble(64, &[&bytes[..split], &bytes[split..]], true);
        assert_eq!(events, vec![line("caf\u{e9} \u{1f3b5}")], "split at {split}");
    }
    let one_byte_at_a_time: Vec<&[u8]> = bytes.chunks(1).collect();
    assert_eq!(assemble(64, &one_byte_at_a_time, true), vec![line(
        "caf\u{e9} \u{1f3b5}"
    )]);
}

#[test]
fn several_lines_in_one_chunk_come_out_in_order_with_blank_lines_kept() {
    assert_eq!(assemble(64, &[b"one\n\ntwo\nthr", b"ee\n"], true), vec![
        line("one"),
        line(""),
        line("two"),
        line("three")
    ]);
}

#[test]
fn a_line_exactly_at_the_cap_is_accepted_and_one_more_byte_is_not() {
    assert_eq!(assemble(4, &[b"abcd\n"], true), vec![line("abcd")]);
    assert_eq!(assemble(4, &[b"abcde\nok\n"], true), vec![
        StreamEvent::LineTooLong { limit: 4 },
        line("ok")
    ]);
}

#[test]
fn an_oversized_line_is_reported_once_and_discarded_up_to_the_next_newline() {
    let events = assemble(4, &[b"aaaa", b"aaaa", b"aaaa\nfine\n"], true);
    assert_eq!(events, vec![StreamEvent::LineTooLong { limit: 4 }, line("fine")]);
}

#[test]
fn an_oversized_line_never_grows_the_buffer_past_the_cap() {
    let mut assembler = LineAssembler::new(8);
    let mut out = VecDeque::new();
    for _ in 0..1000 {
        assembler.push(&[b'x'; 1000], &mut out);
        assert!(assembler.buffer.len() <= 8);
        assert!(assembler.buffer.capacity() <= 1000);
    }
    assert_eq!(out.len(), 1);
}

#[test]
fn a_never_terminated_oversized_line_is_one_error_and_eof_adds_nothing() {
    let events = assemble(4, &[b"aaaaaaaa", b"bbbbbbbb"], true);
    assert_eq!(events, vec![StreamEvent::LineTooLong { limit: 4 }]);
}

#[test]
fn a_short_unterminated_tail_is_reported_at_eof_not_emitted_as_a_line() {
    assert_eq!(assemble(16, &[b"ok\npartial"], true), vec![
        line("ok"),
        StreamEvent::Unterminated { bytes: 7 }
    ]);
}

#[test]
fn invalid_utf8_is_a_typed_error_and_later_lines_survive() {
    assert_eq!(assemble(16, &[b"\xff\xfe\ngood\n"], true), vec![
        StreamEvent::InvalidUtf8 { bytes: 2 },
        line("good")
    ]);
}

#[test]
fn stderr_tail_keeps_only_the_newest_bytes_and_counts_the_rest() {
    let mut tail = StderrTail {
        cap: 8,
        bytes: VecDeque::new(),
        dropped: 0,
    };
    tail.push(b"0123456789");
    assert_eq!(tail.bytes.iter().copied().collect::<Vec<_>>(), b"23456789");
    tail.push(b"ab");
    assert_eq!(tail.bytes.iter().copied().collect::<Vec<_>>(), b"456789ab");
    assert_eq!(tail.dropped, 4);
    assert_eq!(
        StreamLimits::new(0, 0, DEFAULT_TERM_GRACE),
        Err(StreamLimitsError::LineCap)
    );
    assert_eq!(
        StreamLimits::new(1, MAX_STDERR_TAIL + 1, DEFAULT_TERM_GRACE),
        Err(StreamLimitsError::StderrTail)
    );
    assert!(StreamLimits::new(24 * 1024 * 1024, STDERR_TAIL_BYTES, DEFAULT_TERM_GRACE).is_ok());
}

// ---------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Shared {
    stdout: VecDeque<Vec<u8>>,
    stdout_closed: bool,
    stderr: VecDeque<Vec<u8>>,
    reads: usize,
    exit: Option<i32>,
    term_exits: bool,
    kill_exits: bool,
    signals: Vec<&'static str>,
}

struct FakeChild(Arc<Mutex<Shared>>);

fn take(queue: &mut VecDeque<Vec<u8>>, buffer: &mut [u8]) -> Option<usize> {
    let front = queue.front_mut()?;
    let count = front.len().min(buffer.len());
    buffer[..count].copy_from_slice(&front[..count]);
    front.drain(..count);
    if front.is_empty() {
        queue.pop_front();
    }
    Some(count)
}

impl ChildProcess for FakeChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        Ok(lock(&self.0).exit.map(ExitStatus::from_raw))
    }

    fn kill(&mut self) -> io::Result<()> {
        let mut shared = lock(&self.0);
        shared.signals.push("kill");
        if shared.kill_exits {
            shared.exit = Some(9);
        }
        Ok(())
    }

    fn terminate(&mut self) -> io::Result<()> {
        let mut shared = lock(&self.0);
        shared.signals.push("term");
        if shared.term_exits {
            shared.exit = Some(15);
        }
        Ok(())
    }

    fn read_stdout(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut shared = lock(&self.0);
        shared.reads += 1;
        match take(&mut shared.stdout, buffer) {
            Some(count) => Ok(count),
            None if shared.stdout_closed => Ok(0),
            None => Err(io::ErrorKind::WouldBlock.into()),
        }
    }

    fn read_stderr(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut shared = lock(&self.0);
        take(&mut shared.stderr, buffer).ok_or_else(|| io::ErrorKind::WouldBlock.into())
    }
}

type WaitHook = Box<dyn Fn(usize) + Send + Sync>;

#[derive(Default)]
struct FakeClock {
    now: Mutex<Duration>,
    waits: AtomicUsize,
    quanta: Mutex<Vec<Duration>>,
    hook: Mutex<Option<WaitHook>>,
}

impl Clock for FakeClock {
    fn now(&self) -> Duration { *lock(&self.now) }

    fn wait(&self, cancel: Option<&CancellationToken>, quantum: Duration) -> bool {
        let count = self.waits.fetch_add(1, Ordering::SeqCst) + 1;
        *lock(&self.now) += quantum;
        lock(&self.quanta).push(quantum);
        if let Some(hook) = lock(&self.hook).as_ref() {
            hook(count);
        }
        cancel.is_some_and(CancellationToken::is_cancelled)
    }
}

struct FakeFactory {
    shared: Arc<Mutex<Shared>>,
    spawns: AtomicUsize,
}

impl ProcessFactory for FakeFactory {
    fn spawn(&self, spec: &SpawnSpec) -> io::Result<Box<dyn ChildProcess>> {
        assert_eq!(
            spec.stdio,
            StdioMode::Capture,
            "streams use piped stdio and a null stdin"
        );
        self.spawns.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeChild(Arc::clone(&self.shared))))
    }
}

struct Rig {
    executor: CommandExecutor,
    clock: Arc<FakeClock>,
    shared: Arc<Mutex<Shared>>,
    factory: Arc<FakeFactory>,
}

fn rig(setup: impl FnOnce(&mut Shared)) -> Rig {
    let mut shared = Shared::default();
    setup(&mut shared);
    let shared = Arc::new(Mutex::new(shared));
    let clock = Arc::new(FakeClock::default());
    let factory = Arc::new(FakeFactory {
        shared: Arc::clone(&shared),
        spawns: AtomicUsize::new(0),
    });
    let executor = CommandExecutor::with_parts(
        Arc::clone(&factory) as Arc<dyn ProcessFactory>,
        Arc::clone(&clock) as Arc<dyn Clock>,
        CommandEnvironment::default(),
    );
    Rig {
        executor,
        clock,
        shared,
        factory,
    }
}

fn prepared() -> PreparedCommand {
    PreparedCommand {
        program: PathBuf::from("/fake/adapter"),
        arguments: Vec::new(),
    }
}

fn open(rig: &Rig, limits: StreamLimits) -> LineStream {
    let cancel = CancellationToken::new();
    rig.executor
        .open_line_stream(&prepared(), limits, &cancel, &cancel)
        .expect("open")
}

fn small(cap: usize) -> StreamLimits {
    StreamLimits::new(cap, 16, Duration::from_millis(100)).expect("limits")
}

// ---------------------------------------------------------------------------
// Stream behaviour over fakes
// ---------------------------------------------------------------------------

#[test]
fn lines_arrive_in_order_then_closed_repeats_with_the_exit_status() {
    let rig = rig(|shared| {
        shared.stdout.extend([b"{\"a\":1}\n{\"b\"".to_vec(), b":2}\n".to_vec()]);
        shared.stdout_closed = true;
        shared.exit = Some(3 << 8);
    });
    let cancel = CancellationToken::new();
    let mut stream = open(&rig, small(64));
    assert_eq!(stream.next_event(&cancel), line("{\"a\":1}"));
    assert_eq!(stream.next_event(&cancel), line("{\"b\":2}"));
    let closed = stream.next_event(&cancel);
    let StreamEvent::Closed { status: Some(status) } = closed else {
        panic!("{closed:?}")
    };
    assert_eq!(status.code(), Some(3));
    assert!(matches!(stream.next_event(&cancel), StreamEvent::Closed { .. }));
    assert_eq!(stream.stop(), StopOutcome::Exited { status });
    assert!(
        lock(&rig.shared).signals.is_empty(),
        "an exited child is not signalled"
    );
}

#[test]
fn the_consumers_pace_is_the_queue_stdout_is_read_only_on_demand() {
    let rig = rig(|shared| {
        shared.stdout.extend((0..50).map(|index| format!("line{index}\n").into_bytes()));
    });
    let cancel = CancellationToken::new();
    let mut stream = open(&rig, small(64));
    assert_eq!(stream.next_event(&cancel), line("line0"));
    let reads = lock(&rig.shared).reads;
    assert_eq!(
        reads, 1,
        "one chunk read for the first line; the rest stays in the pipe"
    );
    assert_eq!(lock(&rig.shared).stdout.len(), 49);
    drop(stream);
}

#[test]
fn a_stderr_flood_is_bounded_and_does_not_block_stdout_events() {
    let rig = rig(|shared| {
        shared.stderr.extend((0..200).map(|_| vec![b'e'; 16 * 1024]));
        shared.stdout.push_back(b"alive\n".to_vec());
    });
    let cancel = CancellationToken::new();
    let mut stream = open(&rig, small(64));
    assert_eq!(stream.next_event(&cancel), line("alive"));
    assert!(stream.stderr_tail().len() <= 16);
    assert!(stream.stderr_dropped_bytes() > 0);
    assert!(
        lock(&rig.shared).stderr.len() >= 200 - 4 - 4,
        "drain is rate limited per pull, so the flood cannot starve stdout"
    );
    drop(stream);
}

#[test]
fn cancellation_wakes_a_blocked_read_and_wins_over_queued_lines() {
    let rig = rig(|_| {});
    let cancel = CancellationToken::new();
    let hook_cancel = cancel.clone();
    *lock(&rig.clock.hook) = Some(Box::new(move |count| {
        if count == 3 {
            hook_cancel.cancel();
        }
    }));
    let mut stream = open(&rig, small(64));
    assert_eq!(stream.next_event(&cancel), StreamEvent::Cancelled);
    assert_eq!(rig.clock.waits.load(Ordering::SeqCst), 3);
    // Queued data is ignored once cancelled.
    lock(&rig.shared).stdout.push_back(b"late\n".to_vec());
    assert_eq!(stream.next_event(&cancel), StreamEvent::Cancelled);
    lock(&rig.shared).term_exits = true;
    assert!(matches!(stream.stop(), StopOutcome::Exited { .. }));
}

#[test]
fn stop_sends_term_to_the_child_and_reports_the_reaped_status() {
    let rig = rig(|shared| shared.term_exits = true);
    let outcome = open(&rig, small(64)).stop();
    assert_eq!(outcome, StopOutcome::Exited {
        status: ExitStatus::from_raw(15)
    });
    assert_eq!(lock(&rig.shared).signals, ["term"]);
    assert!(rig.executor.sweep().is_empty());
}

#[test]
fn a_child_that_ignores_term_is_killed_after_the_grace_and_reaped() {
    let rig = rig(|shared| shared.kill_exits = true);
    let outcome = open(&rig, small(64)).stop();
    assert_eq!(outcome, StopOutcome::Killed {
        status: ExitStatus::from_raw(9)
    });
    assert_eq!(lock(&rig.shared).signals, ["term", "kill"]);
    assert!(
        rig.clock.now() >= Duration::from_millis(100),
        "the grace elapsed before the kill"
    );
    assert!(rig.executor.sweep().is_empty());
}

#[test]
fn a_child_that_never_exits_reports_reap_timeout_and_stays_owned() {
    let rig = rig(|_| {});
    let outcome = open(&rig, small(64)).stop();
    let StopOutcome::ReapTimeout { diagnostic } = outcome else {
        panic!("{outcome:?}")
    };
    assert!(diagnostic.contains("retained"), "{diagnostic}");
    assert_eq!(
        lock(&rig.shared).signals,
        ["term", "kill"],
        "a kill was issued but not proven"
    );
    assert_eq!(
        rig.executor.sweep().len(),
        1,
        "the exact handle sits in the reaper"
    );
    // The slot is released once the child is finally gone.
    lock(&rig.shared).exit = Some(9);
    assert!(rig.executor.sweep().is_empty());
}

#[test]
fn dropping_an_unstopped_stream_kills_the_child_and_retains_an_unreaped_one() {
    let rig = rig(|_| {});
    drop(open(&rig, small(64)));
    assert_eq!(lock(&rig.shared).signals, ["kill"]);
    assert_eq!(rig.executor.sweep().len(), 1);

    let rig = rig_with_kill_exit();
    drop(open(&rig, small(64)));
    assert!(rig.executor.sweep().is_empty());
}

fn rig_with_kill_exit() -> Rig { rig(|shared| shared.kill_exits = true) }

#[test]
fn opening_respects_cancellation_and_the_spawn_gate() {
    let rig = rig(|_| {});
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let result = rig.executor.open_line_stream(&prepared(), small(64), &cancelled, &cancelled);
    assert!(matches!(result, Err(StreamOpenError::Cancelled)));
    let live = CancellationToken::new();
    let result = rig.executor.open_line_stream(&prepared(), small(64), &live, &cancelled);
    assert!(matches!(result, Err(StreamOpenError::Cancelled)));
    assert_eq!(rig.factory.spawns.load(Ordering::SeqCst), 0);
}

#[test]
fn oversized_and_invalid_lines_surface_as_events_without_ending_the_stream() {
    let rig = rig(|shared| {
        shared.stdout.extend([b"toolongline".to_vec(), b"\n\xff\nok\npart".to_vec()]);
        shared.stdout_closed = true;
        shared.exit = Some(0);
    });
    let cancel = CancellationToken::new();
    let mut stream = open(&rig, small(4));
    let mut events = Vec::new();
    loop {
        let event = stream.next_event(&cancel);
        let done = matches!(event, StreamEvent::Closed { .. });
        events.push(event);
        if done {
            break;
        }
    }
    assert_eq!(&events[..4], [
        StreamEvent::LineTooLong { limit: 4 },
        StreamEvent::InvalidUtf8 { bytes: 1 },
        line("ok"),
        StreamEvent::Unterminated { bytes: 4 },
    ]);
}

// ---------------------------------------------------------------------------
// Real processes (harmless /bin/sh fixtures, no descendants)
// ---------------------------------------------------------------------------

fn real_stream(script: &str, limits: StreamLimits) -> (CommandExecutor, LineStream) {
    let executor = CommandExecutor::new(CommandEnvironment::default());
    let prepared = PreparedCommand {
        program: PathBuf::from("/bin/sh"),
        arguments: vec!["-c".to_owned(), script.to_owned()],
    };
    let cancel = CancellationToken::new();
    let stream = executor.open_line_stream(&prepared, limits, &cancel, &cancel).expect("open");
    (executor, stream)
}

#[test]
fn a_real_child_streams_lines_stderr_and_its_exit_status() {
    let (executor, mut stream) = real_stream(
        "printf 'a\\nb\\n'; printf oops >&2; exit 4",
        StreamLimits::default(),
    );
    let cancel = CancellationToken::new();
    assert_eq!(stream.next_event(&cancel), line("a"));
    assert_eq!(stream.next_event(&cancel), line("b"));
    let outcome = loop {
        match stream.next_event(&cancel) {
            StreamEvent::Closed { status: Some(status) } => break status,
            StreamEvent::Closed { status: None } => {
                let _ = cancel.wait(Duration::from_millis(5));
            }
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(outcome.code(), Some(4));
    assert_eq!(stream.stderr_tail(), "oops");
    assert!(matches!(stream.stop(), StopOutcome::Exited { .. }));
    assert!(executor.sweep().is_empty());
}

#[test]
fn a_real_oversized_line_is_dropped_and_the_next_line_survives() {
    let (_executor, mut stream) = real_stream(
        "i=0; while [ $i -lt 500 ]; do printf xxxxxxxxxx; i=$((i+1)); done; printf '\\nok\\n'",
        small(1000),
    );
    let cancel = CancellationToken::new();
    assert_eq!(stream.next_event(&cancel), StreamEvent::LineTooLong {
        limit: 1000
    });
    assert_eq!(stream.next_event(&cancel), line("ok"));
    assert!(matches!(stream.stop(), StopOutcome::Exited { .. }));
}

#[test]
fn a_real_blocked_read_wakes_on_cancel_and_stop_term_reaps_the_exact_child() {
    // The child announces readiness first, so the shell is running (and the
    // signal reaches the exact child, not a half-started shell) before the cancel.
    let (executor, mut stream) = real_stream("echo ready; while :; do :; done", small(64));
    let cancel = CancellationToken::new();
    assert_eq!(stream.next_event(&cancel), line("ready"));
    let canceller = cancel.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        canceller.cancel();
    });
    let started = Instant::now();
    assert_eq!(stream.next_event(&cancel), StreamEvent::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(2));
    handle.join().expect("join");
    let StopOutcome::Exited { status } = stream.stop() else {
        panic!("expected a term exit")
    };
    assert_eq!(
        status.signal(),
        Some(15),
        "SIGTERM reached the exact child and it was reaped"
    );
    assert!(executor.sweep().is_empty());
}

#[test]
fn a_real_child_that_ignores_term_is_killed_and_reaped_without_a_zombie() {
    let (executor, mut stream) =
        real_stream("trap '' TERM; echo ready; while :; do :; done", small(64));
    // The trap is installed before the readiness line, so SIGTERM is ignored.
    assert_eq!(stream.next_event(&CancellationToken::new()), line("ready"));
    let StopOutcome::Killed { status } = stream.stop() else {
        panic!("expected a kill")
    };
    assert_eq!(status.signal(), Some(9));
    assert!(executor.sweep().is_empty());
}

// ---------------------------------------------------------------------------
// Idle wakeups
// ---------------------------------------------------------------------------

#[test]
fn an_idle_stream_backs_off_to_the_ceiling_and_wakes_at_most_four_times_a_second() {
    let rig = rig(|_| {});
    let cancel = CancellationToken::new();
    let hook_cancel = cancel.clone();
    let clock = Arc::clone(&rig.clock);
    *lock(&rig.clock.hook) = Some(Box::new(move |_| {
        if clock.now() >= Duration::from_secs(600) {
            hook_cancel.cancel();
        }
    }));
    let mut stream = open(&rig, small(64));
    assert_eq!(stream.next_event(&cancel), StreamEvent::Cancelled);
    let quanta = lock(&rig.clock.quanta).clone();
    let ms = |n: u64| Duration::from_millis(n);
    assert_eq!(quanta[..6], [ms(10), ms(20), ms(40), ms(80), ms(160), ms(250)]);
    assert!(
        quanta[5..].iter().all(|q| *q == IDLE_CEILING),
        "stays at the ceiling"
    );
    // 600 s idle: at most 4 wakeups per second plus the short ramp-up.
    assert!(quanta.len() <= 4 * 600 + 6, "{} wakeups", quanta.len());
    lock(&rig.shared).term_exits = true;
    assert!(matches!(stream.stop(), StopOutcome::Exited { .. }));
}

#[test]
fn output_resets_the_idle_wait_to_the_short_quantum() {
    let rig = rig(|_| {});
    let cancel = CancellationToken::new();
    let hook_cancel = cancel.clone();
    let shared = Arc::clone(&rig.shared);
    *lock(&rig.clock.hook) = Some(Box::new(move |count| {
        if count == 8 {
            lock(&shared).stdout.push_back(b"hello\n".to_vec());
        }
        if count == 11 {
            hook_cancel.cancel();
        }
    }));
    let mut stream = open(&rig, small(64));
    assert_eq!(stream.next_event(&cancel), line("hello"));
    assert_eq!(lock(&rig.clock.quanta).last(), Some(&IDLE_CEILING));
    assert_eq!(stream.next_event(&cancel), StreamEvent::Cancelled);
    let quanta = lock(&rig.clock.quanta).clone();
    assert_eq!(quanta[8..], [POLL_QUANTUM, POLL_QUANTUM * 2, POLL_QUANTUM * 4]);
    lock(&rig.shared).term_exits = true;
    assert!(matches!(stream.stop(), StopOutcome::Exited { .. }));
}

#[test]
fn a_real_idle_stream_observes_cancel_within_the_documented_bound() {
    let (executor, mut stream) = real_stream("exec sleep 30", small(64));
    let cancel = CancellationToken::new();
    let canceller = cancel.clone();
    let delay = Duration::from_millis(100);
    let handle = std::thread::spawn(move || {
        std::thread::sleep(delay);
        canceller.cancel();
    });
    let started = Instant::now();
    assert_eq!(stream.next_event(&cancel), StreamEvent::Cancelled);
    let elapsed = started.elapsed();
    assert!(
        elapsed < delay + IDLE_CEILING + Duration::from_millis(300),
        "cancel took {elapsed:?}"
    );
    handle.join().expect("join");
    let StopOutcome::Exited { status } = stream.stop() else {
        panic!("expected a term exit")
    };
    assert_eq!(status.signal(), Some(15));
    assert!(executor.sweep().is_empty());
}

#[test]
fn a_real_idle_stream_wakes_zero_times_until_cancelled() {
    let (executor, mut stream) = real_stream("echo ready; exec sleep 30", small(64));
    let cancel = CancellationToken::new();
    assert_eq!(stream.next_event(&cancel), line("ready"));
    let baseline = stream.idle_wakeups();
    let canceller = cancel.clone();
    let worker = std::thread::spawn(move || {
        let event = stream.next_event(&cancel);
        (stream, event, Instant::now())
    });
    // A polling stream would wake about four times a second over this window.
    std::thread::sleep(Duration::from_millis(1500));
    let cancelled_at = Instant::now();
    canceller.cancel();
    let (stream, event, returned_at) = worker.join().expect("join");
    assert_eq!(event, StreamEvent::Cancelled);
    assert_eq!(
        stream.idle_wakeups() - baseline,
        1,
        "only the cancel wake-up, none while idle"
    );
    assert!(
        returned_at.saturating_duration_since(cancelled_at) < Duration::from_millis(100 + 150),
        "cancel took {:?}",
        returned_at.saturating_duration_since(cancelled_at)
    );
    assert!(matches!(stream.stop(), StopOutcome::Exited { .. }));
    assert!(executor.sweep().is_empty());
}

#[test]
fn a_real_stream_cancelled_before_the_first_idle_wait_wakes_at_once() {
    let (executor, mut stream) = real_stream("echo ready; exec sleep 30", small(64));
    let cancel = CancellationToken::new();
    assert_eq!(stream.next_event(&cancel), line("ready"));
    cancel.cancel();
    assert_eq!(stream.next_event(&cancel), StreamEvent::Cancelled);
    assert!(matches!(stream.stop(), StopOutcome::Exited { .. }));
    assert!(executor.sweep().is_empty());
}
