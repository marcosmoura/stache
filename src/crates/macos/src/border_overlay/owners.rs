//! The main-thread owner map: live overlays, a bounded pool and the apply loop.
//!
//! The logic is generic over an [`OverlayHost`] so the same code runs against
//! real panels (`native.rs`) and the recording fake (`testing.rs`).

use std::collections::BTreeMap;
use std::time::Duration;

use super::animation::{KEY_PING_PONG, KEY_ROTATE, KEY_WIDTH_GLOW, KEY_WIDTH_MASK, KEY_WIDTH_RING};
use super::{
    AnimLayer, AppearanceChange, BorderAppearance, BorderMotion, BorderPaint, BorderPlacement,
    FillSpec, HideReason, LayerAnimation, LayerOverride, LayerSpec, MAX_OVERLAYS, MAX_POOL,
    MotionRepeat, OrderingMode, OverlayCommand, OverlayFailure, OverlayHandle, OverlayKind,
    OverlayPlan, OverlayRect, OverlayScreen, Presented, RingShape, Transition, clamp_motion,
    ping_pong_animation, plan_fade, plan_overlay, plan_width, ring_at_fraction, rotation_animation,
};

/// The platform seam: everything that touches a window or a layer.
///
/// Every method runs on the main thread only. `screens()[0]` is the primary
/// screen.
pub trait OverlayHost {
    /// One overlay window with its layers.
    type Panel;

    /// Builds a hidden panel.
    ///
    /// # Errors
    /// [`OverlayFailure`] when native construction fails.
    fn create_panel(&mut self) -> Result<Self::Panel, OverlayFailure>;
    /// Starts the transaction that disables implicit animations.
    fn begin_transaction(&mut self);
    /// Commits the transaction.
    fn commit_transaction(&mut self);
    /// Attached displays, primary first.
    fn screens(&self) -> Vec<OverlayScreen>;
    /// The target's queried corner radius, when the capability exists.
    fn resolved_corner_radius(&self, target_cg_id: u32) -> Option<f64>;
    /// The per-OS fallback corner radius.
    fn fallback_corner_radius(&self) -> f64;
    /// Drops any cached radius of the target, so the next query re-reads it.
    fn forget_corner_radius(&self, _target_cg_id: u32) {}
    /// The target's window level (normal unless known otherwise).
    fn window_level(&self, target_cg_id: u32) -> isize;
    /// Places and draws the panel from a plan. Does not order it.
    fn configure(&mut self, panel: &mut Self::Panel, plan: &OverlayPlan);
    /// Moves the panel to the plan's frame origin and touches nothing else: no
    /// size, no layer. Only for a plan that differs from the configured one in
    /// the frame origin alone.
    fn translate(&mut self, panel: &mut Self::Panel, plan: &OverlayPlan);
    /// Orders the panel directly below or above the target, per `placement`.
    fn order_relative(
        &mut self,
        panel: &mut Self::Panel,
        target_cg_id: u32,
        level: isize,
        mode: OrderingMode,
        placement: BorderPlacement,
    );
    /// Orders the panel out.
    fn order_out(&mut self, panel: &mut Self::Panel);
    /// The monotonic media time in seconds (`CACurrentMediaTime`). A reading,
    /// not a timer: it is how a rebuilt animation recovers its phase.
    fn media_time(&self) -> f64;
    /// What the panel's presentation layers currently show, `None` when nothing
    /// is drawn or the colors cannot be read.
    fn presented(&self, panel: &Self::Panel) -> Option<Presented>;
    /// Adds one explicit animation inside the open transaction. The host has
    /// no completion callback, delegate or timer by design.
    fn add_animation(&mut self, panel: &mut Self::Panel, animation: &LayerAnimation);
    /// Removes one animation by key.
    fn remove_animation(&mut self, panel: &mut Self::Panel, layer: AnimLayer, key: &'static str);
    /// Overrides the model `path` of the ring layer and of the gradient mask
    /// after a layer write (part 09: a collapsed or collapsing ring).
    fn set_ring_override(&mut self, panel: &mut Self::Panel, shape: RingShape);
    /// Overrides a layer's model `hidden` and `opacity` after a layer write.
    fn set_layer_override(&mut self, panel: &mut Self::Panel, layer_override: LayerOverride);
    /// Closes the panel for good.
    fn close(&mut self, panel: Self::Panel);
    /// The panel's window server number, once it has one (`None` by default).
    fn window_number(&self, _panel: &Self::Panel) -> Option<u32> { None }
}

/// The thread-safe seam the control calls; both methods run on the main thread.
pub trait OverlayBackend: Send + Sync {
    /// Applies a merged batch in order.
    fn apply(&self, work: &OverlayWork) -> ApplyReport;
    /// Orders out and closes every overlay; returns how many were closed.
    fn release_all(&self) -> usize;
}

/// The outcome of one applied batch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApplyReport {
    /// Plain counts.
    pub summary: ApplySummary,
    /// Handles whose `Show` did not produce an overlay (cap or native failure);
    /// the control releases their reservation.
    pub dropped: Vec<OverlayHandle>,
    /// The window number of each live panel a `Show` touched, as known after
    /// the batch (the control records them for `window_number`).
    pub windows: Vec<(OverlayHandle, u32)>,
}

