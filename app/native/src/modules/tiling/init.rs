//! Initialization and global state management for `tiling`.
//!
//! This module provides the entry point for initializing the tiling window manager
//! and accessing the global state actor handle.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────┐
//! │                         init()                                           │
//! │  1. Create StateActor + Handle                                          │
//! │  2. Create EventProcessor (with screen refresh rate batching)           │
//! │  3. Create EffectSubscriber + Executor                                  │
//! │  4. Wire everything together                                            │
//! │  5. Start event adapters (AX, App, Screen monitors)                     │
//! │  6. Detect screens and create initial workspaces                        │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use crate::tiling::init;
//!
//! // During app startup:
//! init::init(app_handle);
//!
//! // Later, to send commands:
//! if let Some(handle) = init::get_handle() {
//!     handle.send(StateMessage::SwitchWorkspace { name: "main".into() });
//! }
//! ```

use std::sync::{Arc, Mutex, OnceLock};

use tauri::Emitter;

use super::actor::{StateActor, StateActorHandle, StateMessage};
use super::borders;
use super::effects::subscriber::EffectSubscriberHandle;
use super::effects::{EffectExecutor, EffectSubscriber};
use super::events::{AppMonitorAdapter, EventProcessor, ScreenMonitorAdapter};
use crate::config::get_config;
use crate::{events, is_accessibility_granted};

// ============================================================================
// Global State
// ============================================================================

/// Global state actor handle.
static HANDLE: OnceLock<StateActorHandle> = OnceLock::new();

/// Global event processor.
static PROCESSOR: OnceLock<Arc<EventProcessor>> = OnceLock::new();

/// Global effect subscriber handle for notifying the subscriber of state changes.
static SUBSCRIBER_HANDLE: OnceLock<EffectSubscriberHandle> = OnceLock::new();

/// Stored Tauri app handle for emitting events.
static APP_HANDLE: Mutex<Option<tauri::AppHandle>> = Mutex::new(None);

/// Whether the tiling system has been initialized.
static INITIALIZED: OnceLock<bool> = OnceLock::new();

// ============================================================================
// Public API
// ============================================================================

/// Gets the global state actor handle.
///
/// Returns `None` if the tiling system hasn't been initialized yet.
#[must_use]
pub fn get_handle() -> Option<&'static StateActorHandle> {
    let handle = HANDLE.get();
    if handle.is_none() {
        tracing::trace!("tiling: get_handle called before initialization");
    }
    handle
}

/// Gets the global event processor.
///
/// Returns `None` if the tiling system hasn't been initialized yet.
#[must_use]
pub fn get_processor() -> Option<Arc<EventProcessor>> { PROCESSOR.get().cloned() }

/// Gets the global effect subscriber handle.
///
/// Returns `None` if the tiling system hasn't been initialized yet.
#[must_use]
pub fn get_subscriber_handle() -> Option<&'static EffectSubscriberHandle> {
    SUBSCRIBER_HANDLE.get()
}

/// Returns whether the tiling system has been initialized.
#[must_use]
pub fn is_initialized() -> bool { INITIALIZED.get().copied().unwrap_or(false) }

/// Returns whether tiling is enabled in config.
#[must_use]
pub fn is_enabled() -> bool { get_config().tiling.is_enabled() }

/// Initializes the `tiling` window manager.
///
/// This function:
/// 1. Checks if tiling is enabled in configuration
/// 2. Verifies accessibility permissions
/// 3. Creates and starts the state actor
/// 4. Creates and starts the event processor
/// 5. Creates and starts the effect subscriber
/// 6. Detects screens and creates initial workspaces
/// 7. Starts monitoring for window/app/screen events
///
/// # Arguments
///
/// * `app_handle` - Tauri app handle for emitting events to the frontend.
///
/// # Returns
///
/// `true` if initialization succeeded, `false` otherwise.
#[allow(clippy::needless_pass_by_value)] // AppHandle is intentionally passed by value for storage
pub fn init(app_handle: tauri::AppHandle) -> bool {
    // Check if already initialized
    if INITIALIZED.get().is_some() {
        tracing::warn!("tiling: already initialized");
        return false;
    }

    let config = get_config();

    // Check if tiling is enabled
    if !config.tiling.is_enabled() {
        tracing::info!("tiling: disabled in config (set enabled=true to enable)");
        let _ = INITIALIZED.set(false);
        return false;
    }

    // Check accessibility permissions
    if !is_accessibility_granted() {
        tracing::warn!("tiling: accessibility permissions not granted");
        let _ = INITIALIZED.set(false);
        return false;
    }

    // Store app handle for event emission
    store_app_handle(app_handle.clone());

    // Initialize the system
    match init_internal() {
        Ok(()) => {
            let _ = INITIALIZED.set(true);
            tracing::info!("tiling: initialized successfully");

            // Emit initialized event
            if let Err(e) = app_handle.emit(
                events::tiling::INITIALIZED,
                serde_json::json!({ "enabled": true, "version": "v2" }),
            ) {
                tracing::warn!("tiling: failed to emit initialized event: {e}");
            }

            true
        }
        Err(e) => {
            tracing::error!("tiling: initialization failed: {e}");
            let _ = INITIALIZED.set(false);
            false
        }
    }
}

