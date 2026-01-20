//! Effect subscriber for reacting to state changes.
//!
//! The subscriber watches for changes in tiling state and computes effects
//! that need to be applied to the system. It connects the reactive state
//! model to the effect executor.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                     TilingState                                  │
//! │  (Observable collections: screens, workspaces, windows, focus)  │
//! └─────────────────────────┬───────────────────────────────────────┘
//!                           │ subscribe()
//!                           ▼
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                   EffectSubscriber                               │
//! │  - Tracks previous layout positions                             │
//! │  - Tracks previous focus state                                  │
//! │  - Computes deltas when state changes                           │
//! │  - Generates effects for executor                               │
//! └─────────────────────────┬───────────────────────────────────────┘
//!                           │ Vec<TilingEffect>
//!                           ▼
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                   EffectExecutor                                 │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! The subscriber is typically spawned as a background task that runs
//! for the lifetime of the tiling system:
//!
//! ```rust,ignore
//! let subscriber = EffectSubscriber::new(actor_handle, executor);
//! tauri::async_runtime::spawn(subscriber.run());
//! ```

use std::collections::HashMap;

use tokio::sync::mpsc;
use uuid::Uuid;

use super::executor::{EffectExecutor, effects_from_focus_change, effects_from_layout_change};
use super::{FocusChange, LayoutChange, TilingEffect, begin_animation, cancel_animation};
use crate::modules::tiling::actor::{QueryResult, StateActorHandle, StateQuery};
use crate::modules::tiling::state::{FocusState, LayoutType, Rect};

// ============================================================================
// Subscriber State
// ============================================================================

/// Tracks the previous state for computing deltas.
#[derive(Debug, Default)]
struct SubscriberState {
    /// Previous layout positions per workspace.
    layout_positions: HashMap<Uuid, Vec<(u32, Rect)>>,

    /// Previous focus state.
    focus: FocusState,

    /// Previous visible workspaces (`workspace_id` -> `screen_id`).
    visible_workspaces: HashMap<Uuid, u32>,

    /// Workspace layouts (for determining monocle/floating state).
    workspace_layouts: HashMap<Uuid, LayoutType>,

    /// Floating window IDs.
    floating_windows: std::collections::HashSet<u32>,
}

impl SubscriberState {
    /// Creates a new empty state.
    fn new() -> Self { Self::default() }

    /// Updates layout positions and returns the change if any.
    ///
    /// For user-triggered changes, we always return a change because the actual
    /// window positions might differ from our tracked positions (e.g., after a drag).
    fn update_layout(
        &mut self,
        workspace_id: Uuid,
        new_positions: Vec<(u32, Rect)>,
        user_triggered: bool,
    ) -> Option<LayoutChange> {
        let old_positions = self.layout_positions.get(&workspace_id).cloned().unwrap_or_default();

        // For user-triggered changes (like after a drag), always apply the layout
        // because the actual window positions might differ from our tracked positions.
        // For programmatic changes, only apply if our tracking shows a change.
        if !user_triggered && old_positions == new_positions {
            return None;
        }

        let change = LayoutChange::new(
            workspace_id,
            old_positions,
            new_positions.clone(),
            user_triggered,
        );

        self.layout_positions.insert(workspace_id, new_positions);

        Some(change)
    }

    /// Updates focus and returns the change if any.
    fn update_focus(&mut self, new_focus: FocusState) -> Option<FocusChange> {
        let old_focus = self.focus.clone();

        // Check if anything actually changed
        if old_focus == new_focus {
            return None;
        }

        let change = FocusChange::new(
            old_focus.focused_window_id,
            new_focus.focused_window_id,
            old_focus.focused_workspace_id,
            new_focus.focused_workspace_id,
        );

        self.focus = new_focus;

        Some(change)
    }

    /// Checks if a workspace is in monocle layout.
    #[allow(dead_code)] // May be useful for future features
    fn is_monocle(&self, workspace_id: Uuid) -> bool {
        self.workspace_layouts
            .get(&workspace_id)
            .is_some_and(|layout| *layout == LayoutType::Monocle)
    }

    /// Checks if a window is floating.
    #[allow(dead_code)] // May be useful for future features
    fn is_floating(&self, window_id: u32) -> bool { self.floating_windows.contains(&window_id) }

    /// Updates workspace layout tracking.
    fn update_workspace_layout(&mut self, workspace_id: Uuid, layout: LayoutType) {
        self.workspace_layouts.insert(workspace_id, layout);
    }

