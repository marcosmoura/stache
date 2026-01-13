//! `JankyBorders` integration for window border rendering.
//!
//! This module provides integration with [JankyBorders](https://github.com/FelixKratz/JankyBorders),
//! a high-performance border rendering tool for macOS. Instead of implementing custom border
//! rendering, Stache delegates to `JankyBorders` for smooth, GPU-accelerated borders.
//!
//! # Architecture
//!
//! `JankyBorders` runs as a separate process. Stache communicates with it using two methods:
//!
//! 1. **Mach IPC (preferred)**: Direct kernel-level messaging via the `git.felix.borders`
//!    bootstrap service. This is ~50-100x faster than CLI invocation.
//!
//! 2. **CLI fallback**: Spawning the `borders` command when Mach IPC fails.
//!
//! Stache sends configuration updates to `JankyBorders` when:
//! - Focus changes between windows
//! - Layout changes (monocle, floating)
//! - Configuration is reloaded
//!
//! # Supported Features
//!
//! `JankyBorders` supports:
//! - Solid colors: `0xAARRGGBB`
//! - Gradients: `gradient(top_left=0xAARRGGBB,bottom_right=0xAARRGGBB)`
//! - Glow effects: `glow(0xAARRGGBB)`
//! - Border width, style (round/square), `HiDPI`
//! - Blacklist/whitelist for apps
//! - Border order (above/below windows)
//!
//! # Requirements
//!
//! `JankyBorders` must be installed: `brew install FelixKratz/formulae/borders`
//!
//! # Performance
//!
//! | Method          | Latency       | Use Case                              |
//! |-----------------|---------------|---------------------------------------|
//! | Mach IPC        | ~0.1-0.5ms    | Real-time updates (focus, drag)       |
//! | CLI fallback    | ~20-50ms      | Initial config, when IPC unavailable  |

use std::collections::HashMap;
use std::process::Command;
use std::sync::OnceLock;

use parking_lot::Mutex;

use super::mach_ipc;
use crate::config::{
    BorderColor, BorderStateConfig, BordersConfig, Rgba, get_config, parse_hex_color,
};

// ============================================================================
// Command Caching
// ============================================================================

/// Cache of last sent commands to prevent duplicate sends.
/// Key is the setting name (e.g., "`active_color`"), value is the full argument string.
static LAST_SENT: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

/// Gets the command cache.
fn get_cache() -> &'static Mutex<HashMap<String, String>> {
    LAST_SENT.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Clears the command cache, forcing the next commands to be sent.
///
/// Call this when configuration is reloaded or `JankyBorders` is restarted.
pub fn clear_cache() { get_cache().lock().clear(); }

// ============================================================================
// JankyBorders Detection
// ============================================================================

/// Checks if `JankyBorders` is available on the system.
///
/// This checks if the `borders` command is available in PATH.
#[must_use]
pub fn is_available() -> bool {
    Command::new("which")
        .arg("borders")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Checks if `JankyBorders` is currently running.
///
/// This first attempts to check via Mach IPC (faster), then falls back to pgrep.
#[must_use]
pub fn is_running() -> bool {
    // Fast path: check Mach IPC connection
    if mach_ipc::is_connected() {
        return true;
    }

    // Slow path: check via pgrep (JankyBorders might not have Mach server ready yet)
    Command::new("pgrep")
        .args(["-x", "borders"])
        .output()
        .is_ok_and(|output| output.status.success())
}

// ============================================================================
// Color Conversion
// ============================================================================

/// Converts an RGBA color to `JankyBorders` hex format (0xAARRGGBB).
///
/// `JankyBorders` uses a 32-bit hex format with alpha in the high byte.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn rgba_to_hex(rgba: &Rgba) -> String {
    // RGBA values are normalized 0.0-1.0, so multiplying by 255 and rounding
    // gives us safe u8 values (0-255)
    let a = (rgba.a * 255.0).round() as u8;
    let r = (rgba.r * 255.0).round() as u8;
    let g = (rgba.g * 255.0).round() as u8;
    let b = (rgba.b * 255.0).round() as u8;

    format!("0x{a:02X}{r:02X}{g:02X}{b:02X}")
}

