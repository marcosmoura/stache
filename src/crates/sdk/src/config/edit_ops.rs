//! Generic comment-preserving edits at a key path.
//!
//! One operation ([`ConfigEditOp`]) changes one item of a TOML file and returns
//! the complete replacement text. `toml_edit` locates the source span of the
//! touched item; only that span (or one inserted line or block) changes, so
//! comments, the `#:schema` line, key order, line endings, tables and arrays of
//! tables elsewhere stay byte for byte. Every planned text is re-parsed and
//! compared with the same operation applied to the file's JSON value, so an
//! edit that would not produce exactly the requested document is refused. The
//! caller validates the candidate through the catalog and writes it; this
//! module never touches disk.
//!
//! Insertion generalises the root `enabled` rule: a new key goes after the last
//! key of its table, before any sub-table header, in the file's line ending. An
//! object (or a list of objects) set under the root or a standard table that is
//! not inside an array of tables becomes a `[table]` (or `[[table]]`) block at
//! the end of the file; anywhere else values are written inline. `null` is never
//! written: setting `null` removes the key.

use std::fmt::Write as _;
use std::ops::Range;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};
use specta::Type;
use toml_edit::{Array, ArrayOfTables, Document, InlineTable, Item, Key, Table, Value};

use crate::config::edit::{
    ConfigEdit, ConfigEditError, Scan, insert_after_keys, insert_before_tables,
};

/// The wire shape of an arbitrary JSON value, used only to type
/// `serde_json::Value` fields in the generated client without 64-bit integers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
#[serde(untagged)]
#[allow(clippy::use_self)] // The derive names the recursive type explicitly.
pub enum JsonValue {
    /// `null`.
    Null,
    /// A boolean.
    Bool(bool),
    /// A number.
    Number(f64),
    /// A string.
    String(String),
    /// An array.
    Array(Vec<JsonValue>),
    /// An object.
    Object(std::collections::BTreeMap<String, JsonValue>),
}

/// One segment of a [`ConfigPath`]: a table key or an array index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(untagged)]
pub enum PathSegment {
    /// A table, inline table or map key.
    Key(String),
    /// An index into an array or array of tables.
    Index(u32),
}

/// A key path from the file root, for example `["workspaces", 1, "name"]`.
pub type ConfigPath = Vec<PathSegment>;

/// One surgical edit of a configuration document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum ConfigEditOp {
    /// Sets or inserts a value; `null` removes the key.
    Set {
        /// The target item.
        path: ConfigPath,
        /// The JSON value to write.
        #[specta(type = JsonValue)]
        value: Json,
    },
    /// Removes a key (reset to default) or an array element.
    Remove {
        /// The item to remove.
        path: ConfigPath,
    },
    /// Inserts an element into an array or array of tables.
    Insert {
        /// The array.
        path: ConfigPath,
        /// The position of the new element.
        index: u32,
        /// The new element.
        #[specta(type = JsonValue)]
        value: Json,
    },
    /// Moves an array element, keeping its comments with it.
    Move {
        /// The array.
        path: ConfigPath,
        /// The current position.
        from: u32,
        /// The new position.
        to: u32,
    },
    /// Renames a map key, keeping its value and comments.
    Rename {
        /// The map (table) that holds the key.
        path: ConfigPath,
        /// The current key.
        from: String,
        /// The new key; refused when it exists.
        to: String,
    },
}

impl ConfigEditOp {
    /// The path an operation addresses, used for diagnostics without a field.
    #[must_use]
    pub fn target(&self) -> ConfigPath {
        match self {
            Self::Set { path, .. } | Self::Remove { path } => path.clone(),
            Self::Insert { path, index, .. } => child(path, PathSegment::Index(*index)),
            Self::Move { path, to, .. } => child(path, PathSegment::Index(*to)),
            Self::Rename { path, to, .. } => child(path, PathSegment::Key(to.clone())),
        }
    }
}

/// The content hash of a file's exact bytes: FNV-1a 64-bit as 16 lowercase
/// hexadecimal digits. It detects change, not tampering.
#[must_use]
pub fn content_hash(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Renders a path as `a.b[1]."quoted key"`.
#[must_use]
pub fn display_path(path: &[PathSegment]) -> String {
    let mut out = String::new();
    for segment in path {
        match segment {
            PathSegment::Key(key) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(&Key::new(key.as_str()).to_string());
            }
            PathSegment::Index(index) => {
                let _ = write!(out, "[{index}]");
            }
        }
    }
    out
}

/// The parsed file as JSON: only the keys present in the text.
///
/// # Errors
/// Returns [`ConfigEditError::Parse`] for invalid TOML.
pub fn document_json(text: &str) -> Result<Json, ConfigEditError> {
    toml::from_str::<Json>(text).map_err(|error| {
        ConfigEditError::Parse(error.message().lines().next().unwrap_or("").to_owned())
    })
}

