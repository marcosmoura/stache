//! Display topology, geometry and reconfiguration hints for window tiling.
//!
//! This is additive: [`crate::desktop::DisplayFrame`], [`crate::desktop::DisplayEntry`]
//! and [`crate::desktop::DisplaySnapshot`] (width/height only, used by
//! Wallpapers) are unchanged. [`DisplayInfo`] carries what tiling needs: stable
//! id, the full logical frame with origin, the visible frame, backing scale,
//! refresh rate, localized name and the primary/main flags.
//!
//! # Spaces
//!
//! Two global coordinate spaces exist and are never mixed implicitly:
//!
//! - **`AppKit`** (`NSScreen.frame`/`visibleFrame`): origin at the bottom-left of
//!   the primary display, y up. [`DisplayInfo`] stores this space.
//! - **AX / `CoreGraphics`** (`AXPosition`, `CGDisplayBounds`): origin at the
//!   top-left of the primary display, y down.
//!
//! Converting a rectangle between them is `y' = primary_height - (y + height)`
//! in both directions (the map is an involution), so
//! [`ax_rect_from_appkit`] and [`appkit_rect_from_ax`] share
//! [`crate::applications::appkit_rect_from_cg`]. Origins may be negative for
//! displays left of or above the primary one. The functions are pure.
//!
//! # Identity
//!
//! [`DisplayId`] is the `CGDirectDisplayID` returned by
//! `-[NSScreen CGDirectDisplayID]`, the same identity `desktop` uses. It is
//! stable while the display stays attached; a reconfiguration can still change
//! frames and scale under the same id, so a consumer re-queries on a hint.
//!
//! # Refresh rate
//!
//! The rate comes from `CGDisplayModeGetRefreshRate` of the current mode. `0`
//! (the documented answer for displays without a fixed rate, such as many LCDs
//! and `ProMotion` panels) and non-finite values are [`None`], never `0 Hz` and
//! never a guessed maximum.
//!
//! # Reconfiguration observer
//!
//! [`DisplayControl::observe_topology`] registers
//! `NSApplicationDidChangeScreenParametersNotification` on the default
//! notification center with explicit main-queue delivery, the same
//! `NSNotificationCenter` block pattern as the application observers. It was
//! chosen over `CGDisplayRegisterReconfigurationCallback` because: `AppKit` posts
//! it only after `NSScreen` data is updated, so a re-query on the hint sees the
//! new geometry (the CG callback fires per display, before and after the change,
//! with begin/end flags a consumer must pair); it is delivered on the main queue
//! rather than an arbitrary thread; and it needs a plain block instead of a C
//! callback with a manually managed context pointer. The cost is that it
//! coalesces: a hint means "something changed", never which display.
//!
//! The block enqueues only a pure [`TopologyHint`] through the caller's sink: no
//! `NSScreen` read, no query, no wait. Delivery is native behavior that default
//! tests do not exercise.
//!
//! # Threading
//!
//! `NSScreen` is main-thread-only. [`DisplayControl::query`] runs the read as a
//! main job and waits at most [`crate::applications::ACKNOWLEDGEMENT_BUDGET`]
//! when called off-main; on main it runs inline and never waits. Only plain
//! values cross threads.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use block2::{DynBlock, RcBlock};
use core_graphics::display::CGDisplay;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2_app_kit::{NSApplicationDidChangeScreenParametersNotification, NSScreen};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSOperationQueue};
use stache_sdk::MainThreadExecutor;

use crate::applications::{ACKNOWLEDGEMENT_BUDGET, DisplayRect, appkit_rect_from_cg};
use crate::ax::AxPoint;
use crate::desktop::DisplayId;
use crate::menu::AppKitPoint;

#[cfg(any(test, feature = "testing"))]
pub mod testing;
#[cfg(test)]
mod tests;

/// One attached display, in the `AppKit` global space.
#[derive(Clone, Debug, PartialEq)]
pub struct DisplayInfo {
    /// Stable native identity.
    pub id: DisplayId,
    /// `NSScreen.frame`: the full logical frame with its origin, in points.
    pub frame: DisplayRect,
    /// `NSScreen.visibleFrame`: the frame minus menu bar and Dock, in points.
    pub visible_frame: DisplayRect,
    /// `NSScreen.backingScaleFactor`.
    pub scale: f64,
    /// Current refresh rate in Hz; `None` when unknown.
    pub refresh_hz: Option<f64>,
    /// `NSScreen.localizedName`; `None` when empty.
    pub name: Option<String>,
    /// Whether this is `NSScreen.screens[0]`, the display whose top-left is the
    /// AX origin.
    pub is_primary: bool,
    /// Whether this is `NSScreen.mainScreen` (the display with the key window;
    /// focus dependent, not the primary).
    pub is_main: bool,
    /// Whether this is a built-in panel (`CGDisplayIsBuiltin`).
    pub is_builtin: bool,
}

