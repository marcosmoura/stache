//! Native overlay windows for Tiling borders.
//!
//! Tiling composes this module for every border it draws. Approach
//! inspiration only, never code: the reference border tool's files
//! `src/border.c`, `src/misc/window.h` and `src/windows.c` were studied for
//! which public and private APIs exist and how overlays are ordered; the
//! implementation here is written from Apple documentation and the spec.
//!
//! # Ownership
//!
//! ```text
//! any thread
//!   └─ OverlayControl (Send + Sync)
//!        ├─ Arc<dyn MainThreadExecutor>   dispatch only; main never waits
//!        ├─ Arc<dyn OverlayBackend>       SystemOverlayBackend is a unit-like struct
//!        └─ plain state: one pending batch, reserved handles, counters, capabilities
//!
//! main thread only
//!   └─ OverlayOwners<Host>                <- live overlays + bounded pool
//!        native: thread-local, panels are NSPanel + CAShapeLayer
//!        tests:  FakeOverlayBackend, FakePanel
//! ```
//!
//! No Cocoa value crosses a thread. A control owns no panel, layer, timer or
//! observer: the first `Show` creates the first panel and [`OverlayControl::release_all`]
//! closes every one (the only teardown path).
//!
//! # Batches
//!
//! [`OverlayControl::apply`] merges the batch into the single pending batch,
//! enqueues at most one main-thread job and returns an [`OverlayTicket`]. The
//! job applies the merged batch in order inside one `CATransaction` with
//! implicit actions disabled. Merge rules per handle: the latest command wins;
//! `Show` absorbs later `Move` and `Order`; `Hide` absorbs later `Move` and
//! `Order` until the next `Show`; `Release` absorbs everything and, like
//! `Hide`, is never dropped. `HideAll` runs before every other command and is
//! never merged away. At most [`MAX_OVERLAYS`] handles are reserved; an excess
//! `Show` is refused with a typed [`RefusalReason`] and counted.
//!
//! # Layers
//!
//! Drawing is Core Animation; only the glow halo is a bitmap. Approach only (never code): the
//! reference border tool's files `src/border.c` (`border_draw`: outset frame, inner clip
//! path) and `src/misc/drawing.h` (stroke, gradient direction, glow) were
//! studied for what is drawn; the layer tree here is written from Apple's
//! `CALayer`, `CAShapeLayer` and `CAGradientLayer` documentation.
//!
//! Per overlay: a container layer holding a ring `CAShapeLayer` (even-odd
//! fill between the outer and inner edge) and a `CAGradientLayer` masked by a
//! ring-shaped `CAShapeLayer`. Solid shows the ring, gradient shows the masked
//! gradient, glow hides the ring and shows only a glow layer whose contents are
//! a `CGBitmapContext` shadow-only render of a core at the window edge: a soft
//! band of light with no crisp line ([`GlowRender`]; native run 11 saw no
//! `CALayer` shadow at all, native run 12 asked for no ring) and
//! clips the container so nothing draws inside the target frame. [`LayerSpec`] is the pure description
//! both the native host and the fake apply; appearance changes only set layer
//! properties on the same panel inside a transaction with implicit actions off.
//!
//! # Animations (part 03)
//!
//! [`BorderMotion`] and [`Transition`] are plain data; `animation.rs` turns them
//! into [`LayerAnimation`] descriptions that the host adds as explicit Core
//! Animation animations inside the batch's transaction. The render server runs
//! them: no timer, display link, completion callback or per-frame wakeup exists
//! in this module, and model values are already the final state when an
//! animation is added. Hide, release and a state change remove them.
//!
//! # Unverified until the native gate
//!
//! Whether `orderWindow(_:relativeTo:)` accepts another process's window
//! number, the effect of each collection-behavior flag, the corner radius call
//! and screen-sharing exclusion are all unverified. They are modeled as
//! capabilities with one status diagnostic each (see [`OverlayDiagnostics`]).

mod animation;
#[cfg(test)]
mod animation_tests;
mod control;
mod corner_radius;
mod geometry;
mod layers;
pub mod live_follow;
mod motion;
pub mod native;
mod owners;
pub mod panel_registry;
mod private_skylight;
pub mod space_sources;
pub mod spaces;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
#[cfg(test)]
mod tests;
mod wake;
#[cfg(test)]
mod width_tests;

use std::sync::atomic::{AtomicU64, Ordering};

