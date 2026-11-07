//! Authoritative Keybindings configuration.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::de::value::MapAccessDeserializer;
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use specta::Type;
use stache_sdk::{ConfigError, ModuleConfig, ModuleId, StartupCommands};

/// Global shortcuts that run commands.
///
/// Validation is deliberately shape-only: a malformed shortcut spelling is a
/// later per-entry planning diagnostic, never a rejection of the whole file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, JsonSchema, Type)]
// `default` only shapes the schema and generated types; decoding is manual.
#[serde(default, rename_all = "camelCase")]
pub struct KeybindingsConfig {
    /// Whether the shortcuts should be active.
    pub enabled: bool,
    /// Shortcut to command mapping. Each value is one command string or an
    /// ordered list of commands that run sequentially and stop at the first
    /// failure.
    pub bindings: BTreeMap<String, StartupCommands>,
}

/// The wire shape; kept private so only maps decode as the configuration.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Wire {
    enabled: bool,
    bindings: BTreeMap<String, StartupCommands>,
}

impl<'de> Deserialize<'de> for KeybindingsConfig {
    /// Accepts only an object. Serde's derived struct decoding also accepts a
    /// sequence, which would silently turn a legacy `[]` section into defaults.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Object;

        impl<'de> Visitor<'de> for Object {
            type Value = Wire;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("struct KeybindingsConfig")
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Wire, A::Error> {
                Wire::deserialize(MapAccessDeserializer::new(map))
            }
        }

        let Wire { enabled, bindings } = deserializer.deserialize_map(Object)?;
        Ok(Self { enabled, bindings })
    }
}

impl KeybindingsConfig {
    /// The catalog identifier and file stem.
    pub const ID: ModuleId = ModuleId::new("keybindings");
}

impl ModuleConfig for KeybindingsConfig {
    fn enabled(&self) -> bool { self.enabled }

    fn validate(&self) -> Result<(), ConfigError> { Ok(()) }

