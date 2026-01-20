//! Simple border management via JankyBorders.
//!
//! This module handles window borders by communicating with JankyBorders.
//! It's intentionally simple:
//!
//! 1. On init: Configure JankyBorders with style settings and blacklist
//! 2. On focus change: Send a single batched command with all border colors
//!
//! # Architecture
//!
//! - Uses Mach IPC for fast communication (falls back to CLI)
//! - Caches the last command to avoid duplicate sends
//! - Batches all settings into a single call

use std::ffi::CString;
use std::process::Command;
use std::sync::OnceLock;

use parking_lot::Mutex;

use crate::config::{BorderColor, BorderStateConfig, Rgba, get_config, parse_hex_color};
use crate::modules::tiling::rules::{SKIP_TILING_APP_NAMES, SKIP_TILING_BUNDLE_IDS};
use crate::modules::tiling::state::LayoutType;

// ============================================================================
// Constants
// ============================================================================

/// Mach service name for JankyBorders.
const JANKY_BORDERS_SERVICE: &str = "git.felix.borders";

// ============================================================================
// State
// ============================================================================

/// Last command sent to JankyBorders (for deduplication).
static LAST_COMMAND: OnceLock<Mutex<String>> = OnceLock::new();

/// Mach port for IPC communication.
static MACH_PORT: OnceLock<Mutex<Option<u32>>> = OnceLock::new();

fn get_last_command() -> &'static Mutex<String> {
    LAST_COMMAND.get_or_init(|| Mutex::new(String::new()))
}

fn get_mach_port() -> &'static Mutex<Option<u32>> { MACH_PORT.get_or_init(|| Mutex::new(None)) }

// ============================================================================
// Mach IPC
// ============================================================================

#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    fn bootstrap_look_up(bp: u32, service_name: *const i8, sp: *mut u32) -> i32;
    fn mach_msg(
        msg: *mut MachMessage,
        option: i32,
        send_size: u32,
        rcv_size: u32,
        rcv_name: u32,
        timeout: u32,
        notify: u32,
    ) -> i32;
}

const BOOTSTRAP_PORT: u32 = 0;
const MACH_SEND_MSG: i32 = 1;
const MACH_MSG_TIMEOUT_NONE: u32 = 0;
const MACH_PORT_NULL: u32 = 0;

#[repr(C)]
struct MachMessage {
    header: MachMsgHeader,
    body: MachMsgBody,
    descriptor: MachMsgOolDescriptor,
}

#[repr(C)]
struct MachMsgHeader {
    bits: u32,
    size: u32,
    remote_port: u32,
    local_port: u32,
    voucher_port: u32,
    id: i32,
}

#[repr(C)]
struct MachMsgBody {
    descriptor_count: u32,
}

#[repr(C)]
struct MachMsgOolDescriptor {
    address: *const u8,
    deallocate: u8,
    copy: u8,
    pad1: u8,
    type_: u8,
    size: u32,
}

/// Connects to JankyBorders via Mach IPC.
fn connect_mach() -> bool {
    let service = match CString::new(JANKY_BORDERS_SERVICE) {
        Ok(s) => s,
        Err(_) => return false,
    };

    let mut port: u32 = 0;
    let result = unsafe { bootstrap_look_up(BOOTSTRAP_PORT, service.as_ptr(), &mut port) };

    if result == 0 && port != 0 {
        *get_mach_port().lock() = Some(port);
        true
    } else {
        false
    }
}

