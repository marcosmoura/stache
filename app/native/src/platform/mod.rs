//! macOS platform abstractions for Stache.
//!
//! This module provides platform-specific utilities for macOS integration:
//!
//! - [`accessibility`] - Accessibility permission utilities
//! - [`objc`] - Objective-C helper utilities
//! - [`window`] - Window manipulation via `SkyLight` and `AppKit`
//! - [`ipc`] - Inter-process communication mechanisms

pub mod accessibility;
pub mod ipc;
pub mod objc;
pub mod window;

pub use accessibility::{check_and_prompt as check_accessibility, is_trusted as has_accessibility};
pub use ipc::notification::{
    StacheNotification, register_notification_handler, send_notification,
    start_notification_listener,
};
pub use ipc::socket::{IpcError, IpcQuery, IpcResponse, send_query, start_server, stop_server};
pub use objc::{get_app_bundle_id, nsstring, nsstring_to_string};
pub use window::{
    get_screen_size, set_position, set_window_always_on_top, set_window_below_menu,
    set_window_level, set_window_sticky,
};
