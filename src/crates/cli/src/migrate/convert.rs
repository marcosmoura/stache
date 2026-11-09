//! Schema-driven conversion of legacy configuration spellings to `snake_case`.
//!
//! [`convert`] is a pure function over a JSON value and the generated JSON
//! Schema of the type that owns it. It walks both together, so it only touches
//! spellings the schema itself declares:
//!
//! * An object key is renamed when its `snake_case` form is a declared property
//!   at that location and the key itself is not. Undeclared keys and
//!   `additionalProperties` map keys (shortcuts, device names, ...) stay as
//!   written.
//! * A string is normalized only where the schema location is a string enum
//!   (`enum`, or `const` branches as schemars emits for documented unit
//!   variants) and the `snake_case` form is a member.
//!
//! Untagged unions (`anyOf`/`oneOf`) are handled as a union of their branches:
//! a key is declared when any branch declares it, and a string is normalized
//! when any branch is a string enum containing the `snake_case` form. The
//! converter never warns; unknown keys surface through the typed decode.

use std::fmt;

use schemars::Schema;
use serde_json::{Map, Value};

/// The deepest `$ref`/combinator nesting followed for one value.
const MAX_DEPTH: usize = 64;

/// A conversion failure with a user-facing reason.
#[derive(Debug, PartialEq, Eq)]
pub struct ConvertError(String);

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

impl std::error::Error for ConvertError {}

/// Converts `value` to the `snake_case` spellings declared by `root`.
///
/// # Errors
/// Fails when two keys at one location would become the same `snake_case` key,
/// for example `holdDuration` next to `hold_duration`. Nothing is dropped.
pub fn convert(value: &Value, root: &Schema) -> Result<Value, ConvertError> {
    let root = root.as_value();
    walk(value, &[root], root, "")
}

/// The `snake_case` form of a camelCase or kebab-case identifier.
fn to_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 2);
    let mut previous_lower = false;
    for ch in name.chars() {
        if ch == '-' {
            out.push('_');
            previous_lower = false;
        } else if ch.is_ascii_uppercase() {
            if previous_lower {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
            previous_lower = false;
        } else {
            out.push(ch);
            previous_lower = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        }
    }
    out
}

fn walk(
    value: &Value,
    schemas: &[&Value],
    root: &Value,
    path: &str,
) -> Result<Value, ConvertError> {
    let mut leaves = Vec::new();
    for schema in schemas {
        flatten(schema, root, 0, &mut leaves);
    }
    match value {
        Value::Object(object) => convert_object(object, &leaves, root, path),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                let mut children = Vec::new();
                for leaf in &leaves {
                    match leaf.get("prefixItems").and_then(Value::as_array) {
                        Some(prefix) if index < prefix.len() => children.push(&prefix[index]),
                        _ => {
                            if let Some(items) = leaf.get("items").filter(|v| v.is_object()) {
                                children.push(items);
                            }
                        }
                    }
                }
                out.push(walk(item, &children, root, &format!("{path}[{index}]"))?);
            }
            Ok(Value::Array(out))
        }
        Value::String(text) => Ok(Value::String(convert_string(text, &leaves))),
        other => Ok(other.clone()),
    }
}

/// Expands `$ref`, `allOf`, `anyOf` and `oneOf` into the concrete schemas a
/// value may match. Every returned leaf is a schema object without `$ref`.
fn flatten<'a>(schema: &'a Value, root: &'a Value, depth: usize, out: &mut Vec<&'a Value>) {
    let Some(object) = schema.as_object() else {
        return;
    };
    if depth > MAX_DEPTH {
        return;
    }
    if let Some(target) = object
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix('#'))
        .and_then(|pointer| root.pointer(pointer))
    {
        flatten(target, root, depth + 1, out);
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = object.get(keyword).and_then(Value::as_array) {
            for branch in branches {
                flatten(branch, root, depth + 1, out);
            }
        }
    }
    out.push(schema);
}

