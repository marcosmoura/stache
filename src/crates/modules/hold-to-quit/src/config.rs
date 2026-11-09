//! Authoritative Hold-to-Quit configuration.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use specta::Type;
use stache_sdk::config::bounds;
use stache_sdk::{ConfigError, ModuleConfig, ModuleId};

/// Where the Hold-to-Quit HUD appears on the display.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "snake_case")]
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
#[serde(rename_all = "snake_case")]
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
#[serde(default, rename_all = "snake_case")]
pub struct CommandQuitConfig {
    /// Whether Hold-to-Quit is enabled. Disabled by default.
    pub enabled: bool,
    /// How long Cmd+Q must be held before the quit proceeds, in milliseconds.
    /// Any value from 0 to 9223372036854775807 (the largest TOML integer) is accepted.
    ///
    /// Not exported to the generated TypeScript: `number` cannot represent
    /// every `u64`. A future consumer must approve an explicit representation.
    #[specta(skip)]
    #[serde(deserialize_with = "bounds::deserialize")]
    #[schemars(range(max = i64::MAX))]
    pub hold_duration: u64,
    /// How long the HUD stays visible after Cmd+Q is released early, in
    /// milliseconds. 0 hides it immediately.
    /// Any value from 0 to 9223372036854775807 (the largest TOML integer) is accepted.
    ///
    /// Not exported to the generated TypeScript: `number` cannot represent
    /// every `u64`. A future consumer must approve an explicit representation.
    #[specta(skip)]
    #[serde(deserialize_with = "bounds::deserialize")]
    #[schemars(range(max = i64::MAX))]
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
    /// The catalog identifier and file stem (`commandQuit.toml`).
    pub const ID: ModuleId = ModuleId::new("commandQuit");
}

impl ModuleConfig for CommandQuitConfig {
    fn enabled(&self) -> bool { self.enabled }

