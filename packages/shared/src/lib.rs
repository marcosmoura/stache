//! Shared types and utilities for Barba Shell.
//!
//! This crate provides common types used by both the desktop app and CLI.

pub mod cache;
pub mod config;
pub mod schema;
pub mod tiling;

pub use cache::{APP_BUNDLE_ID, clear_cache, format_bytes, get_cache_dir, get_cache_subdir};
pub use config::{
    BarConfig, BarbaConfig, ConfigError, ShortcutCommands, WallpaperConfig, WallpaperMode,
    WeatherConfig, load_config,
};
pub use schema::{generate_schema, print_schema};
pub use tiling::{
    AnimationConfig, AnimationSettings, DimensionValue, EasingFunction, FloatingConfig,
    FloatingDefaultPosition, FloatingPreset, FocusedAppInfo, GapsConfig, IgnoreRule, InnerGaps,
    LayoutMode, MasterConfig, OuterGaps, ScreenGaps, ScreenInfo, ScreenTarget, TilingConfig,
    WindowInfo, WindowRule, WorkspaceConfig, WorkspaceInfo,
};
