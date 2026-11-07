//! Safe native Hold-to-Quit HUD adapter.
//!
//! This module is the complete native boundary a later Hold-to-Quit runtime
//! uses to show "Hold ⌘Q to quit `<App>`" with a progress fill. It installs no
//! event tap, tray item, IPC route or application observer, depends on no
//! Hold-to-Quit policy and shows nothing by default: constructing a
//! [`HudControl`] creates no panel, and installation creates only a hidden one.
//!
//! # Ownership
//!
//! ```text
//! runtime (any thread)
//!   ├─ HudControl (Clone, Send + Sync)
//!   │    ├─ Arc<dyn MainThreadExecutor>     dispatch only, never waited on from main
//!   │    ├─ Arc<dyn HudBackend>             SystemHudBackend is a unit struct
//!   │    └─ plain state: HudHandle, phase, current PressId, pending deadlines
//!   ├─ HudEvents (Send)                      plain HudEvent results, 2 s sweep
//!   └─ HudRequest / HudShown / HudRetained   plain owned data
//!
//! main thread only (MainThreadMarker)
//!   └─ thread-local PANELS: BTreeMap<HudHandle, NativePanel>   <- the late-cleanup owner
//!        NativePanel { NSPanel, NSVisualEffectView, NSTextField, track CALayer,
//!                      fill CALayer (+ its CABasicAnimation), displayed PressId }
//! ```
//!
//! No Cocoa value crosses a thread. The panel, views, layers and the running
//! animation live in the main-thread-local owner map, keyed by the opaque
//! [`HudHandle`] (id plus generation). There is no `unsafe impl Send`/`Sync`,
//! no static Cocoa singleton and no forgotten (leaked) object. An owner is
//! removed from the map only by an acknowledged release job (or the
//! best-effort job enqueued when the last control reference drops), which
//! orders the panel out, removes its animations and drops every object on the
//! main thread. A queued release that outlives its waiter is the late-cleanup
//! owner: it finishes exactly once, and the control reports
//! [`HudReleaseOutcome::Retained`] until it does.
//!
//! # Lifecycle and acknowledgements
//!
//! - [`HudControl::install`] runs off-main. It enqueues one main job that
//!   verifies a usable screen, creates the hidden panel and acknowledges. The
//!   wait is bounded by [`ACKNOWLEDGEMENT_BUDGET`] and polls cancellation every
//!   [`CANCELLATION_QUANTUM`]. A repeated call reuses the pending or finished
//!   reservation instead of queueing a second panel. A caller already on the
//!   main thread never waits.
//! - [`HudControl::show`], [`HudControl::hide`] and [`HudControl::freeze`] never
//!   wait. `freeze` stops the progress bar at its on-screen width while the
//!   panel stays visible; it emits no event and leaves the logical press state
//!   untouched. Every show emits
//!   exactly one [`HudEvent`]; a missing acknowledgement becomes
//!   [`HudUnavailable::TimedOut`] after the budget (observed by
//!   [`HudEvents`]) and invalidates the press.
//! - Hide, release and a newer show invalidate the logical press immediately,
//!   before any queued native work. Every job rechecks handle, generation and
//!   press at commit, so an older hide cannot hide a successor press and a
//!   late result cannot resurrect a cancelled press.
//! - [`HudControl::release`] still enqueues physical cleanup when its token is
//!   already cancelled; only the acknowledgement wait gives up.
//!
//! Panics inside backend calls are contained and become typed failures.
//! Completions only enqueue plain results.
//!
//! # Placement and visuals
//!
//! Each show recomputes placement: requested display (if attached), screen
//! containing the mouse, main screen, first screen, else
//! [`HudShowFailure::NoScreen`]. The chosen [`DisplayId`] and
//! [`PlacementSource`] are reported as plain data. Placement needs only
//! `NSScreen` geometry and `NSEvent.mouseLocation`; it never uses Screen
//! Recording or Accessibility. The layout is the pure [`compute_layout`].
//!
//! The anchor ([`HudAnchor`]) picks the edge or centre of the visible frame.
//! Every show fades the panel in over [`SHOW_DURATION`] (ease-out) and every
//! hide fades it out over [`HIDE_DURATION`] (ease-in), both through
//! `NSAnimationContext` on the window's `animator()`. [`HudMotion::Slide`] also
//! moves the panel from the offset frame [`motion_frames`] computes (Reduce
//! Motion turns a slide into a plain fade). A hide orders the panel out only in
//! its completion handler and only while a per-panel `MotionGeneration`
//! token is still current, so a successor show or a release always wins.
//!
//! The progress fill is a `CABasicAnimation` on a layer's `bounds.size.width`
//! so Core Animation drives it; there is no Rust timer, and no animation exists
//! while the panel is hidden. See [`progress_plan`] for the duration math.
//!
//! # Unsupported cases
//!
//! A borderless nonactivating panel ordered with `orderFrontRegardless` is
//! accepted by the window server in every Space, but accepted ordering is not
//! proof of visible pixels: another app's exclusive full-screen Space, a
//! screen-saver or a secure-input surface may still hide it. A display removed
//! between selection and ordering falls back at the next show. Visibility is a
//! manual check, not something compilation or the fakes prove.
//!
//! # Binding evidence
//!
//! Compile-proven against `objc2 0.6.5` and `objc2-app-kit 0.3.2` with the
//! features `NSAccessibility`, `NSApplication`, `NSColor`, `NSControl`, `NSEvent`, `NSFont`,
//! `NSGraphics`, `NSPanel`, `NSAnimation`, `NSAnimationContext`, `NSParagraphStyle`, `NSResponder`,
//! `NSRunningApplication`, `NSScreen`, `NSText`, `NSTextField`, `NSView`,
//! `NSVisualEffectView`, `NSWindow`, `NSWorkspace`, `block2`,
//! `objc2-core-foundation`, `objc2-core-graphics`, `objc2-quartz-core` and
//! `std`, plus `objc2-quartz-core 0.3.2` with `CAAnimation`, `CALayer`,
//! `CABase`, `CAMediaTiming`, `CAMediaTimingFunction`, `CATransaction`,
//! `objc2-core-foundation`, `objc2-core-graphics` and `std`.
//!
//! - Panel, view and text initializers return non-null `Retained<T>`, so nil
//!   construction is unrepresentable there. The one nil-able step, a view's
//!   backing `layer()`, maps to [`HudInstallError::NilConstruction`]. A missing
//!   or degenerate screen maps to [`HudInstallError::NoScreen`].
//! - `NSPanel`, `NSView` and `NSTextField` are `MainThreadOnly`; allocation
//!   needs a `MainThreadMarker`, which is only obtained inside main jobs.
//! - The display identity is `-[NSScreen CGDirectDisplayID]`, the same
//!   [`DisplayId`] the desktop adapter uses.
//! - `NSWorkspace.accessibilityDisplayShouldIncreaseContrast`,
//!   `...ShouldReduceTransparency` and `...ShouldReduceMotion` are public.
//!   Window animation behavior stays `None`: the system's own window animation is
//!   never used. The show and hide animation is ours (see "Placement and
//!   visuals").
//!
//! The only `unsafe` call is `setReleasedWhenClosed(false)`, justified at its
//! use. The compile proofs below never create a panel.
//!
//! The thread-safety contract is a compile-time fact, proved without creating a
//! panel:
//!
//! ```
//! use stache_macos::hold_hud::{HudControl, HudEvent, HudHandle, HudRequest, SystemHudBackend};
//!
//! const fn send_sync<T: Send + Sync>() {}
//! send_sync::<HudControl>();
//! send_sync::<HudHandle>();
//! send_sync::<HudRequest>();
//! send_sync::<HudEvent>();
//! send_sync::<SystemHudBackend>();
//! ```
//!
//! Wiring the system backend (never run by default; installation creates one
//! hidden panel on the main thread):
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use stache_macos::hold_hud::{HudControl, SystemHudBackend};
//! use stache_macos::MainThread;
//! use stache_sdk::CancellationToken;
//!
//! // Nothing is shown; installation only creates a hidden panel.
//! let (control, _events) = HudControl::new(Arc::new(MainThread), Arc::new(SystemHudBackend));
//! let _ = control.install(&CancellationToken::new());
//! ```

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSAnimatablePropertyContainer, NSAnimationContext, NSBackingStoreType, NSColor, NSEvent,
    NSFont, NSLineBreakMode, NSPanel, NSScreen, NSStatusWindowLevel, NSTextAlignment, NSTextField,
    NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
    NSWindowAnimationBehavior, NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{NSNumber, NSPoint, NSRect, NSSize, NSString, ns_string};
use objc2_quartz_core::{
    CABasicAnimation, CACurrentMediaTime, CALayer, CAMediaTiming, CAMediaTimingFunction,
    CATransaction, kCAMediaTimingFunctionEaseIn, kCAMediaTimingFunctionEaseOut,
    kCAMediaTimingFunctionLinear,
};
use stache_sdk::{CancellationToken, MainThreadExecutor};

pub use crate::applications::{ACKNOWLEDGEMENT_BUDGET, CANCELLATION_QUANTUM};
use crate::desktop::DisplayId;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

