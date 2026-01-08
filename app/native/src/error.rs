//! Error types for Barba Shell.
//!
//! This module provides unified error types used throughout the application.

use std::fmt;

/// Errors that can occur during application execution.
#[derive(Debug)]
pub enum BarbaError {
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
    Io(std::io::Error),
}

impl fmt::Display for BarbaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArguments(msg) => {
                write!(f, "{msg}")
            }
            Self::CacheError(msg) => {
                write!(f, "Cache error: {msg}")
            }
            Self::AudioError(msg) => {
                write!(f, "Audio error: {msg}")
            }
            Self::WallpaperError(msg) => {
                write!(f, "Wallpaper error: {msg}")
            }
            Self::ConfigError(msg) => {
                write!(f, "Configuration error: {msg}")
            }
            Self::IpcError(msg) => {
                write!(f, "IPC error: {msg}")
            }
            Self::Io(err) => write!(f, "IO error: {err}"),
        }
    }
}

impl std::error::Error for BarbaError {}

impl From<std::io::Error> for BarbaError {
    fn from(err: std::io::Error) -> Self { Self::Io(err) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invalid_arguments_display() {
        let err = BarbaError::InvalidArguments("Cannot specify both path and random".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Cannot specify both path and random"));
    }

    #[test]
    fn test_cache_error_display() {
        let err = BarbaError::CacheError("Failed to remove directory".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Cache error"));
        assert!(msg.contains("Failed to remove directory"));
    }

    #[test]
    fn test_audio_error_display() {
        let err = BarbaError::AudioError("Device not found".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Audio error"));
        assert!(msg.contains("Device not found"));
    }

    #[test]
    fn test_wallpaper_error_display() {
        let err = BarbaError::WallpaperError("Image not found".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Wallpaper error"));
    }

    #[test]
    fn test_config_error_display() {
        let err = BarbaError::ConfigError("Invalid JSON".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Configuration error"));
    }

    #[test]
    fn test_io_error_display() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = BarbaError::Io(io_err);
        let msg = err.to_string();
        assert!(msg.contains("IO error"));
    }

    #[test]
    fn test_io_error_from_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied");
        let err: BarbaError = io_err.into();
        assert!(matches!(err, BarbaError::Io(_)));
    }

    #[test]
    fn test_ipc_error_display() {
        let err = BarbaError::IpcError("Failed to send notification".to_string());
        let msg = err.to_string();
        assert!(msg.contains("IPC error"));
        assert!(msg.contains("Failed to send notification"));
    }

    #[test]
    fn test_error_is_debug() {
        let err = BarbaError::InvalidArguments("test".to_string());
        let debug_str = format!("{err:?}");
        assert!(debug_str.contains("InvalidArguments"));
    }
}
