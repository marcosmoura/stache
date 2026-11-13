//! Hosts a Tauri webview inside a native non-activating `NSPanel`.
//!
//! Tauri keeps owning the window and the webview. The panel host creates its
//! own `NSPanel` with `Borderless | NonactivatingPanel` **at init** (the style
//! bit has no effect when set on an existing window, and swapping the class of
//! Tauri's already-KVO-observed `TaoWindow` crashes on close), moves the
//! webview into it and keeps Tauri's own window hidden. No class of any Tauri
//! or `AppKit` object is changed.
//!
//! Live evidence (macOS 26, 2026-11-09) is recorded in `docs/guides/testing.md`.
//!
//! # Ownership
//!
//! A [`NativeWindowHandle`] wraps the raw `NSWindow` pointer Tauri hands out on
//! the main thread; it is `!Send`/`!Sync` and is only used during the call that
//! received it. Hosted state (panel, moved views, original content view) lives
//! in a main-thread-only registry keyed by the Tauri window label, so the
//! application layer holds only labels and nothing `!Send` crosses threads.
//!
//! # Hosting, in order
//!
//! 1. Verify the main thread, that the object is an `NSWindow`, that it has a
//!    content view and at least one subview (wry's `WryWebView`).
//! 2. Create the panel at the Tauri window's frame, non-opaque, clear, no
//!    shadow, collection behavior `canJoinAllSpaces |
//!    stationary` (never `fullScreenAuxiliary`), `hidesOnDeactivate = false`.
//!    `setFloatingPanel:` is never called.
//! 3. Move every subview into a plain container view that is the panel's
//!    content view, autoresizing with the panel.
//! 4. Install one tracking area so hover works while the app is inactive.
//! 5. Hide Tauri's window, set level `-1` last, `orderFrontRegardless`.
//!
//! The panel sets no appearance of its own: it inherits the application
//! appearance, which the app host sets from `[ui] appearance`.
//!
//! [`PanelAdapter::unhost`] reverses the view move and closes the panel; it
//! must run before Tauri closes a hosted window. No private API is used.
//!
//! # The popover panel
//!
//! [`PanelAdapter::host_popover`] hosts the tray popover the same way in a
//! small `NSPanel` subclass (`StacheNextPopoverPanel`) whose only override is
//! `canBecomeKeyWindow = YES`: a borderless panel otherwise never becomes key,
//! and text fields need key. It keeps `NonactivatingPanel`, so becoming key
//! never activates Stache or shows a Dock icon. Its content view is an
//! `NSVisualEffectView` (popover material, rounded corners) behind the
//! transparent webview; the panel has the native shadow, floats at the pop-up
//! menu level, joins all Spaces and appears over full-screen apps. A resign-key
//! notification calls the host's callback, which hides it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::fmt;
use std::ptr::NonNull;

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{AnyThread, ClassType, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBackingStoreType, NSColor, NSPanel, NSResponder,
    NSRunningApplication, NSScreen, NSTrackingArea, NSTrackingAreaOptions, NSView,
    NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
    NSWindow, NSWindowCollectionBehavior, NSWindowDidResignKeyNotification, NSWindowLevel,
    NSWindowStyleMask, NSWorkspace,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol, NSOperationQueue};

use crate::popover::{IconAnchor, PopoverScreen, locate_icon, popover_origin};

/// Style mask the panel is created with. Fixed at init; never changed later.
pub const PANEL_STYLE_MASK: NSWindowStyleMask =
    NSWindowStyleMask(NSWindowStyleMask::Borderless.0 | NSWindowStyleMask::NonactivatingPanel.0);

/// `kCGNormalWindowLevel - 1`, tao's `BelowNormalWindowLevel`.
pub const PANEL_LEVEL: NSWindowLevel = -1;

/// The only collection behavior a panel gets. Never includes
/// `fullScreenAuxiliary`.
pub const PANEL_COLLECTION_BEHAVIOR: NSWindowCollectionBehavior = NSWindowCollectionBehavior(
    NSWindowCollectionBehavior::CanJoinAllSpaces.0 | NSWindowCollectionBehavior::Stationary.0,
);

/// `kCGPopUpMenuWindowLevel`: the popover floats like a menu extra's menu.
pub const POPOVER_LEVEL: NSWindowLevel = 101;

