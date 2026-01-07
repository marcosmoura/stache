//! Shared types and utilities for Barba Shell.
//!
//! This crate provides common types used by both the desktop app and CLI.

pub mod cache;
pub mod config;
pub mod schema;

pub use cache::{APP_BUNDLE_ID, clear_cache, format_bytes, get_cache_dir, get_cache_subdir};
pub use config::{
    AudioDevicePriority, BarConfig, BarbaConfig, ConfigError, MatchStrategy, MenuAnywhereConfig,
    MenuAnywhereModifier, MenuAnywhereMouseButton, ProxyAudioConfig, ProxyAudioInputConfig,
    ProxyAudioOutputConfig, ShortcutCommands, WallpaperConfig, WallpaperMode, WeatherConfig,
    load_config,
};
pub use schema::{generate_schema, print_schema};
