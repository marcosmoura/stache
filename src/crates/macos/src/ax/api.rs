//! The AX element abstraction and the narrow raw `ApplicationServices` bindings.
//!
//! [`AxApi`] is the only surface the reader needs. [`SystemAx`] implements it
//! over raw `extern "C"` declarations; the fakes in `testing` implement it over
//! an arena. Both are used on the AX owner thread only: [`AxElement`] is an
//! owning RAII wrapper that is deliberately `!Send` and `!Sync`.
//!
//! # SDK / header evidence
//!
//! Declarations match the macOS SDK `HIServices.framework` headers:
//!
//! - `AXUIElement.h:113` `AXUIElementGetTypeID`; `:148`
//!   `AXUIElementCopyAttributeValue` returns the value *retained*
//!   (`CF_RETURNS_RETAINED`, the CF "Copy" rule: the caller owns one reference).
//! - `AXUIElement.h:356-362` `AXUIElementCreateApplication` follows the "Create"
//!   rule: the caller owns the returned element.
//! - `AXUIElement.h:387-402` `AXUIElementSetMessagingTimeout`: the timeout is set
//!   on that element object only, not on other (even equal) elements, so it is
//!   applied to every element object before it is messaged.
//! - `AXUIElement.h:315-331` `AXUIElementPerformAction` may return
//!   `kAXErrorCannotComplete` after the action already ran; the invocation
//!   path reports that as uncertain and never retries it automatically.
//! - `AXError.h` error codes; `AXAttributeConstants.h:995-999` modifier bits.
//!
//! Every copied value is type-checked with `CFGetTypeID` before it is
//! interpreted, and every owned reference is released on every path by RAII.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::time::Duration;

use core_foundation::array::CFArray;
use core_foundation::base::{CFGetTypeID, CFRelease, CFRetain, CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};

pub(super) type AxUiElementRef = *const c_void;
pub(super) type RawAxError = i32;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    pub(super) fn AXUIElementGetTypeID() -> usize;
    fn AXUIElementCreateApplication(pid: i32) -> AxUiElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AxUiElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> RawAxError;
    pub(super) fn AXUIElementGetAttributeValueCount(
        element: AxUiElementRef,
        attribute: CFStringRef,
        count: *mut isize,
    ) -> RawAxError;
    pub(super) fn AXUIElementCopyAttributeValues(
        element: AxUiElementRef,
        attribute: CFStringRef,
        index: isize,
        max_values: isize,
        values: *mut CFTypeRef,
    ) -> RawAxError;
    fn AXUIElementSetMessagingTimeout(element: AxUiElementRef, seconds: f32) -> RawAxError;
    pub(super) fn AXUIElementPerformAction(
        element: AxUiElementRef,
        action: CFStringRef,
    ) -> RawAxError;
}

/// An AX failure, classified from the raw `AXError` code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxError {
    /// `kAXErrorAPIDisabled`: Accessibility is not granted.
    ApiDisabled,
    /// `kAXErrorAttributeUnsupported`: the element lacks the attribute.
    AttributeUnsupported,
    /// `kAXErrorNoValue`: the attribute has no value.
    NoValue,
    /// `kAXErrorNotImplemented`: the process does not support the API.
    NotImplemented,
    /// `kAXErrorCannotComplete`: messaging failed or timed out.
    CannotComplete,
    /// `kAXErrorInvalidUIElement`: the element is gone or invalid.
    InvalidElement,
    /// `kAXErrorActionUnsupported`: the element does not support the action.
    ActionUnsupported,
    /// The copied CF value had an unexpected type.
    WrongType,
    /// Any other code (`kAXErrorFailure`, illegal argument, ...).
    Other(i32),
}

impl AxError {
    /// Classifies a raw non-success `AXError` code.
    #[must_use]
    pub const fn from_code(code: i32) -> Self {
        match code {
            -25211 => Self::ApiDisabled,
            -25205 => Self::AttributeUnsupported,
            -25212 => Self::NoValue,
            -25208 => Self::NotImplemented,
            -25204 => Self::CannotComplete,
            -25202 => Self::InvalidElement,
            -25206 => Self::ActionUnsupported,
            other => Self::Other(other),
        }
    }

