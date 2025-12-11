//! Window management module.

mod ax_cache;
mod control;
mod info;

pub use ax_cache::{clear_cache, invalidate_app, invalidate_window};
pub use control::*;
pub use info::*;
