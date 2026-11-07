//! KB-04-05: the `hidutil` boundary over the shared executor, fake processes and
//! fake time. No real process is spawned.

use std::collections::VecDeque;
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use stache_sdk::CancellationToken;
use stache_sdk::commands::{
    CANCEL_CLEANUP, ChildProcess, Clock, CommandEnvironment, CommandExecutor, POLL_QUANTUM,
    ProcessFactory, RETAINED_SLOTS, SpawnSpec,
};

use super::super::hidutil::{
    HIDUTIL_DEADLINE, HIDUTIL_PATH, HIDUTIL_STDERR_CAP, HIDUTIL_STDOUT_CAP, Hidutil,
    HidutilErrorKind, ProcessHidutil,
};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct FakeClock {
    now: Mutex<Duration>,
    waits: Mutex<Vec<Duration>>,
    cancel_on_wait: Mutex<Option<CancellationToken>>,
}

impl FakeClock {
    fn elapsed(&self) -> Duration { *lock(&self.now) }
}

impl Clock for FakeClock {
    fn now(&self) -> Duration { *lock(&self.now) }

    fn wait(&self, cancel: Option<&CancellationToken>, quantum: Duration) -> bool {
        lock(&self.waits).push(quantum);
        *lock(&self.now) += quantum;
        if let Some(token) = lock(&self.cancel_on_wait).as_ref() {
            token.cancel();
        }
        cancel.is_some_and(CancellationToken::is_cancelled)
    }
}

#[derive(Clone, Default)]
struct Script {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    /// Fake time at which the child exits by itself; `None` never.
    exit_at: Option<Duration>,
    exit_code: i32,
    dies_on_kill: bool,
    kill_fails: bool,
}

struct FakeChild {
    script: Script,
    clock: Arc<FakeClock>,
    killed: bool,
    stdout: VecDeque<u8>,
    stderr: VecDeque<u8>,
    alive: Arc<Mutex<bool>>,
}

impl FakeChild {
    fn exited(&self) -> bool {
        (self.killed && self.script.dies_on_kill)
            || self.script.exit_at.is_some_and(|at| self.clock.now() >= at)
    }

    fn read(queue: &mut VecDeque<u8>, exited: bool, buffer: &mut [u8]) -> io::Result<usize> {
        if queue.is_empty() {
            return if exited {
                Ok(0)
            } else {
                Err(io::ErrorKind::WouldBlock.into())
            };
        }
        let count = queue.len().min(buffer.len());
        for slot in &mut buffer[..count] {
            *slot = queue.pop_front().unwrap_or_default();
        }
        Ok(count)
    }
}

impl ChildProcess for FakeChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.exited() {
            *lock(&self.alive) = false;
            let code = if self.killed {
                9
            } else {
                self.script.exit_code << 8
            };
            Ok(Some(ExitStatus::from_raw(code)))
        } else {
            Ok(None)
        }
    }

    fn kill(&mut self) -> io::Result<()> {
        if self.script.kill_fails {
            return Err(io::Error::other("kill refused"));
        }
        self.killed = true;
        Ok(())
    }

    fn read_stdout(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let exited = self.exited();
        Self::read(&mut self.stdout, exited, buffer)
    }

    fn read_stderr(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let exited = self.exited();
        Self::read(&mut self.stderr, exited, buffer)
    }
}

struct FakeFactory {
    clock: Arc<FakeClock>,
    scripts: Mutex<VecDeque<Script>>,
    default: Script,
    specs: Mutex<Vec<SpawnSpec>>,
    alive: Mutex<Vec<Arc<Mutex<bool>>>>,
}

impl ProcessFactory for FakeFactory {
    fn spawn(&self, spec: &SpawnSpec) -> io::Result<Box<dyn ChildProcess>> {
        lock(&self.specs).push(spec.clone());
        let script = lock(&self.scripts).pop_front().unwrap_or_else(|| self.default.clone());
        let alive = Arc::new(Mutex::new(true));
        lock(&self.alive).push(Arc::clone(&alive));
        Ok(Box::new(FakeChild {
            stdout: script.stdout.iter().copied().collect(),
            stderr: script.stderr.iter().copied().collect(),
            script,
            clock: Arc::clone(&self.clock),
            killed: false,
            alive,
        }))
    }
}

struct Fixture {
    hidutil: ProcessHidutil,
    clock: Arc<FakeClock>,
    factory: Arc<FakeFactory>,
}

