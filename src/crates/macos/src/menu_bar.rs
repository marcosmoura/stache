//! Menu-bar visibility observation for the Bar's auto-hide behavior.
//!
//! This is the safe adapter behind the Bar module's menu-bar state. It ports
//! the *semantics* of the legacy watcher
//! (`_legacy/app/native/src/modules/bar/menubar.rs`), not its constant 100 ms
//! loop.
//!
//! # Legacy semantics (what is ported)
//!
//! The legacy UI named the flag `menuHidden`, but it is `true` when the menu
//! bar is **visible** (`Bar.state.ts:6-17`): a visible menu bar slides the Bar
//! away (`transform: translateY(100%)`, `opacity: 0`, `Bar.styles.ts:5-28`) and
//! a hidden one lets it show. The Bar therefore lives in the place the menu bar
//! occupies when it is auto-hidden. With auto-hide off the menu bar is always
//! visible and the Bar is always slid away. The legacy native flag was seeded
//! once (`menubar.rs:89-117`), defaulting to "not visible" when nothing could
//! be read, and then refreshed on every signal and every 100 ms.
//!
//! One sample asks, in the legacy order (`menubar.rs:359-372`):
//!
//! 1. `+[NSMenu menuBarVisible]`;
//! 2. whether the pointer is inside the menu-bar row at the top of the Bar's
//!    screen (`NSStatusBar.thickness`, `menubar.rs:437-456`). On recent macOS
//!    the pointer enters the row *before* the system marks the menu bar
//!    visible, which is how an auto-hide reveal is seen early;
//! 3. only when neither signal could be read, the `CGWindowList` layer-25
//!    heuristic (`menubar.rs:245-330`);
//! 4. only when that fails too, the previous state.
//!
//! "Visible" wins if either signal says so; "hidden" needs both signals to say
//! so ([`MenuBarSample::resolve`]).
//!
//! Deliberate differences from legacy: the pointer test also requires the
//! pointer's x to lie inside the screen (legacy compared only y, so on a
//! multi-display layout a pointer high on another display counted as "in the
//! menu bar row"); the screen is the primary display `NSScreen.screens[0]`
//! (overview Decision 3) rather than the Bar window's screen; and an
//! unreadable state with no previous one is the explicit
//! [`MenuBarVisibility::Unknown`] instead of legacy's silent "not visible".
//!
//! # Events versus polling
//!
//! The observer registers the notifications legacy used: workspace active-space
//! change and application activate/deactivate, and `NSApplication` become/resign
//! active and screen-parameter changes. The blocks only record a coalesced
//! [`MenuBarHint`] through the caller's sink: no query, no wait, panics
//! contained. The pointer entering the menu-bar row posts **no** notification,
//! so the consumer polls [`MenuBarControl::sample`] while the auto-hide
//! preference is on or unknown ([`MenuBarSample::auto_hide`]) and stops when
//! auto-hide is known to be off.
//!
//! The auto-hide preference itself has no usable notification:
//! `NSUserDefaultsDidChangeNotification` is not posted for changes made by
//! another process (System Settings), and observing it by KVO needs a native
//! subclass. The preference is re-read on every sample, so a change is seen at
//! the next hint or poll.
//!
//! # Limits of the macOS heuristic
//!
//! `menuBarVisible` and the pointer row are best effort. The layer-25 fallback
//! cannot tell the menu bar from any other layer-25 window (other apps'
//! overlays and the Dock-level system windows also use it; a window with
//! bounds counts), and window *names* are withheld from other processes without
//! the Screen Recording permission, so only the layer/owner branch is reliable.
//! Native delivery is not exercised by default tests.
//!
//! # Threading
//!
//! `NSScreen`, `NSMenu`, `NSEvent` and notification centers are main-thread
//! work. [`MenuBarControl::sample`] runs as a main job and waits at most the
//! control's acknowledgement budget off-main. Only plain values cross threads.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use block2::{DynBlock, RcBlock};
use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_graphics::window::{
    copy_window_info, kCGNullWindowID, kCGWindowBounds, kCGWindowLayer,
    kCGWindowListOptionOnScreenOnly, kCGWindowName, kCGWindowOwnerName,
};
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2_app_kit::{
    NSApplicationDidBecomeActiveNotification, NSApplicationDidChangeScreenParametersNotification,
    NSApplicationDidResignActiveNotification, NSEvent, NSMenu, NSScreen, NSStatusBar, NSWorkspace,
    NSWorkspaceActiveSpaceDidChangeNotification, NSWorkspaceDidActivateApplicationNotification,
    NSWorkspaceDidDeactivateApplicationNotification,
};
use objc2_foundation::{
    NSNotification, NSNotificationCenter, NSOperationQueue, NSString, NSUserDefaults,
};
use stache_sdk::MainThreadExecutor;

