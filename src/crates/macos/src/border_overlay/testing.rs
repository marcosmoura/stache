//! Fakes for the overlay engine: no window, layer or `SkyLight` is created.
//!
//! [`FakeOverlayBackend`] runs the real [`OverlayOwners`] logic against a host
//! that only records. Pair it with `ManualExecutor` (a queued executor that
//! holds jobs until a test drains them).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, ThreadId};

use super::spaces::{
    MAX_SPACE_FACTS, PushOutcome, SpaceEvents, SpaceFact, SpaceSink, UnknownCause,
};
use super::{
    AnimLayer, AnimRepeat, ApplyReport, BorderPlacement, CornerRadiusResolver, FillSpec,
    GlowRender, LayerAnimation, LayerOverride, LayerSpec, OrderingMode, OverlayBackend,
    OverlayFailure, OverlayHost, OverlayOwners, OverlayPlan, OverlayRect, OverlayScreen,
    OverlayWork, Presented, RingShape, StopOutcome,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One recorded native call.
#[allow(
    clippy::large_enum_variant,
    reason = "a test recorder; plans are plain data"
)]
#[derive(Clone, Debug, PartialEq)]
pub enum FakeEvent {
    /// A panel was created.
    Created(u64),
    /// The transaction began.
    Begin,
    /// The transaction committed.
    Commit,
    /// A panel was configured from a plan.
    Configure(u64, OverlayPlan),
    /// A panel's frame origin was set from a plan (layers untouched).
    Translate(u64, OverlayPlan),
    /// A panel was ordered relative to a target.
    OrderRelative {
        /// Panel id.
        panel: u64,
        /// Target `CGWindowID`.
        target: u32,
        /// Level used.
        level: isize,
        /// Ordering mode used.
        mode: OrderingMode,
        /// Below or above the target.
        placement: BorderPlacement,
    },
    /// A panel was ordered out.
    OrderOut(u64),
    /// An animation was added to a layer.
    AddAnimation {
        /// Panel id.
        panel: u64,
        /// The full description.
        animation: Box<LayerAnimation>,
    },
    /// An animation was removed by key.
    RemoveAnimation {
        /// Panel id.
        panel: u64,
        /// Layer.
        layer: AnimLayer,
        /// Key.
        key: &'static str,
    },
    /// The model ring path of the ring layer and gradient mask was overridden.
    RingOverride(u64, RingShape),
    /// A layer's model hidden/opacity was overridden.
    Override(u64, LayerOverride),
    /// A panel was closed.
    Close(u64),
}

/// A fake panel: only an id.
#[derive(Debug)]
pub struct FakePanel {
    id: u64,
}

#[derive(Default)]
struct Records {
    events: Vec<FakeEvent>,
    open: BTreeSet<u64>,
    ordered_in: BTreeSet<u64>,
    /// The recorded layer tree of each open panel.
    layers: BTreeMap<u64, LayerSpec>,
    /// Layer property writes made outside a transaction with implicit actions
    /// disabled (each would animate on a real layer).
    implicit_animations: u32,
    in_transaction: bool,
    threads: Vec<ThreadId>,
    /// Animations currently attached, by panel, layer and key.
    animations: BTreeMap<(u64, AnimLayer, &'static str), LayerAnimation>,
    /// Model overrides in force (a layer write resets a panel's).
    overrides: BTreeMap<(u64, AnimLayer), LayerOverride>,
    /// Ring path overrides in force (a layer write resets a panel's).
    ring_overrides: BTreeMap<u64, RingShape>,
    /// Scripted presentation values, consumed by the next read of a panel.
    scripted: BTreeMap<u64, Presented>,
    /// The scripted media clock, seconds.
    clock: f64,
    /// Animation adds and removals made outside the disabled-actions transaction.
    untransacted_animation_calls: u32,
    /// The glow bitmap each panel last rendered (kept while the fill is not a
    /// glow, like the native cached contents).
    glow_rendered: BTreeMap<u64, GlowRender>,
    /// Every glow bitmap render, in order.
    glow_renders: Vec<(u64, GlowRender)>,
}

/// A resolver with a fixed answer.
#[derive(Clone, Copy, Debug)]
pub struct FakeRadiusResolver(pub Option<f64>);

impl CornerRadiusResolver for FakeRadiusResolver {
    fn corner_radius(&self, _target_cg_id: u32) -> Option<f64> { self.0 }
}

/// The recording host.
pub struct FakeHost {
    records: Arc<Mutex<Records>>,
    next_panel: u64,
    screens: Vec<OverlayScreen>,
    resolver: FakeRadiusResolver,
    fail_create: Arc<AtomicBool>,
}

impl OverlayHost for FakeHost {
    type Panel = FakePanel;

