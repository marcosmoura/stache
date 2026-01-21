//! Handle for communicating with the state actor.
//!
//! The `StateActorHandle` provides a safe, cloneable interface for sending
//! messages to the state actor and subscribing to state changes.

use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use super::messages::{QueryResult, ResizeDimension, StateMessage, StateQuery, TargetScreen};

/// Error types for actor communication.
#[derive(Debug, thiserror::Error)]
pub enum ActorError {
    /// Failed to send message to actor.
    #[error("Failed to send message to actor: channel closed")]
    SendFailed,

    /// Failed to receive response from actor.
    #[error("Failed to receive response from actor: channel closed")]
    ReceiveFailed,

    /// Query timed out.
    #[error("Query timed out after {0:?}")]
    Timeout(Duration),
}

/// Handle for communicating with the state actor.
///
/// This handle is cheap to clone and can be shared across threads.
#[derive(Clone)]
pub struct StateActorHandle {
    sender: mpsc::Sender<StateMessage>,
}

impl StateActorHandle {
    /// Create a new handle with the given sender.
    pub(crate) const fn new(sender: mpsc::Sender<StateMessage>) -> Self { Self { sender } }

    // ========================================================================
    // Fire-and-forget sending
    // ========================================================================

    /// Send a message to the actor without waiting for delivery.
    ///
    /// This is non-blocking and will queue the message if the actor is busy.
    ///
    /// # Errors
    ///
    /// Returns [`ActorError::SendFailed`] if the channel is closed (actor has stopped).
    pub fn send(&self, msg: StateMessage) -> Result<(), ActorError> {
        self.sender.try_send(msg).map_err(|_| ActorError::SendFailed)
    }

    /// Send a message to the actor and wait for delivery.
    ///
    /// This is async and will wait if the channel buffer is full.
    ///
    /// # Errors
    ///
    /// Returns [`ActorError::SendFailed`] if the channel is closed.
    pub async fn send_async(&self, msg: StateMessage) -> Result<(), ActorError> {
        self.sender.send(msg).await.map_err(|_| ActorError::SendFailed)
    }

    // ========================================================================
    // Query methods
    // ========================================================================

    /// Execute a query and wait for the result.
    ///
    /// # Errors
    ///
    /// Returns [`ActorError::SendFailed`] if the channel is closed, or
    /// [`ActorError::ReceiveFailed`] if the response channel is closed.
    pub async fn query(&self, query: StateQuery) -> Result<QueryResult, ActorError> {
        let (tx, rx) = oneshot::channel();

        self.sender
            .send(StateMessage::Query { query, respond_to: tx })
            .await
            .map_err(|_| ActorError::SendFailed)?;

        rx.await.map_err(|_| ActorError::ReceiveFailed)
    }

    /// Execute a query with a timeout.
    ///
    /// # Errors
    ///
    /// Returns [`ActorError::Timeout`] if the query doesn't complete in time,
    /// or any error from [`Self::query`].
    pub async fn query_timeout(
        &self,
        query: StateQuery,
        timeout: Duration,
    ) -> Result<QueryResult, ActorError> {
        tokio::time::timeout(timeout, self.query(query))
            .await
            .map_err(|_| ActorError::Timeout(timeout))?
    }

    // ========================================================================
    // Convenience query methods
    // ========================================================================

    /// Get all screens.
    ///
    /// # Errors
    ///
    /// Returns an error if communication with the actor fails.
    pub async fn get_all_screens(&self) -> Result<QueryResult, ActorError> {
        self.query(StateQuery::GetAllScreens).await
    }

    /// Get all workspaces.
    ///
    /// # Errors
    ///
    /// Returns an error if communication with the actor fails.
    pub async fn get_all_workspaces(&self) -> Result<QueryResult, ActorError> {
        self.query(StateQuery::GetAllWorkspaces).await
    }

    /// Get all windows.
    ///
    /// # Errors
    ///
    /// Returns an error if communication with the actor fails.
    pub async fn get_all_windows(&self) -> Result<QueryResult, ActorError> {
        self.query(StateQuery::GetAllWindows).await
    }

    /// Get the current focus state.
    ///
    /// # Errors
    ///
    /// Returns an error if communication with the actor fails.
    pub async fn get_focus_state(&self) -> Result<QueryResult, ActorError> {
        self.query(StateQuery::GetFocusState).await
    }

    /// Get whether tiling is enabled.
    ///
    /// # Errors
    ///
    /// Returns an error if communication with the actor fails.
    pub async fn get_enabled(&self) -> Result<QueryResult, ActorError> {
        self.query(StateQuery::GetEnabled).await
    }

