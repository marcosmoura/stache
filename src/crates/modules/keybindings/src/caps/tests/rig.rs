//! Shared owner rig: a fake input service, a fake world and a counting sink.

use std::sync::{Arc, Mutex, PoisonError};

use stache_macos::caps_remap::testing::World;
use stache_macos::caps_remap::{CapsRemap, Hidutil, HidutilError, Mapping};
use stache_sdk::CancellationToken;
use stache_sdk::input::testing::{CapacityWork, FakeBackend, FakeClock};
use stache_sdk::input::{
    Disposition, InputBackend, InputClock, InputEventKind, InputPriority, InputService, KeyCode,
    ModifierFlags, PressStream, RawInput, TapKind, WorkReservations,
};

use crate::caps::handler::CapsSink;
use crate::caps::owner::CapsOwner;
use crate::caps::parser::CapsKey;

pub const S: u16 = 1;
pub const F18: u16 = 0x4F;
pub const KEY_A: Mapping = Mapping { src: 100, dst: 200 };

#[derive(Default)]
pub struct Sink {
    keys: Mutex<Vec<u16>>,
}

impl Sink {
    pub fn taken(&self) -> Vec<u16> {
        self.keys.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl CapsSink for Sink {
    fn execute(&self, key: CapsKey) {
        self.keys.lock().unwrap_or_else(PoisonError::into_inner).push(key.0);
    }
}

type Hook = Box<dyn Fn(&'static str) + Send + Sync>;

/// A `hidutil` that reports every call to a hook before delegating.
pub struct Watched {
    inner: Arc<dyn Hidutil>,
    hook: Hook,
}

impl Hidutil for Watched {
    fn get(&self, cancel: &CancellationToken) -> Result<Vec<u8>, HidutilError> {
        (self.hook)("get");
        self.inner.get(cancel)
    }

    fn set(&self, payload: &str, cancel: &CancellationToken) -> Result<(), HidutilError> {
        (self.hook)("set");
        self.inner.set(payload, cancel)
    }

    fn unreaped(&self) -> usize { self.inner.unreaped() }
}

pub struct Rig {
    pub service: Arc<InputService>,
    pub backend: Arc<FakeBackend>,
    pub clock: Arc<FakeClock>,
    pub world: World,
    pub sink: Arc<Sink>,
}

impl Rig {
    pub fn new(initial: Vec<Mapping>) -> Self {
        let clock = FakeClock::new();
        let backend = FakeBackend::new(&clock);
        let service = InputService::with_clock(
            Arc::clone(&backend) as Arc<dyn InputBackend>,
            CapacityWork::new(8) as Arc<dyn WorkReservations>,
            Arc::clone(&clock) as Arc<dyn InputClock>,
        );
        Self {
            service,
            backend,
            clock,
            world: World::new(initial),
            sink: Arc::new(Sink::default()),
        }
    }

    pub fn owner(&self) -> CapsOwner {
        CapsOwner::new(
            Arc::clone(&self.service),
            Arc::new(self.world.remap()),
            InputPriority { level: 100, tie_break: 0 },
        )
    }

    pub fn owner_watching(&self, hook: impl Fn(&'static str) + Send + Sync + 'static) -> CapsOwner {
        let remap = CapsRemap::new(
            Arc::new(Watched {
                inner: self.world.hidutil(),
                hook: Box::new(hook),
            }),
            self.world.storage(),
            self.world.leases(),
        );
        CapsOwner::new(
            Arc::clone(&self.service),
            Arc::new(remap),
            InputPriority::default(),
        )
    }

    pub fn sink(&self) -> Arc<dyn CapsSink> { Arc::clone(&self.sink) as _ }

    pub fn press(&self, kind: InputEventKind, key: u16, repeat: bool) -> Disposition {
        self.service.dispatch(TapKind::Suppressing, RawInput {
            kind,
            stream: PressStream::Key(KeyCode(key)),
            flags: ModifierFlags::NONE,
            repeat,
            timestamp: std::time::Duration::ZERO,
        })
    }

    pub fn down(&self, key: u16) -> Disposition { self.press(InputEventKind::KeyDown, key, false) }

    pub fn up(&self, key: u16) -> Disposition { self.press(InputEventKind::KeyUp, key, false) }
}

pub fn token() -> CancellationToken { CancellationToken::new() }
