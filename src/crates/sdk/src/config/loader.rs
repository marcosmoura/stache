//! Strict JSONC decoding into registered Rust types.
//!
//! Comments and trailing commas are enabled; every other loose JSON extension
//! is disabled. A supplied file that fails to parse or deserialize is reported
//! as a diagnostic and never replaced by defaults.

use std::path::Path;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config::diagnostics::{ConfigDiagnostic, ConfigFileId};
use crate::module::ModuleConfig;

/// The only JSONC extensions Stache accepts.
pub(crate) const PARSE_OPTIONS: jsonc_parser::ParseOptions = jsonc_parser::ParseOptions {
    allow_comments: true,
    allow_trailing_commas: true,
    allow_loose_object_property_names: false,
    allow_missing_commas: false,
    allow_single_quoted_strings: false,
    allow_hexadecimal_numbers: false,
    allow_unary_plus_numbers: false,
    allow_bare_decimal_point_numbers: false,
    allow_non_finite_numbers: false,
    allow_extended_string_escapes: false,
};

/// The diagnostic message for a file that contains no JSON value at all.
const EMPTY_FILE_MESSAGE: &str = "file is empty; write `{}` to use defaults";

/// Parses JSONC text into a JSON value.
///
/// # Errors
/// Returns a type diagnostic for a whitespace-only file, or a parse diagnostic
/// with the source position when the text is not valid under the enabled syntax
/// options.
pub fn parse_text(file: ConfigFileId, path: &Path, text: &str) -> Result<Value, ConfigDiagnostic> {
    if text.trim().is_empty() {
        return Err(ConfigDiagnostic::typed_message(
            file,
            path,
            None,
            EMPTY_FILE_MESSAGE,
        ));
    }
    jsonc_parser::parse_to_serde_value(text, &PARSE_OPTIONS)
        .map_err(|error| ConfigDiagnostic::parse(file, path, &error))
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

/// Parses and deserializes JSONC text in one step.
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
    use crate::module::ModuleId;

    #[derive(Debug, Default, Deserialize, PartialEq)]
    #[serde(default, rename_all = "camelCase")]
    struct Sample {
        enabled: bool,
        items: Vec<String>,
    }

    fn file() -> ConfigFileId { ConfigFileId::module(ModuleId::new("sample")) }

    #[test]
    fn comments_and_trailing_commas_are_accepted() {
        let text = r#"{
            // a line comment
            "enabled": true,
            /* a block comment */
            "items": ["one", "two",],
        }"#;
        let sample: Sample = decode(file(), Path::new("sample.jsonc"), text).expect("decode");
        assert!(sample.enabled);
        assert_eq!(sample.items, ["one", "two"]);
    }

    #[test]
    fn single_quoted_strings_are_rejected() {
        let diagnostic = decode::<Sample>(file(), Path::new("sample.jsonc"), "{ 'enabled': true }")
            .expect_err("single quotes are not JSONC here");
        assert_eq!(diagnostic.location.map(|location| location.line), Some(1));
    }

    #[test]
    fn missing_commas_are_rejected() {
        let text = r#"{ "enabled": true "items": [] }"#;
        assert!(decode::<Sample>(file(), Path::new("sample.jsonc"), text).is_err());
    }

    #[test]
    fn hexadecimal_numbers_are_rejected() {
        assert!(
            decode::<Sample>(file(), Path::new("sample.jsonc"), "{ \"enabled\": 0x10 }").is_err()
        );
    }

    #[test]
    fn type_errors_name_the_field_path() {
        let text = r#"{ "items": ["ok", 7] }"#;
        let diagnostic =
            decode::<Sample>(file(), Path::new("sample.jsonc"), text).expect_err("wrong item type");
        assert_eq!(diagnostic.kind, crate::config::DiagnosticKind::Type);
        assert_eq!(diagnostic.field.as_deref(), Some("items[1]"));
    }

    #[test]
    fn schema_metadata_is_ignored() {
        let text = r#"{ "$schema": "./schemas/sample.schema.json", "enabled": true }"#;
        let sample: Sample = decode(file(), Path::new("sample.jsonc"), text).expect("decode");
        assert!(sample.enabled);
    }

    #[test]
    fn empty_objects_keep_per_field_defaults() {
        let sample: Sample = decode(file(), Path::new("sample.jsonc"), "{}").expect("decode");
        assert_eq!(sample, Sample::default());
    }

    #[test]
    fn an_empty_file_says_how_to_use_defaults() {
        let diagnostic = decode::<Sample>(file(), Path::new("sample.jsonc"), " \n\t")
            .expect_err("empty file is not a value");
        assert_eq!(diagnostic.kind, crate::config::DiagnosticKind::Type);
        assert_eq!(diagnostic.message, "file is empty; write `{}` to use defaults");
    }
}
