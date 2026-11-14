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
use crate::animation::FallbackReason;
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

/// What the spy saw, in one order across every kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seen {
    /// A frame of a submitted batch, with its stage.
    Frame(WindowInstance, FrameStage),
    /// A queued snapshot read.
    Snapshot(WindowInstance),
    /// A blocking snapshot read.
    FullSnapshot(WindowInstance),
    /// A frame-only read.
    FrameRead(WindowInstance),
    /// A re-listing of an application (a reconciliation, not a geometry read).
    Refresh(ApplicationId),
}

/// The production backend with a recorder in front of the frame and snapshot
/// submissions.
struct Spy {
    inner: SystemWindowBackend,
    batches: Mutex<Vec<Batch>>,
    /// Queued (non-blocking) snapshot reads.
    snapshots: Mutex<Vec<WindowInstance>>,
    /// Blocking snapshot reads (the actor's geometry, title and focus reads).
    full_snapshots: Mutex<Vec<WindowInstance>>,
    /// Frame-only reads (settle and retarget without bounds).
    frame_reads: Mutex<Vec<WindowInstance>>,
    /// Every frame and read in one order.
    seen: Mutex<Vec<Seen>>,
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

    fn full_snapshot_reads(&self, window: WindowInstance) -> usize {
        lock(&self.full_snapshots).iter().filter(|w| **w == window).count()
    }

    fn frame_reads(&self, window: WindowInstance) -> usize {
        lock(&self.frame_reads).iter().filter(|w| **w == window).count()
    }

    fn seen(&self) -> Vec<Seen> { lock(&self.seen).clone() }

    /// The settle and retarget reads queued for a window: frame-only reads,
    /// plus any queued snapshot.
    fn queued_reads(&self, window: WindowInstance) -> usize {
        self.snapshot_reads(window) + self.frame_reads(window)
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
        lock(&self.seen).push(Seen::Refresh(application));
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
        lock(&self.full_snapshots).push(window);
        lock(&self.seen).push(Seen::FullSnapshot(window));
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
        lock(&self.seen)
            .extend(batch.iter().map(|(r, _)| Seen::Frame(r.issuance.window(), r.stage)));
        self.inner.submit_frames(application, batch)
    }

    fn submit_snapshot(
        &self,
        window: WindowInstance,
        reply: Box<dyn FnOnce(SnapshotOutcome) + Send>,
    ) -> Result<(), DispatchError> {
        lock(&self.snapshots).push(window);
        lock(&self.seen).push(Seen::Snapshot(window));
        self.inner.submit_snapshot(window, reply)
    }

    fn submit_frame_read(
        &self,
        window: WindowInstance,
        reply: Box<dyn FnOnce(stache_macos::windows::FrameReadOutcome) + Send>,
    ) -> Result<(), DispatchError> {
        lock(&self.frame_reads).push(window);
        lock(&self.seen).push(Seen::FrameRead(window));
        self.inner.submit_frame_read(window, reply)
    }

    fn window_bounds(&self, cg_window_id: u32) -> Option<Rect> {
        self.inner.window_bounds(cg_window_id)
    }