use crate::applications::ACKNOWLEDGEMENT_BUDGET;

#[cfg(any(test, feature = "testing"))]
pub mod testing;
#[cfg(test)]
mod tests;

/// The `NSUserDefaults` key of "automatically hide and show the menu bar"
/// (`NSGlobalDomain`). Absent means off.
pub const AUTO_HIDE_DEFAULTS_KEY: &str = "_HIHideMenuBar";

/// The layer `CGWindowList` reports the menu bar on.
pub const MENU_BAR_WINDOW_LAYER: i32 = 25;

/// Whether the menu bar is on screen, as far as the system lets us tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuBarVisibility {
    /// The menu bar is shown (permanently or revealed by auto-hide).
    Visible,
    /// The menu bar is hidden.
    Hidden,
    /// Nothing could be read and no earlier state exists.
    Unknown,
}

/// Which source decided a [`MenuBarVisibility`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuBarBasis {
    /// `menuBarVisible` and/or the pointer row.
    Signals,
    /// The `CGWindowList` layer-25 heuristic.
    WindowList,
    /// Every source failed; the previous state was kept.
    Held,
    /// Every source failed and there was no previous state.
    Unresolved,
}

/// The decision for one sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuBarResolution {
    /// The decided state.
    pub visibility: MenuBarVisibility,
    /// What decided it.
    pub basis: MenuBarBasis,
}

/// One point-in-time reading of every signal, taken in one main-thread job.
///
/// `None` means that signal could not be read. `window_list` is `None` when it
/// was not consulted, which the backend does only when both `AppKit` signals are
/// unreadable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuBarSample {
    /// `+[NSMenu menuBarVisible]`.
    pub menu_bar_visible: Option<bool>,
    /// Whether the pointer is inside the menu-bar row of the primary screen.
    pub pointer_in_row: Option<bool>,
    /// The layer-25 heuristic: `Ok(found)` or why the list was unavailable.
    pub window_list: Option<Result<bool, String>>,
    /// The auto-hide preference; `None` when it could not be read.
    pub auto_hide: Option<bool>,
}

impl MenuBarSample {
    /// Resolves the sample exactly like legacy `resolve_menu_bar_visible`
    /// (`menubar.rs:359-372`), with `prior` the previous decided state.
    #[must_use]
    pub fn resolve(&self, prior: Option<MenuBarVisibility>) -> MenuBarResolution {
        let decided = |visible: bool, basis| MenuBarResolution {
            visibility: if visible {
                MenuBarVisibility::Visible
            } else {
                MenuBarVisibility::Hidden
            },
            basis,
        };
        match (self.menu_bar_visible, self.pointer_in_row) {
            (Some(true), _) | (_, Some(true)) => decided(true, MenuBarBasis::Signals),
            (Some(false), Some(false)) => decided(false, MenuBarBasis::Signals),
            _ => match &self.window_list {
                Some(Ok(found)) => decided(*found, MenuBarBasis::WindowList),
                _ => match prior {
                    Some(held @ (MenuBarVisibility::Visible | MenuBarVisibility::Hidden)) => {
                        MenuBarResolution {
                            visibility: held,
                            basis: MenuBarBasis::Held,
                        }
                    }
                    _ => MenuBarResolution {
                        visibility: MenuBarVisibility::Unknown,
                        basis: MenuBarBasis::Unresolved,
                    },
                },
            },
        }
    }