#[cfg(test)]
mod tests;

/// Longest remaining time rendered with a Core Animation (10 minutes).
///
/// A longer remainder renders a static fraction instead; the owning runtime
/// deadline stays authoritative.
pub const MAX_ANIMATED_MS: u64 = 600_000;

const PREFIX: &str = "Hold ⌘Q to quit ";
const FALLBACK_APP: &str = "the app";
const MAX_LABEL_CHARS: usize = 200;
const MARGIN: f64 = 24.0;
const MIN_WIDTH: f64 = 160.0;
const MAX_WIDTH: f64 = 420.0;
const HEIGHT: f64 = 64.0;
const PADDING: f64 = 16.0;
const TRACK_HEIGHT: f64 = 6.0;
const BOTTOM_INSET: f64 = 48.0;

/// How long the show animation (fade, plus slide) runs, eased out.
pub const SHOW_DURATION: Duration = Duration::from_millis(200);
/// How long the hide animation runs, eased in.
pub const HIDE_DURATION: Duration = Duration::from_millis(150);
const CHARACTER_WIDTH: f64 = 7.5;

// ---------------------------------------------------------------------------
// Plain identity and geometry data.
// ---------------------------------------------------------------------------

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// Opaque identity of one installed (or installing) HUD owner.
///
/// It carries no native pointer. The generation increases when a failed
/// installation attempt is retried, so a late job from an older attempt can
/// never act on a newer owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HudHandle {
    id: u64,
    generation: u64,
}

impl HudHandle {
    /// The process-unique owner id.
    #[must_use]
    pub const fn id(self) -> u64 { self.id }

    /// The installation attempt generation.
    #[must_use]
    pub const fn generation(self) -> u64 { self.generation }

    /// Builds a handle from raw parts (fakes and tests).
    #[must_use]
    pub const fn from_parts(id: u64, generation: u64) -> Self { Self { id, generation } }
}

/// Identity of one key press the HUD is shown for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PressId(u64);

impl PressId {
    /// Wraps a raw press number.
    #[must_use]
    pub const fn new(raw: u64) -> Self { Self(raw) }

    /// Returns the raw press number.
    #[must_use]
    pub const fn raw(self) -> u64 { self.0 }
}

/// A point in global screen coordinates (points).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HudPoint {
    /// Horizontal coordinate.
    pub x: f64,
    /// Vertical coordinate.
    pub y: f64,
}

/// A rectangle in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HudRect {
    /// Left edge.
    pub x: f64,
    /// Bottom edge.
    pub y: f64,
    /// Width, never negative after sanitizing.
    pub width: f64,
    /// Height, never negative after sanitizing.
    pub height: f64,
}

impl HudRect {
    /// Creates a rectangle.
    #[must_use]
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self { x, y, width, height }
    }

    /// Whether the point lies inside (right and top edges exclusive).
    #[must_use]
    pub fn contains(&self, point: HudPoint) -> bool {
        point.x >= self.x
            && point.x < self.x + self.width
            && point.y >= self.y
            && point.y < self.y + self.height
    }

    /// The right edge.
    #[must_use]
    pub fn right(&self) -> f64 { self.x + self.width }

    /// The top edge.
    #[must_use]
    pub fn top(&self) -> f64 { self.y + self.height }

    fn sanitized(self) -> Self {
        let finite = |value: f64| if value.is_finite() { value } else { 0.0 };
        Self {
            x: finite(self.x),
            y: finite(self.y),
            width: finite(self.width).max(0.0),
            height: finite(self.height).max(0.0),
        }
    }
}

/// One attached display as the placement function sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HudScreen {
    /// Native display identity.
    pub id: DisplayId,
    /// Full frame in global coordinates.
    pub frame: HudRect,
    /// Visible frame (excludes menu bar and Dock) in global coordinates.
    pub visible: HudRect,
}

/// The display facts one placement decision uses.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HudScreens {
    /// Attached displays in `NSScreen.screens` order; index 0 is the first.
    pub screens: Vec<HudScreen>,
    /// Current mouse location in global coordinates, when known.
    pub mouse: Option<HudPoint>,
    /// The main screen's identity, when the system reports one.
    pub main: Option<DisplayId>,
}

/// Why a display was chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlacementSource {
    /// The caller's requested display was attached.
    RequestedDisplay,
    /// The screen containing the mouse.
    MouseScreen,
    /// The main screen.
    MainScreen,
    /// The first screen.
    FirstScreen,
}

/// A chosen display and the reason, as plain diagnostic data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HudPlacement {
    /// The selected display.
    pub display: DisplayId,
    /// Why it was selected.
    pub source: PlacementSource,
}

/// No attached screen can host the HUD.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoScreen;

impl fmt::Display for NoScreen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("no screen is attached")
    }
}

impl std::error::Error for NoScreen {}

/// Chooses the display: requested (if attached) -> mouse -> main -> first.
///
/// # Errors
/// Returns [`NoScreen`] when `screens` has no display.
pub fn select_screen(
    screens: &HudScreens,
    requested: Option<DisplayId>,
) -> Result<(HudScreen, PlacementSource), NoScreen> {
    let find = |id: DisplayId| screens.screens.iter().find(|screen| screen.id == id).copied();
    if let Some(screen) = requested.and_then(find) {
        return Ok((screen, PlacementSource::RequestedDisplay));
    }
    if let Some(mouse) = screens.mouse
        && let Some(screen) =
            screens.screens.iter().find(|screen| screen.frame.sanitized().contains(mouse))
    {
        return Ok((*screen, PlacementSource::MouseScreen));
    }
    if let Some(screen) = screens.main.and_then(find) {
        return Ok((screen, PlacementSource::MainScreen));
    }
    screens
        .screens
        .first()
        .map(|screen| (*screen, PlacementSource::FirstScreen))
        .ok_or(NoScreen)
}

// ---------------------------------------------------------------------------
// Pure text, layout, progress and style.
// ---------------------------------------------------------------------------

/// Where the HUD sits inside the chosen display's visible frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HudAnchor {
    /// Horizontally centered, near the lower edge.
    #[default]
    Bottom,
    /// Horizontally centered, near the upper edge.
    Top,
    /// Vertically centered, near the left edge.
    Left,
    /// Vertically centered, near the right edge.
    Right,
    /// Centered both ways.
    Center,
}

/// How the HUD enters and leaves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HudMotion {
    /// Alpha only, at the final frame.
    #[default]
    Fade,
    /// Alpha plus a slide from (and back toward) the anchor's edge.
    Slide,
}

/// The motion actually played: a slide becomes a fade under Reduce Motion.
#[must_use]
pub const fn effective_motion(motion: HudMotion, reduce_motion: bool) -> HudMotion {
    if reduce_motion {
        HudMotion::Fade
    } else {
        motion
    }
}

/// The start and end panel frames of the show animation (the hide runs it back
/// toward the start frame).
///
/// `fade` has `start == end`. `slide` starts outside the anchored edge by the
/// panel size plus the edge inset (bottom: below, top: above, left: left of,
/// right: right of the final frame) and, for `center`, exactly one panel height
/// above the final frame, moving down. Reduce Motion makes a slide a fade.
#[must_use]
pub fn motion_frames(
    panel: HudRect,
    anchor: HudAnchor,
    motion: HudMotion,
    reduce_motion: bool,
) -> (HudRect, HudRect) {
    let end = panel;
    if effective_motion(motion, reduce_motion) == HudMotion::Fade {
        return (end, end);
    }
    let mut start = end;
    match anchor {
        HudAnchor::Bottom => start.y -= end.height + BOTTOM_INSET,
        HudAnchor::Top => start.y += end.height + BOTTOM_INSET,
        HudAnchor::Left => start.x -= end.width + BOTTOM_INSET,
        HudAnchor::Right => start.x += end.width + BOTTOM_INSET,
        HudAnchor::Center => start.y += end.height,
    }
    (start, end)
}

/// A per-panel generation counter for the animated hide.
///
/// A hide and a show each [`advance`](Self::advance) it; the hide keeps its
/// token and the completion acts only while that token is still
/// [`current`](Self::is_current).
#[derive(Debug, Default)]
pub(crate) struct MotionGeneration(Cell<u64>);

impl MotionGeneration {
    /// Invalidates every earlier token and returns the new one.
    pub(crate) fn advance(&self) -> u64 {
        let next = self.0.get().wrapping_add(1);
        self.0.set(next);
        next
    }

    /// Whether `token` is still the latest.
    pub(crate) const fn is_current(&self, token: u64) -> bool { self.0.get() == token }
}

/// The pure geometry of one HUD.
#[derive(Clone, Debug, PartialEq)]
pub struct HudLayout {
    /// The panel frame in global coordinates, inside the visible frame.
    pub panel: HudRect,
    /// The label frame in panel coordinates.
    pub text_frame: HudRect,
    /// The progress track frame in panel coordinates.
    pub track_frame: HudRect,
    /// The accessible static text actually shown (possibly truncated).
    pub text: String,
    /// Whether the text was shortened with an ellipsis.
    pub truncated: bool,
}

