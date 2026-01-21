//! Configuration template generation.
//!
//! Generates a commented configuration template with all available options.

use std::fs;
use std::path::Path;

/// Generates a configuration template with all options commented out.
///
/// This creates a JSONC file with comprehensive documentation for all
/// available configuration options.
#[must_use]
pub fn generate_config_template() -> String {
    r##"// Stache Configuration File
// =========================
// This file uses JSONC format (JSON with comments).
// All options below are commented out and show their default values.
// Uncomment and modify the options you want to configure.
//
// Documentation: https://github.com/marcosmoura/stache

{
  // ============================================================================
  // Status Bar Configuration
  // ============================================================================
  // "bar": {
  //   // Enable or disable the status bar
  //   "enabled": false,
  //
  //   // Height of the status bar in pixels
  //   "height": 28,
  //
  //   // Padding around the status bar in pixels
  //   "padding": 12,
  //
  //   // Weather widget configuration
  //   "weather": {
  //     // Path to .env file containing VISUAL_CROSSING_API_KEY
  //     "apiKeys": "",
  //
  //     // Default location when geolocation fails (city name or coordinates)
  //     "defaultLocation": ""
  //   }
  // },

  // ============================================================================
  // Command Quit (Hold ⌘Q to Quit)
  // ============================================================================
  // Prevents accidental app quits by requiring you to hold ⌘Q
  // "commandQuit": {
  //   // Enable or disable the hold-to-quit feature
  //   "enabled": true,
  //
  //   // Duration in milliseconds to hold ⌘Q before quitting (default: 1500)
  //   "holdDuration": 1500
  // },

  // ============================================================================
  // Wallpaper Management
  // ============================================================================
  // "wallpapers": {
  //   // Enable wallpaper management
  //   "enabled": false,
  //
  //   // Directory containing wallpaper images
  //   "path": "",
  //
  //   // Or specify a list of wallpaper file paths
  //   "list": [],
  //
  //   // Rotation mode: "random" or "sequential"
  //   "mode": "random",
  //
  //   // Interval in seconds between wallpaper changes (0 = no rotation)
  //   "interval": 0,
  //
  //   // Blur radius in pixels (0 = no blur)
  //   "blur": 0,
  //
  //   // Corner radius in pixels (0 = no rounding)
  //   "radius": 0
  // },

  // ============================================================================
  // Global Keyboard Shortcuts
  // ============================================================================
  // Map keyboard shortcuts to commands. Commands can be shell commands or
  // stache CLI commands.
  // "keybindings": {
  //   // Example: Reload configuration
  //   // "Command+Control+R": "stache reload",
  //
  //   // Example: Set random wallpaper
  //   // "Command+Control+W": "stache wallpaper set --random",
  //
  //   // Example: Multiple commands (executed sequentially)
  //   // "Command+Control+T": ["stache reload", "open -a Terminal"]
  // },

  // ============================================================================
  // Menu Anywhere
  // ============================================================================
  // Summon the current app's menu bar at your cursor position
  // "menuAnywhere": {
  //   // Enable Menu Anywhere
  //   "enabled": false,
  //
  //   // Keyboard modifiers to hold: "control", "option", "command", "shift"
  //   "modifiers": ["control", "command"],
  //
  //   // Mouse button trigger: "rightClick" or "middleClick"
  //   "mouseButton": "rightClick"
  // },

  // ============================================================================
  // Proxy Audio (Automatic Device Switching)
  // ============================================================================
  // "proxyAudio": {
  //   // Enable automatic audio device switching
  //   "enabled": false,
  //
  //   // Output device configuration
  //   "output": {
  //     // Virtual device name (if using a virtual audio driver)
  //     "name": "Stache Virtual Output",
  //
  //     // Audio buffer size (lower = less latency, higher = more stable)
  //     "bufferSize": 256,
  //
  //     // Device priority list (first available device is used)
  //     // AirPlay devices are always given highest priority automatically
  //     "priority": [
  //       // { "name": "External Speakers", "strategy": "exact" },
  //       // { "name": "MacBook Pro Speakers", "strategy": "contains" }
  //     ]
  //   },
  //
  //   // Input device configuration
  //   "input": {
  //     "name": "Stache Virtual Input",
  //     "priority": []
  //   }
  // },

  // ============================================================================
  // NoTunes (Prevent Apple Music Auto-Launch)
  // ============================================================================
  // "notunes": {
  //   // Enable NoTunes
  //   "enabled": false,
  //
  //   // App to launch instead: "spotify", "tidal", or "none"
  //   "targetApp": "spotify"
  // },

  // ============================================================================
  // Tiling Window Manager
  // ============================================================================
  // "tiling": {
  //   // Enable the tiling window manager
  //   "enabled": false,
  //
  //   // Default layout for workspaces: "dwindle", "split", "monocle",
  //   // "master", "grid", or "floating"
  //   "defaultLayout": "dwindle",
  //
  //   // Gap configuration
  //   "gaps": {
  //     // Gap between windows (pixels)
  //     "inner": 8,
  //
  //     // Gap from screen edges (pixels or per-side object)
  //     "outer": 8
  //     // Or specify per-side: { "top": 8, "right": 8, "bottom": 8, "left": 8 }
  //   },
  //
  //   // Master layout configuration
  //   "master": {
  //     // Master window size ratio (0-100)
  //     "ratio": 60,
  //
  //     // Master position: "left", "right", "top", "bottom", or "auto"
  //     "position": "auto"
  //   },
  //
  //   // Animation configuration
  //   "animations": {
  //     "enabled": false,
  //     "duration": 200,
  //     "easing": "ease-out"
  //   },
  //
  //   // Window borders
  //   "borders": {
  //     "enabled": false,
  //     "focused": { "width": 4, "color": "#b4befe" },
  //     "unfocused": { "width": 4, "color": "#6c7086" },
  //     "monocle": { "width": 4, "color": "#cba6f7" },
  //     "floating": { "width": 4, "color": "#94e2d5" },
  //     "ignore": []
  //   },
  //
  //   // Floating window presets
  //   "floating": {
  //     "defaultPosition": "center",
  //     "presets": [
  //       // { "name": "small", "width": 800, "height": 600, "center": true },
  //       // { "name": "large", "width": "80%", "height": "80%", "center": true }
  //     ]
  //   },
  //
  //   // Windows to ignore (never tiled)
  //   "ignore": [
  //     // { "appName": "System Preferences" },
  //     // { "appId": "com.apple.systempreferences" },
  //     // { "title": "Picture in Picture" }
  //   ],
  //
  //   // Workspace definitions
  //   "workspaces": [
  //     // {
  //     //   "name": "main",
  //     //   "layout": "dwindle",
  //     //   "screen": "main",
  //     //   "rules": [
  //     //     { "appName": "Safari" }
  //     //   ]
  //     // },
  //     // {
  //     //   "name": "code",
  //     //   "layout": "monocle",
  //     //   "rules": [
  //     //     { "appId": "com.microsoft.VSCode" }
  //     //   ]
  //     // }
  //   ]
  // }
}
"##
    .to_string()
}

/// Creates a configuration file with the template at the specified path.
///
/// Creates parent directories if they don't exist.
///
/// # Errors
///
/// Returns an error if the file cannot be written.
pub fn create_config_file(path: &Path) -> Result<(), std::io::Error> {
    // Create parent directories if they don't exist
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Write the template
    fs::write(path, generate_config_template())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_config_template_is_valid_jsonc() {
        let template = generate_config_template();
        // The template should contain the opening and closing braces
        assert!(template.contains('{'));
        assert!(template.contains('}'));
        // It should have comments
        assert!(template.contains("//"));
    }

    #[test]
    fn test_generate_config_template_contains_all_sections() {
        let template = generate_config_template();
        assert!(template.contains("bar"));
        assert!(template.contains("commandQuit"));
        assert!(template.contains("wallpapers"));
        assert!(template.contains("keybindings"));
        assert!(template.contains("menuAnywhere"));
        assert!(template.contains("proxyAudio"));
        assert!(template.contains("notunes"));
        assert!(template.contains("tiling"));
    }
}
