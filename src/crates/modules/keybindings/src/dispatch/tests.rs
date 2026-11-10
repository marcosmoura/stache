//! Dispatcher tests over a fake process factory and fake time.
//!
//! No real user command is ever spawned: every command is `/bin/sh <tag>` and
//! the factory decides from the tag how the fake child behaves.

use std::io;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

use stache_sdk::commands::{
    ChildProcess, Clock, CommandEnvironment, CommandExecutor, ProcessFactory, SpawnGate, SpawnSpec,
};
use stache_sdk::input::WorkReservation;
use stache_sdk::threading::CancellationToken;

use super::*;

// ---------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------

/// Fake time: waits advance it without sleeping. A wait with a token also
/// yields for a real millisecond so a cancelling thread can make progress.
#[derive(Default)]
struct FakeClock {
    micros: AtomicU64,
}

impl Clock for FakeClock {
    fn now(&self) -> Duration { Duration::from_micros(self.micros.load(Ordering::SeqCst)) }

    fn wait(&self, cancel: Option<&CancellationToken>, quantum: Duration) -> bool {
        self.micros
            .fetch_add(u64::try_from(quantum.as_micros()).unwrap(), Ordering::SeqCst);
        cancel.is_some_and(|token| token.wait(Duration::from_millis(1)))
    }
}

struct Ctl {
    tag: String,
    exit: Mutex<Option<i32>>,
    dies_on_kill: bool,
    killed: AtomicBool,
}

impl Ctl {
    fn finish(&self, code: i32) { *self.exit.lock().unwrap() = Some(code); }
}

struct FakeChild(Arc<Ctl>);

impl ChildProcess for FakeChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.0.killed.load(Ordering::SeqCst) && self.0.dies_on_kill {
            return Ok(Some(ExitStatus::from_raw(9)));
        }
        Ok((*self.0.exit.lock().unwrap()).map(|code| ExitStatus::from_raw(code << 8)))
    }

    fn kill(&mut self) -> io::Result<()> {
        self.0.killed.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn read_stdout(&mut self, _: &mut [u8]) -> io::Result<usize> { Ok(0) }

    fn read_stderr(&mut self, _: &mut [u8]) -> io::Result<usize> { Ok(0) }
}

/// Tags: `quick*` exits 0 at once, `fail*` exits 1 at once, `stubborn*` ignores
/// kill, anything else runs until the test finishes it.
#[derive(Default)]
struct Factory {
    spawned: Mutex<Vec<Arc<Ctl>>>,
    count: AtomicUsize,
    entered: AtomicUsize,
    hook: Mutex<Option<Arc<Barrier>>>,
}

impl Factory {
    fn tags(&self) -> Vec<String> {
        self.spawned.lock().unwrap().iter().map(|ctl| ctl.tag.clone()).collect()
    }

    fn ctl(&self, tag: &str) -> Arc<Ctl> {
        self.spawned
            .lock()
            .unwrap()
            .iter()
            .find(|ctl| ctl.tag == tag)
            .cloned()
            .unwrap_or_else(|| panic!("{tag} was not spawned"))
    }

    fn finish_all(&self) {
        for ctl in self.spawned.lock().unwrap().iter() {
            ctl.finish(0);
        }
    }

    fn spawn_count(&self) -> usize { self.count.load(Ordering::SeqCst) }
}

impl ProcessFactory for Factory {
    fn spawn(&self, spec: &SpawnSpec) -> io::Result<Box<dyn ChildProcess>> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        let hook = self.hook.lock().unwrap().clone();
        if let Some(barrier) = hook {
            barrier.wait();
        }
        let tag = spec.arguments[0].clone();
        let exit = if tag.starts_with("quick") {
            Some(0)
        } else if tag.starts_with("fail") {
            Some(1)
        } else {
            None
        };
        let ctl = Arc::new(Ctl {
            dies_on_kill: !tag.starts_with("stubborn"),
            tag,
            exit: Mutex::new(exit),
            killed: AtomicBool::new(false),
        });
        self.spawned.lock().unwrap().push(Arc::clone(&ctl));
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeChild(ctl)))
    }
}

