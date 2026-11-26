//! Passive drag: the listen-only observer, its signals and the pure drag
//! geometry.
//!
//! A gesture starts at the first move or resize observation of a tiled window
//! while a mouse button is physically held ([`ButtonState`], legacy
//! `on_batched_geometry_updates`), or earlier at a mouse down when the tap
//! delivers one. It ends at the tap's up or, when the tap misses it, when the
//! button state shows the release. The geometry path works with the tap dead.
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
//! - [`edge_resize`]: the edge-aware replacement of legacy
//!   `on_user_resize_completed`: the edge the user dragged moves the boundary
//!   on that side.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use stache_sdk::InputCapability;
use stache_sdk::input::{
    InertReason, InputError, InputEvent, InputEventKind, InputFilter, InputObserver, KindSet,
    MouseButton, NativeCleanup, ObserverSpec, PressStream, Subscription, TapKind, TapStatus,
};

use crate::config::{LayoutType, MasterPosition};
use crate::events::Ingress;
use crate::geometry::Rect;
use crate::layout::{
    Gaps, LayoutRequest, PlanStatus, WindowEntry, clamp_master_ratio, grid_ratio_axes,
    grid_resolved_ratios, has_valid_cumulative_ratios, plan_layout,
};
use crate::minimum_size::{Size, VIOLATION_TOLERANCE};
use crate::state::Input;

#[allow(clippy::cast_precision_loss)]
const fn count_f64(count: usize) -> f64 { count as f64 }

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
/// How often the actor checks the button state while a gesture is held and no
/// up has arrived, so a missed up ends the gesture within about two periods.
/// No check runs while no gesture exists.
pub const RELEASE_POLL: Duration = Duration::from_millis(50);
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
    /// The button state shows no mouse button held while a gesture waits for
    /// its up: the tap missed it. Ends the gesture like the up.
    Released,
    /// Observation is lost; the current gesture, if any, is void.
    Lost(LostCause),
    /// The passive observer could not be installed (drag is degraded): no
    /// gesture will ever be seen, so a user resize is indistinguishable from a
    /// window re-asserting its size.
    Unavailable,
}

// ------------------------------------------------------------ button state

/// Whether a mouse button is physically held, read without the event tap.
pub trait ButtonState: Send + Sync {
    /// The left or right button is held right now.
    fn held(&self) -> bool;
}

/// The system button state: `CGEventSourceButtonState` over the combined
/// session state (no permission needed).
pub struct SystemButtons;

impl ButtonState for SystemButtons {
    fn held(&self) -> bool {
        stache_macos::input::mouse_button_held(LEFT_BUTTON)
            || stache_macos::input::mouse_button_held(RIGHT_BUTTON)
    }
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

/// A window edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    /// The left edge.
    Left,
    /// The right edge.
    Right,
    /// The top edge.
    Top,
    /// The bottom edge.
    Bottom,
}

impl Edge {
    const ALL: [Self; 4] = [Self::Left, Self::Right, Self::Top, Self::Bottom];

    /// Where this edge of `rect` lies.
    #[must_use]
    pub fn of(self, rect: &Rect) -> f64 {
        match self {
            Self::Left => rect.x,
            Self::Right => rect.x + rect.width,
            Self::Top => rect.y,
            Self::Bottom => rect.y + rect.height,
        }
    }
}

/// The edges of a resized window that moved by more than
/// [`MIN_RESIZE_CHANGE`], in left, right, top, bottom order.
#[must_use]
pub fn moved_edges(old: &Rect, new: &Rect) -> Vec<Edge> {
    Edge::ALL
        .into_iter()
        .filter(|edge| (edge.of(new) - edge.of(old)).abs() > MIN_RESIZE_CHANGE)
        .collect()
}

/// Smallest share a lane of cumulative boundaries keeps (the ratio floor).
const RATIO_FLOOR: f64 = 0.05;
/// Ratio step used to find which boundary moves an edge.
const PROBE: f64 = 1e-3;
/// Bisection steps when solving a boundary position.
const BISECTIONS: usize = 48;

