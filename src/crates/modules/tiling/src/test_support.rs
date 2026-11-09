//! Builders shared by the tracking tests: fake apps, windows, displays, events.

use std::sync::Mutex;

use stache_macos::applications::DisplayRect;
use stache_macos::ax::{AxPoint, AxReading, AxSize, FrameRead};
use stache_macos::windows::{
    AdapterId, RuntimeEpoch, SnapshotOrigin, TabInfo, WindowChange, WindowEvent, WindowEventKind,
    WindowInstance, WindowSnapshot,
};
use stache_macos::{ApplicationId, DisplayId, DisplayInfo, DisplayLayout};

use crate::actor::AppDirectory;
use crate::state::{AppInfo, CompletionResult, Effect, Input, Stamp, TilingState, transition};

pub fn app(n: u64) -> ApplicationId { ApplicationId::for_testing(n) }

pub fn info(n: u64, bundle: &str, name: &str) -> AppInfo {
    AppInfo {
        application: app(n),
        pid: i32::try_from(n).unwrap_or(1) + 100,
        bundle_id: bundle.to_owned(),
        name: name.to_owned(),
    }
}

pub fn snapshot(token: WindowInstance, title: &str, cg: u32) -> WindowSnapshot {
    WindowSnapshot {
        window: token,
        cg_window_id: Some(cg),
        title: AxReading::Value(title.to_owned()),
        role: AxReading::Value("AXWindow".to_owned()),
        subrole: AxReading::Value("AXStandardWindow".to_owned()),
        frame: FrameRead {
            position: AxReading::Value(AxPoint { x: 10.0, y: 10.0 }),
            size: AxReading::Value(AxSize { width: 800.0, height: 600.0 }),
        },
        minimized: AxReading::Value(false),
        fullscreen: AxReading::Value(false),
        resizable: AxReading::Value(true),
        tabs: TabInfo::None,
        tab_members: Vec::new(),
        observed: true,
    }
}

pub fn display(id: u32, primary: bool, hz: Option<f64>, x: f64) -> DisplayInfo {
    DisplayInfo {
        id: DisplayId::new(id),
        frame: DisplayRect::new(x, 0.0, 1920.0, 1080.0),
        visible_frame: DisplayRect::new(x, 0.0, 1920.0, 1055.0),
        scale: 2.0,
        refresh_hz: hz,
        name: Some(format!("Display {id}")),
        is_primary: primary,
        is_main: primary,
        is_builtin: primary,
    }
}

pub fn one_display() -> DisplayLayout {
    DisplayLayout::new(vec![display(1, true, Some(60.0), 0.0)])
}

/// A fake epoch/registry plus a monotonic event sequence.
pub struct Feed {
    pub adapter: AdapterId,
    pub epoch: RuntimeEpoch,
    pub stamp: Stamp,
    sequence: u64,
}

impl Feed {
    pub fn new(epoch: u64) -> Self {
        let epoch = RuntimeEpoch::for_testing(epoch);
        Self {
            adapter: AdapterId::for_testing(),
            epoch,
            stamp: Stamp { epoch, registry: epoch.value() },
            sequence: 0,
        }
    }

    pub fn token(&self, app_n: u64, serial: u64) -> WindowInstance {
        WindowInstance::for_testing(self.adapter, self.epoch, app(app_n), serial)
    }

    pub fn event(&mut self, kind: WindowEventKind) -> WindowEvent {
        self.sequence += 1;
        WindowEvent {
            epoch: self.epoch,
            sequence: self.sequence,
            kind,
        }
    }

    pub fn input(&mut self, kind: WindowEventKind) -> Input { Input::Window(self.event(kind)) }

    pub fn created(&mut self, token: WindowInstance, title: &str, cg: u32) -> Input {
        self.input(WindowEventKind::Snapshot {
            snapshot: snapshot(token, title, cg),
            origin: SnapshotOrigin::Created,
        })
    }

    pub fn enumerated(&mut self, token: WindowInstance, title: &str, cg: u32) -> Input {
        self.input(WindowEventKind::Snapshot {
            snapshot: snapshot(token, title, cg),
            origin: SnapshotOrigin::Enumeration,
        })
    }

    pub fn change(&mut self, token: WindowInstance, change: WindowChange) -> Input {
        self.input(WindowEventKind::Window { window: token, change })
    }

    pub fn complete(&mut self, application: ApplicationId) -> Input {
        self.input(WindowEventKind::EnumerationComplete {
            application,
            windows: 0,
            truncated: false,
        })
    }
}

/// Applies `inputs` in order and returns every effect.
pub fn run(state: TilingState, inputs: Vec<Input>) -> (TilingState, Vec<Effect>) {
    let mut state = state;
    let mut all = Vec::new();
    for input in inputs {
        let (next, effects) = transition(state, input);
        state = next;
        all.extend(effects);
    }
    (state, all)
}

pub fn step(state: TilingState, input: Input) -> (TilingState, Vec<Effect>) {
    transition(state, input)
}

/// A fully started and enumerated state with one display and `apps`.
pub fn ready(self_bundle: &str, apps: &[AppInfo], feed: &mut Feed) -> TilingState {
    let state = TilingState::new(self_bundle);
    let (mut state, _) = run(state, vec![
        Input::Displays(std::sync::Arc::new(one_display())),
        Input::Start(feed.stamp),
        Input::Applications {
            stamp: feed.stamp,
            apps: apps.to_vec(),
        },
    ]);
    let tracks: Vec<ApplicationId> = state.startup.awaiting.iter().copied().collect();
    for application in tracks {
        let (next, _) = step(state, Input::Completion {
            stamp: feed.stamp,
            result: CompletionResult::Track(
                application,
                stache_macos::windows::TrackOutcome::Tracking,
            ),
        });
        let (next, _) = step(next, feed.complete(application));
        state = next;
    }
    state
}

/// A directory that answers from a list.
#[derive(Default)]
pub struct FakeAppDirectory {
    pub apps: Mutex<Vec<AppInfo>>,
}

impl AppDirectory for FakeAppDirectory {
    fn running(&self) -> Vec<AppInfo> { self.apps.lock().expect("apps").clone() }

    fn describe(&self, application: ApplicationId) -> Option<AppInfo> {
        self.apps
            .lock()
            .expect("apps")
            .iter()
            .find(|a| a.application == application)
            .cloned()
    }
}