/// An ordered set of displays, exactly like `NSScreen.screens`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DisplayLayout {
    displays: Vec<DisplayInfo>,
}

impl DisplayLayout {
    /// Builds a layout from displays in native screen order.
    #[must_use]
    pub const fn new(displays: Vec<DisplayInfo>) -> Self { Self { displays } }

    /// The displays in native screen order.
    #[must_use]
    pub fn displays(&self) -> &[DisplayInfo] { &self.displays }

    /// The display with this identity.
    #[must_use]
    pub fn get(&self, id: DisplayId) -> Option<&DisplayInfo> {
        self.displays.iter().find(|display| display.id == id)
    }

    /// The primary display (`screens[0]`), the origin of the AX space.
    #[must_use]
    pub fn primary(&self) -> Option<&DisplayInfo> {
        self.displays
            .iter()
            .find(|display| display.is_primary)
            .or_else(|| self.displays.first())
    }

    /// The primary display's height, the constant of every space conversion.
    #[must_use]
    pub fn primary_height(&self) -> Option<f64> {
        self.primary().map(|display| display.frame.height)
    }

    /// The display frame in the AX space.
    #[must_use]
    pub fn ax_frame(&self, display: &DisplayInfo) -> Option<DisplayRect> {
        self.primary_height().map(|height| ax_rect_from_appkit(display.frame, height))
    }

    /// The display visible frame in the AX space.
    #[must_use]
    pub fn ax_visible_frame(&self, display: &DisplayInfo) -> Option<DisplayRect> {
        self.primary_height()
            .map(|height| ax_rect_from_appkit(display.visible_frame, height))
    }

    /// The display holding the largest part of an AX-space rectangle. The
    /// earlier display wins a tie; `None` when it intersects no display or the
    /// layout is empty.
    #[must_use]
    pub fn display_for_ax_rect(&self, rect: DisplayRect) -> Option<DisplayId> {
        let height = self.primary_height()?;
        let rect = appkit_rect_from_ax(rect, height);
        let mut best: Option<(DisplayId, f64)> = None;
        for display in &self.displays {
            let area = display.frame.intersection_area(&rect);
            if area > 0.0 && best.is_none_or(|(_, held)| area > held) {
                best = Some((display.id, area));
            }
        }
        best.map(|(id, _)| id)
    }
}

/// Converts an `AppKit` global rectangle (bottom-left origin, y up) to the AX
/// global space (top-left origin, y down). Pure.
#[must_use]
pub fn ax_rect_from_appkit(rect: DisplayRect, primary_height: f64) -> DisplayRect {
    appkit_rect_from_cg(rect, primary_height)
}

/// Converts an AX global rectangle to the `AppKit` global space. Pure.
#[must_use]
pub fn appkit_rect_from_ax(rect: DisplayRect, primary_height: f64) -> DisplayRect {
    appkit_rect_from_cg(rect, primary_height)
}

/// Converts an `AppKit` global point to the AX global space. Pure.
#[must_use]
pub fn ax_point_from_appkit(point: AppKitPoint, primary_height: f64) -> AxPoint {
    AxPoint {
        x: point.x,
        y: primary_height - point.y,
    }
}

/// Converts an AX global point to the `AppKit` global space. Pure.
#[must_use]
pub fn appkit_point_from_ax(point: AxPoint, primary_height: f64) -> AppKitPoint {
    AppKitPoint {
        x: point.x,
        y: primary_height - point.y,
    }
}

/// Normalizes `CGDisplayModeGetRefreshRate`: `0` and non-finite values are
/// unknown, never `0 Hz`.
#[must_use]
pub fn refresh_hz_from_native(raw: f64) -> Option<f64> {
    (raw.is_finite() && raw > 0.0).then_some(raw)
}

/// A pure hint that the display topology or its parameters changed.
///
/// It names no display and carries no geometry: the consumer re-queries with
/// [`DisplayControl::query`]. `sequence` increases for every hint delivered to
/// one observer, so a consumer can discard a hint older than a query it already
/// ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopologyHint {
    /// One-based delivery order for this observer.
    pub sequence: u64,
}

/// A failed display operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisplayError {
    /// A native display operation was invoked off the main thread.
    NotMainThread,
    /// The host dispatcher rejected the job.
    Dispatch(String),
    /// No acknowledgement arrived inside the budget.
    TimedOut,
    /// Observation registration failed.
    ObservationFailed(String),
    /// Observation removal failed; ownership stays for a retry.
    RemovalFailed(String),
}

impl fmt::Display for DisplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotMainThread => f.write_str("native display work requires the main thread"),
            Self::Dispatch(message) => write!(f, "cannot dispatch to the main thread: {message}"),
            Self::TimedOut => f.write_str("timed out waiting for a display acknowledgement"),
            Self::ObservationFailed(message) => {
                write!(f, "cannot observe display changes: {message}")
            }
            Self::RemovalFailed(message) => {
                write!(f, "cannot remove display observation: {message}")
            }
        }
    }
}

