//! Module-declared native windows.
//!
//! A module does not receive a raw window handle and cannot name an arbitrary
//! URL or native flag. Its descriptor declares the surfaces it may open, and a
//! [`WindowCapability`] validates a [`WindowRequest`] against those declarations
//! before handing it to the host's [`WindowBackend`].
//!
//! Only [`WindowKind::Standard`] has native semantics today. `Launcher` and
//! `NonActivatingPanel` are reserved names that return
//! [`WindowError::UnsupportedKind`] until a tool spec defines what they mean.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use crate::module::ModuleId;
use crate::threading::CancellationToken;

/// Longest accepted owner-local window key in bytes.
pub const MAX_WINDOW_KEY_LENGTH: usize = 64;

/// Largest accepted window dimension in logical pixels.
pub const MAX_WINDOW_DIMENSION: u32 = 8192;

/// Longest accepted surface identifier in bytes.
pub const MAX_SURFACE_LENGTH: usize = 64;

/// A module-declared window surface.
///
/// Surfaces are `'static` identifiers declared in
/// [`ModuleDescriptor::surfaces`](crate::ModuleDescriptor::surfaces). They name
/// a shipped UI route, never a URL supplied by JavaScript.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SurfaceId(&'static str);

impl SurfaceId {
    /// Declares a surface from a stable literal.
    #[must_use]
    pub const fn new(id: &'static str) -> Self { Self(id) }

    /// The literal spelling used in descriptors and errors.
    #[must_use]
    pub const fn as_str(self) -> &'static str { self.0 }

    /// Whether the surface can be used as a safe route query value.
    ///
    /// Accepted characters are ASCII letters, digits, `-` and `_`, so a surface
    /// can never add another query parameter or change routing.
    #[must_use]
    pub fn is_safe(self) -> bool {
        !self.0.is_empty()
            && self.0.len() <= MAX_SURFACE_LENGTH
            && self
                .0
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    }
}

impl fmt::Display for SurfaceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.0) }
}

/// The native presentation a window requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowKind {
    /// A regular resizable window in the normal application layer.
    Standard,
    /// Reserved for a launcher panel; unsupported until its tool spec exists.
    Launcher,
    /// Reserved for a non-activating floating panel; unsupported until specified.
    NonActivatingPanel,
}

impl fmt::Display for WindowKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Standard => "standard",
            Self::Launcher => "launcher",
            Self::NonActivatingPanel => "non-activating panel",
        })
    }
}

/// Logical window dimensions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowSize {
    /// Width in logical pixels.
    pub width: u32,
    /// Height in logical pixels.
    pub height: u32,
}

impl WindowSize {
    /// Creates a validated size.
    ///
    /// # Errors
    /// Returns [`WindowError::InvalidSize`] for zero or oversized dimensions.
    pub const fn new(width: u32, height: u32) -> Result<Self, WindowError> {
        if width == 0
            || height == 0
            || width > MAX_WINDOW_DIMENSION
            || height > MAX_WINDOW_DIMENSION
        {
            return Err(WindowError::InvalidSize { width, height });
        }
        Ok(Self { width, height })
    }
}

/// An owner-local key used to reuse one window across repeated requests.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowKey(String);

impl WindowKey {
    /// Creates a key from an identifier that is stable within one module.
    ///
    /// Accepted characters are ASCII letters, digits, `-`, `_` and `.`.
    ///
    /// # Errors
    /// Returns [`WindowError::InvalidKey`] for an empty, overlong or otherwise
    /// unsafe key. A key never becomes part of a path or a script.
    pub fn new(key: impl Into<String>) -> Result<Self, WindowError> {
        let key = key.into();
        let valid = !key.is_empty()
            && key.len() <= MAX_WINDOW_KEY_LENGTH
            && key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
        if valid {
            Ok(Self(key))
        } else {
            Err(WindowError::InvalidKey { key })
        }
    }

    /// The key text.
    #[must_use]
    pub fn as_str(&self) -> &str { &self.0 }
}

/// A validated request to open one standard window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowRequest {
    /// Owner-local identity used for reuse and focus.
    pub key: WindowKey,
    /// The declared surface and therefore the UI route to load.
    pub surface: SurfaceId,
    /// Logical dimensions.
    pub size: WindowSize,
    /// The presentation kind.
    pub kind: WindowKind,
}