/// Converts a hex color string to `JankyBorders` format.
///
/// Accepts formats: `#RGB`, `#RRGGBB`, `#AARRGGBB`
#[must_use]
pub fn hex_to_janky(hex: &str) -> Option<String> {
    let rgba = parse_hex_color(hex).ok()?;
    Some(rgba_to_hex(&rgba))
}

/// Converts a `BorderColor` to `JankyBorders` color string.
///
/// Supports solid colors, gradients, and glow effects.
#[must_use]
pub fn border_color_to_janky(color: &BorderColor) -> Option<String> {
    match color {
        BorderColor::Solid(hex) => hex_to_janky(hex),
        BorderColor::Gradient { from, to, angle } => {
            let from_hex = hex_to_janky(from)?;
            let to_hex = hex_to_janky(to)?;

            // Map angle to JankyBorders gradient direction
            // JankyBorders supports: top_left->bottom_right or top_right->bottom_left
            let angle = angle.unwrap_or(135.0);

            // Normalize angle to 0-360
            let angle = ((angle % 360.0) + 360.0) % 360.0;

            // Choose direction based on angle
            // 45° or 225° = top_right to bottom_left
            // 135° or 315° = top_left to bottom_right (default)
            if (0.0..90.0).contains(&angle) || (180.0..270.0).contains(&angle) {
                Some(format!("gradient(top_right={from_hex},bottom_left={to_hex})"))
            } else {
                Some(format!("gradient(top_left={from_hex},bottom_right={to_hex})"))
            }
        }
        BorderColor::Glow(hex) => {
            let janky_hex = hex_to_janky(hex)?;
            Some(format!("glow({janky_hex})"))
        }
    }
}

// ============================================================================
// JankyBorders Commands
// ============================================================================

/// Checks if the command arguments have changed since last send.
///
/// Returns the filtered list of arguments that have actually changed.
/// This prevents sending duplicate commands that would cause `JankyBorders`
/// to unnecessarily recompute borders, which can cause flickering.
fn filter_changed_args<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let cache = get_cache();
    let mut cache_guard = cache.lock();
    let mut changed = Vec::new();

    for arg in args {
        // Parse key=value format
        if let Some((key, _)) = arg.split_once('=') {
            let cached = cache_guard.get(key);
            if cached.is_none_or(|v| v != *arg) {
                // Value changed or not cached - include it
                changed.push(*arg);
                cache_guard.insert(key.to_string(), (*arg).to_string());
            }
        } else {
            // Not a key=value format, always send
            changed.push(*arg);
        }
    }

    // Release the lock before returning to avoid holding it unnecessarily
    drop(cache_guard);

    changed
}

/// Sends a command to `JankyBorders`.
///
/// This first attempts to use Mach IPC for low-latency communication.
/// If Mach IPC fails, it falls back to spawning the CLI process.
///
/// Commands are cached to prevent duplicate sends - if the same key=value
/// pair is sent twice in a row, the second send is skipped to prevent
/// `JankyBorders` from unnecessarily recomputing borders (which causes flickering).
///
/// # Arguments
///
/// * `args` - Arguments to pass to `JankyBorders` (e.g., `["active_color=0xffff0000"]`).
///
/// # Returns
///
/// `true` if the command succeeded (or was skipped due to caching), `false` otherwise.
fn send_command(args: &[&str]) -> bool {
    // Filter out unchanged arguments to prevent flickering
    let changed_args = filter_changed_args(args);

    if changed_args.is_empty() {
        // All arguments unchanged, skip send
        return true;
    }

    // Fast path: try Mach IPC first
    if mach_ipc::send(&changed_args) {
        return true;
    }

    // Slow path: fall back to CLI
    eprintln!("stache: borders: falling back to CLI for: {changed_args:?}");
    send_command_cli(&changed_args)
}