/// The popover's collection behavior: every Space, over full-screen apps,
/// never in the window cycle.
pub const POPOVER_COLLECTION_BEHAVIOR: NSWindowCollectionBehavior = NSWindowCollectionBehavior(
    NSWindowCollectionBehavior::CanJoinAllSpaces.0
        | NSWindowCollectionBehavior::FullScreenAuxiliary.0
        | NSWindowCollectionBehavior::IgnoresCycle.0,
);

/// Corner radius of the popover's material, in points.
///
/// The native radius of menu-bar extras (part 05). The material is `.popover`,
/// which matches Control Center's panels; the webview's `popover.panelRadius`
/// is the same.
pub const POPOVER_CORNER_RADIUS: f64 = 12.0;

/// Tracking options that keep hover alive while the application is inactive.
pub const PANEL_TRACKING_OPTIONS: NSTrackingAreaOptions = NSTrackingAreaOptions(
    NSTrackingAreaOptions::MouseEnteredAndExited.0
        | NSTrackingAreaOptions::MouseMoved.0
        | NSTrackingAreaOptions::ActiveAlways.0
        | NSTrackingAreaOptions::InVisibleRect.0,
);

/// Why a window could not be hosted in a panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelError {
    /// The caller was not the main thread.
    NotMainThread,
    /// The object is not an `NSWindow`.
    NotAWindow,
    /// The window has no content view.
    NoContentView,
    /// The content view does not have the expected webview subview layout.
    UnexpectedLayout {
        /// How many subviews the content view had.
        subviews: usize,
    },
}

impl fmt::Display for PanelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotMainThread => f.write_str("a panel can only be hosted on the main thread"),
            Self::NotAWindow => f.write_str("the native object is not an NSWindow"),
            Self::NoContentView => f.write_str("the native window has no content view"),
            Self::UnexpectedLayout { subviews } => {
                write!(
                    f,
                    "the native content view has an unexpected layout ({subviews} subviews)"
                )
            }
        }
    }
}

impl std::error::Error for PanelError {}

/// An `NSWindow` pointer borrowed from Tauri on the main thread.
///
/// The raw pointer makes this type `!Send` and `!Sync`.
pub struct NativeWindowHandle(NonNull<c_void>);

impl NativeWindowHandle {
    /// Wraps the pointer returned by Tauri's `WebviewWindow::ns_window()`.
    ///
    /// Returns `None` for null. The pointer must be the live window Tauri just
    /// returned on the main thread and must not be kept past the call that
    /// received it; Tauri owns the window.
    #[must_use]
    pub fn from_tauri_ns_window(ptr: *mut c_void) -> Option<Self> { NonNull::new(ptr).map(Self) }
}

impl fmt::Debug for NativeWindowHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeWindowHandle").finish_non_exhaustive()
    }
}

/// A panel frame in logical points, top-left origin of the primary display
/// (`NSScreen.screens[0]`), y growing downwards.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TopLeftFrame {
    /// Left edge.
    pub x: f64,
    /// Top edge, measured down from the primary display's top.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

/// Converts a top-left frame to `AppKit`'s bottom-left global coordinates,
/// given the primary display's current height in points.
#[must_use]
pub fn appkit_rect(frame: TopLeftFrame, primary_height: f64) -> CGRect {
    CGRect::new(
        CGPoint::new(frame.x, primary_height - frame.y - frame.height),
        CGSize::new(frame.width, frame.height),
    )
}

/// Hosts Tauri webviews in native non-activating panels. Fakeable in
/// application tests. Windows are identified by their Tauri label.
pub trait PanelAdapter: Send + Sync {
    /// Displays the window's webview in a non-activating panel at the window's
    /// frame and keeps Tauri's window hidden. Idempotent per label.
    ///
    /// # Errors
    /// Returns a typed [`PanelError`]; nothing is left half-hosted.
    fn host(&self, label: &str, window: &NativeWindowHandle) -> Result<(), PanelError>;

    /// Places the panel at `frame` (top-left coordinates of the primary
    /// display), converted with the primary display's height read at call
    /// time. It never copies the Tauri window's frame: Tauri applies its own
    /// moves asynchronously, so that frame is stale at this point. A label
    /// that is not hosted, or no primary display, is a no-op.
    ///
    /// # Errors
    /// [`PanelError::NotMainThread`] off the main thread.
    fn sync_frame(&self, label: &str, frame: TopLeftFrame) -> Result<(), PanelError>;

