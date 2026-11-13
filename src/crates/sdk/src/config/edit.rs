//! Comment-preserving edits to TOML configuration files.
//!
//! Only a root boolean `enabled` key is edited. `toml_edit` keeps the original
//! text of every item with its source span, so the edit replaces one value span
//! or inserts one line and leaves every other byte (comments, whitespace, key
//! order, tables, arrays of tables) untouched. The caller validates and writes
//! the returned text; this module never touches disk.

use std::fmt;

use toml_edit::{Document, Item, Value};

use crate::config::edit_ops::{ConfigEditOp, PathSegment, apply_edit};

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
    /// The text is not valid TOML.
    Parse(String),
    /// `enabled` exists but is not a boolean literal.
    NotBoolean {
        /// The spelling of the value that was found.
        found: String,
    },
    /// A path segment crosses a value that is not a table or array.
    NotATable {
        /// The offending path.
        path: String,
    },
    /// The path does not exist.
    PathNotFound {
        /// The missing path.
        path: String,
    },
    /// The path does not name an array.
    NotAnArray {
        /// The offending path.
        path: String,
    },
    /// An array index is out of range.
    IndexOutOfRange {
        /// The array path.
        path: String,
        /// The requested index.
        index: u32,
        /// The array length.
        len: usize,
    },
    /// A rename target already exists.
    KeyExists {
        /// The existing path.
        path: String,
    },
    /// The value cannot be written as TOML (for example `null`).
    Unrepresentable(String),
    /// The edit shape is not supported.
    Unsupported(String),
    /// The planned text does not hold the requested document.
    Unverified,
}

impl fmt::Display for ConfigEditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(message) => write!(f, "cannot edit invalid TOML: {message}"),
            Self::NotBoolean { found } => {
                write!(f, "the `enabled` value must be a boolean, found `{found}`")
            }
            Self::NotATable { path } => write!(f, "`{path}` is not a table or array"),
            Self::PathNotFound { path } => write!(f, "`{path}` does not exist"),
            Self::NotAnArray { path } => write!(f, "`{path}` is not an array"),
            Self::IndexOutOfRange { path, index, len } => {
                write!(f, "index {index} is out of range for `{path}` (length {len})")
            }
            Self::KeyExists { path } => write!(f, "`{path}` already exists"),
            Self::Unrepresentable(message) | Self::Unsupported(message) => f.write_str(message),
            Self::Unverified => f.write_str(
                "the edit did not produce the requested value; the file was not changed",
            ),
        }
    }
}

impl std::error::Error for ConfigEditError {}

/// Sets the root `enabled` boolean while preserving the rest of the text.
///
/// When the key is present only its value is replaced, so a trailing comment
/// stays. When it is absent `enabled = <bool>` is inserted into the root
/// key/value block: after the last root key, or, when the file has no root
/// keys, after the leading comment lines (so a `#:schema` line stays first) and
/// before the first table header. This is the generic
/// [`ConfigEditOp::Set`](crate::config::edit_ops::ConfigEditOp::Set) on
/// `["enabled"]`, after refusing a non-boolean `enabled`.
///
/// # Errors
/// Returns [`ConfigEditError`] when the text is not valid TOML or `enabled` is
/// not a boolean.
pub fn set_enabled(text: &str, enabled: bool) -> Result<ConfigEdit, ConfigEditError> {
    let document = Document::parse(text).map_err(|error| {
        ConfigEditError::Parse(error.message().lines().next().unwrap_or("").to_owned())
    })?;
    if let Some(item) = document.get("enabled")
        && !matches!(item, Item::Value(Value::Boolean(_)))
    {
        return Err(ConfigEditError::NotBoolean { found: describe(item) });
    }
    apply_edit(text, &ConfigEditOp::Set {
        path: vec![PathSegment::Key("enabled".to_owned())],
        value: serde_json::Value::Bool(enabled),
    })
}

/// Inserts the line after the line that holds the last root key's value.
pub(crate) fn insert_after_keys(text: &str, end: usize, line: &str, newline: &str) -> String {
    let at = text[end..].find('\n').map_or(text.len(), |offset| end + offset + 1);
    let mut output = String::with_capacity(text.len() + line.len() + 2);
    output.push_str(&text[..at]);
    if !output.ends_with('\n') {
        output.push_str(newline);
    }
    output.push_str(line);
    output.push_str(&text[at..]);
    output
}

/// Inserts the line after the leading comment block, before the first table.
pub(crate) fn insert_before_tables(
    text: &str,
    first_table_start: Option<usize>,
    line: &str,
    newline: &str,
) -> String {
    let boundary = first_table_start.unwrap_or(text.len());
    let (at, followed) = leading_block_end(&text[..boundary]);
    let mut output = String::with_capacity(text.len() + line.len() + 4);
    output.push_str(&text[..at]);
    if !output.is_empty() && !output.ends_with('\n') {
        output.push_str(newline);
    }
    output.push_str(line);
    if followed || first_table_start.is_some() {
        output.push_str(newline);
    }
    output.push_str(&text[at..]);
    output
}