/// Shuts down the tiling system.
///
/// This sends a shutdown message to the state actor and stops the processor.
pub fn shutdown() {
    if let Some(handle) = HANDLE.get() {
        let _ = handle.send(StateMessage::Shutdown);
        tracing::info!("tiling: shutdown requested");
    }

    if let Some(processor) = PROCESSOR.get() {
        processor.stop();
    }
}

// ============================================================================
// Internal Initialization
// ============================================================================

/// Internal initialization that can return errors.
fn init_internal() -> Result<(), String> {
    // Spawn the state actor and get the handle
    // StateActor::spawn() creates the actor and returns the handle
    let handle = StateActor::spawn();

    // Store the handle globally
    HANDLE
        .set(handle.clone())
        .map_err(|_| "Failed to store handle - already initialized")?;

    // Create the event processor
    let processor = Arc::new(EventProcessor::new(handle.clone()));

    // Store the processor globally
    PROCESSOR
        .set(processor.clone())
        .map_err(|_| "Failed to store processor - already initialized")?;

    // Start the event processor
    processor.start();

    // Create the effect executor with app handle for event emission
    let mut executor = get_app_handle().map_or_else(
        || {
            tracing::warn!("tiling: no app handle available, events will not be emitted");
            EffectExecutor::new()
        },
        EffectExecutor::with_app_handle,
    );

    // Enable border updates if borders are configured
    let config = crate::config::get_config();
    if config.tiling.borders.is_enabled() {
        tracing::debug!("tiling: borders enabled in config, enabling border updates in executor");
        executor.set_borders_enabled(true);
    }

    // Create and spawn the effect subscriber using Tauri's async runtime
    let (subscriber, subscriber_handle) = EffectSubscriber::new(handle.clone(), executor);

    // Store the subscriber handle globally so handlers can notify the subscriber
    if SUBSCRIBER_HANDLE.set(subscriber_handle).is_err() {
        tracing::warn!("tiling: subscriber handle already set");
    }

    tauri::async_runtime::spawn(subscriber.run());

    // Create and initialize the app monitor adapter
    let app_monitor = Arc::new(AppMonitorAdapter::new(processor.clone()));
    if !app_monitor.init() {
        tracing::warn!("tiling: app monitor initialization failed");
    }
    // Install the adapter globally so callbacks can access it
    super::events::app_monitor::install_adapter(app_monitor);

    // Create and initialize the screen monitor adapter
    let screen_monitor = Arc::new(ScreenMonitorAdapter::new(processor.clone()));
    if !screen_monitor.init() {
        tracing::warn!("tiling: screen monitor initialization failed");
    }
    // Install the adapter globally so callbacks can access it
    super::events::screen_monitor::install_adapter(screen_monitor);

    // Create and install the AX observer adapter
    let ax_adapter = Arc::new(super::events::AXObserverAdapter::new(processor));
    super::events::ax_observer::install_adapter(ax_adapter.clone());
    ax_adapter.activate();

    // Initialize the standalone v2 AXObserver system
    if super::events::observer::init() {
        tracing::debug!("tiling: AXObserver initialized");
    } else {
        tracing::warn!("tiling: AXObserver initialization failed");
    }

    // Initialize the mouse monitor for drag/resize detection
    if super::events::mouse_monitor::init() {
        // Set up the callback for when mouse is released after a drag/resize
        super::events::mouse_monitor::set_mouse_up_callback(on_mouse_up);
        tracing::debug!("tiling: mouse monitor initialized");
    } else {
        tracing::warn!("tiling: mouse monitor initialization failed");
    }

    // Initialize the border system (connects to JankyBorders if available)
    if !borders::init() {
        tracing::warn!("tiling: borders initialization failed (JankyBorders may not be installed)");
    }

    // Initialize screens and workspaces
    initialize_state(&handle);

    tracing::info!("tiling: all components started");
    Ok(())
}

/// Initializes the tiling state (screens, workspaces).
///
/// Detects screens on the main thread (where macOS APIs work) and sends
/// them to the actor via `SetScreens` message.
fn initialize_state(handle: &StateActorHandle) {
    // Detect screens on the main thread (this is called during Tauri setup)
    // NSScreen APIs must be called from the main thread
    tracing::debug!("tiling: detecting screens on main thread...");
    let screens = super::actor::handlers::get_screens_from_macos();

    if screens.is_empty() {
        tracing::warn!("tiling: no screens detected during initialization");
        return;
    }

    tracing::debug!(
        "tiling: detected {} screen(s): {:?}",
        screens.len(),
        screens.iter().map(|s| &s.name).collect::<Vec<_>>()
    );

    // Send pre-detected screens to the actor
    // This avoids calling macOS APIs from the async actor task
    if let Err(e) = handle.send(StateMessage::SetScreens { screens }) {
        tracing::error!("tiling: failed to send SetScreens message: {e}");
    }

    // Track existing windows
    track_existing_windows(handle);
}

