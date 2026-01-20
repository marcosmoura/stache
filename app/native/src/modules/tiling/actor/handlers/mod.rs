//! Handler modules for the state actor.
//!
//! Each module contains the implementation of handlers for a specific
//! category of messages:
//! - `window` - Window lifecycle and state changes
//! - `app` - Application lifecycle events
//! - `screen` - Display configuration changes
//! - `workspace` - Workspace switching, cycling, balancing
//! - `layout` - Layout switching and cycling
//! - `focus` - Focus cycling and directional focus
//! - `window_move` - Moving windows between workspaces
//! - `preset` - Floating preset application
//! - `resize` - Split ratio manipulation and window resizing

pub mod app;
pub mod focus;
pub mod layout;
pub mod preset;
pub mod resize;
pub mod screen;
pub mod window;
pub mod window_move;
pub mod workspace;

// Re-export handler functions for convenience
pub use app::{on_app_activated, on_app_hidden, on_app_launched, on_app_shown, on_app_terminated};
pub use focus::{on_cycle_focus, on_focus_window, on_swap_window_in_direction};
pub use layout::{on_cycle_layout, on_set_layout};
pub use preset::on_apply_preset;
pub use resize::{on_resize_focused_window, on_resize_split, on_user_resize_completed};
pub use screen::{get_screens_from_macos, on_screens_changed, on_set_screens};
pub use window::{
    on_batched_geometry_updates, on_window_created, on_window_created_silent, on_window_destroyed,
    on_window_focused, on_window_fullscreen_changed, on_window_minimized, on_window_moved,
    on_window_resized, on_window_title_changed, on_window_unfocused,
};
pub use window_move::{
    on_move_window_to_workspace, on_send_window_to_screen, on_swap_windows, on_toggle_floating,
};
pub use workspace::{
    on_balance_workspace, on_cycle_workspace, on_send_workspace_to_screen, on_switch_workspace,
};