    /// Whether the consumer must poll: the pointer entering the row posts no
    /// notification, so polling is needed while auto-hide is on or unknown.
    #[must_use]
    pub const fn needs_poll(&self) -> bool { !matches!(self.auto_hide, Some(false)) }
}

/// Whether `mouse` lies in the menu-bar row of the screen `frame` (`AppKit`
/// space, bottom-left origin): legacy's `mouse_in_menu_bar_row`
/// (`menubar.rs:424-427`) plus the horizontal bound legacy lacked.
#[must_use]
pub fn pointer_in_menu_bar_row(
    mouse: (f64, f64),
    frame: (f64, f64, f64, f64),
    thickness: f64,
) -> bool {
    let (x, y) = mouse;
    let (left, bottom, width, height) = frame;
    let top = bottom + height;
    x >= left && x < left + width && y >= top - thickness && y <= top
}

/// One window of the on-screen list, reduced to what the heuristic reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListedWindow {
    /// `kCGWindowOwnerName`.
    pub owner: Option<String>,
    /// `kCGWindowLayer`.
    pub layer: Option<i32>,
    /// `kCGWindowName` (withheld without Screen Recording permission).
    pub name: Option<String>,
    /// Whether `kCGWindowBounds` was present.
    pub has_bounds: bool,
}

/// The legacy layer-25 heuristic (`menubar.rs:245-330`): a layer-25 window
/// owned by the window server or with bounds, or a layer 24-26 window whose
/// name mentions the menu bar.
#[must_use]
pub fn window_list_has_menu_bar(windows: &[ListedWindow]) -> bool {
    windows.iter().any(|window| {
        let Some(layer) = window.layer else { return false };
        if window.owner.is_none() {
            return false;
        }
        let owner_is_server = window
            .owner
            .as_deref()
            .is_some_and(|owner| owner == "WindowServer" || owner == "Window Server");
        if layer == MENU_BAR_WINDOW_LAYER && (owner_is_server || window.has_bounds) {
            return true;
        }
        (24..=26).contains(&layer)
            && window.name.as_deref().is_some_and(|name| {
                name.contains("Menubar") || name.contains("Menu Bar") || name.contains("StatusBar")
            })
    })
}

/// A pure hint that something that may change the menu-bar state happened.
///
/// It names no signal: the consumer re-samples. `sequence` increases for every
/// hint delivered to one observer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuBarHint {
    /// One-based delivery order for this observer.
    pub sequence: u64,
}

/// A failed menu-bar operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuBarError {
    /// A native operation was invoked off the main thread.
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

impl fmt::Display for MenuBarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotMainThread => f.write_str("native menu-bar work requires the main thread"),
            Self::Dispatch(message) => write!(f, "cannot dispatch to the main thread: {message}"),
            Self::TimedOut => f.write_str("timed out waiting for a menu-bar acknowledgement"),
            Self::ObservationFailed(message) => {
                write!(f, "cannot observe menu-bar changes: {message}")
            }
            Self::RemovalFailed(message) => {
                write!(f, "cannot remove menu-bar observation: {message}")
            }
        }
    }
}

impl std::error::Error for MenuBarError {}

/// The injectable native operations. Called on the main thread.
pub trait MenuBarBackend: Send + Sync {
    /// Reads every signal once.
    ///
    /// # Errors
    /// [`MenuBarError::NotMainThread`] off-main.
    fn sample(&self) -> Result<MenuBarSample, MenuBarError>;

    /// Registers observer `id`. `notify` must be invoked at most once per
    /// native notification and never with a query inside.
    ///
    /// # Errors
    /// A registration failure or [`MenuBarError::NotMainThread`].
    fn observe(&self, id: u64, notify: Arc<dyn Fn() + Send + Sync>) -> Result<(), MenuBarError>;