// ============================================================================
// Window Tracking
// ============================================================================

/// Tracks all existing windows at startup.
///
/// Enumerates all windows using the AX-first approach and sends
/// a batch `BatchWindowsCreated` message to the actor.
/// Also sends a `WindowFocused` message for the currently focused window,
/// and an `InitComplete` message to trigger initial layouts.
fn track_existing_windows(handle: &StateActorHandle) {
    use super::actor::WindowCreatedInfo;
    use super::rules::should_tile_window;
    use super::window::{get_all_windows_including_hidden, get_focused_window_id};

    tracing::debug!("tiling: tracking existing windows...");

    // Get the currently focused window ID first (before enumeration)
    let focused_window_id = get_focused_window_id();
    tracing::trace!("tiling: system focused window id = {focused_window_id:?}");

    // Enumerate all windows including hidden ones
    let windows = get_all_windows_including_hidden();

    tracing::debug!("Found {} windows from system", windows.len());
    for w in &windows {
        tracing::trace!(
            "  - id={}, pid={}, app='{}', title='{}', minimized={}, hidden={}",
            w.id,
            w.pid,
            w.app_name,
            w.title,
            w.is_minimized,
            w.is_hidden
        );
    }

    // Collect all trackable windows
    let mut window_infos: Vec<WindowCreatedInfo> = Vec::new();

    // Group windows by PID for tab detection
    // Collect unique PIDs and scan for tabs
    let mut pids_seen: std::collections::HashSet<i32> = std::collections::HashSet::new();
    for window in &windows {
        if should_tile_window(&window.bundle_id, &window.app_name) {
            pids_seen.insert(window.pid);
        }
    }

    // Scan and register tabs for each app using the new TabRegistry approach
    for pid in &pids_seen {
        crate::modules::tiling::tabs::scan_and_register_tabs_for_app(*pid);
    }

    for window in &windows {
        // Filter out system apps that shouldn't be tiled
        if !should_tile_window(&window.bundle_id, &window.app_name) {
            tracing::trace!(
                "tiling: skipping system window '{}' from '{}'",
                window.title,
                window.app_name
            );
            continue;
        }

        // Check if this window is a tab (skip it - tabs are tracked separately)
        if crate::modules::tiling::tabs::is_tab(window.id) {
            continue;
        }

        // Create WindowCreatedInfo (tab detection is handled by the TabRegistry)
        let info = WindowCreatedInfo {
            window_id: window.id,
            pid: window.pid,
            app_id: window.bundle_id.clone(),
            app_name: window.app_name.clone(),
            title: window.title.clone(),
            frame: window.frame,
            is_minimized: window.is_minimized,
            is_fullscreen: window.is_fullscreen,
            minimum_size: window.minimum_size,
            tab_group_id: None,
            is_active_tab: true,
        };

        window_infos.push(info);
    }

    let tracked_count = window_infos.len();

    // Also track these windows in the event processor for destroy detection
    // This is necessary because BatchWindowsCreated bypasses the processor
    if let Some(processor) = get_processor() {
        let window_pids: Vec<(u32, i32)> =
            window_infos.iter().map(|w| (w.window_id, w.pid)).collect();
        processor.track_windows_for_destroy_detection(&window_pids);
    }

    // Send batch message (no individual layout notifications)
    if !window_infos.is_empty()
        && let Err(e) = handle.send(StateMessage::BatchWindowsCreated(window_infos))
    {
        tracing::error!("tiling: failed to send BatchWindowsCreated: {e}");
    }

    tracing::debug!("tiling: tracked {tracked_count} windows");

    // Send focus event for the currently focused window
    if let Some(window_id) = focused_window_id {
        tracing::trace!("tiling: setting initial focus to window {window_id}");
        if let Err(e) = handle.send(StateMessage::WindowFocused { window_id }) {
            tracing::error!("tiling: failed to send WindowFocused: {e}");
        }
    } else {
        tracing::trace!("tiling: no focused window detected at startup");
    }

    // Signal that initialization is complete - this triggers initial layouts
    tracing::trace!("tiling: sending InitComplete...");
    if let Err(e) = handle.send(StateMessage::InitComplete) {
        tracing::error!("tiling: failed to send InitComplete: {e}");
    }
}

// ============================================================================
// App Handle Management
// ============================================================================

/// Stores the Tauri app handle for later use in event emission.
pub fn store_app_handle(handle: tauri::AppHandle) {
    if let Ok(mut stored) = APP_HANDLE.lock() {
        *stored = Some(handle);
    }
}

/// Gets the stored app handle.
#[must_use]
pub fn get_app_handle() -> Option<tauri::AppHandle> {
    APP_HANDLE.lock().ok().and_then(|guard| guard.clone())
}

// ============================================================================
// Event Emission Helpers
// ============================================================================

/// Emits a workspace changed event to the frontend.
pub fn emit_workspace_changed(workspace: &str, screen: &str, previous_workspace: Option<&str>) {
    if let Some(handle) = get_app_handle() {
        let _ = handle.emit(
            events::tiling::WORKSPACE_CHANGED,
            serde_json::json!({
                "workspace": workspace,
                "screen": screen,
                "previousWorkspace": previous_workspace,
            }),
        );
    }
}

