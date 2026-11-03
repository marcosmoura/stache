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

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

pub use handle::{ActorError, StateActorHandle};
pub use messages::{
    CycleDirection, FocusDirection, GeometryUpdate, GeometryUpdateType, QueryResult, StateMessage,
    StateQuery, WindowCreatedInfo,
};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::config::get_config;
use crate::modules::tiling::actor::handlers::window::VisibilityDelta;
use crate::modules::tiling::effects::window_ops::{
    HideAppOutcome, UnhideAppOutcome, app_instance_is_hidden, hide_app_instance_with_outcome,
    unhide_app_instance_with_outcome,
};
use crate::modules::tiling::identity::{AppIdentity, WindowTarget};
use crate::modules::tiling::init::get_subscriber_handle;
use crate::modules::tiling::layout::{
    Gaps, MasterPosition, calculate_layout_full, has_valid_cumulative_ratios,
};
use crate::modules::tiling::state::{LayoutType, Rect, TilingState};
use crate::modules::tiling::visibility::VisibilityRegistry;

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

    /// Shared semantic overflow used only after the bounded channel fills.
    pending: Arc<parking_lot::Mutex<handle::PendingActorMessages>>,

    /// Shared visibility registry; the actor is the sole runtime writer.
    #[allow(dead_code)] // written by the actor's hide/unhide paths in 19D
    registry: Arc<VisibilityRegistry>,
}

impl StateActor {
    /// Spawn a new state actor and return a handle for communication.
    ///
    /// The actor will run in the background and process messages. The returned
    /// latch completes only after the actor's message loop has fully exited.
    ///
    /// `pub(crate)` because the tuple exposes the crate-private
    /// `crate::modules::tiling::init::CompletionLatch`.
    #[must_use]
    #[allow(dead_code)] // used by test callsites; production uses spawn_with_registry
    pub(crate) fn spawn() -> (StateActorHandle, crate::modules::tiling::init::CompletionLatch) {
        Self::spawn_with_registry(Arc::new(VisibilityRegistry::default()))
    }

    /// Spawn a state actor sharing an explicit visibility registry with its
    /// handle (used by the runtime factory to publish one registry per
    /// generation).
    #[must_use]
    pub(crate) fn spawn_with_registry(
        registry: Arc<VisibilityRegistry>,
    ) -> (StateActorHandle, crate::modules::tiling::init::CompletionLatch) {
        tracing::debug!("tiling: spawning state actor");
        let (sender, receiver) = mpsc::channel(CHANNEL_BUFFER_SIZE);
        let stopped = crate::modules::tiling::init::CompletionLatch::new();
        let stopped_for_task = stopped.clone();
        let handle = StateActorHandle::new_with_registry(sender, Arc::clone(&registry));

        let actor = Self {
            state: TilingState::new(),
            receiver,
            pending: Arc::clone(&handle.pending),
            registry,
        };

        // Spawn the actor task using Tauri's async runtime
        // This works during app setup when tokio runtime isn't directly available
        tauri::async_runtime::spawn(async move {
            actor.run().await;
            stopped_for_task.mark_complete();
        });

        (handle, stopped)
    }

    /// Run the actor's message loop.
    ///
    /// This loop includes panic recovery - if a message handler panics,
    /// the error is logged and the actor continues processing messages.
    async fn run(mut self) {
        tracing::trace!("tiling: actor message loop starting");

        loop {
            let msg = if self.pending.lock().take_shutdown().is_some() {
                StateMessage::Shutdown
            } else {
                match self.receiver.try_recv() {
                    Ok(message) => message,
                    Err(mpsc::error::TryRecvError::Empty) => {
                        let pending_message = self.pending.lock().pop();
                        if let Some(message) = pending_message {
                            message
                        } else if let Some(message) = self.receiver.recv().await {
                            message
                        } else {
                            break;
                        }
                    }
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        let Some(message) = self.pending.lock().pop() else {
                            break;
                        };
                        message
                    }
                }
            };
            if matches!(msg, StateMessage::Shutdown) {
                tracing::debug!("State actor received shutdown message");
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

                tracing::error!("tiling: PANIC in actor while handling '{msg_name}': {panic_msg}");
                tracing::error!(
                    "tiling: Actor recovered from panic - state may be inconsistent. \
                     Consider restarting the application if issues persist."
                );
            }
        }

