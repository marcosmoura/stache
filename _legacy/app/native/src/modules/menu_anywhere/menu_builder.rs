//! Menu Builder for `MenuAnywhere`.
//!
//! This module provides functionality to read the frontmost application's menu bar
//! using macOS Accessibility APIs and rebuild it as an `NSMenu`.

use std::cell::OnceCell;
use std::ffi::c_void;
use std::ptr;
use std::sync::OnceLock;

use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use objc::runtime::{BOOL, Class, NO, Object, Sel, YES};
use objc::{msg_send, sel, sel_impl};

// Accessibility API types and functions
type AXUIElementRef = *mut c_void;
type AXError = i32;

const K_AX_ERROR_SUCCESS: AXError = 0;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: *const c_void,
        value: *mut *mut c_void,
    ) -> AXError;
    fn AXUIElementGetTypeID() -> u64;
    fn AXUIElementPerformAction(element: AXUIElementRef, action: *const c_void) -> AXError;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFGetTypeID(cf: *const c_void) -> u64;
    fn CFArrayGetCount(array: *const c_void) -> i64;
    fn CFArrayGetValueAtIndex(array: *const c_void, idx: i64) -> *const c_void;
    fn CFRelease(cf: *const c_void);
    fn CFRetain(cf: *const c_void) -> *const c_void;
}

#[link(name = "objc", kind = "dylib")]
unsafe extern "C" {
    fn objc_setAssociatedObject(
        object: *mut Object,
        key: *const c_void,
        value: *mut Object,
        policy: usize,
    );
    fn objc_getAssociatedObject(object: *mut Object, key: *const c_void) -> *mut Object;
}

// Thread-local cached CFStrings for attribute names (avoids repeated allocations)
thread_local! {
    static CF_MENU_BAR: OnceCell<CFString> = const { OnceCell::new() };
    static CF_CHILDREN: OnceCell<CFString> = const { OnceCell::new() };
    static CF_TITLE: OnceCell<CFString> = const { OnceCell::new() };
    static CF_ROLE: OnceCell<CFString> = const { OnceCell::new() };
    static CF_ENABLED: OnceCell<CFString> = const { OnceCell::new() };
    static CF_MARK_CHAR: OnceCell<CFString> = const { OnceCell::new() };
    static CF_CMD_CHAR: OnceCell<CFString> = const { OnceCell::new() };
    static CF_CMD_MODS: OnceCell<CFString> = const { OnceCell::new() };
    static CF_PRESS: OnceCell<CFString> = const { OnceCell::new() };
}

/// Gets or creates a cached `CFString`.
macro_rules! cached_cfstring {
    ($cell:expr, $value:expr) => {
        $cell.with(|cell| cell.get_or_init(|| CFString::new($value)).as_concrete_TypeRef().cast())
    };
}

#[inline]
fn cf_menu_bar() -> *const c_void { cached_cfstring!(CF_MENU_BAR, "AXMenuBar") }

#[inline]
fn cf_children() -> *const c_void { cached_cfstring!(CF_CHILDREN, "AXChildren") }

#[inline]
fn cf_title() -> *const c_void { cached_cfstring!(CF_TITLE, "AXTitle") }

#[inline]
fn cf_role() -> *const c_void { cached_cfstring!(CF_ROLE, "AXRole") }

#[inline]
fn cf_enabled() -> *const c_void { cached_cfstring!(CF_ENABLED, "AXEnabled") }

#[inline]
fn cf_mark_char() -> *const c_void { cached_cfstring!(CF_MARK_CHAR, "AXMenuItemMarkChar") }

#[inline]
fn cf_cmd_char() -> *const c_void { cached_cfstring!(CF_CMD_CHAR, "AXMenuItemCmdChar") }

#[inline]
fn cf_cmd_mods() -> *const c_void { cached_cfstring!(CF_CMD_MODS, "AXMenuItemCmdModifiers") }

#[inline]
fn cf_press() -> *const c_void { cached_cfstring!(CF_PRESS, "AXPress") }

/// Gets a string attribute using cached `CFString`.
#[inline]
unsafe fn get_ax_string_attr(element: AXUIElementRef, attr: *const c_void) -> Option<String> {
    if element.is_null() {
        return None;
    }

    let mut value: *mut c_void = ptr::null_mut();
    let result = unsafe { AXUIElementCopyAttributeValue(element, attr, &raw mut value) };

    if result != K_AX_ERROR_SUCCESS || value.is_null() {
        return None;
    }

    let cf_string_type_id = CFString::type_id() as u64;
    if unsafe { CFGetTypeID(value) } != cf_string_type_id {
        unsafe { CFRelease(value) };
        return None;
    }

    let cf_string = unsafe { CFString::wrap_under_get_rule(value.cast()) };
    let string = cf_string.to_string();
    unsafe { CFRelease(value) };

    Some(string)
}

