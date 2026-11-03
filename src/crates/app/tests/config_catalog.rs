//! Composition tests: the real catalog drives schema, template and migration.

use std::ffi::OsString;
use std::fs;
use std::process::ExitCode;

use stache_proxy_audio::ProxyAudioConfig;
use stache_sdk::{CommandCatalog, ConfigCatalog, GlobalConfig};

const LEGACY: &str = include_str!("fixtures/legacy-config.jsonc");

fn catalog() -> ConfigCatalog { stache_app::catalog::catalog().expect("catalog") }

#[test]
fn the_catalog_owns_the_global_and_pilot_files() {
    let catalog = catalog();
    let ids: Vec<&str> = catalog.ids().map(stache_sdk::ModuleId::as_str).collect();
    assert_eq!(ids, ["global", "proxyAudio"]);
    assert_eq!(
        catalog.file_name(GlobalConfig::ID).as_deref(),
        Some("global.jsonc")
    );
    assert_eq!(
        catalog.file_name(ProxyAudioConfig::ID).as_deref(),
        Some("proxyAudio.jsonc")
    );
}

#[test]
fn the_pilot_schema_describes_the_legacy_fields() {
    let rendered = catalog().schema_json(ProxyAudioConfig::ID).expect("schema").expect("render");
    assert!(rendered.contains("/schemas/proxyAudio.schema.json"));
    assert!(rendered.contains("dependsOn"));
    assert!(rendered.contains("startsWith"));
}

#[test]
fn templates_come_from_the_typed_defaults() {
    let catalog = catalog();
    let global = catalog.template(GlobalConfig::ID).expect("global template");
    assert!(global.contains("\"execOnStartup\": []"));
    assert!(global.contains("\"$schema\""));
    let pilot = catalog.template(ProxyAudioConfig::ID).expect("pilot template");
    assert!(pilot.contains("\"enabled\": false"));
    assert!(pilot.contains("\"input\": []"));
}

#[test]
fn migration_converts_the_legacy_fixture_into_valid_typed_files() {
    let root = tempfile::tempdir().expect("temp dir");
    let destination = root.path().join("config");
    let source = root.path().join("legacy.jsonc");
    fs::write(&source, LEGACY).expect("source");
    let catalog = catalog();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let args: Vec<OsString> = vec![
        "stache".into(),
        "--config".into(),
        destination.clone().into_os_string(),
        "config".into(),
        "migrate".into(),
        "--from".into(),
        source.clone().into_os_string(),
    ];
    let code = stache_cli::run_with_io(
        args,
        &catalog,
        &CommandCatalog::new(),
        &stache_cli::CliEnvironment::default(),
        false,
        &mut out,
        &mut err,
    );
    assert_eq!(code, ExitCode::SUCCESS, "{}", String::from_utf8_lossy(&err));

    assert_eq!(fs::read_to_string(&source).expect("source"), LEGACY);
    let pilot_path = destination.join("proxyAudio.jsonc");
    let pilot = fs::read_to_string(&pilot_path).expect("pilot");
    catalog
        .inspect(ProxyAudioConfig::ID, &pilot_path, &pilot)
        .expect("migrated pilot validates");
    assert!(pilot.contains("\"dependsOn\""));
    assert!(destination.join("global.jsonc").exists());
    for name in [
        "bar.jsonc",
        "commandQuit.jsonc",
        "wallpapers.jsonc",
        "keybindings.jsonc",
        "menuAnywhere.jsonc",
        "notunes.jsonc",
        "tiling.jsonc",
    ] {
        assert!(destination.join(name).exists(), "{name}");
    }
    assert!(!destination.join("widgets.jsonc").exists());

    let out = String::from_utf8(out).expect("stdout");
    assert!(out.contains("warning: dropped widgets (not ported)"));
    assert!(out.contains("warning: unknown key `futureKey` was not migrated"));
}
