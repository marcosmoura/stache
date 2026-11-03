//! Schema printing and committed-schema generation.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use stache_sdk::config::schema as sdk_schema;
use stache_sdk::{ConfigCatalog, GlobalConfig, ModuleId};

use crate::{fail, output, write_atomic};

pub(crate) fn print<W: Write, E: Write>(
    catalog: &ConfigCatalog,
    requested: Option<&str>,
    ansi: bool,
    out: &mut W,
    err: &mut E,
) -> ExitCode {
    let Some(id) = resolve(catalog, requested) else {
        let available: Vec<&str> = catalog.ids().map(ModuleId::as_str).collect();
        return fail(
            err,
            &format!(
                "unknown schema `{}` (registered: {})",
                requested.unwrap_or("global"),
                available.join(", ")
            ),
        );
    };
    match catalog.schema_json(id) {
        Some(Ok(rendered)) => {
            let _ = write!(out, "{}", output::highlight_json(&rendered, ansi));
            ExitCode::SUCCESS
        }
        Some(Err(error)) => fail(err, &format!("cannot render `{}`: {error}", id.as_str())),
        None => fail(err, &format!("no schema is registered for `{}`", id.as_str())),
    }
}

fn resolve(catalog: &ConfigCatalog, requested: Option<&str>) -> Option<ModuleId> {
    let wanted = requested.unwrap_or(GlobalConfig::ID.as_str());
    catalog.ids().find(|id| id.as_str() == wanted)
}

/// The committed schema files, in catalog order.
///
/// # Errors
/// Returns a human-readable reason when a schema cannot be serialized.
pub fn generate_files(catalog: &ConfigCatalog) -> Result<Vec<(String, String)>, String> {
    let mut files = Vec::new();
    for id in catalog.ids() {
        let file = stache_sdk::ConfigFileId::module(id);
        let rendered = catalog
            .schema_json(id)
            .ok_or_else(|| format!("no schema is registered for `{}`", id.as_str()))?
            .map_err(|error| format!("cannot render `{}`: {error}", id.as_str()))?;
        files.push((sdk_schema::file_name(file), rendered));
    }
    Ok(files)
}

/// Writes or checks the committed schemas.
///
/// Check mode writes nothing and exits non-zero on any missing or stale file.
pub fn run_generate<W: Write, E: Write>(
    catalog: &ConfigCatalog,
    directory: &Path,
    check: bool,
    out: &mut W,
    err: &mut E,
) -> ExitCode {
    let files = match generate_files(catalog) {
        Ok(files) => files,
        Err(error) => return fail(err, &error),
    };
    if check {
        let mut stale = Vec::new();
        for (name, expected) in &files {
            let path = directory.join(name);
            match fs::read_to_string(&path) {
                Ok(actual) if actual == *expected => {}
                Ok(_) => stale.push(path),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => stale.push(path),
                Err(error) => {
                    return fail(err, &format!("cannot read {}: {error}", path.display()));
                }
            }
        }
        if !stale.is_empty() {
            for path in &stale {
                let _ = writeln!(err, "stache: {} is out of date", path.display());
            }
            let _ = writeln!(
                err,
                "stache: run `bun run generate-schema` and commit the result"
            );
            return ExitCode::FAILURE;
        }
        let _ = writeln!(out, "schemas are up to date");
        return ExitCode::SUCCESS;
    }
    if let Err(error) = fs::create_dir_all(directory) {
        return fail(err, &format!("cannot create {}: {error}", directory.display()));
    }
    for (name, contents) in &files {
        let path = directory.join(name);
        if let Err(error) = write_atomic(&path, contents) {
            return fail(err, &format!("failed to write {}: {error}", path.display()));
        }
        let _ = writeln!(out, "wrote {}", path.display());
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Cursor;

    use super::*;

    #[test]
    fn resolve_defaults_to_global_and_rejects_unknown_ids() {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<GlobalConfig>(GlobalConfig::ID).expect("global");
        assert_eq!(resolve(&catalog, None), Some(GlobalConfig::ID));
        assert_eq!(resolve(&catalog, Some("global")), Some(GlobalConfig::ID));
        assert_eq!(resolve(&catalog, Some("missing")), None);
    }

    #[test]
    fn unknown_schema_errors_with_the_registered_ids() {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<GlobalConfig>(GlobalConfig::ID).expect("global");
        let mut out = Cursor::new(Vec::new());
        let mut err = Cursor::new(Vec::new());
        let code = print(&catalog, Some("missing"), false, &mut out, &mut err);
        assert_eq!(code, ExitCode::FAILURE);
        assert!(
            String::from_utf8(err.into_inner())
                .expect("utf-8")
                .contains("unknown schema `missing` (registered: global)")
        );
    }

    #[test]
    fn check_mode_reports_drift_and_never_writes() {
        let root = tempfile::tempdir().expect("temp dir");
        let directory = root.path().join("schemas");
        let mut catalog = ConfigCatalog::new();
        catalog.register::<GlobalConfig>(GlobalConfig::ID).expect("global");
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(
            run_generate(&catalog, &directory, false, &mut out, &mut err),
            ExitCode::SUCCESS
        );
        let path = directory.join("global.schema.json");
        fs::write(&path, "stale\n").expect("stale schema");
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(
            run_generate(&catalog, &directory, true, &mut out, &mut err),
            ExitCode::FAILURE
        );
        assert_eq!(fs::read_to_string(&path).expect("unchanged"), "stale\n");
        assert!(String::from_utf8(err).expect("utf-8").contains("is out of date"));
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(
            run_generate(&catalog, &directory, false, &mut out, &mut err),
            ExitCode::SUCCESS
        );
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(
            run_generate(&catalog, &directory, true, &mut out, &mut err),
            ExitCode::SUCCESS
        );
        assert!(String::from_utf8(err).expect("utf-8").is_empty());
    }
}