/// The facts [`edge_resize`] needs about the workspace.
#[derive(Clone, Copy, Debug)]
pub struct ResizeRequest<'a> {
    /// The layout in effect.
    pub layout: LayoutType,
    /// How many tiles there are.
    pub count: usize,
    /// The resized window's index among the tiles.
    pub position: usize,
    /// The usable area.
    pub usable: Rect,
    /// Gaps.
    pub gaps: Gaps,
    /// The configured master position.
    pub master_position: MasterPosition,
    /// The effective master share.
    pub master_ratio: f64,
    /// The workspace's effective runtime ratios (may be empty).
    pub ratios: &'a [f64],
    /// Each tile's effective minimum size, by index.
    pub minimums: &'a [Option<Size>],
}

/// What a finished mouse resize does to the workspace ratios.
#[derive(Clone, Debug, PartialEq)]
pub enum EdgeResize {
    /// Boundaries moved: the new ratios (complete for the layout and count)
    /// and master share.
    Moved {
        /// The new runtime ratios.
        ratios: Vec<f64>,
        /// The new master share (unchanged outside Master).
        master_ratio: f64,
    },
    /// A moved edge has a boundary, but the clamps left every boundary where
    /// it was.
    Unchanged,
    /// No moved edge lies on a boundary (outer edges, Monocle, Floating, a
    /// single tile): only replan.
    NoBoundary,
}

/// The workspace's ratios as one parameter vector (Master appends its share),
/// with the runs of cumulative boundaries that bound each parameter.
struct Parameters {
    values: Vec<f64>,
    /// `(range, lowest first value, highest first value)`.
    axes: Vec<(std::ops::Range<usize>, f64, f64)>,
    /// How many leading values are runtime ratios.
    ratio_len: usize,
}

fn parameters(request: &ResizeRequest<'_>) -> Parameters {
    let count = request.count;
    let landscape = request.usable.width >= request.usable.height;
    let cumulative = |ratios: &[f64], lanes: usize| {
        if has_valid_cumulative_ratios(ratios, lanes) {
            ratios.to_vec()
        } else {
            (1..lanes).map(|i| count_f64(i) / count_f64(lanes)).collect()
        }
    };
    let ceiling = 1.0 - RATIO_FLOOR;
    match request.layout {
        LayoutType::Dwindle => {
            let values: Vec<f64> = (0..count - 1)
                .map(|i| {
                    request
                        .ratios
                        .get(i)
                        .copied()
                        .filter(|r| r.is_finite())
                        .map_or(0.5, |r| r.clamp(0.05, 0.95))
                })
                .collect();
            let axes = (0..values.len()).map(|i| (i..i + 1, 0.1, 0.9)).collect();
            Parameters {
                ratio_len: values.len(),
                values,
                axes,
            }
        }
        LayoutType::Grid => {
            let values = grid_resolved_ratios(count, landscape, request.ratios);
            let mut start = 0;
            let axes = grid_ratio_axes(count, landscape)
                .into_iter()
                .map(|(len, low, high)| {
                    start += len;
                    (start - len..start, low, high)
                })
                .collect();
            Parameters {
                ratio_len: values.len(),
                values,
                axes,
            }
        }
        LayoutType::Master => {
            let stack = count - 1;
            let mut values = cumulative(request.ratios, stack);
            let ratio_len = values.len();
            values.push(clamp_master_ratio(request.master_ratio));
            Parameters {
                values,
                axes: vec![
                    (0..ratio_len, RATIO_FLOOR, ceiling),
                    (ratio_len..ratio_len + 1, 0.1, 0.9),
                ],
                ratio_len,
            }
        }
        _ => {
            let values = cumulative(request.ratios, count);
            Parameters {
                ratio_len: values.len(),
                axes: vec![(0..values.len(), RATIO_FLOOR, ceiling)],
                values,
            }
        }
    }
}

