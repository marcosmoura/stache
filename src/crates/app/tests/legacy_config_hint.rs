//! The desktop host's startup legacy-config note over temporary roots
//! (`RELEASE-05-04`). Detection uses paths only, so an unreadable or
//! directory-shaped source still yields the one line and no error.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Mutex;

use stache_app::{catalog, log_legacy_config_hint};
use stache_sdk::config::ConfigEnvironment;
use stache_sdk::{ConfigPaths, LogLevel, LogRecord, Logger};

#[derive(Default)]
struct Captured(Mutex<Vec<LogRecord>>);

impl Logger for Captured {
    fn log(&self, record: LogRecord) { self.0.lock().expect("logs").push(record); }
}

fn startup_lines(root: &Path) -> Vec<String> {
    let environment = ConfigEnvironment {
        xdg_config_home: None,
        home: Some(root.to_path_buf()),
    };
    let paths = ConfigPaths::resolve(None, &environment).expect("paths");
    let logs = Captured::default();
    log_legacy_config_hint(
        &paths,
        &environment,
        &catalog::catalog().expect("catalog"),
        &logs,
    );
    let records = logs.0.into_inner().expect("logs");
    assert!(records.iter().all(|record| record.level == LogLevel::Info));
    records.into_iter().map(|record| record.message).collect()
}

fn expected(source: &Path) -> Vec<String> {
    vec![format!(
        "note: {} is a legacy config and is not read; run `stache config migrate`",
        source.display()
    )]
}

#[test]
fn an_unreadable_legacy_source_is_named_without_being_read() {
    let root = tempfile::tempdir().expect("root");
    let directory = root.path().join(".config").join("stache");
    std::fs::create_dir_all(&directory).expect("dir");
    let source = directory.join("config.jsonc");
    std::fs::write(&source, "{}").expect("source");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    assert!(std::fs::read(&source).is_err(), "the source cannot be read");

    assert_eq!(startup_lines(root.path()), expected(&source));
    let metadata = std::fs::metadata(&source).expect("metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o000);
    assert_eq!(metadata.len(), 2);
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).expect("restore");
}

#[test]
fn a_directory_shaped_legacy_source_is_still_named() {
    let root = tempfile::tempdir().expect("root");
    let source = root.path().join(".config").join("stache").join("config.jsonc");
    std::fs::create_dir_all(&source).expect("directory source");
    assert_eq!(startup_lines(root.path()), expected(&source));
}

#[test]
fn no_line_without_a_source_or_with_global_toml() {
    let root = tempfile::tempdir().expect("root");
    let directory = root.path().join(".config").join("stache");
    std::fs::create_dir_all(&directory).expect("dir");
    assert!(startup_lines(root.path()).is_empty());
    std::fs::write(directory.join("config.jsonc"), "{}").expect("source");
    std::fs::write(directory.join("global.toml"), "").expect("global");
    assert!(startup_lines(root.path()).is_empty());
}
