//! Common types for IPC communication.
//!
//! This module defines the payloads and data structures used for
//! communication between the CLI and the desktop app.

use serde::{Deserialize, Serialize};

/// Payload received from CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcPayload {
    /// The command name.
    pub name: String,
    /// Optional data associated with the command.
    pub data: Option<String>,
}

/// Payload for CLI events emitted to the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct CliEventPayload {
    /// The name of the CLI command/event.
    pub name: String,
    /// Optional data associated with the command.
    pub data: Option<String>,
}

/// Screen target for commands that operate on screens.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScreenTarget {
    /// Apply to all screens.
    All,
    /// Apply to the main screen.
    Main,
    /// Apply to a specific screen by 1-based index.
    Index(usize),
}

/// Direction for window/focus operations.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
    Next,
    Previous,
}

impl Direction {
    /// Returns the direction as a string slice.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Left => "left",
            Self::Right => "right",
            Self::Next => "next",
            Self::Previous => "previous",
        }
    }
}

/// Resize dimension for window resize operations.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResizeDimension {
    Width,
    Height,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ipc_payload_serialization() {
        let payload = IpcPayload {
            name: "test-command".to_string(),
            data: Some("test-data".to_string()),
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("test-command"));
        assert!(json.contains("test-data"));
    }

    #[test]
    fn test_ipc_payload_deserialization() {
        let json = r#"{"name":"reload","data":null}"#;
        let payload: IpcPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.name, "reload");
        assert!(payload.data.is_none());
    }

    #[test]
    fn test_ipc_payload_with_data_deserialization() {
        let json = r#"{"name":"wallpaper-set","data":"/path/to/image.jpg"}"#;
        let payload: IpcPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.name, "wallpaper-set");
        assert_eq!(payload.data, Some("/path/to/image.jpg".to_string()));
    }

    #[test]
    fn test_cli_event_payload_serialization() {
        let payload = CliEventPayload {
            name: "reload".to_string(),
            data: None,
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("reload"));
    }

    #[test]
    fn test_screen_target_all_deserialization() {
        let json = r#""all""#;
        let target: ScreenTarget = serde_json::from_str(json).unwrap();
        assert!(matches!(target, ScreenTarget::All));
    }

    #[test]
    fn test_screen_target_main_deserialization() {
        let json = r#""main""#;
        let target: ScreenTarget = serde_json::from_str(json).unwrap();
        assert!(matches!(target, ScreenTarget::Main));
    }

    #[test]
    fn test_screen_target_index_deserialization() {
        let json = r#"{"index":2}"#;
        let target: ScreenTarget = serde_json::from_str(json).unwrap();
        assert!(matches!(target, ScreenTarget::Index(2)));
    }

    #[test]
    fn test_direction_as_str() {
        assert_eq!(Direction::Up.as_str(), "up");
        assert_eq!(Direction::Down.as_str(), "down");
        assert_eq!(Direction::Left.as_str(), "left");
        assert_eq!(Direction::Right.as_str(), "right");
        assert_eq!(Direction::Next.as_str(), "next");
        assert_eq!(Direction::Previous.as_str(), "previous");
    }
}
