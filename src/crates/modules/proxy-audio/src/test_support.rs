//! Test doubles shared by the proxyAudio suites.
//!
//! The fake backend records every listener generation and default-device set,
//! and exposes failpoints for registration and mutation. Nothing here calls
//! `CoreAudio`, so no test can change the system audio state.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use stache_macos::audio::{
    AudioBackend, AudioChangeSubscription, AudioDevice, AudioDirection, AudioError, AudioWakeup,
};
use stache_sdk::{
    EventBus, LogRecord, Logger, MainThreadExecutor, Permission, PermissionError,
    PermissionProvider, PermissionStatus, Registry, ThreadError,
};

use crate::config::{AudioDevicePriority, MatchStrategy, ProxyAudioConfig};

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A `CoreAudio` device used by the fake backend.
#[must_use]
pub fn device(id: u32, name: &str, input: bool, output: bool) -> AudioDevice {
    AudioDevice {
        id,
        name: name.to_owned(),
        input,
        output,
    }
}

#[must_use]
pub fn output_device(id: u32, name: &str) -> AudioDevice { device(id, name, false, true) }

#[must_use]
pub fn input_device(id: u32, name: &str) -> AudioDevice { device(id, name, true, false) }

#[derive(Default)]
pub struct FakeState {
    pub devices: Mutex<Vec<AudioDevice>>,
    pub defaults: Mutex<BTreeMap<AudioDirection, u32>>,
    pub sets: Mutex<Vec<(AudioDirection, u32)>>,
    pub hdmi: Mutex<BTreeSet<u32>>,
    pub subscribe_calls: AtomicUsize,
    pub generations: AtomicUsize,
    pub live: AtomicUsize,
    pub drops: AtomicUsize,
    pub fail_subscribe: AtomicBool,
    pub fail_set: AtomicBool,
    pub wake_on_drop: AtomicBool,
    pub last_wakeup: Mutex<Option<AudioWakeup>>,
}

/// An in-memory [`AudioBackend`].
#[derive(Clone, Default)]
pub struct FakeBackend {
    pub state: Arc<FakeState>,
}

impl FakeBackend {
    #[must_use]
    pub fn new(devices: Vec<AudioDevice>) -> Self {
        let backend = Self::default();
        *lock(&backend.state.devices) = devices;
        backend
    }

    pub fn seed_default(&self, direction: AudioDirection, id: u32) {
        lock(&self.state.defaults).insert(direction, id);
    }

    pub fn mark_hdmi(&self, id: u32) { lock(&self.state.hdmi).insert(id); }

    #[must_use]
    pub fn sets(&self) -> Vec<(AudioDirection, u32)> { lock(&self.state.sets).clone() }

    #[must_use]
    pub fn default_id(&self, direction: AudioDirection) -> Option<u32> {
        lock(&self.state.defaults).get(&direction).copied()
    }

    #[must_use]
    pub fn live(&self) -> usize { self.state.live.load(Ordering::SeqCst) }

    #[must_use]
    pub fn generations(&self) -> usize { self.state.generations.load(Ordering::SeqCst) }

    #[must_use]
    pub fn drops(&self) -> usize { self.state.drops.load(Ordering::SeqCst) }

    #[must_use]
    pub fn subscribe_calls(&self) -> usize { self.state.subscribe_calls.load(Ordering::SeqCst) }

    #[must_use]
    pub fn wakeup(&self) -> AudioWakeup {
        lock(&self.state.last_wakeup).clone().expect("a subscription was registered")
    }

    pub fn fail_subscribe(&self) { self.state.fail_subscribe.store(true, Ordering::SeqCst); }

    pub fn fail_set(&self) { self.state.fail_set.store(true, Ordering::SeqCst); }

    /// Signals the wakeup when the subscription is dropped, modelling a
    /// `CoreAudio` callback that is already in flight during stop.
    pub fn wake_on_drop(&self) { self.state.wake_on_drop.store(true, Ordering::SeqCst); }
}

impl AudioBackend for FakeBackend {
    fn devices(&self) -> Vec<AudioDevice> { lock(&self.state.devices).clone() }

