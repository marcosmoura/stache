//! Configuration types for Stache.
//!
//! This module provides all configuration types organized by domain.
//! The configuration file supports JSONC format (JSON with comments).
//! Both single-line (`//`) and multi-line (`/* */`) comments are allowed.

// Domain-specific configuration modules
pub mod audio;
pub mod bar;
pub mod borders;
pub mod color;
pub mod command_quit;
pub mod gaps;
pub mod menu_anywhere;
pub mod notunes;
pub mod root;
pub mod tiling;
pub mod wallpaper;
pub mod workspaces;

// Re-export all types for backward compatibility and convenience

// Audio types
pub use audio::{AudioDeviceDependency, AudioDevicePriority, MatchStrategy, ProxyAudioConfig};
// Bar types
pub use bar::{BarConfig, WeatherConfig, WeatherProvider};
// Border types
pub use borders::{BorderColor, BorderStateConfig, BordersConfig, GradientConfig};
// Color types
pub use color::{parse_color, parse_hex_color, parse_rgba_color, Rgba};
// Command Quit types
pub use command_quit::CommandQuitConfig;
// Gap types
pub use gaps::{DimensionValue, GapValue, GapsConfig, GapsConfigValue};
// Menu Anywhere types
pub use menu_anywhere::{MenuAnywhereConfig, MenuAnywhereModifier, MenuAnywhereMouseButton};
// NoTunes types
pub use notunes::{NoTunesConfig, TargetMusicApp};
// Root config types
pub use root::{
    config_paths, load_config, load_config_from_path, ConfigError, ShortcutCommands, StacheConfig,
};
// Tiling types
pub use tiling::{
    AnimationConfig, EasingType, FloatingConfig, FloatingPreset, LayoutType, MasterConfig,
    MasterPosition, TilingConfig,
};
// Wallpaper types
pub use wallpaper::{WallpaperConfig, WallpaperMode};
// Workspace types
pub use workspaces::{WindowRule, WorkspaceConfig};
