//! Passive drag: the listen-only observer, its signals and the pure drag
//! geometry.
//!
//! Tiling observes mouse down and up through the shared input service's
//! **listen-only** tap (`InputCapability::subscribe_observer`). The observer
//! callback only turns the event into a pure [`DragSignal`] and hands it to the
//! actor's bounded drag lane (`Ingress::submit`); it never reads a
//! window, never blocks on native code and has no way to suppress, intercept or
//! inject an event or to change HQ > MA > KB priority (the observer trait
//! returns nothing). Moved and dragged events are masked by the service and not
//! needed: the gesture is recovered from window-frame snapshots.
//!
//! The actor owns the gesture ([`crate::state::DragState`]). Everything that
//! decides a drag outcome is a pure function in this module so the legacy
//! thresholds are testable with exact fixtures:
//!
//! - [`select_dragged`]: legacy `find_drag_swap_target` (`init.rs:1894–1922`),
//!   the window with the greatest centre travel strictly above
//!   [`MIN_DRAG_DISTANCE`].
//! - [`select_target`]: legacy containment (`init.rs:1938–1941`), the first
//!   original frame in stable order that inclusively contains the dropped centre.
//! - [`resized_window`]: legacy `find_resized_window` (`init.rs:1952–1981`).
//! - [`ratio_move`]: legacy `on_user_resize_completed` (`resize.rs:746–910`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use stache_sdk::InputCapability;
use stache_sdk::input::{
    InputError, InputEvent, InputEventKind, InputFilter, InputObserver, KindSet, MouseButton,
    NativeCleanup, ObserverSpec, PressStream, Subscription,
};

use crate::config::LayoutType;
use crate::events::Ingress;
use crate::geometry::Rect;
use crate::intent::ResizeDimension;
use crate::state::Input;

/// Drag signals queued for the actor before the lane drops and reports a lost
/// observation.
pub const DRAG_QUEUE_CAPACITY: usize = 32;
/// Centre travel in logical points above which a window counts as dragged
/// (legacy `MIN_DRAG_DISTANCE`). Exactly this distance is not a drag.
pub const MIN_DRAG_DISTANCE: f64 = 50.0;
/// Combined width and height change in logical points above which a window
/// counts as resized (legacy `find_resized_window`).
pub const MIN_RESIZE_CHANGE: f64 = 5.0;
/// A gesture older than this is abandoned (the actor injects a lost
/// observation). No missing up is ever invented.
pub const GESTURE_TIMEOUT: Duration = Duration::from_secs(60);
/// The left mouse button number.
pub const LEFT_BUTTON: u32 = 0;
/// The right mouse button number.
pub const RIGHT_BUTTON: u32 = 1;

/// A point in global display coordinates (Quartz and AX share this space).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragPoint {
    /// Horizontal coordinate.
    pub x: f64,
    /// Vertical coordinate.
    pub y: f64,
}

/// Why observation of the gesture was lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LostCause {
    /// The bounded drag lane dropped a signal.
    Overflow,
    /// The listen-only tap was disabled or its epoch advanced: events may be
    /// missing.
    Discontinuity,
    /// An event carried no usable location.
    Malformed,
    /// Accessibility was lost.
    PermissionLost,
    /// The gesture outlived [`GESTURE_TIMEOUT`].
    Timeout,
    /// The runtime is stopping or the observer was removed.
    Stopped,
}

/// A pure value the observer callback enqueues. Nothing here refers to a
/// window or to native state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DragSignal {
    /// A mouse button went down.
    Down {
        /// The button number.
        button: u32,
        /// Where, in global display coordinates.
        at: DragPoint,
        /// The input service epoch of the event.
        epoch: u64,
        /// The event's monotonic time.
        time: Duration,
    },
    /// A mouse button went up.
    Up {
        /// The button number.
        button: u32,
        /// Where, in global display coordinates.
        at: DragPoint,
        /// The input service epoch of the event.
        epoch: u64,
        /// The event's monotonic time.
        time: Duration,
    },
    /// Observation is lost; the current gesture, if any, is void.
    Lost(LostCause),
    /// The passive observer could not be installed (drag is degraded): no
    /// gesture will ever be seen, so a user resize is indistinguishable from a
    /// window re-asserting its size.
    Unavailable,
}

// ---------------------------------------------------------------- geometry

/// The centre of `rect`.
#[must_use]
pub fn center(rect: &Rect) -> DragPoint {
    DragPoint {
        x: rect.x + rect.width / 2.0,
        y: rect.y + rect.height / 2.0,
    }
}

/// Whether `rect` contains `point`, edges included (legacy `>=`/`<=`).
#[must_use]
pub fn contains_inclusive(rect: &Rect, point: DragPoint) -> bool {
    point.x >= rect.x
        && point.x <= rect.x + rect.width
        && point.y >= rect.y
        && point.y <= rect.y + rect.height
}

