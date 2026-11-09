//! `config migrate`: convert the legacy single file, or the per-module
//! `<id>.jsonc` files of the previous release, into the new TOML files.
//! Per-module mode skips the legacy hooks; see [`select_source`].
//!
//! The legacy application is never imported. This module reads the legacy JSONC
//! file (the only place JSONC is still parsed), translates legacy spellings
//! (`migrate_legacy_section` hooks, then the schema-driven `snake_case`
//! [`convert`]), validates every section the catalog owns through its
//! authoritative type, and writes it as commented TOML. Unknown keys inside a
//! known section are preserved; keys the catalog does not own are warned about
//! instead of copied. TOML has no null, so a null is dropped (with a warning
//! when it sits inside an unknown key).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::{fmt, fs};

use serde_json::Value;
use stache_sdk::config::{ConfigEnvironment, ConfigFileId, ConfigPaths};
use stache_sdk::{ConfigCatalog, GlobalConfig, LoggingConfig, ModuleId, StartupCommands};

use crate::{MigrateArgs, fail, write_atomic};

mod convert;

/// The only JSONC extensions the legacy file may use.
const LEGACY_PARSE_OPTIONS: jsonc_parser::ParseOptions = jsonc_parser::ParseOptions {
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

/// Parses the legacy JSONC source into a JSON value.
///
/// Returns a `path:line:col: message` string for invalid syntax.
fn parse_legacy(path: &Path, text: &str) -> Result<Value, String> {
    if text.trim().is_empty() {
        return Err(format!(
            "{}: file is empty; write `{{}}` to use defaults",
            path.display()
        ));
    }
    jsonc_parser::parse_to_serde_value(text, &LEGACY_PARSE_OPTIONS).map_err(|error| {
        format!(
            "{}:{}:{}: {}",
            path.display(),
            error.line_display(),
            error.column_display(),
            error.kind()
        )
    })
}

pub(crate) fn run<W: Write, E: Write>(
    args: &MigrateArgs,
    catalog: &ConfigCatalog,
    paths: &ConfigPaths,
    environment: &ConfigEnvironment,
    out: &mut W,
    err: &mut E,
) -> ExitCode {
    let prepared = match prepare(args.from.as_deref(), catalog, paths, environment) {
        Ok(prepared) => prepared,
        Err(message) => return fail(err, &message),
    };
    let Prepared {
        header,
        notes,
        sources,
        plan,
        footer,
    } = prepared;
    let destination = paths.directory();
    let overwrites_source = sources.iter().any(|source| {
        let canonical_source = fs::canonicalize(source).unwrap_or_else(|_| source.clone());
        plan.files.keys().any(|name| {
            canonical_destination(destination, name).is_some_and(|path| path == canonical_source)
        })
    });
    if overwrites_source {
        let source = sources.first().cloned().unwrap_or_default();
        return fail(err, &MigrateError::SourceIsDestination(source).to_string());
    }
    let collisions: Vec<PathBuf> = plan
        .files
        .keys()
        .map(|name| destination.join(name))
        .filter(|path| path.exists())
        .collect();
    if !args.force && !collisions.is_empty() {
        let names: Vec<String> = collisions
            .iter()
            .map(|path| path.file_name().unwrap_or_default().to_string_lossy().into_owned())
            .collect();
        return fail(
            err,
            &format!(
                "refusing to overwrite existing files: {} (use --force)",
                names.join(", ")
            ),
        );
    }
    if let Err(error) = fs::create_dir_all(destination) {
        return fail(
            err,
            &MigrateError::CreateDir {
                path: destination.to_path_buf(),
                message: error.to_string(),
            }
            .to_string(),
        );
    }
    let _ = writeln!(out, "{header}");
    for report in notes.iter().chain(&plan.reports) {
        let _ = writeln!(out, "{report}");
    }
    let total = plan.files.len();
    for (written, (name, contents)) in plan.files.iter().enumerate() {
        let path = destination.join(name);
        if let Err(error) = write_atomic(&path, contents) {
            return fail(
                err,
                &MigrateError::Write {
                    path,
                    message: error.to_string(),
                    written,
                    total,
                }
                .to_string(),
            );
        }
        let _ = writeln!(out, "wrote {}", path.display());
    }
    if let Some(footer) = footer {
        let _ = writeln!(out, "{footer}");
    }
    ExitCode::SUCCESS
}

/// Everything `run` needs once the source is selected, read and validated.
struct Prepared {
    /// The first report line: the mode and the source.
    header: String,
    /// Report lines printed before the plan's own reports.
    notes: Vec<String>,
    /// Source files, for the never-overwrite-the-source check.
    sources: Vec<PathBuf>,
    plan: MigrationPlan,
    /// The last report line, printed after the files are written.
    footer: Option<String>,
}

/// The source `config migrate` reads.
#[derive(Debug, PartialEq, Eq)]
enum Selection {
    /// One legacy single file (`config.jsonc` and friends, or `--from FILE`).
    Legacy(PathBuf),
    /// Existing per-module `<id>.jsonc` files.
    PerModule {
        /// The directory, or the single file named by `--from`.
        origin: PathBuf,
        /// The existing files, in catalog order.
        files: Vec<(ModuleId, PathBuf)>,
        /// A legacy file next to the per-module files that was not migrated.
        unmigrated: Vec<PathBuf>,
    },
}

/// Selects the source, then reads, converts and validates all of it.
///
/// Nothing is written here, so any error leaves the destination untouched.
fn prepare(
    from: Option<&Path>,
    catalog: &ConfigCatalog,
    paths: &ConfigPaths,
    environment: &ConfigEnvironment,
) -> Result<Prepared, String> {
    match select_source(from, catalog, paths, environment).map_err(|error| error.to_string())? {
        Selection::Legacy(source) => {
            let text = fs::read_to_string(&source).map_err(|error| {
                MigrateError::Read {
                    path: source.clone(),
                    message: error.to_string(),
                }
                .to_string()
            })?;
            let value = parse_legacy(&source, &text)?;
            let plan = plan(&value, &source, catalog).map_err(|error| error.to_string())?;
            Ok(Prepared {
                header: format!("migrating legacy file {}", source.display()),
                notes: Vec::new(),
                sources: vec![source],
                plan,
                footer: None,
            })
        }
        Selection::PerModule { origin, files, unmigrated } => {
            let plan = plan_modules(&files, catalog)?;
            let header = if origin.is_dir() {
                format!("migrating per-module files from {}", origin.display())
            } else {
                format!("migrating per-module file {}", origin.display())
            };
            let notes = unmigrated
                .iter()
                .map(|path| {
                    format!(
                        "note: legacy {} was not migrated; use `--from {}` to migrate it",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        path.display()
                    )
                })
                .collect();
            Ok(Prepared {
                header,
                notes,
                sources: files.into_iter().map(|(_, path)| path).collect(),
                plan,
                footer: Some(
                    "the .jsonc files are unchanged and ignored; remove them after checking the .toml files"
                        .to_owned(),
                ),
            })
        }
    }
}

/// Canonical path of a planned destination, which may not exist yet.
///
/// The destination directory itself may be absent on first migration, so its
/// existing parent and the planned file name are canonicalized instead.
fn canonical_destination(directory: &Path, name: &str) -> Option<PathBuf> {
    let base = fs::canonicalize(directory).ok().or_else(|| {
        let parent = directory.parent()?;
        let parent = fs::canonicalize(parent).ok()?;
        Some(parent.join(directory.file_name()?))
    })?;
    Some(base.join(name))
}

/// The legacy single-file names that may sit next to per-module files.
const LEGACY_FILE_NAMES: [&str; 2] = ["config.jsonc", "config.json"];

/// Applies the source-selection order:
///
/// 1. `--from DIR` selects per-module mode over that directory.
/// 2. `--from FILE` named `<registered id>.jsonc` selects per-module mode for
///    that one file; any other file is the legacy single file.
/// 3. Without `--from`, a configuration directory that holds a registered
///    `<id>.jsonc` selects per-module mode; otherwise legacy discovery runs.
fn select_source(
    explicit: Option<&Path>,
    catalog: &ConfigCatalog,
    paths: &ConfigPaths,
    environment: &ConfigEnvironment,
) -> Result<Selection, MigrateError> {
    if let Some(explicit) = explicit {
        if !explicit.exists() {
            return Err(MigrateError::SourceNotFound(explicit.to_path_buf()));
        }
        if explicit.is_dir() {
            let files = module_files(catalog, explicit);
            if files.is_empty() {
                return Err(MigrateError::NoModuleFiles {
                    directory: explicit.to_path_buf(),
                    expected: expected_names(catalog),
                });
            }
            return Ok(Selection::PerModule {
                origin: explicit.to_path_buf(),
                files,
                unmigrated: Vec::new(),
            });
        }
        if let Some(id) = module_file_id(catalog, explicit) {
            return Ok(Selection::PerModule {
                origin: explicit.to_path_buf(),
                files: vec![(id, explicit.to_path_buf())],
                unmigrated: Vec::new(),
            });
        }
        return Ok(Selection::Legacy(explicit.to_path_buf()));
    }
    let directory = paths.directory();
    let files = module_files(catalog, directory);
    if !files.is_empty() {
        let unmigrated = LEGACY_FILE_NAMES
            .iter()
            .map(|name| directory.join(name))
            .filter(|path| path.is_file())
            .collect();
        return Ok(Selection::PerModule {
            origin: directory.to_path_buf(),
            files,
            unmigrated,
        });
    }
    let candidates = paths.legacy_sources(environment);
    candidates
        .iter()
        .find(|candidate| candidate.exists())
        .cloned()
        .map(Selection::Legacy)
        .ok_or(MigrateError::NoSource(candidates))
}

/// The registered ids that have an existing `<id>.jsonc` in a directory.
fn module_files(catalog: &ConfigCatalog, directory: &Path) -> Vec<(ModuleId, PathBuf)> {
    let paths = ConfigPaths::new(directory);
    catalog
        .ids()
        .map(|id| (id, paths.module_jsonc_file(id)))
        .filter(|(_, path)| path.is_file())
        .collect()
}

/// The registered id named by a `<id>.jsonc` file, if it is one.
fn module_file_id(catalog: &ConfigCatalog, path: &Path) -> Option<ModuleId> {
    if path.extension()? != "jsonc" {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    catalog.ids().find(|id| id.as_str() == stem)
}

fn expected_names(catalog: &ConfigCatalog) -> Vec<String> {
    catalog.ids().map(|id| format!("{}.jsonc", id.as_str())).collect()
}

#[derive(Debug)]
struct MigrationPlan {
    files: BTreeMap<String, String>,
    reports: Vec<String>,
}

fn plan(
    value: &Value,
    source: &Path,
    catalog: &ConfigCatalog,
) -> Result<MigrationPlan, MigrateError> {
    let Some(root) = value.as_object() else {
        return Err(MigrateError::Shape {
            path: source.to_path_buf(),
            message: "expected a JSON object".to_owned(),
        });
    };
    let mut global = GlobalConfig::default();
    let mut global_present = false;
    let mut files = BTreeMap::new();
    let mut reports = Vec::new();
    for (key, value) in root {
        if let Some(id) = registered_module(catalog, key) {
            // A legacy section may use spellings the running configuration no
            // longer accepts. The registered type translates its legacy shapes
            // first (hooks match legacy spellings), then the schema-driven
            // converter renames keys and enum values to `snake_case`, and only
            // then is the section validated and written.
            let mut value = value.clone();
            for note in catalog.rewrite_legacy(id, &mut value) {
                reports.push(format!("warning: {note}"));
            }
            let contents = migrate_section(catalog, id, source, &value, &mut reports)?;
            files.insert(format!("{key}.toml"), contents);
            continue;
        }
        match key.as_str() {
            "execOnStartup" => {
                global_present = true;
                global.exec_on_startup =
                    serde_json::from_value::<StartupCommands>(convert_global(catalog, key, value)?)
                        .map_err(|error| MigrateError::Field {
                            field: "exec_on_startup",
                            message: error.to_string(),
                        })?;
                reports.push("converted exec_on_startup -> global.toml".to_owned());
            }
            "logging" => {
                global_present = true;
                let logging = convert_logging(&convert_global(catalog, key, value)?, &mut reports)?;
                global.logging = logging;
                reports.push("converted logging -> global.toml".to_owned());
            }
            "widgets" => reports.push("warning: dropped widgets (not ported)".to_owned()),
            unknown => {
                reports.push(format!("warning: unknown key `{unknown}` was not migrated"));
            }
        }
    }
    let global_contents = if global_present {
        let value = serde_json::to_value(&global).map_err(|error| {
            MigrateError::Serialize(format!("cannot serialize global.toml: {error}"))
        })?;
        render(catalog, GlobalConfig::ID, &value)?
    } else {
        catalog.template(GlobalConfig::ID).ok_or_else(|| {
            MigrateError::Catalog(
                "global configuration is not registered in the configuration catalog".to_owned(),
            )
        })?
    };
    files.insert("global.toml".to_owned(), global_contents);
    for id in catalog.ids() {
        if id == GlobalConfig::ID {
            continue;
        }
        let name = catalog.file_name(id).ok_or_else(|| {
            MigrateError::Catalog(format!("{id} is not registered in the configuration catalog"))
        })?;
        if files.contains_key(&name) {
            continue;
        }
        let template = catalog.template(id).ok_or_else(|| {
            MigrateError::Catalog(format!("{id} is not registered in the configuration catalog"))
        })?;
        reports.push(emitted_default(&name, catalog.default_value(id)));
        files.insert(name, template);
    }
    Ok(MigrationPlan { files, reports })
}

/// Converts, validates and renders one section for a registered id.
///
/// `value` already uses the shape the typed configuration expects, with any
/// legacy spelling of keys and enum values still allowed: the schema-driven
/// converter renames them to `snake_case`, the catalog validates the result and
/// the section is written as commented TOML. Warnings go to `reports`.
fn migrate_section(
    catalog: &ConfigCatalog,
    id: ModuleId,
    source: &Path,
    value: &Value,
    reports: &mut Vec<String>,
) -> Result<String, MigrateError> {
    let key = id.as_str();
    let schema = catalog.schema(id).ok_or_else(|| {
        MigrateError::Catalog(format!("{id} is not registered in the configuration catalog"))
    })?;
    let value = convert::convert(value, &schema).map_err(|error| MigrateError::Field {
        field: key,
        message: error.to_string(),
    })?;
    let warnings = catalog.inspect_json(id, source, value.clone()).map_err(|diagnostic| {
        MigrateError::Field {
            field: key,
            message: diagnostic.to_string(),
        }
    })?;
    for warning in &warnings {
        reports.push(format!(
            "warning: unknown {key} key `{}` is preserved in {key}.toml and ignored by the typed configuration",
            readable_path(warning)
        ));
    }
    let value = drop_nulls(value, &warnings, reports);
    let contents = render(catalog, id, &value)?;
    reports.push(format!("validated {key} -> {key}.toml"));
    Ok(contents)
}

/// Plans per-module mode: every existing `<id>.jsonc` becomes `<id>.toml`.
///
/// All sources are parsed, converted and validated before the caller writes
/// any file. A root `$schema` is dropped, and no legacy hook runs because these
/// files already have the current shape.
fn plan_modules(
    files: &[(ModuleId, PathBuf)],
    catalog: &ConfigCatalog,
) -> Result<MigrationPlan, String> {
    let mut planned = BTreeMap::new();
    let mut reports = Vec::new();
    for (id, path) in files {
        let text = fs::read_to_string(path).map_err(|error| {
            MigrateError::Read {
                path: path.clone(),
                message: error.to_string(),
            }
            .to_string()
        })?;
        let mut value = parse_legacy(path, &text)?;
        let Some(root) = value.as_object_mut() else {
            return Err(MigrateError::Shape {
                path: path.clone(),
                message: "expected a JSON object".to_owned(),
            }
            .to_string());
        };
        root.remove("$schema");
        let contents = migrate_section(catalog, *id, path, &value, &mut reports)
            .map_err(|error| error.to_string())?;
        planned.insert(format!("{}.toml", id.as_str()), contents);
    }
    Ok(MigrationPlan { files: planned, reports })
}

/// Reports a template emitted for a section absent from the source.
///
/// The registered default decides whether the file is described as disabled;
/// migration never guesses from a legacy section that the new file does not own.
fn emitted_default(name: &str, default: Option<&Value>) -> String {
    let disabled =
        default.and_then(|value| value.get("enabled")).and_then(Value::as_bool) == Some(false);
    if disabled {
        format!("emitted default template {name} (disabled)")
    } else {
        format!("emitted default template {name}")
    }
}

/// The registered module whose exact identifier matches a legacy section.
fn registered_module(catalog: &ConfigCatalog, key: &str) -> Option<ModuleId> {
    catalog.ids().find(|id| *id != GlobalConfig::ID && id.as_str() == key)
}

/// Converts one legacy root key of the global file to its `snake_case` form and
/// returns its value. The global schema owns `exec_on_startup` and `logging`.
fn convert_global(
    catalog: &ConfigCatalog,
    key: &str,
    value: &Value,
) -> Result<Value, MigrateError> {
    let schema = catalog.schema(GlobalConfig::ID).ok_or_else(|| {
        MigrateError::Catalog(
            "global configuration is not registered in the configuration catalog".to_owned(),
        )
    })?;
    let mut root = serde_json::Map::new();
    root.insert(key.to_owned(), value.clone());
    let converted =
        convert::convert(&Value::Object(root), &schema).map_err(|error| MigrateError::Field {
            field: "global",
            message: error.to_string(),
        })?;
    let snake = match key {
        "execOnStartup" => "exec_on_startup",
        other => other,
    };
    Ok(converted.get(snake).cloned().unwrap_or(Value::Null))
}

fn convert_logging(
    value: &Value,
    reports: &mut Vec<String>,
) -> Result<LoggingConfig, MigrateError> {
    if let Some(object) = value.as_object() {
        for key in object.keys() {
            if key != "level" && key != "debug" {
                reports.push(format!("warning: unknown logging key `{key}` was not migrated"));
            }
        }
    }
    serde_json::from_value(value.clone()).map_err(|error| MigrateError::Field {
        field: "logging",
        message: error.to_string(),
    })
}

/// Renders a validated JSON value as a commented TOML file for one catalog id.
fn render(catalog: &ConfigCatalog, id: ModuleId, value: &Value) -> Result<String, MigrateError> {
    let file = ConfigFileId::module(id).file_name();
    if let Some(path) = oversized_integer(value, "") {
        return Err(MigrateError::Serialize(format!(
            "cannot write {file}: `{}` is larger than the largest TOML integer ({})",
            readable_path(&path),
            i64::MAX
        )));
    }
    catalog
        .render_value(id, value)
        .map_err(|message| MigrateError::Serialize(format!("cannot write {file}: {message}")))
}

/// The path (`serde_ignored` form) of the first integer TOML cannot hold.
fn oversized_integer(value: &Value, path: &str) -> Option<String> {
    let child = |key: &str| {
        if path.is_empty() {
            key.to_owned()
        } else {
            format!("{path}.{key}")
        }
    };
    match value {
        Value::Number(number) if number.is_u64() && number.as_i64().is_none() => {
            Some(path.to_owned())
        }
        Value::Object(object) => {
            object.iter().find_map(|(key, item)| oversized_integer(item, &child(key)))
        }
        Value::Array(items) => items
            .iter()
            .enumerate()
            .find_map(|(index, item)| oversized_integer(item, &child(&index.to_string()))),
        _ => None,
    }
}

/// Removes every `null`, which TOML cannot hold.
///
/// A null under a typed field is the same as an absent optional (validation
/// already passed), so it is dropped silently. A key the typed configuration
/// ignores is dropped whole, with a warning that names the null's path.
fn drop_nulls(value: Value, unknown: &[String], reports: &mut Vec<String>) -> Value {
    fn walk(value: Value, path: &str, unknown: &[String], reports: &mut Vec<String>) -> Value {
        match value {
            Value::Object(object) => {
                let mut kept = serde_json::Map::new();
                for (key, child) in object {
                    let child_path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    let ignored = unknown.iter().any(|name| {
                        child_path == *name || child_path.starts_with(&format!("{name}."))
                    });
                    if ignored && contains_null(&child) {
                        reports.push(format!(
                            "warning: dropped `{}` because TOML cannot hold null",
                            readable_path(&child_path)
                        ));
                        continue;
                    }
                    if child.is_null() {
                        continue;
                    }
                    kept.insert(key, walk(child, &child_path, unknown, reports));
                }
                Value::Object(kept)
            }
            Value::Array(items) => Value::Array(
                items
                    .into_iter()
                    .enumerate()
                    .map(|(index, item)| walk(item, &format!("{path}.{index}"), unknown, reports))
                    .collect(),
            ),
            other => other,
        }
    }
    fn contains_null(value: &Value) -> bool {
        match value {
            Value::Null => true,
            Value::Array(items) => items.iter().any(contains_null),
            Value::Object(object) => object.values().any(contains_null),
            _ => false,
        }
    }
    let unknown: Vec<String> = unknown.iter().map(|path| without_option_segments(path)).collect();
    walk(value, "", &unknown, reports)
}

/// Removes the `?` segments `serde_ignored` writes for `Some` and newtype
/// wrappers, which have no counterpart in the JSON value.
fn without_option_segments(path: &str) -> String {
    path.split('.').filter(|segment| *segment != "?").collect::<Vec<_>>().join(".")
}

/// Renders a `serde_ignored` path (`workspaces.0.extra`) as `workspaces[0].extra`.
fn readable_path(path: &str) -> String {
    let path = without_option_segments(path);
    let mut output = String::with_capacity(path.len() + 2);
    for (index, segment) in path.split('.').enumerate() {
        if !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_digit()) {
            output.push('[');
            output.push_str(segment);
            output.push(']');
        } else {
            if index > 0 {
                output.push('.');
            }
            output.push_str(segment);
        }
    }
    output
}

/// A migration failure with a user-facing reason.
#[derive(Debug)]
pub enum MigrateError {
    /// An explicit `--from` file does not exist.
    SourceNotFound(PathBuf),
    /// The chosen source is also a planned destination.
    SourceIsDestination(PathBuf),
    /// No discovery candidate exists.
    NoSource(Vec<PathBuf>),
    /// A `--from` directory holds none of the expected per-module files.
    NoModuleFiles {
        /// The directory that was searched.
        directory: PathBuf,
        /// The file names that would have been migrated.
        expected: Vec<String>,
    },
    /// The source could not be read.
    Read {
        /// The source path.
        path: PathBuf,
        /// The I/O reason.
        message: String,
    },
    /// The source is not a JSON object.
    Shape {
        /// The source path.
        path: PathBuf,
        /// The reason.
        message: String,
    },
    /// A converted section has the wrong type.
    Field {
        /// The legacy section name.
        field: &'static str,
        /// The reason.
        message: String,
    },
    /// A required authoritative type is missing from the catalog.
    Catalog(String),
    /// The destination directory could not be created.
    CreateDir {
        /// The destination path.
        path: PathBuf,
        /// The I/O reason.
        message: String,
    },
    /// One destination file could not be written.
    Write {
        /// The destination path.
        path: PathBuf,
        /// The I/O reason.
        message: String,
        /// Files written before the failure.
        written: usize,
        /// Files planned in total.
        total: usize,
    },
    /// A value could not be serialized.
    Serialize(String),
}

impl fmt::Display for MigrateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceNotFound(path) => {
                write!(f, "legacy configuration {} does not exist", path.display())
            }
            Self::SourceIsDestination(path) => {
                write!(f, "source {} is also a migration destination", path.display())
            }
            Self::NoSource(candidates) => {
                let paths: Vec<String> =
                    candidates.iter().map(|path| path.display().to_string()).collect();
                write!(f, "no legacy configuration found in: {}", paths.join(", "))
            }
            Self::NoModuleFiles { directory, expected } => write!(
                f,
                "no per-module configuration found in {}; expected one of: {}",
                directory.display(),
                expected.join(", ")
            ),
            Self::Read { path, message } => {
                write!(f, "cannot read {}: {message}", path.display())
            }
            Self::Shape { path, message } => {
                write!(f, "{}: {message}", path.display())
            }
            Self::Field { field, message } => write!(f, "cannot convert {field}: {message}"),
            Self::Catalog(message) | Self::Serialize(message) => f.write_str(message),
            Self::CreateDir { path, message } => {
                write!(f, "cannot create {}: {message}", path.display())
            }
            Self::Write { path, message, written, total } => write!(
                f,
                "failed to write {}: {message}; wrote {written} of {total} files (no cross-file transaction)",
                path.display()
            ),
        }
    }
}