    /// Removes observer `id`. Idempotent.
    ///
    /// # Errors
    /// A removal failure; the observer stays owned for a retry.
    fn unobserve(&self, id: u64) -> Result<(), MenuBarError>;
}

/// The real `AppKit` backend. Stateless: observer state is main-thread-local.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemMenuBarBackend;

type Registration = (
    Retained<NSNotificationCenter>,
    Retained<ProtocolObject<dyn NSObjectProtocol>>,
);

struct NativeObserver {
    registrations: Vec<Registration>,
}

thread_local! {
    static OBSERVERS: RefCell<BTreeMap<u64, NativeObserver>> =
        const { RefCell::new(BTreeMap::new()) };
}

fn require_main() -> Result<MainThreadMarker, MenuBarError> {
    MainThreadMarker::new().ok_or(MenuBarError::NotMainThread)
}

impl MenuBarBackend for SystemMenuBarBackend {
    fn sample(&self) -> Result<MenuBarSample, MenuBarError> {
        let mtm = require_main()?;
        let menu_bar_visible = Some(NSMenu::menuBarVisible(mtm));
        let pointer_in_row = NSScreen::screens(mtm).to_vec().first().map(|screen| {
            let frame = screen.frame();
            let mouse = NSEvent::mouseLocation();
            pointer_in_menu_bar_row(
                (mouse.x, mouse.y),
                (
                    frame.origin.x,
                    frame.origin.y,
                    frame.size.width,
                    frame.size.height,
                ),
                NSStatusBar::systemStatusBar().thickness(),
            )
        });
        let window_list = (menu_bar_visible.is_none() || pointer_in_row.is_none())
            .then(read_window_list)
            .map(|listed| listed.map(|windows| window_list_has_menu_bar(&windows)));
        let auto_hide = Some(
            NSUserDefaults::standardUserDefaults()
                .boolForKey(&NSString::from_str(AUTO_HIDE_DEFAULTS_KEY)),
        );
        Ok(MenuBarSample {
            menu_bar_visible,
            pointer_in_row,
            window_list,
            auto_hide,
        })
    }

    fn observe(&self, id: u64, notify: Arc<dyn Fn() + Send + Sync>) -> Result<(), MenuBarError> {
        let _main = require_main()?;
        OBSERVERS.with(|cell| {
            let mut observers = cell.borrow_mut();
            if observers.contains_key(&id) {
                return Err(MenuBarError::ObservationFailed("already observing".to_owned()));
            }
            let queue = NSOperationQueue::mainQueue();
            let workspace = NSWorkspace::sharedWorkspace().notificationCenter();
            let application = NSNotificationCenter::defaultCenter();
            let mut registrations = Vec::with_capacity(6);
            // SAFETY (all six statics): immutable AppKit notification names.
            let names = unsafe {
                [
                    (&workspace, NSWorkspaceActiveSpaceDidChangeNotification),
                    (&workspace, NSWorkspaceDidActivateApplicationNotification),
                    (&workspace, NSWorkspaceDidDeactivateApplicationNotification),
                    (&application, NSApplicationDidBecomeActiveNotification),
                    (&application, NSApplicationDidResignActiveNotification),
                    (&application, NSApplicationDidChangeScreenParametersNotification),
                ]
            };
            for (center, name) in names {
                let notify = Arc::clone(&notify);
                let block = RcBlock::new(move |_notification: NonNull<NSNotification>| {
                    // Contain panics (unwind-enabled builds only; release aborts); only enqueue. No
                    // query, no wait.
                    let _ = catch_unwind(AssertUnwindSafe(|| notify()));
                });
                let block: &DynBlock<dyn Fn(NonNull<NSNotification>)> = &block;
                // SAFETY: the notification name is an immutable AppKit
                // constant; the observer object is `None`; delivery is
                // explicitly on the main operation queue, and the center
                // retains the block until the token is removed in `unobserve`.
                let token = unsafe {
                    center.addObserverForName_object_queue_usingBlock(
                        Some(name),
                        None,
                        Some(&queue),
                        block,
                    )
                };
                registrations.push((center.clone(), token));
            }
            observers.insert(id, NativeObserver { registrations });
            Ok(())
        })
    }

