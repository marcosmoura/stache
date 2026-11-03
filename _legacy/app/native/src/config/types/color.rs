//! Color parsing utilities for configuration.
//!
//! Provides types and functions for parsing color values from configuration strings.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// RGBA color representation for border rendering.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Rgba {
    /// Red channel (0.0 - 1.0).
    pub r: f64,
    /// Green channel (0.0 - 1.0).
    pub g: f64,
    /// Blue channel (0.0 - 1.0).
    pub b: f64,
    /// Alpha channel (0.0 - 1.0).
    pub a: f64,
}

impl Rgba {
    /// Creates a new RGBA color.
    #[must_use]
    pub const fn new(r: f64, g: f64, b: f64, a: f64) -> Self { Self { r, g, b, a } }

    /// Creates an opaque black color.
    #[must_use]
    pub const fn black() -> Self { Self::new(0.0, 0.0, 0.0, 1.0) }

    /// Creates an opaque white color.
    #[must_use]
    pub const fn white() -> Self { Self::new(1.0, 1.0, 1.0, 1.0) }
}

impl Default for Rgba {
    fn default() -> Self { Self::black() }
}

/// Parses a color string to RGBA.
///
/// Supports the following formats:
/// - `#RGB` - 3-digit hex (e.g., "#F00" for red)
/// - `#RGBA` - 4-digit hex with alpha (e.g., "#F00F" for opaque red)
/// - `#RRGGBB` - 6-digit hex (e.g., "#FF0000" for red)
/// - `#RRGGBBAA` - 8-digit hex with alpha (e.g., "#FF0000FF" for opaque red)
/// - `rgba(r, g, b, a)` - CSS rgba format (e.g., "rgba(255, 0, 0, 0.5)")
///
/// The `#` prefix is optional for hex colors.
///
/// # Errors
///
/// Returns an error string if the color format is invalid.
pub fn parse_color(color: &str) -> Result<Rgba, String> {
    let trimmed = color.trim();
    if trimmed.starts_with("rgba(") || trimmed.starts_with("rgb(") {
        parse_rgba_color(trimmed)
    } else {
        parse_hex_color(trimmed)
    }
}

/// Parses an `rgba()` or `rgb()` CSS color string to RGBA.
///
/// Supports the following formats:
/// - `rgb(r, g, b)` - CSS rgb format with 0-255 values
/// - `rgba(r, g, b, a)` - CSS rgba format with 0-255 values and alpha 0.0-1.0
///
/// # Examples
///
/// - `rgba(255, 0, 0, 0.5)` - Semi-transparent red
/// - `rgba(137, 180, 250, 0.2)` - Catppuccin blue with 20% opacity
/// - `rgb(255, 255, 255)` - White
///
/// # Errors
///
/// Returns an error string if the format is invalid.
pub fn parse_rgba_color(rgba: &str) -> Result<Rgba, String> {
    let trimmed = rgba.trim();

    // Check for rgb() or rgba() prefix
    let (inner, has_alpha) = if let Some(inner) = trimmed.strip_prefix("rgba(") {
        (
            inner.strip_suffix(')').ok_or("Missing closing parenthesis")?,
            true,
        )
    } else if let Some(inner) = trimmed.strip_prefix("rgb(") {
        (
            inner.strip_suffix(')').ok_or("Missing closing parenthesis")?,
            false,
        )
    } else {
        return Err("Color must start with 'rgb(' or 'rgba('".to_string());
    };

    // Split by comma and parse values
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();

    let expected_parts = if has_alpha { 4 } else { 3 };
    if parts.len() != expected_parts {
        return Err(format!(
            "Expected {} values for {}, got {}",
            expected_parts,
            if has_alpha { "rgba()" } else { "rgb()" },
            parts.len()
        ));
    }

    // Parse RGB values (0-255)
    let r: u8 = parts[0].parse().map_err(|_| format!("Invalid red value: {}", parts[0]))?;
    let g: u8 = parts[1].parse().map_err(|_| format!("Invalid green value: {}", parts[1]))?;
    let b: u8 = parts[2].parse().map_err(|_| format!("Invalid blue value: {}", parts[2]))?;

    // Parse alpha (0.0-1.0 for rgba, default 1.0 for rgb)
    let a: f64 = if has_alpha {
        parts[3].parse().map_err(|_| format!("Invalid alpha value: {}", parts[3]))?
    } else {
        1.0
    };

    // Validate alpha range
    if !(0.0..=1.0).contains(&a) {
        return Err(format!("Alpha value must be between 0.0 and 1.0, got {a}"));
    }

    Ok(Rgba {
        r: f64::from(r) / 255.0,
        g: f64::from(g) / 255.0,
        b: f64::from(b) / 255.0,
        a,
    })
}

