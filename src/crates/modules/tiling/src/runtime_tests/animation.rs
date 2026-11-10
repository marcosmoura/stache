//! End-to-end animation scenarios: the real actor and tracking runtime over the
//! real sharded `WindowAdapter` (one `Core` per application) on a fake AX arena.
//! Applications and windows are modelled like a desktop: several applications,
//! several windows each, real shard threads, real frame pacing. A spy backend
//! between the runtime and the adapter records every batch and snapshot read.
//! Nothing touches a real window, display or process.

use std::collections::BTreeMap;
use std::sync::{Condvar, Mutex, PoisonError};

use serde_json::json;
use stache_macos::ax::testing::{Call, FakeNode, FakeWindowAx, NodeId};
use stache_macos::ax::{AxError, AxPoint, AxSize, BoolAttribute, WindowString};
use stache_macos::windows::testing::{FakeObservers, FakeWindowSystem};
use stache_macos::windows::{
    AdapterOptions, FrameReplyFn, FrameRequest, FrameStage, HiddenOutcome, HiddenRequest,
    LeaseRestoreOutcome, RunLoopDriver, SystemWindowClock, WindowAdapter,
};
use stache_macos::{DisplayId, DisplayLayout, FakeFrameClockFactory};

use super::*;
use crate::actor::AppDirectory;
use crate::backend::SystemWindowBackend;
use crate::config::TilingConfig;
use crate::drag::{DragPoint, DragSignal};
use crate::geometry::Rect;
use crate::state::AppInfo;
use crate::test_support::{FakeAppDirectory, display};

static NAMES: AtomicUsize = AtomicUsize::new(0);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// --------------------------------------------------------------------- spy

/// One batch handed to the adapter.
#[derive(Clone, Debug)]
struct Batch {
    application: ApplicationId,
    frames: Vec<(WindowInstance, FrameStage)>,
}

/// The production backend with a recorder in front of the frame and snapshot
/// submissions.
struct Spy {
    inner: SystemWindowBackend,
    batches: Mutex<Vec<Batch>>,
    snapshots: Mutex<Vec<WindowInstance>>,
}

impl Spy {
    fn batches(&self) -> Vec<Batch> { lock(&self.batches).clone() }

    fn frames_of(&self, from: usize) -> Vec<(ApplicationId, WindowInstance, FrameStage)> {
        self.batches()[from..]
            .iter()
            .flat_map(|b| b.frames.iter().map(|(w, s)| (b.application, *w, *s)))
            .collect()
    }

    fn snapshot_reads(&self, window: WindowInstance) -> usize {
        lock(&self.snapshots).iter().filter(|w| **w == window).count()
    }
}

impl WindowBackend for Spy {
    fn adapter(&self) -> AdapterId { self.inner.adapter() }

    fn reads_tabs(&self) -> bool { self.inner.reads_tabs() }

    fn is_overflow(&self, application: ApplicationId) -> bool {
        self.inner.is_overflow(application)
    }

    fn install(
        &self,
        sink: Arc<EventQueue>,
        gate: Arc<GenerationCell>,
        deadline: Option<Instant>,
    ) -> Result<RuntimeEpoch, InstallError> {
        self.inner.install(sink, gate, deadline)
    }

    fn track(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        pid: i32,
        cancel: CancellationToken,
        deadline: Option<Instant>,
    ) -> TrackOutcome {
        self.inner.track(epoch, application, pid, cancel, deadline)
    }

    fn refresh(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        cancel: CancellationToken,
        deadline: Option<Instant>,
    ) -> TrackOutcome {
        self.inner.refresh(epoch, application, cancel, deadline)
    }