    /// Moves the webview back into Tauri's window and closes the panel. Must run
    /// before Tauri closes a hosted window. Idempotent; unknown labels are a
    /// no-op.
    fn unhost(&self, label: &str);

    /// Drops hosted state for a Tauri window that is already destroyed, without
    /// touching its views. Idempotent; unknown labels are a no-op.
    fn discard(&self, label: &str);

    /// Displays the window's webview in the key-capable popover panel, hidden
    /// until [`show_popover`](Self::show_popover). `on_resign` runs on the main
    /// thread each time the panel stops being key. Idempotent per label;
    /// [`unhost`](Self::unhost) and [`discard`](Self::discard) release it.
    ///
    /// # Errors
    /// Returns a typed [`PanelError`]; nothing is left half-hosted.
    fn host_popover(
        &self,
        _label: &str,
        _window: &NativeWindowHandle,
        _on_resign: Box<dyn Fn()>,
    ) -> Result<(), PanelError> {
        Ok(())
    }

    /// Places the popover under the icon (see [`popover_origin`]) on the
    /// display that holds it and makes it key without activating the app. An
    /// unknown label is a no-op.
    ///
    /// # Errors
    /// [`PanelError::NotMainThread`] off the main thread.
    fn show_popover(&self, _label: &str, _anchor: IconAnchor) -> Result<(), PanelError> { Ok(()) }

    /// Orders the popover out. An unknown label is a no-op.
    fn hide_popover(&self, _label: &str) {}
}

/// The real `AppKit` adapter.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemPanelAdapter;

type Observer = (
    Retained<NSNotificationCenter>,
    Retained<ProtocolObject<dyn NSObjectProtocol>>,
);

struct HostedPanel {
    panel: Retained<NSPanel>,
    moved: Vec<Retained<NSView>>,
    original: Retained<NSView>,
    /// The popover's resign-key observer, removed before the panel closes.
    observer: Option<Observer>,
}

impl HostedPanel {
    fn remove_observer(&mut self) {
        if let Some((center, token)) = self.observer.take() {
            // SAFETY: the token came from this center and was not removed yet.
            unsafe { center.removeObserver(AsRef::<AnyObject>::as_ref(&token)) };
        }
    }
}

define_class!(
    /// A borderless non-activating panel that may become key.
    #[unsafe(super(NSPanel, NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "StacheNextPopoverPanel"]
    struct PopoverPanel;

    impl PopoverPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool { true }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool { false }
    }
);

std::thread_local! {
    /// Main-thread-only hosted state, keyed by Tauri window label.
    static HOSTED: RefCell<HashMap<String, HostedPanel>> = RefCell::new(HashMap::new());
}

impl PanelAdapter for SystemPanelAdapter {
    fn host(&self, label: &str, window: &NativeWindowHandle) -> Result<(), PanelError> {
        let main = MainThreadMarker::new().ok_or(PanelError::NotMainThread)?;
        if HOSTED.with(|hosted| hosted.borrow().contains_key(label)) {
            return Ok(());
        }
        let window = window_ref(window)?;
        let hosted = build_host(main, window)?;
        HOSTED.with(|hosted_map| hosted_map.borrow_mut().insert(label.to_owned(), hosted));
        Ok(())
    }

    fn sync_frame(&self, label: &str, frame: TopLeftFrame) -> Result<(), PanelError> {
        let main = MainThreadMarker::new().ok_or(PanelError::NotMainThread)?;
        // The observer reacts to NSApplicationDidChangeScreenParametersNotification,
        // posted after NSScreen is updated, so screens[0] is current here.
        let Some(primary) = NSScreen::screens(main).to_vec().into_iter().next() else {
            return Ok(());
        };
        let rect = appkit_rect(frame, primary.frame().size.height);
        HOSTED.with(|hosted| {
            if let Some(host) = hosted.borrow().get(label) {
                host.panel.setFrame_display(rect, true);
            }
        });
        Ok(())
    }

