//! Application feature modules for Stache.
//!
//! This module contains all the feature modules that provide the core functionality
//! of the Stache application:
//!
//! - [`audio`] - Audio device management and automatic switching
//! - [`bar`] - Status bar UI and components
//! - [`cmd_q`] - Hold-to-quit (⌘Q) handler
//! - [`hotkey`] - Global keyboard shortcut handling
//! - [`menu_anywhere`] - Summon app menus at cursor position
//! - [`notunes`] - Prevent Apple Music from auto-launching
//! - [`tiling`] - Tiling window manager (reactive architecture)
//! - [`wallpaper`] - Dynamic wallpaper management
//! - [`widgets`] - Widget overlay windows

pub mod audio;
pub mod bar;
pub mod cmd_q;
pub mod hotkey;
pub mod menu_anywhere;
pub mod notunes;
pub mod tiling;
pub mod wallpaper;
pub mod widgets;
