//! Error types for the tiling window manager.
//!
//! This module provides a unified error type for all tiling operations,
//! enabling detailed error reporting and proper error propagation throughout
//! the tiling system.
//!
//! # Error Categories
//!
//! - **Initialization errors**: Manager not initialized, permissions missing
//! - **Lookup errors**: Workspace, window, or screen not found
//! - **Accessibility errors**: macOS Accessibility API failures
//! - **Window operation errors**: Move, resize, focus, hide/show failures
//! - **Observer errors**: Event observation system failures
//! - **Animation errors**: Animation interruption or failure
//!
//! # Example
//!
//! ```rust,ignore
//! use crate::tiling::error::{TilingError, TilingResult};
//!
//! fn focus_window(id: u32) -> TilingResult<()> {
//!     let element = get_ax_element(id)
//!         .ok_or(TilingError::WindowNotFound(id))?;
//!
//!     set_focused_element(&element)
//!         .map_err(|code| TilingError::accessibility(code, "Failed to focus window"))
//! }
//! ```

use std::fmt;

/// Result type alias for tiling operations.
pub type TilingResult<T> = Result<T, TilingError>;

/// Errors that can occur during tiling window management operations.
#[derive(Debug, Clone)]
pub enum TilingError {
    /// The tiling manager has not been initialized.
    ///
    /// This typically occurs when trying to perform operations before
    /// `tiling::init()` has been called or when tiling is disabled.
    NotInitialized,

    /// A workspace with the given name was not found.
    ///
    /// This can occur when:
    /// - Referencing a workspace that doesn't exist in the configuration
    /// - The workspace was removed during operation
    WorkspaceNotFound(String),

    /// A window with the given ID was not found.
    ///
    /// Window IDs are assigned by macOS and may become invalid when:
    /// - The window is closed
    /// - The application terminates
    /// - The window ID was never valid
    WindowNotFound(u32),

    /// A screen with the given identifier was not found.
    ///
    /// Screen identifiers can be:
    /// - "main" for the primary display
    /// - "secondary" for non-primary displays
    /// - Display name (e.g., "DELL U2719D")
    /// - Display ID string
    ScreenNotFound(String),

    /// An error occurred in the macOS Accessibility API.
    ///
    /// Contains the AX error code and a descriptive message.
    /// Common error codes:
    /// - -25200: Not authorized (accessibility permissions needed)
    /// - -25201: Action unsupported
    /// - -25202: Notification unsupported
    /// - -25203: Invalid UI element
    /// - -25204: Attribute unsupported
    /// - -25205: Can't complete action
    AccessibilityError {
        /// The AX error code returned by the API.
        code: i32,
        /// A human-readable description of the error.
        message: String,
    },

    /// A window operation (move, resize, focus, etc.) failed.
    ///
    /// This is a catch-all for window manipulation errors that don't
    /// fall into other categories.
    WindowOperation(String),

    /// An error occurred in the window event observer system.
    ///
    /// This includes failures to:
    /// - Create `AXObserver` instances
    /// - Register for notifications
    /// - Process incoming events
    Observer(String),

    /// An animation was cancelled before completion.
    ///
    /// This occurs when a new command arrives while an animation
    /// is in progress, causing the current animation to be interrupted.
    AnimationCancelled,

    /// A null pointer was returned from an FFI call.
    ///
    /// This typically indicates a system resource allocation failure
    /// or an invalid parameter passed to a macOS API.
    NullPointer(String),

    /// A timeout occurred waiting for an operation to complete.
    ///
    /// Contains the operation name and timeout duration in milliseconds.
    Timeout {
        /// Description of the operation that timed out.
        operation: String,
        /// The timeout duration in milliseconds.
        timeout_ms: u64,
    },

    /// An error occurred during IPC communication.
    ///
    /// This can happen with `JankyBorders` Mach IPC or `NSDistributedNotificationCenter`.
    IpcError(String),
}

impl TilingError {
    /// Creates an accessibility error with the given code and message.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let error = TilingError::accessibility(-25203, "Invalid UI element");
    /// ```
    #[must_use]
    pub fn accessibility(code: i32, message: impl Into<String>) -> Self {
        Self::AccessibilityError { code, message: message.into() }
    }

    /// Creates a window operation error with the given message.
    #[must_use]
    pub fn window_op(message: impl Into<String>) -> Self { Self::WindowOperation(message.into()) }

    /// Creates an observer error with the given message.
    #[must_use]
    pub fn observer(message: impl Into<String>) -> Self { Self::Observer(message.into()) }

    /// Creates a null pointer error for the given FFI function.
    #[must_use]
    pub fn null_pointer(function: impl Into<String>) -> Self { Self::NullPointer(function.into()) }

    /// Creates a timeout error.
    #[must_use]
    pub fn timeout(operation: impl Into<String>, timeout_ms: u64) -> Self {
        Self::Timeout {
            operation: operation.into(),
            timeout_ms,
        }
    }

    /// Creates an IPC error with the given message.
    #[must_use]
    pub fn ipc(message: impl Into<String>) -> Self { Self::IpcError(message.into()) }

    /// Returns `true` if this error indicates the tiling system is not available.
    ///
    /// This can be used to gracefully handle cases where tiling is disabled
    /// or not properly initialized.
    #[must_use]
    pub const fn is_not_available(&self) -> bool { matches!(self, Self::NotInitialized) }

