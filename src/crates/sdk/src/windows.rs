//! Module-declared native windows.
//!
//! A module does not receive a raw window handle and cannot name an arbitrary
//! URL or native flag. Its descriptor declares the surfaces it may open, and a
//! [`WindowCapability`] validates a [`WindowRequest`] against those declarations
//! before handing it to the host's [`WindowBackend`].
//!
//! [`WindowKind::Standard`] and [`WindowKind::NonActivatingPanel`] have native
//! semantics. A panel is placed explicitly ([`WindowRequest::origin`]), moved
//! with [`OwnedWindow::reposition`] and never takes focus. `Launcher` is a
//! reserved name that returns [`WindowError::UnsupportedKind`] until a tool spec
//! defines what it means.
//!
//! Queue acceptance is not creation. The owner observes the real native
//! lifecycle through [`OwnedWindow::phase`] and
//! [`OwnedWindow::wait_phase_change`] ([`WindowPhase`]).

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::module::ModuleId;
use crate::threading::CancellationToken;
use crate::window_phases::{PhaseSnapshot, PhaseWait, WindowPhase, WindowPhases};

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
    /// A borderless panel that never activates the application or takes focus.
    /// It needs an explicit [`WindowRequest::origin`].
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

/// A logical window origin in points, top-left in desktop coordinates.
///
/// Coordinates may be negative on displays left of or above the primary one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowOrigin {
    /// Horizontal position in logical points.
    pub x: i32,
    /// Vertical position in logical points.
    pub y: i32,
}

/// A complete logical window frame used to move or resize a live window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowFrame {
    /// Top-left corner.
    pub origin: WindowOrigin,
    /// Logical size, validated like a request size.
    pub size: WindowSize,
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