/// Sends a command via Mach IPC.
fn send_mach(command: &str) -> bool {
    let port = match *get_mach_port().lock() {
        Some(p) => p,
        None => return false,
    };

    let data = command.as_bytes();

    const MACH_MSGH_BITS_COMPLEX: u32 = 0x8000_0000;
    const MACH_MSGH_BITS_COPY_SEND: u32 = 19;
    const MACH_MSG_OOL_DESCRIPTOR: u8 = 1;
    const MACH_MSG_VIRTUAL_COPY: u8 = 1;

    let mut msg = MachMessage {
        header: MachMsgHeader {
            bits: MACH_MSGH_BITS_COMPLEX | MACH_MSGH_BITS_COPY_SEND,
            size: std::mem::size_of::<MachMessage>() as u32,
            remote_port: port,
            local_port: 0,
            voucher_port: 0,
            id: 0,
        },
        body: MachMsgBody { descriptor_count: 1 },
        descriptor: MachMsgOolDescriptor {
            address: data.as_ptr(),
            deallocate: 0,
            copy: MACH_MSG_VIRTUAL_COPY,
            pad1: 0,
            type_: MACH_MSG_OOL_DESCRIPTOR,
            size: data.len() as u32,
        },
    };

    let result = unsafe {
        mach_msg(
            &mut msg,
            MACH_SEND_MSG,
            msg.header.size,
            0,
            MACH_PORT_NULL,
            MACH_MSG_TIMEOUT_NONE,
            MACH_PORT_NULL,
        )
    };

    result == 0
}

// ============================================================================
// Color Conversion
// ============================================================================

/// Converts RGBA to JankyBorders hex format (0xAARRGGBB).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn rgba_to_hex(rgba: &Rgba) -> String {
    let a = (rgba.a * 255.0).round() as u8;
    let r = (rgba.r * 255.0).round() as u8;
    let g = (rgba.g * 255.0).round() as u8;
    let b = (rgba.b * 255.0).round() as u8;
    format!("0x{a:02X}{r:02X}{g:02X}{b:02X}")
}

/// Converts a hex color string to JankyBorders format.
fn hex_to_janky(hex: &str) -> Option<String> {
    let rgba = parse_hex_color(hex).ok()?;
    Some(rgba_to_hex(&rgba))
}

