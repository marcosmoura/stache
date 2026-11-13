//! Host-owned window bookkeeping and the Tauri adapter.
//!
//! The SDK validates ownership, surfaces, keys and kinds. This module owns the
//! reuse map and hands native work to the main thread without ever waiting for
//! it. Windows are labelled `stache-<surface>-<n>`: `stache-access-<n>` is
//! granted only the access-window capability. `stache-bar-<n>` is the non-activating Bar panel; no production
//! module requests it yet. `stache-popover` is the tray popover: one
//! host-only, key-capable panel created once and then shown and hidden
//! ([`NativeWindows::toggle_popover`]); no module request can open its surface.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use stache_macos::{
    IconAnchor, NativeWindowHandle, PanelAdapter, SystemPanelAdapter, TopLeftFrame,
};
use stache_sdk::{
    Appearance, LogLevel, LogRecord, Logger, MainThreadExecutor, ModuleId, Operation, SurfaceId,
    WindowBackend, WindowError, WindowFrame, WindowId, WindowKind, WindowOrigin, WindowPhase,
    WindowPhases, WindowRequest,
};
use tauri::window::{Effect, EffectState, EffectsBuilder};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::popover::{
    POPOVER_HEIGHT, POPOVER_LABEL, POPOVER_SURFACE, POPOVER_WIDTH, PopoverShown, PopoverToggle,
};
use crate::shutdown::{NativeOwner, OwnerAnswer};

/// Owner used for the host's own windows (the Accessibility guide, the popover).
pub const HOST_OWNER: ModuleId = ModuleId::new("stache");

/// Surface served to the Accessibility guidance window.
pub const ACCESS_SURFACE: SurfaceId = SurfaceId::new("access");

/// Surface served to the non-activating Bar panel.
pub const BAR_SURFACE: SurfaceId = SurfaceId::new("bar");

/// Logical width of the Accessibility guidance window.
pub const ACCESS_WIDTH: u32 = 480;

/// Logical height of the Accessibility guidance window.
pub const ACCESS_HEIGHT: u32 = 640;

/// Native corner radius of the Accessibility guide's material, matching the
/// popover panel (`POPOVER_CORNER_RADIUS`) and the guide's `native.panelRadius`.
pub const ACCESS_CORNER_RADIUS: f64 = 12.0;

/// Native window chrome for one surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // Mirrors the four independent builder switches.
pub struct WindowChrome {
    /// Whether the window has a title bar and frame.
    pub decorations: bool,
    /// Whether the window background is transparent.
    pub transparent: bool,
    /// Whether the user can resize the window.
    pub resizable: bool,
    /// Whether the window has the native shadow.
    pub shadow: bool,
}

/// Maps a surface to its native chrome.
///
/// The access surface is frameless and transparent, its rounded material set
/// by the window effects in `open`; the Bar panel is frameless,
/// transparent and shadowless; the popover is frameless and transparent, its
/// shadow and rounded material drawn by its native panel; every other surface
/// keeps the decorated, opaque builder defaults.
#[must_use]
pub fn chrome_for(surface: SurfaceId) -> WindowChrome {
    if surface == BAR_SURFACE || surface == POPOVER_SURFACE {
        WindowChrome {
            decorations: false,
            transparent: true,
            resizable: false,
            shadow: false,
        }
    } else if surface == ACCESS_SURFACE {
        WindowChrome {
            decorations: false,
            transparent: true,
            resizable: false,
            shadow: true,
        }
    } else {
        WindowChrome {
            decorations: true,
            transparent: false,
            resizable: true,
            shadow: true,
        }
    }
}

/// Maps `[ui] appearance` to the application theme: `None` follows the system.
#[must_use]
pub const fn theme_for(appearance: Appearance) -> Option<tauri::Theme> {
    match appearance {
        Appearance::System => None,
        Appearance::Light => Some(tauri::Theme::Light),
        Appearance::Dark => Some(tauri::Theme::Dark),
    }
}

/// The host-owned request for the Accessibility guidance window.
///
/// # Errors
/// Returns a backend error if a built-in key or size is rejected.
pub fn access_window_request() -> Result<WindowRequest, WindowError> {
    Ok(WindowRequest::new(
        stache_sdk::WindowKey::new("access")
            .map_err(|error| WindowError::Backend(error.to_string()))?,
        ACCESS_SURFACE,
        stache_sdk::WindowSize::new(ACCESS_WIDTH, ACCESS_HEIGHT)
            .map_err(|error| WindowError::Backend(error.to_string()))?,
        stache_sdk::WindowKind::Standard,
    ))
}

/// The native operations the bookkeeping layer needs.
///
/// Every method runs on the main thread; an implementation may therefore touch
/// `AppKit` and the Tauri window registry directly.
pub trait WindowHost: Send + Sync {
    /// Creates and shows a window.
    ///
    /// # Errors
    /// Returns a native creation failure so the bookkeeping layer can drop the
    /// reservation and allow a retry.
    fn open(&self, spec: &NativeWindowSpec) -> Result<(), String>;
    /// Focuses an existing window, if it still exists.
    fn focus(&self, label: &str);
    /// Moves and resizes an existing window, if it still exists.
    fn reposition(&self, _label: &str, _frame: WindowFrame) {}
    /// Registers where page-load and failure events go. Called once by
    /// [`NativeWindows::new`]; fake hosts ignore it.
    fn attach_events(&self, _events: std::sync::Weak<dyn WindowEvents>) {}
    /// Sets the application-wide native appearance (`NSApp.appearance`;
    /// `System` clears it). Every live and later window and panel inherits it,
    /// so no window carries an appearance of its own.
    fn apply_appearance(&self, _appearance: Appearance) {}
    /// Places the hosted popover under the tray icon, shows it as key and
    /// tells its page it was shown.
    fn show_popover(&self, _label: &str, _anchor: IconAnchor) {}
    /// Orders the hosted popover out.
    fn hide_popover(&self, _label: &str) {}
    /// Closes an existing window and reports what happened to it.
    fn close(&self, label: &str) -> CloseOutcome;
    /// The OS destroyed this window (its `Destroyed` event). Called on the main
    /// thread, possibly for a window this host never asked to close.
    fn destroyed(&self, _label: &str) {}

    /// Whether the page in this window finished loading.
    ///
    /// Native adapters report their page-load hook; fake adapters are already
    /// loaded.
    fn is_loaded(&self, _label: &str) -> bool { true }
}

/// Asynchronous native events a [`WindowHost`] reports back by label.
///
/// Late events for a label that was closed, failed or replaced are ignored.
pub trait WindowEvents: Send + Sync {
    /// The page in this window finished loading.
    fn page_loaded(&self, label: &str);
    /// The page in this window failed to load.
    fn page_failed(&self, label: &str, reason: String);
    /// The popover panel lost key (a click outside it or on the tray icon).
    fn popover_resigned(&self, _label: &str) {}
}

/// What one native close request did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloseOutcome {
    /// The window was asked to close; its `Destroyed` event acknowledges it.
    Closing,
    /// The window was already gone, which counts as closed.
    AlreadyGone,
    /// The native close failed.
    Failed(String),
}

/// A window creation request translated for the native adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeWindowSpec {
    /// Tauri window label.
    pub label: String,
    /// The UI surface and query value.
    pub surface: SurfaceId,
    /// Logical width.
    pub width: u32,
    /// Logical height.
    pub height: u32,
    /// Native chrome for the surface.
    pub chrome: WindowChrome,
    /// The presentation kind; a panel never takes focus.
    pub kind: WindowKind,
    /// Explicit logical placement; present for a panel.
    pub origin: Option<WindowOrigin>,
}

/// A Tauri [window host](WindowHost).
pub struct TauriWindowHost {
    app: AppHandle,
    panels: PanelHosting,
    events: Arc<OnceLock<std::sync::Weak<dyn WindowEvents>>>,
    #[cfg(feature = "smoke")]
    loaded: Arc<Mutex<Vec<String>>>,
}

impl TauriWindowHost {
    /// Creates the adapter for one running application.
    #[must_use]
    pub fn new(app: AppHandle) -> Self {
        Self::with_panel_adapter(app, Arc::new(SystemPanelAdapter))
    }

    /// Creates the adapter with an explicit panel adapter.
    #[allow(clippy::missing_const_for_fn)] // Smoke-only state needs `Arc::new`.
    #[must_use]
    pub fn with_panel_adapter(app: AppHandle, panels: Arc<dyn PanelAdapter>) -> Self {
        Self {
            app,
            panels: PanelHosting::new(panels),
            events: Arc::new(OnceLock::new()),
            #[cfg(feature = "smoke")]
            loaded: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl fmt::Debug for TauriWindowHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TauriWindowHost").finish_non_exhaustive()
    }
}

/// The panel path of the Tauri host, separated from Tauri so fakes can test it.
///
/// Tauri owns the window and the webview; the adapter displays the webview in a
/// non-activating panel while Tauri's window stays hidden. Every close path
/// must call [`before_close`](Self::before_close) before Tauri closes or
/// destroys a hosted window, and [`destroyed`](Self::destroyed) when Tauri
/// reports a window gone.
struct PanelHosting {
    adapter: Arc<dyn PanelAdapter>,
    /// Labels created as panels; they never take focus.
    labels: Mutex<BTreeSet<String>>,
}

type NsWindow<E> = Result<*mut std::ffi::c_void, E>;

fn native_handle<E: fmt::Display>(ns_window: NsWindow<E>) -> Result<NativeWindowHandle, String> {
    let pointer = ns_window.map_err(|error| format!("no native window: {error}"))?;
    NativeWindowHandle::from_tauri_ns_window(pointer)
        .ok_or_else(|| "the native window pointer is null".to_owned())
}

impl PanelHosting {
    fn new(adapter: Arc<dyn PanelAdapter>) -> Self {
        Self {
            adapter,
            labels: Mutex::new(BTreeSet::new()),
        }
    }

    fn labels(&self) -> std::sync::MutexGuard<'_, BTreeSet<String>> {
        self.labels.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn register(&self, label: &str) { self.labels().insert(label.to_owned()); }

    fn is_panel(&self, label: &str) -> bool { self.labels().contains(label) }

    /// Hosts a window Tauri just built. The adapter runs exactly once; on
    /// failure anything partial is unhosted and the caller must destroy the
    /// window.
    fn adopt<E: fmt::Display>(&self, label: &str, ns_window: NsWindow<E>) -> Result<(), String> {
        let result = native_handle(ns_window)
            .and_then(|handle| self.adapter.host(label, &handle).map_err(|e| e.to_string()));
        if result.is_err() {
            self.adapter.unhost(label);
            self.labels().remove(label);
        }
        result
    }

