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

use std::panic::{AssertUnwindSafe, catch_unwind};

pub use handle::{ActorError, StateActorHandle};
pub use messages::{
    CycleDirection, FocusDirection, GeometryUpdate, GeometryUpdateType, QueryResult, StateMessage,
    StateQuery, WindowCreatedInfo,
};
use tokio::sync::mpsc;

use crate::config::get_config;
use crate::modules::tiling::init::get_subscriber_handle;
use crate::modules::tiling::layout::{Gaps, LayoutResult, MasterPosition, calculate_layout_full};
use crate::modules::tiling::state::{LayoutType, Rect, TilingState, Window};

/// Channel buffer size for the state actor.
const CHANNEL_BUFFER_SIZE: usize = 256;

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
    fn on_set_expected_frames(&mut self, frames: Vec<(u32, Rect)>) {
        for (window_id, frame) in frames {
            self.state.update_window(window_id, |w| {
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
                enforce_minimum_sizes_for_split(
                    &result,
                    &layoutable_windows,
                    &window_ids,
                    &screen.visible_frame,
                    &gaps,
                    workspace.layout,
                    &split_ratios,
                )
            }
            LayoutType::Dwindle => enforce_minimum_sizes_for_dwindle(
                &result,
                &layoutable_windows,
                &window_ids,
                &screen.visible_frame,
                &gaps,
                &split_ratios,
            ),
            LayoutType::Grid => enforce_minimum_sizes_for_grid(
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
// Minimum Size Enforcement
// ============================================================================

/// Enforces minimum window sizes for split layouts by adjusting ratios.
///
/// If any window would be smaller than its minimum size, this function:
/// 1. Calculates the minimum ratio each window needs
/// 2. Adjusts the split ratios to accommodate minimums
/// 3. Returns a new layout with adjusted positions
///
/// Returns `None` if no adjustments are needed.
fn enforce_minimum_sizes_for_split(
    initial_result: &LayoutResult,
    layoutable_windows: &[Window],
    window_ids: &[u32],
    screen_frame: &Rect,
    gaps: &Gaps,
    layout: LayoutType,
    current_ratios: &[f64],
) -> Option<LayoutResult> {
    if window_ids.len() < 2 {
        return None; // Single window always gets full space
    }

    // Determine if horizontal or vertical split
    let is_horizontal = matches!(
        layout,
        LayoutType::SplitHorizontal
            | LayoutType::Split if screen_frame.width >= screen_frame.height
    );

    // Get usable dimension (accounting for outer gaps)
    let usable_frame = gaps.apply_outer(screen_frame);
    let total_dimension = if is_horizontal {
        usable_frame.width
    } else {
        usable_frame.height
    };

    // Account for inner gaps between windows
    let inner_gap = if is_horizontal {
        gaps.inner_h
    } else {
        gaps.inner_v
    };
    #[allow(clippy::cast_precision_loss)]
    let total_gaps = inner_gap * (window_ids.len() - 1) as f64;
    let available_space = total_dimension - total_gaps;

    if available_space <= 0.0 {
        return None; // Can't layout if no space
    }

    // Build minimum size map (using effective_minimum_size to include inferred minimums)
    let min_sizes: Vec<f64> = window_ids
        .iter()
        .map(|&id| {
            layoutable_windows
                .iter()
                .find(|w| w.id == id)
                .and_then(super::state::Window::effective_minimum_size)
                .map_or(0.0, |(min_w, min_h)| if is_horizontal { min_w } else { min_h })
        })
        .collect();

    // Check for violations in initial layout
    let mut has_violations = false;
    for (window_id, frame) in initial_result {
        if let Some((min_w, min_h)) = layoutable_windows
            .iter()
            .find(|w| w.id == *window_id)
            .and_then(super::state::Window::effective_minimum_size)
        {
            let current_dim = if is_horizontal {
                frame.width
            } else {
                frame.height
            };
            let min_dim = if is_horizontal { min_w } else { min_h };
            if current_dim < min_dim - 1.0 {
                has_violations = true;
                break;
            }
        }
    }

    if !has_violations {
        return None; // No adjustments needed
    }

    log::debug!("Minimum size violations detected in split layout, adjusting ratios");

    // Calculate minimum ratios for each window
    let min_ratios: Vec<f64> =
        min_sizes.iter().map(|&min| (min / available_space).min(1.0)).collect();

    // Check if total minimum requirements exceed available space
    let total_min_ratio: f64 = min_ratios.iter().sum();
    if total_min_ratio > 1.0 {
        log::warn!(
            "Total minimum size requirements ({:.2}%) exceed available space, \
             some windows will be smaller than their minimums",
            total_min_ratio * 100.0
        );
        // Scale down minimums proportionally
        let scale = 1.0 / total_min_ratio;
        let scaled_min_ratios: Vec<f64> = min_ratios.iter().map(|r| r * scale).collect();
        return Some(compute_layout_with_ratios(
            &scaled_min_ratios,
            window_ids,
            &usable_frame,
            gaps,
            is_horizontal,
        ));
    }

    // Compute adjusted ratios that respect minimums while preserving relative sizes
    // where possible
    let adjusted_ratios = compute_adjusted_ratios(current_ratios, &min_ratios, window_ids.len());

    Some(compute_layout_with_ratios(
        &adjusted_ratios,
        window_ids,
        &usable_frame,
        gaps,
        is_horizontal,
    ))
}

/// Computes adjusted window ratios that respect minimum sizes.
///
/// Takes cumulative ratios (0.0 to 1.0) and minimum ratios per window,
/// returns adjusted window size ratios (not cumulative).
fn compute_adjusted_ratios(
    cumulative_ratios: &[f64],
    min_ratios: &[f64],
    window_count: usize,
) -> Vec<f64> {
    // Convert cumulative ratios to per-window ratios
    #[allow(clippy::cast_precision_loss)]
    let mut window_ratios: Vec<f64> = if cumulative_ratios.is_empty() {
        // Default: equal distribution
        vec![1.0 / window_count as f64; window_count]
    } else {
        let mut ratios = Vec::with_capacity(window_count);
        for i in 0..window_count {
            let start = if i == 0 {
                0.0
            } else {
                cumulative_ratios[i - 1]
            };
            let end = if i < cumulative_ratios.len() {
                cumulative_ratios[i]
            } else {
                1.0
            };
            ratios.push(end - start);
        }
        ratios
    };

    // Ensure each window meets its minimum
    for i in 0..window_count {
        if window_ratios[i] < min_ratios[i] {
            let deficit = min_ratios[i] - window_ratios[i];
            window_ratios[i] = min_ratios[i];

            // Take space from other windows that have room
            let mut remaining_deficit = deficit;
            for j in 0..window_count {
                if j != i && remaining_deficit > 0.0 {
                    let available = window_ratios[j] - min_ratios[j];
                    if available > 0.0 {
                        let take = available.min(remaining_deficit);
                        window_ratios[j] -= take;
                        remaining_deficit -= take;
                    }
                }
            }
        }
    }

    // Normalize to ensure sum is exactly 1.0
    let sum: f64 = window_ratios.iter().sum();
    if (sum - 1.0).abs() > 0.001 {
        for ratio in &mut window_ratios {
            *ratio /= sum;
        }
    }

    window_ratios
}

/// Computes layout frames from window size ratios.
fn compute_layout_with_ratios(
    window_ratios: &[f64],
    window_ids: &[u32],
    usable_frame: &Rect,
    gaps: &Gaps,
    is_horizontal: bool,
) -> LayoutResult {
    use smallvec::SmallVec;

    let mut result: LayoutResult = SmallVec::new();
    let inner_gap = if is_horizontal {
        gaps.inner_h
    } else {
        gaps.inner_v
    };
    #[allow(clippy::cast_precision_loss)]
    let total_gaps = inner_gap * (window_ids.len() - 1) as f64;

    let total_dimension = if is_horizontal {
        usable_frame.width - total_gaps
    } else {
        usable_frame.height - total_gaps
    };

    let mut position = if is_horizontal {
        usable_frame.x
    } else {
        usable_frame.y
    };

    for (i, &window_id) in window_ids.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let size = total_dimension
            * window_ratios.get(i).copied().unwrap_or(1.0 / window_ids.len() as f64);

        let frame = if is_horizontal {
            Rect::new(position, usable_frame.y, size, usable_frame.height)
        } else {
            Rect::new(usable_frame.x, position, usable_frame.width, size)
        };

        result.push((window_id, frame));
        position += size + inner_gap;
    }

    result
}

/// Enforces minimum window sizes for Dwindle layout by adjusting ratios.
///
/// Dwindle uses a binary tree structure where each ratio controls a split level.
/// This implementation uses proportional adjustments based on violation severity
/// for faster convergence (typically 1-3 iterations instead of 10).
fn enforce_minimum_sizes_for_dwindle(
    initial_result: &LayoutResult,
    layoutable_windows: &[Window],
    window_ids: &[u32],
    screen_frame: &Rect,
    gaps: &Gaps,
    current_ratios: &[f64],
) -> Option<LayoutResult> {
    // Reduced from 10 - proportional adjustments converge faster
    const MAX_ITERATIONS: usize = 3;

    if window_ids.len() < 2 {
        return None;
    }

    // Build minimum size lookup for O(1) access
    let min_sizes: std::collections::HashMap<u32, (f64, f64)> = layoutable_windows
        .iter()
        .filter_map(|w| w.effective_minimum_size().map(|min| (w.id, min)))
        .collect();

    // Early exit: no windows have minimum sizes
    if min_sizes.is_empty() {
        return None;
    }

    // Find violations and their indices
    let violations = find_minimum_size_violations(initial_result, layoutable_windows);
    if violations.is_empty() {
        return None;
    }

    log::debug!(
        "Minimum size violations in dwindle layout for {} windows",
        violations.len()
    );

    let mut ratios = if current_ratios.is_empty() {
        vec![0.5; window_ids.len().saturating_sub(1)]
    } else {
        current_ratios.to_vec()
    };

    // Ensure we have enough ratios
    while ratios.len() < window_ids.len().saturating_sub(1) {
        ratios.push(0.5);
    }

    let is_landscape = screen_frame.width >= screen_frame.height;

    for _iteration in 0..MAX_ITERATIONS {
        // Collect adjustment magnitudes based on violation severity
        let mut adjustments: Vec<(usize, f64)> = Vec::new();

        for &(window_idx, violation_axis) in &violations {
            // Get the window's frame and minimum size
            let Some((_, frame)) = initial_result.get(window_idx) else {
                continue;
            };
            let window_id = window_ids.get(window_idx).copied().unwrap_or(0);
            let Some(&(min_w, min_h)) = min_sizes.get(&window_id) else {
                continue;
            };

            // Calculate proportional adjustment based on violation magnitude
            let width_deficit = (min_w - frame.width).max(0.0);
            let height_deficit = (min_h - frame.height).max(0.0);

            let width_violated = violation_axis == 0 || violation_axis == 2;
            let height_violated = violation_axis == 1 || violation_axis == 2;

            if window_idx == 0 {
                // Window 0 gets space from the first split
                if !ratios.is_empty() {
                    let is_h = is_dwindle_split_horizontal(0, is_landscape);
                    let deficit = if is_h { width_deficit } else { height_deficit };
                    let total_dim = if is_h {
                        screen_frame.width
                    } else {
                        screen_frame.height
                    };
                    // Proportional adjustment: how much ratio change needed
                    let adjustment = (deficit / total_dim).min(0.3);
                    if adjustment > 0.01 {
                        adjustments.push((0, adjustment));
                    }
                }
            } else {
                let ratio_idx = window_idx - 1;
                if ratio_idx < ratios.len() {
                    let is_h_split = is_dwindle_split_horizontal(window_idx, is_landscape);

                    if (is_h_split && width_violated) || (!is_h_split && height_violated) {
                        let deficit = if is_h_split {
                            width_deficit
                        } else {
                            height_deficit
                        };
                        let total_dim = if is_h_split {
                            screen_frame.width
                        } else {
                            screen_frame.height
                        };
                        let adjustment = (deficit / total_dim).min(0.3);
                        if adjustment > 0.01 {
                            // Negative adjustment to give more space to second half
                            adjustments.push((ratio_idx, -adjustment));
                        }
                    }
                }
            }
        }

        // Apply all adjustments
        for (idx, adj) in adjustments {
            ratios[idx] = (ratios[idx] + adj).clamp(0.1, 0.9);
        }

        // Recompute layout with adjusted ratios
        let new_result = calculate_layout_full(
            LayoutType::Dwindle,
            window_ids,
            screen_frame,
            0.5,
            gaps,
            &ratios,
            MasterPosition::Auto,
        );

        // Check if violations are resolved
        let new_violations = find_minimum_size_violations(&new_result, layoutable_windows);
        if new_violations.is_empty() {
            return Some(new_result);
        }
    }

    // After max iterations, return best effort
    let final_result = calculate_layout_full(
        LayoutType::Dwindle,
        window_ids,
        screen_frame,
        0.5,
        gaps,
        &ratios,
        MasterPosition::Auto,
    );
    Some(final_result)
}

/// Determines if a Dwindle split at the given index is horizontal.
const fn is_dwindle_split_horizontal(split_index: usize, is_landscape: bool) -> bool {
    if is_landscape {
        !split_index.is_multiple_of(2)
    } else {
        split_index.is_multiple_of(2)
    }
}

/// Enforces minimum window sizes for Grid layout by adjusting ratios.
fn enforce_minimum_sizes_for_grid(
    initial_result: &LayoutResult,
    layoutable_windows: &[Window],
    window_ids: &[u32],
    screen_frame: &Rect,
    gaps: &Gaps,
    current_ratios: &[f64],
) -> Option<LayoutResult> {
    // Reduced from 10 - proportional adjustments converge faster
    const MAX_ITERATIONS: usize = 3;

    if window_ids.len() < 2 {
        return None;
    }

    // Build minimum size lookup for O(1) access
    let min_sizes: std::collections::HashMap<u32, (f64, f64)> = layoutable_windows
        .iter()
        .filter_map(|w| w.effective_minimum_size().map(|min| (w.id, min)))
        .collect();

    // Early exit: no windows have minimum sizes
    if min_sizes.is_empty() {
        return None;
    }

    // Find violations
    let violations = find_minimum_size_violations(initial_result, layoutable_windows);
    if violations.is_empty() {
        return None;
    }

    log::debug!(
        "Minimum size violations in grid layout for {} windows",
        violations.len()
    );

    // Grid layout ratio interpretation varies by window count
    // For simplicity, we'll focus on the primary ratio (first one)
    let mut ratios = if current_ratios.is_empty() {
        vec![0.5]
    } else {
        current_ratios.to_vec()
    };

    let is_landscape = screen_frame.width >= screen_frame.height;

    for _iteration in 0..MAX_ITERATIONS {
        // Collect proportional adjustments based on violation severity
        let mut total_adjustment: f64 = 0.0;

        for &(window_idx, violation_axis) in &violations {
            // Get the window's frame and minimum size
            let Some((_, frame)) = initial_result.get(window_idx) else {
                continue;
            };
            let window_id = window_ids.get(window_idx).copied().unwrap_or(0);
            let Some(&(min_w, min_h)) = min_sizes.get(&window_id) else {
                continue;
            };

            // Calculate proportional adjustment based on violation magnitude
            let width_deficit = (min_w - frame.width).max(0.0);
            let height_deficit = (min_h - frame.height).max(0.0);

            let width_violated = violation_axis == 0 || violation_axis == 2;
            let height_violated = violation_axis == 1 || violation_axis == 2;

            // Determine relevant deficit based on layout orientation
            let relevant_deficit = if is_landscape && width_violated {
                width_deficit
            } else if !is_landscape && height_violated {
                height_deficit
            } else {
                continue;
            };

            let total_dim = if is_landscape {
                screen_frame.width
            } else {
                screen_frame.height
            };

            // Proportional adjustment: how much ratio change needed
            let adjustment = (relevant_deficit / total_dim).min(0.3);
            if adjustment < 0.01 {
                continue;
            }

            if window_ids.len() == 2 {
                // Two windows: side by side (landscape) or stacked (portrait)
                // First ratio controls the split
                if window_idx == 0 {
                    // First window needs more space
                    total_adjustment += adjustment;
                } else {
                    // Second window needs more space
                    total_adjustment -= adjustment;
                }
            } else if matches!(window_ids.len(), 3 | 5 | 7) {
                // Master-stack layouts: first ratio controls master width/height
                if window_idx == 0 {
                    // Master window needs more space
                    total_adjustment += adjustment;
                } else {
                    // Stack window needs more space - reduce master
                    total_adjustment -= adjustment;
                }
            }
            // For other window counts (4, 6, 8, 9+), ratio adjustment is more complex
            // and would require knowing the specific grid structure. For now, we'll
            // make best-effort adjustments to the primary ratio.
        }

        // Apply accumulated adjustment
        if total_adjustment.abs() > 0.01 && !ratios.is_empty() {
            ratios[0] = (ratios[0] + total_adjustment).clamp(0.1, 0.9);
        }

        // Recompute layout using calculate_layout_full
        let new_result = calculate_layout_full(
            LayoutType::Grid,
            window_ids,
            screen_frame,
            0.5, // master_ratio not used for grid
            gaps,
            &ratios,
            MasterPosition::Auto,
        );

        // Check if violations are resolved
        let new_violations = find_minimum_size_violations(&new_result, layoutable_windows);
        if new_violations.is_empty() {
            return Some(new_result);
        }
    }

    // Return best effort
    let final_result = calculate_layout_full(
        LayoutType::Grid,
        window_ids,
        screen_frame,
        0.5,
        gaps,
        &ratios,
        MasterPosition::Auto,
    );
    Some(final_result)
}

/// Finds minimum size violations in a layout result.
///
/// Returns a vector of `(window_index, violation_axis)` where:
/// - `violation_axis`: `0` = width, `1` = height, `2` = both
fn find_minimum_size_violations(
    result: &LayoutResult,
    layoutable_windows: &[Window],
) -> Vec<(usize, u8)> {
    let mut violations = Vec::new();

    for (idx, (window_id, frame)) in result.iter().enumerate() {
        if let Some(window) = layoutable_windows.iter().find(|w| w.id == *window_id) {
            // Use effective_minimum_size() to include both reported and inferred minimums
            if let Some((min_w, min_h)) = window.effective_minimum_size() {
                let width_violation = frame.width < min_w - 1.0;
                let height_violation = frame.height < min_h - 1.0;

                if width_violation || height_violation {
                    let axis = match (width_violation, height_violation) {
                        (true, false) => 0,
                        (false, true) => 1,
                        (true, true) => 2,
                        _ => continue,
                    };
                    violations.push((idx, axis));
                }
            }
        }
    }

    violations
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

    // ========================================================================
    // Minimum Size Enforcement Tests
    // ========================================================================

    #[test]
    fn test_compute_adjusted_ratios_no_minimums() {
        // Equal distribution with no minimums
        let cumulative = vec![0.5]; // Two windows at 50-50
        let min_ratios = vec![0.0, 0.0];
        let result = compute_adjusted_ratios(&cumulative, &min_ratios, 2);

        assert_eq!(result.len(), 2);
        assert!((result[0] - 0.5).abs() < 0.01);
        assert!((result[1] - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_compute_adjusted_ratios_one_minimum() {
        // Two windows at 50-50, but first needs 70%
        let cumulative = vec![0.5];
        let min_ratios = vec![0.7, 0.0];
        let result = compute_adjusted_ratios(&cumulative, &min_ratios, 2);

        assert_eq!(result.len(), 2);
        assert!(result[0] >= 0.7, "First window should get at least 70%");
        // Sum should be 1.0
        let sum: f64 = result.iter().sum();
        assert!((sum - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_compute_adjusted_ratios_both_minimums() {
        // Two windows at 50-50, first needs 30%, second needs 60%
        let cumulative = vec![0.5];
        let min_ratios = vec![0.3, 0.6];
        let result = compute_adjusted_ratios(&cumulative, &min_ratios, 2);

        assert_eq!(result.len(), 2);
        assert!(result[0] >= 0.3, "First window should get at least 30%");
        assert!(result[1] >= 0.6, "Second window should get at least 60%");
        // Sum should be 1.0
        let sum: f64 = result.iter().sum();
        assert!((sum - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_compute_adjusted_ratios_empty_cumulative() {
        // No existing ratios, should use equal distribution
        let cumulative: Vec<f64> = vec![];
        let min_ratios = vec![0.0, 0.0, 0.0];
        let result = compute_adjusted_ratios(&cumulative, &min_ratios, 3);

        assert_eq!(result.len(), 3);
        // Should be approximately equal
        for ratio in &result {
            assert!((*ratio - 1.0 / 3.0).abs() < 0.01);
        }
    }

    #[test]
    fn test_compute_adjusted_ratios_three_windows() {
        // Three windows at 33-33-33, first needs 50%
        let cumulative = vec![0.33, 0.66];
        let min_ratios = vec![0.5, 0.0, 0.0];
        let result = compute_adjusted_ratios(&cumulative, &min_ratios, 3);

        assert_eq!(result.len(), 3);
        assert!(result[0] >= 0.5, "First window should get at least 50%");
        // Sum should be 1.0
        let sum: f64 = result.iter().sum();
        assert!((sum - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_compute_layout_with_ratios_horizontal() {
        let ratios = vec![0.5, 0.5];
        let window_ids = vec![1, 2];
        let usable_frame = Rect::new(0.0, 0.0, 1000.0, 500.0);
        let gaps = Gaps::uniform(10.0, 0.0);

        let result = compute_layout_with_ratios(&ratios, &window_ids, &usable_frame, &gaps, true);

        assert_eq!(result.len(), 2);

        let (id1, frame1) = result[0];
        let (id2, frame2) = result[1];

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);

        // With 1000px width, 10px gap, available = 990px
        // Each window gets 495px
        assert!((frame1.width - 495.0).abs() < 1.0);
        assert!((frame2.width - 495.0).abs() < 1.0);
        // Second window should start after first + gap
        assert!((frame2.x - frame1.width - 10.0).abs() < 1.0);
    }

    #[test]
    fn test_compute_layout_with_ratios_vertical() {
        let ratios = vec![0.6, 0.4];
        let window_ids = vec![1, 2];
        let usable_frame = Rect::new(0.0, 0.0, 500.0, 1000.0);
        let gaps = Gaps::uniform(10.0, 0.0);

        let result = compute_layout_with_ratios(&ratios, &window_ids, &usable_frame, &gaps, false);

        assert_eq!(result.len(), 2);

        let (_, frame1) = result[0];
        let (_, frame2) = result[1];

        // With 1000px height, 10px gap, available = 990px
        // First window gets 60% = 594px
        // Second window gets 40% = 396px
        assert!((frame1.height - 594.0).abs() < 1.0);
        assert!((frame2.height - 396.0).abs() < 1.0);
    }

    #[test]
    fn test_enforce_minimum_sizes_no_violations() {
        use smallvec::smallvec;

        // Initial layout with no violations
        let initial_result: LayoutResult = smallvec![
            (1, Rect::new(0.0, 0.0, 500.0, 1000.0)),
            (2, Rect::new(510.0, 0.0, 490.0, 1000.0)),
        ];

        // Windows with no minimum sizes
        let layoutable_windows = vec![
            Window {
                id: 1,
                minimum_size: None,
                ..Default::default()
            },
            Window {
                id: 2,
                minimum_size: None,
                ..Default::default()
            },
        ];
        let window_ids = vec![1, 2];
        let screen_frame = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let gaps = Gaps::uniform(10.0, 0.0);

        let result = enforce_minimum_sizes_for_split(
            &initial_result,
            &layoutable_windows,
            &window_ids,
            &screen_frame,
            &gaps,
            LayoutType::SplitHorizontal,
            &[0.5],
        );

        // No adjustment needed
        assert!(result.is_none());
    }

    #[test]
    fn test_enforce_minimum_sizes_with_violation() {
        use smallvec::smallvec;

        // Initial layout where window 2 is too small
        let initial_result: LayoutResult = smallvec![
            (1, Rect::new(0.0, 0.0, 800.0, 1000.0)),
            (2, Rect::new(810.0, 0.0, 190.0, 1000.0)), // Too small!
        ];

        // Window 2 has minimum width of 400px
        let layoutable_windows = vec![
            Window {
                id: 1,
                minimum_size: None,
                ..Default::default()
            },
            Window {
                id: 2,
                minimum_size: Some((400.0, 100.0)),
                ..Default::default()
            },
        ];
        let window_ids = vec![1, 2];
        let screen_frame = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let gaps = Gaps::uniform(10.0, 0.0);

        let result = enforce_minimum_sizes_for_split(
            &initial_result,
            &layoutable_windows,
            &window_ids,
            &screen_frame,
            &gaps,
            LayoutType::SplitHorizontal,
            &[0.8],
        );

        // Should have adjusted
        assert!(result.is_some());
        let adjusted = result.unwrap();
        assert_eq!(adjusted.len(), 2);

        // Window 2 should now have at least 400px
        let (_, frame2) = adjusted[1];
        assert!(
            frame2.width >= 399.0,
            "Window 2 should have at least ~400px width, got {}",
            frame2.width
        );
    }

    #[test]
    fn test_enforce_minimum_sizes_single_window() {
        use smallvec::smallvec;

        // Single window - no enforcement needed
        let initial_result: LayoutResult = smallvec![(1, Rect::new(0.0, 0.0, 1000.0, 1000.0)),];

        let layoutable_windows = vec![Window {
            id: 1,
            minimum_size: Some((2000.0, 2000.0)),
            ..Default::default()
        }];
        let window_ids = vec![1];
        let screen_frame = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let gaps = Gaps::default();

        let result = enforce_minimum_sizes_for_split(
            &initial_result,
            &layoutable_windows,
            &window_ids,
            &screen_frame,
            &gaps,
            LayoutType::Split,
            &[],
        );

        // Single window always gets full space, no adjustment
        assert!(result.is_none());
    }

    // ========================================================================
    // Dwindle Minimum Size Tests
    // ========================================================================

    #[test]
    fn test_enforce_minimum_sizes_dwindle_no_violations() {
        use smallvec::smallvec;

        // Dwindle layout with no violations
        let initial_result: LayoutResult = smallvec![
            (1, Rect::new(0.0, 0.0, 500.0, 1000.0)),
            (2, Rect::new(500.0, 0.0, 500.0, 1000.0)),
        ];

        let layoutable_windows = vec![
            Window {
                id: 1,
                minimum_size: None,
                ..Default::default()
            },
            Window {
                id: 2,
                minimum_size: None,
                ..Default::default()
            },
        ];
        let window_ids = vec![1, 2];
        let screen_frame = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let gaps = Gaps::default();

        let result = enforce_minimum_sizes_for_dwindle(
            &initial_result,
            &layoutable_windows,
            &window_ids,
            &screen_frame,
            &gaps,
            &[0.5],
        );

        // No adjustment needed
        assert!(result.is_none());
    }

    #[test]
    fn test_enforce_minimum_sizes_dwindle_with_violation() {
        use smallvec::smallvec;

        // Dwindle layout where window 2 is too small
        let initial_result: LayoutResult = smallvec![
            (1, Rect::new(0.0, 0.0, 900.0, 1000.0)),
            (2, Rect::new(900.0, 0.0, 100.0, 1000.0)), // Too small!
        ];

        let layoutable_windows = vec![
            Window {
                id: 1,
                minimum_size: None,
                ..Default::default()
            },
            Window {
                id: 2,
                minimum_size: Some((300.0, 100.0)), // Needs at least 300px width
                ..Default::default()
            },
        ];
        let window_ids = vec![1, 2];
        let screen_frame = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let gaps = Gaps::default();

        let result = enforce_minimum_sizes_for_dwindle(
            &initial_result,
            &layoutable_windows,
            &window_ids,
            &screen_frame,
            &gaps,
            &[0.9], // 90% to first window, 10% to second
        );

        // Should have adjusted
        assert!(result.is_some());
        let adjusted = result.unwrap();
        assert_eq!(adjusted.len(), 2);

        // Window 2 should now have at least 300px (or close to it after adjustment)
        let (_, frame2) = adjusted[1];
        assert!(
            frame2.width >= 290.0,
            "Window 2 should have at least ~300px width after adjustment, got {}",
            frame2.width
        );
    }

    #[test]
    fn test_is_dwindle_split_horizontal() {
        // Landscape mode: odd indices are horizontal
        assert!(is_dwindle_split_horizontal(1, true)); // First split horizontal
        assert!(!is_dwindle_split_horizontal(2, true)); // Second split vertical
        assert!(is_dwindle_split_horizontal(3, true)); // Third split horizontal

        // Portrait mode: even indices are horizontal
        assert!(!is_dwindle_split_horizontal(1, false)); // First split vertical
        assert!(is_dwindle_split_horizontal(2, false)); // Second split horizontal
        assert!(!is_dwindle_split_horizontal(3, false)); // Third split vertical
    }

    // ========================================================================
    // Grid Minimum Size Tests
    // ========================================================================

    #[test]
    fn test_enforce_minimum_sizes_grid_no_violations() {
        use smallvec::smallvec;

        // Grid layout with no violations
        let initial_result: LayoutResult = smallvec![
            (1, Rect::new(0.0, 0.0, 500.0, 1000.0)),
            (2, Rect::new(500.0, 0.0, 500.0, 1000.0)),
        ];

        let layoutable_windows = vec![
            Window {
                id: 1,
                minimum_size: None,
                ..Default::default()
            },
            Window {
                id: 2,
                minimum_size: None,
                ..Default::default()
            },
        ];
        let window_ids = vec![1, 2];
        let screen_frame = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let gaps = Gaps::default();

        let result = enforce_minimum_sizes_for_grid(
            &initial_result,
            &layoutable_windows,
            &window_ids,
            &screen_frame,
            &gaps,
            &[0.5],
        );

        // No adjustment needed
        assert!(result.is_none());
    }

    #[test]
    fn test_enforce_minimum_sizes_grid_two_windows_violation() {
        use smallvec::smallvec;

        // Grid layout where window 2 is too small
        let initial_result: LayoutResult = smallvec![
            (1, Rect::new(0.0, 0.0, 900.0, 1000.0)),
            (2, Rect::new(900.0, 0.0, 100.0, 1000.0)), // Too small!
        ];

        let layoutable_windows = vec![
            Window {
                id: 1,
                minimum_size: None,
                ..Default::default()
            },
            Window {
                id: 2,
                minimum_size: Some((300.0, 100.0)),
                ..Default::default()
            },
        ];
        let window_ids = vec![1, 2];
        let screen_frame = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let gaps = Gaps::default();

        let result = enforce_minimum_sizes_for_grid(
            &initial_result,
            &layoutable_windows,
            &window_ids,
            &screen_frame,
            &gaps,
            &[0.9],
        );

        // Should have adjusted
        assert!(result.is_some());
        let adjusted = result.unwrap();
        assert_eq!(adjusted.len(), 2);

        // Window 2 should now have more space
        let (_, frame2) = adjusted[1];
        assert!(
            frame2.width > 100.0,
            "Window 2 should have more than 100px after adjustment, got {}",
            frame2.width
        );
    }

    #[test]
    fn test_find_minimum_size_violations() {
        use smallvec::smallvec;

        let result: LayoutResult = smallvec![
            (1, Rect::new(0.0, 0.0, 500.0, 400.0)),   // OK
            (2, Rect::new(500.0, 0.0, 100.0, 400.0)), // Width violation
            (3, Rect::new(0.0, 400.0, 500.0, 50.0)),  // Height violation
        ];

        let windows = vec![
            Window {
                id: 1,
                minimum_size: Some((300.0, 300.0)),
                ..Default::default()
            },
            Window {
                id: 2,
                minimum_size: Some((200.0, 300.0)),
                ..Default::default()
            },
            Window {
                id: 3,
                minimum_size: Some((300.0, 100.0)),
                ..Default::default()
            },
        ];

        let violations = find_minimum_size_violations(&result, &windows);

        assert_eq!(violations.len(), 2);
        // Window 2 (index 1) has width violation (axis 0)
        assert!(violations.iter().any(|&(idx, axis)| idx == 1 && axis == 0));
        // Window 3 (index 2) has height violation (axis 1)
        assert!(violations.iter().any(|&(idx, axis)| idx == 2 && axis == 1));
    }
}