impl std::error::Error for MigrateError {}

#[cfg(test)]
mod tests {
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use stache_sdk::{ConfigError, ModuleConfig};

    use super::*;

    #[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
    #[serde(default, rename_all = "snake_case")]
    pub(super) struct Pilot {
        enabled: bool,
        input: Vec<Entry>,
        output: Vec<Entry>,
    }

    #[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
    #[serde(default, rename_all = "snake_case")]
    struct Entry {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        depends_on: Option<Dependency>,
    }

    #[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
    #[serde(default, rename_all = "snake_case")]
    struct Dependency {
        name: String,
    }

    impl ModuleConfig for Pilot {
        fn enabled(&self) -> bool { self.enabled }

        fn validate(&self) -> Result<(), ConfigError> { Ok(()) }
    }

    /// A stand-in shaped like `MenuAnywhere`: a disabled default with enum-like
    /// strings, validated only by the generic path.
    #[derive(Clone, Debug, Deserialize, PartialEq, Serialize, JsonSchema)]
    #[serde(default, rename_all = "snake_case")]
    struct MenuLike {
        enabled: bool,
        modifiers: Vec<String>,
        mouse_button: String,
    }

    impl Default for MenuLike {
        fn default() -> Self {
            Self {
                enabled: false,
                modifiers: vec!["control".to_owned(), "command".to_owned()],
                mouse_button: "right_click".to_owned(),
            }
        }
    }

