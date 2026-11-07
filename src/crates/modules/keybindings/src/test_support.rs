//! Fakes for the Keybindings runtime.
//!
//! Everything here is in memory: the Carbon/TIS layer is `FakeLog`, the tap is
//! the fake input backend, `hidutil` and the journal are the Caps `World`,
//! processes are a scripted factory and time is fake. Nothing registers a real
//! hotkey, installs a real tap, runs `hidutil` or spawns a real command, so
//! default tests never touch the machine they run on.

use std::io;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use stache_macos::caps_remap::testing::World;
use stache_macos::caps_remap::{CapsRemap, Mapping};
use stache_macos::hotkeys::testing::FakeLog;
use stache_macos::hotkeys::{HotkeyControl, HotkeySink};
use stache_sdk::commands::{
    ChildProcess, Clock, CommandEnvironment, CommandExecutor, ProcessFactory, SpawnSpec,
};
use stache_sdk::input::testing::{FakeBackend, FakeClock as FakeInputClock};
use stache_sdk::input::{InputBackend, InputPriority, InputService};
use stache_sdk::threading::CancellationToken;
use stache_sdk::{MainThreadExecutor, StartupCommands, ThreadError};

use crate::KeybindingsConfig;
use crate::caps::CapsOwner;
use crate::dispatch::{Dispatcher, DispatcherConfig};
use crate::runtime::{KeybindingsParts, KeybindingsRuntime, Router, RuntimeTiming};
use crate::status::DiagnosticLog;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Fake time: waits advance it without sleeping; a wait with a token also
/// yields for a real millisecond so a cancelling thread can make progress.
#[derive(Default)]
pub struct FakeTime {
    micros: AtomicU64,
}

impl Clock for FakeTime {
    fn now(&self) -> Duration { Duration::from_micros(self.micros.load(Ordering::SeqCst)) }

    fn wait(&self, cancel: Option<&CancellationToken>, quantum: Duration) -> bool {
        self.micros
            .fetch_add(u64::try_from(quantum.as_micros()).unwrap_or(0), Ordering::SeqCst);
        cancel.is_some_and(|token| token.wait(Duration::from_millis(1)))
    }
}

/// One fake child.
pub struct Child {
    /// The tag (first argument).
    pub tag: String,
    exit: Mutex<Option<i32>>,
    killed: AtomicBool,
}

impl Child {
    /// Makes the child exit with `code`.
    pub fn finish(&self, code: i32) { *lock(&self.exit) = Some(code); }

    /// Whether the runner killed it.
    #[must_use]
    pub fn killed(&self) -> bool { self.killed.load(Ordering::SeqCst) }
}

struct FakeChild(Arc<Child>);

impl ChildProcess for FakeChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.0.killed() {
            return Ok(Some(ExitStatus::from_raw(9)));
        }
        Ok((*lock(&self.0.exit)).map(|code| ExitStatus::from_raw(code << 8)))
    }

    fn kill(&mut self) -> io::Result<()> {
        self.0.killed.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn read_stdout(&mut self, _: &mut [u8]) -> io::Result<usize> { Ok(0) }

    fn read_stderr(&mut self, _: &mut [u8]) -> io::Result<usize> { Ok(0) }
}

/// A process factory: a command `/bin/sh <tag>` starts a child whose behavior
/// the tag selects. `quick*` exits 0 at once, `fail*` exits 1 at once, anything
/// else runs until the test finishes it.
#[derive(Default)]
pub struct FakeProcesses {
    spawned: Mutex<Vec<Arc<Child>>>,
}

impl FakeProcesses {
    /// Tags spawned so far, in order.
    #[must_use]
    pub fn tags(&self) -> Vec<String> {
        lock(&self.spawned).iter().map(|child| child.tag.clone()).collect()
    }

    /// How many children were spawned.
    #[must_use]
    pub fn count(&self) -> usize { lock(&self.spawned).len() }

    /// The child with `tag`.
    ///
    /// # Panics
    /// When no such child was spawned.
    #[must_use]
    pub fn child(&self, tag: &str) -> Arc<Child> {
        lock(&self.spawned)
            .iter()
            .find(|child| child.tag == tag)
            .cloned()
            .unwrap_or_else(|| panic!("`{tag}` was not spawned"))
    }