    fn create_panel(&mut self) -> Result<FakePanel, OverlayFailure> {
        if self.fail_create.load(Ordering::SeqCst) {
            return Err(OverlayFailure::PanelCreation("fake"));
        }
        self.next_panel += 1;
        let id = self.next_panel;
        let mut records = lock(&self.records);
        records.open.insert(id);
        records.events.push(FakeEvent::Created(id));
        drop(records);
        Ok(FakePanel { id })
    }

    fn begin_transaction(&mut self) {
        let mut records = lock(&self.records);
        records.in_transaction = true;
        records.events.push(FakeEvent::Begin);
    }

    fn commit_transaction(&mut self) {
        let mut records = lock(&self.records);
        records.in_transaction = false;
        records.events.push(FakeEvent::Commit);
    }

    fn screens(&self) -> Vec<OverlayScreen> { self.screens.clone() }

    fn resolved_corner_radius(&self, target_cg_id: u32) -> Option<f64> {
        self.resolver.corner_radius(target_cg_id)
    }

    fn fallback_corner_radius(&self) -> f64 { 16.0 }

    fn window_level(&self, _target_cg_id: u32) -> isize { 0 }

    fn configure(&mut self, panel: &mut FakePanel, plan: &OverlayPlan) {
        let mut records = lock(&self.records);
        records.overrides.retain(|(id, _), _| *id != panel.id);
        records.ring_overrides.remove(&panel.id);
        if !records.in_transaction {
            records.implicit_animations += 1;
        }
        let spec = plan.layer_spec();
        if let Some(render) = spec.glow_render()
            && records.glow_rendered.get(&panel.id) != Some(&render)
        {
            records.glow_rendered.insert(panel.id, render);
            records.glow_renders.push((panel.id, render));
        }
        records.layers.insert(panel.id, spec);
        records.events.push(FakeEvent::Configure(panel.id, *plan));
    }

    fn translate(&mut self, panel: &mut FakePanel, plan: &OverlayPlan) {
        lock(&self.records).events.push(FakeEvent::Translate(panel.id, *plan));
    }

    fn order_relative(
        &mut self,
        panel: &mut FakePanel,
        target_cg_id: u32,
        level: isize,
        mode: OrderingMode,
        placement: BorderPlacement,
    ) {
        let mut records = lock(&self.records);
        records.ordered_in.insert(panel.id);
        records.events.push(FakeEvent::OrderRelative {
            panel: panel.id,
            target: target_cg_id,
            level,
            mode,
            placement,
        });
    }

    fn order_out(&mut self, panel: &mut FakePanel) {
        let mut records = lock(&self.records);
        records.ordered_in.remove(&panel.id);
        records.events.push(FakeEvent::OrderOut(panel.id));
    }

    fn media_time(&self) -> f64 { lock(&self.records).clock }

