//! State actor module.
//!
//! The state actor owns all tiling state and processes messages sequentially.
//! This ensures thread-safe state management without complex locking.
//!
//! # Panic Recovery
//!
//! The actor implements panic recovery to maintain system stability. If a
//! message handler panics:
//! 1. The panic is caught and logged
//! 2. The actor continues processing subsequent messages
//! 3. State may be partially inconsistent but the system remains operational
//!
//! This prevents a single bad window event or command from taking down the
//! entire tiling system.

mod handle;
pub mod handlers;
mod messages;
mod minimum_size;

use std::panic::{AssertUnwindSafe, catch_unwind};

pub use handle::{ActorError, StateActorHandle};
pub use messages::{
    CycleDirection, FocusDirection, GeometryUpdate, GeometryUpdateType, QueryResult, StateMessage,
    StateQuery, WindowCreatedInfo,
};
use tokio::sync::mpsc;

use crate::config::get_config;
use crate::modules::tiling::init::get_subscriber_handle;
use crate::modules::tiling::layout::{Gaps, MasterPosition, calculate_layout_full};
use crate::modules::tiling::state::{LayoutType, Rect, TilingState};

/// Channel buffer size for the state actor.
///
/// Increased from 256 to 1024 to handle burst events (e.g., multiple windows
/// moving during layout changes) without backpressure.
const CHANNEL_BUFFER_SIZE: usize = 1024;

/// The state actor that owns all tiling state.
///
/// Messages are processed sequentially, ensuring consistent state updates.
/// The actor runs in its own tokio task and communicates via channels.
pub struct StateActor {
    /// The tiling state owned by this actor.
    state: TilingState,

    /// Receiver for incoming messages.
    receiver: mpsc::Receiver<StateMessage>,
}

impl StateActor {
    /// Spawn a new state actor and return a handle for communication.
    ///
    /// The actor will run in the background and process messages.
    #[must_use]
    pub fn spawn() -> StateActorHandle {
        log::debug!("tiling: spawning state actor");
        let (sender, receiver) = mpsc::channel(CHANNEL_BUFFER_SIZE);

        let actor = Self {
            state: TilingState::new(),
            receiver,
        };

        // Spawn the actor task using Tauri's async runtime
        // This works during app setup when tokio runtime isn't directly available
        tauri::async_runtime::spawn(async move {
            actor.run().await;
        });

        StateActorHandle::new(sender)
    }

    /// Run the actor's message loop.
    ///
    /// This loop includes panic recovery - if a message handler panics,
    /// the error is logged and the actor continues processing messages.
    async fn run(mut self) {
        log::trace!("tiling: actor message loop starting");

        while let Some(msg) = self.receiver.recv().await {
            if matches!(msg, StateMessage::Shutdown) {
                log::debug!("State actor received shutdown message");
                return;
            }

            // Wrap message handling in catch_unwind for panic recovery
            // This ensures a single bad event doesn't take down the entire tiling system
            let msg_name = msg.name();
            let result = catch_unwind(AssertUnwindSafe(|| {
                self.handle_message(msg);
            }));

            if let Err(panic_info) = result {
                // Extract panic message if possible
                let panic_msg = panic_info
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| panic_info.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".to_string());

                log::error!("tiling: PANIC in actor while handling '{msg_name}': {panic_msg}");
                log::error!(
                    "tiling: Actor recovered from panic - state may be inconsistent. \
                     Consider restarting the application if issues persist."
                );
            }
        }

        log::debug!("State actor channel closed, exiting");
    }