    /// Places a hosted panel at `frame` directly. Tauri queues its own size
    /// and position changes on the main queue, so its window frame is stale
    /// here and is never copied.
    fn reposition(&self, label: &str, frame: WindowFrame) {
        if self.is_panel(label) {
            let _ = self.adapter.sync_frame(label, TopLeftFrame {
                x: f64::from(frame.origin.x),
                y: f64::from(frame.origin.y),
                width: f64::from(frame.size.width),
                height: f64::from(frame.size.height),
            });
        }
    }

    /// Hosts the popover Tauri just built in its key-capable panel; a resign
    /// key reaches `events`. Same failure contract as [`adopt`](Self::adopt).
    fn adopt_popover<E: fmt::Display>(
        &self,
        label: &str,
        ns_window: NsWindow<E>,
        events: Arc<OnceLock<std::sync::Weak<dyn WindowEvents>>>,
    ) -> Result<(), String> {
        let owned = label.to_owned();
        let on_resign = Box::new(move || {
            if let Some(events) = events.get().and_then(std::sync::Weak::upgrade) {
                events.popover_resigned(&owned);
            }
        });
        let result = native_handle(ns_window).and_then(|handle| {
            self.adapter.host_popover(label, &handle, on_resign).map_err(|e| e.to_string())
        });
        if result.is_err() {
            self.adapter.unhost(label);
            self.labels().remove(label);
        }
        result
    }

    /// Must run before Tauri closes or destroys the window.
    fn before_close(&self, label: &str) {
        self.adapter.unhost(label);
        self.labels().remove(label);
    }

    /// The window is already destroyed: drop state without touching views.
    fn destroyed(&self, label: &str) {
        self.adapter.discard(label);
        self.labels().remove(label);
    }
}

impl WindowHost for TauriWindowHost {
    fn attach_events(&self, events: std::sync::Weak<dyn WindowEvents>) {
        let _ = self.events.set(events);
    }

    #[allow(clippy::too_many_lines)] // One builder with the panel and popover paths.
    fn open(&self, spec: &NativeWindowSpec) -> Result<(), String> {
        let popover = spec.surface == POPOVER_SURFACE;
        let panel = popover || spec.kind == WindowKind::NonActivatingPanel;
        let builder = WebviewWindowBuilder::new(
            &self.app,
            &spec.label,
            WebviewUrl::App(format!("index.html?surface={}", spec.surface.as_str()).into()),
        )
        .title(&self.app.package_info().name)
        .visible(!panel)
        .inner_size(f64::from(spec.width), f64::from(spec.height))
        .decorations(spec.chrome.decorations)
        .transparent(spec.chrome.transparent)
        .resizable(spec.chrome.resizable)
        .shadow(spec.chrome.shadow);
        // No `.theme(..)`: in tao it sets the whole application's appearance,
        // which `apply_appearance` owns. Windows inherit it.
        let builder = if popover {
            builder
                .focused(false)
                .skip_taskbar(true)
                .closable(false)
                .minimizable(false)
                .maximizable(false)
                .hidden_title(true)
                .disable_drag_drop_handler()
                .zoom_hotkeys_enabled(false)
        } else if panel {
            let origin = spec.origin.unwrap_or(WindowOrigin { x: 0, y: 0 });
            builder
                .position(f64::from(origin.x), f64::from(origin.y))
                .focused(false)
                .focusable(false)
                .accept_first_mouse(true)
                .skip_taskbar(true)
                .visible_on_all_workspaces(true)
                .always_on_bottom(true)
                .closable(false)
                .minimizable(false)
                .maximizable(false)
                .hidden_title(true)
                .disable_drag_drop_handler()
                .zoom_hotkeys_enabled(false)
        } else if spec.surface == ACCESS_SURFACE {
            // The guide's page is transparent: the popover's native material,
            // rounded, shows through it and follows the system appearance.
            builder.effects(
                EffectsBuilder::new()
                    .effect(Effect::Popover)
                    .state(EffectState::Active)
                    .radius(ACCESS_CORNER_RADIUS)
                    .build(),
            )
        } else {
            builder
        };
        let events = Arc::clone(&self.events);
        #[cfg(feature = "smoke")]
        let loaded = Arc::clone(&self.loaded);
        let builder = builder.on_page_load(move |window, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Finished {
                #[cfg(feature = "smoke")]
                loaded
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(window.label().to_owned());
                if let Some(events) = events.get().and_then(std::sync::Weak::upgrade) {
                    events.page_loaded(window.label());
                }
            }
        });
        if panel {
            self.panels.register(&spec.label);
        }
        let window = match builder.build() {
            Ok(window) => window,
            Err(error) => {
                self.panels.before_close(&spec.label);
                return Err(error.to_string());
            }
        };
        if popover {
            // Hosted hidden; `show_popover` orders it in.
            let events = Arc::clone(&self.events);
            if let Err(error) = self.panels.adopt_popover(&spec.label, window.ns_window(), events) {
                let _ = window.destroy();
                return Err(error);
            }
        } else if panel {
            // Never `show()` + `set_focus()`: Tauri's window stays hidden and
            // its webview is displayed in a non-activating panel.
            if let Err(error) = self.panels.adopt(&spec.label, window.ns_window()) {
                // `adopt` already unhosted; only then may Tauri destroy it.
                let _ = window.destroy();
                return Err(error);
            }
        } else {
            let _ = window.show();
            let _ = window.set_focus();
        }
        Ok(())
    }

    fn focus(&self, label: &str) {
        if self.panels.is_panel(label) {
            return;
        }
        if let Some(window) = self.app.get_webview_window(label) {
            let _ = window.show();
            let _ = window.set_focus();
        }
    }

    fn reposition(&self, label: &str, frame: WindowFrame) {
        if let Some(window) = self.app.get_webview_window(label) {
            let _ = window.set_size(tauri::Size::Logical(tauri::LogicalSize {
                width: f64::from(frame.size.width),
                height: f64::from(frame.size.height),
            }));
            let _ = window.set_position(tauri::Position::Logical(tauri::LogicalPosition {
                x: f64::from(frame.origin.x),
                y: f64::from(frame.origin.y),
            }));
            // Tauri's hidden window moves later (queued); the panel moves now.
            self.panels.reposition(label, frame);
        }
    }

    fn show_popover(&self, label: &str, anchor: IconAnchor) {
        if !self.panels.is_panel(label) {
            return;
        }
        let _ = self.panels.adapter.show_popover(label, anchor);
        let _ = tauri_specta::Event::emit_to(&PopoverShown {}, &self.app, label);
    }

    fn hide_popover(&self, label: &str) {
        if self.panels.is_panel(label) {
            self.panels.adapter.hide_popover(label);
        }
    }

    fn apply_appearance(&self, appearance: Appearance) {
        // App-level: tao sets `NSApp.appearance` (nil for `None`) and
        // refreshes each window's cached theme.
        self.app.set_theme(theme_for(appearance));
    }

    fn close(&self, label: &str) -> CloseOutcome {
        #[cfg(feature = "smoke")]
        self.loaded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|loaded| loaded != label);
        // Before Tauri closes anything: the webview must be back in Tauri's
        // window and the panel closed, or AppKit KVO observers crash.
        self.panels.before_close(label);
        let Some(window) = self.app.get_webview_window(label) else {
            return CloseOutcome::AlreadyGone;
        };
        match window.close() {
            Ok(()) => CloseOutcome::Closing,
            Err(error) => CloseOutcome::Failed(error.to_string()),
        }
    }

    fn destroyed(&self, label: &str) { self.panels.destroyed(label); }

    #[cfg(feature = "smoke")]
    fn is_loaded(&self, label: &str) -> bool {
        self.loaded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|loaded| loaded == label)
    }
}

#[derive(Clone, Debug)]
struct Entry {
    id: WindowId,
    label: String,
    owner: ModuleId,
    kind: WindowKind,
}

#[derive(Default)]
struct WindowState {
    by_key: BTreeMap<(ModuleId, String), Entry>,
    by_id: BTreeMap<WindowId, Entry>,
    /// Live label to identity, so a native callback can name its window. A
    /// label is removed on `Destroyed`, so a late callback finds nothing.
    labels: BTreeMap<String, WindowId>,
    /// Labels a close was dispatched for and whose `Destroyed` event is still
    /// outstanding, including windows released by a stopping module.
    closing: BTreeSet<String>,
    /// A close for this cycle failed or could not be dispatched.
    close_failed: bool,
    /// One-shot answers waiting for `closing` to drain.
    releases: Vec<OwnerAnswer>,
}

impl WindowState {
    /// Answers every waiting release once no label is still closing.
    ///
    /// A failure is kept until a release is actually answered, so a close that
    /// failed during a module stop still makes the later windows release fail.
    fn settle(&mut self) {
        if !self.closing.is_empty() || self.releases.is_empty() {
            return;
        }
        for answer in self.releases.drain(..) {
            if self.close_failed {
                answer.fail();
            } else {
                answer.succeed();
            }
        }
        self.close_failed = false;
    }
}

/// Reuses one window per owner/key and closes module windows on stop.
pub struct NativeWindows {
    shared: Arc<WindowShared>,
}

struct WindowShared {
    main_thread: Arc<dyn MainThreadExecutor>,
    host: Arc<dyn WindowHost>,
    logger: Arc<dyn Logger>,
    next: AtomicU64,
    phases: Arc<WindowPhases>,
    state: Mutex<WindowState>,
    /// The last accepted `[ui] appearance`, applied application-wide; `None`
    /// until the first accepted value so startup always applies it.
    appearance: Mutex<Option<Appearance>>,
    /// The popover's visibility and the last tray icon rectangle.
    popover: Mutex<(PopoverToggle, IconAnchor)>,
    /// Set by [`NativeWindows::release_all`]; a later prewarm builds nothing.
    released: AtomicBool,
}

/// The popover's fixed request.
fn popover_request() -> Result<WindowRequest, WindowError> {
    Ok(WindowRequest::new(
        stache_sdk::WindowKey::new("popover")
            .map_err(|error| WindowError::Backend(error.to_string()))?,
        POPOVER_SURFACE,
        stache_sdk::WindowSize::new(POPOVER_WIDTH, POPOVER_HEIGHT)
            .map_err(|error| WindowError::Backend(error.to_string()))?,
        WindowKind::NonActivatingPanel,
    ))
}

impl NativeWindows {
    /// Creates the bookkeeping layer.
    #[must_use]
    pub fn new(
        main_thread: Arc<dyn MainThreadExecutor>,
        host: Arc<dyn WindowHost>,
        logger: Arc<dyn Logger>,
    ) -> Self {
        let shared = Arc::new(WindowShared {
            main_thread,
            host,
            logger,
            next: AtomicU64::new(0),
            phases: WindowPhases::new(),
            state: Mutex::new(WindowState::default()),
            appearance: Mutex::new(None),
            popover: Mutex::new((PopoverToggle::default(), IconAnchor::default())),
            released: AtomicBool::new(false),
        });
        let events: Arc<dyn WindowEvents> = shared.clone();
        shared.host.attach_events(Arc::downgrade(&events));
        Self { shared }
    }