impl WindowRequest {
    /// Creates a request. Surface permission is checked by the capability.
    #[must_use]
    pub const fn new(
        key: WindowKey,
        surface: SurfaceId,
        size: WindowSize,
        kind: WindowKind,
    ) -> Self {
        Self { key, surface, size, kind }
    }
}

/// The host's stable identity for one open window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowId(u64);

impl WindowId {
    /// Creates an identity from the backend's monotonic number.
    #[must_use]
    pub const fn new(value: u64) -> Self { Self(value) }

    /// The numeric value.
    #[must_use]
    pub const fn get(self) -> u64 { self.0 }
}

impl fmt::Display for WindowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{}", self.0) }
}

/// A window request the host cannot honour.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowError {
    /// The key is empty, too long or contains an unsafe character.
    InvalidKey {
        /// The rejected key.
        key: String,
    },
    /// The requested logical size is zero or above the accepted maximum.
    InvalidSize {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
    /// The module did not declare this surface.
    SurfaceNotAllowed {
        /// The module that made the request.
        owner: ModuleId,
        /// The undeclared surface.
        surface: SurfaceId,
    },
    /// The surface spelling is not a safe route identifier.
    InvalidSurface {
        /// The rejected surface.
        surface: SurfaceId,
    },
    /// The host does not implement this surface yet.
    UnsupportedSurface {
        /// The surface requested.
        surface: SurfaceId,
    },
    /// The window kind has no native semantics yet.
    UnsupportedKind {
        /// The reserved kind.
        kind: WindowKind,
    },
    /// The owning module runtime has already stopped.
    OwnerStopped {
        /// The module that retained the capability.
        owner: ModuleId,
    },
    /// The host could not create, focus or release the native window.
    Backend(String),
}

impl fmt::Display for WindowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidKey { key } => write!(f, "invalid window key `{key}`"),
            Self::InvalidSize { width, height } => write!(
                f,
                "window size {width}x{height} is outside 1..={MAX_WINDOW_DIMENSION}"
            ),
            Self::SurfaceNotAllowed { owner, surface } => {
                write!(f, "module `{owner}` did not declare surface `{surface}`")
            }
            Self::InvalidSurface { surface } => {
                write!(f, "surface `{surface}` is not a safe route identifier")
            }
            Self::UnsupportedSurface { surface } => {
                write!(f, "surface `{surface}` is not implemented by this host")
            }
            Self::UnsupportedKind { kind } => {
                write!(f, "window kind `{kind}` is not supported yet")
            }
            Self::OwnerStopped { owner } => {
                write!(f, "module `{owner}` stopped; its windows are closed")
            }
            Self::Backend(message) => write!(f, "window backend failed: {message}"),
        }
    }
}

impl std::error::Error for WindowError {}

/// The host adapter that owns native windows.
///
/// Calls arrive from module lifecycle code running off the main thread. An
/// implementation queues platform work on the main thread and returns without
/// waiting for it; the returned [`WindowId`] is stable for the window's life.
///
/// The capability serializes one session's open/close/release calls. An
/// implementation must not block, call back into a capability, or wait for the
/// main thread; production dispatchers only enqueue.
pub trait WindowBackend: Send + Sync {
    /// Opens a window, or reuses and focuses the one for this owner and key.
    ///
    /// # Errors
    /// Returns a backend error when native creation or dispatch fails.
    fn open(&self, owner: ModuleId, request: &WindowRequest) -> Result<WindowId, WindowError>;

    /// Focuses a window that is still open.
    ///
    /// # Errors
    /// Returns a backend error when the window is gone or dispatch fails.
    fn focus(&self, window: WindowId) -> Result<(), WindowError>;

    /// Closes a window and removes it from the backend's reuse map.
    fn close(&self, window: WindowId);

    /// Closes every window owned by one module.
    ///
    /// The registry calls this when the module stops, including after a failed
    /// configure or start, so an owner never leaks a window.
    fn release_owner(&self, owner: ModuleId);
}

/// The narrow window surface handed to one module through its context.
///
/// The capability binds the module identity, its declared surfaces and the
/// runtime's window session. Every context created for one runtime clone shares
/// the same session, so repeated owner/key requests share one lease map and one
/// cancellation state. A caller cannot widen either by constructing a request.
#[derive(Clone)]
pub struct WindowCapability {
    session: Arc<WindowSession>,
}

