//! Commented TOML templates generated from a type's default and its schema.
//!
//! There is no hand-written template: the values come from `Default` serialized
//! straight to a TOML document (so keys keep struct declaration order), and every
//! comment comes from the schemars description of the field. The same function
//! therefore stays correct when a configuration field is added or renamed.
//!
//! Layout: line 1 is `#:schema <url>`. Scalars and arrays are written before the
//! sub-tables of the same table (TOML requires it), non-empty arrays of objects
//! become `[[arrays of tables]]`, and empty maps stay inline as `{}`. A field's
//! description is written as `#` comment lines above the key, above a `[table]`
//! header, or above the first `[[element]]` of an array of tables.

use schemars::Schema;
use serde::Serialize;
use serde_json::Value as Json;
use toml_edit::{DocumentMut, Item, Table, Value};

use crate::config::diagnostics::ConfigFileId;
use crate::config::schema::schema_url;

/// Serializes a value to an ordered TOML document.
///
/// # Errors
/// Returns the serializer's reason, for example for a `None` that was not
/// skipped or an integer above `i64::MAX`.
pub fn to_document<T: Serialize + ?Sized>(value: &T) -> Result<DocumentMut, toml_edit::ser::Error> {
    toml_edit::ser::to_document(value)
}

/// Renders one configuration file from a document and its schema.
#[must_use]
pub fn render(file: ConfigFileId, mut document: DocumentMut, schema: &Schema) -> String {
    let renderer = Renderer { root: schema.as_value() };
    renderer.table(document.as_table_mut(), Some(schema.as_value()));
    let body = document.to_string();
    let body = body.trim_start_matches('\n');
    format!("#:schema {}\n\n{body}", schema_url(file))
}

struct Renderer<'a> {
    root: &'a Json,
}

impl Renderer<'_> {
    /// Turns inline tables into real tables, adds comments, recursively.
    fn table(&self, table: &mut Table, schema: Option<&Json>) {
        let keys: Vec<String> = table.iter().map(|(key, _)| key.to_owned()).collect();
        let mut first_value = true;
        for key in keys {
            let property = schema.and_then(|schema| self.property(schema, &key));
            let comment = property.and_then(comment_lines);
            let Some(item) = table.get_mut(&key) else {
                continue;
            };
            restructure(item);
            match item {
                Item::Table(child) => {
                    child.set_implicit(true);
                    self.table(child, property);
                    child.decor_mut().set_prefix(
                        comment.map_or_else(|| "\n".to_owned(), |lines| format!("\n{lines}")),
                    );
                }
                Item::ArrayOfTables(array) => {
                    let element = property.and_then(|schema| self.items(schema));
                    for (index, child) in array.iter_mut().enumerate() {
                        self.table(child, element);
                        let prefix = match (&comment, index) {
                            (Some(lines), 0) => format!("\n{lines}"),
                            _ => "\n".to_owned(),
                        };
                        child.decor_mut().set_prefix(prefix);
                    }
                }
                Item::Value(_) | Item::None => {
                    let prefix = match (&comment, first_value) {
                        (Some(lines), true) => lines.clone(),
                        (Some(lines), false) => format!("\n{lines}"),
                        (None, _) => String::new(),
                    };
                    first_value = false;
                    if !prefix.is_empty()
                        && let Some(mut key) = table.key_mut(&key)
                    {
                        key.leaf_decor_mut().set_prefix(prefix);
                    }
                }
            }
        }
    }

    fn property<'s>(&'s self, schema: &'s Json, key: &str) -> Option<&'s Json> {
        let schema = self.resolve(schema);
        schema
            .get("properties")
            .and_then(|properties| properties.get(key))
            .or_else(|| self.branch_property(schema, key))
            .or_else(|| schema.get("additionalProperties").filter(|extra| extra.is_object()))
    }

    /// The property from the first union branch that declares it, for untagged
    /// unions such as gaps (one table or a per-display list).
    fn branch_property<'s>(&'s self, schema: &'s Json, key: &str) -> Option<&'s Json> {
        ["anyOf", "oneOf"]
            .iter()
            .filter_map(|keyword| schema.get(*keyword).and_then(Json::as_array))
            .flatten()
            .find_map(|branch| {
                self.resolve(branch)
                    .get("properties")
                    .and_then(|properties| properties.get(key))
            })
    }

    fn items<'s>(&'s self, schema: &'s Json) -> Option<&'s Json> {
        self.resolve(schema).get("items").filter(|items| items.is_object())
    }

    /// Follows `$ref`, a single `allOf`, and the non-null branch of an optional.
    fn resolve<'s>(&'s self, schema: &'s Json) -> &'s Json {
        if let Some(pointer) = schema
            .get("$ref")
            .and_then(Json::as_str)
            .and_then(|reference| reference.strip_prefix('#'))
            && let Some(target) = self.root.pointer(pointer)
        {
            return self.resolve(target);
        }
        for keyword in ["allOf", "anyOf", "oneOf"] {
            let Some(branches) = schema.get(keyword).and_then(Json::as_array) else {
                continue;
            };
            let mut candidates =
                branches.iter().filter(|branch| branch.get("type") != Some(&Json::from("null")));
            if let (Some(only), None) = (candidates.next(), candidates.next()) {
                return self.resolve(only);
            }
        }
        schema
    }
}

