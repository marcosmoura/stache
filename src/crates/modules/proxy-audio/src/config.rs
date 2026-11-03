//! Configuration for automatic audio-device selection.
//!
//! Field names, defaults and matching strategies mirror the legacy audio
//! configuration so migration can decode existing files with this type and part
//! 6 can consume the same values without a second representation.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use specta::Type;
use stache_sdk::{ConfigError, ModuleConfig, ModuleId};

/// Strategy used to match a device name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "camelCase")]
pub enum MatchStrategy {
    /// Case-insensitive exact match. This is the default.
    #[default]
    Exact,
    /// Case-insensitive containment.
    Contains,
    /// Case-insensitive prefix match.
    StartsWith,
    /// Case-sensitive regular expression.
    Regex,
}

/// A device that must be present for a priority entry to be considered.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(default, rename_all = "camelCase")]
pub struct AudioDeviceDependency {
    /// The name or pattern of the required device.
    pub name: String,
    /// How the required device name is matched.
    #[serde(default)]
    pub strategy: MatchStrategy,
}

/// One entry in an input or output priority list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(default, rename_all = "camelCase")]
pub struct AudioDevicePriority {
    /// The device name or pattern to match.
    pub name: String,
    /// How the device name is matched.
    #[serde(default)]
    pub strategy: MatchStrategy,
    /// A device that must be connected before this entry is eligible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<AudioDeviceDependency>,
}

/// Automatic audio-device selection.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(default, rename_all = "camelCase")]
pub struct ProxyAudioConfig {
    /// Whether the module should run.
    pub enabled: bool,
    /// Ordered input devices; the first eligible entry wins.
    #[serde(default)]
    pub input: Vec<AudioDevicePriority>,
    /// Ordered output devices; the first eligible entry wins.
    #[serde(default)]
    pub output: Vec<AudioDevicePriority>,
}

impl ProxyAudioConfig {
    /// The catalog identifier and file stem of the pilot configuration.
    pub const ID: ModuleId = ModuleId::new("proxyAudio");
}

impl ModuleConfig for ProxyAudioConfig {
    fn enabled(&self) -> bool { self.enabled }

    fn validate(&self) -> Result<(), ConfigError> {
        for (direction, entries) in [("input", &self.input), ("output", &self.output)] {
            for (index, entry) in entries.iter().enumerate() {
                validate_name(direction, index, "name", &entry.name, entry.strategy)?;
                if let Some(dependency) = &entry.depends_on {
                    validate_name(
                        direction,
                        index,
                        "dependsOn.name",
                        &dependency.name,
                        dependency.strategy,
                    )?;
                }
            }
        }
        Ok(())
    }
}

/// Rejects an empty name and an invalid regex pattern.
fn validate_name(
    direction: &str,
    index: usize,
    field: &str,
    name: &str,
    strategy: MatchStrategy,
) -> Result<(), ConfigError> {
    if name.trim().is_empty() {
        return Err(ConfigError::new(format!(
            "{direction}[{index}].{field} must not be empty"
        )));
    }
    validate_pattern(direction, index, field, name, strategy)
}

/// Compiles a `regex` pattern so a broken one fails at config validation.
fn validate_pattern(
    direction: &str,
    index: usize,
    field: &str,
    name: &str,
    strategy: MatchStrategy,
) -> Result<(), ConfigError> {
    if strategy != MatchStrategy::Regex {
        return Ok(());
    }
    regex::Regex::new(name).map(|_| ()).map_err(|error| {
        ConfigError::new(format!(
            "{direction}[{index}].{field} is not a valid regular expression: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn defaults_are_disabled_with_empty_lists() {
        let config = ProxyAudioConfig::default();
        assert!(!config.enabled());
        assert!(config.input.is_empty());
        assert!(config.output.is_empty());
    }

    #[test]
    fn legacy_camel_case_fields_decode() {
        let config: ProxyAudioConfig = serde_json::from_value(json!({
            "enabled": true,
            "output": [{
                "name": "External Speakers",
                "strategy": "startsWith",
                "dependsOn": { "name": "MiniFuse", "strategy": "contains" }
            }]
        }))
        .expect("decode");
        assert!(config.enabled);
        assert_eq!(config.output[0].strategy, MatchStrategy::StartsWith);
        assert_eq!(
            config.output[0].depends_on.as_ref().map(|value| value.strategy),
            Some(MatchStrategy::Contains)
        );
    }

    #[test]
    fn match_strategy_spellings_are_camel_case() {
        assert_eq!(
            serde_json::to_string(&MatchStrategy::StartsWith).expect("serialize"),
            "\"startsWith\""
        );
        assert_eq!(
            serde_json::to_string(&MatchStrategy::Regex).expect("serialize"),
            "\"regex\""
        );
    }

    #[test]
    fn empty_priority_names_are_rejected() {
        let config = ProxyAudioConfig {
            enabled: true,
            input: vec![AudioDevicePriority {
                name: "  ".to_owned(),
                ..AudioDevicePriority::default()
            }],
            output: Vec::new(),
        };
        let error = config.validate().expect_err("empty name must fail");
        assert!(error.message().contains("input[0].name"));
    }

    #[test]
    fn valid_regex_patterns_are_accepted() {
        let config = ProxyAudioConfig {
            enabled: true,
            input: Vec::new(),
            output: vec![AudioDevicePriority {
                name: "^External (Speakers|Headphones)$".to_owned(),
                strategy: MatchStrategy::Regex,
                depends_on: Some(AudioDeviceDependency {
                    name: "MiniFuse.*".to_owned(),
                    strategy: MatchStrategy::Regex,
                }),
            }],
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn an_invalid_regex_pattern_names_the_entry_and_pattern() {
        let config = ProxyAudioConfig {
            enabled: true,
            input: Vec::new(),
            output: vec![AudioDevicePriority {
                name: "(External".to_owned(),
                strategy: MatchStrategy::Regex,
                depends_on: None,
            }],
        };
        let error = config.validate().expect_err("invalid pattern must fail");
        assert!(error.message().contains("output[0].name"), "{}", error.message());
        assert!(error.message().contains("(External"), "{}", error.message());
        assert!(
            error.message().contains("not a valid regular expression"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn a_non_regex_entry_may_contain_regex_metacharacters() {
        let config = ProxyAudioConfig {
            enabled: true,
            input: Vec::new(),
            output: vec![AudioDevicePriority {
                name: "(External".to_owned(),
                strategy: MatchStrategy::Contains,
                depends_on: None,
            }],
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn empty_objects_fall_back_to_field_defaults() {
        let config: ProxyAudioConfig = serde_json::from_str("{}").expect("decode");
        assert_eq!(config, ProxyAudioConfig::default());
    }
}
