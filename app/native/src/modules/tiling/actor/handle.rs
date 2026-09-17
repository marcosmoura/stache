//! Handle for communicating with the state actor.
//!
//! The `StateActorHandle` provides a safe, cloneable interface for sending
//! messages to the state actor and subscribing to state changes.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};

use super::messages::{
    GeometryUpdate, GeometryUpdateType, QueryResult, ResizeDimension, StateMessage, StateQuery,
    TargetScreen,
};
use crate::modules::tiling::identity::WindowTarget;
use crate::modules::tiling::visibility::VisibilityRegistry;

/// Error types for actor communication.
#[derive(Debug, thiserror::Error)]
pub enum ActorError {
    /// Failed to send message to actor.
    #[error("Failed to send message to actor: channel closed")]
    SendFailed,

    /// The bounded overflow mailbox is full.
    #[error("Actor overflow mailbox is full")]
    OverflowFull,

    /// Failed to receive response from actor.
    #[error("Failed to receive response from actor: channel closed")]
    ReceiveFailed,

    /// Query timed out.
    #[error("Query timed out after {0:?}")]
    Timeout(Duration),
}

/// Work retained only while the actor's bounded primary channel is saturated.
///
/// Geometry is compacted by exact window identity. The reliable queue is also
/// bounded: producers must observe backpressure rather than turning a stalled
/// actor into an unbounded allocation sink.
const MAX_RELIABLE_OVERFLOW: usize = 1024;
#[derive(Default)]
pub(super) struct PendingActorMessages {
    reliable: VecDeque<StateMessage>,
    geometry: HashMap<WindowTarget, GeometryUpdate>,
    shutdown_requested: bool,
}

impl PendingActorMessages {
    fn push(&mut self, message: StateMessage) -> bool {
        if matches!(message, StateMessage::Shutdown) {
            self.shutdown_requested = true;
            return true;
        }

        match message {
            StateMessage::BatchedGeometryUpdates(updates) => {
                for update in updates {
                    self.merge_geometry(update);
                }
            }
            StateMessage::WindowMoved { window_id, identity, frame } => {
                self.merge_geometry(GeometryUpdate {
                    window_id,
                    identity,
                    frame,
                    update_type: GeometryUpdateType::Move,
                });
            }
            StateMessage::WindowResized { window_id, identity, frame } => {
                self.merge_geometry(GeometryUpdate {
                    window_id,
                    identity,
                    frame,
                    update_type: GeometryUpdateType::Resize,
                });
            }
            message => {
                if self.reliable.len() == MAX_RELIABLE_OVERFLOW {
                    tracing::warn!(
                        message = message.name(),
                        capacity = MAX_RELIABLE_OVERFLOW,
                        "tiling: actor reliable overflow is full"
                    );
                    return false;
                }
                self.reliable.push_back(message);
            }
        }
        true
    }

    fn merge_geometry(&mut self, update: GeometryUpdate) {
        let target = WindowTarget {
            identity: update.identity,
            window_id: update.window_id,
        };
        self.geometry
            .entry(target)
            .and_modify(|existing| {
                existing.frame = update.frame;
                existing.update_type = match (existing.update_type, update.update_type) {
                    (GeometryUpdateType::MoveResize, _)
                    | (_, GeometryUpdateType::MoveResize)
                    | (GeometryUpdateType::Move, GeometryUpdateType::Resize)
                    | (GeometryUpdateType::Resize, GeometryUpdateType::Move) => {
                        GeometryUpdateType::MoveResize
                    }
                    (kind, _) => kind,
                };
            })
            .or_insert(update);
    }

    pub(crate) fn pop(&mut self) -> Option<StateMessage> {
        if self.take_shutdown().is_some() {
            return Some(StateMessage::Shutdown);
        }
        if let Some(message) = self.reliable.pop_front() {
            return Some(message);
        }
        (!self.geometry.is_empty()).then(|| {
            StateMessage::BatchedGeometryUpdates(
                self.geometry.drain().map(|(_, update)| update).collect(),
            )
        })
    }

    fn is_empty(&self) -> bool {
        !self.shutdown_requested && self.reliable.is_empty() && self.geometry.is_empty()
    }

    /// Removes and returns the shutdown request when it must preempt queued
    /// work. `None` means normal FIFO delivery should continue.
    pub(crate) fn take_shutdown(&mut self) -> Option<()> {
        self.shutdown_requested.then(|| {
            self.shutdown_requested = false;
        })
    }
}

/// Handle for communicating with the state actor.
///
/// This handle is cheap to clone and can be shared across threads.
#[derive(Clone)]
pub struct StateActorHandle {
    sender: mpsc::Sender<StateMessage>,
    pub(super) pending: Arc<Mutex<PendingActorMessages>>,
    #[allow(dead_code)] // sealed/drained by the 19D cutover
    registry: Arc<VisibilityRegistry>,
}

