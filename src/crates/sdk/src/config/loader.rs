//! Strict TOML decoding into registered Rust types.
//!
//! A supplied file that fails to parse or deserialize is reported as a
//! diagnostic and never replaced by defaults. The parsed table is converted to
//! a JSON value so the catalog keeps one type-erased decode boundary; values
//! JSON cannot hold (date/times, `nan`, `inf`) are rejected on the way.

use std::path::Path;

use serde::de::DeserializeOwned;
use serde_json::{Map, Number, Value};

use crate::config::diagnostics::{ConfigDiagnostic, ConfigFileId, SourceLocation};
use crate::module::ModuleConfig;

/// The diagnostic message for a file that contains no TOML content at all.
const EMPTY_FILE_MESSAGE: &str = "file is empty; add a `#` comment or a key to use defaults";

/// Parses TOML text into a JSON object value.
///
/// A file that holds only comments is an empty table.
///
/// # Errors
/// Returns a type diagnostic for a whitespace-only file or a value JSON cannot
/// hold, or a parse diagnostic with the source position for invalid TOML.
pub fn parse_text(file: ConfigFileId, path: &Path, text: &str) -> Result<Value, ConfigDiagnostic> {
    if text.trim().is_empty() {
        return Err(ConfigDiagnostic::typed_message(
            file,
            path,
            None,
            EMPTY_FILE_MESSAGE,
        ));
    }
    let table = text.parse::<toml::Table>().map_err(|error| {
        let offset = error.span().map_or(0, |span| span.start);
        ConfigDiagnostic::parse(
            file,
            path,
            location_at(text, offset),
            single_line(error.message()),
        )
    })?;
    let mut object = Map::with_capacity(table.len());
    for (key, value) in table {
        let field = key.clone();
        object.insert(key, to_json(file, path, value, &field)?);
    }
    Ok(Value::Object(object))
}

/// The 1-based line and character column of a byte offset.
fn location_at(text: &str, offset: usize) -> SourceLocation {
    let mut end = offset.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let before = &text[..end];
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    SourceLocation {
        line,
        column: before[line_start..].chars().count() + 1,
    }
}

/// The first line of a parser reason.
fn single_line(message: &str) -> &str { message.lines().next().unwrap_or("").trim_end() }

fn to_json(
    file: ConfigFileId,
    path: &Path,
    value: toml::Value,
    field: &str,
) -> Result<Value, ConfigDiagnostic> {
    let reject = |message: &str| {
        Err(ConfigDiagnostic::typed_message(
            file,
            path,
            Some(field.to_owned()),
            message,
        ))
    };
    match value {
        toml::Value::String(text) => Ok(Value::String(text)),
        toml::Value::Integer(number) => Ok(Value::Number(number.into())),
        toml::Value::Float(number) => Number::from_f64(number).map(Value::Number).map_or_else(
            || reject("`nan` and `inf` are not valid configuration values"),
            Ok,
        ),
        toml::Value::Boolean(flag) => Ok(Value::Bool(flag)),
        toml::Value::Datetime(_) => reject("TOML date and time values are not supported"),
        toml::Value::Array(items) => items
            .into_iter()
            .enumerate()
            .map(|(index, item)| to_json(file, path, item, &format!("{field}[{index}]")))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        toml::Value::Table(table) => {
            let mut object = Map::with_capacity(table.len());
            for (key, item) in table {
                let nested = format!("{field}.{key}");
                object.insert(key, to_json(file, path, item, &nested)?);
            }
            Ok(Value::Object(object))
        }
    }
}

/// Deserializes an already parsed value into `C`.
///
/// # Errors
/// Returns a type diagnostic naming the failing field path.
pub fn deserialize<C: DeserializeOwned>(
    file: ConfigFileId,
    path: &Path,
    value: Value,
) -> Result<C, ConfigDiagnostic> {
    serde_path_to_error::deserialize(value)
        .map_err(|error| ConfigDiagnostic::typed(file, path, &error))
}

/// Parses and deserializes TOML text in one step.
///
/// # Errors
/// Returns a parse or type diagnostic for the supplied text.
pub fn decode<C: DeserializeOwned>(
    file: ConfigFileId,
    path: &Path,
    text: &str,
) -> Result<C, ConfigDiagnostic> {
    deserialize(file, path, parse_text(file, path, text)?)
}

