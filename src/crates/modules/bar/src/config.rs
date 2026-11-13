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

use crate::layout::BarLayoutConfig;

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
    /// Which widgets the Bar shows, in three ordered zones: `start` (leading
    /// edge), `center` (centered on the Bar) and `end` (trailing edge).
    /// An entry is a widget name (`spaces`, `media`, `weather`, `cpu`,
    /// `battery`, `keep_awake`, `wifi`, `clock`) or a table with `widget`, an
    /// optional unique `id` (ASCII letters, digits, `-` or `_`) and that
    /// widget's options: `clock` takes `show_date` and `show_seconds` (default
    /// true) and `hour_cycle` (`"h23"` or `"h12"`, default `"h23"`); `cpu`
    /// takes `temperature_alert` (1 to 150 degrees Celsius, default 85). A
    /// widget may appear more than once. Without `[layout]` the default below
    /// is used; a zone left out of a present `[layout]` is empty.
    ///
    /// Example:
    ///   start = ["spaces"]
    ///   center = ["media"]
    ///   end = ["weather", { widget = "cpu", temperature_alert = 90 }, { widget = "clock", id = "main_clock", show_seconds = false }]
    #[allow(
        clippy::doc_link_with_quotes,
        clippy::doc_markdown,
        reason = "the example is TOML, rendered verbatim into the template comment"
    )]
    pub layout: BarLayoutConfig,
}

