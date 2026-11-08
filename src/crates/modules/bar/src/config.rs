//! Authoritative Bar configuration.
//!
//! This is configuration only. `enabled: true` validates but starts nothing:
//! no runtime, window or provider consumes it yet. Structural decoding and
//! native feasibility are separate; geometry limits are owned by the later
//! panel part, so every `u16` dimension (including zero) is a valid value here.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use specta::Type;
use stache_sdk::{ConfigError, ModuleConfig, ModuleId};

/// Default bar height in pixels.
const DEFAULT_HEIGHT: u16 = 28;
/// Default bar padding in pixels.
const DEFAULT_PADDING: u16 = 12;

/// Weather data provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "kebab-case")]
pub enum WeatherProvider {
    /// Use Visual Crossing when an API key file is configured, otherwise Open Meteo.
    #[default]
    Auto,
    /// Force Visual Crossing (requires an API key file).
    VisualCrossing,
    /// Use Open Meteo (free, no API key required).
    OpenMeteo,
}

/// Weather settings for the status bar.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(default, rename_all = "camelCase")]
pub struct WeatherConfig {
    /// Weather data provider.
    pub provider: WeatherProvider,
    /// Path to an environment file containing API keys (for example
    /// `VISUAL_CROSSING_API_KEY=...`).
    ///
    /// The value is trimmed. `~` and `~/...` expand to the home directory,
    /// absolute paths are used as written and anything else is relative to the
    /// configuration directory. Environment variables and shell syntax are not
    /// expanded, and the process working directory is never used.
    pub api_keys: String,
    /// Default location for weather data when geolocation fails.
    /// Can be a city name, address, or coordinates.
    pub default_location: String,
}

impl WeatherConfig {
    /// Resolves `apiKeys` against injected home and configuration directories.
    ///
    /// Returns `Ok(None)` for an empty (after trimming) value. Reads no file,
    /// environment variable or process state.
    ///
    /// # Errors
    /// Returns an error when the value is home-anchored but no home directory
    /// is supplied.
    pub fn api_keys_path(
        &self,
        home: Option<&Path>,
        config_dir: &Path,
    ) -> Result<Option<PathBuf>, ConfigError> {
        let value = self.api_keys.trim();
        if value.is_empty() {
            return Ok(None);
        }
        let home_anchored = if value == "~" {
            Some("")
        } else {
            value.strip_prefix("~/")
        };
        if let Some(rest) = home_anchored {
            let home = home.ok_or_else(|| {
                ConfigError::new("weather.apiKeys starts with `~` but no home directory is known")
            })?;
            return Ok(Some(if rest.is_empty() {
                home.to_path_buf()
            } else {
                home.join(rest)
            }));
        }
        let path = Path::new(value);
        Ok(Some(if path.is_absolute() {
            path.to_path_buf()
        } else {
            config_dir.join(path)
        }))
    }
}

/// Status bar configuration (inert: nothing consumes it yet).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(default, rename_all = "camelCase")]
pub struct BarConfig {
    /// Whether the status bar is enabled. Validates today but starts nothing.
    pub enabled: bool,
    /// Height of the status bar in pixels. Zero is preserved, not replaced.
    pub height: u16,
    /// Padding around the status bar in pixels. Zero is preserved, not replaced.
    pub padding: u16,
    /// Weather status bar configuration.
    pub weather: WeatherConfig,
}

impl Default for BarConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            height: DEFAULT_HEIGHT,
            padding: DEFAULT_PADDING,
            weather: WeatherConfig::default(),
        }
    }
}

impl BarConfig {
    /// The catalog identifier and file stem (`bar.jsonc`).
    pub const ID: ModuleId = ModuleId::new("bar");

    /// The checked sum of height and padding in a wider type.
    #[must_use]
    pub fn height_plus_padding(&self) -> u32 { u32::from(self.height) + u32::from(self.padding) }
}