/// One merged batch as the main thread applies it.
#[derive(Clone, Debug, PartialEq)]
pub struct OverlayWork {
    /// Ordering mode in force.
    pub ordering: OrderingMode,
    /// The engine-wide cross-fade in force when the batch was built.
    pub transition: Option<Transition>,
    /// Applied before every command.
    pub hide_all: Option<HideReason>,
    /// The gate epoch of `hide_all` (meaningful only when it is `Some`).
    pub hide_epoch: u64,
    /// Commands in order.
    pub commands: Vec<OverlayCommand>,
}

/// What one applied batch did, as plain counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ApplySummary {
    /// Overlays shown.
    pub shown: u32,
    /// Overlays moved.
    pub moved: u32,
    /// Overlays re-ordered.
    pub ordered: u32,
    /// Overlays hidden by `Hide`.
    pub hidden: u32,
    /// Overlays released to the pool.
    pub released: u32,
    /// Panels created (not taken from the pool).
    pub created: u32,
    /// Panels taken from the pool.
    pub reused: u32,
    /// `Show` commands refused at the live cap.
    pub refused_shows: u32,
    /// Commands that failed natively.
    pub failed: u32,
    /// Animations added (motions, fades).
    pub animations_installed: u32,
    /// Motions that could not be described and are drawn statically.
    pub animation_failures: u32,
    /// Whether a `HideAll` ran.
    pub hid_all: bool,
    /// `Show` and `Order` commands dropped for an epoch older than the latest
    /// applied `HideAll`.
    pub stale_dropped: u32,
}

/// What differs between two tracks of the same motion: a change rebuilds the
/// track (keeping the phase), equal signatures leave it running.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Signature {
    Rotate {
        size: (f64, f64),
        angle: f64,
    },
    PingPong {
        from: super::BorderColor,
        to: super::BorderColor,
    },
}

/// The motion currently installed on a live, shown overlay.
struct RunningMotion {
    motion: BorderMotion,
    signature: Signature,
    /// Media time at which phase 0 happened (start minus any carried phase).
    started: f64,
    /// Whether a render-server animation is installed (false once a `once`
    /// turn is over; the model already shows the final state).
    installed: bool,
}