/// Picks the dragged window: the one with the greatest centre travel whose
/// distance is strictly above [`MIN_DRAG_DISTANCE`]. `originals` is in stable
/// workspace order; an equal distance keeps the earlier window. A window with no
/// current frame is skipped. Returns the key and its current frame.
#[must_use]
pub fn select_dragged<K: Copy + PartialEq>(
    originals: &[(K, Rect)],
    currents: &[(K, Rect)],
) -> Option<(K, Rect)> {
    let mut best: Option<(K, Rect)> = None;
    let mut max_distance = 0.0_f64;
    for (key, original) in originals {
        let Some((_, current)) = currents.iter().find(|(held, _)| held == key) else {
            continue;
        };
        let from = center(original);
        let to = center(current);
        let distance = (to.x - from.x).hypot(to.y - from.y);
        if distance > max_distance && distance > MIN_DRAG_DISTANCE {
            max_distance = distance;
            best = Some((*key, *current));
        }
    }
    best
}

/// Picks the swap target: the first original frame, in stable order, that
/// inclusively contains `dropped` (the dragged window's current centre),
/// ignoring the dragged window.
#[must_use]
pub fn select_target<K: Copy + PartialEq>(
    originals: &[(K, Rect)],
    dragged: K,
    dropped: DragPoint,
) -> Option<K> {
    originals
        .iter()
        .find(|(key, original)| *key != dragged && contains_inclusive(original, dropped))
        .map(|(key, _)| *key)
}

/// Picks the resized window: the greatest combined width and height change,
/// strictly above [`MIN_RESIZE_CHANGE`]. Returns the key with its old and new
/// frame.
#[must_use]
pub fn resized_window<K: Copy + PartialEq>(
    originals: &[(K, Rect)],
    currents: &[(K, Rect)],
) -> Option<(K, Rect, Rect)> {
    let mut best: Option<(K, Rect, Rect)> = None;
    let mut max_change = 0.0_f64;
    for (key, original) in originals {
        let Some((_, current)) = currents.iter().find(|(held, _)| held == key) else {
            continue;
        };
        let change =
            (current.width - original.width).abs() + (current.height - original.height).abs();
        if change > max_change {
            max_change = change;
            best = Some((*key, *original, *current));
        }
    }
    best.filter(|_| max_change > MIN_RESIZE_CHANGE)
}

/// What a finished resize asks of the workspace ratios.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RatioMove {
    /// Move the master share. `delta` is the size change over the usable
    /// extent of `dimension`, positive when the window grew; the caller flips it
    /// for a stack window.
    Master {
        /// The dimension the master boundary lies along.
        dimension: ResizeDimension,
        /// The unsigned-by-position ratio delta.
        delta: f64,
    },
    /// Move ratio `index` by the signed `delta`.
    Split {
        /// The ratio to move.
        index: usize,
        /// The signed delta.
        delta: f64,
    },
    /// This layout or geometry has no ratio to move: only replan.
    Relayout,
    /// Nothing can be resized (fewer than two tiles): change nothing.
    Nothing,
}

/// The facts [`ratio_move`] needs about the workspace.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResizeContext {
    /// The layout in effect.
    pub layout: LayoutType,
    /// The resized window's index among the tiles.
    pub position: usize,
    /// How many tiles there are.
    pub count: usize,
    /// Usable area width.
    pub width: f64,
    /// Usable area height.
    pub height: f64,
    /// Whether the master boundary lies along the width (master left or right).
    pub master_uses_width: bool,
}

/// Maps a finished resize onto a ratio change (legacy `on_user_resize_completed`,
/// with the usable area standing in for the screen's visible frame).
#[must_use]
pub fn ratio_move(context: &ResizeContext, old: &Rect, new: &Rect) -> RatioMove {
    let width_delta = new.width - old.width;
    let height_delta = new.height - old.height;
    if matches!(context.layout, LayoutType::Floating | LayoutType::Monocle) {
        return RatioMove::Relayout;
    }
    if context.count <= 1 {
        return RatioMove::Nothing;
    }
    if !(context.width > 0.0 && context.height > 0.0) {
        return RatioMove::Relayout;
    }
    let width_dominates = width_delta.abs() > height_delta.abs();
    let ratio_delta = if width_dominates {
        width_delta / context.width
    } else {
        height_delta / context.height
    };
    let last = context.count - 1;
    match context.layout {
        LayoutType::Master => {
            if context.master_uses_width {
                RatioMove::Master {
                    dimension: ResizeDimension::Width,
                    delta: width_delta / context.width,
                }
            } else {
                RatioMove::Master {
                    dimension: ResizeDimension::Height,
                    delta: height_delta / context.height,
                }
            }
        }
        LayoutType::Dwindle => {
            let index = if context.position == 0 {
                0
            } else {
                (context.position - 1).min(context.count.saturating_sub(2))
            };
            RatioMove::Split { index, delta: ratio_delta }
        }
        LayoutType::Grid => {
            if width_dominates {
                RatioMove::Split { index: 0, delta: ratio_delta }
            } else {
                RatioMove::Relayout
            }
        }
        LayoutType::Split | LayoutType::SplitHorizontal | LayoutType::SplitVertical => {
            if context.position >= last {
                RatioMove::Split {
                    index: context.position - 1,
                    delta: -ratio_delta,
                }
            } else {
                RatioMove::Split {
                    index: context.position,
                    delta: ratio_delta,
                }
            }
        }
        LayoutType::Floating | LayoutType::Monocle => RatioMove::Relayout,
    }
}

