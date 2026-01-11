//! CLI output formatting utilities.
//!
//! This module provides utilities for formatting CLI output including:
//! - Tables for structured data display
//! - JSON syntax highlighting

use colored::Colorize;

/// Prints JSON with syntax highlighting.
///
/// Colors:
/// - Keys: Cyan
/// - Strings: Green
/// - Numbers: Yellow
/// - Booleans/Null: Magenta
/// - Brackets/Braces: White (default)
pub fn print_highlighted_json(value: &serde_json::Value) {
    let json_str = serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string());
    print_highlighted_json_str(&json_str);
}

/// Prints a JSON string with syntax highlighting.
fn print_highlighted_json_str(json: &str) {
    let mut in_string = false;
    let mut is_key = false;
    let mut escape_next = false;
    let mut current_token = String::new();
    let mut after_colon = false;

    for ch in json.chars() {
        if escape_next {
            current_token.push(ch);
            escape_next = false;
            continue;
        }

        if ch == '\\' && in_string {
            current_token.push(ch);
            escape_next = true;
            continue;
        }

        match ch {
            '"' => {
                if in_string {
                    // End of string
                    current_token.push(ch);
                    if is_key {
                        print!("{}", current_token.cyan());
                    } else {
                        print!("{}", current_token.green());
                    }
                    current_token.clear();
                    in_string = false;
                    is_key = false;
                } else {
                    // Start of string
                    flush_token(&mut current_token, after_colon);
                    current_token.push(ch);
                    in_string = true;
                    // It's a key if we're not after a colon
                    is_key = !after_colon;
                    after_colon = false;
                }
            }
            ':' if !in_string => {
                flush_token(&mut current_token, false);
                print!("{}", ":".white());
                after_colon = true;
            }
            ',' if !in_string => {
                flush_token(&mut current_token, after_colon);
                print!("{}", ",".white());
                after_colon = false;
            }
            '{' | '}' | '[' | ']' if !in_string => {
                flush_token(&mut current_token, after_colon);
                print!("{}", ch.to_string().white().bold());
                after_colon = false;
            }
            _ => {
                current_token.push(ch);
            }
        }
    }

    // Flush any remaining token
    flush_token(&mut current_token, after_colon);
    println!();
}

/// Flushes the current token with appropriate coloring.
fn flush_token(token: &mut String, is_value: bool) {
    if token.is_empty() {
        return;
    }

    let trimmed = token.trim();
    if trimmed.is_empty() {
        print!("{token}");
    } else if is_value {
        // Find the actual value position in the token
        let start = token.find(|c: char| !c.is_whitespace()).unwrap_or(0);
        let end = token.rfind(|c: char| !c.is_whitespace()).map_or(token.len(), |i| i + 1);

        let prefix = &token[..start];
        let value = &token[start..end];
        let suffix = &token[end..];

        // Check if it's a number, boolean, or null
        if value == "true" || value == "false" || value == "null" {
            print!("{}{}{}", prefix, value.magenta(), suffix);
        } else if value.parse::<f64>().is_ok() {
            print!("{}{}{}", prefix, value.yellow(), suffix);
        } else {
            print!("{token}");
        }
    } else {
        print!("{token}");
    }

    token.clear();
}

/// Truncates a string to a maximum length, adding ellipsis if needed.
#[must_use]
pub fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else if max_len <= 1 {
        "…".to_string()
    } else {
        format!("{}…", &s[..max_len - 1])
    }
}

/// Formats a boolean as a colored string.
#[must_use]
pub fn format_bool(value: bool) -> String {
    if value {
        "✓".green().to_string()
    } else {
        "✗".red().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_long_string() {
        assert_eq!(truncate("hello world", 8), "hello w…");
    }

    #[test]
    fn test_truncate_exact_length() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_min_length() {
        assert_eq!(truncate("hello", 1), "…");
    }

    #[test]
    fn test_format_bool_true() {
        let result = format_bool(true);
        assert!(result.contains('✓'));
    }

    #[test]
    fn test_format_bool_false() {
        let result = format_bool(false);
        assert!(result.contains('✗'));
    }
}