    fn unhost(&self, label: &str) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        let Some(mut host) = HOSTED.with(|hosted| hosted.borrow_mut().remove(label)) else {
            return;
        };
        host.remove_observer();
        host.panel.orderOut(None);
        for view in &host.moved {
            view.removeFromSuperview();
            view.setFrame(host.original.bounds());
            host.original.addSubview(view);
        }
        host.panel.close();
    }

    fn discard(&self, label: &str) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        if let Some(mut host) = HOSTED.with(|hosted| hosted.borrow_mut().remove(label)) {
            host.remove_observer();
            // The views stay with the panel (never touched); closing it
            // releases them together with our retained references.
            host.panel.orderOut(None);
            host.panel.close();
        }
    }

    fn host_popover(
        &self,
        label: &str,
        window: &NativeWindowHandle,
        on_resign: Box<dyn Fn()>,
    ) -> Result<(), PanelError> {
        let main = MainThreadMarker::new().ok_or(PanelError::NotMainThread)?;
        if HOSTED.with(|hosted| hosted.borrow().contains_key(label)) {
            return Ok(());
        }
        let window = window_ref(window)?;
        let hosted = build_popover(main, window, on_resign)?;
        HOSTED.with(|hosted_map| hosted_map.borrow_mut().insert(label.to_owned(), hosted));
        Ok(())
    }

    fn show_popover(&self, label: &str, anchor: IconAnchor) -> Result<(), PanelError> {
        let main = MainThreadMarker::new().ok_or(PanelError::NotMainThread)?;
        let screens = NSScreen::screens(main).to_vec();
        let primary_height = screens.first().map_or(0.0, |screen| screen.frame().size.height);
        let placement: Vec<PopoverScreen> = screens
            .iter()
            .map(|screen| PopoverScreen {
                frame: top_left(screen.frame(), primary_height),
                visible: top_left(screen.visibleFrame(), primary_height),
                scale: screen.backingScaleFactor(),
            })
            .collect();
        HOSTED.with(|hosted| {
            if let Some(host) = hosted.borrow().get(label) {
                let size = host.panel.frame().size;
                if let Some((icon, visible)) = locate_icon(anchor, &placement) {
                    let (x, y) = popover_origin(icon, (size.width, size.height), visible);
                    let frame = TopLeftFrame {
                        x,
                        y,
                        width: size.width,
                        height: size.height,
                    };
                    host.panel.setFrame_display(appkit_rect(frame, primary_height), true);
                }
                host.panel.makeKeyAndOrderFront(None);
                if let Some(view) = host.moved.first() {
                    host.panel.makeFirstResponder(Some(view));
                }
                host.panel.invalidateShadow();
            }
        });
        Ok(())
    }

    fn hide_popover(&self, label: &str) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        HOSTED.with(|hosted| {
            if let Some(host) = hosted.borrow().get(label) {
                host.panel.orderOut(None);
            }
        });
    }
}

/// Converts an `AppKit` bottom-left rect to a top-left frame of the primary
/// display.
fn top_left(rect: CGRect, primary_height: f64) -> TopLeftFrame {
    TopLeftFrame {
        x: rect.origin.x,
        y: primary_height - rect.origin.y - rect.size.height,
        width: rect.size.width,
        height: rect.size.height,
    }
}

/// Reads the activation state without changing it: the frontmost application
/// and whether Stache is active. Diagnostic only, for the live gate harness.
#[must_use]
pub fn diagnostic_activation_state() -> String {
    let front = NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .and_then(|app| app.localizedName())
        .map_or_else(|| "?".to_owned(), |name| name.to_string());
    let active = NSRunningApplication::currentApplication().isActive();
    format!("frontmost={front} stache_active={active}")
}

/// Rejects a content view layout that is not "one or more subviews".
const fn check_layout(subviews: usize) -> Result<(), PanelError> {
    if subviews == 0 {
        Err(PanelError::UnexpectedLayout { subviews })
    } else {
        Ok(())
    }
}

fn window_ref(handle: &NativeWindowHandle) -> Result<&NSWindow, PanelError> {
    let object = handle.0.as_ptr().cast::<AnyObject>();
    // SAFETY: the handle holds the live window pointer Tauri returned on this
    // main-thread call; Tauri owns it and keeps it alive for the call, and the
    // handle is !Send so it cannot be used from another thread.
    let object = unsafe { &*object };
    // SAFETY: `isKindOfClass:` is an NSObject method taking a class and
    // returning BOOL; it is valid on any live object.
    let is_window: objc2::runtime::Bool =
        unsafe { msg_send![object, isKindOfClass: NSWindow::class()] };
    if !is_window.as_bool() {
        return Err(PanelError::NotAWindow);
    }
    // SAFETY: the object is an NSWindow (checked above) and outlives the borrow
    // of the handle for this call.
    Ok(unsafe { &*std::ptr::from_ref(object).cast::<NSWindow>() })
}