fn describe(item: &Item) -> String {
    match item {
        Item::Value(value) => value.to_string().trim().to_owned(),
        Item::Table(_) | Item::ArrayOfTables(_) => "a table".to_owned(),
        Item::None => String::new(),
    }
}

fn min_start(current: Option<usize>, candidate: Option<usize>) -> Option<usize> {
    match (current, candidate) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn max_end(current: Option<usize>, candidate: Option<usize>) -> Option<usize> {
    match (current, candidate) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// Root key/value ends and the earliest table header found in a document.
#[derive(Default)]
pub(crate) struct Scan {
    pub(crate) last_value_end: Option<usize>,
    pub(crate) first_table_start: Option<usize>,
}

impl Scan {
    /// Visits a root item. Dotted and implicit tables only group children, so
    /// they are descended; explicit tables and arrays of tables are headers.
    pub(crate) fn visit(&mut self, item: &Item) {
        match item {
            Item::Value(value) => {
                self.last_value_end = max_end(self.last_value_end, value.span().map(|s| s.end));
            }
            Item::Table(table) if table.is_dotted() || table.is_implicit() => {
                for (_, nested) in table {
                    self.visit(nested);
                }
            }
            Item::Table(table) => self.header(table),
            Item::ArrayOfTables(array) => {
                for table in array {
                    self.header(table);
                }
            }
            Item::None => {}
        }
    }

    /// Records an explicit header and every header nested under it, because a
    /// sub-table such as `[x.y]` may appear before its parent's `[x]`.
    fn header(&mut self, table: &toml_edit::Table) {
        if !table.is_implicit() && !table.is_dotted() {
            self.first_table_start =
                min_start(self.first_table_start, table.span().map(|span| span.start));
        }
        for (_, nested) in table {
            match nested {
                Item::Table(child) => self.header(child),
                Item::ArrayOfTables(array) => {
                    for child in array {
                        self.header(child);
                    }
                }
                Item::Value(_) | Item::None => {}
            }
        }
    }
}

/// The insertion offset inside the text before the first table (or the whole
/// text) and whether non-blank content follows it.
///
/// The leading block is every line up to and including the first blank line,
/// or every line when there is none. A blank line separates a file header
/// (`#:schema`) from the comment of the first table.
fn leading_block_end(leading: &str) -> (usize, bool) {
    let mut offset = 0;
    for line in leading.split_inclusive('\n') {
        offset += line.len();
        if line.trim().is_empty() {
            return (offset, offset < leading.len());
        }
    }
    (offset, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(source: &str, enabled: bool) -> String {
        match set_enabled(source, enabled).expect("edit") {
            ConfigEdit::Changed(text) => text,
            ConfigEdit::Unchanged => panic!("expected a change"),
        }
    }

    fn parsed(text: &str) -> toml::Table { text.parse().expect("valid TOML") }

    #[test]
    fn toggling_preserves_comments_tables_and_unrelated_values() {
        let source = "\
#:schema https://example.invalid/proxyAudio.schema.json

# Keep this explanation.
enabled = false # trailing note
name   =   'kept'

[[output]]
name = \"Speakers\" # speaker
strategy = \"exact\"

[inner]
flag = true
";
        let edited = changed(source, true);
        assert_eq!(edited, source.replace("enabled = false", "enabled = true"));
        assert_eq!(parsed(&edited)["enabled"].as_bool(), Some(true));
        assert_eq!(parsed(&edited)["output"][0]["name"].as_str(), Some("Speakers"));
    }

    #[test]
    fn the_same_value_is_unchanged() {
        assert_eq!(
            set_enabled("enabled = true\n", true).expect("edit"),
            ConfigEdit::Unchanged
        );
    }

    #[test]
    fn a_missing_key_is_inserted_after_the_last_root_key() {
        let source =
            "#:schema x\n\n# first\nname = \"a\" # why\nitems = [\n  1,\n  2,\n]\n\n[t]\nk = 1\n";
        let edited = changed(source, true);
        assert_eq!(
            edited,
            "#:schema x\n\n# first\nname = \"a\" # why\nitems = [\n  1,\n  2,\n]\nenabled = true\n\n[t]\nk = 1\n"
        );
        assert_eq!(parsed(&edited)["enabled"].as_bool(), Some(true));
    }

    #[test]
    fn insertion_into_a_schema_line_plus_tables_keeps_the_schema_first() {
        let source = "#:schema x\n\n# about t\n[t]\nk = 1\n\n[[rows]]\nn = 1\n";
        let edited = changed(source, false);
        assert!(
            edited.starts_with("#:schema x\n\nenabled = false\n\n# about t\n[t]"),
            "{edited}"
        );
        let table = parsed(&edited);
        assert_eq!(table["enabled"].as_bool(), Some(false));
        assert_eq!(table["t"]["k"].as_integer(), Some(1));
        assert_eq!(table["rows"][0]["n"].as_integer(), Some(1));

        let tight = changed("#:schema x\n[t]\nk = 1\n", true);
        assert_eq!(tight, "#:schema x\nenabled = true\n\n[t]\nk = 1\n");
    }

    #[test]
    fn comment_only_and_table_first_files_get_a_root_key() {
        assert_eq!(changed("#:schema x\n", true), "#:schema x\nenabled = true\n");
        assert_eq!(
            changed("# only\n# comments", true),
            "# only\n# comments\nenabled = true\n"
        );
        assert_eq!(changed("[t]\nk = 1\n", true), "enabled = true\n\n[t]\nk = 1\n");
        assert_eq!(changed("k = 1", false), "k = 1\nenabled = false\n");
        assert_eq!(changed("a.b = 1\n[t]\n", true), "a.b = 1\nenabled = true\n[t]\n");
    }

    #[test]
    fn crlf_files_stay_valid() {
        let edited = changed("enabled = false\r\nname = \"a\"\r\n", true);
        assert_eq!(edited, "enabled = true\r\nname = \"a\"\r\n");
        let edited = changed("name = \"a\"\r\n[t]\r\n", true);
        assert_eq!(parsed(&edited)["enabled"].as_bool(), Some(true));
        assert_eq!(edited, "name = \"a\"\r\nenabled = true\r\n[t]\r\n");
        let edited = changed("#:schema x\r\n[a.b]\r\nk = 1\r\n", true);
        assert_eq!(edited, "#:schema x\r\nenabled = true\r\n\r\n[a.b]\r\nk = 1\r\n");
        let edited = changed("k = 1", true);
        assert_eq!(edited, "k = 1\nenabled = true\n");
        let edited = changed("a = 1\r\nk = 1", true);
        assert_eq!(edited, "a = 1\r\nk = 1\r\nenabled = true\r\n");
    }

    #[test]
    fn non_boolean_enabled_values_are_rejected() {
        for source in [
            "enabled = \"yes\"\n",
            "enabled = 1\n",
            "[enabled]\nx = 1\n",
            "enabled = []\n",
        ] {
            assert!(
                matches!(
                    set_enabled(source, true),
                    Err(ConfigEditError::NotBoolean { .. })
                ),
                "{source}"
            );
        }
        assert_eq!(
            set_enabled("enabled = \"yes\"\n", true).expect_err("rejected").to_string(),
            "the `enabled` value must be a boolean, found `\"yes\"`"
        );
    }

    #[test]
    fn a_nested_enabled_key_is_not_the_root_key() {
        let edited = changed("[t]\nenabled = false\n", true);
        assert_eq!(edited, "enabled = true\n\n[t]\nenabled = false\n");
    }

    #[test]
    fn implicit_parents_and_dotted_keys_do_not_break_headers() {
        let cases = [
            "[a.b]\nk = 1\n",
            "#:schema x\n[a.b]\nk = 1\n",
            "[[a.b]]\nk = 1\n",
            "a.b = 1\n[a.c]\nk = 1\n",
            "# c\n\n[borders.focused]\nwidth = 2 # w\n\n[borders.other]\nx = 1\n",
            "[[floating.presets]]\nname = \"a\"\n\n[[floating.presets]]\nname = \"b\"\n",
            "[x.y]\nz = 1\n\n[x]\nk = 1\n",
            "[x.y]\nz = 1\n[x]\nk = 1\n",
            "[[x.y]]\nz = 1\n[x]\nk = 1\n",
        ];
        for source in cases {
            let edited = changed(source, true);
            let mut now = parsed(&edited);
            assert_eq!(now["enabled"].as_bool(), Some(true), "{edited}");
            now.remove("enabled");
            assert_eq!(now, parsed(source), "{edited}");
            // Only the inserted line (and optional separator) is added.
            let restored = edited
                .replacen("enabled = true\n\n", "", 1)
                .len()
                .max(edited.replacen("enabled = true\n", "", 1).len());
            assert!(restored >= source.len(), "{edited}");
        }
        assert_eq!(
            changed("[a.b]\nk = 1\n", true),
            "enabled = true\n\n[a.b]\nk = 1\n"
        );
        assert_eq!(
            changed("#:schema x\n[a.b]\nk = 1\n", true),
            "#:schema x\nenabled = true\n\n[a.b]\nk = 1\n"
        );
        assert_eq!(
            changed("a.b = 1\n[a.c]\nk = 1\n", true),
            "a.b = 1\nenabled = true\n[a.c]\nk = 1\n"
        );
    }

    #[test]
    fn invalid_toml_is_a_parse_error() {
        let error = set_enabled("enabled = \n", true).expect_err("invalid");
        assert!(matches!(error, ConfigEditError::Parse(_)));
        assert!(error.to_string().starts_with("cannot edit invalid TOML: "));
        assert!(set_enabled("a = 1\na = 2\n", true).is_err());
    }
}