/// A fade in flight: the model overrides to keep re-applying until it is over.
struct ActiveFade {
    until: f64,
    overrides: Vec<LayerOverride>,
    keys: Vec<(AnimLayer, &'static str)>,
}

/// A focus width animation in flight (part 09), as plain timing data: the
/// width fraction it shows at any media time is computed, never observed.
#[derive(Clone, Copy, Debug)]
struct WidthMotion {
    from: f64,
    to: f64,
    started: f64,
    seconds: f64,
    easing: super::OverlayEasing,
}

impl WidthMotion {
    fn fraction_at(&self, now: f64) -> f64 {
        let elapsed = ((now - self.started) / self.seconds).clamp(0.0, 1.0);
        if elapsed >= 1.0 {
            return self.to;
        }
        (self.to - self.from).mul_add(self.easing.progress(elapsed), self.from)
    }
}

struct Live<P> {
    panel: P,
    target: u32,
    frame: OverlayRect,
    appearance: BorderAppearance,
    resolved_radius: Option<f64>,
    visible: bool,
    motion: Option<RunningMotion>,
    fade: Option<ActiveFade>,
    /// The plan the panel was last configured or translated to.
    plan: Option<OverlayPlan>,
    /// The model ring is at width 0 (a shrink was applied; part 09). The panel
    /// stays ordered in, drawing nothing, until the engine's `Hide`.
    collapsed: bool,
    /// The width animation in flight, if any.
    width: Option<WidthMotion>,
}

/// Live overlays keyed by handle plus the bounded pool of released panels.
pub struct OverlayOwners<H: OverlayHost> {
    host: H,
    live: BTreeMap<OverlayHandle, Live<H::Panel>>,
    pool: Vec<H::Panel>,
    transition: Option<Transition>,
    /// The gate epoch of the latest `HideAll` applied (part 05): a `Show` or
    /// `Order` computed under an older epoch is dropped.
    hide_epoch: u64,
}

impl<H: OverlayHost> OverlayOwners<H> {
    /// An empty owner map; no panel exists yet.
    pub const fn new(host: H) -> Self {
        Self {
            host,
            live: BTreeMap::new(),
            pool: Vec::new(),
            transition: None,
            hide_epoch: 0,
        }
    }

    /// The host (fakes read their records through it).
    pub const fn host(&self) -> &H { &self.host }

    /// Overlays currently owned by a handle.
    #[must_use]
    pub fn live_len(&self) -> usize { self.live.len() }

    /// Overlays currently ordered in.
    #[must_use]
    pub fn visible_len(&self) -> usize { self.live.values().filter(|live| live.visible).count() }

    /// Released panels kept for reuse.
    #[must_use]
    pub const fn pool_len(&self) -> usize { self.pool.len() }

    /// Applies a merged batch in order inside one transaction.
    pub fn apply(&mut self, work: &OverlayWork) -> ApplyReport {
        let mut report = ApplyReport::default();
        self.transition = work.transition;
        self.host.begin_transaction();
        if work.hide_all.is_some() {
            report.summary.hid_all = true;
            self.hide_epoch = self.hide_epoch.max(work.hide_epoch);
            for live in self.live.values_mut().filter(|live| live.visible) {
                Self::stop_animations(&mut self.host, live);
                self.host.order_out(&mut live.panel);
                live.visible = false;
            }
        }
        for command in &work.commands {
            self.command(command, work.ordering, &mut report);
        }
        self.host.commit_transaction();
        report
    }