fn build_host(main: MainThreadMarker, window: &NSWindow) -> Result<HostedPanel, PanelError> {
    let original = window.contentView().ok_or(PanelError::NoContentView)?;
    let subviews = original.subviews().to_vec();
    check_layout(subviews.len())?;
    let frame = window.frame();
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        main.alloc::<NSPanel>(),
        frame,
        PANEL_STYLE_MASK,
        NSBackingStoreType::Buffered,
        false,
    );
    // SAFETY: the Rust `Retained` owns the panel; AppKit must not also release
    // it when it closes.
    unsafe { panel.setReleasedWhenClosed(false) };
    let pw: &NSWindow = &panel;
    pw.setOpaque(false);
    pw.setBackgroundColor(Some(&NSColor::clearColor()));
    pw.setHasShadow(false);
    // No appearance here: the panel inherits the application appearance.
    pw.setCollectionBehavior(PANEL_COLLECTION_BEHAVIOR);
    pw.setHidesOnDeactivate(false);
    panel.setWorksWhenModal(true);
    panel.setBecomesKeyOnlyIfNeeded(true);
    pw.setAcceptsMouseMovedEvents(true);
    pw.setIgnoresMouseEvents(false);
    let container = NSView::initWithFrame(main.alloc::<NSView>(), original.bounds());
    pw.setContentView(Some(&container));
    let mut moved = Vec::with_capacity(subviews.len());
    for view in subviews {
        view.removeFromSuperview();
        view.setFrame(container.bounds());
        view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        container.addSubview(&view);
        moved.push(view);
    }
    ensure_tracking_area(&container);
    window.orderOut(None);
    // Last, so nothing above can change it.
    pw.setLevel(PANEL_LEVEL);
    pw.orderFrontRegardless();
    Ok(HostedPanel {
        panel,
        moved,
        original,
        observer: None,
    })
}

/// Moves every subview of `original` into `container`, sized with it.
fn move_views(original: &NSView, container: &NSView) -> Vec<Retained<NSView>> {
    let subviews = original.subviews().to_vec();
    let mut moved = Vec::with_capacity(subviews.len());
    for view in subviews {
        view.removeFromSuperview();
        view.setFrame(container.bounds());
        view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        container.addSubview(&view);
        moved.push(view);
    }
    moved
}

fn build_popover(
    main: MainThreadMarker,
    window: &NSWindow,
    on_resign: Box<dyn Fn()>,
) -> Result<HostedPanel, PanelError> {
    let original = window.contentView().ok_or(PanelError::NoContentView)?;
    check_layout(original.subviews().len())?;
    let frame = window.frame();
    // SAFETY: `initWithContentRect:styleMask:backing:defer:` is NSWindow's
    // designated initializer, inherited unchanged by the subclass.
    let panel: Retained<PopoverPanel> = unsafe {
        msg_send![
            main.alloc::<PopoverPanel>(),
            initWithContentRect: frame,
            styleMask: PANEL_STYLE_MASK,
            backing: NSBackingStoreType::Buffered,
            defer: false,
        ]
    };
    let panel: Retained<NSPanel> = Retained::into_super(panel);
    // SAFETY: the Rust `Retained` owns the panel; AppKit must not also release
    // it when it closes.
    unsafe { panel.setReleasedWhenClosed(false) };
    let pw: &NSWindow = &panel;
    pw.setOpaque(false);
    pw.setBackgroundColor(Some(&NSColor::clearColor()));
    pw.setHasShadow(true);
    pw.setCollectionBehavior(POPOVER_COLLECTION_BEHAVIOR);
    pw.setHidesOnDeactivate(false);
    panel.setFloatingPanel(true);
    panel.setWorksWhenModal(true);
    panel.setBecomesKeyOnlyIfNeeded(false);
    pw.setAcceptsMouseMovedEvents(true);
    let effect =
        NSVisualEffectView::initWithFrame(main.alloc::<NSVisualEffectView>(), original.bounds());
    effect.setMaterial(NSVisualEffectMaterial::Popover);
    effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    effect.setState(NSVisualEffectState::Active);
    effect.setWantsLayer(true);
    if let Some(layer) = effect.layer() {
        layer.setCornerRadius(POPOVER_CORNER_RADIUS);
        layer.setMasksToBounds(true);
    }
    pw.setContentView(Some(&effect));
    let moved = move_views(&original, &effect);
    ensure_tracking_area(&effect);
    window.orderOut(None);
    pw.setLevel(POPOVER_LEVEL);
    let center = NSNotificationCenter::defaultCenter();
    let block = RcBlock::new(move |_notification: NonNull<NSNotification>| {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&on_resign));
    });
    let block: &DynBlock<dyn Fn(NonNull<NSNotification>)> = &block;
    let object: &AnyObject = pw;
    // SAFETY: the name is an immutable AppKit constant; the object is this
    // panel; delivery is on the main queue; the center retains the block until
    // `remove_observer` runs before the panel closes.
    let token = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(NSWindowDidResignKeyNotification),
            Some(object),
            Some(&NSOperationQueue::mainQueue()),
            block,
        )
    };
    Ok(HostedPanel {
        panel,
        moved,
        original,
        observer: Some((center, token)),
    })
}

