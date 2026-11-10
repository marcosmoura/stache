//! The `CFString` attribute and notification names the window adapters use.
//!
//! The values are the literal `AXAttributeConstants.h` / `AXNotificationConstants.h`
//! string values (`kAXPositionAttribute` is `"AXPosition"`, ...). They are
//! created once per [`super::SystemAx`] on the owner thread.

use core_foundation::string::CFString;

use super::window::{BoolAttribute, WindowString};

/// Cached `CFString` names. `!Send` with its owner (`SystemAx`).
pub(super) struct Names {
    pub(super) position: CFString,
    pub(super) size: CFString,
    pub(super) windows: CFString,
    pub(super) focused_window: CFString,
    pub(super) main_window: CFString,
    pub(super) tabs: CFString,
    pub(super) value: CFString,
    pub(super) title: CFString,
    pub(super) role: CFString,
    pub(super) subrole: CFString,
    pub(super) minimized: CFString,
    pub(super) full_screen: CFString,
    pub(super) hidden: CFString,
    pub(super) enhanced_ui: CFString,
    pub(super) main: CFString,
    pub(super) raise_action: CFString,
}

impl Names {
    pub(super) fn new() -> Self {
        Self {
            position: CFString::from_static_string("AXPosition"),
            size: CFString::from_static_string("AXSize"),
            windows: CFString::from_static_string("AXWindows"),
            focused_window: CFString::from_static_string("AXFocusedWindow"),
            main_window: CFString::from_static_string("AXMainWindow"),
            tabs: CFString::from_static_string("AXTabs"),
            value: CFString::from_static_string("AXValue"),
            title: CFString::from_static_string("AXTitle"),
            role: CFString::from_static_string("AXRole"),
            subrole: CFString::from_static_string("AXSubrole"),
            minimized: CFString::from_static_string("AXMinimized"),
            full_screen: CFString::from_static_string("AXFullScreen"),
            hidden: CFString::from_static_string("AXHidden"),
            enhanced_ui: CFString::from_static_string("AXEnhancedUserInterface"),
            main: CFString::from_static_string("AXMain"),
            raise_action: CFString::from_static_string("AXRaise"),
        }
    }

    pub(super) const fn bool_name(&self, attribute: BoolAttribute) -> &CFString {
        match attribute {
            BoolAttribute::Minimized => &self.minimized,
            BoolAttribute::FullScreen => &self.full_screen,
            BoolAttribute::Hidden => &self.hidden,
            BoolAttribute::Main => &self.main,
        }
    }

    pub(super) const fn string_name(&self, attribute: WindowString) -> &CFString {
        match attribute {
            WindowString::Title => &self.title,
            WindowString::Role => &self.role,
            WindowString::Subrole => &self.subrole,
        }
    }
}

/// The notification names (string values of the `kAX*Notification` constants).
pub(super) mod notification {
    pub const WINDOW_CREATED: &str = "AXWindowCreated";
    pub const ELEMENT_DESTROYED: &str = "AXUIElementDestroyed";
    pub const FOCUSED_WINDOW_CHANGED: &str = "AXFocusedWindowChanged";
    pub const MAIN_WINDOW_CHANGED: &str = "AXMainWindowChanged";
    pub const WINDOW_MOVED: &str = "AXWindowMoved";
    pub const WINDOW_RESIZED: &str = "AXWindowResized";
    pub const WINDOW_MINIATURIZED: &str = "AXWindowMiniaturized";
    pub const WINDOW_DEMINIATURIZED: &str = "AXWindowDeminiaturized";
    pub const APPLICATION_HIDDEN: &str = "AXApplicationHidden";
    pub const APPLICATION_SHOWN: &str = "AXApplicationShown";
    pub const TITLE_CHANGED: &str = "AXTitleChanged";
}