    /// The lifecycle tracker owners observe through their window handles.
    #[must_use]
    pub fn window_phases(&self) -> Arc<WindowPhases> { Arc::clone(&self.shared.phases) }

    /// A left click on the tray icon at `anchor`: shows the popover under it,
    /// or hides a shown one (including the click that just took key away
    /// from it; see [`PopoverToggle::click`]).
    ///
    /// # Errors
    /// Returns a dispatch failure.
    pub fn toggle_popover(&self, anchor: IconAnchor) -> Result<(), WindowError> {
        self.toggle_popover_at(anchor, std::time::Instant::now())
    }

    /// [`toggle_popover`](Self::toggle_popover) at an explicit time.
    ///
    /// # Errors
    /// Returns a dispatch failure.
    pub fn toggle_popover_at(
        &self,
        anchor: IconAnchor,
        now: std::time::Instant,
    ) -> Result<(), WindowError> {
        let (was_visible, show) = {
            let mut popover = self.shared.popover();
            popover.1 = anchor;
            let was_visible = popover.0.is_visible();
            (was_visible, popover.0.click(now))
        };
        if show {
            self.present_popover(anchor)
        } else if was_visible {
            self.dispatch_popover_hide()
        } else {
            // The click that took key away: the resign already hid it.
            Ok(())
        }
    }

    /// Records the tray icon rectangle of a click that did not toggle (a
    /// right click), for a later [`open_popover`](Self::open_popover).
    pub fn note_tray_anchor(&self, anchor: IconAnchor) { self.shared.popover().1 = anchor; }

    /// Shows the popover under the last tray icon rectangle (`Open Stache`).
    ///
    /// # Errors
    /// Returns a dispatch failure.
    pub fn open_popover(&self) -> Result<(), WindowError> {
        let anchor = {
            let mut popover = self.shared.popover();
            popover.0.open();
            popover.1
        };
        self.present_popover(anchor)
    }

    /// Hides a shown popover; does nothing otherwise.
    pub fn hide_popover(&self) {
        if self.shared.popover().0.hide() {
            let _ = self.dispatch_popover_hide();
        }
    }

    /// Whether the popover is shown, for status and tests.
    #[must_use]
    pub fn popover_visible(&self) -> bool { self.shared.popover().0.is_visible() }

    /// Creates the popover window hidden ahead of the first click, through
    /// the same creation path; shows nothing and emits no `popover-shown`. A
    /// click before or during it reuses the one window. Does nothing once
    /// the windows were released for shutdown.
    ///
    /// # Errors
    /// Returns a dispatch failure; a creation failure is logged and the next
    /// click creates the window as usual.
    pub fn prewarm_popover(&self) -> Result<(), WindowError> {
        if self.shared.released.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.open_validated(HOST_OWNER, &popover_request()?, Some(POPOVER_LABEL))
            .map(|_| ())
    }

    /// Creates the popover window once, then shows it under `anchor`.
    fn present_popover(&self, anchor: IconAnchor) -> Result<(), WindowError> {
        let request = popover_request()?;
        let result =
            self.open_validated(HOST_OWNER, &request, Some(POPOVER_LABEL)).and_then(|_| {
                let shared = Arc::clone(&self.shared);
                self.shared.dispatch(move |host| {
                    if shared.live_id(POPOVER_LABEL).is_some() && shared.popover().0.is_visible() {
                        host.show_popover(POPOVER_LABEL, anchor);
                    }
                })
            });
        if result.is_err() {
            self.shared.popover().0.hide();
        }
        result
    }

    fn dispatch_popover_hide(&self) -> Result<(), WindowError> {
        let shared = Arc::clone(&self.shared);
        self.shared.dispatch(move |host| {
            if shared.live_id(POPOVER_LABEL).is_some() {
                host.hide_popover(POPOVER_LABEL);
            }
        })
    }

    /// Removes a window that the OS destroyed, without asking it to close.
    pub fn window_closed(&self, label: &str) {
        self.shared.host.destroyed(label);
        self.shared.forget_label(label);
    }

    /// Queues one native close and records the label until its event arrives.
    fn dispatch_close(&self, label: &str) {
        {
            let mut state = self.shared.lock();
            state.closing.insert(label.to_owned());
            if let Some(id) = state.labels.get(label).copied() {
                drop(state);
                self.shared.phases.advance(id, WindowPhase::Closing);
            }
        }
        let shared = Arc::clone(&self.shared);
        let job_label = label.to_owned();
        let dispatched = self.shared.dispatch(move |host| match host.close(&job_label) {
            CloseOutcome::Closing => {}
            CloseOutcome::AlreadyGone => shared.forget_label(&job_label),
            CloseOutcome::Failed(error) => {
                shared.lock().close_failed = true;
                shared.log(LogLevel::Warn, format!("window close failed: {error}"));
                shared.forget_label(&job_label);
            }
        });
        if let Err(error) = dispatched {
            self.shared.lock().close_failed = true;
            self.shared
                .log(LogLevel::Warn, format!("window close dispatch failed: {error}"));
            self.shared.forget_label(label);
        }
    }

    /// Closes every tracked window and answers once each close is acknowledged.
    ///
    /// The answer is immediate when nothing is open. A window already gone or
    /// closed by an earlier module release counts as closed; a failed close or a
    /// failed dispatch makes the answer `failed`.
    #[must_use]
    pub fn release_all(&self) -> OwnerAnswer {
        let answer = OwnerAnswer::new();
        let labels: Vec<String> = {
            let mut state = self.shared.lock();
            let labels: Vec<String> =
                state.by_id.values().map(|entry| entry.label.clone()).collect();
            state.closing.extend(labels.iter().cloned());
            state.by_id.clear();
            state.by_key.clear();
            state.releases.push(answer.clone());
            self.shared.released.store(true, Ordering::SeqCst);
            self.shared.popover().0.hide();
            state.settle();
            labels
        };
        for label in labels {
            self.dispatch_close(&label);
        }
        answer
    }

    /// Whether the page in this window finished loading.
    #[cfg(feature = "smoke")]
    #[must_use]
    pub fn is_loaded(&self, label: &str) -> bool { self.shared.host.is_loaded(label) }

    /// The label of the live (tracked, not closing) window `owner` holds, if
    /// any. Reads bookkeeping only: no native call, no wait.
    #[must_use]
    pub fn live_label(&self, owner: ModuleId) -> Option<String> {
        let state = self.shared.lock();
        state
            .by_id
            .values()
            .find(|entry| entry.owner == owner && !state.closing.contains(&entry.label))
            .map(|entry| entry.label.clone())
    }

    /// Whether `label` is a live window held by `owner`. A label of a window
    /// that was closed, failed or replaced is not live, and a label is never
    /// reused.
    #[must_use]
    pub fn is_live_label(&self, owner: ModuleId, label: &str) -> bool {
        self.live_label(owner).as_deref() == Some(label)
    }

    /// The number of tracked windows, for status and tests.
    #[must_use]
    pub fn len(&self) -> usize { self.shared.lock().by_id.len() }

    /// Whether no window is tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool { self.len() == 0 }
}

impl WindowShared {
    fn lock(&self) -> std::sync::MutexGuard<'_, WindowState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn is_live(&self, id: WindowId) -> bool { self.lock().by_id.contains_key(&id) }

    fn popover(&self) -> std::sync::MutexGuard<'_, (PopoverToggle, IconAnchor)> {
        self.popover.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn appearance(&self) -> Option<Appearance> {
        *self.appearance.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn forget(&self, id: WindowId) {
        let mut state = self.lock();
        state.by_id.remove(&id);
        state.by_key.retain(|_, entry| entry.id != id);
        state.labels.retain(|_, labelled| *labelled != id);
    }

    fn forget_label(&self, label: &str) {
        let mut state = self.lock();
        state.by_id.retain(|_, entry| entry.label != label);
        state.by_key.retain(|_, entry| entry.label != label);
        let closed = state.labels.remove(label);
        state.closing.remove(label);
        state.settle();
        drop(state);
        if let Some(id) = closed {
            self.phases.advance(id, WindowPhase::Closed);
        }
    }

    /// The identity of a label whose window is still tracked and live.
    fn live_id(&self, label: &str) -> Option<WindowId> {
        let state = self.lock();
        let id = state.labels.get(label).copied()?;
        (state.by_id.contains_key(&id) && !state.closing.contains(label)).then_some(id)
    }

    fn dispatch(
        &self,
        job: impl FnOnce(Arc<dyn WindowHost>) + Send + 'static,
    ) -> Result<(), WindowError> {
        let host = Arc::clone(&self.host);
        self.main_thread
            .execute(Box::new(move || job(host)))
            .map_err(|error| WindowError::Backend(error.to_string()))
    }

    fn log(&self, level: LogLevel, message: impl Into<String>) {
        self.logger.log(LogRecord {
            level,
            module: ModuleId::new("windows"),
            operation: Operation::Start,
            message: message.into(),
        });
    }
}

impl WindowEvents for WindowShared {
    fn page_loaded(&self, label: &str) {
        if let Some(id) = self.live_id(label) {
            self.phases.advance(id, WindowPhase::Loaded);
        }
    }

    fn page_failed(&self, label: &str, reason: String) {
        if let Some(id) = self.live_id(label) {
            self.phases.advance(id, WindowPhase::Failed(reason));
        }
    }

