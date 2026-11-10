//! Composition tests: the real catalog drives schema, template and migration.

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use stache_bar::{BarConfig, WeatherProvider};
use stache_hold_to_quit::{CommandQuitConfig, HudAnimation, HudPosition};
use stache_keep_awake::KeepAwakeConfig;
use stache_keybindings::KeybindingsConfig;
use stache_menu_anywhere::MenuAnywhereConfig;
use stache_notunes::NoTunesConfig;
use stache_proxy_audio::ProxyAudioConfig;
use stache_sdk::{CommandCatalog, ConfigCatalog, GlobalConfig};
use stache_tiling::{LayoutType, TilingConfig};
use stache_wallpapers::WallpapersConfig;

const LEGACY: &str = include_str!("fixtures/legacy-config.jsonc");

fn catalog() -> ConfigCatalog { stache_app::catalog::catalog().expect("catalog") }

/// A written TOML file as a JSON value.
fn toml_value(text: &str) -> serde_json::Value {
    stache_sdk::config::loader::parse_text(
        stache_sdk::config::ConfigFileId::Global,
        Path::new("written.toml"),
        text,
    )
    .unwrap_or_else(|error| panic!("valid TOML: {error}\n{text}"))
}

/// A written TOML file decoded through its typed configuration.
fn typed<T: serde::de::DeserializeOwned>(text: &str) -> T {
    serde_json::from_value(toml_value(text)).expect("typed decode")
}

#[test]
fn the_catalog_owns_the_global_and_module_files() {
    let catalog = catalog();
    let ids: Vec<&str> = catalog.ids().map(stache_sdk::ModuleId::as_str).collect();
    assert_eq!(ids, [
        "global",
        "bar",
        "commandQuit",
        "keepAwake",
        "keybindings",
        "menuAnywhere",
        "notunes",
        "proxyAudio",
        "tiling",
        "wallpapers"
    ]);
    assert_eq!(
        catalog.file_name(GlobalConfig::ID).as_deref(),
        Some("global.toml")
    );
    assert_eq!(catalog.file_name(BarConfig::ID).as_deref(), Some("bar.toml"));
    assert_eq!(
        catalog.file_name(CommandQuitConfig::ID).as_deref(),
        Some("commandQuit.toml")
    );
    assert_eq!(
        catalog.file_name(KeepAwakeConfig::ID).as_deref(),
        Some("keepAwake.toml")
    );
    assert_eq!(
        catalog.file_name(KeybindingsConfig::ID).as_deref(),
        Some("keybindings.toml")
    );
    assert_eq!(
        catalog.file_name(MenuAnywhereConfig::ID).as_deref(),
        Some("menuAnywhere.toml")
    );
    assert_eq!(
        catalog.file_name(NoTunesConfig::ID).as_deref(),
        Some("notunes.toml")
    );
    assert_eq!(
        catalog.file_name(ProxyAudioConfig::ID).as_deref(),
        Some("proxyAudio.toml")
    );
    assert_eq!(
        catalog.file_name(WallpapersConfig::ID).as_deref(),
        Some("wallpapers.toml")
    );
}

#[test]
fn the_notunes_schema_describes_an_optional_target_string() {
    let rendered = catalog().schema_json(NoTunesConfig::ID).expect("schema").expect("render");
    assert!(rendered.contains("/schemas/notunes.schema.json"));
    let schema: serde_json::Value = serde_json::from_str(&rendered).expect("schema JSON");
    let target = &schema["properties"]["target_app"];
    assert_eq!(
        target["description"],
        "The replacement opened after a blocked application: an absolute `.app`\npath or a bundle identifier. Omitting it blocks without launching\nanything."
    );
    let types = target["type"].as_array().expect("target_app has explicit types");
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
    assert!(rendered.contains("depends_on"));
    assert!(rendered.contains("starts_with"));
}

#[test]
fn templates_come_from_the_typed_defaults() {
    let catalog = catalog();
    let global = catalog.template(GlobalConfig::ID).expect("global template");
    assert!(global.contains("exec_on_startup = []"));
    assert!(global.starts_with("#:schema "), "{global}");
    let keep_awake = catalog.template(KeepAwakeConfig::ID).expect("keepAwake template");
    assert!(keep_awake.contains("enabled = false"), "{keep_awake}");
    assert!(!keep_awake.contains("duration"), "{keep_awake}");
    let keybindings = catalog.template(KeybindingsConfig::ID).expect("keybindings template");
    assert!(keybindings.contains("enabled = false"), "{keybindings}");
    assert!(keybindings.contains("bindings = {}"), "{keybindings}");
    let menu = catalog.template(MenuAnywhereConfig::ID).expect("menuAnywhere template");
    assert!(menu.contains("enabled = false"), "{menu}");
    assert!(menu.contains("modifiers = ["), "{menu}");
    assert!(menu.contains("mouse_button = \"right_click\""), "{menu}");
    assert!(
        menu.contains("system-wide"),
        "the empty-modifiers caution reaches the template: {menu}"
    );
    assert!(menu.contains("button number 2 only"), "{menu}");
    let notunes = catalog.template(NoTunesConfig::ID).expect("notunes template");
    assert!(notunes.contains("enabled = false"));
    assert!(!notunes.contains("target_app"), "{notunes}");
    let pilot = catalog.template(ProxyAudioConfig::ID).expect("pilot template");
    assert!(pilot.contains("enabled = false"));
    assert!(pilot.contains("input = []"));
}