    /// Whether the error means the attribute is simply absent.
    #[must_use]
    pub const fn is_absent(self) -> bool {
        matches!(
            self,
            Self::AttributeUnsupported | Self::NoValue | Self::NotImplemented
        )
    }
}

/// The string attributes the reader reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringAttribute {
    /// `AXRole`.
    Role,
    /// `AXTitle`.
    Title,
    /// `AXMenuItemMarkChar`.
    MarkChar,
    /// `AXMenuItemCmdChar`.
    CmdChar,
}

/// The element operations the reader needs, on the owner thread only.
///
/// Implementations create/copy/release elements with CF ownership rules; the
/// `Element` type releases its reference on drop. It is not required to be
/// `Send`: it never leaves the owner thread.
pub trait AxApi {
    /// An owned AX element.
    type Element;

    /// Creates the application root for `pid` (never messages it).
    ///
    /// # Errors
    ///
    /// Returns an error when no element can be created.
    fn create_application(&self, pid: i32) -> Result<Self::Element, AxError>;

    /// Sets the messaging timeout on exactly this element object.
    ///
    /// # Errors
    ///
    /// Returns the AX failure.
    fn set_messaging_timeout(
        &self,
        element: &Self::Element,
        timeout: Duration,
    ) -> Result<(), AxError>;

    /// Copies the `AXMenuBar` element.
    ///
    /// # Errors
    ///
    /// Returns the AX failure or [`AxError::WrongType`].
    fn menu_bar(&self, application: &Self::Element) -> Result<Self::Element, AxError>;

    /// Copies a string attribute.
    ///
    /// # Errors
    ///
    /// Returns the AX failure or [`AxError::WrongType`].
    fn string(
        &self,
        element: &Self::Element,
        attribute: StringAttribute,
    ) -> Result<String, AxError>;

    /// Copies `AXEnabled`.
    ///
    /// # Errors
    ///
    /// Returns the AX failure or [`AxError::WrongType`].
    fn enabled(&self, element: &Self::Element) -> Result<bool, AxError>;

    /// Copies `AXMenuItemCmdModifiers`.
    ///
    /// # Errors
    ///
    /// Returns the AX failure or [`AxError::WrongType`].
    fn cmd_modifiers(&self, element: &Self::Element) -> Result<i64, AxError>;

    /// The `AXChildren` count, without copying the array.
    ///
    /// # Errors
    ///
    /// Returns the AX failure.
    fn child_count(&self, element: &Self::Element) -> Result<usize, AxError>;

    /// Copies at most `limit` `AXChildren`, each type-checked.
    ///
    /// # Errors
    ///
    /// Returns the AX failure or [`AxError::WrongType`].
    fn children(
        &self,
        element: &Self::Element,
        limit: usize,
    ) -> Result<Vec<Self::Element>, AxError>;

    /// Performs `AXPress` on exactly this element object. The caller sets the
    /// press messaging timeout on the same element object first.
    ///
    /// # Errors
    ///
    /// Returns the AX failure. [`AxError::CannotComplete`] may follow an action
    /// that already ran and must never be retried automatically.
    fn press(&self, element: &Self::Element) -> Result<(), AxError>;

    /// Whether two elements are the same accessibility object (`CFEqual`).
    /// Local; never messages the target.
    fn same_element(&self, a: &Self::Element, b: &Self::Element) -> bool;
}

/// An owned `AXUIElementRef`, released on drop. `!Send` and `!Sync`.
pub struct AxElement {
    raw: NonNull<c_void>,
    _owner_local: PhantomData<*const ()>,
}

impl AxElement {
    /// Takes ownership of a +1 reference already known to be an `AXUIElement`.
    ///
    /// # Safety
    ///
    /// `raw` must be a valid `AXUIElementRef` whose retain the caller owns.
    pub(super) const unsafe fn from_owned(raw: NonNull<c_void>) -> Self {
        Self { raw, _owner_local: PhantomData }
    }

    pub(super) const fn as_ref(&self) -> AxUiElementRef { self.raw.as_ptr().cast_const() }

    /// Another owning handle to the same accessibility object (one `CFRetain`).
    pub(super) fn retained(&self) -> Self {
        // SAFETY: `raw` is a live reference this wrapper owns; CFRetain adds the
        // reference that the new wrapper owns independently of this one.
        unsafe {
            CFRetain(self.raw.as_ptr().cast_const());
            Self::from_owned(self.raw)
        }
    }
}