struct Rig {
    dispatcher: Dispatcher,
    factory: Arc<Factory>,
    diagnostics: Arc<Mutex<Vec<String>>>,
}

fn rig_with(config: DispatcherConfig) -> Rig {
    let factory = Arc::new(Factory::default());
    let executor = CommandExecutor::with_parts(
        Arc::clone(&factory) as Arc<dyn ProcessFactory>,
        Arc::new(FakeClock::default()),
        CommandEnvironment::default(),
    );
    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&diagnostics);
    let dispatcher = Dispatcher::with_config(
        executor,
        config,
        Arc::new(move |message: &str| sink.lock().unwrap().push(message.to_owned())),
    );
    Rig {
        dispatcher,
        factory,
        diagnostics,
    }
}

fn rig() -> Rig {
    rig_with(DispatcherConfig {
        spawn_drain: Duration::from_secs(5),
        shutdown_bound: Duration::from_secs(5),
    })
}

fn cmd(tag: &str) -> String { format!("/bin/sh {tag}") }

fn seq(tags: &[&str]) -> Vec<String> { tags.iter().map(|tag| cmd(tag)).collect() }

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(1));
    }
}

fn settle(rig: &Rig) {
    wait_until("idle", || {
        rig.factory.finish_all();
        let snapshot = rig.dispatcher.snapshot();
        snapshot.active == 0 && snapshot.waiting == 0
    });
}

/// Fills 16 active and 64 waiting with held single-step sequences `h0..h79`.
fn saturate(rig: &Rig) {
    for index in 0..CAPACITY {
        let admission = rig.dispatcher.submit(&seq(&[&format!("h{index}")])).unwrap();
        if index < MAX_ACTIVE {
            assert_eq!(admission, Admission::Active);
        } else {
            assert_eq!(admission, Admission::Queued { position: index - MAX_ACTIVE });
        }
    }
    wait_until("16 spawned", || rig.factory.spawn_count() == MAX_ACTIVE);
}

// ---------------------------------------------------------------------------
// KB-05-02
// ---------------------------------------------------------------------------

#[test]
fn lazy_zero_idle_tasks_and_completed_tasks_are_reaped() {
    let rig = rig();
    assert_eq!(rig.dispatcher.snapshot(), DispatchSnapshot::default());
    rig.dispatcher.submit(&seq(&["quick"])).unwrap();
    wait_until("completion", || rig.dispatcher.snapshot().completed == 1);
    wait_until("reap", || {
        rig.dispatcher.reap();
        rig.dispatcher.snapshot().tasks == 0
    });
    assert_eq!(rig.dispatcher.snapshot().active, 0);
}

#[test]
fn admits_16_active_and_64_waiting_and_rejects_the_81st() {
    let rig = rig();
    saturate(&rig);
    let snapshot = rig.dispatcher.snapshot();
    assert_eq!((snapshot.active, snapshot.waiting, snapshot.tasks), (16, 64, 16));
    assert_eq!(rig.dispatcher.submit(&seq(&["extra"])), Err(Rejection::Full));
    let snapshot = rig.dispatcher.snapshot();
    assert_eq!((snapshot.overflow_drops, snapshot.waiting), (1, 64));
    assert!(rig.diagnostics.lock().unwrap().iter().any(|message| message.contains("full")));
    settle(&rig);
    rig.dispatcher.shutdown();
    assert!(!rig.factory.tags().contains(&"extra".to_owned()));
}

#[test]
fn fifo_promotion_without_bypass_when_completion_order_differs() {
    let rig = rig();
    saturate(&rig);
    // Complete out of admission order: h5 then h3.
    rig.factory.ctl("h5").finish(0);
    wait_until("h16", || {
        rig.factory.tags().last().is_some_and(|tag| tag == "h16")
    });
    rig.factory.ctl("h3").finish(0);
    wait_until("h17", || {
        rig.factory.tags().last().is_some_and(|tag| tag == "h17")
    });
    // A newcomer cannot bypass the 62 waiters.
    assert_eq!(
        rig.dispatcher.submit(&seq(&["newcomer"])),
        Ok(Admission::Queued { position: 62 })
    );
    assert_eq!(rig.dispatcher.snapshot().active, 16);
    // Free one slot at a time and check each promotion in order.
    let mut expected: Vec<String> = (18..CAPACITY).map(|index| format!("h{index}")).collect();
    expected.push("newcomer".to_owned());
    let mut finished = 0;
    for tag in &expected {
        let victim = rig
            .factory
            .tags()
            .into_iter()
            .find(|tag| {
                let ctl = rig.factory.ctl(tag);
                ctl.exit.lock().unwrap().is_none()
            })
            .unwrap();
        rig.factory.ctl(&victim).finish(0);
        finished += 1;
        wait_until(tag, || rig.factory.tags().iter().any(|spawned| spawned == tag));
        assert_eq!(rig.factory.tags().last(), Some(tag), "promotion {finished}");
    }
    settle(&rig);
    assert_eq!(rig.dispatcher.snapshot().completed, 81);
    rig.dispatcher.shutdown();
}