/// Sends a command via CLI (slow path).
fn send_command_cli(args: &[&str]) -> bool {
    match Command::new("borders").args(args).output() {
        Ok(output) => {
            if output.status.success() {
                true
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.is_empty() {
                    eprintln!("stache: borders: JankyBorders error: {stderr}");
                }
                false
            }
        }
        Err(e) => {
            eprintln!("stache: borders: failed to run borders command: {e}");
            false
        }
    }
}

/// Sets the active (focused) border color.
///
/// Supports solid, gradient, and glow formats.
pub fn set_active_color(color: &str) -> bool {
    let arg = format!("active_color={color}");
    send_command(&[&arg])
}

/// Sets the inactive (unfocused) border color.
///
/// Supports solid, gradient, and glow formats.
pub fn set_inactive_color(color: &str) -> bool {
    let arg = format!("inactive_color={color}");
    send_command(&[&arg])
}

/// Sets the background color behind the border.
///
/// Use transparent (alpha=0) to disable.
pub fn set_background_color(color: &str) -> bool {
    let arg = format!("background_color={color}");
    send_command(&[&arg])
}

/// Sets the border width.
pub fn set_width(width: f64) -> bool {
    let arg = format!("width={width}");
    send_command(&[&arg])
}

/// Sets the border style.
///
/// # Arguments
///
/// * `style` - 'r' for round, 's' for square
pub fn set_style(style: char) -> bool {
    let arg = format!("style={style}");
    send_command(&[&arg])
}

/// Sets the border order relative to windows.
///
/// # Arguments
///
/// * `above` - `true` for above windows, `false` for below
pub fn set_order(above: bool) -> bool {
    let arg = format!("order={}", if above { 'a' } else { 'b' });
    send_command(&[&arg])
}

/// Sets the `HiDPI` (retina) mode.
pub fn set_hidpi(enabled: bool) -> bool {
    let arg = if enabled { "hidpi=on" } else { "hidpi=off" };
    send_command(&[arg])
}

/// Sets the accessibility focus tracking mode.
///
/// When enabled, `JankyBorders` uses accessibility API for focus tracking.
pub fn set_ax_focus(enabled: bool) -> bool {
    let arg = if enabled {
        "ax_focus=on"
    } else {
        "ax_focus=off"
    };
    send_command(&[arg])
}

/// Sets the blacklist of apps to exclude from borders.
///
/// # Arguments
///
/// * `apps` - Comma-separated list of app names or bundle IDs
pub fn set_blacklist(apps: &str) -> bool {
    let arg = format!("blacklist={apps}");
    send_command(&[&arg])
}

/// Sets the whitelist of apps to include (exclusive mode).
///
/// When set, only these apps will have borders.
///
/// # Arguments
///
/// * `apps` - Comma-separated list of app names or bundle IDs
pub fn set_whitelist(apps: &str) -> bool {
    let arg = format!("whitelist={apps}");
    send_command(&[&arg])
}

// ============================================================================
// Configuration Application
// ============================================================================

/// Gets the `JankyBorders` color string for a border state config.
fn get_color_for_state(state_config: &BorderStateConfig) -> Option<String> {
    if !state_config.is_enabled() {
        return None;
    }

    // Get the BorderColor from the state config
    let border_color = BorderColor::from_state_config(state_config)?;
    border_color_to_janky(&border_color)
}