/// Converts an inline table or a non-empty array of inline tables into
/// the table forms; everything else (including `{}` and `[]`) stays a value.
fn restructure(item: &mut Item) {
    match std::mem::take(item) {
        Item::Value(Value::InlineTable(inline)) if !inline.is_empty() => {
            *item = Item::Table(inline.into_table());
        }
        Item::Value(Value::Array(array)) if !array.is_empty() => {
            *item = match Item::Value(Value::Array(array)).into_array_of_tables() {
                Ok(tables) => Item::ArrayOfTables(tables),
                Err(original) => original,
            };
        }
        other => *item = other,
    }
}

/// The description as `#` lines, each ending in a newline.
fn comment_lines(schema: &Json) -> Option<String> {
    let description = schema.get("description")?.as_str()?;
    let mut lines = String::new();
    for line in description.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            lines.push_str("#\n");
        } else {
            lines.push_str("# ");
            lines.push_str(line);
            lines.push('\n');
        }
    }
    (!lines.is_empty()).then_some(lines)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::module::ModuleId;

    #[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
    #[serde(default, rename_all = "snake_case")]
    struct Inner {
        /// A nested switch.
        flag: bool,
    }

    #[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
    #[serde(default, rename_all = "snake_case")]
    struct Rule {
        /// Rule name.
        name: String,
        /// Rule weight.
        weight: u32,
    }

    #[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
    #[serde(default, rename_all = "snake_case")]
    struct Sample {
        /// Whether the feature is enabled.
        ///
        /// A second paragraph.
        enabled: bool,
        /// Nested settings.
        inner: Inner,
        /// Ordered names.
        names: Vec<String>,
        /// Rules in order.
        rules: Vec<Rule>,
        /// Free-form map.
        extra: BTreeMap<String, String>,
        /// An optional note.
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        /// A trailing scalar.
        count: u32,
    }

    fn file() -> ConfigFileId { ConfigFileId::module(ModuleId::new("sample")) }

    fn rendered(value: &impl Serialize) -> String {
        render(
            file(),
            to_document(value).expect("document"),
            &schemars::schema_for!(Sample),
        )
    }

    #[test]
    fn scalars_come_first_in_declaration_order_then_tables() {
        assert_eq!(
            rendered(&Sample::default()),
            "\
#:schema https://raw.githubusercontent.com/marcosmoura/stache/main/schemas/sample.schema.json

# Whether the feature is enabled.
#
# A second paragraph.
enabled = false

# Ordered names.
names = []

# Rules in order.
rules = []

# Free-form map.
extra = {}

# A trailing scalar.
count = 0

# Nested settings.
[inner]
# A nested switch.
flag = false
"
        );
    }

    #[test]
    fn arrays_of_objects_become_arrays_of_tables_with_one_comment() {
        let sample = Sample {
            rules: vec![
                Rule {
                    name: "a".to_owned(),
                    weight: 1,
                },
                Rule {
                    name: "b".to_owned(),
                    weight: 2,
                },
            ],
            ..Sample::default()
        };
        let text = rendered(&sample);
        assert!(text.contains(
            "\n# Rules in order.\n[[rules]]\n# Rule name.\nname = \"a\"\n\n# Rule weight.\nweight = 1\n\n[[rules]]\n# Rule name.\nname = \"b\""
        ), "{text}");
        let parsed: toml::Table = text.parse().expect("valid TOML");
        assert_eq!(parsed["rules"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn map_keys_that_are_not_bare_are_quoted() {
        let sample = Sample {
            extra: BTreeMap::from([("cmd+shift+.".to_owned(), "x".to_owned())]),
            ..Sample::default()
        };
        let text = rendered(&sample);
        assert!(text.contains("[extra]\n\"cmd+shift+.\" = \"x\""), "{text}");
        let parsed: toml::Table = text.parse().expect("valid TOML");
        assert_eq!(parsed["extra"]["cmd+shift+."].as_str(), Some("x"));
    }

    #[test]
    fn absent_options_are_skipped_and_present_ones_written() {
        let sample = Sample {
            note: Some("hi".to_owned()),
            ..Sample::default()
        };
        assert!(rendered(&sample).contains("# An optional note.\nnote = \"hi\""));
        assert!(!rendered(&Sample::default()).contains("note"));
    }
}