    /// Accepts every duration TOML can represent; runtime arithmetic is
    /// checked later.
    fn validate(&self) -> Result<(), ConfigError> {
        bounds::validate_field("hold_duration", self.hold_duration)?;
        bounds::validate_field("dismiss_delay", self.dismiss_delay)
    }
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
        catalog().inspect(CommandQuitConfig::ID, Path::new("commandQuit.toml"), text)
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
                "hold_duration": 1500,
                "dismiss_delay": 1500,
                "position": "bottom",
                "animation": "fade"
            })
        );
        assert_eq!(CommandQuitConfig::ID.as_str(), "commandQuit");
    }

    fn from_json(
        value: serde_json::Value,
    ) -> Result<CommandQuitConfig, stache_sdk::config::ConfigDiagnostic> {
        stache_sdk::config::loader::deserialize(
            stache_sdk::config::ConfigFileId::module(CommandQuitConfig::ID),
            Path::new("commandQuit.toml"),
            value,
        )
    }

    #[test]
    fn comment_only_files_keep_the_defaults() {
        let config: CommandQuitConfig = serde_json::from_str("{}").expect("decode");
        assert_eq!(config, CommandQuitConfig::default());
        inspect("# defaults\n").expect("valid");
    }

    #[test]
    fn explicit_values_decode_with_snake_case_keys() {
        let config: CommandQuitConfig =
            serde_json::from_str(r#"{ "enabled": true, "hold_duration": 800 }"#).expect("decode");
        assert!(config.enabled());
        assert_eq!(config.hold_duration, 800);
    }

    #[test]
    fn every_toml_integer_is_accepted_without_clamping() {
        for value in [0_u64, 1, 1500, i64::MAX as u64] {
            let text = format!("hold_duration = {value}\n");
            inspect(&text).unwrap_or_else(|d| panic!("{value}: {d}"));
            let config = from_json(serde_json::json!({ "hold_duration": value })).expect("decode");
            assert_eq!(config.hold_duration, value);
            config.validate().expect("valid");
        }
    }

    #[test]
    fn wrong_types_are_type_diagnostics_with_a_field_path() {
        for value in ["-1", "1.5", r#""800""#, "true", "[1]"] {
            let diagnostic =
                inspect(&format!("hold_duration = {value}\n")).expect_err("wrong type");
            assert_eq!(diagnostic.kind, DiagnosticKind::Type, "{value}");
            assert_eq!(diagnostic.field.as_deref(), Some("hold_duration"), "{value}");
        }
        let diagnostic = inspect("enabled = \"yes\"\n").expect_err("wrong type");
        assert_eq!(diagnostic.kind, DiagnosticKind::Type);
        assert_eq!(diagnostic.field.as_deref(), Some("enabled"));
    }

    #[test]
    fn values_above_the_toml_integer_range_are_rejected_naming_the_field() {
        // TOML text cannot hold them: the parser rejects the integer.
        let parse = inspect("hold_duration = 9223372036854775808\n").expect_err("too large");
        assert_eq!(parse.kind, DiagnosticKind::Parse);
        // Migration JSON can: the typed decode rejects them and names the field.
        for field in ["hold_duration", "dismiss_delay"] {
            from_json(serde_json::json!({ field: i64::MAX as u64 })).expect("max");
            let diagnostic = from_json(serde_json::json!({ field: (i64::MAX as u64) + 1 }))
                .expect_err("above the TOML maximum");
            assert_eq!(diagnostic.kind, DiagnosticKind::Type, "{field}");
            assert_eq!(diagnostic.field.as_deref(), Some(field));
            assert!(
                diagnostic.message.contains("9223372036854775808"),
                "{diagnostic}"
            );
        }
        // Direct construction is caught by validation.
        for config in [
            CommandQuitConfig {
                hold_duration: u64::MAX,
                ..CommandQuitConfig::default()
            },
            CommandQuitConfig {
                dismiss_delay: u64::MAX,
                ..CommandQuitConfig::default()
            },
        ] {
            assert!(config.validate().is_err());
        }
    }

    #[test]
    fn dismiss_delay_mirrors_hold_duration() {
        for value in [0_u64, 1, 300, i64::MAX as u64] {
            let text = format!("dismiss_delay = {value}\n");
            inspect(&text).unwrap_or_else(|d| panic!("{value}: {d}"));
            let config = from_json(serde_json::json!({ "dismiss_delay": value })).expect("decode");
            assert_eq!(config.dismiss_delay, value);
            assert_eq!(config.hold_duration, 1500);
            config.validate().expect("valid");
        }
        for value in ["-1", "1.5", r#""800""#, "true"] {
            let diagnostic =
                inspect(&format!("dismiss_delay = {value}\n")).expect_err("wrong type");
            assert_eq!(diagnostic.kind, DiagnosticKind::Type, "{value}");
            assert_eq!(diagnostic.field.as_deref(), Some("dismiss_delay"), "{value}");
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
            let text = format!("position = \"{text}\"\n");
            inspect(&text).unwrap_or_else(|d| panic!("{text}: {d}"));
            let (config, _) = catalog()
                .decode_typed::<CommandQuitConfig>(
                    CommandQuitConfig::ID,
                    Path::new("commandQuit.toml"),
                    &text,
                )
                .expect("decode");
            assert_eq!(config.position, position);
            assert_eq!(config.animation, HudAnimation::Fade);
        }
        for (text, animation) in [("fade", HudAnimation::Fade), ("slide", HudAnimation::Slide)] {
            let text = format!("animation = \"{text}\"\n");
            inspect(&text).unwrap_or_else(|d| panic!("{text}: {d}"));
            let (config, _) = catalog()
                .decode_typed::<CommandQuitConfig>(
                    CommandQuitConfig::ID,
                    Path::new("commandQuit.toml"),
                    &text,
                )
                .expect("decode");
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
            ("animation", r#""zoom""#),
            ("animation", "true"),
        ] {
            let diagnostic = inspect(&format!("{key} = {value}\n")).expect_err("rejected");
            assert_eq!(diagnostic.kind, DiagnosticKind::Type, "{key} {value}");
            assert_eq!(diagnostic.field.as_deref(), Some(key), "{key} {value}");
        }
    }

    #[test]
    fn empty_and_malformed_files_are_invalid() {
        let diagnostic = inspect("  \n").expect_err("empty file");
        assert!(diagnostic.message.contains("file is empty"), "{diagnostic}");
        assert!(inspect("{ not toml").is_err());
    }

    #[test]
    fn unknown_keys_are_warnings_and_no_exclusion_key_is_recognized() {
        let warnings = inspect("enabled = true\nexclude = [\"Finder\"]\nforce = true\n")
            .expect("the file stays valid");
        assert_eq!(warnings, ["exclude", "force"]);
    }

    #[test]
    fn the_template_and_schema_come_from_the_single_type() {
        let catalog = catalog();
        let template = catalog.template(CommandQuitConfig::ID).expect("template");
        assert_eq!(
            template,
            "\
#:schema https://raw.githubusercontent.com/marcosmoura/stache/main/schemas/commandQuit.schema.json

# Whether Hold-to-Quit is enabled. Disabled by default.
enabled = false

# How long Cmd+Q must be held before the quit proceeds, in milliseconds.
# Any value from 0 to 9223372036854775807 (the largest TOML integer) is accepted.
#
# Not exported to the generated TypeScript: `number` cannot represent
# every `u64`. A future consumer must approve an explicit representation.
hold_duration = 1500

# How long the HUD stays visible after Cmd+Q is released early, in
# milliseconds. 0 hides it immediately.
# Any value from 0 to 9223372036854775807 (the largest TOML integer) is accepted.
#
# Not exported to the generated TypeScript: `number` cannot represent
# every `u64`. A future consumer must approve an explicit representation.
dismiss_delay = 1500

# Where the HUD appears on the display. Defaults to `bottom`.
position = \"bottom\"

# How the HUD enters and leaves. Defaults to `fade`.
animation = \"fade\"
"
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
        let duration = &schema["properties"]["hold_duration"];
        assert_eq!(duration["default"], 1500);
        assert_eq!(duration["type"], "integer");
        assert_eq!(duration["format"], "uint64");
        assert_eq!(duration["minimum"], 0);
        assert_eq!(duration["maximum"], 9_223_372_036_854_775_807_i64);
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
        let delay = &schema["properties"]["dismiss_delay"];
        assert_eq!(delay["default"], 1500);
        assert_eq!(delay["type"], "integer");
        assert_eq!(delay["format"], "uint64");
        assert_eq!(delay["maximum"], 9_223_372_036_854_775_807_i64);
    }

    #[test]
    fn a_duplicate_registration_is_rejected() {
        let mut catalog = catalog();
        assert!(catalog.register::<CommandQuitConfig>(CommandQuitConfig::ID).is_err());
    }
}