fn clean_label(app: Option<&str>) -> String {
    let cleaned: String = app
        .unwrap_or_default()
        .chars()
        .take(MAX_LABEL_CHARS)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        FALLBACK_APP.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// The full, untruncated HUD text: "Hold ⌘Q to quit `<App>`".
///
/// A missing, blank or control-only name becomes "the app".
#[must_use]
pub fn hud_text(app: Option<&str>) -> String { format!("{PREFIX}{}", clean_label(app)) }

fn fit_text(text: &str, max_chars: usize) -> (String, bool) {
    let count = text.chars().count();
    if count <= max_chars {
        return (text.to_owned(), false);
    }
    if max_chars == 0 {
        return (String::new(), true);
    }
    let mut kept: String = text.chars().take(max_chars - 1).collect();
    kept.push('…');
    (kept, true)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is a finite non-negative floor of a small width estimate"
)]
fn chars_fitting(width: f64) -> usize {
    if !width.is_finite() || width <= 0.0 {
        0
    } else {
        (width / CHARACTER_WIDTH).floor() as usize
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "the character count is bounded by MAX_LABEL_CHARS"
)]
fn estimated_text_width(text: &str) -> f64 { text.chars().count() as f64 * CHARACTER_WIDTH }

/// Computes the panel, text and track geometry inside `visible`.
///
/// The panel sits at `anchor`: bottom or top is horizontally centered the same
/// inset from that edge, left or right is vertically centered the same inset
/// from that edge, and center is centered both ways. It is never wider or
/// taller than the visible frame minus margins, and the track and text always
/// lie inside the panel. A long label is truncated with an ellipsis; the track is
/// laid out independently of the text, so it can never move off-screen.
#[must_use]
pub fn compute_layout(visible: HudRect, app: Option<&str>, anchor: HudAnchor) -> HudLayout {
    let visible = visible.sanitized();
    let full = hud_text(app);
    let margin_x = MARGIN.min(visible.width / 8.0);
    let margin_y = MARGIN.min(visible.height / 8.0);
    let available_width = (visible.width - 2.0 * margin_x).max(0.0);
    let desired = (estimated_text_width(&full) + 2.0 * PADDING).clamp(MIN_WIDTH, MAX_WIDTH);
    let width = desired.min(available_width);
    let height = HEIGHT.min((visible.height - 2.0 * margin_y).max(0.0));
    let centered_x = visible.x + (visible.width - width) / 2.0;
    let centered_y = visible.y + (visible.height - height) / 2.0;
    let inset_y = BOTTOM_INSET.clamp(margin_y, (visible.height - height - margin_y).max(margin_y));
    let inset_x = BOTTOM_INSET.clamp(margin_x, (visible.width - width - margin_x).max(margin_x));
    let (x, y) = match anchor {
        HudAnchor::Bottom => (centered_x, visible.y + inset_y),
        HudAnchor::Top => (centered_x, visible.y + visible.height - height - inset_y),
        HudAnchor::Left => (visible.x + inset_x, centered_y),
        HudAnchor::Right => (visible.x + visible.width - width - inset_x, centered_y),
        HudAnchor::Center => (centered_x, centered_y),
    };

    let padding = PADDING.min(width / 8.0).min(height / 8.0);
    let track_height = TRACK_HEIGHT.min(height / 4.0);
    let inner_width = (width - 2.0 * padding).max(0.0);
    let track_frame = HudRect::new(padding, padding, inner_width, track_height);
    let text_y = padding + track_height + padding / 2.0;
    let text_frame = HudRect::new(
        padding,
        text_y,
        inner_width,
        (height - text_y - padding).max(0.0),
    );
    let (text, truncated) = fit_text(&full, chars_fitting(inner_width));
    HudLayout {
        panel: HudRect::new(x, y, width, height),
        text_frame,
        track_frame,
        text,
        truncated,
    }
}

/// The progress fraction for a hold, in `0.0..=1.0`.
///
/// A zero duration or `elapsed_ms >= duration_ms` is full. The `u64` to `f64`
/// conversion is lossy only in the low bits and cannot overflow or panic.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "a fraction needs only approximate magnitude; u64 to f64 never overflows"
)]
pub fn progress_fraction(elapsed_ms: u64, duration_ms: u64) -> f64 {
    if duration_ms == 0 || elapsed_ms >= duration_ms {
        return 1.0;
    }
    (elapsed_ms as f64 / duration_ms as f64).clamp(0.0, 1.0)
}

/// How the progress fill is rendered for one show.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ProgressPlan {
    /// Zero or elapsed duration: a full, static fill and no animation.
    Full,
    /// A Core Animation from `from` to full over `seconds`.
    Animate {
        /// Starting fraction in `0.0..1.0`.
        from: f64,
        /// Remaining animation time in seconds.
        seconds: f64,
    },
    /// A remainder too long for a native animation: a static safe fraction.
    Static(f64),
}

/// Chooses the rendering of the progress fill.
///
/// Zero or elapsed-or-longer durations are [`ProgressPlan::Full`]. A remainder
/// above [`MAX_ANIMATED_MS`] is a [`ProgressPlan::Static`] fraction, never full,
/// so a huge duration is not mistaken for completion. No `u64` input overflows.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "the remainder is at most MAX_ANIMATED_MS, exactly representable"
)]
pub fn progress_plan(elapsed_ms: u64, duration_ms: u64) -> ProgressPlan {
    if duration_ms == 0 || elapsed_ms >= duration_ms {
        return ProgressPlan::Full;
    }
    let fraction = progress_fraction(elapsed_ms, duration_ms);
    let remaining = duration_ms - elapsed_ms;
    if remaining > MAX_ANIMATED_MS {
        return ProgressPlan::Static(fraction);
    }
    ProgressPlan::Animate {
        from: fraction,
        seconds: remaining as f64 / 1000.0,
    }
}

/// The fraction of the track a progress animation has reached `elapsed_seconds`
/// after it began, on a linear run from `from` to full over `seconds`.
///
/// Pure arithmetic so the freeze position never depends on what Core Animation
/// has committed: the result is clamped to `from..=1.0`, a non-positive or
/// non-finite `seconds` is already complete and a negative or NaN elapsed time
/// is the start.
#[must_use]
pub fn progress_fraction_at(from: f64, seconds: f64, elapsed_seconds: f64) -> f64 {
    let from = if from.is_finite() {
        from.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if seconds.is_nan() || seconds <= 0.0 {
        return 1.0;
    }
    let ratio = if elapsed_seconds.is_nan() {
        0.0
    } else {
        (elapsed_seconds / seconds).clamp(0.0, 1.0)
    };
    from + (1.0 - from) * ratio
}

/// System accessibility display options read when showing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HudAppearance {
    /// Increase Contrast is enabled.
    pub increase_contrast: bool,
    /// Reduce Motion is enabled. A slide becomes a plain fade (see
    /// [`effective_motion`]); progress is unchanged.
    pub reduce_motion: bool,
    /// Reduce Transparency is enabled.
    pub reduce_transparency: bool,
}

/// Concrete visual choices derived from [`HudAppearance`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HudStyle {
    /// Alpha of the progress track.
    pub track_alpha: f64,
    /// Alpha of the progress fill.
    pub fill_alpha: f64,
    /// Whether the fill uses the high-contrast label color instead of the accent.
    pub fill_uses_label_color: bool,
    /// Whether the surface is an opaque window-background color.
    pub opaque_surface: bool,
}

/// Derives the style: stronger track and fill for Increase Contrast, an opaque
/// surface for Reduce Transparency. Motion is separate: see
/// [`effective_motion`].
#[must_use]
pub const fn style_for(appearance: HudAppearance) -> HudStyle {
    HudStyle {
        track_alpha: if appearance.increase_contrast {
            0.45
        } else {
            0.2
        },
        fill_alpha: 1.0,
        fill_uses_label_color: appearance.increase_contrast,
        opaque_surface: appearance.reduce_transparency,
    }
}

// ---------------------------------------------------------------------------
// Requests, results and errors.
// ---------------------------------------------------------------------------

/// A pure, re-evaluated authorization for one show.
///
/// The predicate must be cheap and side-effect free; it is evaluated on the main
/// thread at commit and again immediately before ordering front. A later part
/// supplies an application-revalidated predicate. It is not a native pointer or
/// an arbitrary module callback.
#[derive(Clone)]
pub struct HudAuthorization(Arc<dyn Fn() -> bool + Send + Sync>);

impl HudAuthorization {
    /// Always authorized.
    #[must_use]
    pub fn always() -> Self { Self(Arc::new(|| true)) }

    /// Wraps a pure predicate.
    #[must_use]
    pub fn new(check: impl Fn() -> bool + Send + Sync + 'static) -> Self { Self(Arc::new(check)) }

    /// Evaluates the predicate.
    #[must_use]
    pub fn is_authorized(&self) -> bool { (self.0)() }
}

impl fmt::Debug for HudAuthorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HudAuthorization").finish_non_exhaustive()
    }
}