    /// Handle a single message.
    #[allow(clippy::too_many_lines)]
    fn handle_message(&mut self, msg: StateMessage) {
        match msg {
            // Window events - delegated to handlers
            StateMessage::WindowCreated(info) => {
                handlers::on_window_created(&mut self.state, info);
            }
            StateMessage::WindowDestroyed { window_id } => {
                log::debug!(
                    "tiling: actor received WindowDestroyed message for window_id={window_id}"
                );
                if let Some(ws_id) = handlers::on_window_destroyed(&mut self.state, window_id) {
                    log::debug!(
                        "tiling: window {window_id} was in workspace {ws_id}, notifying subscriber"
                    );
                    // Notify subscriber to recompute layout for the affected workspace
                    if let Some(handle) = get_subscriber_handle() {
                        log::debug!(
                            "tiling: sending layout_changed notification to subscriber for workspace {ws_id}"
                        );
                        handle.notify_layout_changed(ws_id, false);
                    } else {
                        log::warn!(
                            "tiling: no subscriber handle available to notify layout change!"
                        );
                    }
                } else {
                    log::debug!(
                        "tiling: window {window_id} was not tracked or workspace not found"
                    );
                }
            }
            StateMessage::WindowFocused { window_id } => {
                handlers::on_window_focused(&mut self.state, window_id);
            }
            StateMessage::WindowUnfocused { window_id } => {
                handlers::on_window_unfocused(&mut self.state, window_id);
            }
            StateMessage::WindowMoved { window_id, frame } => {
                handlers::on_window_moved(&mut self.state, window_id, frame);
            }
            StateMessage::WindowResized { window_id, frame } => {
                handlers::on_window_resized(&mut self.state, window_id, frame);
            }
            StateMessage::WindowMinimized { window_id, minimized } => {
                handlers::on_window_minimized(&mut self.state, window_id, minimized);
            }
            StateMessage::WindowTitleChanged { window_id, title } => {
                handlers::on_window_title_changed(&mut self.state, window_id, &title);
            }
            StateMessage::WindowFullscreenChanged { window_id, fullscreen } => {
                handlers::on_window_fullscreen_changed(&mut self.state, window_id, fullscreen);
            }

            // App events - delegated to handlers
            StateMessage::AppLaunched { pid, bundle_id, name } => {
                handlers::on_app_launched(&mut self.state, pid, &bundle_id, &name);
            }
            StateMessage::AppTerminated { pid } => {
                handlers::on_app_terminated(&mut self.state, pid);
            }
            StateMessage::AppHidden { pid } => {
                handlers::on_app_hidden(&mut self.state, pid);
            }
            StateMessage::AppShown { pid } => {
                handlers::on_app_shown(&mut self.state, pid);
            }
            StateMessage::AppActivated { pid } => {
                handlers::on_app_activated(&mut self.state, pid);
            }

            // Screen events - delegated to handlers
            StateMessage::ScreensChanged => {
                handlers::on_screens_changed(&mut self.state);
            }
            StateMessage::SetScreens { screens } => {
                log::trace!("tiling: received SetScreens with {} screens", screens.len());
                handlers::on_set_screens(&mut self.state, screens);
            }

            // User commands (stubs for Phase 4+)
            StateMessage::SwitchWorkspace { name } => self.on_switch_workspace(&name),
            StateMessage::CycleWorkspace { direction } => self.on_cycle_workspace(direction),
            StateMessage::SetLayout { workspace_id, layout } => {
                self.on_set_layout(workspace_id, layout);
            }
            StateMessage::CycleLayout { workspace_id } => self.on_cycle_layout(workspace_id),
            StateMessage::MoveWindowToWorkspace { window_id, workspace_id } => {
                self.on_move_window_to_workspace(window_id, workspace_id);
            }
            StateMessage::SwapWindows { window_id_a, window_id_b } => {
                self.on_swap_windows(window_id_a, window_id_b);
            }
            StateMessage::CycleFocus { direction } => self.on_cycle_focus(direction),
            StateMessage::FocusWindow { direction } => self.on_focus_window(direction),
            StateMessage::SwapWindowInDirection { direction } => {
                self.on_swap_window_in_direction(direction);
            }
            StateMessage::ToggleFloating { window_id } => self.on_toggle_floating(window_id),
            StateMessage::ResizeSplit {
                workspace_id,
                window_index,
                delta,
            } => self.on_resize_split(workspace_id, window_index, delta),
            StateMessage::BalanceWorkspace { workspace_id } => {
                self.on_balance_workspace(workspace_id);
            }
            StateMessage::SendWindowToScreen { target_screen } => {
                self.on_send_window_to_screen(&target_screen);
            }
            StateMessage::SendWorkspaceToScreen { target_screen } => {
                self.on_send_workspace_to_screen(&target_screen);
            }
            StateMessage::ResizeFocusedWindow { dimension, amount } => {
                self.on_resize_focused_window(dimension, amount);
            }
            StateMessage::ApplyPreset { preset } => {
                self.on_apply_preset(&preset);
            }
            StateMessage::SetEnabled { enabled } => self.on_set_enabled(enabled),

            // Queries
            StateMessage::Query { query, respond_to } => {
                let result = self.execute_query(query);
                if respond_to.send(result).is_err() {
                    log::warn!("tiling: failed to send query response (channel closed)");
                }
            }

            // Batched geometry - delegated to handlers
            StateMessage::BatchedGeometryUpdates(updates) => {
                handlers::on_batched_geometry_updates(&mut self.state, &updates);
            }

            // User-initiated resize completed
            StateMessage::UserResizeCompleted {
                workspace_id,
                window_id,
                old_frame,
                new_frame,
            } => {
                self.on_user_resize_completed(workspace_id, window_id, old_frame, new_frame);
            }

            // User-initiated move completed (no swap) - snap back to layout
            StateMessage::UserMoveCompleted { workspace_id } => {
                if let Some(handle) = get_subscriber_handle() {
                    handle.notify_layout_changed(workspace_id, true);
                }
            }

            // Batch window creation during initialization (no layout notifications)
            StateMessage::BatchWindowsCreated(windows) => {
                self.on_batch_windows_created(windows);
            }

            // Initialization complete - apply layouts
            StateMessage::InitComplete => {
                self.on_init_complete();
            }

            // Update expected frames for minimum size detection
            StateMessage::SetExpectedFrames { frames } => {
                self.on_set_expected_frames(frames);
            }

            // Shutdown handled in run()
            StateMessage::Shutdown => unreachable!(),
        }
    }

