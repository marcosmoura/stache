//! Authoritative Bar configuration.
//!
//! This is configuration only; the panel runtime that consumes it lives in
//! `runtime`/`module` and is registered in the production host (default off).
//! Structural decoding and native feasibility are separate; geometry limits are
//! owned by the panel runtime (`window::plan`), so every `u16` dimension
//! (including zero) is a valid value here.

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
#[serde(rename_all = "snake_case")]
pub enum WeatherProvider {
    /// Use Visual Crossing when an API key file is configured, otherwise Open Meteo.
    #[default]
    Auto,
    /// Force Visual Crossing (requires an API key file).
    VisualCrossing,
    /// Use Open Meteo (free, no API key required).
    OpenMeteo,
}

/// Default weather refresh interval in minutes.
pub const DEFAULT_WEATHER_REFRESH_MINUTES: u32 = 20;
/// Smallest accepted weather refresh interval in minutes.
pub const MIN_WEATHER_REFRESH_MINUTES: u32 = 5;
/// Largest accepted weather refresh interval in minutes (24 hours).
pub const MAX_WEATHER_REFRESH_MINUTES: u32 = 1440;

const fn default_refresh_interval() -> u32 { DEFAULT_WEATHER_REFRESH_MINUTES }

fn deserialize_refresh_interval<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<u32, D::Error> {
    let value = u32::deserialize(deserializer)?;
    check_refresh_interval(value).map_err(serde::de::Error::custom)?;
    Ok(value)
}

fn check_refresh_interval(value: u32) -> Result<(), String> {
    if (MIN_WEATHER_REFRESH_MINUTES..=MAX_WEATHER_REFRESH_MINUTES).contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "{value} is outside the accepted range {MIN_WEATHER_REFRESH_MINUTES} to {MAX_WEATHER_REFRESH_MINUTES} minutes"
        ))
    }
}

/// Weather settings for the status bar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(default, rename_all = "snake_case")]
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
    /// Default location for weather data, used when native macOS Location is
    /// unavailable, denied or times out. Can be a city name, address, or
    /// coordinates.
    pub default_location: String,
    /// Whether weather asks macOS for a precise location.
    ///
    /// `false` (the default) requests an approximate, kilometer-level location
    /// and accepts the approximate-location choice the user made in macOS
    /// Location settings. `true` requests the best accuracy and, only when macOS
    /// reports approximate access and the app bundle declares a purpose, asks at
    /// most once per run for temporary full accuracy. Neither setting re-asks
    /// for Location access once macOS has recorded a decision.
    pub precise_location: bool,
    /// Minutes between automatic weather refreshes, from 5 to 1440 (24 hours).
    /// Applies after a configuration reload without restarting Bar.
    #[specta(type = u32)]
    #[serde(deserialize_with = "deserialize_refresh_interval")]
    #[schemars(range(min = 5, max = 1440))]
    pub refresh_interval: u32,
}

impl Default for WeatherConfig {
    fn default() -> Self {
        Self {
            provider: WeatherProvider::default(),
            api_keys: String::new(),
            default_location: String::new(),
            precise_location: false,
            refresh_interval: default_refresh_interval(),
        }
    }
}

impl WeatherConfig {
    /// Resolves `api_keys` against injected home and configuration directories.
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
                ConfigError::new("weather.api_keys starts with `~` but no home directory is known")
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

/// Status bar configuration (the Bar module reads it; default disabled).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(default, rename_all = "snake_case")]
pub struct BarConfig {
    /// Whether the status bar is enabled.
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
    /// The catalog identifier and file stem (`bar.toml`).
    pub const ID: ModuleId = ModuleId::new("bar");

    /// The checked sum of height and padding in a wider type.
    #[must_use]
    pub fn height_plus_padding(&self) -> u32 { u32::from(self.height) + u32::from(self.padding) }
}

impl ModuleConfig for BarConfig {
    fn enabled(&self) -> bool { self.enabled }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.weather.api_keys.contains('\0') {
            return Err(ConfigError::new("weather.api_keys must not contain a NUL byte"));
        }
        check_refresh_interval(self.weather.refresh_interval)
            .map_err(|message| ConfigError::new(format!("weather.refresh_interval: {message}")))
    }

