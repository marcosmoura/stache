//! Event types for the tiling v2 event pipeline.
//!
//! These types represent raw events from macOS before they are transformed
//! into `StateMessage`s for the state actor.

// ============================================================================
// Notification Constants
// ============================================================================

/// Notification names for accessibility events.
pub mod notifications {
    /// Window was created.
    pub const WINDOW_CREATED: &str = "AXWindowCreated";
    /// UI element was destroyed (window closed).
    pub const UI_ELEMENT_DESTROYED: &str = "AXUIElementDestroyed";
    /// Focused UI element changed.
    pub const FOCUSED_UI_ELEMENT_CHANGED: &str = "AXFocusedUIElementChanged";
    /// Focused window changed.
    pub const FOCUSED_WINDOW_CHANGED: &str = "AXFocusedWindowChanged";
    /// Window was moved.
    pub const WINDOW_MOVED: &str = "AXWindowMoved";
    /// Window was resized.
    pub const WINDOW_RESIZED: &str = "AXWindowResized";
    /// Window was minimized.
    pub const WINDOW_MINIMIZED: &str = "AXWindowMiniaturized";
    /// Window was unminimized.
    pub const WINDOW_UNMINIMIZED: &str = "AXWindowDeminiaturized";
    /// Window title changed.
    pub const TITLE_CHANGED: &str = "AXTitleChanged";
    /// Application was activated.
    pub const APPLICATION_ACTIVATED: &str = "AXApplicationActivated";
    /// Application was deactivated.
    pub const APPLICATION_DEACTIVATED: &str = "AXApplicationDeactivated";
    /// Application was hidden.
    pub const APPLICATION_HIDDEN: &str = "AXApplicationHidden";
    /// Application was shown.
    pub const APPLICATION_SHOWN: &str = "AXApplicationShown";
}

// ============================================================================
// Event Types
// ============================================================================

/// Types of window events that can be observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowEventType {
    /// A new window was created.
    Created,
    /// A window was closed/destroyed.
    Destroyed,
    /// A window gained focus.
    Focused,
    /// A window lost focus.
    Unfocused,
    /// A window was moved to a new position.
    Moved,
    /// A window was resized.
    Resized,
    /// A window was minimized.
    Minimized,
    /// A window was restored from minimized state.
    Unminimized,
    /// A window's title changed.
    TitleChanged,
    /// An application was activated (brought to front).
    AppActivated,
    /// An application was deactivated (moved to background).
    AppDeactivated,
    /// An application was hidden.
    AppHidden,
    /// An application was shown.
    AppShown,
}

impl WindowEventType {
    /// Returns the accessibility notification name for this event type.
    #[must_use]
    pub const fn notification_name(self) -> &'static str {
        match self {
            Self::Created => notifications::WINDOW_CREATED,
            Self::Destroyed => notifications::UI_ELEMENT_DESTROYED,
            Self::Focused => notifications::FOCUSED_WINDOW_CHANGED,
            Self::Unfocused => notifications::FOCUSED_UI_ELEMENT_CHANGED,
            Self::Moved => notifications::WINDOW_MOVED,
            Self::Resized => notifications::WINDOW_RESIZED,
            Self::Minimized => notifications::WINDOW_MINIMIZED,
            Self::Unminimized => notifications::WINDOW_UNMINIMIZED,
            Self::TitleChanged => notifications::TITLE_CHANGED,
            Self::AppActivated => notifications::APPLICATION_ACTIVATED,
            Self::AppDeactivated => notifications::APPLICATION_DEACTIVATED,
            Self::AppHidden => notifications::APPLICATION_HIDDEN,
            Self::AppShown => notifications::APPLICATION_SHOWN,
        }
    }

    /// Parses a notification name string into an event type.
    #[must_use]
    pub fn from_notification(name: &str) -> Option<Self> {
        match name {
            notifications::WINDOW_CREATED => Some(Self::Created),
            notifications::UI_ELEMENT_DESTROYED => Some(Self::Destroyed),
            notifications::FOCUSED_WINDOW_CHANGED => Some(Self::Focused),
            notifications::FOCUSED_UI_ELEMENT_CHANGED => Some(Self::Unfocused),
            notifications::WINDOW_MOVED => Some(Self::Moved),
            notifications::WINDOW_RESIZED => Some(Self::Resized),
            notifications::WINDOW_MINIMIZED => Some(Self::Minimized),
            notifications::WINDOW_UNMINIMIZED => Some(Self::Unminimized),
            notifications::TITLE_CHANGED => Some(Self::TitleChanged),
            notifications::APPLICATION_ACTIVATED => Some(Self::AppActivated),
            notifications::APPLICATION_DEACTIVATED => Some(Self::AppDeactivated),
            notifications::APPLICATION_HIDDEN => Some(Self::AppHidden),
            notifications::APPLICATION_SHOWN => Some(Self::AppShown),
            _ => None,
        }
    }

    /// Returns all event types that should be observed.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Created,
            Self::Destroyed,
            Self::Focused,
            Self::Moved,
            Self::Resized,
            Self::Minimized,
            Self::Unminimized,
            Self::TitleChanged,
            Self::AppActivated,
            Self::AppDeactivated,
            Self::AppHidden,
            Self::AppShown,
        ]
    }
}

