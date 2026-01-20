//! State module for the tiling window manager.
//!
//! This module contains:
//! - Core types (`Screen`, `Workspace`, `Window`, `Rect`, etc.)
//! - The main `TilingState` struct with reactive collections

mod tiling_state;
mod types;

pub use tiling_state::TilingState;
pub use types::{FocusState, LayoutType, Rect, Screen, Window, Workspace};