struct WindowSession {
    owner: ModuleId,
    surfaces: &'static [SurfaceId],
    backend: Arc<dyn WindowBackend>,
    cancellation: CancellationToken,
    state: Mutex<SessionState>,
}

#[derive(Default)]
struct SessionState {
    active: bool,
    leases: HashMap<WindowId, LeaseEntry>,
}

struct LeaseEntry {
    lease: Arc<Lease>,
    /// Logical handles, updated only under the session gate.
    external: usize,
}

impl WindowCapability {
    pub(crate) fn new(
        owner: ModuleId,
        surfaces: &'static [SurfaceId],
        backend: Arc<dyn WindowBackend>,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            session: Arc::new(WindowSession {
                owner,
                surfaces,
                backend,
                cancellation,
                state: Mutex::new(SessionState {
                    active: true,
                    leases: HashMap::new(),
                }),
            }),
        }
    }

    /// The module that owns windows opened through this capability.
    #[must_use]
    pub fn owner(&self) -> ModuleId { self.session.owner }

    /// The surfaces this module declared.
    #[must_use]
    pub fn surfaces(&self) -> &'static [SurfaceId] { self.session.surfaces }

    /// Validates and opens a window, reusing the owner/key entry when present.
    ///
    /// Repeated requests for one window share a lease: the window closes when
    /// the last [`OwnedWindow`] for that identity is dropped or explicitly
    /// closed. A capability retained after its runtime stopped rejects every
    /// request, and a request that races the stop releases its owner's windows
    /// instead of leaving one open.
    ///
    /// The session gate is held across the backend's open/close/release calls.
    /// A [`WindowBackend`] queues native work and must not block or reenter the
    /// capability, so this serializes bookkeeping without waiting for `AppKit`.
    ///
    /// # Errors
    /// Returns [`WindowError::UnsupportedKind`] for a reserved kind,
    /// [`WindowError::InvalidSurface`] for an unsafe surface spelling,
    /// [`WindowError::SurfaceNotAllowed`] for an undeclared surface,
    /// [`WindowError::OwnerStopped`] after runtime cancellation, or the
    /// backend's failure.
    pub fn request(&self, request: &WindowRequest) -> Result<OwnedWindow, WindowError> {
        let mut state = self.session.lock();
        if !state.active || self.session.cancellation.is_cancelled() {
            return Err(WindowError::OwnerStopped { owner: self.session.owner });
        }
        if request.size.width == 0
            || request.size.height == 0
            || request.size.width > MAX_WINDOW_DIMENSION
            || request.size.height > MAX_WINDOW_DIMENSION
        {
            return Err(WindowError::InvalidSize {
                width: request.size.width,
                height: request.size.height,
            });
        }
        if request.kind != WindowKind::Standard {
            return Err(WindowError::UnsupportedKind { kind: request.kind });
        }
        if !request.surface.is_safe() {
            return Err(WindowError::InvalidSurface { surface: request.surface });
        }
        if !self.session.surfaces.contains(&request.surface) {
            return Err(WindowError::SurfaceNotAllowed {
                owner: self.session.owner,
                surface: request.surface,
            });
        }
        let id = self.session.backend.open(self.session.owner, request)?;
        if !state.active || self.session.cancellation.is_cancelled() {
            // The backend only queued this window. Its runtime was invalidated
            // before the open completed, so release the owner while the session
            // gate still excludes a successor runtime, then report the stop.
            self.session.backend.release_owner(self.session.owner);
            return Err(WindowError::OwnerStopped { owner: self.session.owner });
        }
        if let Some(entry) = state.leases.get_mut(&id) {
            entry.external += 1;
            return Ok(OwnedWindow {
                session: Arc::clone(&self.session),
                lease: Arc::clone(&entry.lease),
            });
        }
        let lease = Arc::new(Lease { id });
        state.leases.insert(id, LeaseEntry {
            lease: Arc::clone(&lease),
            external: 1,
        });
        Ok(OwnedWindow {
            session: Arc::clone(&self.session),
            lease,
        })
    }

    /// Terminally invalidates this runtime's session before its owner cleanup.
    ///
    /// Existing handles keep their identity but can no longer open or close
    /// through this session after [`release_owner`](Self::release_owner).
    pub(crate) fn invalidate(&self) { self.session.lock().active = false; }

    /// Releases every window owned by this session's runtime.
    ///
    /// The session is invalidated and its lease cache cleared while the gate is
    /// held, so an obsolete capability cannot release a successor runtime's
    /// windows or open a new one.
    pub(crate) fn release_owner(&self) {
        let mut state = self.session.lock();
        state.active = false;
        state.leases.clear();
        self.session.backend.release_owner(self.session.owner);
    }

    #[cfg(test)]
    fn cached_leases(&self) -> usize { self.session.lock().leases.len() }
}