    fn default_device(&self, direction: AudioDirection) -> Option<AudioDevice> {
        let id = self.default_id(direction)?;
        lock(&self.state.devices).iter().find(|device| device.id == id).cloned()
    }

    fn set_default_device(&self, direction: AudioDirection, id: u32) -> Result<(), AudioError> {
        if self.state.fail_set.load(Ordering::SeqCst) {
            return Err(AudioError::new("set default audio device", -1));
        }
        lock(&self.state.sets).push((direction, id));
        lock(&self.state.defaults).insert(direction, id);
        Ok(())
    }

    fn is_hdmi(&self, id: u32) -> bool { lock(&self.state.hdmi).contains(&id) }

    fn subscribe(
        &self,
        wakeup: AudioWakeup,
    ) -> Result<Box<dyn AudioChangeSubscription>, AudioError> {
        self.state.subscribe_calls.fetch_add(1, Ordering::SeqCst);
        if self.state.fail_subscribe.load(Ordering::SeqCst) {
            return Err(AudioError::new("add audio property listener", -1));
        }
        self.state.generations.fetch_add(1, Ordering::SeqCst);
        self.state.live.fetch_add(1, Ordering::SeqCst);
        *lock(&self.state.last_wakeup) = Some(wakeup.clone());
        Ok(Box::new(FakeSubscription {
            state: Arc::clone(&self.state),
            wakeup,
        }))
    }
}

struct FakeSubscription {
    state: Arc<FakeState>,
    wakeup: AudioWakeup,
}

impl AudioChangeSubscription for FakeSubscription {}

impl Drop for FakeSubscription {
    fn drop(&mut self) {
        self.state.live.fetch_sub(1, Ordering::SeqCst);
        self.state.drops.fetch_add(1, Ordering::SeqCst);
        if self.state.wake_on_drop.load(Ordering::SeqCst) {
            self.wakeup.signal();
        }
    }
}

/// A priority entry that matches a name with the legacy `contains` strategy.
#[must_use]
pub fn contains(name: &str) -> AudioDevicePriority {
    AudioDevicePriority {
        name: name.to_owned(),
        strategy: MatchStrategy::Contains,
        depends_on: None,
    }
}

/// An enabled config with one priority list per direction.
#[must_use]
pub fn config(output: &[&str], input: &[&str]) -> ProxyAudioConfig {
    ProxyAudioConfig {
        enabled: true,
        input: input.iter().map(|name| contains(name)).collect(),
        output: output.iter().map(|name| contains(name)).collect(),
    }
}

/// An enabled config whose output entry is an invalid regex.
#[must_use]
pub fn invalid_regex_config() -> ProxyAudioConfig {
    ProxyAudioConfig {
        enabled: true,
        input: Vec::new(),
        output: vec![AudioDevicePriority {
            name: "(broken".to_owned(),
            strategy: MatchStrategy::Regex,
            depends_on: None,
        }],
    }
}

pub struct AllowAll;

impl PermissionProvider for AllowAll {
    fn status(&self, _: Permission) -> PermissionStatus { PermissionStatus::Granted }

    fn request(&self, _: Permission) -> Result<PermissionStatus, PermissionError> {
        Ok(PermissionStatus::Granted)
    }
}

pub struct NoMainThread;

impl MainThreadExecutor for NoMainThread {
    fn is_main_thread(&self) -> bool { false }

    fn execute(&self, _: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> { Ok(()) }
}

#[derive(Default)]
pub struct CaptureLogger {
    pub records: Mutex<Vec<LogRecord>>,
}

impl Logger for CaptureLogger {
    fn log(&self, record: LogRecord) { lock(&self.records).push(record); }
}

impl CaptureLogger {
    #[must_use]
    pub fn messages(&self) -> Vec<String> {
        lock(&self.records).iter().map(|record| record.message.clone()).collect()
    }
}

/// A registry with permissive host services.
#[must_use]
pub fn registry(logger: Arc<dyn Logger>) -> Registry {
    Registry::new(
        logger,
        Arc::new(EventBus::new()),
        Arc::new(NoMainThread),
        Arc::new(AllowAll),
        Arc::new(stache_sdk::windows::UnavailableWindows),
    )
}