#[test]
fn a_long_running_child_occupies_only_its_slot() {
    let rig = rig();
    rig.dispatcher.submit(&seq(&["long"])).unwrap();
    wait_until("long spawned", || rig.factory.spawn_count() == 1);
    for _ in 0..40 {
        rig.dispatcher.submit(&seq(&["quick"])).unwrap();
    }
    wait_until("others complete", || rig.dispatcher.snapshot().completed == 40);
    let snapshot = rig.dispatcher.snapshot();
    assert_eq!((snapshot.active, snapshot.waiting), (1, 0));
    assert!(!rig.factory.ctl("long").killed.load(Ordering::SeqCst));
    rig.factory.ctl("long").finish(0);
    wait_until("long done", || rig.dispatcher.snapshot().completed == 41);
    rig.dispatcher.shutdown();
}

#[test]
fn two_presses_overlap_with_sequential_independently_aborting_steps() {
    let rig = rig();
    let commands = seq(&["a", "b"]);
    rig.dispatcher.submit(&commands).unwrap();
    rig.dispatcher.submit(&commands).unwrap();
    wait_until("both first steps", || rig.factory.spawn_count() == 2);
    assert_eq!(rig.factory.tags(), ["a", "a"]);
    assert_eq!(rig.dispatcher.snapshot().active, 2);
    let (first, second) = {
        let spawned = rig.factory.spawned.lock().unwrap();
        (Arc::clone(&spawned[0]), Arc::clone(&spawned[1]))
    };
    // The first press fails at step one: its step two never runs.
    first.finish(1);
    wait_until("first press failed", || rig.dispatcher.snapshot().failed == 1);
    assert_eq!(rig.factory.spawn_count(), 2);
    // The second press is unaffected and proceeds to step two only after step one.
    second.finish(0);
    wait_until("second step", || rig.factory.spawn_count() == 3);
    assert_eq!(rig.factory.tags(), ["a", "a", "b"]);
    rig.factory.ctl("b").finish(0);
    wait_until("second press done", || rig.dispatcher.snapshot().completed == 1);
    rig.dispatcher.shutdown();
}

#[test]
fn a_superseded_tag_starts_no_further_child_before_invalidation() {
    let rig = rig();
    let current = Arc::new(AtomicU64::new(1));
    let seen = Arc::clone(&current);
    assert!(
        rig.dispatcher
            .set_currency_guard(Arc::new(move |tag| seen.load(Ordering::SeqCst) == tag))
    );
    let generation = rig.dispatcher.generation();
    let commands: Arc<[String]> = seq(&["a", "b"]).into();
    rig.dispatcher.submit_tagged(generation, 1, &commands).unwrap();
    wait_until("first step", || rig.factory.spawn_count() == 1);
    // A newer revision is published; the dispatcher has not been invalidated.
    current.store(2, Ordering::SeqCst);
    rig.factory.ctl("a").finish(0);
    wait_until("sequence stopped", || {
        let snapshot = rig.dispatcher.snapshot();
        snapshot.active == 0 && snapshot.cancelled + snapshot.failed + snapshot.completed == 1
    });
    assert_eq!(
        rig.factory.tags(),
        ["a"],
        "step two of a superseded sequence never starts"
    );
    assert_eq!(
        rig.dispatcher.generation(),
        generation,
        "no invalidation was needed"
    );
    // Work tagged with the current revision still runs.
    rig.dispatcher.submit_tagged(generation, 2, &commands).unwrap();
    wait_until("current work", || rig.factory.spawn_count() == 2);
    rig.factory.finish_all();
    rig.dispatcher.shutdown();
}