fn fixture(default: Script) -> Fixture {
    let clock = Arc::new(FakeClock::default());
    let factory = Arc::new(FakeFactory {
        clock: Arc::clone(&clock),
        scripts: Mutex::new(VecDeque::new()),
        default,
        specs: Mutex::new(Vec::new()),
        alive: Mutex::new(Vec::new()),
    });
    let executor = CommandExecutor::with_parts(
        Arc::clone(&factory) as Arc<dyn ProcessFactory>,
        Arc::clone(&clock) as Arc<dyn Clock>,
        CommandEnvironment::default(),
    );
    Fixture {
        hidutil: ProcessHidutil::new(executor),
        clock,
        factory,
    }
}

fn exits_at(at: Duration) -> Script {
    Script {
        exit_at: Some(at),
        dies_on_kill: true,
        ..Script::default()
    }
}

fn token() -> CancellationToken { CancellationToken::new() }

#[test]
fn kb_04_05_the_constants_are_the_specified_engineering_bounds() {
    assert_eq!(HIDUTIL_PATH, "/usr/bin/hidutil");
    assert_eq!(HIDUTIL_DEADLINE, Duration::from_secs(5));
    assert_eq!(HIDUTIL_STDOUT_CAP, 1024 * 1024);
    assert_eq!(HIDUTIL_STDERR_CAP, 64 * 1024);
    assert_eq!(CANCEL_CLEANUP, Duration::from_secs(2));
    assert_eq!(POLL_QUANTUM, Duration::from_millis(10));
}

#[test]
fn kb_04_05_get_and_set_run_the_exact_absolute_program_with_exact_arguments() {
    let fixture = fixture(Script {
        stdout: b"(null)\n".to_vec(),
        exit_at: Some(Duration::ZERO),
        ..Script::default()
    });
    assert_eq!(fixture.hidutil.get(&token()).expect("get"), b"(null)\n");
    fixture.hidutil.set("{\"UserKeyMapping\":[]}", &token()).expect("set");
    let specs = lock(&fixture.factory.specs).clone();
    assert_eq!(specs.len(), 2);
    assert!(specs.iter().all(|spec| spec.program.to_str() == Some("/usr/bin/hidutil")));
    assert_eq!(specs[0].arguments, ["property", "--get", "UserKeyMapping"]);
    assert_eq!(specs[1].arguments, [
        "property",
        "--set",
        "{\"UserKeyMapping\":[]}"
    ]);
}

#[test]
fn kb_04_05_stdout_is_capped_at_exactly_one_mebibyte_and_overflow_returns_nothing() {
    let at_cap = fixture(Script {
        stdout: vec![b'x'; HIDUTIL_STDOUT_CAP],
        exit_at: Some(Duration::ZERO),
        ..Script::default()
    });
    assert_eq!(
        at_cap.hidutil.get(&token()).expect("exactly the cap").len(),
        HIDUTIL_STDOUT_CAP
    );

    let over = fixture(Script {
        stdout: vec![b'x'; HIDUTIL_STDOUT_CAP + 1],
        dies_on_kill: true,
        ..Script::default()
    });
    let error = over.hidutil.get(&token()).expect_err("overflow");
    assert_eq!(error.kind, HidutilErrorKind::Overflow);
    assert!(!error.child_unconfirmed);
}

#[test]
fn kb_04_05_stderr_is_capped_at_exactly_sixty_four_kibibytes() {
    let at_cap = fixture(Script {
        stderr: vec![b'e'; HIDUTIL_STDERR_CAP],
        exit_at: Some(Duration::ZERO),
        ..Script::default()
    });
    at_cap.hidutil.get(&token()).expect("exactly the cap");
    let over = fixture(Script {
        stderr: vec![b'e'; HIDUTIL_STDERR_CAP + 1],
        dies_on_kill: true,
        ..Script::default()
    });
    assert_eq!(
        over.hidutil.get(&token()).expect_err("overflow").kind,
        HidutilErrorKind::Overflow
    );
}

