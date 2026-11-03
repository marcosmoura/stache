//! JSON Schema generation for Stache configuration.
//!
//! This module generates a JSON Schema that describes the configuration file
//! format, which can be used for editor validation and autocompletion.

use crate::config::StacheConfig;

/// Generates a JSON Schema for the Stache configuration.
///
/// The schema includes all configuration options with their types,
/// descriptions, and default values.
#[must_use]
pub fn generate_schema() -> schemars::Schema {
    let mut schema = schemars::schema_for!(StacheConfig);

    // Add $id for proper schema identification
    if let Some(obj) = schema.as_object_mut() {
        obj.insert(
            "$id".to_string(),
            serde_json::json!(
                "https://raw.githubusercontent.com/marcosmoura/stache/main/_legacy/stache.schema.json"
            ),
        );
    }

    schema
}

/// Generates a JSON Schema string for the Stache configuration.
///
/// Returns a pretty-printed JSON string that can be saved to a file
/// or used for validation.
#[must_use]
pub fn print_schema() -> String {
    let schema = generate_schema();
    serde_json::to_string_pretty(&schema).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_schema_produces_valid_json() {
        let schema_json = print_schema();
        assert!(!schema_json.is_empty());

        let parsed: serde_json::Value = serde_json::from_str(&schema_json).unwrap();

        assert!(parsed["$id"].as_str().unwrap().contains("stache.schema.json"));
        assert_eq!(parsed["$schema"], "https://json-schema.org/draft/2020-12/schema");
        assert_eq!(parsed["title"], "StacheConfig");
        assert!(parsed["properties"]["keybindings"].is_object());
        assert!(parsed["properties"]["bar"].is_object());
    }

    #[test]
    fn test_generate_schema_returns_schema_object() {
        let schema = generate_schema();
        // Schema should be a valid schema object
        assert!(schema.as_object().is_some());
    }

    #[test]
    fn test_schema_has_id_field() {
        let schema = generate_schema();
        let obj = schema.as_object().unwrap();
        assert!(obj.contains_key("$id"));
    }

    #[test]
    fn test_schema_id_is_correct_url() {
        let schema_json = print_schema();
        let parsed: serde_json::Value = serde_json::from_str(&schema_json).unwrap();
        let id = parsed["$id"].as_str().unwrap();

        assert!(id.starts_with("https://"));
        assert!(id.contains("githubusercontent.com"));
        assert!(id.ends_with("stache.schema.json"));
        assert!(id.contains("/main/_legacy/"));
    }

    #[test]
    fn test_schema_contains_wallpapers_config() {
        let schema_json = print_schema();
        let parsed: serde_json::Value = serde_json::from_str(&schema_json).unwrap();

        // WallpaperConfig is at the root level of StacheConfig, referenced in $defs
        let wallpaper_config = &parsed["$defs"]["WallpaperConfig"];
        assert!(wallpaper_config.is_object());
    }

    #[test]
    fn test_schema_contains_weather_config() {
        let schema_json = print_schema();
        let parsed: serde_json::Value = serde_json::from_str(&schema_json).unwrap();

        // Weather is now under BarConfig, referenced in $defs
        let weather_config = &parsed["$defs"]["WeatherConfig"];
        assert!(weather_config.is_object());
    }

    #[test]
    fn test_schema_json_is_pretty_printed() {
        let schema_json = print_schema();
        // Pretty-printed JSON should have newlines
        assert!(schema_json.contains('\n'));
    }
}