/// Gets a boolean attribute using cached `CFString`.
#[inline]
unsafe fn get_ax_bool_attr(element: AXUIElementRef, attr: *const c_void) -> Option<bool> {
    if element.is_null() {
        return None;
    }

    let mut value: *mut c_void = ptr::null_mut();
    let result = unsafe { AXUIElementCopyAttributeValue(element, attr, &raw mut value) };

    if result != K_AX_ERROR_SUCCESS || value.is_null() {
        return None;
    }

    let bool_value =
        unsafe { core_foundation::boolean::CFBoolean::wrap_under_get_rule(value.cast()) };
    let result = bool_value.into();
    unsafe { CFRelease(value) };

    Some(result)
}

/// Gets an integer attribute using cached `CFString`.
#[inline]
unsafe fn get_ax_int_attr(element: AXUIElementRef, attr: *const c_void) -> Option<i64> {
    if element.is_null() {
        return None;
    }

    let mut value: *mut c_void = ptr::null_mut();
    let result = unsafe { AXUIElementCopyAttributeValue(element, attr, &raw mut value) };

    if result != K_AX_ERROR_SUCCESS || value.is_null() {
        return None;
    }

    let number = unsafe { core_foundation::number::CFNumber::wrap_under_get_rule(value.cast()) };
    let int_value = number.to_i64();
    unsafe { CFRelease(value) };

    int_value
}

/// Gets children elements.
#[inline]
unsafe fn get_ax_children(element: AXUIElementRef) -> Option<Vec<AXUIElementRef>> {
    if element.is_null() {
        return None;
    }

    let mut value: *mut c_void = ptr::null_mut();
    let result = unsafe { AXUIElementCopyAttributeValue(element, cf_children(), &raw mut value) };

    if result != K_AX_ERROR_SUCCESS || value.is_null() {
        return None;
    }

    let count = unsafe { CFArrayGetCount(value) };
    if count <= 0 {
        unsafe { CFRelease(value) };
        return None;
    }

    let ax_type_id = unsafe { AXUIElementGetTypeID() };

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let mut children = Vec::with_capacity(count as usize);

    for i in 0..count {
        let child = unsafe { CFArrayGetValueAtIndex(value, i) };
        if !child.is_null() && unsafe { CFGetTypeID(child) } == ax_type_id {
            unsafe { CFRetain(child) };
            children.push(child.cast_mut());
        }
    }

    unsafe { CFRelease(value) };

    if children.is_empty() {
        None
    } else {
        Some(children)
    }
}

/// Gets the frontmost app PID.
#[inline]
fn get_frontmost_app_pid() -> Option<i32> {
    unsafe {
        let workspace_class = Class::get("NSWorkspace")?;
        let workspace: *mut Object = msg_send![workspace_class, sharedWorkspace];
        if workspace.is_null() {
            return None;
        }

        let frontmost_app: *mut Object = msg_send![workspace, frontmostApplication];
        if frontmost_app.is_null() {
            return None;
        }

        Some(msg_send![frontmost_app, processIdentifier])
    }
}

/// Builds an `NSMenu` from the frontmost application's menu bar.
#[must_use]
pub fn build_frontmost_app_menu() -> Option<*mut Object> {
    let pid = get_frontmost_app_pid()?;

    unsafe {
        let app_element = AXUIElementCreateApplication(pid);
        if app_element.is_null() {
            return None;
        }

        let mut menu_bar: *mut c_void = ptr::null_mut();
        let result = AXUIElementCopyAttributeValue(app_element, cf_menu_bar(), &raw mut menu_bar);

        if result != K_AX_ERROR_SUCCESS || menu_bar.is_null() {
            CFRelease(app_element.cast());
            return None;
        }

        if CFGetTypeID(menu_bar) != AXUIElementGetTypeID() {
            CFRelease(menu_bar);
            CFRelease(app_element.cast());
            return None;
        }

        let ns_menu = build_menu_from_ax_element(menu_bar as AXUIElementRef, false);

        CFRelease(menu_bar);
        CFRelease(app_element.cast());

        ns_menu
    }
}

