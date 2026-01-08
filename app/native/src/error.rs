//! Error types for Stache.
//!
//! This module provides unified error types used throughout the application.
//! These types implement the necessary traits to be returned from Tauri commands.

use std::fmt;

use serde::Serialize;

/// Errors that can occur during application execution.
///
/// This enum implements `Serialize` and `Into<tauri::ipc::InvokeError>` to be
/// used as a return type for Tauri commands, providing structured error information
/// to the frontend.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "message")]
pub enum StacheError {
    /// Invalid command arguments.
    InvalidArguments(String),
    /// Cache operation failed.
    CacheError(String),
    /// Audio operation failed.
    AudioError(String),
    /// Wallpaper operation failed.
    WallpaperError(String),
    /// Configuration error.
    ConfigError(String),
    /// IPC communication error.
    IpcError(String),
    /// IO error.
    IoError(String),
    /// Battery operation failed.
    BatteryError(String),
    /// Hyprspace/AeroSpace operation failed.
    HyprspaceError(String),
    /// Shell command execution failed.
    ShellError(String),
    /// Generic command error.
    CommandError(String),
}

impl fmt::Display for StacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CacheError(msg) => write!(f, "Cache error: {msg}"),
            Self::AudioError(msg) => write!(f, "Audio error: {msg}"),
            Self::WallpaperError(msg) => write!(f, "Wallpaper error: {msg}"),
            Self::ConfigError(msg) => write!(f, "Configuration error: {msg}"),
            Self::IpcError(msg) => write!(f, "IPC error: {msg}"),
            Self::IoError(msg) => write!(f, "IO error: {msg}"),
            Self::BatteryError(msg) => write!(f, "Battery error: {msg}"),
            Self::HyprspaceError(msg) => write!(f, "Hyprspace error: {msg}"),
            Self::ShellError(msg) => write!(f, "Shell error: {msg}"),
            Self::InvalidArguments(msg) | Self::CommandError(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for StacheError {}

impl From<std::io::Error> for StacheError {
    fn from(err: std::io::Error) -> Self { Self::IoError(err.to_string()) }
}

impl From<serde_json::Error> for StacheError {
    fn from(err: serde_json::Error) -> Self { Self::CommandError(err.to_string()) }
}

impl From<String> for StacheError {
    fn from(msg: String) -> Self { Self::CommandError(msg) }
}

impl From<&str> for StacheError {
    fn from(msg: &str) -> Self { Self::CommandError(msg.to_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invalid_arguments_display() {
        let err = StacheError::InvalidArguments("Cannot specify both path and random".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Cannot specify both path and random"));
    }

    #[test]
    fn test_cache_error_display() {
        let err = StacheError::CacheError("Failed to remove directory".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Cache error"));
        assert!(msg.contains("Failed to remove directory"));
    }

    #[test]
    fn test_audio_error_display() {
        let err = StacheError::AudioError("Device not found".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Audio error"));
        assert!(msg.contains("Device not found"));
    }

    #[test]
    fn test_wallpaper_error_display() {
        let err = StacheError::WallpaperError("Image not found".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Wallpaper error"));
    }

    #[test]
    fn test_config_error_display() {
        let err = StacheError::ConfigError("Invalid JSON".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Configuration error"));
    }

    #[test]
    fn test_io_error_display() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err: StacheError = io_err.into();
        let msg = err.to_string();
        assert!(msg.contains("IO error"));
    }

    #[test]
    fn test_io_error_from_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied");
        let err: StacheError = io_err.into();
        assert!(matches!(err, StacheError::IoError(_)));
    }

    #[test]
    fn test_ipc_error_display() {
        let err = StacheError::IpcError("Failed to send notification".to_string());
        let msg = err.to_string();
        assert!(msg.contains("IPC error"));
        assert!(msg.contains("Failed to send notification"));
    }

    #[test]
    fn test_error_is_debug() {
        let err = StacheError::InvalidArguments("test".to_string());
        let debug_str = format!("{err:?}");
        assert!(debug_str.contains("InvalidArguments"));
    }

    #[test]
    fn test_battery_error_display() {
        let err = StacheError::BatteryError("No battery found".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Battery error"));
    }

    #[test]
    fn test_hyprspace_error_display() {
        let err = StacheError::HyprspaceError("Workspace not found".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Hyprspace error"));
    }

    #[test]
    fn test_shell_error_display() {
        let err = StacheError::ShellError("Command failed".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Shell error"));
    }

    #[test]
    fn test_command_error_display() {
        let err = StacheError::CommandError("Generic failure".to_string());
        let msg = err.to_string();
        assert_eq!(msg, "Generic failure");
    }

    #[test]
    fn test_from_string() {
        let err: StacheError = "test error".into();
        assert!(matches!(err, StacheError::CommandError(_)));
    }

    #[test]
    fn test_error_serializes_with_kind() {
        let err = StacheError::BatteryError("No battery".to_string());
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("BatteryError"));
        assert!(json.contains("No battery"));
    }
}
