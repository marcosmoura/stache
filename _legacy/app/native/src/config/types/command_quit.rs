//! Command Quit (`cmd_q`) configuration types.
//!
//! Configuration for the hold-to-quit feature that prevents accidental app termination.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Default hold duration in milliseconds (1500ms = 1.5 seconds).
const DEFAULT_HOLD_DURATION_MS: u64 = 1500;

/// Configuration for the Command Quit (hold ⌘Q to quit) feature.
///
/// This feature prevents accidental application quits by requiring
/// users to hold ⌘Q for a configurable duration before the frontmost
/// application is terminated.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct CommandQuitConfig {
    /// Whether the command quit feature is enabled.
    /// Default: false
    pub enabled: bool,

    /// Duration in milliseconds to hold ⌘Q before quitting.
    /// Default: 1500 (1.5 seconds)
    pub hold_duration: u64,
}

impl Default for CommandQuitConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            hold_duration: DEFAULT_HOLD_DURATION_MS,
        }
    }
}

impl CommandQuitConfig {
    /// Returns whether the command quit feature is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool { self.enabled }

    /// Returns the hold duration in seconds (for use in the `cmd_q` module).
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // Precision loss is negligible for millisecond values
    pub fn hold_duration_secs(&self) -> f64 { self.hold_duration as f64 / 1000.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_quit_config_default() {
        let config = CommandQuitConfig::default();
        assert!(!config.enabled); // Disabled by default (opt-in feature)
        assert_eq!(config.hold_duration, 1500);
    }

    #[test]
    fn test_hold_duration_secs_conversion() {
        let config = CommandQuitConfig::default();
        assert!((config.hold_duration_secs() - 1.5).abs() < f64::EPSILON);

        let config_custom = CommandQuitConfig {
            enabled: true,
            hold_duration: 2000,
        };
        assert!((config_custom.hold_duration_secs() - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_is_enabled() {
        let enabled_config = CommandQuitConfig {
            enabled: true,
            ..Default::default()
        };
        assert!(enabled_config.is_enabled());

        let disabled_config = CommandQuitConfig {
            enabled: false,
            ..Default::default()
        };
        assert!(!disabled_config.is_enabled());
    }

    #[test]
    fn test_deserialize_from_json() {
        let json = r#"{"enabled": false, "holdDuration": 2000}"#;
        let config: CommandQuitConfig = serde_json::from_str(json).unwrap();
        assert!(!config.enabled);
        assert_eq!(config.hold_duration, 2000);
    }

    #[test]
    fn test_deserialize_partial_json_uses_defaults() {
        let json = r#"{"enabled": false}"#;
        let config: CommandQuitConfig = serde_json::from_str(json).unwrap();
        assert!(!config.enabled);
        assert_eq!(config.hold_duration, 1500);
    }
}