/// Applies one operation to source text, preserving every untouched byte.
///
/// # Errors
/// Returns [`ConfigEditError`] for invalid TOML, a path that crosses a
/// non-table, an out-of-range index, an existing rename target, a value TOML
/// cannot hold, or a candidate that does not round-trip to the requested
/// document.
pub fn apply_edit(text: &str, op: &ConfigEditOp) -> Result<ConfigEdit, ConfigEditError> {
    let before = document_json(text)?;
    // Removing the last key of a dotted table (`a.b = 1`) leaves no text for
    // `a`, so the removal covers the now-empty dotted parents in both models.
    let collapsed;
    let op = match op {
        ConfigEditOp::Remove { path } => {
            collapsed = ConfigEditOp::Remove {
                path: dotted_removal_path(text, path)?,
            };
            &collapsed
        }
        _ => op,
    };
    let mut expected = before.clone();
    apply_json(&mut expected, op)?;
    let edited = match op {
        ConfigEditOp::Set { path, value } => set(text, path, value)?,
        ConfigEditOp::Remove { path } => remove(text, path)?,
        ConfigEditOp::Insert { path, index, value } => insert(text, path, *index, value)?,
        ConfigEditOp::Move { path, from, to } => move_element(text, path, *from, *to)?,
        ConfigEditOp::Rename { path, from, to } => rename(text, path, from, to)?,
    };
    if edited == text {
        return if expected == before {
            Ok(ConfigEdit::Unchanged)
        } else {
            Err(ConfigEditError::Unverified)
        };
    }
    if document_json(&edited)? != expected {
        return Err(ConfigEditError::Unverified);
    }
    Ok(ConfigEdit::Changed(edited))
}

// ---------------------------------------------------------------------------
// JSON model: the document an operation must produce.
// ---------------------------------------------------------------------------

/// Applies an operation to a JSON document, the reference every text edit is
/// checked against.
///
/// # Errors
/// Returns the same path refusals as [`apply_edit`].
pub fn apply_json(document: &mut Json, op: &ConfigEditOp) -> Result<(), ConfigEditError> {
    match op {
        ConfigEditOp::Set { path, value } if value.is_null() => json_remove(document, path),
        ConfigEditOp::Set { path, value } => {
            check_representable(value)?;
            let (last, parent) = json_parent(document, path, true)?;
            match (parent, last) {
                (Json::Object(map), PathSegment::Key(key)) => {
                    map.insert(key.clone(), value.clone());
                    Ok(())
                }
                (Json::Array(items), PathSegment::Index(index)) => {
                    let len = items.len();
                    let slot = items
                        .get_mut(*index as usize)
                        .ok_or_else(|| out_of_range(path, *index, len))?;
                    *slot = value.clone();
                    Ok(())
                }
                _ => Err(ConfigEditError::NotATable { path: display_path(path) }),
            }
        }
        ConfigEditOp::Remove { path } => json_remove(document, path),
        ConfigEditOp::Insert { path, index, value } => {
            check_representable(value)?;
            if json_get(document, path)?.is_none() && *index == 0 && !path.is_empty() {
                let (last, parent) = json_parent(document, path, true)?;
                let (Json::Object(map), PathSegment::Key(key)) = (parent, last) else {
                    return Err(ConfigEditError::NotAnArray { path: display_path(path) });
                };
                map.insert(key.clone(), Json::Array(vec![value.clone()]));
                return Ok(());
            }
            let items = json_array(document, path)?;
            if *index as usize > items.len() {
                return Err(out_of_range(path, *index, items.len()));
            }
            items.insert(*index as usize, value.clone());
            Ok(())
        }
        ConfigEditOp::Move { path, from, to } => {
            let items = json_array(document, path)?;
            let len = items.len();
            for index in [from, to] {
                if *index as usize >= len {
                    return Err(out_of_range(path, *index, len));
                }
            }
            let item = items.remove(*from as usize);
            items.insert(*to as usize, item);
            Ok(())
        }
        ConfigEditOp::Rename { path, from, to } => {
            let Some(Json::Object(map)) = json_get_mut(document, path)? else {
                return Err(ConfigEditError::NotATable { path: display_path(path) });
            };
            if map.contains_key(to) {
                return Err(ConfigEditError::KeyExists {
                    path: display_path(&child(path, PathSegment::Key(to.clone()))),
                });
            }
            let value = map.remove(from).ok_or_else(|| ConfigEditError::PathNotFound {
                path: display_path(&child(path, PathSegment::Key(from.clone()))),
            })?;
            map.insert(to.clone(), value);
            Ok(())
        }
    }
}

fn check_representable(value: &Json) -> Result<(), ConfigEditError> { to_toml(value).map(|_| ()) }

fn json_remove(document: &mut Json, path: &[PathSegment]) -> Result<(), ConfigEditError> {
    let Some((last, parent_path)) = path.split_last() else {
        return Err(ConfigEditError::PathNotFound { path: String::new() });
    };
    match (json_get_mut(document, parent_path)?, last) {
        (None, _) => Ok(()),
        (Some(Json::Object(map)), PathSegment::Key(key)) => {
            map.remove(key);
            Ok(())
        }
        (Some(Json::Array(items)), PathSegment::Index(index)) => {
            if *index as usize >= items.len() {
                return Err(out_of_range(path, *index, items.len()));
            }
            items.remove(*index as usize);
            Ok(())
        }
        _ => Err(ConfigEditError::NotATable { path: display_path(path) }),
    }
}

fn json_array<'a>(
    document: &'a mut Json,
    path: &[PathSegment],
) -> Result<&'a mut Vec<Json>, ConfigEditError> {
    match json_get_mut(document, path)? {
        Some(Json::Array(items)) => Ok(items),
        Some(_) => Err(ConfigEditError::NotAnArray { path: display_path(path) }),
        None => Err(ConfigEditError::PathNotFound { path: display_path(path) }),
    }
}

fn json_get<'a>(
    document: &'a Json,
    path: &[PathSegment],
) -> Result<Option<&'a Json>, ConfigEditError> {
    let mut node = document;
    for (depth, segment) in path.iter().enumerate() {
        node = match (node, segment) {
            (Json::Object(map), PathSegment::Key(key)) => match map.get(key) {
                Some(next) => next,
                None => return Ok(None),
            },
            (Json::Array(items), PathSegment::Index(index)) => match items.get(*index as usize) {
                Some(next) => next,
                None => return Err(out_of_range(&path[..=depth], *index, items.len())),
            },
            _ => {
                return Err(ConfigEditError::NotATable {
                    path: display_path(&path[..=depth]),
                });
            }
        };
    }
    Ok(Some(node))
}