/// Emits a window focus changed event to the frontend.
pub fn emit_window_focus_changed(window_id: u32, workspace: &str) {
    if let Some(handle) = get_app_handle() {
        let _ = handle.emit(
            events::tiling::WINDOW_FOCUS_CHANGED,
            serde_json::json!({
                "windowId": window_id,
                "workspace": workspace,
            }),
        );
    }
}

/// Emits a window tracked event to the frontend.
pub fn emit_window_tracked(window_id: u32, workspace: &str) {
    if let Some(handle) = get_app_handle() {
        let _ = handle.emit(
            events::tiling::WINDOW_TRACKED,
            serde_json::json!({
                "windowId": window_id,
                "workspace": workspace,
            }),
        );
    }
}

/// Emits a window untracked event to the frontend.
pub fn emit_window_untracked(window_id: u32, workspace: &str) {
    if let Some(handle) = get_app_handle() {
        let _ = handle.emit(
            events::tiling::WINDOW_UNTRACKED,
            serde_json::json!({
                "windowId": window_id,
                "workspace": workspace,
            }),
        );
    }
}

/// Emits a layout applied event to the frontend.
pub fn emit_layout_applied(workspace: &str, layout: &str, window_count: usize) {
    if let Some(handle) = get_app_handle() {
        let _ = handle.emit(
            events::tiling::LAYOUT_CHANGED,
            serde_json::json!({
                "workspace": workspace,
                "layout": layout,
                "windowCount": window_count,
            }),
        );
    }
}

/// Emits a window title changed event to the frontend.
pub fn emit_window_title_changed(window_id: u32, title: &str) {
    if let Some(handle) = get_app_handle() {
        let _ = handle.emit(
            events::tiling::WINDOW_TITLE_CHANGED,
            serde_json::json!({
                "windowId": window_id,
                "title": title,
            }),
        );
    }
}

/// Emits a workspace windows changed event to the frontend.
///
/// This is called when windows in a workspace change (added, removed, minimized, etc.).
pub fn emit_workspace_windows_changed(workspace: &str, window_ids: &[u32]) {
    if let Some(handle) = get_app_handle() {
        let _ = handle.emit(
            events::tiling::WORKSPACE_WINDOWS_CHANGED,
            serde_json::json!({
                "workspace": workspace,
                "windows": window_ids,
            }),
        );
    }
}

// ============================================================================
// IPC Query Handler
// ============================================================================

use crate::platform::ipc_socket::{IpcQuery, IpcResponse};