#[test]
fn invalidation_cancels_all_active_and_waiting_with_no_stale_spawn() {
    let rig = rig();
    saturate(&rig);
    let old_generation = rig.dispatcher.generation();
    let invalidated = rig.dispatcher.invalidate();
    assert_eq!(invalidated.discarded, 64);
    assert!(invalidated.drained);
    assert_eq!(invalidated.generation, old_generation + 1);
    wait_until("slots cleared", || rig.dispatcher.snapshot().active == 0);
    let snapshot = rig.dispatcher.snapshot();
    assert_eq!(
        (snapshot.waiting, snapshot.discarded, snapshot.cancelled),
        (0, 64, 16)
    );
    assert_eq!(rig.factory.spawn_count(), 16, "no waiter ever started");
    for ctl in rig.factory.spawned.lock().unwrap().iter() {
        assert!(ctl.killed.load(Ordering::SeqCst), "{} was not killed", ctl.tag);
    }
    // Nothing is leaked: every killed child was confirmed reaped.
    assert_eq!(snapshot.retained, 0);
    // A press captured before the invalidation is stale; the new one works.
    assert_eq!(
        rig.dispatcher.submit_at(old_generation, &seq(&["quick"])),
        Err(Rejection::Stale)
    );
    assert_eq!(rig.dispatcher.snapshot().stale_drops, 1);
    rig.dispatcher.submit_at(invalidated.generation, &seq(&["quick"])).unwrap();
    wait_until("new generation runs", || {
        rig.dispatcher.snapshot().completed == 1
    });
    rig.dispatcher.shutdown();
}

#[test]
fn an_unchanged_reload_preserves_every_accepted_sequence() {
    let rig = rig();
    saturate(&rig);
    // A rejected or unchanged candidate never calls invalidate.
    let snapshot = rig.dispatcher.snapshot();
    assert_eq!(
        (snapshot.active, snapshot.waiting, snapshot.cancelled),
        (16, 64, 0)
    );
    settle(&rig);
    wait_until("all ran", || rig.dispatcher.snapshot().completed == 80);
    let snapshot = rig.dispatcher.snapshot();
    assert_eq!((snapshot.cancelled, snapshot.discarded), (0, 0));
    rig.dispatcher.shutdown();
}

#[test]
fn spawn_gate_serializes_invalidation_against_child_creation() {
    let rig = rig();
    let barrier = Arc::new(Barrier::new(2));
    *rig.factory.hook.lock().unwrap() = Some(Arc::clone(&barrier));
    rig.dispatcher.submit(&seq(&["racing", "never"])).unwrap();
    let before = rig.dispatcher.generation();
    wait_until("worker inside the gate", || {
        rig.factory.entered.load(Ordering::SeqCst) == 1
    });

    // The worker is now inside the gate and blocked creating its child.
    let invalidator = {
        let dispatcher = rig.dispatcher.clone();
        thread::spawn(move || dispatcher.invalidate())
    };
    wait_until("generation bumped", || rig.dispatcher.generation() == before + 1);
    assert!(
        !invalidator.is_finished(),
        "invalidation must wait for the spawn in progress"
    );
    barrier.wait();
    let invalidated = invalidator.join().unwrap();
    assert!(invalidated.drained);

    // The child that was already created is cancelled; the next step never runs.
    *rig.factory.hook.lock().unwrap() = None;
    wait_until("idle", || rig.dispatcher.snapshot().active == 0);
    assert_eq!(rig.factory.tags(), ["racing"]);
    assert!(rig.factory.ctl("racing").killed.load(Ordering::SeqCst));

    // Once invalidation has won, a stale gate admits nothing.
    let inner = &rig.dispatcher.inner;
    let gate = GenerationGate {
        inner,
        generation: before,
        tag: None,
        token: CancellationToken::new(),
    };
    assert!(gate.admit().is_none());
    rig.dispatcher.shutdown();
}

