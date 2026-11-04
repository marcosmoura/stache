//! Composition tests: the real catalog drives schema, template and migration.

use std::ffi::OsString;
use std::fs;
use std::process::ExitCode;

use stache_keep_awake::KeepAwakeConfig;
use stache_notunes::NoTunesConfig;
use stache_proxy_audio::ProxyAudioConfig;
use stache_sdk::{CommandCatalog, ConfigCatalog, GlobalConfig};

const LEGACY: &str = include_str!("fixtures/legacy-config.jsonc");

fn catalog() -> ConfigCatalog { stache_app::catalog::catalog().expect("catalog") }

#[test]
fn the_catalog_owns_the_global_and_module_files() {
    let catalog = catalog();
    let ids: Vec<&str> = catalog.ids().map(stache_sdk::ModuleId::as_str).collect();
    assert_eq!(ids, ["global", "keepAwake", "notunes", "proxyAudio"]);
    assert_eq!(
        catalog.file_name(GlobalConfig::ID).as_deref(),
        Some("global.jsonc")
    );
    assert_eq!(
        catalog.file_name(KeepAwakeConfig::ID).as_deref(),
        Some("keepAwake.jsonc")
    );
    assert_eq!(
        catalog.file_name(NoTunesConfig::ID).as_deref(),
        Some("notunes.jsonc")
    );
    assert_eq!(
        catalog.file_name(ProxyAudioConfig::ID).as_deref(),
        Some("proxyAudio.jsonc")
    );
}

#[test]
fn the_notunes_schema_describes_an_optional_target_string() {
    let rendered = catalog().schema_json(NoTunesConfig::ID).expect("schema").expect("render");
    assert!(rendered.contains("/schemas/notunes.schema.json"));
    let schema: serde_json::Value = serde_json::from_str(&rendered).expect("schema JSON");
    let target = &schema["properties"]["targetApp"];
    assert_eq!(
        target["description"],
        "The replacement opened after a blocked application: an absolute `.app`\npath or a bundle identifier. Omitting it blocks without launching\nanything."
    );
    let types = target["type"].as_array().expect("targetApp has explicit types");
    assert!(types.iter().any(|kind| kind == "string"), "{types:?}");
    assert!(types.iter().any(|kind| kind == "null"), "{types:?}");
    assert!(
        schema.get("$defs").is_none(),
        "the preset enum must be gone: {rendered}"
    );
    for preset in ["tidal", "spotify", "feishin", "none"] {
        assert!(!rendered.contains(preset), "{preset}: {rendered}");
    }
}

#[test]
fn the_keep_awake_schema_describes_the_single_boolean() {
    let catalog = catalog();
    let rendered = catalog.schema_json(KeepAwakeConfig::ID).expect("registered").expect("render");
    assert!(rendered.contains("/schemas/keepAwake.schema.json"), "{rendered}");
    let schema: serde_json::Value = serde_json::from_str(&rendered).expect("schema JSON");
    assert_eq!(schema["title"], "KeepAwakeConfig");
    assert_eq!(schema["properties"]["enabled"]["type"], "boolean");
    assert_eq!(schema["properties"]["enabled"]["default"], false);
    assert_eq!(
        schema["properties"]["enabled"]["description"],
        "Whether Keep Awake should prevent display, idle and system sleep."
    );
    assert!(
        schema.get("$defs").is_none(),
        "one boolean needs no definitions: {rendered}"
    );
}