    fn presented(&self, panel: &FakePanel) -> Option<Presented> {
        let mut records = lock(&self.records);
        if let Some(scripted) = records.scripted.remove(&panel.id) {
            return Some(scripted);
        }
        match records.layers.get(&panel.id)?.fill {
            FillSpec::None => None,
            FillSpec::Solid(color) => Some(Presented::Solid(color)),
            FillSpec::Gradient { from, to, .. } => Some(Presented::Gradient(from, to)),
            FillSpec::Glow { color, .. } => Some(Presented::Glow(color)),
        }
    }

    fn add_animation(&mut self, panel: &mut FakePanel, animation: &LayerAnimation) {
        let mut records = lock(&self.records);
        if !records.in_transaction {
            records.untransacted_animation_calls += 1;
        }
        records
            .animations
            .insert((panel.id, animation.layer, animation.key), animation.clone());
        records.events.push(FakeEvent::AddAnimation {
            panel: panel.id,
            animation: Box::new(animation.clone()),
        });
    }

    fn remove_animation(&mut self, panel: &mut FakePanel, layer: AnimLayer, key: &'static str) {
        let mut records = lock(&self.records);
        if !records.in_transaction {
            records.untransacted_animation_calls += 1;
        }
        records.animations.remove(&(panel.id, layer, key));
        records.events.push(FakeEvent::RemoveAnimation { panel: panel.id, layer, key });
    }

    fn set_ring_override(&mut self, panel: &mut FakePanel, shape: RingShape) {
        let mut records = lock(&self.records);
        records.ring_overrides.insert(panel.id, shape);
        records.events.push(FakeEvent::RingOverride(panel.id, shape));
    }

    fn set_layer_override(&mut self, panel: &mut FakePanel, layer_override: LayerOverride) {
        let mut records = lock(&self.records);
        records.overrides.insert((panel.id, layer_override.layer), layer_override);
        records.events.push(FakeEvent::Override(panel.id, layer_override));
    }

    fn close(&mut self, panel: FakePanel) {
        let mut records = lock(&self.records);
        records.animations.retain(|(id, _, _), _| *id != panel.id);
        records.overrides.retain(|(id, _), _| *id != panel.id);
        records.ring_overrides.remove(&panel.id);
        records.open.remove(&panel.id);
        records.ordered_in.remove(&panel.id);
        records.layers.remove(&panel.id);
        records.glow_rendered.remove(&panel.id);
        records.events.push(FakeEvent::Close(panel.id));
    }
}

/// The fake backend: real owner-map logic, recorded native calls.
pub struct FakeOverlayBackend {
    owners: Mutex<OverlayOwners<FakeHost>>,
    records: Arc<Mutex<Records>>,
    fail_create: Arc<AtomicBool>,
}

impl FakeOverlayBackend {
    /// A backend over `screens` (primary first) with a corner radius answer.
    #[must_use]
    pub fn new(screens: Vec<OverlayScreen>, radius: Option<f64>) -> Self {
        let records = Arc::new(Mutex::new(Records::default()));
        let fail_create = Arc::new(AtomicBool::new(false));
        let host = FakeHost {
            records: Arc::clone(&records),
            next_panel: 0,
            screens,
            resolver: FakeRadiusResolver(radius),
            fail_create: Arc::clone(&fail_create),
        };
        Self {
            owners: Mutex::new(OverlayOwners::new(host)),
            records,
            fail_create,
        }
    }

    /// A backend with one 1000x800 display at scale 2.
    #[must_use]
    pub fn single_display() -> Self {
        Self::new(
            vec![OverlayScreen {
                frame: OverlayRect::new(0.0, 0.0, 1000.0, 800.0),
                scale: 2.0,
            }],
            None,
        )
    }

    /// Makes panel creation fail (or succeed again).
    pub fn fail_creation(&self, fail: bool) { self.fail_create.store(fail, Ordering::SeqCst); }

    /// Every recorded native call, in order.
    #[must_use]
    pub fn events(&self) -> Vec<FakeEvent> { lock(&self.records).events.clone() }