    fn command(
        &mut self,
        command: &OverlayCommand,
        ordering: OrderingMode,
        report: &mut ApplyReport,
    ) {
        let handle = command.handle;
        // A command computed before the latest applied `HideAll` must never
        // order a window in. The reservation stays: the caller re-shows the
        // same handle once its gate reopens.
        if command.epoch < self.hide_epoch
            && matches!(
                command.kind,
                OverlayKind::Show { .. } | OverlayKind::Order { .. }
            )
        {
            report.summary.stale_dropped += 1;
            return;
        }
        match command.kind {
            OverlayKind::Show {
                target_cg_id,
                frame,
                appearance,
                change,
            } => {
                // A shrink of a border that is not drawn: no panel, no show.
                if change == AppearanceChange::Shrink && !self.live.contains_key(&handle) {
                    return;
                }
                self.show(
                    handle,
                    (target_cg_id, frame, appearance, change),
                    ordering,
                    report,
                );
                if let Some(live) = self.live.get(&handle)
                    && let Some(number) = self.host.window_number(&live.panel)
                {
                    report.windows.push((handle, number));
                }
            }
            OverlayKind::Move { frame } => {
                if let Some(live) = self.live.get_mut(&handle) {
                    live.frame = frame;
                    if !Self::translate(&mut self.host, live) {
                        Self::redraw(&mut self.host, live, report);
                    }
                    report.summary.moved += 1;
                }
            }
            OverlayKind::Order { target_cg_id } => {
                if let Some(live) = self.live.get_mut(&handle) {
                    live.target = target_cg_id;
                    // A radius can change (fullscreen, zoom): Order re-queries.
                    self.host.forget_corner_radius(target_cg_id);
                    let resolved = self.host.resolved_corner_radius(target_cg_id);
                    let reshaped = resolved != live.resolved_radius;
                    live.resolved_radius = resolved;
                    if live.visible {
                        // Only a changed radius re-plans: an `Order` is a pure
                        // re-stack and must not rewrite the layers.
                        if reshaped {
                            Self::redraw(&mut self.host, live, report);
                        }
                        // `orderWindow:relativeTo:` on a visible panel only:
                        // never ordered out and in (no visible hide).
                        let level = self.host.window_level(target_cg_id);
                        let placement = live.appearance.placement;
                        self.host.order_relative(
                            &mut live.panel,
                            target_cg_id,
                            level,
                            ordering,
                            placement,
                        );
                    }
                    report.summary.ordered += 1;
                }
            }
            OverlayKind::Hide => {
                if let Some(live) = self.live.get_mut(&handle) {
                    Self::stop_animations(&mut self.host, live);
                    if live.visible {
                        self.host.order_out(&mut live.panel);
                        live.visible = false;
                    }
                    report.summary.hidden += 1;
                }
            }
            OverlayKind::Release => {
                if let Some(mut live) = self.live.remove(&handle) {
                    self.host.forget_corner_radius(live.target);
                    Self::stop_animations(&mut self.host, &mut live);
                    self.host.order_out(&mut live.panel);
                    self.recycle(live.panel);
                    report.summary.released += 1;
                }
            }
        }
    }

