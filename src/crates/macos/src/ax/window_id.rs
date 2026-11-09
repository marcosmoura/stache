//! `_AXUIElementGetWindow`, resolved only by runtime symbol lookup.
//!
//! # Inventory (private API)
//!
//! - Symbol: `AXError _AXUIElementGetWindow(AXUIElementRef, CGWindowID *)`,
//!   exported by `HIServices` (re-exported through `ApplicationServices`). It is
//!   **not** declared in any public SDK header.
//! - Missing public equivalent: no public AX attribute exposes the
//!   `CGWindowID` of an `AXUIElement` window, and `CGWindowListCopyWindowInfo`
//!   exposes no AX element. The only public way to correlate the two is by
//!   comparing geometry or title, which is ambiguous for same-size windows.
//! - Minimum need: query/diagnostic metadata and the legacy numeric CLI
//!   selector. The `CGWindowID` is never window identity or action authority.
//!
//! The function is never linked: [`WindowIdResolver::system`] looks it up with
//! `dlopen`/`dlsym` on the `HIServices` and `ApplicationServices` images at
//! construction. A missing symbol makes [`WindowIdResolver::capability`] report
//! [`WindowIdCapability::Unsupported`] and every lookup return
//! [`WindowIdError::Unsupported`]; nothing else is disabled.

use std::ffi::{CStr, c_char, c_int, c_void};

use super::api::{AxElement, AxError, AxUiElementRef, RawAxError};

/// Whether `CGWindowID` lookup exists on this system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowIdCapability {
    /// The private symbol resolved.
    Available,
    /// The private symbol is missing; the capability is disabled.
    Unsupported,
}

/// A failed `CGWindowID` lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowIdError {
    /// The private symbol is missing: the capability is explicitly unsupported.
    Unsupported,
    /// The symbol resolved but the call failed.
    Ax(AxError),
    /// The call succeeded but returned the null window id `0`.
    NoWindow,
}

type GetWindowFn = unsafe extern "C" fn(AxUiElementRef, *mut u32) -> RawAxError;

const SYMBOL: &CStr = c"_AXUIElementGetWindow";
const IMAGES: [&CStr; 2] = [
    c"/System/Library/Frameworks/ApplicationServices.framework/Frameworks/HIServices.framework/HIServices",
    c"/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices",
];

const RTLD_LAZY: c_int = 0x1;
const RTLD_LOCAL: c_int = 0x4;

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// The resolved (or missing) `_AXUIElementGetWindow`.
#[derive(Clone, Copy, Debug)]
pub struct WindowIdResolver {
    function: Option<GetWindowFn>,
}

impl WindowIdResolver {
    /// Resolves the symbol on the system images. Sends no AX message.
    #[must_use]
    pub fn system() -> Self {
        let lookup = |symbol: &CStr| {
            for image in IMAGES {
                // SAFETY: `image` is a NUL-terminated path literal. The framework
                // is already loaded (it is linked), so this returns the existing
                // handle; the handle is intentionally never closed.
                let handle = unsafe { dlopen(image.as_ptr(), RTLD_LAZY | RTLD_LOCAL) };
                if handle.is_null() {
                    continue;
                }
                // SAFETY: `handle` is a live image handle and `symbol` a
                // NUL-terminated name.
                let address = unsafe { dlsym(handle, symbol.as_ptr()) };
                if !address.is_null() {
                    return address;
                }
            }
            std::ptr::null_mut()
        };
        // SAFETY: the lookup only returns the `dlsym` address of the exact
        // private symbol `_AXUIElementGetWindow`, or null.
        unsafe { Self::from_lookup(lookup) }
    }

    /// A resolver whose symbol is missing.
    #[must_use]
    pub const fn missing() -> Self { Self { function: None } }

    /// Resolves through an injected lookup (tests inject a missing symbol).
    ///
    /// A non-null address is assumed to have the documented signature; it is
    /// only ever called through [`Self::window_id`] with a live element.
    ///
    /// # Safety
    ///
    /// A non-null address returned by `lookup` must be the address of a function
    /// with the exact signature `AXError (AXUIElementRef, CGWindowID *)`.
    pub(crate) unsafe fn from_lookup(lookup: impl FnOnce(&CStr) -> *mut c_void) -> Self {
        let address = lookup(SYMBOL);
        let function = if address.is_null() {
            None
        } else {
            // SAFETY: the address came from a symbol lookup of the exact name
            // `_AXUIElementGetWindow`, whose long-stable private signature is
            // `AXError (AXUIElementRef, CGWindowID *)`. Data and function
            // pointers have the same size on every supported target.
            Some(unsafe { std::mem::transmute::<*mut c_void, GetWindowFn>(address) })
        };
        Self { function }
    }