impl ModuleConfig for BarConfig {
    fn enabled(&self) -> bool { self.enabled }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.weather.api_keys.contains('\0') {
            return Err(ConfigError::new("weather.apiKeys must not contain a NUL byte"));
        }
        Ok(())
    }

    /// Materializes dimensions that the legacy derive rendered as zero and
    /// flags `weather.apiKeys` paths whose historical base is unknown.
    fn migrate_legacy_section(section: &mut serde_json::Value) -> Vec<String> {
        let Some(object) = section.as_object_mut() else {
            return Vec::new();
        };
        let mut notes = Vec::new();
        for (key, default) in [("height", DEFAULT_HEIGHT), ("padding", DEFAULT_PADDING)] {
            if !object.contains_key(key) {
                object.insert(key.to_owned(), serde_json::Value::from(default));
                notes.push(format!(
                    "bar `{key}` was omitted; the legacy application rendered an omitted `{key}` as 0, the migrated file sets it to the documented default {default}"
                ));
            }
        }
        if let Some(keys) = object
            .get("weather")
            .and_then(|weather| weather.get("apiKeys"))
            .and_then(serde_json::Value::as_str)
        {
            let keys = keys.trim();
            if !keys.is_empty()
                && keys != "~"
                && !keys.starts_with("~/")
                && !Path::new(keys).is_absolute()
            {
                notes.push(format!(
                    "bar `weather.apiKeys` value `{keys}` is relative; the migrated file resolves it under the configuration directory, which may differ from the legacy base or working directory, so rebase it manually if needed"
                ));
            }
        }
        notes
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use stache_sdk::{ConfigCatalog, DiagnosticKind};

    use super::*;

    fn catalog() -> ConfigCatalog {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<BarConfig>(BarConfig::ID).expect("register");
        catalog
    }

    fn inspect(text: &str) -> Result<Vec<String>, stache_sdk::config::ConfigDiagnostic> {
        catalog().inspect(BarConfig::ID, Path::new("bar.jsonc"), text)
    }

    fn decode(text: &str) -> BarConfig { serde_json::from_str(text).expect("decode") }

    #[test]
    fn defaults_match_the_spec() {
        let config = BarConfig::default();
        assert!(!config.enabled());
        assert_eq!(BarConfig::ID.as_str(), "bar");
        assert_eq!(
            serde_json::to_value(&config).expect("encode"),
            json!({
                "enabled": false,
                "height": 28,
                "padding": 12,
                "weather": { "provider": "auto", "apiKeys": "", "defaultLocation": "" },
            })
        );
        assert_eq!(config.height_plus_padding(), 40);
    }

    #[test]
    fn empty_objects_and_omitted_nested_fields_use_defaults() {
        assert_eq!(decode("{}"), BarConfig::default());
        assert_eq!(decode(r#"{ "weather": {} }"#), BarConfig::default());
        assert_eq!(decode(r#"{ "height": 30 }"#).padding, 12);
        assert_eq!(decode(r#"{ "padding": 3 }"#).height, 28);
        let weather = decode(r#"{ "weather": { "defaultLocation": "Lisbon" } }"#).weather;
        assert_eq!(weather.provider, WeatherProvider::Auto);
        assert_eq!(weather.api_keys, "");
        assert_eq!(weather.default_location, "Lisbon");
        let weather = decode(r#"{ "weather": { "apiKeys": ".env" } }"#).weather;
        assert_eq!(weather.api_keys, ".env");
        assert_eq!(weather.default_location, "");
    }

    #[test]
    fn every_provider_spelling_decodes() {
        for (text, expected) in [
            ("auto", WeatherProvider::Auto),
            ("visual-crossing", WeatherProvider::VisualCrossing),
            ("open-meteo", WeatherProvider::OpenMeteo),
        ] {
            let config = decode(&format!(r#"{{ "weather": {{ "provider": "{text}" }} }}"#));
            assert_eq!(config.weather.provider, expected);
            assert_eq!(serde_json::to_value(expected).expect("encode"), json!(text));
            inspect(&format!(r#"{{ "weather": {{ "provider": "{text}" }} }}"#)).expect("valid");
        }
        for bad in ["visualCrossing", "Auto", "other", ""] {
            let error = inspect(&format!(r#"{{ "weather": {{ "provider": "{bad}" }} }}"#))
                .expect_err("unknown provider");
            assert_eq!(error.kind, DiagnosticKind::Type, "{bad}");
        }
    }

    #[test]
    fn explicit_zero_and_max_dimensions_are_preserved_and_valid() {
        let config = decode(r#"{ "height": 0, "padding": 0 }"#);
        assert_eq!((config.height, config.padding), (0, 0));
        let config = decode(r#"{ "height": 65535, "padding": 65535 }"#);
        assert_eq!((config.height, config.padding), (u16::MAX, u16::MAX));
        assert_eq!(config.height_plus_padding(), 131_070);
        inspect(r#"{ "height": 65535, "padding": 65535 }"#).expect("beyond 8192 stays valid");
        inspect(r#"{ "height": 0, "padding": 0 }"#).expect("zero is valid");
    }

    #[test]
    fn negative_overflowing_and_fractional_dimensions_are_type_diagnostics() {
        for value in ["-1", "65536", "1.5", r#""28""#, "null"] {
            for field in ["height", "padding"] {
                let error = inspect(&format!(r#"{{ "{field}": {value} }}"#)).expect_err(value);
                assert_eq!(error.kind, DiagnosticKind::Type, "{field} {value}");
                assert_eq!(error.field.as_deref(), Some(field), "{field} {value}");
            }
        }
    }

    #[test]
    fn enabled_true_validates_without_claiming_a_runtime() {
        inspect(r#"{ "enabled": true }"#).expect("valid");
        assert!(decode(r#"{ "enabled": true }"#).enabled());
    }

    #[test]
    fn malformed_wrong_typed_and_empty_files_are_diagnostics() {
        for value in [r#""yes""#, "1", "null"] {
            let error = inspect(&format!(r#"{{ "enabled": {value} }}"#)).expect_err(value);
            assert_eq!(error.kind, DiagnosticKind::Type, "{value}");
        }
        assert!(inspect("{ not json").is_err());
        let error = inspect("  \n").expect_err("empty file");
        assert!(error.message.contains("file is empty"), "{error}");
        assert!(inspect("{}").is_ok(), "an empty object versus an empty file");
    }

    #[test]
    fn unknown_keys_are_warnings() {
        let warnings = inspect(r#"{ "enabled": true, "future": 1 }"#).expect("valid");
        assert_eq!(warnings, ["future"]);
    }

    #[test]
    fn nul_bytes_in_api_keys_are_rejected() {
        let error = inspect(r#"{ "weather": { "apiKeys": "a\u0000b" } }"#).expect_err("NUL");
        assert!(error.message.contains("NUL"), "{error}");
    }

    #[test]
    fn template_and_schema_come_from_the_single_type() {
        let catalog = catalog();
        let template = catalog.template(BarConfig::ID).expect("template");
        assert!(template.contains(r#""height": 28"#), "{template}");
        assert!(template.contains(r#""padding": 12"#), "{template}");
        assert!(template.contains(r#""enabled": false"#), "{template}");
        let schema = catalog.schema_json(BarConfig::ID).expect("schema").expect("render");
        let schema: serde_json::Value = serde_json::from_str(&schema).expect("schema JSON");
        assert_eq!(schema["title"], "BarConfig");
        assert_eq!(schema["properties"]["height"]["default"], 28);
        assert_eq!(schema["properties"]["padding"]["default"], 12);
        assert_eq!(schema["properties"]["height"]["maximum"], 65535);
        assert_eq!(schema["properties"]["height"]["minimum"], 0);
        let rendered = schema.to_string();
        for spelling in ["auto", "visual-crossing", "open-meteo"] {
            assert!(rendered.contains(spelling), "{spelling}");
        }
    }

    #[test]
    fn a_duplicate_registration_is_rejected() {
        let mut catalog = catalog();
        assert!(catalog.register::<BarConfig>(BarConfig::ID).is_err());
    }

    #[test]
    fn migration_keeps_explicit_values_including_zero_silently() {
        let mut section = json!({ "enabled": true, "height": 0, "padding": 0 });
        let before = section.clone();
        assert!(BarConfig::migrate_legacy_section(&mut section).is_empty());
        assert_eq!(section, before);
    }

    #[test]
    fn migration_materializes_omitted_dimensions_with_warnings() {
        let mut section = json!({ "enabled": true });
        let notes = BarConfig::migrate_legacy_section(&mut section);
        assert_eq!(section, json!({ "enabled": true, "height": 28, "padding": 12 }));
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(
            notes[0].contains("`height`") && notes[0].contains("as 0"),
            "{notes:?}"
        );
        assert!(
            notes[1].contains("`padding`") && notes[1].contains("12"),
            "{notes:?}"
        );

        let mut section = json!({ "height": 40 });
        let notes = BarConfig::migrate_legacy_section(&mut section);
        assert_eq!(section, json!({ "height": 40, "padding": 12 }));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("`padding`"));
    }

    #[test]
    fn migration_ignores_non_objects_and_retains_unknown_keys() {
        let mut value = json!(true);
        assert!(BarConfig::migrate_legacy_section(&mut value).is_empty());
        assert_eq!(value, json!(true));
        let mut section = json!({ "height": 1, "padding": 2, "future": 3 });
        assert!(BarConfig::migrate_legacy_section(&mut section).is_empty());
        assert_eq!(section["future"], 3);
    }

    #[test]
    fn migration_warns_about_relative_api_keys_and_keeps_the_text() {
        for relative in [".env", "  secrets/.env  ", "~other/.env"] {
            let mut section =
                json!({ "height": 1, "padding": 1, "weather": { "apiKeys": relative } });
            let notes = BarConfig::migrate_legacy_section(&mut section);
            assert_eq!(notes.len(), 1, "{relative}: {notes:?}");
            assert!(notes[0].contains("configuration directory"), "{notes:?}");
            assert_eq!(section["weather"]["apiKeys"], relative, "text is retained");
        }
        for quiet in ["", "   ", "/abs/.env", "~", "~/x/.env"] {
            let mut section = json!({ "height": 1, "padding": 1, "weather": { "apiKeys": quiet } });
            assert!(
                BarConfig::migrate_legacy_section(&mut section).is_empty(),
                "{quiet}"
            );
        }
    }

    #[test]
    fn api_keys_paths_resolve_deterministically_from_injected_context() {
        let home = Path::new("/home/test");
        let config_dir = Path::new("/cfg/stache");
        let resolve = |value: &str, home: Option<&Path>| {
            WeatherConfig {
                api_keys: value.to_owned(),
                ..WeatherConfig::default()
            }
            .api_keys_path(home, config_dir)
        };
        let some = |path: &str| Ok(Some(PathBuf::from(path)));
        assert_eq!(resolve("", Some(home)), Ok(None));
        assert_eq!(resolve("   ", Some(home)), Ok(None));
        assert_eq!(resolve("  ~  ", Some(home)), some("/home/test"));
        assert_eq!(
            resolve("~/.secrets/.env", Some(home)),
            some("/home/test/.secrets/.env")
        );
        assert_eq!(resolve("/abs/.env", Some(home)), some("/abs/.env"));
        assert_eq!(resolve(".env", Some(home)), some("/cfg/stache/.env"));
        assert_eq!(resolve("secrets/.env", None), some("/cfg/stache/secrets/.env"));
        assert_eq!(
            resolve("~other/.env", Some(home)),
            some("/cfg/stache/~other/.env")
        );
        assert_eq!(resolve("$HOME/.env", Some(home)), some("/cfg/stache/$HOME/.env"));
        assert!(resolve("~/x", None).is_err());
        assert!(resolve("~", None).is_err());
    }
}
