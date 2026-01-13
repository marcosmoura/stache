//! Window border management via `JankyBorders` integration.
//!
//! This module provides visual borders around managed windows to indicate
//! focus state, layout mode, and floating status. Border rendering is delegated
//! to `JankyBorders`, a high-performance external tool.
//!
//! # Architecture
//!
//! - `janky`: `JankyBorders` CLI/Mach IPC integration
//! - `mach_ipc`: Low-latency Mach IPC for direct `JankyBorders` communication
//! - `manager`: Tracks border state for all windows
//!
//! # Configuration
//!
//! Borders are configured via `TilingConfig.borders`:
//! - `enabled`: Whether borders are rendered (default: false)
//! - `focused`: Color/style for focused windows
//! - `unfocused`: Color/style for unfocused windows
//! - `monocle`: Color/style for monocle layout
//! - `floating`: Color/style for floating windows

pub mod janky;
pub mod mach_ipc;
pub mod manager;

// Re-export commonly used types
pub use manager::{BorderManager, BorderState, get_border_manager, init_border_manager};

use crate::config::get_config;

/// Initializes the border system.
///
/// This checks if borders are enabled in the configuration and initializes
/// the border manager if enabled. Also attempts to connect to `JankyBorders`
/// via Mach IPC for low-latency communication.
///
/// # Returns
///
/// Returns `true` if borders were successfully initialized (or are disabled),
/// `false` if initialization failed.
pub fn init() -> bool {
    let config = get_config();

    if !config.tiling.borders.is_enabled() {
        return true; // Not an error, just disabled
    }

    // Check if JankyBorders is available
    if !janky::is_available() {
        eprintln!("stache: borders: JankyBorders not found");
        eprintln!("stache: borders: install with: brew install FelixKratz/formulae/borders");
        return false;
    }

    // Try to establish Mach IPC connection for low-latency updates
    if mach_ipc::connect() {
        eprintln!("stache: borders: connected to JankyBorders via Mach IPC");
    } else {
        eprintln!("stache: borders: JankyBorders not running, will use CLI fallback");
    }

    // Initialize the border manager
    if !init_border_manager() {
        // Manager already initialized or disabled
        return true;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_returns_true() {
        // init() should return true even when borders are disabled
        assert!(init());
    }
}