    /// Get a screen by ID.
    ///
    /// # Errors
    ///
    /// Returns an error if communication with the actor fails.
    pub async fn get_screen(&self, id: u32) -> Result<QueryResult, ActorError> {
        self.query(StateQuery::GetScreen { id }).await
    }

    /// Get a workspace by ID.
    ///
    /// # Errors
    ///
    /// Returns an error if communication with the actor fails.
    pub async fn get_workspace(&self, id: uuid::Uuid) -> Result<QueryResult, ActorError> {
        self.query(StateQuery::GetWorkspace { id }).await
    }

    /// Get a workspace by name.
    ///
    /// # Errors
    ///
    /// Returns an error if communication with the actor fails.
    pub async fn get_workspace_by_name(&self, name: &str) -> Result<QueryResult, ActorError> {
        self.query(StateQuery::GetWorkspaceByName { name: name.to_string() }).await
    }

    /// Get a window by ID.
    ///
    /// # Errors
    ///
    /// Returns an error if communication with the actor fails.
    pub async fn get_window(&self, id: u32) -> Result<QueryResult, ActorError> {
        self.query(StateQuery::GetWindow { id }).await
    }

    /// Get the focused workspace.
    ///
    /// # Errors
    ///
    /// Returns an error if communication with the actor fails.
    pub async fn get_focused_workspace(&self) -> Result<QueryResult, ActorError> {
        self.query(StateQuery::GetFocusedWorkspace).await
    }

    /// Get the focused window.
    ///
    /// # Errors
    ///
    /// Returns an error if communication with the actor fails.
    pub async fn get_focused_window(&self) -> Result<QueryResult, ActorError> {
        self.query(StateQuery::GetFocusedWindow).await
    }

    // ========================================================================
    // Convenience command methods
    // ========================================================================

    /// Switch to a workspace by name.
    ///
    /// # Errors
    ///
    /// Returns [`ActorError::SendFailed`] if the channel is closed.
    pub fn switch_workspace(&self, name: &str) -> Result<(), ActorError> {
        self.send(StateMessage::SwitchWorkspace { name: name.to_string() })
    }

    /// Set the layout for a workspace.
    ///
    /// # Errors
    ///
    /// Returns [`ActorError::SendFailed`] if the channel is closed.
    pub fn set_layout(
        &self,
        workspace_id: uuid::Uuid,
        layout: crate::modules::tiling::state::LayoutType,
    ) -> Result<(), ActorError> {
        self.send(StateMessage::SetLayout { workspace_id, layout })
    }

    /// Toggle floating for a window.
    ///
    /// # Errors
    ///
    /// Returns [`ActorError::SendFailed`] if the channel is closed.
    pub fn toggle_floating(&self, window_id: u32) -> Result<(), ActorError> {
        self.send(StateMessage::ToggleFloating { window_id })
    }

    /// Enable or disable tiling.
    ///
    /// # Errors
    ///
    /// Returns [`ActorError::SendFailed`] if the channel is closed.
    pub fn set_enabled(&self, enabled: bool) -> Result<(), ActorError> {
        self.send(StateMessage::SetEnabled { enabled })
    }

    /// Focus a window in a direction.
    ///
    /// Supports spatial directions (up/down/left/right) and cycling (next/previous).
    ///
    /// # Errors
    ///
    /// Returns [`ActorError::SendFailed`] if the channel is closed.
    pub fn focus_window(&self, direction: super::FocusDirection) -> Result<(), ActorError> {
        self.send(StateMessage::FocusWindow { direction })
    }

    /// Swap focused window with another in a direction.
    ///
    /// Supports spatial directions (up/down/left/right) and cycling (next/previous).
    ///
    /// # Errors
    ///
    /// Returns [`ActorError::SendFailed`] if the channel is closed.
    pub fn swap_window_in_direction(
        &self,
        direction: super::FocusDirection,
    ) -> Result<(), ActorError> {
        self.send(StateMessage::SwapWindowInDirection { direction })
    }

    /// Balance split ratios in the focused workspace.
    ///
    /// # Errors
    ///
    /// Returns [`ActorError::SendFailed`] if the channel is closed.
    pub fn balance_workspace(&self, workspace_id: uuid::Uuid) -> Result<(), ActorError> {
        self.send(StateMessage::BalanceWorkspace { workspace_id })
    }