impl WindowSession {
    fn lock(&self) -> std::sync::MutexGuard<'_, SessionState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn release(&self, lease: &Arc<Lease>) {
        let mut state = self.lock();
        if !state.active {
            return;
        }
        let Some(entry) = state.leases.get_mut(&lease.id) else {
            return;
        };
        if !Arc::ptr_eq(&entry.lease, lease) {
            return;
        }
        debug_assert!(entry.external > 0, "a lease release without a logical handle");
        entry.external = entry.external.saturating_sub(1);
        if entry.external == 0 {
            state.leases.remove(&lease.id);
            // The backend only queues native work; keeping the gate through the
            // close makes a repeated request observe the removal first.
            self.backend.close(lease.id);
        }
    }
}

/// A module's shared lease on one open window.
///
/// Every request for the same owner/key receives one lease. Dropping or
/// explicitly closing a handle releases only that lease; the window closes when
/// the last lease is released. Dropping the capability itself does not close
/// windows; the registry releases the owner on stop.
pub struct OwnedWindow {
    session: Arc<WindowSession>,
    lease: Arc<Lease>,
}

struct Lease {
    id: WindowId,
}

impl OwnedWindow {
    /// The host identity of this window.
    #[must_use]
    pub fn id(&self) -> WindowId { self.lease.id }

    /// Releases this lease now. The window stays open while another lease for
    /// the same window exists; later drops are no-ops.
    pub fn close(self) {}
}

impl fmt::Debug for OwnedWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnedWindow")
            .field("id", &self.lease.id)
            .finish_non_exhaustive()
    }
}

impl Drop for OwnedWindow {
    fn drop(&mut self) { self.session.release(&self.lease); }
}

#[cfg(test)]
impl OwnedWindow {
    /// Releases the logical lease while keeping both `Arc` fields alive, so a
    /// test can prove the count is not inferred from `Arc::strong_count`.
    fn release_without_dropping_fields(&self) { self.session.release(&self.lease); }
}

/// A backend that refuses every request, used when no native adapter exists.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableWindows;

impl WindowBackend for UnavailableWindows {
    fn open(&self, _: ModuleId, _: &WindowRequest) -> Result<WindowId, WindowError> {
        Err(WindowError::Backend(
            "window service is not available".to_owned(),
        ))
    }

    fn focus(&self, _: WindowId) -> Result<(), WindowError> {
        Err(WindowError::Backend(
            "window service is not available".to_owned(),
        ))
    }

    fn close(&self, _: WindowId) {}

