//! Tiling window manager module.
//!
//! This module provides a tiling window manager with support for multiple layouts,
//! virtual workspaces, multi-screen support, and window rules.

pub mod accessibility;
pub mod animation;
pub mod command_queue;
pub mod commands;
pub mod debouncer;
pub mod error;
pub mod layout;
pub mod manager;
pub mod observer;
pub mod rules;
pub mod screen;
pub mod state;
pub mod window;
pub mod workspace;

// Re-export commonly used types
pub use error::TilingError;
pub use manager::{init, try_get_manager};
