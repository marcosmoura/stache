//! Weather configuration component.
//!
//! Exposes the weather configuration from the config file to the frontend.
//! API keys are loaded from a separate environment file to avoid leaking
//! secrets in the configuration file.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::env::load_api_keys;
use crate::config::{WeatherConfig, get_config, get_config_path};

/// Weather configuration payload for the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeatherConfigInfo {
    /// API key for Visual Crossing Weather API.
    pub visual_crossing_api_key: String,
    /// Default location for weather data when geolocation fails.
    pub default_location: String,
}

impl WeatherConfigInfo {
    /// Creates a `WeatherConfigInfo` from a `WeatherConfig`, loading API keys
    /// from the configured environment file.
    ///
    /// # Arguments
    ///
    /// * `config` - The weather configuration from the config file
    /// * `config_dir` - The directory containing the config file (for resolving relative paths)
    #[must_use]
    pub fn from_config(config: &WeatherConfig, config_dir: &Path) -> Self {
        let api_keys = load_api_keys(&config.api_keys, config_dir);

        Self {
            visual_crossing_api_key: api_keys.visual_crossing_api_key().to_string(),
            default_location: config.default_location.clone(),
        }
    }
}

/// Get the weather configuration from the config file.
///
/// This loads the weather config and resolves API keys from the configured
/// environment file. The env file path is resolved relative to the config
/// file's directory.
#[tauri::command]
pub fn get_weather_config() -> WeatherConfigInfo {
    let config = get_config();
    let config_path = get_config_path();
    let config_dir = config_path
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));

    let weather_config = &config.bar.weather;
    WeatherConfigInfo::from_config(weather_config, &config_dir)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_weather_config_info_default() {
        let config = WeatherConfig::default();
        let config_dir = Path::new("/tmp");
        let info = WeatherConfigInfo::from_config(&config, config_dir);

        assert!(info.visual_crossing_api_key.is_empty());
        assert!(info.default_location.is_empty());
    }

    #[test]
    fn test_weather_config_info_with_env_file() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");

        let mut file = fs::File::create(&env_path).unwrap();
        writeln!(file, "VISUAL_CROSSING_API_KEY=test_key_12345").unwrap();

        let config = WeatherConfig {
            api_keys: ".env".to_string(),
            default_location: "New York".to_string(),
        };

        let info = WeatherConfigInfo::from_config(&config, temp_dir.path());

        assert_eq!(info.visual_crossing_api_key, "test_key_12345");
        assert_eq!(info.default_location, "New York");
    }

    #[test]
    fn test_weather_config_info_missing_env_file() {
        let config = WeatherConfig {
            api_keys: "nonexistent.env".to_string(),
            default_location: "London".to_string(),
        };

        let config_dir = Path::new("/nonexistent/dir");
        let info = WeatherConfigInfo::from_config(&config, config_dir);

        assert!(info.visual_crossing_api_key.is_empty());
        assert_eq!(info.default_location, "London");
    }

    #[test]
    fn test_weather_config_info_absolute_env_path() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join("secrets.env");

        let mut file = fs::File::create(&env_path).unwrap();
        writeln!(file, "VISUAL_CROSSING_API_KEY=absolute_path_key").unwrap();

        let config = WeatherConfig {
            api_keys: env_path.to_string_lossy().to_string(),
            default_location: "Paris".to_string(),
        };

        // Config dir doesn't matter for absolute paths
        let config_dir = Path::new("/some/other/dir");
        let info = WeatherConfigInfo::from_config(&config, config_dir);

        assert_eq!(info.visual_crossing_api_key, "absolute_path_key");
        assert_eq!(info.default_location, "Paris");
    }
}
