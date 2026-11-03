//! Output formatting shared by the built-in commands.
//!
//! JSON is serialized by serde; this module only adds optional ANSI styling
//! when stdout is a terminal. Byte counts keep the legacy formatting so cache
//! messages stay byte-for-byte compatible.

const RESET: &str = "\x1b[0m";
const KEY: &str = "\x1b[36m";
const STRING: &str = "\x1b[32m";
const NUMBER: &str = "\x1b[33m";
const LITERAL: &str = "\x1b[35m";
const PUNCTUATION: &str = "\x1b[37m";
const BRACKET: &str = "\x1b[1;37m";

/// Formats a byte count exactly like the legacy cache command.
#[must_use]
#[allow(clippy::cast_precision_loss)] // Human-readable sizes tolerate precision loss.
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} bytes")
    }
}

/// Styles already-serialized pretty JSON when `ansi` is enabled.
///
/// The input is copied verbatim apart from inserted escape sequences, so piped
/// output and JSON round-tripping are unaffected.
#[must_use]
pub fn highlight_json(json: &str, ansi: bool) -> String {
    if !ansi {
        return json.to_owned();
    }
    let characters: Vec<char> = json.chars().collect();
    let mut styled = String::with_capacity(json.len() * 2);
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        match character {
            '"' => {
                let end = string_end(&characters, index);
                let token: String = characters[index..=end].iter().collect();
                let key = next_significant(&characters, end + 1) == Some(':');
                styled.push_str(if key { KEY } else { STRING });
                styled.push_str(&token);
                styled.push_str(RESET);
                index = end + 1;
            }
            ':' | ',' => {
                styled.push_str(PUNCTUATION);
                styled.push(character);
                styled.push_str(RESET);
                index += 1;
            }
            '{' | '}' | '[' | ']' => {
                styled.push_str(BRACKET);
                styled.push(character);
                styled.push_str(RESET);
                index += 1;
            }
            _ => {
                let start = index;
                while index < characters.len()
                    && !matches!(characters[index], '"' | ':' | ',' | '{' | '}' | '[' | ']')
                {
                    index += 1;
                }
                let token: String = characters[start..index].iter().collect();
                styled.push_str(&style_value(&token));
            }
        }
    }
    styled
}

const fn string_end(characters: &[char], start: usize) -> usize {
    let mut index = start + 1;
    let mut escaped = false;
    while index < characters.len() {
        let character = characters[index];
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            return index;
        }
        index += 1;
    }
    characters.len() - 1
}

fn next_significant(characters: &[char], start: usize) -> Option<char> {
    characters
        .iter()
        .skip(start)
        .find(|character| !character.is_whitespace())
        .copied()
}

fn style_value(token: &str) -> String {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return token.to_owned();
    }
    let Some(start) = token.find(|character: char| !character.is_whitespace()) else {
        return token.to_owned();
    };
    let end = token
        .rfind(|character: char| !character.is_whitespace())
        .map_or(token.len(), |index| index + 1);
    let color = if matches!(trimmed, "true" | "false" | "null") {
        LITERAL
    } else if trimmed.parse::<f64>().is_ok() {
        NUMBER
    } else {
        return token.to_owned();
    };
    format!(
        "{}{}{}{}{}",
        &token[..start],
        color,
        &token[start..end],
        RESET,
        &token[end..]
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn byte_formatting_matches_the_legacy_command() {
        assert_eq!(format_bytes(0), "0 bytes");
        assert_eq!(format_bytes(512), "512 bytes");
        assert_eq!(format_bytes(1023), "1023 bytes");
        assert_eq!(format_bytes(1024), "1.00 KB");
        assert_eq!(format_bytes(1536), "1.50 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.00 MB");
        assert_eq!(format_bytes(1024 * 1024 + 512 * 1024), "1.50 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.00 GB");
    }

    #[test]
    fn disabled_styling_copies_the_json_verbatim() {
        let json =
            serde_json::to_string_pretty(&json!({ "enabled": true, "value": 3 })).expect("json");
        assert_eq!(highlight_json(&json, false), json);
    }

    #[test]
    fn styling_only_inserts_escape_sequences() {
        let json = serde_json::to_string_pretty(&json!({
            "enabled": true,
            "value": 3,
            "name": "a \"quoted\" value",
            "missing": null
        }))
        .expect("json");
        let styled = highlight_json(&json, true);
        assert_ne!(styled, json);
        assert_eq!(strip_ansi(&styled), json);
        assert!(styled.contains(KEY), "keys are styled");
        assert!(styled.contains(STRING), "strings are styled");
        assert!(styled.contains(NUMBER), "numbers are styled");
        assert!(styled.contains(LITERAL), "literals are styled");
    }

    #[test]
    fn keys_are_distinguished_from_string_values_by_the_following_colon() {
        let styled = highlight_json(r#"{ "key": "value" }"#, true);
        assert!(styled.contains(&format!("{KEY}\"key\"{RESET}")));
        assert!(styled.contains(&format!("{STRING}\"value\"{RESET}")));
    }

    fn strip_ansi(input: &str) -> String {
        let mut output = String::with_capacity(input.len());
        let mut characters = input.chars();
        while let Some(character) = characters.next() {
            if character == '\x1b' {
                for next in characters.by_ref() {
                    if next == 'm' {
                        break;
                    }
                }
            } else {
                output.push(character);
            }
        }
        output
    }
}
