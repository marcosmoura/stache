//! Authoritative Keep Awake configuration.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use specta::Type;
use stache_sdk::{ConfigError, ModuleConfig, ModuleId};

/// Automatic prevention of display, idle and system sleep.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(default, rename_all = "snake_case")]
pub struct KeepAwakeConfig {
    /// Whether Keep Awake should prevent display, idle and system sleep.
    pub enabled: bool,
}

impl KeepAwakeConfig {
    /// The catalog identifier and file stem (`keepAwake.toml`).
    pub const ID: ModuleId = ModuleId::new("keepAwake");
}

impl ModuleConfig for KeepAwakeConfig {
    fn enabled(&self) -> bool { self.enabled }

    fn validate(&self) -> Result<(), ConfigError> { Ok(()) }
}

#[cfg(test)]
mod tests {
    use stache_sdk::{ConfigCatalog, DiagnosticKind};

    use super::*;

    fn catalog() -> ConfigCatalog {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<KeepAwakeConfig>(KeepAwakeConfig::ID).expect("register");
        catalog
    }

    #[test]
    fn defaults_are_disabled() {
        let config = KeepAwakeConfig::default();
        assert!(!config.enabled());
        assert_eq!(
            serde_json::to_value(&config).expect("encode"),
            serde_json::json!({ "enabled": false })
        );
        assert_eq!(KeepAwakeConfig::ID.as_str(), "keepAwake");
    }

    #[test]
    fn empty_objects_keep_the_disabled_default() {
        let config: KeepAwakeConfig = serde_json::from_str("{}").expect("decode");
        assert_eq!(config, KeepAwakeConfig::default());
        assert!(!config.enabled());
    }

    #[test]
    fn explicit_booleans_decode() {
        for (text, expected) in [("true", true), ("false", false)] {
            let config: KeepAwakeConfig =
                serde_json::from_str(&format!(r#"{{ "enabled": {text} }}"#)).expect("decode");
            assert_eq!(config.enabled, expected);
            assert_eq!(config.enabled(), expected);
        }
    }

    #[test]
    fn a_non_boolean_enabled_value_is_a_type_diagnostic() {
        let catalog = catalog();
        for value in [r#""yes""#, "1", "[]", "{}"] {
            let diagnostic = catalog
                .inspect(
                    KeepAwakeConfig::ID,
                    std::path::Path::new("keepAwake.toml"),
                    &format!("enabled = {value}\n"),
                )
                .expect_err("wrong type");
            assert_eq!(diagnostic.kind, DiagnosticKind::Type, "{value}");
            assert_eq!(diagnostic.field.as_deref(), Some("enabled"), "{value}");
        }
    }

    #[test]
    fn empty_text_is_an_invalid_diagnostic_with_the_template_hint() {
        let diagnostic = catalog()
            .inspect(
                KeepAwakeConfig::ID,
                std::path::Path::new("keepAwake.toml"),
                "  \n",
            )
            .expect_err("empty file");
        assert_eq!(diagnostic.kind, DiagnosticKind::Type);
        assert!(diagnostic.message.contains("file is empty"), "{diagnostic}");
        assert!(diagnostic.message.contains("add a `#` comment"), "{diagnostic}");
    }

    #[test]
    fn unknown_keys_are_warnings_but_the_file_stays_valid() {
        let warnings = catalog()
            .inspect(
                KeepAwakeConfig::ID,
                std::path::Path::new("keepAwake.toml"),
                "enabled = true\nduration = 60\n",
            )
            .expect("the file stays valid");
        assert_eq!(warnings, ["duration"]);
    }

    #[test]
    fn the_template_and_schema_come_from_the_single_type() {
        let catalog = catalog();
        let template = catalog.template(KeepAwakeConfig::ID).expect("template");
        assert_eq!(
            template,
            "\
#:schema https://raw.githubusercontent.com/marcosmoura/stache/main/schemas/keepAwake.schema.json

# Whether Keep Awake should prevent display, idle and system sleep.
enabled = false
"
        );
        let schema = catalog.schema_json(KeepAwakeConfig::ID).expect("schema").expect("render");
        let schema: serde_json::Value = serde_json::from_str(&schema).expect("schema JSON");
        assert_eq!(
            schema["$id"],
            "https://raw.githubusercontent.com/marcosmoura/stache/main/schemas/keepAwake.schema.json"
        );
        assert_eq!(schema["title"], "KeepAwakeConfig");
        assert_eq!(
            schema["description"],
            "Automatic prevention of display, idle and system sleep."
        );
        assert_eq!(
            schema["properties"]["enabled"]["description"],
            "Whether Keep Awake should prevent display, idle and system sleep."
        );
        assert_eq!(schema["properties"]["enabled"]["default"], false);
        assert_eq!(schema["properties"]["enabled"]["type"], "boolean");
    }

    #[test]
    fn a_duplicate_registration_is_rejected() {
        let mut catalog = catalog();
        assert!(
            catalog.register::<KeepAwakeConfig>(KeepAwakeConfig::ID).is_err(),
            "the catalog owns exactly one keepAwake entry"
        );
    }
}