/// Parses a hex color string to RGBA.
///
/// Supports the following formats:
/// - `#RGB` - 3-digit hex (e.g., "#F00" for red)
/// - `#RGBA` - 4-digit hex with alpha (e.g., "#F00F" for opaque red)
/// - `#RRGGBB` - 6-digit hex (e.g., "#FF0000" for red)
/// - `#RRGGBBAA` - 8-digit hex with alpha (e.g., "#FF0000FF" for opaque red)
///
/// The `#` prefix is optional.
///
/// # Errors
///
/// Returns an error string if the hex color is invalid.
pub fn parse_hex_color(hex: &str) -> Result<Rgba, String> {
    let hex = hex.trim().trim_start_matches('#');

    let (r, g, b, a) = match hex.len() {
        3 => {
            // RGB format
            let r = u8::from_str_radix(&hex[0..1], 16).map_err(|e| e.to_string())?;
            let g = u8::from_str_radix(&hex[1..2], 16).map_err(|e| e.to_string())?;
            let b = u8::from_str_radix(&hex[2..3], 16).map_err(|e| e.to_string())?;
            // Expand 4-bit to 8-bit by repeating (0xF -> 0xFF)
            (r * 17, g * 17, b * 17, 255)
        }
        4 => {
            // RGBA format
            let r = u8::from_str_radix(&hex[0..1], 16).map_err(|e| e.to_string())?;
            let g = u8::from_str_radix(&hex[1..2], 16).map_err(|e| e.to_string())?;
            let b = u8::from_str_radix(&hex[2..3], 16).map_err(|e| e.to_string())?;
            let a = u8::from_str_radix(&hex[3..4], 16).map_err(|e| e.to_string())?;
            (r * 17, g * 17, b * 17, a * 17)
        }
        6 => {
            // RRGGBB format
            let r = u8::from_str_radix(&hex[0..2], 16).map_err(|e| e.to_string())?;
            let g = u8::from_str_radix(&hex[2..4], 16).map_err(|e| e.to_string())?;
            let b = u8::from_str_radix(&hex[4..6], 16).map_err(|e| e.to_string())?;
            (r, g, b, 255)
        }
        8 => {
            // RRGGBBAA format
            let r = u8::from_str_radix(&hex[0..2], 16).map_err(|e| e.to_string())?;
            let g = u8::from_str_radix(&hex[2..4], 16).map_err(|e| e.to_string())?;
            let b = u8::from_str_radix(&hex[4..6], 16).map_err(|e| e.to_string())?;
            let a = u8::from_str_radix(&hex[6..8], 16).map_err(|e| e.to_string())?;
            (r, g, b, a)
        }
        _ => {
            return Err(format!(
                "Invalid hex color length: expected 3, 4, 6, or 8 characters, got {}",
                hex.len()
            ));
        }
    };

    Ok(Rgba {
        r: f64::from(r) / 255.0,
        g: f64::from(g) / 255.0,
        b: f64::from(b) / 255.0,
        a: f64::from(a) / 255.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hex_3_digit() {
        let color = parse_hex_color("#F00").unwrap();
        assert!((color.r - 1.0).abs() < 0.01);
        assert!(color.g.abs() < 0.01);
        assert!(color.b.abs() < 0.01);
        assert!((color.a - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_hex_6_digit() {
        let color = parse_hex_color("#FF0000").unwrap();
        assert!((color.r - 1.0).abs() < 0.01);
        assert!(color.g.abs() < 0.01);
        assert!(color.b.abs() < 0.01);
    }

    #[test]
    fn test_parse_hex_8_digit() {
        let color = parse_hex_color("#FF000080").unwrap();
        assert!((color.r - 1.0).abs() < 0.01);
        assert!((color.a - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_parse_rgba() {
        let color = parse_rgba_color("rgba(255, 0, 0, 0.5)").unwrap();
        assert!((color.r - 1.0).abs() < 0.01);
        assert!(color.g.abs() < 0.01);
        assert!(color.b.abs() < 0.01);
        assert!((color.a - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_parse_rgb() {
        let color = parse_rgba_color("rgb(255, 255, 255)").unwrap();
        assert!((color.r - 1.0).abs() < 0.01);
        assert!((color.g - 1.0).abs() < 0.01);
        assert!((color.b - 1.0).abs() < 0.01);
        assert!((color.a - 1.0).abs() < 0.01);
    }
}