impl Default for BarConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            height: DEFAULT_HEIGHT,
            padding: DEFAULT_PADDING,
            weather: WeatherConfig::default(),
            layout: BarLayoutConfig::default(),
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
            .map_err(|message| ConfigError::new(format!("weather.refresh_interval: {message}")))?;
        self.layout.validate()
    }

    /// Drops another widget's options from layout entry tables, warning.
    fn prune_section(section: &mut serde_json::Value) -> Vec<String> {
        crate::layout::prune_foreign_options(section)
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
                "layout": {
                    "start": ["spaces"],
                    "center": ["media"],
                    "end": ["weather", "cpu", "battery", "keep_awake", "wifi", "clock"],
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

    #[test]
    fn an_entry_id_is_a_plain_optional_string_in_the_schema() {
        let schema = catalog().schema_json(BarConfig::ID).expect("schema").expect("render");
        let schema: serde_json::Value = serde_json::from_str(&schema).expect("JSON");
        for fields in [
            "BarEntryNoOptions",
            "BarEntryCpuOptions",
            "BarEntryClockOptions",
        ] {
            let table = &schema["$defs"][fields];
            let id = &table["properties"]["id"];
            assert_eq!(id["type"], "string", "no null in TOML: {id}");
            assert_eq!(id["minLength"], 1, "{id}");
            assert_eq!(id["pattern"], "^[A-Za-z0-9_-]+$", "{id}");
            assert!(table.get("required").is_none(), "{table}");
        }
        // Nothing decodes a null id either.
        let null: Result<crate::layout::BarLayoutEntry, _> =
            serde_json::from_value(json!({ "widget": "clock", "id": null }));
        assert!(null.is_err());
    }

    #[test]
    fn the_entry_schema_has_one_object_per_widget_with_only_its_options() {
        let schema = catalog().schema_json(BarConfig::ID).expect("schema").expect("render");
        let schema: serde_json::Value = serde_json::from_str(&schema).expect("JSON");
        let defs = &schema["$defs"];
        let variants = defs["BarLayoutEntryTable"]["oneOf"].as_array().expect("oneOf");
        let mut seen = Vec::new();
        for variant in variants {
            let widget = variant["properties"]["widget"]["const"].as_str().expect("const");
            assert_eq!(variant["required"], json!(["widget"]), "{variant}");
            let target = variant["$ref"].as_str().expect("$ref");
            let fields = &defs[target.trim_start_matches("#/$defs/")];
            let mut keys: Vec<&str> = fields["properties"]
                .as_object()
                .expect("properties")
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            let expected: &[&str] = match widget {
                "clock" => &["hour_cycle", "id", "show_date", "show_seconds"],
                "cpu" => &["id", "temperature_alert"],
                _ => &["id"],
            };
            assert_eq!(keys, expected, "{widget}");
            seen.push(widget.to_owned());
        }
        assert_eq!(seen, [
            "spaces",
            "media",
            "weather",
            "cpu",
            "battery",
            "keep_awake",
            "wifi",
            "clock"
        ]);
        let alert = &defs["BarEntryCpuOptions"]["properties"]["temperature_alert"];
        assert_eq!((&alert["minimum"], &alert["maximum"]), (&json!(1), &json!(150)));
        assert_eq!(alert["default"], 85);
        let clock = &defs["BarEntryClockOptions"]["properties"];
        assert_eq!(clock["show_date"]["default"], true);
        assert_eq!(clock["show_seconds"]["default"], true);
        assert_eq!(clock["hour_cycle"]["default"], "h23");
        assert_eq!(
            defs["HourCycle"]["oneOf"]
                .as_array()
                .expect("variants")
                .iter()
                .map(|v| v["const"].clone())
                .collect::<Vec<_>>(),
            [json!("h23"), json!("h12")]
        );
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

    // ------------------------------------------------------------ layout

    use crate::layout::{BarLayoutItem, BarLayoutView, BarWidgetKind as K};

    fn ids(view: &BarLayoutView) -> [Vec<(String, K)>; 3] {
        let pairs = |items: &[BarLayoutItem]| {
            items.iter().map(|item| (item.id().to_owned(), item.widget())).collect()
        };
        [pairs(&view.start), pairs(&view.center), pairs(&view.end)]
    }

    fn items(list: &[(&str, K)]) -> Vec<(String, K)> {
        list.iter().map(|(id, kind)| ((*id).to_owned(), *kind)).collect()
    }

    #[test]
    fn a_missing_layout_section_is_the_default_arrangement() {
        let view = decode("# defaults\n").layout.normalized();
        assert_eq!(ids(&view), [
            items(&[("spaces", K::Spaces)]),
            items(&[("media", K::Media)]),
            items(&[
                ("weather", K::Weather),
                ("cpu", K::Cpu),
                ("battery", K::Battery),
                ("keep_awake", K::KeepAwake),
                ("wifi", K::Wifi),
                ("clock", K::Clock),
            ]),
        ]);
    }

    #[test]
    fn a_missing_zone_in_a_present_layout_is_empty_and_empty_layouts_are_valid() {
        let view = decode("[layout]\nend = [\"clock\"]\n").layout.normalized();
        assert_eq!(ids(&view), [vec![], vec![], items(&[("clock", K::Clock)])]);
        inspect("[layout]\n").expect("an empty layout is valid");
        let view = decode("[layout]\n").layout.normalized();
        assert_eq!(view, BarLayoutView::default());
        let view = decode("[layout]\nstart = []\ncenter = []\nend = []\n").layout.normalized();
        assert_eq!(view, BarLayoutView::default());
    }

    #[test]
    fn string_table_and_mixed_entries_decode_with_every_widget_name() {
        let text = "[layout]\nstart = [\"spaces\", { widget = \"media\" }]\ncenter = [{ widget = \"weather\", id = \"w-1\" }]\nend = [\"cpu\", \"battery\", \"keep_awake\", \"wifi\", { widget = \"clock\", id = \"Clock_A\" }]\n";
        inspect(text).expect("valid");
        assert_eq!(ids(&decode(text).layout.normalized()), [
            items(&[("spaces", K::Spaces), ("media", K::Media)]),
            items(&[("w-1", K::Weather)]),
            items(&[
                ("cpu", K::Cpu),
                ("battery", K::Battery),
                ("keep_awake", K::KeepAwake),
                ("wifi", K::Wifi),
                ("Clock_A", K::Clock),
            ]),
        ]);
    }

    #[test]
    fn generated_ids_count_repeats_in_file_order_and_skip_explicit_ids() {
        let text = "[layout]\nstart = [\"clock\", \"cpu\"]\ncenter = [{ widget = \"cpu\", id = \"clock_2\" }, \"clock\"]\nend = [\"clock\", { widget = \"wifi\", id = \"cpu\" }, \"cpu\"]\n";
        assert_eq!(ids(&decode(text).layout.normalized()), [
            items(&[("clock", K::Clock), ("cpu_2", K::Cpu)]),
            items(&[("clock_2", K::Cpu), ("clock_3", K::Clock)]),
            items(&[("clock_4", K::Clock), ("cpu", K::Wifi), ("cpu_3", K::Cpu)]),
        ]);
    }

    #[test]
    fn layout_rejections_name_their_field_path() {
        for (text, field) in [
            ("[layout]\nend = [\"cpu\", \"cpu\", \"nope\"]\n", "layout.end[2]"),
            (
                "[layout]\nend = [\"cpu\", \"cpu\", { widget = \"nope\" }]\n",
                "layout.end[2].widget",
            ),
            ("[layout]\nstart = [{ id = \"x\" }]\n", "layout.start[0]"),
            (
                "[layout]\nstart = [{ widget = \"cpu\", id = \"\" }]\n",
                "layout.start[0].id",
            ),
            (
                "[layout]\nstart = [{ widget = \"cpu\", id = \"a b\" }]\n",
                "layout.start[0].id",
            ),
            (
                "[layout]\nstart = [{ widget = \"cpu\", id = \"é\" }]\n",
                "layout.start[0].id",
            ),
            (
                "[layout]\nstart = [{ widget = \"cpu\", id = 3 }]\n",
                "layout.start[0].id",
            ),
            ("[layout]\ncenter = [1]\n", "layout.center[0]"),
            ("[layout]\ncenter = [[\"cpu\"]]\n", "layout.center[0]"),
            ("[layout]\ncenter = \"cpu\"\n", "layout.center"),
            ("layout = 1\n", "layout"),
            ("[layout]\nend = [\"Clock\"]\n", "layout.end[0]"),
            ("[layout]\nend = [\"keepAwake\"]\n", "layout.end[0]"),
        ] {
            let error = inspect(text).expect_err(text);
            assert_eq!(error.kind, DiagnosticKind::Type, "{text}: {error}");
            assert_eq!(error.field.as_deref(), Some(field), "{text}: {error}");
        }
    }

    #[test]
    fn duplicate_explicit_ids_are_rejected_with_both_paths() {
        let error = inspect(
            "[layout]\nstart = [{ widget = \"cpu\", id = \"a\" }]\nend = [\"clock\", { widget = \"wifi\", id = \"a\" }]\n",
        )
        .expect_err("duplicate");
        assert_eq!(error.kind, DiagnosticKind::Validation, "{error}");
        assert!(error.message.contains("layout.end[1].id"), "{error}");
        assert!(error.message.contains("layout.start[0].id"), "{error}");
        // A generated id never collides: an explicit `cpu` pushes the plain one to `cpu_2`.
        inspect("[layout]\nstart = [\"cpu\", { widget = \"wifi\", id = \"cpu\" }]\n")
            .expect("valid");
    }

    #[test]
    fn unknown_keys_inside_an_entry_table_warn_and_do_not_reject() {
        let warnings =
            inspect("[layout]\nend = [\"cpu\", { widget = \"clock\", format = \"HH:mm\" }]\n")
                .expect("valid");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("format"), "{warnings:?}");
        let warnings = inspect("[layout]\nmiddle = []\n").expect("valid");
        assert_eq!(warnings, ["layout.middle"]);
    }

    #[test]
    fn the_layout_round_trips_and_appears_in_the_schema_and_template() {
        let catalog = catalog();
        let template = catalog.template(BarConfig::ID).expect("template");
        assert!(template.contains("\n[layout]\n"), "{template}");
        assert!(template.contains("start = [\"spaces\"]"), "{template}");
        assert!(
            template.contains("{ widget = \"clock\", id = \"main_clock\", show_seconds = false }"),
            "the commented example: {template}"
        );
        let schema = catalog.schema_json(BarConfig::ID).expect("schema").expect("render");
        for spelling in [
            "spaces",
            "media",
            "weather",
            "cpu",
            "battery",
            "keep_awake",
            "wifi",
            "clock",
        ] {
            assert!(schema.contains(&format!("\"{spelling}\"")), "{spelling}");
        }
        let text = "[layout]\nend = [\"cpu\", { widget = \"clock\", id = \"c\" }, { widget = \"wifi\" }]\n";
        let config = decode(text);
        let encoded = serde_json::to_value(&config.layout).expect("encode");
        assert_eq!(
            encoded,
            json!({ "start": [], "center": [], "end": ["cpu", { "widget": "clock", "id": "c", "show_date": true, "show_seconds": true, "hour_cycle": "h23" }, { "widget": "wifi" }] })
        );
        let again: BarConfig = catalog
            .decode_json(
                BarConfig::ID,
                Path::new("bar.toml"),
                serde_json::to_value(&config).expect("json"),
            )
            .expect("decode json")
            .into_parts()
            .0
            .downcast::<BarConfig>()
            .map(|boxed| *boxed)
            .expect("type");
        assert_eq!(again, config);
    }

    use crate::layout::{BarLayoutEntry, ClockOptions, CpuOptions, HourCycle, NoOptions};

    fn end(text: &str) -> Vec<BarLayoutItem> {
        decode(&format!("[layout]\nend = [{text}]\n")).layout.normalized().end
    }

    #[test]
    fn entry_options_default_when_omitted() {
        let items = end(
            "\"clock\", { widget = \"clock\" }, \"cpu\", { widget = \"cpu\", id = \"c\" }, { widget = \"wifi\" }",
        );
        let clock = ClockOptions {
            show_date: true,
            show_seconds: true,
            hour_cycle: HourCycle::H23,
        };
        let cpu = CpuOptions { temperature_alert: 85 };
        assert_eq!(items, [
            BarLayoutItem::Clock {
                id: "clock".into(),
                options: clock
            },
            BarLayoutItem::Clock {
                id: "clock_2".into(),
                options: clock
            },
            BarLayoutItem::Cpu { id: "cpu".into(), options: cpu },
            BarLayoutItem::Cpu { id: "c".into(), options: cpu },
            BarLayoutItem::Wifi {
                id: "wifi".into(),
                options: NoOptions {}
            },
        ]);
        let published = serde_json::to_value(&items).expect("encode");
        assert_eq!(
            published,
            json!([
                { "widget": "clock", "id": "clock", "options": { "show_date": true, "show_seconds": true, "hour_cycle": "h23" } },
                { "widget": "clock", "id": "clock_2", "options": { "show_date": true, "show_seconds": true, "hour_cycle": "h23" } },
                { "widget": "cpu", "id": "cpu", "options": { "temperature_alert": 85 } },
                { "widget": "cpu", "id": "c", "options": { "temperature_alert": 85 } },
                { "widget": "wifi", "id": "wifi", "options": {} },
            ])
        );
    }

    #[test]
    fn entry_options_decode_in_any_key_order() {
        let expected = [
            BarLayoutItem::Clock {
                id: "k".into(),
                options: ClockOptions {
                    show_date: false,
                    show_seconds: false,
                    hour_cycle: HourCycle::H12,
                },
            },
            BarLayoutItem::Cpu {
                id: "cpu".into(),
                options: CpuOptions { temperature_alert: 90 },
            },
        ];
        for text in [
            "{ widget = \"clock\", id = \"k\", show_date = false, show_seconds = false, hour_cycle = \"h12\" }, { widget = \"cpu\", temperature_alert = 90 }",
            "{ hour_cycle = \"h12\", show_seconds = false, id = \"k\", show_date = false, widget = \"clock\" }, { temperature_alert = 90, widget = \"cpu\" }",
        ] {
            assert_eq!(end(text), expected, "{text}");
        }
        // A decoder that keeps file order sees `widget` first or last; both work.
        for value in [
            r#"{ "widget": "cpu", "temperature_alert": 150 }"#,
            r#"{ "temperature_alert": 150, "widget": "cpu" }"#,
        ] {
            let entry: BarLayoutEntry = serde_json::from_str(value).expect(value);
            let layout = crate::layout::BarLayoutConfig {
                start: vec![entry],
                center: vec![],
                end: vec![],
            };
            assert_eq!(layout.normalized().start, [BarLayoutItem::Cpu {
                id: "cpu".into(),
                options: CpuOptions { temperature_alert: 150 },
            }]);
        }
        assert_eq!(end("{ widget = \"cpu\", temperature_alert = 1 }"), [
            BarLayoutItem::Cpu {
                id: "cpu".into(),
                options: CpuOptions { temperature_alert: 1 }
            }
        ]);
    }

    #[test]
    fn bad_option_values_reject_the_file_with_their_field_path() {
        for (entry, field) in [
            (
                "{ widget = \"cpu\", temperature_alert = 0 }",
                "layout.end[1].temperature_alert",
            ),
            (
                "{ widget = \"cpu\", temperature_alert = 151 }",
                "layout.end[1].temperature_alert",
            ),
            (
                "{ widget = \"cpu\", temperature_alert = 300 }",
                "layout.end[1].temperature_alert",
            ),
            (
                "{ widget = \"cpu\", temperature_alert = -1 }",
                "layout.end[1].temperature_alert",
            ),
            (
                "{ widget = \"cpu\", temperature_alert = 9.5 }",
                "layout.end[1].temperature_alert",
            ),
            (
                "{ widget = \"cpu\", temperature_alert = \"90\" }",
                "layout.end[1].temperature_alert",
            ),
            (
                "{ widget = \"clock\", show_seconds = \"no\" }",
                "layout.end[1].show_seconds",
            ),
            (
                "{ widget = \"clock\", show_date = 1 }",
                "layout.end[1].show_date",
            ),
            (
                "{ widget = \"clock\", hour_cycle = \"h24\" }",
                "layout.end[1].hour_cycle",
            ),
            (
                "{ widget = \"clock\", hour_cycle = \"H12\" }",
                "layout.end[1].hour_cycle",
            ),
            (
                "{ widget = \"clock\", hour_cycle = 12 }",
                "layout.end[1].hour_cycle",
            ),
            ("{ show_seconds = false }", "layout.end[1]"),
        ] {
            let text = format!("[layout]\nend = [\"wifi\", {entry}]\n");
            let error = inspect(&text).expect_err(&text);
            assert_eq!(error.kind, DiagnosticKind::Type, "{text}: {error}");
            assert_eq!(error.field.as_deref(), Some(field), "{text}: {error}");
        }
        // Validation also guards a configuration built in code.
        let mut config = BarConfig::default();
        config.layout.end = vec![BarLayoutEntry::Table(
            crate::layout::BarLayoutEntryTable::Cpu(crate::layout::BarEntryFields {
                id: None,
                options: CpuOptions { temperature_alert: 0 },
            }),
        )];
        let error = config.validate().expect_err("out of range");
        assert!(
            error.message().contains("layout.end[0].temperature_alert"),
            "{error}"
        );
    }

    #[test]
    fn unknown_keys_and_another_widgets_options_warn() {
        for (entry, warning) in [
            (
                "{ widget = \"clock\", temperature_alert = 90 }",
                "layout.end.0.temperature_alert",
            ),
            (
                "{ widget = \"clock\", temperature_alert = \"bad\" }",
                "layout.end.0.temperature_alert",
            ),
            (
                "{ widget = \"cpu\", show_seconds = false }",
                "layout.end.0.show_seconds",
            ),
            (
                "{ widget = \"cpu\", hour_cycle = \"nope\" }",
                "layout.end.0.hour_cycle",
            ),
            (
                "{ widget = \"wifi\", show_date = true }",
                "layout.end.0.show_date",
            ),
            ("{ widget = \"wifi\", color = \"red\" }", "layout.end.0.color"),
            (
                "{ widget = \"cpu\", temperature_alert = 70, format = 1 }",
                "layout.end.0.format",
            ),
        ] {
            let text = format!("[layout]\nend = [{entry}]\n");
            let warnings = inspect(&text).expect(&text);
            assert_eq!(warnings, [warning], "{text}");
        }
        // The foreign option is dropped; the entry keeps its own defaults.
        assert_eq!(
            end("{ widget = \"wifi\", show_date = false, temperature_alert = 3 }"),
            [BarLayoutItem::Wifi {
                id: "wifi".into(),
                options: NoOptions {}
            }]
        );
        assert_eq!(
            end("{ widget = \"clock\", temperature_alert = 3, show_date = false }"),
            [BarLayoutItem::Clock {
                id: "clock".into(),
                options: ClockOptions {
                    show_date: false,
                    ..ClockOptions::default()
                },
            }]
        );
        // A decoder that keeps file order reports a foreign option after `widget`
        // as ignored and drops it.
        let entry: BarLayoutEntry =
            serde_json::from_str(r#"{ "widget": "cpu", "show_date": "x" }"#).expect("ignored");
        assert_eq!(
            entry,
            BarLayoutEntry::Table(crate::layout::BarLayoutEntryTable::new(K::Cpu, None))
        );
    }

    #[test]
    fn the_template_example_shows_an_entry_with_an_option_and_round_trips() {
        let catalog = catalog();
        let template = catalog.template(BarConfig::ID).expect("template");
        assert!(
            template.contains("{ widget = \"cpu\", temperature_alert = 90 }"),
            "{template}"
        );
        let decoded = catalog
            .decode(BarConfig::ID, Path::new("bar.toml"), &template)
            .expect("template decodes")
            .into_parts();
        assert!(decoded.1.is_empty(), "{:?}", decoded.1);
        let options = "[layout]\nend = [{ widget = \"clock\", hour_cycle = \"h12\" }, { widget = \"cpu\", temperature_alert = 70, id = \"c\" }]\n";
        let config = decode(options);
        let again: BarConfig = catalog
            .decode_json(
                BarConfig::ID,
                Path::new("bar.toml"),
                serde_json::to_value(&config).expect("json"),
            )
            .expect("decode json")
            .into_parts()
            .0
            .downcast::<BarConfig>()
            .map(|boxed| *boxed)
            .expect("type");
        assert_eq!(again, config);
    }

    #[test]
    fn migration_never_writes_a_layout_so_the_default_applies() {
        let mut section = json!({ "enabled": true, "height": 30, "padding": 8 });
        BarConfig::migrate_legacy_section(&mut section);
        assert!(section.get("layout").is_none(), "{section}");
    }
}