    impl ModuleConfig for MenuLike {
        fn enabled(&self) -> bool { self.enabled }

        fn validate(&self) -> Result<(), ConfigError> { Ok(()) }
    }

    pub(super) fn menu_catalog() -> ConfigCatalog {
        let mut catalog = catalog();
        catalog
            .register::<MenuLike>(ModuleId::new("menuAnywhere"))
            .expect("menuAnywhere");
        catalog
    }

    /// A pilot whose typed default is enabled, to prove the report is generic.
    #[derive(Clone, Debug, Deserialize, PartialEq, Serialize, JsonSchema)]
    #[serde(default, rename_all = "snake_case")]
    struct EnabledPilot {
        enabled: bool,
    }

    impl Default for EnabledPilot {
        fn default() -> Self { Self { enabled: true } }
    }

    impl ModuleConfig for EnabledPilot {
        fn enabled(&self) -> bool { self.enabled }

        fn validate(&self) -> Result<(), ConfigError> { Ok(()) }
    }

    /// A pilot whose replacement target still accepts a legacy spelling.
    #[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
    #[serde(default, rename_all = "snake_case")]
    pub(super) struct LegacyTarget {
        enabled: bool,
        target_app: Option<String>,
    }

    impl ModuleConfig for LegacyTarget {
        fn enabled(&self) -> bool { self.enabled }