    /// Runs on the main thread, so the hide runs inline, before the tray
    /// click that may have caused it.
    fn popover_resigned(&self, label: &str) {
        if label == POPOVER_LABEL
            && self.popover().0.resigned(std::time::Instant::now())
            && self.live_id(label).is_some()
        {
            self.host.hide_popover(label);
        }
    }
}

/// Validates the narrow native surface again at the backend boundary.
///
/// The SDK capability checks the same rules, but a host-owned request bypasses
/// it; no caller may create a reserved kind, an unsafe route or an unimplemented
/// surface.
fn validate(request: &WindowRequest) -> Result<(), WindowError> {
    if request.size.width == 0
        || request.size.height == 0
        || request.size.width > stache_sdk::MAX_WINDOW_DIMENSION
        || request.size.height > stache_sdk::MAX_WINDOW_DIMENSION
    {
        return Err(WindowError::InvalidSize {
            width: request.size.width,
            height: request.size.height,
        });
    }
    if request.kind == WindowKind::Launcher {
        return Err(WindowError::UnsupportedKind { kind: request.kind });
    }
    if !request.surface.is_safe() {
        return Err(WindowError::InvalidSurface { surface: request.surface });
    }
    match (request.kind, request.origin) {
        (WindowKind::Standard, None) => {
            if request.surface != ACCESS_SURFACE {
                return Err(WindowError::UnsupportedSurface { surface: request.surface });
            }
        }
        (WindowKind::NonActivatingPanel, Some(_)) => {
            if request.surface != BAR_SURFACE {
                return Err(WindowError::UnsupportedSurface { surface: request.surface });
            }
        }
        (kind, _) => return Err(WindowError::InvalidPlacement { kind }),
    }
    Ok(())
}

enum Reserved {
    Existing(WindowId, String, WindowKind),
    New(WindowId, String),
}

impl NativeWindows {
    // The guard must cover both the lookup and the reservation; releasing it
    // between them is the duplicate-window race this method exists to prevent.
    #[allow(clippy::significant_drop_tightening, clippy::option_if_let_else)]
    fn open_validated(
        &self,
        owner: ModuleId,
        request: &WindowRequest,
        fixed_label: Option<&str>,
    ) -> Result<WindowId, WindowError> {
        if request.surface == ACCESS_SURFACE {
            // Opening another host window (from the popover or the tray)
            // dismisses the popover first.
            self.hide_popover();
        }
        let key = (owner, request.key.as_str().to_owned());
        let reserved = {
            let mut state = self.shared.lock();
            let existing = state.by_key.get(&key).cloned();
            if let Some(entry) = existing {
                Reserved::Existing(entry.id, entry.label, entry.kind)
            } else {
                let id = WindowId::new(self.shared.next.fetch_add(1, Ordering::Relaxed) + 1);
                let label = fixed_label.map_or_else(
                    || format!("stache-{}-{}", request.surface.as_str(), id.get()),
                    str::to_owned,
                );
                let entry = Entry {
                    id,
                    label: label.clone(),
                    owner,
                    kind: request.kind,
                };
                state.by_key.insert(key, entry.clone());
                state.by_id.insert(id, entry);
                state.labels.insert(label.clone(), id);
                self.shared.phases.begin(owner, id);
                Reserved::New(id, label)
            }
        };
        let (id, label) = match reserved {
            Reserved::Existing(id, _, WindowKind::NonActivatingPanel) => {
                // A panel never takes focus, so reuse queues nothing.
                return Ok(id);
            }
            Reserved::Existing(id, label, _) => {
                self.shared.dispatch(move |host| host.focus(&label)).map_err(|error| {
                    self.shared.log(LogLevel::Warn, format!("window focus failed: {error}"));
                    error
                })?;
                return Ok(id);
            }
            Reserved::New(id, label) => (id, label),
        };
        let spec = NativeWindowSpec {
            label: label.clone(),
            surface: request.surface,
            width: request.size.width,
            height: request.size.height,
            chrome: chrome_for(request.surface),
            kind: request.kind,
            origin: request.origin,
        };
        let shared = Arc::clone(&self.shared);
        let dispatched = self.shared.dispatch(move |host| {
            if !shared.is_live(id) {
                return;
            }
            match host.open(&spec) {
                Ok(()) => {
                    shared.phases.advance(id, WindowPhase::Created);
                }
                Err(error) => {
                    shared.phases.advance(id, WindowPhase::Failed(error.clone()));
                    shared.forget(id);
                    if spec.label == POPOVER_LABEL {
                        shared.popover().0.hide();
                    }
                    shared.log(LogLevel::Warn, format!("cannot create window {label}: {error}"));
                }
            }
        });
        if let Err(error) = dispatched {
            self.shared.phases.advance(id, WindowPhase::Failed(error.to_string()));
            self.shared.forget(id);
            self.shared.log(LogLevel::Warn, format!("window dispatch failed: {error}"));
            return Err(error);
        }
        Ok(id)
    }

    /// Opens a `panel-check` surface as a non-activating panel for the live
    /// native gate. It bypasses only the surface check of [`validate`] and
    /// exists only under the `panel-native-check` feature.
    ///
    /// # Errors
    /// Returns a typed key, size or dispatch failure.
    #[cfg(feature = "panel-native-check")]
    pub fn open_test_panel(
        &self,
        origin: WindowOrigin,
        size: stache_sdk::WindowSize,
    ) -> Result<WindowId, WindowError> {
        // No routed UI surface: the page renders the local fallback and calls
        // no command, so the panel needs no capability.
        const PANEL_CHECK_SURFACE: SurfaceId = SurfaceId::new("panel-check");
        let request = WindowRequest::new(
            stache_sdk::WindowKey::new("test-panel")
                .map_err(|error| WindowError::Backend(error.to_string()))?,
            PANEL_CHECK_SURFACE,
            size,
            WindowKind::NonActivatingPanel,
        )
        .with_origin(origin);
        self.open_validated(HOST_OWNER, &request, None)
    }
}

impl WindowBackend for NativeWindows {
    fn open(&self, owner: ModuleId, request: &WindowRequest) -> Result<WindowId, WindowError> {
        validate(request)?;
        self.open_validated(owner, request, None)
    }

    fn focus(&self, window: WindowId) -> Result<(), WindowError> {
        let (label, kind) = self
            .shared
            .lock()
            .by_id
            .get(&window)
            .map(|entry| (entry.label.clone(), entry.kind))
            .ok_or_else(|| WindowError::Backend(format!("window {window} is not open")))?;
        if kind == WindowKind::NonActivatingPanel {
            return Ok(());
        }
        self.shared.dispatch(move |host| host.focus(&label))
    }

    fn reposition(
        &self,
        owner: ModuleId,
        window: WindowId,
        frame: WindowFrame,
    ) -> Result<(), WindowError> {
        let label = {
            let state = self.shared.lock();
            state
                .by_id
                .get(&window)
                .filter(|entry| entry.owner == owner && !state.closing.contains(&entry.label))
                .map(|entry| entry.label.clone())
                .ok_or_else(|| WindowError::Backend(format!("window {window} is not live")))?
        };
        let shared = Arc::clone(&self.shared);
        self.shared.dispatch(move |host| {
            // Rechecked on the main thread: a close or failure that won the
            // race must not move anything, and a label is never reused.
            if shared.live_id(&label) == Some(window) {
                host.reposition(&label, frame);
            }
        })
    }

    fn phases(&self) -> Option<Arc<WindowPhases>> { Some(Arc::clone(&self.shared.phases)) }

    fn set_appearance(&self, appearance: Appearance) {
        let previous = {
            let mut current =
                self.shared.appearance.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if *current == Some(appearance) {
                return;
            }
            current.replace(appearance)
        };
        // Applied to the application whether or not a window is live: every
        // live window and panel and every later build inherits it.
        let shared = Arc::clone(&self.shared);
        let dispatched = self.shared.dispatch(move |host| {
            // A newer value queued after this one wins; a stale job does nothing.
            if shared.appearance() == Some(appearance) {
                host.apply_appearance(appearance);
            }
        });
        if let Err(error) = dispatched {
            // Nothing was applied: restore the previous value (unless a newer
            // call replaced it) so a later call with this value retries.
            {
                let mut current = self
                    .shared
                    .appearance
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if *current == Some(appearance) {
                    *current = previous;
                }
            }
            self.shared.log(
                LogLevel::Warn,
                format!("application appearance dispatch failed: {error}"),
            );
        }
    }

    fn close(&self, window: WindowId) {
        let entry = {
            let mut state = self.shared.lock();
            let Some(entry) = state.by_id.remove(&window) else {
                drop(state);
                // A failed window left no native state; only its terminal
                // phase entry remains.
                self.shared.phases.discard(window);
                return;
            };
            state.by_key.retain(|_, entry| entry.id != window);
            entry
        };
        self.dispatch_close(&entry.label);
    }

    fn release_owner(&self, owner: ModuleId) {
        let labels: Vec<String> = {
            let mut state = self.shared.lock();
            let ids: Vec<WindowId> = state
                .by_id
                .values()
                .filter(|entry| entry.owner == owner)
                .map(|entry| entry.id)
                .collect();
            let mut labels = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(entry) = state.by_id.remove(&id) {
                    labels.push(entry.label);
                }
            }
            state.by_key.retain(|(entry_owner, _), _| *entry_owner != owner);
            labels
        };
        self.shared.phases.discard_failed(owner);
        for label in labels {
            self.dispatch_close(&label);
        }
    }
}

impl fmt::Debug for NativeWindows {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeWindows")
            .field("open", &self.len())
            .finish_non_exhaustive()
    }
}

/// The acknowledged owner for every tracked window.
pub struct WindowsOwner {
    windows: Arc<NativeWindows>,
}

impl WindowsOwner {
    /// Wraps one bookkeeping layer as a teardown owner.
    #[must_use]
    pub const fn new(windows: Arc<NativeWindows>) -> Self { Self { windows } }
}

impl NativeOwner for WindowsOwner {
    fn name(&self) -> &'static str { "windows" }

    fn on_main(&self) -> bool { true }

    fn release(&self) -> OwnerAnswer { self.windows.release_all() }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use stache_macos::PanelAdapter;
    use stache_sdk::{
        DiscardLogger, MainThreadExecutor, ThreadError, WindowKey, WindowKind, WindowRequest,
        WindowSize,
    };

    use super::*;
    use crate::shutdown::OwnerState;

    #[derive(Default)]
    struct FakeHost {
        opened: Mutex<Vec<NativeWindowSpec>>,
        focused: Mutex<Vec<String>>,
        closed: Mutex<Vec<String>>,
        destroyed: Mutex<Vec<String>>,
        fail_next: AtomicBool,
        fail_close: AtomicBool,
        gone: AtomicBool,
        moved: Mutex<Vec<(String, WindowFrame)>>,
        appearances: Mutex<Vec<Appearance>>,
        /// Popover shows and hides, in main-thread order.
        popover: Mutex<Vec<String>>,
        /// Opens and appearance applications, in main-thread order.
        order: Mutex<Vec<String>>,
        events: Mutex<Option<std::sync::Weak<dyn WindowEvents>>>,
    }

    impl FakeHost {
        fn events(&self) -> Arc<dyn WindowEvents> {
            self.events
                .lock()
                .expect("events")
                .as_ref()
                .and_then(std::sync::Weak::upgrade)
                .expect("attached events")
        }
    }

    impl WindowHost for FakeHost {
        fn attach_events(&self, events: std::sync::Weak<dyn WindowEvents>) {
            *self.events.lock().expect("events") = Some(events);
        }

        fn reposition(&self, label: &str, frame: WindowFrame) {
            self.moved.lock().expect("moved").push((label.to_owned(), frame));
        }