pub use animation::{
    ANIMATION_DIAGNOSTIC, AnimKind, AnimLayer, AnimProperty, AnimRepeat, AnimValue, DescribeError,
    FadePlan, KEY_PING_PONG, KEY_ROTATE, KEY_WIDTH_GLOW, KEY_WIDTH_MASK, KEY_WIDTH_RING,
    LayerAnimation, LayerOverride, Presented, ROTATION_STEP_DEGREES, WidthPlan,
    ping_pong_animation, plan_fade, plan_width, rotation_animation,
};
pub use control::{
    AckError, ApplyAck, ApplyReceipt, OverlayControl, OverlayCounters, OverlayTicket, Refusal,
    RefusalReason, ReleaseAck,
};
pub use corner_radius::{
    MAX_SANE_RADIUS, RADIUS_CACHE_LIMIT, RADIUS_SYMBOLS, RawRadiusQuery, sane_radius,
};
pub use geometry::{
    ABOVE_INNER_RADIUS_ADJUSTMENT, INNER_RADIUS_ADJUSTMENT, OverlayPlan, UnitPoint,
    VISIBLE_FRACTION, appkit_frame_from_top_left, display_scale_for_frame, fallback_corner_radius,
    glow_blur, glow_core, glow_radius, gradient_points, outset_frame, plan_overlay,
    ring_at_fraction, scale_for_overlay, select_corner_radius,
};
pub use layers::{ClipSpec, FillSpec, GLOW_PASSES, GlowRender, LayerSpec, RingShape};
pub use live_follow::{
    EventSlots, FAST_REORDER_CAP, FAST_REORDER_WINDOW, FastOrderResult, FastReorder, FollowCore,
    FollowCounters, FollowMove, FollowSink, FollowTarget, FollowTiming, LiveFollow,
    LiveFollowFactory, LiveStatus, LiveUnavailable, QUIET, RawEvent, RawWindowEvent, SessionMaker,
    SlotEvent, SlotsCounters, SystemLiveFollow, UnavailableLiveFollow, WindowEventKind,
    WindowEventSession, unavailable_message,
};
#[cfg(any(test, feature = "testing"))]
pub use live_follow::{FakeLiveFollow, FakeSessionControl, FakeSink, SinkCall};
pub use motion::{
    AppearanceChange, BorderMotion, MIN_MOTION, MotionRepeat, OverlayEasing, Transition,
    clamp_motion,
};
pub use native::{
    COLLECTION_BEHAVIOR, CornerRadiusResolver, LazyCornerRadiusResolver,
    SystemCornerRadiusResolver, SystemOverlayBackend,
};
pub use owners::{
    ApplyReport, ApplySummary, OverlayBackend, OverlayHost, OverlayOwners, OverlayWork,
};
pub use space_sources::{
    OnScreenWindows, STOP_BUDGET, StopObserver, StopOutcome, SystemOnScreenWindows,
    SystemSpaceEvents, WindowListError, confirm_on_screen, forgotten_space_owners,
};
pub use spaces::{
    MAX_SPACE_FACTS, NoSpaceEvents, PushOutcome, SpaceDiagnostic, SpaceDrain, SpaceEvents,
    SpaceEventsFactory, SpaceFact, SpaceMailbox, SpaceSink, SpaceWaker, UnknownCause,
};

/// Most overlays one control reserves at once (live plus pending).
pub const MAX_OVERLAYS: usize = 64;
/// Most released overlays kept ordered out for reuse.
pub const MAX_POOL: usize = 8;
/// Extra transparent margin around the stroke, in points.
pub const OVERLAY_PADDING: f64 = 2.0;
/// Largest border width honored, in points; larger values are clamped.
pub const MAX_BORDER_WIDTH: f64 = 128.0;
/// Longest wait an off-main caller should request for an acknowledgement.
pub const ACK_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

/// Diagnostic published when relative ordering is unavailable.
pub const ORDERING_DIAGNOSTIC: &str =
    "borders: overlays cannot be ordered relative to other applications' windows";
/// Diagnostic published when the corner radius cannot be queried.
pub const CORNER_RADIUS_DIAGNOSTIC: &str =
    "borders: window corner radius is unavailable; using a fixed fallback radius";
/// Diagnostic published when the Space of an overlay cannot be determined.
pub const SPACE_DIAGNOSTIC: &str =
    "borders: Space and Mission Control state cannot be determined; overlays stay hidden";

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// Opaque identity of one overlay. Single use: a released handle is never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OverlayHandle(u64);

impl OverlayHandle {
    /// Allocates a process-unique handle.
    #[must_use]
    pub fn next() -> Self { Self(NEXT_HANDLE.fetch_add(1, Ordering::Relaxed)) }

    /// Wraps a raw id (fakes and tests).
    #[must_use]
    pub const fn from_raw(raw: u64) -> Self { Self(raw) }

    /// The raw id.
    #[must_use]
    pub const fn raw(self) -> u64 { self.0 }
}

/// A rectangle in points.
///
/// Command frames are global top-left coordinates (Tiling's `Rect`); plans and
/// `AppKit` values are bottom-left.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OverlayRect {
    /// Left edge.
    pub x: f64,
    /// Top edge (top-left coordinates) or bottom edge (`AppKit` coordinates).
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