    /// Finishes every held child.
    pub fn finish_all(&self) {
        for child in lock(&self.spawned).iter() {
            child.finish(0);
        }
    }
}

impl ProcessFactory for FakeProcesses {
    fn spawn(&self, spec: &SpawnSpec) -> io::Result<Box<dyn ChildProcess>> {
        let tag = spec.arguments.first().cloned().unwrap_or_default();
        let exit = if tag.starts_with("quick") {
            Some(0)
        } else if tag.starts_with("fail") {
            Some(1)
        } else {
            None
        };
        let child = Arc::new(Child {
            tag,
            exit: Mutex::new(exit),
            killed: AtomicBool::new(false),
        });
        lock(&self.spawned).push(Arc::clone(&child));
        Ok(Box::new(FakeChild(child)))
    }
}

type MainJob = Box<dyn FnOnce() + Send>;

/// A real thread that plays the main thread: every job runs on it, in order, so
/// the thread-local Carbon store and the fake handler registry behave as they do
/// on the real main thread.
pub struct PumpMain {
    sender: Mutex<Option<std::sync::mpsc::Sender<MainJob>>>,
    thread: std::thread::ThreadId,
    reject: AtomicBool,
}

impl PumpMain {
    /// Starts the pump thread.
    ///
    /// # Panics
    /// When the thread cannot be created.
    #[must_use]
    pub fn new() -> Arc<Self> {
        let (sender, receiver) = std::sync::mpsc::channel::<MainJob>();
        let handle = std::thread::Builder::new()
            .name("keybindings-test-main".to_owned())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    job();
                }
            })
            .expect("spawn the fake main thread");
        Arc::new(Self {
            sender: Mutex::new(Some(sender)),
            thread: handle.thread().id(),
            reject: AtomicBool::new(false),
        })
    }

    /// Makes new jobs fail as if the main thread were gone.
    pub fn reject(&self, reject: bool) { self.reject.store(reject, Ordering::SeqCst); }

    /// Runs `work` on the pump and returns its result.
    ///
    /// # Panics
    /// When the pump is gone.
    pub fn run<T: Send + 'static>(&self, work: impl FnOnce() -> T + Send + 'static) -> T {
        let (done, result) = std::sync::mpsc::channel();
        self.execute(Box::new(move || {
            let _ = done.send(work());
        }))
        .expect("the pump accepts jobs");
        result.recv().expect("the pump answers")
    }
}

impl MainThreadExecutor for PumpMain {
    fn is_main_thread(&self) -> bool { std::thread::current().id() == self.thread }

    fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
        if self.reject.load(Ordering::SeqCst) {
            return Err(ThreadError::GroupStopped);
        }
        let sender = lock(&self.sender);
        sender
            .as_ref()
            .ok_or(ThreadError::GroupStopped)
            .and_then(|sender| sender.send(job).map_err(|_| ThreadError::GroupStopped))
    }
}

/// `/bin/sh <tag>`: the command a fake child is spawned for.
#[must_use]
pub fn cmd(tag: &str) -> String { format!("/bin/sh {tag}") }

/// A configuration from `(shortcut, tags)` pairs; an empty tag list captures
/// without spawning.
#[must_use]
pub fn config(enabled: bool, bindings: &[(&str, &[&str])]) -> KeybindingsConfig {
    KeybindingsConfig {
        enabled,
        bindings: bindings
            .iter()
            .map(|(shortcut, tags)| {
                let commands = tags.iter().map(|tag| cmd(tag)).collect::<Vec<_>>();
                let value = match commands.len() {
                    0 => StartupCommands::Single(String::new()),
                    1 => StartupCommands::Single(commands[0].clone()),
                    _ => StartupCommands::Multiple(commands),
                };
                ((*shortcut).to_owned(), value)
            })
            .collect(),
    }
}

