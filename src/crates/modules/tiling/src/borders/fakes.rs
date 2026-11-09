//! Thread-safe fakes for the borders transport and helper CLI. Nothing here
//! touches bootstrap, Mach or a real process.

use std::collections::VecDeque;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use stache_macos::borders::{BordersTransport, MissingService, SendOutcome};
use stache_sdk::CancellationToken;
use stache_sdk::commands::{
    ChildProcess, Clock as CommandClock, CommandEnvironment, CommandExecutor, ProcessFactory,
    SpawnSpec, SystemClock,
};

use super::{ExecutorFactory, TransportFactory};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct NetState {
    present: bool,
    lookups: usize,
    sends: Vec<Vec<String>>,
    deadlines: Vec<Duration>,
    scripted: VecDeque<SendOutcome>,
    created: usize,
    dropped: usize,
    hold: bool,
    entered: usize,
}

/// The fake Mach world the runner's transport talks to.
pub struct Net {
    state: Mutex<NetState>,
    changed: Condvar,
}

impl Net {
    pub fn new(present: bool) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(NetState { present, ..NetState::default() }),
            changed: Condvar::new(),
        })
    }

    pub fn sends(&self) -> Vec<Vec<String>> { lock(&self.state).sends.clone() }

    pub fn deadlines(&self) -> Vec<Duration> { lock(&self.state).deadlines.clone() }

    pub fn lookups(&self) -> usize { lock(&self.state).lookups }

    pub fn created(&self) -> usize { lock(&self.state).created }

    pub fn dropped(&self) -> usize { lock(&self.state).dropped }

    /// Makes every send block after it is recorded, until [`Self::release`].
    pub fn hold(&self) { lock(&self.state).hold = true; }

    pub fn release(&self) {
        lock(&self.state).hold = false;
        self.changed.notify_all();
    }

    /// Waits until at least `count` sends were recorded.
    pub fn wait_sends(&self, count: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.state);
        while state.sends.len() < count {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                drop(state);
                return false;
            }
            state =
                self.changed.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0;
        }
        drop(state);
        true
    }

    /// Waits until `count` sends entered a held send.
    pub fn wait_entered(&self, count: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.state);
        while state.entered < count {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                drop(state);
                return false;
            }
            state =
                self.changed.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0;
        }
        drop(state);
        true
    }

    /// A factory that, like production, builds the transport on the runner
    /// thread.
    pub fn factory(self: &Arc<Self>) -> TransportFactory {
        let net = Arc::clone(self);
        Arc::new(move || {
            lock(&net.state).created += 1;
            Box::new(NetTransport {
                net: Arc::clone(&net),
                _confined: std::marker::PhantomData,
            }) as Box<dyn BordersTransport>
        })
    }
}

struct NetTransport {
    net: Arc<Net>,
    _confined: std::marker::PhantomData<*const ()>,
}

impl BordersTransport for NetTransport {
    fn lookup(&mut self) -> Result<(), MissingService> {
        let mut state = lock(&self.net.state);
        state.lookups += 1;
        if state.present {
            Ok(())
        } else {
            Err(MissingService { code: 1102 })
        }
    }

    fn send(&mut self, args: &[String], deadline: Duration) -> SendOutcome {
        let mut state = lock(&self.net.state);
        state.sends.push(args.to_vec());
        state.deadlines.push(deadline);
        state.entered += 1;
        self.net.changed.notify_all();
        while state.hold {
            state = self.net.changed.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
        state.scripted.pop_front().unwrap_or(SendOutcome::Sent)
    }
}

impl Drop for NetTransport {
    fn drop(&mut self) {
        lock(&self.net.state).dropped += 1;
        self.net.changed.notify_all();
    }
}

/// An instantly advancing virtual clock for the command executor.
#[derive(Default)]
pub struct Virtual {
    now: Mutex<Duration>,
}

impl CommandClock for Virtual {
    fn now(&self) -> Duration { *lock(&self.now) }

    fn wait(&self, cancel: Option<&CancellationToken>, quantum: Duration) -> bool {
        *lock(&self.now) += quantum;
        cancel.is_some_and(CancellationToken::is_cancelled)
    }
}

struct FakeChild {
    code: i32,
    hang: bool,
}

impl ChildProcess for FakeChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.hang {
            Ok(None)
        } else {
            Ok(Some(ExitStatus::from_raw(self.code << 8)))
        }
    }

    fn kill(&mut self) -> io::Result<()> { Ok(()) }

    fn read_stdout(&mut self, _: &mut [u8]) -> io::Result<usize> { Ok(0) }

    fn read_stderr(&mut self, _: &mut [u8]) -> io::Result<usize> { Ok(0) }
}

/// A fake helper-process world with a `borders` binary on a private path.
pub struct Cli {
    specs: Mutex<Vec<SpawnSpec>>,
    code: AtomicI32,
    hang: AtomicBool,
    dir: tempfile::TempDir,
}

impl Cli {
    pub fn new() -> Arc<Self> {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("borders");
        std::fs::write(&binary, b"#!/bin/sh\n").expect("write");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        Arc::new(Self {
            specs: Mutex::new(Vec::new()),
            code: AtomicI32::new(0),
            hang: AtomicBool::new(false),
            dir,
        })
    }

    pub fn set_exit(&self, code: i32) { self.code.store(code, Ordering::SeqCst); }

    pub fn set_hang(&self, hang: bool) { self.hang.store(hang, Ordering::SeqCst); }

    /// Every spawned helper's argv.
    pub fn spawned(&self) -> Vec<Vec<String>> {
        lock(&self.specs).iter().map(|spec| spec.arguments.clone()).collect()
    }

    /// An executor over this fake world; `virtual_time` makes deadlines instant.
    pub fn executor(self: &Arc<Self>, virtual_time: bool) -> CommandExecutor {
        let environment = CommandEnvironment {
            extra_paths: Some(self.dir.path().as_os_str().to_owned()),
            ..CommandEnvironment::default()
        };
        let clock: Arc<dyn CommandClock> = if virtual_time {
            Arc::new(Virtual::default())
        } else {
            Arc::new(SystemClock::default())
        };
        CommandExecutor::with_parts(Arc::clone(self) as Arc<dyn ProcessFactory>, clock, environment)
    }

    pub fn factory(self: &Arc<Self>) -> ExecutorFactory {
        let cli = Arc::clone(self);
        Arc::new(move || cli.executor(false))
    }
}

impl ProcessFactory for Cli {
    fn spawn(&self, spec: &SpawnSpec) -> io::Result<Box<dyn ChildProcess>> {
        lock(&self.specs).push(spec.clone());
        Ok(Box::new(FakeChild {
            code: self.code.load(Ordering::SeqCst),
            hang: self.hang.load(Ordering::SeqCst),
        }))
    }
}