/// Handles IPC queries for tiling v2.
///
/// This is called by the IPC socket server when v2 is enabled.
/// Handles both v2-specific queries and standard queries (Screens, Workspaces, Windows).
/// Returns None if the query should be handled by v1 handler.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn handle_ipc_query(query: &IpcQuery) -> Option<IpcResponse> {
    match query {
        // Ping is universal - respond to confirm app is running
        IpcQuery::Ping => Some(IpcResponse::success("pong")),

        // V2-specific enabled check
        IpcQuery::V2Enabled => Some(IpcResponse::success(is_initialized() && is_enabled())),

        // Standard queries - handle when v2 is enabled
        IpcQuery::Screens => handle_screens_query(),

        IpcQuery::Workspaces { screen, focused_screen } => {
            handle_workspaces_query(screen.as_deref(), *focused_screen)
        }

        IpcQuery::Windows {
            screen,
            workspace,
            focused_screen,
            focused_workspace,
            ..
        } => handle_windows_query(
            screen.as_deref(),
            workspace.as_deref(),
            *focused_screen,
            *focused_workspace,
        ),

        IpcQuery::Apps => handle_apps_query(),

        IpcQuery::V2State => {
            if !is_initialized() {
                return Some(IpcResponse::error("Tiling v2 not initialized"));
            }

            let handle = get_handle()?;

            // Use blocking query for IPC (runs in dedicated thread)
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;

            rt.block_on(async {
                // Get enabled state
                let enabled = handle
                    .query(super::actor::StateQuery::GetEnabled)
                    .await
                    .ok()
                    .and_then(super::actor::QueryResult::into_enabled)
                    .unwrap_or(false);

                // Get focus state
                let focus = handle
                    .query(super::actor::StateQuery::GetFocusState)
                    .await
                    .ok()
                    .and_then(super::actor::QueryResult::into_focus);

                // Get counts
                let screens = handle
                    .query(super::actor::StateQuery::GetAllScreens)
                    .await
                    .ok()
                    .and_then(super::actor::QueryResult::into_screens)
                    .unwrap_or_default();

                let workspaces = handle
                    .query(super::actor::StateQuery::GetAllWorkspaces)
                    .await
                    .ok()
                    .and_then(super::actor::QueryResult::into_workspaces)
                    .unwrap_or_default();

                let windows = handle
                    .query(super::actor::StateQuery::GetAllWindows)
                    .await
                    .ok()
                    .and_then(super::actor::QueryResult::into_windows)
                    .unwrap_or_default();

                let (focused_workspace_id, focused_window_id) =
                    focus.map_or((None, None), |f| (f.focused_workspace_id, f.focused_window_id));

                Some(IpcResponse::success(serde_json::json!({
                    "isEnabled": enabled,
                    "screenCount": screens.len(),
                    "workspaceCount": workspaces.len(),
                    "windowCount": windows.len(),
                    "focusedWorkspaceId": focused_workspace_id.map(|id| id.to_string()),
                    "focusedWindowId": focused_window_id,
                })))
            })
        }

        IpcQuery::V2Screens => {
            if !is_initialized() {
                return Some(IpcResponse::error("Tiling v2 not initialized"));
            }

            let handle = get_handle()?;

            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;

            rt.block_on(async {
                let screens = handle
                    .query(super::actor::StateQuery::GetAllScreens)
                    .await
                    .ok()
                    .and_then(super::actor::QueryResult::into_screens)
                    .unwrap_or_default();

                let screen_infos: Vec<_> = screens
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "id": s.id,
                            "name": s.name,
                            "isMain": s.is_main,
                            "frame": {
                                "x": s.frame.x,
                                "y": s.frame.y,
                                "width": s.frame.width,
                                "height": s.frame.height,
                            },
                            "visibleFrame": {
                                "x": s.visible_frame.x,
                                "y": s.visible_frame.y,
                                "width": s.visible_frame.width,
                                "height": s.visible_frame.height,
                            },
                        })
                    })
                    .collect();

                Some(IpcResponse::success(screen_infos))
            })
        }

        IpcQuery::V2Workspaces => {
            if !is_initialized() {
                return Some(IpcResponse::error("Tiling v2 not initialized"));
            }

            let handle = get_handle()?;

            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;

            rt.block_on(async {
                let workspaces = handle
                    .query(super::actor::StateQuery::GetAllWorkspaces)
                    .await
                    .ok()
                    .and_then(super::actor::QueryResult::into_workspaces)
                    .unwrap_or_default();

                let workspace_infos: Vec<_> = workspaces
                    .iter()
                    .map(|ws| {
                        let layout = ws.layout;
                        serde_json::json!({
                            "id": ws.id.to_string(),
                            "name": ws.name,
                            "screenId": ws.screen_id,
                            "layout": layout.as_str(),
                            "isVisible": ws.is_visible,
                            "isFocused": ws.is_focused,
                            "windowCount": ws.window_ids.len(),
                            "windowIds": ws.window_ids,
                        })
                    })
                    .collect();

                Some(IpcResponse::success(workspace_infos))
            })
        }

        IpcQuery::V2Windows { workspace_id } => {
            if !is_initialized() {
                return Some(IpcResponse::error("Tiling v2 not initialized"));
            }

            let handle = get_handle()?;
            let workspace_filter = workspace_id.clone();

            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;

            rt.block_on(async {
                // Get focus state for marking focused window
                let focus = handle
                    .query(super::actor::StateQuery::GetFocusState)
                    .await
                    .ok()
                    .and_then(super::actor::QueryResult::into_focus);
                let focused_window_id = focus.and_then(|f| f.focused_window_id);

                // Get all windows
                let windows = handle
                    .query(super::actor::StateQuery::GetAllWindows)
                    .await
                    .ok()
                    .and_then(super::actor::QueryResult::into_windows)
                    .unwrap_or_default();

                // Filter by workspace if specified
                let filtered_windows: Vec<_> = windows
                    .iter()
                    .filter(|w| {
                        workspace_filter.as_ref().is_none_or(|id| w.workspace_id.to_string() == *id)
                    })
                    .map(|w| {
                        serde_json::json!({
                            "id": w.id,
                            "pid": w.pid,
                            "appId": w.app_id,
                            "appName": w.app_name,
                            "title": w.title,
                            "workspaceId": w.workspace_id.to_string(),
                            "frame": {
                                "x": w.frame.x,
                                "y": w.frame.y,
                                "width": w.frame.width,
                                "height": w.frame.height,
                            },
                            "isMinimized": w.is_minimized,
                            "isFullscreen": w.is_fullscreen,
                            "isFloating": w.is_floating,
                            "isFocused": focused_window_id == Some(w.id),
                        })
                    })
                    .collect();

                Some(IpcResponse::success(filtered_windows))
            })
        }
    }
}

// ============================================================================
// Standard Query Handlers (v1 API compatibility)
// ============================================================================

/// Handles the standard `screens` query using v2 state.
fn handle_screens_query() -> Option<IpcResponse> {
    if !is_initialized() {
        return Some(IpcResponse::error("Tiling v2 not initialized"));
    }

    let handle = get_handle()?;
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;

    rt.block_on(async {
        let screens = handle
            .query(super::actor::StateQuery::GetAllScreens)
            .await
            .ok()
            .and_then(super::actor::QueryResult::into_screens)
            .unwrap_or_default();

        // Format response to match v1 API format
        let screen_infos: Vec<_> = screens
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "name": s.name,
                    "isMain": s.is_main,
                    "isBuiltin": s.is_builtin,
                    "scaleFactor": s.scale_factor,
                    "frame": {
                        "x": s.frame.x,
                        "y": s.frame.y,
                        "width": s.frame.width,
                        "height": s.frame.height,
                    },
                    "visibleFrame": {
                        "x": s.visible_frame.x,
                        "y": s.visible_frame.y,
                        "width": s.visible_frame.width,
                        "height": s.visible_frame.height,
                    },
                })
            })
            .collect();

        Some(IpcResponse::success(screen_infos))
    })
}

