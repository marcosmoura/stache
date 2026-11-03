//! Error types for Stache.
//!
//! This module provides unified error types used throughout the application.
//! These types implement the necessary traits to be returned from Tauri commands.

use serde::Serialize;
use thiserror::Error;

/// Errors that can occur during application execution.
///
/// This enum implements `Serialize` and `Into<tauri::ipc::InvokeError>` to be
/// used as a return type for Tauri commands, providing structured error information
/// to the frontend.
#[derive(Debug, Error, Serialize)]
#[serde(tag = "kind", content = "message")]
pub enum StacheError {
    /// Invalid command arguments.
    #[error("{0}")]
    InvalidArguments(String),
    /// Cache operation failed.
    #[error("Cache error: {0}")]
    CacheError(String),
    /// Audio operation failed.
    #[error("Audio error: {0}")]
    AudioError(String),
    /// Wallpaper operation failed.
    #[error("Wallpaper error: {0}")]
    WallpaperError(String),
    /// Configuration error.
    #[error("Configuration error: {0}")]
    ConfigError(String),
    /// IPC communication error.
    #[error("IPC error: {0}")]
    IpcError(String),
    /// IO error.
    #[error("IO error: {0}")]
    IoError(String),
    /// Battery operation failed.
    #[error("Battery error: {0}")]
    BatteryError(String),
    /// Tiling window manager operation failed.
    #[error("Tiling error: {0}")]
    TilingError(String),
    /// Shell command execution failed.
    #[error("Shell error: {0}")]
    ShellError(String),
    /// Generic command error.
    #[error("{0}")]
    CommandError(String),
}

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
    fn test_tiling_error_display() {
        let err = StacheError::TilingError("Workspace not found".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Tiling error"));
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