/// The legacy fixture's `commandQuit` section keeps its keys and gains defaults.
fn assert_migrated_command_quit_keeps_legacy_keys(catalog: &ConfigCatalog, destination: &Path) {
    let command_quit_path = destination.join("commandQuit.toml");
    let command_quit = fs::read_to_string(&command_quit_path).expect("commandQuit");
    catalog
        .inspect(CommandQuitConfig::ID, &command_quit_path, &command_quit)
        .expect("migrated commandQuit validates");
    let decoded: CommandQuitConfig = typed(&command_quit);
    assert!(decoded.enabled);
    assert_eq!(decoded.hold_duration, 1500);
    assert!(
        !command_quit.contains("dismiss_delay"),
        "migration copies only legacy keys, so the file has no dismiss_delay"
    );
    assert_eq!(
        decoded.dismiss_delay, 1500,
        "a legacy file has no dismiss_delay, so the default applies"
    );
    assert!(!command_quit.contains("position") && !command_quit.contains("animation"));
    assert_eq!(decoded.position, HudPosition::Bottom);
    assert_eq!(decoded.animation, HudAnimation::Fade);
}

/// The legacy fixture's `menuAnywhere` section migrates to a valid, disabled file.
fn assert_migrated_menu_anywhere_is_disabled(catalog: &ConfigCatalog, destination: &Path) {
    let menu_path = destination.join("menuAnywhere.toml");
    let menu = fs::read_to_string(&menu_path).expect("menuAnywhere");
    catalog
        .inspect(MenuAnywhereConfig::ID, &menu_path, &menu)
        .expect("migrated menuAnywhere validates");
    let decoded: MenuAnywhereConfig = typed(&menu);
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
    let pilot_path = destination.join("proxyAudio.toml");
    let pilot = fs::read_to_string(&pilot_path).expect("pilot");
    catalog
        .inspect(ProxyAudioConfig::ID, &pilot_path, &pilot)
        .expect("migrated pilot validates");
    assert!(pilot.contains("depends_on"));
    assert!(destination.join("global.toml").exists());

    let notunes_path = destination.join("notunes.toml");
    let notunes = fs::read_to_string(&notunes_path).expect("notunes");
    catalog
        .inspect(NoTunesConfig::ID, &notunes_path, &notunes)
        .expect("migrated notunes validates");
    let decoded: NoTunesConfig = typed(&notunes);
    assert_eq!(decoded.target_app.as_deref(), Some("com.spotify.client"));
    let wallpapers_path = destination.join("wallpapers.toml");
    let wallpapers = fs::read_to_string(&wallpapers_path).expect("wallpapers");
    catalog
        .inspect(WallpapersConfig::ID, &wallpapers_path, &wallpapers)
        .expect("migrated wallpapers validates");
    let decoded: WallpapersConfig = typed(&wallpapers);
    assert!(!decoded.enabled, "migration never enables Wallpapers");
    assert_migrated_command_quit_keeps_legacy_keys(&catalog, &destination);
    assert!(destination.join("tiling.toml").exists());
    assert_migrated_bar_is_typed(&catalog, &destination);
    assert_migrated_menu_anywhere_is_disabled(&catalog, &destination);
    let keybindings_path = destination.join("keybindings.toml");
    let keybindings = fs::read_to_string(&keybindings_path).expect("keybindings");
    catalog
        .inspect(KeybindingsConfig::ID, &keybindings_path, &keybindings)
        .expect("migrated keybindings validates");
    let decoded: KeybindingsConfig = typed(&keybindings);
    assert!(decoded.enabled, "a nonempty legacy map enables the new file");
    assert_eq!(decoded.bindings["Command+Control+R"].commands(), [
        "stache reload"
    ]);
    assert!(!destination.join("widgets.toml").exists());

    let out = String::from_utf8(out).expect("stdout");
    assert!(out.contains("validated notunes -> notunes.toml"));
    assert!(
        out.contains("validated menuAnywhere -> menuAnywhere.toml"),
        "{out}"
    );
    assert!(!out.contains("copied menuAnywhere"));
    assert!(out.contains("validated wallpapers -> wallpapers.toml"));
    assert!(out.contains("validated keybindings -> keybindings.toml"));
    assert!(!out.contains("copied keybindings"));
    assert!(!out.contains("copied notunes"));
    assert!(!out.contains("copied wallpapers"));
    assert!(out.contains("validated commandQuit -> commandQuit.toml"));
    assert!(!out.contains("copied commandQuit"));
    assert!(out.contains("validated tiling -> tiling.toml"), "{out}");
    assert!(!out.contains("copied tiling"));
    assert!(out.contains("validated bar -> bar.toml"), "{out}");
    assert!(!out.contains("copied bar"));
    assert!(out.contains("warning: bar `padding` was omitted"), "{out}");
    assert!(out.contains("warning: dropped widgets (not ported)"));
    assert!(out.contains("warning: unknown key `futureKey` was not migrated"));
}