/// A request to show the HUD for one press.
#[derive(Clone, Debug)]
pub struct HudRequest {
    /// The owner this request targets.
    pub handle: HudHandle,
    /// The press the HUD is for.
    pub press: PressId,
    /// The application name; `None` or blank renders "the app".
    pub label: Option<String>,
    /// An opaque display hint; a detached display falls back.
    pub display: Option<DisplayId>,
    /// Total hold duration in milliseconds; zero is full immediately.
    pub duration_ms: u64,
    /// Milliseconds already held when the HUD appears.
    pub elapsed_ms: u64,
    /// The pure authorization checked before ordering front.
    pub authorization: HudAuthorization,
    /// Where the HUD sits; defaults to [`HudAnchor::Bottom`].
    pub anchor: HudAnchor,
    /// How the HUD enters and leaves; defaults to [`HudMotion::Fade`].
    pub motion: HudMotion,
}

impl HudRequest {
    /// Creates an authorized request with no label or display hint.
    #[must_use]
    pub fn new(handle: HudHandle, press: PressId, duration_ms: u64) -> Self {
        Self {
            handle,
            press,
            label: None,
            display: None,
            duration_ms,
            elapsed_ms: 0,
            authorization: HudAuthorization::always(),
            anchor: HudAnchor::Bottom,
            motion: HudMotion::Fade,
        }
    }

    /// Sets the application label.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Sets the display hint.
    #[must_use]
    pub const fn with_display(mut self, display: DisplayId) -> Self {
        self.display = Some(display);
        self
    }

    /// Sets the already-elapsed milliseconds.
    #[must_use]
    pub const fn with_elapsed_ms(mut self, elapsed_ms: u64) -> Self {
        self.elapsed_ms = elapsed_ms;
        self
    }

    /// Sets the anchor.
    #[must_use]
    pub const fn with_anchor(mut self, anchor: HudAnchor) -> Self {
        self.anchor = anchor;
        self
    }

    /// Sets the motion.
    #[must_use]
    pub const fn with_motion(mut self, motion: HudMotion) -> Self {
        self.motion = motion;
        self
    }

    /// Sets the authorization.
    #[must_use]
    pub fn with_authorization(mut self, authorization: HudAuthorization) -> Self {
        self.authorization = authorization;
        self
    }
}

/// The plain native command a backend receives for a show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HudShowCommand {
    /// The press being shown.
    pub press: PressId,
    /// The application label.
    pub label: Option<String>,
    /// The display hint.
    pub display: Option<DisplayId>,
    /// Total duration in milliseconds.
    pub duration_ms: u64,
    /// Already-elapsed milliseconds.
    pub elapsed_ms: u64,
    /// Where the HUD sits.
    pub anchor: HudAnchor,
    /// How the HUD enters and leaves.
    pub motion: HudMotion,
}

/// Evidence that native ordering succeeded for a still-authorized press.
#[derive(Clone, Debug, PartialEq)]
pub struct HudShown {
    /// The selected display and why.
    pub placement: HudPlacement,
    /// The accessible text shown.
    pub text: String,
    /// The panel frame used.
    pub panel: HudRect,
    /// How progress is rendered.
    pub plan: ProgressPlan,
    /// The accessibility options read.
    pub appearance: HudAppearance,
}

/// Why a show produced no visible HUD.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HudUnavailable {
    /// No screen could host the HUD.
    NoScreen,
    /// The control is not installed (yet).
    NotReady,
    /// The control is released or retired.
    Retired,
    /// The main-thread dispatcher rejected the job.
    DispatchRejected,
    /// No acknowledgement arrived within the budget.
    TimedOut,
}

/// The asynchronous result of one show.
#[derive(Clone, Debug, PartialEq)]
pub enum HudOutcome {
    /// Native ordering succeeded for this still-authorized press.
    Shown(HudShown),
    /// The press was superseded, hidden, released or unauthorized.
    Stale,
    /// The HUD could not be shown.
    Unavailable(HudUnavailable),
    /// A native step failed or panicked.
    Failed(String),
}

/// A generation- and press-tagged show result.
#[derive(Clone, Debug, PartialEq)]
pub struct HudEvent {
    /// The owner the show targeted.
    pub handle: HudHandle,
    /// The press the result belongs to.
    pub press: PressId,
    /// The result.
    pub outcome: HudOutcome,
}

/// Why a backend show failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HudShowFailure {
    /// The gate refused ordering (press no longer current or unauthorized).
    Stale,
    /// No screen could host the HUD.
    NoScreen,
    /// The backend has no panel for the handle.
    NotInstalled,
    /// A native step failed.
    Native(String),
}

/// Why installation did not produce a ready HUD.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HudInstallError {
    /// A native object that must exist was nil.
    NilConstruction(&'static str),
    /// No usable screen is attached.
    NoScreen,
    /// A backend job ran off the main thread.
    NotMainThread,
    /// A native step failed.
    Native(String),
    /// The main-thread dispatcher rejected the job.
    DispatchRejected,
    /// The caller's token was cancelled while waiting.
    Cancelled,
    /// The acknowledgement did not arrive within the budget.
    TimedOut,
    /// The control was released.
    Retired,
    /// A failed cleanup still owns the native objects.
    Retained(HudRetainReason),
}

impl fmt::Display for HudInstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NilConstruction(what) => write!(f, "{what} was nil"),
            Self::NoScreen => f.write_str("no usable screen is attached"),
            Self::NotMainThread => f.write_str("native HUD job ran off the main thread"),
            Self::Native(message) => write!(f, "native HUD installation failed: {message}"),
            Self::DispatchRejected => f.write_str("the main-thread dispatcher rejected the job"),
            Self::Cancelled => f.write_str("installation wait was cancelled"),
            Self::TimedOut => f.write_str("installation was not acknowledged in time"),
            Self::Retired => f.write_str("the HUD control was released"),
            Self::Retained(reason) => write!(f, "HUD cleanup is retained: {reason:?}"),
        }
    }
}

impl std::error::Error for HudInstallError {}

/// A backend release failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HudReleaseError(pub String);

/// Why native ownership is still held.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HudRetainReason {
    /// Native removal reported an error (or panicked); the owner stays.
    RemovalFailed(String),
    /// The acknowledgement did not arrive within the budget.
    TimedOut,
    /// The wait was cancelled before the acknowledgement.
    Cancelled,
    /// The dispatcher rejected the cleanup job.
    DispatchRejected,
    /// The caller was on the main thread and cannot wait for its own queue.
    MainThreadCaller,
}

/// Whether physical cleanup is still queued or already failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HudCleanup {
    /// A cleanup job is queued and may still finish exactly once.
    Queued,
    /// The last cleanup attempt failed; a retry enqueues a new one.
    Failed,
}

/// Exact description of native ownership that release could not free.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HudRetained {
    /// The owner whose native objects remain.
    pub handle: HudHandle,
    /// Why they remain.
    pub reason: HudRetainReason,
    /// The state of the cleanup.
    pub cleanup: HudCleanup,
}

/// The result of [`HudControl::release`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HudReleaseOutcome {
    /// Every native object was dropped on the main thread.
    Released,
    /// Native objects remain; the control is retired.
    Retained(HudRetained),
}

/// A diagnostic view of a control's lifecycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HudPhase {
    /// Nothing native exists or is queued.
    Idle,
    /// An installation job is queued or running.
    Installing,
    /// A hidden panel is ready.
    Ready,
    /// A release job is queued or running.
    Releasing,
    /// Everything was dropped; the control is retired.
    Released,
    /// A failed cleanup retains the native objects.
    Retained(HudRetainReason),
}

/// Information a backend reports from installation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HudInstallInfo {
    /// How many screens were attached.
    pub screens: usize,
}

// ---------------------------------------------------------------------------
// Backend contract.
// ---------------------------------------------------------------------------

/// The main-thread-only native contract.
///
/// Every method is called on the main thread by a [`MainThreadExecutor`] job.
/// Implementations own Cocoa objects only in main-thread-local storage; the
/// trait object itself is `Send + Sync` plain data.
pub trait HudBackend: Send + Sync {
    /// Creates the hidden panel for `handle`.
    ///
    /// # Errors
    /// Returns a typed error for nil construction, no screen or native failure.
    fn install(&self, handle: HudHandle) -> Result<HudInstallInfo, HudInstallError>;

    /// Places, configures and orders front the panel for one press.
    ///
    /// `gate` is the commit check; the backend calls it immediately before
    /// ordering front and must not order when it returns `false`.
    ///
    /// # Errors
    /// Returns a typed failure; nothing is ordered front on error.
    fn show(
        &self,
        handle: HudHandle,
        command: &HudShowCommand,
        gate: &dyn Fn() -> bool,
    ) -> Result<HudShown, HudShowFailure>;

    /// Animates the panel out and then orders it out if it still shows
    /// `press` and no later show took over; a successor press is left
    /// untouched.
    fn hide(&self, handle: HudHandle, press: PressId);

    /// Stops the progress at its current on-screen width if the panel still
    /// displays `press`; the panel stays ordered front. Another press is left
    /// untouched.
    fn freeze(&self, handle: HudHandle, press: PressId);

