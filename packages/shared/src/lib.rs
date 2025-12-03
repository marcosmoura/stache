//! Shared types and utilities for Barba Shell.
//!
//! This crate provides common types used by both the desktop app and CLI.

pub mod config;
pub mod schema;

pub use config::{
    BarbaConfig, ConfigError, ShortcutCommands, WallpaperConfig, WallpaperMode, load_config,
};
pub use schema::{generate_schema, generate_schema_json};