    /// A `Show` orders the panel only when it is ordered in now, retargeted or
    /// re-placed: re-ordering a visible panel on every focus recolor fought
    /// the application's own raise (`above` blinked). Re-stacking is `Order`.
    fn show(
        &mut self,
        handle: OverlayHandle,
        (target, frame, appearance, change): (u32, OverlayRect, BorderAppearance, AppearanceChange),
        ordering: OrderingMode,
        report: &mut ApplyReport,
    ) {
        if !self.live.contains_key(&handle) {
            if self.live.len() >= MAX_OVERLAYS {
                report.summary.refused_shows += 1;
                report.dropped.push(handle);
                return;
            }
            // A new handle (a window id may be reused): re-read the radius.
            self.host.forget_corner_radius(target);
            let panel = if let Some(panel) = self.pool.pop() {
                report.summary.reused += 1;
                panel
            } else {
                let Ok(panel) = self.host.create_panel() else {
                    report.summary.failed += 1;
                    report.dropped.push(handle);
                    return;
                };
                report.summary.created += 1;
                panel
            };
            self.live.insert(handle, Live {
                panel,
                target,
                frame,
                appearance,
                resolved_radius: None,
                visible: false,
                motion: None,
                fade: None,
                plan: None,
                collapsed: false,
                width: None,
            });
        }
        let resolved = self.host.resolved_corner_radius(target);
        let level = self.host.window_level(target);
        let transition = self.transition;
        let Some(live) = self.live.get_mut(&handle) else { return };
        let was_visible = live.visible;
        let now = self.host.media_time();
        let width_fade = transition.and_then(|fade| fade.effective().map(|d| (d, fade.easing)));
        let presented_width = if was_visible {
            Self::width_fraction(live, now)
        } else {
            0.0
        };
        if change == AppearanceChange::Shrink {
            live.target = target;
            live.frame = frame;
            live.appearance = appearance;
            live.resolved_radius = resolved;
            Self::shrink(&mut self.host, live, width_fade, presented_width, now);
            report.summary.shown += 1;
            return;
        }
        // Read before any layer write: the presentation is what the user sees now.
        let previous = (change == AppearanceChange::StateChange
            && was_visible
            && transition.is_some_and(|fade| fade.effective().is_some()))
        .then(|| self.host.presented(&live.panel))
        .flatten();
        let moved = live.target != target || live.appearance.placement != appearance.placement;
        let reorder = (!was_visible || moved).then_some(appearance.placement);
        (live.target, live.frame) = (target, frame);
        (live.appearance, live.resolved_radius) = (appearance, resolved);
        live.collapsed = false; // A later re-plan must not re-collapse it.
        let spec = Self::place(&mut self.host, live);
        if let Some(spec) = spec {
            if let Some(placement) = reorder {
                self.host.order_relative(&mut live.panel, target, level, ordering, placement);
            }
            live.visible = true;
            // A reload or a new state ends any running fade at once.
            Self::clear_fades(&mut self.host, live);
            Self::clear_width(&mut self.host, live);
            let restart = !was_visible
                || matches!(change, AppearanceChange::StateChange | AppearanceChange::Grow);
            Self::sync_motion(&mut self.host, live, &spec, restart, now, report);
            if let (Some(previous), Some(transition)) = (previous, transition) {
                Self::start_fade(&mut self.host, live, previous, &spec, transition, now, report);
            }
            if change == AppearanceChange::Grow
                && let Some((duration, easing)) = width_fade
                && presented_width < 1.0
            {
                Self::start_width(
                    &mut self.host,
                    live,
                    presented_width,
                    1.0,
                    (duration, easing),
                    now,
                    report,
                );
            }
        } else {
            // Width 0 draws nothing: the overlay is ordered out, not transparent.
            Self::stop_animations(&mut self.host, live);
            if live.visible {
                self.host.order_out(&mut live.panel);
            }
            live.visible = false;
        }
        report.summary.shown += 1;
    }

    /// Re-plans and configures the panel; returns the layer tree when anything is drawn.
    fn place(host: &mut H, live: &mut Live<H::Panel>) -> Option<LayerSpec> {
        let screens = host.screens();
        let plan = plan_overlay(
            live.frame,
            &live.appearance,
            live.resolved_radius,
            host.fallback_corner_radius(),
            &screens,
        );
        host.configure(&mut live.panel, &plan);
        live.plan = Some(plan);
        plan.visible.then(|| plan.layer_spec())
    }

    /// A `Move` that only translates the target (same size, same display scale,
    /// same radii and paint) sets the panel's frame origin and nothing else: the
    /// layers, their animations and any fade in flight are untouched, so there
    /// is nothing to rebuild or restore. Returns whether it did; any other move
    /// takes the full re-plan.
    fn translate(host: &mut H, live: &mut Live<H::Panel>) -> bool {
        let Some(previous) = live.plan else { return false };
        let screens = host.screens();
        let plan = plan_overlay(
            live.frame,
            &live.appearance,
            live.resolved_radius,
            host.fallback_corner_radius(),
            &screens,
        );
        let same_but_origin = OverlayPlan {
            panel_frame: OverlayRect {
                x: previous.panel_frame.x,
                y: previous.panel_frame.y,
                ..plan.panel_frame
            },
            ..plan
        } == previous;
        if !same_but_origin {
            return false;
        }
        host.translate(&mut live.panel, &plan);
        live.plan = Some(plan);
        true
    }