    /// Wraps the whole legacy shortcut map as `bindings`.
    ///
    /// The legacy section is the map itself, so a shortcut literally named
    /// `enabled` or `bindings` is a binding and never the new envelope. The new
    /// file is enabled exactly when the map is nonempty, even if every command
    /// is blank. A nonobject section is left unchanged and is rejected by
    /// validation.
    fn migrate_legacy_section(section: &mut Value) -> Vec<String> {
        let Some(map) = section.as_object_mut() else {
            return Vec::new();
        };
        let bindings = std::mem::take(map);
        let enabled = !bindings.is_empty();
        let mut envelope = Map::new();
        envelope.insert("enabled".to_owned(), Value::Bool(enabled));
        envelope.insert("bindings".to_owned(), Value::Object(bindings));
        *section = Value::Object(envelope);
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use stache_sdk::ConfigCatalog;

    use super::*;

    fn migrate(mut section: Value) -> Value {
        assert!(KeybindingsConfig::migrate_legacy_section(&mut section).is_empty());
        section
    }

    #[test]
    fn defaults_are_disabled_and_empty() {
        let config = KeybindingsConfig::default();
        assert!(!config.enabled());
        assert!(config.bindings.is_empty());
        assert_eq!(
            serde_json::to_value(&config).expect("encode"),
            json!({ "enabled": false, "bindings": {} })
        );
        let decoded: KeybindingsConfig = serde_json::from_str("{}").expect("decode");
        assert_eq!(decoded, config);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn values_keep_their_string_or_array_shape_and_order() {
        let config: KeybindingsConfig = serde_json::from_value(json!({
            "enabled": true,
            "bindings": {
                "Command+K": "open -a Terminal",
                "Control+J": ["b", "a", ""],
                "§+X": []
            }
        }))
        .expect("decode");
        assert_eq!(
            serde_json::to_value(&config).expect("encode")["bindings"]["Control+J"],
            json!(["b", "a", ""])
        );
        assert_eq!(
            serde_json::to_value(&config).expect("encode")["bindings"]["Command+K"],
            json!("open -a Terminal")
        );
        assert_eq!(config.bindings["Control+J"].commands(), ["b", "a"]);
    }

    #[test]
    fn malformed_shortcut_syntax_is_not_a_file_rejection() {
        let config: KeybindingsConfig = serde_json::from_value(json!({
            "bindings": { "Hyper++": "x", "": "y", "Nonsense": [] }
        }))
        .expect("decode");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn non_string_values_and_elements_are_rejected_with_field_paths() {
        let catalog = {
            let mut catalog = ConfigCatalog::new();
            catalog.register::<KeybindingsConfig>(KeybindingsConfig::ID).expect("register");
            catalog
        };
        let source = std::path::Path::new("keybindings.jsonc");
        for (text, path) in [
            (r#"{ "bindings": { "A": 1 } }"#, "bindings.A"),
            (r#"{ "bindings": { "A": ["x", 2] } }"#, "bindings.A"),
            (r#"{ "bindings": { "A": null } }"#, "bindings.A"),
            (r#"{ "bindings": ["x"] }"#, "bindings"),
            (r#"{ "enabled": "yes" }"#, "enabled"),
        ] {
            let error = catalog
                .inspect(KeybindingsConfig::ID, source, text)
                .expect_err(text)
                .to_string();
            assert!(error.contains(path), "{text}: {error}");
        }
    }

    #[test]
    fn migration_wraps_the_whole_map_and_enables_only_a_nonempty_map() {
        assert_eq!(
            migrate(json!({ "Command+K": "open -a Terminal", "Control+J": ["a", "b"] })),
            json!({
                "enabled": true,
                "bindings": { "Command+K": "open -a Terminal", "Control+J": ["a", "b"] }
            })
        );
        assert_eq!(migrate(json!({})), json!({ "enabled": false, "bindings": {} }));
        // Blank-only maps are still nonempty: the user declared shortcuts.
        assert_eq!(
            migrate(json!({ "Command+K": "", "Command+J": ["  "] })),
            json!({
                "enabled": true,
                "bindings": { "Command+K": "", "Command+J": ["  "] }
            })
        );
    }

    #[test]
    fn shortcuts_named_enabled_or_bindings_are_bindings_not_the_envelope() {
        assert_eq!(
            migrate(json!({ "enabled": "echo on", "bindings": ["echo a", "echo b"] })),
            json!({
                "enabled": true,
                "bindings": { "enabled": "echo on", "bindings": ["echo a", "echo b"] }
            })
        );
        assert_eq!(
            migrate(json!({ "bindings": { "Command+K": "x" } })),
            json!({ "enabled": true, "bindings": { "bindings": { "Command+K": "x" } } }),
            "an envelope-looking legacy map is still wrapped, and then fails type validation"
        );
        let wrapped = migrate(json!({ "bindings": { "Command+K": "x" } }));
        assert!(serde_json::from_value::<KeybindingsConfig>(wrapped).is_err());
    }

    #[test]
    fn nonobject_sections_are_left_for_validation_to_reject() {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<KeybindingsConfig>(KeybindingsConfig::ID).expect("register");
        for section in [
            json!([]),
            json!(["x"]),
            json!("x"),
            json!(null),
            json!(3),
            json!(true),
        ] {
            let mut migrated = section.clone();
            assert!(KeybindingsConfig::migrate_legacy_section(&mut migrated).is_empty());
            assert_eq!(migrated, section);
            let result = catalog.inspect(
                KeybindingsConfig::ID,
                std::path::Path::new("keybindings.jsonc"),
                &migrated.to_string(),
            );
            assert!(result.is_err(), "{section} must stay invalid: {result:?}");
        }
    }

    #[test]
    fn the_template_is_the_disabled_default() {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<KeybindingsConfig>(KeybindingsConfig::ID).expect("register");
        assert_eq!(
            catalog.file_name(KeybindingsConfig::ID).as_deref(),
            Some("keybindings.jsonc")
        );
        assert_eq!(
            catalog.default_value(KeybindingsConfig::ID),
            Some(&json!({ "enabled": false, "bindings": {} }))
        );
        let template = catalog.template(KeybindingsConfig::ID).expect("template");
        assert!(template.contains("\"enabled\": false"), "{template}");
        assert!(template.contains("\"bindings\": {}"), "{template}");
    }
}
