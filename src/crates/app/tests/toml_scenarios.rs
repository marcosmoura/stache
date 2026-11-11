//! End-to-end TOML configuration scenarios on the real catalog.
//!
//! These drive the shipped surfaces together instead of faking one slice: the
//! `config init` command writes the templates, a user edits them, the store
//! reloads them, the tray toggle rewrites the file, and `core.status` reports
//! the result. The legacy fixture migrates to `.toml` files that load cleanly.

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use stache_app::config_edit::{CommitOutcome, plan_enabled};
use stache_app::host::ModuleFactories;
use stache_proxy_audio::ProxyAudioConfig;
use stache_sdk::config::ConfigPaths;
use stache_sdk::ipc::{ConfigCore, CoreApplyOutcome, CoreFileState, CoreHandler};
use stache_sdk::{
    CommandCatalog, ConfigCatalog, ConfigStore, DiscardLogger, EventBus, LogFilter,
    MainThreadExecutor, ModuleStatus, Permission, PermissionError, PermissionProvider,
    PermissionStatus, Registry, ThreadError, UnavailableWindows,
};
use stache_tiling::TilingConfig;

const LEGACY_FULL: &str = include_str!("fixtures/legacy-full.jsonc");

struct NoExecutor;

impl MainThreadExecutor for NoExecutor {
    fn is_main_thread(&self) -> bool { false }

    fn execute(&self, _: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> { Ok(()) }
}

struct AllowAll;

impl PermissionProvider for AllowAll {
    fn status(&self, _: Permission) -> PermissionStatus { PermissionStatus::Granted }

