//! Host-owned window bookkeeping and the Tauri adapter.
//!
//! The SDK validates ownership, surfaces, keys and kinds. This module owns the
//! reuse map and hands native work to the main thread without ever waiting for
//! it. Windows are labelled `stache-<surface>-<n>`: `stache-test-<n>` is granted
//! only the test-window capability and `stache-access-<n>` only the access-window
//! capability.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use stache_sdk::{
    LogLevel, LogRecord, Logger, MainThreadExecutor, ModuleId, Operation, SurfaceId, WindowBackend,
    WindowError, WindowId, WindowRequest,
};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::shutdown::{NativeOwner, OwnerAnswer};

/// Owner used for the explicit developer test window.
pub const HOST_OWNER: ModuleId = ModuleId::new("stache");

/// Surface served to the explicit test window.
pub const TEST_SURFACE: SurfaceId = SurfaceId::new("test");

/// Surface served to the Accessibility guidance window.
pub const ACCESS_SURFACE: SurfaceId = SurfaceId::new("access");

/// Logical width of the Accessibility guidance window.
pub const ACCESS_WIDTH: u32 = 480;

/// Logical height of the Accessibility guidance window.
pub const ACCESS_HEIGHT: u32 = 640;

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
/// The access surface is frameless and transparent; every other surface keeps
/// the decorated, opaque builder defaults.
#[must_use]
pub fn chrome_for(surface: SurfaceId) -> WindowChrome {
    if surface == ACCESS_SURFACE {
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
    /// Closes an existing window and reports what happened to it.
    fn close(&self, label: &str) -> CloseOutcome;

    /// Whether the page in this window finished loading.
    ///
    /// Native adapters report their page-load hook; fake adapters are already
    /// loaded.
    fn is_loaded(&self, _label: &str) -> bool { true }
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
}

/// A Tauri [window host](WindowHost).
pub struct TauriWindowHost {
    app: AppHandle,
    #[cfg(feature = "smoke")]
    loaded: Arc<Mutex<Vec<String>>>,
}

impl TauriWindowHost {
    /// Creates the adapter for one running application.
    #[allow(clippy::missing_const_for_fn)] // Smoke-only state needs `Arc::new`.
    #[must_use]
    pub fn new(app: AppHandle) -> Self {
        Self {
            app,
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

impl WindowHost for TauriWindowHost {
    fn open(&self, spec: &NativeWindowSpec) -> Result<(), String> {
        let builder = WebviewWindowBuilder::new(
            &self.app,
            &spec.label,
            WebviewUrl::App(format!("index.html?surface={}", spec.surface.as_str()).into()),
        )
        .title("Stache Next")
        .visible(true)
        .inner_size(f64::from(spec.width), f64::from(spec.height))
        .decorations(spec.chrome.decorations)
        .transparent(spec.chrome.transparent)
        .resizable(spec.chrome.resizable)
        .shadow(spec.chrome.shadow);
        #[cfg(feature = "smoke")]
        let builder = {
            let loaded = Arc::clone(&self.loaded);
            builder.on_page_load(move |window, payload| {
                if payload.event() == tauri::webview::PageLoadEvent::Finished {
                    loaded
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(window.label().to_owned());
                }
            })
        };
        builder
            .build()
            .map(|window| {
                let _ = window.show();
                let _ = window.set_focus();
            })
            .map_err(|error| error.to_string())
    }

    fn focus(&self, label: &str) {
        if let Some(window) = self.app.get_webview_window(label) {
            let _ = window.show();
            let _ = window.set_focus();
        }
    }

    fn close(&self, label: &str) -> CloseOutcome {
        #[cfg(feature = "smoke")]
        self.loaded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|loaded| loaded != label);
        let Some(window) = self.app.get_webview_window(label) else {
            return CloseOutcome::AlreadyGone;
        };
        match window.close() {
            Ok(()) => CloseOutcome::Closing,
            Err(error) => CloseOutcome::Failed(error.to_string()),
        }
    }

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
}

#[derive(Default)]
struct WindowState {
    by_key: BTreeMap<(ModuleId, String), Entry>,
    by_id: BTreeMap<WindowId, Entry>,
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
    state: Mutex<WindowState>,
}

impl NativeWindows {
    /// Creates the bookkeeping layer.
    #[must_use]
    pub fn new(
        main_thread: Arc<dyn MainThreadExecutor>,
        host: Arc<dyn WindowHost>,
        logger: Arc<dyn Logger>,
    ) -> Self {
        Self {
            shared: Arc::new(WindowShared {
                main_thread,
                host,
                logger,
                next: AtomicU64::new(0),
                state: Mutex::new(WindowState::default()),
            }),
        }
    }

    /// Opens the explicit standard test window without a module request.
    ///
    /// # Errors
    /// Returns a typed validation or dispatch failure.
    pub fn open_test_window(&self) -> Result<WindowId, WindowError> {
        let request = WindowRequest::new(
            stache_sdk::WindowKey::new("test")
                .map_err(|error| WindowError::Backend(error.to_string()))?,
            TEST_SURFACE,
            stache_sdk::WindowSize::new(900, 640)
                .map_err(|error| WindowError::Backend(error.to_string()))?,
            stache_sdk::WindowKind::Standard,
        );
        self.open(HOST_OWNER, &request)
    }

    /// Removes a window that the OS destroyed, without asking it to close.
    pub fn window_closed(&self, label: &str) { self.shared.forget_label(label); }

    /// Queues one native close and records the label until its event arrives.
    fn dispatch_close(&self, label: &str) {
        self.shared.lock().closing.insert(label.to_owned());
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

    fn forget(&self, id: WindowId) {
        let mut state = self.lock();
        state.by_id.remove(&id);
        state.by_key.retain(|_, entry| entry.id != id);
    }

    fn forget_label(&self, label: &str) {
        let mut state = self.lock();
        state.by_id.retain(|_, entry| entry.label != label);
        state.by_key.retain(|_, entry| entry.label != label);
        state.closing.remove(label);
        state.settle();
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
    if request.kind != stache_sdk::WindowKind::Standard {
        return Err(WindowError::UnsupportedKind { kind: request.kind });
    }
    if !request.surface.is_safe() {
        return Err(WindowError::InvalidSurface { surface: request.surface });
    }
    if request.surface != TEST_SURFACE && request.surface != ACCESS_SURFACE {
        return Err(WindowError::UnsupportedSurface { surface: request.surface });
    }
    Ok(())
}

enum Reserved {
    Existing(WindowId, String),
    New(WindowId, String),
}

impl WindowBackend for NativeWindows {
    // The guard must cover both the lookup and the reservation; releasing it
    // between them is the duplicate-window race this method exists to prevent.
    #[allow(clippy::significant_drop_tightening, clippy::option_if_let_else)]
    fn open(&self, owner: ModuleId, request: &WindowRequest) -> Result<WindowId, WindowError> {
        validate(request)?;
        let key = (owner, request.key.as_str().to_owned());
        let reserved = {
            let mut state = self.shared.lock();
            let existing = state.by_key.get(&key).cloned();
            if let Some(entry) = existing {
                Reserved::Existing(entry.id, entry.label)
            } else {
                let id = WindowId::new(self.shared.next.fetch_add(1, Ordering::Relaxed) + 1);
                let label = format!("stache-{}-{}", request.surface.as_str(), id.get());
                let entry = Entry {
                    id,
                    label: label.clone(),
                    owner,
                };
                state.by_key.insert(key, entry.clone());
                state.by_id.insert(id, entry);
                Reserved::New(id, label)
            }
        };
        let (id, label) = match reserved {
            Reserved::Existing(id, label) => {
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
        };
        let shared = Arc::clone(&self.shared);
        let dispatched = self.shared.dispatch(move |host| {
            if !shared.is_live(id) {
                return;
            }
            if let Err(error) = host.open(&spec) {
                shared.forget(id);
                shared.log(LogLevel::Warn, format!("cannot create window {label}: {error}"));
            }
        });
        if let Err(error) = dispatched {
            self.shared.forget(id);
            self.shared.log(LogLevel::Warn, format!("window dispatch failed: {error}"));
            return Err(error);
        }
        Ok(id)
    }

    fn focus(&self, window: WindowId) -> Result<(), WindowError> {
        let label = self
            .shared
            .lock()
            .by_id
            .get(&window)
            .map(|entry| entry.label.clone())
            .ok_or_else(|| WindowError::Backend(format!("window {window} is not open")))?;
        self.shared.dispatch(move |host| host.focus(&label))
    }

    fn close(&self, window: WindowId) {
        let entry = {
            let mut state = self.shared.lock();
            let Some(entry) = state.by_id.remove(&window) else {
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
    use std::sync::atomic::AtomicBool;

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
        fail_next: AtomicBool,
        fail_close: AtomicBool,
        gone: AtomicBool,
    }

    impl WindowHost for FakeHost {
        fn open(&self, spec: &NativeWindowSpec) -> Result<(), String> {
            self.opened.lock().expect("opened").push(spec.clone());
            if self.fail_next.swap(false, Ordering::SeqCst) {
                return Err("injected creation failure".to_owned());
            }
            Ok(())
        }

        fn focus(&self, label: &str) {
            self.focused.lock().expect("focused").push(label.to_owned());
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
        let first = windows.open(owner, &request("test", "main")).expect("first");
        let second = windows.open(owner, &request("test", "main")).expect("second");
        assert_eq!(first, second);
        assert_eq!(host.opened.lock().expect("opened").len(), 1);
        assert_eq!(host.focused.lock().expect("focused").len(), 1);
    }

    #[test]
    fn closing_removes_the_entry_and_allows_a_new_window() {
        let (windows, host) = windows();
        let owner = ModuleId::new("one");
        let first = windows.open(owner, &request("test", "main")).expect("first");
        windows.close(first);
        assert!(windows.is_empty());
        assert_eq!(host.closed.lock().expect("closed").len(), 1);
        let second = windows.open(owner, &request("test", "main")).expect("second");
        assert_ne!(first, second);
        assert_eq!(host.opened.lock().expect("opened").len(), 2);
    }

    #[test]
    fn an_os_close_removes_the_entry_without_closing_again() {
        let (windows, host) = windows();
        let owner = ModuleId::new("one");
        let id = windows.open(owner, &request("test", "main")).expect("open");
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
        windows.open(one, &request("test", "a")).expect("one a");
        windows.open(one, &request("test", "b")).expect("one b");
        windows.open(two, &request("test", "a")).expect("two a");
        windows.release_owner(one);
        assert_eq!(host.closed.lock().expect("closed").len(), 2);
        assert_eq!(windows.len(), 1);
        windows.release_owner(two);
        assert!(windows.is_empty());
    }

    #[test]
    fn the_explicit_test_window_uses_the_test_label_and_surface() {
        let (windows, host) = windows();
        windows.open_test_window().expect("open");
        let opened = host.opened.lock().expect("opened");
        assert!(opened[0].label.starts_with("stache-test-"));
        assert_eq!(opened[0].surface, TEST_SURFACE);
        assert_eq!((opened[0].width, opened[0].height), (900, 640));
    }

    #[test]
    fn concurrent_open_calls_reserve_only_one_window() {
        let (windows, host) = windows();
        let owner = ModuleId::new("one");
        let mut workers = Vec::new();
        for _ in 0..8 {
            let windows = Arc::clone(&windows);
            workers.push(std::thread::spawn(move || {
                windows.open(owner, &request("test", "shared")).expect("open")
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
        windows.open(owner, &request("test", "one")).expect("open");
        windows.close(WindowId::new(1));
        executor.run();
        assert!(host.opened.lock().expect("opened").is_empty());
        assert_eq!(host.closed.lock().expect("closed").len(), 1);

        windows.open(owner, &request("test", "two")).expect("open");
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
        windows.open(owner, &request("test", "a")).expect("a");
        windows.open(owner, &request("test", "b")).expect("b");
        let answer = windows.release_all();
        executor.run();
        assert!(host.opened.lock().expect("opened").is_empty());
        assert!(windows.is_empty());
        let closed = host.closed.lock().expect("closed").clone();
        for label in closed {
            windows.window_closed(&label);
        }
        assert_eq!(answer.state(), OwnerState::Succeeded);

        windows.open(owner, &request("test", "a")).expect("reopen");
        executor.run();
        assert_eq!(host.opened.lock().expect("opened").len(), 1);
        assert_eq!(windows.len(), 1);
    }

    #[test]
    fn release_all_answers_only_after_every_labels_close_event() {
        let (windows, host) = windows();
        let one = ModuleId::new("one");
        let two = ModuleId::new("two");
        windows.open(one, &request("test", "a")).expect("one a");
        windows.open(one, &request("test", "b")).expect("one b");
        windows.open(two, &request("test", "a")).expect("two a");
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
        windows.open(ModuleId::new("one"), &request("test", "main")).expect("open");
        let answer = windows.release_all();
        assert_eq!(answer.state(), OwnerState::Succeeded);
        assert_eq!(host.closed.lock().expect("closed").len(), 1);
    }

    #[test]
    fn a_failed_close_makes_the_release_a_failure() {
        let (windows, host) = windows();
        host.fail_close.store(true, Ordering::SeqCst);
        windows.open(ModuleId::new("one"), &request("test", "main")).expect("open");
        let answer = windows.release_all();
        assert_eq!(answer.state(), OwnerState::Failed);
    }

    #[test]
    fn a_missing_close_event_leaves_the_release_pending() {
        let (windows, _host) = windows();
        windows.open(ModuleId::new("one"), &request("test", "main")).expect("open");
        let answer = windows.release_all();
        assert_eq!(answer.state(), OwnerState::Pending);
        assert!(
            !answer.is_answered(),
            "a window without its Destroyed event is not acknowledged"
        );
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
        let first = windows.open(owner, &request("test", "main")).expect("reservation");
        assert!(
            windows.is_empty(),
            "a failed native creation must not stay tracked"
        );
        let second = windows.open(owner, &request("test", "main")).expect("retry");
        assert_ne!(first, second);
        assert_eq!(windows.len(), 1);
        assert_eq!(host.opened.lock().expect("opened").len(), 2);
    }

    #[test]
    fn the_backend_rechecks_kind_size_and_surface_for_host_requests() {
        let (windows, host) = windows();
        let owner = ModuleId::new("one");
        let mut launcher = request("test", "main");
        launcher.kind = WindowKind::Launcher;
        assert!(matches!(
            windows.open(owner, &launcher),
            Err(WindowError::UnsupportedKind { .. })
        ));
        let mut zero = request("test", "main");
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
    fn access_and_test_host_requests_are_accepted_with_their_own_labels() {
        let (windows, host) = windows();
        windows.open_test_window().expect("test");
        windows
            .open(HOST_OWNER, &access_window_request().expect("request"))
            .expect("access");
        let opened = host.opened.lock().expect("opened");
        assert_eq!(opened.len(), 2);
        assert!(opened[0].label.starts_with("stache-test-"));
        assert_eq!(opened[0].surface, TEST_SURFACE);
        assert!(
            opened[1].label.starts_with("stache-access-"),
            "{}",
            opened[1].label
        );
        assert_eq!(opened[1].surface, ACCESS_SURFACE);
        let (width, height) = (opened[1].width, opened[1].height);
        drop(opened);
        assert_eq!((width, height), (480, 640));
        assert!((1..=stache_sdk::MAX_WINDOW_DIMENSION).contains(&width));
        assert!((1..=stache_sdk::MAX_WINDOW_DIMENSION).contains(&height));
    }

    #[test]
    fn chrome_is_frameless_for_access_and_default_for_test() {
        assert_eq!(chrome_for(ACCESS_SURFACE), WindowChrome {
            decorations: false,
            transparent: true,
            resizable: false,
            shadow: true,
        });
        assert_eq!(chrome_for(TEST_SURFACE), WindowChrome {
            decorations: true,
            transparent: false,
            resizable: true,
            shadow: true,
        });
        let (windows, host) = windows();
        windows.open_test_window().expect("test");
        windows
            .open(HOST_OWNER, &access_window_request().expect("request"))
            .expect("access");
        let opened = host.opened.lock().expect("opened");
        assert_eq!(opened[0].chrome, chrome_for(TEST_SURFACE));
        assert_eq!(opened[1].chrome, chrome_for(ACCESS_SURFACE));
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
}