    /// The number of recorded native calls.
    #[must_use]
    pub fn event_count(&self) -> usize { lock(&self.records).events.len() }

    /// Panels created and not yet closed (live plus pooled).
    #[must_use]
    pub fn open_panels(&self) -> usize { lock(&self.records).open.len() }

    /// Panels currently ordered in.
    #[must_use]
    pub fn ordered_in(&self) -> usize { lock(&self.records).ordered_in.len() }

    /// Overlays in the owner map.
    #[must_use]
    pub fn live_overlays(&self) -> usize { lock(&self.owners).live_len() }

    /// Released panels in the pool.
    #[must_use]
    pub fn pool_len(&self) -> usize { lock(&self.owners).pool_len() }

    /// The layer tree last applied to a panel (`None` before its first draw or
    /// after it closed). Panel ids count from 1 in creation order.
    #[must_use]
    pub fn layer_tree(&self, panel: u64) -> Option<LayerSpec> {
        lock(&self.records).layers.get(&panel).copied()
    }

    /// The layers a panel's last layer tree shows (empty before its first draw).
    #[must_use]
    pub fn shown_layers(&self, panel: u64) -> Vec<AnimLayer> {
        lock(&self.records)
            .layers
            .get(&panel)
            .map(|spec| spec.shown_layers().to_vec())
            .unwrap_or_default()
    }

    /// The glow bitmaps a panel rendered, in order (one entry per re-render).
    #[must_use]
    pub fn glow_renders(&self, panel: u64) -> Vec<GlowRender> {
        lock(&self.records)
            .glow_renders
            .iter()
            .filter(|(id, _)| *id == panel)
            .map(|(_, render)| *render)
            .collect()
    }

    /// Panels ever created (pool reuse does not count).
    #[must_use]
    pub fn panels_created(&self) -> usize {
        lock(&self.records)
            .events
            .iter()
            .filter(|event| matches!(event, FakeEvent::Created(_)))
            .count()
    }

    /// Layer writes that happened with implicit actions enabled; always 0 when
    /// every change goes through the disabled-actions transaction.
    #[must_use]
    pub fn implicit_animations(&self) -> u32 { lock(&self.records).implicit_animations }

    /// Animations currently attached to a panel, sorted by layer then key.
    #[must_use]
    pub fn animations(&self, panel: u64) -> Vec<(AnimLayer, LayerAnimation)> {
        lock(&self.records)
            .animations
            .iter()
            .filter(|((id, _, _), _)| *id == panel)
            .map(|((_, layer, _), animation)| (*layer, animation.clone()))
            .collect()
    }

    /// The attached animation with `key` on a panel, if any.
    #[must_use]
    pub fn animation(&self, panel: u64, key: &str) -> Option<LayerAnimation> {
        self.animations(panel)
            .into_iter()
            .map(|(_, animation)| animation)
            .find(|a| a.key == key)
    }

    /// Animations added over the fake's life.
    #[must_use]
    pub fn animations_added(&self) -> usize {
        lock(&self.records)
            .events
            .iter()
            .filter(|event| matches!(event, FakeEvent::AddAnimation { .. }))
            .count()
    }

    /// Removals recorded for `key`, in order.
    #[must_use]
    pub fn removals(&self, key: &str) -> usize {
        lock(&self.records)
            .events
            .iter()
            .filter(|event| matches!(event, FakeEvent::RemoveAnimation { key: k, .. } if *k == key))
            .count()
    }

    /// The model override in force on a panel's layer.
    #[must_use]
    pub fn layer_override(&self, panel: u64, layer: AnimLayer) -> Option<LayerOverride> {
        lock(&self.records).overrides.get(&(panel, layer)).copied()
    }

    /// The model ring path override in force on a panel (a collapsed ring).
    #[must_use]
    pub fn ring_override(&self, panel: u64) -> Option<RingShape> {
        lock(&self.records).ring_overrides.get(&panel).copied()
    }