fn convert_object(
    object: &Map<String, Value>,
    leaves: &[&Value],
    root: &Value,
    path: &str,
) -> Result<Value, ConvertError> {
    let declared = |name: &str| leaves.iter().any(|leaf| property(leaf, name).is_some());
    let mut out = Map::with_capacity(object.len());
    let mut origin: Vec<(String, &str)> = Vec::with_capacity(object.len());
    for (key, child) in object {
        let snake = to_snake(key);
        let target = if snake != *key && !declared(key) && declared(&snake) {
            snake
        } else {
            key.clone()
        };
        if let Some((_, first)) = origin.iter().find(|(name, _)| *name == target) {
            let at = if path.is_empty() {
                String::new()
            } else {
                format!("{path}: ")
            };
            return Err(ConvertError(format!(
                "{at}`{first}` and `{key}` both set `{target}`; keep only one"
            )));
        }
        let mut children: Vec<&Value> =
            leaves.iter().filter_map(|leaf| property(leaf, &target)).collect();
        if children.is_empty() {
            children = leaves
                .iter()
                .filter_map(|leaf| leaf.get("additionalProperties"))
                .filter(|schema| schema.is_object())
                .collect();
        }
        let child_path = if path.is_empty() {
            target.clone()
        } else {
            format!("{path}.{target}")
        };
        out.insert(target.clone(), walk(child, &children, root, &child_path)?);
        origin.push((target, key));
    }
    Ok(Value::Object(out))
}

fn property<'a>(leaf: &'a Value, name: &str) -> Option<&'a Value> {
    leaf.get("properties").and_then(Value::as_object).and_then(|all| all.get(name))
}