impl Parameters {
    /// The range parameter `k` may take while its run stays strictly
    /// increasing with every lane at least [`RATIO_FLOOR`] wide.
    fn bounds(&self, k: usize) -> (f64, f64) {
        let Some((range, low, high)) = self.axes.iter().find(|(range, ..)| range.contains(&k))
        else {
            return (self.values[k], self.values[k]);
        };
        let first = k == range.start;
        let lowest = if first {
            *low
        } else {
            self.values[k - 1] + RATIO_FLOOR
        };
        let mut highest = if first { *high } else { 1.0 - RATIO_FLOOR };
        if k + 1 < range.end {
            highest = highest.min(self.values[k + 1] - RATIO_FLOOR);
        }
        (lowest, highest)
    }
}

/// Plans the workspace with `values`; `None` when the plan is infeasible.
fn plan_with(
    request: &ResizeRequest<'_>,
    parameters: &Parameters,
    values: &[f64],
) -> Option<Vec<Rect>> {
    let entries: Vec<WindowEntry<usize>> = (0..request.count).map(WindowEntry::new).collect();
    let master = values.get(parameters.ratio_len).copied().unwrap_or(request.master_ratio);
    let plan = plan_layout(&LayoutRequest {
        layout: request.layout,
        windows: &entries,
        usable: request.usable,
        gaps: request.gaps,
        master_ratio: master,
        master_position: request.master_position,
        ratios: &values[..parameters.ratio_len],
    });
    (matches!(plan.status, PlanStatus::Ready) && plan.frames.len() == request.count)
        .then(|| plan.frames.into_iter().map(|(_, frame)| frame).collect())
}

/// Whether two planned frames are the same.
fn same(a: &Rect, b: &Rect) -> bool {
    Edge::ALL.into_iter().all(|edge| (edge.of(a) - edge.of(b)).abs() < 1e-6)
}

/// How far the frames fall short of their minimum sizes, in points.
fn deficit(frames: &[Rect], minimums: &[Option<Size>]) -> f64 {
    frames
        .iter()
        .zip(minimums)
        .filter_map(|(frame, minimum)| {
            minimum.map(|m| (m.width - frame.width).max(0.0) + (m.height - frame.height).max(0.0))
        })
        .sum()
}

/// The boundary of `edge` of the resized tile: the parameter that moves it
/// while changing the fewest tiles. `None` for an outer edge.
fn boundary(
    request: &ResizeRequest<'_>,
    parameters: &Parameters,
    base: &[Rect],
    edge: Edge,
) -> Option<usize> {
    let position = request.position;
    let mut best: Option<(usize, usize)> = None;
    for k in 0..parameters.values.len() {
        let (low, high) = parameters.bounds(k);
        if high <= low {
            continue;
        }
        let mut probe = parameters.values.clone();
        probe[k] += if probe[k] + PROBE <= high {
            PROBE
        } else {
            -PROBE
        };
        let Some(frames) = plan_with(request, parameters, &probe) else {
            continue;
        };
        if (edge.of(&frames[position]) - edge.of(&base[position])).abs() < 1e-6 {
            continue;
        }
        let changed = frames.iter().zip(base).filter(|(a, b)| !same(a, b)).count();
        if best.is_none_or(|(fewest, _)| changed < fewest) {
            best = Some((changed, k));
        }
    }
    best.map(|(_, k)| k)
}

/// The value of parameter `k`, inside its bounds, that puts `edge` of the
/// resized tile at `target` (or as near as the bounds allow).
fn solve(
    request: &ResizeRequest<'_>,
    parameters: &Parameters,
    k: usize,
    edge: Edge,
    target: f64,
) -> Option<f64> {
    let (low, high) = parameters.bounds(k);
    let edge_at = |value: f64| {
        let mut values = parameters.values.clone();
        values[k] = value;
        plan_with(request, parameters, &values).map(|frames| edge.of(&frames[request.position]))
    };
    let (at_low, at_high) = (edge_at(low)?, edge_at(high)?);
    if (target - at_low) * (target - at_high) > 0.0 {
        return Some(if (target - at_low).abs() < (target - at_high).abs() {
            low
        } else {
            high
        });
    }
    let increasing = at_high > at_low;
    let (mut a, mut b) = (low, high);
    for _ in 0..BISECTIONS {
        let middle = f64::midpoint(a, b);
        let found = edge_at(middle)?;
        if (found < target) == increasing {
            a = middle;
        } else {
            b = middle;
        }
    }
    Some(f64::midpoint(a, b))
}