/// Rejects a typed value that its module refuses to apply.
///
/// # Errors
/// Returns a validation diagnostic carrying the module's message.
pub fn validate<C: ModuleConfig>(
    file: ConfigFileId,
    path: &Path,
    config: &C,
) -> Result<(), ConfigDiagnostic> {
    config
        .validate()
        .map_err(|error| ConfigDiagnostic::validation(file, path, &error))
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;
    use crate::config::DiagnosticKind;
    use crate::module::ModuleId;

    #[derive(Debug, Default, Deserialize, PartialEq)]
    #[serde(default, rename_all = "snake_case")]
    struct Sample {
        enabled: bool,
        items: Vec<String>,
        ratio: f64,
    }

    fn file() -> ConfigFileId { ConfigFileId::module(ModuleId::new("sample")) }

    fn diagnose(text: &str) -> ConfigDiagnostic {
        decode::<Sample>(file(), Path::new("sample.toml"), text).expect_err("rejected")
    }

    #[test]
    fn comments_and_quoted_keys_are_accepted() {
        let text = "# a comment\nenabled = true # trailing\nitems = [\"one\", \"two\",]\n";
        let sample: Sample = decode(file(), Path::new("sample.toml"), text).expect("decode");
        assert!(sample.enabled);
        assert_eq!(sample.items, ["one", "two"]);
    }

    #[test]
    fn parse_errors_carry_a_one_based_line_and_column() {
        let diagnostic = diagnose("enabled = true\nitems = [\n\"é\" \"x\"\n]\n");
        assert_eq!(diagnostic.kind, DiagnosticKind::Parse);
        let location = diagnostic.location.expect("location");
        assert_eq!(location.line, 3);
        assert!(location.column >= 1);
        assert!(!diagnostic.message.contains('\n'));
        assert!(diagnostic.to_string().starts_with("sample.toml:3:"));
    }

    #[test]
    fn character_columns_count_characters_not_bytes() {
        let location = location_at("é = 1\nx = ", "é = 1\nx = ".len());
        assert_eq!(location, SourceLocation { line: 2, column: 5 });
        assert_eq!(location_at("éé", 2), SourceLocation { line: 1, column: 2 });
    }

    #[test]
    fn duplicate_keys_and_redefined_tables_are_parse_errors() {
        assert_eq!(
            diagnose("enabled = true\nenabled = false\n").kind,
            DiagnosticKind::Parse
        );
        assert_eq!(diagnose("[a]\nx = 1\n[a]\ny = 2\n").kind, DiagnosticKind::Parse);
        assert_eq!(diagnose("items = [\"\\q\"]\n").kind, DiagnosticKind::Parse);
        assert_eq!(diagnose("items = [\"open]\n").kind, DiagnosticKind::Parse);
    }

    #[test]
    fn json_text_is_not_toml() {
        assert_eq!(diagnose("{ \"enabled\": true }").kind, DiagnosticKind::Parse);
    }

    #[test]
    fn type_errors_name_the_field_path() {
        let diagnostic = diagnose("items = [\"ok\", 7]\n");
        assert_eq!(diagnostic.kind, DiagnosticKind::Type);
        assert_eq!(diagnostic.field.as_deref(), Some("items[1]"));
    }

    #[test]
    fn date_times_are_rejected_naming_the_field() {
        for text in [
            "stamp = 1979-05-27T07:32:00Z\n",
            "stamp = 1979-05-27\n",
            "stamp = 07:32:00\n",
        ] {
            let diagnostic = diagnose(text);
            assert_eq!(diagnostic.kind, DiagnosticKind::Type, "{text}");
            assert_eq!(diagnostic.field.as_deref(), Some("stamp"), "{text}");
        }
        let nested = diagnose("[a]\nlist = [1979-05-27]\n");
        assert_eq!(nested.field.as_deref(), Some("a.list[0]"));
    }

    #[test]
    fn nan_and_inf_are_rejected_naming_the_field() {
        for text in ["ratio = nan\n", "ratio = inf\n", "ratio = -inf\n"] {
            let diagnostic = diagnose(text);
            assert_eq!(diagnostic.kind, DiagnosticKind::Type, "{text}");
            assert_eq!(diagnostic.field.as_deref(), Some("ratio"), "{text}");
        }
    }

    #[test]
    fn schema_is_an_ordinary_key_and_the_schema_comment_is_a_comment() {
        let text = "#:schema https://example.invalid/sample.schema.json\nenabled = true\n";
        let sample: Sample = decode(file(), Path::new("sample.toml"), text).expect("decode");
        assert!(sample.enabled);
    }

    #[test]
    fn a_comment_only_file_uses_the_defaults() {
        let sample: Sample =
            decode(file(), Path::new("sample.toml"), "#:schema x\n# nothing\n").expect("decode");
        assert_eq!(sample, Sample::default());
    }

    #[test]
    fn an_empty_file_says_how_to_use_defaults() {
        let diagnostic = diagnose(" \n\t");
        assert_eq!(diagnostic.kind, DiagnosticKind::Type);
        assert_eq!(
            diagnostic.message,
            "file is empty; add a `#` comment or a key to use defaults"
        );
    }
}