/// Polls `condition` until it holds, panicking after ten seconds.
///
/// # Panics
/// When the condition never holds.
pub fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A fully faked Keybindings runtime.
pub struct Rig {
    /// The runtime under test.
    pub runtime: KeybindingsRuntime,
    /// The bounded dispatcher.
    pub dispatcher: Dispatcher,
    /// The router (Carbon and Caps sink).
    pub router: Arc<Router>,
    /// The fake Carbon/TIS layer.
    pub carbon: Arc<FakeLog>,
    /// The inline "main thread".
    pub executor: Arc<PumpMain>,
    /// The fake `hidutil`, journal and lease.
    pub world: World,
    /// The shared remap over the world.
    pub remap: Arc<CapsRemap>,
    /// The fake tap backend.
    pub backend: Arc<FakeBackend>,
    /// The shared input service.
    pub input: Arc<InputService>,
    /// The scripted processes.
    pub processes: Arc<FakeProcesses>,
    /// The runtime diagnostics.
    pub log: DiagnosticLog,
}

impl Rig {
    /// A rig whose Caps world starts with no mappings.
    #[must_use]
    pub fn new() -> Self { Self::build(Vec::new(), fast_timing()) }

    /// A rig whose Caps world starts with `mappings`.
    #[must_use]
    pub fn with_mappings(mappings: Vec<Mapping>) -> Self { Self::build(mappings, fast_timing()) }

    /// A rig with explicit coordinator timing and seams.
    #[must_use]
    pub fn with_timing(timing: RuntimeTiming) -> Self { Self::build(Vec::new(), timing) }

    /// Delivers a press of the chord registered under `key_code`/`modifiers` on
    /// the fake main thread, as the Carbon callback would.
    ///
    /// # Panics
    /// When no such chord is registered.
    pub fn press_chord(&self, key_code: u16, modifiers: u32) {
        let id = self.carbon.id_of(key_code, modifiers).expect("the chord is registered");
        let carbon = Arc::clone(&self.carbon);
        self.executor.run(move || carbon.press(id));
    }

    fn build(mappings: Vec<Mapping>, timing: RuntimeTiming) -> Self {
        let log = DiagnosticLog::new();
        let processes = Arc::new(FakeProcesses::default());
        let executor = CommandExecutor::with_parts(
            Arc::clone(&processes) as Arc<dyn ProcessFactory>,
            Arc::new(FakeTime::default()),
            CommandEnvironment::default(),
        );
        let dispatcher = Dispatcher::with_config(
            executor,
            DispatcherConfig {
                spawn_drain: Duration::from_secs(5),
                shutdown_bound: Duration::from_secs(5),
            },
            log.sink(),
        );
        let clock = FakeInputClock::new();
        let backend = FakeBackend::new(&clock);
        // The service keeps the real clock so a held acknowledgement really
        // blocks until cancelled or bounded.
        let input = InputService::new(
            Arc::clone(&backend) as Arc<dyn InputBackend>,
            dispatcher.reservations(),
        );
        let router = Router::new(dispatcher.clone());
        let main = PumpMain::new();
        let carbon = FakeLog::new();
        let hotkeys = HotkeyControl::new(
            Arc::clone(&main) as Arc<dyn MainThreadExecutor>,
            carbon.factory(),
            Arc::clone(&input),
            input.reservations().new_owner(),
            Arc::clone(&router) as Arc<dyn HotkeySink>,
        );
        let world = World::new(mappings);
        let remap = Arc::new(world.remap());
        let caps = CapsOwner::new(Arc::clone(&input), Arc::clone(&remap), InputPriority {
            level: 100,
            tie_break: 0,
        });
        let runtime = KeybindingsRuntime::new(KeybindingsParts {
            dispatcher: dispatcher.clone(),
            router: Arc::clone(&router),
            hotkeys: Arc::new(hotkeys),
            caps: Arc::new(caps),
            remap: Arc::clone(&remap),
            input: Arc::clone(&input),
            log: log.clone(),
            priority: InputPriority { level: 100, tie_break: 0 },
            timing,
        });
        Self {
            runtime,
            dispatcher,
            router,
            carbon,
            executor: main,
            world,
            remap,
            backend,
            input,
            processes,
            log,
        }
    }
}

fn fast_timing() -> RuntimeTiming {
    RuntimeTiming {
        poll: Duration::from_millis(5),
        observed: None,
        adopting: None,
    }
}

impl Default for Rig {
    fn default() -> Self { Self::new() }
}
