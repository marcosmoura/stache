//! CLI error types.

use std::fmt;

/// Errors that can occur during CLI execution.
#[derive(Debug)]
pub enum CliError {
    /// The desktop app is not running.
    DesktopAppNotRunning,
    /// Failed to connect to the desktop app.
    ConnectionFailed(String),
    /// Failed to send message to the desktop app.
    SendFailed(String),
    /// Invalid wallpaper action.
    InvalidWallpaperAction(String),
    /// IO error.
    Io(std::io::Error),
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DesktopAppNotRunning => {
                write!(f, "Barba desktop app is not running. Please start it first.")
            }
            Self::ConnectionFailed(msg) => {
                write!(f, "Failed to connect to Barba desktop app: {msg}")
            }
            Self::SendFailed(msg) => {
                write!(f, "Failed to send command to Barba desktop app: {msg}")
            }
            Self::InvalidWallpaperAction(action) => {
                write!(
                    f,
                    "Invalid wallpaper action: '{action}'. Expected: next, previous, random, or an index number"
                )
            }
            Self::Io(err) => write!(f, "IO error: {err}"),
        }
    }
}

impl std::error::Error for CliError {}

impl From<std::io::Error> for CliError {
    fn from(err: std::io::Error) -> Self { Self::Io(err) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_desktop_app_not_running_display() {
        let err = CliError::DesktopAppNotRunning;
        let msg = err.to_string();
        assert!(msg.contains("not running"));
    }

    #[test]
    fn test_connection_failed_display() {
        let err = CliError::ConnectionFailed("timeout".to_string());
        let msg = err.to_string();
        assert!(msg.contains("timeout"));
        assert!(msg.contains("connect"));
    }

    #[test]
    fn test_send_failed_display() {
        let err = CliError::SendFailed("network error".to_string());
        let msg = err.to_string();
        assert!(msg.contains("network error"));
        assert!(msg.contains("send"));
    }

    #[test]
    fn test_invalid_wallpaper_action_display() {
        let err = CliError::InvalidWallpaperAction("bad-action".to_string());
        let msg = err.to_string();
        assert!(msg.contains("bad-action"));
        assert!(msg.contains("Invalid wallpaper action"));
    }

    #[test]
    fn test_io_error_display() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = CliError::Io(io_err);
        let msg = err.to_string();
        assert!(msg.contains("IO error"));
    }

    #[test]
    fn test_io_error_from_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied");
        let err: CliError = io_err.into();
        assert!(matches!(err, CliError::Io(_)));
    }

    #[test]
    fn test_error_is_debug() {
        let err = CliError::DesktopAppNotRunning;
        let debug_str = format!("{err:?}");
        assert!(debug_str.contains("DesktopAppNotRunning"));
    }
}