fn ensure_tracking_area(content: &NSView) {
    let owner: &AnyObject = content;
    // SAFETY: `owner` is the container view, which retains the area it adds;
    // the zero rect is valid with `InVisibleRect`; userInfo is nil.
    let area: Retained<NSTrackingArea> = unsafe {
        let alloc = NSTrackingArea::alloc();
        NSTrackingArea::initWithRect_options_owner_userInfo(
            alloc,
            CGRect::ZERO,
            PANEL_TRACKING_OPTIONS,
            Some(owner),
            None,
        )
    };
    content.addTrackingArea(&area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_constants_match_the_approved_behavior() {
        assert_eq!(PANEL_LEVEL, -1);
        assert_eq!(PANEL_COLLECTION_BEHAVIOR.0, (1 << 0) | (1 << 4));
        assert!(
            !PANEL_COLLECTION_BEHAVIOR.contains(NSWindowCollectionBehavior::FullScreenAuxiliary)
        );
        assert_eq!(PANEL_TRACKING_OPTIONS.0, 0x1 | 0x2 | 0x80 | 0x200);
        assert_eq!(PANEL_STYLE_MASK.0, 1 << 7);
        assert!(PANEL_STYLE_MASK.contains(NSWindowStyleMask::NonactivatingPanel));
    }

    #[test]
    fn a_top_left_frame_converts_against_the_current_primary_height() {
        let bar = TopLeftFrame {
            x: 14.0,
            y: 14.0,
            width: 2532.0,
            height: 28.0,
        };
        let on_external = appkit_rect(bar, 1440.0);
        assert_eq!((on_external.origin.x, on_external.origin.y), (14.0, 1398.0));
        assert_eq!((on_external.size.width, on_external.size.height), (2532.0, 28.0));
        // After the external display is gone the built-in (982 pt) is primary.
        let on_builtin = appkit_rect(TopLeftFrame { width: 1484.0, ..bar }, 982.0);
        assert_eq!((on_builtin.origin.x, on_builtin.origin.y), (14.0, 940.0));
        assert!((on_builtin.size.width - 1484.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_window_without_subviews_is_an_unexpected_layout() {
        assert_eq!(
            check_layout(0),
            Err(PanelError::UnexpectedLayout { subviews: 0 })
        );
        assert_eq!(check_layout(1), Ok(()));
        assert_eq!(check_layout(3), Ok(()));
    }

    #[test]
    fn a_null_pointer_is_refused_and_off_main_calls_never_dereference() {
        assert!(NativeWindowHandle::from_tauri_ns_window(std::ptr::null_mut()).is_none());
        // The handle is !Send, so build and use it on a thread that is not main.
        let (host, sync) = std::thread::spawn(|| {
            let mut byte = 0_u8;
            let handle =
                NativeWindowHandle::from_tauri_ns_window((&raw mut byte).cast()).expect("non-null");
            let host = SystemPanelAdapter.host("w", &handle);
            let sync = SystemPanelAdapter.sync_frame("w", TopLeftFrame {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            });
            SystemPanelAdapter.unhost("w");
            SystemPanelAdapter.discard("w");
            (host, sync)
        })
        .join()
        .expect("thread");
        assert_eq!(host, Err(PanelError::NotMainThread));
        assert_eq!(sync, Err(PanelError::NotMainThread));
    }
}
