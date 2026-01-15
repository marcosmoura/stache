//! Centralized event definitions for Tauri frontend communication.
//!
//! All events emitted to the frontend are defined here to ensure consistency
//! and make it easy to keep Rust and TypeScript in sync.
//!
//! ## Naming Convention
//!
//! All events follow the pattern: `stache://<module>/<event-name>`
//!
//! - `stache://` - Prefix identifying this as a Stache event
//! - `<module>` - The module/feature that owns the event (e.g., `media`, `menubar`)
//! - `<event-name>` - Descriptive kebab-case name for the event
//!
//! ## Examples
//!
//! - `stache://media/playback-changed` - Media playback state changed
//! - `stache://menubar/visibility-changed` - Menu bar visibility changed
//! - `stache://spaces/window-focus-changed` - Window focus changed (for Spaces component)

/// Menubar-related events.
pub mod menubar {
    /// Emitted when the system menu bar visibility changes.
    ///
    /// Payload: `bool` - `true` if visible, `false` if hidden.
    pub const VISIBILITY_CHANGED: &str = "stache://menubar/visibility-changed";
}

/// Keep-awake (caffeinate) related events.
pub mod keepawake {
    /// Emitted when the keep-awake state changes.
    ///
    /// Payload: `{ locked: bool, desired_awake: bool }`
    pub const STATE_CHANGED: &str = "stache://keepawake/state-changed";
}

/// Media playback related events.
pub mod media {
    /// Emitted when media playback state changes.
    ///
    /// Payload: Media info JSON object with title, artist, album, artwork, etc.
    pub const PLAYBACK_CHANGED: &str = "stache://media/playback-changed";
}

/// Spaces/workspace related events.
///
/// These events are triggered by CLI commands (`stache event ...`) and are used
/// by the Spaces component to refresh workspace and window data.
pub mod spaces {
    /// Emitted when the focused window changes.
    ///
    /// Triggered by: `stache event window-focus-changed`
    ///
    /// Payload: `()` (no payload)
    pub const WINDOW_FOCUS_CHANGED: &str = "stache://spaces/window-focus-changed";

    /// Emitted when the active workspace changes.
    ///
    /// Triggered by: `stache event workspace-changed <name>`
    ///
    /// Payload: `String` - The new workspace name.
    pub const WORKSPACE_CHANGED: &str = "stache://spaces/workspace-changed";
}

/// Widget-related events.
pub mod widgets {
    /// Emitted to toggle a widget's visibility.
    ///
    /// Sent from the bar when a widget trigger is clicked.
    ///
    /// Payload: `WidgetConfig` - Configuration for the widget to toggle.
    pub const TOGGLE: &str = "stache://widgets/toggle";

    /// Emitted when user clicks outside the widgets window.
    ///
    /// Used to close the widgets overlay when clicking away.
    ///
    /// Payload: `()` (no payload)
    pub const CLICK_OUTSIDE: &str = "stache://widgets/click-outside";
}

/// Cmd+Q hold-to-quit related events.
pub mod cmd_q {
    /// Emitted when user presses Cmd+Q to show the hold-to-quit alert.
    ///
    /// Payload: `String` - The message to display (e.g., "Hold ⌘Q to quit Safari").
    pub const ALERT: &str = "stache://cmd-q/alert";
}

/// Application lifecycle events.
pub mod app {
    /// Emitted when a reload is requested via CLI (`stache reload`).
    ///
    /// The frontend can use this to refresh data or perform cleanup before
    /// the app restarts (in release mode) or to manually refresh state
    /// (in debug mode where restart doesn't happen).
    ///
    /// Payload: `()` (no payload)
    pub const RELOAD: &str = "stache://app/reload";
}

/// Tiling window manager events.
///
/// These events are emitted by the tiling module to notify the frontend
/// about workspace, window, and layout changes.
pub mod tiling {
    /// Emitted when the focused workspace changes.
    ///
    /// Payload: `{ workspace: String, screen: String }`
    pub const WORKSPACE_CHANGED: &str = "stache://tiling/workspace-changed";