#[test]
fn tiling_migration_validates_keeps_shapes_and_warns_about_corrections() {
    let root = tempfile::tempdir().expect("tempdir");
    let source = root.path().join("legacy.jsonc");
    let legacy = r#"{ "tiling": {
        "enabled": true, "defaultLayout": "grid",
        "gaps": { "inner": { "horizontal": 4, "vertical": 6 }, "outer": 8 },
        "floating": { "presets": [{ "name": "wide", "width": "80%", "height": 600 }] },
        "workspaces": [{ "name": "web", "screen": "secondary", "preset-on-open": "wide" }]
    } }"#;
    fs::write(&source, legacy).expect("source");
    let destination = root.path().join("new");
    let (code, out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.contains("validated tiling -> tiling.toml"), "{out}");
    assert!(
        out.contains("warning: tiling grid layout now tiles at most 10"),
        "{out}"
    );
    assert!(out.contains("default_position"), "{out}");
    assert_eq!(fs::read_to_string(&source).expect("source"), legacy);
    let path = destination.join("tiling.toml");
    let text = fs::read_to_string(&path).expect("tiling");
    catalog().inspect(TilingConfig::ID, &path, &text).expect("validates");
    let decoded: TilingConfig = typed(&text);
    assert!(decoded.enabled && decoded.default_layout == LayoutType::Grid);
    assert_eq!(decoded.workspaces[0].preset_on_open.as_deref(), Some("wide"));

    let invalid = root.path().join("invalid.jsonc");
    fs::write(&invalid, r#"{ "tiling": { "master": { "ratio": 500 } } }"#).expect("invalid");
    let (code, _, err) = migrate(&invalid, &root.path().join("rejected"), false);
    assert_ne!(code, ExitCode::SUCCESS);
    assert!(err.contains("master.ratio"), "{err}");
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
    assert!(out.contains("validated notunes -> notunes.toml"), "{out}");
    let notunes_path = destination.join("notunes.toml");
    let notunes = fs::read_to_string(&notunes_path).expect("notunes");
    let decoded: NoTunesConfig = typed(&notunes);
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
            "warning: unknown notunes key `legacyKey` is preserved in notunes.toml and ignored by the typed configuration"
        ),
        "{out}"
    );
    let written = fs::read_to_string(destination.join("notunes.toml")).expect("notunes");
    assert!(written.contains("legacyKey = true"), "{written}");
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
        assert!(err.contains("target_app"), "{invalid}: {err}");
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
        fs::read_to_string(destination.join("notunes.toml")).expect("notunes"),
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
        let written = fs::read_to_string(destination.join("keepAwake.toml")).expect("keepAwake");
        assert_eq!(written, expected, "{label}");
        let (decoded, warnings) = catalog()
            .decode_typed::<KeepAwakeConfig>(
                KeepAwakeConfig::ID,
                &destination.join("keepAwake.toml"),
                &written,
            )
            .expect("typed keepAwake");
        assert!(warnings.is_empty(), "{label}: {warnings:?}");
        assert!(
            !decoded.enabled,
            "{label}: migration must not infer consent from Bar"
        );
        assert!(
            out.contains("emitted default template keepAwake.toml (disabled)"),
            "{label}: {out}"
        );
        assert_eq!(
            fs::read_to_string(&source).expect("source"),
            source_text,
            "{label}"
        );
        match label {
            "enabled" | "disabled" => {
                let bar =
                    toml_value(&fs::read_to_string(destination.join("bar.toml")).expect("bar"));
                assert_eq!(
                    bar,
                    serde_json::json!({
                        "enabled": label == "enabled",
                        "height": 28,
                        "padding": 12,
                        "weather": { "precise_location": false, "refresh_interval": 20 },
                    }),
                    "{label}: omitted dimensions are materialized"
                );
            }
            _ => assert_eq!(
                fs::read_to_string(destination.join("bar.toml")).expect("bar"),
                catalog().template(BarConfig::ID).expect("bar template"),
                "{label}: a missing section emits the disabled template"
            ),
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
    let sentinel = destination.join("keepAwake.toml");
    fs::write(&sentinel, "{ \"sentinel\": true }\n").expect("sentinel");

    let (code, _out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::FAILURE);
    assert!(err.contains("refusing to overwrite existing files"), "{err}");
    assert!(err.contains("keepAwake.toml"), "{err}");
    assert_eq!(
        fs::read_to_string(&sentinel).expect("sentinel"),
        "{ \"sentinel\": true }\n"
    );
    assert!(
        !destination.join("global.toml").exists(),
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
    let source = directory.join("keepAwake.toml");
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
        directory.join("notunes.toml"),
        "enabled = true\ntarget_app = \"com.example.player\"\n",
    )
    .expect("notunes");
    let (code, out, err) = run_path(&directory);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.contains("  notunes.toml (exists, offline)\n"), "{out}");

    fs::write(directory.join("notunes.toml"), "target_app = \"music\"\n").expect("invalid");
    let (code, out, err) = run_path(&directory);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.contains("  notunes.toml (exists, invalid)\n"), "{out}");
    assert!(out.contains("target_app"), "{out}");
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

    let written = fs::read_to_string(destination.join("wallpapers.toml")).expect("wallpapers");
    let decoded: WallpapersConfig = typed(&written);
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
    let written = fs::read_to_string(destination.join("wallpapers.toml")).expect("wallpapers");
    assert_eq!(
        written,
        catalog().template(WallpapersConfig::ID).expect("wallpapers template")
    );
    let (decoded, warnings) = catalog()
        .decode_typed::<WallpapersConfig>(
            WallpapersConfig::ID,
            &destination.join("wallpapers.toml"),
            &written,
        )
        .expect("typed wallpapers");
    assert!(warnings.is_empty());
    assert!(!decoded.enabled);
    assert!(
        out.contains("emitted default template wallpapers.toml (disabled)"),
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
    let existing = destination.join("wallpapers.toml");
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
        directory.join("wallpapers.toml"),
        "enabled = true\npath = \"images\"\n",
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
    assert!(out.contains("  wallpapers.toml (exists, offline)\n"), "{out}");

    let (code, out, err) = run_args(&["stache", "schema", "wallpapers"]);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.contains("/schemas/wallpapers.schema.json"), "{out}");
}