    fn unobserve(&self, id: u64) -> Result<(), MenuBarError> {
        let _main = require_main()?;
        let removed = OBSERVERS.with(|cell| cell.borrow_mut().remove(&id));
        if let Some(observer) = removed {
            for (center, token) in observer.registrations {
                // SAFETY: the token came from this center and has not been
                // removed; removal is idempotent for an installed observer.
                unsafe { center.removeObserver(AsRef::<AnyObject>::as_ref(&token)) };
            }
        }
        Ok(())
    }
}

/// Reads the on-screen window list as [`ListedWindow`]s.
fn read_window_list() -> Result<Vec<ListedWindow>, String> {
    // `copy_window_info` takes ownership of the returned (create-rule) array.
    let array: CFArray = copy_window_info(kCGWindowListOptionOnScreenOnly, kCGNullWindowID)
        .ok_or_else(|| "the window list was unavailable".to_owned())?;
    let mut windows = Vec::new();
    for item in array.iter() {
        let raw: *const c_void = *item;
        if raw.is_null() {
            continue;
        }
        // SAFETY: the documented element type of the window-info array is
        // `CFDictionary`; the array outlives this loop and the get rule retains
        // the dictionary for the wrapper's lifetime.
        let dictionary: CFDictionary<*const c_void, *const c_void> =
            unsafe { CFDictionary::wrap_under_get_rule(raw as CFDictionaryRef) };
        // SAFETY: immutable `CFString` constants exported by CoreGraphics.
        let (owner_key, layer_key, name_key, bounds_key) = unsafe {
            (
                kCGWindowOwnerName.cast::<c_void>(),
                kCGWindowLayer.cast::<c_void>(),
                kCGWindowName.cast::<c_void>(),
                kCGWindowBounds.cast::<c_void>(),
            )
        };
        windows.push(ListedWindow {
            owner: string_value(&dictionary, owner_key),
            layer: number_value(&dictionary, layer_key).and_then(|layer| i32::try_from(layer).ok()),
            name: string_value(&dictionary, name_key),
            has_bounds: dictionary.find(bounds_key).is_some_and(|value| !(*value).is_null()),
        });
    }
    Ok(windows)
}

fn string_value(
    dictionary: &CFDictionary<*const c_void, *const c_void>,
    key: *const c_void,
) -> Option<String> {
    let value = dictionary.find(key)?;
    let raw: *const c_void = *value;
    if raw.is_null() {
        return None;
    }
    // SAFETY: the type is verified by the downcast before reading.
    let value = unsafe { CFType::wrap_under_get_rule(raw.cast()) };
    value.downcast::<CFString>().map(|string| string.to_string())
}

fn number_value(
    dictionary: &CFDictionary<*const c_void, *const c_void>,
    key: *const c_void,
) -> Option<i64> {
    let value = dictionary.find(key)?;
    let raw: *const c_void = *value;
    if raw.is_null() {
        return None;
    }
    // SAFETY: the type is verified by the downcast before reading.
    let value = unsafe { CFType::wrap_under_get_rule(raw.cast()) };
    value.downcast::<CFNumber>().and_then(|number| number.to_i64())
}

static NEXT_OBSERVER: AtomicU64 = AtomicU64::new(1);

/// The module-facing menu-bar handle: `Send + Sync`, owns no native object.
#[derive(Clone)]
pub struct MenuBarControl {
    executor: Arc<dyn MainThreadExecutor>,
    backend: Arc<dyn MenuBarBackend>,
    budget: Duration,
}

impl fmt::Debug for MenuBarControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MenuBarControl").finish_non_exhaustive()
    }
}