impl Drop for AxElement {
    fn drop(&mut self) {
        // SAFETY: `raw` holds exactly one reference this wrapper owns (create
        // or copy rule, or an explicit CFRetain); it is released once here.
        unsafe { CFRelease(self.raw.as_ptr().cast_const()) };
    }
}

/// The real AX implementation. Created on the owner thread; `!Send`.
pub struct SystemAx {
    pub(super) ax_type: usize,
    pub(super) names: super::names::Names,
    pub(super) window_id: super::window_id::WindowIdResolver,
    menu_bar: CFString,
    children: CFString,
    enabled: CFString,
    cmd_modifiers: CFString,
    role: CFString,
    title: CFString,
    mark_char: CFString,
    cmd_char: CFString,
    press_action: CFString,
    _owner_local: PhantomData<*const ()>,
}

impl Default for SystemAx {
    fn default() -> Self { Self::new() }
}

impl SystemAx {
    /// Creates the binding. Touches no application and sends no message.
    #[must_use]
    pub fn new() -> Self {
        Self {
            // SAFETY: no arguments; returns the process-constant type id.
            ax_type: unsafe { AXUIElementGetTypeID() },
            names: super::names::Names::new(),
            window_id: super::window_id::WindowIdResolver::system(),
            menu_bar: CFString::from_static_string("AXMenuBar"),
            children: CFString::from_static_string("AXChildren"),
            enabled: CFString::from_static_string("AXEnabled"),
            cmd_modifiers: CFString::from_static_string("AXMenuItemCmdModifiers"),
            role: CFString::from_static_string("AXRole"),
            title: CFString::from_static_string("AXTitle"),
            mark_char: CFString::from_static_string("AXMenuItemMarkChar"),
            cmd_char: CFString::from_static_string("AXMenuItemCmdChar"),
            press_action: CFString::from_static_string("AXPress"),
            _owner_local: PhantomData,
        }
    }

    /// Copies an attribute as an owned, untyped CF value.
    pub(super) fn copy_value(element: &AxElement, attribute: &CFString) -> Result<CFType, AxError> {
        let mut value: CFTypeRef = std::ptr::null();
        // SAFETY: `element` is a live AXUIElement, `attribute` a live CFString
        // and `value` a valid out pointer. On success the callee returns a +1
        // reference (AXUIElement.h:148) that is wrapped below exactly once.
        let code = unsafe {
            AXUIElementCopyAttributeValue(
                element.as_ref(),
                attribute.as_concrete_TypeRef(),
                &raw mut value,
            )
        };
        if code != 0 {
            return Err(AxError::from_code(code));
        }
        if value.is_null() {
            return Err(AxError::NoValue);
        }
        // SAFETY: non-null +1 reference from a Copy call; wrapper takes it.
        Ok(unsafe { CFType::wrap_under_create_rule(value) })
    }

    /// Converts a borrowed CF object into an owned element after a type-id
    /// check; the new element holds its own retain.
    pub(super) fn element_from(&self, object: CFTypeRef) -> Result<AxElement, AxError> {
        let raw = NonNull::new(object.cast_mut()).ok_or(AxError::NoValue)?;
        // SAFETY: `object` is a live CF object borrowed from a value the caller
        // still owns.
        if unsafe { CFGetTypeID(object) } != self.ax_type {
            return Err(AxError::WrongType);
        }
        // SAFETY: the object is an AXUIElement (checked); CFRetain adds the
        // reference that the new wrapper owns, independent of the borrow.
        unsafe {
            CFRetain(object);
            Ok(AxElement::from_owned(raw))
        }
    }

    const fn attribute(&self, which: StringAttribute) -> &CFString {
        match which {
            StringAttribute::Role => &self.role,
            StringAttribute::Title => &self.title,
            StringAttribute::MarkChar => &self.mark_char,
            StringAttribute::CmdChar => &self.cmd_char,
        }
    }
}

impl AxApi for SystemAx {
    type Element = AxElement;

    fn create_application(&self, pid: i32) -> Result<AxElement, AxError> {
        // SAFETY: returns a +1 reference (Create rule, AXUIElement.h:356-362) or
        // null; null is rejected, non-null is owned by the wrapper.
        let raw = unsafe { AXUIElementCreateApplication(pid) };
        NonNull::new(raw.cast_mut())
            // SAFETY: non-null Create-rule AXUIElement, owned.
            .map(|raw| unsafe { AxElement::from_owned(raw) })
            .ok_or(AxError::InvalidElement)
    }