#[test]
fn command_quit_migration_preserves_values_including_the_toml_integer_maximum() {
    for (value, expected) in [
        ("800", 800_u64),
        ("0", 0),
        ("9223372036854775807", i64::MAX as u64),
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
            out.contains("validated commandQuit -> commandQuit.toml"),
            "{out}"
        );
        assert!(!out.contains("copied commandQuit"), "{out}");
        let written = fs::read_to_string(destination.join("commandQuit.toml")).expect("file");
        let decoded: CommandQuitConfig = typed(&written);
        assert!(decoded.enabled);
        assert_eq!(decoded.hold_duration, expected);
    }
}

#[test]
fn an_invalid_command_quit_value_aborts_before_writing_anything() {
    for invalid in [
        "-1",
        "1.5",
        r#""800""#,
        "9223372036854775808",
        "18446744073709551615",
        "18446744073709551616",
    ] {
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
        assert!(err.contains("hold_duration"), "{invalid}: {err}");
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
        out.contains("emitted default template commandQuit.toml (disabled)"),
        "{out}"
    );
    assert_eq!(
        fs::read_to_string(destination.join("commandQuit.toml")).expect("file"),
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

    let target = destination.join("commandQuit.toml");
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
    let own_source = own.join("commandQuit.toml");
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
    fs::write(directory.join("commandQuit.toml"), "hold_duration = 0\n").expect("file");
    let (code, out, err) = run_path(&directory);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.contains("  commandQuit.toml (exists, offline)\n"), "{out}");

    fs::write(directory.join("commandQuit.toml"), "hold_duration = -1\n").expect("invalid");
    let (_, out, _) = run_path(&directory);
    assert!(out.contains("  commandQuit.toml (exists, invalid)\n"), "{out}");

    let (code, out, err) = run_args(&["stache", "schema", "commandQuit"]);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.contains("/schemas/commandQuit.schema.json"), "{out}");
    assert!(
        out.contains("9223372036854775807"),
        "the i64 bound is declared: {out}"
    );
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

    fs::write(directory.join("keepAwake.toml"), "enabled = true\n").expect("keepAwake");

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
        out.contains("validated keybindings -> keybindings.toml"),
        "{out}"
    );
    let path = destination.join("keybindings.toml");
    let text = fs::read_to_string(&path).expect("keybindings");
    catalog().inspect(KeybindingsConfig::ID, &path, &text).expect("validates");
    typed(&text)
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

    let path = destination.join("keybindings.toml");
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
    let source = destination.join("keybindings.toml");
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
        out.contains("emitted default template keybindings.toml (disabled)"),
        "{out}"
    );
    assert_eq!(
        fs::read_to_string(destination.join("keybindings.toml")).expect("keybindings"),
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
    assert_eq!(names, ["enabled", "modifiers", "mouse_button"]);
    assert_eq!(schema["properties"]["enabled"]["default"], false);
    assert_eq!(
        schema["properties"]["modifiers"]["default"],
        serde_json::json!(["control", "command"])
    );
    assert_eq!(schema["properties"]["mouse_button"]["default"], "right_click");
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
        "right_click",
        "middle_click",
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
        out.contains("validated menuAnywhere -> menuAnywhere.toml"),
        "{out}"
    );
    let written = fs::read_to_string(destination.join("menuAnywhere.toml")).expect("written");
    let decoded: MenuAnywhereConfig = typed(&written);
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
    let written = fs::read_to_string(other.join("menuAnywhere.toml")).expect("written");
    let decoded: MenuAnywhereConfig = typed(&written);
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
            "warning: unknown menuAnywhere key `preset` is preserved in menuAnywhere.toml and ignored by the typed configuration"
        ),
        "{out}"
    );
    let written = fs::read_to_string(destination.join("menuAnywhere.toml")).expect("written");
    assert!(written.contains("preset = \"wide\""), "{written}");
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
        out.contains("emitted default template menuAnywhere.toml (disabled)"),
        "{out}"
    );
    assert_eq!(
        fs::read_to_string(destination.join("menuAnywhere.toml")).expect("menuAnywhere"),
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

    let target = destination.join("menuAnywhere.toml");
    fs::write(&target, "{ \"sentinel\": true }\n").expect("sentinel");
    fs::write(destination.join("notes.txt"), "keep me").expect("unrelated");
    let (code, _out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::FAILURE);
    assert!(err.contains("menuAnywhere.toml"), "{err}");
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

    let same = root.path().join("menuAnywhere.toml");
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
        directory.join("menuAnywhere.toml"),
        "enabled = true\nmodifiers = []\n",
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
    assert!(out.contains("  menuAnywhere.toml (exists, offline)\n"), "{out}");

    let (code, out, err) = run_args(&["stache", "schema", "menuAnywhere"]);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.contains("/schemas/menuAnywhere.schema.json"), "{out}");
}

fn assert_migrated_bar_is_typed(catalog: &ConfigCatalog, destination: &Path) {
    let path = destination.join("bar.toml");
    let text = fs::read_to_string(&path).expect("bar");
    catalog.inspect(BarConfig::ID, &path, &text).expect("migrated bar validates");
    let decoded: BarConfig = typed(&text);
    assert!(decoded.enabled, "explicit enabled is kept; nothing runs");
    assert_eq!(decoded.height, 32, "explicit height is kept");
    assert_eq!(decoded.padding, 12, "omitted padding is materialized");
    assert_eq!(decoded.weather.provider, WeatherProvider::Auto);
}