fn json_get_mut<'a>(
    document: &'a mut Json,
    path: &[PathSegment],
) -> Result<Option<&'a mut Json>, ConfigEditError> {
    // Validate first with the shared reader, then walk mutably.
    if json_get(document, path)?.is_none() {
        return Ok(None);
    }
    let mut node = document;
    for segment in path {
        node = match (node, segment) {
            (Json::Object(map), PathSegment::Key(key)) => {
                map.get_mut(key).ok_or(ConfigEditError::Unverified)?
            }
            (Json::Array(items), PathSegment::Index(index)) => {
                items.get_mut(*index as usize).ok_or(ConfigEditError::Unverified)?
            }
            _ => return Err(ConfigEditError::Unverified),
        };
    }
    Ok(Some(node))
}

/// The parent container of `path`, creating missing tables when `create`.
fn json_parent<'a, 'p>(
    document: &'a mut Json,
    path: &'p [PathSegment],
    create: bool,
) -> Result<(&'p PathSegment, &'a mut Json), ConfigEditError> {
    let Some((last, parents)) = path.split_last() else {
        return Err(ConfigEditError::PathNotFound { path: String::new() });
    };
    let mut node = document;
    for (depth, segment) in parents.iter().enumerate() {
        node = match (node, segment) {
            (Json::Object(map), PathSegment::Key(key)) => {
                if !map.contains_key(key) {
                    if !create {
                        return Err(ConfigEditError::PathNotFound {
                            path: display_path(&path[..=depth]),
                        });
                    }
                    map.insert(key.clone(), Json::Object(Map::new()));
                }
                map.get_mut(key).ok_or(ConfigEditError::Unverified)?
            }
            (Json::Array(items), PathSegment::Index(index)) => {
                let len = items.len();
                items
                    .get_mut(*index as usize)
                    .ok_or_else(|| out_of_range(&path[..=depth], *index, len))?
            }
            _ => {
                return Err(ConfigEditError::NotATable {
                    path: display_path(&path[..=depth]),
                });
            }
        };
    }
    Ok((last, node))
}

fn out_of_range(path: &[PathSegment], index: u32, len: usize) -> ConfigEditError {
    ConfigEditError::IndexOutOfRange {
        path: display_path(path),
        index,
        len,
    }
}

fn child(path: &[PathSegment], segment: PathSegment) -> ConfigPath {
    let mut next = path.to_vec();
    next.push(segment);
    next
}

// ---------------------------------------------------------------------------
// Rendering JSON as TOML text.
// ---------------------------------------------------------------------------

fn to_toml(value: &Json) -> Result<Value, ConfigEditError> {
    Ok(match value {
        Json::Null => {
            return Err(ConfigEditError::Unrepresentable(
                "TOML has no null value".to_owned(),
            ));
        }
        Json::Bool(flag) => Value::from(*flag),
        Json::Number(number) => {
            if let Some(integer) = number.as_i64() {
                Value::from(integer)
            } else if number.is_u64() {
                return Err(ConfigEditError::Unrepresentable(format!(
                    "{number} is larger than a TOML integer"
                )));
            } else {
                Value::from(number.as_f64().unwrap_or_default())
            }
        }
        Json::String(text) => Value::from(text.as_str()),
        Json::Array(items) => {
            let mut array = Array::new();
            for item in items {
                array.push(to_toml(item)?);
            }
            Value::Array(array)
        }
        Json::Object(map) => {
            let mut table = InlineTable::new();
            for (key, item) in map {
                table.insert(key.as_str(), to_toml(item)?);
            }
            Value::InlineTable(table)
        }
    })
}

fn render(value: &Json) -> Result<String, ConfigEditError> { Ok(to_toml(value)?.to_string()) }

fn render_key(key: &str) -> String { Key::new(key).to_string() }