    /// Emitted when windows in a workspace change (added/removed).
    ///
    /// Payload: `{ workspace: String, windows: Vec<u32> }`
    pub const WORKSPACE_WINDOWS_CHANGED: &str = "stache://tiling/workspace-windows-changed";

    /// Emitted when a workspace's layout changes.
    ///
    /// Payload: `{ workspace: String, layout: String }`
    pub const LAYOUT_CHANGED: &str = "stache://tiling/layout-changed";

    /// Emitted when a new window is tracked by the tiling manager.
    ///
    /// Payload: `{ windowId: u32, workspace: String }`
    pub const WINDOW_TRACKED: &str = "stache://tiling/window-tracked";

    /// Emitted when a window is no longer tracked.
    ///
    /// Payload: `{ windowId: u32 }`
    pub const WINDOW_UNTRACKED: &str = "stache://tiling/window-untracked";

    /// Emitted when screens are connected or disconnected.
    ///
    /// Payload: Array of screen objects.
    pub const SCREENS_CHANGED: &str = "stache://tiling/screens-changed";

    /// Emitted when the tiling manager finishes initialization.
    ///
    /// Payload: `{ enabled: bool }`
    pub const INITIALIZED: &str = "stache://tiling/initialized";

    /// Emitted when window focus changes.
    ///
    /// Payload: `{ windowId: u32, workspace: String }`
    pub const WINDOW_FOCUS_CHANGED: &str = "stache://tiling/window-focus-changed";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_events_have_stache_prefix() {
        let events = [
            menubar::VISIBILITY_CHANGED,
            keepawake::STATE_CHANGED,
            media::PLAYBACK_CHANGED,
            spaces::WINDOW_FOCUS_CHANGED,
            spaces::WORKSPACE_CHANGED,
            widgets::TOGGLE,
            widgets::CLICK_OUTSIDE,
            cmd_q::ALERT,
            app::RELOAD,
            tiling::WORKSPACE_CHANGED,
            tiling::WORKSPACE_WINDOWS_CHANGED,
            tiling::LAYOUT_CHANGED,
            tiling::WINDOW_TRACKED,
            tiling::WINDOW_UNTRACKED,
            tiling::SCREENS_CHANGED,
            tiling::INITIALIZED,
            tiling::WINDOW_FOCUS_CHANGED,
        ];

        for event in events {
            assert!(
                event.starts_with("stache://"),
                "Event '{event}' should start with 'stache://'"
            );
        }
    }

    #[test]
    fn test_event_naming_convention() {
        // All events should follow stache://<module>/<event-name> pattern
        let events = [
            (menubar::VISIBILITY_CHANGED, "menubar", "visibility-changed"),
            (keepawake::STATE_CHANGED, "keepawake", "state-changed"),
            (media::PLAYBACK_CHANGED, "media", "playback-changed"),
            (spaces::WINDOW_FOCUS_CHANGED, "spaces", "window-focus-changed"),
            (spaces::WORKSPACE_CHANGED, "spaces", "workspace-changed"),
            (widgets::TOGGLE, "widgets", "toggle"),
            (widgets::CLICK_OUTSIDE, "widgets", "click-outside"),
            (cmd_q::ALERT, "cmd-q", "alert"),
            (app::RELOAD, "app", "reload"),
            (tiling::WORKSPACE_CHANGED, "tiling", "workspace-changed"),
            (
                tiling::WORKSPACE_WINDOWS_CHANGED,
                "tiling",
                "workspace-windows-changed",
            ),
            (tiling::LAYOUT_CHANGED, "tiling", "layout-changed"),
            (tiling::WINDOW_TRACKED, "tiling", "window-tracked"),
            (tiling::WINDOW_UNTRACKED, "tiling", "window-untracked"),
            (tiling::SCREENS_CHANGED, "tiling", "screens-changed"),
            (tiling::INITIALIZED, "tiling", "initialized"),
            (tiling::WINDOW_FOCUS_CHANGED, "tiling", "window-focus-changed"),
        ];

        for (event, module, name) in events {
            let expected = format!("stache://{module}/{name}");
            assert_eq!(event, expected, "Event should match expected format");
        }
    }
}