fn bar_migration(legacy_bar: &str) -> (ExitCode, String, String, serde_json::Value) {
    let root = tempfile::tempdir().expect("temp dir");
    let source = root.path().join("legacy.jsonc");
    let destination = root.path().join("config");
    fs::write(&source, format!(r#"{{ "bar": {legacy_bar} }}"#)).expect("source");
    let (code, out, err) = migrate(&source, &destination, false);
    let bar = fs::read_to_string(destination.join("bar.toml"))
        .ok()
        .map_or(serde_json::Value::Null, |text| typed(&text));
    (code, out, err, bar)
}

#[test]
fn bar_migration_keeps_explicit_zero_and_max_values_without_warnings() {
    let (code, out, err, bar) = bar_migration(r#"{ "height": 0, "padding": 65535 }"#);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(bar["height"], 0);
    assert_eq!(bar["padding"], 65535);
    assert!(!out.contains("warning: bar"), "{out}");
}

#[test]
fn bar_migration_materializes_omissions_retains_unknown_keys_and_warns() {
    let (code, out, err, bar) = bar_migration(
        r#"{ "future": 1, "widgets": [], "weather": { "provider": "open-meteo", "apiKeys": ".env" } }"#,
    );
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!((&bar["height"], &bar["padding"]), (&28.into(), &12.into()));
    assert_eq!(bar["future"], 1);
    assert_eq!(bar["weather"]["api_keys"], ".env", "relative text is retained");
    assert!(out.contains("warning: bar `height` was omitted"), "{out}");
    assert!(out.contains("warning: bar `padding` was omitted"), "{out}");
    assert!(
        out.contains("warning: bar `weather.api_keys` value `.env` is relative"),
        "{out}"
    );
    assert!(
        out.contains("warning: unknown bar key `future` is preserved in bar.toml"),
        "{out}"
    );
}

#[test]
fn bar_migration_rejects_invalid_values_before_writing_anything() {
    for invalid in [
        r#"{ "height": -1 }"#,
        r#"{ "padding": 65536 }"#,
        r#"{ "enabled": "yes" }"#,
        r#"{ "weather": { "provider": "other" } }"#,
    ] {
        let (code, _out, err, _bar) = bar_migration(invalid);
        assert_eq!(code, ExitCode::FAILURE, "{invalid}");
        assert!(err.contains("cannot convert bar"), "{invalid}: {err}");
    }
}

#[test]
fn bar_registration_adds_no_runtime_command_or_active_state() {
    let catalog = catalog();
    assert!(catalog.contains(BarConfig::ID));
    let template = catalog.template(BarConfig::ID).expect("template");
    assert!(template.contains("enabled = false"), "{template}");
}

const LEGACY_FULL: &str = include_str!("fixtures/legacy-full.jsonc");

/// The key and enum spelling every registered schema must follow.
fn is_snake(name: &str) -> bool {
    let mut previous_underscore = true;
    for (index, ch) in name.chars().enumerate() {
        match ch {
            'a'..='z' => previous_underscore = false,
            '0'..='9' if index > 0 && !previous_underscore => previous_underscore = false,
            '_' if !previous_underscore => previous_underscore = true,
            _ => return false,
        }
    }
    !previous_underscore
}

fn collect_spellings(node: &serde_json::Value, bad: &mut Vec<String>, at: &str) {
    match node {
        serde_json::Value::Object(object) => {
            if let Some(properties) = object.get("properties").and_then(|v| v.as_object()) {
                for name in properties.keys().filter(|name| *name != "$schema") {
                    if !is_snake(name) {
                        bad.push(format!("{at}: property `{name}`"));
                    }
                }
            }
            let values = object
                .get("enum")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .chain(object.get("const"));
            for value in values.filter_map(serde_json::Value::as_str) {
                if !is_snake(value) {
                    bad.push(format!("{at}: value `{value}`"));
                }
            }
            for (key, child) in object {
                // `additionalProperties` maps carry user data, not keys.
                if key == "default" {
                    continue;
                }
                collect_spellings(child, bad, &format!("{at}/{key}"));
            }
        }
        serde_json::Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_spellings(item, bad, &format!("{at}/{index}"));
            }
        }
        _ => {}
    }
}

#[test]
fn every_registered_schema_uses_snake_case_names_and_values() {
    let catalog = catalog();
    let mut bad = Vec::new();
    for id in catalog.ids() {
        let schema = catalog.schema(id).expect("schema");
        collect_spellings(schema.as_value(), &mut bad, id.as_str());
    }
    assert!(bad.is_empty(), "non-snake_case spellings: {bad:#?}");
    assert!(is_snake("exec_on_startup") && is_snake("ease_out") && is_snake("a1_b2"));
    for wrong in [
        "execOnStartup",
        "ease-out",
        "_a",
        "a_",
        "A",
        "a__b",
        "1a",
        "",
    ] {
        assert!(!is_snake(wrong), "{wrong}");
    }
}

#[test]
fn every_template_decodes_to_its_default_without_warnings() {
    let catalog = catalog();
    for id in catalog.ids() {
        let template = catalog.template(id).expect("template");
        let path = Path::new("template.toml");
        let schema_line = format!(
            "#:schema https://raw.githubusercontent.com/marcosmoura/stache/main/schemas/{id}.schema.json\n"
        );
        assert!(template.starts_with(&schema_line), "{id}: {template}");
        let (value, warnings) = catalog
            .decode(id, path, &template)
            .unwrap_or_else(|error| panic!("{id}: {error}"))
            .into_parts();
        assert!(warnings.is_empty(), "{id}: {warnings:?}");
        drop(value);
        assert!(
            catalog.inspect(id, path, &template).expect("inspect").is_empty(),
            "{id}"
        );
        let decoded = toml_value(&template);
        let expected = catalog.default_value(id).expect("default");
        assert!(
            !contains_null(expected),
            "{id}: a default must never contain null"
        );
        assert_eq!(
            &decoded, expected,
            "{id}: the template decodes to the typed default"
        );
    }
}

fn contains_null(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::Array(items) => items.iter().any(contains_null),
        serde_json::Value::Object(map) => map.values().any(contains_null),
        _ => false,
    }
}