    /// Orders out and drops every native object for `handle`; idempotent.
    ///
    /// # Errors
    /// Returns an error when objects could not be removed; they stay owned.
    fn release(&self, handle: HudHandle) -> Result<(), HudReleaseError>;
}

// ---------------------------------------------------------------------------
// Control.
// ---------------------------------------------------------------------------

enum Phase {
    Idle,
    Installing,
    Ready,
    Releasing,
    Released,
    Retained(HudRetainReason),
}

struct State {
    handle: HudHandle,
    phase: Phase,
    current: Option<PressId>,
    pending: BTreeMap<PressId, Option<Instant>>,
    install_error: Option<HudInstallError>,
    release_cleanup: HudCleanup,
}

struct Shared {
    executor: Arc<dyn MainThreadExecutor>,
    backend: Arc<dyn HudBackend>,
    budget: Duration,
    events: Sender<HudEvent>,
    state: Mutex<State>,
    changed: Condvar,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> { lock(&self.state) }

    fn send(&self, handle: HudHandle, press: PressId, outcome: HudOutcome) {
        // The receiver may be gone; results are best-effort plain data.
        let _ = self.events.send(HudEvent { handle, press, outcome });
    }

    /// Delivers `outcome` only if `press` is still pending.
    fn deliver(&self, handle: HudHandle, press: PressId, outcome: HudOutcome) -> bool {
        let mut state = self.state();
        if state.current == Some(press) && !matches!(outcome, HudOutcome::Shown(_)) {
            state.current = None;
        }
        let removed = state.pending.remove(&press).is_some();
        drop(state);
        if removed {
            self.send(handle, press, outcome);
        }
        removed
    }

    /// Expires overdue shows, emitting `TimedOut` and invalidating them.
    fn sweep(&self) {
        let now = Instant::now();
        let mut expired = Vec::new();
        let handle = {
            let mut state = self.state();
            let overdue: Vec<PressId> = state
                .pending
                .iter()
                .filter(|(_, deadline)| deadline.is_some_and(|d| d <= now))
                .map(|(press, _)| *press)
                .collect();
            for press in overdue {
                state.pending.remove(&press);
                if state.current == Some(press) {
                    state.current = None;
                }
                expired.push(press);
            }
            state.handle
        };
        for press in expired {
            self.send(handle, press, HudOutcome::Unavailable(HudUnavailable::TimedOut));
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.state().pending.values().flatten().min().copied()
    }

    fn run_backend<T>(call: impl FnOnce() -> T) -> Result<T, String> {
        catch_unwind(AssertUnwindSafe(call)).map_err(|payload| {
            payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .map_or_else(|| "native job panicked".to_owned(), |s| format!("panicked: {s}"))
        })
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(PoisonError::into_inner);
        if matches!(
            state.phase,
            Phase::Installing | Phase::Ready | Phase::Retained(_)
        ) {
            // Best-effort physical cleanup; if the dispatcher rejects it, the
            // owner stays accounted in the main-thread map, never forgotten.
            let backend = Arc::clone(&self.backend);
            let handle = state.handle;
            let _ = self.executor.execute(Box::new(move || {
                let _ = Self::run_backend(|| backend.release(handle));
            }));
        }
    }
}

/// Receives show results and expires unacknowledged shows.
///
/// The consumer calls [`HudEvents::recv_timeout`] or [`HudEvents::try_recv`];
/// either first expires every show whose acknowledgement is overdue, so no timer
/// thread exists.
pub struct HudEvents {
    shared: Weak<Shared>,
    receiver: Receiver<HudEvent>,
}

impl HudEvents {
    /// Returns a ready result, expiring overdue shows first.
    #[must_use]
    pub fn try_recv(&self) -> Option<HudEvent> {
        if let Some(shared) = self.shared.upgrade() {
            shared.sweep();
        }
        self.receiver.try_recv().ok()
    }