    /// Updates floating window tracking.
    fn set_window_floating(&mut self, window_id: u32, floating: bool) {
        if floating {
            self.floating_windows.insert(window_id);
        } else {
            self.floating_windows.remove(&window_id);
        }
    }
}

// ============================================================================
// Effect Subscriber
// ============================================================================

/// Subscriber notification types.
#[derive(Debug)]
pub enum SubscriberNotification {
    /// Layout needs to be recomputed for a workspace.
    LayoutChanged {
        workspace_id: Uuid,
        user_triggered: bool,
    },

    /// Focus state changed.
    FocusChanged,

    /// Workspace visibility changed.
    VisibilityChanged { workspace_id: Uuid, visible: bool },

    /// Window floating state changed.
    FloatingChanged { window_id: u32, floating: bool },

    /// Workspace layout type changed.
    WorkspaceLayoutChanged {
        workspace_id: Uuid,
        layout: LayoutType,
    },

    /// Shutdown the subscriber.
    Shutdown,
}

/// Subscribes to state changes and computes effects.
///
/// The subscriber maintains a cache of previous state to compute deltas
/// when state changes. It then generates effects that the executor applies.
pub struct EffectSubscriber {
    /// Handle to the state actor for queries.
    actor_handle: StateActorHandle,

    /// Executor for applying effects.
    executor: EffectExecutor,

    /// Receiver for notifications.
    notification_rx: mpsc::Receiver<SubscriberNotification>,

    /// Previous state for computing deltas.
    state: SubscriberState,
}

/// Handle for sending notifications to the subscriber.
#[derive(Clone)]
pub struct EffectSubscriberHandle {
    notification_tx: mpsc::Sender<SubscriberNotification>,
}

impl EffectSubscriberHandle {
    /// Notifies the subscriber that a layout changed.
    pub fn notify_layout_changed(&self, workspace_id: Uuid, user_triggered: bool) {
        let _ = self
            .notification_tx
            .try_send(SubscriberNotification::LayoutChanged { workspace_id, user_triggered });
    }

    /// Notifies the subscriber that focus changed.
    pub fn notify_focus_changed(&self) {
        let _ = self.notification_tx.try_send(SubscriberNotification::FocusChanged);
    }

    /// Notifies the subscriber that workspace visibility changed.
    pub fn notify_visibility_changed(&self, workspace_id: Uuid, visible: bool) {
        let _ = self
            .notification_tx
            .try_send(SubscriberNotification::VisibilityChanged { workspace_id, visible });
    }

    /// Notifies the subscriber that a window's floating state changed.
    pub fn notify_floating_changed(&self, window_id: u32, floating: bool) {
        let _ = self
            .notification_tx
            .try_send(SubscriberNotification::FloatingChanged { window_id, floating });
    }

    /// Notifies the subscriber that a workspace's layout changed.
    pub fn notify_workspace_layout_changed(&self, workspace_id: Uuid, layout: LayoutType) {
        let _ = self
            .notification_tx
            .try_send(SubscriberNotification::WorkspaceLayoutChanged { workspace_id, layout });
    }

    /// Shuts down the subscriber.
    pub fn shutdown(&self) {
        let _ = self.notification_tx.try_send(SubscriberNotification::Shutdown);
    }
}

impl EffectSubscriber {
    /// Creates a new effect subscriber.
    ///
    /// # Arguments
    ///
    /// * `actor_handle` - Handle to the state actor for queries.
    /// * `executor` - Executor for applying effects.
    ///
    /// # Returns
    ///
    /// A tuple of (subscriber, handle). The subscriber should be spawned
    /// as a background task, and the handle used to send notifications.
    #[must_use]
    pub fn new(
        actor_handle: StateActorHandle,
        executor: EffectExecutor,
    ) -> (Self, EffectSubscriberHandle) {
        let (notification_tx, notification_rx) = mpsc::channel(256);

        let subscriber = Self {
            actor_handle,
            executor,
            notification_rx,
            state: SubscriberState::new(),
        };

        let handle = EffectSubscriberHandle { notification_tx };

        (subscriber, handle)
    }