    /// Update expected frames for all windows in the list.
    ///
    /// This also updates the window's actual `frame` field so that directional
    /// operations (like swap, focus) use the correct positions immediately,
    /// even while animations are in progress.
    fn on_set_expected_frames(&mut self, frames: Vec<(u32, Rect)>) {
        for (window_id, frame) in frames {
            self.state.update_window(window_id, |w| {
                w.frame = frame;
                w.expected_frame = Some(frame);
            });
        }
    }

    // ========================================================================
    // Query Execution
    // ========================================================================

    fn execute_query(&self, query: StateQuery) -> QueryResult {
        match query {
            StateQuery::GetAllScreens => {
                QueryResult::Screens(self.state.screens.iter().cloned().collect())
            }
            StateQuery::GetAllWorkspaces => {
                QueryResult::Workspaces(self.state.workspaces.iter().cloned().collect())
            }
            StateQuery::GetAllWindows => {
                QueryResult::Windows(self.state.windows.iter().cloned().collect())
            }
            StateQuery::GetFocusState => {
                QueryResult::Focus(eyeball::Observable::get(&self.state.focus).clone())
            }
            StateQuery::GetEnabled => QueryResult::Enabled(self.state.is_enabled()),

            StateQuery::GetScreen { id } => QueryResult::Screen(self.state.get_screen(id)),
            StateQuery::GetWorkspace { id } => QueryResult::Workspace(self.state.get_workspace(id)),
            StateQuery::GetWorkspaceByName { name } => {
                QueryResult::Workspace(self.state.get_workspace_by_name(&name))
            }
            StateQuery::GetWindow { id } => QueryResult::Window(self.state.get_window(id)),

            StateQuery::GetWindowsForWorkspace { workspace_id } => {
                QueryResult::Windows(self.state.get_windows_for_workspace(workspace_id))
            }
            StateQuery::GetWindowsForPid { pid } => {
                let window_ids: Vec<u32> =
                    self.state.windows.iter().filter(|w| w.pid == pid).map(|w| w.id).collect();
                QueryResult::WindowIds(window_ids)
            }
            StateQuery::GetWorkspacesForScreen { screen_id } => {
                QueryResult::Workspaces(self.state.get_workspaces_for_screen(screen_id))
            }
            StateQuery::GetVisibleWorkspaces => {
                QueryResult::Workspaces(self.state.get_visible_workspaces())
            }
            StateQuery::GetFocusedWorkspace => {
                QueryResult::Workspace(self.state.get_focused_workspace())
            }
            StateQuery::GetFocusedWindow => QueryResult::Window(self.state.get_focused_window()),
            StateQuery::GetLayoutableWindows { workspace_id } => {
                QueryResult::Windows(self.state.get_layoutable_windows(workspace_id))
            }

            StateQuery::GetTabGroup { tab_group_id } => {
                QueryResult::Windows(self.state.get_windows_in_tab_group(tab_group_id))
            }

            StateQuery::GetWindowLayout { workspace_id } => {
                QueryResult::Layout(self.compute_layout(workspace_id))
            }

            // ════════════════════════════════════════════════════════════════════════
            // ID-Only Queries (zero-clone, for hot paths)
            // ════════════════════════════════════════════════════════════════════════
            StateQuery::GetAllScreenIds => QueryResult::ScreenIds(self.state.get_all_screen_ids()),
            StateQuery::GetAllWorkspaceIds => {
                QueryResult::WorkspaceIds(self.state.get_all_workspace_ids())
            }
            StateQuery::GetAllWindowIds => QueryResult::WindowIds(self.state.get_all_window_ids()),
            StateQuery::GetWindowIdsForWorkspace { workspace_id } => {
                QueryResult::WindowIds(self.state.get_window_ids_for_workspace(workspace_id))
            }
            StateQuery::GetLayoutableWindowIds { workspace_id } => {
                QueryResult::WindowIds(self.state.get_layoutable_window_ids(workspace_id))
            }
            StateQuery::GetVisibleWorkspaceIds => {
                QueryResult::WorkspaceIds(self.state.get_visible_workspace_ids())
            }
            StateQuery::HasWindow { id } => QueryResult::Exists(self.state.has_window(id)),
            StateQuery::HasWorkspace { id } => QueryResult::Exists(self.state.has_workspace(id)),
            StateQuery::HasScreen { id } => QueryResult::Exists(self.state.has_screen(id)),
        }
    }