/// Builds an `NSMenu` from an accessibility element.
unsafe fn build_menu_from_ax_element(
    element: AXUIElementRef,
    is_submenu: bool,
) -> Option<*mut Object> {
    let children = unsafe { get_ax_children(element)? };

    let menu_class = Class::get("NSMenu")?;
    let menu: *mut Object = unsafe { msg_send![menu_class, alloc] };
    let empty = unsafe { ns_string("") };
    let menu: *mut Object = unsafe { msg_send![menu, initWithTitle: empty] };

    // Performance optimizations
    let _: () = unsafe { msg_send![menu, setAutoenablesItems: NO] };
    let _: () = unsafe { msg_send![menu, setMinimumWidth: 0.0f64] };

    let mut is_first = true;
    let start_idx = usize::from(!is_submenu); // Skip Apple menu for root

    for child in children.iter().skip(start_idx) {
        if let Some(item) = unsafe { build_menu_item_from_ax_element(*child, is_submenu, is_first) }
        {
            let _: () = unsafe { msg_send![menu, addItem: item] };

            let is_separator: BOOL = unsafe { msg_send![item, isSeparatorItem] };
            if is_separator == NO {
                is_first = false;
            }
        }
    }

    Some(menu)
}

/// Builds an `NSMenuItem` from an accessibility element.
unsafe fn build_menu_item_from_ax_element(
    element: AXUIElementRef,
    is_submenu: bool,
    is_first: bool,
) -> Option<*mut Object> {
    let title = unsafe { get_ax_string_attr(element, cf_title()) }.unwrap_or_default();
    let role = unsafe { get_ax_string_attr(element, cf_role()) }.unwrap_or_default();

    // Handle separators
    if title.is_empty() || role == "AXSeparator" {
        let item_class = Class::get("NSMenuItem")?;
        return Some(unsafe { msg_send![item_class, separatorItem] });
    }

    let item_class = Class::get("NSMenuItem")?;
    let item: *mut Object = unsafe { msg_send![item_class, alloc] };

    let title_ns = unsafe { ns_string(&title) };
    let empty_str = unsafe { ns_string("") };

    let item: *mut Object = unsafe {
        msg_send![item, initWithTitle: title_ns action: ptr::null::<Sel>() keyEquivalent: empty_str]
    };

    // Store AX element reference
    unsafe { store_ax_element_for_item(item, element) };

    // Set enabled state
    let enabled = unsafe { get_ax_bool_attr(element, cf_enabled()) }.unwrap_or(true);
    let _: () = unsafe { msg_send![item, setEnabled: if enabled { YES } else { NO }] };

    // Set checkmark state
    if let Some(mark) = unsafe { get_ax_string_attr(element, cf_mark_char()) }
        && !mark.is_empty()
    {
        let state: i64 = match mark.as_str() {
            "✓" => 1,
            "•" => -1,
            _ => 0,
        };
        let _: () = unsafe { msg_send![item, setState: state] };
    }

    // Set keyboard shortcut
    if let Some(cmd) = unsafe { get_ax_string_attr(element, cf_cmd_char()) }
        && !cmd.is_empty()
    {
        let key_equiv = unsafe { ns_string(&cmd.to_lowercase()) };
        let _: () = unsafe { msg_send![item, setKeyEquivalent: key_equiv] };

        let mods = unsafe { get_ax_int_attr(element, cf_cmd_mods()) };
        let _: () =
            unsafe { msg_send![item, setKeyEquivalentModifierMask: ax_modifiers_to_ns(mods)] };
    }

    // Check for submenu
    if let Some(children) = unsafe { get_ax_children(element) } {
        for child in &children {
            if unsafe { get_ax_string_attr(*child, cf_role()) }.as_deref() == Some("AXMenu") {
                if let Some(submenu) = unsafe { build_menu_from_ax_element(*child, true) } {
                    let _: () = unsafe { msg_send![submenu, setTitle: title_ns] };
                    let _: () = unsafe { msg_send![item, setSubmenu: submenu] };
                }
                break;
            }
        }
    }

    // Set action for leaf items
    let has_submenu: *mut Object = unsafe { msg_send![item, submenu] };
    if has_submenu.is_null() && enabled {
        unsafe { set_menu_item_action(item) };
    }

    // Bold the first item (app name)
    if !is_submenu && is_first {
        unsafe { apply_bold_title(item, &title) };
    }

    Some(item)
}

