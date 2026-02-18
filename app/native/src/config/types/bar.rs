//! Status bar configuration types.
//!
//! Configuration for the status bar UI components including weather.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Weather provider options.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum WeatherProvider {
    /// Automatically select provider based on available API keys.
    /// Uses Visual Crossing if API key is available, otherwise Open Meteo.
    #[default]
    Auto,
    /// Force use of Visual Crossing Weather API (requires API key).
    VisualCrossing,
    /// Use Open Meteo (free, no API key required).
    OpenMeteo,
}

impl WeatherProvider {
    /// Returns true if this provider requires an API key.
    #[must_use]
    pub const fn requires_api_key(self) -> bool { matches!(self, Self::VisualCrossing) }

    /// Returns the display name of the provider.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::VisualCrossing => "Visual Crossing",
            Self::OpenMeteo => "Open Meteo",
        }
    }
}

/// Weather configuration for the status bar.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct WeatherConfig {
    /// Weather data provider.
    /// Default: "auto" - Uses Visual Crossing if API key exists, otherwise Open Meteo.
    pub provider: WeatherProvider,
    /// Path to an environment file containing API keys.
    ///
    /// The file should contain key-value pairs in the format `KEY=value`.
    /// Supported keys:
    /// - `VISUAL_CROSSING_API_KEY` - API key for Visual Crossing Weather API
    ///
    /// The path can be:
    /// - Relative to the config file directory (e.g., `.env`, `secrets/.env`)
    /// - Absolute path (e.g., `/Users/username/.secrets/.env`)
    /// - Use `~` for home directory (e.g., `~/.config/stache/.env`)
    ///
    /// Example `.env` file:
    /// ```env
    /// VISUAL_CROSSING_API_KEY=your_api_key_here
    /// ```
    pub api_keys: String,

    /// Default location for weather data when geolocation fails.
    /// Can be a city name, address, or coordinates.
    pub default_location: String,
}

impl WeatherConfig {
    /// Returns whether weather functionality is enabled.
    ///
    /// Weather is considered enabled if:
    /// - Provider is Auto and API keys file is configured, OR
    /// - Provider is `VisualCrossing` and API keys file is configured, OR
    /// - Provider is `OpenMeteo` (always works without API key)
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        match self.provider {
            WeatherProvider::Auto | WeatherProvider::VisualCrossing => !self.api_keys.is_empty(),
            WeatherProvider::OpenMeteo => true,
        }
    }
}

/// Bar configuration for the status bar UI components.
///
/// Contains settings for bar-specific features like weather and dimensions.
/// The bar dimensions are used by the tiling window manager to account for
/// the status bar when calculating window layouts on the main screen.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
#[derive(Default)]
pub struct BarConfig {
    /// Whether the status bar is enabled.
    /// Default: false
    pub enabled: bool,

    /// Height of the status bar in pixels.
    /// Default: 28
    pub height: u16,

    /// Padding around the status bar in pixels.
    /// This is added to the height when calculating the top gap for tiling.
    /// Default: 12
    pub padding: u16,

    /// Weather status bar configuration.
    pub weather: WeatherConfig,
}

impl BarConfig {
    /// Returns whether the status bar is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool { self.enabled }
}