#[test]
fn kb_04_05_the_child_lifetime_is_five_seconds_of_fake_time() {
    // Exits one poll before the deadline: success.
    let fine = fixture(exits_at(Duration::from_millis(4_990)));
    fine.hidutil.get(&token()).expect("inside the deadline");

    // Never exits: timed out at five seconds, then killed and reaped.
    let hung = fixture(Script {
        dies_on_kill: true,
        ..Script::default()
    });
    let error = hung.hidutil.set("{}", &token()).expect_err("timeout");
    assert_eq!(error.kind, HidutilErrorKind::TimedOut);
    assert!(error.may_have_mutated(), "a timeout cannot undo a write");
    assert!(!error.child_unconfirmed);
    let elapsed = hung.clock.elapsed();
    assert!(
        elapsed >= HIDUTIL_DEADLINE && elapsed <= HIDUTIL_DEADLINE + POLL_QUANTUM * 3,
        "{elapsed:?}"
    );
    assert_eq!(hung.hidutil.unreaped(), 0);
}

#[test]
fn kb_04_05_an_unkillable_child_is_retained_after_two_seconds_and_blocks_trust() {
    let stuck = fixture(Script {
        kill_fails: true,
        ..Script::default()
    });
    let error = stuck.hidutil.set("{}", &token()).expect_err("timeout");
    assert_eq!(error.kind, HidutilErrorKind::TimedOut);
    assert!(error.child_unconfirmed);
    assert!(error.message.contains("retained"), "{}", error.message);
    let elapsed = stuck.clock.elapsed();
    assert!(
        elapsed >= HIDUTIL_DEADLINE + CANCEL_CLEANUP,
        "cleanup polls for the full 2 s before retaining: {elapsed:?}"
    );
    assert_eq!(
        stuck.hidutil.unreaped(),
        1,
        "the exact handle is owned by the reaper"
    );
}

#[test]
fn kb_04_05_reaper_saturation_refuses_a_new_child_before_spawning_it() {
    let stuck = fixture(Script {
        kill_fails: true,
        ..Script::default()
    });
    for _ in 0..RETAINED_SLOTS {
        let error = stuck.hidutil.set("{}", &token()).expect_err("timeout");
        assert!(error.child_unconfirmed);
    }
    let spawned = lock(&stuck.factory.specs).len();
    assert_eq!(spawned, RETAINED_SLOTS);
    let error = stuck.hidutil.get(&token()).expect_err("saturated");
    assert_eq!(error.kind, HidutilErrorKind::NotRun);
    assert!(!error.may_have_mutated());
    assert_eq!(
        lock(&stuck.factory.specs).len(),
        spawned,
        "capacity is reserved before the spawn"
    );
    assert_eq!(stuck.hidutil.unreaped(), RETAINED_SLOTS);
}

#[test]
fn kb_04_05_a_retained_child_is_released_once_it_is_confirmed_gone() {
    // The kill is accepted but ignored; the child only exits at 20 s.
    let stuck = fixture(Script {
        exit_at: Some(Duration::from_secs(20)),
        ..Script::default()
    });
    let error = stuck.hidutil.set("{}", &token()).expect_err("timeout");
    assert!(error.child_unconfirmed);
    assert_eq!(stuck.hidutil.unreaped(), 1);
    stuck.clock.wait(None, Duration::from_secs(14));
    assert_eq!(stuck.hidutil.unreaped(), 0, "confirmed gone releases the slot");
    assert!(!*lock(&lock(&stuck.factory.alive)[0]));
}

#[test]
fn kb_04_05_a_nonzero_exit_and_a_cancelled_call_are_typed_not_successes() {
    let failing = fixture(Script {
        exit_at: Some(Duration::ZERO),
        exit_code: 1,
        stderr: b"denied".to_vec(),
        ..Script::default()
    });
    let error = failing.hidutil.get(&token()).expect_err("exit");
    assert_eq!(error.kind, HidutilErrorKind::Exited);
    assert!(error.message.contains("denied"));

    let before = fixture(Script::default());
    let cancelled = token();
    cancelled.cancel();
    let error = before.hidutil.get(&cancelled).expect_err("cancelled");
    assert_eq!(error.kind, HidutilErrorKind::NotRun);
    assert!(
        lock(&before.factory.specs).is_empty(),
        "no child is created after cancellation"
    );

    let during = fixture(Script {
        dies_on_kill: true,
        ..Script::default()
    });
    let cancel = token();
    *lock(&during.clock.cancel_on_wait) = Some(cancel.clone());
    let error = during.hidutil.set("{}", &cancel).expect_err("cancelled");
    assert_eq!(error.kind, HidutilErrorKind::Cancelled);
    assert!(
        error.may_have_mutated(),
        "a running write cannot be assumed undone"
    );
    assert!(!error.child_unconfirmed, "only that child was killed and reaped");
}