    /// `place` for a `Move` or `Order`: a shown overlay keeps its animations.
    /// A resized gradient rebuilds the rotation track keeping the phase, and a
    /// fade still in flight gets its model overrides back (the layer write
    /// reset them).
    fn redraw(host: &mut H, live: &mut Live<H::Panel>, report: &mut ApplyReport) {
        let previous = live.plan;
        let spec = Self::place(host, live);
        if !live.visible {
            return;
        }
        let Some(spec) = spec else { return };
        // A width animation's rings were planned for the old frame: when the
        // geometry changed, the re-planned border snaps to the end of it
        // (documented, part 09). An unchanged plan (an `Order` with the same
        // geometry) keeps the running track.
        if previous != live.plan {
            Self::clear_width(host, live);
        }
        if live.collapsed {
            Self::apply_collapsed(host, live);
        }
        let now = host.media_time();
        Self::sync_motion(host, live, &spec, false, now, report);
        if let Some(fade) = live.fade.as_ref().filter(|fade| fade.until > now) {
            for layer_override in fade.overrides.clone() {
                host.set_layer_override(&mut live.panel, layer_override);
            }
        }
    }

    fn desired_motion(
        live: &Live<H::Panel>,
        spec: &LayerSpec,
    ) -> Option<(BorderMotion, Signature)> {
        let motion = live.appearance.motion?;
        let (BorderPaint::Gradient { angle, .. }, FillSpec::Gradient { from, to, .. }) =
            (live.appearance.paint, spec.fill)
        else {
            return None;
        };
        let signature = match motion {
            BorderMotion::Rotate { .. } => Signature::Rotate {
                size: (spec.ring_frame.width, spec.ring_frame.height),
                angle,
            },
            BorderMotion::PingPong { .. } => Signature::PingPong { from, to },
        };
        Some((motion, signature))
    }

    /// Brings the installed motion in line with the appearance.
    ///
    /// `restart` starts from phase 0 (first show, state change). Otherwise an
    /// unchanged motion keeps running, and one whose track must change (a
    /// resize, a ping-pong recolor) is rebuilt resuming from the elapsed phase.
    fn sync_motion(
        host: &mut H,
        live: &mut Live<H::Panel>,
        spec: &LayerSpec,
        restart: bool,
        now: f64,
        report: &mut ApplyReport,
    ) {
        let Some((motion, signature)) = Self::desired_motion(live, spec) else {
            Self::clear_motion(host, live);
            return;
        };
        if !(spec.inner.width > 0.0 && spec.inner.height > 0.0) {
            // A zero-size target has no direction to turn or colors worth
            // swapping: draw statically and let the control publish the diagnostic.
            Self::clear_motion(host, live);
            report.summary.animation_failures += 1;
            return;
        }
        let started = match &live.motion {
            Some(running) if !restart && running.motion == motion => {
                if running.signature == signature {
                    return;
                }
                running.started
            }
            _ => now,
        };
        Self::clear_motion(host, live);
        let elapsed = Duration::try_from_secs_f64((now - started).max(0.0)).unwrap_or_default();
        let mut running = RunningMotion {
            motion,
            signature,
            started,
            installed: false,
        };
        let animation = match motion {
            BorderMotion::Rotate { repeat, .. } => {
                if repeat == MotionRepeat::Once && elapsed >= motion.period() {
                    // The turn is over: the model already shows the final angle.
                    live.motion = Some(running);
                    return;
                }
                let phase = match repeat {
                    MotionRepeat::Once => elapsed,
                    MotionRepeat::Loop => Duration::from_secs_f64(
                        elapsed.as_secs_f64() % motion.period().as_secs_f64(),
                    ),
                };
                let Signature::Rotate { size, angle } = signature else {
                    return;
                };
                rotation_animation(size, angle, motion, phase).ok()
            }
            BorderMotion::PingPong { .. } => {
                let Signature::PingPong { from, to } = signature else {
                    return;
                };
                Some(ping_pong_animation(from, to, motion, elapsed))
            }
        };
        if let Some(animation) = animation {
            host.add_animation(&mut live.panel, &animation);
            report.summary.animations_installed += 1;
            running.installed = true;
            live.motion = Some(running);
        } else {
            report.summary.animation_failures += 1;
        }
    }