/// A request to open one native window.
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
    /// Explicit logical placement. Required for a panel and rejected for a
    /// standard window, which the OS places.
    pub origin: Option<WindowOrigin>,
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
        Self {
            key,
            surface,
            size,
            kind,
            origin: None,
        }
    }

    /// Returns the request with an explicit logical origin.
    #[must_use]
    pub const fn with_origin(mut self, origin: WindowOrigin) -> Self {
        self.origin = Some(origin);
        self
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
    /// A panel has no origin, or a standard window supplied one.
    InvalidPlacement {
        /// The requested kind.
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
            Self::InvalidPlacement { kind } => {
                write!(f, "window kind `{kind}` has an invalid placement")
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

    /// Moves and resizes a live window of this owner.
    ///
    /// The identity is exact: an unknown, closed, replaced or foreign window is
    /// an error and never touches another window. Like `open`, this only queues
    /// native work.
    ///
    /// # Errors
    /// Returns a backend error when the window is not live for `owner` or the
    /// dispatch fails. The default reports that the backend cannot reposition.
    fn reposition(
        &self,
        _owner: ModuleId,
        _window: WindowId,
        _frame: WindowFrame,
    ) -> Result<(), WindowError> {
        Err(WindowError::Backend(
            "window backend cannot reposition".to_owned(),
        ))
    }

    /// Applies the configured native appearance to every live window and
    /// panel and to every window built afterwards, without recreating any.
    /// Like `open`, this only queues native work. The default does nothing.
    fn set_appearance(&self, _appearance: crate::config::Appearance) {}

    /// The shared lifecycle tracker this backend reports into, if any.
    ///
    /// A backend without one never reports more than [`WindowPhase::Requested`].
    fn phases(&self) -> Option<Arc<WindowPhases>> { None }

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
    /// [`WindowError::InvalidPlacement`] for a panel without an origin or a
    /// standard window with one,
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
        match (request.kind, request.origin) {
            (WindowKind::Standard, None) | (WindowKind::NonActivatingPanel, Some(_)) => {}
            (WindowKind::Standard | WindowKind::NonActivatingPanel, _) => {
                return Err(WindowError::InvalidPlacement { kind: request.kind });
            }
            (WindowKind::Launcher, _) => {
                return Err(WindowError::UnsupportedKind { kind: request.kind });
            }
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

    /// Moves and resizes this exact window.
    ///
    /// Valid only while this handle's lease is live in an active session, so a
    /// stale handle never touches a successor window. The backend only queues
    /// the native move.
    ///
    /// # Errors
    /// Returns [`WindowError::InvalidSize`] for an out-of-range size,
    /// [`WindowError::OwnerStopped`] after the session stopped, or a backend
    /// error when the window is no longer live.
    pub fn reposition(&self, frame: WindowFrame) -> Result<(), WindowError> {
        if frame.size.width == 0
            || frame.size.height == 0
            || frame.size.width > MAX_WINDOW_DIMENSION
            || frame.size.height > MAX_WINDOW_DIMENSION
        {
            return Err(WindowError::InvalidSize {
                width: frame.size.width,
                height: frame.size.height,
            });
        }
        let state = self.session.lock();
        if !state.active || self.session.cancellation.is_cancelled() {
            return Err(WindowError::OwnerStopped { owner: self.session.owner });
        }
        let live = state
            .leases
            .get(&self.lease.id)
            .is_some_and(|entry| Arc::ptr_eq(&entry.lease, &self.lease));
        if !live {
            return Err(WindowError::Backend(format!(
                "window {} is not live",
                self.lease.id
            )));
        }
        self.session.backend.reposition(self.session.owner, self.lease.id, frame)
    }

    /// The native lifecycle phase now.
    ///
    /// `Requested` means only that the host queue accepted the request. A
    /// stopped session reads as `Closed`; a backend without a tracker never
    /// leaves `Requested`.
    #[must_use]
    pub fn phase(&self) -> PhaseSnapshot {
        if !self.session.lock().active {
            return PhaseSnapshot {
                phase: WindowPhase::Closed,
                revision: u64::MAX,
            };
        }
        self.session.backend.phases().map_or(
            PhaseSnapshot {
                phase: WindowPhase::Requested,
                revision: 0,
            },
            |phases| phases.snapshot(self.lease.id),
        )
    }

    /// Waits for the phase to differ from `after` (a prior
    /// [`PhaseSnapshot::revision`]), for `timeout`, or for cancellation.
    ///
    /// Either `cancel` or the session's own cancellation ends the wait with
    /// [`PhaseWait::Cancelled`]. No SDK gate is held while waiting and the main
    /// thread is never involved, so call this from a worker. A terminal phase
    /// returns immediately.
    #[must_use]
    pub fn wait_phase_change(
        &self,
        after: u64,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> PhaseWait {
        let deadline = Instant::now() + timeout;
        let tokens = [cancel, &self.session.cancellation];
        if let Some(phases) = self.session.backend.phases() {
            return phases.wait_change(self.lease.id, after, deadline, &tokens);
        }
        let snapshot = PhaseSnapshot {
            phase: WindowPhase::Requested,
            revision: 0,
        };
        // Poll both tokens in short slices; there is no tracker to wake.
        loop {
            if tokens.iter().any(|token| token.is_cancelled()) {
                return PhaseWait::Cancelled;
            }
            let now = Instant::now();
            if now >= deadline {
                return PhaseWait::TimedOut(snapshot);
            }
            let _ = cancel.wait((deadline - now).min(Duration::from_millis(20)));
        }
    }

    /// Releases this lease now. The window stays open while another lease for
    /// the same window exists; later drops are no-ops.
    pub fn close(self) {}

    /// A cloneable, lease-free observer of this exact window.
    ///
    /// Unlike the handle it holds no lease, so keeping it never delays a close,
    /// and unlike [`OwnedWindow::phase`] it keeps reading the tracker after the
    /// session ended, so an owner can wait for the close acknowledgement of a
    /// window it already released.
    #[must_use]
    pub fn watch(&self) -> WindowWatch {
        WindowWatch {
            phases: self.session.backend.phases(),
            id: self.lease.id,
        }
    }
}

/// A lease-free observer of one native window's lifecycle phase.
///
/// See [`OwnedWindow::watch`]. A backend without a tracker never leaves
/// [`WindowPhase::Requested`].
#[derive(Clone)]
pub struct WindowWatch {
    phases: Option<Arc<WindowPhases>>,
    id: WindowId,
}

impl WindowWatch {
    /// The exact window this watch observes.
    #[must_use]
    pub const fn id(&self) -> WindowId { self.id }

    /// The tracker's current phase; an unknown identity reads as closed.
    #[must_use]
    pub fn phase(&self) -> PhaseSnapshot {
        self.phases.as_ref().map_or(
            PhaseSnapshot {
                phase: WindowPhase::Requested,
                revision: 0,
            },
            |phases| phases.snapshot(self.id),
        )
    }

    /// Waits for the phase to differ from `after`, for `timeout`, or until any
    /// token in `cancel` fires. A terminal phase returns immediately. No SDK
    /// gate is held and the main thread is never involved.
    #[must_use]
    pub fn wait_change(
        &self,
        after: u64,
        timeout: Duration,
        cancel: &[&CancellationToken],
    ) -> PhaseWait {
        let deadline = Instant::now() + timeout;
        if let Some(phases) = &self.phases {
            return phases.wait_change(self.id, after, deadline, cancel);
        }
        let snapshot = PhaseSnapshot {
            phase: WindowPhase::Requested,
            revision: 0,
        };
        loop {
            if cancel.iter().any(|token| token.is_cancelled()) {
                return PhaseWait::Cancelled;
            }
            let now = Instant::now();
            if now >= deadline {
                return PhaseWait::TimedOut(snapshot);
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(5)));
        }
    }
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
        moved: Mutex<Vec<(ModuleId, WindowId, WindowFrame)>>,
        phases: Arc<WindowPhases>,
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
            self.phases.begin(owner, id);
            Ok(id)
        }

        fn reposition(
            &self,
            owner: ModuleId,
            window: WindowId,
            frame: WindowFrame,
        ) -> Result<(), WindowError> {
            let live = self
                .entries
                .lock()
                .expect("entries")
                .iter()
                .any(|((entry_owner, _), id)| *entry_owner == owner && *id == window);
            if !live {
                return Err(WindowError::Backend("not live".to_owned()));
            }
            self.moved.lock().expect("moved").push((owner, window, frame));
            Ok(())
        }

        fn phases(&self) -> Option<Arc<WindowPhases>> { Some(Arc::clone(&self.phases)) }

        fn focus(&self, window: WindowId) -> Result<(), WindowError> {
            self.focused.lock().expect("focused").push(window);
            Ok(())
        }

        fn close(&self, window: WindowId) {
            self.closed.lock().expect("closed").push(window);
            self.entries.lock().expect("entries").retain(|_, id| *id != window);
            self.phases.advance(window, WindowPhase::Closed);
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
        let mut request = request("one", SurfaceId::new("test"));
        request.kind = WindowKind::Launcher;
        assert!(matches!(
            windows.request(&request),
            Err(WindowError::UnsupportedKind { kind: WindowKind::Launcher })
        ));
        request.origin = Some(ORIGIN);
        assert!(matches!(
            windows.request(&request),
            Err(WindowError::UnsupportedKind { .. })
        ));
        assert_eq!(backend.open_count(), 0);
    }

    const ORIGIN: WindowOrigin = WindowOrigin { x: -20, y: 8 };

    fn panel(key: &str) -> WindowRequest {
        WindowRequest::new(
            WindowKey::new(key).expect("key"),
            SurfaceId::new("test"),
            WindowSize::new(1000, 40).expect("size"),
            WindowKind::NonActivatingPanel,
        )
        .with_origin(ORIGIN)
    }

    fn frame(x: i32) -> WindowFrame {
        WindowFrame {
            origin: WindowOrigin { x, y: 0 },
            size: WindowSize::new(500, 30).expect("size"),
        }
    }

    #[test]
    fn a_panel_with_an_origin_is_admitted_and_placement_is_validated() {
        let backend = Arc::new(FakeBackend::default());
        let windows = capability(Arc::clone(&backend));
        let window = windows.request(&panel("bar")).expect("panel");
        assert_eq!(backend.open_count(), 1);
        assert_eq!(window.id(), WindowId::new(1));

        let mut without = panel("other");
        without.origin = None;
        assert!(matches!(
            windows.request(&without),
            Err(WindowError::InvalidPlacement {
                kind: WindowKind::NonActivatingPanel
            })
        ));
        let standard = request("std", SurfaceId::new("test")).with_origin(ORIGIN);
        assert!(matches!(
            windows.request(&standard),
            Err(WindowError::InvalidPlacement { kind: WindowKind::Standard })
        ));
        let mut zero = panel("zero");
        zero.size = WindowSize { width: 0, height: 40 };
        assert!(matches!(
            windows.request(&zero),
            Err(WindowError::InvalidSize { .. })
        ));
        assert_eq!(backend.open_count(), 1);
    }

    #[test]
    fn reposition_reaches_only_the_exact_live_window_of_the_owner() {
        let backend = Arc::new(FakeBackend::default());
        let windows = capability(Arc::clone(&backend));
        let window = windows.request(&panel("bar")).expect("panel");
        window.reposition(frame(5)).expect("move");
        assert_eq!(*backend.moved.lock().expect("moved"), vec![(
            windows.owner(),
            window.id(),
            frame(5)
        )]);
        // Another owner naming the same identity is refused by the backend.
        assert!(backend.reposition(ModuleId::new("intruder"), window.id(), frame(9)).is_err());
        // An out-of-range size never reaches the backend.
        let huge = WindowFrame {
            origin: ORIGIN,
            size: WindowSize {
                width: MAX_WINDOW_DIMENSION + 1,
                height: 1,
            },
        };
        assert!(matches!(
            window.reposition(huge),
            Err(WindowError::InvalidSize { .. })
        ));
        assert_eq!(backend.moved.lock().expect("moved").len(), 1);
    }

    #[test]
    fn a_stale_handle_cannot_reposition_after_release_or_a_successor() {
        let backend = Arc::new(FakeBackend::default());
        let windows = capability(Arc::clone(&backend));
        let stale = windows.request(&panel("bar")).expect("first");
        stale.release_without_dropping_fields();
        assert!(stale.reposition(frame(1)).is_err());
        let successor = windows.request(&panel("bar")).expect("successor");
        assert_ne!(successor.id(), stale.id());
        assert!(stale.reposition(frame(2)).is_err());
        successor.reposition(frame(3)).expect("successor moves");
        let moved = backend.moved.lock().expect("moved").clone();
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].1, successor.id());
    }

    #[test]
    fn reposition_after_the_session_stopped_is_refused() {
        let backend = Arc::new(FakeBackend::default());
        let (windows, token) = stopped_capability(Arc::clone(&backend));
        let window = windows.request(&panel("bar")).expect("panel");
        token.cancel();
        assert!(matches!(
            window.reposition(frame(1)),
            Err(WindowError::OwnerStopped { .. })
        ));
        windows.release_owner();
        assert!(backend.moved.lock().expect("moved").is_empty());
    }

    #[test]
    fn queue_acceptance_is_requested_then_created_then_loaded() {
        let backend = Arc::new(FakeBackend::default());
        let windows = capability(Arc::clone(&backend));
        let window = windows.request(&panel("bar")).expect("panel");
        let first = window.phase();
        assert_eq!(first.phase, WindowPhase::Requested);
        assert!(backend.phases.advance(window.id(), WindowPhase::Created));
        let created = window.phase();
        assert_eq!(created.phase, WindowPhase::Created);
        assert_ne!(created.revision, first.revision);
        assert!(backend.phases.advance(window.id(), WindowPhase::Loaded));
        assert_eq!(window.phase().phase, WindowPhase::Loaded);
        assert!(!backend.phases.advance(window.id(), WindowPhase::Created));
        assert_eq!(window.phase().phase, WindowPhase::Loaded);
    }

    #[test]
    fn create_and_load_failures_are_typed_and_terminal() {
        let backend = Arc::new(FakeBackend::default());
        let windows = capability(Arc::clone(&backend));
        let create = windows.request(&panel("a")).expect("a");
        assert!(backend.phases.advance(create.id(), WindowPhase::Failed("create".to_owned())));
        assert_eq!(create.phase().phase, WindowPhase::Failed("create".to_owned()));
        assert!(!backend.phases.advance(create.id(), WindowPhase::Created));
        assert!(!backend.phases.advance(create.id(), WindowPhase::Loaded));

        let load = windows.request(&panel("b")).expect("b");
        backend.phases.advance(load.id(), WindowPhase::Created);
        assert!(backend.phases.advance(load.id(), WindowPhase::Failed("load".to_owned())));
        assert_eq!(load.phase().phase, WindowPhase::Failed("load".to_owned()));
        // A failed phase is returned immediately by a wait.
        let token = CancellationToken::new();
        assert!(matches!(
            load.wait_phase_change(load.phase().revision, Duration::from_secs(5), &token),
            PhaseWait::Changed(PhaseSnapshot {
                phase: WindowPhase::Failed(_),
                ..
            })
        ));
    }

    #[test]
    fn a_late_created_or_loaded_callback_cannot_resurrect_a_closed_window() {
        let backend = Arc::new(FakeBackend::default());
        let windows = capability(Arc::clone(&backend));
        let window = windows.request(&panel("bar")).expect("panel");
        let id = window.id();
        drop(window);
        assert!(!backend.phases.advance(id, WindowPhase::Created));
        assert!(!backend.phases.advance(id, WindowPhase::Loaded));
        assert!(!backend.phases.is_live(id));
        assert_eq!(backend.phases.snapshot(id).phase, WindowPhase::Closed);
        let successor = windows.request(&panel("bar")).expect("successor");
        assert_eq!(successor.phase().phase, WindowPhase::Requested);
    }

    #[test]
    fn a_wait_times_out_at_the_deadline_without_a_change() {
        let backend = Arc::new(FakeBackend::default());
        let windows = capability(Arc::clone(&backend));
        let window = windows.request(&panel("bar")).expect("panel");
        let token = CancellationToken::new();
        let started = Instant::now();
        let outcome =
            window.wait_phase_change(window.phase().revision, Duration::from_millis(40), &token);
        assert!(started.elapsed() >= Duration::from_millis(40));
        assert!(matches!(
            outcome,
            PhaseWait::TimedOut(PhaseSnapshot {
                phase: WindowPhase::Requested,
                ..
            })
        ));
    }

    #[test]
    fn a_wait_wakes_on_a_change_without_losing_it_between_read_and_wait() {
        for _ in 0..200 {
            let backend = Arc::new(FakeBackend::default());
            let windows = capability(Arc::clone(&backend));
            let window = windows.request(&panel("bar")).expect("panel");
            let seen = window.phase().revision;
            let id = window.id();
            let phases = Arc::clone(&backend.phases);
            let writer = std::thread::spawn(move || phases.advance(id, WindowPhase::Created));
            let outcome =
                window.wait_phase_change(seen, Duration::from_secs(5), &CancellationToken::new());
            writer.join().expect("writer");
            assert!(matches!(
                outcome,
                PhaseWait::Changed(PhaseSnapshot {
                    phase: WindowPhase::Created,
                    ..
                })
            ));
        }
    }

    #[test]
    fn caller_cancellation_wakes_a_waiter_promptly() {
        let backend = Arc::new(FakeBackend::default());
        let windows = capability(Arc::clone(&backend));
        let window = windows.request(&panel("bar")).expect("panel");
        let token = CancellationToken::new();
        let seen = window.phase().revision;
        let waiter = {
            let token = token.clone();
            std::thread::spawn(move || {
                window.wait_phase_change(seen, Duration::from_secs(30), &token)
            })
        };
        std::thread::sleep(Duration::from_millis(30));
        token.cancel();
        assert_eq!(waiter.join().expect("waiter"), PhaseWait::Cancelled);
        // An already cancelled token returns without waiting.
        let window = windows.request(&panel("again")).expect("panel");
        assert_eq!(
            window.wait_phase_change(window.phase().revision, Duration::from_secs(30), &token),
            PhaseWait::Cancelled
        );
    }

    #[test]
    fn session_stop_wakes_a_waiter() {
        let backend = Arc::new(FakeBackend::default());
        let (windows, session) = stopped_capability(Arc::clone(&backend));
        let window = windows.request(&panel("bar")).expect("panel");
        let seen = window.phase().revision;
        let waiter = std::thread::spawn(move || {
            window.wait_phase_change(seen, Duration::from_secs(30), &CancellationToken::new())
        });
        std::thread::sleep(Duration::from_millis(30));
        session.cancel();
        assert_eq!(waiter.join().expect("waiter"), PhaseWait::Cancelled);
    }

    #[test]
    fn a_backend_without_a_tracker_never_reports_created() {
        let windows = WindowCapability::new(
            ModuleId::new("module"),
            SURFACES,
            Arc::new(Untracked),
            CancellationToken::new(),
        );
        let window = windows.request(&panel("bar")).expect("panel");
        assert_eq!(window.phase().phase, WindowPhase::Requested);
        assert!(matches!(
            window.wait_phase_change(0, Duration::from_millis(30), &CancellationToken::new()),
            PhaseWait::TimedOut(PhaseSnapshot {
                phase: WindowPhase::Requested,
                ..
            })
        ));
    }

    struct Untracked;

    impl WindowBackend for Untracked {
        fn open(&self, _: ModuleId, _: &WindowRequest) -> Result<WindowId, WindowError> {
            Ok(WindowId::new(1))
        }

        fn focus(&self, _: WindowId) -> Result<(), WindowError> { Ok(()) }

        fn close(&self, _: WindowId) {}

        fn release_owner(&self, _: ModuleId) {}
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
