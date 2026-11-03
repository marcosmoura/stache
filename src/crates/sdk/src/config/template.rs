//! Commented templates generated from a type's default and its schema.
//!
//! There is no hand-written template: the values come from `Default` through
//! serde, and every comment comes from the schemars description of the field.
//! The same function therefore stays correct when a configuration field is
//! added or renamed.

use schemars::Schema;
use serde_json::{Map, Value};

use crate::config::diagnostics::ConfigFileId;
use crate::config::schema::schema_url;

const INDENT: &str = "  ";

/// Renders one configuration file template.
#[must_use]
pub fn render(file: ConfigFileId, default: &Value, schema: &Schema) -> String {
    let mut value = default.clone();
    if let Value::Object(object) = &mut value {
        object.insert("$schema".to_owned(), Value::String(schema_url(file)));
    }
    let renderer = Renderer { root: schema.as_value() };
    let mut output = String::new();
    renderer.value(&mut output, &value, schema.as_value(), 0, true);
    output.push('\n');
    output
}

struct Renderer<'a> {
    root: &'a Value,
}

impl Renderer<'_> {
    fn value(&self, output: &mut String, value: &Value, schema: &Value, depth: usize, top: bool) {
        match value {
            Value::Object(object) => self.object(output, object, schema, depth, top),
            Value::Array(items) => self.array(output, items, schema, depth),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
                output.push_str(&value.to_string());
            }
        }
    }

    fn object(
        &self,
        output: &mut String,
        object: &Map<String, Value>,
        schema: &Value,
        depth: usize,
        top: bool,
    ) {
        if object.is_empty() {
            output.push_str("{}");
            return;
        }
        let schema = self.resolve(schema);
        output.push_str("{\n");
        for (index, (key, value)) in object.iter().enumerate() {
            let property = schema.get("properties").and_then(|properties| properties.get(key));
            if let Some(description) = property
                .and_then(|property| property.get("description"))
                .and_then(Value::as_str)
            {
                write_comment(output, description, depth + 1);
            } else if top && *key == "$schema" {
                write_comment(output, "Schema for editor validation.", depth + 1);
            }
            output.push_str(&INDENT.repeat(depth + 1));
            output.push_str(&Value::String(key.clone()).to_string());
            output.push_str(": ");
            self.value(
                output,
                value,
                property.unwrap_or(&Value::Bool(true)),
                depth + 1,
                false,
            );
            if index + 1 < object.len() {
                output.push(',');
            }
            output.push('\n');
        }
        output.push_str(&INDENT.repeat(depth));
        output.push('}');
    }

    fn array(&self, output: &mut String, items: &[Value], schema: &Value, depth: usize) {
        if items.is_empty() {
            output.push_str("[]");
            return;
        }
        let schema = self.resolve(schema);
        let item_schema = schema.get("items").unwrap_or(&Value::Bool(true));
        output.push_str("[\n");
        for (index, item) in items.iter().enumerate() {
            output.push_str(&INDENT.repeat(depth + 1));
            self.value(output, item, item_schema, depth + 1, false);
            if index + 1 < items.len() {
                output.push(',');
            }
            output.push('\n');
        }
        output.push_str(&INDENT.repeat(depth));
        output.push(']');
    }

    fn resolve<'a>(&'a self, schema: &'a Value) -> &'a Value {
        let Some(reference) = schema.get("$ref").and_then(Value::as_str) else {
            return schema;
        };
        let Some(pointer) = reference.strip_prefix('#') else {
            return schema;
        };
        self.root.pointer(pointer).unwrap_or(schema)
    }
}

fn write_comment(output: &mut String, description: &str, depth: usize) {
    let indent = INDENT.repeat(depth);
    for line in description.lines() {
        let line = line.trim_end();
        output.push_str(&indent);
        if line.is_empty() {
            output.push_str("//\n");
        } else {
            output.push_str("// ");
            output.push_str(line);
            output.push('\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::module::ModuleId;

    #[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
    #[serde(default, rename_all = "camelCase")]
    struct Inner {
        /// A nested switch.
        flag: bool,
    }

    #[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
    #[serde(default, rename_all = "camelCase")]
    struct Sample {
        /// Whether the feature is enabled.
        enabled: bool,
        /// Ordered names.
        names: Vec<String>,
        /// Nested settings.
        inner: Inner,
    }

    fn file() -> ConfigFileId { ConfigFileId::module(ModuleId::new("sample")) }

    #[test]
    fn template_uses_default_values_and_schema_descriptions() {
        let value = serde_json::to_value(Sample::default()).expect("serialize default");
        let schema = schemars::schema_for!(Sample);
        let rendered = render(file(), &value, &schema);
        assert_eq!(
            rendered,
            r#"{
  // Schema for editor validation.
  "$schema": "https://raw.githubusercontent.com/marcosmoura/stache/main/schemas/sample.schema.json",
  // Whether the feature is enabled.
  "enabled": false,
  // Nested settings.
  "inner": {
    // A nested switch.
    "flag": false
  },
  // Ordered names.
  "names": []
}
"#
        );
    }

    #[test]
    fn non_empty_arrays_render_their_items() {
        let value = serde_json::json!({ "names": ["one", "two"] });
        let schema = schemars::schema_for!(Sample);
        let rendered = render(file(), &value, &schema);
        assert!(rendered.contains("\"names\": [\n    \"one\",\n    \"two\"\n  ]"));
    }
}