    /// Runs the subscriber event loop.
    ///
    /// This should be spawned as a background task:
    ///
    /// ```rust,ignore
    /// tauri::async_runtime::spawn(subscriber.run());
    /// ```
    pub async fn run(mut self) {
        log::debug!("Effect subscriber started");

        // Initialize with current state before processing notifications
        self.initialize().await;

        // Apply initial border colors based on focused workspace layout
        self.apply_initial_border_colors().await;

        while let Some(notification) = self.notification_rx.recv().await {
            match notification {
                SubscriberNotification::Shutdown => {
                    log::debug!("Effect subscriber received shutdown");
                    break;
                }
                notification => {
                    self.handle_notification(notification).await;
                }
            }
        }

        log::debug!("Effect subscriber stopped");
    }

    /// Handles a single notification.
    async fn handle_notification(&mut self, notification: SubscriberNotification) {
        log::debug!("tiling: subscriber received notification: {:?}", notification);

        // For layout changes that may trigger animations, signal cancellation
        // of any ongoing animation so the new one can take priority, then
        // immediately decrement to indicate we're now the active command.
        // This pattern ensures:
        // 1. Any running animation sees WAITING_COMMANDS > 0 and cancels
        // 2. Our animation sees WAITING_COMMANDS == 0 and runs normally
        let is_layout_change = matches!(notification, SubscriberNotification::LayoutChanged { .. });
        if is_layout_change {
            cancel_animation();
            begin_animation();
        }

        let effects = match notification {
            SubscriberNotification::LayoutChanged { workspace_id, user_triggered } => {
                log::debug!(
                    "tiling: subscriber handling LayoutChanged for workspace {workspace_id}"
                );
                self.handle_layout_changed(workspace_id, user_triggered).await
            }

            SubscriberNotification::FocusChanged => self.handle_focus_changed().await,

            SubscriberNotification::VisibilityChanged { workspace_id, visible } => {
                self.handle_visibility_changed(workspace_id, visible).await
            }

            SubscriberNotification::FloatingChanged { window_id, floating } => {
                self.handle_floating_changed(window_id, floating);
                Vec::new()
            }

            SubscriberNotification::WorkspaceLayoutChanged { workspace_id, layout } => {
                self.handle_workspace_layout_changed(workspace_id, layout);
                Vec::new()
            }

            SubscriberNotification::Shutdown => Vec::new(),
        };

        log::debug!("tiling: subscriber generated {} effects", effects.len());
        if !effects.is_empty() {
            let count = self.executor.execute_batch(effects);
            log::debug!("tiling: subscriber executed {count} effects");
        }
    }

    /// Handles a layout change notification.
    async fn handle_layout_changed(
        &mut self,
        workspace_id: Uuid,
        user_triggered: bool,
    ) -> Vec<TilingEffect> {
        log::debug!(
            "tiling: handle_layout_changed for workspace {workspace_id}, user_triggered={user_triggered}"
        );

        // Query the current layout for this workspace
        let layout_result =
            self.actor_handle.query(StateQuery::GetWindowLayout { workspace_id }).await;

        let Ok(QueryResult::Layout(new_positions)) = layout_result else {
            log::warn!("tiling: failed to query layout for workspace {workspace_id}");
            return Vec::new();
        };

        log::debug!(
            "tiling: queried layout for workspace {workspace_id}: {} windows",
            new_positions.len()
        );
        for (win_id, frame) in &new_positions {
            log::trace!("tiling:   window {} -> frame {:?}", win_id, frame);
        }

        // Store expected frames for minimum size detection before applying layout
        // This allows us to detect when windows fail to resize to their calculated positions
        if !new_positions.is_empty() {
            if let Err(e) = self.actor_handle.set_expected_frames(new_positions.clone()) {
                log::warn!("tiling: failed to set expected frames: {e}");
            }
        }

        // Update state and get the change
        let Some(change) = self.state.update_layout(workspace_id, new_positions, user_triggered)
        else {
            log::debug!("tiling: no actual layout change detected for workspace {workspace_id}");
            return Vec::new(); // No actual change
        };

        log::debug!(
            "tiling: layout change detected - old: {} windows, new: {} windows",
            change.old_positions.len(),
            change.new_positions.len()
        );

        // Convert change to effects
        effects_from_layout_change(&change)
    }