impl std::error::Error for DisplayError {}

/// The injectable native display operations. Called on the main thread.
pub trait DisplayBackend: Send + Sync {
    /// Reads the current layout.
    ///
    /// # Errors
    /// [`DisplayError::NotMainThread`] off-main.
    fn layout(&self) -> Result<DisplayLayout, DisplayError>;

    /// Registers a reconfiguration observer named `id`. `notify` must be
    /// invoked at most once per native notification, never with a query inside.
    ///
    /// # Errors
    /// A registration failure or [`DisplayError::NotMainThread`].
    fn observe(&self, id: u64, notify: Arc<dyn Fn() + Send + Sync>) -> Result<(), DisplayError>;

    /// Removes observer `id`. Idempotent.
    ///
    /// # Errors
    /// A removal failure; the observer stays owned for a retry.
    fn unobserve(&self, id: u64) -> Result<(), DisplayError>;
}

/// The real `AppKit` display backend. Stateless: observer state lives in a
/// main-thread-local map.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemDisplayBackend;

struct NativeObserver {
    center: Retained<NSNotificationCenter>,
    token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

thread_local! {
    static OBSERVERS: RefCell<BTreeMap<u64, NativeObserver>> =
        const { RefCell::new(BTreeMap::new()) };
}

fn require_main() -> Result<MainThreadMarker, DisplayError> {
    MainThreadMarker::new().ok_or(DisplayError::NotMainThread)
}

impl DisplayBackend for SystemDisplayBackend {
    fn layout(&self) -> Result<DisplayLayout, DisplayError> {
        let mtm = require_main()?;
        let main_id = NSScreen::mainScreen(mtm).map(|screen| screen.CGDirectDisplayID());
        let displays = NSScreen::screens(mtm)
            .to_vec()
            .iter()
            .enumerate()
            .map(|(index, screen)| {
                let raw = screen.CGDirectDisplayID();
                let name = screen.localizedName().to_string();
                DisplayInfo {
                    id: DisplayId::new(raw),
                    frame: rect(screen.frame()),
                    visible_frame: rect(screen.visibleFrame()),
                    scale: screen.backingScaleFactor(),
                    refresh_hz: CGDisplay::new(raw)
                        .display_mode()
                        .and_then(|mode| refresh_hz_from_native(mode.refresh_rate())),
                    name: (!name.is_empty()).then_some(name),
                    is_primary: index == 0,
                    is_main: main_id == Some(raw),
                    is_builtin: CGDisplay::new(raw).is_builtin(),
                }
            })
            .collect();
        Ok(DisplayLayout::new(displays))
    }

    fn observe(&self, id: u64, notify: Arc<dyn Fn() + Send + Sync>) -> Result<(), DisplayError> {
        let _main = require_main()?;
        OBSERVERS.with(|cell| {
            let mut observers = cell.borrow_mut();
            if observers.contains_key(&id) {
                return Err(DisplayError::ObservationFailed("already observing".to_owned()));
            }
            let center = NSNotificationCenter::defaultCenter();
            let queue = NSOperationQueue::mainQueue();
            let block = RcBlock::new(move |_notification: NonNull<NSNotification>| {
                // Contain panics (unwind-enabled builds only; release aborts); only enqueue. No
                // NSScreen read, query or wait.
                let _ = catch_unwind(AssertUnwindSafe(|| notify()));
            });
            let block: &DynBlock<dyn Fn(NonNull<NSNotification>)> = &block;
            // SAFETY: the notification name is an immutable AppKit constant; the
            // observer object is `None`; delivery is explicitly on the main
            // operation queue, and the center retains the block until the token
            // is removed in `unobserve`.
            let token = unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSApplicationDidChangeScreenParametersNotification),
                    None,
                    Some(&queue),
                    block,
                )
            };
            observers.insert(id, NativeObserver { center, token });
            Ok(())
        })
    }

    fn unobserve(&self, id: u64) -> Result<(), DisplayError> {
        let _main = require_main()?;
        let removed = OBSERVERS.with(|cell| cell.borrow_mut().remove(&id));
        if let Some(observer) = removed {
            // SAFETY: the token came from this center and has not been removed;
            // removal is idempotent for an installed observer.
            unsafe {
                observer.center.removeObserver(AsRef::<AnyObject>::as_ref(&observer.token));
            }
        }
        Ok(())
    }
}

const fn rect(frame: objc2_foundation::NSRect) -> DisplayRect {
    DisplayRect::new(
        frame.origin.x,
        frame.origin.y,
        frame.size.width,
        frame.size.height,
    )
}