fn inspect_value(
    id: stache_sdk::ModuleId,
    value: &serde_json::Value,
) -> Result<Vec<String>, stache_sdk::config::ConfigDiagnostic> {
    catalog().inspect_json(id, Path::new("x.toml"), value.clone())
}

fn assert_warns_about(id: stache_sdk::ModuleId, value: &serde_json::Value, old: &[&str]) {
    let warnings = inspect_value(id, value).expect("old key still decodes");
    for key in old {
        assert!(
            warnings.iter().any(|w| w.contains(key)),
            "{id} {key}: {warnings:?}"
        );
    }
}

fn assert_fails_at(id: stache_sdk::ModuleId, value: &serde_json::Value, field: &str) {
    let error = inspect_value(id, value).expect_err("old value is rejected");
    assert!(error.to_string().contains(field), "{id} {value}: {error}");
}

#[test]
fn new_spellings_decode_silently_for_every_type_family() {
    use serde_json::json;
    for (id, value) in [
        (
            GlobalConfig::ID,
            json!({ "exec_on_startup": ["x"], "logging": { "level": "warn" } }),
        ),
        (
            BarConfig::ID,
            json!({ "weather": { "provider": "visual_crossing", "api_keys": "",
                "default_location": "" } }),
        ),
        (
            CommandQuitConfig::ID,
            json!({ "hold_duration": 1, "dismiss_delay": 2 }),
        ),
        (MenuAnywhereConfig::ID, json!({ "mouse_button": "middle_click" })),
        (NoTunesConfig::ID, json!({ "target_app": "com.example.player" })),
        (
            ProxyAudioConfig::ID,
            json!({ "output": [{ "name": "a", "strategy": "starts_with",
                "depends_on": { "name": "b" } }] }),
        ),
        (
            TilingConfig::ID,
            json!({ "default_layout": "split_vertical",
                "animations": { "easing": "ease_out" },
                "floating": { "default_position": "default",
                    "presets": [{ "name": "p", "width": 1, "height": 1 }] },
                "workspaces": [{ "name": "a", "preset_on_open": "p" }],
                "ignore": [{ "app_id": "x", "app_name": "y" }],
                "borders": { "split_vertical": false, "split_horizontal": false } }),
        ),
    ] {
        let warnings = inspect_value(id, &value).unwrap_or_else(|error| panic!("{id}: {error}"));
        assert!(warnings.is_empty(), "{id}: {warnings:?}");
    }
    let missing = json!({ "workspaces": [{ "name": "a", "preset_on_open": "missing" }] });
    let error = inspect_value(TilingConfig::ID, &missing).expect_err("missing preset");
    assert!(error.to_string().contains("preset_on_open"), "{error}");
}

#[test]
fn old_keys_warn_for_every_type_family() {
    use serde_json::json;
    assert_warns_about(GlobalConfig::ID, &json!({ "execOnStartup": [] }), &[
        "execOnStartup",
    ]);
    assert_warns_about(
        CommandQuitConfig::ID,
        &json!({ "holdDuration": 1, "dismissDelay": 1 }),
        &["holdDuration", "dismissDelay"],
    );
    assert_warns_about(
        MenuAnywhereConfig::ID,
        &json!({ "mouseButton": "right_click" }),
        &["mouseButton"],
    );
    assert_warns_about(
        NoTunesConfig::ID,
        &json!({ "targetApp": "com.example.player" }),
        &["targetApp"],
    );
    assert_warns_about(
        BarConfig::ID,
        &json!({ "weather": { "apiKeys": "", "defaultLocation": "" } }),
        &["apiKeys", "defaultLocation"],
    );
    assert_warns_about(
        ProxyAudioConfig::ID,
        &json!({ "output": [{ "name": "a", "dependsOn": { "name": "b" } }] }),
        &["dependsOn"],
    );
    assert_warns_about(
        TilingConfig::ID,
        &json!({ "defaultLayout": "grid", "workspaces": [{ "name": "a", "preset-on-open": "p" }],
            "ignore": [{ "appId": "x" }], "floating": { "defaultPosition": "default" } }),
        &[
            "defaultLayout",
            "preset-on-open",
            "appId",
            "defaultPosition",
        ],
    );
}