    // ========================================================================
    // Layout Computation
    // ========================================================================

    /// Compute the layout for a workspace.
    ///
    /// Returns a vector of (`window_id`, `frame`) pairs for all layoutable windows.
    /// Enforces minimum window sizes by adjusting split ratios when necessary.
    fn compute_layout(
        &self,
        workspace_id: uuid::Uuid,
    ) -> Vec<(u32, crate::modules::tiling::state::Rect)> {
        // Get workspace
        let Some(workspace) = self.state.get_workspace(workspace_id) else {
            log::warn!("compute_layout: workspace {workspace_id} not found");
            return Vec::new();
        };

        // Get screen for this workspace
        let Some(screen) = self.state.get_screen(workspace.screen_id) else {
            log::warn!(
                "compute_layout: screen {} not found for workspace {}",
                workspace.screen_id,
                workspace_id
            );
            return Vec::new();
        };

        // Get layoutable windows (filter out minimized, hidden, fullscreen, floating, inactive tabs)
        let layoutable_windows = self.state.get_layoutable_windows(workspace_id);
        if layoutable_windows.is_empty() {
            return Vec::new();
        }

        // Extract window IDs in stack order
        let window_ids: Vec<u32> = workspace
            .window_ids
            .iter()
            .filter(|id| layoutable_windows.iter().any(|w| w.id == **id))
            .copied()
            .collect();

        if window_ids.is_empty() {
            return Vec::new();
        }

        // Get gaps from config with bar offset for main screen
        let config = get_config();
        let bar_offset = if config.bar.is_enabled() {
            f64::from(config.bar.height) + f64::from(config.bar.padding)
        } else {
            0.0
        };
        let gaps = Gaps::from_config(&config.tiling.gaps, &screen.name, screen.is_main, bar_offset);

        // Get master ratio from config (default 0.5)
        let master_ratio = f64::from(config.tiling.master.ratio) / 100.0;

        // Get split ratios from workspace (may be adjusted for minimum sizes)
        let split_ratios = workspace.split_ratios.clone();

        // Compute initial layout
        let result = calculate_layout_full(
            workspace.layout,
            &window_ids,
            &screen.visible_frame,
            master_ratio,
            &gaps,
            &split_ratios,
            MasterPosition::Auto,
        );

        // Enforce minimum sizes by adjusting ratios if needed
        let adjusted_result = match workspace.layout {
            LayoutType::Split | LayoutType::SplitHorizontal | LayoutType::SplitVertical => {
                minimum_size::enforce_minimum_sizes_for_split(
                    &result,
                    &layoutable_windows,
                    &window_ids,
                    &screen.visible_frame,
                    &gaps,
                    workspace.layout,
                    &split_ratios,
                )
            }
            LayoutType::Dwindle => minimum_size::enforce_minimum_sizes_for_dwindle(
                &result,
                &layoutable_windows,
                &window_ids,
                &screen.visible_frame,
                &gaps,
                &split_ratios,
            ),
            LayoutType::Grid => minimum_size::enforce_minimum_sizes_for_grid(
                &result,
                &layoutable_windows,
                &window_ids,
                &screen.visible_frame,
                &gaps,
                &split_ratios,
            ),
            // Floating/Monocle/Master don't need minimum size enforcement
            _ => None,
        };

        if let Some(adjusted) = adjusted_result {
            return adjusted.into_vec();
        }

        // Convert SmallVec to Vec for the query result
        result.into_vec()
    }

