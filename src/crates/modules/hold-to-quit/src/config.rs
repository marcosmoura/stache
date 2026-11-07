//! Authoritative Hold-to-Quit configuration.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use specta::Type;
use stache_sdk::{ConfigError, ModuleConfig, ModuleId};

/// Where the Hold-to-Quit HUD appears on the display.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "lowercase")]
pub enum HudPosition {
    /// Horizontally centered near the lower edge.
    #[default]
    Bottom,
    /// Horizontally centered near the upper edge.
    Top,
    /// Vertically centered near the left edge.
    Left,
    /// Vertically centered near the right edge.
    Right,
    /// Centered on the display.
    Center,
}

/// How the Hold-to-Quit HUD enters and leaves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "lowercase")]
pub enum HudAnimation {
    /// Fade in and out in place.
    #[default]
    Fade,
    /// Fade while sliding in from the configured position (down from above
    /// for `center`) and back out. Reduce Motion makes it a plain fade.
    Slide,
}

/// Require holding Cmd+Q before an application quits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(default, rename_all = "camelCase")]
pub struct CommandQuitConfig {
    /// Whether Hold-to-Quit is enabled. Disabled by default.
    pub enabled: bool,
    /// How long Cmd+Q must be held before the quit proceeds, in milliseconds.
    /// Any unsigned 64-bit value is accepted, including 0.
    ///
    /// Not exported to the generated TypeScript: `number` cannot represent
    /// every `u64`. A future consumer must approve an explicit representation.
    #[specta(skip)]
    pub hold_duration: u64,
    /// How long the HUD stays visible after Cmd+Q is released early, in
    /// milliseconds. 0 hides it immediately.
    /// Any unsigned 64-bit value is accepted, including 0.
    ///
    /// Not exported to the generated TypeScript: `number` cannot represent
    /// every `u64`. A future consumer must approve an explicit representation.
    #[specta(skip)]
    pub dismiss_delay: u64,
    /// Where the HUD appears on the display. Defaults to `bottom`.
    pub position: HudPosition,
    /// How the HUD enters and leaves. Defaults to `fade`.
    pub animation: HudAnimation,
}

impl Default for CommandQuitConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            hold_duration: 1500,
            dismiss_delay: 1500,
            position: HudPosition::Bottom,
            animation: HudAnimation::Fade,
        }
    }
}

impl CommandQuitConfig {
    /// The catalog identifier and file stem (`commandQuit.jsonc`).
    pub const ID: ModuleId = ModuleId::new("commandQuit");
}

impl ModuleConfig for CommandQuitConfig {
    fn enabled(&self) -> bool { self.enabled }

    /// Accepts every `u64` hold duration; runtime arithmetic is checked later.
    fn validate(&self) -> Result<(), ConfigError> { Ok(()) }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use stache_sdk::{ConfigCatalog, DiagnosticKind};

    use super::*;

    fn catalog() -> ConfigCatalog {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<CommandQuitConfig>(CommandQuitConfig::ID).expect("register");
        catalog
    }

    fn inspect(text: &str) -> Result<Vec<String>, stache_sdk::config::ConfigDiagnostic> {
        catalog().inspect(CommandQuitConfig::ID, Path::new("commandQuit.jsonc"), text)
    }

    #[test]
    fn defaults_are_disabled_with_1500_milliseconds() {
        let config = CommandQuitConfig::default();
        assert!(!config.enabled());
        assert_eq!(config.hold_duration, 1500);
        assert_eq!(config.dismiss_delay, 1500);
        assert_eq!(
            serde_json::to_value(&config).expect("encode"),
            serde_json::json!({
                "enabled": false,
                "holdDuration": 1500,
                "dismissDelay": 1500,
                "position": "bottom",
                "animation": "fade"
            })
        );
        assert_eq!(CommandQuitConfig::ID.as_str(), "commandQuit");
    }

    #[test]
    fn empty_objects_keep_the_defaults() {
        let config: CommandQuitConfig = serde_json::from_str("{}").expect("decode");
        assert_eq!(config, CommandQuitConfig::default());
        inspect("{}").expect("valid");
    }