#[test]
fn old_enum_values_fail_with_a_field_path_for_every_type_family() {
    use serde_json::json;
    assert_fails_at(
        BarConfig::ID,
        &json!({ "weather": { "provider": "visual-crossing" } }),
        "weather.provider",
    );
    assert_fails_at(
        BarConfig::ID,
        &json!({ "weather": { "provider": "openMeteo" } }),
        "weather.provider",
    );
    assert_fails_at(
        MenuAnywhereConfig::ID,
        &json!({ "mouse_button": "rightClick" }),
        "mouse_button",
    );
    assert_fails_at(
        ProxyAudioConfig::ID,
        &json!({ "output": [{ "name": "a", "strategy": "startsWith" }] }),
        "output[0].strategy",
    );
    assert_fails_at(
        TilingConfig::ID,
        &json!({ "default_layout": "split-vertical" }),
        "default_layout",
    );
    assert_fails_at(
        TilingConfig::ID,
        &json!({ "animations": { "easing": "ease-out" } }),
        "animations.easing",
    );
}

#[test]
fn the_full_legacy_fixture_migrates_to_snake_case_files_with_zero_warnings() {
    let root = tempfile::tempdir().expect("tempdir");
    let source = root.path().join("legacy.jsonc");
    let destination = root.path().join("new");
    fs::write(&source, LEGACY_FULL).expect("source");
    let (code, out, err) = migrate(&source, &destination, false);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.contains("converted exec_on_startup -> global.toml"), "{out}");
    assert!(
        !out.contains("emitted default template"),
        "every section is present: {out}"
    );
    assert!(!out.contains("unknown"), "{out}");
    assert_eq!(fs::read_to_string(&source).expect("source"), LEGACY_FULL);

    let catalog = catalog();
    for id in catalog.ids() {
        let name = catalog.file_name(id).expect("file name");
        let path = destination.join(&name);
        let text = fs::read_to_string(&path).unwrap_or_else(|error| panic!("{name}: {error}"));
        let warnings = catalog
            .inspect(id, &path, &text)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(warnings.is_empty(), "{name}: {warnings:?}");
    }

    let read = |id: stache_sdk::ModuleId| {
        let path = destination.join(catalog.file_name(id).expect("file name"));
        fs::read_to_string(path).expect("file")
    };
    let global: GlobalConfig = typed(&read(GlobalConfig::ID));
    assert_eq!(global.exec_on_startup.commands(), [
        "open -a Terminal",
        "echo camelCase-stays-here"
    ]);
    let bar: BarConfig = typed(&read(BarConfig::ID));
    assert_eq!(bar.weather.provider, WeatherProvider::VisualCrossing);
    assert_eq!(bar.weather.default_location, "Lisbon");
    let quit: CommandQuitConfig = typed(&read(CommandQuitConfig::ID));
    assert_eq!((quit.hold_duration, quit.dismiss_delay), (900, 300));
    assert_eq!(quit.animation, HudAnimation::Slide);
    let keybindings = read(KeybindingsConfig::ID);
    for key in ["CapsLock+a", "§", "cmd+shift+.", "enabledApps"] {
        let bare = key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        let spelled = if bare {
            (*key).to_owned()
        } else {
            format!("\"{key}\"")
        };
        assert!(
            keybindings.contains(&format!("{spelled} = ")),
            "{key}: {keybindings}"
        );
    }
    assert!(keybindings.contains("echo enabledApps"), "{keybindings}");
    let keybindings: KeybindingsConfig = typed(&keybindings);
    assert!(keybindings.enabled && keybindings.bindings.contains_key("enabledApps"));
    let menu: MenuAnywhereConfig = typed(&read(MenuAnywhereConfig::ID));
    assert_eq!(
        menu.mouse_button,
        stache_menu_anywhere::MenuAnywhereMouseButton::MiddleClick
    );
    let tiling: TilingConfig = typed(&read(TilingConfig::ID));
    assert_eq!(tiling.default_layout, LayoutType::SplitVertical);
    assert_eq!(tiling.workspaces[0].layout, Some(LayoutType::SplitHorizontal));
    assert_eq!(tiling.workspaces[0].preset_on_open.as_deref(), Some("wide"));
    assert_eq!(
        tiling.workspaces[0].rules[0].app_id.as_deref(),
        Some("com.apple.dt.Xcode")
    );
    assert_eq!(tiling.ignore.len(), 2);
    assert!(tiling.borders.split_vertical.is_some());
    let notunes = read(NoTunesConfig::ID);
    assert!(
        notunes.contains("target_app = \"com.spotify.client\""),
        "{notunes}"
    );
    let pilot = read(ProxyAudioConfig::ID);
    assert!(
        pilot.contains("starts_with") && pilot.contains("depends_on"),
        "{pilot}"
    );
}

/// Decodes TOML text, writes it back out through the catalog's TOML emitter,
/// decodes that again, and returns both values for comparison.
fn toml_round_trip<T>(id: stache_sdk::ModuleId, text: &str) -> (T, T, String)
where T: stache_sdk::ModuleConfig + serde::de::DeserializeOwned + serde::Serialize {
    let catalog = catalog();
    let path = Path::new("round-trip.toml");
    let (first, warnings): (T, _) = catalog.decode_typed(id, path, text).expect("first decode");
    assert!(warnings.is_empty(), "{warnings:?}");
    let emitted = catalog
        .render_value(id, &serde_json::to_value(&first).expect("to JSON"))
        .expect("emit TOML");
    let (second, warnings): (T, _) =
        catalog.decode_typed(id, path, &emitted).expect("second decode");
    assert!(warnings.is_empty(), "{emitted}\n{warnings:?}");
    (first, second, emitted)
}