    // ========================================================================
    // Command Handlers - Delegate to handlers module
    // ========================================================================

    fn on_switch_workspace(&mut self, name: &str) {
        handlers::on_switch_workspace(&mut self.state, name);
    }

    fn on_cycle_workspace(&mut self, direction: CycleDirection) {
        handlers::on_cycle_workspace(&mut self.state, direction);
    }

    fn on_set_layout(
        &mut self,
        workspace_id: uuid::Uuid,
        layout: crate::modules::tiling::state::LayoutType,
    ) {
        handlers::on_set_layout(&mut self.state, workspace_id, layout);
    }

    fn on_cycle_layout(&mut self, workspace_id: uuid::Uuid) {
        handlers::on_cycle_layout(&mut self.state, workspace_id);
    }

    fn on_move_window_to_workspace(&mut self, window_id: u32, workspace_id: uuid::Uuid) {
        handlers::on_move_window_to_workspace(&mut self.state, window_id, workspace_id);
    }

    fn on_swap_windows(&mut self, window_id_a: u32, window_id_b: u32) {
        handlers::on_swap_windows(&mut self.state, window_id_a, window_id_b);
    }

    fn on_cycle_focus(&mut self, direction: CycleDirection) {
        handlers::on_cycle_focus(&mut self.state, direction);
    }

    fn on_focus_window(&mut self, direction: FocusDirection) {
        handlers::on_focus_window(&mut self.state, direction);
    }

    fn on_swap_window_in_direction(&mut self, direction: FocusDirection) {
        handlers::on_swap_window_in_direction(&mut self.state, direction);
    }

    fn on_toggle_floating(&mut self, window_id: u32) {
        handlers::on_toggle_floating(&mut self.state, window_id);
    }

    fn on_resize_split(&mut self, workspace_id: uuid::Uuid, window_index: usize, delta: f64) {
        handlers::on_resize_split(&mut self.state, workspace_id, window_index, delta);
    }

    fn on_balance_workspace(&mut self, workspace_id: uuid::Uuid) {
        handlers::on_balance_workspace(&mut self.state, workspace_id);
    }

    fn on_send_window_to_screen(&mut self, target_screen: &messages::TargetScreen) {
        handlers::on_send_window_to_screen(&mut self.state, target_screen);
    }

    fn on_send_workspace_to_screen(&mut self, target_screen: &messages::TargetScreen) {
        handlers::on_send_workspace_to_screen(&mut self.state, target_screen);
    }

    fn on_resize_focused_window(&mut self, dimension: messages::ResizeDimension, amount: i32) {
        handlers::on_resize_focused_window(&mut self.state, dimension, amount);
    }

    fn on_apply_preset(&mut self, preset_name: &str) {
        handlers::on_apply_preset(&mut self.state, preset_name);
    }

    fn on_set_enabled(&mut self, enabled: bool) {
        log::debug!("Set enabled: {enabled}");
        self.state.set_enabled(enabled);
    }

    fn on_user_resize_completed(
        &mut self,
        workspace_id: uuid::Uuid,
        window_id: u32,
        old_frame: Rect,
        new_frame: Rect,
    ) {
        handlers::on_user_resize_completed(
            &mut self.state,
            workspace_id,
            window_id,
            old_frame,
            new_frame,
        );
    }

    // ========================================================================
    // Initialization Handlers
    // ========================================================================