impl OverlayRect {
    /// Creates a rectangle.
    #[must_use]
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self { x, y, width, height }
    }
}

/// A straight-alpha sRGB color with components in `0.0..=1.0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorderColor {
    /// Red.
    pub r: f64,
    /// Green.
    pub g: f64,
    /// Blue.
    pub b: f64,
    /// Alpha.
    pub a: f64,
}

/// How the ring around the target is filled.
///
/// Motion lives next to this value in [`BorderAppearance::motion`]; it does
/// not change these variants.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BorderPaint {
    /// One flat color.
    Solid(BorderColor),
    /// An axial two-stop gradient drawn exactly along `angle` degrees (see
    /// [`gradient_points`] for the convention: 0 is left to right, 90 bottom to top).
    Gradient {
        /// Color at the start of the gradient.
        from: BorderColor,
        /// Color at the end of the gradient.
        to: BorderColor,
        /// Direction in degrees, any finite value (normalized to `[0, 360)`).
        angle: f64,
    },
    /// A soft band of light in this color, brightest at the window edge and
    /// fading outward (no crisp ring), kept outside the target frame.
    Glow(BorderColor),
}

/// Where the ring is ordered relative to its target window.
///
/// Plain data; the `order` configuration key that selects it ships in part 08.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BorderPlacement {
    /// Directly below the target: the window covers the inner part of the ring
    /// and focus changes cannot flip the border over it.
    #[default]
    Below,
    /// Directly above the target, entirely outside its frame.
    Above,
}

/// Plain appearance of one overlay.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorderAppearance {
    /// Stroke width in points; 0 hides the overlay.
    pub width: f64,
    /// Whether the layer uses the display's backing scale factor.
    pub hidpi: bool,
    /// What fills the ring.
    pub paint: BorderPaint,
    /// Explicit inner corner radius in points, overriding the resolved
    /// radius. `None` keeps the resolution in [`select_corner_radius`]
    /// (resolved or fallback; borders are always round), which stays the
    /// single source for that case. Tiling passes
    /// `None`; the override exists for callers that measured a radius.
    pub corner_radius: Option<f64>,
    /// What moves on this border, if anything. The caller decides which
    /// borders carry which motion (ping-pong only on the focused border); the
    /// overlay installs exactly what it is given. Only a gradient paint
    /// animates; the field is ignored for solid and glow.
    pub motion: Option<BorderMotion>,
    /// Whether the ring is ordered below or above its target.
    pub placement: BorderPlacement,
}

/// Why every overlay is being hidden.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HideReason {
    /// A Space change started or finished.
    SpaceChange,
    /// Mission Control, App Exposé or Show Desktop is active.
    MissionControl,
    /// A fullscreen transition is active.
    Fullscreen,
    /// The state cannot be determined.
    Unknown,
}

/// What a command does to one overlay.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OverlayKind {
    /// Create or reuse the overlay, place it, draw it and order it above the target.
    Show {
        /// Target window id (`CGWindowID`).
        target_cg_id: u32,
        /// Target frame, global top-left points.
        frame: OverlayRect,
        /// Appearance.
        appearance: BorderAppearance,
        /// Why the appearance is applied. On a live, shown overlay this is the
        /// explicit appearance-change command: [`AppearanceChange::StateChange`]
        /// cross-fades from the presented colors (when the control has a
        /// [`Transition`]) and restarts the motion; [`AppearanceChange::Reload`]
        /// is instant and keeps a running motion whose parameters did not change.
        change: AppearanceChange,
    },
    /// Re-place a shown overlay for a new target frame.
    Move {
        /// Target frame, global top-left points.
        frame: OverlayRect,
    },
    /// Re-order a shown overlay above another target.
    Order {
        /// Target window id.
        target_cg_id: u32,
    },
    /// Order the overlay out, keeping it for a later `Show`.
    Hide,
    /// Return the overlay to the pool; the handle is retired.
    Release,
}

/// One command for one overlay.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayCommand {
    /// Overlay identity.
    pub handle: OverlayHandle,
    /// The action.
    pub kind: OverlayKind,
    /// The gate epoch the command was computed under (part 05). The main
    /// thread drops a `Show` or `Order` older than the latest applied
    /// `HideAll`. `Move`, `Hide` and `Release` never order a window in, so they
    /// are never dropped for their epoch.
    pub epoch: u64,
}

/// A caller's batch: an optional control-wide hide plus ordered commands.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OverlayBatch {
    /// Control-wide `HideAll { reason }`, applied before every command.
    pub hide_all: Option<HideReason>,
    /// The gate epoch of `hide_all` and of every command added with
    /// [`Self::with`]. Zero for callers without a gate (frame `Move`s from the
    /// route, per-window hides).
    pub epoch: u64,
    /// Per-overlay commands in caller order.
    pub commands: Vec<OverlayCommand>,
}