#[test]
fn startup_commands_round_trip_as_a_string_and_as_an_array() {
    for text in [
        "exec_on_startup = \"open -a Terminal\"\n",
        "exec_on_startup = [\"one\", \"two\"]\n",
        "exec_on_startup = []\n",
    ] {
        let (first, second, emitted) = toml_round_trip::<GlobalConfig>(GlobalConfig::ID, text);
        assert_eq!(
            first.exec_on_startup.commands(),
            second.exec_on_startup.commands(),
            "{emitted}"
        );
        assert_eq!(
            toml_value(text)["exec_on_startup"],
            toml_value(&emitted)["exec_on_startup"]
        );
    }
}

#[test]
fn tiling_untagged_unions_round_trip_through_toml() {
    let cases = [
        // Uniform, per-axis and per-side gaps.
        "[gaps]\ninner = 4\nouter = 8\n",
        "[gaps]\ninner = { horizontal = 4, vertical = 6 }\nouter = { top = 1, right = 2, bottom = 3, left = 4 }\n",
        // Per-screen gaps are an array of tables.
        "[[gaps]]\nscreen = \"main\"\ninner = 4\nouter = { top = 1, right = 2, bottom = 3, left = 4 }\n\n[[gaps]]\nscreen = \"Studio Display\"\ninner = { horizontal = 2, vertical = 3 }\n",
        // Integer and percentage dimensions, optional positions.
        "[floating]\n[[floating.presets]]\nname = \"wide\"\nwidth = \"80%\"\nheight = 600\nx = \"10%\"\ny = 20\n\n[[floating.presets]]\nname = \"tall\"\nwidth = 1440\nheight = \"100%\"\n",
        // Every border state form.
        "[borders]\nenabled = true\nunfocused = false\n[borders.focused]\nwidth = 4\ncolor = \"#b4befe\"\n[borders.monocle]\nwidth = 3\n[borders.monocle.gradient]\nfrom = \"#b4befe\"\nto = \"#cba6f7\"\nangle = 45.5\n[borders.monocle.animation]\nduration = 800\neasing = \"ease_in_out\"\n[borders.floating]\nwidth = 2\nglow = \"#94e2d5\"\n",
    ];
    for text in cases {
        let (first, second, emitted) = toml_round_trip::<TilingConfig>(TilingConfig::ID, text);
        assert_eq!(first, second, "{text}\n--- emitted ---\n{emitted}");
        assert_eq!(
            toml_value(&emitted),
            serde_json::to_value(&first).expect("to JSON"),
            "{emitted}"
        );
    }
}

#[test]
fn keybinding_keys_survive_a_serialize_parse_round_trip() {
    let text = "enabled = true\n\n[bindings]\n\"CapsLock+a\" = \"echo a\"\n\"§\" = [\"echo b\", \"echo c\"]\n\"cmd+shift+.\" = \"echo dot\"\n\"with spaces\" = \"echo spaced\"\n";
    let (first, second, emitted) =
        toml_round_trip::<KeybindingsConfig>(KeybindingsConfig::ID, text);
    assert_eq!(first, second, "{emitted}");
    for key in ["CapsLock+a", "§", "cmd+shift+.", "with spaces"] {
        assert!(first.bindings.contains_key(key), "{key}");
        assert!(emitted.contains(&format!("\"{key}\" = ")), "{key}: {emitted}");
    }
    assert_eq!(first.bindings["§"].commands(), ["echo b", "echo c"]);
}

/// The names of the root keys and table headers in a template, in file order.
fn root_order(template: &str) -> Vec<String> {
    template
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .filter_map(|line| {
            if line.starts_with('[') {
                Some(line.to_owned())
            } else if line.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                line.split(" = ").next().map(str::to_owned)
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn templates_follow_struct_declaration_order_with_scalars_before_tables() {
    let catalog = catalog();
    let order = |id| root_order(&catalog.template(id).expect("template"));
    assert_eq!(order(GlobalConfig::ID), [
        "exec_on_startup",
        "[logging]",
        "level",
        "debug"
    ]);
    assert_eq!(order(CommandQuitConfig::ID), [
        "enabled",
        "hold_duration",
        "dismiss_delay",
        "position",
        "animation"
    ]);
    assert_eq!(order(ProxyAudioConfig::ID), ["enabled", "input", "output"]);
    assert_eq!(order(TilingConfig::ID), [
        "enabled",
        "default_layout",
        "workspaces",
        "ignore",
        "[animations]",
        "enabled",
        "duration",
        "easing",
        "[gaps]",
        "inner",
        "outer",
        "[floating]",
        "default_position",
        "presets",
        "[master]",
        "ratio",
        "position",
        "[borders]",
        "enabled",
        "ignore",
        "[borders.focused]",
        "width",
        "color",
        "[borders.unfocused]",
        "width",
        "color",
        "[borders.monocle]",
        "width",
        "color",
        "[borders.floating]",
        "width",
        "color"
    ]);
}

/// Keys inside untagged unions (gaps is one table or a per-display list) still
/// carry their schema descriptions.
#[test]
fn union_table_keys_keep_their_template_comments() {
    let template = catalog().template(TilingConfig::ID).expect("template");
    let gaps = &template[template.find("[gaps]\n").expect("gaps table")..];
    let gaps = &gaps[..gaps.find("\n[").expect("next table")];
    for key in ["inner", "outer"] {
        let line = gaps.lines().position(|line| line.starts_with(&format!("{key} ="))).expect(key);
        assert!(
            gaps.lines().nth(line - 1).is_some_and(|above| above.starts_with("# ")),
            "{key} has no comment:\n{gaps}"
        );
    }
}