/// Converts a BorderColor to JankyBorders color string.
fn border_color_to_janky(color: &BorderColor) -> Option<String> {
    match color {
        BorderColor::Solid(hex) => hex_to_janky(hex),
        BorderColor::Gradient { from, to, angle } => {
            let from_hex = hex_to_janky(from)?;
            let to_hex = hex_to_janky(to)?;
            let angle = angle.unwrap_or(135.0);
            let angle = ((angle % 360.0) + 360.0) % 360.0;

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

/// Gets the JankyBorders color string for a border state config.
/// Returns transparent (0x00000000) and width 0 if disabled.
fn get_border_settings(config: &BorderStateConfig) -> (String, u32) {
    if !config.is_enabled() {
        return ("0x00000000".to_string(), 0);
    }

    let color = BorderColor::from_state_config(config)
        .and_then(|c| border_color_to_janky(&c))
        .unwrap_or_else(|| "0x00000000".to_string());

    let width = config.width().unwrap_or(0);

    (color, width)
}

// ============================================================================
// JankyBorders Communication
// ============================================================================

/// Checks if JankyBorders is available.
fn is_available() -> bool {
    Command::new("which")
        .arg("borders")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Sends a command to JankyBorders (with deduplication).
fn send_command(command: &str) -> bool {
    // Check if command is the same as last time
    {
        let mut last = get_last_command().lock();
        if *last == command {
            return true; // Already sent this exact command
        }
        *last = command.to_string();
    }

    // Try Mach IPC first
    if send_mach(command) {
        return true;
    }

    // Fall back to CLI
    let args: Vec<&str> = command.split_whitespace().collect();
    Command::new("borders")
        .args(&args)
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Builds the blacklist string for JankyBorders.
fn build_blacklist() -> String {
    let config = get_config();
    let mut apps: Vec<String> = Vec::new();

    // Add built-in skip lists from rules module
    apps.extend(SKIP_TILING_BUNDLE_IDS.iter().map(|s| (*s).to_string()));
    apps.extend(SKIP_TILING_APP_NAMES.iter().map(|s| (*s).to_string()));

    // Add user-configured ignore rules (app names and bundle IDs)
    for rule in &config.tiling.ignore {
        if let Some(app_id) = &rule.app_id {
            apps.push(app_id.clone());
        }
        if let Some(app_name) = &rule.app_name {
            apps.push(app_name.clone());
        }
    }

    // Add border-specific ignore rules
    for rule in &config.tiling.borders.ignore {
        if let Some(app_id) = &rule.app_id {
            apps.push(app_id.clone());
        }
        if let Some(app_name) = &rule.app_name {
            apps.push(app_name.clone());
        }
    }

    apps.join(",")
}

// ============================================================================
// Public API
// ============================================================================

/// Initializes the border system.
///
/// Sets up JankyBorders with:
/// - Style settings (width, style, hidpi)
/// - Blacklist of ignored apps
/// - Initial colors (unfocused always, active based on initial layout)
pub fn init() -> bool {
    let config = get_config();

    if !config.tiling.borders.is_enabled() {
        log::debug!("tiling: borders disabled in config");
        return true;
    }

    if !is_available() {
        log::warn!("tiling: JankyBorders not found");
        return false;
    }

    // Connect via Mach IPC
    if connect_mach() {
        log::debug!("tiling: connected to JankyBorders via Mach IPC");
    }

    // Build initial command with all settings
    let borders = &config.tiling.borders;
    let blacklist = build_blacklist();

    // Get style settings
    let width = borders.focused.width().unwrap_or(4);
    let style = borders.style.as_deref().unwrap_or("round");
    let style_char = if style == "square" { 's' } else { 'r' };
    let hidpi = if borders.hidpi.unwrap_or(true) {
        "on"
    } else {
        "off"
    };

    // Get unfocused color (always needed)
    let (inactive_color, _) = get_border_settings(&borders.unfocused);

    // Get initial active color (default to focused)
    let (active_color, _) = get_border_settings(&borders.focused);

    // Build and send the initial command
    let command = format!(
        "width={width} style={style_char} hidpi={hidpi} \
         active_color={active_color} inactive_color={inactive_color} \
         blacklist={blacklist}"
    );

    // Clear the cache so first command always sends
    *get_last_command().lock() = String::new();

    if send_command(&command) {
        log::debug!("tiling: borders initialized");
        true
    } else {
        log::warn!("tiling: failed to initialize borders");
        false
    }
}

/// Updates borders based on workspace layout.
///
/// Called when focus changes. Determines the correct active color based on:
/// - Monocle layout → monocle config (if enabled)
/// - Floating layout → floating config (if enabled)
/// - Otherwise → focused config
///
/// Always sends unfocused color as inactive_color.
/// All settings are batched into a single JankyBorders call.
pub fn on_focus_changed(layout: LayoutType, is_window_floating: bool) {
    let config = get_config();
    let borders = &config.tiling.borders;

    if !borders.is_enabled() {
        return;
    }

    // Determine which config to use for active color
    let active_config = if layout == LayoutType::Monocle && borders.monocle.is_enabled() {
        &borders.monocle
    } else if (layout == LayoutType::Floating || is_window_floating)
        && borders.floating.is_enabled()
    {
        &borders.floating
    } else {
        &borders.focused
    };

    // Get colors and width
    let (active_color, width) = get_border_settings(active_config);
    let (inactive_color, _) = get_border_settings(&borders.unfocused);

    // Build and send command
    let command =
        format!("width={width} active_color={active_color} inactive_color={inactive_color}");

    send_command(&command);
}

/// Refreshes border configuration.
///
/// Call this when configuration is reloaded.
pub fn refresh() {
    // Clear cache to force re-send
    *get_last_command().lock() = String::new();

    // Re-initialize
    init();
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rgba_to_hex() {
        let rgba = Rgba { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };
        assert_eq!(rgba_to_hex(&rgba), "0xFFFF0000");
    }

    #[test]
    fn test_hex_to_janky() {
        assert_eq!(hex_to_janky("#FF0000"), Some("0xFFFF0000".to_string()));
        assert_eq!(hex_to_janky("#00FF00"), Some("0xFF00FF00".to_string()));
    }

    #[test]
    fn test_get_border_settings_disabled() {
        let config = BorderStateConfig::Disabled(false);
        let (color, width) = get_border_settings(&config);
        assert_eq!(color, "0x00000000");
        assert_eq!(width, 0);
    }
}