    /// Waits up to `timeout` for a result, expiring overdue shows as it goes.
    #[must_use]
    pub fn recv_timeout(&self, timeout: Duration) -> Option<HudEvent> {
        let end = Instant::now().checked_add(timeout);
        loop {
            if let Some(event) = self.try_recv() {
                return Some(event);
            }
            let now = Instant::now();
            let mut wait = end.map_or(timeout, |end| end.saturating_duration_since(now));
            if wait.is_zero() {
                return None;
            }
            if let Some(deadline) = self.shared.upgrade().and_then(|s| s.next_deadline()) {
                wait =
                    wait.min(deadline.saturating_duration_since(now).max(Duration::from_millis(1)));
            }
            match self.receiver.recv_timeout(wait) {
                Ok(event) => return Some(event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    /// Returns every ready result.
    #[must_use]
    pub fn drain(&self) -> Vec<HudEvent> { std::iter::from_fn(|| self.try_recv()).collect() }
}

impl fmt::Debug for HudEvents {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HudEvents").finish_non_exhaustive()
    }
}

/// The thread-safe handle to one HUD owner.
///
/// It holds only an executor, a backend trait object, an opaque [`HudHandle`]
/// and plain synchronization state; every native object stays in the
/// main-thread-local owner map. Cloning shares the same owner.
#[derive(Clone)]
pub struct HudControl {
    shared: Arc<Shared>,
}

impl HudControl {
    /// Creates a control over `backend` using the default acknowledgement
    /// budget. Nothing native is created or shown.
    #[must_use]
    pub fn new(
        executor: Arc<dyn MainThreadExecutor>,
        backend: Arc<dyn HudBackend>,
    ) -> (Self, HudEvents) {
        Self::with_acknowledgement_budget(executor, backend, ACKNOWLEDGEMENT_BUDGET)
    }

    /// Creates a control with the system backend.
    #[must_use]
    pub fn system(executor: Arc<dyn MainThreadExecutor>) -> (Self, HudEvents) {
        Self::new(executor, Arc::new(SystemHudBackend))
    }

    /// Creates a control with an explicit per-hop acknowledgement budget.
    #[must_use]
    pub fn with_acknowledgement_budget(
        executor: Arc<dyn MainThreadExecutor>,
        backend: Arc<dyn HudBackend>,
        budget: Duration,
    ) -> (Self, HudEvents) {
        let (events, receiver) = mpsc::channel();
        let shared = Arc::new(Shared {
            executor,
            backend,
            budget,
            events,
            state: Mutex::new(State {
                handle: HudHandle {
                    id: NEXT_HANDLE.fetch_add(1, Ordering::Relaxed),
                    generation: 1,
                },
                phase: Phase::Idle,
                current: None,
                pending: BTreeMap::new(),
                install_error: None,
                release_cleanup: HudCleanup::Queued,
            }),
            changed: Condvar::new(),
        });
        let events = HudEvents {
            shared: Arc::downgrade(&shared),
            receiver,
        };
        (Self { shared }, events)
    }

    /// The current owner handle. It changes after a failed installation.
    #[must_use]
    pub fn handle(&self) -> HudHandle { self.shared.state().handle }

    /// A diagnostic snapshot of the lifecycle.
    #[must_use]
    pub fn phase(&self) -> HudPhase {
        match &self.shared.state().phase {
            Phase::Idle => HudPhase::Idle,
            Phase::Installing => HudPhase::Installing,
            Phase::Ready => HudPhase::Ready,
            Phase::Releasing => HudPhase::Releasing,
            Phase::Released => HudPhase::Released,
            Phase::Retained(reason) => HudPhase::Retained(reason.clone()),
        }
    }

    /// Installs the hidden panel and waits (bounded) for the acknowledgement.
    ///
    /// Repeated calls reuse a pending or finished installation instead of
    /// queueing a duplicate panel. A caller on the main thread never waits and
    /// reports [`HudInstallError::TimedOut`] if the job is still queued.
    ///
    /// # Errors
    /// Returns a typed [`HudInstallError`]; success means a hidden panel exists
    /// and a screen was usable.
    pub fn install(&self, cancel: &CancellationToken) -> Result<HudHandle, HudInstallError> {
        let shared = &self.shared;
        let (handle, enqueue) = {
            let mut state = shared.state();
            match &state.phase {
                Phase::Ready => return Ok(state.handle),
                Phase::Installing => (state.handle, false),
                Phase::Idle => {
                    state.phase = Phase::Installing;
                    state.install_error = None;
                    (state.handle, true)
                }
                Phase::Releasing | Phase::Released => return Err(HudInstallError::Retired),
                Phase::Retained(reason) => {
                    return Err(HudInstallError::Retained(reason.clone()));
                }
            }
        };
        if enqueue {
            let job_shared = Arc::clone(shared);
            let queued =
                shared.executor.execute(Box::new(move || install_job(&job_shared, handle)));
            if queued.is_err() {
                let mut state = shared.state();
                if matches!(state.phase, Phase::Installing) && state.handle == handle {
                    state.phase = Phase::Idle;
                    state.handle.generation += 1;
                }
                drop(state);
                shared.changed.notify_all();
                return Err(HudInstallError::DispatchRejected);
            }
        }
        self.wait_installed(cancel)
    }

    fn wait_installed(&self, cancel: &CancellationToken) -> Result<HudHandle, HudInstallError> {
        let shared = &self.shared;
        let start = Instant::now();
        let mut state = shared.state();
        loop {
            match &state.phase {
                Phase::Ready => return Ok(state.handle),
                Phase::Idle => {
                    return Err(state.install_error.clone().unwrap_or(HudInstallError::Cancelled));
                }
                Phase::Releasing | Phase::Released => return Err(HudInstallError::Retired),
                Phase::Retained(reason) => return Err(HudInstallError::Retained(reason.clone())),
                Phase::Installing => {}
            }
            if cancel.is_cancelled() {
                return Err(HudInstallError::Cancelled);
            }
            let elapsed = start.elapsed();
            if shared.executor.is_main_thread() || elapsed >= shared.budget {
                return Err(HudInstallError::TimedOut);
            }
            let wait = CANCELLATION_QUANTUM.min(shared.budget.saturating_sub(elapsed));
            state = shared
                .changed
                .wait_timeout(state, wait)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Enqueues a show without waiting; the result arrives as a [`HudEvent`].
    pub fn show(&self, request: HudRequest) {
        let shared = &self.shared;
        let HudRequest {
            handle,
            press,
            label,
            display,
            duration_ms,
            elapsed_ms,
            authorization,
            anchor,
            motion,
        } = request;
        let deadline = Instant::now().checked_add(shared.budget);
        let mut superseded = Vec::new();
        let rejection = {
            let mut state = shared.state();
            let rejection = match &state.phase {
                Phase::Ready if state.handle == handle => None,
                Phase::Ready => Some(HudOutcome::Stale),
                Phase::Idle | Phase::Installing => {
                    Some(HudOutcome::Unavailable(HudUnavailable::NotReady))
                }
                Phase::Releasing | Phase::Released | Phase::Retained(_) => {
                    Some(HudOutcome::Unavailable(HudUnavailable::Retired))
                }
            };
            if rejection.is_none() {
                superseded = std::mem::take(&mut state.pending).into_keys().collect();
                state.current = Some(press);
                state.pending.insert(press, deadline);
            }
            rejection
        };
        for old in superseded {
            shared.send(handle, old, HudOutcome::Stale);
        }
        if let Some(outcome) = rejection {
            shared.send(handle, press, outcome);
            return;
        }
        let command = HudShowCommand {
            press,
            label,
            display,
            duration_ms,
            elapsed_ms,
            anchor,
            motion,
        };
        let job_shared = Arc::clone(shared);
        let queued = shared.executor.execute(Box::new(move || {
            show_job(&job_shared, handle, &command, &authorization);
        }));
        if queued.is_err() {
            shared.deliver(
                handle,
                press,
                HudOutcome::Unavailable(HudUnavailable::DispatchRejected),
            );
        }
    }

    /// Logically hides `press` immediately and enqueues the native order-out.
    ///
    /// The native job rechecks the handle and press, so an older hide cannot
    /// hide a successor press.
    pub fn hide(&self, handle: HudHandle, press: PressId) {
        let shared = &self.shared;
        let stale_pending = {
            let mut state = shared.state();
            if state.handle != handle || !matches!(state.phase, Phase::Ready) {
                return;
            }
            if state.current == Some(press) {
                state.current = None;
            }
            state.pending.remove(&press).is_some()
        };
        if stale_pending {
            shared.send(handle, press, HudOutcome::Stale);
        }
        let job_shared = Arc::clone(shared);
        // A rejected dispatch leaves the logical state invalidated; release
        // performs the physical order-out.
        let _ = shared.executor.execute(Box::new(move || {
            let ready = {
                let state = job_shared.state();
                state.handle == handle && matches!(state.phase, Phase::Ready)
            };
            if ready {
                let _ = Shared::run_backend(|| job_shared.backend.hide(handle, press));
            }
        }));
    }

    /// Enqueues a main-thread job that stops the progress of `press` at its
    /// current on-screen width while the panel stays visible.
    ///
    /// Never waits and never emits a [`HudEvent`]; `current` and `pending` are
    /// unchanged. The job rechecks the handle, the `Ready` phase and that
    /// `press` is still the current press, so it is a no-op for another handle,
    /// a successor press or a released control.
    pub fn freeze(&self, handle: HudHandle, press: PressId) {
        let shared = &self.shared;
        {
            let state = shared.state();
            if state.handle != handle || !matches!(state.phase, Phase::Ready) {
                return;
            }
        }
        let job_shared = Arc::clone(shared);
        let _ = shared.executor.execute(Box::new(move || {
            let live = {
                let state = job_shared.state();
                state.handle == handle
                    && matches!(state.phase, Phase::Ready)
                    && state.current == Some(press)
            };
            if live {
                let _ = Shared::run_backend(|| job_shared.backend.freeze(handle, press));
            }
        }));
    }

    /// Releases every native object and waits (bounded) for the acknowledgement.
    ///
    /// Invalidation is immediate. Physical cleanup is enqueued even when
    /// `cancel` is already cancelled; only the wait gives up. A timeout,
    /// cancellation, dispatch rejection or removal error reports
    /// [`HudReleaseOutcome::Retained`] with the exact owner; a queued cleanup
    /// may still finish exactly once, and calling again reuses it.
    #[must_use]
    pub fn release(&self, cancel: &CancellationToken) -> HudReleaseOutcome {
        let shared = &self.shared;
        let mut superseded = Vec::new();
        let (handle, enqueue) = {
            let mut state = shared.state();
            let handle = state.handle;
            let enqueue = match &state.phase {
                Phase::Released => return HudReleaseOutcome::Released,
                Phase::Idle => {
                    state.phase = Phase::Released;
                    drop(state);
                    shared.changed.notify_all();
                    return HudReleaseOutcome::Released;
                }
                Phase::Releasing => false,
                Phase::Installing | Phase::Ready | Phase::Retained(_) => {
                    state.phase = Phase::Releasing;
                    state.current = None;
                    superseded = std::mem::take(&mut state.pending).into_keys().collect();
                    true
                }
            };
            drop(state);
            (handle, enqueue)
        };
        for press in superseded {
            shared.send(handle, press, HudOutcome::Stale);
        }
        if enqueue {
            let job_shared = Arc::clone(shared);
            if shared
                .executor
                .execute(Box::new(move || release_job(&job_shared, handle)))
                .is_err()
            {
                shared.state().phase = Phase::Retained(HudRetainReason::DispatchRejected);
                shared.changed.notify_all();
                return HudReleaseOutcome::Retained(HudRetained {
                    handle,
                    reason: HudRetainReason::DispatchRejected,
                    cleanup: HudCleanup::Failed,
                });
            }
        }
        self.wait_released(handle, cancel)
    }

    fn wait_released(&self, handle: HudHandle, cancel: &CancellationToken) -> HudReleaseOutcome {
        let shared = &self.shared;
        let start = Instant::now();
        let mut state = shared.state();
        let retained = |reason: HudRetainReason, cleanup: HudCleanup| {
            HudReleaseOutcome::Retained(HudRetained { handle, reason, cleanup })
        };
        loop {
            match &state.phase {
                Phase::Released => return HudReleaseOutcome::Released,
                Phase::Retained(reason) => return retained(reason.clone(), HudCleanup::Failed),
                Phase::Releasing => {}
                Phase::Idle | Phase::Installing | Phase::Ready => {
                    return retained(
                        HudRetainReason::RemovalFailed("unexpected lifecycle state".to_owned()),
                        HudCleanup::Failed,
                    );
                }
            }
            if shared.executor.is_main_thread() {
                return retained(HudRetainReason::MainThreadCaller, HudCleanup::Queued);
            }
            if cancel.is_cancelled() {
                return retained(HudRetainReason::Cancelled, HudCleanup::Queued);
            }
            let elapsed = start.elapsed();
            if elapsed >= shared.budget {
                return retained(HudRetainReason::TimedOut, HudCleanup::Queued);
            }
            let wait = CANCELLATION_QUANTUM.min(shared.budget.saturating_sub(elapsed));
            state = shared
                .changed
                .wait_timeout(state, wait)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

impl fmt::Debug for HudControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.shared.state();
        f.debug_struct("HudControl")
            .field("handle", &state.handle)
            .field("current", &state.current)
            .finish_non_exhaustive()
    }
}

fn install_job(shared: &Arc<Shared>, handle: HudHandle) {
    {
        let state = shared.state();
        if !matches!(state.phase, Phase::Installing) || state.handle != handle {
            return;
        }
    }
    let result = match Shared::run_backend(|| shared.backend.install(handle)) {
        Ok(result) => result,
        Err(message) => Err(HudInstallError::Native(message)),
    };
    {
        let mut state = shared.state();
        if matches!(state.phase, Phase::Installing) && state.handle == handle {
            match result {
                Ok(_) => state.phase = Phase::Ready,
                Err(error) => {
                    state.phase = Phase::Idle;
                    state.install_error = Some(error);
                    state.handle.generation += 1;
                }
            }
        }
        // Otherwise a release raced ahead; its queued job owns the cleanup.
    }
    shared.changed.notify_all();
}

fn show_job(
    shared: &Arc<Shared>,
    handle: HudHandle,
    command: &HudShowCommand,
    authorization: &HudAuthorization,
) {
    let press = command.press;
    let committed = {
        let state = shared.state();
        state.handle == handle
            && matches!(state.phase, Phase::Ready)
            && state.current == Some(press)
            && state.pending.contains_key(&press)
    };
    if !committed || !authorization.is_authorized() {
        shared.deliver(handle, press, HudOutcome::Stale);
        return;
    }
    let gate = || {
        let current = {
            let state = shared.state();
            state.handle == handle
                && matches!(state.phase, Phase::Ready)
                && state.current == Some(press)
                && state.pending.contains_key(&press)
        };
        current && authorization.is_authorized()
    };
    let result = Shared::run_backend(|| shared.backend.show(handle, command, &gate));
    let outcome = match result {
        Ok(Ok(shown)) => {
            let delivered = {
                let mut state = shared.state();
                let live = state.handle == handle
                    && state.current == Some(press)
                    && state.pending.remove(&press).is_some();
                drop(state);
                live
            };
            if delivered {
                shared.send(handle, press, HudOutcome::Shown(shown));
            } else {
                // Hidden, released or timed out between ordering and delivery:
                // the late success is inert and must not stay on screen.
                let _ = Shared::run_backend(|| shared.backend.hide(handle, press));
            }
            return;
        }
        Ok(Err(HudShowFailure::Stale)) => HudOutcome::Stale,
        Ok(Err(HudShowFailure::NoScreen)) => HudOutcome::Unavailable(HudUnavailable::NoScreen),
        Ok(Err(HudShowFailure::NotInstalled)) => HudOutcome::Unavailable(HudUnavailable::NotReady),
        Ok(Err(HudShowFailure::Native(message))) | Err(message) => HudOutcome::Failed(message),
    };
    shared.deliver(handle, press, outcome);
}

fn release_job(shared: &Arc<Shared>, handle: HudHandle) {
    let result = match Shared::run_backend(|| shared.backend.release(handle)) {
        Ok(result) => result.map_err(|error| error.0),
        Err(message) => Err(message),
    };
    {
        let mut state = shared.state();
        if matches!(state.phase, Phase::Releasing) && state.handle == handle {
            match result {
                Ok(()) => state.phase = Phase::Released,
                Err(message) => {
                    state.release_cleanup = HudCleanup::Failed;
                    state.phase = Phase::Retained(HudRetainReason::RemovalFailed(message));
                }
            }
        }
    }
    shared.changed.notify_all();
}

// ---------------------------------------------------------------------------
// Native backend.
// ---------------------------------------------------------------------------

/// The production [`HudBackend`]; a stateless unit struct.
///
/// Native objects live in a main-thread-local owner map, never in this value.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemHudBackend;

struct NativePanel {
    panel: Retained<NSPanel>,
    effect: Retained<NSVisualEffectView>,
    label: Retained<NSTextField>,
    track: Retained<CALayer>,
    fill: Retained<CALayer>,
    displayed: Option<PressId>,
    /// Invalidates a pending hide completion when a show or order-out happens.
    generation: MotionGeneration,
    /// The start and end frames of the current show, so a hide animates back.
    frames: Cell<Option<(HudRect, HudRect)>>,
    /// The running `hold-progress` animation, if `apply` added one.
    progress: Cell<Option<ProgressRun>>,
}

/// What `apply` recorded about the progress animation it added, so a freeze
/// computes the width instead of reading what Core Animation has committed.
#[derive(Clone, Copy)]
struct ProgressRun {
    from: f64,
    seconds: f64,
    track_width: f64,
    /// `CACurrentMediaTime()` at the animation's explicit `beginTime`.
    start: f64,
}

thread_local! {
    static PANELS: RefCell<BTreeMap<HudHandle, NativePanel>> =
        const { RefCell::new(BTreeMap::new()) };
}

const fn ns_rect(rect: HudRect) -> NSRect {
    NSRect::new(
        NSPoint::new(rect.x, rect.y),
        NSSize::new(rect.width, rect.height),
    )
}

fn collect_screens(mtm: MainThreadMarker) -> HudScreens {
    let screens = NSScreen::screens(mtm)
        .to_vec()
        .iter()
        .map(|screen| {
            let frame = screen.frame();
            let visible = screen.visibleFrame();
            HudScreen {
                id: DisplayId::new(screen.CGDirectDisplayID()),
                frame: HudRect::new(
                    frame.origin.x,
                    frame.origin.y,
                    frame.size.width,
                    frame.size.height,
                ),
                visible: HudRect::new(
                    visible.origin.x,
                    visible.origin.y,
                    visible.size.width,
                    visible.size.height,
                ),
            }
        })
        .collect();
    let mouse = NSEvent::mouseLocation();
    HudScreens {
        screens,
        mouse: Some(HudPoint { x: mouse.x, y: mouse.y }),
        main: NSScreen::mainScreen(mtm).map(|screen| DisplayId::new(screen.CGDirectDisplayID())),
    }
}

fn read_appearance() -> HudAppearance {
    let workspace = NSWorkspace::sharedWorkspace();
    HudAppearance {
        increase_contrast: workspace.accessibilityDisplayShouldIncreaseContrast(),
        reduce_motion: workspace.accessibilityDisplayShouldReduceMotion(),
        reduce_transparency: workspace.accessibilityDisplayShouldReduceTransparency(),
    }
}

fn build_panel(mtm: MainThreadMarker) -> Result<NativePanel, HudInstallError> {
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        mtm.alloc::<NSPanel>(),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(MIN_WIDTH, HEIGHT)),
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        true,
    );
    // SAFETY: this module never calls `close`, and the panel is owned only by
    // the main-thread-local map. Disabling release-on-close guarantees the
    // window cannot over-release the single strong reference we hold.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setIgnoresMouseEvents(true);
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHasShadow(true);
    panel.setHidesOnDeactivate(false);
    panel.setBecomesKeyOnlyIfNeeded(true);
    panel.setLevel(NSStatusWindowLevel);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    panel.setAnimationBehavior(NSWindowAnimationBehavior::None);

    let effect = NSVisualEffectView::initWithFrame(
        mtm.alloc::<NSVisualEffectView>(),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(MIN_WIDTH, HEIGHT)),
    );
    effect.setMaterial(NSVisualEffectMaterial::HUDWindow);
    effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    effect.setState(NSVisualEffectState::Active);
    effect.setWantsLayer(true);
    let Some(root) = effect.layer() else {
        return Err(HudInstallError::NilConstruction("NSVisualEffectView.layer"));
    };
    root.setCornerRadius(14.0);
    root.setMasksToBounds(true);

    let label = NSTextField::labelWithString(ns_string!(""), mtm);
    label.setAlignment(NSTextAlignment::Center);
    label.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    label.setFont(Some(&NSFont::systemFontOfSize(13.0)));
    label.setTextColor(Some(&NSColor::labelColor()));
    effect.addSubview(&label);

    let track = CALayer::layer();
    let fill = CALayer::layer();
    root.addSublayer(&track);
    root.addSublayer(&fill);
    panel.setContentView(Some(&effect));
    Ok(NativePanel {
        panel,
        effect,
        label,
        track,
        fill,
        displayed: None,
        generation: MotionGeneration::default(),
        frames: Cell::new(None),
        progress: Cell::new(None),
    })
}

impl NativePanel {
    fn apply(&self, layout: &HudLayout, start: HudRect, style: HudStyle, plan: ProgressPlan) {
        self.fill.removeAllAnimations();
        self.progress.set(None);
        // A panel still showing a press (for example during a release linger)
        // is not blinked out: the entrance animation continues from where it
        // is. Otherwise jump to the show's start state (hidden, at the start
        // frame). Correctness never depends on this zero-duration group
        // cancelling a hide in flight: `animate_in` replaces the alpha and frame
        // animations in the same main-queue pass.
        if self.displayed.is_none() {
            run_group(
                Duration::ZERO,
                None,
                || {
                    self.panel.animator().setAlphaValue(0.0);
                    self.panel.animator().setFrame_display(ns_rect(start), false);
                },
                None,
            );
        }
        self.effect.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(layout.panel.width, layout.panel.height),
        ));
        self.label.setFrame(ns_rect(layout.text_frame));
        self.label.setStringValue(&NSString::from_str(&layout.text));

        let track = layout.track_frame;
        let radius = track.height / 2.0;
        let fill_color = if style.fill_uses_label_color {
            NSColor::labelColor()
        } else {
            NSColor::controlAccentColor()
        };
        let track_color = NSColor::labelColor().colorWithAlphaComponent(style.track_alpha);
        let fraction = match plan {
            ProgressPlan::Full => 1.0,
            ProgressPlan::Static(fraction) => fraction,
            ProgressPlan::Animate { from, .. } => from,
        };
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.track.setFrame(ns_rect(track));
        self.track.setCornerRadius(radius);
        self.track.setBackgroundColor(Some(&track_color.CGColor()));
        self.fill.setAnchorPoint(NSPoint::new(0.0, 0.5));
        self.fill.setPosition(NSPoint::new(track.x, track.y + track.height / 2.0));
        // The model value is the final width for an animation, so the layer
        // rests on full when Core Animation finishes.
        let model_fraction = if matches!(plan, ProgressPlan::Animate { .. }) {
            1.0
        } else {
            fraction
        };
        self.fill.setBounds(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(track.width * model_fraction, track.height),
        ));
        self.fill.setCornerRadius(radius);
        self.fill.setBackgroundColor(Some(
            &fill_color.colorWithAlphaComponent(style.fill_alpha).CGColor(),
        ));
        CATransaction::commit();

        if let ProgressPlan::Animate { from, seconds } = plan {
            let animation =
                CABasicAnimation::animationWithKeyPath(Some(ns_string!("bounds.size.width")));
            // SAFETY: `bounds.size.width` animates a CGFloat, and both values are
            // `NSNumber` doubles, the exact type Core Animation interpolates.
            unsafe {
                animation.setFromValue(Some(&NSNumber::new_f64(track.width * from)));
                animation.setToValue(Some(&NSNumber::new_f64(track.width)));
            }
            animation.setDuration(seconds);
            // The explicit begin time makes the animation start exactly where
            // the recorded `start` says, so a freeze needs no committed
            // presentation state.
            let start = CACurrentMediaTime();
            animation.setBeginTime(start);
            self.progress.set(Some(ProgressRun {
                from,
                seconds,
                track_width: track.width,
                start,
            }));
            animation.setTimingFunction(Some(&CAMediaTimingFunction::functionWithName(
                // SAFETY: reading an immutable CoreAnimation string constant.
                unsafe { kCAMediaTimingFunctionLinear },
            )));
            self.fill.addAnimation_forKey(&animation, Some(ns_string!("hold-progress")));
        }
    }