    fn window_bounds_missing(&self) -> bool { self.inner.window_bounds_missing() }

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
    /// The actor's clock; ticks are fired at its time.
    clock: Arc<dyn crate::actor::Clock>,
    /// The fake window server, when the bounds symbols are present.
    server: Option<Arc<stache_macos::windows::FakeWindowBounds>>,
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

/// The fake window server: answers a window's bounds by `CGWindowID` from the
/// same node the fake AX arena moves (no AX call is logged), so a bounds frame
/// is the frame the rig last wrote.
fn window_server(
    ax: &FakeWindowAx,
    windows: Vec<(u32, NodeId)>,
) -> Arc<stache_macos::windows::FakeWindowBounds> {
    let ax = ax.clone();
    Arc::new(stache_macos::windows::FakeWindowBounds::new(move |cg| {
        let node = windows.iter().find(|(id, _)| *id == cg)?.1;
        let mut found = None;
        ax.edit(node, |n| {
            if let (Ok(p), Ok(s)) = (n.position, n.size) {
                found = Some(stache_macos::ax::AxFrame { origin: p, size: s });
            }
        });
        found
    }))
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
            None,
            true,
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
            None,
            true,
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
        Self::boot_with(
            counts,
            config,
            counts.iter().sum(),
            clocks,
            layout,
            &[],
            None,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)] // the one rig builder every variant calls
    fn boot_with(
        counts: &[usize],
        config: Arc<TilingConfig>,
        visible: usize,
        clocks: FakeFrameClockFactory,
        layout: DisplayLayout,
        eui: &[Result<bool, AxError>],
        extras: Option<(Arc<dyn crate::actor::Clock>, stache_sdk::ScopedLogger)>,
        bounds: bool,
    ) -> Self {
        let ax = FakeWindowAx::new();
        let mut applications = Vec::new();
        let mut windows = Vec::new();
        let mut cg = 100;
        for (index, count) in counts.iter().enumerate() {
            let nodes: Vec<NodeId> = (0..*count)
                .map(|_| {
                    cg += 1;
                    let node = window_node(&ax, cg);
                    windows.push((cg, node));
                    node
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
        let inner = SystemWindowBackend::from_adapter(adapter, false);
        let server = bounds.then(|| window_server(&ax, windows));
        let spy = Arc::new(Spy {
            inner: match &server {
                Some(server) => inner.with_bounds(Arc::clone(server) as _),
                None => inner,
            },
            batches: Mutex::new(Vec::new()),
            snapshots: Mutex::new(Vec::new()),
            full_snapshots: Mutex::new(Vec::new()),
            frame_reads: Mutex::new(Vec::new()),
            seen: Mutex::new(Vec::new()),
        });
        let directory = Arc::new(FakeAppDirectory::default());
        *directory.apps.lock().expect("apps") =
            applications.iter().map(|a| a.info.clone()).collect();
        let mut options = TrackingOptions::new("");
        options.frame_clocks = Some(Arc::new(clocks.clone()));
        let clock = match extras {
            Some((clock, logger)) => {
                options.logger = Some(logger);
                clock
            }
            None => Arc::new(crate::actor::SystemClock) as Arc<dyn crate::actor::Clock>,
        };
        let runtime = TrackingRuntime::start_with_clock(
            Arc::clone(&spy) as Arc<dyn WindowBackend>,
            directory as Arc<dyn AppDirectory>,
            &options,
            Arc::clone(&clock),
            None,
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
            clock,
            server,
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
    let reads_before: usize = before.keys().map(|w| world.spy.queued_reads(*w)).sum();

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
            world.spy.queued_reads(token),
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
        wait_until("the settle read", || world.spy.queued_reads(token) >= lower);
        let reads = world.spy.queued_reads(token);
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
        .map(|n| world.spy.queued_reads(world.token(*n)))
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
        .map(|n| world.spy.queued_reads(world.token(*n)))
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
                self.clocks.fire(*display, self.clock.now());
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
    assert_eq!(report.fallbacks[0].application, slow);
    let line = world
        .diagnostics()
        .into_iter()
        .find(|line| line.starts_with("animation: "))
        .expect("the fallback is reported");
    assert!(
        line.starts_with("animation: App 2 (com.test.app2) moves without animation for resizes: ")
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
        "animation: App 1 (com.test.app1) moves without animation for moves and resizes: turns \
         Enhanced UI back on"
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
    let application = world.token(nodes[0]).application();
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
        // The three windows of one layout change are one record.
        let records = world.runtime.animation().shared().book().records(application);
        assert_eq!(
            records.len(),
            round + 1,
            "round {round} ({layout}): one record per layout change: {records:?}"
        );
        // The rate and end lag are wall-clock measurements of real sleeps, so a
        // loaded machine can really make this application miss the thresholds.
        // While the measurements are healthy (they are at these write costs on
        // an idle machine), it must keep animating; once they are not, the
        // only correct outcome is the fallback they call for, and the rounds
        // after it move directly by design. The thresholds themselves are
        // pinned with a fake clock in `animation::fallback_tests`.
        let bad = records.iter().rev().take(3).filter(|r| r.rate_bad() || r.lag_bad()).count();
        if bad >= 2 {
            assert!(
                matches!(
                    report.fallbacks.as_slice(),
                    [crate::animation::FallbackEntry {
                        application: app,
                        reason: FallbackReason::BelowMinimumRate | FallbackReason::LateEnd,
                        ..
                    }] if *app == application
                ),
                "round {round} ({layout}): measured too slow, so in fallback: {report:?}"
            );
            // Slow because pending frames were replaced, never because every
            // frame was queued: at these write costs a correct sink always
            // drops frames, and more so under load.
            let dropped: usize = report.metrics.iter().map(|(_, m)| m.dropped).sum();
            assert!(
                dropped > 0,
                "round {round} ({layout}): slow without dropped frames: {report:?}"
            );
            return;
        }
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

/// The real time plus a jump a test sets: the actor and the animation worker
/// run in real time, and a cool-down passes in one step.
#[derive(Default)]
struct JumpClock {
    jump: Mutex<Duration>,
}

impl crate::actor::Clock for JumpClock {
    fn now(&self) -> Instant { Instant::now() + *lock(&self.jump) }
}

#[derive(Default)]
struct CaptureLog {
    lines: Mutex<Vec<String>>,
}

impl stache_sdk::Logger for CaptureLog {
    fn log(&self, record: stache_sdk::LogRecord) { lock(&self.lines).push(record.message); }
}

impl CaptureLog {
    fn has(&self, part: &str) -> bool { lock(&self.lines).iter().any(|line| line.contains(part)) }
}

/// Through the real actor: an application whose writes are slow enters the
/// fallback for resizes (logged, listed by `query status` with a retry time);
/// once its cool-down passed on the actor clock the next input ends it (logged),
/// status no longer lists it and its next resize animates again.
#[test]
fn a_fallback_is_listed_logged_and_ends_after_its_cool_down() {
    let clock = Arc::new(JumpClock::default());
    let log = Arc::new(CaptureLog::default());
    let logger = stache_sdk::ScopedLogger::new(
        Arc::clone(&log) as Arc<dyn stache_sdk::Logger>,
        stache_sdk::ModuleId::new("tiling"),
        stache_sdk::Operation::Start,
    );
    let mut world = World::boot_with(
        &[1],
        one_workspace(0),
        1,
        FakeFrameClockFactory::new(),
        one_display(),
        &[],
        Some((Arc::clone(&clock) as Arc<dyn crate::actor::Clock>, logger)),
        true,
    );
    let slow = Arc::new(AtomicBool::new(true));
    let gate = Arc::clone(&slow);
    world.ax.on_node_write(move |_| {
        if gate.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(150));
        }
    });
    world.change_layout(200);
    world.change_layout(0);
    let status = crate::ipc::status(&world.runtime.snapshot());
    let [entry] = status.animation_fallbacks.as_slice() else {
        panic!("one fallback listed: {status:?}");
    };
    assert_eq!((entry.name.as_str(), entry.kind.as_str()), ("App 1", "resize"));
    assert!(
        entry.reason.ends_with("in 2 of its last 3 layout changes"),
        "{entry:?}"
    );
    assert!(
        entry.retry_in_ms.is_some_and(|ms| ms > 0 && ms <= 60_000),
        "a retry time: {entry:?}"
    );
    assert!(log.has(
        "animation: App 1 (com.test.app1) moves without animation for resizes (until a cool-down \
         passes): "
    ));

    // Healthy again, and the cool-down passes on the actor clock.
    slow.store(false, Ordering::SeqCst);
    *lock(&clock.jump) += Duration::from_secs(61);
    let base = world.change_layout(200);
    assert!(
        world.stages_of(0, base).iter().any(|s| *s != FrameStage::Direct),
        "the next resize animates again"
    );
    let status = crate::ipc::status(&world.runtime.snapshot());
    assert!(status.animation_fallbacks.is_empty(), "{status:?}");
    assert!(
        log.has("animation: App 1 (com.test.app1) animates resizes again (its cool-down passed): ")
    );
}

// ------------------------------------------------- in-flight geometry reads

/// While it holds a gate, every rig write stalls in its shard until released.
type HoldSlot = Arc<Mutex<Option<Arc<Gate>>>>;

/// The write latency of the in-flight rig, cycled per application: 5 to 15 ms.
const LATENCIES_MS: [u64; 4] = [5, 10, 15, 10];

impl World {
    /// Two applications with two windows each on one 120 Hz display paced by
    /// a fake clock, with or without the window-server bounds.
    fn in_flight(bounds: bool) -> Self {
        Self::boot_with(
            &[2, 2],
            one_workspace(0),
            4,
            FakeFrameClockFactory::new(),
            DisplayLayout::new(vec![display(1, true, Some(120.0), 0.0)]),
            &[],
            None,
            bounds,
        )
    }

    /// Every native write of a window takes 5 to 15 ms on its application's
    /// shard (so writes are serialized per application) and then notifies
    /// `AXWindowMoved`, like a real application echoing Stache's own frames.
    /// Returns the number of echoes sent so far and a slot that, while it
    /// holds a gate, stalls every later write in its shard until released.
    fn echo_moves_slowly(&self) -> (Arc<AtomicUsize>, HoldSlot) {
        let owners: Vec<(NodeId, i32)> = self
            .applications
            .iter()
            .flat_map(|a| a.nodes.iter().map(|n| (*n, a.info.pid)))
            .collect();
        let observers = self.observers.clone();
        let echoes = Arc::new(AtomicUsize::new(0));
        let sent = Arc::clone(&echoes);
        let turns: Arc<Mutex<BTreeMap<i32, usize>>> = Arc::default();
        let slot: HoldSlot = Arc::default();
        let held = Arc::clone(&slot);
        self.ax.on_node_write(move |node| {
            if let Some((_, pid)) = owners.iter().find(|(n, _)| *n == node) {
                let gate = lock(&held).clone();
                if let Some(gate) = gate {
                    gate.pass();
                }
                let turn = *lock(&turns).entry(*pid).and_modify(|t| *t += 1).or_insert(1);
                std::thread::sleep(Duration::from_millis(LATENCIES_MS[turn % LATENCIES_MS.len()]));
                observers.inject(*pid, stache_macos::ax::AxNotification::WindowMoved, node);
                sent.fetch_add(1, Ordering::SeqCst);
            }
        });
        (echoes, slot)
    }

    /// Fires the 120 Hz clock until `done`, sampling every window's observed
    /// frame after each tick.
    fn drive_120(
        &self,
        what: &str,
        samples: &mut Vec<BTreeMap<WindowInstance, Rect>>,
        done: impl Fn(&Self) -> bool,
    ) {
        wait_until(what, || {
            self.clocks.fire(D1, Instant::now());
            // About one 120 Hz period, sampled every millisecond.
            for _ in 0..8 {
                std::thread::sleep(Duration::from_millis(1));
                samples.push(
                    self.runtime
                        .snapshot()
                        .frames
                        .iter()
                        .filter_map(|f| f.actual.map(|actual| (f.window, actual)))
                        .collect(),
                );
            }
            done(self)
        });
    }

    /// The fake AX calls to the windows and the element of application `index`
    /// since call `from`: (frame writes, every other call).
    fn ax_calls_of(&self, index: usize, from: usize) -> (usize, usize, Vec<Call>) {
        let application = &self.applications[index];
        let mine = |node: &NodeId| *node == application.element || application.nodes.contains(node);
        let calls: Vec<Call> = self.ax.calls()[from..]
            .iter()
            .filter(|call| match call {
                Call::SetTimeout(n, _)
                | Call::Position(n)
                | Call::Size(n)
                | Call::SetPosition(n, _)
                | Call::SetSize(n, _)
                | Call::IsSettable(n, _)
                | Call::Bool(n, _)
                | Call::String(n, _)
                | Call::Windows(n, _, _)
                | Call::Children(n, _)
                | Call::Tabs(n, _)
                | Call::TabSelected(n)
                | Call::WindowId(n)
                | Call::Raise(n)
                | Call::SetHidden(n, _)
                | Call::EnhancedUi(n)
                | Call::SetEnhancedUi(n, _) => mine(n),
                Call::Observe(..) | Call::Unobserve(..) => false,
            })
            .cloned()
            .collect();
        let writes = calls
            .iter()
            .filter(|c| matches!(c, Call::SetPosition(..) | Call::SetSize(..)))
            .count();
        (writes, calls.len() - writes, calls)
    }

    /// Whether `window` took an observed frame differing from both its start
    /// and its target in any sample.
    fn passed_through(
        samples: &[BTreeMap<WindowInstance, Rect>],
        window: WindowInstance,
        start: Rect,
        target: Rect,
    ) -> bool {
        samples
            .iter()
            .filter_map(|sample| sample.get(&window))
            .any(|frame| *frame != start && *frame != target)
    }
}

/// The index of the first frame of `window` that starts a write (a first or
/// a direct frame) at or after `from`.
fn first_frame(seen: &[Seen], window: WindowInstance, from: usize) -> Option<usize> {
    seen.iter()
        .enumerate()
        .skip(from)
        .find(|(_, s)| {
            matches!(s, Seen::Frame(w, FrameStage::First | FrameStage::Direct) if *w == window)
        })
        .map(|(i, _)| i)
}

fn is_snapshot_of(seen: &Seen, window: WindowInstance) -> bool {
    matches!(seen, Seen::Snapshot(w) | Seen::FullSnapshot(w) if *w == window)
}

/// One layout change in the in-flight rig: every window animates, its
/// observed frame moves through the animation, and the reads are answered by
/// the window server (`bounds`) or by Accessibility (without). Returns the
/// per-application Accessibility calls of the animation.
fn one_layout_change(bounds: bool) -> Vec<AxCount> {
    let mut world = World::in_flight(bounds);
    assert_eq!(world.runtime.window_bounds_missing(), !bounds);
    let (echoes, _) = world.echo_moves_slowly();
    let before = world.desired();
    let windows: Vec<WindowInstance> = before.keys().copied().collect();
    let seen_base = world.spy.seen().len();
    let calls_base = world.ax.calls().len();
    let echoes_base = echoes.load(Ordering::SeqCst);
    let reads_base: BTreeMap<_, _> =
        windows.iter().map(|w| (*w, world.spy.frame_reads(*w))).collect();

    world.reload(one_workspace(200));
    world.wait_replanned(&before);
    let after = world.desired();
    let mut samples = Vec::new();
    world.drive_120("the layout change", &mut samples, |w| {
        w.is_settled() && w.clocks.live() == 0
    });

    let seen = world.spy.seen();
    let mut passed = 0;
    let mut late: BTreeMap<ApplicationId, usize> = BTreeMap::new();
    for window in &windows {
        let first = first_frame(&seen, *window, seen_base).expect("the window animated");
        let settle = seen
            .iter()
            .enumerate()
            .skip(first)
            .find(|(_, s)| **s == Seen::FrameRead(*window))
            .map(|(i, _)| i)
            .expect("the settle read is frame-only");
        let snapshots = seen[first..settle].iter().filter(|s| is_snapshot_of(s, *window)).count();
        // An echo that arrives once the attempt settled reads the snapshot,
        // as for any window Stache is not moving.
        *late.entry(window.application()).or_default() +=
            seen[settle..].iter().filter(|s| is_snapshot_of(s, *window)).count();
        if bounds {
            assert_eq!(
                snapshots, 0,
                "no snapshot read between the first frame and the settle"
            );
        } else {
            assert!(snapshots >= 1, "without bounds every echo reads a snapshot");
        }
        assert_eq!(
            world.spy.frame_reads(*window) - reads_base[window],
            1,
            "exactly one frame-only read: the settle"
        );
        let moved = World::passed_through(&samples, *window, before[window], after[window]);
        passed += usize::from(moved);
        if bounds {
            assert!(
                moved,
                "the observed frame moved through the animation: {window:?}"
            );
        }
        assert_eq!(
            world.frame_of_token(*window),
            after[window],
            "it ends on its target"
        );
    }
    // Without the bounds every echo waits for a blocking snapshot on the one
    // effect thread behind the application's own writes, so a late window's
    // first echo read can land after its motion (the previous behaviour).
    assert!(passed >= 1, "an observed frame moved through the animation");
    let report = world.runtime.animation().report();
    assert!(report.fallbacks.is_empty(), "no instant fallback: {report:?}");
    // Both applications' shards emit echoes at once; their events still reach
    // the actor in sequence order, so no gap is read as a lost event.
    assert_eq!(world.runtime.ingress().stats().sequence_gaps, 0);

    let echoes = echoes.load(Ordering::SeqCst) - echoes_base;
    (0..world.applications.len())
        .map(|index| {
            let (writes, _, calls) = world.ax_calls_of(index, calls_base);
            let count = |wanted: fn(&Call) -> bool| calls.iter().filter(|c| wanted(c)).count();
            AxCount {
                writes,
                timeouts: count(|c| matches!(c, Call::SetTimeout(..))),
                listings: count(|c| matches!(c, Call::Windows(..))),
                positions: count(|c| matches!(c, Call::Position(..))),
                sizes: count(|c| matches!(c, Call::Size(..))),
                snapshot_reads: count(|c| {
                    matches!(
                        c,
                        Call::String(..)
                            | Call::Bool(..)
                            | Call::WindowId(..)
                            | Call::IsSettable(..)
                    )
                }),
                refreshes: world.spy.seen()[seen_base..]
                    .iter()
                    .filter(|s| **s == Seen::Refresh(world.applications[index].info.application))
                    .count(),
                late_snapshots: late
                    .get(&world.applications[index].info.application)
                    .copied()
                    .unwrap_or(0),
                // Echoes are not per application; the rig counts them all.
                echoes,
            }
        })
        .collect()
}

impl World {
    /// Makes every shard take a turn (a resolve visits each), so a
    /// notification injected from the test thread is drained.
    fn wake_shards(&self) {
        let _ = self.spy.resolve_window_id(u32::MAX, &CancellationToken::new(), None);
    }

    fn frame_of_token(&self, window: WindowInstance) -> Rect {
        let node = self
            .applications
            .iter()
            .flat_map(|a| a.nodes.iter())
            .copied()
            .find(|n| self.token(*n) == window)
            .expect("a node");
        self.frame_of(node)
    }
}

/// PF-07-01, PF-07-02 and PF-07-07: while Stache moves a window, its echoes
/// are answered by the window server, its observed frame still follows the
/// motion, and the Accessibility calls to an application are its frame writes
/// plus a fixed number per window, whatever the number of echoes.
#[test]
fn in_flight_echoes_read_the_window_server_and_the_accessibility_calls_do_not_grow_with_them() {
    let counts = one_layout_change(true);
    for (index, count) in counts.iter().enumerate() {
        eprintln!("bounds: application {index}: {count:?}");
        assert!(count.writes >= 4, "the application animated: {count:?}");
        // Fixed per window: the first and final frames' membership listings
        // and the settle read's timeout, `AXPosition` and `AXSize`. On a busy
        // machine an echo can be read only once its window settled; it is then
        // an ordinary snapshot (a listing, a timeout, position, size and the
        // snapshot attributes), at most one per window.
        let late = count.late_snapshots;
        assert!(late <= 2, "{count:?}");
        // Nothing re-lists the application during the animation.
        assert_eq!(count.refreshes, 0, "{count:?}");
        // Each written frame sets the messaging timeout once before its
        // position and size writes.
        assert!(count.timeouts <= count.writes + 2 + late, "{count:?}");
        assert!(count.listings <= 2 * 2 + late, "{count:?}");
        assert_eq!((count.positions, count.sizes), (2 + late, 2 + late), "{count:?}");
        // No snapshot attribute is read while the windows move, so nothing
        // follows the echoes.
        assert!(count.snapshot_reads <= late * 7, "{count:?}");
        // The application's own echoes (about half of the rig's) outnumber
        // its fixed reads, which therefore cannot be one per echo.
        assert!(count.echoes / 2 > 2 * 2 + 2 * 3, "{count:?}");
    }
}

/// The Accessibility calls one application received during one animation.
#[derive(Clone, Copy, Debug, Default)]
struct AxCount {
    /// `AXPosition` and `AXSize` writes.
    writes: usize,
    /// Messaging timeouts set.
    timeouts: usize,
    /// `AXWindows` listings.
    listings: usize,
    /// `AXPosition` reads.
    positions: usize,
    /// `AXSize` reads.
    sizes: usize,
    /// Attribute reads only a full snapshot makes (title, role, flags, id,
    /// settable), summed.
    snapshot_reads: usize,
    /// Snapshot reads after a window's settle read (late echoes of a window
    /// no longer in flight).
    late_snapshots: usize,
    /// Re-listings of the application (a reconciliation).
    refreshes: usize,
    /// Echoes the rig sent during the animation (both applications).
    echoes: usize,
}

/// PF-07-04 (first half): without the bounds the same layout change still
/// moves the observed frame through the animation, with a snapshot per echo.
#[test]
fn without_bounds_echoes_read_snapshots_and_the_observed_frame_still_follows() {
    let counts = one_layout_change(false);
    for (index, count) in counts.iter().enumerate() {
        eprintln!("no bounds: application {index}: {count:?}");
        // The previous pattern: a snapshot per echo, so the snapshot reads
        // follow the echoes.
        assert!(count.snapshot_reads > 0 && count.listings > 2 * 2, "{count:?}");
    }
}

/// An A, B, A plan: the return animates, starts from where the window is
/// (from the window server with `bounds`, else from a frame-only read) and
/// ends exactly on A. With the bounds the plan reverses while the
/// applications' shards are stalled in a write, so the running jobs are
/// retargeted (not ended first) and a read of the application could not be
/// answered until they resume. Without them the retarget needs a frame-only
/// read of the application, which a stalled shard cannot answer, so the plan
/// reverses unstalled; that read itself is pinned in
/// `animation::pipeline_tests`.
#[allow(clippy::too_many_lines)] // one scenario: out, stall, reverse, back, verify
fn reversed_in_flight(bounds: bool) {
    let world = World::in_flight(bounds);
    let (_, hold) = world.echo_moves_slowly();
    let a = world.desired();
    let windows: Vec<WindowInstance> = a.keys().copied().collect();
    // Layout intents keep the configuration revision, so a running job is
    // retargeted rather than ended by a revision change.
    let layout = |layout| {
        world
            .runtime
            .ingress()
            .submit(Input::Intent(crate::intent::Intent::SetLayout(layout)))
            .expect("intent");
    };
    let batch_base = world.spy.batches().len();
    layout(crate::config::LayoutType::Monocle);
    world.wait_replanned(&a);
    let b = world.desired();
    let mut samples = Vec::new();
    // In flight: a window seen between A and B whose job has not sent its
    // final frame yet.
    world.drive_120("a window between A and B", &mut samples, |w| {
        let finals: Vec<WindowInstance> = w.spy.batches()[batch_base..]
            .iter()
            .flat_map(|batch| batch.frames.clone())
            .filter(|(_, stage)| *stage == FrameStage::Final)
            .map(|(window, _)| window)
            .collect();
        w.runtime.snapshot().frames.iter().any(|f| {
            f.in_flight
                && !finals.contains(&f.window)
                && f.actual.is_some_and(|actual| actual != a[&f.window] && actual != b[&f.window])
        })
    });
    // With the bounds, stall the shards in their next write; then reverse.
    let gate = Arc::new(Gate::default());
    if bounds {
        *lock(&hold) = Some(Arc::clone(&gate));
        wait_until("a shard stalled in a write", || gate.entered() >= 1);
        // Both shards, unless one has nothing left to write.
        let until = Instant::now() + Duration::from_millis(300);
        while gate.entered() < 2 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    let seen_base = world.spy.seen().len();
    layout(crate::config::LayoutType::Dwindle);
    wait_until("the reversed plan", || world.desired() == a);
    let returns = |seen: &[Seen]| -> Vec<WindowInstance> {
        windows
            .iter()
            .copied()
            .filter(|w| first_frame(seen, *w, seen_base).is_some())
            .collect()
    };
    if bounds {
        // The window server answers at once: the return's first frames are
        // queued while every shard is still stalled, and nothing waits on
        // the application: a read stuck on a stalled shard holds only that
        // application's read lane.
        wait_until("every return started while stalled", || {
            returns(&world.spy.seen()).len() == windows.len()
        });
        assert!(!lock(&gate.state).1, "still stalled");
        let seen = world.spy.seen();
        assert!(
            !seen[seen_base..].iter().any(|s| matches!(s, Seen::Refresh(_))),
            "nothing re-lists an application"
        );
        for window in returns(&seen) {
            let first = first_frame(&seen, window, seen_base).expect("started");
            assert!(
                !seen[seen_base..first].contains(&Seen::FrameRead(window)),
                "no read of the application before the return"
            );
        }
    }
    *lock(&hold) = None;
    gate.release();
    world.drive_120("back at A", &mut samples, |w| {
        w.is_settled() && w.clocks.live() == 0
    });

    let seen = world.spy.seen();
    for window in &windows {
        assert_eq!(world.frame_of_token(*window), a[window], "back at A, exactly");
        let status = world
            .runtime
            .snapshot()
            .frames
            .iter()
            .find(|f| f.window == *window)
            .cloned()
            .expect("status");
        assert!(
            matches!(status.outcome, Some(crate::OperationOutcome::Applied { .. })),
            "the latest plan (A) is reported applied: {status:?}"
        );
        let first = first_frame(&seen, *window, seen_base).expect("written back");
        let read_first = seen[seen_base..first].contains(&Seen::FrameRead(*window));
        if bounds {
            assert!(!read_first, "the return starts from the window server");
        }
        let stages: Vec<FrameStage> = seen[seen_base..]
            .iter()
            .filter_map(|s| match s {
                Seen::Frame(w, stage) if w == window => Some(*stage),
                _ => None,
            })
            .collect();
        if bounds || read_first {
            assert_eq!(
                stages.last(),
                Some(&FrameStage::Final),
                "the return animated: {stages:?}"
            );
        } else {
            // Without the bounds a window whose observed frame still read A
            // when the plan reversed has no travel to animate: it is written
            // directly (the previous behaviour, as in the A, B, A test above).
            assert!(
                matches!(stages.last(), Some(FrameStage::Final | FrameStage::Direct)),
                "{stages:?}"
            );
        }
    }
    assert!(world.runtime.animation().report().fallbacks.is_empty());
    assert_eq!(world.runtime.outstanding_frames(), 0);
}

/// PF-07-03.
#[test]
fn a_plan_reversed_in_flight_starts_from_the_window_server_and_ends_on_a() {
    reversed_in_flight(true);
}

/// PF-07-04 (second half).
#[test]
fn without_bounds_a_plan_reversed_in_flight_retargets_with_a_frame_only_read() {
    reversed_in_flight(false);
}

/// PF-07-06: a user move, and an entry into native fullscreen, of a window
/// with no write in flight still read the whole snapshot and update the
/// fullscreen flag, with the bounds available.
#[test]
fn a_user_move_and_a_fullscreen_entry_without_a_write_in_flight_read_the_snapshot() {
    let world = World::in_flight(true);
    let node = world.nodes(0)[0];
    let window = world.token(node);
    let pid = world.applications[0].info.pid;
    let full_before = world.spy.full_snapshot_reads(window);
    let seen_base = world.spy.seen().len();
    assert!(world.server.is_some(), "bounds available");

    world.ax.edit(node, |n| n.position = Ok(AxPoint { x: 333.0, y: 444.0 }));
    world.observers.inject(pid, stache_macos::ax::AxNotification::WindowMoved, node);
    world.wake_shards();
    wait_until("the user move is read", || {
        world.clocks.fire(D1, Instant::now());
        std::thread::sleep(Duration::from_millis(2));
        world.spy.full_snapshot_reads(window) > full_before
    });

    // Tiling puts the window back in its slot; the fullscreen entry comes
    // once that write settled, so no write is in flight.
    let mut samples = Vec::new();
    world.drive_120("the window is back in its slot", &mut samples, |w| {
        w.is_settled() && w.clocks.live() == 0
    });
    let full_before = world.spy.full_snapshot_reads(window);
    world.ax.edit(node, |n| {
        n.position = Ok(AxPoint { x: 0.0, y: 0.0 });
        n.size = Ok(AxSize { width: 1920.0, height: 1080.0 });
        n.bools.insert(BoolAttribute::FullScreen, Ok(true));
    });
    world
        .observers
        .inject(pid, stache_macos::ax::AxNotification::WindowResized, node);
    world.wake_shards();
    wait_until("the fullscreen flag", || {
        world.clocks.fire(D1, Instant::now());
        std::thread::sleep(Duration::from_millis(2));
        world
            .runtime
            .snapshot()
            .windows
            .iter()
            .any(|w| w.token == window && w.observed.fullscreen)
    });
    assert!(world.spy.full_snapshot_reads(window) > full_before);
    // The first read of each notification was the whole snapshot (a write
    // back to the slot may follow, and its own echoes may use the bounds).
    let reads: Vec<Seen> = world.spy.seen()[seen_base..]
        .iter()
        .filter(|s| matches!(s, Seen::Snapshot(w) | Seen::FullSnapshot(w) | Seen::FrameRead(w) if *w == window))
        .copied()
        .collect();
    assert_eq!(reads.first(), Some(&Seen::FullSnapshot(window)), "{reads:?}");
}