    /// Whether the capability exists.
    #[must_use]
    pub const fn capability(&self) -> WindowIdCapability {
        if self.function.is_some() {
            WindowIdCapability::Available
        } else {
            WindowIdCapability::Unsupported
        }
    }

    /// Reads the `CGWindowID` of one window element.
    ///
    /// # Errors
    ///
    /// [`WindowIdError::Unsupported`] when the symbol is missing, otherwise the
    /// AX failure or [`WindowIdError::NoWindow`].
    pub fn window_id(&self, element: &AxElement) -> Result<u32, WindowIdError> {
        let function = self.function.ok_or(WindowIdError::Unsupported)?;
        let mut id: u32 = 0;
        // SAFETY: `function` has the documented signature (see `from_lookup`);
        // `element` is a live AXUIElement and `id` a valid out pointer. The call
        // neither retains nor consumes either.
        let code = unsafe { function(element.as_ref(), &raw mut id) };
        if code != 0 {
            return Err(WindowIdError::Ax(AxError::from_code(code)));
        }
        if id == 0 {
            return Err(WindowIdError::NoWindow);
        }
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_symbol_is_an_explicit_unsupported_capability() {
        let resolver = unsafe { WindowIdResolver::from_lookup(|_| std::ptr::null_mut()) };
        assert_eq!(resolver.capability(), WindowIdCapability::Unsupported);
        let element = local_element();
        assert_eq!(resolver.window_id(&element), Err(WindowIdError::Unsupported));
        assert_eq!(
            WindowIdResolver::missing().capability(),
            WindowIdCapability::Unsupported
        );
    }

    #[test]
    fn the_lookup_receives_the_private_symbol_name() {
        let mut seen = None;
        // SAFETY: the lookup returns null.
        let _ = unsafe {
            WindowIdResolver::from_lookup(|name| {
                seen = Some(name.to_owned());
                std::ptr::null_mut()
            })
        };
        assert_eq!(seen.as_deref(), Some(c"_AXUIElementGetWindow"));
    }

    unsafe extern "C" fn fake_get_window(_: AxUiElementRef, out: *mut u32) -> RawAxError {
        // SAFETY: the test passes a valid out pointer.
        unsafe { *out = 4242 };
        0
    }

    unsafe extern "C" fn failing_get_window(_: AxUiElementRef, _: *mut u32) -> RawAxError { -25202 }

    unsafe extern "C" fn null_get_window(_: AxUiElementRef, out: *mut u32) -> RawAxError {
        // SAFETY: the test passes a valid out pointer.
        unsafe { *out = 0 };
        0
    }

    #[test]
    fn a_resolved_symbol_reports_ids_and_failures_explicitly() {
        let element = local_element();
        // SAFETY: `fake_get_window` has the documented signature.
        let ok = unsafe { WindowIdResolver::from_lookup(|_| fake_get_window as *mut c_void) };
        assert_eq!(ok.capability(), WindowIdCapability::Available);
        assert_eq!(ok.window_id(&element), Ok(4242));
        // SAFETY: `failing_get_window` has the documented signature.
        let failing =
            unsafe { WindowIdResolver::from_lookup(|_| failing_get_window as *mut c_void) };
        assert_eq!(
            failing.window_id(&element),
            Err(WindowIdError::Ax(AxError::InvalidElement))
        );
        // SAFETY: `null_get_window` has the documented signature.
        let zero = unsafe { WindowIdResolver::from_lookup(|_| null_get_window as *mut c_void) };
        assert_eq!(zero.window_id(&element), Err(WindowIdError::NoWindow));
    }

    /// `AXUIElementCreateApplication` is a local constructor: it sends no
    /// message, so creating an element for a PID is not an AX query.
    fn local_element() -> AxElement {
        use super::super::api::{AxApi, SystemAx};
        SystemAx::new().create_application(1).expect("local element")
    }
}