/// Handles the standard `workspaces` query using v2 state.
#[allow(clippy::too_many_lines)]
fn handle_workspaces_query(screen: Option<&str>, focused_screen: bool) -> Option<IpcResponse> {
    if !is_initialized() {
        return Some(IpcResponse::error("Tiling v2 not initialized"));
    }

    let handle = get_handle()?;
    let screen_filter = screen.map(ToString::to_string);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;

    rt.block_on(async {
        // Get workspaces
        let workspaces = handle
            .query(super::actor::StateQuery::GetAllWorkspaces)
            .await
            .ok()
            .and_then(super::actor::QueryResult::into_workspaces)
            .unwrap_or_default();

        // Get focus state for focused_screen filter
        let focus = handle
            .query(super::actor::StateQuery::GetFocusState)
            .await
            .ok()
            .and_then(super::actor::QueryResult::into_focus);

        // Get all screens to map screen_id to screen name
        let screens = handle
            .query(super::actor::StateQuery::GetAllScreens)
            .await
            .ok()
            .and_then(super::actor::QueryResult::into_screens)
            .unwrap_or_default();

        // Determine focused screen ID
        let focused_screen_id = if focused_screen {
            focus.and_then(|f| {
                f.focused_workspace_id.and_then(|ws_id| {
                    workspaces.iter().find(|ws| ws.id == ws_id).map(|ws| ws.screen_id)
                })
            })
        } else {
            None
        };

        // Filter workspaces
        let filtered_workspaces: Vec<_> = workspaces
            .iter()
            .filter(|ws| {
                // Filter by screen name if provided
                if let Some(ref filter) = screen_filter {
                    let screen = screens.iter().find(|s| s.id == ws.screen_id);

                    // Handle special screen names
                    let matches = match filter.as_str() {
                        "main" | "primary" => screen.is_some_and(|s| s.is_main),
                        "secondary" => screen.is_some_and(|s| !s.is_main),
                        _ => screen.is_some_and(|s| s.name == *filter),
                    };

                    if !matches {
                        return false;
                    }
                }
                // Filter by focused screen if requested
                if focused_screen {
                    if let Some(focused_id) = focused_screen_id {
                        return ws.screen_id == focused_id;
                    }
                    return false;
                }
                true
            })
            .map(|ws| {
                let screen_id = ws.screen_id;
                let screen_name = screens
                    .iter()
                    .find(|s| s.id == ws.screen_id)
                    .map_or_else(|| format!("screen-{screen_id}"), |s| s.name.clone());

                let layout = ws.layout;
                serde_json::json!({
                    "id": ws.id.to_string(),
                    "name": ws.name,
                    "screenName": screen_name,
                    "layout": layout.as_str(),
                    "isVisible": ws.is_visible,
                    "isFocused": ws.is_focused,
                    "windowCount": ws.window_ids.len(),
                })
            })
            .collect();

        Some(IpcResponse::success(filtered_workspaces))
    })
}

