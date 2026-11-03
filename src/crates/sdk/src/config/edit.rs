//! Comment-preserving edits to JSONC configuration files.
//!
//! Only a boolean `enabled` property is edited. The parser's AST ranges locate
//! the existing value or the object opening brace, so comments, ordering,
//! unknown keys and formatting outside that span are preserved. The caller
//! validates and writes the returned text; this module never touches disk.

use std::fmt;

use jsonc_parser::ast::Value;
use jsonc_parser::common::Ranged;
use jsonc_parser::{CollectOptions, parse_to_ast};

use crate::config::loader;

/// The outcome of applying an edit to source text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigEdit {
    /// The requested value was already present.
    Unchanged,
    /// The complete replacement text.
    Changed(String),
}

impl ConfigEdit {
    /// The edited text, or `None` when nothing changed.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Changed(text) => Some(text),
            Self::Unchanged => None,
        }
    }
}

/// Why a comment-preserving edit could not be planned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigEditError {
    /// The text is not valid JSONC under the loader's syntax options.
    Parse(String),
    /// The root value is not an object, so it has no `enabled` property.
    NotAnObject,
    /// `enabled` exists but is not a boolean literal.
    NotBoolean {
        /// The spelling of the value that was found.
        found: String,
    },
}

impl fmt::Display for ConfigEditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(message) => write!(f, "cannot edit invalid JSONC: {message}"),
            Self::NotAnObject => f.write_str("the configuration root must be an object"),
            Self::NotBoolean { found } => {
                write!(f, "the `enabled` value must be a boolean, found `{found}`")
            }
        }
    }
}

impl std::error::Error for ConfigEditError {}

/// Sets the root `enabled` boolean while preserving the rest of the text.
///
/// When the property is absent it is inserted at the start of the object, which
/// leaves every existing comment, value and trailing comma untouched. When it is
/// present only its value span is replaced.
///
/// # Errors
/// Returns [`ConfigEditError`] when the text is not a JSONC object or `enabled`
/// is not a boolean.
pub fn set_enabled(text: &str, enabled: bool) -> Result<ConfigEdit, ConfigEditError> {
    let parsed = parse_to_ast(text, &CollectOptions::default(), &loader::PARSE_OPTIONS)
        .map_err(|error| ConfigEditError::Parse(error.kind().to_string()))?;
    let Some(Value::Object(object)) = parsed.value else {
        return Err(ConfigEditError::NotAnObject);
    };
    let Some(property) = object
        .properties
        .iter()
        .rev()
        .find(|property| property.name.as_str() == "enabled")
    else {
        let mut output = String::with_capacity(text.len() + 24);
        let insert_at = object.range.start + 1;
        output.push_str(&text[..insert_at]);
        if object.properties.is_empty() {
            output.push_str(if enabled {
                "\"enabled\": true"
            } else {
                "\"enabled\": false"
            });
        } else if enabled {
            output.push_str("\"enabled\": true,");
        } else {
            output.push_str("\"enabled\": false,");
        }
        output.push_str(&text[insert_at..]);
        return Ok(ConfigEdit::Changed(output));
    };
    let Value::BooleanLit(boolean) = &property.value else {
        return Err(ConfigEditError::NotBoolean {
            found: property.value.text(text).to_owned(),
        });
    };
    if boolean.value == enabled {
        return Ok(ConfigEdit::Unchanged);
    }
    let mut output = String::with_capacity(text.len());
    output.push_str(&text[..property.value.start()]);
    output.push_str(if enabled { "true" } else { "false" });
    output.push_str(&text[property.value.end()..]);
    Ok(ConfigEdit::Changed(output))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggling_preserves_comments_and_unrelated_values() {
        let source = r#"{
  // Keep this explanation.
  "$schema": "https://example.invalid/global.schema.json",
  "enabled": false, // trailing note
  "output": [{ "name": "Speakers", "strategy": "exact" }]
}
"#;
        let ConfigEdit::Changed(edited) = set_enabled(source, true).expect("edit") else {
            panic!("expected a change");
        };
        assert!(edited.contains("// Keep this explanation."));
        assert!(edited.contains("// trailing note"));
        assert!(edited.contains("\"$schema\""));
        assert!(edited.contains("\"Speakers\""));
        assert!(edited.contains("\"enabled\": true,"));
        assert!(!edited.contains("\"enabled\": false"));
        let value: serde_json::Value =
            jsonc_parser::parse_to_serde_value(&edited, &loader::PARSE_OPTIONS)
                .expect("valid edited text");
        assert_eq!(value["enabled"], serde_json::json!(true));
        assert_eq!(value["output"][0]["name"], serde_json::json!("Speakers"));
    }

    #[test]
    fn the_same_value_is_unchanged() {
        assert_eq!(
            set_enabled("{ \"enabled\": true }\n", true).expect("edit"),
            ConfigEdit::Unchanged
        );
    }

    #[test]
    fn a_missing_property_is_inserted_without_reformatting() {
        let source = "{\n  // only a comment\n}\n";
        let ConfigEdit::Changed(edited) = set_enabled(source, true).expect("edit") else {
            panic!("expected a change");
        };
        assert!(edited.starts_with("{\"enabled\": true\n"));
        assert!(edited.contains("// only a comment"));
        let value: serde_json::Value =
            jsonc_parser::parse_to_serde_value(&edited, &loader::PARSE_OPTIONS).expect("valid");
        assert_eq!(value["enabled"], serde_json::json!(true));

        let source = "{ \"output\": [] }";
        let ConfigEdit::Changed(edited) = set_enabled(source, false).expect("edit") else {
            panic!("expected a change");
        };
        let value: serde_json::Value =
            jsonc_parser::parse_to_serde_value(&edited, &loader::PARSE_OPTIONS).expect("valid");
        assert_eq!(value["enabled"], serde_json::json!(false));
        assert_eq!(value["output"], serde_json::json!([]));
    }

    #[test]
    fn an_empty_object_stays_valid() {
        let ConfigEdit::Changed(edited) = set_enabled("{}", true).expect("edit") else {
            panic!("expected a change");
        };
        assert_eq!(edited, "{\"enabled\": true}");
    }

    #[test]
    fn a_trailing_comma_and_duplicate_keys_are_tolerated() {
        let source = "{ \"enabled\": false, \"enabled\": true, }\n";
        let ConfigEdit::Changed(edited) = set_enabled(source, false).expect("edit") else {
            panic!("expected a change");
        };
        let value: serde_json::Value =
            jsonc_parser::parse_to_serde_value(&edited, &loader::PARSE_OPTIONS).expect("valid");
        assert_eq!(value["enabled"], serde_json::json!(false));
    }

    #[test]
    fn invalid_roots_and_values_are_rejected() {
        assert_eq!(set_enabled("[]", true), Err(ConfigEditError::NotAnObject));
        assert!(matches!(
            set_enabled("{ \"enabled\": \"yes\" }", true),
            Err(ConfigEditError::NotBoolean { .. })
        ));
        assert!(matches!(
            set_enabled("{ \"enabled\": ", true),
            Err(ConfigEditError::Parse(_))
        ));
    }
}