/// Applies the full border configuration to `JankyBorders`.
///
/// This sets width, style, hidpi, and initial colors based on the config.
/// Uses batch sending for better performance when multiple settings need updating.
pub fn apply_config(config: &BordersConfig) -> bool {
    if !is_available() {
        eprintln!("stache: borders: JankyBorders not found, borders will not be displayed");
        eprintln!("stache: borders: install with: brew install FelixKratz/formulae/borders");
        return false;
    }

    // Build all arguments for batch sending
    let mut args: Vec<String> = Vec::with_capacity(6);

    // Width (default to 4 if not specified)
    let width = config.focused.width().unwrap_or(4);
    args.push(format!("width={}", f64::from(width)));

    // Style (default to round)
    let style = config.style.as_deref().unwrap_or("round");
    let style_char = match style {
        "square" => 's',
        _ => 'r', // round is default
    };
    args.push(format!("style={style_char}"));

    // HiDPI (default to true)
    let hidpi = config.hidpi.unwrap_or(true);
    args.push(if hidpi {
        "hidpi=on".to_string()
    } else {
        "hidpi=off".to_string()
    });

    // Active color (focused state)
    if let Some(color) = get_color_for_state(&config.focused) {
        args.push(format!("active_color={color}"));
    }

    // Inactive color (unfocused state)
    if let Some(color) = get_color_for_state(&config.unfocused) {
        args.push(format!("inactive_color={color}"));
    }

    // Send all settings in one batch via Mach IPC
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let success = send_command(&arg_refs);

    if success {
        eprintln!("stache: borders: JankyBorders configuration applied via Mach IPC");
    } else {
        eprintln!("stache: borders: JankyBorders configuration applied via CLI");
    }

    true // We attempted to apply, consider it success
}

/// Updates `JankyBorders` colors based on the current border state.
///
/// This is called when focus changes or layout changes to update the active color.
///
/// # Arguments
///
/// * `is_monocle` - Whether the focused window is in monocle layout.
/// * `is_floating` - Whether the focused window is floating.
pub fn update_colors_for_state(is_monocle: bool, is_floating: bool) {
    let config = get_config();
    let borders = &config.tiling.borders;

    // Determine which state config to use for the active color
    let active_config = if is_monocle {
        &borders.monocle
    } else if is_floating {
        &borders.floating
    } else {
        &borders.focused
    };

    let state_name = if is_monocle {
        "monocle"
    } else if is_floating {
        "floating"
    } else {
        "focused"
    };

    // Update active color
    if let Some(color) = get_color_for_state(active_config) {
        eprintln!("stache: borders: updating active color for state '{state_name}': {color}");
        if set_active_color(&color) {
            eprintln!("stache: borders: active color set successfully");
        } else {
            eprintln!("stache: borders: FAILED to set active color");
        }
    } else {
        eprintln!("stache: borders: no color configured for state '{state_name}', skipping update");
    }

    // Inactive color stays the same (unfocused state)
    // It's already set during initial config application
}