    fn release_owner(&self, _: ModuleId) {}
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Barrier, Mutex};
    use std::time::Duration;

    use super::*;

    #[derive(Default)]
    struct FakeBackend {
        next: Mutex<u64>,
        entries: Mutex<BTreeMap<(ModuleId, String), WindowId>>,
        focused: Mutex<Vec<WindowId>>,
        closed: Mutex<Vec<WindowId>>,
        released: Mutex<Vec<ModuleId>>,
    }

    impl FakeBackend {
        fn open_count(&self) -> u64 { *self.next.lock().expect("next") }
    }

    impl WindowBackend for FakeBackend {
        fn open(&self, owner: ModuleId, request: &WindowRequest) -> Result<WindowId, WindowError> {
            let key = (owner, request.key.as_str().to_owned());
            let existing = self.entries.lock().expect("entries").get(&key).copied();
            if let Some(id) = existing {
                self.focused.lock().expect("focused").push(id);
                return Ok(id);
            }
            let mut next = self.next.lock().expect("next");
            *next += 1;
            let id = WindowId::new(*next);
            drop(next);
            self.entries.lock().expect("entries").insert(key, id);
            Ok(id)
        }

        fn focus(&self, window: WindowId) -> Result<(), WindowError> {
            self.focused.lock().expect("focused").push(window);
            Ok(())
        }

        fn close(&self, window: WindowId) {
            self.closed.lock().expect("closed").push(window);
            self.entries.lock().expect("entries").retain(|_, id| *id != window);
        }

        fn release_owner(&self, owner: ModuleId) {
            self.released.lock().expect("released").push(owner);
            self.entries
                .lock()
                .expect("entries")
                .retain(|(entry_owner, _), _| *entry_owner != owner);
        }
    }

    const SURFACES: &[SurfaceId] = &[SurfaceId::new("test")];

    struct RacingBackend {
        inner: FakeBackend,
        token: CancellationToken,
    }

    impl RacingBackend {
        fn new(token: CancellationToken) -> Self {
            Self {
                inner: FakeBackend::default(),
                token,
            }
        }
    }

    impl WindowBackend for RacingBackend {
        fn open(&self, owner: ModuleId, request: &WindowRequest) -> Result<WindowId, WindowError> {
            let id = self.inner.open(owner, request)?;
            self.token.cancel();
            Ok(id)
        }

        fn focus(&self, window: WindowId) -> Result<(), WindowError> { self.inner.focus(window) }

        fn close(&self, window: WindowId) { self.inner.close(window); }

        fn release_owner(&self, owner: ModuleId) { self.inner.release_owner(owner); }
    }

    fn capability(backend: Arc<FakeBackend>) -> WindowCapability {
        WindowCapability::new(
            ModuleId::new("module"),
            SURFACES,
            backend,
            CancellationToken::new(),
        )
    }

    fn stopped_capability(backend: Arc<FakeBackend>) -> (WindowCapability, CancellationToken) {
        let token = CancellationToken::new();
        (
            WindowCapability::new(ModuleId::new("module"), SURFACES, backend, token.clone()),
            token,
        )
    }

    fn request(key: &str, surface: SurfaceId) -> WindowRequest {
        WindowRequest::new(
            WindowKey::new(key).expect("key"),
            surface,
            WindowSize::new(800, 600).expect("size"),
            WindowKind::Standard,
        )
    }

    #[test]
    fn the_same_owner_and_key_reuses_and_focuses() {
        let backend = Arc::new(FakeBackend::default());
        let windows = capability(Arc::clone(&backend));
        let first = windows.request(&request("one", SurfaceId::new("test"))).expect("first");
        let second = windows.request(&request("one", SurfaceId::new("test"))).expect("second");
        assert_eq!(first.id(), second.id());
        assert_eq!(backend.open_count(), 1);
        assert_eq!(*backend.focused.lock().expect("focused"), vec![first.id()]);
    }

    #[test]
    fn dropping_an_owned_window_closes_it_and_removes_it() {
        let backend = Arc::new(FakeBackend::default());
        let windows = capability(Arc::clone(&backend));
        let window = windows.request(&request("one", SurfaceId::new("test"))).expect("open");
        let id = window.id();
        drop(window);
        assert_eq!(*backend.closed.lock().expect("closed"), vec![id]);
        assert!(backend.entries.lock().expect("entries").is_empty());
    }

    #[test]
    fn two_leases_on_one_window_close_only_after_the_last_drop() {
        let backend = Arc::new(FakeBackend::default());
        let windows = capability(Arc::clone(&backend));
        let first = windows.request(&request("one", SurfaceId::new("test"))).expect("first");
        let second = windows.request(&request("one", SurfaceId::new("test"))).expect("second");
        assert_eq!(first.id(), second.id());
        drop(first);
        assert!(backend.closed.lock().expect("closed").is_empty());
        let id = second.id();
        drop(second);
        assert_eq!(*backend.closed.lock().expect("closed"), vec![id]);
    }

    #[test]
    fn dropping_leases_does_not_accumulate_dead_cache_entries() {
        let backend = Arc::new(FakeBackend::default());
        let windows = capability(Arc::clone(&backend));
        for _ in 0..4 {
            let window = windows.request(&request("one", SurfaceId::new("test"))).expect("open");
            drop(window);
        }
        assert_eq!(windows.cached_leases(), 0);
        assert_eq!(backend.open_count(), 4);
        assert_eq!(backend.closed.lock().expect("closed").len(), 4);
    }

    #[test]
    fn simultaneous_logical_releases_close_once_while_arc_fields_stay_alive() {
        let backend = Arc::new(FakeBackend::default());
        let windows = capability(Arc::clone(&backend));
        let first = windows.request(&request("one", SurfaceId::new("test"))).expect("first");
        let second = windows.request(&request("one", SurfaceId::new("test"))).expect("second");
        let id = first.id();
        let barrier = Arc::new(Barrier::new(2));
        let workers: Vec<_> = [first, second]
            .into_iter()
            .map(|window| {
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    window.release_without_dropping_fields();
                    barrier.wait();
                    drop(window);
                })
            })
            .collect();
        for worker in workers {
            worker.join().expect("release worker");
        }
        assert_eq!(*backend.closed.lock().expect("closed"), vec![id]);
        assert_eq!(windows.cached_leases(), 0);
    }

    #[test]
    fn a_request_after_the_last_lease_closes_opens_a_fresh_window() {
        struct GatedClose {
            inner: FakeBackend,
            entered: std::sync::mpsc::Sender<()>,
            release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
        }

        impl WindowBackend for GatedClose {
            fn open(
                &self,
                owner: ModuleId,
                request: &WindowRequest,
            ) -> Result<WindowId, WindowError> {
                self.inner.open(owner, request)
            }

            fn focus(&self, window: WindowId) -> Result<(), WindowError> {
                self.inner.focus(window)
            }

            fn close(&self, window: WindowId) {
                let _ = self.entered.send(());
                let release = self.release.lock().expect("release").take();
                if let Some(release) = release {
                    let _ = release.recv_timeout(Duration::from_secs(5));
                }
                self.inner.close(window);
            }

            fn release_owner(&self, owner: ModuleId) { self.inner.release_owner(owner); }
        }

        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let backend = Arc::new(GatedClose {
            inner: FakeBackend::default(),
            entered: entered_tx,
            release: Mutex::new(Some(release_rx)),
        });
        let windows = WindowCapability::new(
            ModuleId::new("module"),
            SURFACES,
            backend.clone(),
            CancellationToken::new(),
        );
        let first = windows.request(&request("one", SurfaceId::new("test"))).expect("open");
        let first_id = first.id();
        let drop_thread = std::thread::spawn(move || drop(first));
        entered_rx.recv_timeout(Duration::from_secs(5)).expect("close entered");
        let reopen = std::thread::spawn(move || {
            windows.request(&request("one", SurfaceId::new("test"))).expect("reopen")
        });
        release_tx.send(()).expect("release close");
        drop_thread.join().expect("drop thread");
        let second = reopen.join().expect("reopen thread");
        assert_ne!(second.id(), first_id);
        assert_eq!(*backend.inner.closed.lock().expect("closed"), vec![first_id]);
        assert_eq!(backend.inner.open_count(), 2);
        drop(second);
    }

    #[test]
    fn an_old_request_paused_before_open_cannot_use_a_restarted_session() {
        let backend = Arc::new(FakeBackend::default());
        let old = capability(Arc::clone(&backend));
        let new = WindowCapability::new(
            ModuleId::new("module"),
            SURFACES,
            Arc::clone(&backend) as Arc<dyn WindowBackend>,
            CancellationToken::new(),
        );
        let (gate_tx, gate_rx) = std::sync::mpsc::channel();
        let old_request = {
            let old = old.clone();
            std::thread::spawn(move || {
                gate_rx.recv_timeout(Duration::from_secs(5)).expect("gate");
                old.request(&request("main", SurfaceId::new("test")))
            })
        };
        old.invalidate();
        let new_window = new.request(&request("main", SurfaceId::new("test"))).expect("new");
        gate_tx.send(()).expect("release old request");
        assert!(matches!(
            old_request.join().expect("old request"),
            Err(WindowError::OwnerStopped { .. })
        ));
        assert_eq!(backend.open_count(), 1);
        assert_eq!(new_window.id(), WindowId::new(1));
        drop(old);
        assert!(backend.closed.lock().expect("closed").is_empty());
        drop(new_window);
        assert_eq!(*backend.closed.lock().expect("closed"), vec![WindowId::new(1)]);
    }

    #[test]
    fn a_retained_capability_rejects_requests_after_the_runtime_stopped() {
        let backend = Arc::new(FakeBackend::default());
        let (windows, token) = stopped_capability(Arc::clone(&backend));
        let retained = windows.clone();
        let window = windows.request(&request("one", SurfaceId::new("test"))).expect("open");
        token.cancel();
        backend.release_owner(windows.owner());
        drop(window);
        assert!(matches!(
            retained.request(&request("one", SurfaceId::new("test"))),
            Err(WindowError::OwnerStopped { .. })
        ));
        assert_eq!(backend.open_count(), 1);
    }

    #[test]
    fn a_request_that_races_stop_releases_the_owner_instead_of_resurrecting() {
        let token = CancellationToken::new();
        let backend = Arc::new(RacingBackend::new(token.clone()));
        let windows =
            WindowCapability::new(ModuleId::new("module"), SURFACES, backend.clone(), token);
        assert!(matches!(
            windows.request(&request("one", SurfaceId::new("test"))),
            Err(WindowError::OwnerStopped { .. })
        ));
        assert_eq!(*backend.inner.released.lock().expect("released"), vec![
            ModuleId::new("module")
        ]);
        assert!(backend.inner.entries.lock().expect("entries").is_empty());
    }

    #[test]
    fn unsafe_surface_spellings_are_rejected_before_the_backend() {
        const UNSAFE_SURFACES: &[SurfaceId] = &[SurfaceId::new("test&other=1")];
        let backend = Arc::new(FakeBackend::default());
        let surfaces: &'static [SurfaceId] = UNSAFE_SURFACES;
        let windows = WindowCapability::new(
            ModuleId::new("module"),
            surfaces,
            backend.clone(),
            CancellationToken::new(),
        );
        assert!(!SurfaceId::new("test&other=1").is_safe());
        assert!(matches!(
            windows.request(&request("one", SurfaceId::new("test&other=1"))),
            Err(WindowError::InvalidSurface { .. })
        ));
        assert_eq!(backend.open_count(), 0);
    }

    #[test]
    fn reserved_kinds_fail_without_reaching_the_backend() {
        let backend = Arc::new(FakeBackend::default());
        let windows = capability(Arc::clone(&backend));
        for kind in [WindowKind::Launcher, WindowKind::NonActivatingPanel] {
            let mut request = request("one", SurfaceId::new("test"));
            request.kind = kind;
            assert!(matches!(
                windows.request(&request),
                Err(WindowError::UnsupportedKind { .. })
            ));
        }
        assert_eq!(backend.open_count(), 0);
    }

    #[test]
    fn undeclared_surfaces_fail_without_reaching_the_backend() {
        let backend = Arc::new(FakeBackend::default());
        let windows = capability(Arc::clone(&backend));
        let error = windows
            .request(&request("one", SurfaceId::new("settings")))
            .expect_err("undeclared");
        assert!(matches!(error, WindowError::SurfaceNotAllowed { .. }));
        assert_eq!(backend.open_count(), 0);
    }

    #[test]
    fn keys_and_sizes_are_validated() {
        for key in ["", "white space", "slash/key", "../escape"] {
            assert!(WindowKey::new(key).is_err(), "{key}");
        }
        assert!(WindowKey::new("ok-key_1.2").is_ok());
        assert!(WindowSize::new(0, 10).is_err());
        assert!(WindowSize::new(10, 0).is_err());
        assert!(WindowSize::new(u32::MAX, 10).is_err());
        assert!(WindowSize::new(800, 600).is_ok());
    }

    #[test]
    fn a_hand_built_size_cannot_bypass_the_capability() {
        let backend = Arc::new(FakeBackend::default());
        let windows = capability(Arc::clone(&backend));
        let mut request = request("one", SurfaceId::new("test"));
        request.size = WindowSize { width: 0, height: 600 };
        assert!(matches!(
            windows.request(&request),
            Err(WindowError::InvalidSize { .. })
        ));
        assert_eq!(backend.open_count(), 0);
    }

    #[test]
    fn owner_release_is_forwarded_to_the_backend() {
        let backend = Arc::new(FakeBackend::default());
        let windows = capability(Arc::clone(&backend));
        backend.release_owner(windows.owner());
        assert_eq!(*backend.released.lock().expect("released"), vec![
            windows.owner()
        ]);
    }
}