    #[test]
    fn explicit_values_decode_with_camel_case_keys() {
        let config: CommandQuitConfig =
            serde_json::from_str(r#"{ "enabled": true, "holdDuration": 800 }"#).expect("decode");
        assert!(config.enabled());
        assert_eq!(config.hold_duration, 800);
    }

    #[test]
    fn every_u64_is_accepted_without_clamping() {
        for value in [0_u64, 1, 1500, u64::MAX] {
            let text = format!(r#"{{ "holdDuration": {value} }}"#);
            inspect(&text).unwrap_or_else(|d| panic!("{value}: {d}"));
            let config: CommandQuitConfig = serde_json::from_str(&text).expect("decode");
            assert_eq!(config.hold_duration, value);
            config.validate().expect("valid");
        }
    }

    #[test]
    fn wrong_types_and_overflow_are_type_diagnostics_with_a_field_path() {
        for value in [
            "-1",
            "1.5",
            r#""800""#,
            "null",
            "true",
            "18446744073709551616",
        ] {
            let diagnostic =
                inspect(&format!(r#"{{ "holdDuration": {value} }}"#)).expect_err("wrong type");
            assert_eq!(diagnostic.kind, DiagnosticKind::Type, "{value}");
            assert_eq!(diagnostic.field.as_deref(), Some("holdDuration"), "{value}");
        }
        let diagnostic = inspect(r#"{ "enabled": "yes" }"#).expect_err("wrong type");
        assert_eq!(diagnostic.kind, DiagnosticKind::Type);
        assert_eq!(diagnostic.field.as_deref(), Some("enabled"));
    }

    #[test]
    fn dismiss_delay_mirrors_hold_duration() {
        for value in [0_u64, 1, 300, u64::MAX] {
            let text = format!(r#"{{ "dismissDelay": {value} }}"#);
            inspect(&text).unwrap_or_else(|d| panic!("{value}: {d}"));
            let config: CommandQuitConfig = serde_json::from_str(&text).expect("decode");
            assert_eq!(config.dismiss_delay, value);
            assert_eq!(config.hold_duration, 1500);
            config.validate().expect("valid");
        }
        for value in [
            "-1",
            "1.5",
            r#""800""#,
            "null",
            "true",
            "18446744073709551616",
        ] {
            let diagnostic =
                inspect(&format!(r#"{{ "dismissDelay": {value} }}"#)).expect_err("wrong type");
            assert_eq!(diagnostic.kind, DiagnosticKind::Type, "{value}");
            assert_eq!(diagnostic.field.as_deref(), Some("dismissDelay"), "{value}");
        }
    }

    #[test]
    fn position_and_animation_accept_every_listed_value() {
        for (text, position) in [
            ("top", HudPosition::Top),
            ("right", HudPosition::Right),
            ("left", HudPosition::Left),
            ("bottom", HudPosition::Bottom),
            ("center", HudPosition::Center),
        ] {
            let text = format!(r#"{{ "position": "{text}" }}"#);
            inspect(&text).unwrap_or_else(|d| panic!("{text}: {d}"));
            let config: CommandQuitConfig = serde_json::from_str(&text).expect("decode");
            assert_eq!(config.position, position);
            assert_eq!(config.animation, HudAnimation::Fade);
        }
        for (text, animation) in [("fade", HudAnimation::Fade), ("slide", HudAnimation::Slide)] {
            let text = format!(r#"{{ "animation": "{text}" }}"#);
            inspect(&text).unwrap_or_else(|d| panic!("{text}: {d}"));
            let config: CommandQuitConfig = serde_json::from_str(&text).expect("decode");
            assert_eq!(config.animation, animation);
            assert_eq!(config.position, HudPosition::Bottom);
            config.validate().expect("valid");
        }
    }

    #[test]
    fn unknown_strings_and_wrong_types_are_type_diagnostics_naming_the_key() {
        for (key, value) in [
            ("position", r#""middle""#),
            ("position", r#""Top""#),
            ("position", "1"),
            ("position", "null"),
            ("animation", r#""zoom""#),
            ("animation", "true"),
            ("animation", "null"),
        ] {
            let diagnostic = inspect(&format!(r#"{{ "{key}": {value} }}"#)).expect_err("rejected");
            assert_eq!(diagnostic.kind, DiagnosticKind::Type, "{key} {value}");
            assert_eq!(diagnostic.field.as_deref(), Some(key), "{key} {value}");
        }
    }

    #[test]
    fn empty_and_malformed_files_are_invalid() {
        let diagnostic = inspect("  \n").expect_err("empty file");
        assert!(diagnostic.message.contains("file is empty"), "{diagnostic}");
        assert!(inspect("{ not json").is_err());
    }

    #[test]
    fn unknown_keys_are_warnings_and_no_exclusion_key_is_recognized() {
        let warnings = inspect(r#"{ "enabled": true, "exclude": ["Finder"], "force": true }"#)
            .expect("the file stays valid");
        assert_eq!(warnings, ["exclude", "force"]);
    }

    #[test]
    fn the_template_and_schema_come_from_the_single_type() {
        let catalog = catalog();
        let template = catalog.template(CommandQuitConfig::ID).expect("template");
        assert_eq!(
            template,
            r#"{
  // Schema for editor validation.
  "$schema": "https://raw.githubusercontent.com/marcosmoura/stache/main/schemas/commandQuit.schema.json",
  // How the HUD enters and leaves. Defaults to `fade`.
  "animation": "fade",
  // How long the HUD stays visible after Cmd+Q is released early, in
  // milliseconds. 0 hides it immediately.
  // Any unsigned 64-bit value is accepted, including 0.
  //
  // Not exported to the generated TypeScript: `number` cannot represent
  // every `u64`. A future consumer must approve an explicit representation.
  "dismissDelay": 1500,
  // Whether Hold-to-Quit is enabled. Disabled by default.
  "enabled": false,
  // How long Cmd+Q must be held before the quit proceeds, in milliseconds.
  // Any unsigned 64-bit value is accepted, including 0.
  //
  // Not exported to the generated TypeScript: `number` cannot represent
  // every `u64`. A future consumer must approve an explicit representation.
  "holdDuration": 1500,
  // Where the HUD appears on the display. Defaults to `bottom`.
  "position": "bottom"
}
"#
        );
        let schema = catalog.schema_json(CommandQuitConfig::ID).expect("schema").expect("render");
        let schema: serde_json::Value = serde_json::from_str(&schema).expect("schema JSON");
        assert_eq!(
            schema["$id"],
            "https://raw.githubusercontent.com/marcosmoura/stache/main/schemas/commandQuit.schema.json"
        );
        assert_eq!(schema["title"], "CommandQuitConfig");
        assert_eq!(schema["properties"]["enabled"]["default"], false);
        assert_eq!(schema["properties"]["enabled"]["type"], "boolean");
        let duration = &schema["properties"]["holdDuration"];
        assert_eq!(duration["default"], 1500);
        assert_eq!(duration["type"], "integer");
        assert_eq!(duration["format"], "uint64");
        assert_eq!(duration["minimum"], 0);
        assert!(
            duration.get("maximum").is_none(),
            "no invented upper bound: {duration}"
        );
        assert!(duration["description"].as_str().expect("description").contains("milliseconds"));
        let position = &schema["properties"]["position"];
        assert_eq!(position["default"], "bottom");
        assert_eq!(position["$ref"], "#/$defs/HudPosition");
        let values = |name: &str| -> Vec<String> {
            schema["$defs"][name]["oneOf"]
                .as_array()
                .expect("oneOf")
                .iter()
                .map(|v| v["const"].as_str().expect("const").to_owned())
                .collect()
        };
        assert_eq!(values("HudPosition"), [
            "bottom", "top", "left", "right", "center"
        ]);
        assert_eq!(values("HudAnimation"), ["fade", "slide"]);
        let animation = &schema["properties"]["animation"];
        assert_eq!(animation["default"], "fade");
        assert_eq!(animation["$ref"], "#/$defs/HudAnimation");
        let delay = &schema["properties"]["dismissDelay"];
        assert_eq!(delay["default"], 1500);
        assert_eq!(delay["type"], "integer");
        assert_eq!(delay["format"], "uint64");
        assert!(
            delay.get("maximum").is_none(),
            "no invented upper bound: {delay}"
        );
    }

    #[test]
    fn a_duplicate_registration_is_rejected() {
        let mut catalog = catalog();
        assert!(catalog.register::<CommandQuitConfig>(CommandQuitConfig::ID).is_err());
    }
}