impl StateActorHandle {
    /// Create a new handle with the given sender.
    #[allow(dead_code)] // legacy constructor; 19D spawns via spawn_with_registry
    pub(crate) fn new(sender: mpsc::Sender<StateMessage>) -> Self {
        Self::new_with_registry(sender, Arc::new(VisibilityRegistry::default()))
    }

    /// Create a handle sharing an explicit visibility registry with the actor.
    pub(crate) fn new_with_registry(
        sender: mpsc::Sender<StateMessage>,
        registry: Arc<VisibilityRegistry>,
    ) -> Self {
        Self {
            sender,
            pending: Arc::new(Mutex::new(PendingActorMessages::default())),
            registry,
        }
    }

    /// Atomically seals the registry and drains all owned identities for
    /// restoration. The actor channel MUST be alive when called (seal happens
    /// before tiling shutdown closes the actor).
    #[allow(dead_code)] // called by the 19D restore path
    pub(crate) fn seal_and_drain_visibility(
        &self,
    ) -> Vec<crate::modules::tiling::identity::AppIdentity> {
        self.registry.seal_and_drain()
    }

    // ========================================================================
    // Fire-and-forget sending
    // ========================================================================

    /// Send a message to the actor without waiting for delivery.
    ///
    /// This is non-blocking and will queue the message if the actor is busy.
    ///
    /// # Errors
    ///
    /// On saturation, retains the message in a bounded semantic overflow
    /// mailbox. Returns [`ActorError::OverflowFull`] when that mailbox is
    /// full, or [`ActorError::SendFailed`] once the actor has stopped.
    pub fn send(&self, msg: StateMessage) -> Result<(), ActorError> {
        if self.sender.is_closed() {
            return Err(ActorError::SendFailed);
        }
        let mut pending = self.pending.lock();
        if matches!(msg, StateMessage::Shutdown) {
            pending.push(msg);
            // The actor may be idle in `recv`. Retaining shutdown in the
            // semantic mailbox preserves preemption, while this wake-up lets
            // the actor observe it without waiting for another event.
            match self.sender.try_send(StateMessage::Shutdown) {
                Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
                Err(mpsc::error::TrySendError::Closed(_)) => return Err(ActorError::SendFailed),
            }
            drop(pending);
            return Ok(());
        }
        if !pending.is_empty() {
            if !pending.push(msg) {
                return Err(ActorError::OverflowFull);
            }
            drop(pending);
            return Ok(());
        }

        match self.sender.try_send(msg) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(message)) => {
                if !pending.push(message) {
                    return Err(ActorError::OverflowFull);
                }
                drop(pending);
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                drop(pending);
                Err(ActorError::SendFailed)
            }
        }
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

        // Route queries through the same semantic mailbox as commands. A raw
        // sender call could otherwise overtake state updates retained during a
        // saturated burst and observe stale actor state.
        self.send(StateMessage::Query { query, respond_to: tx })?;

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
            tracing::warn!("resize_focused_window: invalid dimension '{dimension}'");
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
        frames: Vec<(
            crate::modules::tiling::identity::WindowTarget,
            crate::modules::tiling::state::Rect,
        )>,
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

    #[test]
    fn saturated_channel_retains_reliable_messages_and_prioritizes_shutdown() {
        let (tx, mut rx) = mpsc::channel(1);
        let handle = StateActorHandle::new(tx);

        handle.send(StateMessage::SetEnabled { enabled: true }).unwrap();
        handle.send(StateMessage::SetEnabled { enabled: false }).unwrap();

        assert!(matches!(
            rx.try_recv(),
            Ok(StateMessage::SetEnabled { enabled: true })
        ));
        assert!(matches!(
            handle.pending.lock().pop(),
            Some(StateMessage::SetEnabled { enabled: false })
        ));

        handle.shutdown().unwrap();
        assert!(matches!(
            handle.pending.lock().pop(),
            Some(StateMessage::Shutdown)
        ));
    }

    #[test]
    fn reliable_overflow_has_a_fixed_capacity() {
        let (tx, _rx) = mpsc::channel(1);
        let handle = StateActorHandle::new(tx);
        handle.send(StateMessage::SetEnabled { enabled: true }).unwrap();

        for _ in 0..MAX_RELIABLE_OVERFLOW {
            handle.send(StateMessage::SetEnabled { enabled: false }).unwrap();
        }
        assert!(handle.send(StateMessage::SetEnabled { enabled: false }).is_err());
        assert_eq!(handle.pending.lock().reliable.len(), MAX_RELIABLE_OVERFLOW);
    }

    #[test]
    fn shutdown_wakes_an_idle_actor() {
        let (tx, mut rx) = mpsc::channel(1);
        let handle = StateActorHandle::new(tx);

        handle.shutdown().unwrap();

        assert!(matches!(rx.try_recv(), Ok(StateMessage::Shutdown)));
    }
}