    /// Handles a focus change notification.
    async fn handle_focus_changed(&mut self) -> Vec<TilingEffect> {
        // Query the current focus state
        let focus_result = self.actor_handle.query(StateQuery::GetFocusState).await;

        let Ok(QueryResult::Focus(new_focus)) = focus_result else {
            log::warn!("Failed to query focus state");
            return Vec::new();
        };

        // Update state and get the change
        let Some(change) = self.state.update_focus(new_focus.clone()) else {
            return Vec::new(); // No actual change
        };

        // Query workspace layout and window floating state for borders
        let mut layout = LayoutType::Floating;
        let mut is_window_floating = false;

        if let Ok(QueryResult::Workspace(Some(workspace))) =
            self.actor_handle.query(StateQuery::GetFocusedWorkspace).await
        {
            layout = workspace.layout;
        }

        // Check if the focused window itself is floating
        if let Some(window_id) = new_focus.focused_window_id {
            if let Ok(QueryResult::Window(Some(window))) =
                self.actor_handle.query(StateQuery::GetWindow { id: window_id }).await
            {
                is_window_floating = window.is_floating;
            }
        }

        // Update borders via the simple API
        crate::modules::tiling::borders::on_focus_changed(layout, is_window_floating);

        // Generate effects for other systems (not borders - handled above)
        let is_monocle = layout == LayoutType::Monocle;
        let is_floating = layout == LayoutType::Floating || is_window_floating;
        effects_from_focus_change(&change, is_monocle, is_floating)
    }

    /// Handles a visibility change notification.
    async fn handle_visibility_changed(
        &mut self,
        workspace_id: Uuid,
        visible: bool,
    ) -> Vec<TilingEffect> {
        let mut effects = Vec::new();

        if visible {
            // Workspace became visible - need to apply layout and show borders
            // Query current layout
            let layout_result =
                self.actor_handle.query(StateQuery::GetWindowLayout { workspace_id }).await;

            if let Ok(QueryResult::Layout(positions)) = layout_result {
                // Apply layout to all windows (no animation since workspace just appeared)
                for (window_id, frame) in &positions {
                    effects.push(TilingEffect::SetWindowFrame {
                        window_id: *window_id,
                        frame: *frame,
                        animate: false,
                    });
                }

                // Show borders for all windows in this workspace
                let window_ids: Vec<u32> = positions.iter().map(|(id, _)| *id).collect();
                if !window_ids.is_empty() {
                    effects.push(TilingEffect::ShowBorders { window_ids });
                }

                // Update cached positions
                let _ = self.state.update_layout(workspace_id, positions, false);
            }
        } else {
            // Workspace became hidden - hide borders for its windows
            if let Some(positions) = self.state.layout_positions.get(&workspace_id) {
                let window_ids: Vec<u32> = positions.iter().map(|(id, _)| *id).collect();
                if !window_ids.is_empty() {
                    effects.push(TilingEffect::HideBorders { window_ids });
                }
            }
        }

        // Update visibility tracking
        if visible {
            // Query workspace to get screen_id and layout
            let ws_result =
                self.actor_handle.query(StateQuery::GetWorkspace { id: workspace_id }).await;

            if let Ok(QueryResult::Workspace(Some(ws))) = ws_result {
                self.state.visible_workspaces.insert(workspace_id, ws.screen_id);
                // Track workspace layout for monocle/floating detection
                self.state.update_workspace_layout(workspace_id, ws.layout);
            }
        } else {
            self.state.visible_workspaces.remove(&workspace_id);
        }

        effects
    }

    /// Handles a floating state change.
    fn handle_floating_changed(&mut self, window_id: u32, floating: bool) {
        self.state.set_window_floating(window_id, floating);
    }

    /// Handles a workspace layout type change.
    fn handle_workspace_layout_changed(&mut self, workspace_id: Uuid, layout: LayoutType) {
        self.state.update_workspace_layout(workspace_id, layout);
    }

    /// Initializes the subscriber with current state.
    ///
    /// Call this after creating the subscriber to sync with current state
    /// before starting the event loop.
    pub async fn initialize(&mut self) {
        // Query all visible workspaces and their layouts
        let ws_result = self.actor_handle.query(StateQuery::GetVisibleWorkspaces).await;

        if let Ok(QueryResult::Workspaces(workspaces)) = ws_result {
            for ws in &workspaces {
                self.state.visible_workspaces.insert(ws.id, ws.screen_id);
                self.state.update_workspace_layout(ws.id, ws.layout);

                // Query and cache layout for this workspace
                let layout_result = self
                    .actor_handle
                    .query(StateQuery::GetWindowLayout { workspace_id: ws.id })
                    .await;

                if let Ok(QueryResult::Layout(positions)) = layout_result {
                    self.state.layout_positions.insert(ws.id, positions);
                }
            }
        }

        // Query current focus
        let focus_result = self.actor_handle.query(StateQuery::GetFocusState).await;

        if let Ok(QueryResult::Focus(focus)) = focus_result {
            self.state.focus = focus;
        }

        // Query all windows to track floating state
        let windows_result = self.actor_handle.query(StateQuery::GetAllWindows).await;

        if let Ok(QueryResult::Windows(windows)) = windows_result {
            for window in windows {
                if window.is_floating {
                    self.state.floating_windows.insert(window.id);
                }
            }
        }

        log::debug!(
            "Effect subscriber initialized with {} visible workspaces",
            self.state.visible_workspaces.len()
        );
    }

