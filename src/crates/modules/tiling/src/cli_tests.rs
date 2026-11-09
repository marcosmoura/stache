//! The composed CLI against the real handler: `stache_cli` parsing, the
//! tiling contribution, a real temporary Unix socket, the router, the slice-A
//! handler over a fake actor, client serde and the renderers (TI-07-01,
//! TI-07-02, TI-07-03). Transport-fault adapters use a canned transport.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use stache_cli::CliEnvironment;
use stache_sdk::ipc::{
    ClientError, CoreHandler, CoreReload, CoreStatus, HandlerResult, IpcHandler, IpcServer,
    Request, Response, RetryPolicy, Router, WireError, WireErrorKind,
};
use stache_sdk::{
    AppEnvironment, AppPaths, CommandCatalog, ConfigCatalog, ConfigEnvironment, ModuleStatus,
};

use crate::cli::{IpcTransport, TilingCliEnv, TilingTransport, contribution};
use crate::config::LayoutType;
use crate::dto::{
    ActionEvidence, ActionStatus, OperationEvidence, OperationKind, OperationResponse,
    RequestEvidence,
};
use crate::intent::{
    FocusDirection, FocusTarget, Intent, ResizeDimension, ScreenTarget, SwapDirection,
};
use crate::ipc::{IpcTiming, LifecycleFailure, body};
use crate::ipc_tests::{Built, Mode, Rig, config, module, quiet_pause, standard};

struct HostCore {
    directory: PathBuf,
    delay: Duration,
}

impl CoreHandler for HostCore {
    fn reload(&self) -> HandlerResult<CoreReload> {
        Ok(CoreReload {
            files: Vec::new(),
            access: None,
        })
    }

    fn status(&self) -> HandlerResult<CoreStatus> {
        std::thread::sleep(self.delay);
        Ok(CoreStatus {
            directory: self.directory.clone(),
            files: Vec::new(),
            access: None,
        })
    }
}

#[derive(Debug)]
struct Run {
    code: ExitCode,
    out: String,
    err: String,
}

fn os(args: &[&str]) -> Vec<OsString> {
    std::iter::once("stache")
        .chain(args.iter().copied())
        .map(OsString::from)
        .collect()
}

fn paths_env(root: &Path) -> (ConfigEnvironment, AppEnvironment) {
    (
        ConfigEnvironment {
            xdg_config_home: None,
            home: Some(root.to_path_buf()),
        },
        AppEnvironment {
            home: Some(root.to_path_buf()),
            temporary: Some(root.to_path_buf()),
        },
    )
}

fn highlight() -> crate::cli::JsonHighlighter { Arc::new(stache_cli::output::highlight_json) }

fn run_with(transport: Arc<dyn TilingTransport>, root: &Path, args: &[&str], ansi: bool) -> Run {
    let (config, app) = paths_env(root);
    let mut env = TilingCliEnv::new(config.clone(), app.clone(), highlight());
    env.transport = transport;
    let mut catalog = CommandCatalog::new();
    catalog.add(contribution(env)).expect("add");
    let environment = CliEnvironment::new(config, app);
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = stache_cli::run_with_io(
        os(args),
        &ConfigCatalog::new(),
        &catalog,
        &environment,
        ansi,
        &mut out,
        &mut err,
    );
    Run {
        code,
        out: String::from_utf8(out).expect("utf-8 stdout"),
        err: String::from_utf8(err).expect("utf-8 stderr"),
    }
}

/// A live host: temporary socket, router, slice-A handler over a fake actor.
struct Stage {
    root: tempfile::TempDir,
    rig: Rig,
    _server: IpcServer,
    transport: Arc<dyn TilingTransport>,
}

impl Stage {
    fn new(rig: Rig) -> Self { Self::with(rig, Duration::ZERO, Duration::from_secs(5)) }

    fn with(rig: Rig, status_delay: Duration, timeout: Duration) -> Self {
        let root = tempfile::tempdir().expect("root");
        let directory = root.path().join(".config").join("stache");
        let (_, app) = paths_env(root.path());
        let paths = AppPaths::resolve(&app).expect("paths");
        let mut router = Router::new(Arc::new(HostCore { directory, delay: status_delay }));
        router
            .register("tiling", {
                let handler: Arc<dyn IpcHandler> = rig.ipc.clone();
                handler
            })
            .expect("register");
        let server = IpcServer::start(&paths, Arc::new(router)).expect("server");
        Self {
            root,
            rig,
            _server: server,
            transport: Arc::new(IpcTransport::with_options(RetryPolicy::single(), timeout)),
        }
    }

    fn standard() -> Self { Self::new(Rig::standard().0) }

    fn run(&self, args: &[&str]) -> Run {
        run_with(Arc::clone(&self.transport), self.root.path(), args, false)
    }

    fn run_ansi(&self, args: &[&str]) -> Run {
        run_with(Arc::clone(&self.transport), self.root.path(), args, true)
    }
}

// ----------------------------------------------------------------- helpers

fn ok(run: &Run) {
    assert_eq!(run.code, ExitCode::SUCCESS, "{run:?}");
    assert_eq!(run.err, "", "{run:?}");
}

fn silent_ok(run: &Run) {
    ok(run);
    assert_eq!(run.out, "", "{run:?}");
}

fn exit(run: &Run, code: u8) {
    assert_eq!(run.code, ExitCode::from(code), "{run:?}");
}

/// A failed action: stderr only, exit 1.
fn action_error(run: &Run, line: &str) {
    exit(run, 1);
    assert_eq!(run.out, "", "{run:?}");
    assert_eq!(run.err, format!("{line}\n"), "{run:?}");
}

/// A failed query: stdout only, exit 1.
fn query_error(run: &Run, stdout: &str) {
    exit(run, 1);
    assert_eq!(run.out, stdout, "{run:?}");
    assert_eq!(run.err, "", "{run:?}");
}

fn json_rows(stage: &Stage, args: &[&str]) -> Value {
    let run = stage.run(args);
    ok(&run);
    assert!(run.out.ends_with("\n]\n") || run.out == "[]\n", "{}", run.out);
    serde_json::from_str(&run.out).expect("json")
}

