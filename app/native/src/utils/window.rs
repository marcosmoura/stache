use std::ffi::c_void;
use std::ptr;
use std::sync::OnceLock;

use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};
use tauri::{LogicalPosition, LogicalSize, Position, Size, WebviewWindow};

// Cache created Space so we only create/show once.
static G_SPACE: OnceLock<u64> = OnceLock::new();
static NON_ACTIVATING_PANEL_CLASS: OnceLock<&'static Class> = OnceLock::new();

type ObjcId = *mut objc::runtime::Object;

const NS_WINDOW_STYLE_MASK_NONACTIVATING_PANEL: u64 = 1 << 7;
const NS_WINDOW_COLLECTION_BEHAVIOR_CAN_JOIN_ALL_SPACES: u64 = 1 << 0;
const NS_WINDOW_COLLECTION_BEHAVIOR_STATIONARY: u64 = 1 << 4;
const NSTRACKING_MOUSE_ENTERED_AND_EXITED: u64 = 0x1;
const NSTRACKING_MOUSE_MOVED: u64 = 0x2;
const NSTRACKING_ACTIVE_ALWAYS: u64 = 0x80;
const NSTRACKING_IN_VISIBLE_RECT: u64 = 0x200;
const NS_EVENT_TYPE_LEFT_MOUSE_DOWN: u64 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct NSPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NSSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NSRect {
    origin: NSPoint,
    size: NSSize,
}

const fn zero_rect() -> NSRect {
    NSRect {
        origin: NSPoint { x: 0.0, y: 0.0 },
        size: NSSize { width: 0.0, height: 0.0 },
    }
}

fn non_activating_panel_class() -> &'static Class {
    NON_ACTIVATING_PANEL_CLASS.get_or_init(|| unsafe {
        let superclass = Class::get("TaoWindow").expect("TaoWindow class missing");
        if let Some(existing) = Class::get("BarbaNonActivatingTaoWindow") {
            return existing;
        }
        let mut decl = ClassDecl::new("BarbaNonActivatingTaoWindow", superclass)
            .expect("failed to declare BarbaNonActivatingTaoWindow");
        decl.add_method(
            sel!(acceptsFirstMouse:),
            accepts_first_mouse as extern "C" fn(&Object, Sel, ObjcId) -> bool,
        );
        decl.add_method(
            sel!(sendEvent:),
            forward_send_event as extern "C" fn(&Object, Sel, ObjcId),
        );
        decl.register()
    })
}

#[link(name = "SkyLight", kind = "framework")]
unsafe extern "C" {
    fn SLSMainConnectionID() -> i32;
    fn SLSSpaceCreate(connection: u32, space_type: u64, flags: u64) -> u64;
    fn SLSSpaceSetAbsoluteLevel(connection: u32, space: u64, level: i32);
    fn SLSShowSpaces(connection: u32, spaces: ObjcId);
    fn SLSSpaceAddWindowsAndRemoveFromSpaces(
        connection: u32,
        space: u64,
        windows: ObjcId,
        flags: u64,
    );
}

unsafe extern "C" {
    fn object_setClass(obj: ObjcId, cls: *const Class) -> *const Class;
    fn objc_setAssociatedObject(object: ObjcId, key: *const c_void, value: ObjcId, policy: usize);
    fn objc_getAssociatedObject(object: ObjcId, key: *const c_void) -> ObjcId;
}

const extern "C" fn accepts_first_mouse(_: &Object, _: Sel, _: ObjcId) -> bool { true }

extern "C" fn forward_send_event(this: &Object, _: Sel, event: ObjcId) {
    unsafe {
        let event_type: u64 = msg_send![event, type];
        if event_type == NS_EVENT_TYPE_LEFT_MOUSE_DOWN {
            let is_movable: bool = msg_send![this, isMovableByWindowBackground];
            if is_movable {
                let _: () = msg_send![this, performWindowDragWithEvent: event];
            }
        }

        let _: () = msg_send![super(this, class!(NSWindow)), sendEvent: event];
    }
}

const OBJC_ASSOCIATION_RETAIN_NONATOMIC: usize = 0x301;
static TRACKING_AREA_ASSOC_KEY: u8 = 0;

pub fn set_position(window: &WebviewWindow, x: f64, y: f64, width: f64, height: f64) {
    let _ = window.set_size(Size::Logical(LogicalSize { width, height }));
    let _ = window.set_position(Position::Logical(LogicalPosition { x, y }));
}