        fn validate(&self) -> Result<(), ConfigError> { Ok(()) }

        fn migrate_legacy_section(section: &mut Value) -> Vec<String> {
            let Some(object) = section.as_object_mut() else {
                return Vec::new();
            };
            let Some(spelling) = object.get("targetApp").and_then(Value::as_str).map(str::to_owned)
            else {
                let relied_on_default = !object.contains_key("targetApp")
                    && object.get("enabled").and_then(Value::as_bool) == Some(true);
                return if relied_on_default {
                    vec!["notunes blocks without a replacement".to_owned()]
                } else {
                    Vec::new()
                };
            };
            match spelling.as_str() {
                "spotify" => {
                    object.insert(
                        "targetApp".to_owned(),
                        Value::String("com.spotify.client".to_owned()),
                    );
                }
                "none" => {
                    object.remove("targetApp");
                }
                _ => {}
            }
            Vec::new()
        }
    }

    #[test]
    fn a_legacy_spelling_is_translated_before_validation_and_writing() {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<GlobalConfig>(GlobalConfig::ID).expect("global");
        catalog.register::<LegacyTarget>(ModuleId::new("notunes")).expect("notunes");

        let spotify_plan = plan(
            &parse(r#"{ "notunes": { "enabled": true, "targetApp": "spotify" } }"#),
            Path::new("config.jsonc"),
            &catalog,
        )
        .expect("plan");
        assert!(
            spotify_plan.reports.contains(&"validated notunes -> notunes.toml".to_owned()),
            "{:?}",
            spotify_plan.reports
        );
        assert!(spotify_plan.files["notunes.toml"].contains("\"com.spotify.client\""));
        assert!(!spotify_plan.files["notunes.toml"].contains("\"spotify\""));

        let block_only = plan(
            &parse(r#"{ "notunes": { "enabled": true, "targetApp": "none" } }"#),
            Path::new("config.jsonc"),
            &catalog,
        )
        .expect("plan");
        assert!(
            !block_only.files["notunes.toml"].contains("targetApp"),
            "an explicit block-only value becomes an absent key: {}",
            block_only.files["notunes.toml"]
        );

        let implicit = plan(
            &parse(r#"{ "notunes": { "enabled": true } }"#),
            Path::new("config.jsonc"),
            &catalog,
        )
        .expect("plan");
        assert!(
            implicit
                .reports
                .contains(&"warning: notunes blocks without a replacement".to_owned()),
            "a legacy section that relied on the removed default is reported: {:?}",
            implicit.reports
        );
    }

    pub(super) fn catalog() -> ConfigCatalog {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<GlobalConfig>(GlobalConfig::ID).expect("global");
        catalog.register::<Pilot>(ModuleId::new("proxyAudio")).expect("pilot");
        catalog
    }

    pub(super) fn parse(value: &str) -> Value { serde_json::from_str(value).expect("json") }

    /// A written TOML file as a JSON value, for structural assertions.
    pub(super) fn written(text: &str) -> Value {
        stache_sdk::config::loader::parse_text(ConfigFileId::Global, Path::new("out.toml"), text)
            .unwrap_or_else(|error| panic!("the written file is valid TOML: {error}\n{text}"))
    }

    #[test]
    fn converts_known_sections_and_warns_about_unregistered_ones() {
        let source = Path::new("/legacy/config.jsonc");
        let plan = plan(
            &parse(
                r#"{
                    "execOnStartup": ["one", "two"],
                    "logging": { "level": "debug", "unexpected": true },
                    "proxyAudio": { "enabled": true, "output": [{ "name": "Speakers" }] },
                    "tiling": { "enabled": true },
                    "widgets": {}
                }"#,
            ),
            source,
            &catalog(),
        )
        .expect("plan");
        assert_eq!(plan.reports, vec![
            "converted exec_on_startup -> global.toml",
            "warning: unknown logging key `unexpected` was not migrated",
            "converted logging -> global.toml",
            "validated proxyAudio -> proxyAudio.toml",
            "warning: unknown key `tiling` was not migrated",
            "warning: dropped widgets (not ported)",
        ]);
        assert!(!plan.files.contains_key("tiling.toml"));
        assert!(plan.files.contains_key("global.toml"));
        assert!(plan.files.contains_key("proxyAudio.toml"));
        assert!(!plan.files.contains_key("widgets.toml"));
        assert!(plan.files["global.toml"].contains("level = \"debug\""));
    }

    #[test]
    fn command_quit_is_no_longer_copied_verbatim() {
        let source = Path::new("/legacy/config.jsonc");
        let legacy = parse(r#"{ "commandQuit": { "enabled": true }, "tiling": {} }"#);

        let mut catalog = catalog();
        catalog.register::<Pilot>(ModuleId::new("commandQuit")).expect("commandQuit");
        let plan = plan(&legacy, source, &catalog).expect("plan");
        assert!(plan.reports.contains(&"validated commandQuit -> commandQuit.toml".to_owned()));
        assert!(!plan.reports.iter().any(|line| line.starts_with("copied commandQuit")));
        assert!(!plan.reports.iter().any(|line| line.starts_with("copied ")));

        let plan = super::plan(&legacy, source, &super::tests::catalog()).expect("plan");
        assert!(!plan.files.contains_key("commandQuit.toml"));
    }

    #[test]
    fn absent_known_sections_receive_typed_defaults() {
        let catalog = catalog();
        let plan = plan(&parse("{}"), Path::new("config.jsonc"), &catalog).expect("plan");
        assert_eq!(
            plan.files["global.toml"],
            catalog.template(GlobalConfig::ID).expect("global template")
        );
        assert_eq!(
            plan.files["proxyAudio.toml"],
            catalog.template(ModuleId::new("proxyAudio")).expect("pilot template")
        );
        assert_eq!(plan.files.len(), 2);
    }

    #[test]
    fn unknown_root_keys_are_warned_about() {
        let plan = plan(
            &parse(r#"{ "mystery": 1 }"#),
            Path::new("config.jsonc"),
            &catalog(),
        )
        .expect("plan");
        assert!(
            plan.reports
                .contains(&"warning: unknown key `mystery` was not migrated".to_owned())
        );
    }

    #[test]
    fn an_invalid_pilot_value_is_rejected() {
        let mut catalog = catalog();
        catalog.register::<Pilot>(ModuleId::new("other")).expect("other");
        let error = plan(
            &parse(r#"{ "proxyAudio": { "enabled": "yes" } }"#),
            Path::new("config.jsonc"),
            &catalog,
        )
        .expect_err("invalid pilot");
        assert!(error.to_string().contains("cannot convert proxyAudio"));
    }

    #[test]
    fn registered_modules_are_validated_generically() {
        let mut catalog = catalog();
        catalog.register::<Pilot>(ModuleId::new("notunes")).expect("notunes");
        let plan = plan(
            &parse(r#"{ "notunes": { "enabled": true } }"#),
            Path::new("config.jsonc"),
            &catalog,
        )
        .expect("plan");
        assert!(plan.reports.contains(&"validated notunes -> notunes.toml".to_owned()));
        assert!(!plan.reports.iter().any(|report| report == "copied notunes -> notunes.toml"));
        assert!(plan.files.contains_key("notunes.toml"));
    }

    #[test]
    fn menu_anywhere_is_validated_generically_and_never_copied() {
        let catalog = menu_catalog();
        let plan = plan(
            &parse(
                r#"{ "menuAnywhere": { "enabled": true, "modifiers": [], "mouseButton": "middleClick" } }"#,
            ),
            Path::new("config.jsonc"),
            &catalog,
        )
        .expect("plan");
        assert!(
            plan.reports.contains(&"validated menuAnywhere -> menuAnywhere.toml".to_owned()),
            "{:?}",
            plan.reports
        );
        assert!(!plan.reports.iter().any(|report| report.starts_with("copied menuAnywhere")));
        let menu = written(&plan.files["menuAnywhere.toml"]);
        assert_eq!(
            menu,
            parse(r#"{ "enabled": true, "modifiers": [], "mouse_button": "middleClick" }"#),
            "[] and a string value are accepted unchanged"
        );
        let duplicated = super::plan(
            &parse(r#"{ "menuAnywhere": { "modifiers": ["shift", "shift"] } }"#),
            Path::new("config.jsonc"),
            &catalog,
        )
        .expect("plan");
        assert_eq!(
            written(&duplicated.files["menuAnywhere.toml"]),
            parse(r#"{ "modifiers": ["shift", "shift"] }"#)
        );
    }

    #[test]
    fn menu_anywhere_unknown_keys_warn_and_are_preserved() {
        let plan = plan(
            &parse(r#"{ "menuAnywhere": { "enabled": false, "preset": "wide" } }"#),
            Path::new("config.jsonc"),
            &menu_catalog(),
        )
        .expect("plan");
        assert!(
            plan.reports.contains(
                &"warning: unknown menuAnywhere key `preset` is preserved in menuAnywhere.toml and ignored by the typed configuration"
                    .to_owned()
            ),
            "{:?}",
            plan.reports
        );
        assert!(plan.files["menuAnywhere.toml"].contains("preset = \"wide\""));
    }

    #[test]
    fn an_absent_menu_anywhere_section_receives_the_commented_default_template() {
        let catalog = menu_catalog();
        let plan = plan(&parse("{}"), Path::new("config.jsonc"), &catalog).expect("plan");
        assert_eq!(
            plan.files["menuAnywhere.toml"],
            catalog.template(ModuleId::new("menuAnywhere")).expect("template")
        );
        assert!(
            plan.reports
                .contains(&"emitted default template menuAnywhere.toml (disabled)".to_owned()),
            "{:?}",
            plan.reports
        );
    }

    #[test]
    fn a_bad_menu_anywhere_section_is_rejected() {
        for invalid in [
            r#"{ "enabled": "yes" }"#,
            r#"{ "modifiers": "command" }"#,
            r#"{ "mouseButton": 2 }"#,
            r"null",
        ] {
            let error = plan(
                &parse(&format!(r#"{{ "menuAnywhere": {invalid} }}"#)),
                Path::new("config.jsonc"),
                &menu_catalog(),
            )
            .expect_err(invalid);
            assert!(
                error.to_string().contains("cannot convert menuAnywhere"),
                "{invalid}: {error}"
            );
        }
    }

    #[test]
    fn registered_module_unknown_keys_warn_before_validation() {
        let mut catalog = catalog();
        catalog.register::<Pilot>(ModuleId::new("notunes")).expect("notunes");
        let plan = plan(
            &parse(r#"{ "notunes": { "enabled": true, "legacy": 1 } }"#),
            Path::new("config.jsonc"),
            &catalog,
        )
        .expect("plan");
        assert_eq!(plan.reports, vec![
            "warning: unknown notunes key `legacy` is preserved in notunes.toml and ignored by the typed configuration",
            "validated notunes -> notunes.toml",
            "emitted default template proxyAudio.toml (disabled)",
        ]);
        assert!(plan.files["notunes.toml"].contains("legacy = 1"));
    }

    #[test]
    fn every_written_file_starts_with_the_schema_line_and_loads_back() {
        let catalog = catalog();
        let plan = plan(
            &parse(
                r#"{ "logging": { "level": "debug" }, "proxyAudio": { "enabled": true, "output": [{ "name": "Speakers" }] } }"#,
            ),
            Path::new("config.jsonc"),
            &catalog,
        )
        .expect("plan");
        for (name, contents) in &plan.files {
            let stem = name.trim_end_matches(".toml");
            assert!(
                contents.starts_with(&format!("#:schema https://raw.githubusercontent.com/marcosmoura/stache/main/schemas/{stem}.schema.json\n")),
                "{name}: {contents}"
            );
            let id = if stem == "global" {
                GlobalConfig::ID
            } else {
                ModuleId::new("proxyAudio")
            };
            let warnings = catalog.inspect(id, Path::new(name), contents).expect(name);
            assert!(warnings.is_empty(), "{name}: {warnings:?}");
        }
        assert!(plan.files["proxyAudio.toml"].contains("[[output]]"));
    }

    #[test]
    fn nulls_are_dropped_and_a_null_inside_an_unknown_key_warns() {
        let mut catalog = catalog();
        catalog.register::<Pilot>(ModuleId::new("notunes")).expect("notunes");
        let plan = plan(
            &parse(
                r#"{ "notunes": { "enabled": true, "ghost": null, "extra": { "a": [1, null], "b": 2 }, "fine": { "c": 3 } } }"#,
            ),
            Path::new("config.jsonc"),
            &catalog,
        )
        .expect("plan");
        let text = &plan.files["notunes.toml"];
        assert!(!text.contains("ghost") && !text.contains("extra"), "{text}");
        assert!(text.contains("[fine]\nc = 3"), "{text}");
        for expected in [
            "warning: dropped `ghost` because TOML cannot hold null",
            "warning: dropped `extra` because TOML cannot hold null",
        ] {
            assert!(
                plan.reports.iter().any(|report| report == expected),
                "{:?}",
                plan.reports
            );
        }
        written(text);
    }

    #[test]
    fn nulls_inside_unknown_keys_of_array_elements_warn_in_both_modes() {
        let mut catalog = catalog();
        catalog.register::<Pilot>(ModuleId::new("notunes")).expect("notunes");
        let nested = "warning: dropped `output[0].extra` because TOML cannot hold null";
        let direct = "warning: dropped `output[0].extraKey` because TOML cannot hold null";
        let legacy = plan(
            &parse(
                r#"{ "notunes": { "output": [{ "name": "a", "extra": [1, null] }, { "name": "b", "extraKey": null }] } }"#,
            ),
            Path::new("config.jsonc"),
            &catalog,
        )
        .expect("legacy plan");
        let text = &legacy.files["notunes.toml"];
        assert!(!text.contains("extra"), "{text}");
        assert!(
            legacy.reports.iter().any(|r| r == nested),
            "{:?}",
            legacy.reports
        );
        assert!(
            legacy.reports.iter().any(|r| r
                == "warning: dropped `output[1].extraKey` because TOML cannot hold null"),
            "{:?}",
            legacy.reports
        );
        assert!(
            legacy
                .reports
                .iter()
                .any(|r| r.contains("unknown notunes key `output[0].extra`")),
            "{:?}",
            legacy.reports
        );
        written(text);

        let root = tempfile::tempdir().expect("temp dir");
        for (name, body) in [
            ("a", r#"{ "output": [{ "name": "one", "extra": [1, null] }] }"#),
            ("b", r#"{ "output": [{ "name": "one", "extraKey": null }] }"#),
        ] {
            let path = root.path().join(format!("{name}.jsonc"));
            fs::write(&path, body).expect("write");
            let files = vec![(ModuleId::new("notunes"), path)];
            let plan = plan_modules(&files, &catalog).expect("per-module plan");
            let expected = if name == "a" { nested } else { direct };
            assert!(plan.reports.iter().any(|r| r == expected), "{:?}", plan.reports);
            assert!(!plan.files["notunes.toml"].contains("extra"));
            written(&plan.files["notunes.toml"]);
        }
    }

    #[test]
    fn nulls_inside_unknown_keys_under_an_optional_struct_warn_with_a_clean_path() {
        let mut catalog = catalog();
        catalog.register::<Pilot>(ModuleId::new("notunes")).expect("notunes");
        let legacy = plan(
            &parse(
                r#"{ "notunes": { "output": [{ "name": "a", "dependsOn": { "name": "b", "extra": [1, null] } }, { "name": "c", "dependsOn": { "name": "d", "extraKey": null } }] } }"#,
            ),
            Path::new("config.jsonc"),
            &catalog,
        )
        .expect("legacy plan");
        let text = &legacy.files["notunes.toml"];
        assert!(!text.contains("extra"), "{text}");
        assert!(text.contains("depends_on"), "{text}");
        for expected in [
            "warning: dropped `output[0].depends_on.extra` because TOML cannot hold null",
            "warning: dropped `output[1].depends_on.extraKey` because TOML cannot hold null",
        ] {
            assert!(
                legacy.reports.iter().any(|r| r == expected),
                "{:?}",
                legacy.reports
            );
        }
        assert!(
            legacy.reports.iter().all(|r| !r.contains('?')),
            "{:?}",
            legacy.reports
        );
        written(text);
    }

    #[test]
    fn integers_above_the_toml_range_in_unknown_keys_fail_without_writing() {
        let mut catalog = catalog();
        catalog.register::<Pilot>(ModuleId::new("notunes")).expect("notunes");
        let error = plan(
            &parse(r#"{ "notunes": { "big": 9223372036854775808 } }"#),
            Path::new("config.jsonc"),
            &catalog,
        )
        .expect_err("unrepresentable");
        assert_eq!(
            error.to_string(),
            "cannot write notunes.toml: `big` is larger than the largest TOML integer \
             (9223372036854775807)"
        );
        let error = plan(
            &parse(
                r#"{ "notunes": { "output": [{ "name": "a", "future": { "n": 18446744073709551615 } }] } }"#,
            ),
            Path::new("config.jsonc"),
            &catalog,
        )
        .expect_err("unrepresentable nested");
        assert!(error.to_string().contains("`output[0].future.n`"), "{error}");
    }

    #[test]
    fn legacy_jsonc_syntax_is_still_read_by_the_migration_parser() {
        let value = parse_legacy(
            Path::new("config.jsonc"),
            "// note\n{ \"logging\": { \"level\": \"debug\", }, /* x */ }",
        )
        .expect("jsonc");
        assert_eq!(value["logging"]["level"], "debug");
        let error =
            parse_legacy(Path::new("config.jsonc"), "{ 'a': 1 }").expect_err("single quotes");
        assert!(error.starts_with("config.jsonc:1:"), "{error}");
        assert!(parse_legacy(Path::new("config.jsonc"), "  ").is_err());
    }

    #[test]
    fn an_invalid_registered_module_is_rejected_generically() {
        let mut catalog = catalog();
        catalog.register::<Pilot>(ModuleId::new("notunes")).expect("notunes");
        let error = plan(
            &parse(r#"{ "notunes": { "enabled": "yes" } }"#),
            Path::new("config.jsonc"),
            &catalog,
        )
        .expect_err("invalid module");
        assert!(error.to_string().contains("cannot convert notunes"), "{error}");
    }

    #[test]
    fn absent_registered_modules_receive_their_templates() {
        let mut catalog = catalog();
        catalog.register::<Pilot>(ModuleId::new("notunes")).expect("notunes");
        let plan = plan(&parse("{}"), Path::new("config.jsonc"), &catalog).expect("plan");
        assert_eq!(
            plan.files["notunes.toml"],
            catalog.template(ModuleId::new("notunes")).expect("notunes template")
        );
        assert_eq!(plan.files.len(), 3);
    }

    #[test]
    fn an_absent_module_with_a_disabled_default_is_reported_as_disabled() {
        let mut catalog = catalog();
        catalog.register::<Pilot>(ModuleId::new("keepAwake")).expect("keepAwake");
        let plan = plan(&parse("{}"), Path::new("config.jsonc"), &catalog).expect("plan");
        assert_eq!(
            plan.files["keepAwake.toml"],
            catalog.template(ModuleId::new("keepAwake")).expect("keepAwake template")
        );
        assert!(
            plan.reports
                .contains(&"emitted default template keepAwake.toml (disabled)".to_owned()),
            "{:?}",
            plan.reports
        );
    }

    #[test]
    fn a_default_enabled_type_is_not_reported_as_disabled() {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<GlobalConfig>(GlobalConfig::ID).expect("global");
        catalog.register::<EnabledPilot>(ModuleId::new("optIn")).expect("optIn");
        let plan = plan(&parse("{}"), Path::new("config.jsonc"), &catalog).expect("plan");
        assert_eq!(plan.reports, ["emitted default template optIn.toml"]);
    }

    #[test]
    fn a_present_registered_section_is_not_reported_as_a_default() {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<GlobalConfig>(GlobalConfig::ID).expect("global");
        catalog.register::<Pilot>(ModuleId::new("keepAwake")).expect("keepAwake");
        let plan = plan(
            &parse(r#"{ "keepAwake": { "enabled": true } }"#),
            Path::new("config.jsonc"),
            &catalog,
        )
        .expect("plan");
        assert!(
            !plan.reports.iter().any(|report| report.starts_with("emitted default template")),
            "{:?}",
            plan.reports
        );
        assert!(plan.reports.contains(&"validated keepAwake -> keepAwake.toml".to_owned()));
    }
}

#[cfg(test)]
mod integration_tests {
    use std::fs;

    use super::tests::{catalog, menu_catalog, parse, written};
    use super::*;

    fn run_migration(
        source: Option<&Path>,
        destination: &Path,
        force: bool,
        environment: &ConfigEnvironment,
    ) -> (ExitCode, String, String) {
        run_migration_with(&catalog(), source, destination, force, environment)
    }

    fn run_migration_with(
        catalog: &ConfigCatalog,
        source: Option<&Path>,
        destination: &Path,
        force: bool,
        environment: &ConfigEnvironment,
    ) -> (ExitCode, String, String) {
        let args = MigrateArgs {
            from: source.map(Path::to_path_buf),
            force,
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(
            &args,
            catalog,
            &ConfigPaths::new(destination),
            environment,
            &mut out,
            &mut err,
        );
        (
            code,
            String::from_utf8(out).expect("utf-8 stdout"),
            String::from_utf8(err).expect("utf-8 stderr"),
        )
    }

    #[test]
    fn migration_preserves_the_source_and_writes_the_expected_files() {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("legacy.toml");
        let destination = root.path().join("new");
        let contents = r#"{
            "execOnStartup": "open -a Terminal",
            "proxyAudio": { "enabled": true, "output": [{ "name": "Speakers" }] },
            "unknownThing": true
        }"#;
        fs::write(&source, contents).expect("source");

        let (code, out, err) =
            run_migration(Some(&source), &destination, false, &ConfigEnvironment::default());
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(out.contains("converted exec_on_startup -> global.toml"));
        assert!(out.contains("warning: unknown key `unknownThing` was not migrated"));
        assert_eq!(fs::read_to_string(&source).expect("source"), contents);
        assert!(destination.join("global.toml").exists());
        assert!(destination.join("proxyAudio.toml").exists());
        assert!(!destination.join("unknownThing.toml").exists());
    }

    #[test]
    fn collisions_change_nothing_without_force_and_force_replaces_planned_files() {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("legacy.toml");
        let destination = root.path().join("new");
        fs::write(&source, "{}").expect("source");
        let (code, _out, err) =
            run_migration(Some(&source), &destination, false, &ConfigEnvironment::default());
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        let sentinel = destination.join("global.toml");
        fs::write(&sentinel, "{ \"marker\": true }").expect("marker");
        fs::write(destination.join("notes.txt"), "keep me").expect("unrelated");
        let before = fs::read_to_string(&sentinel).expect("before");

        let (code, _out, err) =
            run_migration(Some(&source), &destination, false, &ConfigEnvironment::default());
        assert_eq!(code, ExitCode::FAILURE);
        assert!(err.contains("refusing to overwrite existing files"));
        assert_eq!(fs::read_to_string(&sentinel).expect("after"), before);

        let (code, _out, err) =
            run_migration(Some(&source), &destination, true, &ConfigEnvironment::default());
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert_ne!(fs::read_to_string(&sentinel).expect("forced"), before);
        let unrelated = destination.join("notes.txt");
        assert_eq!(fs::read_to_string(unrelated).expect("unrelated"), "keep me");
    }

    #[test]
    fn an_explicit_source_must_exist() {
        let root = tempfile::tempdir().expect("temp dir");
        let (code, _out, err) = run_migration(
            Some(&root.path().join("missing.toml")),
            &root.path().join("new"),
            false,
            &ConfigEnvironment::default(),
        );
        assert_eq!(code, ExitCode::FAILURE);
        assert!(err.contains("does not exist"));
    }

    #[test]
    fn the_first_discovered_source_is_authoritative() {
        let root = tempfile::tempdir().expect("temp dir");
        let home = root.path().join("home");
        let first = home.join(".config").join("stache");
        let second = home.join("Library").join("Application Support").join("stache");
        fs::create_dir_all(&first).expect("first");
        fs::create_dir_all(&second).expect("second");
        fs::write(first.join("config.jsonc"), "{ broken").expect("broken source");
        fs::write(second.join("config.jsonc"), "{}").expect("valid source");
        let destination = root.path().join("new");
        let environment = ConfigEnvironment {
            xdg_config_home: None,
            home: Some(home),
        };
        let (code, _out, _err) = run_migration(None, &destination, false, &environment);
        assert_eq!(code, ExitCode::FAILURE);
        assert!(!destination.exists());
    }

    #[test]
    fn discovery_uses_the_injected_home_and_creates_only_registered_files() {
        let root = tempfile::tempdir().expect("temp dir");
        let home = root.path().join("home");
        let source = home.join(".stache.json");
        fs::create_dir_all(&home).expect("home");
        fs::write(&source, r#"{ "execOnStartup": ["one"] }"#).expect("source");
        let destination = root.path().join("new");
        let environment = ConfigEnvironment {
            xdg_config_home: None,
            home: Some(home),
        };
        let (code, out, err) = run_migration(None, &destination, false, &environment);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(out.contains("wrote"));
        assert!(destination.join("global.toml").exists());
        assert!(destination.join("proxyAudio.toml").exists());
        assert!(!destination.join("tiling.toml").exists());
    }

    #[test]
    fn a_source_that_is_a_destination_is_never_overwritten() {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("global.toml");
        let contents = r#"{ "execOnStartup": ["one"] }"#;
        fs::write(&source, contents).expect("source");
        for force in [false, true] {
            let (code, _out, err) =
                run_migration(Some(&source), root.path(), force, &ConfigEnvironment::default());
            assert_eq!(code, ExitCode::FAILURE, "force={force}: {err}");
            assert!(err.contains("is also a migration destination"), "{err}");
            assert_eq!(
                fs::read_to_string(&source).expect("source"),
                contents,
                "force={force}: the source must stay untouched"
            );
        }
    }

    #[test]
    fn absent_registered_modules_report_the_disabled_default_on_stdout() {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("legacy.toml");
        let destination = root.path().join("new");
        fs::write(&source, "{}").expect("source");
        let (code, out, err) =
            run_migration(Some(&source), &destination, false, &ConfigEnvironment::default());
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(
            out.contains("emitted default template proxyAudio.toml (disabled)"),
            "{out}"
        );
        assert_eq!(
            fs::read_to_string(destination.join("proxyAudio.toml")).expect("pilot"),
            catalog().template(ModuleId::new("proxyAudio")).expect("pilot template")
        );
    }

    #[test]
    fn generated_defaults_match_config_init_templates() {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("legacy.toml");
        let destination = root.path().join("new");
        fs::write(&source, "{}").expect("source");
        let (code, _out, err) =
            run_migration(Some(&source), &destination, false, &ConfigEnvironment::default());
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        let catalog = catalog();
        assert_eq!(
            fs::read_to_string(destination.join("global.toml")).expect("global"),
            catalog.template(GlobalConfig::ID).expect("global template")
        );
        assert_eq!(
            fs::read_to_string(destination.join("proxyAudio.toml")).expect("pilot"),
            catalog.template(ModuleId::new("proxyAudio")).expect("pilot template")
        );
    }
    #[test]
    fn menu_anywhere_migrates_unchanged_and_respects_collisions_and_force() {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("legacy.toml");
        let destination = root.path().join("new");
        let contents = r#"{ "menuAnywhere": { "enabled": true, "modifiers": [], "mouseButton": "middleClick", "extra": 1 } }"#;
        fs::write(&source, contents).expect("source");
        let environment = ConfigEnvironment::default();
        let (code, out, err) =
            run_migration_with(&menu_catalog(), Some(&source), &destination, false, &environment);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(
            out.contains("validated menuAnywhere -> menuAnywhere.toml"),
            "{out}"
        );
        assert!(out.contains("unknown menuAnywhere key `extra`"), "{out}");
        assert_eq!(fs::read_to_string(&source).expect("source"), contents);
        assert_eq!(
            written(&fs::read_to_string(destination.join("menuAnywhere.toml")).expect("written")),
            parse(
                r#"{ "enabled": true, "modifiers": [], "mouse_button": "middleClick", "extra": 1 }"#
            )
        );

        let target = destination.join("menuAnywhere.toml");
        fs::write(&target, "{ \"sentinel\": true }\n").expect("sentinel");
        fs::write(destination.join("notes.txt"), "keep me").expect("unrelated");
        let (code, _out, err) =
            run_migration_with(&menu_catalog(), Some(&source), &destination, false, &environment);
        assert_eq!(code, ExitCode::FAILURE);
        assert!(err.contains("refusing to overwrite existing files"), "{err}");
        assert_eq!(
            fs::read_to_string(&target).expect("kept"),
            "{ \"sentinel\": true }\n"
        );
        let (code, _out, err) =
            run_migration_with(&menu_catalog(), Some(&source), &destination, true, &environment);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert_ne!(
            fs::read_to_string(&target).expect("forced"),
            "{ \"sentinel\": true }\n"
        );
        assert_eq!(
            fs::read_to_string(destination.join("notes.txt")).expect("kept"),
            "keep me"
        );
    }

    #[test]
    fn a_bad_menu_anywhere_section_writes_nothing() {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("legacy.toml");
        let destination = root.path().join("new");
        let contents = r#"{ "execOnStartup": ["one"], "menuAnywhere": { "mouseButton": 7 } }"#;
        fs::write(&source, contents).expect("source");
        let (code, out, err) = run_migration_with(
            &menu_catalog(),
            Some(&source),
            &destination,
            false,
            &ConfigEnvironment::default(),
        );
        assert_eq!(code, ExitCode::FAILURE);
        assert!(err.contains("cannot convert menuAnywhere"), "{err}");
        assert!(out.is_empty(), "{out}");
        assert!(
            !destination.exists(),
            "nothing is created before validation passes"
        );
        assert_eq!(fs::read_to_string(&source).expect("source"), contents);
    }

    #[test]
    fn a_menu_anywhere_source_that_is_a_destination_is_never_overwritten() {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("menuAnywhere.toml");
        let contents = r#"{ "menuAnywhere": { "enabled": false } }"#;
        fs::write(&source, contents).expect("source");
        for force in [false, true] {
            let (code, _out, err) = run_migration_with(
                &menu_catalog(),
                Some(&source),
                root.path(),
                force,
                &ConfigEnvironment::default(),
            );
            assert_eq!(code, ExitCode::FAILURE, "force={force}: {err}");
            assert!(err.contains("is also a migration destination"), "{err}");
            assert_eq!(fs::read_to_string(&source).expect("source"), contents);
        }
    }

    fn bar_catalog() -> ConfigCatalog {
        let mut catalog = catalog();
        catalog.register::<super::tests::Pilot>(ModuleId::new("bar")).expect("bar");
        catalog
    }

    fn select(
        from: Option<&Path>,
        directory: &Path,
        home: Option<&Path>,
    ) -> Result<Selection, MigrateError> {
        select_source(
            from,
            &bar_catalog(),
            &ConfigPaths::new(directory),
            &ConfigEnvironment {
                xdg_config_home: None,
                home: home.map(Path::to_path_buf),
            },
        )
    }

    #[test]
    fn tc_03_01_source_selection_follows_the_documented_order() {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("source");
        let config = root.path().join("config");
        let home = root.path().join("home");
        fs::create_dir_all(&source).expect("source");
        fs::create_dir_all(&config).expect("config");
        fs::create_dir_all(&home).expect("home");
        fs::write(source.join("bar.jsonc"), "{}").expect("bar");
        fs::write(source.join("proxyAudio.jsonc"), "{}").expect("proxyAudio");
        fs::write(source.join("other.jsonc"), "{}").expect("unregistered");

        // --from DIR: per-module over that directory, registered ids only.
        let Selection::PerModule { origin, files, unmigrated } =
            select(Some(&source), &config, None).expect("directory")
        else {
            panic!("a directory selects per-module mode");
        };
        assert_eq!(origin, source);
        let ids: Vec<&str> = files.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["bar", "proxyAudio"], "catalog order, registered ids only");
        assert!(unmigrated.is_empty());

        // --from <registered id>.jsonc: per-module for that one file.
        let file = source.join("bar.jsonc");
        assert_eq!(
            select(Some(&file), &config, None).expect("file"),
            Selection::PerModule {
                origin: file.clone(),
                files: vec![(ModuleId::new("bar"), file)],
                unmigrated: Vec::new(),
            }
        );

        // --from any other file: the legacy single file, even a .jsonc one.
        for name in ["config.jsonc", "other.jsonc", "bar.json"] {
            let legacy = source.join(name);
            fs::write(&legacy, "{}").expect("legacy");
            assert_eq!(
                select(Some(&legacy), &config, None).expect(name),
                Selection::Legacy(legacy)
            );
        }

        // A missing path fails, and an empty directory lists the expected names.
        assert!(matches!(
            select(Some(&source.join("missing")), &config, None),
            Err(MigrateError::SourceNotFound(_))
        ));
        let empty = root.path().join("empty");
        fs::create_dir_all(&empty).expect("empty");
        let error = select(Some(&empty), &config, None).expect_err("empty directory");
        assert!(error.to_string().contains("bar.jsonc"), "{error}");

        // No --from, nothing anywhere: no source.
        assert!(matches!(
            select(None, &config, Some(&home)),
            Err(MigrateError::NoSource(_))
        ));

        // No --from, no per-module file: today's legacy discovery.
        let discovered = home.join(".stache.json");
        fs::write(&discovered, "{}").expect("discovered");
        assert_eq!(
            select(None, &config, Some(&home)).expect("legacy discovery"),
            Selection::Legacy(discovered)
        );

        // No --from, a registered file in the directory: per-module, even if
        // legacy discovery would also find something.
        fs::write(config.join("bar.jsonc"), "{}").expect("bar");
        let Selection::PerModule { origin, unmigrated, .. } =
            select(None, &config, Some(&home)).expect("auto per-module")
        else {
            panic!("a registered file selects per-module mode");
        };
        assert_eq!(origin, config);
        assert!(unmigrated.is_empty());

        // The same, with a legacy file next to it: reported as not migrated.
        fs::write(config.join("config.jsonc"), "{}").expect("legacy next to it");
        let Selection::PerModule { unmigrated, .. } =
            select(None, &config, Some(&home)).expect("auto per-module")
        else {
            panic!("a registered file selects per-module mode");
        };
        assert_eq!(unmigrated, vec![config.join("config.jsonc")]);
    }

    #[test]
    fn the_report_names_the_mode_and_the_source_and_the_unmigrated_legacy_file() {
        let root = tempfile::tempdir().expect("temp dir");
        let config = root.path().join("config");
        fs::create_dir_all(&config).expect("config");
        fs::write(config.join("bar.jsonc"), r#"{ "enabled": true }"#).expect("bar");
        fs::write(config.join("config.jsonc"), "{}").expect("legacy");
        let (code, out, err) = run_migration_with(
            &bar_catalog(),
            None,
            &config,
            false,
            &ConfigEnvironment::default(),
        );
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[0],
            format!("migrating per-module files from {}", config.display())
        );
        assert!(
            lines.iter().any(
                |line| line.starts_with("note: legacy config.jsonc was not migrated")
                    && line.contains("--from")
            ),
            "{out}"
        );
        assert!(
            lines.last().expect("last").contains("unchanged and ignored"),
            "{out}"
        );
        assert!(config.join("bar.toml").exists());
        assert!(
            !config.join("proxyAudio.toml").exists(),
            "no template for a missing id"
        );
        assert!(
            !config.join("global.toml").exists(),
            "no template for a missing id"
        );

        let legacy = root.path().join("legacy.jsonc");
        fs::write(&legacy, "{}").expect("legacy");
        let (code, out, err) = run_migration_with(
            &bar_catalog(),
            Some(&legacy),
            &root.path().join("new"),
            false,
            &ConfigEnvironment::default(),
        );
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert_eq!(
            out.lines().next().expect("first"),
            format!("migrating legacy file {}", legacy.display())
        );
    }

    #[test]
    fn tc_03_03_a_partial_directory_writes_only_the_existing_files() {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("source");
        let destination = root.path().join("new");
        fs::create_dir_all(&source).expect("source");
        let contents = "// c\n{ \"$schema\": \"https://example.test/x.json\", \"enabled\": true, \"input\": [{ \"name\": \"Mic\", },], }";
        fs::write(source.join("proxyAudio.jsonc"), contents).expect("proxyAudio");
        let (code, out, err) =
            run_migration(Some(&source), &destination, false, &ConfigEnvironment::default());
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(!out.contains("$schema") && !out.contains("warning"), "{out}");
        let mut names: Vec<String> = fs::read_dir(&destination)
            .expect("destination")
            .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["proxyAudio.toml"]);
        let text = fs::read_to_string(destination.join("proxyAudio.toml")).expect("written");
        assert!(text.starts_with("#:schema "), "{text}");
        assert!(!text.contains("example.test"), "{text}");
        assert_eq!(
            written(&text),
            parse(r#"{ "enabled": true, "input": [{ "name": "Mic" }] }"#)
        );
        assert_eq!(
            fs::read_to_string(source.join("proxyAudio.jsonc")).expect("source"),
            contents
        );
    }

    #[test]
    fn tc_03_03_an_invalid_source_writes_nothing_and_a_non_object_is_a_shape_error() {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("source");
        let destination = root.path().join("new");
        fs::create_dir_all(&source).expect("source");
        fs::write(source.join("bar.jsonc"), r#"{ "enabled": true }"#).expect("bar");
        for (invalid, expected) in [
            (r#"{ "enabled": "yes" }"#, "cannot convert proxyAudio"),
            ("{ broken", "proxyAudio.jsonc:1:"),
            ("[]", "expected a JSON object"),
            ("  ", "file is empty"),
        ] {
            fs::write(source.join("proxyAudio.jsonc"), invalid).expect("invalid");
            let (code, out, err) = run_migration_with(
                &bar_catalog(),
                Some(&source),
                &destination,
                false,
                &ConfigEnvironment::default(),
            );
            assert_eq!(code, ExitCode::FAILURE, "{invalid}");
            assert!(err.contains(expected), "{invalid}: {err}");
            assert!(out.is_empty(), "{out}");
            assert!(!destination.exists(), "{invalid}: nothing is written");
        }
    }

    #[test]
    fn tc_03_03_collisions_refuse_without_force_and_force_replaces_only_planned_files() {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("source");
        let destination = root.path().join("new");
        fs::create_dir_all(&source).expect("source");
        fs::create_dir_all(&destination).expect("destination");
        fs::write(source.join("bar.jsonc"), r#"{ "enabled": true }"#).expect("bar");
        fs::write(destination.join("bar.toml"), "sentinel = true\n").expect("existing");
        fs::write(destination.join("proxyAudio.toml"), "untouched = true\n").expect("other");
        let environment = ConfigEnvironment::default();

        let (code, _out, err) =
            run_migration_with(&bar_catalog(), Some(&source), &destination, false, &environment);
        assert_eq!(code, ExitCode::FAILURE);
        assert!(
            err.contains("refusing to overwrite existing files: bar.toml"),
            "{err}"
        );
        assert_eq!(
            fs::read_to_string(destination.join("bar.toml")).expect("kept"),
            "sentinel = true\n"
        );

        let (code, _out, err) =
            run_migration_with(&bar_catalog(), Some(&source), &destination, true, &environment);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert_eq!(
            written(&fs::read_to_string(destination.join("bar.toml")).expect("bar")),
            parse(r#"{ "enabled": true }"#)
        );
        assert_eq!(
            fs::read_to_string(destination.join("proxyAudio.toml")).expect("other"),
            "untouched = true\n"
        );
    }

    #[test]
    fn per_module_legacy_hooks_do_not_run() {
        // `targetApp: "spotify"` is rewritten by the legacy hook only; a
        // per-module file already holds the final value, so it stays as written.
        let mut catalog = ConfigCatalog::new();
        catalog.register::<GlobalConfig>(GlobalConfig::ID).expect("global");
        catalog
            .register::<super::tests::LegacyTarget>(ModuleId::new("notunes"))
            .expect("notunes");
        let root = tempfile::tempdir().expect("temp dir");
        fs::write(
            root.path().join("notunes.jsonc"),
            r#"{ "enabled": true, "targetApp": "spotify" }"#,
        )
        .expect("notunes");
        let files = vec![(ModuleId::new("notunes"), root.path().join("notunes.jsonc"))];
        let plan = plan_modules(&files, &catalog).expect("plan");
        assert!(plan.files["notunes.toml"].contains("target_app = \"spotify\""));
    }
}