fn column(rows: &Value, key: &str) -> Vec<String> {
    rows.as_array()
        .expect("array")
        .iter()
        .map(|row| match &row[key] {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .collect()
}

/// Removes `ESC [ ... m` sequences.
fn strip(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for next in chars.by_ref() {
                if next == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn host_directory(root: &Path) -> PathBuf { root.join(".config").join("stache") }

// ============================================== TI-07-01 queries (real socket)

const SCREENS: &str = "\
Screens (2)
╭────┬───────────┬────────────┬──────────┬───────┬──────┬──────────╮
│ ID │ Name      │ Resolution │ Position │ Scale │ Main │ Built-in │
├────┼───────────┼────────────┼──────────┼───────┼──────┼──────────┤
│  1 │ Display 1 │  1920x1080 │     0, 0 │    2x │  ✓   │    ✓     │
│  2 │ Display 2 │  1920x1080 │  1920, 0 │    2x │  ✗   │    ✗     │
╰────┴───────────┴────────────┴──────────┴───────┴──────┴──────────╯
";

const WORKSPACES: &str = "\
Workspaces (4)
╭──────┬───────────┬──────────┬─────────┬─────────┬─────────╮
│ Name │ Screen    │ Layout   │ Windows │ Visible │ Focused │
├──────┼───────────┼──────────┼─────────┼─────────┼─────────┤
│ a    │ Display 1 │ dwindle  │       1 │    ✓    │    ✓    │
│ b    │ Display 1 │ dwindle  │       0 │    ✗    │    ✗    │
│ g    │ Display 2 │ grid     │       1 │    ✓    │    ✗    │
│ f    │ Display 2 │ floating │       0 │    ✗    │    ✗    │
╰──────┴───────────┴──────────┴─────────┴─────────┴─────────╯
";

const WINDOWS: &str = "\
Windows (2)
╭─────┬─────────┬─────────────────────────────┬───────────┬────────────────────┬─────────╮
│  ID │ App     │ Title                       │ Workspace │              Frame │ Focused │
├─────┼─────────┼─────────────────────────────┼───────────┼────────────────────┼─────────┤
│ 101 │ Editor  │ Editor — main.rs            │ a         │   800x600 @ 10, 10 │    ✓    │
│ 201 │ Browser │ Browser \"quoted\" 日本語\\n🙂 │ g         │ 800x600 @ 1930, 10 │    ✗    │
╰─────┴─────────┴─────────────────────────────┴───────────┴────────────────────┴─────────╯
";

const WINDOWS_DETAILED: &str = "\
Windows (2)
╭─────┬─────┬─────────┬─────────────┬─────────────────────────────┬───────────┬────────────────────┬─────┬───────┬───────╮
│  ID │ PID │ App     │ Bundle ID   │ Title                       │ Workspace │ Frame              │ Min │ Float │ Focus │
├─────┼─────┼─────────┼─────────────┼─────────────────────────────┼───────────┼────────────────────┼─────┼───────┼───────┤
│ 101 │ 101 │ Editor  │ com.editor  │ Editor — main.rs            │ a         │ 800x600 @ 10, 10   │  ✗  │   ✗   │   ✓   │
│ 201 │ 102 │ Browser │ com.browser │ Browser \"quoted\" 日本語\\n🙂 │ g         │ 800x600 @ 1930, 10 │  ✗  │   ✗   │   ✗   │
╰─────┴─────┴─────────┴─────────────┴─────────────────────────────┴───────────┴────────────────────┴─────┴───────┴───────╯
";

const APPS: &str = "\
Running Apps (2)
╭─────┬─────────┬─────────────┬─────────╮
│ PID │ Name    │ Bundle ID   │ Visible │
├─────┼─────────┼─────────────┼─────────┤
│ 101 │ Editor  │ com.editor  │    ✓    │
│ 102 │ Browser │ com.browser │    ✓    │
╰─────┴─────────┴─────────────┴─────────╯
";

#[test]
fn every_query_table_matches_its_golden_through_the_real_socket() {
    let stage = Stage::standard();
    for (args, golden) in [
        (&["tiling", "query", "screens"][..], SCREENS),
        (&["tiling", "query", "workspaces"][..], WORKSPACES),
        (&["tiling", "query", "windows"][..], WINDOWS),
        (&["tiling", "query", "windows", "-d"][..], WINDOWS_DETAILED),
        (&["tiling", "query", "-d", "windows"][..], WINDOWS_DETAILED),
        (
            &["tiling", "query", "--detailed", "windows"][..],
            WINDOWS_DETAILED,
        ),
        (&["tiling", "query", "apps"][..], APPS),
        (&["tiling", "query", "-d", "apps"][..], APPS),
        (&["tiling", "query", "-d", "screens"][..], SCREENS),
    ] {
        let run = stage.run(args);
        ok(&run);
        assert_eq!(run.out, golden, "{args:?}");
    }
}

#[test]
fn the_status_query_and_a_response_far_over_one_read_chunk_work_through_the_real_socket() {
    let stage = Stage::standard();
    let table = stage.run(&["tiling", "query", "status"]);
    ok(&table);
    assert!(table.out.contains("phase: "), "{}", table.out);
    let json = stage.run(&["tiling", "query", "--json", "status"]);
    ok(&json);
    let parsed: Value = serde_json::from_str(&json.out).expect("one JSON object");
    assert!(parsed["phase"].is_string() && parsed["trackedWindows"].is_array());
    // Sixty windows: well over the 4 KiB the client reads at a time. macOS
    // refuses to change a socket timeout once the host closed its end, which
    // used to fail every such answer with `Invalid argument`.
    let mut built = Built::new(config());
    for serial in 0..60_u64 {
        built.add(
            1,
            serial + 1,
            &format!("window number {serial}"),
            0.0,
            500 + u32::try_from(serial).unwrap_or(0),
        );
    }
    let stage = Stage::new(Rig::new(built.state));
    let run = stage.run(&["tiling", "query", "--json", "-d", "windows"]);
    ok(&run);
    let rows: Value = serde_json::from_str(&run.out).expect("windows");
    assert_eq!(rows.as_array().map(Vec::len), Some(60));
    assert!(run.out.len() > 16 * 1024, "{}", run.out.len());
}

#[test]
fn ansi_tables_strip_to_the_plain_golden_and_pipes_never_get_escapes() {
    let stage = Stage::standard();
    for (args, golden) in [
        (&["tiling", "query", "screens"][..], SCREENS),
        (&["tiling", "query", "workspaces"][..], WORKSPACES),
        (&["tiling", "query", "windows"][..], WINDOWS),
        (&["tiling", "query", "-d", "windows"][..], WINDOWS_DETAILED),
        (&["tiling", "query", "apps"][..], APPS),
    ] {
        let styled = stage.run_ansi(args);
        ok(&styled);
        assert!(styled.out.contains('\u{1b}'), "{args:?}");
        assert_eq!(strip(&styled.out), golden, "{args:?}");
        assert!(!stage.run(args).out.contains('\u{1b}'));
    }
    let json = stage.run_ansi(&["tiling", "query", "--json", "apps"]);
    ok(&json);
    assert!(json.out.contains('\u{1b}'));
    assert_eq!(
        strip(&json.out),
        stage.run(&["tiling", "query", "--json", "apps"]).out,
        "the highlighter only inserts escapes"
    );
}

#[test]
fn json_is_the_pretty_whole_array_in_the_frozen_field_order() {
    let stage = Stage::standard();
    let run = stage.run(&["tiling", "query", "--json", "screens"]);
    ok(&run);
    assert!(run.out.starts_with(
        "[\n  {\n    \"id\": 1,\n    \"name\": \"Display 1\",\n    \"isMain\": true,\n    \
         \"isBuiltin\": true,\n    \"scaleFactor\": 2.0,\n    \"frame\": {\n      \"x\": 0.0,\n      \
         \"y\": 0.0,\n      \"width\": 1920.0,\n      \"height\": 1080.0\n    },\n    \
         \"visibleFrame\": {"
    ));
    assert!(run.out.ends_with("    }\n  }\n]\n"));
    // Whole shape equals the handler's rows, whatever the detailed flag.
    for (args, kind) in [
        (&["tiling", "query", "-j", "screens"][..], "screens"),
        (&["tiling", "query", "workspaces", "-j"][..], "workspaces"),
        (&["tiling", "query", "-j", "windows"][..], "windows"),
        (&["tiling", "query", "-j", "-d", "windows"][..], "windows"),
        (
            &["tiling", "query", "windows", "--json", "--detailed"][..],
            "windows",
        ),
        (&["tiling", "query", "-jd", "apps"][..], "apps"),
    ] {
        let rows = json_rows(&stage, args);
        assert_eq!(rows, stage.rig.query(json!({"kind": kind})), "{args:?}");
    }
    let windows = stage.run(&["tiling", "query", "--json", "windows"]);
    let order = [
        "id",
        "pid",
        "appId",
        "appName",
        "title",
        "workspace",
        "frame",
        "isMinimized",
        "isFullscreen",
        "isFloating",
        "isFocused",
    ];
    let at: Vec<usize> = order
        .iter()
        .map(|key| windows.out.find(&format!("    \"{key}\":")).expect("key"))
        .collect();
    assert!(at.windows(2).all(|pair| pair[0] < pair[1]), "{}", windows.out);
    assert!(windows.out.contains(r#""title": "Browser \"quoted\" 日本語\n🙂""#));
}

#[test]
fn query_filters_alone_and_combined_reach_the_handler_exactly() {
    let stage = Stage::standard();
    let names = |args: &[&str]| column(&json_rows(&stage, args), "name");
    assert_eq!(
        names(&["tiling", "query", "-j", "workspaces", "--screen", "main"]),
        ["a", "b"]
    );
    assert_eq!(
        names(&[
            "tiling",
            "query",
            "-j",
            "workspaces",
            "--screen",
            "secondary"
        ]),
        ["g", "f"]
    );
    assert_eq!(
        names(&[
            "tiling",
            "query",
            "-j",
            "workspaces",
            "--screen",
            "Display 2"
        ]),
        ["g", "f"]
    );
    assert_eq!(
        names(&["tiling", "query", "workspaces", "--focused-screen", "-j"]),
        ["a", "b"]
    );
    let ids = |args: &[&str]| column(&json_rows(&stage, args), "id");
    assert_eq!(ids(&["tiling", "query", "-j", "windows", "--workspace", "g"]), [
        "201"
    ]);
    assert_eq!(
        ids(&["tiling", "query", "-j", "windows", "--screen", "secondary"]),
        ["201"]
    );
    assert_eq!(
        ids(&[
            "tiling",
            "query",
            "-j",
            "windows",
            "--screen",
            "main",
            "--workspace",
            "a"
        ]),
        ["101"]
    );
    assert_eq!(
        ids(&[
            "tiling",
            "query",
            "-j",
            "windows",
            "--screen",
            "main",
            "--workspace",
            "g"
        ]),
        Vec::<String>::new()
    );
    assert_eq!(
        ids(&["tiling", "query", "-j", "windows", "--focused-workspace"]),
        ["101"]
    );
    assert_eq!(ids(&["tiling", "query", "-j", "windows", "--focused-screen"]), [
        "101"
    ]);
}

#[test]
fn empty_results_use_the_exact_bodies_and_empty_arrays() {
    let stage = Stage::standard();
    let run = stage.run(&["tiling", "query", "windows", "--workspace", "nope"]);
    ok(&run);
    assert_eq!(run.out, "No windows found.\n");
    let run = stage.run(&[
        "tiling",
        "query",
        "windows",
        "--workspace",
        "nope",
        "--json",
    ]);
    ok(&run);
    assert_eq!(run.out, "[]\n");
    let run = stage.run(&["tiling", "query", "workspaces", "--screen", "nope"]);
    assert_eq!(run.out, "No workspaces found.\n");
    let ansi = stage.run_ansi(&["tiling", "query", "windows", "--workspace", "nope"]);
    assert_eq!(ansi.out, "\u{1b}[2mNo windows found.\u{1b}[0m\n");
}

#[test]
fn flag_conflicts_and_unknown_flags_are_parse_errors_with_exit_two() {
    let stage = Stage::standard();
    for args in [
        &[
            "tiling",
            "query",
            "workspaces",
            "--focused-screen",
            "--screen",
            "main",
        ][..],
        &[
            "tiling",
            "query",
            "windows",
            "--focused-screen",
            "--screen",
            "main",
        ][..],
        &[
            "tiling",
            "query",
            "windows",
            "--focused-screen",
            "--workspace",
            "a",
        ][..],
        &[
            "tiling",
            "query",
            "windows",
            "--focused-screen",
            "--focused-workspace",
        ][..],
        &[
            "tiling",
            "query",
            "windows",
            "--focused-workspace",
            "--screen",
            "main",
        ][..],
        &[
            "tiling",
            "query",
            "windows",
            "--focused-workspace",
            "--workspace",
            "a",
        ][..],
        &["tiling", "query", "screens", "--screen", "main"][..],
        &["tiling", "query", "apps", "--focused-screen"][..],
        &["tiling", "query", "bogus"][..],
        &["tiling", "bogus"][..],
        &["tiling"][..],
        &["tiling", "pause", "--now"][..],
    ] {
        let run = stage.run(args);
        exit(&run, 2);
        assert_eq!(run.out, "", "{args:?}");
        assert!(
            run.err.contains("error") || run.err.contains("Usage"),
            "{args:?}: {}",
            run.err
        );
    }
    assert!(stage.rig.actor.submitted().is_empty());
}

#[test]
fn query_without_a_subcommand_prints_its_help_and_a_newline_and_reads_nothing() {
    let stage = Stage::standard();
    let run = stage.run(&["tiling", "query"]);
    ok(&run);
    assert_eq!(
        run.out,
        "Query tiling state (screens, workspaces, windows, apps, status)\n\nUsage: stache tiling query \
         [OPTIONS] [COMMAND]\n\nCommands:\n  apps        List all running applications\n  help        \
         Print this message or the help of the given subcommand(s)\n  screens     List all \
         connected screens\n  status      Show why windows are or are not being laid out\n  windows     List managed windows\n  workspaces  List \
         workspaces\n\nOptions:\n  -d, --detailed  Show detailed information (more \
         columns/fields)\n  -h, --help      Print help\n  -j, --json      Output in JSON format \
         instead of table format\n\n"
    );
    // The help is local: it works with the host absent too.
    let root = tempfile::tempdir().expect("root");
    let offline = run_with(
        Arc::new(IpcTransport::with_options(
            RetryPolicy::single(),
            Duration::from_secs(1),
        )),
        root.path(),
        &["tiling", "query", "--json"],
        false,
    );
    ok(&offline);
    assert_eq!(offline.out, run.out);
}

#[test]
fn the_global_config_flag_is_a_directory_accepted_before_and_after_subcommands() {
    let stage = Stage::standard();
    let host = host_directory(stage.root.path());
    let host = host.to_str().expect("utf-8");
    for args in [
        vec!["-c", host, "tiling", "query", "screens"],
        vec!["--config", host, "tiling", "query", "screens"],
        vec![
            &format!("--config={host}")[..],
            "tiling",
            "query",
            "screens",
        ],
        vec!["tiling", "-c", host, "query", "screens"],
        vec!["tiling", "query", "-c", host, "screens"],
        vec!["tiling", "query", "screens", "--config", host],
        vec!["tiling", "query", "screens", "-c", host],
    ] {
        let run = stage.run(&args);
        ok(&run);
        assert_eq!(run.out, SCREENS, "{args:?}");
    }
    quiet_pause(&stage.rig);
    let run = stage.run(&["tiling", "pause", "-c", host]);
    silent_ok(&run);
}

#[test]
fn a_different_host_config_is_refused_without_touching_tiling() {
    let stage = Stage::standard();
    let other = stage.root.path().join("elsewhere");
    std::fs::create_dir_all(&other).expect("dir");
    let other_text = std::fs::canonicalize(&other).expect("canonical");
    let host = host_directory(stage.root.path());
    let body = format!(
        "Running host uses config directory {}; requested {}. Use the same config directory or \
         stop that host.",
        serde_json::to_string(host.to_str().expect("utf-8")).expect("quoted"),
        serde_json::to_string(other_text.to_str().expect("utf-8")).expect("quoted"),
    );
    let other = other.to_str().expect("utf-8");
    query_error(
        &stage.run(&["-c", other, "tiling", "query", "screens"]),
        &format!("Error: {body}\n"),
    );
    query_error(
        &stage.run(&["tiling", "query", "--json", "apps", "-c", other]),
        &format!("{}\n", json!({ "error": body })),
    );
    action_error(
        &stage.run(&["tiling", "window", "--toggle-floating", "-c", other]),
        &format!("stache: IPC error: {body}"),
    );
    action_error(
        &stage.run(&["-c", other, "tiling", "pause"]),
        &format!("stache: IPC error: {body}"),
    );
    assert!(stage.rig.actor.submitted().is_empty());
    assert_eq!(stage.rig.life.pauses.load(Ordering::SeqCst), 0);
}

// ============================================ TI-07-01 / 02 mutations (socket)

fn intents(stage: &Stage) -> Vec<Intent> { stage.rig.actor.submitted() }

#[test]
fn every_window_action_alone_is_silent_admission_with_exit_zero() {
    for (args, expected) in [
        (
            vec!["--focus", "left"],
            Intent::FocusWindow(FocusTarget::Direction(FocusDirection::Left)),
        ),
        (
            vec!["--focus", "PREV"],
            Intent::FocusWindow(FocusTarget::Direction(FocusDirection::Previous)),
        ),
        (vec!["--swap", "down"], Intent::SwapWindow(SwapDirection::Down)),
        (
            vec!["--swap", "previous"],
            Intent::SwapWindow(SwapDirection::Previous),
        ),
        (vec!["--toggle-floating"], Intent::ToggleFloating),
        (vec!["--preset", "center"], Intent::Preset("center".to_owned())),
        (vec!["--resize", "width", "100"], Intent::Resize {
            dimension: ResizeDimension::Width,
            amount: 100,
        }),
        (vec!["--resize", "HEIGHT", "-25"], Intent::Resize {
            dimension: ResizeDimension::Height,
            amount: -25,
        }),
        (
            vec!["--send-to-screen", "secondary"],
            Intent::SendWindowToScreen(ScreenTarget::Secondary),
        ),
        (vec!["--send-to-workspace", "b"], Intent::SendWindowToWorkspace {
            window: None,
            workspace: "b".to_owned(),
        }),
    ] {
        let stage = Stage::standard();
        let mut full = vec!["tiling", "window"];
        full.extend(&args);
        silent_ok(&stage.run(&full));
        assert_eq!(intents(&stage), vec![expected], "{args:?}");
    }
}

#[test]
fn a_numeric_focus_target_selects_that_window_id() {
    let (rig, _, browser) = Rig::standard();
    let stage = Stage::new(rig);
    silent_ok(&stage.run(&["tiling", "window", "--focus", "201"]));
    assert_eq!(intents(&stage), vec![Intent::FocusWindow(FocusTarget::Window(
        browser
    ))]);
    action_error(
        &stage.run(&["tiling", "window", "--focus", "9999"]),
        "stache: IPC error: Window 9999 not found",
    );
    assert_eq!(intents(&stage).len(), 1, "an unknown id is never sent");
}

#[test]
fn window_actions_run_in_the_fixed_order_whatever_the_flag_order() {
    let stage = Stage::standard();
    silent_ok(&stage.run(&[
        "tiling",
        "window",
        "--send-to-workspace",
        "b",
        "--resize",
        "height",
        "-4",
        "--resize",
        "Width",
        "7",
        "--preset",
        "center",
        "--toggle-floating",
        "--swap",
        "next",
        "--focus",
        "next",
    ]));
    assert_eq!(intents(&stage), vec![
        Intent::FocusWindow(FocusTarget::Direction(FocusDirection::Next)),
        Intent::SwapWindow(SwapDirection::Next),
        Intent::ToggleFloating,
        Intent::Preset("center".to_owned()),
        Intent::Resize {
            dimension: ResizeDimension::Height,
            amount: -4
        },
        Intent::Resize {
            dimension: ResizeDimension::Width,
            amount: 7
        },
        Intent::SendWindowToWorkspace {
            window: None,
            workspace: "b".to_owned()
        },
    ]);
}

#[test]
fn every_workspace_action_alone_and_combined_is_silent_admission() {
    for (args, expected) in [
        (vec!["--focus", "b"], vec![Intent::FocusWorkspace(
            "b".to_owned(),
        )]),
        (vec!["--layout", "split-vertical"], vec![Intent::SetLayout(
            LayoutType::SplitVertical,
        )]),
        (vec!["--balance"], vec![Intent::Balance]),
        (vec!["--send-to-screen", "secondary"], vec![
            Intent::SendWorkspaceToScreen(ScreenTarget::Secondary),
        ]),
        (
            vec![
                "--send-to-screen",
                "main",
                "--balance",
                "--layout",
                "grid",
                "--focus",
                "b",
            ],
            vec![
                Intent::FocusWorkspace("b".to_owned()),
                Intent::SetLayout(LayoutType::Grid),
                Intent::Balance,
                Intent::SendWorkspaceToScreen(ScreenTarget::Main),
            ],
        ),
    ] {
        let stage = Stage::standard();
        let mut full = vec!["tiling", "workspace"];
        full.extend(&args);
        silent_ok(&stage.run(&full));
        assert_eq!(intents(&stage), expected, "{args:?}");
    }
    for layout in [
        "dwindle",
        "split",
        "split-vertical",
        "split-horizontal",
        "monocle",
        "master",
        "grid",
        "floating",
    ] {
        let stage = Stage::standard();
        silent_ok(&stage.run(&["tiling", "workspace", "--layout", layout]));
        assert_eq!(intents(&stage).len(), 1, "{layout}");
    }
}

#[test]
fn pause_and_resume_are_silent_only_after_the_registry_acknowledged() {
    let stage = Stage::standard();
    quiet_pause(&stage.rig);
    silent_ok(&stage.run(&["tiling", "pause"]));
    assert_eq!(stage.rig.life.pauses.load(Ordering::SeqCst), 1);
    let life = Arc::clone(&stage.rig.life);
    let status = stage.rig.status.clone();
    stage.rig.life.on_resume(move || {
        life.set_module(Some(module(ModuleStatus::Running, true, false)));
        status.running(Arc::new(crate::actor::SnapshotCell::default()));
        Ok(())
    });
    silent_ok(&stage.run(&["tiling", "resume"]));
    assert_eq!(stage.rig.life.resumes.load(Ordering::SeqCst), 1);
}

#[test]
fn pause_failures_are_never_a_silent_success() {
    let stage = Stage::standard();
    stage.rig.life.set_module(Some(module(ModuleStatus::Disabled, false, false)));
    action_error(
        &stage.run(&["tiling", "pause"]),
        "stache: IPC error: Tiling is disabled",
    );
    action_error(
        &stage.run(&["tiling", "resume"]),
        "stache: IPC error: Tiling is disabled",
    );

    let stage = Stage::standard();
    let status = stage.rig.status.clone();
    stage.rig.life.on_pause(move || {
        status.quarantined(vec!["display observer".to_owned()], "stuck".to_owned());
        Err(LifecycleFailure::Failed("x".to_owned()))
    });
    action_error(
        &stage.run(&["tiling", "pause"]),
        "stache: IPC error: Tiling operation outcome is uncertain; it may already have taken \
         effect.",
    );

    let stage = Stage::standard();
    stage.rig.life.set_module(None);
    action_error(
        &stage.run(&["tiling", "pause"]),
        "stache: IPC error: Tiling is not initialized",
    );
}

#[test]
fn argument_errors_keep_the_legacy_stache_prefix_and_exit_one() {
    let stage = Stage::standard();
    let none = "No window operation specified. Use --help for available options.";
    action_error(&stage.run(&["tiling", "window"]), &format!("stache: {none}"));
    action_error(
        &stage.run(&["tiling", "workspace"]),
        "stache: No workspace operation specified. Use --help for available options.",
    );
    action_error(
        &stage.run(&["tiling", "window", "--resize", "depth", "5"]),
        "stache: Invalid resize dimension 'depth'. Must be 'width' or 'height'.",
    );
    for amount in ["abc", "1.5", "99999999999", "", "0x10"] {
        action_error(
            &stage.run(&["tiling", "window", "--resize", "width", amount]),
            &format!("stache: Invalid resize amount '{amount}'. Must be an integer."),
        );
    }
    assert!(intents(&stage).is_empty(), "nothing was admitted");
}

#[test]
fn a_late_resize_failure_keeps_the_earlier_admitted_actions() {
    let stage = Stage::standard();
    action_error(
        &stage.run(&[
            "tiling",
            "window",
            "--focus",
            "next",
            "--toggle-floating",
            "--resize",
            "width",
            "5",
            "--resize",
            "depth",
            "1",
            "--send-to-workspace",
            "b",
        ]),
        "stache: Invalid resize dimension 'depth'. Must be 'width' or 'height'.",
    );
    assert_eq!(intents(&stage), vec![
        Intent::FocusWindow(FocusTarget::Direction(FocusDirection::Next)),
        Intent::ToggleFloating,
        Intent::Resize {
            dimension: ResizeDimension::Width,
            amount: 5
        },
    ]);
}

#[test]
fn parse_errors_are_exit_two_and_never_reach_the_host() {
    let stage = Stage::standard();
    for args in [
        vec!["tiling", "window", "--swap", "Up"],
        vec!["tiling", "window", "--swap", "prev"],
        vec!["tiling", "window", "--swap", "NEXT"],
        vec!["tiling", "window", "--swap", "diagonal"],
        vec!["tiling", "workspace", "--layout", "Dwindle"],
        vec!["tiling", "workspace", "--layout", "nope"],
        vec!["tiling", "window", "--resize", "width"],
        vec!["tiling", "window", "--resize", "width", "5", "height"],
        vec!["tiling", "window", "--focus"],
        vec!["tiling", "window", "--bogus"],
        vec!["tiling", "workspace", "--bogus"],
        vec!["tiling", "workspace", "--balance", "extra"],
    ] {
        let run = stage.run(&args);
        exit(&run, 2);
        assert_eq!(run.out, "", "{args:?}");
        assert!(!run.err.starts_with("stache:"), "{args:?}: {}", run.err);
    }
    assert!(intents(&stage).is_empty());
    assert_eq!(stage.rig.life.pauses.load(Ordering::SeqCst), 0);
}

#[test]
fn semantic_handler_errors_are_stderr_ipc_errors_with_exit_one() {
    let stage = Stage::standard();
    action_error(
        &stage.run(&["tiling", "window", "--focus", "sideways"]),
        "stache: IPC error: Invalid focus direction: sideways",
    );
    action_error(
        &stage.run(&[
            "tiling",
            "workspace",
            "--focus",
            "missing",
            "--layout",
            "grid",
            "--balance",
        ]),
        "stache: IPC error: Workspace not found",
    );
    assert_eq!(intents(&stage), vec![Intent::FocusWorkspace(
        "missing".to_owned()
    )]);
}

#[test]
fn a_middle_failure_keeps_earlier_actions_and_skips_later_ones() {
    let stage = Stage::standard();
    action_error(
        &stage.run(&[
            "tiling",
            "window",
            "--toggle-floating",
            "--send-to-workspace",
            "missing",
            "--preset",
            "x",
        ]),
        "stache: IPC error: Workspace not found",
    );
    assert_eq!(intents(&stage), vec![
        Intent::ToggleFloating,
        Intent::Preset("x".to_owned()),
        Intent::SendWindowToWorkspace {
            window: None,
            workspace: "missing".to_owned()
        },
    ]);
}

#[test]
fn queue_saturation_and_a_closed_actor_are_actionable_errors() {
    let stage = Stage::standard();
    stage.rig.actor.set_mode(Mode::Full);
    action_error(
        &stage.run(&["tiling", "window", "--toggle-floating"]),
        &format!("stache: IPC error: {}", body::OVERFLOW),
    );
    stage.rig.actor.set_mode(Mode::Closed);
    action_error(
        &stage.run(&["tiling", "window", "--toggle-floating"]),
        &format!("stache: IPC error: {}", body::SEND_FAILED),
    );
}

#[test]
fn an_unconfirmed_send_is_the_uncertain_adapter_and_is_not_retried() {
    let timing = IpcTiming {
        budget: Duration::from_secs(3),
        hop: Duration::from_millis(50),
        poll: Duration::from_millis(1),
    };
    let (built, ..) = standard();
    let rig = Rig::timed(built.state, timing);
    rig.actor.set_mode(Mode::Swallow);
    let stage = Stage::new(rig);
    action_error(
        &stage.run(&["tiling", "window", "--toggle-floating"]),
        "stache: IPC error: Tiling operation outcome is uncertain; it may already have taken \
         effect.",
    );
    assert_eq!(
        intents(&stage).len(),
        1,
        "no automatic retry after possible send"
    );
}

// ===================================================== query failures (stdout)

#[test]
fn unavailable_tiling_answers_a_query_failure_on_stdout_with_exit_one() {
    let stage = Stage::standard();
    stage.rig.life.set_module(Some(module(ModuleStatus::Disabled, false, false)));
    query_error(
        &stage.run(&["tiling", "query", "screens"]),
        "Error: Tiling is disabled\n",
    );
    query_error(
        &stage.run(&["tiling", "query", "--json", "windows"]),
        "{\"error\":\"Tiling is disabled\"}\n",
    );
    let ansi = stage.run_ansi(&["tiling", "query", "apps"]);
    exit(&ansi, 1);
    assert_eq!(ansi.out, "\u{1b}[31mError:\u{1b}[0m Tiling is disabled\n");

    let stage = Stage::standard();
    stage.rig.actor.kill();
    query_error(
        &stage.run(&["tiling", "query", "workspaces"]),
        "Error: Tiling is not initialized\n",
    );
    let stage = Stage::standard();
    stage.rig.cancel.cancel();
    query_error(
        &stage.run(&["tiling", "query", "apps"]),
        "Error: Tiling is shutting down\n",
    );
}

#[test]
fn a_connection_timeout_is_a_deterministic_failure_in_every_form() {
    let stage = Stage::with(
        Rig::standard().0,
        Duration::from_millis(700),
        Duration::from_millis(100),
    );
    query_error(
        &stage.run(&["tiling", "query", "screens"]),
        "Error: Connection timed out\n",
    );
    query_error(
        &stage.run(&["tiling", "query", "-j", "screens"]),
        "{\"error\":\"Connection timed out\"}\n",
    );
    action_error(
        &stage.run(&["tiling", "window", "--toggle-floating"]),
        "stache: IPC error: Connection timed out",
    );
    assert!(intents(&stage).is_empty());
}

#[test]
fn an_absent_host_never_starts_anything_and_never_succeeds() {
    let root = tempfile::tempdir().expect("root");
    let (_, app) = paths_env(root.path());
    let socket = AppPaths::resolve(&app).expect("paths").socket_path().expect("socket");
    let transport: Arc<dyn TilingTransport> = Arc::new(IpcTransport::with_options(
        RetryPolicy::single(),
        Duration::from_secs(1),
    ));
    let go = |args: &[&str]| run_with(Arc::clone(&transport), root.path(), args, false);
    query_error(
        &go(&["tiling", "query", "screens"]),
        "Stache app is not running.\n",
    );
    query_error(
        &go(&["tiling", "query", "apps", "--json"]),
        "{\"error\":\"Stache app is not running\"}\n",
    );
    for args in [
        &["tiling", "pause"][..],
        &["tiling", "resume"][..],
        &["tiling", "window", "--toggle-floating"][..],
        &["tiling", "workspace", "--balance"][..],
    ] {
        action_error(&go(args), "stache: IPC error: Stache app is not running");
    }
    let ansi = run_with(
        Arc::clone(&transport),
        root.path(),
        &["tiling", "query", "windows"],
        true,
    );
    assert_eq!(ansi.out, "\u{1b}[31mStache app is not running.\u{1b}[0m\n");
    assert!(
        !socket.exists(),
        "the CLI never creates a socket or starts a host"
    );
}

// ======================================== canned transport: adapters and wire

type StatusFn = Box<dyn Fn() -> Result<CoreStatus, ClientError> + Send + Sync>;
type ReplyFn = Box<dyn Fn() -> Result<Response, ClientError> + Send + Sync>;

struct Canned {
    status: StatusFn,
    reply: ReplyFn,
    sent: Mutex<Vec<Request>>,
}

impl Canned {
    fn sent(&self) -> Vec<Request> {
        self.sent.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl TilingTransport for Arc<Canned> {
    fn status(&self, _: &Path) -> Result<CoreStatus, ClientError> { (self.status)() }

    fn send(&self, _: &Path, request: &Request) -> Result<Response, ClientError> {
        self.sent.lock().unwrap_or_else(PoisonError::into_inner).push(request.clone());
        (self.reply)()
    }
}

fn host_status(root: &Path) -> StatusFn {
    let directory = host_directory(root);
    Box::new(move || {
        Ok(CoreStatus {
            directory: directory.clone(),
            files: Vec::new(),
            access: None,
        })
    })
}

fn canned_with(
    status: impl FnOnce(&Path) -> StatusFn,
    reply: impl Fn() -> Result<Response, ClientError> + Send + Sync + 'static,
    args: &[&str],
) -> (Run, Vec<Request>) {
    let root = tempfile::tempdir().expect("root");
    let canned = Arc::new(Canned {
        status: status(root.path()),
        reply: Box::new(reply),
        sent: Mutex::new(Vec::new()),
    });
    let run = run_with(Arc::new(Arc::clone(&canned)), root.path(), args, false);
    (run, canned.sent())
}

fn canned_run(
    reply: impl Fn() -> Result<Response, ClientError> + Send + Sync + 'static,
    args: &[&str],
) -> (Run, Vec<Request>) {
    canned_with(host_status, reply, args)
}

fn ok_data(data: Value) -> impl Fn() -> Result<Response, ClientError> + Send + Sync {
    move || Ok(Response::Ok { data: data.clone() })
}

fn evidence(kind: OperationKind) -> OperationEvidence {
    OperationEvidence::new(RequestEvidence {
        operation: kind,
        actions: vec![ActionEvidence {
            action: "toggleFloating".to_owned(),
            status: ActionStatus::AdmittedChanged,
        }],
    })
}

fn outcome(response: OperationResponse) -> Value { serde_json::to_value(response).expect("json") }

#[test]
fn partial_and_uncertain_outcomes_are_typed_stderr_failures() {
    let window = ["tiling", "window", "--toggle-floating"];
    let (run, sent) = canned_run(
        ok_data(outcome(OperationResponse::Partial(evidence(
            OperationKind::Window,
        )))),
        &window,
    );
    action_error(&run, "stache: IPC error: Tiling operation partially applied.");
    assert_eq!(sent.len(), 1);
    let (run, sent) = canned_run(
        ok_data(outcome(OperationResponse::Uncertain(evidence(
            OperationKind::Window,
        )))),
        &["tiling", "workspace", "--balance"],
    );
    action_error(
        &run,
        "stache: IPC error: Tiling operation outcome is uncertain; it may already have taken \
         effect.",
    );
    assert_eq!(sent.len(), 1, "never retried after uncertainty");
}

#[test]
fn enqueued_and_applied_are_silent_and_a_pause_must_be_applied() {
    for response in [
        OperationResponse::Enqueued(evidence(OperationKind::Window)),
        OperationResponse::Applied(evidence(OperationKind::Window)),
    ] {
        let (run, _) = canned_run(ok_data(outcome(response)), &[
            "tiling",
            "window",
            "--toggle-floating",
        ]);
        silent_ok(&run);
    }
    let (run, _) = canned_run(
        ok_data(outcome(OperationResponse::Applied(evidence(
            OperationKind::Pause,
        )))),
        &["tiling", "pause"],
    );
    silent_ok(&run);
    let (run, _) = canned_run(
        ok_data(outcome(OperationResponse::Enqueued(evidence(
            OperationKind::Resume,
        )))),
        &["tiling", "resume"],
    );
    silent_ok(&run);
    let (run, _) = canned_run(
        ok_data(outcome(OperationResponse::Enqueued(evidence(
            OperationKind::Pause,
        )))),
        &["tiling", "pause"],
    );
    action_error(
        &run,
        "stache: IPC error: Tiling operation outcome is uncertain; it may already have taken \
         effect.",
    );
}

#[test]
fn malformed_mutation_data_is_an_error_that_never_echoes_the_payload() {
    for data in [
        json!(null),
        json!("secret-token"),
        json!({"outcome": "bogus", "request": {}}),
        json!({"outcome": "enqueued", "secret": "token"}),
    ] {
        let (run, _) = canned_run(ok_data(data), &["tiling", "window", "--toggle-floating"]);
        action_error(
            &run,
            "stache: IPC error: Invalid response: the Tiling response could not be decoded",
        );
    }
}

#[test]
fn malformed_query_data_is_a_failure_never_an_empty_success() {
    let secret = "super-secret-token";
    for data in [
        json!(null),
        json!({"rows": []}),
        json!([{"unexpected": secret}]),
        json!([{"pid": 1, "name": "x", "bundleId": "y", "isHidden": false, "extra": secret}]),
        json!([{"pid": "one", "name": "x", "bundleId": "y", "isHidden": false}]),
        json!([{"pid": 1, "name": "x", "isHidden": false}]),
    ] {
        let (run, _) = canned_run(ok_data(data.clone()), &["tiling", "query", "apps"]);
        query_error(
            &run,
            "Error: Invalid response: the Tiling response could not be decoded\n",
        );
        let (run, _) = canned_run(ok_data(data), &["tiling", "query", "-j", "apps"]);
        query_error(
            &run,
            "{\"error\":\"Invalid response: the Tiling response could not be decoded\"}\n",
        );
        assert!(!run.out.contains(secret));
    }
}

type MakeError = fn() -> ClientError;

fn client_errors() -> Vec<(MakeError, &'static str)> {
    vec![
        (|| ClientError::Timeout, "Connection timed out"),
        (
            || ClientError::Io(std::io::Error::other("fixed source")),
            "IO error: fixed source",
        ),
        (
            || ClientError::InvalidResponse("fixed detail".to_owned()),
            "Invalid response: fixed detail",
        ),
        (|| ClientError::Serialize("ignored".to_owned()), SERIALIZE),
        (
            || {
                ClientError::Remote(WireError {
                    kind: WireErrorKind::MalformedRequest,
                    message: "bad line".to_owned(),
                })
            },
            "bad line",
        ),
    ]
}

const SERIALIZE: &str = "Cannot serialize tiling request";

#[test]
fn typed_client_errors_map_through_the_compatibility_adapter_not_the_sdk_display() {
    for (error, body) in client_errors() {
        for phase in ["send", "status"] {
            let go = |args: &[&str]| {
                if phase == "send" {
                    canned_run(move || Err(error()), args)
                } else {
                    canned_with(
                        |_| Box::new(move || Err(error())),
                        || panic!("a failed status never reaches the request"),
                        args,
                    )
                }
            };
            let (run, sent) = go(&["tiling", "query", "screens"]);
            query_error(&run, &format!("Error: {body}\n"));
            assert_eq!(sent.len(), usize::from(phase == "send"), "{body}");
            let (run, _) = go(&["tiling", "query", "--json", "apps"]);
            query_error(&run, &format!("{}\n", json!({ "error": body })));
            let (run, _) = go(&["tiling", "workspace", "--balance"]);
            action_error(&run, &format!("stache: IPC error: {body}"));
            let (run, _) = go(&["tiling", "resume"]);
            action_error(&run, &format!("stache: IPC error: {body}"));
        }
    }
    // The legacy offline forms, from either phase.
    for phase in ["send", "status"] {
        let go = |args: &[&str]| {
            if phase == "send" {
                canned_run(|| Err(ClientError::NotRunning), args)
            } else {
                canned_with(
                    |_| Box::new(|| Err(ClientError::NotRunning)),
                    || panic!("unreachable"),
                    args,
                )
            }
        };
        query_error(
            &go(&["tiling", "query", "screens"]).0,
            "Stache app is not running.\n",
        );
        query_error(
            &go(&["tiling", "query", "-j", "screens"]).0,
            "{\"error\":\"Stache app is not running\"}\n",
        );
        action_error(
            &go(&["tiling", "pause"]).0,
            "stache: IPC error: Stache app is not running",
        );
    }
}

#[test]
fn remote_wire_errors_keep_their_frozen_body_and_escape_control_characters() {
    for kind in [
        WireErrorKind::Unavailable,
        WireErrorKind::Disabled,
        WireErrorKind::Handler,
        WireErrorKind::InvalidParams,
        WireErrorKind::UnknownMethod,
        WireErrorKind::UnknownModule,
        WireErrorKind::MalformedRequest,
    ] {
        let reply = move || {
            Ok(Response::Error {
                error: WireError {
                    kind,
                    message: "Tiling handle is unavailable".to_owned(),
                },
            })
        };
        let (run, _) = canned_run(reply, &["tiling", "query", "windows"]);
        query_error(&run, "Error: Tiling handle is unavailable\n");
        let (run, _) = canned_run(reply, &["tiling", "window", "--toggle-floating"]);
        action_error(&run, "stache: IPC error: Tiling handle is unavailable");
    }
    let hostile = || {
        Ok(Response::Error {
            error: WireError {
                kind: WireErrorKind::Handler,
                message: "bad \u{1b}[2J \"quote\"".to_owned(),
            },
        })
    };
    let (run, _) = canned_run(hostile, &["tiling", "query", "apps"]);
    query_error(&run, "Error: bad \\u001b[2J \"quote\"\n");
    let (run, _) = canned_run(hostile, &["tiling", "query", "-j", "apps"]);
    query_error(&run, "{\"error\":\"bad \\u001b[2J \\\"quote\\\"\"}\n");
    let (run, _) = canned_run(hostile, &["tiling", "pause"]);
    action_error(&run, "stache: IPC error: bad \\u001b[2J \"quote\"");
    // Argument bodies reported by the host keep the legacy prefix.
    let argument = || {
        Ok(Response::Error {
            error: WireError {
                kind: WireErrorKind::Handler,
                message: "Invalid resize amount 'x'. Must be an integer.".to_owned(),
            },
        })
    };
    let (run, _) = canned_run(argument, &["tiling", "window", "--resize", "width", "1"]);
    action_error(&run, "stache: Invalid resize amount 'x'. Must be an integer.");
}

#[test]
fn flags_become_the_exact_wire_requests_with_raw_strings() {
    let applied = ok_data(outcome(OperationResponse::Applied(evidence(
        OperationKind::Window,
    ))));
    let (run, sent) = canned_run(applied, &[
        "tiling",
        "window",
        "--focus",
        "Prev",
        "--swap",
        "up",
        "--toggle-floating",
        "--preset",
        "Wide ",
        "--resize",
        "Width",
        "-5",
        "--resize",
        "height",
        "+7",
        "--send-to-screen",
        "Studio Display",
        "--send-to-workspace",
        " web",
    ]);
    silent_ok(&run);
    assert_eq!(
        (sent[0].module.as_str(), sent[0].method.as_str()),
        ("tiling", "window")
    );
    assert_eq!(
        sent[0].params,
        json!({
            "focus": "Prev", "swap": "up", "toggleFloating": true, "preset": "Wide ",
            "resize": [
                {"dimension": "Width", "amount": "-5"},
                {"dimension": "height", "amount": "+7"}
            ],
            "sendToScreen": "Studio Display", "sendToWorkspace": " web"
        }),
        "names are never trimmed or case-folded"
    );
}

#[test]
fn workspace_and_query_flags_become_the_exact_wire_requests() {
    let applied = ok_data(outcome(OperationResponse::Applied(evidence(
        OperationKind::Workspace,
    ))));
    let (_, sent) = canned_run(applied, &[
        "tiling",
        "workspace",
        "--focus",
        "Code",
        "--layout",
        "split-horizontal",
        "--balance",
        "--send-to-screen",
        "secondary",
    ]);
    assert_eq!(sent[0].method, "workspace");
    assert_eq!(
        sent[0].params,
        json!({
            "focus": "Code", "layout": "split-horizontal", "balance": true,
            "sendToScreen": "secondary"
        })
    );
    for (args, params) in [
        (vec!["tiling", "query", "screens"], json!({"kind": "screens"})),
        (vec!["tiling", "query", "apps"], json!({"kind": "apps"})),
        (
            vec!["tiling", "query", "workspaces", "--focused-screen"],
            json!({"kind": "workspaces", "focusedScreen": true}),
        ),
        (
            vec!["tiling", "query", "workspaces", "--screen", "main"],
            json!({"kind": "workspaces", "screen": "main"}),
        ),
        (
            vec![
                "tiling",
                "query",
                "windows",
                "--screen",
                "Left",
                "--workspace",
                "a b",
            ],
            json!({"kind": "windows", "screen": "Left", "workspace": "a b"}),
        ),
        (
            vec!["tiling", "query", "windows", "--focused-workspace"],
            json!({"kind": "windows", "focusedWorkspace": true}),
        ),
        (
            vec!["tiling", "query", "windows", "--focused-screen"],
            json!({"kind": "windows", "focusedScreen": true}),
        ),
    ] {
        let (run, sent) = canned_run(ok_data(json!([])), &args);
        ok(&run);
        assert_eq!(sent.len(), 1);
        assert_eq!(
            (sent[0].module.as_str(), sent[0].method.as_str()),
            ("tiling", "query")
        );
        assert_eq!(sent[0].params, params, "{args:?}");
    }
    let (_, sent) = canned_run(
        ok_data(outcome(OperationResponse::Applied(evidence(
            OperationKind::Pause,
        )))),
        &["tiling", "pause"],
    );
    assert_eq!((sent[0].method.as_str(), &sent[0].params), ("pause", &json!({})));
}

#[test]
fn control_characters_in_desktop_text_never_reach_the_terminal_in_tables() {
    let rows = json!([{
        "id": 1, "pid": 2, "appId": "a\u{1b}[31m", "appName": "n", "title": "\u{1b}]0;x\u{7}t",
        "workspace": "w", "frame": {"x": 0.0, "y": 0.0, "width": 1.0, "height": 1.0},
        "isMinimized": false, "isFullscreen": false, "isFloating": false, "isFocused": true
    }]);
    let (run, _) = canned_run(ok_data(rows), &["tiling", "query", "-d", "windows"]);
    ok(&run);
    assert!(
        !run.out.contains('\u{1b}') && !run.out.contains('\u{7}'),
        "{}",
        run.out
    );
    assert!(run.out.contains("\\u001b]0;x\\u0007t"), "{}", run.out);
}
