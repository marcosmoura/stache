//! Tiling Window Manager - Reactive State Architecture
//!
//! This module provides the tiling window manager using:
//! - `eyeball` + `eyeball-im` for reactive observable state
//! - Actor model with `tokio` channels for message passing
//! - Computed queries for efficient, granular subscriptions
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    macOS Event Sources                       │
//! │  (AXObserver, NSWorkspace, CGDisplay notifications)         │
//! └─────────────────────────┬───────────────────────────────────┘
//!                           │
//!                           ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    Event Processor                           │
//! │  - Immediate dispatch for focus events                      │
//! │  - Batched dispatch for geometry (per display refresh)      │
//! └─────────────────────────┬───────────────────────────────────┘
//!                           │ mpsc::Sender<StateMessage>
//!                           ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │                     State Actor                              │
//! │  - Owns all state (screens, workspaces, windows)            │
//! │  - Processes messages sequentially                          │
//! │  - Mutations trigger Observable notifications               │
//! └─────────────────────────┬───────────────────────────────────┘
//!                           │ Observable subscriptions
//!                           ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │                   Effect Subscribers                         │
//! │  - Layout subscriber (compute & apply window positions)     │
//! │  - Border subscriber (update JankyBorders)                  │
//! │  - Frontend subscriber (emit Tauri events)                  │
//! └─────────────────────────────────────────────────────────────┘
//! ```

pub mod actor;
pub mod borders;
pub mod commands;
pub mod effects;
pub mod events;
pub mod ffi;
pub mod init;
pub mod layout;
pub mod rules;
pub mod state;
pub mod tabs;
pub mod window;

// Re-export commonly used types
pub use actor::{StateActor, StateActorHandle, StateMessage, StateQuery};
// Helper functions used by bar module
pub use commands::{is_tiling_enabled, layout_to_string_pub};
pub use effects::{
    AnimationConfig, AnimationSystem, BorderState, EffectExecutor, EffectSubscriber,
    EffectSubscriberHandle, FocusChange, LayoutChange, TilingEffect, VisibilityChange,
    WindowTransition, begin_animation, cancel_animation, focus_window, get_interrupted_position,
    get_window_frame, is_animation_active, is_animation_settling, raise_window, set_window_frame,
    set_window_frame_fast, should_ignore_geometry_events,
};
pub use events::{
    AXObserverAdapter, AppMonitorAdapter, EventProcessor, ScreenMonitorAdapter, WindowEvent,
    WindowEventType,
};
pub use init::{
    emit_layout_applied, emit_window_focus_changed, emit_window_tracked, emit_window_untracked,
    emit_workspace_changed, get_handle, get_subscriber_handle, init, is_enabled, is_initialized,
    shutdown,
};
pub use layout::{
    Gaps, LAYOUT_INLINE_CAP, LayoutResult, MAX_GRID_WINDOWS, MasterPosition, calculate_layout,
    calculate_layout_full, calculate_layout_with_gaps,
};
pub use state::{FocusState, LayoutType, Rect, Screen, TilingState, Window, Workspace};
pub use window::{
    AppInfo, WindowInfo, get_all_windows_including_hidden, get_running_apps, get_visible_windows,
};
