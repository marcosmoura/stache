//! Audio configuration types.
//!
//! Configuration for automatic audio device switching based on priority rules.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Strategy for matching device names in the priority list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MatchStrategy {
    /// Exact match (case-insensitive). This is the default strategy.
    #[default]
    Exact,
    /// Device name contains the specified string (case-insensitive).
    Contains,
    /// Device name starts with the specified string (case-insensitive).
    StartsWith,
    /// Device name matches the specified regex pattern.
    Regex,
}

/// Dependency condition for audio device selection.
///
/// Specifies a device that must be present (connected) for the parent device
/// to be considered in the priority list. The dependent device itself will
/// never be switched to; it only serves as a condition.
///
/// Example: "External Speakers" might depend on "`MiniFuse` 2" being connected,
/// since the speakers are physically connected through the audio interface.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct AudioDeviceDependency {
    /// The name (or pattern) of the device that must be present.
    pub name: String,

    /// The strategy for matching the dependency device name.
    /// - `exact`: Exact match (case-insensitive). Default if not specified.
    /// - `contains`: Device name contains the string (case-insensitive).
    /// - `startsWith`: Device name starts with the string (case-insensitive).
    /// - `regex`: Device name matches the regex pattern.
    #[serde(default)]
    pub strategy: MatchStrategy,
}

/// Priority entry for audio device selection.
///
/// Defines a single device in the priority list with its name and matching strategy.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct AudioDevicePriority {
    /// The name (or pattern) of the audio device to match.
    pub name: String,

    /// The strategy for matching the device name.
    /// - `exact`: Exact match (case-insensitive). Default if not specified.
    /// - `contains`: Device name contains the string (case-insensitive).
    /// - `startsWith`: Device name starts with the string (case-insensitive).
    /// - `regex`: Device name matches the regex pattern.
    #[serde(default)]
    pub strategy: MatchStrategy,

    /// Optional dependency condition.
    /// If specified, this device will only be considered if the dependent device
    /// is present (connected). The dependent device will never be switched to;
    /// it only serves as a condition for enabling this device.
    ///
    /// Example: External speakers connected via an audio interface might
    /// depend on the interface being present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<AudioDeviceDependency>,
}

/// Proxy audio configuration for automatic device routing.
///
/// This configuration enables intelligent audio device switching based on
/// device availability and priority. When enabled, the app automatically
/// switches to the highest-priority available device when devices connect
/// or disconnect.
///
/// `AirPlay` devices are always given the highest priority, even if not
/// explicitly listed in the priority configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct ProxyAudioConfig {
    /// Whether proxy audio functionality is enabled.
    /// When enabled, the app will automatically switch audio devices
    /// based on the priority configuration.
    /// Default: false
    pub enabled: bool,

    /// Priority list for input device selection.
    /// Devices are checked in order; the first available device is selected.
    /// `AirPlay` devices are always given highest priority automatically.
    #[serde(default)]
    pub input: Vec<AudioDevicePriority>,

    /// Priority list for output device selection.
    /// Devices are checked in order; the first available device is selected.
    /// `AirPlay` devices are always given highest priority automatically.
    #[serde(default)]
    pub output: Vec<AudioDevicePriority>,
}

impl ProxyAudioConfig {
    /// Returns whether proxy audio functionality is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool { self.enabled }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_match_strategy_default_is_exact() {
        assert_eq!(MatchStrategy::default(), MatchStrategy::Exact);
    }

    #[test]
    fn test_match_strategy_serialization() {
        assert_eq!(
            serde_json::to_string(&MatchStrategy::Exact).unwrap(),
            r#""exact""#
        );
        assert_eq!(
            serde_json::to_string(&MatchStrategy::Contains).unwrap(),
            r#""contains""#
        );
        assert_eq!(
            serde_json::to_string(&MatchStrategy::StartsWith).unwrap(),
            r#""startsWith""#
        );
        assert_eq!(
            serde_json::to_string(&MatchStrategy::Regex).unwrap(),
            r#""regex""#
        );
    }

    #[test]
    fn test_proxy_audio_default() {
        let config = ProxyAudioConfig::default();
        assert!(!config.is_enabled());
    }
}