    /// Stops the progress bar at the width it has reached now.
    ///
    /// The width is computed from the animation `apply` recorded (start time,
    /// `from` and duration), so it is exact even when this runs in the same main
    /// queue pass as the show and Core Animation has not committed the
    /// animation yet. A panel with no animation (a full or static fill) keeps
    /// its model width.
    fn freeze_progress(&self) {
        let Some(run) = self.progress.take() else {
            return;
        };
        let now = CACurrentMediaTime();
        let fraction = progress_fraction_at(run.from, run.seconds, now - run.start);
        let mut bounds = self.fill.bounds();
        bounds.size.width = run.track_width * fraction;
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.fill.setBounds(bounds);
        CATransaction::commit();
        self.fill.removeAnimationForKey(ns_string!("hold-progress"));
    }

    /// Orders out immediately with no animation, invalidating any pending hide.
    fn order_out(&mut self) {
        self.generation.advance();
        self.fill.removeAllAnimations();
        self.progress.set(None);
        self.frames.set(None);
        self.panel.orderOut(None);
        self.displayed = None;
    }

    /// Animates alpha 1 and the final frame in after ordering front.
    fn animate_in(&self, end: HudRect) {
        run_group(
            SHOW_DURATION,
            // SAFETY: reading an immutable CoreAnimation string constant.
            Some(unsafe { kCAMediaTimingFunctionEaseOut }),
            || {
                self.panel.animator().setAlphaValue(1.0);
                self.panel.animator().setFrame_display(ns_rect(end), false);
            },
            None,
        );
    }
}

/// Runs `changes` in an `NSAnimationContext` group with `duration`, an optional
/// timing function and an optional completion handler.
///
/// The group, the context and the completion handler all run on the main
/// thread; a zero duration applies the animator changes immediately. Whether it
/// also interrupts an animation in flight is not relied on: a following group
/// on the same properties replaces it.
fn run_group(
    duration: Duration,
    timing: Option<&'static objc2_quartz_core::CAMediaTimingFunctionName>,
    changes: impl Fn(),
    completion: Option<&RcBlock<dyn Fn()>>,
) {
    let changes = RcBlock::new(move |_context: std::ptr::NonNull<NSAnimationContext>| {
        let context = NSAnimationContext::currentContext();
        context.setDuration(duration.as_secs_f64());
        if let Some(name) = timing {
            context.setTimingFunction(Some(&CAMediaTimingFunction::functionWithName(name)));
        }
        changes();
    });
    NSAnimationContext::runAnimationGroup_completionHandler(&changes, completion.map(|c| &**c));
}

/// Completes an animated hide: orders the panel out only while `token` is the
/// panel's current generation (no show, order-out or newer hide since).
///
/// If `AppKit` runs the completion synchronously while the map is already
/// borrowed (inside a show, hide, freeze or release on this thread), the hide
/// is stale by construction (show and order-out advance the generation first),
/// so it returns instead of panicking inside an Objective-C callback.
fn finish_hide(handle: HudHandle, token: u64) {
    PANELS.with(|panels| {
        let Ok(mut panels) = panels.try_borrow_mut() else {
            return;
        };
        if let Some(panel) = panels.get_mut(&handle)
            && panel.generation.is_current(token)
        {
            panel.order_out();
        }
    });
}

impl HudBackend for SystemHudBackend {
    fn install(&self, handle: HudHandle) -> Result<HudInstallInfo, HudInstallError> {
        let mtm = MainThreadMarker::new().ok_or(HudInstallError::NotMainThread)?;
        let screens = collect_screens(mtm);
        let usable = screens.screens.iter().any(|screen| {
            screen.visible.sanitized().width > 0.0 && screen.visible.sanitized().height > 0.0
        });
        if !usable {
            return Err(HudInstallError::NoScreen);
        }
        let reuse = PANELS.with_borrow(|panels| panels.contains_key(&handle));
        if !reuse {
            let panel = build_panel(mtm)?;
            PANELS.with_borrow_mut(|panels| panels.insert(handle, panel));
        }
        Ok(HudInstallInfo { screens: screens.screens.len() })
    }