/// Converts AX modifier flags to `NSEventModifierFlags`.
#[inline]
const fn ax_modifiers_to_ns(mods: Option<i64>) -> u64 {
    let Some(m) = mods else {
        return 1 << 20; // Command default
    };

    let mut flags: u64 = 0;
    if m & 1 != 0 {
        flags |= 1 << 17;
    } // Shift
    if m & 2 != 0 {
        flags |= 1 << 19;
    } // Option
    if m & 4 != 0 {
        flags |= 1 << 18;
    } // Control
    if m & 8 != 0 {
        flags |= 1 << 20;
    } // Command

    if flags == 0 { 1 << 20 } else { flags }
}

/// Creates an `NSString` from a Rust string.
#[inline]
unsafe fn ns_string(s: &str) -> *mut Object {
    use std::ffi::CString;

    let string_class = Class::get("NSString").expect("NSString class not found");

    if let Ok(c_string) = CString::new(s) {
        let ns: *mut Object =
            unsafe { msg_send![string_class, stringWithUTF8String: c_string.as_ptr()] };
        if !ns.is_null() {
            return ns;
        }
    }

    // Fallback for strings with null bytes or invalid UTF-8
    let ns: *mut Object = unsafe { msg_send![string_class, alloc] };
    unsafe { msg_send![ns, initWithBytes: s.as_ptr() length: s.len() encoding: 4u64] }
}

/// Applies bold font to menu item.
unsafe fn apply_bold_title(item: *mut Object, title: &str) {
    let Some(font_manager_class) = Class::get("NSFontManager") else {
        return;
    };
    let font_manager: *mut Object = unsafe { msg_send![font_manager_class, sharedFontManager] };
    if font_manager.is_null() {
        return;
    }

    let Some(font_class) = Class::get("NSFont") else {
        return;
    };
    let menu_font: *mut Object = unsafe { msg_send![font_class, menuFontOfSize: 0.0f64] };
    if menu_font.is_null() {
        return;
    }

    let bold_font: *mut Object =
        unsafe { msg_send![font_manager, convertFont: menu_font toHaveTrait: 2u64] };
    if bold_font.is_null() {
        return;
    }

    let Some(dict_class) = Class::get("NSDictionary") else {
        return;
    };
    let Some(attr_string_class) = Class::get("NSAttributedString") else {
        return;
    };

    unsafe {
        let font_key = ns_string("NSFont");
        let dict: *mut Object =
            msg_send![dict_class, dictionaryWithObject: bold_font forKey: font_key];

        let title_ns = ns_string(title);
        let attr_string: *mut Object = msg_send![attr_string_class, alloc];
        let attr_string: *mut Object =
            msg_send![attr_string, initWithString: title_ns attributes: dict];

        let _: () = msg_send![item, setAttributedTitle: attr_string];
    }
}

// ============================================================================
// Menu Action Handling
// ============================================================================

static AX_ELEMENT_KEY: u8 = 0;

unsafe fn store_ax_element_for_item(item: *mut Object, element: AXUIElementRef) {
    let Some(value_class) = Class::get("NSValue") else {
        return;
    };

    let value: *mut Object = unsafe { msg_send![value_class, valueWithPointer: element] };
    unsafe {
        objc_setAssociatedObject(item, ptr::addr_of!(AX_ELEMENT_KEY).cast(), value, 1);
    }
}

unsafe fn get_ax_element_for_item(item: *mut Object) -> Option<AXUIElementRef> {
    let value: *mut Object =
        unsafe { objc_getAssociatedObject(item, ptr::addr_of!(AX_ELEMENT_KEY).cast()) };

    if value.is_null() {
        return None;
    }

    let ptr: *mut c_void = unsafe { msg_send![value, pointerValue] };
    if ptr.is_null() { None } else { Some(ptr) }
}

struct ObjcPtr(*mut Object);
unsafe impl Send for ObjcPtr {}
unsafe impl Sync for ObjcPtr {}

static MENU_HANDLER: OnceLock<ObjcPtr> = OnceLock::new();

unsafe fn set_menu_item_action(item: *mut Object) {
    let handler_ptr = MENU_HANDLER.get_or_init(|| unsafe { ObjcPtr(get_or_create_menu_handler()) });
    if !handler_ptr.0.is_null() {
        let _: () = unsafe { msg_send![item, setTarget: handler_ptr.0] };
        let _: () = unsafe { msg_send![item, setAction: sel!(menuItemClicked:)] };
    }
}