/// Handles the standard `windows` query using v2 state.
#[allow(clippy::too_many_lines)]
fn handle_windows_query(
    screen: Option<&str>,
    workspace: Option<&str>,
    focused_screen: bool,
    focused_workspace: bool,
) -> Option<IpcResponse> {
    if !is_initialized() {
        return Some(IpcResponse::error("Tiling v2 not initialized"));
    }

    let handle = get_handle()?;
    let screen_filter = screen.map(ToString::to_string);
    let workspace_filter = workspace.map(ToString::to_string);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;

    rt.block_on(async {
        // Get all windows
        let windows = handle
            .query(super::actor::StateQuery::GetAllWindows)
            .await
            .ok()
            .and_then(super::actor::QueryResult::into_windows)
            .unwrap_or_default();

        // Get workspaces
        let workspaces = handle
            .query(super::actor::StateQuery::GetAllWorkspaces)
            .await
            .ok()
            .and_then(super::actor::QueryResult::into_workspaces)
            .unwrap_or_default();

        // Get screens
        let screens = handle
            .query(super::actor::StateQuery::GetAllScreens)
            .await
            .ok()
            .and_then(super::actor::QueryResult::into_screens)
            .unwrap_or_default();

        // Get focus state
        let focus = handle
            .query(super::actor::StateQuery::GetFocusState)
            .await
            .ok()
            .and_then(super::actor::QueryResult::into_focus);

        let focused_window_id = focus.as_ref().and_then(|f| f.focused_window_id);
        let focused_workspace_id = focus.as_ref().and_then(|f| f.focused_workspace_id);

        // Determine focused screen ID
        let focused_screen_id = focused_workspace_id
            .and_then(|ws_id| workspaces.iter().find(|ws| ws.id == ws_id).map(|ws| ws.screen_id));

        // Filter windows
        let filtered_windows: Vec<_> = windows
            .iter()
            .filter(|w| {
                // Get window's workspace
                let ws = workspaces.iter().find(|ws| ws.id == w.workspace_id);

                // Filter by workspace name
                if let Some(ref filter) = workspace_filter {
                    if let Some(ws) = ws {
                        if ws.name != *filter {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }

                // Filter by focused workspace
                if focused_workspace {
                    if let Some(focused_id) = focused_workspace_id {
                        if w.workspace_id != focused_id {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }

                // Filter by screen name
                if let Some(ref filter) = screen_filter {
                    if let Some(ws) = ws {
                        let screen = screens.iter().find(|s| s.id == ws.screen_id);

                        // Handle special screen names
                        let matches = match filter.as_str() {
                            "main" | "primary" => screen.is_some_and(|s| s.is_main),
                            "secondary" => screen.is_some_and(|s| !s.is_main),
                            _ => screen.is_some_and(|s| s.name == *filter),
                        };

                        if !matches {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }

                // Filter by focused screen
                if focused_screen
                    && let Some(ws) = ws
                    && let Some(focused_id) = focused_screen_id
                {
                    return ws.screen_id == focused_id;
                } else if focused_screen {
                    return false;
                }

                true
            })
            .map(|w| {
                let workspace_name = workspaces
                    .iter()
                    .find(|ws| ws.id == w.workspace_id)
                    .map(|ws| ws.name.clone())
                    .unwrap_or_default();

                serde_json::json!({
                    "id": w.id,
                    "pid": w.pid,
                    "appId": w.app_id,
                    "appName": w.app_name,
                    "title": w.title,
                    "workspace": workspace_name,
                    "frame": {
                        "x": w.frame.x,
                        "y": w.frame.y,
                        "width": w.frame.width,
                        "height": w.frame.height,
                    },
                    "isMinimized": w.is_minimized,
                    "isFullscreen": w.is_fullscreen,
                    "isFloating": w.is_floating,
                    "isFocused": focused_window_id == Some(w.id),
                })
            })
            .collect();

        Some(IpcResponse::success(filtered_windows))
    })
}

/// Handles the `apps` query - returns all running applications (excluding ignored apps).
#[allow(clippy::unnecessary_wraps)] // Matches other handler signatures
fn handle_apps_query() -> Option<IpcResponse> {
    use super::rules::should_tile_window;
    use super::window::get_running_apps;

    // Get all running apps
    let apps = get_running_apps();

    // Filter out apps that match ignore rules and format response
    let app_infos: Vec<_> = apps
        .iter()
        .filter(|app| should_tile_window(&app.bundle_id, &app.name))
        .map(|app| {
            serde_json::json!({
                "pid": app.pid,
                "name": app.name,
                "bundleId": app.bundle_id,
                "isHidden": app.is_hidden,
            })
        })
        .collect();

    Some(IpcResponse::success(app_infos))
}

// ============================================================================
// Mouse Up Callback (Drag Completion)
// ============================================================================

/// Called when the mouse button is released after a drag/resize operation.
///
/// This is registered as a callback with the mouse monitor and runs on
/// the mouse monitor thread (not the async runtime).
fn on_mouse_up() {
    use super::events::drag_state::{self, DragOperation};

    // Finish any ongoing operation
    let Some(info) = drag_state::finish_operation() else {
        return;
    };

    // Get the actor handle to send messages
    let Some(handle) = get_handle() else {
        return;
    };

    // Process the completed operation
    match info.operation {
        DragOperation::Move => handle_move_finished(&info, handle),
        DragOperation::Resize => handle_resize_finished(&info, handle),
    }
}

/// Handles the completion of a move operation.
///
/// For tiled windows:
/// - If dropped on another tiled window, swap them
/// - Otherwise, reapply the layout to snap back to position
///
/// For floating windows: leave them where they are.
fn handle_move_finished(info: &super::events::drag_state::DragInfo, handle: &StateActorHandle) {
    if !info.has_tiled_windows() {
        // All floating windows - nothing to snap back
        return;
    }

    // Get current window frames by querying the AX system directly
    let current_frames = get_current_frames_for_snapshots(&info.window_snapshots);

    // Check if a window was dragged onto another window for swapping
    if let Some((dragged_id, target_id)) =
        find_drag_swap_target(&info.window_snapshots, &current_frames)
    {
        // Send swap command
        let _ = handle.send(StateMessage::SwapWindows {
            window_id_a: dragged_id,
            window_id_b: target_id,
        });
        return;
    }

    // No swap target - send message to actor to trigger layout refresh
    // (going through actor ensures reliable processing from the mouse monitor thread)
    let _ = handle.send(StateMessage::UserMoveCompleted {
        workspace_id: info.workspace_id,
    });
}

/// Handles the completion of a resize operation.
///
/// For tiled windows: calculate new split ratios based on how the windows were resized.
/// For floating windows: just accept their new positions.
fn handle_resize_finished(info: &super::events::drag_state::DragInfo, handle: &StateActorHandle) {
    if !info.has_tiled_windows() {
        // All floating windows - nothing to do
        return;
    }

    // Get current window frames by querying the AX system directly
    let current_frames = get_current_frames_for_snapshots(&info.window_snapshots);

    // Find which window was resized
    let resized_info = find_resized_window(&info.window_snapshots, &current_frames);

    if let Some((window_id, old_frame, new_frame)) = resized_info {
        // Send the resize completion message with window info
        let _ = handle.send(StateMessage::UserResizeCompleted {
            workspace_id: info.workspace_id,
            window_id,
            old_frame,
            new_frame,
        });
    } else {
        // No significant resize detected - send message to actor to trigger layout refresh
        let _ = handle.send(StateMessage::UserMoveCompleted {
            workspace_id: info.workspace_id,
        });
    }
}

/// Gets current window frames by querying the AX system for each window in the snapshots.
///
/// This is used during mouse-up handling when we need current positions.
fn get_current_frames_for_snapshots(
    snapshots: &[super::events::drag_state::WindowSnapshot],
) -> Vec<(u32, super::state::Rect)> {
    use super::effects::window_ops;

    let mut frames = Vec::with_capacity(snapshots.len());

    for snapshot in snapshots {
        if let Some(frame) = window_ops::get_window_frame(snapshot.window_id) {
            frames.push((snapshot.window_id, frame));
        }
    }

    frames
}

/// Finds if a dragged window should be swapped with another window.
///
/// Returns `Some((dragged_id, target_id))` if a swap should occur.
fn find_drag_swap_target(
    snapshots: &[super::events::drag_state::WindowSnapshot],
    current_frames: &[(u32, super::state::Rect)],
) -> Option<(u32, u32)> {
    const MIN_DRAG_DISTANCE: f64 = 50.0;

    // Find which window was dragged (moved significantly from original position)
    let mut dragged: Option<(u32, super::state::Rect)> = None;
    let mut max_distance = 0.0f64;

    for snapshot in snapshots {
        if snapshot.is_floating {
            continue;
        }

        let Some((_, current_frame)) =
            current_frames.iter().find(|(id, _)| *id == snapshot.window_id)
        else {
            continue;
        };

        let orig_center_x = snapshot.original_frame.x + snapshot.original_frame.width / 2.0;
        let orig_center_y = snapshot.original_frame.y + snapshot.original_frame.height / 2.0;
        let curr_center_x = current_frame.x + current_frame.width / 2.0;
        let curr_center_y = current_frame.y + current_frame.height / 2.0;

        let dx = curr_center_x - orig_center_x;
        let dy = curr_center_y - orig_center_y;
        let distance = dx.hypot(dy);

        if distance > max_distance && distance > MIN_DRAG_DISTANCE {
            max_distance = distance;
            dragged = Some((snapshot.window_id, *current_frame));
        }
    }

    let (dragged_id, dragged_frame) = dragged?;

    let dragged_center_x = dragged_frame.x + dragged_frame.width / 2.0;
    let dragged_center_y = dragged_frame.y + dragged_frame.height / 2.0;

    for snapshot in snapshots {
        if snapshot.window_id == dragged_id || snapshot.is_floating {
            continue;
        }

        let orig = &snapshot.original_frame;

        if dragged_center_x >= orig.x
            && dragged_center_x <= orig.x + orig.width
            && dragged_center_y >= orig.y
            && dragged_center_y <= orig.y + orig.height
        {
            return Some((dragged_id, snapshot.window_id));
        }
    }

    None
}

/// Finds which window was resized by comparing snapshots to current frames.
fn find_resized_window(
    snapshots: &[super::events::drag_state::WindowSnapshot],
    current_frames: &[(u32, super::state::Rect)],
) -> Option<(u32, super::state::Rect, super::state::Rect)> {
    let mut max_diff = 0.0f64;
    let mut resized: Option<(u32, super::state::Rect, super::state::Rect)> = None;

    for snapshot in snapshots {
        if snapshot.is_floating {
            continue;
        }

        let Some((_, current_frame)) =
            current_frames.iter().find(|(id, _)| *id == snapshot.window_id)
        else {
            continue;
        };

        let width_diff = (current_frame.width - snapshot.original_frame.width).abs();
        let height_diff = (current_frame.height - snapshot.original_frame.height).abs();
        let size_diff = width_diff + height_diff;

        if size_diff > max_diff {
            max_diff = size_diff;
            resized = Some((snapshot.window_id, snapshot.original_frame, *current_frame));
        }
    }

    // Only return if there was a significant change (more than 5 pixels)
    if max_diff > 5.0 { resized } else { None }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_initialized_default() {
        // In a fresh test environment, this just verifies the function doesn't panic
        let _ = is_initialized();
    }

    #[test]
    fn test_is_enabled_reads_config() {
        // This just verifies the function doesn't panic
        let _ = is_enabled();
    }

    #[test]
    fn test_get_app_handle_without_store() {
        // Without storing, should return None
        // Note: this may be affected by other tests
        let _ = get_app_handle();
    }
}
