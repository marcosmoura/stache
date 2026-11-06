//! Authoritative Wallpapers configuration.

use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use specta::Type;
use stache_sdk::{ConfigError, ModuleConfig, ModuleId};

/// Wallpaper cycling mode.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "lowercase")]
pub enum WallpaperMode {
    /// Select a random wallpaper each time.
    #[default]
    Random,
    /// Cycle through wallpapers in order.
    Sequential,
}

/// Wallpaper source selection, rotation timing and image effects.
///
/// Defaults are disabled with no sources. A nonempty `path` replaces `list`
/// entirely, including when that directory is invalid. Configured source
/// strings are trimmed; paths are resolved relative to the configuration
/// directory, never the process working directory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(default, rename_all = "camelCase")]
pub struct WallpapersConfig {
    /// Whether Wallpapers should run.
    pub enabled: bool,
    /// Directory containing wallpaper images.
    /// If specified, all image files in this directory are used and the `list`
    /// field is ignored.
    pub path: String,
    /// Wallpaper filenames to use. If `path` is specified, this list is ignored.
    pub list: Vec<String>,
    /// Time in seconds between wallpaper changes. Zero changes only once, when
    /// rotation starts.
    ///
    /// The Rust field and schema are `u64`; the generated TypeScript uses
    /// `number`, which loses precision above 2^53.
    #[specta(type = specta_typescript::Number)]
    pub interval: u64,
    /// Wallpaper selection mode: random or sequential.
    pub mode: WallpaperMode,
    /// Radius in pixels for rounded corners.
    pub radius: u32,
    /// Blur level in pixels for the Gaussian blur effect.
    pub blur: u32,
}

impl WallpapersConfig {
    /// The catalog identifier and file stem (`wallpapers.jsonc`).
    pub const ID: ModuleId = ModuleId::new("wallpapers");
}

impl ModuleConfig for WallpapersConfig {
    fn enabled(&self) -> bool { self.enabled }

    /// Rejects structurally impossible source strings without touching disk.
    ///
    /// Missing, unreadable or unsupported sources are source-resolution
    /// failures, not configuration failures; an empty source list is valid.
    fn validate(&self) -> Result<(), ConfigError> {
        if self.path.contains('\0') {
            return Err(ConfigError::new("path must not contain a NUL byte"));
        }
        for (index, entry) in self.list.iter().enumerate() {
            if entry.contains('\0') {
                return Err(ConfigError::new(format!(
                    "list[{index}] must not contain a NUL byte"
                )));
            }
        }
        Ok(())
    }

    /// Warns about bare relative paths whose historical base cannot be known.
    ///
    /// The value itself is preserved: the new file resolves it under the
    /// configuration directory, while the legacy working directory is not
    /// recoverable from the migrated file.
    fn migrate_legacy_section(section: &mut serde_json::Value) -> Vec<String> {
        let Some(object) = section.as_object() else {
            return Vec::new();
        };
        let mut notes = Vec::new();
        if let Some(path) = object.get("path").and_then(serde_json::Value::as_str) {
            note_relative_source("path", path, &mut notes);
        }
        if let Some(entries) = object.get("list").and_then(serde_json::Value::as_array) {
            for (index, entry) in entries.iter().enumerate() {
                if let Some(entry) = entry.as_str() {
                    note_relative_source(&format!("list[{index}]"), entry, &mut notes);
                }
            }
        }
        notes
    }
}

