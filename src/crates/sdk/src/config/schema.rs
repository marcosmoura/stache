//! Per-file JSON Schemas generated from the registered Rust types.
//!
//! Schemas are committed under `schemas/` by the `generate-schema` binary. The
//! `$id` matches the file's public URL, so the `#:schema` first line of a
//! configuration file points editors at the authoritative schema for that file.

use schemars::Schema;

use crate::config::diagnostics::ConfigFileId;

/// Directory of the committed schemas in the repository.
pub const SCHEMA_DIRECTORY: &str = "schemas";

/// Base URL used by committed schema `$id`s and generated `#:schema` lines.
pub const SCHEMA_BASE_URL: &str =
    "https://raw.githubusercontent.com/marcosmoura/stache/main/schemas";

/// The public URL of one file's schema.
#[must_use]
pub fn schema_url(file: ConfigFileId) -> String {
    format!("{SCHEMA_BASE_URL}/{}.schema.json", file.stem())
}

/// Renders a schema deterministically with a trailing newline.
///
/// # Errors
/// Returns the serialization error instead of emitting a partial schema.
pub fn render(schema: &Schema) -> Result<String, serde_json::Error> {
    let mut rendered = serde_json::to_string_pretty(schema)?;
    rendered.push('\n');
    Ok(rendered)
}

/// The committed file name for one schema.
#[must_use]
pub fn file_name(file: ConfigFileId) -> String { format!("{}.schema.json", file.stem()) }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::ModuleId;

    #[test]
    fn schema_file_names_match_the_public_url() {
        let file = ConfigFileId::module(ModuleId::new("proxyAudio"));
        assert_eq!(file_name(file), "proxyAudio.schema.json");
        assert_eq!(
            schema_url(file),
            "https://raw.githubusercontent.com/marcosmoura/stache/main/schemas/proxyAudio.schema.json"
        );
    }

    #[test]
    fn render_ends_with_a_newline() {
        let schema: Schema = serde_json::json!({"type": "object"}).try_into().expect("schema");
        assert_eq!(
            render(&schema).expect("render"),
            "{\n  \"type\": \"object\"\n}\n"
        );
    }
}