#[test]
fn no_process_starts_after_invalidation_returns() {
    for _ in 0..60 {
        let rig = rig();
        for _ in 0..MAX_ACTIVE {
            rig.dispatcher.submit(&seq(&["quick1", "quick2", "quick3"])).unwrap();
        }
        let invalidated = rig.dispatcher.invalidate();
        assert!(invalidated.drained);
        let count = rig.factory.spawn_count();
        wait_until("idle", || rig.dispatcher.snapshot().active == 0);
        assert_eq!(rig.factory.spawn_count(), count, "a stale process started");
        rig.dispatcher.shutdown();
    }
}

#[test]
fn empty_capture_is_admitted_at_saturation() {
    let rig = rig();
    saturate(&rig);
    assert_eq!(rig.dispatcher.submit(&[]), Ok(Admission::Bypassed));
    let snapshot = rig.dispatcher.snapshot();
    assert_eq!(
        (snapshot.active, snapshot.waiting, snapshot.overflow_drops),
        (16, 64, 0)
    );
    settle(&rig);
    rig.dispatcher.shutdown();
}

#[test]
fn retained_ownership_exhaustion_blocks_new_dispatch() {
    let rig = rig();
    // Three rounds of 16 children that ignore kill leave 48 retained.
    for round in 0..3 {
        for index in 0..MAX_ACTIVE {
            rig.dispatcher.submit(&seq(&[&format!("stubborn{round}-{index}")])).unwrap();
        }
        wait_until("round spawned", || {
            rig.factory.spawn_count() == (round + 1) * MAX_ACTIVE
        });
        rig.dispatcher.invalidate();
        wait_until("round retained", || {
            let snapshot = rig.dispatcher.snapshot();
            snapshot.active == 0 && snapshot.retained == (round + 1) * MAX_ACTIVE
        });
    }
    assert_eq!(rig.dispatcher.snapshot().retained_total, 48);
    // 48 retained + 16 active uses every ownership slot.
    for index in 0..MAX_ACTIVE {
        rig.dispatcher.submit(&seq(&[&format!("h{index}")])).unwrap();
    }
    assert_eq!(
        rig.dispatcher.submit(&seq(&["blocked"])),
        Err(Rejection::Blocked { retained: 48 })
    );
    let reservations = rig.dispatcher.reservations();
    assert!(WorkReservation::try_new(&reservations).is_err());
    assert_eq!(rig.dispatcher.snapshot().blocked_drops, 2);
    // An empty capture still passes.
    assert_eq!(rig.dispatcher.submit(&[]), Ok(Admission::Bypassed));
    // Reaping the retained children reopens dispatch; nothing was leaked.
    wait_until("held children spawned", || rig.factory.spawn_count() == 64);
    for ctl in rig.factory.spawned.lock().unwrap().iter() {
        ctl.finish(0);
    }
    wait_until("idle", || rig.dispatcher.snapshot().active == 0);
    assert_eq!(rig.dispatcher.submit(&seq(&["quick"])), Ok(Admission::Active));
    wait_until("dispatch resumed", || rig.dispatcher.snapshot().retained == 0);
    rig.dispatcher.shutdown();
}