static NEXT_OBSERVER: AtomicU64 = AtomicU64::new(1);

/// The module-facing display handle: `Send + Sync`, owns no native object.
#[derive(Clone)]
pub struct DisplayControl {
    executor: Arc<dyn MainThreadExecutor>,
    backend: Arc<dyn DisplayBackend>,
    budget: Duration,
}

impl fmt::Debug for DisplayControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DisplayControl").finish_non_exhaustive()
    }
}

impl DisplayControl {
    /// A control over the host dispatcher and the system `AppKit` backend.
    #[must_use]
    pub fn system(executor: Arc<dyn MainThreadExecutor>) -> Self {
        Self::new(executor, Arc::new(SystemDisplayBackend))
    }

    /// A control over an injected backend with the standard per-hop budget.
    #[must_use]
    pub fn new(executor: Arc<dyn MainThreadExecutor>, backend: Arc<dyn DisplayBackend>) -> Self {
        Self::with_budget(executor, backend, ACKNOWLEDGEMENT_BUDGET)
    }

    /// A control with an explicit per-hop acknowledgement budget.
    #[must_use]
    pub fn with_budget(
        executor: Arc<dyn MainThreadExecutor>,
        backend: Arc<dyn DisplayBackend>,
        budget: Duration,
    ) -> Self {
        Self { executor, backend, budget }
    }

    fn run<T: Send + 'static>(
        &self,
        job: impl FnOnce(&dyn DisplayBackend) -> T + Send + 'static,
    ) -> Result<T, DisplayError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let backend = Arc::clone(&self.backend);
        self.executor
            .execute(Box::new(move || {
                let _ = sender.send(job(backend.as_ref()));
            }))
            .map_err(|error| DisplayError::Dispatch(error.to_string()))?;
        receiver.recv_timeout(self.budget).map_err(|_| DisplayError::TimedOut)
    }

    /// Reads the current display layout on the main thread.
    ///
    /// On main the read runs inline; off-main the caller waits at most the
    /// budget. Nothing waits main -> worker -> main.
    ///
    /// # Errors
    /// A dispatch rejection, a timeout or a backend failure.
    pub fn query(&self) -> Result<DisplayLayout, DisplayError> {
        self.run(|backend| backend.layout())?
    }

    /// Observes display reconfiguration. `sink` receives a pure
    /// [`TopologyHint`] on the main thread for every native notification while
    /// the returned observer lives; it must only enqueue.
    ///
    /// # Errors
    /// A dispatch rejection, a timeout (the abandoned registration is removed by
    /// an ordered cleanup job) or a registration failure.
    pub fn observe_topology(
        &self,
        sink: impl Fn(TopologyHint) + Send + Sync + 'static,
    ) -> Result<DisplayObserver, DisplayError> {
        let id = NEXT_OBSERVER.fetch_add(1, Ordering::Relaxed);
        let active = Arc::new(AtomicBool::new(true));
        let sequence = Arc::new(AtomicU64::new(0));
        let notify_active = Arc::clone(&active);
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if notify_active.load(Ordering::SeqCst) {
                sink(TopologyHint {
                    sequence: sequence.fetch_add(1, Ordering::SeqCst) + 1,
                });
            }
        });
        let observer = DisplayObserver {
            control: self.clone(),
            id,
            active,
            removed: AtomicBool::new(false),
        };
        match self.run(move |backend| backend.observe(id, notify)) {
            Ok(Ok(())) => Ok(observer),
            Ok(Err(error)) => {
                // Registered partially or not: stay inert, let `Drop` retry the
                // removal exactly once.
                observer.active.store(false, Ordering::SeqCst);
                Err(error)
            }
            Err(error) => {
                observer.active.store(false, Ordering::SeqCst);
                Err(error)
            }
        }
    }
}

/// One live reconfiguration observation. Dropping it removes the native
/// observer through the dispatcher and waits for a bounded acknowledgement.
pub struct DisplayObserver {
    control: DisplayControl,
    id: u64,
    active: Arc<AtomicBool>,
    removed: AtomicBool,
}

impl fmt::Debug for DisplayObserver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DisplayObserver").field("id", &self.id).finish_non_exhaustive()
    }
}

impl DisplayObserver {
    /// Removes the observer. The sink is inert immediately, before the native
    /// removal; a failed or timed-out removal keeps ownership for a retry.
    ///
    /// # Errors
    /// A dispatch rejection, timeout or removal failure.
    pub fn stop(&self) -> Result<(), DisplayError> {
        self.active.store(false, Ordering::SeqCst);
        if self.removed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let id = self.id;
        self.control.run(move |backend| backend.unobserve(id))??;
        self.removed.store(true, Ordering::SeqCst);
        Ok(())
    }
}

impl Drop for DisplayObserver {
    fn drop(&mut self) { let _ = self.stop(); }
}
