//! `NoTunes` configuration types.
//!
//! Configuration for preventing Apple Music auto-launch and launching alternatives.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Target music application for noTunes replacement.
///
/// When Apple Music or iTunes is blocked, this app will be launched instead.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TargetMusicApp {
    /// Tidal music streaming service.
    #[default]
    Tidal,
    /// Spotify music streaming service.
    Spotify,
    /// Do not launch any replacement app.
    None,
}

impl TargetMusicApp {
    /// Returns the application path for the target music app.
    #[must_use]
    pub const fn app_path(&self) -> Option<&'static str> {
        match self {
            Self::Tidal => Some("/Applications/TIDAL.app"),
            Self::Spotify => Some("/Applications/Spotify.app"),
            Self::None => None,
        }
    }

    /// Returns the bundle identifier for the target music app.
    #[must_use]
    pub const fn bundle_id(&self) -> Option<&'static str> {
        match self {
            Self::Tidal => Some("com.tidal.desktop"),
            Self::Spotify => Some("com.spotify.client"),
            Self::None => None,
        }
    }

    /// Returns the display name for the target music app.
    #[must_use]
    pub const fn display_name(&self) -> &'static str {
        match self {
            Self::Tidal => "Tidal",
            Self::Spotify => "Spotify",
            Self::None => "None",
        }
    }
}

/// Configuration for the noTunes feature.
///
/// noTunes prevents Apple Music or iTunes from launching automatically
/// (e.g., when pressing media keys or connecting Bluetooth headphones)
/// and optionally launches a preferred music player instead.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct NoTunesConfig {
    /// Whether noTunes functionality is enabled.
    /// Default: false
    pub enabled: bool,

    /// The music app to launch when Apple Music/iTunes is blocked.
    /// Options: "tidal", "spotify", "none"
    /// Default: "spotify"
    pub target_app: TargetMusicApp,
}

impl Default for NoTunesConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            target_app: TargetMusicApp::Spotify,
        }
    }
}

impl NoTunesConfig {
    /// Returns whether noTunes functionality is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool { self.enabled }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_target_music_app_default_is_tidal() {
        assert_eq!(TargetMusicApp::default(), TargetMusicApp::Tidal);
    }

    #[test]
    fn test_target_music_app_app_path() {
        assert_eq!(TargetMusicApp::Tidal.app_path(), Some("/Applications/TIDAL.app"));
        assert_eq!(
            TargetMusicApp::Spotify.app_path(),
            Some("/Applications/Spotify.app")
        );
        assert_eq!(TargetMusicApp::None.app_path(), None);
    }

    #[test]
    fn test_target_music_app_bundle_id() {
        assert_eq!(TargetMusicApp::Tidal.bundle_id(), Some("com.tidal.desktop"));
        assert_eq!(TargetMusicApp::Spotify.bundle_id(), Some("com.spotify.client"));
        assert_eq!(TargetMusicApp::None.bundle_id(), None);
    }

    #[test]
    fn test_target_music_app_display_name() {
        assert_eq!(TargetMusicApp::Tidal.display_name(), "Tidal");
        assert_eq!(TargetMusicApp::Spotify.display_name(), "Spotify");
        assert_eq!(TargetMusicApp::None.display_name(), "None");
    }

    #[test]
    fn test_notunes_config_default() {
        let config = NoTunesConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.target_app, TargetMusicApp::Spotify);
    }
}