    /// Whether a panel is ordered in.
    #[must_use]
    pub fn is_ordered_in(&self, panel: u64) -> bool {
        lock(&self.records).ordered_in.contains(&panel)
    }

    /// Scripts what the next presentation read of `panel` returns.
    pub fn script_presentation(&self, panel: u64, presented: Presented) {
        lock(&self.records).scripted.insert(panel, presented);
    }

    /// Sets the media clock (seconds).
    pub fn set_media_time(&self, seconds: f64) { lock(&self.records).clock = seconds; }

    /// Advances the media clock. Nothing is scheduled by it: a fake clock only
    /// answers the reads the engine makes.
    pub fn advance_media_time(&self, seconds: f64) { lock(&self.records).clock += seconds; }

    /// Timers, display links and completion callbacks scheduled for any
    /// animation. The host seam has no way to schedule one (no method takes a
    /// callback, delegate or interval), so this is structurally zero; tests
    /// pair it with an empty executor queue and a stable event count.
    #[must_use]
    pub const fn scheduled_callbacks(&self) -> usize { 0 }

    /// Animation add/remove calls made outside the disabled-actions transaction.
    #[must_use]
    pub fn untransacted_animation_calls(&self) -> u32 {
        lock(&self.records).untransacted_animation_calls
    }

    /// Whether any attached animation repeats forever.
    #[must_use]
    pub fn forever_animations(&self) -> usize {
        lock(&self.records)
            .animations
            .values()
            .filter(|animation| animation.repeat == AnimRepeat::Forever)
            .count()
    }

    /// The threads native code ran on.
    #[must_use]
    pub fn apply_threads(&self) -> Vec<ThreadId> { lock(&self.records).threads.clone() }
}

impl OverlayBackend for FakeOverlayBackend {
    fn apply(&self, work: &OverlayWork) -> ApplyReport {
        lock(&self.records).threads.push(thread::current().id());
        lock(&self.owners).apply(work)
    }