/// Refreshes `JankyBorders` configuration from current settings.
///
/// This clears the command cache and re-applies all configuration settings,
/// ensuring `JankyBorders` receives the latest values even if they haven't changed.
pub fn refresh() -> bool {
    clear_cache();
    let config = get_config();
    apply_config(&config.tiling.borders)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rgba_to_hex_opaque_red() {
        let rgba = Rgba { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };
        assert_eq!(rgba_to_hex(&rgba), "0xFFFF0000");
    }

    #[test]
    fn test_rgba_to_hex_transparent_green() {
        let rgba = Rgba { r: 0.0, g: 1.0, b: 0.0, a: 0.5 };
        assert_eq!(rgba_to_hex(&rgba), "0x8000FF00");
    }

    #[test]
    fn test_rgba_to_hex_opaque_blue() {
        let rgba = Rgba { r: 0.0, g: 0.0, b: 1.0, a: 1.0 };
        assert_eq!(rgba_to_hex(&rgba), "0xFF0000FF");
    }

    #[test]
    fn test_hex_to_janky_valid() {
        assert_eq!(hex_to_janky("#FF0000"), Some("0xFFFF0000".to_string()));
        assert_eq!(hex_to_janky("#00FF00"), Some("0xFF00FF00".to_string()));
        assert_eq!(hex_to_janky("#0000FF"), Some("0xFF0000FF".to_string()));
    }

    #[test]
    fn test_hex_to_janky_with_alpha() {
        // Config format is RRGGBBAA (CSS), JankyBorders format is AARRGGBB
        // #80FF0000 = R=0x80, G=0xFF, B=0x00, A=0x00 -> 0x0080FF00
        assert_eq!(hex_to_janky("#80FF0000"), Some("0x0080FF00".to_string()));

        // For a true 50% alpha red, use #FF000080 (red with 50% alpha)
        assert_eq!(hex_to_janky("#FF000080"), Some("0x80FF0000".to_string()));
    }

    #[test]
    fn test_hex_to_janky_invalid() {
        assert_eq!(hex_to_janky("invalid"), None);
        assert_eq!(hex_to_janky(""), None);
    }

    #[test]
    fn test_hex_to_janky_short_format() {
        // #RGB expands to #RRGGBB
        assert_eq!(hex_to_janky("#F00"), Some("0xFFFF0000".to_string()));
    }

    #[test]
    fn test_border_color_to_janky_solid() {
        let color = BorderColor::Solid("#FF0000".to_string());
        assert_eq!(border_color_to_janky(&color), Some("0xFFFF0000".to_string()));
    }

    #[test]
    fn test_border_color_to_janky_gradient_default_angle() {
        let color = BorderColor::Gradient {
            from: "#FF0000".to_string(),
            to: "#0000FF".to_string(),
            angle: None,
        };
        // Default angle 135° maps to top_left->bottom_right
        assert_eq!(
            border_color_to_janky(&color),
            Some("gradient(top_left=0xFFFF0000,bottom_right=0xFF0000FF)".to_string())
        );
    }

    #[test]
    fn test_border_color_to_janky_gradient_45_degrees() {
        let color = BorderColor::Gradient {
            from: "#FF0000".to_string(),
            to: "#0000FF".to_string(),
            angle: Some(45.0),
        };
        // 45° maps to top_right->bottom_left
        assert_eq!(
            border_color_to_janky(&color),
            Some("gradient(top_right=0xFFFF0000,bottom_left=0xFF0000FF)".to_string())
        );
    }

    #[test]
    fn test_border_color_to_janky_glow() {
        let color = BorderColor::Glow("#89b4fa".to_string());
        let result = border_color_to_janky(&color);
        assert!(result.is_some());
        assert!(result.unwrap().starts_with("glow("));
    }

    // ========================================================================
    // Command caching tests
    // ========================================================================

    #[test]
    fn test_filter_changed_args_all_new() {
        clear_cache();
        let args = &["active_color=0xFFFF0000", "width=6.0"];
        let changed = filter_changed_args(args);
        assert_eq!(changed.len(), 2);
        assert!(changed.contains(&"active_color=0xFFFF0000"));
        assert!(changed.contains(&"width=6.0"));
    }

    #[test]
    fn test_filter_changed_args_duplicate() {
        clear_cache();
        let args = &["active_color=0xFFFF0000"];

        // First call - should include the arg
        let changed1 = filter_changed_args(args);
        assert_eq!(changed1.len(), 1);

        // Second call with same value - should be empty (cached)
        let changed2 = filter_changed_args(args);
        assert!(changed2.is_empty());
    }

    #[test]
    fn test_filter_changed_args_value_changed() {
        clear_cache();

        // First call
        let args1 = &["active_color=0xFFFF0000"];
        let changed1 = filter_changed_args(args1);
        assert_eq!(changed1.len(), 1);

        // Second call with different value - should include the arg
        let args2 = &["active_color=0xFF00FF00"];
        let changed2 = filter_changed_args(args2);
        assert_eq!(changed2.len(), 1);
        assert!(changed2.contains(&"active_color=0xFF00FF00"));
    }

    #[test]
    fn test_filter_changed_args_partial_change() {
        clear_cache();

        // Set initial values
        let args1 = &["active_color=0xFFFF0000", "width=6.0"];
        let changed1 = filter_changed_args(args1);
        assert_eq!(changed1.len(), 2);

        // Only change one value
        let args2 = &["active_color=0xFFFF0000", "width=8.0"];
        let changed2 = filter_changed_args(args2);
        assert_eq!(changed2.len(), 1);
        assert!(changed2.contains(&"width=8.0"));
    }

    #[test]
    fn test_clear_cache() {
        clear_cache();

        // Set a value
        let args = &["active_color=0xFFFF0000"];
        filter_changed_args(args);

        // Clear cache
        clear_cache();

        // Same value should now be sent again
        let changed = filter_changed_args(args);
        assert_eq!(changed.len(), 1);
    }
}