fn note_relative_source(field: &str, value: &str, notes: &mut Vec<String>) {
    let value = value.trim();
    if value.is_empty() || value == "~" || value.starts_with("~/") || Path::new(value).is_absolute()
    {
        return;
    }
    notes.push(format!(
        "wallpapers `{field}` value `{value}` is relative; the migrated file resolves it under the configuration directory, not the legacy working directory, so rebase it manually if it was relative to the old working directory"
    ));
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;
    use stache_sdk::{ConfigCatalog, DiagnosticKind};

    use super::*;

    fn catalog() -> ConfigCatalog {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<WallpapersConfig>(WallpapersConfig::ID).expect("register");
        catalog
    }

    fn inspect(text: &str) -> Result<Vec<String>, stache_sdk::config::ConfigDiagnostic> {
        catalog().inspect(WallpapersConfig::ID, Path::new("wallpapers.jsonc"), text)
    }

    #[test]
    fn defaults_are_disabled_with_no_sources() {
        let config = WallpapersConfig::default();
        assert!(!config.enabled());
        assert_eq!(config.path, "");
        assert!(config.list.is_empty());
        assert_eq!(config.interval, 0);
        assert_eq!(config.mode, WallpaperMode::Random);
        assert_eq!(config.radius, 0);
        assert_eq!(config.blur, 0);
        assert_eq!(
            serde_json::to_value(&config).expect("encode"),
            json!({
                "enabled": false,
                "path": "",
                "list": [],
                "interval": 0,
                "mode": "random",
                "radius": 0,
                "blur": 0,
            })
        );
        assert_eq!(WallpapersConfig::ID.as_str(), "wallpapers");
    }

    #[test]
    fn empty_objects_keep_every_field_default() {
        let config: WallpapersConfig = serde_json::from_str("{}").expect("decode");
        assert_eq!(config, WallpapersConfig::default());
    }

    #[test]
    fn explicit_values_roundtrip_including_the_sequential_mode() {
        let text = r#"{
            "enabled": true,
            "path": "~/Pictures",
            "list": ["a.jpg", "b.png"],
            "interval": 60,
            "mode": "sequential",
            "radius": 12,
            "blur": 4
        }"#;
        let config: WallpapersConfig = serde_json::from_str(text).expect("decode");
        assert!(config.enabled);
        assert_eq!(config.path, "~/Pictures");
        assert_eq!(config.list, ["a.jpg", "b.png"]);
        assert_eq!(config.interval, 60);
        assert_eq!(config.mode, WallpaperMode::Sequential);
        assert_eq!(config.radius, 12);
        assert_eq!(config.blur, 4);
        let encoded = serde_json::to_value(&config).expect("encode");
        assert_eq!(encoded["mode"], "sequential");
    }

    #[test]
    fn numeric_boundaries_are_accepted() {
        let text = format!(
            r#"{{ "interval": {}, "radius": {}, "blur": {} }}"#,
            u64::MAX,
            u32::MAX,
            u32::MAX
        );
        let config: WallpapersConfig = serde_json::from_str(&text).expect("decode");
        assert_eq!(config.interval, u64::MAX);
        assert_eq!(config.radius, u32::MAX);
        assert_eq!(config.blur, u32::MAX);
        inspect(&text).expect("the boundary values are valid");
    }

    #[test]
    fn numeric_overflows_and_negatives_are_type_diagnostics() {
        for text in [
            r#"{ "interval": 18446744073709551616 }"#,
            r#"{ "interval": -1 }"#,
            r#"{ "radius": 4294967296 }"#,
            r#"{ "blur": -1 }"#,
            r#"{ "mode": "Random" }"#,
        ] {
            let diagnostic = inspect(text).expect_err(text);
            assert_eq!(diagnostic.kind, DiagnosticKind::Type, "{text}");
        }
    }

    #[test]
    fn a_non_boolean_enabled_value_is_a_type_diagnostic() {
        let diagnostic = inspect(r#"{ "enabled": "yes" }"#).expect_err("wrong type");
        assert_eq!(diagnostic.kind, DiagnosticKind::Type);
        assert_eq!(diagnostic.field.as_deref(), Some("enabled"));
    }

    #[test]
    fn unknown_keys_are_warnings_but_the_file_stays_valid() {
        let warnings = inspect(r#"{ "enabled": true, "enable": true }"#).expect("valid");
        assert_eq!(warnings, ["enable"]);
    }

    #[test]
    fn empty_and_malformed_files_are_rejected_with_diagnostics() {
        let empty = inspect("  \n").expect_err("empty file");
        assert_eq!(empty.kind, DiagnosticKind::Type);
        assert!(empty.message.contains("file is empty"), "{empty}");

        let malformed = inspect("{ \"enabled\": ").expect_err("malformed file");
        assert_eq!(malformed.kind, DiagnosticKind::Parse);
    }

    #[test]
    fn a_nul_byte_is_a_structural_validation_failure() {
        let path = WallpapersConfig {
            path: "bad\0path".to_owned(),
            ..WallpapersConfig::default()
        };
        let error = path.validate().expect_err("NUL path");
        assert!(error.message().contains("path"), "{error}");

        let entry = WallpapersConfig {
            list: vec!["ok.jpg".to_owned(), "bad\0.jpg".to_owned()],
            ..WallpapersConfig::default()
        };
        let error = entry.validate().expect_err("NUL entry");
        assert!(error.message().contains("list[1]"), "{error}");

        let diagnostic = inspect(r#"{ "path": "bad\u0000path" }"#).expect_err("NUL");
        assert_eq!(diagnostic.kind, DiagnosticKind::Validation);
    }

    #[test]
    fn the_template_and_schema_are_deterministic_and_descriptive() {
        let catalog = catalog();
        let template = catalog.template(WallpapersConfig::ID).expect("template");
        for key in [
            "\"enabled\": false",
            "\"path\": \"\"",
            "\"list\": []",
            "\"interval\": 0",
            "\"mode\": \"random\"",
            "\"radius\": 0",
            "\"blur\": 0",
        ] {
            assert!(template.contains(key), "{key}: {template}");
        }
        assert_eq!(
            template,
            catalog.template(WallpapersConfig::ID).expect("template")
        );

        let schema = catalog.schema_json(WallpapersConfig::ID).expect("schema").expect("render");
        assert_eq!(
            schema,
            catalog.schema_json(WallpapersConfig::ID).expect("schema").expect("render")
        );
        assert!(schema.contains("/schemas/wallpapers.schema.json"), "{schema}");
        let schema: serde_json::Value = serde_json::from_str(&schema).expect("schema JSON");
        assert_eq!(schema["title"], "WallpapersConfig");
        assert_eq!(schema["properties"]["enabled"]["default"], false);
        assert_eq!(schema["properties"]["mode"]["default"], "random");
    }

    #[test]
    fn migration_warns_only_for_bare_relative_sources() {
        let mut section = json!({
            "enabled": true,
            "path": "  images  ",
            "list": [
                "/absolute/a.jpg",
                "~/Pictures/b.jpg",
                "~",
                "relative/c.jpg",
                "   ",
                "~other/d.jpg",
            ],
        });
        let notes = WallpapersConfig::migrate_legacy_section(&mut section);
        assert_eq!(notes.len(), 3, "{notes:?}");
        assert!(notes[0].contains("`path`"), "{notes:?}");
        assert!(notes[0].contains("`images`"), "{notes:?}");
        assert!(notes[1].contains("`list[3]`"), "{notes:?}");
        assert!(notes[2].contains("`list[5]`"), "{notes:?}");
        for note in &notes {
            assert!(note.contains("relative"), "{note}");
            assert!(note.contains("configuration directory"), "{note}");
            assert!(note.contains("rebase it manually"), "{note}");
        }
        assert_eq!(
            section,
            json!({
                "enabled": true,
                "path": "  images  ",
                "list": [
                    "/absolute/a.jpg",
                    "~/Pictures/b.jpg",
                    "~",
                    "relative/c.jpg",
                    "   ",
                    "~other/d.jpg",
                ],
            }),
            "migration preserves the source text"
        );
    }

    #[test]
    fn migration_is_silent_for_absolute_home_anchored_and_empty_sources() {
        for section in [
            json!({ "path": "/absolute", "list": ["~/a.jpg", "~"] }),
            json!({ "path": "", "list": [] }),
            json!({ "path": "   ", "list": ["  "] }),
            json!({ "enabled": false }),
            json!({}),
        ] {
            let mut section = section;
            assert!(WallpapersConfig::migrate_legacy_section(&mut section).is_empty());
        }
        let mut not_an_object = json!(["relative.jpg"]);
        assert!(WallpapersConfig::migrate_legacy_section(&mut not_an_object).is_empty());
    }
}