impl MenuBarControl {
    /// A control over the host dispatcher and the system `AppKit` backend.
    #[must_use]
    pub fn system(executor: Arc<dyn MainThreadExecutor>) -> Self {
        Self::new(executor, Arc::new(SystemMenuBarBackend))
    }

    /// A control over an injected backend with the standard per-hop budget.
    #[must_use]
    pub fn new(executor: Arc<dyn MainThreadExecutor>, backend: Arc<dyn MenuBarBackend>) -> Self {
        Self::with_budget(executor, backend, ACKNOWLEDGEMENT_BUDGET)
    }

    /// A control with an explicit per-hop acknowledgement budget.
    #[must_use]
    pub fn with_budget(
        executor: Arc<dyn MainThreadExecutor>,
        backend: Arc<dyn MenuBarBackend>,
        budget: Duration,
    ) -> Self {
        Self { executor, backend, budget }
    }

    fn run<T: Send + 'static>(
        &self,
        job: impl FnOnce(&dyn MenuBarBackend) -> T + Send + 'static,
    ) -> Result<T, MenuBarError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let backend = Arc::clone(&self.backend);
        self.executor
            .execute(Box::new(move || {
                let _ = sender.send(job(backend.as_ref()));
            }))
            .map_err(|error| MenuBarError::Dispatch(error.to_string()))?;
        receiver.recv_timeout(self.budget).map_err(|_| MenuBarError::TimedOut)
    }

    /// Reads every signal once, as a bounded main-thread job.
    ///
    /// # Errors
    /// A dispatch rejection, a timeout or a backend failure.
    pub fn sample(&self) -> Result<MenuBarSample, MenuBarError> {
        self.run(|backend| backend.sample())?
    }

    /// Observes the notifications that may change the menu-bar state. `sink`
    /// receives a pure [`MenuBarHint`] on the main thread for every native
    /// notification while the returned observer lives; it must only enqueue.
    ///
    /// # Errors
    /// A dispatch rejection, a timeout (the abandoned registration is removed
    /// by an ordered cleanup on drop) or a registration failure.
    pub fn observe(
        &self,
        sink: impl Fn(MenuBarHint) + Send + Sync + 'static,
    ) -> Result<MenuBarObserver, MenuBarError> {
        let id = NEXT_OBSERVER.fetch_add(1, Ordering::Relaxed);
        let active = Arc::new(AtomicBool::new(true));
        let sequence = Arc::new(AtomicU64::new(0));
        let notify_active = Arc::clone(&active);
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if notify_active.load(Ordering::SeqCst) {
                sink(MenuBarHint {
                    sequence: sequence.fetch_add(1, Ordering::SeqCst) + 1,
                });
            }
        });
        let observer = MenuBarObserver {
            control: self.clone(),
            id,
            active,
            removed: AtomicBool::new(false),
        };
        match self.run(move |backend| backend.observe(id, notify)) {
            Ok(Ok(())) => Ok(observer),
            Ok(Err(error)) | Err(error) => {
                // Registered partially or not: stay inert, let `Drop` retry the
                // removal exactly once.
                observer.active.store(false, Ordering::SeqCst);
                Err(error)
            }
        }
    }
}

/// One live observation. Dropping it removes the native observers through the
/// dispatcher and waits for a bounded acknowledgement.
pub struct MenuBarObserver {
    control: MenuBarControl,
    id: u64,
    active: Arc<AtomicBool>,
    removed: AtomicBool,
}

impl fmt::Debug for MenuBarObserver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MenuBarObserver").field("id", &self.id).finish_non_exhaustive()
    }
}

impl MenuBarObserver {
    /// Removes the observer. The sink is inert immediately, before the native
    /// removal; a failed or timed-out removal keeps ownership for a retry.
    ///
    /// # Errors
    /// A dispatch rejection, timeout or removal failure.
    pub fn stop(&self) -> Result<(), MenuBarError> {
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

impl Drop for MenuBarObserver {
    fn drop(&mut self) { let _ = self.stop(); }
}
