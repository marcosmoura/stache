//! `config migrate`: convert the legacy single file into the new files.
//!
//! The legacy application is never imported. This module reads JSON values,
//! converts every section the catalog owns through its authoritative type, and
//! warns about any other key instead of copying it.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::{fmt, fs};

use serde_json::Value;
use stache_sdk::config::loader::parse_text;
use stache_sdk::config::{ConfigEnvironment, ConfigFileId, ConfigPaths};
use stache_sdk::{ConfigCatalog, GlobalConfig, LoggingConfig, ModuleId, StartupCommands};

use crate::{MigrateArgs, fail, write_atomic};

pub(crate) fn run<W: Write, E: Write>(
    args: &MigrateArgs,
    catalog: &ConfigCatalog,
    paths: &ConfigPaths,
    environment: &ConfigEnvironment,
    out: &mut W,
    err: &mut E,
) -> ExitCode {
    let source = match choose_source(args.from.as_deref(), paths, environment) {
        Ok(source) => source,
        Err(error) => return fail(err, &error.to_string()),
    };
    let text = match fs::read_to_string(&source) {
        Ok(text) => text,
        Err(error) => {
            return fail(
                err,
                &MigrateError::Read {
                    path: source,
                    message: error.to_string(),
                }
                .to_string(),
            );
        }
    };
    let value = match parse_text(ConfigFileId::Global, &source, &text) {
        Ok(value) => value,
        Err(diagnostic) => return fail(err, &diagnostic.to_string()),
    };
    let plan = match plan(&value, &source, catalog) {
        Ok(plan) => plan,
        Err(error) => return fail(err, &error.to_string()),
    };
    let destination = paths.directory();
    let canonical_source = fs::canonicalize(&source).unwrap_or_else(|_| source.clone());
    let overwrites_source = plan.files.keys().any(|name| {
        canonical_destination(destination, name).is_some_and(|path| path == canonical_source)
    });
    if overwrites_source {
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
    for report in &plan.reports {
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
    ExitCode::SUCCESS
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

fn choose_source(
    explicit: Option<&Path>,
    paths: &ConfigPaths,
    environment: &ConfigEnvironment,
) -> Result<PathBuf, MigrateError> {
    if let Some(explicit) = explicit {
        if !explicit.exists() {
            return Err(MigrateError::SourceNotFound(explicit.to_path_buf()));
        }
        return Ok(explicit.to_path_buf());
    }
    let candidates = paths.legacy_sources(environment);
    candidates
        .iter()
        .find(|candidate| candidate.exists())
        .cloned()
        .ok_or(MigrateError::NoSource(candidates))
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
            // longer accepts; the registered type translates them before the
            // section is validated or written.
            let mut value = value.clone();
            for note in catalog.rewrite_legacy(id, &mut value) {
                reports.push(format!("warning: {note}"));
            }
            let warnings =
                catalog.inspect(id, source, &value.to_string()).map_err(|diagnostic| {
                    MigrateError::Field {
                        field: id.as_str(),
                        message: diagnostic.to_string(),
                    }
                })?;
            for warning in warnings {
                reports.push(format!(
                    "warning: unknown {key} key `{warning}` is preserved in {key}.jsonc and ignored by the typed configuration"
                ));
            }
            files.insert(format!("{key}.jsonc"), prettify(&value)?);
            reports.push(format!("validated {key} -> {key}.jsonc"));
            continue;
        }
        match key.as_str() {
            "execOnStartup" => {
                global_present = true;
                global.exec_on_startup = serde_json::from_value::<StartupCommands>(value.clone())
                    .map_err(|error| MigrateError::Field {
                    field: "execOnStartup",
                    message: error.to_string(),
                })?;
                reports.push("converted execOnStartup -> global.jsonc".to_owned());
            }
            "logging" => {
                global_present = true;
                let logging = convert_logging(value, &mut reports)?;
                global.logging = logging;
                reports.push("converted logging -> global.jsonc".to_owned());
            }
            "widgets" => reports.push("warning: dropped widgets (not ported)".to_owned()),
            unknown => {
                reports.push(format!("warning: unknown key `{unknown}` was not migrated"));
            }
        }
    }
    let global_contents = if global_present {
        prettify(&serde_json::to_value(&global).map_err(|error| {
            MigrateError::Serialize(format!("cannot serialize global.jsonc: {error}"))
        })?)?
    } else {
        catalog.template(GlobalConfig::ID).ok_or_else(|| {
            MigrateError::Catalog(
                "global configuration is not registered in the configuration catalog".to_owned(),
            )
        })?
    };
    files.insert("global.jsonc".to_owned(), global_contents);
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

fn prettify(value: &Value) -> Result<String, MigrateError> {
    let mut rendered = serde_json::to_string_pretty(value)
        .map_err(|error| MigrateError::Serialize(error.to_string()))?;
    rendered.push('\n');
    Ok(rendered)
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
    #[serde(default, rename_all = "camelCase")]
    struct Pilot {
        enabled: bool,
        input: Vec<Entry>,
        output: Vec<Entry>,
    }

    #[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
    #[serde(default, rename_all = "camelCase")]
    struct Entry {
        name: String,
    }

    impl ModuleConfig for Pilot {
        fn enabled(&self) -> bool { self.enabled }

        fn validate(&self) -> Result<(), ConfigError> { Ok(()) }
    }

    /// A stand-in shaped like `MenuAnywhere`: a disabled default with enum-like
    /// strings, validated only by the generic path.
    #[derive(Clone, Debug, Deserialize, PartialEq, Serialize, JsonSchema)]
    #[serde(default, rename_all = "camelCase")]
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
                mouse_button: "rightClick".to_owned(),
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
    #[serde(default, rename_all = "camelCase")]
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
    #[serde(default, rename_all = "camelCase")]
    struct LegacyTarget {
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
            spotify_plan.reports.contains(&"validated notunes -> notunes.jsonc".to_owned()),
            "{:?}",
            spotify_plan.reports
        );
        assert!(spotify_plan.files["notunes.jsonc"].contains("\"com.spotify.client\""));
        assert!(!spotify_plan.files["notunes.jsonc"].contains("\"spotify\""));

        let block_only = plan(
            &parse(r#"{ "notunes": { "enabled": true, "targetApp": "none" } }"#),
            Path::new("config.jsonc"),
            &catalog,
        )
        .expect("plan");
        assert!(
            !block_only.files["notunes.jsonc"].contains("targetApp"),
            "an explicit block-only value becomes an absent key: {}",
            block_only.files["notunes.jsonc"]
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
            "converted execOnStartup -> global.jsonc",
            "warning: unknown logging key `unexpected` was not migrated",
            "converted logging -> global.jsonc",
            "validated proxyAudio -> proxyAudio.jsonc",
            "warning: unknown key `tiling` was not migrated",
            "warning: dropped widgets (not ported)",
        ]);
        assert!(!plan.files.contains_key("tiling.jsonc"));
        assert!(plan.files.contains_key("global.jsonc"));
        assert!(plan.files.contains_key("proxyAudio.jsonc"));
        assert!(!plan.files.contains_key("widgets.jsonc"));
        assert!(plan.files["global.jsonc"].contains("\"level\": \"debug\""));
    }

    #[test]
    fn command_quit_is_no_longer_copied_verbatim() {
        let source = Path::new("/legacy/config.jsonc");
        let legacy = parse(r#"{ "commandQuit": { "enabled": true }, "tiling": {} }"#);

        let mut catalog = catalog();
        catalog.register::<Pilot>(ModuleId::new("commandQuit")).expect("commandQuit");
        let plan = plan(&legacy, source, &catalog).expect("plan");
        assert!(plan.reports.contains(&"validated commandQuit -> commandQuit.jsonc".to_owned()));
        assert!(!plan.reports.iter().any(|line| line.starts_with("copied commandQuit")));
        assert!(!plan.reports.iter().any(|line| line.starts_with("copied ")));

        let plan = super::plan(&legacy, source, &super::tests::catalog()).expect("plan");
        assert!(!plan.files.contains_key("commandQuit.jsonc"));
    }

    #[test]
    fn absent_known_sections_receive_typed_defaults() {
        let catalog = catalog();
        let plan = plan(&parse("{}"), Path::new("config.jsonc"), &catalog).expect("plan");
        assert_eq!(
            plan.files["global.jsonc"],
            catalog.template(GlobalConfig::ID).expect("global template")
        );
        assert_eq!(
            plan.files["proxyAudio.jsonc"],
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
        assert!(plan.reports.contains(&"validated notunes -> notunes.jsonc".to_owned()));
        assert!(!plan.reports.iter().any(|report| report == "copied notunes -> notunes.jsonc"));
        assert!(plan.files.contains_key("notunes.jsonc"));
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
            plan.reports
                .contains(&"validated menuAnywhere -> menuAnywhere.jsonc".to_owned()),
            "{:?}",
            plan.reports
        );
        assert!(!plan.reports.iter().any(|report| report.starts_with("copied menuAnywhere")));
        let written = parse(&plan.files["menuAnywhere.jsonc"]);
        assert_eq!(
            written,
            parse(r#"{ "enabled": true, "modifiers": [], "mouseButton": "middleClick" }"#),
            "[] and middleClick are accepted unchanged"
        );
        let duplicated = super::plan(
            &parse(r#"{ "menuAnywhere": { "modifiers": ["shift", "shift"] } }"#),
            Path::new("config.jsonc"),
            &catalog,
        )
        .expect("plan");
        assert_eq!(
            parse(&duplicated.files["menuAnywhere.jsonc"]),
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
                &"warning: unknown menuAnywhere key `preset` is preserved in menuAnywhere.jsonc and ignored by the typed configuration"
                    .to_owned()
            ),
            "{:?}",
            plan.reports
        );
        assert!(plan.files["menuAnywhere.jsonc"].contains("\"preset\""));
    }

    #[test]
    fn an_absent_menu_anywhere_section_receives_the_commented_default_template() {
        let catalog = menu_catalog();
        let plan = plan(&parse("{}"), Path::new("config.jsonc"), &catalog).expect("plan");
        assert_eq!(
            plan.files["menuAnywhere.jsonc"],
            catalog.template(ModuleId::new("menuAnywhere")).expect("template")
        );
        assert!(
            plan.reports
                .contains(&"emitted default template menuAnywhere.jsonc (disabled)".to_owned()),
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
            "warning: unknown notunes key `legacy` is preserved in notunes.jsonc and ignored by the typed configuration",
            "validated notunes -> notunes.jsonc",
            "emitted default template proxyAudio.jsonc (disabled)",
        ]);
        assert!(plan.files["notunes.jsonc"].contains("\"legacy\": 1"));
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
            plan.files["notunes.jsonc"],
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
            plan.files["keepAwake.jsonc"],
            catalog.template(ModuleId::new("keepAwake")).expect("keepAwake template")
        );
        assert!(
            plan.reports
                .contains(&"emitted default template keepAwake.jsonc (disabled)".to_owned()),
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
        assert_eq!(plan.reports, ["emitted default template optIn.jsonc"]);
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
        assert!(plan.reports.contains(&"validated keepAwake -> keepAwake.jsonc".to_owned()));
    }
}

#[cfg(test)]
mod integration_tests {
    use std::fs;

    use super::tests::{catalog, menu_catalog, parse};
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
        let source = root.path().join("legacy.jsonc");
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
        assert!(out.contains("converted execOnStartup -> global.jsonc"));
        assert!(out.contains("warning: unknown key `unknownThing` was not migrated"));
        assert_eq!(fs::read_to_string(&source).expect("source"), contents);
        assert!(destination.join("global.jsonc").exists());
        assert!(destination.join("proxyAudio.jsonc").exists());
        assert!(!destination.join("unknownThing.jsonc").exists());
    }

    #[test]
    fn collisions_change_nothing_without_force_and_force_replaces_planned_files() {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("legacy.jsonc");
        let destination = root.path().join("new");
        fs::write(&source, "{}").expect("source");
        let (code, _out, err) =
            run_migration(Some(&source), &destination, false, &ConfigEnvironment::default());
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        let sentinel = destination.join("global.jsonc");
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
            Some(&root.path().join("missing.jsonc")),
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
        assert!(destination.join("global.jsonc").exists());
        assert!(destination.join("proxyAudio.jsonc").exists());
        assert!(!destination.join("tiling.jsonc").exists());
    }

    #[test]
    fn a_source_that_is_a_destination_is_never_overwritten() {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("global.jsonc");
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
        let source = root.path().join("legacy.jsonc");
        let destination = root.path().join("new");
        fs::write(&source, "{}").expect("source");
        let (code, out, err) =
            run_migration(Some(&source), &destination, false, &ConfigEnvironment::default());
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(
            out.contains("emitted default template proxyAudio.jsonc (disabled)"),
            "{out}"
        );
        assert_eq!(
            fs::read_to_string(destination.join("proxyAudio.jsonc")).expect("pilot"),
            catalog().template(ModuleId::new("proxyAudio")).expect("pilot template")
        );
    }

    #[test]
    fn generated_defaults_match_config_init_templates() {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("legacy.jsonc");
        let destination = root.path().join("new");
        fs::write(&source, "{}").expect("source");
        let (code, _out, err) =
            run_migration(Some(&source), &destination, false, &ConfigEnvironment::default());
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        let catalog = catalog();
        assert_eq!(
            fs::read_to_string(destination.join("global.jsonc")).expect("global"),
            catalog.template(GlobalConfig::ID).expect("global template")
        );
        assert_eq!(
            fs::read_to_string(destination.join("proxyAudio.jsonc")).expect("pilot"),
            catalog.template(ModuleId::new("proxyAudio")).expect("pilot template")
        );
    }
    #[test]
    fn menu_anywhere_migrates_unchanged_and_respects_collisions_and_force() {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("legacy.jsonc");
        let destination = root.path().join("new");
        let contents = r#"{ "menuAnywhere": { "enabled": true, "modifiers": [], "mouseButton": "middleClick", "extra": 1 } }"#;
        fs::write(&source, contents).expect("source");
        let environment = ConfigEnvironment::default();
        let (code, out, err) =
            run_migration_with(&menu_catalog(), Some(&source), &destination, false, &environment);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(
            out.contains("validated menuAnywhere -> menuAnywhere.jsonc"),
            "{out}"
        );
        assert!(out.contains("unknown menuAnywhere key `extra`"), "{out}");
        assert_eq!(fs::read_to_string(&source).expect("source"), contents);
        assert_eq!(
            parse(&fs::read_to_string(destination.join("menuAnywhere.jsonc")).expect("written")),
            parse(
                r#"{ "enabled": true, "modifiers": [], "mouseButton": "middleClick", "extra": 1 }"#
            )
        );

        let target = destination.join("menuAnywhere.jsonc");
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
        let source = root.path().join("legacy.jsonc");
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
        let source = root.path().join("menuAnywhere.jsonc");
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
}