        tracing::debug!("State actor channel closed, exiting");
    }

    /// Handle a single message.
    #[allow(clippy::too_many_lines)]
    fn handle_message(&mut self, msg: StateMessage) {
        match msg {
            // Window events - delegated to handlers
            StateMessage::WindowCreated(info) => {
                handlers::on_window_created(&mut self.state, info);
            }
            StateMessage::WindowDestroyed { window_id, identity } => {
                if !self.window_event_matches(window_id, identity) {
                    tracing::trace!(
                        "tiling: dropping stale WindowDestroyed for window_id={window_id}"
                    );
                    return;
                }
                tracing::debug!(
                    "tiling: actor received WindowDestroyed message for window_id={window_id}"
                );
                if let Some(ws_id) =
                    handlers::on_window_destroyed(&mut self.state, window_id, identity)
                {
                    tracing::debug!(
                        "tiling: window {window_id} was in workspace {ws_id}, notifying subscriber"
                    );
                    // Notify subscriber to prune per-window cache state and
                    // recompute layout for the affected workspace
                    if let Some(handle) = get_subscriber_handle() {
                        tracing::debug!(
                            "tiling: sending layout_changed notification to subscriber for workspace {ws_id}"
                        );
                        handle.notify_window_destroyed(window_id);
                        handle.notify_layout_changed(ws_id, false);
                    } else {
                        tracing::warn!(
                            "tiling: no subscriber handle available to notify layout change!"
                        );
                    }
                } else {
                    tracing::debug!(
                        "tiling: window {window_id} was not tracked or workspace not found"
                    );
                }
            }
            StateMessage::WindowFocused { window_id, identity } => {
                if !self.window_event_matches(window_id, identity) {
                    tracing::trace!(
                        "tiling: dropping stale WindowFocused for window_id={window_id}"
                    );
                    return;
                }
                let delta = handlers::on_window_focused(&mut self.state, window_id);
                self.sync_visibility_for_workspaces(delta);
            }
            StateMessage::WindowUnfocused { window_id, identity } => {
                if !self.window_event_matches(window_id, identity) {
                    tracing::trace!(
                        "tiling: dropping stale WindowUnfocused for window_id={window_id}"
                    );
                    return;
                }
                handlers::on_window_unfocused(&mut self.state, window_id);
            }
            StateMessage::WindowMoved { window_id, identity, frame } => {
                if !self.window_event_matches(window_id, identity) {
                    tracing::trace!("tiling: dropping stale WindowMoved for window_id={window_id}");
                    return;
                }
                handlers::on_window_moved(&mut self.state, window_id, frame);
            }
            StateMessage::WindowResized { window_id, identity, frame } => {
                if !self.window_event_matches(window_id, identity) {
                    tracing::trace!(
                        "tiling: dropping stale WindowResized for window_id={window_id}"
                    );
                    return;
                }
                handlers::on_window_resized(&mut self.state, window_id, frame);
            }
            StateMessage::WindowMinimized { window_id, identity, minimized } => {
                if !self.window_event_matches(window_id, identity) {
                    tracing::trace!(
                        "tiling: dropping stale WindowMinimized for window_id={window_id}"
                    );
                    return;
                }
                handlers::on_window_minimized(&mut self.state, window_id, minimized);
            }
            StateMessage::WindowTitleChanged { window_id, identity, title } => {
                if !self.window_event_matches(window_id, identity) {
                    tracing::trace!(
                        "tiling: dropping stale WindowTitleChanged for window_id={window_id}"
                    );
                    return;
                }
                handlers::on_window_title_changed(&mut self.state, window_id, &title);
            }
            StateMessage::WindowFullscreenChanged {
                window_id,
                identity,
                fullscreen,
            } => {
                if !self.window_event_matches(window_id, identity) {
                    tracing::trace!(
                        "tiling: dropping stale WindowFullscreenChanged for window_id={window_id}"
                    );
                    return;
                }
                handlers::on_window_fullscreen_changed(&mut self.state, window_id, fullscreen);
            }

            // App events - delegated to handlers
            StateMessage::AppLaunched { identity, pid, bundle_id, name } => {
                handlers::on_app_launched(&mut self.state, identity, pid, &bundle_id, &name);
            }
            StateMessage::AppTerminated { identity, pid: _ } => {
                self.on_app_terminated_exact(identity);
            }
            StateMessage::AppHidden { identity, pid: _ } => {
                let os_hidden = app_instance_is_hidden(identity);
                self.on_app_hidden_revalidated(identity, os_hidden);
            }
            StateMessage::AppShown { identity, pid: _ } => {
                let os_hidden = app_instance_is_hidden(identity);
                self.on_app_shown_revalidated(identity, |_| os_hidden);
            }
            StateMessage::AppActivated { identity, pid } => {
                handlers::on_app_activated(&mut self.state, identity, pid);
            }

            // Screen events - delegated to handlers
            StateMessage::ScreensChanged => {
                let delta = handlers::on_screens_changed(&mut self.state);
                self.sync_visibility_for_workspaces(delta);
            }
            StateMessage::SetScreens { screens } => {
                tracing::trace!("tiling: received SetScreens with {} screens", screens.len());
                let delta = handlers::on_set_screens(&mut self.state, screens);
                self.sync_visibility_for_workspaces(delta);
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
            StateMessage::SwapWindows { target_a, target_b } => {
                self.on_swap_windows(target_a, target_b);
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
                let delta = handlers::on_send_workspace_to_screen(&mut self.state, &target_screen);
                self.sync_visibility_for_workspaces(delta);
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
                    tracing::warn!("tiling: failed to send query response (channel closed)");
                }
            }

            // Batched geometry - delegated to handlers (per-update identity check)
            StateMessage::BatchedGeometryUpdates(updates) => {
                let updates: Vec<GeometryUpdate> = updates
                    .into_iter()
                    .filter(|u| self.window_event_matches(u.window_id, u.identity))
                    .collect();
                handlers::on_batched_geometry_updates(&mut self.state, &updates);
            }

            // User-initiated resize completed
            StateMessage::UserResizeCompleted {
                workspace_id,
                target,
                old_frame,
                new_frame,
            } => {
                self.on_user_resize_completed(workspace_id, target, old_frame, new_frame);
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
            StateMessage::InitComplete { subscriber } => {
                self.on_init_complete(&subscriber);
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
    fn on_set_expected_frames(&mut self, frames: Vec<(WindowTarget, Rect)>) {
        for (target, frame) in frames {
            // Only apply when the stored window identity equals the target identity.
            let matches = self
                .state
                .get_window(target.window_id)
                .is_some_and(|w| w.identity == Some(target.identity));
            if !matches {
                tracing::trace!(
                    "tiling: dropping stale expected frame for window_id={}",
                    target.window_id
                );
                continue;
            }
            self.state.update_window(target.window_id, |w| {
                w.frame = frame;
                w.expected_frame = Some(frame);
            });
        }
    }

    /// Rejects AX-derived events for a window whose stored identity differs
    /// from the event identity (stale or PID-reused window ID).
    fn window_event_matches(&self, window_id: u32, identity: AppIdentity) -> bool {
        match self.state.get_window(window_id) {
            Some(w) => w.identity.is_none_or(|stored| stored == identity),
            None => false,
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

            StateQuery::GetWindowLayoutTargets { workspace_id } => {
                QueryResult::TargetLayout(self.compute_layout_targets(workspace_id))
            }

            StateQuery::GetFocusTargets => {
                let focus = eyeball::Observable::get(&self.state.focus);
                let focused_window = focus.focused_window_id.and_then(|id| {
                    self.state.get_window(id).and_then(|w| {
                        w.identity.map(|identity| WindowTarget { identity, window_id: id })
                    })
                });
                QueryResult::TargetFocus {
                    focused_window,
                    focused_workspace_id: focus.focused_workspace_id,
                }
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

    /// Identity-keyed layout targets for the effect subscriber.
    ///
    /// Windows without a stored identity are omitted (fail-closed).
    fn compute_layout_targets(&self, workspace_id: uuid::Uuid) -> Vec<(WindowTarget, Rect)> {
        self.compute_layout(workspace_id)
            .into_iter()
            .filter_map(|(window_id, frame)| {
                self.state.get_window(window_id).and_then(|w| {
                    w.identity.map(|identity| (WindowTarget { identity, window_id }, frame))
                })
            })
            .collect()
    }

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
            tracing::warn!("compute_layout: workspace {workspace_id} not found");
            return Vec::new();
        };

        // Get screen for this workspace
        let Some(screen) = self.state.get_screen(workspace.screen_id) else {
            tracing::warn!(
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

        // Get master ratio: prefer workspace runtime value (set by user resize),
        // falling back to the config default.
        let master_ratio = workspace
            .master_ratio
            .unwrap_or_else(|| f64::from(config.tiling.master.ratio) / 100.0);

        // Get master position from config
        let master_position = MasterPosition::from(config.tiling.master.position);

        // Split ratios are tied to the current layoutable membership. A window
        // add/remove can make persisted cumulative boundaries stale; use the
        // same equal-split fallback for the initial layout and minimum-size
        // enforcement until the next user resize establishes new boundaries.
        let split_ratios =
            if matches!(
                workspace.layout,
                LayoutType::Split | LayoutType::SplitHorizontal | LayoutType::SplitVertical
            ) && !has_valid_cumulative_ratios(&workspace.split_ratios, window_ids.len())
            {
                Vec::new()
            } else {
                workspace.split_ratios.clone()
            };

        // Compute initial layout
        let result = calculate_layout_full(
            workspace.layout,
            &window_ids,
            &screen.visible_frame,
            master_ratio,
            &gaps,
            &split_ratios,
            master_position,
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

    fn on_cycle_workspace(&mut self, direction: CycleDirection) {
        let delta = handlers::on_cycle_workspace(&mut self.state, direction);
        self.sync_workspace_switch_visibility(delta);
    }

    fn on_switch_workspace(&mut self, name: &str) {
        let delta = handlers::on_switch_workspace(&mut self.state, name);
        self.sync_workspace_switch_visibility(delta);
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
        let delta = handlers::on_move_window_to_workspace(&mut self.state, window_id, workspace_id);
        self.sync_visibility_for_workspaces(delta);
    }

    fn on_swap_windows(&mut self, target_a: WindowTarget, target_b: WindowTarget) {
        // Exact identity validation: never swap against a stale window ID.
        let a_ok = self.window_event_matches(target_a.window_id, target_a.identity);
        let b_ok = self.window_event_matches(target_b.window_id, target_b.identity);
        if !(a_ok && b_ok) {
            tracing::trace!("tiling: dropping stale SwapWindows (identity mismatch)");
            return;
        }
        handlers::on_swap_windows(&mut self.state, target_a.window_id, target_b.window_id);
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
        let delta = handlers::on_send_window_to_screen(&mut self.state, target_screen);
        self.sync_visibility_for_workspaces(delta);
    }

    fn on_resize_focused_window(&mut self, dimension: messages::ResizeDimension, amount: i32) {
        handlers::on_resize_focused_window(&mut self.state, dimension, amount);
    }

    fn on_apply_preset(&mut self, preset_name: &str) {
        handlers::on_apply_preset(&mut self.state, preset_name);
    }

    fn on_set_enabled(&mut self, enabled: bool) {
        tracing::debug!("Set enabled: {enabled}");
        self.state.set_enabled(enabled);
    }

    fn on_user_resize_completed(
        &mut self,
        workspace_id: uuid::Uuid,
        target: WindowTarget,
        old_frame: Rect,
        new_frame: Rect,
    ) {
        // Exact identity validation before ratio recalculation.
        if !self.window_event_matches(target.window_id, target.identity) {
            tracing::trace!("tiling: dropping stale UserResizeCompleted (identity mismatch)");
            return;
        }
        handlers::on_user_resize_completed(
            &mut self.state,
            workspace_id,
            target.window_id,
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
        tracing::debug!(
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
    fn on_init_complete(
        &mut self,
        subscriber: &crate::modules::tiling::effects::subscriber::EffectSubscriberHandle,
    ) {
        tracing::debug!("Initialization complete, applying initial layouts");

        // Sync window visibility based on workspace visibility
        self.sync_window_visibility();

        // Get all visible workspaces and trigger layout for each
        let visible_workspace_ids: Vec<uuid::Uuid> =
            self.state.get_visible_workspaces().iter().map(|ws| ws.id).collect();

        // Notify subscriber for each visible workspace
        for ws_id in visible_workspace_ids {
            subscriber.notify_layout_changed(ws_id, false);
        }

        tracing::debug!("Initial layout notifications sent");
    }

    /// Syncs window visibility based on workspace visibility (identity-exact).
    ///
    /// - Unhides apps that have windows in visible workspaces
    /// - Hides apps that have windows ONLY in non-visible workspaces
    fn sync_window_visibility(&mut self) {
        use std::collections::{BTreeSet, HashSet};

        use uuid::Uuid;

        let visible_ws_ids: HashSet<Uuid> =
            self.state.get_visible_workspaces().iter().map(|ws| ws.id).collect();
        let mut visible = BTreeSet::new();
        let mut non_visible = BTreeSet::new();
        for window in self.state.windows.iter() {
            let Some(identity) = window.identity else {
                continue;
            };
            if visible_ws_ids.contains(&window.workspace_id) {
                visible.insert(identity);
            } else {
                non_visible.insert(identity);
            }
        }
        for identity in visible.iter().copied() {
            self.handle_unhide_for_workspace(identity);
        }
        for identity in non_visible.difference(&visible).copied() {
            self.handle_hide_for_workspace(identity);
        }

        tracing::debug!(
            "Synced window visibility: {} apps shown, {} apps hidden",
            visible.len(),
            non_visible.difference(&visible).count()
        );
    }

    /// Hides one identity through the registry (actor is the sole writer).
    fn handle_hide_for_workspace_with(
        &mut self,
        identity: AppIdentity,
        hide: impl FnOnce(AppIdentity) -> HideAppOutcome,
    ) -> HideAppOutcome {
        let outcome = self.registry.hide_if_open(identity, hide);
        // Registry lock already released — update window state on actor side.
        if outcome == HideAppOutcome::HiddenByStache {
            for wid in self.state.windows_identity_iter(&identity) {
                self.state.update_window(wid, |w| w.is_hidden = true);
            }
        }
        outcome
    }

    fn handle_hide_for_workspace(&mut self, identity: AppIdentity) -> HideAppOutcome {
        self.handle_hide_for_workspace_with(identity, hide_app_instance_with_outcome)
    }

    /// Unhides one identity through the registry (actor is the sole writer).
    fn handle_unhide_for_workspace_with(
        &mut self,
        identity: AppIdentity,
        unhide: impl FnOnce(AppIdentity) -> UnhideAppOutcome,
    ) -> UnhideAppOutcome {
        let outcome = self.registry.unhide_if_open(identity, unhide);
        if matches!(
            outcome,
            UnhideAppOutcome::UnhiddenByStache | UnhideAppOutcome::AlreadyShown
        ) {
            for wid in self.state.windows_identity_iter(&identity) {
                self.state.update_window(wid, |w| w.is_hidden = false);
            }
        }
        outcome
    }

    fn handle_unhide_for_workspace(&mut self, identity: AppIdentity) -> UnhideAppOutcome {
        self.handle_unhide_for_workspace_with(identity, unhide_app_instance_with_outcome)
    }

    /// Applies a visibility delta produced by a workspace transition.
    fn sync_visibility_for_workspaces(&mut self, delta: VisibilityDelta) {
        for identity in delta.showing {
            self.handle_unhide_for_workspace(identity);
        }
        for identity in delta.hiding {
            self.handle_hide_for_workspace(identity);
        }
    }

    /// Applies a user-initiated workspace switch in the order required by
    /// `AppKit`: show the destination app, make its selected window active, then
    /// hide apps which no longer appear on a visible workspace.
    ///
    /// This is deliberately separate from generic visibility deltas. Focus
    /// events already originate from an active app; explicit switches do not.
    fn sync_workspace_switch_visibility(&mut self, delta: VisibilityDelta) {
        for identity in delta.showing {
            self.handle_unhide_for_workspace(identity);
        }

        if let Some(window_id) = self.state.get_focus_state().focused_window_id
            && !crate::modules::tiling::effects::window_ops::focus_stored_window_sync(
                &self.state,
                window_id,
            )
        {
            tracing::debug!("workspace switch: failed to focus window {window_id}");
        }

        for identity in delta.hiding {
            self.handle_hide_for_workspace(identity);
        }
    }

    /// `AppShown` with identity revalidation. OS visible → relinquish ownership and
    /// mark windows shown; OS hidden/unknown → retain (unavoidable-history policy).
    pub(crate) fn on_app_shown_revalidated(
        &mut self,
        identity: AppIdentity,
        query_os: impl FnOnce(AppIdentity) -> Option<bool>,
    ) {
        if query_os(identity) == Some(false) {
            self.registry.relinquish_if_open(&identity);
            for wid in self.state.windows_identity_iter(&identity) {
                self.state.update_window(wid, |w| w.is_hidden = false);
            }
        }
    }

    /// `AppHidden` — OS confirms hidden. Mismatch/unknown → no state change.
    fn on_app_hidden_revalidated(&mut self, identity: AppIdentity, os_hidden: Option<bool>) {
        if os_hidden == Some(true) {
            for wid in self.state.windows_identity_iter(&identity) {
                self.state.update_window(wid, |w| w.is_hidden = true);
            }
        }
    }

    /// Exact termination cleanup. Observer removal already ran on the main thread
    /// in `app_monitor` before this handler.
    fn on_app_terminated_exact(&mut self, identity: AppIdentity) {
        self.on_app_terminated_exact_with(
            identity,
            crate::modules::tiling::tabs::clear_tabs_for_identity,
            |target| crate::modules::tiling::effects::get_window_cache().invalidate_window(target),
            |identity| {
                crate::modules::tiling::effects::get_window_cache()
                    .invalidate_app_identity(identity);
            },
        );
    }

    pub(crate) fn on_app_terminated_exact_with(
        &mut self,
        identity: AppIdentity,
        mut clear_tabs_for_identity: impl FnMut(AppIdentity),
        mut invalidate_window: impl FnMut(WindowTarget),
        mut invalidate_app: impl FnMut(AppIdentity),
    ) {
        clear_tabs_for_identity(identity);
        invalidate_app(identity);

        let window_ids: Vec<u32> = self
            .state
            .windows
            .iter()
            .filter(|w| w.identity.as_ref() == Some(&identity))
            .map(|w| w.id)
            .collect();

        if window_ids.is_empty() {
            self.registry.relinquish_if_open(&identity);
            return;
        }

        let mut affected_workspaces: HashSet<Uuid> = HashSet::new();
        let subscriber_handle = crate::modules::tiling::init::get_subscriber_handle();
        for wid in &window_ids {
            invalidate_window(WindowTarget { identity, window_id: *wid });
            if let Some(handle) = &subscriber_handle {
                handle.notify_window_destroyed(*wid);
            }
            self.state.remove_window_from_focus_history(*wid);
            if let Some(ws_id) = self.state.get_window(*wid).map(|w| w.workspace_id) {
                affected_workspaces.insert(ws_id);
                self.state.update_workspace(ws_id, |ws| {
                    // Preserve the focused window by ID; index shifts must not
                    // silently focus a sibling.
                    let focused_window_id =
                        ws.focused_window_index.and_then(|index| ws.window_ids.get(index).copied());
                    ws.window_ids.retain(|id| id != wid);
                    ws.focused_window_index = focused_window_id
                        .and_then(|focused_id| {
                            ws.window_ids.iter().position(|id| *id == focused_id)
                        })
                        .or_else(|| (!ws.window_ids.is_empty()).then_some(0));
                });
            }
        }

        let current_focus = eyeball::Observable::get(&self.state.focus);
        if current_focus.focused_window_id.is_some_and(|fid| window_ids.contains(&fid)) {
            self.state.clear_focus();
        }
        for wid in &window_ids {
            self.state.remove_window(*wid);
        }
        if let Some(handle) = subscriber_handle {
            for ws_id in &affected_workspaces {
                handle.notify_layout_changed(*ws_id, false);
            }
        }
        self.registry.relinquish_if_open(&identity);
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::handle::PendingActorMessages;
    use super::*;
    use crate::modules::tiling::identity::{AppIdentity, LaunchDateBits};
    use crate::modules::tiling::state::{Window, Workspace};

    #[allow(clippy::cast_precision_loss)] // test-only identity construction
    fn test_identity(pid: i32, v: u64) -> AppIdentity {
        AppIdentity {
            pid,
            launch_date: LaunchDateBits::from_time_interval_since_reference_date(v as f64).unwrap(),
        }
    }

    fn make_actor() -> (StateActor, Arc<VisibilityRegistry>) {
        let registry = Arc::new(VisibilityRegistry::default());
        let actor = StateActor {
            state: TilingState::new(),
            receiver: tokio::sync::mpsc::channel(16).1,
            pending: Arc::new(parking_lot::Mutex::new(PendingActorMessages::default())),
            registry: Arc::clone(&registry),
        };
        (actor, registry)
    }

    #[test]
    fn init_visibility_owns_only_hidden_only_identities() {
        let (mut actor, registry) = make_actor();
        let visible_id = Uuid::now_v7();
        let hidden_id = Uuid::now_v7();
        let a = test_identity(10, 1);
        let b = test_identity(20, 2);
        actor.state.upsert_workspace(Workspace {
            id: visible_id,
            name: "v".into(),
            screen_id: 1,
            is_visible: true,
            is_focused: true,
            ..Workspace::default()
        });
        actor.state.upsert_workspace(Workspace {
            id: hidden_id,
            name: "h".into(),
            screen_id: 1,
            is_visible: false,
            ..Workspace::default()
        });
        actor.state.upsert_window(Window {
            id: 1,
            pid: 10,
            identity: Some(a),
            workspace_id: visible_id,
            ..Window::default()
        });
        actor.state.upsert_window(Window {
            id: 2,
            pid: 20,
            identity: Some(b),
            workspace_id: hidden_id,
            ..Window::default()
        });

        // Init flow with injected outcomes: visible app already shown, hidden
        // app hidden by Stache.
        actor.handle_unhide_for_workspace_with(a, |_| UnhideAppOutcome::AlreadyShown);
        actor.handle_hide_for_workspace_with(b, |_| HideAppOutcome::HiddenByStache);

        assert!(!registry.contains(&a), "visible identity must not be owned");
        assert!(registry.contains(&b), "hidden-only identity must be owned");
    }

    #[test]
    fn cycle_workspace_does_not_touch_registry() {
        let (mut actor, registry) = make_actor();
        let ws_id = Uuid::now_v7();
        actor.state.upsert_workspace(Workspace {
            id: ws_id,
            name: "main".into(),
            screen_id: 1,
            is_visible: true,
            is_focused: true,
            ..Workspace::default()
        });
        actor.state.upsert_window(Window {
            id: 1,
            pid: 10,
            identity: Some(test_identity(10, 1)),
            workspace_id: ws_id,
            ..Window::default()
        });

        actor.on_cycle_workspace(CycleDirection::Next);
        assert!(
            registry.is_empty(),
            "workspace cycling must not mutate ownership"
        );
    }

    #[test]
    fn failed_hide_does_not_claim_ownership() {
        let (mut actor, registry) = make_actor();
        let identity = test_identity(10, 1);
        actor.state.upsert_window(Window {
            id: 1,
            pid: 10,
            identity: Some(identity),
            workspace_id: Uuid::nil(),
            ..Window::default()
        });

        let outcome = actor.handle_hide_for_workspace_with(identity, |_| HideAppOutcome::Failed);
        assert_eq!(outcome, HideAppOutcome::Failed);
        assert!(
            !registry.contains(&identity),
            "failed hide must not claim ownership"
        );
    }

    #[tokio::test]
    async fn test_actor_spawn_and_shutdown() {
        let (handle, _stopped) = StateActor::spawn();
        assert!(handle.is_alive());

        // Send shutdown
        handle.shutdown().unwrap();

        // Give actor time to process
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn test_actor_query_enabled() {
        let (handle, _stopped) = StateActor::spawn();

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
        let (handle, _stopped) = StateActor::spawn();

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
        let (handle, _stopped) = StateActor::spawn();

        let result = handle.get_focus_state().await.unwrap();
        let focus = result.into_focus().unwrap();
        assert!(!focus.has_focus());

        handle.shutdown().unwrap();
    }
}