#[test]
fn reservations_share_one_exact_budget_with_the_dispatcher() {
    let rig = rig();
    let owner = rig.dispatcher.reservations();
    let mut held: Vec<WorkReservation> =
        (0..CAPACITY).map(|_| WorkReservation::try_new(&owner).unwrap()).collect();
    assert_eq!(rig.dispatcher.snapshot().reserved, 80);
    // The 81st reservation and a direct submission both see a full budget.
    assert!(WorkReservation::try_new(&owner).is_err());
    assert_eq!(rig.dispatcher.submit(&seq(&["direct"])), Err(Rejection::Full));
    assert_eq!(rig.dispatcher.snapshot().overflow_drops, 2);

    // Dropping one unused reservation frees exactly one slot.
    drop(held.pop());
    assert_eq!(rig.dispatcher.snapshot().reserved, 79);
    drop(WorkReservation::try_new(&owner).unwrap());
    assert_eq!(rig.dispatcher.snapshot().reserved, 79);

    // Converting moves a reservation into an active or queued sequence.
    for index in 0..MAX_ACTIVE {
        let reservation = held.pop().unwrap();
        let admission = rig
            .dispatcher
            .submit_reserved(reservation, None, &seq(&[&format!("h{index}")]))
            .unwrap();
        assert_eq!(admission, Admission::Active);
    }
    let snapshot = rig.dispatcher.snapshot();
    assert_eq!((snapshot.active, snapshot.reserved), (16, 63));
    let queued = held.pop().unwrap();
    assert_eq!(
        rig.dispatcher.submit_reserved(queued, None, &seq(&["q"])),
        Ok(Admission::Queued { position: 0 })
    );
    let snapshot = rig.dispatcher.snapshot();
    assert_eq!(
        (snapshot.active, snapshot.waiting, snapshot.reserved),
        (16, 1, 62)
    );
    // An empty capture releases its reservation without using capacity.
    let empty = held.pop().unwrap();
    assert_eq!(
        rig.dispatcher.submit_reserved(empty, None, &[]),
        Ok(Admission::Bypassed)
    );
    assert_eq!(rig.dispatcher.snapshot().reserved, 61);
    // A stale reservation is released, not leaked.
    let stale = held.pop().unwrap();
    let old = rig.dispatcher.generation();
    let _ = rig.dispatcher.invalidate();
    assert_eq!(
        rig.dispatcher.submit_reserved(stale, Some(old), &seq(&["late"])),
        Err(Rejection::Stale)
    );
    assert_eq!(rig.dispatcher.snapshot().reserved, 60);
    drop(held);
    assert_eq!(rig.dispatcher.snapshot().reserved, 0);
    settle(&rig);
    rig.dispatcher.shutdown();
}

#[test]
fn a_foreign_reservation_is_refused_and_released_by_its_owner() {
    let first = rig();
    let second = rig();
    let reservation = WorkReservation::try_new(&first.dispatcher.reservations()).unwrap();
    assert_eq!(
        second.dispatcher.submit_reserved(reservation, None, &seq(&["x"])),
        Err(Rejection::ForeignReservation)
    );
    assert_eq!(first.dispatcher.snapshot().reserved, 0);
    assert_eq!(second.dispatcher.snapshot().reserved, 0);
}

#[test]
fn shutdown_is_bounded_with_a_child_that_ignores_kill() {
    let rig = rig();
    rig.dispatcher.submit(&seq(&["stubborn"])).unwrap();
    wait_until("spawned", || rig.factory.spawn_count() == 1);
    let started = Instant::now();
    let report = rig.dispatcher.shutdown();
    assert!(started.elapsed() < Duration::from_secs(4));
    assert_eq!(report, ShutdownReport { detached: 0, retained: 1 });
    assert_eq!(rig.dispatcher.snapshot().retained_total, 1);
    assert_eq!(rig.dispatcher.submit(&seq(&["quick"])), Err(Rejection::Closed));
    assert!(WorkReservation::try_new(&rig.dispatcher.reservations()).is_err());
}

#[test]
fn shutdown_detaches_a_task_stuck_in_process_creation() {
    let rig = rig_with(DispatcherConfig {
        spawn_drain: Duration::from_millis(50),
        shutdown_bound: Duration::from_millis(100),
    });
    let barrier = Arc::new(Barrier::new(2));
    *rig.factory.hook.lock().unwrap() = Some(Arc::clone(&barrier));
    rig.dispatcher.submit(&seq(&["stuck"])).unwrap();
    wait_until("task started", || rig.dispatcher.snapshot().active == 1);
    thread::sleep(Duration::from_millis(20));
    let started = Instant::now();
    let report = rig.dispatcher.shutdown();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(report.detached, 1);
    // Releasing the stuck spawn lets the detached task finish inertly.
    barrier.wait();
    wait_until("detached task exits", || {
        rig.factory.spawn_count() == 0 || rig.factory.ctl("stuck").killed.load(Ordering::SeqCst)
    });
}

#[test]
fn snapshot_reads_do_not_wait_on_the_core_lock() {
    let rig = rig();
    let guard = rig.dispatcher.inner.core.lock().unwrap();
    let snapshot = rig.dispatcher.snapshot();
    assert_eq!(snapshot.active, 0);
    drop(guard);
}