    /// Cycle through layouts for a workspace.
    ///
    /// # Errors
    ///
    /// Returns [`ActorError::SendFailed`] if the channel is closed.
    pub fn cycle_layout(&self, workspace_id: uuid::Uuid) -> Result<(), ActorError> {
        self.send(StateMessage::CycleLayout { workspace_id })
    }

    /// Send focused window to another screen.
    ///
    /// Supports "main"/"primary", "secondary", or display name.
    ///
    /// # Errors
    ///
    /// Returns [`ActorError::SendFailed`] if the channel is closed.
    pub fn send_window_to_screen(&self, target_screen: &str) -> Result<(), ActorError> {
        self.send(StateMessage::SendWindowToScreen {
            target_screen: TargetScreen::parse(target_screen),
        })
    }

    /// Send focused workspace to another screen.
    ///
    /// Supports "main"/"primary", "secondary", or display name.
    ///
    /// # Errors
    ///
    /// Returns [`ActorError::SendFailed`] if the channel is closed.
    pub fn send_workspace_to_screen(&self, target_screen: &str) -> Result<(), ActorError> {
        self.send(StateMessage::SendWorkspaceToScreen {
            target_screen: TargetScreen::parse(target_screen),
        })
    }

    /// Resize the focused window in a dimension.
    ///
    /// Adjusts split ratios to resize the window by the specified amount.
    ///
    /// # Arguments
    ///
    /// * `dimension` - "width" or "height"
    /// * `amount` - Pixels to add (positive) or remove (negative)
    ///
    /// # Errors
    ///
    /// Returns [`ActorError::SendFailed`] if the channel is closed.
    /// Returns `Ok(())` but logs a warning if the dimension is invalid.
    pub fn resize_focused_window(&self, dimension: &str, amount: i32) -> Result<(), ActorError> {
        let Some(dim) = ResizeDimension::parse(dimension) else {
            log::warn!("resize_focused_window: invalid dimension '{dimension}'");
            return Ok(());
        };
        self.send(StateMessage::ResizeFocusedWindow { dimension: dim, amount })
    }

    /// Apply a floating preset to the focused window.
    ///
    /// Presets define window size and position (centered, half-screen, etc.).
    /// Only works when the workspace is in Floating layout mode.
    ///
    /// # Errors
    ///
    /// Returns [`ActorError::SendFailed`] if the channel is closed.
    pub fn apply_preset(&self, preset_name: &str) -> Result<(), ActorError> {
        self.send(StateMessage::ApplyPreset {
            preset: preset_name.to_string(),
        })
    }

    /// Request shutdown of the actor.
    ///
    /// # Errors
    ///
    /// Returns [`ActorError::SendFailed`] if the channel is closed.
    pub fn shutdown(&self) -> Result<(), ActorError> { self.send(StateMessage::Shutdown) }

    /// Set expected frames for windows (for minimum size detection).
    ///
    /// This should be called after computing layout but before effects are applied,
    /// so that when windows report their actual positions we can detect minimum
    /// size violations.
    ///
    /// # Errors
    ///
    /// Returns [`ActorError::SendFailed`] if the channel is closed.
    pub fn set_expected_frames(
        &self,
        frames: Vec<(u32, crate::modules::tiling::state::Rect)>,
    ) -> Result<(), ActorError> {
        self.send(StateMessage::SetExpectedFrames { frames })
    }

    // ========================================================================
    // Channel state
    // ========================================================================

    /// Check if the actor is still running (channel is open).
    #[must_use]
    pub fn is_alive(&self) -> bool { !self.sender.is_closed() }

    /// Get the number of messages waiting in the queue.
    #[must_use]
    pub fn pending_messages(&self) -> usize { self.sender.max_capacity() - self.sender.capacity() }
}

impl std::fmt::Debug for StateActorHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateActorHandle")
            .field("alive", &self.is_alive())
            .field("pending", &self.pending_messages())
            .finish()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_handle_creation() {
        let (tx, _rx) = mpsc::channel(16);
        let handle = StateActorHandle::new(tx);
        assert!(handle.is_alive());
    }

    #[tokio::test]
    async fn test_handle_closed_detection() {
        let (tx, rx) = mpsc::channel(16);
        let handle = StateActorHandle::new(tx);
        assert!(handle.is_alive());

        drop(rx);
        // After dropping receiver, channel is closed
        assert!(!handle.is_alive());
    }

    #[tokio::test]
    async fn test_send_to_closed_channel() {
        let (tx, rx) = mpsc::channel(16);
        let handle = StateActorHandle::new(tx);
        drop(rx);

        let result = handle.send(StateMessage::Shutdown);
        assert!(result.is_err());
    }
}