#[test]
fn keep_awake_schema_and_template_generation_is_deterministic() {
    let catalog = catalog();
    let schema = catalog.schema_json(KeepAwakeConfig::ID).expect("registered").expect("render");
    assert_eq!(
        schema,
        catalog.schema_json(KeepAwakeConfig::ID).expect("registered").expect("render")
    );
    assert_eq!(
        catalog.template(KeepAwakeConfig::ID),
        catalog.template(KeepAwakeConfig::ID)
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
    let keep_awake = catalog.template(KeepAwakeConfig::ID).expect("keepAwake template");
    assert!(keep_awake.contains("\"enabled\": false"), "{keep_awake}");
    assert!(!keep_awake.contains("duration"), "{keep_awake}");
    let notunes = catalog.template(NoTunesConfig::ID).expect("notunes template");
    assert!(notunes.contains("\"enabled\": false"));
    assert!(!notunes.contains("targetApp"), "{notunes}");
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

    let notunes_path = destination.join("notunes.jsonc");
    let notunes = fs::read_to_string(&notunes_path).expect("notunes");
    catalog
        .inspect(NoTunesConfig::ID, &notunes_path, &notunes)
        .expect("migrated notunes validates");
    let decoded: NoTunesConfig = serde_json::from_str(&notunes).expect("typed notunes");
    assert_eq!(decoded.target_app.as_deref(), Some("com.spotify.client"));
    for name in [
        "bar.jsonc",
        "commandQuit.jsonc",
        "wallpapers.jsonc",
        "keybindings.jsonc",
        "menuAnywhere.jsonc",
        "tiling.jsonc",
    ] {
        assert!(destination.join(name).exists(), "{name}");
    }
    assert!(!destination.join("widgets.jsonc").exists());

    let out = String::from_utf8(out).expect("stdout");
    assert!(out.contains("validated notunes -> notunes.jsonc"));
    assert!(!out.contains("copied notunes"));
    assert!(out.contains("warning: dropped widgets (not ported)"));
    assert!(out.contains("warning: unknown key `futureKey` was not migrated"));
}

fn migrate(
    source: &std::path::Path,
    destination: &std::path::Path,
    force: bool,
) -> (ExitCode, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut args: Vec<OsString> = vec![
        "stache".into(),
        "--config".into(),
        destination.to_path_buf().into_os_string(),
        "config".into(),
        "migrate".into(),
        "--from".into(),
        source.to_path_buf().into_os_string(),
    ];
    if force {
        args.push("--force".into());
    }
    let code = stache_cli::run_with_io(
        args,
        &catalog(),
        &CommandCatalog::new(),
        &stache_cli::CliEnvironment::default(),
        false,
        &mut out,
        &mut err,
    );
    (
        code,
        String::from_utf8(out).expect("stdout"),
        String::from_utf8(err).expect("stderr"),
    )
}

#[test]
fn notunes_migration_validates_types_and_preflights_collisions() {
    let root = tempfile::tempdir().expect("temp dir");
    let source = root.path().join("legacy.jsonc");
    let destination = root.path().join("config");
    fs::write(
        &source,
        r#"{ "notunes": { "enabled": true, "targetApp": "none" } }"#,
    )
    .expect("source");

    let (code, out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.contains("validated notunes -> notunes.jsonc"), "{out}");
    let notunes_path = destination.join("notunes.jsonc");
    let notunes = fs::read_to_string(&notunes_path).expect("notunes");
    let decoded: NoTunesConfig = serde_json::from_str(&notunes).expect("typed notunes");
    assert!(decoded.enabled);
    assert_eq!(decoded.target_app, None);

    fs::write(&notunes_path, "{ \"sentinel\": true }\n").expect("sentinel");
    let (code, _out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::FAILURE);
    assert!(err.contains("refusing to overwrite existing files"), "{err}");
    assert_eq!(
        fs::read_to_string(&notunes_path).expect("sentinel"),
        "{ \"sentinel\": true }\n"
    );

    let (code, _out, err) = migrate(&source, &destination, true);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_ne!(
        fs::read_to_string(&notunes_path).expect("forced"),
        "{ \"sentinel\": true }\n"
    );
}

#[test]
fn notunes_migration_warns_about_unknown_keys_and_preserves_them() {
    let root = tempfile::tempdir().expect("temp dir");
    let source = root.path().join("legacy.jsonc");
    let destination = root.path().join("config");
    fs::write(
        &source,
        r#"{ "notunes": { "enabled": false, "targetApp": "spotify", "legacyKey": true } }"#,
    )
    .expect("source");
    let (code, out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(
        out.contains(
            "warning: unknown notunes key `legacyKey` is preserved in notunes.jsonc and ignored by the typed configuration"
        ),
        "{out}"
    );
    let written = fs::read_to_string(destination.join("notunes.jsonc")).expect("notunes");
    assert!(written.contains("\"legacyKey\": true"), "{written}");
}

#[test]
fn an_invalid_notunes_value_aborts_before_writing_anything() {
    for invalid in ["music", "Spotify.app"] {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("legacy.jsonc");
        let destination = root.path().join("config");
        fs::write(
            &source,
            format!(r#"{{ "notunes": {{ "enabled": true, "targetApp": "{invalid}" }} }}"#),
        )
        .expect("source");
        let (code, _out, err) = migrate(&source, &destination, false);
        assert_eq!(code, ExitCode::FAILURE, "{invalid}: {err}");
        assert!(err.contains("cannot convert notunes"), "{invalid}: {err}");
        assert!(err.contains("targetApp"), "{invalid}: {err}");
        assert!(!destination.exists(), "{invalid}");
    }
}

#[test]
fn an_absent_notunes_section_receives_the_same_template_as_init() {
    let root = tempfile::tempdir().expect("temp dir");
    let source = root.path().join("legacy.jsonc");
    let destination = root.path().join("config");
    fs::write(&source, "{}").expect("source");
    let (code, _out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(
        fs::read_to_string(destination.join("notunes.jsonc")).expect("notunes"),
        catalog().template(NoTunesConfig::ID).expect("notunes template")
    );
}

#[test]
fn keep_awake_migration_always_emits_the_disabled_default() {
    for (label, source_text) in [
        ("enabled", r#"{ "bar": { "enabled": true } }"#),
        ("disabled", r#"{ "bar": { "enabled": false } }"#),
        ("absent", "{}"),
    ] {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("legacy.jsonc");
        let destination = root.path().join("config");
        fs::write(&source, source_text).expect("source");
        let (code, out, err) = migrate(&source, &destination, false);
        assert_eq!(code, ExitCode::SUCCESS, "{label}: {err}");
        let expected = catalog().template(KeepAwakeConfig::ID).expect("keepAwake template");
        let written = fs::read_to_string(destination.join("keepAwake.jsonc")).expect("keepAwake");
        assert_eq!(written, expected, "{label}");
        let (decoded, warnings) = catalog()
            .decode_typed::<KeepAwakeConfig>(
                KeepAwakeConfig::ID,
                &destination.join("keepAwake.jsonc"),
                &written,
            )
            .expect("typed keepAwake");
        assert!(warnings.is_empty(), "{label}: {warnings:?}");
        assert!(
            !decoded.enabled,
            "{label}: migration must not infer consent from Bar"
        );
        assert!(
            out.contains("emitted default template keepAwake.jsonc (disabled)"),
            "{label}: {out}"
        );
        assert_eq!(
            fs::read_to_string(&source).expect("source"),
            source_text,
            "{label}"
        );
        match label {
            "enabled" | "disabled" => {
                let bar: serde_json::Value = serde_json::from_str(
                    &fs::read_to_string(destination.join("bar.jsonc")).expect("bar"),
                )
                .expect("bar JSON");
                assert_eq!(
                    bar,
                    serde_json::json!({ "enabled": label == "enabled" }),
                    "{label}: the unported section stays verbatim"
                );
            }
            _ => assert!(!destination.join("bar.jsonc").exists(), "{label}"),
        }
    }
}

#[test]
fn keep_awake_migration_refuses_collisions_and_never_writes_the_source() {
    let root = tempfile::tempdir().expect("temp dir");
    let source = root.path().join("legacy.jsonc");
    let destination = root.path().join("config");
    fs::create_dir_all(&destination).expect("destination");
    fs::write(&source, "{}").expect("source");
    let sentinel = destination.join("keepAwake.jsonc");
    fs::write(&sentinel, "{ \"sentinel\": true }\n").expect("sentinel");

    let (code, _out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::FAILURE);
    assert!(err.contains("refusing to overwrite existing files"), "{err}");
    assert!(err.contains("keepAwake.jsonc"), "{err}");
    assert_eq!(
        fs::read_to_string(&sentinel).expect("sentinel"),
        "{ \"sentinel\": true }\n"
    );
    assert!(
        !destination.join("global.jsonc").exists(),
        "a refused plan writes nothing"
    );

    let (code, _out, err) = migrate(&source, &destination, true);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(
        fs::read_to_string(&sentinel).expect("replaced"),
        catalog().template(KeepAwakeConfig::ID).expect("keepAwake template")
    );

    let root = tempfile::tempdir().expect("temp dir");
    let directory = root.path().join("config");
    fs::create_dir_all(&directory).expect("config");
    let source = directory.join("keepAwake.jsonc");
    let contents = r#"{ "bar": { "enabled": true } }"#;
    fs::write(&source, contents).expect("source");
    for force in [false, true] {
        let (code, _out, err) = migrate(&source, &directory, force);
        assert_eq!(code, ExitCode::FAILURE, "force={force}: {err}");
        assert!(
            err.contains("is also a migration destination"),
            "force={force}: {err}"
        );
        assert_eq!(
            fs::read_to_string(&source).expect("source"),
            contents,
            "force={force}: the source must stay untouched"
        );
    }
}

#[test]
fn config_path_reports_typed_notunes_state() {
    let root = tempfile::tempdir().expect("temp dir");
    let directory = root.path().join("config");
    fs::create_dir_all(&directory).expect("dir");
    fs::write(
        directory.join("notunes.jsonc"),
        r#"{ "enabled": true, "targetApp": "com.example.player" }"#,
    )
    .expect("notunes");
    let (code, out, err) = run_path(&directory);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.contains("  notunes.jsonc (exists, offline)\n"), "{out}");

    fs::write(directory.join("notunes.jsonc"), r#"{ "targetApp": "music" }"#).expect("invalid");
    let (code, out, err) = run_path(&directory);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.contains("  notunes.jsonc (exists, invalid)\n"), "{out}");
    assert!(out.contains("targetApp"), "{out}");
}

fn run_path(directory: &std::path::Path) -> (ExitCode, String, String) {
    let args: Vec<OsString> = vec![
        "stache".into(),
        "--config".into(),
        directory.to_path_buf().into_os_string(),
        "config".into(),
        "path".into(),
    ];
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = stache_cli::run_with_io(
        args,
        &catalog(),
        &CommandCatalog::new(),
        &stache_cli::CliEnvironment::default(),
        false,
        &mut out,
        &mut err,
    );
    (
        code,
        String::from_utf8(out).expect("stdout"),
        String::from_utf8(err).expect("stderr"),
    )
}

#[test]
fn production_registers_the_audio_pilot_the_blocker_and_keep_awake() {
    use std::sync::Arc;

    use stache_app::host::ModuleFactories;
    use stache_sdk::{MainThreadExecutor, ThreadError};

    struct NoExecutor;

    impl MainThreadExecutor for NoExecutor {
        fn is_main_thread(&self) -> bool { false }

        fn execute(&self, _: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
            Ok(())
        }
    }

    let production = ModuleFactories::production(Arc::new(NoExecutor));
    let ids: Vec<&str> = production.entries().iter().map(|factory| factory.id().as_str()).collect();
    assert_eq!(ids, ["proxyAudio", "notunes", "keepAwake"]);
}

#[test]
fn an_enabled_keep_awake_file_is_bound_but_stays_stopped_with_production_factories() {
    use std::sync::Arc;

    use stache_app::host::ModuleFactories;
    use stache_sdk::{
        ConfigFileId, ConfigPaths, ConfigStore, DiscardLogger, EventBus, Initial, LogFilter,
        MainThreadExecutor, ModuleStatus, Permission, PermissionError, PermissionProvider,
        PermissionStatus, Registry, ReloadOutcome, ThreadError, UnavailableWindows,
    };

    struct NoExecutor;

    impl MainThreadExecutor for NoExecutor {
        fn is_main_thread(&self) -> bool { false }

        fn execute(&self, _: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
            Ok(())
        }
    }

    struct AllowAll;

    impl PermissionProvider for AllowAll {
        fn status(&self, _: Permission) -> PermissionStatus { PermissionStatus::Granted }

        fn request(&self, _: Permission) -> Result<PermissionStatus, PermissionError> {
            Ok(PermissionStatus::Granted)
        }
    }

    let root = tempfile::tempdir().expect("temp dir");
    let directory = root.path().join("config");
    fs::create_dir_all(&directory).expect("config");

    let registry = Arc::new(Registry::new(
        Arc::new(DiscardLogger),
        Arc::new(EventBus::new()),
        Arc::new(NoExecutor),
        Arc::new(AllowAll),
        Arc::new(UnavailableWindows),
    ));
    let store = ConfigStore::new(
        Arc::clone(&registry),
        catalog(),
        ConfigPaths::new(&directory),
        Arc::new(LogFilter::default()),
    );
    let missing = store.initial::<KeepAwakeConfig>(KeepAwakeConfig::ID).expect("initial");
    assert!(
        matches!(missing, Initial::Default(config) if !config.enabled),
        "a missing file uses the disabled default"
    );

    fs::write(directory.join("keepAwake.jsonc"), r#"{ "enabled": true }"#).expect("keepAwake");

    // Production registration binds the enabled file but never starts it. It is
    // safe because registration performs no native call; only `start_all` starts
    // an enabled module, and that would use the real system backend.
    let factories = ModuleFactories::production(Arc::new(NoExecutor));
    for factory in factories.entries() {
        factory.register(&registry, &store).expect("register");
    }
    let keep_awake = registry
        .snapshots()
        .into_iter()
        .find(|snapshot| snapshot.id == KeepAwakeConfig::ID)
        .expect("keepAwake is bound");
    assert!(keep_awake.enabled, "the enabled file is accepted");
    assert_eq!(
        keep_awake.status,
        ModuleStatus::Stopped,
        "registration alone must not start the module"
    );
    assert_eq!(keep_awake.revision.to_string(), "1");
    assert!(
        store.current_source_accepted(ConfigFileId::module(KeepAwakeConfig::ID)),
        "the enabled file is accepted for the bound runtime"
    );
    assert!(
        matches!(
            store.reload_one(KeepAwakeConfig::ID),
            Ok(ReloadOutcome::Unchanged)
        ),
        "an unchanged reload keeps the accepted configuration"
    );

    let initial = store.initial::<KeepAwakeConfig>(KeepAwakeConfig::ID).expect("initial");
    assert!(
        matches!(initial, Initial::Loaded(config) if config.enabled),
        "the configuration itself is valid and enabled"
    );
    assert!(
        stache_sdk::inspect_paths(store.catalog(), store.paths())
            .entries
            .iter()
            .any(|entry| entry.file == ConfigFileId::module(KeepAwakeConfig::ID)
                && entry.state == stache_sdk::PathState::Valid),
        "catalog commands still discover the file"
    );
}