pub fn set_window_sticky(window: &WebviewWindow) {
    let _ = window.set_resizable(false);
    let _ = window.set_focusable(true);

    if let Ok(ns_win_ptr) = window.ns_window() {
        unsafe {
            let connection = SLSMainConnectionID();
            if connection != 0 {
                #[allow(clippy::cast_sign_loss)]
                let connection = connection as u32;
                let ns_win: ObjcId = ns_win_ptr as ObjcId;
                enforce_non_activating_click_behavior(ns_win);
                let window_number: usize = msg_send![ns_win, windowNumber];
                let space_id = *G_SPACE.get_or_init(|| {
                    let space = SLSSpaceCreate(connection, 1, 0);
                    SLSSpaceSetAbsoluteLevel(connection, space, 0);
                    let ns_space_num: ObjcId =
                        msg_send![class!(NSNumber), numberWithUnsignedLongLong: space];
                    let space_list: ObjcId =
                        msg_send![class!(NSArray), arrayWithObject: ns_space_num];
                    SLSShowSpaces(connection, space_list);
                    space
                });
                let ns_win_id: ObjcId =
                    msg_send![class!(NSNumber), numberWithUnsignedInteger: window_number];
                let window_list: ObjcId = msg_send![class!(NSArray), arrayWithObject: ns_win_id];
                SLSSpaceAddWindowsAndRemoveFromSpaces(connection, space_id, window_list, 0x7);
            }
        }
    }
}

pub fn set_window_level(window: &WebviewWindow, level: i32) {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGWindowLevelForKey(key: i32) -> i32;
    }

    if let Ok(ns_win_ptr) = window.ns_window() {
        unsafe {
            let ns_win: ObjcId = ns_win_ptr as ObjcId;
            let menu_level = CGWindowLevelForKey(level);
            let _: () = msg_send![ns_win, setLevel: menu_level];
        }
    }
}

pub fn set_window_below_menu(window: &WebviewWindow) {
    // Menu level
    set_window_level(window, 8);
    let _ = window.set_always_on_bottom(true);
}

pub fn set_window_always_on_top(window: &WebviewWindow) {
    set_window_level(window, 20);
    let _ = window.set_always_on_top(true);
}

pub fn get_screen_size(window: &WebviewWindow) -> Result<(f64, f64), Box<dyn std::error::Error>> {
    let monitor = window.primary_monitor()?.ok_or("No primary monitor found")?;

    let scale = monitor.scale_factor();
    let physical = monitor.size();
    let logical_width = f64::from(physical.width) / scale;
    let logical_height = f64::from(physical.height) / scale;

    Ok((logical_width, logical_height))
}

fn enforce_non_activating_click_behavior(ns_win: ObjcId) {
    unsafe {
        let panel_class = non_activating_panel_class();
        let _ = object_setClass(ns_win, panel_class);

        let style_mask: u64 = msg_send![ns_win, styleMask];
        if style_mask & NS_WINDOW_STYLE_MASK_NONACTIVATING_PANEL == 0 {
            let updated_mask = style_mask | NS_WINDOW_STYLE_MASK_NONACTIVATING_PANEL;
            let _: () = msg_send![ns_win, setStyleMask: updated_mask];
        }

        // Avoid FULL_SCREEN_AUXILIARY so fullscreen spaces stay unaffected by the bar.
        let behaviors = NS_WINDOW_COLLECTION_BEHAVIOR_CAN_JOIN_ALL_SPACES
            | NS_WINDOW_COLLECTION_BEHAVIOR_STATIONARY;

        let _: () = msg_send![ns_win, setCollectionBehavior: behaviors];
        if msg_send![ns_win, respondsToSelector: sel!(setFloatingPanel:)] {
            let _: () = msg_send![ns_win, setFloatingPanel: true];
        }
        if msg_send![ns_win, respondsToSelector: sel!(setHidesOnDeactivate:)] {
            let _: () = msg_send![ns_win, setHidesOnDeactivate: false];
        }
        if msg_send![ns_win, respondsToSelector: sel!(setWorksWhenModal:)] {
            let _: () = msg_send![ns_win, setWorksWhenModal: true];
        }
        if msg_send![ns_win, respondsToSelector: sel!(setBecomesKeyOnlyIfNeeded:)] {
            let _: () = msg_send![ns_win, setBecomesKeyOnlyIfNeeded: true];
        }
        let _: () = msg_send![ns_win, setAcceptsMouseMovedEvents: true];
        let _: () = msg_send![ns_win, setIgnoresMouseEvents: false];
        ensure_tracking_area(ns_win);
    }
}