fn convert_string(text: &str, leaves: &[&Value]) -> String {
    let members = |wanted: &str| {
        leaves.iter().any(|leaf| {
            leaf.get("const").and_then(Value::as_str) == Some(wanted)
                || leaf
                    .get("enum")
                    .and_then(Value::as_array)
                    .is_some_and(|all| all.iter().any(|item| item.as_str() == Some(wanted)))
        })
    };
    if members(text) {
        return text.to_owned();
    }
    let snake = to_snake(text);
    if snake != text && members(&snake) {
        snake
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use schemars::{JsonSchema, schema_for};
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use super::*;

    /// Untagged string | array, as `StartupCommands`.
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[serde(untagged)]
    enum Commands {
        One(String),
        Many(Vec<String>),
    }

    /// Untagged integer | string, as `DimensionValue`.
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[serde(untagged)]
    enum Dimension {
        Pixels(u32),
        Percent(String),
    }

    /// Untagged number | object, as `GapValue`.
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[serde(untagged)]
    enum Gap {
        All(u32),
        Sides(Sides),
    }

    #[derive(Serialize, Deserialize, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    struct Sides {
        top_side: u32,
    }

    /// Untagged `false` | object branches, as `BorderStateConfig`.
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[serde(untagged)]
    enum Border {
        Off(bool),
        Solid { width: u32, color: String },
        Glow { width: u32, glow_color: String },
    }

    /// A documented unit enum, which schemars emits as `oneOf` of `const`.
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    enum Easing {
        /// Linear.
        Linear,
        /// Slow start.
        EaseIn,
        /// Slow end.
        EaseOutExpo,
    }

    /// An undocumented unit enum, which schemars emits as `enum`.
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    enum Plain {
        OpenMeteo,
        Auto,
    }

    #[derive(Serialize, Deserialize, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    struct Inner {
        hold_duration: u64,
        easing: Easing,
    }

    #[derive(Serialize, Deserialize, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    struct Sample {
        exec_on_startup: Commands,
        default_layout: Plain,
        nested: Inner,
        list: Vec<Inner>,
        width: Dimension,
        gap: Gap,
        border: Border,
        maybe: Option<Inner>,
        bindings: BTreeMap<String, Commands>,
        modes: BTreeMap<String, Inner>,
        pair: (Easing, Plain),
    }

    fn run(value: &Value) -> Result<Value, ConvertError> { convert(value, &schema_for!(Sample)) }

    fn ok(value: &Value) -> Value { run(value).expect("converts") }

    #[test]
    fn to_snake_handles_camel_kebab_and_snake() {
        for (input, expected) in [
            ("holdDuration", "hold_duration"),
            ("preset-on-open", "preset_on_open"),
            ("ease-out-expo", "ease_out_expo"),
            ("rightClick", "right_click"),
            ("already_snake", "already_snake"),
            ("enabled", "enabled"),
            ("§", "§"),
        ] {
            assert_eq!(to_snake(input), expected);
        }
    }

    #[test]
    fn renames_nested_keys_through_refs_and_arrays() {
        let out = ok(&json!({
            "execOnStartup": "a",
            "nested": { "holdDuration": 1, "easing": "ease-in" },
            "list": [{ "holdDuration": 2, "easing": "linear" }, { "hold_duration": 3 }],
            "maybe": { "holdDuration": 4 }
        }));
        assert_eq!(
            out,
            json!({
                "exec_on_startup": "a",
                "nested": { "hold_duration": 1, "easing": "ease_in" },
                "list": [{ "hold_duration": 2, "easing": "linear" }, { "hold_duration": 3 }],
                "maybe": { "hold_duration": 4 }
            })
        );
    }

    #[test]
    fn untagged_unions_use_the_union_of_their_branches() {
        let out = ok(&json!({
            "execOnStartup": ["x", "y"],
            "width": "50%",
            "gap": { "topSide": 4 },
            "border": { "width": 2, "glowColor": "#fff" }
        }));
        assert_eq!(out["exec_on_startup"], json!(["x", "y"]));
        assert_eq!(out["width"], json!("50%"));
        assert_eq!(out["gap"], json!({ "top_side": 4 }));
        assert_eq!(out["border"], json!({ "width": 2, "glow_color": "#fff" }));
        for plain in [
            json!({ "gap": 3 }),
            json!({ "border": false }),
            json!({ "width": 10 }),
        ] {
            assert_eq!(ok(&plain), plain);
        }
    }

    #[test]
    fn string_enums_normalize_in_const_and_enum_forms() {
        let out = ok(&json!({
            "defaultLayout": "open-meteo",
            "nested": { "easing": "ease-out-expo" },
            "pair": ["ease-in", "openMeteo"]
        }));
        assert_eq!(out["default_layout"], "open_meteo");
        assert_eq!(out["nested"]["easing"], "ease_out_expo");
        assert_eq!(out["pair"], json!(["ease_in", "open_meteo"]));
        let value_with_no_snake_member = ok(&json!({ "nested": { "easing": "Bogus-value" } }));
        assert_eq!(value_with_no_snake_member["nested"]["easing"], "Bogus-value");
    }

    #[test]
    fn map_keys_and_user_strings_are_left_untouched() {
        let input = json!({
            "bindings": {
                "CapsLock+a": "open -a Terminal",
                "§": ["do-thing", "camelCase"],
                "enabledApps": "enabledApps"
            },
            "modes": { "myMode": { "holdDuration": 5, "easing": "ease-in" } },
            "execOnStartup": "some-command --flagValue"
        });
        let out = ok(&input);
        assert_eq!(out["bindings"], input["bindings"]);
        assert_eq!(
            out["modes"],
            json!({ "myMode": { "hold_duration": 5, "easing": "ease_in" } })
        );
        assert_eq!(out["exec_on_startup"], "some-command --flagValue");
    }

    #[test]
    fn unknown_keys_are_left_as_written() {
        let input = json!({ "futureKey": { "innerKey": 1 }, "nested": { "oddKey": [1] } });
        assert_eq!(ok(&input), input);
    }

    #[test]
    fn already_snake_input_is_unchanged_and_conversion_is_idempotent() {
        let input = json!({
            "exec_on_startup": ["a"],
            "default_layout": "open_meteo",
            "nested": { "hold_duration": 1, "easing": "ease_in" },
            "list": [{ "hold_duration": 2 }]
        });
        assert_eq!(ok(&input), input);
        let legacy = json!({ "nested": { "holdDuration": 1, "easing": "ease-in" } });
        let once = ok(&legacy);
        assert_eq!(ok(&once), once);
    }

    #[test]
    fn a_camel_and_snake_collision_fails_naming_both() {
        let error = run(&json!({ "nested": { "holdDuration": 1, "hold_duration": 2 } }))
            .expect_err("collision");
        let message = error.to_string();
        assert!(message.contains("nested"), "{message}");
        assert!(
            message.contains("holdDuration") && message.contains("hold_duration"),
            "{message}"
        );
    }

    #[test]
    fn non_object_roots_and_scalars_pass_through() {
        let schema = schema_for!(Sample);
        for value in [
            json!(null),
            json!(3),
            json!("holdDuration"),
            json!(true),
            json!([1]),
        ] {
            assert_eq!(convert(&value, &schema).expect("converts"), value);
        }
    }
}
