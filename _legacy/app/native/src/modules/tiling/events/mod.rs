//! Event processing module for the tiling window manager.
//!
//! This module handles:
//! - Converting raw macOS events into `StateMessage`s
//! - Batching geometry updates per display refresh rate
//! - Immediate dispatch for time-sensitive events (focus, create, destroy)
//!
//! # Adapters
//!
//! The module provides adapters that bridge existing macOS event sources
//! to the new event processor:
//!
//! - [`ax_observer`] - `AXObserver` window events (created, destroyed, focused, etc.)
//! - [`app_monitor`] - `NSWorkspace` app lifecycle events (launch, terminate)
//! - [`screen_monitor`] - CoreGraphics display configuration events
//!
//! # Usage
//!
//! ```ignore
//! use std::sync::Arc;
//! use stache::modules::tiling::actor::StateActor;
//! use stache::modules::tiling::events::{
//!     EventProcessor,
//!     ax_observer::{AXObserverAdapter, install_adapter as install_ax_adapter},
//!     app_monitor::{AppMonitorAdapter, install_adapter as install_app_adapter},
//!     screen_monitor::{ScreenMonitorAdapter, install_adapter as install_screen_adapter},
//! };
//!
//! // Create state actor
//! let actor_handle = StateActor::spawn();
//!
//! // Create event processor
//! let processor = Arc::new(EventProcessor::new(actor_handle.clone()));
//!
//! // Create and install adapters
//! let ax_adapter = Arc::new(AXObserverAdapter::new(processor.clone()));
//! let app_adapter = Arc::new(AppMonitorAdapter::new(processor.clone()));
//! let screen_adapter = Arc::new(ScreenMonitorAdapter::new(processor.clone()));
//!
//! install_ax_adapter(ax_adapter.clone());
//! install_app_adapter(app_adapter.clone());
//! install_screen_adapter(screen_adapter.clone());
//!
//! // Initialize adapters
//! screen_adapter.init();  // Registers screens first
//! app_adapter.init();     // Registers for app events
//! ax_adapter.activate();  // Activates window event processing
//!
//! // Start the processor
//! processor.start();
//! ```

mod processor;
mod types;

pub mod app_monitor;
pub mod ax_observer;
pub mod drag_state;
pub mod mouse_monitor;
pub mod observer;
pub mod screen_monitor;

// Re-export processor
// Re-export adapter types for convenience
pub use app_monitor::AppMonitorAdapter;
pub use ax_observer::AXObserverAdapter;
pub use processor::{EventProcessor, get_display_refresh_rate, get_main_display_refresh_rate};
pub use screen_monitor::ScreenMonitorAdapter;
// Re-export event types
pub use types::{WindowEvent, WindowEventType, notifications};
