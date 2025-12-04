//! Wallpaper management module for Barba Shell.
//!
//! This module provides dynamic wallpaper functionality including:
//! - Loading wallpapers from a directory or a predefined list
//! - Processing images with rounded corners and Gaussian blur
//! - Caching processed images to avoid redundant processing
//! - Automatic wallpaper cycling based on interval settings
//! - Manual wallpaper control via CLI commands
//! - Multi-screen support with per-screen wallpapers

mod macos;
mod manager;
mod processing;

pub use manager::{
    WallpaperAction, WallpaperManagerError, generate_all_streaming, init, perform_action, start,
};