fn ensure_tracking_area(ns_win: ObjcId) {
    unsafe {
        let content_view: ObjcId = msg_send![ns_win, contentView];
        if content_view.is_null() {
            return;
        }

        let key_ptr = (&raw const TRACKING_AREA_ASSOC_KEY).cast::<c_void>();
        let existing: ObjcId = objc_getAssociatedObject(content_view, key_ptr);
        if !existing.is_null() {
            return;
        }

        let options = NSTRACKING_MOUSE_ENTERED_AND_EXITED
            | NSTRACKING_MOUSE_MOVED
            | NSTRACKING_ACTIVE_ALWAYS
            | NSTRACKING_IN_VISIBLE_RECT;

        let tracking_area: ObjcId = msg_send![class!(NSTrackingArea), alloc];
        let tracking_area: ObjcId = msg_send![tracking_area,
            initWithRect: zero_rect()
            options: options
            owner: content_view
            userInfo: ptr::null_mut::<c_void>()
        ];
        let _: () = msg_send![content_view, addTrackingArea: tracking_area];
        objc_setAssociatedObject(
            content_view,
            key_ptr,
            tracking_area,
            OBJC_ASSOCIATION_RETAIN_NONATOMIC,
        );
        let _: () = msg_send![tracking_area, release];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_rect_has_all_zero_values() {
        let rect = zero_rect();

        assert!((rect.origin.x).abs() < f64::EPSILON);
        assert!((rect.origin.y).abs() < f64::EPSILON);
        assert!((rect.size.width).abs() < f64::EPSILON);
        assert!((rect.size.height).abs() < f64::EPSILON);
    }

    #[test]
    fn ns_point_can_be_created() {
        let point = NSPoint { x: 10.0, y: 20.0 };
        assert!((point.x - 10.0).abs() < f64::EPSILON);
        assert!((point.y - 20.0).abs() < f64::EPSILON);
    }

    #[test]
    fn ns_size_can_be_created() {
        let size = NSSize { width: 100.0, height: 50.0 };
        assert!((size.width - 100.0).abs() < f64::EPSILON);
        assert!((size.height - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn ns_rect_can_be_created() {
        let rect = NSRect {
            origin: NSPoint { x: 5.0, y: 10.0 },
            size: NSSize { width: 200.0, height: 100.0 },
        };
        assert!((rect.origin.x - 5.0).abs() < f64::EPSILON);
        assert!((rect.origin.y - 10.0).abs() < f64::EPSILON);
        assert!((rect.size.width - 200.0).abs() < f64::EPSILON);
        assert!((rect.size.height - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn ns_rect_can_be_copied() {
        let rect1 = NSRect {
            origin: NSPoint { x: 1.0, y: 2.0 },
            size: NSSize { width: 3.0, height: 4.0 },
        };
        let rect2 = rect1;
        assert!((rect2.origin.x - 1.0).abs() < f64::EPSILON);
        assert!((rect2.origin.y - 2.0).abs() < f64::EPSILON);
        assert!((rect2.size.width - 3.0).abs() < f64::EPSILON);
        assert!((rect2.size.height - 4.0).abs() < f64::EPSILON);
    }

    #[test]
    fn window_style_mask_constants_are_valid() {
        assert_eq!(NS_WINDOW_STYLE_MASK_NONACTIVATING_PANEL, 1 << 7);
        assert_eq!(NS_WINDOW_COLLECTION_BEHAVIOR_CAN_JOIN_ALL_SPACES, 1 << 0);
        assert_eq!(NS_WINDOW_COLLECTION_BEHAVIOR_STATIONARY, 1 << 4);
    }

    #[test]
    fn tracking_area_constants_are_valid() {
        assert_eq!(NSTRACKING_MOUSE_ENTERED_AND_EXITED, 0x1);
        assert_eq!(NSTRACKING_MOUSE_MOVED, 0x2);
        assert_eq!(NSTRACKING_ACTIVE_ALWAYS, 0x80);
        assert_eq!(NSTRACKING_IN_VISIBLE_RECT, 0x200);
    }

    #[test]
    fn event_type_constant_is_valid() {
        assert_eq!(NS_EVENT_TYPE_LEFT_MOUSE_DOWN, 1);
    }

    #[test]
    fn objc_association_constant_is_valid() {
        assert_eq!(OBJC_ASSOCIATION_RETAIN_NONATOMIC, 0x301);
    }
}
