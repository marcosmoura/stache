//! Host-owned window bookkeeping and the Tauri adapter.
//!
//! The SDK validates ownership, surfaces, keys and kinds. This module owns the
//! reuse map and hands native work to the main thread without ever waiting for
//! it. Windows are labelled `stache-test-<n>`, the only labels the test-window
//! capability grants.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use stache_sdk::{
    LogLevel, LogRecord, Logger, MainThreadExecutor, ModuleId, Operation, SurfaceId, WindowBackend,
    WindowError, WindowId, WindowRequest,
};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// Owner used for the explicit developer test window.
pub const HOST_OWNER: ModuleId = ModuleId::new("stache");

/// Surface served to the explicit test window.
pub const TEST_SURFACE: SurfaceId = SurfaceId::new("test");

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
    /// Closes an existing window, if it still exists.
    fn close(&self, label: &str);

    /// Whether the page in this window finished loading.
    ///
    /// Native adapters report their page-load hook; fake adapters are already
    /// loaded.
    fn is_loaded(&self, _label: &str) -> bool { true }
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
        .inner_size(f64::from(spec.width), f64::from(spec.height));
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

    fn close(&self, label: &str) {
        if let Some(window) = self.app.get_webview_window(label) {
            let _ = window.close();
        }
        #[cfg(feature = "smoke")]
        self.loaded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|loaded| loaded != label);
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
    if request.surface != TEST_SURFACE {
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
                let label = format!("stache-test-{}", id.get());
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
        if let Err(error) = self.shared.dispatch(move |host| host.close(&entry.label)) {
            self.shared
                .log(LogLevel::Warn, format!("window close dispatch failed: {error}"));
        }
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
            if let Err(error) = self.shared.dispatch(move |host| host.close(&label)) {
                self.shared.log(
                    LogLevel::Warn,
                    format!("window release dispatch failed: {error}"),
                );
            }
        }
    }

    fn close_all(&self) {
        let labels: Vec<String> = {
            let mut state = self.shared.lock();
            let labels = state.by_id.values().map(|entry| entry.label.clone()).collect();
            state.by_id.clear();
            state.by_key.clear();
            labels
        };
        for label in labels {
            if let Err(error) = self.shared.dispatch(move |host| host.close(&label)) {
                self.shared.log(
                    LogLevel::Warn,
                    format!("window close-all dispatch failed: {error}"),
                );
            }
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;

    use stache_sdk::{
        DiscardLogger, MainThreadExecutor, ThreadError, WindowKey, WindowKind, WindowRequest,
        WindowSize,
    };

    use super::*;

    #[derive(Default)]
    struct FakeHost {
        opened: Mutex<Vec<NativeWindowSpec>>,
        focused: Mutex<Vec<String>>,
        closed: Mutex<Vec<String>>,
        fail_next: AtomicBool,
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

        fn close(&self, label: &str) { self.closed.lock().expect("closed").push(label.to_owned()); }
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
        windows.close_all();
        executor.run();
        assert!(host.opened.lock().expect("opened").is_empty());
        assert!(windows.is_empty());

        windows.open(owner, &request("test", "a")).expect("reopen");
        executor.run();
        assert_eq!(host.opened.lock().expect("opened").len(), 1);
        assert_eq!(windows.len(), 1);
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
}
