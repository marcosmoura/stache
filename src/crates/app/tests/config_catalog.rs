//! Composition tests: the real catalog drives schema, template and migration.

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use stache_hold_to_quit::{CommandQuitConfig, HudAnimation, HudPosition};
use stache_keep_awake::KeepAwakeConfig;
use stache_keybindings::KeybindingsConfig;
use stache_menu_anywhere::MenuAnywhereConfig;
use stache_notunes::NoTunesConfig;
use stache_proxy_audio::ProxyAudioConfig;
use stache_sdk::{CommandCatalog, ConfigCatalog, GlobalConfig};
use stache_wallpapers::WallpapersConfig;

const LEGACY: &str = include_str!("fixtures/legacy-config.jsonc");

fn catalog() -> ConfigCatalog { stache_app::catalog::catalog().expect("catalog") }

#[test]
fn the_catalog_owns_the_global_and_module_files() {
    let catalog = catalog();
    let ids: Vec<&str> = catalog.ids().map(stache_sdk::ModuleId::as_str).collect();
    assert_eq!(ids, [
        "global",
        "commandQuit",
        "keepAwake",
        "keybindings",
        "menuAnywhere",
        "notunes",
        "proxyAudio",
        "wallpapers"
    ]);
    assert_eq!(
        catalog.file_name(GlobalConfig::ID).as_deref(),
        Some("global.jsonc")
    );
    assert_eq!(
        catalog.file_name(CommandQuitConfig::ID).as_deref(),
        Some("commandQuit.jsonc")
    );
    assert_eq!(
        catalog.file_name(KeepAwakeConfig::ID).as_deref(),
        Some("keepAwake.jsonc")
    );
    assert_eq!(
        catalog.file_name(KeybindingsConfig::ID).as_deref(),
        Some("keybindings.jsonc")
    );
    assert_eq!(
        catalog.file_name(MenuAnywhereConfig::ID).as_deref(),
        Some("menuAnywhere.jsonc")
    );
    assert_eq!(
        catalog.file_name(NoTunesConfig::ID).as_deref(),
        Some("notunes.jsonc")
    );
    assert_eq!(
        catalog.file_name(ProxyAudioConfig::ID).as_deref(),
        Some("proxyAudio.jsonc")
    );
    assert_eq!(
        catalog.file_name(WallpapersConfig::ID).as_deref(),
        Some("wallpapers.jsonc")
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
    let keybindings = catalog.template(KeybindingsConfig::ID).expect("keybindings template");
    assert!(keybindings.contains("\"enabled\": false"), "{keybindings}");
    assert!(keybindings.contains("\"bindings\": {}"), "{keybindings}");
    let menu = catalog.template(MenuAnywhereConfig::ID).expect("menuAnywhere template");
    assert!(menu.contains("\"enabled\": false"), "{menu}");
    assert!(menu.contains("\"modifiers\": ["), "{menu}");
    assert!(menu.contains("\"mouseButton\": \"rightClick\""), "{menu}");
    assert!(
        menu.contains("system-wide"),
        "the empty-modifiers caution reaches the template: {menu}"
    );
    assert!(menu.contains("button number 2 only"), "{menu}");
    let notunes = catalog.template(NoTunesConfig::ID).expect("notunes template");
    assert!(notunes.contains("\"enabled\": false"));
    assert!(!notunes.contains("targetApp"), "{notunes}");
    let pilot = catalog.template(ProxyAudioConfig::ID).expect("pilot template");
    assert!(pilot.contains("\"enabled\": false"));
    assert!(pilot.contains("\"input\": []"));
}

/// The legacy fixture's `commandQuit` section keeps its keys and gains defaults.
fn assert_migrated_command_quit_keeps_legacy_keys(catalog: &ConfigCatalog, destination: &Path) {
    let command_quit_path = destination.join("commandQuit.jsonc");
    let command_quit = fs::read_to_string(&command_quit_path).expect("commandQuit");
    catalog
        .inspect(CommandQuitConfig::ID, &command_quit_path, &command_quit)
        .expect("migrated commandQuit validates");
    let decoded: CommandQuitConfig =
        serde_json::from_str(&command_quit).expect("typed commandQuit");
    assert!(decoded.enabled);
    assert_eq!(decoded.hold_duration, 1500);
    assert!(
        !command_quit.contains("dismissDelay"),
        "migration copies only legacy keys, so the file has no dismissDelay"
    );
    assert_eq!(
        decoded.dismiss_delay, 1500,
        "a legacy file has no dismissDelay, so the default applies"
    );
    assert!(!command_quit.contains("position") && !command_quit.contains("animation"));
    assert_eq!(decoded.position, HudPosition::Bottom);
    assert_eq!(decoded.animation, HudAnimation::Fade);
}

/// The legacy fixture's `menuAnywhere` section migrates to a valid, disabled file.
fn assert_migrated_menu_anywhere_is_disabled(catalog: &ConfigCatalog, destination: &Path) {
    let menu_path = destination.join("menuAnywhere.jsonc");
    let menu = fs::read_to_string(&menu_path).expect("menuAnywhere");
    catalog
        .inspect(MenuAnywhereConfig::ID, &menu_path, &menu)
        .expect("migrated menuAnywhere validates");
    let decoded: MenuAnywhereConfig = serde_json::from_str(&menu).expect("typed menuAnywhere");
    assert!(!decoded.enabled);
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
    let wallpapers_path = destination.join("wallpapers.jsonc");
    let wallpapers = fs::read_to_string(&wallpapers_path).expect("wallpapers");
    catalog
        .inspect(WallpapersConfig::ID, &wallpapers_path, &wallpapers)
        .expect("migrated wallpapers validates");
    let decoded: WallpapersConfig = serde_json::from_str(&wallpapers).expect("typed wallpapers");
    assert!(!decoded.enabled, "migration never enables Wallpapers");
    assert_migrated_command_quit_keeps_legacy_keys(&catalog, &destination);
    for name in ["bar.jsonc", "tiling.jsonc"] {
        assert!(destination.join(name).exists(), "{name}");
    }
    assert_migrated_menu_anywhere_is_disabled(&catalog, &destination);
    let keybindings_path = destination.join("keybindings.jsonc");
    let keybindings = fs::read_to_string(&keybindings_path).expect("keybindings");
    catalog
        .inspect(KeybindingsConfig::ID, &keybindings_path, &keybindings)
        .expect("migrated keybindings validates");
    let decoded: KeybindingsConfig = serde_json::from_str(&keybindings).expect("typed keybindings");
    assert!(decoded.enabled, "a nonempty legacy map enables the new file");
    assert_eq!(decoded.bindings["Command+Control+R"].commands(), [
        "stache reload"
    ]);
    assert!(!destination.join("widgets.jsonc").exists());

    let out = String::from_utf8(out).expect("stdout");
    assert!(out.contains("validated notunes -> notunes.jsonc"));
    assert!(
        out.contains("validated menuAnywhere -> menuAnywhere.jsonc"),
        "{out}"
    );
    assert!(!out.contains("copied menuAnywhere"));
    assert!(out.contains("validated wallpapers -> wallpapers.jsonc"));
    assert!(out.contains("validated keybindings -> keybindings.jsonc"));
    assert!(!out.contains("copied keybindings"));
    assert!(!out.contains("copied notunes"));
    assert!(!out.contains("copied wallpapers"));
    assert!(out.contains("validated commandQuit -> commandQuit.jsonc"));
    assert!(!out.contains("copied commandQuit"));
    for unported in ["bar", "tiling"] {
        assert!(
            out.contains(&format!("copied {unported} -> {unported}.jsonc")),
            "{unported}"
        );
    }
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
fn wallpapers_migration_preserves_values_and_warns_about_relative_sources() {
    let root = tempfile::tempdir().expect("temp dir");
    let source = root.path().join("legacy.jsonc");
    let destination = root.path().join("config");
    fs::write(
        &source,
        r#"{ "wallpapers": { "enabled": true, "path": "images", "list": ["~/a.jpg"], "interval": 30, "mode": "sequential" } }"#,
    )
    .expect("source");
    let (code, out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(
        out.contains(
            "warning: wallpapers `path` value `images` is relative; the migrated file resolves it under the configuration directory"
        ),
        "{out}"
    );
    assert!(!out.contains("copied wallpapers"), "{out}");

    let written = fs::read_to_string(destination.join("wallpapers.jsonc")).expect("wallpapers");
    let decoded: WallpapersConfig = serde_json::from_str(&written).expect("typed wallpapers");
    assert!(decoded.enabled);
    assert_eq!(decoded.path, "images", "the relative text is preserved");
    assert_eq!(decoded.list, ["~/a.jpg"]);
    assert_eq!(decoded.interval, 30);
    assert_eq!(decoded.mode, stache_wallpapers::WallpaperMode::Sequential);
}

#[test]
fn absent_wallpapers_section_receives_the_disabled_template() {
    let root = tempfile::tempdir().expect("temp dir");
    let source = root.path().join("legacy.jsonc");
    let destination = root.path().join("config");
    fs::write(&source, "{}").expect("source");
    let (code, out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    let written = fs::read_to_string(destination.join("wallpapers.jsonc")).expect("wallpapers");
    assert_eq!(
        written,
        catalog().template(WallpapersConfig::ID).expect("wallpapers template")
    );
    let (decoded, warnings) = catalog()
        .decode_typed::<WallpapersConfig>(
            WallpapersConfig::ID,
            &destination.join("wallpapers.jsonc"),
            &written,
        )
        .expect("typed wallpapers");
    assert!(warnings.is_empty());
    assert!(!decoded.enabled);
    assert!(
        out.contains("emitted default template wallpapers.jsonc (disabled)"),
        "{out}"
    );
}

#[test]
fn wallpapers_force_preserves_the_existing_file_mode() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("temp dir");
    let source = root.path().join("legacy.jsonc");
    let destination = root.path().join("config");
    fs::create_dir_all(&destination).expect("destination");
    fs::write(&source, "{}").expect("source");
    let existing = destination.join("wallpapers.jsonc");
    fs::write(&existing, "{ \"sentinel\": true }\n").expect("sentinel");
    fs::set_permissions(&existing, fs::Permissions::from_mode(0o640)).expect("mode");

    let (code, _out, err) = migrate(&source, &destination, true);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_ne!(
        fs::read_to_string(&existing).expect("replaced"),
        "{ \"sentinel\": true }\n"
    );
    assert_eq!(
        fs::metadata(&existing).expect("metadata").permissions().mode() & 0o777,
        0o640
    );
}

#[test]
fn wallpapers_is_catalogued_without_a_runtime_and_ignored_by_unbound_reloads() {
    use std::sync::Arc;

    use stache_app::bindings::{ConfigFileState, config_file_view};
    use stache_sdk::{
        ChangeSet, ConfigFileId, ConfigPaths, ConfigStore, ConfigStoreError, DiscardLogger,
        EventBus, LogFilter, MainThreadExecutor, Permission, PermissionError, PermissionProvider,
        PermissionStatus, Registry, ThreadError, UnavailableWindows, inspect_paths,
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
    fs::write(
        directory.join("wallpapers.jsonc"),
        r#"{ "enabled": true, "path": "images" }"#,
    )
    .expect("wallpapers");

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

    let report = inspect_paths(store.catalog(), store.paths());
    let entry = report
        .entries
        .iter()
        .find(|entry| entry.file == ConfigFileId::module(WallpapersConfig::ID))
        .expect("wallpapers is a catalog file");
    assert_eq!(entry.state, stache_sdk::PathState::Valid);
    let view = config_file_view(entry, &store);
    assert_eq!(view.state, ConfigFileState::Valid);
    assert!(!view.active, "an unbound module never marks its file active");
    assert!(view.diagnostic.is_none());

    assert!(
        store
            .reload_all()
            .iter()
            .all(|outcome| outcome.file != ConfigFileId::module(WallpapersConfig::ID)),
        "reload_all skips a catalog entry without a bound module"
    );
    assert!(
        store.diagnostic(ConfigFileId::module(WallpapersConfig::ID)).is_none(),
        "an unbound file produces no warning storm"
    );
    let mut changes = ChangeSet::default();
    changes.files.insert(ConfigFileId::module(WallpapersConfig::ID));
    assert!(
        store.apply(&changes).is_empty(),
        "a watcher batch for an unbound module is a no-op"
    );
    assert!(matches!(
        store.reload_one(WallpapersConfig::ID),
        Err(ConfigStoreError::Unbound { .. })
    ));
    assert!(
        registry.snapshots().is_empty(),
        "no Wallpapers runtime is registered"
    );

    let (code, out, err) = run_path(&directory);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.contains("  wallpapers.jsonc (exists, offline)\n"), "{out}");

    let (code, out, err) = run_args(&["stache", "schema", "wallpapers"]);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.contains("/schemas/wallpapers.schema.json"), "{out}");
}

#[test]
fn command_quit_migration_preserves_values_including_the_u64_extremes() {
    for (value, expected) in [
        ("800", 800_u64),
        ("0", 0),
        ("18446744073709551615", u64::MAX),
    ] {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("legacy.jsonc");
        let destination = root.path().join("config");
        fs::write(
            &source,
            format!(r#"{{ "commandQuit": {{ "enabled": true, "holdDuration": {value} }} }}"#),
        )
        .expect("source");
        let (code, out, err) = migrate(&source, &destination, false);
        assert_eq!(code, ExitCode::SUCCESS, "{value}: {err}");
        assert!(
            out.contains("validated commandQuit -> commandQuit.jsonc"),
            "{out}"
        );
        assert!(!out.contains("copied commandQuit"), "{out}");
        let written = fs::read_to_string(destination.join("commandQuit.jsonc")).expect("file");
        let decoded: CommandQuitConfig = serde_json::from_str(&written).expect("typed");
        assert!(decoded.enabled);
        assert_eq!(decoded.hold_duration, expected);
    }
}

#[test]
fn an_invalid_command_quit_value_aborts_before_writing_anything() {
    for invalid in ["-1", "1.5", r#""800""#, "18446744073709551616"] {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("legacy.jsonc");
        let destination = root.path().join("config");
        fs::write(
            &source,
            format!(r#"{{ "commandQuit": {{ "enabled": true, "holdDuration": {invalid} }} }}"#),
        )
        .expect("source");
        let (code, _out, err) = migrate(&source, &destination, false);
        assert_eq!(code, ExitCode::FAILURE, "{invalid}: {err}");
        assert!(err.contains("cannot convert commandQuit"), "{invalid}: {err}");
        assert!(err.contains("holdDuration"), "{invalid}: {err}");
        assert!(!destination.exists(), "{invalid}");
    }
}

#[test]
fn an_absent_command_quit_section_receives_the_same_template_as_init() {
    let root = tempfile::tempdir().expect("temp dir");
    let source = root.path().join("legacy.jsonc");
    let destination = root.path().join("config");
    fs::write(&source, "{}").expect("source");
    let (code, out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(
        out.contains("emitted default template commandQuit.jsonc (disabled)"),
        "{out}"
    );
    assert_eq!(
        fs::read_to_string(destination.join("commandQuit.jsonc")).expect("file"),
        catalog().template(CommandQuitConfig::ID).expect("template")
    );
}

#[test]
fn command_quit_migration_preflights_collisions_and_never_overwrites_the_source() {
    let root = tempfile::tempdir().expect("temp dir");
    let destination = root.path().join("config");
    let source = root.path().join("legacy.jsonc");
    let legacy = r#"{ "commandQuit": { "enabled": true, "holdDuration": 800 } }"#;
    fs::write(&source, legacy).expect("source");
    let (code, _out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");

    let target = destination.join("commandQuit.jsonc");
    fs::write(&target, "{ \"sentinel\": true }\n").expect("sentinel");
    let (code, _out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::FAILURE);
    assert!(err.contains("refusing to overwrite existing files"), "{err}");
    assert_eq!(
        fs::read_to_string(&target).expect("kept"),
        "{ \"sentinel\": true }\n"
    );

    let (code, _out, err) = migrate(&source, &destination, true);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(fs::read_to_string(&target).expect("forced").contains("800"));
    assert_eq!(fs::read_to_string(&source).expect("source"), legacy);

    // A destination that is the legacy source itself is refused, never replaced.
    let own = root.path().join("own");
    fs::create_dir_all(&own).expect("own");
    let own_source = own.join("commandQuit.jsonc");
    fs::write(&own_source, legacy).expect("own source");
    let (code, _out, _err) = migrate(&own_source, &own, true);
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(fs::read_to_string(&own_source).expect("source"), legacy);
}

#[test]
fn command_quit_config_path_and_schema_include_the_file_as_inactive() {
    let root = tempfile::tempdir().expect("temp dir");
    let directory = root.path().join("config");
    fs::create_dir_all(&directory).expect("config");
    fs::write(directory.join("commandQuit.jsonc"), r#"{ "holdDuration": 0 }"#).expect("file");
    let (code, out, err) = run_path(&directory);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.contains("  commandQuit.jsonc (exists, offline)\n"), "{out}");

    fs::write(directory.join("commandQuit.jsonc"), r#"{ "holdDuration": -1 }"#).expect("invalid");
    let (_, out, _) = run_path(&directory);
    assert!(out.contains("  commandQuit.jsonc (exists, invalid)\n"), "{out}");

    let (code, out, err) = run_args(&["stache", "schema", "commandQuit"]);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.contains("/schemas/commandQuit.schema.json"), "{out}");
    assert!(!out.contains("\"maximum\""), "{out}");
}

fn run_args(arguments: &[&str]) -> (ExitCode, String, String) {
    let args: Vec<OsString> = arguments.iter().map(OsString::from).collect();
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

    let production =
        ModuleFactories::production(Arc::new(NoExecutor), stache_macos::AssertionOwner::system());
    let ids: Vec<&str> = production.entries().iter().map(|factory| factory.id().as_str()).collect();
    assert_eq!(
        ids,
        ["proxyAudio", "notunes", "keepAwake", "menuAnywhere"],
        "existing factories keep their order and MenuAnywhere is appended"
    );
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
    let factories =
        ModuleFactories::production(Arc::new(NoExecutor), stache_macos::AssertionOwner::system());
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

fn migrated_keybindings(section: &str) -> KeybindingsConfig {
    let root = tempfile::tempdir().expect("temp dir");
    let source = root.path().join("legacy.jsonc");
    let destination = root.path().join("config");
    fs::write(&source, format!(r#"{{ "keybindings": {section} }}"#)).expect("source");
    let (code, out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::SUCCESS, "{section}: {err}");
    assert!(
        out.contains("validated keybindings -> keybindings.jsonc"),
        "{out}"
    );
    let path = destination.join("keybindings.jsonc");
    let text = fs::read_to_string(&path).expect("keybindings");
    catalog().inspect(KeybindingsConfig::ID, &path, &text).expect("validates");
    serde_json::from_str(&text).expect("typed keybindings")
}

#[test]
fn keybindings_migration_wraps_every_legacy_map_shape() {
    let populated =
        migrated_keybindings(r#"{ "Command+K": "open -a Terminal", "Control+J": ["a", "b"] }"#);
    assert!(populated.enabled);
    assert_eq!(populated.bindings.len(), 2);
    assert_eq!(populated.bindings["Control+J"].commands(), ["a", "b"]);

    let empty = migrated_keybindings("{}");
    assert!(!empty.enabled);
    assert!(empty.bindings.is_empty());

    let blank_only = migrated_keybindings(r#"{ "Command+K": "", "Command+J": ["  "] }"#);
    assert!(
        blank_only.enabled,
        "a nonempty map of blank commands is still enabled"
    );
    assert!(blank_only.bindings["Command+K"].is_empty());

    let reserved = migrated_keybindings(r#"{ "enabled": "echo on", "bindings": ["echo a"] }"#);
    assert!(reserved.enabled);
    assert_eq!(reserved.bindings["enabled"].commands(), ["echo on"]);
    assert_eq!(reserved.bindings["bindings"].commands(), ["echo a"]);
}

#[test]
fn keybindings_migration_rejects_invalid_shapes_before_writing_anything() {
    for invalid in [
        r#"{ "Command+K": 1 }"#,
        r#"{ "Command+K": ["ok", 2] }"#,
        r#"{ "Command+K": null }"#,
        r#"{ "Command+K": { "run": "x" } }"#,
        "[]",
        r#""Command+K""#,
        "3",
        "null",
    ] {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("legacy.jsonc");
        let destination = root.path().join("config");
        fs::write(&source, format!(r#"{{ "keybindings": {invalid} }}"#)).expect("source");
        let (code, _out, err) = migrate(&source, &destination, false);
        assert_eq!(code, ExitCode::FAILURE, "{invalid}: {err}");
        assert!(err.contains("cannot convert keybindings"), "{invalid}: {err}");
        assert!(!destination.exists(), "{invalid}");
    }
}

#[test]
fn keybindings_migration_preflights_collisions_and_honors_force() {
    let root = tempfile::tempdir().expect("temp dir");
    let source = root.path().join("legacy.jsonc");
    let destination = root.path().join("config");
    fs::write(&source, r#"{ "keybindings": { "Command+K": "x" } }"#).expect("source");
    let (code, _out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");

    let path = destination.join("keybindings.jsonc");
    fs::write(&path, "{ \"sentinel\": true }\n").expect("sentinel");
    let (code, _out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::FAILURE);
    assert!(err.contains("refusing to overwrite existing files"), "{err}");
    assert_eq!(
        fs::read_to_string(&path).expect("kept"),
        "{ \"sentinel\": true }\n"
    );

    let (code, _out, err) = migrate(&source, &destination, true);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(fs::read_to_string(&path).expect("forced").contains("Command+K"));
}

#[test]
fn keybindings_migration_refuses_a_source_that_is_a_destination() {
    let root = tempfile::tempdir().expect("temp dir");
    let destination = root.path().join("config");
    fs::create_dir_all(&destination).expect("directory");
    let source = destination.join("keybindings.jsonc");
    fs::write(&source, r#"{ "keybindings": { "Command+K": "x" } }"#).expect("source");
    let (code, _out, err) = migrate(&source, &destination, true);
    assert_eq!(code, ExitCode::FAILURE);
    assert!(err.contains("source") || err.contains("destination"), "{err}");
    assert_eq!(
        fs::read_to_string(&source).expect("source"),
        r#"{ "keybindings": { "Command+K": "x" } }"#
    );
}

#[test]
fn an_absent_keybindings_section_receives_the_disabled_template() {
    let root = tempfile::tempdir().expect("temp dir");
    let source = root.path().join("legacy.jsonc");
    let destination = root.path().join("config");
    fs::write(&source, "{}").expect("source");
    let (code, out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(
        out.contains("emitted default template keybindings.jsonc (disabled)"),
        "{out}"
    );
    assert_eq!(
        fs::read_to_string(destination.join("keybindings.jsonc")).expect("keybindings"),
        catalog().template(KeybindingsConfig::ID).expect("keybindings template")
    );
}

#[test]
fn construction_runs_no_commands_and_the_schema_describes_string_or_array_values() {
    let rendered = catalog().schema_json(KeybindingsConfig::ID).expect("schema").expect("render");
    assert!(rendered.contains("/schemas/keybindings.schema.json"));
    let schema: serde_json::Value = serde_json::from_str(&rendered).expect("schema JSON");
    assert_eq!(schema["properties"]["enabled"]["default"], false);
    assert_eq!(
        schema["properties"]["bindings"]["default"],
        serde_json::json!({})
    );
    let kinds = &schema["$defs"]["StartupCommands"]["anyOf"];
    assert_eq!(kinds[0]["type"], "string");
    assert_eq!(kinds[1]["type"], "array");
}

#[test]
fn the_menu_anywhere_schema_describes_the_three_fields_and_the_caution() {
    let rendered = catalog().schema_json(MenuAnywhereConfig::ID).expect("schema").expect("render");
    assert!(rendered.contains("/schemas/menuAnywhere.schema.json"));
    let schema: serde_json::Value = serde_json::from_str(&rendered).expect("schema JSON");
    let properties = schema["properties"].as_object().expect("properties");
    let mut names: Vec<&str> = properties.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(names, ["enabled", "modifiers", "mouseButton"]);
    assert_eq!(schema["properties"]["enabled"]["default"], false);
    assert_eq!(
        schema["properties"]["modifiers"]["default"],
        serde_json::json!(["control", "command"])
    );
    assert_eq!(schema["properties"]["mouseButton"]["default"], "rightClick");
    assert_eq!(schema["properties"]["modifiers"]["type"], "array");
    let description = schema["properties"]["modifiers"]["description"].as_str().expect("text");
    assert!(
        description.contains("`[]`") && description.contains("system-wide"),
        "{description}"
    );
    for spelling in [
        "control",
        "option",
        "command",
        "shift",
        "rightClick",
        "middleClick",
    ] {
        assert!(
            rendered.contains(&format!("\"{spelling}\"")),
            "{spelling}: {rendered}"
        );
    }
}

#[test]
fn menu_anywhere_schema_and_template_generation_is_deterministic() {
    let first = catalog();
    let second = catalog();
    assert_eq!(
        first.schema_json(MenuAnywhereConfig::ID).expect("schema").expect("render"),
        second.schema_json(MenuAnywhereConfig::ID).expect("schema").expect("render")
    );
    assert_eq!(
        first.template(MenuAnywhereConfig::ID),
        second.template(MenuAnywhereConfig::ID)
    );
}

#[test]
fn menu_anywhere_migration_accepts_empty_modifiers_and_middle_click_unchanged() {
    let root = tempfile::tempdir().expect("temp dir");
    let source = root.path().join("legacy.jsonc");
    let destination = root.path().join("new");
    let section = r#"{ "enabled": true, "modifiers": [], "mouseButton": "middleClick" }"#;
    fs::write(&source, format!(r#"{{ "menuAnywhere": {section} }}"#)).expect("source");
    let (code, out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(
        out.contains("validated menuAnywhere -> menuAnywhere.jsonc"),
        "{out}"
    );
    let written = fs::read_to_string(destination.join("menuAnywhere.jsonc")).expect("written");
    let decoded: MenuAnywhereConfig = serde_json::from_str(&written).expect("typed");
    assert!(decoded.enabled && decoded.modifiers.is_empty());
    assert_eq!(
        decoded.mouse_button,
        stache_menu_anywhere::MenuAnywhereMouseButton::MiddleClick
    );

    let duplicates = root.path().join("duplicates.jsonc");
    let other = root.path().join("other");
    fs::write(
        &duplicates,
        r#"{ "menuAnywhere": { "modifiers": ["shift", "shift"] } }"#,
    )
    .expect("source");
    let (code, _out, err) = migrate(&duplicates, &other, false);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    let written = fs::read_to_string(other.join("menuAnywhere.jsonc")).expect("written");
    let decoded: MenuAnywhereConfig = serde_json::from_str(&written).expect("typed");
    assert_eq!(
        decoded.modifiers.len(),
        2,
        "duplicates are preserved, not normalized"
    );
}

#[test]
fn menu_anywhere_migration_warns_about_unknown_keys_and_preserves_them() {
    let root = tempfile::tempdir().expect("temp dir");
    let source = root.path().join("legacy.jsonc");
    let destination = root.path().join("new");
    fs::write(
        &source,
        r#"{ "menuAnywhere": { "enabled": false, "preset": "wide" } }"#,
    )
    .expect("source");
    let (code, out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(
        out.contains(
            "warning: unknown menuAnywhere key `preset` is preserved in menuAnywhere.jsonc and ignored by the typed configuration"
        ),
        "{out}"
    );
    let written = fs::read_to_string(destination.join("menuAnywhere.jsonc")).expect("written");
    assert!(written.contains("\"preset\""), "{written}");
}

#[test]
fn an_absent_menu_anywhere_section_receives_the_same_template_as_init() {
    let root = tempfile::tempdir().expect("temp dir");
    let source = root.path().join("legacy.jsonc");
    let destination = root.path().join("new");
    fs::write(&source, "{}").expect("source");
    let (code, out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(
        out.contains("emitted default template menuAnywhere.jsonc (disabled)"),
        "{out}"
    );
    assert_eq!(
        fs::read_to_string(destination.join("menuAnywhere.jsonc")).expect("menuAnywhere"),
        catalog().template(MenuAnywhereConfig::ID).expect("template")
    );
}

#[test]
fn an_invalid_menu_anywhere_section_aborts_before_writing_anything() {
    for invalid in [
        r#"{ "modifiers": ["hyper"] }"#,
        r#"{ "modifiers": "command" }"#,
        r#"{ "modifiers": null }"#,
        r#"{ "mouseButton": "leftClick" }"#,
        r#"{ "enabled": 1 }"#,
        r"[]",
        r"null",
        r#""command""#,
    ] {
        let root = tempfile::tempdir().expect("temp dir");
        let source = root.path().join("legacy.jsonc");
        let destination = root.path().join("new");
        let contents = format!(r#"{{ "execOnStartup": ["one"], "menuAnywhere": {invalid} }}"#);
        fs::write(&source, &contents).expect("source");
        let (code, out, err) = migrate(&source, &destination, false);
        assert_eq!(code, ExitCode::FAILURE, "{invalid}");
        assert!(err.contains("cannot convert menuAnywhere"), "{invalid}: {err}");
        assert!(out.is_empty(), "{invalid}: {out}");
        assert!(!destination.exists(), "{invalid}: nothing may be written");
        assert_eq!(fs::read_to_string(&source).expect("source"), contents);
    }
}

#[test]
fn menu_anywhere_migration_preflights_collisions_honors_force_and_refuses_the_source() {
    let root = tempfile::tempdir().expect("temp dir");
    let source = root.path().join("legacy.jsonc");
    let destination = root.path().join("new");
    let contents = r#"{ "menuAnywhere": { "enabled": true } }"#;
    fs::write(&source, contents).expect("source");
    let (code, _out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");

    let target = destination.join("menuAnywhere.jsonc");
    fs::write(&target, "{ \"sentinel\": true }\n").expect("sentinel");
    fs::write(destination.join("notes.txt"), "keep me").expect("unrelated");
    let (code, _out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::FAILURE);
    assert!(err.contains("menuAnywhere.jsonc"), "{err}");
    assert_eq!(
        fs::read_to_string(&target).expect("kept"),
        "{ \"sentinel\": true }\n"
    );
    let (code, _out, err) = migrate(&source, &destination, true);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_ne!(
        fs::read_to_string(&target).expect("forced"),
        "{ \"sentinel\": true }\n"
    );
    assert_eq!(
        fs::read_to_string(destination.join("notes.txt")).expect("kept"),
        "keep me"
    );
    assert_eq!(fs::read_to_string(&source).expect("source"), contents);

    let same = root.path().join("menuAnywhere.jsonc");
    let same_contents = r#"{ "menuAnywhere": { "enabled": true } }"#;
    fs::write(&same, same_contents).expect("source");
    for force in [false, true] {
        let (code, _out, err) = migrate(&same, root.path(), force);
        assert_eq!(code, ExitCode::FAILURE, "force={force}");
        assert!(err.contains("is also a migration destination"), "{err}");
        assert_eq!(fs::read_to_string(&same).expect("source"), same_contents);
    }
}

#[test]
fn menu_anywhere_is_catalogued_inert_and_never_marked_active_while_unbound() {
    use std::sync::Arc;

    use stache_app::bindings::{ConfigFileState, config_file_view};
    use stache_sdk::{
        ChangeSet, ConfigFileId, ConfigPaths, ConfigStore, ConfigStoreError, DiscardLogger,
        EventBus, LogFilter, MainThreadExecutor, Permission, PermissionError, PermissionProvider,
        PermissionStatus, Registry, ThreadError, UnavailableWindows, inspect_paths,
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
    fs::write(
        directory.join("menuAnywhere.jsonc"),
        r#"{ "enabled": true, "modifiers": [] }"#,
    )
    .expect("menuAnywhere");

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
    let file = ConfigFileId::module(MenuAnywhereConfig::ID);

    let report = inspect_paths(store.catalog(), store.paths());
    let entry = report
        .entries
        .iter()
        .find(|entry| entry.file == file)
        .expect("menuAnywhere is a catalog file");
    assert_eq!(entry.state, stache_sdk::PathState::Valid);
    let view = config_file_view(entry, &store);
    assert_eq!(view.state, ConfigFileState::Valid);
    assert!(!view.active, "an unbound module never marks its file active");
    assert!(view.diagnostic.is_none());

    assert!(store.reload_all().iter().all(|outcome| outcome.file != file));
    assert!(store.diagnostic(file).is_none());
    let mut changes = ChangeSet::default();
    changes.files.insert(file);
    assert!(store.apply(&changes).is_empty());
    assert!(matches!(
        store.reload_one(MenuAnywhereConfig::ID),
        Err(ConfigStoreError::Unbound { .. })
    ));
    assert!(
        registry.snapshots().is_empty(),
        "no MenuAnywhere runtime is registered"
    );

    let (code, out, err) = run_path(&directory);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.contains("  menuAnywhere.jsonc (exists, offline)\n"), "{out}");

    let (code, out, err) = run_args(&["stache", "schema", "menuAnywhere"]);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.contains("/schemas/menuAnywhere.schema.json"), "{out}");
}