    fn release_all(&self) -> usize {
        lock(&self.records).threads.push(thread::current().id());
        lock(&self.owners).release_all()
    }
}

// ------------------------------------------------------------ space events

struct FakeSpaceState {
    sink: Option<SpaceSink>,
    starts: u32,
    stops: u32,
    resyncs: u32,
    auto_register: bool,
    auto_resync: bool,
    /// While set, `stop` times out like a stuck owner (the handle is kept).
    stuck: bool,
}

/// A scriptable [`SpaceEvents`] source: no observer, symbol or thread exists.
///
/// Clones share state, so a test keeps one handle after boxing another into the
/// engine. By default `start` immediately reports "registered" and `resync`
/// immediately reports `SourceRestored(MailboxOverflow)`; [`Self::manual`]
/// turns both off.
#[derive(Clone)]
pub struct FakeSpaceEvents {
    state: Arc<Mutex<FakeSpaceState>>,
}

impl Default for FakeSpaceEvents {
    fn default() -> Self { Self::new() }
}

impl FakeSpaceEvents {
    /// Registers on start and re-syncs on request.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeSpaceState {
                sink: None,
                starts: 0,
                stops: 0,
                resyncs: 0,
                auto_register: true,
                auto_resync: true,
                stuck: false,
            })),
        }
    }

    /// Reports nothing by itself: the test drives registration and resync.
    #[must_use]
    pub fn manual() -> Self {
        let fake = Self::new();
        {
            let mut state = lock(&fake.state);
            state.auto_register = false;
            state.auto_resync = false;
        }
        fake
    }

    /// Times `start` ran.
    #[must_use]
    pub fn starts(&self) -> u32 { lock(&self.state).starts }

    /// Times `stop` ran.
    #[must_use]
    pub fn stops(&self) -> u32 { lock(&self.state).stops }

    /// While `stuck`, `stop` reports a timed out owner and keeps the source
    /// running; a later `stop` after `hold_stop(false)` succeeds (the retry).
    pub fn hold_stop(&self, stuck: bool) { lock(&self.state).stuck = stuck; }

    /// Times `resync` was requested.
    #[must_use]
    pub fn resyncs(&self) -> u32 { lock(&self.state).resyncs }

    /// Pushes one fact like a native callback would; `Closed` before `start`
    /// and after `stop`.
    #[must_use]
    pub fn inject(&self, fact: SpaceFact) -> PushOutcome {
        let sink = lock(&self.state).sink.clone();
        sink.map_or(PushOutcome::Closed, |sink| sink.push(fact))
    }

    /// Reports that every registration succeeded.
    pub fn register(&self) {
        let _ = self.inject(SpaceFact::SourceRestored(UnknownCause::SourcesNotStarted));
    }

    /// Mission Control began.
    pub fn enter_mission_control(&self) { let _ = self.inject(SpaceFact::MissionControlEntered); }

    /// Mission Control ended.
    pub fn exit_mission_control(&self) { let _ = self.inject(SpaceFact::MissionControlExited); }

    /// A Space change began on `display`.
    pub fn begin_space_change(&self, display: crate::DisplayId) {
        let _ = self.inject(SpaceFact::SpaceChangeBegan { display });
    }

    /// The Space change on `display` completed.
    pub fn complete_space_change(&self, display: crate::DisplayId) {
        let _ = self.inject(SpaceFact::SpaceChangeCompleted { display });
    }

    /// A window entered or left fullscreen.
    pub fn fullscreen(&self, window: crate::windows::WindowInstance, entering: bool) {
        let _ = self.inject(SpaceFact::FullscreenChanged { window, entering });
    }

    /// A source or registration was lost.
    pub fn lose(&self, cause: UnknownCause) { let _ = self.inject(SpaceFact::SourceLost(cause)); }

    /// A lost source works again.
    pub fn restore(&self, cause: UnknownCause) {
        let _ = self.inject(SpaceFact::SourceRestored(cause));
    }

    /// The Dock quit: its observer is gone.
    pub fn dock_quit(&self) { self.lose(UnknownCause::DockUnregistered); }

    /// The Dock relaunched and its observer was registered again.
    pub fn dock_relaunched(&self) { self.restore(UnknownCause::DockUnregistered); }

    /// Fills the mailbox past its bound with harmless duplicate facts (an exit
    /// without an enter), so the next drain reports an overflow. Returns how
    /// many were dropped.
    #[must_use]
    pub fn overflow(&self) -> usize {
        (0..=MAX_SPACE_FACTS)
            .filter(|_| self.inject(SpaceFact::MissionControlExited) == PushOutcome::Overflowed)
            .count()
    }
}

impl SpaceEvents for FakeSpaceEvents {
    fn start(&mut self, sink: SpaceSink) {
        let register = {
            let mut state = lock(&self.state);
            state.starts += 1;
            state.sink = Some(sink.clone());
            state.auto_register
        };
        if register {
            let _ = sink.push(SpaceFact::SourceRestored(UnknownCause::SourcesNotStarted));
        }
    }

    fn resync(&mut self) {
        let (sink, auto) = {
            let mut state = lock(&self.state);
            state.resyncs += 1;
            (state.sink.clone(), state.auto_resync)
        };
        if auto && let Some(sink) = sink {
            let _ = sink.push(SpaceFact::SourceRestored(UnknownCause::MailboxOverflow));
        }
    }

    fn stop(&mut self) -> StopOutcome {
        let mut state = lock(&self.state);
        state.stops += 1;
        if state.stuck {
            return StopOutcome::TimedOut {
                owner_joined: false,
                callbacks_drained: true,
            };
        }
        if state.sink.take().is_some() {
            StopOutcome::Stopped
        } else {
            StopOutcome::NotRunning
        }
    }
}