/// Maps a finished mouse resize onto the workspace ratios: every moved edge
/// of `new` (against `old`) that lies on a boundary moves that boundary so the
/// window's edge lands where the user left it, clamped to the boundary's run
/// (the ratio floor) and so no tile falls further below its minimum size than
/// before. The boundary of an edge is the one ratio that moves it while
/// changing the fewest tiles: the split or lattice boundary next to the edge,
/// a Dwindle ancestor split, the Master share or a Master stack boundary. An
/// edge no ratio moves lies on the usable area's border and is ignored.
#[must_use]
pub fn edge_resize(request: &ResizeRequest<'_>, old: &Rect, new: &Rect) -> EdgeResize {
    if matches!(request.layout, LayoutType::Floating | LayoutType::Monocle)
        || request.count < 2
        || request.position >= request.count
        || !request.usable.has_area()
    {
        return EdgeResize::NoBoundary;
    }
    let mut parameters = parameters(request);
    let initial = parameters.values.clone();
    let mut any_boundary = false;
    for edge in moved_edges(old, new) {
        let Some(base) = plan_with(request, &parameters, &parameters.values) else {
            break;
        };
        let Some(k) = boundary(request, &parameters, &base, edge) else {
            continue;
        };
        any_boundary = true;
        let Some(mut value) = solve(request, &parameters, k, edge, edge.of(new)) else {
            continue;
        };
        let at = |values: &[f64]| plan_with(request, &parameters, values);

        // Minimum sizes: never leave a tile further below its minimum.
        let allowed = deficit(&base, request.minimums) + VIOLATION_TOLERANCE;
        let fits = |value: f64| {
            let mut values = parameters.values.clone();
            values[k] = value;
            at(&values).is_some_and(|frames| deficit(&frames, request.minimums) <= allowed)
        };
        if !fits(value) {
            let (mut good, mut bad) = (parameters.values[k], value);
            for _ in 0..BISECTIONS {
                let middle = f64::midpoint(good, bad);
                if fits(middle) {
                    good = middle;
                } else {
                    bad = middle;
                }
            }
            value = good;
        }
        parameters.values[k] = value;
    }
    if !any_boundary {
        return EdgeResize::NoBoundary;
    }
    if parameters.values.iter().zip(&initial).all(|(a, b)| (a - b).abs() < 1e-9) {
        return EdgeResize::Unchanged;
    }
    let master_ratio = parameters
        .values
        .get(parameters.ratio_len)
        .copied()
        .unwrap_or(request.master_ratio);
    parameters.values.truncate(parameters.ratio_len);
    EdgeResize::Moved {
        ratios: parameters.values,
        master_ratio,
    }
}

// ---------------------------------------------------------------- observer

/// The passive callback. Cheap, bounded and non-panicking: it builds a value
/// and calls [`Ingress::submit_drag`], which never waits for the mailbox lock
/// (a busy lock stages the signal), so the actor can never make it slow.
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
        self.ingress.submit_drag(Input::Drag(signal));
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
    input: InputCapability,
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
            input: input.clone(),
        })
    }

    /// The live state of the listen-only tap and of this observer:
    /// `(delivering, description)`. A tap the system disabled, a re-enable
    /// that failed or an observer the service made inert delivers nothing.
    #[must_use]
    pub fn tap_health(&self) -> (bool, String) {
        let inert = self
            .subscription
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map_or(Some(InertReason::Unsubscribed), Subscription::inert_reason);
        if let Some(reason) = inert {
            return (false, format!("observer inert ({reason:?})"));
        }
        match self.input.tap_status(TapKind::ListenOnly) {
            TapStatus::Ready => (true, "ready".to_owned()),
            TapStatus::Absent => (false, "absent".to_owned()),
            TapStatus::Disabled => (false, "disabled by macOS, re-enabling".to_owned()),
            TapStatus::ReenableFailed { attempts, cause } => (
                false,
                format!("re-enable failed after {attempts} attempts ({cause:?})"),
            ),
            TapStatus::Retained(retained) => (false, format!("retained ({retained:?})")),
        }
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