    fn set_messaging_timeout(&self, element: &AxElement, timeout: Duration) -> Result<(), AxError> {
        // SAFETY: live element; the timeout is a positive finite f32.
        let code =
            unsafe { AXUIElementSetMessagingTimeout(element.as_ref(), timeout.as_secs_f32()) };
        if code == 0 {
            Ok(())
        } else {
            Err(AxError::from_code(code))
        }
    }

    fn menu_bar(&self, application: &AxElement) -> Result<AxElement, AxError> {
        let value = Self::copy_value(application, &self.menu_bar)?;
        self.element_from(value.as_CFTypeRef())
    }

    fn string(&self, element: &AxElement, attribute: StringAttribute) -> Result<String, AxError> {
        let value = Self::copy_value(element, self.attribute(attribute))?;
        value
            .downcast_into::<CFString>()
            .map(|s| s.to_string())
            .ok_or(AxError::WrongType)
    }

    fn enabled(&self, element: &AxElement) -> Result<bool, AxError> {
        let value = Self::copy_value(element, &self.enabled)?;
        value.downcast_into::<CFBoolean>().map(bool::from).ok_or(AxError::WrongType)
    }

    fn cmd_modifiers(&self, element: &AxElement) -> Result<i64, AxError> {
        let value = Self::copy_value(element, &self.cmd_modifiers)?;
        value
            .downcast_into::<CFNumber>()
            .ok_or(AxError::WrongType)?
            .to_i64()
            .ok_or(AxError::WrongType)
    }

    fn child_count(&self, element: &AxElement) -> Result<usize, AxError> {
        let mut count: isize = 0;
        // SAFETY: live element and attribute; valid out pointer; no ownership.
        let code = unsafe {
            AXUIElementGetAttributeValueCount(
                element.as_ref(),
                self.children.as_concrete_TypeRef(),
                &raw mut count,
            )
        };
        if code != 0 {
            return Err(AxError::from_code(code));
        }
        usize::try_from(count).map_err(|_| AxError::WrongType)
    }

    fn children(&self, element: &AxElement, limit: usize) -> Result<Vec<AxElement>, AxError> {
        let max = isize::try_from(limit).map_err(|_| AxError::WrongType)?;
        let mut array: CFTypeRef = std::ptr::null();
        // SAFETY: live element and attribute; valid out pointer. On success the
        // callee returns a +1 CFArray (Copy rule) wrapped below exactly once.
        let code = unsafe {
            AXUIElementCopyAttributeValues(
                element.as_ref(),
                self.children.as_concrete_TypeRef(),
                0,
                max,
                &raw mut array,
            )
        };
        if code != 0 {
            return Err(AxError::from_code(code));
        }
        if array.is_null() {
            return Err(AxError::NoValue);
        }
        // SAFETY: non-null +1 reference owned by the wrapper.
        let array = unsafe { CFType::wrap_under_create_rule(array) };
        let array = array.downcast_into::<CFArray>().ok_or(AxError::WrongType)?;
        let mut out = Vec::with_capacity(usize::try_from(array.len()).unwrap_or(0));
        for index in 0..array.len() {
            let item = array.get(index).ok_or(AxError::NoValue)?;
            out.push(self.element_from(*item)?);
        }
        Ok(out)
    }

    fn press(&self, element: &AxElement) -> Result<(), AxError> {
        // SAFETY: `element` is a live AXUIElement and `press_action` a live
        // CFString (`kAXPressAction`); the call neither retains nor consumes
        // either. The owner thread is the only caller and has set the press
        // timeout on this element object (AXUIElement.h:315-331, 387-396).
        let code = unsafe {
            AXUIElementPerformAction(element.as_ref(), self.press_action.as_concrete_TypeRef())
        };
        if code == 0 {
            Ok(())
        } else {
            Err(AxError::from_code(code))
        }
    }

    fn same_element(&self, a: &AxElement, b: &AxElement) -> bool {
        // SAFETY: both are live CF objects; CFEqual only reads them.
        unsafe { core_foundation::base::CFEqual(a.as_ref(), b.as_ref()) != 0 }
    }
}