// ---------------------------------------------------------------- observer

/// The passive callback. Cheap, bounded and non-panicking: it builds a value
/// and calls [`Ingress::submit`], which takes one short lock and never blocks on
/// native code or the actor's work.
pub struct DragObserver {
    ingress: Arc<Ingress>,
    closed: AtomicBool,
}

impl DragObserver {
    const fn new(ingress: Arc<Ingress>) -> Self {
        Self {
            ingress,
            closed: AtomicBool::new(false),
        }
    }

    fn send(&self, signal: DragSignal) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        // A closed mailbox means the actor is gone; nobody is waiting.
        let _ = self.ingress.submit(Input::Drag(signal));
    }
}

const fn button_of(event: &InputEvent) -> Option<u32> {
    match event.stream() {
        PressStream::Button(MouseButton(button)) => Some(button),
        PressStream::Key(_) => None,
    }
}

impl InputObserver for DragObserver {
    fn on_event(&self, event: &InputEvent) {
        let Some(button) = button_of(event) else { return };
        let signal = event.location().filter(|p| p.x.is_finite() && p.y.is_finite()).map(|p| {
            let at = DragPoint { x: p.x, y: p.y };
            let (epoch, time) = (event.press().epoch.0, event.timestamp());
            match event.kind() {
                InputEventKind::MouseDown => Some(DragSignal::Down { button, at, epoch, time }),
                InputEventKind::MouseUp => Some(DragSignal::Up { button, at, epoch, time }),
                _ => None,
            }
        });
        match (signal, event.kind()) {
            (Some(Some(signal)), _) => self.send(signal),
            (_, InputEventKind::MouseDown | InputEventKind::MouseUp) => {
                self.send(DragSignal::Lost(LostCause::Malformed));
            }
            _ => {}
        }
    }

    fn on_discontinuity(&self) { self.send(DragSignal::Lost(LostCause::Discontinuity)); }
}

/// The observer filter: mouse down and up of the left and right buttons.
#[must_use]
pub fn filter() -> InputFilter {
    InputFilter::new(KindSet::of(InputEventKind::MouseDown).with(InputEventKind::MouseUp))
        .with_buttons([MouseButton(LEFT_BUTTON), MouseButton(RIGHT_BUTTON)])
}

/// Why the passive drag observer is not running. Drag is degraded; ordinary
/// tiling is unaffected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DragUnavailable(pub String);

impl std::fmt::Display for DragUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}

/// The subscribed observer. Dropping it unsubscribes.
pub struct DragFeed {
    observer: Arc<DragObserver>,
    subscription: Mutex<Option<Subscription>>,
}

impl DragFeed {
    /// Subscribes the observer to the listen-only tap. Input Monitoring is not a
    /// typed permission, so a tap that cannot be installed shows up here as an
    /// error.
    ///
    /// # Errors
    ///
    /// [`DragUnavailable`] when the shared input service is not attached or the
    /// subscription failed. Nothing is retained.
    pub fn subscribe(
        input: &InputCapability,
        ingress: Arc<Ingress>,
    ) -> Result<Self, DragUnavailable> {
        if !input.is_available() {
            return Err(DragUnavailable(
                "drag is unavailable: the shared input service is not attached".to_owned(),
            ));
        }
        let observer = Arc::new(DragObserver::new(ingress));
        let subscription = input
            .subscribe_observer(
                ObserverSpec { filter: filter() },
                Arc::clone(&observer) as Arc<dyn InputObserver>,
            )
            .map_err(|failure| {
                DragUnavailable(match failure {
                    InputError::Unavailable => {
                        "drag is unavailable: the shared input service is not attached".to_owned()
                    }
                    other => format!(
                        "drag is unavailable: the listen-only mouse observer could not be \
                         installed ({other}); Input Monitoring may be missing"
                    ),
                })
            })?;
        Ok(Self {
            observer,
            subscription: Mutex::new(Some(subscription)),
        })
    }

    /// Removes the observer: callbacks stop at once, then the service closes
    /// the listen-only tap if this was its last passive subscriber. Idempotent.
    /// Returns a note when the service retained native resources.
    pub fn stop(&self) -> Option<String> {
        self.observer.send(DragSignal::Lost(LostCause::Stopped));
        self.observer.closed.store(true, Ordering::SeqCst);
        let subscription =
            self.subscription.lock().unwrap_or_else(PoisonError::into_inner).take()?;
        match subscription.unsubscribe().native {
            NativeCleanup::Retained(retained) => Some(format!(
                "the listen-only tap cleanup is retained by the input service: {retained:?}"
            )),
            _ => None,
        }
    }
}
