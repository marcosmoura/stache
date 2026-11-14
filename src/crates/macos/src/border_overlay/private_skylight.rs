//! The shared private `SkyLight` resolver and connection-notification helper
//! (decision 4; same policy as `ax/window_id.rs`).
//!
//! Part 05 (Space changes) and part 06 (window events) register private
//! connection notifications with the same three functions. They share this one
//! `dlsym` resolver and one registration helper instead of declaring them twice.
//!
//! # Inventory (private API)
//!
//! - Symbols, all exported by the `SkyLight` image
//!   (`/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight`) and not
//!   declared in any public SDK header: `SLSMainConnectionID`,
//!   `SLSRegisterConnectionNotifyProc`, `SLSRemoveConnectionNotifyProc`.
//!   **Observed:** all three resolve with `dlsym` on the macOS 26 development
//!   machine (checked once with a throwaway lookup outside Stache).
//! - Minimum need: one connection-level notify registration per event id.
//!
//! The functions are never linked. A missing symbol makes the registration
//! helper return `None`; callers turn that into their own typed failure.
//!
//! # Signatures: assumptions
//!
//! The parameter types were **not observed** (a symbol lookup reveals no
//! types); they are the long-standing C shapes of these functions as the author
//! recalls them, written independently of any border tool or yabai header:
//!
//! - `int SLSMainConnectionID(void)`;
//! - `int SLSRegisterConnectionNotifyProc(int connection, Callback, uint32_t
//!   event, void *context)` and `SLSRemoveConnectionNotifyProc` with the same
//!   parameters, both returning a `CGError`;
//! - `Callback = void (*)(uint32_t event, void *data, <length>, void *context,
//!   int connection)`.

use std::ffi::{CStr, c_char, c_int, c_void};

const IMAGE: &CStr = c"/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight";
const SYM_MAIN_CONNECTION: &CStr = c"SLSMainConnectionID";
const SYM_REGISTER: &CStr = c"SLSRegisterConnectionNotifyProc";
const SYM_REMOVE: &CStr = c"SLSRemoveConnectionNotifyProc";

const RTLD_LAZY: c_int = 0x1;
const RTLD_LOCAL: c_int = 0x4;

/// The connection notify callback shape (see the module documentation).
pub type NotifyCallback = unsafe extern "C" fn(u32, *mut c_void, usize, *mut c_void, c_int);
type MainConnectionFn = unsafe extern "C" fn() -> c_int;
type NotifyProcFn = unsafe extern "C" fn(c_int, NotifyCallback, u32, *mut c_void) -> c_int;

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// Looks `symbol` up on the `SkyLight` image; null when it is absent. Registers
/// and calls nothing.
#[must_use]
pub fn system_lookup(symbol: &CStr) -> *mut c_void {
    // SAFETY: `IMAGE` is a NUL-terminated path literal; the image is already
    // loaded in a GUI process, else this loads it. The handle is intentionally
    // never closed.
    let handle = unsafe { dlopen(IMAGE.as_ptr(), RTLD_LAZY | RTLD_LOCAL) };
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a live image handle and a NUL-terminated symbol name.
    unsafe { dlsym(handle, symbol.as_ptr()) }
}

/// The resolved (or missing) connection-notification symbols.
#[derive(Clone, Copy, Debug)]
pub struct SkyLightSymbols {
    main_connection: Option<MainConnectionFn>,
    register: Option<NotifyProcFn>,
    remove: Option<NotifyProcFn>,
}

impl SkyLightSymbols {
    /// Resolves the symbols on the `SkyLight` image. Registers nothing.
    #[must_use]
    pub fn system() -> Self {
        // SAFETY: the lookup returns only the `dlsym` address of the exact
        // named symbol, or null.
        unsafe { Self::from_lookup(system_lookup) }
    }

    /// Every symbol missing.
    #[must_use]
    pub const fn missing() -> Self {
        Self {
            main_connection: None,
            register: None,
            remove: None,
        }
    }

    /// Resolves through an injected lookup (tests inject missing symbols).
    ///
    /// # Safety
    ///
    /// A non-null address returned for a name must be the address of a function
    /// with the signature documented for that name in the module documentation.
    pub(crate) unsafe fn from_lookup(lookup: impl Fn(&CStr) -> *mut c_void) -> Self {
        // SAFETY (all three): see the function contract; data and function
        // pointers have the same size on every supported target.
        let main = lookup(SYM_MAIN_CONNECTION);
        let register = lookup(SYM_REGISTER);
        let remove = lookup(SYM_REMOVE);
        Self {
            main_connection: (!main.is_null())
                .then(|| unsafe { std::mem::transmute::<*mut c_void, MainConnectionFn>(main) }),
            register: (!register.is_null())
                .then(|| unsafe { std::mem::transmute::<*mut c_void, NotifyProcFn>(register) }),
            remove: (!remove.is_null())
                .then(|| unsafe { std::mem::transmute::<*mut c_void, NotifyProcFn>(remove) }),
        }
    }

    /// The name of the first missing symbol.
    #[must_use]
    pub const fn missing_symbol(&self) -> Option<&'static str> {
        if self.main_connection.is_none() {
            Some("SLSMainConnectionID")
        } else if self.register.is_none() {
            Some("SLSRegisterConnectionNotifyProc")
        } else if self.remove.is_none() {
            Some("SLSRemoveConnectionNotifyProc")
        } else {
            None
        }
    }

    /// Whether `SLSMainConnectionID` resolved (calls nothing).
    #[must_use]
    pub const fn has_main_connection(&self) -> bool { self.main_connection.is_some() }

    /// The process's main `SkyLight` connection id, when the symbol resolved.
    #[must_use]
    pub fn connection(&self) -> Option<c_int> {
        // SAFETY: resolved from the exact symbol; no arguments (module docs).
        self.main_connection.map(|main| unsafe { main() })
    }

    /// Registers `callback` for `event` on `connection` (context null). `None`
    /// when the symbol is missing; otherwise the returned `CGError` (0 is
    /// success).
    #[must_use]
    pub fn register_proc(
        &self,
        connection: c_int,
        callback: NotifyCallback,
        event: u32,
    ) -> Option<c_int> {
        // SAFETY: resolved symbol with the documented shape; the callbacks used
        // ignore the context, which is null.
        self.register
            .map(|register| unsafe { register(connection, callback, event, std::ptr::null_mut()) })
    }

    /// Removes a registration made with [`Self::register_proc`] (best effort;
    /// `None` when the symbol is missing).
    #[must_use]
    pub fn remove_proc(
        &self,
        connection: c_int,
        callback: NotifyCallback,
        event: u32,
    ) -> Option<c_int> {
        // SAFETY: resolved symbol with the documented shape, same arguments as
        // the registration.
        self.remove
            .map(|remove| unsafe { remove(connection, callback, event, std::ptr::null_mut()) })
    }
}
