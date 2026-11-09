//! Border color syntax validation.

/// Checks a hex (`#RGB`, `#RGBA`, `#RRGGBB`, `#RRGGBBAA`, `#` optional) or
/// `rgb()`/`rgba()` color without converting it.
pub(super) fn validate_color(field: &str, value: &str) -> Result<(), String> {
    let trimmed = value.trim();
    let result = if trimmed.starts_with("rgba(") || trimmed.starts_with("rgb(") {
        validate_functional(trimmed)
    } else {
        validate_hex(trimmed)
    };
    result.map_err(|reason| format!("{field}: invalid color `{value}`: {reason}"))
}

fn validate_hex(value: &str) -> Result<(), String> {
    let digits = value.trim_start_matches('#');
    if !matches!(digits.len(), 3 | 4 | 6 | 8) || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("expected 3, 4, 6 or 8 hexadecimal digits".to_owned());
    }
    Ok(())
}

fn validate_functional(value: &str) -> Result<(), String> {
    let (inner, has_alpha) = if let Some(inner) = value.strip_prefix("rgba(") {
        (inner, true)
    } else if let Some(inner) = value.strip_prefix("rgb(") {
        (inner, false)
    } else {
        return Err("expected rgb( or rgba(".to_owned());
    };
    let inner = inner.strip_suffix(')').ok_or("missing closing parenthesis")?;
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    let expected = if has_alpha { 4 } else { 3 };
    if parts.len() != expected {
        return Err(format!("expected {expected} values, got {}", parts.len()));
    }
    for part in &parts[..3] {
        part.parse::<u8>()
            .map_err(|_| format!("`{part}` is not a channel from 0 to 255"))?;
    }
    if has_alpha {
        let alpha: f64 = parts[3].parse().map_err(|_| format!("`{}` is not an alpha", parts[3]))?;
        if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
            return Err(format!("alpha must be between 0 and 1, got {alpha}"));
        }
    }
    Ok(())
}