    fn request(&self, _: Permission) -> Result<PermissionStatus, PermissionError> {
        Ok(PermissionStatus::Granted)
    }
}

fn catalog() -> ConfigCatalog { stache_app::catalog::catalog().expect("catalog") }

fn cli(arguments: &[OsString]) -> (ExitCode, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = stache_cli::run_with_io(
        arguments.to_vec(),
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

fn args(directory: &Path, tail: &[&str]) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "stache".into(),
        "--config".into(),
        directory.as_os_str().to_owned(),
    ];
    args.extend(tail.iter().map(OsString::from));
    args
}

fn is_toml(name: &str) -> bool {
    Path::new(name).extension().is_some_and(|extension| extension == "toml")
}

fn registry() -> Arc<Registry> {
    Arc::new(Registry::new(
        Arc::new(DiscardLogger),
        Arc::new(EventBus::new()),
        Arc::new(NoExecutor),
        Arc::new(AllowAll),
        Arc::new(UnavailableWindows),
    ))
}

/// init -> edit -> reload -> tray enable preserves comments -> status accepted,
/// then a malformed edit keeps the last good revision.
#[test]
#[allow(clippy::too_many_lines)]
fn init_edit_reload_tray_enable_and_status_work_together() {
    let root = tempfile::tempdir().expect("temp dir");
    let directory = root.path().join("config");
    let (code, _out, err) = cli(&args(&directory, &["config", "init"]));
    assert_eq!(code, ExitCode::SUCCESS, "{err}");

    // A user edits two generated files, adding their own comments.
    let proxy_path = directory.join("proxyAudio.toml");
    let generated = fs::read_to_string(&proxy_path).expect("proxyAudio.toml");
    assert!(generated.starts_with("#:schema "), "{generated}");
    let edited = format!(
        "{}\n# My speakers come first.\n[[output]]\nname = \"Speakers\" # built-in\nstrategy = \"exact\"\n",
        generated.replace("output = []\n", ""),
    );
    fs::write(&proxy_path, &edited).expect("edit proxyAudio");
    let tiling_path = directory.join("tiling.toml");
    let tiling = fs::read_to_string(&tiling_path).expect("tiling.toml");
    fs::write(
        &tiling_path,
        tiling
            .replace(
                "default_layout = \"dwindle\"",
                "default_layout = \"split_vertical\"",
            )
            .replace("ratio = 60", "ratio = 55 # wider stack"),
    )
    .expect("edit tiling");

    // The store binds the production factories (registration never starts them).
    let registry = registry();
    let store = Arc::new(ConfigStore::new(
        Arc::clone(&registry),
        catalog(),
        ConfigPaths::new(&directory),
        Arc::new(LogFilter::default()),
    ));
    let factories =
        ModuleFactories::production(Arc::new(NoExecutor), stache_macos::AssertionOwner::system());
    for factory in factories.entries() {
        factory.register(&registry, &store).expect("register");
    }
    let config_core = ConfigCore::new(Arc::clone(&store));
    config_core.reload().expect("reload");

    let proxy_snapshot = || {
        registry
            .snapshots()
            .into_iter()
            .find(|snapshot| snapshot.id == ProxyAudioConfig::ID)
            .expect("proxyAudio is bound")
    };
    let before = proxy_snapshot();
    assert!(!before.enabled);

    // The tray toggle edits the file in place and keeps the user's comments.
    let catalog = catalog();
    let paths = ConfigPaths::new(&directory);
    let pending = plan_enabled(&catalog, &paths, ProxyAudioConfig::ID, true).expect("plan");
    assert!(pending.is_change());
    assert_eq!(pending.commit().expect("commit"), CommitOutcome::Written);
    let toggled = fs::read_to_string(&proxy_path).expect("toggled");
    assert_eq!(toggled, edited.replace("enabled = false", "enabled = true"));
    assert!(toggled.starts_with("#:schema "), "the schema stays on line 1");
    assert!(toggled.contains("# My speakers come first.") && toggled.contains("# built-in"));

    // Reload applies the committed bytes and status reports them accepted.
    let report = config_core.reload().expect("reload");
    let proxy = report
        .files
        .iter()
        .find(|file| file.file == "proxyAudio.toml")
        .expect("outcome");
    assert!(
        matches!(proxy.outcome, CoreApplyOutcome::Applied { .. }),
        "{proxy:?}"
    );
    assert!(proxy_snapshot().enabled);
    assert!(proxy_snapshot().revision > before.revision);
    let accepted_revision = proxy_snapshot().revision;

    let status = config_core.status().expect("status");
    for name in ["proxyAudio.toml", "tiling.toml", "global.toml"] {
        let file = status.files.iter().find(|file| file.file == name).expect(name);
        assert_eq!(file.state, CoreFileState::Valid, "{name}: {file:?}");
        assert!(file.diagnostic.is_none(), "{name}: {file:?}");
        assert!(file.warnings.is_empty(), "{name}: {file:?}");
    }
    let proxy_status = status
        .files
        .iter()
        .find(|file| file.file == "proxyAudio.toml")
        .expect("proxyAudio");
    assert!(proxy_status.active, "{proxy_status:?}");
    assert!(status.files.iter().all(|file| is_toml(&file.file)), "{status:?}");

    // A malformed save is a parse diagnostic with line and column, and the
    // last good revision stays active.
    fs::write(&proxy_path, "enabled = true\n[[output]\nname = \"x\"\n").expect("break");
    let rejected = config_core.reload().expect("reload");
    let proxy = rejected
        .files
        .iter()
        .find(|file| file.file == "proxyAudio.toml")
        .expect("outcome");
    assert_eq!(proxy.outcome, CoreApplyOutcome::Rejected);
    let diagnostic = proxy.diagnostic.as_deref().expect("diagnostic");
    assert!(diagnostic.contains("proxyAudio.toml:2:"), "{diagnostic}");
    assert_eq!(proxy_snapshot().revision, accepted_revision);
    assert!(proxy_snapshot().enabled);
    assert!(!matches!(proxy_snapshot().status, ModuleStatus::Failed(_)));
    let status = config_core.status().expect("status");
    let file = status
        .files
        .iter()
        .find(|file| file.file == "proxyAudio.toml")
        .expect("proxyAudio");
    assert_eq!(file.state, CoreFileState::Invalid);

    // Tiling's valid edit loaded as written, with typed values.
    let (tiling, warnings) = catalog
        .decode_typed::<TilingConfig>(
            TilingConfig::ID,
            &tiling_path,
            &fs::read_to_string(&tiling_path).expect("tiling"),
        )
        .expect("tiling decodes");
    assert!(warnings.is_empty());
    assert_eq!(tiling.master.ratio, 55);
    assert_eq!(tiling.default_layout, stache_tiling::LayoutType::SplitVertical);
}

/// The realistic legacy fixture migrates to `.toml` files that load with zero
/// warnings, and `config path` shows every file as existing and offline.
#[test]
fn the_legacy_fixture_migrates_to_toml_files_that_load_without_warnings() {
    let root = tempfile::tempdir().expect("temp dir");
    let source = root.path().join("legacy.jsonc");
    let destination = root.path().join("new");
    fs::write(&source, LEGACY_FULL).expect("source");
    let (code, out, err) = cli(&[
        "stache".into(),
        "--config".into(),
        destination.as_os_str().to_owned(),
        "config".into(),
        "migrate".into(),
        "--from".into(),
        source.as_os_str().to_owned(),
    ]);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(!out.contains("unknown"), "{out}");
    assert!(!out.contains("emitted default template"), "{out}");

    let catalog = catalog();
    let mut names = Vec::new();
    for id in catalog.ids() {
        let name = catalog.file_name(id).expect("file name");
        assert!(is_toml(&name), "{name}");
        let path = destination.join(&name);
        let text = fs::read_to_string(&path).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(text.starts_with("#:schema "), "{name}: {text}");
        let warnings = catalog
            .inspect(id, &path, &text)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(warnings.is_empty(), "{name}: {warnings:?}");
        names.push(name);
    }
    let leftovers: Vec<String> = fs::read_dir(&destination)
        .expect("read dir")
        .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
        .filter(|name| !names.contains(name))
        .collect();
    assert!(leftovers.is_empty(), "no other files: {leftovers:?}");

    let (code, out, err) = cli(&args(&destination, &["config", "path"]));
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    for name in &names {
        assert!(
            out.contains(&format!("  {name} (exists, offline)\n")),
            "{name}: {out}"
        );
    }
    assert!(!out.contains("warning"), "{out}");
}

// ---------------------------------------------------------------------------
// Part 03: per-module `.jsonc` files written in the previous release's
// spellings migrate to `.toml` files that load without warnings.
// ---------------------------------------------------------------------------

/// Every registered id, as the previous release wrote it (camelCase keys,
/// kebab-case enum values, `preset-on-open`, `$schema`, comments, trailing
/// commas).
const PER_MODULE: [(&str, &str); 10] = [
    ("global", include_str!("fixtures/per-module/global.jsonc")),
    ("bar", include_str!("fixtures/per-module/bar.jsonc")),
    (
        "commandQuit",
        include_str!("fixtures/per-module/commandQuit.jsonc"),
    ),
    ("keepAwake", include_str!("fixtures/per-module/keepAwake.jsonc")),
    (
        "keybindings",
        include_str!("fixtures/per-module/keybindings.jsonc"),
    ),
    (
        "menuAnywhere",
        include_str!("fixtures/per-module/menuAnywhere.jsonc"),
    ),
    ("notunes", include_str!("fixtures/per-module/notunes.jsonc")),
    (
        "proxyAudio",
        include_str!("fixtures/per-module/proxyAudio.jsonc"),
    ),
    ("tiling", include_str!("fixtures/per-module/tiling.jsonc")),
    (
        "wallpapers",
        include_str!("fixtures/per-module/wallpapers.jsonc"),
    ),
];

/// The same configuration spelled the way the running configuration expects it.
const EXPECTED_TILING: &str = r##"
enabled = true
default_layout = "split_vertical"

[animations]
enabled = true
duration = 250
easing = "ease_out_expo"

[[gaps]]
screen = "main"
outer = 8
[gaps.inner]
top = 2
right = 4
bottom = 2
left = 4

[[gaps]]
screen = "secondary"
inner = 4
outer = 6

[master]
position = "left"
ratio = 60

[floating]
default_position = "default"

[[floating.presets]]
name = "wide"
width = "80%"
height = 600
center = true

[[floating.presets]]
name = "corner"
width = 640
height = "50%"
x = 20
y = "10%"

[[workspaces]]
name = "code"
screen = "main"
layout = "split_horizontal"
preset_on_open = "wide"
rules = [{ app_id = "com.apple.dt.Xcode", title = "Docs" }, { app_name = "Terminal" }]

[[workspaces]]
name = "web"
screen = "secondary"
layout = "master"

[[ignore]]
app_id = "com.apple.systempreferences"

[[ignore]]
app_name = "Preview"

[borders]
enabled = true
style = "round"
hidpi = true
floating = false
split_horizontal = false

[borders.focused]
width = 4
color = "#b4befe"

[borders.unfocused]
width = 2
glow = "#45475a"

[borders.monocle]
width = 4
gradient = { from = "#f38ba8", to = "#89b4fa", angle = 45 }
animation = { duration = 800, easing = "ease_in_out" }

[borders.split_vertical]
width = 3
color = "#a6e3a1"

[[borders.ignore]]
app_name = "Calculator"
"##;

const EXPECTED_BAR: &str = r#"
enabled = true
height = 32
padding = 8

[weather]
provider = "visual_crossing"
api_keys = "/Users/me/.keys.env"
default_location = "Lisbon"
"#;

const EXPECTED_PROXY_AUDIO: &str = r#"
enabled = true
input = [{ name = "AirPods Pro" }]

[[output]]
name = "External Speakers"
strategy = "starts_with"
depends_on = { name = "MiniFuse", strategy = "contains" }
"#;

fn decode<T: serde::de::DeserializeOwned>(text: &str) -> T {
    let value = stache_sdk::config::loader::parse_text(
        stache_sdk::config::ConfigFileId::Global,
        Path::new("scenario.toml"),
        text,
    )
    .unwrap_or_else(|error| panic!("valid TOML: {error}\n{text}"));
    serde_json::from_value(value).unwrap_or_else(|error| panic!("typed decode: {error}\n{text}"))
}

/// A configuration directory holding the per-module fixtures, optionally a subset.
fn per_module_dir(root: &Path, only: &[&str]) -> std::path::PathBuf {
    let directory = root.join("config");
    fs::create_dir_all(&directory).expect("config dir");
    for (id, text) in PER_MODULE {
        if only.is_empty() || only.contains(&id) {
            fs::write(directory.join(format!("{id}.jsonc")), text).expect("fixture");
        }
    }
    directory
}

fn entries(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .expect("read dir")
        .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
#[allow(clippy::too_many_lines)] // one end-to-end migration scenario
fn per_module_jsonc_files_in_the_previous_spellings_migrate_and_load_cleanly() {
    let root = tempfile::tempdir().expect("temp dir");
    let directory = per_module_dir(root.path(), &[]);
    let catalog = catalog();

    let (code, out, err) = cli(&args(&directory, &["config", "path"]));
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    for (id, _) in PER_MODULE {
        assert!(
            out.contains(&format!(
                "note: {id}.jsonc is ignored; run `stache config migrate`"
            )),
            "{id}: {out}"
        );
    }

    let (code, out, err) = cli(&args(&directory, &["config", "migrate"]));
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(
        out.lines().next(),
        Some(format!("migrating per-module files from {}", directory.display()).as_str())
    );
    // The removed `borders.style` key is the only thing carried as unknown:
    // per-module files never run legacy hooks, and borders are always round.
    let removed_style = "warning: unknown tiling key `borders.style` is preserved in tiling.toml and ignored by \
         the typed configuration";
    assert!(
        out.lines()
            .filter(|line| line.contains("warning"))
            .all(|line| line == removed_style),
        "{out}"
    );
    assert!(!out.contains("emitted default template"), "{out}");

    for (id, text) in PER_MODULE {
        assert_eq!(
            fs::read_to_string(directory.join(format!("{id}.jsonc"))).expect("source"),
            text,
            "{id}.jsonc is never modified"
        );
    }
    for id in catalog.ids() {
        let name = catalog.file_name(id).expect("file name");
        let path = directory.join(&name);
        let text = fs::read_to_string(&path).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(text.starts_with("#:schema "), "{name}: {text}");
        assert!(!text.contains("$schema"), "{name}: {text}");
        let warnings = catalog
            .inspect(id, &path, &text)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        if name == "tiling.toml" {
            assert_eq!(warnings.len(), 1, "{name}: {warnings:?}");
            assert!(warnings[0].contains("borders.style"), "{name}: {warnings:?}");
        } else {
            assert!(warnings.is_empty(), "{name}: {warnings:?}");
        }
    }
    let read = |id: &str| fs::read_to_string(directory.join(format!("{id}.toml"))).expect(id);

    let global: stache_sdk::GlobalConfig = decode(&read("global"));
    assert_eq!(
        global.exec_on_startup,
        stache_sdk::StartupCommands::Multiple(vec![
            "open -a Terminal".to_owned(),
            "echo camelCase-stays-here".to_owned()
        ])
    );
    assert!(global.logging.debug);
    assert_eq!(
        decode::<stache_bar::BarConfig>(&read("bar")),
        decode(EXPECTED_BAR)
    );
    assert_eq!(
        decode::<stache_proxy_audio::ProxyAudioConfig>(&read("proxyAudio")),
        decode(EXPECTED_PROXY_AUDIO)
    );
    assert_eq!(decode::<TilingConfig>(&read("tiling")), decode(EXPECTED_TILING));
    let quit: stache_hold_to_quit::CommandQuitConfig = decode(&read("commandQuit"));
    assert_eq!((quit.hold_duration, quit.dismiss_delay), (900, 300));
    assert_eq!(quit.position, stache_hold_to_quit::HudPosition::Top);
    assert!(decode::<stache_keep_awake::KeepAwakeConfig>(&read("keepAwake")).enabled);
    let notunes: stache_notunes::NoTunesConfig = decode(&read("notunes"));
    assert_eq!(notunes.target_app.as_deref(), Some("com.spotify.client"));
    let menu: stache_menu_anywhere::MenuAnywhereConfig = decode(&read("menuAnywhere"));
    assert!(menu.enabled);
    let wallpapers: stache_wallpapers::WallpapersConfig = decode(&read("wallpapers"));
    assert!(wallpapers.enabled);
    assert_eq!(wallpapers.interval, 600);

    // TC-03-05: the keybindings file keeps `{ enabled, bindings }`.
    let keybindings_text = read("keybindings");
    assert!(
        !keybindings_text.contains("[bindings.bindings]"),
        "{keybindings_text}"
    );
    let keybindings: stache_keybindings::KeybindingsConfig = decode(&keybindings_text);
    assert!(keybindings.enabled);
    assert_eq!(keybindings.bindings.len(), 4);
    assert_eq!(keybindings.bindings["§"].commands(), [
        "open -a Safari",
        "echo enabledApps"
    ]);
    assert_eq!(keybindings.bindings["CapsLock+a"].commands(), [
        "stache tiling workspace --layout split-vertical"
    ]);
    assert_eq!(keybindings.bindings["cmd+shift+."].commands(), ["stache reload"]);
    assert!(
        keybindings.bindings.contains_key("enabledApps"),
        "user data is never renamed"
    );

    // Nothing is stale any more; only the removed `borders.style` key warns.
    let (code, out, err) = cli(&args(&directory, &["config", "path"]));
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(!out.contains("migrate"), "{out}");
    assert!(
        out.lines()
            .filter(|line| line.contains("warning"))
            .all(|line| line.contains("unknown key `borders.style` is ignored")),
        "{out}"
    );
}

#[test]
fn a_partial_directory_migrates_only_what_exists() {
    let root = tempfile::tempdir().expect("temp dir");
    let directory = per_module_dir(root.path(), &["tiling"]);
    let (code, out, err) = cli(&args(&directory, &["config", "migrate"]));
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(!out.contains("emitted default template"), "{out}");
    assert_eq!(entries(&directory), ["tiling.jsonc", "tiling.toml"]);
}

#[test]
fn one_invalid_file_writes_nothing() {
    let root = tempfile::tempdir().expect("temp dir");
    let directory = per_module_dir(root.path(), &[]);
    fs::write(
        directory.join("tiling.jsonc"),
        r#"{ "master": { "ratio": 500 } }"#,
    )
    .expect("invalid tiling");
    let before = entries(&directory);
    let (code, out, err) = cli(&args(&directory, &["config", "migrate"]));
    assert_ne!(code, ExitCode::SUCCESS);
    assert!(err.contains("master.ratio"), "{err}");
    assert!(out.is_empty(), "{out}");
    assert_eq!(entries(&directory), before, "no .toml was written");
}

#[test]
fn a_collision_refuses_without_force_and_force_replaces_only_planned_files() {
    let root = tempfile::tempdir().expect("temp dir");
    let directory = per_module_dir(root.path(), &["tiling", "bar"]);
    fs::write(directory.join("bar.toml"), "# mine\nenabled = false\n").expect("existing");
    fs::write(directory.join("wallpapers.toml"), "# unrelated\n").expect("unrelated");

    let (code, _out, err) = cli(&args(&directory, &["config", "migrate"]));
    assert_ne!(code, ExitCode::SUCCESS);
    assert!(
        err.contains("refusing to overwrite existing files: bar.toml"),
        "{err}"
    );
    assert!(!directory.join("tiling.toml").exists(), "nothing was written");
    assert_eq!(
        fs::read_to_string(directory.join("bar.toml")).expect("kept"),
        "# mine\nenabled = false\n"
    );

    let (code, _out, err) = cli(&args(&directory, &["config", "migrate", "--force"]));
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(directory.join("tiling.toml").exists());
    assert!(
        fs::read_to_string(directory.join("bar.toml"))
            .expect("bar")
            .contains("height = 32")
    );
    assert_eq!(
        fs::read_to_string(directory.join("wallpapers.toml")).expect("unrelated"),
        "# unrelated\n"
    );
}

#[test]
fn a_single_per_module_file_and_a_stale_hint_for_a_partial_migration() {
    let root = tempfile::tempdir().expect("temp dir");
    let directory = per_module_dir(root.path(), &["tiling", "bar"]);
    let destination = root.path().join("new");
    let (code, out, err) = cli(&args(&destination, &[
        "config",
        "migrate",
        "--from",
        directory.join("bar.jsonc").to_str().expect("utf-8 path"),
    ]));
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.starts_with("migrating per-module file "), "{out}");
    assert_eq!(entries(&destination), ["bar.toml"]);

    // A `.toml` next to a `.jsonc` silences the hint for that file only.
    fs::copy(destination.join("bar.toml"), directory.join("bar.toml")).expect("copy");
    let (_, out, _) = cli(&args(&directory, &["config", "path"]));
    assert!(out.contains("tiling.jsonc is ignored"), "{out}");
    assert!(!out.contains("bar.jsonc is ignored"), "{out}");
}

/// TB-08-04: the approved OQ-A border example plus `order = "above"` loads on
/// the real catalog without warnings, with typed values and validation; the
/// same file without the new keys keeps the legacy meaning.
#[test]
fn the_feature_doc_borders_example_loads_and_validates() {
    // The `tiling.toml` example in docs/features/tiling.md (Borders section),
    // kept verbatim so the shipped doc cannot drift from the loader.
    const DOC_EXAMPLE: &str = r##"
[borders]
enabled = true
order = "below"
transition = { duration = 200, easing = "ease_out" }
unfocused = false

[borders.focused]
width = 6
gradient = { from = "#b4befe", to = "#cba6f7", angle = 45 }
animation = { style = "loop", duration = 4000, easing = "linear" }

[[borders.ignore]]
app_id = "com.apple.systempreferences"
"##;
    let catalog = catalog();
    let path = Path::new("tiling.toml");
    let (tiling, warnings) = catalog
        .decode_typed::<TilingConfig>(TilingConfig::ID, path, DOC_EXAMPLE)
        .expect("the documented example decodes");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(
        catalog
            .inspect(TilingConfig::ID, path, DOC_EXAMPLE)
            .expect("inspect")
            .is_empty()
    );
    stache_sdk::config::loader::validate(
        stache_sdk::config::ConfigFileId::module(TilingConfig::ID),
        path,
        &tiling,
    )
    .expect("the documented example validates");
}

#[test]
fn the_approved_native_borders_example_loads_with_typed_values() {
    use stache_tiling::{
        BorderAnimationStyle, BorderOrder, BorderStateConfig, BorderTransitionConfig, EasingType,
    };

    const APPROVED: &str = r##"
[borders]
enabled = true
transition = { duration = 200, easing = "ease_out" }
order = "above"

[borders.focused]
width = 4
gradient = { from = "#b4befe", to = "#cba6f7", angle = 45 }
animation = { style = "loop", duration = 4000, easing = "linear" }
"##;
    let catalog = catalog();
    let path = Path::new("tiling.toml");
    let (tiling, warnings) = catalog
        .decode_typed::<TilingConfig>(TilingConfig::ID, path, APPROVED)
        .expect("the approved example decodes");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(catalog.inspect(TilingConfig::ID, path, APPROVED).expect("inspect").is_empty());
    let borders = &tiling.borders;
    assert_eq!(
        borders.transition,
        Some(BorderTransitionConfig {
            duration: 200,
            easing: EasingType::EaseOut,
        })
    );
    assert_eq!(borders.order, Some(BorderOrder::Above));
    let BorderStateConfig::GradientColor {
        animation: Some(animation),
        gradient,
        width,
    } = &borders.focused
    else {
        panic!("an animated gradient: {:?}", borders.focused);
    };
    assert_eq!(*width, 4);
    assert!((gradient.angle - 45.0).abs() < f64::EPSILON);
    assert_eq!(animation.style, Some(BorderAnimationStyle::Loop));
    assert_eq!(animation.duration, 4000);
    assert_eq!(animation.easing, EasingType::Linear);

    // Without the additions: no transition, below, and a ping-pong animation.
    let legacy = APPROVED
        .replace("transition = { duration = 200, easing = \"ease_out\" }\n", "")
        .replace("order = \"above\"\n", "")
        .replace("style = \"loop\", ", "");
    let (old, warnings) = catalog
        .decode_typed::<TilingConfig>(TilingConfig::ID, path, &legacy)
        .expect("the legacy shape decodes");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(old.borders.transition, None);
    assert_eq!(old.borders.order, None);
    let BorderStateConfig::GradientColor { animation: Some(animation), .. } = &old.borders.focused
    else {
        panic!("an animated gradient");
    };
    assert_eq!(animation.effective_style(), BorderAnimationStyle::PingPong);

    // An unknown style fails module validation with its path; an unknown order
    // fails decoding.
    let validate = |config: &TilingConfig| {
        stache_sdk::config::loader::validate(
            stache_sdk::config::ConfigFileId::module(TilingConfig::ID),
            path,
            config,
        )
    };
    validate(&tiling).expect("the approved example validates");
    let bad_style = APPROVED.replace("\"loop\"", "\"spin\"");
    let (spin, _) = catalog
        .decode_typed::<TilingConfig>(TilingConfig::ID, path, &bad_style)
        .expect("an unknown style still decodes");
    let error = validate(&spin).expect_err("an unknown style is invalid");
    assert!(
        error
            .to_string()
            .contains("borders.focused.animation.style: unknown style `spin`"),
        "{error}"
    );
    let bad_order = APPROVED.replace("\"above\"", "\"top\"");
    catalog
        .decode_typed::<TilingConfig>(TilingConfig::ID, path, &bad_order)
        .expect_err("unknown order");
}