#[allow(clippy::items_after_statements)]
unsafe fn get_or_create_menu_handler() -> *mut Object {
    use objc::declare::ClassDecl;

    let superclass = Class::get("NSObject").expect("NSObject not found");

    if let Some(existing) = Class::get("StacheMenuHandler") {
        return unsafe { msg_send![existing, new] };
    }

    let Some(mut decl) = ClassDecl::new("StacheMenuHandler", superclass) else {
        return ptr::null_mut();
    };

    extern "C" fn menu_item_clicked(_this: &Object, _sel: Sel, sender: *mut Object) {
        unsafe {
            if let Some(element) = get_ax_element_for_item(sender) {
                activate_frontmost_app();
                AXUIElementPerformAction(element, cf_press());
            }
        }
    }

    unsafe {
        decl.add_method(
            sel!(menuItemClicked:),
            menu_item_clicked as extern "C" fn(&Object, Sel, *mut Object),
        );
    }

    let handler_class = decl.register();
    unsafe { msg_send![handler_class, new] }
}

unsafe fn activate_frontmost_app() {
    let Some(workspace_class) = Class::get("NSWorkspace") else {
        return;
    };

    let workspace: *mut Object = unsafe { msg_send![workspace_class, sharedWorkspace] };
    if workspace.is_null() {
        return;
    }

    let frontmost_app: *mut Object = unsafe { msg_send![workspace, frontmostApplication] };
    if frontmost_app.is_null() {
        return;
    }

    let is_active: BOOL = unsafe { msg_send![frontmost_app, isActive] };
    if is_active == NO {
        let _: BOOL = unsafe { msg_send![frontmost_app, activateWithOptions: 2u64] };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // ax_modifiers_to_ns tests
    // ========================================================================

    #[test]
    fn test_ax_modifiers_default() {
        assert_eq!(ax_modifiers_to_ns(None), 1 << 20);
    }

    #[test]
    fn test_ax_modifiers_shift() {
        assert_eq!(ax_modifiers_to_ns(Some(1)), 1 << 17);
    }

    #[test]
    fn test_ax_modifiers_combined() {
        assert_eq!(ax_modifiers_to_ns(Some(9)), (1 << 17) | (1 << 20));
    }

    #[test]
    fn test_ax_modifiers_option() {
        // Option/Alt is bit 2 (0x02), maps to 1 << 19
        assert_eq!(ax_modifiers_to_ns(Some(2)), 1 << 19);
    }

    #[test]
    fn test_ax_modifiers_control() {
        // Control is bit 4 (0x04), maps to 1 << 18
        assert_eq!(ax_modifiers_to_ns(Some(4)), 1 << 18);
    }

    #[test]
    fn test_ax_modifiers_command() {
        // Command is bit 8 (0x08), maps to 1 << 20
        assert_eq!(ax_modifiers_to_ns(Some(8)), 1 << 20);
    }

    #[test]
    fn test_ax_modifiers_shift_option() {
        // Shift + Option = 1 + 2 = 3
        let expected = (1 << 17) | (1 << 19);
        assert_eq!(ax_modifiers_to_ns(Some(3)), expected);
    }

    #[test]
    fn test_ax_modifiers_shift_control() {
        // Shift + Control = 1 + 4 = 5
        let expected = (1 << 17) | (1 << 18);
        assert_eq!(ax_modifiers_to_ns(Some(5)), expected);
    }

    #[test]
    fn test_ax_modifiers_option_control() {
        // Option + Control = 2 + 4 = 6
        let expected = (1 << 19) | (1 << 18);
        assert_eq!(ax_modifiers_to_ns(Some(6)), expected);
    }

    #[test]
    fn test_ax_modifiers_all() {
        // All modifiers: Shift(1) + Option(2) + Control(4) + Command(8) = 15
        let expected = (1 << 17) | (1 << 19) | (1 << 18) | (1 << 20);
        assert_eq!(ax_modifiers_to_ns(Some(15)), expected);
    }

    #[test]
    fn test_ax_modifiers_zero_returns_command() {
        // Zero should default to Command
        assert_eq!(ax_modifiers_to_ns(Some(0)), 1 << 20);
    }

    #[test]
    fn test_ax_modifiers_shift_option_command() {
        // Shift + Option + Command = 1 + 2 + 8 = 11
        let expected = (1 << 17) | (1 << 19) | (1 << 20);
        assert_eq!(ax_modifiers_to_ns(Some(11)), expected);
    }

    #[test]
    fn test_ax_modifiers_control_command() {
        // Control + Command = 4 + 8 = 12
        let expected = (1 << 18) | (1 << 20);
        assert_eq!(ax_modifiers_to_ns(Some(12)), expected);
    }

    // ========================================================================
    // Constant tests
    // ========================================================================

    #[test]
    fn test_k_ax_error_success_is_zero() {
        assert_eq!(K_AX_ERROR_SUCCESS, 0);
    }

    // ========================================================================
    // CFString cache function tests (verifying they don't panic)
    // ========================================================================

    #[test]
    fn test_cf_menu_bar_returns_valid_pointer() {
        let ptr = cf_menu_bar();
        assert!(!ptr.is_null());
    }

    #[test]
    fn test_cf_children_returns_valid_pointer() {
        let ptr = cf_children();
        assert!(!ptr.is_null());
    }

    #[test]
    fn test_cf_title_returns_valid_pointer() {
        let ptr = cf_title();
        assert!(!ptr.is_null());
    }

    #[test]
    fn test_cf_role_returns_valid_pointer() {
        let ptr = cf_role();
        assert!(!ptr.is_null());
    }

    #[test]
    fn test_cf_enabled_returns_valid_pointer() {
        let ptr = cf_enabled();
        assert!(!ptr.is_null());
    }

    #[test]
    fn test_cf_mark_char_returns_valid_pointer() {
        let ptr = cf_mark_char();
        assert!(!ptr.is_null());
    }

    #[test]
    fn test_cf_cmd_char_returns_valid_pointer() {
        let ptr = cf_cmd_char();
        assert!(!ptr.is_null());
    }

    #[test]
    fn test_cf_cmd_mods_returns_valid_pointer() {
        let ptr = cf_cmd_mods();
        assert!(!ptr.is_null());
    }

    #[test]
    fn test_cf_press_returns_valid_pointer() {
        let ptr = cf_press();
        assert!(!ptr.is_null());
    }

    // ========================================================================
    // Cached CFStrings are consistent across calls
    // ========================================================================

    #[test]
    fn test_cf_menu_bar_is_consistent() {
        let ptr1 = cf_menu_bar();
        let ptr2 = cf_menu_bar();
        assert_eq!(ptr1, ptr2);
    }

    #[test]
    fn test_cf_children_is_consistent() {
        let ptr1 = cf_children();
        let ptr2 = cf_children();
        assert_eq!(ptr1, ptr2);
    }

    #[test]
    fn test_cf_title_is_consistent() {
        let ptr1 = cf_title();
        let ptr2 = cf_title();
        assert_eq!(ptr1, ptr2);
    }

    // ========================================================================
    // ns_string tests
    // ========================================================================

    #[test]
    fn test_ns_string_empty() {
        unsafe {
            let ns = ns_string("");
            assert!(!ns.is_null());
        }
    }

    #[test]
    fn test_ns_string_ascii() {
        unsafe {
            let ns = ns_string("hello");
            assert!(!ns.is_null());
        }
    }

    #[test]
    fn test_ns_string_unicode() {
        unsafe {
            let ns = ns_string("hello 世界");
            assert!(!ns.is_null());
        }
    }

    #[test]
    fn test_ns_string_with_special_chars() {
        unsafe {
            let ns = ns_string("File → Edit → View");
            assert!(!ns.is_null());
        }
    }

    #[test]
    fn test_ns_string_with_emoji() {
        unsafe {
            let ns = ns_string("Hello 🎉");
            assert!(!ns.is_null());
        }
    }

    // ========================================================================
    // get_ax_*_attr with null element tests
    // ========================================================================

    #[test]
    fn test_get_ax_string_attr_null_element() {
        unsafe {
            let result = get_ax_string_attr(std::ptr::null_mut(), cf_title());
            assert!(result.is_none());
        }
    }

    #[test]
    fn test_get_ax_bool_attr_null_element() {
        unsafe {
            let result = get_ax_bool_attr(std::ptr::null_mut(), cf_enabled());
            assert!(result.is_none());
        }
    }

    #[test]
    fn test_get_ax_int_attr_null_element() {
        unsafe {
            let result = get_ax_int_attr(std::ptr::null_mut(), cf_cmd_mods());
            assert!(result.is_none());
        }
    }

    #[test]
    fn test_get_ax_children_null_element() {
        unsafe {
            let result = get_ax_children(std::ptr::null_mut());
            assert!(result.is_none());
        }
    }
}