fn render_keys(path: &[PathSegment]) -> String {
    path.iter()
        .filter_map(|segment| match segment {
            PathSegment::Key(key) => Some(render_key(key)),
            PathSegment::Index(_) => None,
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn newline(text: &str) -> &'static str { if text.contains("\r\n") { "\r\n" } else { "\n" } }

/// Whether a value becomes a `[table]` or `[[table]]` block.
fn block_kind(value: &Json) -> Option<bool> {
    match value {
        Json::Object(_) => Some(false),
        Json::Array(items) if !items.is_empty() && items.iter().all(Json::is_object) => Some(true),
        _ => None,
    }
}

/// Renders a `[path]` or `[[path]]` block (one per element) with inline values.
fn render_block(path: &[PathSegment], value: &Json, nl: &str) -> Result<String, ConfigEditError> {
    let header = render_keys(path);
    let mut out = String::new();
    let tables: Vec<(&Map<String, Json>, bool)> = match value {
        Json::Object(map) => vec![(map, false)],
        Json::Array(items) => {
            items.iter().filter_map(Json::as_object).map(|map| (map, true)).collect()
        }
        _ => Vec::new(),
    };
    for (position, (map, array)) in tables.iter().enumerate() {
        if position > 0 {
            out.push_str(nl);
        }
        if *array {
            let _ = write!(out, "[[{header}]]{nl}");
        } else {
            let _ = write!(out, "[{header}]{nl}");
        }
        for (key, item) in *map {
            if item.is_null() {
                continue;
            }
            let _ = write!(out, "{} = {}{nl}", render_key(key), render(item)?);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Navigation over the parsed document.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Node<'a> {
    Table(&'a Table),
    Inline(&'a InlineTable),
    Array(&'a Array),
    Aot(&'a ArrayOfTables),
    Value(&'a Value),
}

const fn item_node(item: &Item) -> Option<Node<'_>> {
    match item {
        Item::Table(table) => Some(Node::Table(table)),
        Item::ArrayOfTables(array) => Some(Node::Aot(array)),
        Item::Value(value) => Some(value_node(value)),
        Item::None => None,
    }
}

const fn value_node(value: &Value) -> Node<'_> {
    match value {
        Value::Array(array) => Node::Array(array),
        Value::InlineTable(table) => Node::Inline(table),
        other => Node::Value(other),
    }
}

impl Node<'_> {
    fn value_span(self) -> Option<Range<usize>> {
        match self {
            Self::Inline(table) => table.span(),
            Self::Array(array) => array.span(),
            Self::Value(value) => value.span(),
            Self::Table(_) | Self::Aot(_) => None,
        }
    }

    /// The child at one segment; `Ok(None)` when absent.
    fn child(self, segment: &PathSegment) -> Result<Option<Self>, ()> {
        match (self, segment) {
            (Self::Table(table), PathSegment::Key(key)) => Ok(table.get(key).and_then(item_node)),
            (Self::Inline(table), PathSegment::Key(key)) => Ok(table.get(key).map(value_node)),
            (Self::Array(array), PathSegment::Index(index)) => {
                Ok(array.get(*index as usize).map(value_node))
            }
            (Self::Aot(array), PathSegment::Index(index)) => {
                Ok(array.get(*index as usize).map(Self::Table))
            }
            _ => Err(()),
        }
    }

    fn len(self) -> Option<usize> {
        match self {
            Self::Array(array) => Some(array.len()),
            Self::Aot(array) => Some(array.len()),
            _ => None,
        }
    }
}

/// Walks a path, returning every existing node from the root. The result is
/// shorter than `path + 1` when the path leaves the document.
fn walk<'a>(root: &'a Table, path: &[PathSegment]) -> Result<Vec<Node<'a>>, ConfigEditError> {
    let mut nodes = vec![Node::Table(root)];
    for (depth, segment) in path.iter().enumerate() {
        let current = nodes[depth];
        match current.child(segment) {
            Ok(Some(next)) => nodes.push(next),
            Ok(None) => {
                if let (PathSegment::Index(index), Some(len)) = (segment, current.len()) {
                    return Err(out_of_range(&path[..=depth], *index, len));
                }
                break;
            }
            Err(()) => {
                return Err(ConfigEditError::NotATable {
                    path: display_path(&path[..=depth]),
                });
            }
        }
    }
    Ok(nodes)
}

fn parse(text: &str) -> Result<Document<&str>, ConfigEditError> {
    Document::parse(text).map_err(|error| {
        ConfigEditError::Parse(error.message().lines().next().unwrap_or("").to_owned())
    })
}

fn missing_span() -> ConfigEditError {
    ConfigEditError::Parse("an item has no source span".to_owned())
}

// ---------------------------------------------------------------------------
// Line helpers.
// ---------------------------------------------------------------------------

fn line_start(text: &str, at: usize) -> usize {
    text[..at].rfind('\n').map_or(0, |offset| offset + 1)
}

fn line_end(text: &str, at: usize) -> usize {
    text[at..].find('\n').map_or(text.len(), |offset| at + offset + 1)
}

/// The start of the comment lines directly above the line holding `at`. A
/// blank line or `#:schema` ends the block.
fn attached_start(text: &str, at: usize) -> usize {
    let mut start = line_start(text, at);
    while start > 0 {
        let previous = line_start(text, start - 1);
        let line = text[previous..start].trim();
        if line.starts_with('#') && !line.starts_with("#:schema") {
            start = previous;
        } else {
            break;
        }
    }
    start
}

/// Moves `end` back over blank lines, never before `start`.
fn trim_blank_tail(text: &str, start: usize, mut end: usize) -> usize {
    while end > start {
        let previous = line_start(text, end - 1);
        if previous < start || !text[previous..end].trim().is_empty() {
            break;
        }
        end = previous;
    }
    end
}

fn splice(text: &str, range: Range<usize>, with: &str) -> String {
    let mut out = String::with_capacity(text.len() + with.len());
    out.push_str(&text[..range.start]);
    out.push_str(with);
    out.push_str(&text[range.end..]);
    out
}

fn remove_ranges(text: &str, mut ranges: Vec<Range<usize>>) -> String {
    ranges.sort_by_key(|range| range.start);
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for range in ranges {
        if range.end <= cursor {
            continue;
        }
        out.push_str(&text[cursor..range.start.max(cursor)]);
        cursor = range.end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// Every explicit header start (`[t]` and `[[t]]`) in the document.
fn headers(root: &Table) -> Vec<usize> {
    let mut starts = Vec::new();
    collect_headers(root, &mut starts);
    starts.sort_unstable();
    starts
}

fn collect_headers(table: &Table, starts: &mut Vec<usize>) {
    for (_, item) in table {
        match item {
            Item::Table(child) => {
                if is_explicit(child)
                    && let Some(span) = child.span()
                {
                    starts.push(span.start);
                }
                collect_headers(child, starts);
            }
            Item::ArrayOfTables(array) => {
                for child in array {
                    if let Some(span) = child.span() {
                        starts.push(span.start);
                    }
                    collect_headers(child, starts);
                }
            }
            Item::Value(_) | Item::None => {}
        }
    }
}

fn is_explicit(table: &Table) -> bool { !table.is_implicit() && !table.is_dotted() }

/// The byte ranges that hold a table and everything under it.
fn table_ranges(
    text: &str,
    all: &[usize],
    table: &Table,
    explicit_header: bool,
    out: &mut Vec<Range<usize>>,
) {
    if explicit_header && let Some(span) = table.span() {
        let end = all
            .iter()
            .find(|start| **start > span.start)
            .map_or(text.len(), |next| attached_start(text, *next));
        out.push(line_start(text, span.start)..end);
    }
    for (_, item) in table {
        item_ranges(text, all, item, out);
    }
}

fn item_ranges(text: &str, all: &[usize], item: &Item, out: &mut Vec<Range<usize>>) {
    match item {
        Item::Value(value) => {
            if let Some(span) = value.span() {
                out.push(line_start(text, span.start)..line_end(text, span.end));
            }
        }
        Item::Table(table) => table_ranges(text, all, table, is_explicit(table), out),
        Item::ArrayOfTables(array) => {
            for table in array {
                table_ranges(text, all, table, true, out);
            }
        }
        Item::None => {}
    }
}

/// The region of one array-of-tables element: its attached comments, header,
/// body and sub-tables, without trailing blank lines.
fn element_region(
    text: &str,
    all: &[usize],
    table: &Table,
) -> Result<Range<usize>, ConfigEditError> {
    let header = table.span().ok_or_else(missing_span)?.start;
    let mut own = Vec::new();
    collect_headers(table, &mut own);
    let end = all
        .iter()
        .find(|start| **start > header && !own.contains(start))
        .map_or(text.len(), |next| attached_start(text, *next));
    let start = attached_start(text, header);
    Ok(start..trim_blank_tail(text, start, end))
}

// ---------------------------------------------------------------------------
// set
// ---------------------------------------------------------------------------

fn set(text: &str, path: &[PathSegment], value: &Json) -> Result<String, ConfigEditError> {
    if value.is_null() {
        return remove(text, path);
    }
    if path.is_empty() {
        return Err(ConfigEditError::NotATable { path: String::new() });
    }
    let document = parse(text)?;
    let nodes = walk(document.as_table(), path)?;
    if nodes.len() == path.len() + 1 {
        let target = nodes[path.len()];
        return match (target, value) {
            (Node::Table(table), Json::Object(map)) => {
                let existing: Vec<String> = table.iter().map(|(key, _)| key.to_owned()).collect();
                set_members(text, path, map, &existing)
            }
            (Node::Aot(array), Json::Array(items))
                if items.iter().all(Json::is_object) && !items.is_empty() =>
            {
                let len = array.len();
                let mut edited = text.to_owned();
                for (index, item) in items.iter().enumerate().take(len) {
                    edited =
                        set(&edited, &child(path, PathSegment::Index(index_u32(index))), item)?;
                }
                for index in (items.len()..len).rev() {
                    edited = remove(&edited, &child(path, PathSegment::Index(index_u32(index))))?;
                }
                for (index, item) in items.iter().enumerate().skip(len) {
                    edited = insert(&edited, path, index_u32(index), item)?;
                }
                Ok(edited)
            }
            (Node::Table(_) | Node::Aot(_), _) => {
                let removed = remove(text, path)?;
                set(&removed, path, value)
            }
            (node, _) => {
                let span = node.value_span().ok_or_else(missing_span)?;
                Ok(splice(text, span, &render(value)?))
            }
        };
    }
    // The path leaves the document: add the first missing key to the deepest
    // existing container, wrapping the value in the remaining keys.
    let depth = nodes.len() - 1;
    let PathSegment::Key(key) = &path[depth] else {
        return Err(ConfigEditError::PathNotFound {
            path: display_path(&path[..=depth]),
        });
    };
    let mut nested = value.clone();
    for segment in path[depth + 1..].iter().rev() {
        let PathSegment::Key(name) = segment else {
            return Err(ConfigEditError::PathNotFound { path: display_path(path) });
        };
        let mut map = Map::new();
        map.insert(name.clone(), nested);
        nested = Json::Object(map);
    }
    insert_key(text, &document, &path[..depth], &nodes, key, &nested)
}

fn index_u32(index: usize) -> u32 { u32::try_from(index).unwrap_or(u32::MAX) }

fn set_members(
    text: &str,
    path: &[PathSegment],
    map: &Map<String, Json>,
    existing: &[String],
) -> Result<String, ConfigEditError> {
    let mut edited = text.to_owned();
    for (key, item) in map {
        edited = set(&edited, &child(path, PathSegment::Key(key.clone())), item)?;
    }
    for key in existing {
        if !map.contains_key(key) {
            edited = remove(&edited, &child(path, PathSegment::Key(key.clone())))?;
        }
    }
    Ok(edited)
}

/// Inserts `key = value` into the container at `parent` (which exists).
fn insert_key(
    text: &str,
    document: &Document<&str>,
    parent: &[PathSegment],
    nodes: &[Node<'_>],
    key: &str,
    value: &Json,
) -> Result<String, ConfigEditError> {
    let nl = newline(text);
    let container = nodes[parent.len()];
    match container {
        Node::Inline(table) => {
            let span = table.span().ok_or_else(missing_span)?;
            let entry = format!("{} = {}", render_key(key), render(value)?);
            let last = table.iter().filter_map(|(_, item)| item.span()).map(|span| span.end).max();
            Ok(last.map_or_else(
                || splice(text, span.start + 1..span.end - 1, &format!(" {entry} ")),
                |end| splice(text, end..end, &format!(", {entry}")),
            ))
        }
        Node::Table(table) => {
            let in_array = parent.iter().any(|segment| matches!(segment, PathSegment::Index(_)));
            let full = child(parent, PathSegment::Key(key.to_owned()));
            if !in_array && !table.is_dotted() && block_kind(value).is_some() {
                return Ok(append_block(text, &render_block(&full, value, nl)?, nl));
            }
            // A dotted or implicit table has no body of its own: write a
            // dotted key into its nearest explicit ancestor (or the root).
            let mut anchor = parent.len();
            while anchor > 0 {
                match nodes[anchor] {
                    Node::Table(table) if is_explicit(table) => break,
                    _ => anchor -= 1,
                }
            }
            if anchor < parent.len()
                && !in_array
                && let Node::Table(table) = container
                && table.is_implicit()
                && !table.is_dotted()
            {
                // `[a.b]` exists but `[a]` does not: open `[a]` at the end.
                let line = format!("{} = {}{nl}", render_key(key), render(value)?);
                return Ok(append_block(
                    text,
                    &format!("[{}]{nl}{line}", render_keys(parent)),
                    nl,
                ));
            }
            let mut dotted: Vec<String> = parent[anchor..]
                .iter()
                .filter_map(|segment| match segment {
                    PathSegment::Key(name) => Some(render_key(name)),
                    PathSegment::Index(_) => None,
                })
                .collect();
            dotted.push(render_key(key));
            let line = format!("{} = {}{nl}", dotted.join("."), render(value)?);
            let Node::Table(anchor_table) = nodes[anchor] else {
                return Err(ConfigEditError::NotATable { path: display_path(parent) });
            };
            if anchor == 0 {
                Ok(insert_root_line(text, document.as_table(), &line, nl))
            } else {
                insert_table_line(text, anchor_table, &line, nl)
            }
        }
        _ => Err(ConfigEditError::NotATable { path: display_path(parent) }),
    }
}

/// The root insertion rule shared with `set_enabled`.
pub(crate) fn insert_root_line(text: &str, root: &Table, line: &str, nl: &str) -> String {
    let mut scan = Scan::default();
    for (_, item) in root {
        scan.visit(item);
    }
    scan.last_value_end.map_or_else(
        || insert_before_tables(text, scan.first_table_start, line, nl),
        |end| insert_after_keys(text, end, line, nl),
    )
}

/// Inserts a line after the last key of an explicit table, or right after its
/// header when it has no keys.
fn insert_table_line(
    text: &str,
    table: &Table,
    line: &str,
    nl: &str,
) -> Result<String, ConfigEditError> {
    let mut scan = Scan::default();
    for (_, item) in table {
        if let Item::Value(_) = item {
            scan.visit(item);
        } else if let Item::Table(child) = item
            && child.is_dotted()
        {
            scan.visit(item);
        }
    }
    let end = match scan.last_value_end {
        Some(end) => end,
        None => table.span().ok_or_else(missing_span)?.end,
    };
    Ok(insert_after_keys(text, end, line, nl))
}

/// Appends a block at the end of the file, separated by one blank line.
fn append_block(text: &str, block: &str, nl: &str) -> String {
    let mut out = text.to_owned();
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push_str(nl);
        }
        out.push_str(nl);
    }
    out.push_str(block);
    out
}

// ---------------------------------------------------------------------------
// remove
// ---------------------------------------------------------------------------

fn remove(text: &str, path: &[PathSegment]) -> Result<String, ConfigEditError> {
    let Some((last, parent)) = path.split_last() else {
        return Err(ConfigEditError::PathNotFound { path: String::new() });
    };
    let document = parse(text)?;
    let nodes = walk(document.as_table(), path)?;
    if nodes.len() < path.len() + 1 {
        return Ok(text.to_owned());
    }
    let all = headers(document.as_table());
    match (nodes[parent.len()], last) {
        (Node::Table(table), PathSegment::Key(key)) => {
            let item = table.get(key).ok_or_else(missing_span)?;
            let mut ranges = Vec::new();
            item_ranges(text, &all, item, &mut ranges);
            Ok(remove_ranges(text, ranges))
        }
        (Node::Aot(array), PathSegment::Index(index)) => {
            let table = array.get(*index as usize).ok_or_else(missing_span)?;
            let mut ranges = Vec::new();
            table_ranges(text, &all, table, true, &mut ranges);
            Ok(remove_ranges(text, ranges))
        }
        (Node::Array(array), PathSegment::Index(index)) => {
            let spans: Vec<Range<usize>> = array.iter().filter_map(Value::span).collect();
            let outer = array.span().ok_or_else(missing_span)?;
            Ok(remove_element(text, &spans, *index as usize, &outer))
        }
        (Node::Inline(table), PathSegment::Key(key)) => {
            let mut starts = Vec::new();
            let mut ends = Vec::new();
            let mut position = 0;
            for (index, (name, value)) in table.iter().enumerate() {
                let key_span = table.key(name).and_then(Key::span).ok_or_else(missing_span)?;
                starts.push(key_span.start);
                ends.push(value.span().ok_or_else(missing_span)?.end);
                if name == key {
                    position = index;
                }
            }
            let spans: Vec<Range<usize>> =
                starts.into_iter().zip(ends).map(|(start, end)| start..end).collect();
            let outer = table.span().ok_or_else(missing_span)?;
            Ok(remove_element(text, &spans, position, &outer))
        }
        _ => Err(ConfigEditError::NotATable { path: display_path(path) }),
    }
}

/// The path a removal actually covers: `path` itself, or its nearest ancestor
/// that is not a dotted table whose only entry leads to `path`.
fn dotted_removal_path(
    text: &str,
    path: &[PathSegment],
) -> Result<Vec<PathSegment>, ConfigEditError> {
    let document = parse(text)?;
    let nodes = walk(document.as_table(), path)?;
    let mut end = path.len();
    if nodes.len() < end + 1 {
        return Ok(path.to_vec());
    }
    while end > 1 {
        match nodes[end - 1] {
            Node::Table(table) if table.is_dotted() && table.len() == 1 => end -= 1,
            _ => break,
        }
    }
    Ok(path[..end].to_vec())
}

/// Removes one comma-separated element from an inline array or table.
fn remove_element(
    text: &str,
    spans: &[Range<usize>],
    index: usize,
    outer: &Range<usize>,
) -> String {
    if spans.len() == 1 {
        return splice(text, outer.start + 1..outer.end - 1, "");
    }
    if index + 1 < spans.len() {
        splice(text, spans[index].start..spans[index + 1].start, "")
    } else {
        splice(text, spans[index - 1].end..spans[index].end, "")
    }
}

// ---------------------------------------------------------------------------
// insert
// ---------------------------------------------------------------------------

fn insert(
    text: &str,
    path: &[PathSegment],
    index: u32,
    value: &Json,
) -> Result<String, ConfigEditError> {
    let document = parse(text)?;
    let nodes = walk(document.as_table(), path)?;
    if nodes.len() < path.len() + 1 {
        if index != 0 {
            return Err(out_of_range(path, index, 0));
        }
        return set(text, path, &Json::Array(vec![value.clone()]));
    }
    let at = index as usize;
    match nodes[path.len()] {
        Node::Array(array) => {
            let spans: Vec<Range<usize>> = array.iter().filter_map(Value::span).collect();
            if at > spans.len() {
                return Err(out_of_range(path, index, spans.len()));
            }
            let rendered = render(value)?;
            let outer = array.span().ok_or_else(missing_span)?;
            if spans.is_empty() {
                return Ok(splice(text, outer.start + 1..outer.end - 1, &rendered));
            }
            if at < spans.len() {
                let gap_start = if at == 0 {
                    outer.start + 1
                } else {
                    comma_after(text, spans[at - 1].end)
                };
                let gap = &text[gap_start..spans[at].start];
                let separator = separator(gap);
                let start = spans[at].start;
                return Ok(splice(text, start..start, &format!("{rendered},{separator}")));
            }
            let last = spans[spans.len() - 1].clone();
            let gap_start = if spans.len() == 1 {
                outer.start + 1
            } else {
                comma_after(text, spans[spans.len() - 2].end)
            };
            let separator = separator(&text[gap_start..last.start]);
            Ok(splice(
                text,
                last.end..last.end,
                &format!(",{separator}{rendered}"),
            ))
        }
        Node::Aot(array) => {
            if at > array.len() {
                return Err(out_of_range(path, index, array.len()));
            }
            if !value.is_object() {
                return Err(ConfigEditError::Unrepresentable(
                    "an array of tables holds only tables".to_owned(),
                ));
            }
            let nl = newline(text);
            let block = render_block(path, &Json::Array(vec![value.clone()]), nl)?;
            let all = headers(document.as_table());
            if at < array.len() {
                let table = array.get(at).ok_or_else(missing_span)?;
                let start = element_region(text, &all, table)?.start;
                return Ok(splice(text, start..start, &format!("{block}{nl}")));
            }
            let table = array.get(array.len() - 1).ok_or_else(missing_span)?;
            let end = element_region(text, &all, table)?.end;
            let lead = if text[..end].ends_with('\n') { "" } else { nl };
            Ok(splice(text, end..end, &format!("{lead}{nl}{block}")))
        }
        _ => Err(ConfigEditError::NotAnArray { path: display_path(path) }),
    }
}

/// The offset just after the comma that follows an element ending at `end`.
fn comma_after(text: &str, end: usize) -> usize {
    text[end..].find(',').map_or(end, |offset| end + offset + 1)
}

/// The whitespace to put after a new element's comma: the line break and
/// indentation of a multi-line array, or one space.
fn separator(gap: &str) -> String {
    gap.rfind('\n').map_or_else(
        || " ".to_owned(),
        |offset| {
            let indent = &gap[offset + 1..];
            let line_break = if gap[..offset].ends_with('\r') {
                "\r\n"
            } else {
                "\n"
            };
            format!("{line_break}{indent}")
        },
    )
}

// ---------------------------------------------------------------------------
// move
// ---------------------------------------------------------------------------

fn move_element(
    text: &str,
    path: &[PathSegment],
    from: u32,
    to: u32,
) -> Result<String, ConfigEditError> {
    let document = parse(text)?;
    let nodes = walk(document.as_table(), path)?;
    if nodes.len() < path.len() + 1 {
        return Err(ConfigEditError::PathNotFound { path: display_path(path) });
    }
    let node = nodes[path.len()];
    let len = node
        .len()
        .ok_or_else(|| ConfigEditError::NotAnArray { path: display_path(path) })?;
    for index in [from, to] {
        if index as usize >= len {
            return Err(out_of_range(path, index, len));
        }
    }
    if from == to {
        return Ok(text.to_owned());
    }
    let nl = newline(text);
    let slots: Vec<(Range<usize>, String)> = match node {
        Node::Aot(array) => {
            let all = headers(document.as_table());
            let mut slots = Vec::new();
            for table in array {
                let region = element_region(text, &all, table)?;
                let content = text[region.clone()].to_owned();
                slots.push((region, content));
            }
            slots
        }
        Node::Array(array) => {
            let spans: Vec<Range<usize>> = array.iter().filter_map(Value::span).collect();
            line_slots(text, &spans).unwrap_or_else(|| {
                spans.iter().map(|span| (span.clone(), text[span.clone()].to_owned())).collect()
            })
        }
        _ => return Err(ConfigEditError::NotAnArray { path: display_path(path) }),
    };
    let mut contents: Vec<String> = slots.iter().map(|(_, content)| content.clone()).collect();
    let moved = contents.remove(from as usize);
    contents.insert(to as usize, moved);
    let mut out = text.to_owned();
    for ((range, original), content) in slots.iter().zip(contents).rev() {
        let mut content = content;
        let had_newline = original.ends_with('\n');
        let trimmed = content.trim_end_matches(['\n', '\r']).len();
        content.truncate(trimmed);
        if had_newline {
            content.push_str(nl);
        }
        out = splice(&out, range.clone(), &content);
    }
    Ok(out)
}

/// Whole-line slots for a multi-line array whose elements each start their
/// own line, so comments travel with their element. `None` when elements share
/// a line.
fn line_slots(text: &str, spans: &[Range<usize>]) -> Option<Vec<(Range<usize>, String)>> {
    let mut slots = Vec::new();
    let mut previous_end = 0;
    for span in spans {
        let start = line_start(text, span.start);
        if start < previous_end || !text[start..span.start].trim().is_empty() {
            return None;
        }
        let start = attached_start(text, span.start).max(previous_end);
        let after = &text[span.end..];
        let rest_of_line = &after[..after.find('\n').unwrap_or(after.len())];
        let tail = rest_of_line.trim_start();
        let (has_comma, tail) =
            tail.strip_prefix(',').map_or((false, tail), |rest| (true, rest.trim_start()));
        if !tail.is_empty() && !tail.starts_with('#') {
            return None;
        }
        let end = line_end(text, span.end);
        if end == text.len() && !text.ends_with('\n') {
            return None;
        }
        let mut content = text[start..end].to_owned();
        if !has_comma {
            content.insert(span.end - start, ',');
        }
        slots.push((start..end, content));
        previous_end = end;
    }
    Some(slots)
}

// ---------------------------------------------------------------------------
// rename
// ---------------------------------------------------------------------------

fn rename(
    text: &str,
    path: &[PathSegment],
    from: &str,
    to: &str,
) -> Result<String, ConfigEditError> {
    let document = parse(text)?;
    let nodes = walk(document.as_table(), path)?;
    let from_path = child(path, PathSegment::Key(from.to_owned()));
    if nodes.len() < path.len() + 1 {
        return Err(ConfigEditError::PathNotFound { path: display_path(&from_path) });
    }
    let new_key = render_key(to);
    match nodes[path.len()] {
        Node::Inline(table) => {
            if table.contains_key(to) {
                return Err(ConfigEditError::KeyExists {
                    path: display_path(&child(path, PathSegment::Key(to.to_owned()))),
                });
            }
            let span = table
                .key(from)
                .and_then(Key::span)
                .ok_or_else(|| ConfigEditError::PathNotFound { path: display_path(&from_path) })?;
            Ok(splice(text, span, &new_key))
        }
        Node::Table(table) => {
            if table.contains_key(to) {
                return Err(ConfigEditError::KeyExists {
                    path: display_path(&child(path, PathSegment::Key(to.to_owned()))),
                });
            }
            let item = table
                .get(from)
                .ok_or_else(|| ConfigEditError::PathNotFound { path: display_path(&from_path) })?;
            match item {
                Item::Value(_) => {
                    let span = table.key(from).and_then(Key::span).ok_or_else(missing_span)?;
                    Ok(splice(text, span, &new_key))
                }
                Item::Table(inner) if inner.is_dotted() => Err(ConfigEditError::Unsupported(
                    "renaming a dotted-key table is not supported".to_owned(),
                )),
                Item::Table(_) | Item::ArrayOfTables(_) => {
                    let to_path = child(path, PathSegment::Key(to.to_owned()));
                    let mut headers = Vec::new();
                    rename_headers(item, &to_path, &mut headers);
                    let mut out = text.to_owned();
                    headers.sort_by_key(|(range, _)| range.start);
                    for (range, header) in headers.into_iter().rev() {
                        out = splice(&out, range, &header);
                    }
                    Ok(out)
                }
                Item::None => Err(ConfigEditError::PathNotFound { path: display_path(&from_path) }),
            }
        }
        _ => Err(ConfigEditError::NotATable { path: display_path(path) }),
    }
}

/// Every explicit header under a renamed item, re-rendered with its new path.
fn rename_headers(item: &Item, path: &[PathSegment], out: &mut Vec<(Range<usize>, String)>) {
    match item {
        Item::Table(table) => {
            if is_explicit(table)
                && let Some(span) = table.span()
            {
                out.push((span, format!("[{}]", render_keys(path))));
            }
            for (key, nested) in table {
                rename_headers(nested, &child(path, PathSegment::Key(key.to_owned())), out);
            }
        }
        Item::ArrayOfTables(array) => {
            for (index, table) in array.iter().enumerate() {
                if let Some(span) = table.span() {
                    out.push((span, format!("[[{}]]", render_keys(path))));
                }
                let element = child(path, PathSegment::Index(index_u32(index)));
                for (key, nested) in table {
                    rename_headers(nested, &child(&element, PathSegment::Key(key.to_owned())), out);
                }
            }
        }
        Item::Value(_) | Item::None => {}
    }
}

#[cfg(test)]
#[path = "edit_ops_tests.rs"]
mod tests;
