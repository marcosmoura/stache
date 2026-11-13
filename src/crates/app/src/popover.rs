//! The tray popover: identity, the toggle state machine, its command and its
//! event.
//!
//! The popover is one host-owned window (`stache-popover`, surface `popover`)
//! created on first open and then only shown and hidden. [`NativeWindows`]
//! owns it; this module holds the pure decisions so the tray click race is
//! testable without `AppKit`.
//!
//! [`NativeWindows`]: crate::windows::NativeWindows

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use specta::Type;
use stache_sdk::SurfaceId;

/// The popover's fixed window label.
pub const POPOVER_LABEL: &str = "stache-popover";

/// The UI surface the popover renders.
pub const POPOVER_SURFACE: SurfaceId = SurfaceId::new("popover");

/// Logical width of the popover.
pub const POPOVER_WIDTH: u32 = 480;

/// Logical height of the popover.
pub const POPOVER_HEIGHT: u32 = 640;

/// How long after a resign-key hide a tray click still counts as the click
/// that caused it, so it closes the popover instead of reopening it.
pub const TOGGLE_GUARD: Duration = Duration::from_millis(400);

/// Whether the popover is shown, and when it last hid because it lost key.
///
/// Every transition happens under the windows lock, so a click and a
/// resign-key notification are ordered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PopoverToggle {
    visible: bool,
    resigned_at: Option<Instant>,
}

impl PopoverToggle {
    /// Whether the popover is shown.
    #[must_use]
    pub const fn is_visible(&self) -> bool { self.visible }

    /// A left click on the tray icon. Returns `true` to show the popover.
    ///
    /// A shown popover hides. A click within [`TOGGLE_GUARD`] of a resign-key
    /// hide is the same gesture (pressing the icon took key away first), so it
    /// keeps the popover closed.
    pub fn click(&mut self, now: Instant) -> bool {
        if self.visible {
            self.visible = false;
            return false;
        }
        if let Some(at) = self.resigned_at.take()
            && now.saturating_duration_since(at) < TOGGLE_GUARD
        {
            return false;
        }
        self.visible = true;
        true
    }

    /// An explicit open (`Open Stache`). Always shows; a shown popover is
    /// shown again so it regains key.
    pub const fn open(&mut self) {
        self.visible = true;
        self.resigned_at = None;
    }

    /// An explicit hide (Esc, opening another host window, shutdown). Returns
    /// whether it was shown.
    pub const fn hide(&mut self) -> bool {
        let was = self.visible;
        self.visible = false;
        was
    }

    /// The panel lost key. Returns whether it was shown (and must hide).
    pub const fn resigned(&mut self, now: Instant) -> bool {
        if !self.visible {
            return false;
        }
        self.visible = false;
        self.resigned_at = Some(now);
        true
    }
}

/// Emitted to the popover each time it is shown, so it can focus the
/// selected tab and refetch.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type, tauri_specta::Event,
)]
#[serde(rename_all = "camelCase")]
pub struct PopoverShown {}

/// Hides the popover (Esc in the popover). Returns at once.
#[tauri::command]
#[specta::specta]
#[allow(clippy::needless_pass_by_value)] // Tauri injects the handle by value.
pub fn hide_popover(app: tauri::AppHandle) {
    use tauri::Manager as _;

    if let Some(windows) = app.try_state::<crate::WindowsState>() {
        windows.0.hide_popover();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_shows_and_a_second_click_hides() {
        let now = Instant::now();
        let mut toggle = PopoverToggle::default();
        assert!(toggle.click(now));
        assert!(toggle.is_visible());
        assert!(!toggle.click(now + Duration::from_secs(1)));
        assert!(!toggle.is_visible());
        assert!(toggle.click(now + Duration::from_secs(2)));
    }

    #[test]
    fn a_click_right_after_a_resign_key_hide_keeps_it_closed() {
        let now = Instant::now();
        let mut toggle = PopoverToggle::default();
        assert!(toggle.click(now));
        assert!(toggle.resigned(now + Duration::from_millis(10)));
        assert!(!toggle.click(now + Duration::from_millis(60)));
        assert!(!toggle.is_visible());
        // The guard is spent: the next click opens.
        assert!(toggle.click(now + Duration::from_millis(100)));
    }

    #[test]
    fn a_click_long_after_a_resign_key_hide_opens() {
        let now = Instant::now();
        let mut toggle = PopoverToggle::default();
        toggle.open();
        assert!(toggle.resigned(now));
        assert!(toggle.click(now + TOGGLE_GUARD));
    }

    #[test]
    fn resign_and_hide_only_report_a_shown_popover() {
        let now = Instant::now();
        let mut toggle = PopoverToggle::default();
        assert!(!toggle.resigned(now));
        assert!(!toggle.hide());
        toggle.open();
        assert!(toggle.hide());
        // An explicit hide sets no guard.
        assert!(toggle.click(now));
    }

    #[test]
    fn open_clears_a_pending_guard() {
        let now = Instant::now();
        let mut toggle = PopoverToggle::default();
        toggle.open();
        toggle.resigned(now);
        toggle.open();
        assert!(toggle.is_visible());
        toggle.hide();
        assert!(toggle.click(now));
    }
}