    /// Materializes dimensions that the legacy derive rendered as zero and
    /// flags `weather.api_keys` paths whose historical base is unknown.
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
        // Legacy has no `preciseLocation` or `refreshInterval`; the migrated file states the defaults.
        // A `weather` value that is not a table is left for the typed decoder.
        let weather = object
            .entry("weather")
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if let Some(weather) = weather.as_object_mut() {
            weather.entry("preciseLocation").or_insert(serde_json::Value::Bool(false));
            weather
                .entry("refreshInterval")
                .or_insert(serde_json::Value::from(DEFAULT_WEATHER_REFRESH_MINUTES));
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
                    "bar `weather.api_keys` value `{keys}` is relative; the migrated file resolves it under the configuration directory, which may differ from the legacy base or working directory, so rebase it manually if needed"
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
        catalog().inspect(BarConfig::ID, Path::new("bar.toml"), text)
    }

    fn decode(text: &str) -> BarConfig {
        catalog()
            .decode_typed(BarConfig::ID, Path::new("bar.toml"), text)
            .expect("decode")
            .0
    }

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
                "weather": {
                    "provider": "auto",
                    "api_keys": "",
                    "default_location": "",
                    "precise_location": false,
                    "refresh_interval": 20,
                },
            })
        );
        assert_eq!(config.height_plus_padding(), 40);
    }

    #[test]
    fn empty_objects_and_omitted_nested_fields_use_defaults() {
        assert_eq!(decode("# defaults\n"), BarConfig::default());
        assert_eq!(decode("weather = {}\n"), BarConfig::default());
        assert_eq!(decode("height = 30\n").padding, 12);
        assert_eq!(decode("padding = 3\n").height, 28);
        let weather = decode("weather = { default_location = \"Lisbon\" }\n").weather;
        assert_eq!(weather.provider, WeatherProvider::Auto);
        assert_eq!(weather.api_keys, "");
        assert_eq!(weather.default_location, "Lisbon");
        let weather = decode("weather = { api_keys = \".env\" }\n").weather;
        assert_eq!(weather.api_keys, ".env");
        assert_eq!(weather.default_location, "");
    }

    #[test]
    fn every_provider_spelling_decodes() {
        for (text, expected) in [
            ("auto", WeatherProvider::Auto),
            ("visual_crossing", WeatherProvider::VisualCrossing),
            ("open_meteo", WeatherProvider::OpenMeteo),
        ] {
            let config = decode(&format!("weather = {{ provider = \"{text}\" }}\n"));
            assert_eq!(config.weather.provider, expected);
            assert_eq!(serde_json::to_value(expected).expect("encode"), json!(text));
            inspect(&format!("weather = {{ provider = \"{text}\" }}\n")).expect("valid");
        }
        for bad in ["visualCrossing", "Auto", "other", ""] {
            let error = inspect(&format!("weather = {{ provider = \"{bad}\" }}\n"))
                .expect_err("unknown provider");
            assert_eq!(error.kind, DiagnosticKind::Type, "{bad}");
        }
    }

    #[test]
    fn explicit_zero_and_max_dimensions_are_preserved_and_valid() {
        let config = decode("height = 0\npadding = 0\n");
        assert_eq!((config.height, config.padding), (0, 0));
        let config = decode("height = 65535\npadding = 65535\n");
        assert_eq!((config.height, config.padding), (u16::MAX, u16::MAX));
        assert_eq!(config.height_plus_padding(), 131_070);
        inspect("height = 65535\npadding = 65535\n").expect("beyond 8192 stays valid");
        inspect("height = 0\npadding = 0\n").expect("zero is valid");
    }

    #[test]
    fn negative_overflowing_and_fractional_dimensions_are_type_diagnostics() {
        for value in ["-1", "65536", "1.5", r#""28""#, "[]"] {
            for field in ["height", "padding"] {
                let error = inspect(&format!("{field} = {value}\n")).expect_err(value);
                assert_eq!(error.kind, DiagnosticKind::Type, "{field} {value}");
                assert_eq!(error.field.as_deref(), Some(field), "{field} {value}");
            }
        }
    }

    #[test]
    fn enabled_true_validates_without_claiming_a_runtime() {
        inspect("enabled = true\n").expect("valid");
        assert!(decode("enabled = true\n").enabled());
    }

    #[test]
    fn malformed_wrong_typed_and_empty_files_are_diagnostics() {
        for value in [r#""yes""#, "1", "[]"] {
            let error = inspect(&format!("enabled = {value}\n")).expect_err(value);
            assert_eq!(error.kind, DiagnosticKind::Type, "{value}");
        }
        assert!(inspect("{ not toml").is_err());
        let error = inspect("  \n").expect_err("empty file");
        assert!(error.message.contains("file is empty"), "{error}");
        assert!(
            inspect("# defaults\n").is_ok(),
            "an empty object versus an empty file"
        );
    }

    #[test]
    fn unknown_keys_are_warnings() {
        let warnings = inspect("enabled = true\nfuture = 1\n").expect("valid");
        assert_eq!(warnings, ["future"]);
    }

    #[test]
    fn nul_bytes_in_api_keys_are_rejected() {
        let error = inspect("weather = { api_keys = \"a\\u0000b\" }\n").expect_err("NUL");
        assert!(error.message.contains("NUL"), "{error}");
    }

    #[test]
    fn template_and_schema_come_from_the_single_type() {
        let catalog = catalog();
        let template = catalog.template(BarConfig::ID).expect("template");
        assert!(template.starts_with("#:schema "), "{template}");
        assert!(template.contains("\nheight = 28\n"), "{template}");
        assert!(template.contains("\npadding = 12\n"), "{template}");
        assert!(template.contains("\nenabled = false\n"), "{template}");
        assert!(template.contains("\n[weather]\n"), "{template}");
        let schema = catalog.schema_json(BarConfig::ID).expect("schema").expect("render");
        let schema: serde_json::Value = serde_json::from_str(&schema).expect("schema JSON");
        assert_eq!(schema["title"], "BarConfig");
        assert_eq!(schema["properties"]["height"]["default"], 28);
        assert_eq!(schema["properties"]["padding"]["default"], 12);
        assert_eq!(schema["properties"]["height"]["maximum"], 65535);
        assert_eq!(schema["properties"]["height"]["minimum"], 0);
        let rendered = schema.to_string();
        assert!(rendered.contains("precise_location"), "{rendered}");
        assert!(template.contains("precise_location = false"), "{template}");
        assert!(template.contains("refresh_interval = 20"), "{template}");
        let interval = &schema["properties"]["weather"];
        let rendered_interval = interval.to_string();
        assert!(
            rendered_interval.contains("refresh_interval"),
            "{rendered_interval}"
        );
        for spelling in ["auto", "visual_crossing", "open_meteo"] {
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
        let mut section = json!({
            "enabled": true,
            "height": 0,
            "padding": 0,
            "weather": { "preciseLocation": true, "refreshInterval": 45 },
        });
        let before = section.clone();
        assert!(BarConfig::migrate_legacy_section(&mut section).is_empty());
        assert_eq!(section, before);
    }

    #[test]
    fn migration_materializes_omitted_dimensions_with_warnings() {
        let mut section = json!({ "enabled": true });
        let notes = BarConfig::migrate_legacy_section(&mut section);
        assert_eq!(
            section,
            json!({
                "enabled": true,
                "height": 28,
                "padding": 12,
                "weather": { "preciseLocation": false, "refreshInterval": 20 },
            })
        );
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
        assert_eq!(
            section,
            json!({ "height": 40, "padding": 12, "weather": { "preciseLocation": false, "refreshInterval": 20 } })
        );
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("`padding`"));
    }

    #[test]
    fn migration_materializes_precise_location_silently() {
        let mut section = json!({ "height": 1, "padding": 1 });
        assert!(BarConfig::migrate_legacy_section(&mut section).is_empty());
        assert_eq!(
            section["weather"],
            json!({ "preciseLocation": false, "refreshInterval": 20 })
        );

        let mut section = json!({ "height": 1, "padding": 1, "weather": { "provider": "auto" } });
        assert!(BarConfig::migrate_legacy_section(&mut section).is_empty());
        assert_eq!(
            section["weather"],
            json!({ "provider": "auto", "preciseLocation": false, "refreshInterval": 20 })
        );

        let mut section = json!({ "height": 1, "padding": 1, "weather": "oops" });
        assert!(BarConfig::migrate_legacy_section(&mut section).is_empty());
        assert_eq!(
            section["weather"], "oops",
            "a non-table is left to the typed decoder"
        );
    }

    #[test]
    fn precise_location_defaults_to_false_and_decodes_explicit_values() {
        assert!(!decode("# defaults\n").weather.precise_location);
        assert!(!decode("weather = {}\n").weather.precise_location);
        assert!(decode("weather = { precise_location = true }\n").weather.precise_location);
        assert!(!decode("weather = { precise_location = false }\n").weather.precise_location);
    }

    #[test]
    fn a_wrong_typed_precise_location_is_a_diagnostic_at_its_field() {
        for value in [r#""yes""#, "1", "[]"] {
            let error =
                inspect(&format!("weather = {{ precise_location = {value} }}\n")).expect_err(value);
            assert_eq!(error.kind, DiagnosticKind::Type, "{value}");
            assert_eq!(
                error.field.as_deref(),
                Some("weather.precise_location"),
                "{value}"
            );
        }
    }

    #[test]
    fn refresh_interval_defaults_to_twenty_and_accepts_the_bounds() {
        assert_eq!(decode("# defaults\n").weather.refresh_interval, 20);
        assert_eq!(decode("weather = {}\n").weather.refresh_interval, 20);
        for minutes in [5, 20, 45, 1440] {
            let text = format!("weather = {{ refresh_interval = {minutes} }}\n");
            inspect(&text).expect("valid");
            assert_eq!(decode(&text).weather.refresh_interval, minutes);
        }
    }

    #[test]
    fn refresh_interval_outside_5_to_1440_or_mistyped_is_rejected_at_its_field() {
        for value in [
            "4",
            "1441",
            "0",
            "-1",
            "1.5",
            r#""20""#,
            "true",
            "[]",
            "4294967296",
        ] {
            let error =
                inspect(&format!("weather = {{ refresh_interval = {value} }}\n")).expect_err(value);
            assert_eq!(error.kind, DiagnosticKind::Type, "{value}");
            assert_eq!(
                error.field.as_deref(),
                Some("weather.refresh_interval"),
                "{value}"
            );
        }
        let mut config = BarConfig::default();
        config.weather.refresh_interval = 4;
        assert!(config.validate().expect_err("below").message().contains("refresh_interval"));
        config.weather.refresh_interval = 1441;
        assert!(config.validate().is_err());
    }

    #[test]
    fn refresh_interval_schema_states_the_range() {
        let schema = catalog().schema_json(BarConfig::ID).expect("schema").expect("render");
        let schema: serde_json::Value = serde_json::from_str(&schema).expect("JSON");
        let found = find_key(&schema, "refresh_interval").expect("property");
        assert_eq!(found["minimum"], 5, "{found}");
        assert_eq!(found["maximum"], 1440, "{found}");
    }

    fn find_key<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
        match value {
            serde_json::Value::Object(map) => {
                map.get(key).or_else(|| map.values().find_map(|inner| find_key(inner, key)))
            }
            serde_json::Value::Array(items) => items.iter().find_map(|inner| find_key(inner, key)),
            _ => None,
        }
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