    fn show(
        &self,
        handle: HudHandle,
        command: &HudShowCommand,
        gate: &dyn Fn() -> bool,
    ) -> Result<HudShown, HudShowFailure> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| HudShowFailure::Native("not on the main thread".to_owned()))?;
        let screens = collect_screens(mtm);
        let (screen, source) = select_screen(&screens, command.display)
            .map_err(|NoScreen| HudShowFailure::NoScreen)?;
        let layout = compute_layout(screen.visible, command.label.as_deref(), command.anchor);
        let appearance = read_appearance();
        let plan = progress_plan(command.elapsed_ms, command.duration_ms);
        let (start, end) = motion_frames(
            layout.panel,
            command.anchor,
            command.motion,
            appearance.reduce_motion,
        );
        PANELS.with_borrow_mut(|panels| {
            let panel = panels.get_mut(&handle).ok_or(HudShowFailure::NotInstalled)?;
            // A show supersedes any hide completion still pending.
            panel.generation.advance();
            panel.apply(&layout, start, style_for(appearance), plan);
            if !gate() {
                panel.order_out();
                return Err(HudShowFailure::Stale);
            }
            // Ordering front never makes the panel key/main or activates Stache.
            panel.panel.orderFrontRegardless();
            panel.frames.set(Some((start, end)));
            panel.animate_in(end);
            panel.displayed = Some(command.press);
            Ok(HudShown {
                placement: HudPlacement { display: screen.id, source },
                text: layout.text.clone(),
                panel: layout.panel,
                plan,
                appearance,
            })
        })
    }

    fn hide(&self, handle: HudHandle, press: PressId) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        // Decide under the borrow, then animate outside it: a completion
        // handler that ran synchronously must be able to borrow the map.
        let hiding = PANELS.with_borrow_mut(|panels| {
            let panel = panels.get_mut(&handle)?;
            if panel.displayed != Some(press) {
                return None;
            }
            panel.displayed = None;
            let token = panel.generation.advance();
            let start = panel.frames.get().map(|(start, _)| start);
            Some((panel.panel.clone(), token, start))
        });
        let Some((window, token, start)) = hiding else {
            return;
        };
        let completion = RcBlock::new(move || finish_hide(handle, token));
        run_group(
            HIDE_DURATION,
            // SAFETY: reading an immutable CoreAnimation string constant.
            Some(unsafe { kCAMediaTimingFunctionEaseIn }),
            || {
                window.animator().setAlphaValue(0.0);
                if let Some(start) = start {
                    window.animator().setFrame_display(ns_rect(start), false);
                }
            },
            Some(&completion),
        );
    }

    fn freeze(&self, handle: HudHandle, press: PressId) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        PANELS.with_borrow_mut(|panels| {
            if let Some(panel) = panels.get_mut(&handle)
                && panel.displayed == Some(press)
            {
                panel.freeze_progress();
            }
        });
    }

    fn release(&self, handle: HudHandle) -> Result<(), HudReleaseError> {
        if MainThreadMarker::new().is_none() {
            return Err(HudReleaseError("release ran off the main thread".to_owned()));
        }
        let removed = PANELS.with_borrow_mut(|panels| panels.remove(&handle));
        if let Some(mut owner) = removed {
            owner.order_out();
            owner.track.removeFromSuperlayer();
            owner.fill.removeFromSuperlayer();
            owner.panel.setContentView(None);
            // Dropping here releases every object on the main thread.
        }
        Ok(())
    }
}