        fn open(&self, spec: &NativeWindowSpec) -> Result<(), String> {
            self.opened.lock().expect("opened").push(spec.clone());
            self.order.lock().expect("order").push("open".to_owned());
            if self.fail_next.swap(false, Ordering::SeqCst) {
                return Err("injected creation failure".to_owned());
            }
            Ok(())
        }

        fn focus(&self, label: &str) {
            self.focused.lock().expect("focused").push(label.to_owned());
        }

        fn show_popover(&self, label: &str, anchor: IconAnchor) {
            self.popover.lock().expect("popover").push(format!("show {label} {}", anchor.x));
        }

        fn hide_popover(&self, label: &str) {
            self.popover.lock().expect("popover").push(format!("hide {label}"));
        }

        fn apply_appearance(&self, appearance: Appearance) {
            self.appearances.lock().expect("appearances").push(appearance);
            self.order.lock().expect("order").push(format!("apply {appearance:?}"));
        }

        fn destroyed(&self, label: &str) {
            self.destroyed.lock().expect("destroyed").push(label.to_owned());
        }

        fn close(&self, label: &str) -> CloseOutcome {
            self.closed.lock().expect("closed").push(label.to_owned());
            if self.fail_close.swap(false, Ordering::SeqCst) {
                return CloseOutcome::Failed("injected close failure".to_owned());
            }
            if self.gone.load(Ordering::SeqCst) {
                CloseOutcome::AlreadyGone
            } else {
                CloseOutcome::Closing
            }
        }
    }

    struct Inline;

    impl MainThreadExecutor for Inline {
        fn is_main_thread(&self) -> bool { true }

        fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
            job();
            Ok(())
        }
    }

    #[derive(Default)]
    struct Queued {
        jobs: Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>,
    }

    impl Queued {
        fn run(&self) {
            let jobs = std::mem::take(&mut *self.jobs.lock().expect("jobs"));
            for job in jobs {
                job();
            }
        }
    }

    impl MainThreadExecutor for Queued {
        fn is_main_thread(&self) -> bool { false }

        fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
            self.jobs.lock().expect("jobs").push(job);
            Ok(())
        }
    }

    fn windows_with(
        main_thread: Arc<dyn MainThreadExecutor>,
    ) -> (Arc<NativeWindows>, Arc<FakeHost>) {
        let host = Arc::new(FakeHost::default());
        let windows = Arc::new(NativeWindows::new(
            main_thread,
            host.clone(),
            Arc::new(DiscardLogger),
        ));
        (windows, host)
    }

    fn windows() -> (Arc<NativeWindows>, Arc<FakeHost>) { windows_with(Arc::new(Inline)) }

    const ORIGIN: WindowOrigin = WindowOrigin { x: -40, y: 6 };

    fn panel_request(surface: &'static str, key: &str) -> WindowRequest {
        WindowRequest::new(
            WindowKey::new(key).expect("key"),
            SurfaceId::new(surface),
            WindowSize::new(1200, 36).expect("size"),
            WindowKind::NonActivatingPanel,
        )
        .with_origin(ORIGIN)
    }

    fn frame(x: i32) -> WindowFrame {
        WindowFrame {
            origin: WindowOrigin { x, y: 2 },
            size: WindowSize::new(640, 30).expect("size"),
        }
    }

    fn request(owner_surface: &'static str, key: &str) -> WindowRequest {
        WindowRequest::new(
            WindowKey::new(key).expect("key"),
            SurfaceId::new(owner_surface),
            WindowSize::new(800, 600).expect("size"),
            WindowKind::Standard,
        )
    }

    #[test]
    fn the_same_owner_and_key_reuses_and_focuses() {
        let (windows, host) = windows();
        let owner = ModuleId::new("one");
        let first = windows.open(owner, &request("access", "main")).expect("first");
        let second = windows.open(owner, &request("access", "main")).expect("second");
        assert_eq!(first, second);
        assert_eq!(host.opened.lock().expect("opened").len(), 1);
        assert_eq!(host.focused.lock().expect("focused").len(), 1);
    }

    #[test]
    fn closing_removes_the_entry_and_allows_a_new_window() {
        let (windows, host) = windows();
        let owner = ModuleId::new("one");
        let first = windows.open(owner, &request("access", "main")).expect("first");
        windows.close(first);
        assert!(windows.is_empty());
        assert_eq!(host.closed.lock().expect("closed").len(), 1);
        let second = windows.open(owner, &request("access", "main")).expect("second");
        assert_ne!(first, second);
        assert_eq!(host.opened.lock().expect("opened").len(), 2);
    }

    #[test]
    fn an_os_close_removes_the_entry_without_closing_again() {
        let (windows, host) = windows();
        let owner = ModuleId::new("one");
        let id = windows.open(owner, &request("access", "main")).expect("open");
        let label = host.opened.lock().expect("opened")[0].label.clone();
        windows.window_closed(&label);
        assert!(windows.is_empty());
        assert!(host.closed.lock().expect("closed").is_empty());
        windows.close(id);
        assert!(host.closed.lock().expect("closed").is_empty());
    }

    #[test]
    fn stopping_one_owner_closes_only_its_windows() {
        let (windows, host) = windows();
        let one = ModuleId::new("one");
        let two = ModuleId::new("two");
        windows.open(one, &request("access", "a")).expect("one a");
        windows.open(one, &request("access", "b")).expect("one b");
        windows.open(two, &request("access", "a")).expect("two a");
        windows.release_owner(one);
        assert_eq!(host.closed.lock().expect("closed").len(), 2);
        assert_eq!(windows.len(), 1);
        windows.release_owner(two);
        assert!(windows.is_empty());
    }

    #[test]
    fn concurrent_open_calls_reserve_only_one_window() {
        let (windows, host) = windows();
        let owner = ModuleId::new("one");
        let mut workers = Vec::new();
        for _ in 0..8 {
            let windows = Arc::clone(&windows);
            workers.push(std::thread::spawn(move || {
                windows.open(owner, &request("access", "shared")).expect("open")
            }));
        }
        let ids: Vec<WindowId> =
            workers.into_iter().map(|worker| worker.join().expect("worker")).collect();
        assert!(ids.windows(2).all(|pair| pair[0] == pair[1]), "{ids:?}");
        assert_eq!(host.opened.lock().expect("opened").len(), 1);
    }

    #[test]
    fn a_queued_open_is_aborted_by_close_or_release_before_it_runs() {
        let executor = Arc::new(Queued::default());
        let (windows, host) = windows_with(executor.clone());
        let owner = ModuleId::new("one");
        windows.open(owner, &request("access", "one")).expect("open");
        windows.close(WindowId::new(1));
        executor.run();
        assert!(host.opened.lock().expect("opened").is_empty());
        assert_eq!(host.closed.lock().expect("closed").len(), 1);

        windows.open(owner, &request("access", "two")).expect("open");
        windows.release_owner(owner);
        executor.run();
        assert!(host.opened.lock().expect("opened").is_empty());
        assert_eq!(host.closed.lock().expect("closed").len(), 2);
        assert!(windows.is_empty());
    }

    #[test]
    fn close_all_aborts_queued_opens_and_allows_a_later_window() {
        let executor = Arc::new(Queued::default());
        let (windows, host) = windows_with(executor.clone());
        let owner = ModuleId::new("one");
        windows.open(owner, &request("access", "a")).expect("a");
        windows.open(owner, &request("access", "b")).expect("b");
        let answer = windows.release_all();
        executor.run();
        assert!(host.opened.lock().expect("opened").is_empty());
        assert!(windows.is_empty());
        let closed = host.closed.lock().expect("closed").clone();
        for label in closed {
            windows.window_closed(&label);
        }
        assert_eq!(answer.state(), OwnerState::Succeeded);

        windows.open(owner, &request("access", "a")).expect("reopen");
        executor.run();
        assert_eq!(host.opened.lock().expect("opened").len(), 1);
        assert_eq!(windows.len(), 1);
    }

    #[test]
    fn release_all_answers_only_after_every_labels_close_event() {
        let (windows, host) = windows();
        let one = ModuleId::new("one");
        let two = ModuleId::new("two");
        windows.open(one, &request("access", "a")).expect("one a");
        windows.open(one, &request("access", "b")).expect("one b");
        windows.open(two, &request("access", "a")).expect("two a");
        // The registry releases a module window before the windows stage runs.
        windows.release_owner(two);
        let answer = windows.release_all();
        assert!(!answer.is_answered());
        let mut labels = host.closed.lock().expect("closed").clone();
        assert_eq!(labels.len(), 3);
        let last = labels.pop().expect("last label");
        for label in labels {
            windows.window_closed(&label);
            assert!(
                !answer.is_answered(),
                "one outstanding label must keep the answer pending"
            );
        }
        windows.window_closed(&last);
        assert_eq!(
            answer.state(),
            OwnerState::Succeeded,
            "every released and teardown-closed label is acknowledged"
        );
    }

    #[test]
    fn release_all_with_nothing_open_answers_immediately() {
        let (windows, _host) = windows();
        assert_eq!(windows.release_all().state(), OwnerState::Succeeded);
    }

    #[test]
    fn an_already_gone_window_counts_as_closed() {
        let (windows, host) = windows();
        host.gone.store(true, Ordering::SeqCst);
        windows.open(ModuleId::new("one"), &request("access", "main")).expect("open");
        let answer = windows.release_all();
        assert_eq!(answer.state(), OwnerState::Succeeded);
        assert_eq!(host.closed.lock().expect("closed").len(), 1);
    }

    #[test]
    fn a_failed_close_makes_the_release_a_failure() {
        let (windows, host) = windows();
        host.fail_close.store(true, Ordering::SeqCst);
        windows.open(ModuleId::new("one"), &request("access", "main")).expect("open");
        let answer = windows.release_all();
        assert_eq!(answer.state(), OwnerState::Failed);
    }

    #[test]
    fn a_missing_close_event_leaves_the_release_pending() {
        let (windows, _host) = windows();
        windows.open(ModuleId::new("one"), &request("access", "main")).expect("open");
        let answer = windows.release_all();
        assert_eq!(answer.state(), OwnerState::Pending);
        assert!(
            !answer.is_answered(),
            "a window without its Destroyed event is not acknowledged"
        );
    }

    const ICON: IconAnchor = IconAnchor {
        x: 2000.0,
        y: 0.0,
        width: 48.0,
        height: 66.0,
    };

    fn popover_log(host: &FakeHost) -> Vec<String> { host.popover.lock().expect("popover").clone() }

    #[test]
    fn the_popover_is_created_once_then_shown_and_hidden() {
        let (windows, host) = windows();
        let now = std::time::Instant::now();
        windows.toggle_popover_at(ICON, now).expect("show");
        windows.toggle_popover_at(ICON, now + Duration::from_secs(1)).expect("hide");
        windows
            .toggle_popover_at(ICON, now + Duration::from_secs(2))
            .expect("show again");
        let opened = host.opened.lock().expect("opened").clone();
        assert_eq!(opened.len(), 1, "created once");
        assert_eq!(opened[0].label, POPOVER_LABEL);
        assert_eq!(opened[0].surface, POPOVER_SURFACE);
        assert_eq!((opened[0].width, opened[0].height), (480, 640));
        assert_eq!(opened[0].chrome, chrome_for(POPOVER_SURFACE));
        assert!(!opened[0].chrome.decorations && opened[0].chrome.transparent);
        assert_eq!(popover_log(&host), [
            "show stache-popover 2000",
            "hide stache-popover",
            "show stache-popover 2000",
        ]);
        assert!(windows.popover_visible());
        assert!(
            host.focused.lock().expect("focused").is_empty(),
            "never focused like a window"
        );
        assert_eq!(windows.live_label(HOST_OWNER).as_deref(), Some(POPOVER_LABEL));
    }

    #[test]
    fn hide_popover_hides_only_a_shown_popover() {
        let (windows, host) = windows();
        windows.hide_popover();
        assert!(popover_log(&host).is_empty());
        windows.open_popover().expect("open");
        windows.hide_popover();
        windows.hide_popover();
        assert_eq!(popover_log(&host), [
            "show stache-popover 0",
            "hide stache-popover"
        ]);
    }

    #[test]
    fn open_stache_uses_the_icon_of_the_last_right_click() {
        let (windows, host) = windows();
        windows.note_tray_anchor(ICON);
        windows.open_popover().expect("open");
        // Opening a shown popover shows it again (it regains key).
        windows.open_popover().expect("again");
        assert_eq!(popover_log(&host), [
            "show stache-popover 2000",
            "show stache-popover 2000"
        ]);
    }

    #[test]
    fn a_resign_key_hides_and_the_racing_tray_click_keeps_it_closed() {
        let (windows, host) = windows();
        windows.toggle_popover(ICON).expect("show");
        // The press on the tray icon takes key away first.
        host.events().popover_resigned(POPOVER_LABEL);
        assert!(!windows.popover_visible());
        windows.toggle_popover(ICON).expect("the same click");
        assert!(!windows.popover_visible());
        assert_eq!(popover_log(&host), [
            "show stache-popover 2000",
            "hide stache-popover"
        ]);
        // A later click opens again.
        windows
            .toggle_popover_at(ICON, std::time::Instant::now() + Duration::from_secs(1))
            .expect("later");
        assert!(windows.popover_visible());
        // A resign for another label or a hidden popover does nothing.
        host.events().popover_resigned("stache-access-9");
        windows.hide_popover();
        host.events().popover_resigned(POPOVER_LABEL);
        assert_eq!(popover_log(&host).len(), 4);
    }

    #[test]
    fn opening_the_guide_hides_the_popover_first() {
        let (windows, host) = windows();
        windows.open_popover().expect("open");
        windows
            .open(
                HOST_OWNER,
                &crate::windows::access_window_request().expect("request"),
            )
            .expect("guide");
        assert!(!windows.popover_visible());
        assert_eq!(popover_log(&host), [
            "show stache-popover 0",
            "hide stache-popover"
        ]);
    }

    #[test]
    fn a_failed_popover_creation_resets_it_and_the_next_click_retries() {
        let (windows, host) = windows();
        host.fail_next.store(true, Ordering::SeqCst);
        windows.toggle_popover(ICON).expect("dispatched");
        assert!(!windows.popover_visible());
        assert!(popover_log(&host).is_empty());
        windows.toggle_popover(ICON).expect("retry");
        assert!(windows.popover_visible());
        assert_eq!(host.opened.lock().expect("opened").len(), 2);
        assert_eq!(popover_log(&host), ["show stache-popover 2000"]);
    }

    #[test]
    fn shutdown_releases_the_popover_through_the_windows_owner() {
        let (windows, host) = windows();
        windows.toggle_popover(ICON).expect("show");
        let owner = WindowsOwner::new(windows.clone());
        let answer = owner.release();
        assert!(!windows.popover_visible());
        assert_eq!(*host.closed.lock().expect("closed"), [POPOVER_LABEL]);
        windows.window_closed(POPOVER_LABEL);
        assert_eq!(answer.state(), OwnerState::Succeeded);
        assert!(windows.is_empty());
    }

    #[test]
    fn prewarm_builds_one_hidden_popover_and_the_first_click_only_shows_it() {
        let (windows, host) = windows();
        windows.prewarm_popover().expect("prewarm");
        windows.prewarm_popover().expect("again");
        assert_eq!(host.opened.lock().expect("opened").len(), 1);
        assert_eq!(host.opened.lock().expect("opened")[0].label, POPOVER_LABEL);
        assert!(popover_log(&host).is_empty(), "built hidden, nothing shown");
        assert!(!windows.popover_visible());
        windows.toggle_popover(ICON).expect("click");
        assert_eq!(host.opened.lock().expect("opened").len(), 1);
        assert_eq!(popover_log(&host), ["show stache-popover 2000"]);
    }

    #[test]
    fn a_click_during_the_prewarm_shows_the_one_window_once() {
        let executor = Arc::new(Queued::default());
        let (building, host) = windows_with(executor.clone());
        building.prewarm_popover().expect("prewarm");
        building.toggle_popover(ICON).expect("click while building");
        executor.run();
        assert_eq!(host.opened.lock().expect("opened").len(), 1);
        assert_eq!(popover_log(&host), ["show stache-popover 2000"]);
        // A click before the prewarm: the prewarm reuses its window.
        let (early, host) = windows();
        early.toggle_popover(ICON).expect("click");
        early.prewarm_popover().expect("prewarm");
        assert_eq!(host.opened.lock().expect("opened").len(), 1);
        assert_eq!(popover_log(&host), ["show stache-popover 2000"]);
    }

    #[test]
    fn a_prewarm_after_shutdown_builds_nothing() {
        let (windows, host) = windows();
        let answer = WindowsOwner::new(windows.clone()).release();
        assert_eq!(answer.state(), OwnerState::Succeeded);
        windows.prewarm_popover().expect("inert");
        assert!(host.opened.lock().expect("opened").is_empty());
        assert!(windows.is_empty());
    }

    #[test]
    fn a_failed_prewarm_still_lets_a_click_open_the_popover() {
        let (windows, host) = windows();
        host.fail_next.store(true, Ordering::SeqCst);
        windows.prewarm_popover().expect("dispatched");
        assert!(windows.is_empty());
        windows.toggle_popover(ICON).expect("click");
        assert_eq!(host.opened.lock().expect("opened").len(), 2);
        assert_eq!(popover_log(&host), ["show stache-popover 2000"]);
        assert!(windows.popover_visible());
    }

    #[test]
    fn a_queued_show_after_a_hide_does_not_show() {
        let executor = Arc::new(Queued::default());
        let (windows, host) = windows_with(executor.clone());
        windows.toggle_popover(ICON).expect("show");
        windows.hide_popover();
        executor.run();
        assert_eq!(host.opened.lock().expect("opened").len(), 1);
        assert_eq!(popover_log(&host), ["hide stache-popover"]);
    }

    #[test]
    fn no_module_can_open_the_popover_surface() {
        let (windows, host) = windows();
        for owner in [ModuleId::new("bar"), HOST_OWNER] {
            let standard = request("popover", "popover");
            assert_eq!(
                windows.open(owner, &standard),
                Err(WindowError::UnsupportedSurface { surface: POPOVER_SURFACE })
            );
            assert_eq!(
                windows.open(owner, &panel_request("popover", "popover")),
                Err(WindowError::UnsupportedSurface { surface: POPOVER_SURFACE })
            );
        }
        assert!(host.opened.lock().expect("opened").is_empty());
    }

    #[test]
    fn the_windows_owner_is_named_and_waited_on_the_main_hop_budget() {
        let (windows, _host) = windows();
        let owner = WindowsOwner::new(windows);
        assert_eq!(owner.name(), "windows");
        assert!(owner.on_main());
        assert_eq!(owner.release().state(), OwnerState::Succeeded);
    }

    #[test]
    fn a_failed_creation_drops_the_reservation_and_allows_a_retry() {
        let (windows, host) = windows();
        let owner = ModuleId::new("one");
        host.fail_next.store(true, Ordering::SeqCst);
        let first = windows.open(owner, &request("access", "main")).expect("reservation");
        assert!(
            windows.is_empty(),
            "a failed native creation must not stay tracked"
        );
        let second = windows.open(owner, &request("access", "main")).expect("retry");
        assert_ne!(first, second);
        assert_eq!(windows.len(), 1);
        assert_eq!(host.opened.lock().expect("opened").len(), 2);
    }

    #[test]
    fn the_backend_rechecks_kind_size_and_surface_for_host_requests() {
        let (windows, host) = windows();
        let owner = ModuleId::new("one");
        let mut launcher = request("access", "main");
        launcher.kind = WindowKind::Launcher;
        assert!(matches!(
            windows.open(owner, &launcher),
            Err(WindowError::UnsupportedKind { .. })
        ));
        let mut zero = request("access", "main");
        zero.size = WindowSize { width: 0, height: 600 };
        assert!(matches!(
            windows.open(owner, &zero),
            Err(WindowError::InvalidSize { .. })
        ));
        assert!(matches!(
            windows.open(owner, &request("test&other=1", "main")),
            Err(WindowError::InvalidSurface { .. })
        ));
        assert!(matches!(
            windows.open(owner, &request("settings", "main")),
            Err(WindowError::UnsupportedSurface { .. })
        ));
        assert!(host.opened.lock().expect("opened").is_empty());
    }

    #[test]
    fn the_access_host_request_is_accepted_with_its_own_label() {
        let (windows, host) = windows();
        windows
            .open(HOST_OWNER, &access_window_request().expect("request"))
            .expect("access");
        let opened = host.opened.lock().expect("opened");
        assert_eq!(opened.len(), 1);
        assert!(
            opened[0].label.starts_with("stache-access-"),
            "{}",
            opened[0].label
        );
        assert_eq!(opened[0].surface, ACCESS_SURFACE);
        let (width, height) = (opened[0].width, opened[0].height);
        drop(opened);
        assert_eq!((width, height), (480, 640));
        assert!((1..=stache_sdk::MAX_WINDOW_DIMENSION).contains(&width));
        assert!((1..=stache_sdk::MAX_WINDOW_DIMENSION).contains(&height));
    }

    #[test]
    fn chrome_is_frameless_for_access_and_default_otherwise() {
        assert_eq!(chrome_for(ACCESS_SURFACE), WindowChrome {
            decorations: false,
            transparent: true,
            resizable: false,
            shadow: true,
        });
        assert_eq!(chrome_for(SurfaceId::new("other")), WindowChrome {
            decorations: true,
            transparent: false,
            resizable: true,
            shadow: true,
        });
        let (windows, host) = windows();
        windows
            .open(HOST_OWNER, &access_window_request().expect("request"))
            .expect("access");
        let opened = host.opened.lock().expect("opened");
        assert_eq!(opened[0].chrome, chrome_for(ACCESS_SURFACE));
    }

    #[test]
    fn the_access_window_is_reused_and_every_other_surface_kind_or_size_is_refused() {
        let (windows, host) = windows();
        let first = windows.open(HOST_OWNER, &request("access", "access")).expect("first");
        let second = windows.open(HOST_OWNER, &request("access", "access")).expect("second");
        assert_eq!(first, second);
        assert_eq!(host.opened.lock().expect("opened").len(), 1);
        assert_eq!(host.focused.lock().expect("focused").len(), 1);

        let mut launcher = request("access", "access");
        launcher.kind = WindowKind::Launcher;
        assert!(matches!(
            windows.open(HOST_OWNER, &launcher),
            Err(WindowError::UnsupportedKind { .. })
        ));
        let mut huge = request("access", "other");
        huge.size = WindowSize {
            width: stache_sdk::MAX_WINDOW_DIMENSION + 1,
            height: 420,
        };
        assert!(matches!(
            windows.open(HOST_OWNER, &huge),
            Err(WindowError::InvalidSize { .. })
        ));
        for surface in ["accessx", "guide", "main", "access&x=1"] {
            let refused = windows.open(HOST_OWNER, &request(surface, "k"));
            assert!(
                matches!(
                    refused,
                    Err(WindowError::UnsupportedSurface { .. } | WindowError::InvalidSurface { .. })
                ),
                "{surface}: {refused:?}"
            );
        }
        assert_eq!(host.opened.lock().expect("opened").len(), 1);
    }

    #[test]
    fn bar_chrome_is_frameless_transparent_unresizable_and_shadowless() {
        assert_eq!(chrome_for(BAR_SURFACE), WindowChrome {
            decorations: false,
            transparent: true,
            resizable: false,
            shadow: false,
        });
    }

    #[test]
    fn validate_admits_a_panel_only_for_the_bar_surface() {
        let (windows, host) = windows();
        let owner = ModuleId::new("bar");
        let id = windows.open(owner, &panel_request("bar", "main")).expect("bar panel");
        assert_eq!(id, WindowId::new(1));
        {
            let opened = host.opened.lock().expect("opened");
            assert_eq!(opened.len(), 1);
            assert!(opened[0].label.starts_with("stache-bar-"));
            assert_eq!(opened[0].kind, WindowKind::NonActivatingPanel);
            assert_eq!(opened[0].origin, Some(ORIGIN));
            assert_eq!(opened[0].chrome, chrome_for(BAR_SURFACE));
            assert_eq!((opened[0].width, opened[0].height), (1200, 36));
        }
        for surface in ["test", "access", "settings"] {
            assert!(
                matches!(
                    windows.open(owner, &panel_request(surface, "other")),
                    Err(WindowError::UnsupportedSurface { .. })
                ),
                "{surface}"
            );
        }
        let mut missing = panel_request("bar", "origin");
        missing.origin = None;
        assert!(matches!(
            windows.open(owner, &missing),
            Err(WindowError::InvalidPlacement { .. })
        ));
        let mut launcher = panel_request("bar", "launcher");
        launcher.kind = WindowKind::Launcher;
        assert!(matches!(
            windows.open(owner, &launcher),
            Err(WindowError::UnsupportedKind { .. })
        ));
        assert_eq!(host.opened.lock().expect("opened").len(), 1);
    }

    #[test]
    fn bar_is_rejected_as_a_standard_window_and_access_stays_standard_only() {
        let (windows, host) = windows();
        let owner = ModuleId::new("one");
        assert!(matches!(
            windows.open(owner, &request("bar", "main")),
            Err(WindowError::UnsupportedSurface { .. })
        ));
        let with_origin = request("access", "main").with_origin(ORIGIN);
        assert!(matches!(
            windows.open(owner, &with_origin),
            Err(WindowError::InvalidPlacement { .. })
        ));
        assert!(host.opened.lock().expect("opened").is_empty());
        windows
            .open(HOST_OWNER, &access_window_request().expect("request"))
            .expect("access stays standard");
        assert_eq!(host.opened.lock().expect("opened")[0].kind, WindowKind::Standard);
        assert_eq!(host.opened.lock().expect("opened")[0].origin, None);
    }

    #[test]
    fn a_panel_never_reaches_the_host_focus_path() {
        let (windows, host) = windows();
        let owner = ModuleId::new("bar");
        let first = windows.open(owner, &panel_request("bar", "main")).expect("open");
        let again = windows.open(owner, &panel_request("bar", "main")).expect("reuse");
        assert_eq!(first, again);
        windows.focus(first).expect("focus is a no-op");
        assert_eq!(host.opened.lock().expect("opened").len(), 1);
        assert!(host.focused.lock().expect("focused").is_empty());
    }

    #[test]
    fn a_standard_window_still_focuses_on_reuse_and_focus() {
        let (windows, host) = windows();
        let owner = ModuleId::new("one");
        let id = windows.open(owner, &request("access", "main")).expect("open");
        windows.open(owner, &request("access", "main")).expect("reuse");
        windows.focus(id).expect("focus");
        assert_eq!(host.focused.lock().expect("focused").len(), 2);
    }

    #[test]
    fn phases_follow_the_native_lifecycle_not_queue_acceptance() {
        let executor = Arc::new(Queued::default());
        let (windows, host) = windows_with(executor.clone());
        let phases = windows.window_phases();
        let owner = ModuleId::new("bar");
        let id = windows.open(owner, &panel_request("bar", "main")).expect("open");
        assert_eq!(phases.snapshot(id).phase, WindowPhase::Requested);
        executor.run();
        assert_eq!(phases.snapshot(id).phase, WindowPhase::Created);
        let label = host.opened.lock().expect("opened")[0].label.clone();
        host.events().page_loaded(&label);
        assert_eq!(phases.snapshot(id).phase, WindowPhase::Loaded);
        windows.close(id);
        assert_eq!(phases.snapshot(id).phase, WindowPhase::Closing);
        // A load callback racing the close cannot move the window backwards.
        host.events().page_loaded(&label);
        assert_eq!(phases.snapshot(id).phase, WindowPhase::Closing);
        executor.run();
        windows.window_closed(&label);
        assert_eq!(phases.snapshot(id).phase, WindowPhase::Closed);
        host.events().page_loaded(&label);
        host.events().page_failed(&label, "late".to_owned());
        assert_eq!(phases.snapshot(id).phase, WindowPhase::Closed);
        assert!(!phases.is_live(id));
    }

    #[test]
    fn a_create_failure_after_queue_acceptance_is_failed_and_discarded_by_close() {
        let executor = Arc::new(Queued::default());
        let (windows, host) = windows_with(executor.clone());
        let phases = windows.window_phases();
        let owner = ModuleId::new("bar");
        host.fail_next.store(true, Ordering::SeqCst);
        let id = windows.open(owner, &panel_request("bar", "main")).expect("accepted");
        assert_eq!(phases.snapshot(id).phase, WindowPhase::Requested);
        executor.run();
        assert_eq!(
            phases.snapshot(id).phase,
            WindowPhase::Failed("injected creation failure".to_owned())
        );
        assert!(windows.is_empty());
        let label = host.opened.lock().expect("opened")[0].label.clone();
        host.events().page_loaded(&label);
        assert!(matches!(phases.snapshot(id).phase, WindowPhase::Failed(_)));
        windows.close(id);
        assert_eq!(phases.snapshot(id).phase, WindowPhase::Closed);
    }

    #[test]
    fn a_page_load_failure_is_typed_and_terminal() {
        let (windows, host) = windows();
        let phases = windows.window_phases();
        let id = windows.open(ModuleId::new("bar"), &panel_request("bar", "main")).expect("open");
        let label = host.opened.lock().expect("opened")[0].label.clone();
        host.events().page_failed(&label, "timeout".to_owned());
        assert_eq!(
            phases.snapshot(id).phase,
            WindowPhase::Failed("timeout".to_owned())
        );
        host.events().page_loaded(&label);
        assert!(matches!(phases.snapshot(id).phase, WindowPhase::Failed(_)));
    }

    #[test]
    fn a_stop_before_the_queued_create_runs_never_reports_created() {
        let executor = Arc::new(Queued::default());
        let (windows, host) = windows_with(executor.clone());
        let phases = windows.window_phases();
        let owner = ModuleId::new("bar");
        let id = windows.open(owner, &panel_request("bar", "main")).expect("open");
        windows.release_owner(owner);
        executor.run();
        assert!(host.opened.lock().expect("opened").is_empty());
        let label = host.closed.lock().expect("closed")[0].clone();
        windows.window_closed(&label);
        assert_eq!(phases.snapshot(id).phase, WindowPhase::Closed);
    }

    #[test]
    fn reposition_moves_only_the_exact_live_window_of_the_owner() {
        let (windows, host) = windows();
        let owner = ModuleId::new("bar");
        let id = windows.open(owner, &panel_request("bar", "main")).expect("open");
        let label = host.opened.lock().expect("opened")[0].label.clone();
        windows.reposition(owner, id, frame(10)).expect("move");
        assert_eq!(*host.moved.lock().expect("moved"), vec![(
            label.clone(),
            frame(10)
        )]);
        assert!(windows.reposition(ModuleId::new("other"), id, frame(11)).is_err());
        assert!(windows.reposition(owner, WindowId::new(99), frame(12)).is_err());
        windows.close(id);
        assert!(windows.reposition(owner, id, frame(13)).is_err());
        windows.window_closed(&label);
        let successor = windows.open(owner, &panel_request("bar", "main")).expect("successor");
        assert_ne!(successor, id);
        assert!(windows.reposition(owner, id, frame(14)).is_err());
        assert_eq!(host.moved.lock().expect("moved").len(), 1);
    }

    #[test]
    fn a_queued_reposition_is_dropped_when_a_close_wins_the_race() {
        let executor = Arc::new(Queued::default());
        let (windows, host) = windows_with(executor.clone());
        let owner = ModuleId::new("bar");
        let id = windows.open(owner, &panel_request("bar", "main")).expect("open");
        executor.run();
        windows.reposition(owner, id, frame(10)).expect("queued");
        windows.close(id);
        executor.run();
        assert!(host.moved.lock().expect("moved").is_empty());
    }

    #[test]
    fn a_destroyed_event_reaches_the_host_so_hosted_state_is_dropped() {
        let (windows, host) = windows();
        let owner = ModuleId::new("bar");
        windows.open(owner, &panel_request("bar", "main")).expect("open");
        let label = host.opened.lock().expect("opened")[0].label.clone();
        windows.window_closed(&label);
        assert_eq!(*host.destroyed.lock().expect("destroyed"), [label]);
    }

    #[test]
    fn appearance_maps_to_the_tauri_theme() {
        assert_eq!(theme_for(Appearance::System), None);
        assert_eq!(theme_for(Appearance::Light), Some(tauri::Theme::Light));
        assert_eq!(theme_for(Appearance::Dark), Some(tauri::Theme::Dark));
    }

    fn applied(host: &FakeHost) -> Vec<Appearance> {
        host.appearances.lock().expect("appearances").clone()
    }

    #[test]
    fn a_change_with_no_live_window_reaches_the_application_before_a_later_build() {
        let queued = Arc::new(Queued::default());
        let (windows, host) = windows_with(queued.clone());
        windows.set_appearance(Appearance::Dark);
        assert!(
            applied(&host).is_empty(),
            "native work waits for the main thread"
        );
        windows.open(ModuleId::new("one"), &request("access", "a")).expect("open");
        queued.run();
        assert_eq!(*host.order.lock().expect("order"), ["apply Dark", "open"]);
    }

    #[test]
    fn dark_then_system_with_no_window_live_clears_the_application_appearance() {
        let (windows, host) = windows();
        windows.set_appearance(Appearance::Dark);
        let id = windows.open(ModuleId::new("one"), &request("access", "a")).expect("open");
        windows.close(id);
        windows.set_appearance(Appearance::System);
        windows.open(ModuleId::new("two"), &request("access", "b")).expect("reopen");
        assert_eq!(*host.order.lock().expect("order"), [
            "apply Dark",
            "open",
            "apply System",
            "open"
        ]);
    }

    #[test]
    fn the_first_value_is_always_applied_and_an_unchanged_value_does_nothing() {
        let (windows, host) = windows();
        windows.set_appearance(Appearance::System);
        windows.set_appearance(Appearance::System);
        assert_eq!(applied(&host), [Appearance::System]);
        windows.open(ModuleId::new("one"), &request("access", "a")).expect("open");
        windows.set_appearance(Appearance::System);
        assert_eq!(applied(&host), [Appearance::System]);
        // A change reaches the application; live windows are never recreated.
        windows.set_appearance(Appearance::Light);
        assert_eq!(applied(&host), [Appearance::System, Appearance::Light]);
        assert_eq!(host.opened.lock().expect("opened").len(), 1);
        assert!(host.closed.lock().expect("closed").is_empty());
    }

    #[test]
    fn a_superseded_queued_value_is_not_applied() {
        let queued = Arc::new(Queued::default());
        let (windows, host) = windows_with(queued.clone());
        windows.set_appearance(Appearance::Light);
        windows.set_appearance(Appearance::Dark);
        queued.run();
        assert_eq!(applied(&host), [Appearance::Dark]);
    }

    /// Runs jobs inline, or refuses them while `refuse` is set.
    #[derive(Default)]
    struct Refusing {
        refuse: AtomicBool,
    }

    impl MainThreadExecutor for Refusing {
        fn is_main_thread(&self) -> bool { true }

        fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
            if self.refuse.load(Ordering::SeqCst) {
                return Err(ThreadError::GroupStopped);
            }
            job();
            Ok(())
        }
    }

    #[test]
    fn a_failed_dispatch_is_retried_by_the_same_value() {
        let executor = Arc::new(Refusing::default());
        let (windows, host) = windows_with(executor.clone());
        executor.refuse.store(true, Ordering::SeqCst);
        windows.set_appearance(Appearance::Dark);
        assert!(applied(&host).is_empty());
        executor.refuse.store(false, Ordering::SeqCst);
        windows.set_appearance(Appearance::Dark);
        assert_eq!(applied(&host), [Appearance::Dark]);
    }

    #[derive(Default)]
    struct RecordingAdapter {
        events: Mutex<Vec<String>>,
        fail_host: bool,
    }

    impl RecordingAdapter {
        fn log(&self, event: &str) { self.events.lock().expect("events").push(event.to_owned()); }

        fn events(&self) -> Vec<String> { self.events.lock().expect("events").clone() }
    }

    impl PanelAdapter for RecordingAdapter {
        fn host(
            &self,
            label: &str,
            _: &NativeWindowHandle,
        ) -> Result<(), stache_macos::PanelError> {
            self.log(&format!("host {label}"));
            if self.fail_host {
                Err(stache_macos::PanelError::UnexpectedLayout { subviews: 0 })
            } else {
                Ok(())
            }
        }

        fn sync_frame(
            &self,
            label: &str,
            frame: TopLeftFrame,
        ) -> Result<(), stache_macos::PanelError> {
            self.log(&format!(
                "sync {label} {} {} {} {}",
                frame.x, frame.y, frame.width, frame.height
            ));
            Ok(())
        }

        fn unhost(&self, label: &str) { self.log(&format!("unhost {label}")); }

        fn discard(&self, label: &str) { self.log(&format!("discard {label}")); }

        fn host_popover(
            &self,
            label: &str,
            _: &NativeWindowHandle,
            on_resign: Box<dyn Fn()>,
        ) -> Result<(), stache_macos::PanelError> {
            self.log(&format!("host popover {label}"));
            if self.fail_host {
                return Err(stache_macos::PanelError::UnexpectedLayout { subviews: 0 });
            }
            on_resign();
            Ok(())
        }
    }

    fn hosting(fail_host: bool) -> (Arc<RecordingAdapter>, PanelHosting) {
        let adapter = Arc::new(RecordingAdapter {
            fail_host,
            ..RecordingAdapter::default()
        });
        let hosting = PanelHosting::new(adapter.clone());
        (adapter, hosting)
    }

    #[allow(clippy::unnecessary_wraps)] // Mirrors Tauri's `ns_window()` result.
    fn pointer(byte: &mut u8) -> NsWindow<String> { Ok(std::ptr::from_mut(byte).cast()) }

    #[test]
    fn a_created_panel_is_hosted_exactly_once_and_never_focused() {
        let (adapter, hosting) = hosting(false);
        let mut byte = 0_u8;
        hosting.register("w");
        assert_eq!(hosting.adopt("w", pointer(&mut byte)), Ok(()));
        assert_eq!(adapter.events(), ["host w"]);
        assert!(hosting.is_panel("w"));
        assert!(!hosting.is_panel("other"));
    }

    #[test]
    fn no_native_window_or_null_pointer_never_reaches_the_adapter_host() {
        let (adapter, hosting) = hosting(false);
        hosting.register("w");
        assert!(hosting.adopt::<String>("w", Ok(std::ptr::null_mut())).is_err());
        assert!(hosting.adopt("w", Err("gone".to_owned())).is_err());
        assert!(!adapter.events().iter().any(|event| event.starts_with("host")));
    }

    #[test]
    fn a_host_failure_unhosts_and_forgets_the_label_so_the_caller_can_destroy() {
        let (adapter, hosting) = hosting(true);
        let mut byte = 0_u8;
        hosting.register("w");
        let error = hosting.adopt("w", pointer(&mut byte)).expect_err("host failure");
        assert!(error.contains("unexpected layout"), "{error}");
        assert_eq!(adapter.events(), ["host w", "unhost w"]);
        assert!(!hosting.is_panel("w"));
    }

    #[test]
    fn closing_unhosts_before_anything_else_and_is_idempotent() {
        let (adapter, hosting) = hosting(false);
        let mut byte = 0_u8;
        hosting.register("w");
        hosting.adopt("w", pointer(&mut byte)).expect("host");
        hosting.before_close("w");
        hosting.before_close("w");
        assert_eq!(adapter.events(), ["host w", "unhost w", "unhost w"]);
        assert!(!hosting.is_panel("w"));
    }

    #[test]
    fn a_destroyed_window_drops_hosted_state_without_unhosting() {
        let (adapter, hosting) = hosting(false);
        let mut byte = 0_u8;
        hosting.register("w");
        hosting.adopt("w", pointer(&mut byte)).expect("host");
        hosting.destroyed("w");
        assert_eq!(adapter.events(), ["host w", "discard w"]);
        assert!(!hosting.is_panel("w"));
    }

    #[derive(Default)]
    struct ResignEvents(Mutex<Vec<String>>);

    impl WindowEvents for ResignEvents {
        fn page_loaded(&self, _: &str) {}

        fn page_failed(&self, _: &str, _: String) {}

        fn popover_resigned(&self, label: &str) {
            self.0.lock().expect("resigned").push(label.to_owned());
        }
    }

    #[test]
    fn the_popover_is_hosted_with_a_resign_callback_that_reaches_the_events() {
        let (adapter, hosting) = hosting(false);
        let events = Arc::new(ResignEvents::default());
        let sink: Arc<dyn WindowEvents> = events.clone();
        let cell = Arc::new(OnceLock::new());
        let _ = cell.set(Arc::downgrade(&sink));
        let mut byte = 0_u8;
        hosting.register(POPOVER_LABEL);
        hosting.adopt_popover(POPOVER_LABEL, pointer(&mut byte), cell).expect("host");
        assert_eq!(adapter.events(), ["host popover stache-popover"]);
        assert_eq!(*events.0.lock().expect("resigned"), [POPOVER_LABEL]);
        hosting.before_close(POPOVER_LABEL);
        assert_eq!(
            adapter.events().last().map(String::as_str),
            Some("unhost stache-popover")
        );
    }

    #[test]
    fn a_popover_host_failure_unhosts_and_forgets_the_label() {
        let (adapter, hosting) = hosting(true);
        let mut byte = 0_u8;
        hosting.register(POPOVER_LABEL);
        let error = hosting
            .adopt_popover(POPOVER_LABEL, pointer(&mut byte), Arc::new(OnceLock::new()))
            .expect_err("host failure");
        assert!(error.contains("unexpected layout"), "{error}");
        assert_eq!(adapter.events(), [
            "host popover stache-popover",
            "unhost stache-popover"
        ]);
        assert!(!hosting.is_panel(POPOVER_LABEL));
    }

    #[test]
    fn repositioning_places_the_panel_at_the_requested_frame_not_the_tauri_window() {
        let (adapter, hosting) = hosting(false);
        let mut byte = 0_u8;
        hosting.reposition("w", frame(14));
        hosting.register("w");
        hosting.adopt("w", pointer(&mut byte)).expect("host");
        // Same frame twice (a topology change that keeps the frame) reaches
        // the panel twice: the system may have moved it meanwhile.
        hosting.reposition("w", frame(14));
        hosting.reposition("w", frame(14));
        hosting.reposition("other", frame(14));
        assert_eq!(adapter.events(), [
            "host w",
            "sync w 14 2 640 30",
            "sync w 14 2 640 30"
        ]);
    }
}