    /// Handles batch window creation during initialization.
    ///
    /// Creates window entries without triggering individual layout notifications.
    /// Call `on_init_complete` after all windows are tracked to apply layouts.
    fn on_batch_windows_created(&mut self, windows: Vec<WindowCreatedInfo>) {
        log::debug!(
            "Batch creating {} windows (no layout notifications)",
            windows.len()
        );

        for info in windows {
            // Use the window handler but it won't notify subscriber during init
            // because subscriber handle won't be stored yet
            handlers::on_window_created_silent(&mut self.state, info);
        }
    }

    /// Handles initialization complete.
    ///
    /// Triggers layout calculation for all visible workspaces and hides
    /// windows from non-visible workspaces.
    fn on_init_complete(&self) {
        log::debug!("Initialization complete, applying initial layouts");

        // Sync window visibility based on workspace visibility
        self.sync_window_visibility();

        // Get all visible workspaces and trigger layout for each
        let visible_workspace_ids: Vec<uuid::Uuid> =
            self.state.get_visible_workspaces().iter().map(|ws| ws.id).collect();

        // Notify subscriber for each visible workspace
        if let Some(handle) = crate::modules::tiling::init::get_subscriber_handle() {
            for ws_id in visible_workspace_ids {
                handle.notify_layout_changed(ws_id, false);
            }
        }

        log::debug!("Initial layout notifications sent");
    }

    /// Syncs window visibility based on workspace visibility.
    ///
    /// - Shows (unhides) apps that have windows in visible workspaces
    /// - Hides apps that have windows ONLY in non-visible workspaces
    fn sync_window_visibility(&self) {
        use std::collections::HashSet;

        use crate::modules::tiling::effects::window_ops::{hide_app, unhide_app};

        // Collect visible workspace IDs
        let visible_ws_ids: HashSet<uuid::Uuid> =
            self.state.get_visible_workspaces().iter().map(|ws| ws.id).collect();

        // Collect PIDs for windows in visible vs non-visible workspaces
        let mut pids_in_visible: HashSet<i32> = HashSet::new();
        let mut pids_in_non_visible: HashSet<i32> = HashSet::new();

        for window in self.state.windows.iter() {
            if visible_ws_ids.contains(&window.workspace_id) {
                pids_in_visible.insert(window.pid);
            } else {
                pids_in_non_visible.insert(window.pid);
            }
        }

        // PIDs that should be hidden: only in non-visible workspaces
        let pids_to_hide: Vec<i32> =
            pids_in_non_visible.difference(&pids_in_visible).copied().collect();

        // Unhide apps with windows in visible workspaces (in case they were hidden before)
        for pid in &pids_in_visible {
            let _ = unhide_app(*pid);
        }

        // Hide apps with windows only in non-visible workspaces
        for pid in &pids_to_hide {
            let _ = hide_app(*pid);
        }

        log::debug!(
            "Synced window visibility: {} apps shown, {} apps hidden",
            pids_in_visible.len(),
            pids_to_hide.len()
        );
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_actor_spawn_and_shutdown() {
        let handle = StateActor::spawn();
        assert!(handle.is_alive());

        // Send shutdown
        handle.shutdown().unwrap();

        // Give actor time to process
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn test_actor_query_enabled() {
        let handle = StateActor::spawn();

        let result = handle.get_enabled().await.unwrap();
        assert_eq!(result.into_enabled(), Some(true));

        handle.set_enabled(false).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let result = handle.get_enabled().await.unwrap();
        assert_eq!(result.into_enabled(), Some(false));

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_actor_query_empty_state() {
        let handle = StateActor::spawn();

        let result = handle.get_all_screens().await.unwrap();
        assert_eq!(result.into_screens().unwrap().len(), 0);

        let result = handle.get_all_workspaces().await.unwrap();
        assert_eq!(result.into_workspaces().unwrap().len(), 0);

        let result = handle.get_all_windows().await.unwrap();
        assert_eq!(result.into_windows().unwrap().len(), 0);

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_actor_focus_state() {
        let handle = StateActor::spawn();

        let result = handle.get_focus_state().await.unwrap();
        let focus = result.into_focus().unwrap();
        assert!(!focus.has_focus());

        handle.shutdown().unwrap();
    }
}