/// A window event received from the accessibility system.
#[derive(Debug, Clone, Copy)]
pub struct WindowEvent {
    /// The type of event.
    pub event_type: WindowEventType,
    /// The process ID of the application that owns the element.
    pub pid: i32,
    /// The accessibility element that triggered the event.
    /// This is an opaque pointer that should not be dereferenced directly.
    pub element: usize,
}

impl WindowEvent {
    /// Creates a new window event.
    #[must_use]
    pub const fn new(event_type: WindowEventType, pid: i32, element: usize) -> Self {
        Self { event_type, pid, element }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_window_event_type_notification_names() {
        assert_eq!(WindowEventType::Created.notification_name(), "AXWindowCreated");
        assert_eq!(
            WindowEventType::Destroyed.notification_name(),
            "AXUIElementDestroyed"
        );
        assert_eq!(
            WindowEventType::Focused.notification_name(),
            "AXFocusedWindowChanged"
        );
        assert_eq!(WindowEventType::Moved.notification_name(), "AXWindowMoved");
        assert_eq!(WindowEventType::Resized.notification_name(), "AXWindowResized");
        assert_eq!(
            WindowEventType::Minimized.notification_name(),
            "AXWindowMiniaturized"
        );
        assert_eq!(
            WindowEventType::Unminimized.notification_name(),
            "AXWindowDeminiaturized"
        );
        assert_eq!(
            WindowEventType::TitleChanged.notification_name(),
            "AXTitleChanged"
        );
        assert_eq!(
            WindowEventType::AppActivated.notification_name(),
            "AXApplicationActivated"
        );
        assert_eq!(
            WindowEventType::AppDeactivated.notification_name(),
            "AXApplicationDeactivated"
        );
        assert_eq!(
            WindowEventType::AppHidden.notification_name(),
            "AXApplicationHidden"
        );
        assert_eq!(
            WindowEventType::AppShown.notification_name(),
            "AXApplicationShown"
        );
    }

    #[test]
    fn test_window_event_type_from_notification() {
        assert_eq!(
            WindowEventType::from_notification("AXWindowCreated"),
            Some(WindowEventType::Created)
        );
        assert_eq!(
            WindowEventType::from_notification("AXUIElementDestroyed"),
            Some(WindowEventType::Destroyed)
        );
        assert_eq!(
            WindowEventType::from_notification("AXFocusedWindowChanged"),
            Some(WindowEventType::Focused)
        );
        assert_eq!(
            WindowEventType::from_notification("AXWindowMoved"),
            Some(WindowEventType::Moved)
        );
        assert_eq!(
            WindowEventType::from_notification("AXWindowResized"),
            Some(WindowEventType::Resized)
        );
        assert_eq!(
            WindowEventType::from_notification("AXWindowMiniaturized"),
            Some(WindowEventType::Minimized)
        );
        assert_eq!(
            WindowEventType::from_notification("AXWindowDeminiaturized"),
            Some(WindowEventType::Unminimized)
        );
        assert_eq!(
            WindowEventType::from_notification("AXTitleChanged"),
            Some(WindowEventType::TitleChanged)
        );
        assert_eq!(WindowEventType::from_notification("Unknown"), None);
    }

    #[test]
    fn test_window_event_type_all() {
        let all = WindowEventType::all();
        assert!(all.len() >= 10);
        assert!(all.contains(&WindowEventType::Created));
        assert!(all.contains(&WindowEventType::Destroyed));
        assert!(all.contains(&WindowEventType::Focused));
        assert!(all.contains(&WindowEventType::Moved));
        assert!(all.contains(&WindowEventType::Resized));
    }

    #[test]
    fn test_window_event_new() {
        let event = WindowEvent::new(WindowEventType::Created, 1234, 0x1234_5678);
        assert_eq!(event.event_type, WindowEventType::Created);
        assert_eq!(event.pid, 1234);
        assert_eq!(event.element, 0x1234_5678);
    }
}