    fn focused_window(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> stache_macos::windows::FocusOutcome {
        self.inner.focused_window(epoch, application, cancel, deadline)
    }

    fn untrack(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> StopOutcome {
        self.inner.untrack(epoch, application, cancel, deadline)
    }

    fn snapshot(
        &self,
        window: WindowInstance,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> SnapshotOutcome {
        self.inner.snapshot(window, cancel, deadline)
    }

    fn resolve_window_id(
        &self,
        id: u32,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> Option<WindowInstance> {
        self.inner.resolve_window_id(id, cancel, deadline)
    }

    fn submit_action(
        &self,
        request: ActionRequest,
        reply: Box<dyn FnOnce(WindowActionOutcome) + Send>,
    ) -> Result<(), DispatchError> {
        self.inner.submit_action(request, reply)
    }

    fn act(&self, request: ActionRequest, cancel: &CancellationToken) -> WindowActionOutcome {
        self.inner.act(request, cancel)
    }

    fn submit_frames(
        &self,
        application: ApplicationId,
        batch: Vec<(FrameRequest, FrameReplyFn)>,
    ) -> Result<(), DispatchError> {
        lock(&self.batches).push(Batch {
            application,
            frames: batch.iter().map(|(r, _)| (r.issuance.window(), r.stage)).collect(),
        });
        self.inner.submit_frames(application, batch)
    }

    fn submit_snapshot(
        &self,
        window: WindowInstance,
        reply: Box<dyn FnOnce(SnapshotOutcome) + Send>,
    ) -> Result<(), DispatchError> {
        lock(&self.snapshots).push(window);
        self.inner.submit_snapshot(window, reply)
    }

    fn application_hidden(
        &self,
        request: HiddenRequest,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> HiddenOutcome {
        self.inner.application_hidden(request, cancel, deadline)
    }

    fn stop(
        &self,
        epoch: RuntimeEpoch,
        gate: &GenerationCell,
        deadline: Option<Instant>,
    ) -> StopOutcome {
        self.inner.stop(epoch, gate, deadline)
    }

    fn restore_enhanced_ui(&self, deadline: Option<Instant>) -> LeaseRestoreOutcome {
        self.inner.restore_enhanced_ui(deadline)
    }

    fn retry_cleanup(
        &self,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> Option<CleanupReport> {
        self.inner.retry_cleanup(cancel, deadline)
    }

    fn close(&self) -> CloseOutcome { self.inner.close() }
}

// ------------------------------------------------------------------- world

/// Stalls the writes of chosen nodes until released, like a shard stuck in a
/// native call.
#[derive(Default)]
struct Gate {
    state: Mutex<(usize, bool)>,
    changed: Condvar,
}

impl Gate {
    #[allow(clippy::significant_drop_tightening)] // held across the wait on purpose
    fn pass(&self) {
        let mut state = lock(&self.state);
        state.0 += 1;
        self.changed.notify_all();
        let released = self
            .changed
            .wait_while(state, |state| !state.1)
            .unwrap_or_else(PoisonError::into_inner);
        drop(released);
    }

    fn entered(&self) -> usize { lock(&self.state).0 }

    fn release(&self) {
        lock(&self.state).1 = true;
        self.changed.notify_all();
    }
}

struct Application {
    info: AppInfo,
    /// The application element (where `AXEnhancedUserInterface` lives).
    element: NodeId,
    nodes: Vec<NodeId>,
}

struct World {
    ax: FakeWindowAx,
    observers: FakeObservers,
    spy: Arc<Spy>,
    runtime: TrackingRuntime,
    applications: Vec<Application>,
    revision: u64,
    /// Windows on a visible workspace: the ones that get frames.
    visible: usize,
    /// The fake frame clocks the runtime paces its animations with.
    clocks: FakeFrameClockFactory,
}

fn window_node(ax: &FakeWindowAx, cg: u32) -> NodeId {
    let mut node = FakeNode {
        position: Ok(AxPoint { x: 10.0, y: 20.0 }),
        size: Ok(AxSize { width: 300.0, height: 200.0 }),
        position_settable: Ok(true),
        size_settable: Ok(true),
        window_id: Ok(cg),
        ..FakeNode::default()
    };
    node.strings.insert(WindowString::Title, Ok(format!("window {cg}")));
    node.strings.insert(WindowString::Role, Ok("AXWindow".to_owned()));
    node.strings.insert(WindowString::Subrole, Ok("AXStandardWindow".to_owned()));
    node.bools.insert(BoolAttribute::Minimized, Ok(false));
    node.bools.insert(BoolAttribute::FullScreen, Ok(false));
    ax.add_node(node)
}

fn config(outer: u32, workspaces: &serde_json::Value) -> Arc<TilingConfig> {
    Arc::new(
        serde_json::from_value(json!({
            "enabled": true,
            "gaps": { "inner": 0, "outer": outer },
            "animations": { "enabled": true, "duration": 200 },
            "workspaces": workspaces,
        }))
        .expect("config"),
    )
}

fn one_workspace(outer: u32) -> Arc<TilingConfig> { config(outer, &json!([])) }

impl World {
    /// Applications with `counts[i]` windows each, tracked, enumerated and laid
    /// out by their first (direct) plan, under `config`.
    fn boot(counts: &[usize], config: Arc<TilingConfig>) -> Self {
        Self::boot_visible(counts, config, counts.iter().sum())
    }

    /// [`Self::boot`] where only `visible` windows are on a visible workspace.
    /// Every frame clock creation fails, so the timer paces the animations.
    fn boot_visible(counts: &[usize], config: Arc<TilingConfig>, visible: usize) -> Self {
        Self::boot_with(
            counts,
            config,
            visible,
            FakeFrameClockFactory::failing("timer"),
            one_display(),
            &[],
        )
    }

    /// [`Self::boot`] where application `i` starts with `AXEnhancedUserInterface`
    /// set to `eui[i]` (the others lack the attribute).
    fn boot_leased(
        counts: &[usize],
        config: Arc<TilingConfig>,
        eui: &[Result<bool, AxError>],
    ) -> Self {
        Self::boot_with(
            counts,
            config,
            counts.iter().sum(),
            FakeFrameClockFactory::failing("timer"),
            one_display(),
            eui,
        )
    }

    /// [`Self::boot`] with fake frame clocks and a display layout. The test
    /// fires the ticks.
    fn boot_clocked(
        counts: &[usize],
        config: Arc<TilingConfig>,
        clocks: FakeFrameClockFactory,
        layout: DisplayLayout,
    ) -> Self {
        Self::boot_with(counts, config, counts.iter().sum(), clocks, layout, &[])
    }

    fn boot_with(
        counts: &[usize],
        config: Arc<TilingConfig>,
        visible: usize,
        clocks: FakeFrameClockFactory,
        layout: DisplayLayout,
        eui: &[Result<bool, AxError>],
    ) -> Self {
        let ax = FakeWindowAx::new();
        let mut applications = Vec::new();
        let mut cg = 100;
        for (index, count) in counts.iter().enumerate() {
            let nodes: Vec<NodeId> = (0..*count)
                .map(|_| {
                    cg += 1;
                    window_node(&ax, cg)
                })
                .collect();
            let element = ax.add_node(FakeNode {
                windows: Ok(nodes.clone()),
                enhanced_ui: eui.get(index).copied().unwrap_or(Err(AxError::AttributeUnsupported)),
                ..FakeNode::default()
            });
            let n = index as u64 + 1;
            let info = info(n, &format!("com.test.app{n}"), &format!("App {n}"));
            ax.set_application(info.pid, element);
            applications.push(Application { info, element, nodes });
        }
        let (system, observers): (_, FakeObservers) = FakeWindowSystem::factory(ax.clone());
        let adapter = WindowAdapter::spawn(
            &format!(
                "stache-test-tiling-anim-{}",
                NAMES.fetch_add(1, Ordering::SeqCst)
            ),
            &AdapterOptions::default(),
            Arc::new(SystemWindowClock),
            system,
            RunLoopDriver::new,
        )
        .expect("spawn");
        let spy = Arc::new(Spy {
            inner: SystemWindowBackend::from_adapter(adapter, false),
            batches: Mutex::new(Vec::new()),
            snapshots: Mutex::new(Vec::new()),
        });
        let directory = Arc::new(FakeAppDirectory::default());
        *directory.apps.lock().expect("apps") =
            applications.iter().map(|a| a.info.clone()).collect();
        let mut options = TrackingOptions::new("");
        options.frame_clocks = Some(Arc::new(clocks.clone()));
        let runtime = TrackingRuntime::start(
            Arc::clone(&spy) as Arc<dyn WindowBackend>,
            directory as Arc<dyn AppDirectory>,
            &options,
        )
        .expect("start");
        runtime.submit_displays(None, layout).expect("displays");
        runtime.submit_config(1, config).expect("config");
        let total: usize = counts.iter().sum();
        wait_until("enumeration", || {
            let snap = runtime.snapshot();
            snap.phase == Phase::Complete && snap.windows.len() == total
        });
        assert!(runtime.authorize(1));
        let world = Self {
            ax,
            observers,
            spy,
            runtime,
            applications,
            revision: 1,
            visible,
            clocks,
        };
        world.wait_settled("the first layout");
        world
    }

    /// Accepts a new configuration revision.
    fn reload(&mut self, config: Arc<TilingConfig>) {
        self.revision += 1;
        self.runtime.submit_config(self.revision, config).expect("config");
    }

    /// Every window has a settled attempt that reached its target and nothing
    /// is outstanding.
    fn is_settled(&self) -> bool {
        let snap = self.runtime.snapshot();
        // A window on a hidden workspace keeps its record but has no target.
        let live: Vec<_> = snap.frames.iter().filter(|f| f.desired.is_some()).collect();
        live.len() == self.visible
            && live
                .iter()
                .all(|f| !f.in_flight && f.outcome.is_some() && f.actual == f.desired)
            && self.runtime.outstanding_frames() == 0
    }

    fn wait_settled(&self, what: &str) { wait_until(what, || self.is_settled()); }

    fn frame_of(&self, node: NodeId) -> Rect {
        let mut frame = None;
        self.ax.edit(node, |n| {
            let (Ok(p), Ok(s)) = (n.position, n.size) else { return };
            frame = Some(Rect::new(p.x, p.y, s.width, s.height));
        });
        frame.expect("a readable node")
    }

    fn token(&self, node: NodeId) -> WindowInstance {
        let cg = self.cg_of(node);
        self.runtime
            .snapshot()
            .windows
            .iter()
            .find(|w| w.cg_window_id == Some(cg))
            .map(|w| w.token)
            .expect("tracked")
    }

    fn cg_of(&self, node: NodeId) -> u32 {
        let mut cg = 0;
        self.ax.edit(node, |n| cg = n.window_id.unwrap_or(0));
        cg
    }

    fn desired(&self) -> BTreeMap<WindowInstance, Rect> {
        self.runtime
            .snapshot()
            .frames
            .iter()
            .filter_map(|f| f.desired.map(|d| (f.window, d)))
            .collect()
    }

    /// Makes every native position write notify the observer like a real
    /// application does (`AXWindowMoved`), so the planner's observed frame
    /// follows a window in motion.
    fn echo_moves(&self) {
        let owners: Vec<(NodeId, i32)> = self
            .applications
            .iter()
            .flat_map(|a| a.nodes.iter().map(|n| (*n, a.info.pid)))
            .collect();
        let observers = self.observers.clone();
        self.ax.on_node_write(move |node| {
            if let Some((_, pid)) = owners.iter().find(|(n, _)| *n == node) {
                observers.inject(*pid, stache_macos::ax::AxNotification::WindowMoved, node);
            }
        });
    }

    /// The nodes of one application.
    fn nodes(&self, application: usize) -> &[NodeId] { &self.applications[application].nodes }

    /// How many native position writes the fake AX saw for `nodes`.
    fn position_writes(&self, nodes: &[NodeId]) -> usize {
        self.ax
            .calls()
            .iter()
            .filter(|c| matches!(c, Call::SetPosition(node, _) if nodes.contains(node)))
            .count()
    }

    fn all_at_target(&self, nodes: &[NodeId]) -> bool {
        let desired = self.desired();
        nodes
            .iter()
            .all(|node| desired.get(&self.token(*node)) == Some(&self.frame_of(*node)))
    }
}

impl Drop for World {
    fn drop(&mut self) { self.runtime.shutdown(); }
}

// --------------------------------------------------------------- scenarios

/// (a) Several applications with several windows each go through a layout
/// change: every window reaches its exact target, with one batch per
/// application per tick, write-only intermediate frames and one settle read.
#[test]
fn several_applications_animate_a_layout_change_in_per_application_batches() {
    let mut world = World::boot(&[3, 2, 2], one_workspace(0));
    assert!(world.runtime.animation().is_running());
    let before = world.desired();
    let base = world.spy.batches().len();
    assert!(
        world.spy.frames_of(0).iter().all(|(_, _, stage)| *stage == FrameStage::Direct),
        "the first layout is written directly"
    );
    let reads_before: usize = before.keys().map(|w| world.spy.snapshot_reads(*w)).sum();

    world.reload(one_workspace(200));
    wait_until("the animation", || {
        let now = world.desired();
        now.len() == before.len()
            && now.iter().all(|(w, d)| before.get(w) != Some(d))
            && world.is_settled()
    });

    // Every window is at its exact target and the attempt is reported applied.
    let snap = world.runtime.snapshot();
    for status in &snap.frames {
        assert!(
            matches!(status.outcome, Some(crate::OperationOutcome::Applied { .. })),
            "{status:?}"
        );
    }
    for node in world.applications.iter().flat_map(|a| a.nodes.iter()) {
        assert_eq!(
            Some(world.frame_of(*node)),
            world.desired().get(&world.token(*node)).copied(),
            "the window reached its exact target"
        );
    }

    let frames = world.spy.frames_of(base);
    let batches = &world.spy.batches()[base..];
    assert!(frames.iter().all(|(_, _, stage)| *stage != FrameStage::Direct));
    // One batch holds one application's frames only.
    for batch in batches {
        assert!(batch.frames.iter().all(|(w, _)| w.application() == batch.application));
    }
    // Windows of one application share batches, so there are fewer batches than
    // frames and at least one batch carries a whole application.
    for application in &world.applications {
        let n = application.nodes.len();
        let mine: Vec<&Batch> = batches
            .iter()
            .filter(|b| b.application == application.info.application)
            .collect();
        assert!(
            mine.iter().any(|b| b.frames.len() == n),
            "app {:?}: one batch carries all {n} windows: {:?}",
            application.info.application,
            mine.iter().map(|b| b.frames.len()).collect::<Vec<_>>()
        );
        assert!(mine.len() < mine.iter().map(|b| b.frames.len()).sum::<usize>());
    }
    // Stages: first, intermediate, final; membership only at the ends.
    for node in world.applications.iter().flat_map(|a| a.nodes.iter()) {
        let token = world.token(*node);
        let stages: Vec<FrameStage> =
            frames.iter().filter(|(_, w, _)| *w == token).map(|(_, _, s)| *s).collect();
        assert_eq!(stages.first(), Some(&FrameStage::First), "{stages:?}");
        assert_eq!(stages.last(), Some(&FrameStage::Final), "{stages:?}");
        assert_eq!(stages.iter().filter(|s| **s == FrameStage::Final).count(), 1);
        assert!(
            stages.len() >= 4,
            "intermediate frames, not one jump: {stages:?}"
        );
        assert_eq!(
            world.spy.snapshot_reads(token),
            1 + reads_before / before.len().max(1),
            "intermediate frames read nothing back; the final settles with one read"
        );
    }
}

/// (b) An animation interrupted by a newer plan and sent back (A, B, A) ends at
/// the right frame, every window brought back exactly.
#[test]
fn an_animation_interrupted_and_sent_back_ends_at_the_original_frame() {
    let mut world = World::boot(&[2, 2], one_workspace(0));
    let a = world.desired();
    let nodes: Vec<NodeId> = world.applications.iter().flat_map(|x| x.nodes.clone()).collect();
    let base = world.spy.batches().len();
    world.echo_moves();

    world.reload(one_workspace(200));
    // In flight: the planner has seen every window leave A (the moves are
    // echoed like a real application) while none has reached B.
    wait_until("the windows left A", || {
        let snap = world.runtime.snapshot();
        // A window whose origin does not change only resizes, and the size is
        // written once, at the end (position-only intermediate frames), so
        // there is nothing to see in flight for it.
        let moved = |f: &crate::FrameStatus| {
            let (start, target) = (a[&f.window], f.desired);
            target.is_some_and(|t| (t.x, t.y) == (start.x, start.y))
                || f.actual.is_some_and(|actual| actual != start) && f.actual != f.desired
        };
        snap.frames.len() == nodes.len()
            && snap.frames.iter().all(|f| moved(f) && f.desired != a.get(&f.window).copied())
    });
    let b = world.desired();
    world.reload(one_workspace(0));
    wait_until("back at A", || world.desired() == a && world.is_settled());

    for node in &nodes {
        let token = world.token(*node);
        assert_eq!(
            world.frame_of(*node),
            a[&token],
            "the window is back at A, exactly"
        );
        let status = world
            .runtime
            .snapshot()
            .frames
            .iter()
            .find(|f| f.window == token)
            .cloned()
            .expect("status");
        assert!(
            matches!(status.outcome, Some(crate::OperationOutcome::Applied { .. })),
            "the latest plan (A) is the one reported applied: {status:?}"
        );
        assert_eq!(status.actual, Some(a[&token]));
        let stages: Vec<FrameStage> = world
            .spy
            .frames_of(base)
            .iter()
            .filter(|(_, w, _)| *w == token)
            .map(|(_, _, s)| *s)
            .collect();
        // A window that only resizes never left A (its size is written at the
        // end), so the way back is a plain direct write, not an animation.
        assert!(
            matches!(stages.last(), Some(FrameStage::Final | FrameStage::Direct)),
            "{stages:?}"
        );
        // The retarget read and the final settle read: at most the interrupted
        // job's own settle on top.
        // A window that only resizes was not retargeted mid-motion.
        let resize_only = (b[&token].x, b[&token].y) == (a[&token].x, a[&token].y);
        // The retarget read happens only when the interrupted job was still
        // running; a frame the shard refused as `RevisionChanged` ends it first.
        let lower = usize::from(!resize_only);
        // The settle read follows the final write by 50 ms.
        wait_until("the settle read", || world.spy.snapshot_reads(token) >= lower);
        let reads = world.spy.snapshot_reads(token);
        assert!(reads <= 3, "reads: {reads} {stages:?}");
    }
    assert_eq!(world.runtime.outstanding_frames(), 0);
}

/// (c) A slow application skips frames while the others animate on time, and
/// still lands on its exact target. The slow application's writes are held in
/// its shard (no sleeps to race): the fast application must finish while they
/// are held, so the proof is causal and independent of machine load.
#[test]
fn a_slow_application_skips_frames_while_the_others_animate_on_time() {
    let mut world = World::boot(&[2, 2], one_workspace(0));
    let fast: Vec<NodeId> = world.nodes(0).to_vec();
    let slow: Vec<NodeId> = world.nodes(1).to_vec();
    let stalled = slow.clone();
    let gate = Arc::new(Gate::default());
    let held = Arc::clone(&gate);
    world.ax.on_node_write(move |node| {
        if stalled.contains(&node) {
            held.pass();
        }
    });
    let base = world.spy.batches().len();
    let (fast_before, slow_before) = (world.position_writes(&fast), world.position_writes(&slow));
    world.reload(one_workspace(200));

    wait_until("a slow write is held in its shard", || gate.entered() >= 1);
    wait_until("the fast application reached its target", || {
        world.desired().values().all(|d| d.x > 100.0) && world.all_at_target(&fast)
    });
    assert!(
        !world.all_at_target(&slow),
        "the slow application is still held while the fast one is done"
    );
    let slow_frames_while_held = world
        .spy
        .frames_of(base)
        .iter()
        .filter(|(a, _, _)| *a == world.applications[1].info.application)
        .count();
    gate.release();
    world.wait_settled("both applications settled");

    let frames = world.spy.frames_of(base);
    let submitted = |application: usize| {
        frames
            .iter()
            .filter(|(a, _, _)| *a == world.applications[application].info.application)
            .count()
    };
    let (fast_frames, slow_frames) = (submitted(0), submitted(1));
    let fast_writes = world.position_writes(&fast) - fast_before;
    let slow_writes = world.position_writes(&slow) - slow_before;
    assert!(fast_writes >= 1 && fast_frames >= 2, "the fast windows animated");
    assert!(
        slow_writes < slow_frames,
        "the slow application replaced frames: wrote {slow_writes} of {slow_frames} sent"
    );
    assert!(
        slow_frames_while_held >= 1,
        "frames kept being sent to it while held"
    );
    for node in &slow {
        assert_eq!(
            Some(world.frame_of(*node)),
            world.desired().get(&world.token(*node)).copied(),
            "the slow application still lands on its exact target"
        );
    }
    assert_eq!(world.runtime.outstanding_frames(), 0);
}

/// (d) A workspace switch never animates: only direct writes happen, in both
/// directions.
#[test]
fn a_workspace_switch_moves_windows_with_direct_writes_only() {
    let workspaces = json!([
        { "name": "one", "layout": "master", "screen": "main",
          "rules": [{ "app_id": "com.test.app1" }] },
        { "name": "two", "layout": "master", "screen": "main",
          "rules": [{ "app_id": "com.test.app2" }] },
    ]);
    let world = World::boot_visible(&[2, 2], config(0, &workspaces), 2);
    assert!(world.runtime.animation().is_running(), "animations are enabled");
    let base = world.spy.batches().len();
    let reads: usize = world
        .applications
        .iter()
        .flat_map(|a| a.nodes.iter())
        .map(|n| world.spy.snapshot_reads(world.token(*n)))
        .sum();

    // Back and forth; a workspace whose windows already sit on their targets
    // needs no write, but every write there is is direct.
    for (workspace, application) in [("two", 1), ("one", 0), ("two", 1)] {
        let owner = world.applications[application].info.application;
        world
            .runtime
            .ingress()
            .submit(Input::Intent(crate::intent::Intent::FocusWorkspace(
                workspace.to_owned(),
            )))
            .expect("intent");
        wait_until("the switch settled", || {
            let desired = world.desired();
            !desired.is_empty()
                && desired.keys().all(|w| w.application() == owner)
                && world.is_settled()
        });
    }

    let frames = world.spy.frames_of(base);
    assert!(!frames.is_empty(), "the switch wrote windows");
    assert!(
        frames.iter().all(|(_, _, stage)| *stage == FrameStage::Direct),
        "nothing animates on a workspace switch: {frames:?}"
    );
    let after: usize = world
        .applications
        .iter()
        .flat_map(|a| a.nodes.iter())
        .map(|n| world.spy.snapshot_reads(world.token(*n)))
        .sum();
    assert_eq!(
        after, reads,
        "no settle or retarget read: those belong to animations"
    );
    assert_eq!(world.runtime.outstanding_frames(), 0);
}

/// (e) A drag during an animation cancels it: no further intermediate frames
/// are sent afterwards.
#[test]
fn a_drag_during_an_animation_cancels_it_with_no_further_frames() {
    let mut world = World::boot(&[2, 2], one_workspace(0));
    let base = world.spy.batches().len();
    world.reload(one_workspace(200));
    wait_until("an intermediate frame", || {
        world.spy.frames_of(base).iter().any(|(_, _, s)| *s == FrameStage::Intermediate)
    });
    world
        .runtime
        .ingress()
        .submit(Input::Drag(DragSignal::Down {
            button: 0,
            at: DragPoint { x: 10.0, y: 10.0 },
            epoch: 1,
            time: Duration::ZERO,
        }))
        .expect("drag");
    wait_until("the gesture", || world.runtime.snapshot().drag.active);
    wait_until("the animations ended", || {
        world.runtime.outstanding_frames() == 0
    });
    let sent = world.spy.frames_of(0).len();
    let positions: Vec<Rect> = world
        .applications
        .iter()
        .flat_map(|a| a.nodes.iter())
        .map(|n| world.frame_of(*n))
        .collect();
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        world.spy.frames_of(0).len(),
        sent,
        "no frame was sent after the drag cancelled the animation"
    );
    let still: Vec<Rect> = world
        .applications
        .iter()
        .flat_map(|a| a.nodes.iter())
        .map(|n| world.frame_of(*n))
        .collect();
    assert_eq!(positions, still, "no window moved afterwards");
    assert!(
        world.spy.frames_of(base).iter().any(|(_, _, s)| *s == FrameStage::Intermediate),
        "the animation had started"
    );
}

/// (f) A stop mid-animation: seal, then the drain waits for the held write, then
/// the animation worker and the restore follow; a stuck write leaves the frame
/// drain owed (the module reports it as "frame drain").
#[test]
fn a_stop_mid_animation_drains_the_held_write_before_the_restore() {
    // Only the application whose write is held has the attribute on.
    let mut world = World::boot_leased(&[2, 2], one_workspace(0), &[
        Err(AxError::AttributeUnsupported),
        Ok(true),
    ]);
    world.wait_boot_lease_restored(1);
    std::thread::sleep(Duration::from_millis(120));
    world.ax.clear_calls();
    let held_nodes: Vec<NodeId> = world.nodes(1).to_vec();
    let gate = Arc::new(Gate::default());
    let stalled = Arc::clone(&gate);
    world.ax.on_node_write(move |node| {
        if held_nodes.contains(&node) {
            stalled.pass();
        }
    });
    world.reload(one_workspace(200));
    wait_until("a write is held in a shard", || gate.entered() >= 1);
    let is_write = |call: &Call| matches!(call, Call::SetPosition(..) | Call::SetSize(..));
    let is_restore = |call: &Call| matches!(call, Call::SetEnhancedUi(_, true));
    let writes_when_held = world.ax.calls().iter().filter(|c| is_write(c)).count();

    world.runtime.seal();
    let cancel = CancellationToken::new();
    let drained = world.runtime.drain_frames(Duration::from_millis(80), &cancel);
    assert!(
        matches!(drained, crate::DrainOutcome::TimedOut { outstanding } if outstanding >= 1),
        "the drain waits for the stuck write and reports it: {drained:?}"
    );
    assert!(
        !world.ax.calls().iter().any(is_restore),
        "nothing is restored while the drain is owed"
    );
    assert!(
        !world.runtime.stop_animation(Duration::from_millis(80)),
        "the coordinator stays alive until the held frame answers: the worker is owed too"
    );
    assert!(
        world
            .runtime
            .snapshot()
            .frames
            .iter()
            .any(|f| f.in_flight || f.outcome.is_none())
            || world.runtime.outstanding_frames() >= 1
    );

    gate.release();
    assert_eq!(
        world.runtime.drain_frames(Duration::from_secs(10), &cancel),
        crate::DrainOutcome::Drained,
        "every issued write answered"
    );
    assert!(
        world.runtime.stop_animation(Duration::from_secs(10)),
        "the coordinator ended"
    );
    // The stages after this point are observed in the fake AX call log.
    let after_drain = world.ax.calls().len();
    assert!(
        !world.ax.calls().iter().any(is_restore),
        "the drain and the join restored nothing"
    );
    let lease = world.runtime.restore_enhanced_ui(Duration::from_secs(5));
    assert!(matches!(lease, LeaseRestoreOutcome::Clean { .. }), "{lease:?}");
    let after_lease = world.ax.calls().len();
    let _ = world.runtime.restore_visibility(Duration::from_secs(1));
    std::thread::sleep(Duration::from_millis(120));
    let calls = world.ax.calls();

    let first_restore = calls.iter().position(is_restore).expect("the lease was restored");
    let last_write = calls.iter().rposition(is_write).expect("frames were written");
    assert!(
        first_restore >= after_drain,
        "the lease restore follows the completed drain and join"
    );
    assert!(last_write < after_drain, "no write after the drain completed");
    assert!(
        calls[first_restore..].iter().all(|c| !is_write(c)),
        "no AX write after the restore"
    );
    let hidden = calls.iter().position(|c| matches!(c, Call::SetHidden(..)));
    assert!(
        hidden.is_none_or(|at| at >= after_lease),
        "any hidden restore follows the lease restore: {hidden:?} vs {after_lease}"
    );
    let writes = calls.iter().filter(|c| is_write(c)).count();
    assert!(
        writes <= writes_when_held + 2,
        "only the held frame's remaining write finished after the seal"
    );
    // The held frame was issued: its attempt is not reported as applied.
    let snap = world.runtime.snapshot();
    assert!(
        snap.frames.iter().all(|f| !matches!(
            f.outcome,
            Some(crate::OperationOutcome::Applied { .. })
        ) || f.actual == f.desired),
        "an interrupted attempt is never reported as an applied target"
    );
}

// ------------------------------------------------------- display-clock pacing

const D1: DisplayId = DisplayId::new(1);
const D2: DisplayId = DisplayId::new(2);

fn two_displays() -> DisplayLayout {
    DisplayLayout::new(vec![
        display(1, true, Some(60.0), 0.0),
        display(2, false, Some(120.0), 1920.0),
    ])
}

impl World {
    /// Fires every display's clock until `done`.
    fn drive(&self, displays: &[DisplayId], what: &str, mut done: impl FnMut() -> bool) {
        wait_until(what, || {
            for display in displays {
                self.clocks.fire(*display, Instant::now());
            }
            std::thread::sleep(Duration::from_millis(2));
            done()
        });
    }

    /// The fake AX writes to `node` since call number `from`, in order:
    /// `(is_size, call index)`.
    fn writes_to(&self, node: NodeId, from: usize) -> Vec<(bool, usize)> {
        self.ax
            .calls()
            .iter()
            .enumerate()
            .skip(from)
            .filter_map(|(index, call)| match call {
                Call::SetPosition(n, _) if *n == node => Some((false, index)),
                Call::SetSize(n, _) if *n == node => Some((true, index)),
                _ => None,
            })
            .collect()
    }

    /// The sizes the fake AX was asked to set on `node` since call `from`.
    fn size_writes_to(&self, node: NodeId, from: usize) -> Vec<(f64, f64)> {
        self.ax
            .calls()
            .iter()
            .skip(from)
            .filter_map(|call| match call {
                Call::SetSize(n, size) if *n == node => Some((size.width, size.height)),
                _ => None,
            })
            .collect()
    }

    /// Asserts that the sizes written to `node` since call `from` ramp
    /// monotonically from `start` to `target` in several writes, the last of
    /// them exactly the target.
    fn assert_size_ramp(&self, node: NodeId, from: usize, start: (f64, f64), target: (f64, f64)) {
        let sizes = self.size_writes_to(node, from);
        assert!(sizes.len() >= 3, "several size writes: {sizes:?}");
        assert_eq!(
            sizes.last(),
            Some(&target),
            "the last write is the target: {sizes:?}"
        );
        for (axis, from, to) in [(0, start.0, target.0), (1, start.1, target.1)] {
            let values: Vec<f64> =
                sizes.iter().map(|z| if axis == 0 { z.0 } else { z.1 }).collect();
            let step = if to >= from { 1.0 } else { -1.0 };
            assert!(
                values.windows(2).all(|w| (w[1] - w[0]) * step >= 0.0),
                "monotonic: {values:?}"
            );
            let (low, high) = (from.min(to), from.max(to));
            assert!(
                values.iter().all(|v| *v >= low && *v <= high),
                "between start and target: {values:?}"
            );
        }
        assert!(
            sizes[..sizes.len() - 1].iter().any(|z| *z != target),
            "intermediate sizes differ from the target: {sizes:?}"
        );
    }

    /// The layout change took effect: every window has a new target.
    fn wait_replanned(&self, before: &BTreeMap<WindowInstance, Rect>) {
        wait_until("the new plan", || {
            let now = self.desired();
            now.len() == before.len() && now.iter().all(|(w, d)| before.get(w) != Some(d))
        });
    }
}

/// An animation paced by a fake clock advances exactly one frame per tick, and
/// the clock lives only as long as the display has a running animation.
#[test]
fn an_animation_paced_by_a_fake_clock_advances_one_frame_per_tick() {
    let clocks = FakeFrameClockFactory::new();
    let mut world = World::boot_clocked(&[1], one_workspace(200), clocks.clone(), one_display());
    assert_eq!(clocks.live(), 0, "idle owns no clock");
    assert!(clocks.attempts().is_empty());
    let before = world.desired();
    let base = world.spy.batches().len();
    world.reload(one_workspace(0));
    world.wait_replanned(&before);
    wait_until("the display clock", || clocks.is_live(D1));
    let node = world.nodes(0)[0];
    for tick in 1..=4 {
        let seen = world.spy.frames_of(base).len();
        assert!(clocks.fire(D1, Instant::now()));
        wait_until("the frame of the tick", || {
            world.spy.frames_of(base).len() > seen
        });
        std::thread::sleep(Duration::from_millis(15));
        assert_eq!(
            world.spy.frames_of(base).len(),
            seen + 1,
            "tick {tick}: one frame, not a timer's worth"
        );
    }
    world.drive(&[D1], "the animation", || {
        world.is_settled() && world.all_at_target(&[node]) && clocks.live() == 0
    });
    assert_eq!(clocks.created(), vec![D1], "one clock for the whole animation");
    assert_eq!(
        clocks.stopped(),
        vec![D1],
        "stopped when the display had none left"
    );
    assert!(world.runtime.animation_pacing().is_empty());
}

/// Smoke test over the real sharded adapter: display ticks arrive at the
/// display's real period (not every 4 ms) and a healthy application is never
/// put in fallback. The tick phase here is not controlled; the phase coverage
/// that pins the end-lag rule lives in `fallback_tests` with a fake clock.
#[test]
fn animations_ticked_at_the_real_period_never_put_a_healthy_application_in_fallback() {
    let clocks = FakeFrameClockFactory::new();
    let mut world = World::boot_clocked(&[2], one_workspace(0), clocks.clone(), one_display());
    let node = world.nodes(0)[0];
    for round in 0..6_u32 {
        let before = world.desired();
        world.reload(one_workspace(if round % 2 == 0 { 200 } else { 0 }));
        world.wait_replanned(&before);
        // A different phase each round.
        std::thread::sleep(Duration::from_millis(u64::from(round) * 3));
        wait_until("the animation", || {
            clocks.fire(D1, Instant::now());
            std::thread::sleep(Duration::from_micros(16_667));
            world.is_settled() && world.all_at_target(&[node]) && clocks.live() == 0
        });
        assert!(
            world.runtime.animation().report().fallbacks.is_empty(),
            "round {round}: {:?}",
            world.runtime.animation().report()
        );
    }
}

/// A growth or shrink move writes the interpolated size on every frame, with
/// the position, and the last write is exactly the target.
#[test]
fn growth_and_shrink_moves_ramp_the_size_at_the_fake_ax_and_end_exact() {
    let mut world = World::boot_clocked(
        &[1],
        one_workspace(200),
        FakeFrameClockFactory::new(),
        one_display(),
    );
    let node = world.nodes(0)[0];

    for (name, outer) in [("growth", 0), ("shrink", 200)] {
        let before = world.desired();
        let start = world.frame_of(node);
        let from = world.ax.calls().len();
        world.reload(one_workspace(outer));
        world.wait_replanned(&before);
        world.drive(&[D1], name, || {
            world.is_settled() && world.all_at_target(&[node])
        });
        let target = world.desired()[&world.token(node)];
        let positions = world.writes_to(node, from).iter().filter(|(size, _)| !*size).count();
        assert!(positions >= 3, "{name}: several position writes");
        world.assert_size_ramp(
            node,
            from,
            (start.width, start.height),
            (target.width, target.height),
        );
        assert_eq!(world.frame_of(node), target, "{name}: exact");
    }
}

/// A window sent to another display is paced by that display's clock and gets
/// its size ramps like any other move.
#[test]
fn a_cross_display_move_is_paced_by_the_target_display_and_ramps_its_size() {
    let clocks = FakeFrameClockFactory::new();
    let on = |screen: &str, outer| {
        config(
            outer,
            &json!([{ "name": "w", "layout": "master", "screen": screen,
                      "rules": [{ "app_id": "com.test.app1" }, { "app_id": "com.test.app2" }] }]),
        )
    };
    let mut world = World::boot_clocked(&[1, 1], on("main", 0), clocks.clone(), two_displays());
    let moved = world.nodes(0)[0];
    let before = world.desired();
    let start = world.frame_of(moved);
    let from = world.ax.calls().len();
    // The workspace moves to the other display, and its windows with it.
    world.reload(on("secondary", 100));
    world.wait_replanned(&before);
    let target = world.desired()[&world.token(moved)];
    assert!(
        target.x >= 1920.0,
        "the window goes to the second display: {target:?}"
    );
    world.drive(&[D1, D2], "the move", || {
        world.is_settled() && world.all_at_target(&[moved]) && clocks.live() == 0
    });
    assert!(
        clocks.created().contains(&D2),
        "paced by the target display: {:?}",
        clocks.created()
    );
    world.assert_size_ramp(
        moved,
        from,
        (start.width, start.height),
        (target.width, target.height),
    );
    assert_eq!(world.frame_of(moved), target);
}

/// Stopping Tiling with a clock running stops and frees it.
#[test]
fn stopping_tiling_with_a_clock_running_frees_the_clock() {
    let clocks = FakeFrameClockFactory::new();
    let mut world = World::boot_clocked(&[1], one_workspace(200), clocks.clone(), one_display());
    let before = world.desired();
    world.reload(one_workspace(0));
    world.wait_replanned(&before);
    wait_until("the display clock", || clocks.live() == 1);
    world.runtime.shutdown();
    assert_eq!(clocks.live(), 0, "stopped and freed");
    assert_eq!(clocks.stopped(), vec![D1]);
    assert!(
        !clocks.fire(D1, Instant::now()),
        "no callback can run after the stop"
    );
}

/// A display whose clock cannot be created animates on the timer and says so.
#[test]
fn a_clock_creation_failure_animates_on_the_timer_and_reports_it() {
    let mut world = World::boot(&[1], one_workspace(200));
    let node = world.nodes(0)[0];
    let before = world.desired();
    world.reload(one_workspace(0));
    world.wait_replanned(&before);
    world.wait_settled("the timer-paced animation");
    assert!(world.all_at_target(&[node]));
    assert_eq!(world.runtime.animation_pacing(), vec![
        "animation pacing: display 1 uses a timer: timer".to_owned()
    ]);
    assert_eq!(world.clocks.live(), 0);
}

// --------------------------------------------- enhanced user interface lease

impl World {
    fn attribute(&self, application: usize) -> Result<bool, AxError> {
        use stache_macos::ax::WindowAx;
        self.ax
            .enhanced_user_interface(&self.ax.element(self.applications[application].element))
    }

    /// The `AXEnhancedUserInterface` writes made to application `i`, in order.
    fn lease_writes(&self, application: usize) -> Vec<bool> {
        let element = self.applications[application].element;
        self.ax
            .enhanced_ui_writes()
            .into_iter()
            .filter(|(node, _)| *node == element)
            .map(|(_, on)| on)
            .collect()
    }

    /// Waits until the first layout's lease (if any) is restored.
    fn wait_boot_lease_restored(&self, application: usize) {
        wait_until("the boot lease to be restored", || {
            self.attribute(application) == Ok(true)
        });
    }
}

/// One application with the attribute on and one with it off animate a layout
/// change over the real sharded adapter. The "on" application toggles off once
/// before its first frame and back on once after its last, long after the
/// 50 ms grace has passed many times; the "off" application is never written.
#[test]
fn a_lease_wraps_the_animation_of_the_application_with_the_attribute_on() {
    let mut world = World::boot_leased(&[2, 2], one_workspace(0), &[Ok(true), Ok(false)]);
    world.wait_boot_lease_restored(0);
    // Let the boot layout's grace for the other application expire too.
    std::thread::sleep(Duration::from_millis(120));
    world.ax.clear_calls();
    let before = world.desired();

    world.reload(one_workspace(200));
    wait_until("the animation", || {
        let now = world.desired();
        now.len() == before.len()
            && now.iter().all(|(w, d)| before.get(w) != Some(d))
            && world.is_settled()
    });
    wait_until("the lease to be restored after the grace", || {
        world.lease_writes(0) == [false, true]
    });

    assert_eq!(
        world.lease_writes(0),
        [false, true],
        "one toggle for the whole animation"
    );
    assert_eq!(world.attribute(0), Ok(true));
    let calls = world.ax.calls();
    let index_of = |wanted: &Call| calls.iter().position(|call| call == wanted).expect("recorded");
    let off = index_of(&Call::SetEnhancedUi(world.applications[0].element, false));
    let on = index_of(&Call::SetEnhancedUi(world.applications[0].element, true));
    let mine: Vec<usize> = calls
        .iter()
        .enumerate()
        .filter(
            |(_, call)| matches!(call, Call::SetPosition(node, _) if world.nodes(0).contains(node)),
        )
        .map(|(index, _)| index)
        .collect();
    assert!(!mine.is_empty());
    assert!(
        mine.iter().all(|index| off < *index && *index < on),
        "writes sit inside the lease"
    );

    assert!(
        world.lease_writes(1).is_empty(),
        "an application already off is never written"
    );
    assert_eq!(world.attribute(1), Ok(false));
    assert!(world.position_writes(world.nodes(1)) > 0, "it still animated");
    let diagnostics = crate::status::enhanced_ui_diagnostics(&world.runtime.snapshot());
    assert!(
        diagnostics.is_empty(),
        "a clean lease reports nothing: {diagnostics:?}"
    );
}

/// A stop mid-animation restores the lease only after the held write drained.
#[test]
fn a_stop_mid_animation_restores_the_lease_after_the_drain() {
    let mut world = World::boot_leased(&[2], one_workspace(0), &[Ok(true)]);
    world.wait_boot_lease_restored(0);
    std::thread::sleep(Duration::from_millis(120));
    world.ax.clear_calls();
    let held_nodes: Vec<NodeId> = world.nodes(0).to_vec();
    let gate = Arc::new(Gate::default());
    let stalled = Arc::clone(&gate);
    world.ax.on_node_write(move |node| {
        if held_nodes.contains(&node) {
            stalled.pass();
        }
    });
    world.reload(one_workspace(200));
    wait_until("a write is held in the shard", || gate.entered() >= 1);
    assert_eq!(
        world.lease_writes(0),
        [false],
        "the lease is open under the held write"
    );

    world.runtime.seal();
    let cancel = CancellationToken::new();
    assert!(matches!(
        world.runtime.drain_frames(Duration::from_millis(80), &cancel),
        crate::DrainOutcome::TimedOut { .. }
    ));
    assert_eq!(
        world.lease_writes(0),
        [false],
        "nothing is restored before the drain"
    );

    gate.release();
    assert_eq!(
        world.runtime.drain_frames(Duration::from_secs(10), &cancel),
        crate::DrainOutcome::Drained
    );
    assert!(world.runtime.stop_animation(Duration::from_secs(10)));
    let outcome = world.runtime.restore_enhanced_ui(Duration::from_secs(5));
    assert!(
        matches!(outcome, LeaseRestoreOutcome::Clean { .. }),
        "{outcome:?}"
    );
    assert_eq!(world.lease_writes(0), [false, true]);
    assert_eq!(world.attribute(0), Ok(true));
    let calls = world.ax.calls();
    let restore = calls
        .iter()
        .position(|call| *call == Call::SetEnhancedUi(world.applications[0].element, true))
        .expect("restored");
    let last_write = calls
        .iter()
        .rposition(|call| matches!(call, Call::SetPosition(..) | Call::SetSize(..)))
        .expect("written");
    assert!(last_write < restore, "the restore follows every frame write");
}

/// A third party that turns the attribute back on is counted and not fought,
/// and the count reaches the status.
#[test]
fn a_third_party_re_enable_shows_up_in_the_status() {
    let mut world = World::boot_leased(&[1], one_workspace(0), &[Ok(true)]);
    world.wait_boot_lease_restored(0);
    std::thread::sleep(Duration::from_millis(120));
    world.ax.clear_calls();
    let ax = world.ax.clone();
    world.ax.on_enhanced_ui_set(move |node, on| {
        if !on {
            ax.edit(node, |app| app.enhanced_ui = Ok(true));
        }
    });
    world.reload(one_workspace(200));
    let application = world.applications[0].info.application;
    wait_until("the re-enable to be reported", || {
        world
            .runtime
            .snapshot()
            .enhanced_ui_of(application)
            .is_some_and(|stats| stats.re_enables >= 1)
    });
    world.wait_settled("the animation");
    assert_eq!(
        world.lease_writes(0),
        [false],
        "it was turned off once and never fought"
    );
    assert_eq!(world.attribute(0), Ok(true), "the third party's value stands");
    let diagnostics = crate::status::enhanced_ui_diagnostics(&world.runtime.snapshot());
    assert_eq!(diagnostics, [
        "enhanced ui: App 1 (com.test.app1): AXEnhancedUserInterface was turned back on during \
         animation (1 in a row); it is not fought"
    ]);
}

// ------------------------------------------------ metrics and instant fallback

impl World {
    /// Changes the layout (outer gap `outer`) and waits until every window is
    /// at its new target and the display clocks are gone. Returns where the
    /// spy's batch list stood before the change.
    fn change_layout(&mut self, outer: u32) -> usize {
        let before = self.desired();
        let base = self.spy.batches().len();
        self.reload(one_workspace(outer));
        self.wait_replanned(&before);
        self.drive(&[D1], "the layout change", || {
            self.is_settled() && self.clocks.live() == 0
        });
        base
    }

    /// The stages written to `application`'s windows since batch `base`.
    fn stages_of(&self, application: usize, base: usize) -> Vec<FrameStage> {
        let id = self.applications[application].info.application;
        self.spy
            .frames_of(base)
            .into_iter()
            .filter(|(a, _, _)| *a == id)
            .map(|(_, _, stage)| stage)
            .collect()
    }

    fn diagnostics(&self) -> Vec<String> {
        crate::status::animation_diagnostics(
            &self.runtime.snapshot(),
            &self.runtime.animation().report(),
        )
    }

    /// Terminates application `index` and launches a new instance of it (a new
    /// `ApplicationId`) with fresh windows, then waits until they are laid out.
    fn relaunch(&mut self, index: usize, n: u64) {
        let old = self.applications[index].info.clone();
        let count = self.applications[index].nodes.len();
        self.runtime
            .ingress()
            .submit(Input::Lifecycle {
                kind: LifecycleKind::Terminated,
                info: old.clone(),
            })
            .expect("terminate");
        let mut cg = 900 + 10 * n as u32;
        let nodes: Vec<NodeId> = (0..count)
            .map(|_| {
                cg += 1;
                window_node(&self.ax, cg)
            })
            .collect();
        let element = self.ax.add_node(FakeNode {
            windows: Ok(nodes.clone()),
            enhanced_ui: Err(AxError::AttributeUnsupported),
            ..FakeNode::default()
        });
        let fresh = info(n, &old.bundle_id, &old.name);
        self.ax.set_application(fresh.pid, element);
        self.runtime
            .ingress()
            .submit(Input::Lifecycle {
                kind: LifecycleKind::Launched,
                info: fresh.clone(),
            })
            .expect("launch");
        self.applications[index] = Application { info: fresh, element, nodes };
        let fresh_cgs: Vec<u32> =
            self.applications[index].nodes.iter().map(|n| self.cg_of(*n)).collect();
        wait_until("the relaunched windows", || {
            let snap = self.runtime.snapshot();
            snap.windows.len() == self.visible
                && fresh_cgs
                    .iter()
                    .all(|cg| snap.windows.iter().any(|w| w.cg_window_id == Some(*cg)))
                && self.is_settled()
        });
    }
}

/// A realistic desktop: three applications on one display with a frame clock.
/// The second one's AX writes are slow (a stand-in for an application that
/// cannot keep up). It crosses into instant fallback after its second slow
/// animation; its next layout change is one direct write per window, the others
/// keep animating, status says so, and a relaunch of the slow application
/// animates again.
#[test]
fn a_slow_application_falls_back_while_the_others_keep_animating() {
    let mut world = World::boot_clocked(
        &[2, 1, 2],
        one_workspace(0),
        FakeFrameClockFactory::new(),
        one_display(),
    );
    let slow_nodes = world.nodes(1).to_vec();
    world.ax.on_node_write(move |node| {
        if slow_nodes.contains(&node) {
            std::thread::sleep(Duration::from_millis(150));
        }
    });
    let slow = world.applications[1].info.application;

    // First slow animation: 1 of 1 is not enough.
    let base = world.change_layout(200);
    assert!(
        world.stages_of(1, base).iter().any(|s| *s != FrameStage::Direct),
        "the slow application animates at first"
    );
    assert!(
        world.runtime.animation().report().fallbacks.is_empty(),
        "one bad animation"
    );

    // Second: 2 of its last 3 cross the line, mid-run of the whole desktop.
    world.change_layout(0);
    let report = world.runtime.animation().report();
    assert_eq!(report.fallbacks.len(), 1, "{report:?}");
    assert_eq!(report.fallbacks[0].0, slow);
    let line = world
        .diagnostics()
        .into_iter()
        .find(|line| line.starts_with("animation: "))
        .expect("the fallback is reported");
    assert!(
        line.starts_with("animation: App 2 (com.test.app2) moves without animation: ")
            && line.ends_with(" in 2 of its last 3 layout changes"),
        "{line}"
    );
    assert!(
        line.contains("writes fewer than 15 frames per second")
            || line.contains("ends more than 250 ms late"),
        "{line}"
    );

    // Its next layout change is a single direct write per window; the others
    // keep animating.
    let base = world.change_layout(200);
    assert_eq!(
        world.stages_of(1, base),
        [FrameStage::Direct],
        "one direct write"
    );
    for application in [0, 2] {
        let stages = world.stages_of(application, base);
        assert!(
            stages.len() > 2 * world.nodes(application).len()
                && stages.iter().all(|s| *s != FrameStage::Direct),
            "application {application} animates: {stages:?}"
        );
    }
    assert!(
        world.all_at_target(world.nodes(1)),
        "it still reached the exact target"
    );
    assert_eq!(
        world.runtime.animation().report().fallbacks.len(),
        1,
        "only the slow one"
    );

    // Status: the fallback line and a metrics line per application with data.
    let lines = world.diagnostics();
    for who in [
        "App 1 (com.test.app1)",
        "App 2 (com.test.app2)",
        "App 3 (com.test.app3)",
    ] {
        assert!(
            lines.iter().any(|l| l.starts_with(&format!("animation metrics: {who}: "))
                && l.contains(" animations, rate p50 ")
                && l.contains(", dropped ")
                && l.contains(" ms, end lag p95 ")),
            "{who}: {lines:?}"
        );
    }

    // After the final write and its settle readback, nothing writes it again.
    let writes = world.position_writes(world.nodes(1));
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(world.position_writes(world.nodes(1)), writes, "no fighting");

    // A relaunch is a new instance: it is not in fallback and animates again.
    world.relaunch(1, 11);
    wait_until("the old instance to be forgotten", || {
        world.runtime.animation().report().fallbacks.is_empty()
    });
    let base = world.change_layout(0);
    let stages = world.stages_of(1, base);
    assert!(
        stages.len() >= 3 && stages.iter().all(|s| *s != FrameStage::Direct),
        "the relaunched application animates: {stages:?}"
    );
    assert!(
        !world.diagnostics().iter().any(|l| l.starts_with("animation: ")),
        "no fallback line"
    );
}

/// An application that keeps turning Enhanced UI back on moves without
/// animation after the second consecutive re-enable.
#[test]
fn a_third_party_that_keeps_re_enabling_enhanced_ui_falls_back_to_direct_writes() {
    let mut world = World::boot_leased(&[1], one_workspace(0), &[Ok(true)]);
    world.wait_boot_lease_restored(0);
    std::thread::sleep(Duration::from_millis(120));
    let ax = world.ax.clone();
    world.ax.on_enhanced_ui_set(move |node, on| {
        if !on {
            ax.edit(node, |app| app.enhanced_ui = Ok(true));
        }
    });
    let application = world.applications[0].info.application;
    let enhanced = |world: &World| {
        world.runtime.snapshot().enhanced_ui_of(application).map_or(0, |s| s.re_enables)
    };
    world.reload(one_workspace(200));
    wait_until("the first re-enable", || enhanced(&world) >= 1);
    world.wait_settled("the first animation");
    assert!(
        world.runtime.animation().report().fallbacks.is_empty(),
        "one is tolerated"
    );
    // The streak counts only consecutive leases: wait out the grace, then again.
    std::thread::sleep(Duration::from_millis(120));
    world.reload(one_workspace(0));
    wait_until("the second re-enable", || enhanced(&world) >= 2);
    wait_until("the fallback", || {
        !world.runtime.animation().report().fallbacks.is_empty()
    });
    assert_eq!(
        world.diagnostics()[0],
        "animation: App 1 (com.test.app1) moves without animation: turns Enhanced UI back on"
    );
    world.wait_settled("the second animation");
    let base = world.spy.batches().len();
    let before = world.desired();
    world.reload(one_workspace(200));
    world.wait_replanned(&before);
    world.wait_settled("the direct write");
    let stages: Vec<_> = world.spy.frames_of(base).into_iter().map(|(_, _, stage)| stage).collect();
    assert_eq!(stages, [FrameStage::Direct], "one direct write");
}

/// Regression for a real 120 Hz run: one application with three windows, whose
/// every AX write takes 5 ms (a frame writes position and size, so a batch of three windows outlasts the 8.3 ms tick),
/// changes layout many times. It writes well above 15 frames per second and
/// ends within about 100 ms of the scheduled end, so it must keep
/// animating: the three windows of one layout change count as one record, and
/// the thresholds do not depend on the refresh rate.
#[test]
fn a_three_window_application_on_a_120_hz_display_keeps_animating_across_layout_changes() {
    three_windows_keep_animating_at_120_hz(5, false);
}

/// The same with 7 ms per AX call (14 ms per position+size write, so 42 ms per
/// three-window batch; 10 ms per call would be 12 Hz, below the threshold): the
/// application cannot keep up with
/// 120 Hz, frames are dropped, and it still is not put in fallback.
#[test]
fn a_three_window_application_with_heavy_writes_drops_frames_but_keeps_animating() {
    three_windows_keep_animating_at_120_hz(7, true);
}

fn three_windows_keep_animating_at_120_hz(write_ms: u64, expect_drops: bool) {
    let on = |layout: &str| {
        config(
            0,
            &json!([{ "name": "w", "layout": layout, "screen": "main",
                      "rules": [{ "app_id": "com.test.app1" }] }]),
        )
    };
    let mut world = World::boot_clocked(
        &[3],
        on("grid"),
        FakeFrameClockFactory::new(),
        DisplayLayout::new(vec![display(1, true, Some(120.0), 0.0)]),
    );
    let nodes = world.nodes(0).to_vec();
    let slow = nodes.clone();
    world.ax.on_node_write(move |node| {
        if slow.contains(&node) {
            std::thread::sleep(Duration::from_millis(write_ms));
        }
    });
    for (round, layout) in [
        "master",
        "monocle",
        "master",
        "grid",
        "split_vertical",
        "master",
        "dwindle",
    ]
    .into_iter()
    .enumerate()
    {
        let before = world.desired();
        let base = world.spy.batches().len();
        world.reload(on(layout));
        wait_until(&format!("the new plan for {layout}"), || {
            world.desired() != before
        });
        wait_until("the layout change", || {
            world.clocks.fire(D1, Instant::now());
            std::thread::sleep(Duration::from_micros(8_333));
            world.is_settled() && world.clocks.live() == 0
        });
        let stages = world.stages_of(0, base);
        assert!(
            stages.len() > 3 && stages.iter().all(|s| *s != FrameStage::Direct),
            "round {round} ({layout}) animates: {stages:?}"
        );
        assert!(
            world.all_at_target(&nodes),
            "round {round} ({layout}) reached its target"
        );
        let report = world.runtime.animation().report();
        assert!(
            report.fallbacks.is_empty(),
            "round {round} ({layout}): {report:?}"
        );
        assert!(
            !world.diagnostics().iter().any(|l| l.starts_with("animation: ")),
            "round {round} ({layout}): no fallback line"
        );
    }
    if expect_drops {
        let report = world.runtime.animation().report();
        let dropped: usize = report.metrics.iter().map(|(_, m)| m.dropped).sum();
        assert!(dropped > 0, "the application could not keep up: {report:?}");
    }
}

/// Regression for a real run where windows slid at their old size and snapped
/// at the end: three windows of one application on a 120 Hz display, every AX
/// write slow enough that frames are dropped, go through grid, master,
/// monocle and grid. After every change each window sits exactly on its planned
/// target, size included, and every window that resized wrote several
/// intermediate sizes on the way.
#[test]
fn three_windows_ramp_their_sizes_and_end_exact_through_grid_master_monocle_grid() {
    three_windows_ramp_and_end_exact(5, false);
}

#[test]
fn three_windows_with_heavy_writes_ramp_and_end_exact_while_dropping_frames() {
    three_windows_ramp_and_end_exact(7, true);
}

fn three_windows_ramp_and_end_exact(write_ms: u64, expect_drops: bool) {
    let on = |layout: &str| {
        config(
            0,
            &json!([{ "name": "w", "layout": layout, "screen": "main",
                      "rules": [{ "app_id": "com.test.app1" }] }]),
        )
    };
    let mut world = World::boot_clocked(
        &[3],
        on("grid"),
        FakeFrameClockFactory::new(),
        DisplayLayout::new(vec![display(1, true, Some(120.0), 0.0)]),
    );
    let nodes = world.nodes(0).to_vec();
    let slow = nodes.clone();
    world.ax.on_node_write(move |node| {
        if slow.contains(&node) {
            std::thread::sleep(Duration::from_millis(write_ms));
        }
    });
    let mut resized = 0;
    for layout in ["master", "monocle", "grid"] {
        let before = world.desired();
        let starts: Vec<Rect> = nodes.iter().map(|n| world.frame_of(*n)).collect();
        let from = world.ax.calls().len();
        world.reload(on(layout));
        wait_until(&format!("the new plan for {layout}"), || {
            world.desired() != before
        });
        wait_until(&format!("the {layout} animation"), || {
            world.clocks.fire(D1, Instant::now());
            std::thread::sleep(Duration::from_micros(8_333));
            world.is_settled() && world.clocks.live() == 0
        });
        assert!(
            world.runtime.animation().report().fallbacks.is_empty(),
            "{layout}: the application keeps animating"
        );
        let desired = world.desired();
        for (node, start) in nodes.iter().zip(&starts) {
            let target = desired[&world.token(*node)];
            assert_eq!(world.frame_of(*node), target, "{layout}: exact final frame");
            let sizes = world.size_writes_to(*node, from);
            assert_eq!(
                sizes.last(),
                Some(&(target.width, target.height)),
                "{layout}: the last size write is the target: {sizes:?}"
            );
            if (start.width - target.width).abs() > 0.5
                || (start.height - target.height).abs() > 0.5
            {
                resized += 1;
                world.assert_size_ramp(
                    *node,
                    from,
                    (start.width, start.height),
                    (target.width, target.height),
                );
            }
        }
    }
    assert!(resized >= 3, "the scenario resizes windows: {resized}");
    if expect_drops {
        let report = world.runtime.animation().report();
        let dropped: usize = report.metrics.iter().map(|(_, m)| m.dropped).sum();
        assert!(dropped > 0, "the application could not keep up: {report:?}");
    }
}