impl OverlayBatch {
    /// An empty batch.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            hide_all: None,
            epoch: 0,
            commands: Vec::new(),
        }
    }

    /// A batch holding only `HideAll { reason }`.
    #[must_use]
    pub const fn hide_all(reason: HideReason) -> Self {
        Self {
            hide_all: Some(reason),
            epoch: 0,
            commands: Vec::new(),
        }
    }

    /// Sets the gate epoch of the batch: its `HideAll` and the commands added
    /// afterwards with [`Self::with`].
    #[must_use]
    pub const fn at_epoch(mut self, epoch: u64) -> Self {
        self.epoch = epoch;
        self
    }

    /// Appends one command stamped with the batch epoch.
    #[must_use]
    pub fn with(mut self, handle: OverlayHandle, kind: OverlayKind) -> Self {
        let epoch = self.epoch;
        self.push_at(handle, kind, epoch);
        self
    }

    /// Appends one command computed under `epoch`.
    pub fn push_at(&mut self, handle: OverlayHandle, kind: OverlayKind, epoch: u64) {
        self.commands.push(OverlayCommand { handle, kind, epoch });
    }
}

/// How an overlay is ordered relative to its target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderingMode {
    /// `orderWindow(_:relativeTo:)` with the target's `CGWindowID`, below or
    /// above it according to the [`BorderPlacement`].
    RelativeToTarget,
    /// Order at the target's level, to the front of that level; the caller
    /// raises the target first. Used when `AppKit` ignores foreign window
    /// numbers. Without relative ordering a below-placed border may be covered
    /// by other windows of the level; the native check decides.
    RaiseFallback,
}

/// Whether the target's corner radius can be queried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadiusCapability {
    /// The radius is queried per target.
    Available,
    /// Only the fixed per-OS fallback is used.
    FallbackOnly,
}

/// Whether the Space of an overlay is understood.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpaceCapability {
    /// The structural collection behavior keeps overlays on one Space.
    Structural,
    /// The Space state cannot be determined.
    Undetermined,
}

/// Capability facts the control reports as diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverlayCapabilities {
    /// Ordering capability.
    pub ordering: OrderingMode,
    /// Corner radius capability.
    pub radius: RadiusCapability,
    /// Space capability.
    pub space: SpaceCapability,
}

impl Default for OverlayCapabilities {
    fn default() -> Self {
        Self {
            ordering: OrderingMode::RelativeToTarget,
            radius: RadiusCapability::FallbackOnly,
            space: SpaceCapability::Structural,
        }
    }
}

/// One status diagnostic per capability gap, as plain data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OverlayDiagnostics {
    /// Relative ordering gap.
    pub ordering: Option<&'static str>,
    /// Corner radius gap.
    pub corner_radius: Option<&'static str>,
    /// Space gap.
    pub space: Option<&'static str>,
    /// A motion that could not be described (the border is drawn statically);
    /// set by the control after an applied batch reports it.
    pub animation: Option<&'static str>,
}

impl OverlayDiagnostics {
    /// Derives the diagnostics for a capability set.
    #[must_use]
    pub const fn from_capabilities(capabilities: &OverlayCapabilities) -> Self {
        Self {
            ordering: match capabilities.ordering {
                OrderingMode::RelativeToTarget => None,
                OrderingMode::RaiseFallback => Some(ORDERING_DIAGNOSTIC),
            },
            corner_radius: match capabilities.radius {
                RadiusCapability::Available => None,
                RadiusCapability::FallbackOnly => Some(CORNER_RADIUS_DIAGNOSTIC),
            },
            space: match capabilities.space {
                SpaceCapability::Structural => None,
                SpaceCapability::Undetermined => Some(SPACE_DIAGNOSTIC),
            },
            animation: None,
        }
    }

    /// Every published message.
    #[must_use]
    pub fn messages(&self) -> Vec<&'static str> {
        [
            self.ordering,
            self.corner_radius,
            self.space,
            self.animation,
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

/// One attached display as the overlay geometry sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayScreen {
    /// Frame in `AppKit` global coordinates (bottom-left origin).
    pub frame: OverlayRect,
    /// Backing scale factor.
    pub scale: f64,
}

/// The overlay could not be created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayFailure {
    /// Native construction returned nothing.
    PanelCreation(&'static str),
}

#[cfg(test)]
mod doc_checks {
    const fn send_sync<T: Send + Sync>() {}
    const _: () = send_sync::<super::OverlayControl>();
    const _: () = send_sync::<super::OverlayHandle>();
    const _: () = send_sync::<super::OverlayBatch>();
}