    fn clear_motion(host: &mut H, live: &mut Live<H::Panel>) {
        if let Some(running) = live.motion.take().filter(|running| running.installed) {
            let key = match running.motion {
                BorderMotion::Rotate { .. } => KEY_ROTATE,
                BorderMotion::PingPong { .. } => KEY_PING_PONG,
            };
            host.remove_animation(&mut live.panel, AnimLayer::Gradient, key);
        }
    }

    fn clear_fades(host: &mut H, live: &mut Live<H::Panel>) {
        if let Some(fade) = live.fade.take() {
            for (layer, key) in fade.keys {
                host.remove_animation(&mut live.panel, layer, key);
            }
        }
    }

    /// Removes every animation: a hidden or released border carries none.
    fn stop_animations(host: &mut H, live: &mut Live<H::Panel>) {
        Self::clear_fades(host, live);
        Self::clear_motion(host, live);
        Self::clear_width(host, live);
        // The next `Show` configures the full ring again.
        live.collapsed = false;
    }

    /// The width fraction the ring shows at `now`: computed from the width
    /// animation's timing, or 0/1 for a collapsed/full model at rest.
    fn width_fraction(live: &Live<H::Panel>, now: f64) -> f64 {
        live.width.map_or(if live.collapsed { 0.0 } else { 1.0 }, |motion| {
            motion.fraction_at(now)
        })
    }

    fn clear_width(host: &mut H, live: &mut Live<H::Panel>) {
        if live.width.take().is_some() {
            host.remove_animation(&mut live.panel, AnimLayer::Ring, KEY_WIDTH_RING);
            host.remove_animation(&mut live.panel, AnimLayer::GradientMask, KEY_WIDTH_MASK);
            host.remove_animation(&mut live.panel, AnimLayer::Glow, KEY_WIDTH_GLOW);
        }
    }

    fn ring_at(host: &H, live: &Live<H::Panel>, fraction: f64) -> Option<RingShape> {
        let plan = live.plan?;
        let screens = host.screens();
        Some(ring_at_fraction(
            &plan,
            live.frame,
            &live.appearance,
            (live.resolved_radius, host.fallback_corner_radius()),
            &screens,
            fraction,
        ))
    }

    /// Sets the model to the zero-width ring (and an invisible halo).
    fn apply_collapsed(host: &mut H, live: &mut Live<H::Panel>) {
        if let Some(shape) = Self::ring_at(host, live, 0.0) {
            host.set_ring_override(&mut live.panel, shape);
        }
        if matches!(live.appearance.paint, BorderPaint::Glow(_)) {
            host.set_layer_override(&mut live.panel, LayerOverride {
                layer: AnimLayer::Glow,
                hidden: false,
                opacity: 0.0,
            });
        }
    }