    /// Returns `true` if this error indicates a resource was not found.
    #[must_use]
    pub const fn is_not_found(&self) -> bool {
        matches!(
            self,
            Self::WorkspaceNotFound(_) | Self::WindowNotFound(_) | Self::ScreenNotFound(_)
        )
    }

    /// Returns `true` if this error is related to accessibility permissions.
    #[must_use]
    pub const fn is_permission_error(&self) -> bool {
        matches!(self, Self::AccessibilityError { code: -25200, .. })
    }

    /// Returns `true` if this error is transient and the operation might succeed on retry.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Timeout { .. } | Self::AnimationCancelled | Self::IpcError(_)
        )
    }

    /// Returns the AX error code if this is an accessibility error.
    #[must_use]
    pub const fn ax_error_code(&self) -> Option<i32> {
        if let Self::AccessibilityError { code, .. } = self {
            Some(*code)
        } else {
            None
        }
    }
}

impl fmt::Display for TilingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInitialized => {
                write!(f, "Tiling manager not initialized")
            }
            Self::WorkspaceNotFound(name) => {
                write!(f, "Workspace '{name}' not found")
            }
            Self::WindowNotFound(id) => {
                write!(f, "Window {id} not found")
            }
            Self::ScreenNotFound(name) => {
                write!(f, "Screen '{name}' not found")
            }
            Self::AccessibilityError { code, message } => {
                write!(f, "Accessibility error ({code}): {message}")
            }
            Self::WindowOperation(msg) => {
                write!(f, "Window operation failed: {msg}")
            }
            Self::Observer(msg) => {
                write!(f, "Observer error: {msg}")
            }
            Self::AnimationCancelled => {
                write!(f, "Animation was cancelled")
            }
            Self::NullPointer(func) => {
                write!(f, "Null pointer returned from {func}")
            }
            Self::Timeout { operation, timeout_ms } => {
                write!(f, "Timeout after {timeout_ms}ms: {operation}")
            }
            Self::IpcError(msg) => {
                write!(f, "IPC error: {msg}")
            }
        }
    }
}

impl std::error::Error for TilingError {}

// ============================================================================
// Conversions
// ============================================================================

impl From<String> for TilingError {
    fn from(s: String) -> Self { Self::WindowOperation(s) }
}

impl From<&str> for TilingError {
    fn from(s: &str) -> Self { Self::WindowOperation(s.to_string()) }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        assert_eq!(
            TilingError::NotInitialized.to_string(),
            "Tiling manager not initialized"
        );

        assert_eq!(
            TilingError::WorkspaceNotFound("coding".to_string()).to_string(),
            "Workspace 'coding' not found"
        );

        assert_eq!(
            TilingError::WindowNotFound(123).to_string(),
            "Window 123 not found"
        );

        assert_eq!(
            TilingError::ScreenNotFound("main".to_string()).to_string(),
            "Screen 'main' not found"
        );

        assert_eq!(
            TilingError::accessibility(-25203, "Invalid element").to_string(),
            "Accessibility error (-25203): Invalid element"
        );
    }

    #[test]
    fn test_error_constructors() {
        let ax_err = TilingError::accessibility(-25200, "Not authorized");
        assert!(matches!(ax_err, TilingError::AccessibilityError {
            code: -25200,
            ..
        }));

        let win_err = TilingError::window_op("Failed to move");
        assert!(matches!(win_err, TilingError::WindowOperation(_)));

        let obs_err = TilingError::observer("Callback failed");
        assert!(matches!(obs_err, TilingError::Observer(_)));

        let null_err = TilingError::null_pointer("AXUIElementCreateApplication");
        assert!(matches!(null_err, TilingError::NullPointer(_)));

        let timeout_err = TilingError::timeout("Window ready", 25);
        assert!(matches!(timeout_err, TilingError::Timeout {
            timeout_ms: 25,
            ..
        }));

        let ipc_err = TilingError::ipc("Mach port unavailable");
        assert!(matches!(ipc_err, TilingError::IpcError(_)));
    }

    #[test]
    fn test_error_predicates() {
        assert!(TilingError::NotInitialized.is_not_available());
        assert!(!TilingError::WindowNotFound(1).is_not_available());

        assert!(TilingError::WorkspaceNotFound("x".into()).is_not_found());
        assert!(TilingError::WindowNotFound(1).is_not_found());
        assert!(TilingError::ScreenNotFound("x".into()).is_not_found());
        assert!(!TilingError::NotInitialized.is_not_found());

        assert!(TilingError::accessibility(-25200, "Not authorized").is_permission_error());
        assert!(!TilingError::accessibility(-25203, "Invalid").is_permission_error());

        assert!(TilingError::AnimationCancelled.is_transient());
        assert!(TilingError::timeout("op", 100).is_transient());
        assert!(TilingError::ipc("error").is_transient());
        assert!(!TilingError::NotInitialized.is_transient());
    }

    #[test]
    fn test_ax_error_code() {
        let ax_err = TilingError::accessibility(-25203, "Invalid");
        assert_eq!(ax_err.ax_error_code(), Some(-25203));

        assert_eq!(TilingError::NotInitialized.ax_error_code(), None);
    }

    #[test]
    fn test_from_string() {
        let err: TilingError = "Operation failed".into();
        assert!(matches!(err, TilingError::WindowOperation(_)));

        let err: TilingError = String::from("Another error").into();
        assert!(matches!(err, TilingError::WindowOperation(_)));
    }
}