    /// Applies initial border colors based on the focused workspace's layout.
    ///
    /// This should be called after `initialize()` to set up JankyBorders
    /// with the correct active color for the current state.
    async fn apply_initial_border_colors(&mut self) {
        let mut layout = LayoutType::Floating;
        let mut is_window_floating = false;

        if let Ok(QueryResult::Workspace(Some(workspace))) =
            self.actor_handle.query(StateQuery::GetFocusedWorkspace).await
        {
            layout = workspace.layout;
        }

        // Check if the focused window itself is floating
        if let Some(window_id) = self.state.focus.focused_window_id {
            if let Ok(QueryResult::Window(Some(window))) =
                self.actor_handle.query(StateQuery::GetWindow { id: window_id }).await
            {
                is_window_floating = window.is_floating;
            }
        }

        // Update borders via the simple API
        crate::modules::tiling::borders::on_focus_changed(layout, is_window_floating);
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subscriber_state_default() {
        let state = SubscriberState::new();
        assert!(state.layout_positions.is_empty());
        assert!(!state.focus.has_focus());
        assert!(state.visible_workspaces.is_empty());
    }

    #[test]
    fn test_subscriber_state_update_layout() {
        let mut state = SubscriberState::new();
        let ws_id = Uuid::now_v7();
        let positions = vec![(1, Rect::new(0.0, 0.0, 100.0, 100.0))];

        // First update should produce a change
        let change = state.update_layout(ws_id, positions.clone(), false);
        assert!(change.is_some());
        assert_eq!(change.unwrap().new_positions, positions);

        // Same update should not produce a change
        let change = state.update_layout(ws_id, positions, false);
        assert!(change.is_none());

        // Different update should produce a change
        let new_positions = vec![(1, Rect::new(50.0, 50.0, 100.0, 100.0))];
        let change = state.update_layout(ws_id, new_positions, false);
        assert!(change.is_some());
    }

    #[test]
    fn test_subscriber_state_update_focus() {
        let mut state = SubscriberState::new();

        // First update should produce a change
        let new_focus = FocusState {
            focused_window_id: Some(1),
            focused_workspace_id: Some(Uuid::now_v7()),
            focused_screen_id: Some(1),
        };
        let change = state.update_focus(new_focus.clone());
        assert!(change.is_some());

        // Same update should not produce a change
        let change = state.update_focus(new_focus.clone());
        assert!(change.is_none());

        // Different update should produce a change
        let new_focus2 = FocusState {
            focused_window_id: Some(2),
            ..new_focus
        };
        let change = state.update_focus(new_focus2);
        assert!(change.is_some());
    }

    #[test]
    fn test_subscriber_state_monocle_tracking() {
        let mut state = SubscriberState::new();
        let ws_id = Uuid::now_v7();

        assert!(!state.is_monocle(ws_id));

        state.update_workspace_layout(ws_id, LayoutType::Monocle);
        assert!(state.is_monocle(ws_id));

        state.update_workspace_layout(ws_id, LayoutType::Dwindle);
        assert!(!state.is_monocle(ws_id));
    }

    #[test]
    fn test_subscriber_state_floating_tracking() {
        let mut state = SubscriberState::new();

        assert!(!state.is_floating(1));

        state.set_window_floating(1, true);
        assert!(state.is_floating(1));

        state.set_window_floating(1, false);
        assert!(!state.is_floating(1));
    }

    #[test]
    fn test_subscriber_handle_send() {
        let (tx, mut rx) = mpsc::channel(10);
        let handle = EffectSubscriberHandle { notification_tx: tx };

        let ws_id = Uuid::now_v7();
        handle.notify_layout_changed(ws_id, true);

        let notification = rx.try_recv().unwrap();
        match notification {
            SubscriberNotification::LayoutChanged { workspace_id, user_triggered } => {
                assert_eq!(workspace_id, ws_id);
                assert!(user_triggered);
            }
            _ => panic!("Wrong notification type"),
        }
    }
}