    /// Applies a `Shrink` (part 09): the ring animates from its presented
    /// width to zero and the model is set to the zero ring at once. The panel
    /// stays ordered in, drawing nothing once the animation ends, until the
    /// engine's `Hide` (or any `HideAll`, `Hide` or `Release`, which still
    /// orders it out at once). No callback observes the end. Without a
    /// transition, a hidden overlay, or nothing to draw, it hides at once.
    fn shrink(
        host: &mut H,
        live: &mut Live<H::Panel>,
        width_fade: Option<(Duration, super::OverlayEasing)>,
        presented: f64,
        now: f64,
    ) {
        if !live.visible {
            return;
        }
        let Some((duration, easing)) = width_fade else {
            Self::stop_animations(host, live);
            host.order_out(&mut live.panel);
            live.visible = false;
            return;
        };
        Self::clear_fades(host, live);
        Self::clear_width(host, live);
        if Self::place(host, live).is_none() {
            Self::stop_animations(host, live);
            host.order_out(&mut live.panel);
            live.visible = false;
            return;
        }
        live.collapsed = true;
        Self::apply_collapsed(host, live);
        if presented > 0.0 {
            let mut report = ApplyReport::default();
            Self::start_width(host, live, presented, 0.0, (duration, easing), now, &mut report);
        }
    }

    /// Installs the width animation from `from` to `to` (fractions). The
    /// duration scales with the distance, so an interrupted change keeps the
    /// configured speed.
    fn start_width(
        host: &mut H,
        live: &mut Live<H::Panel>,
        from: f64,
        to: f64,
        (duration, easing): (Duration, super::OverlayEasing),
        now: f64,
        report: &mut ApplyReport,
    ) {
        let (Some(start), Some(end)) =
            (Self::ring_at(host, live, from), Self::ring_at(host, live, to))
        else {
            return;
        };
        let seconds =
            clamp_motion(duration.mul_f64((to - from).abs().clamp(0.0, 1.0))).as_secs_f64();
        let glow = matches!(live.appearance.paint, BorderPaint::Glow(_)).then_some((from, to));
        let plan = plan_width(start, end, glow, seconds, easing);
        for layer_override in &plan.overrides {
            host.set_layer_override(&mut live.panel, *layer_override);
        }
        for animation in &plan.animations {
            host.add_animation(&mut live.panel, animation);
            report.summary.animations_installed += 1;
        }
        live.width = Some(WidthMotion {
            from,
            to,
            started: now,
            seconds,
            easing,
        });
    }

    fn start_fade(
        host: &mut H,
        live: &mut Live<H::Panel>,
        previous: Presented,
        spec: &LayerSpec,
        transition: Transition,
        now: f64,
        report: &mut ApplyReport,
    ) {
        let Some(duration) = transition.effective() else { return };
        // A ping-pong owns the gradient colors, so no color fade runs against it.
        let owns_colors = matches!(
            live.motion,
            Some(RunningMotion {
                motion: BorderMotion::PingPong { .. },
                installed: true,
                ..
            })
        );
        let plan = plan_fade(previous, &spec.fill, duration, transition.easing, owns_colors);
        if plan.animations.is_empty() {
            return;
        }
        for layer_override in &plan.overrides {
            host.set_layer_override(&mut live.panel, *layer_override);
        }
        let mut keys = Vec::with_capacity(plan.animations.len());
        for animation in &plan.animations {
            host.add_animation(&mut live.panel, animation);
            keys.push((animation.layer, animation.key));
            report.summary.animations_installed += 1;
        }
        live.fade = Some(ActiveFade {
            until: now + duration.as_secs_f64(),
            overrides: plan.overrides,
            keys,
        });
    }

    fn recycle(&mut self, panel: H::Panel) {
        if self.pool.len() < MAX_POOL {
            self.pool.push(panel);
        } else {
            self.host.close(panel);
        }
    }

    /// Orders out and closes every live and pooled overlay; returns how many.
    pub fn release_all(&mut self) -> usize {
        // A later engine starts again at gate epoch 0.
        self.hide_epoch = 0;
        let mut closed = 0;
        for (_, mut live) in std::mem::take(&mut self.live) {
            self.host.order_out(&mut live.panel);
            self.host.close(live.panel);
            closed += 1;
        }
        for panel in std::mem::take(&mut self.pool) {
            self.host.close(panel);
            closed += 1;
        }
        closed
    }
}
