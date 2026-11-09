//! CLI contract tests.
//!
//! Every test uses a disposable root, `FakeDesktop`, a fake transport and the
//! real image pipeline over tiny generated images. Nothing touches the real
//! home, `~/.config`, a socket, a desktop or the legacy application, and no
//! test sleeps.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{fs, thread};

use clap::error::ErrorKind;
use clap::{Arg, Command};
use stache_macos::{
    DesktopError, DisplayEntry, DisplayFrame, DisplayId, DisplaySnapshot, FakeDesktop,
    GuardedDesktopBackend,
};
use stache_sdk::ipc::{ClientError, Request, Response, WireError, WireErrorKind};
use stache_sdk::{AppEnvironment, AppPaths, CancellationToken, ConfigEnvironment};

use super::adapters::{render_outcome, response_uncertainty_body};
use super::generate::render::{header_text, progress_text, results_text};
use super::generate::{Row, RowOutcome, Summary};
use super::token::{TokenResolution, resolve_token};
use super::{
    CancelGuard, CancelScope, IpcTransport, SetTransport, WallpapersCliEnv, command, contribution,
    parse_screen,
};
use crate::apply::{PlannedTarget, PrepareError, Prepared};
use crate::runtime::{
    Processor, RandomSource, RealProcessor, RuntimeClock, WallpaperScreen, WallpaperSelection,
    WallpapersOutcomeCause, WallpapersOutcomeStatus, WallpapersSetOutcome, WallpapersSetParams,
    WallpapersTargetFailure, WallpapersTargetOutcome, WallpapersTargetResult,
};
use crate::test_support::{solid, write_png};

// ---------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------

type Reply = Result<Response, ClientError>;

#[derive(Default)]
struct FakeTransport {
    replies: Mutex<Vec<Reply>>,
    requests: Mutex<Vec<Request>>,
    sockets: Mutex<Vec<PathBuf>>,
}

impl FakeTransport {
    fn replying(reply: Reply) -> Arc<Self> {
        let transport = Self::default();
        transport.replies.lock().unwrap().push(reply);
        Arc::new(transport)
    }

    fn absent() -> Arc<Self> { Self::replying(Err(ClientError::NotRunning)) }

    fn sends(&self) -> usize { self.requests.lock().unwrap().len() }

    fn params(&self) -> WallpapersSetParams {
        let requests = self.requests.lock().unwrap();
        assert_eq!(requests.len(), 1, "exactly one send");
        assert_eq!(
            (requests[0].module.as_str(), requests[0].method.as_str()),
            ("wallpapers", "set")
        );
        serde_json::from_value(requests[0].params.clone()).expect("typed params")
    }
}

impl SetTransport for FakeTransport {
    fn send(&self, socket: &Path, request: &Request) -> Reply {
        self.requests.lock().unwrap().push(request.clone());
        self.sockets.lock().unwrap().push(socket.to_path_buf());
        let mut replies = self.replies.lock().unwrap();
        if replies.is_empty() {
            // A second send would be a bug; make it loud and non-retryable.
            return Err(ClientError::Io(io::Error::other("unexpected extra send")));
        }
        replies.remove(0)
    }
}

#[derive(Default)]
struct CountingProcessor {
    calls: AtomicUsize,
    sources: Mutex<Vec<PathBuf>>,
}

impl Processor for CountingProcessor {
    fn prepare(
        &self,
        plan: &[PlannedTarget],
        paths: &AppPaths,
        cancel: &CancellationToken,
    ) -> Result<Prepared, PrepareError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.sources
            .lock()
            .unwrap()
            .extend(plan.iter().map(|planned| planned.source.clone()));
        RealProcessor.prepare(plan, paths, cancel)
    }
}

struct ScriptedRandom(Mutex<Vec<usize>>);

impl RandomSource for ScriptedRandom {
    fn index(&self, length: usize) -> usize {
        let mut draws = self.0.lock().unwrap();
        let draw = if draws.is_empty() { 0 } else { draws.remove(0) };
        draw % length
    }
}

/// A clock whose readings alternate between the base and two seconds later
/// (start, end), so the summary time is fixed. It can cancel on a start read.
struct StepClock {
    base: Instant,
    reads: AtomicUsize,
    cancel_on_first: Option<CancellationToken>,
}

impl RuntimeClock for StepClock {
    fn now(&self) -> Instant {
        let read = self.reads.fetch_add(1, Ordering::SeqCst);
        if read.is_multiple_of(2) {
            if let Some(token) = &self.cancel_on_first {
                token.cancel();
            }
            self.base
        } else {
            self.base + Duration::from_secs(2)
        }
    }
}

#[allow(clippy::struct_field_names)]
struct Out {
    code: ExitCode,
    out: String,
    err: String,
}

fn two_displays() -> DisplaySnapshot {
    DisplaySnapshot::new(vec![
        DisplayEntry {
            id: DisplayId::new(1),
            frame: DisplayFrame { width: 16.0, height: 9.0 },
            scale: 1.0,
        },
        DisplayEntry {
            id: DisplayId::new(2),
            frame: DisplayFrame { width: 12.0, height: 8.0 },
            scale: 2.0,
        },
    ])
}

struct Rig {
    root: tempfile::TempDir,
    transport: Arc<FakeTransport>,
    desktop: FakeDesktop,
    processor: Arc<CountingProcessor>,
    draws: Vec<usize>,
    clock: Option<Arc<StepClock>>,
    token: CancellationToken,
    observer: Option<Arc<dyn crate::batch::BatchObserver + Send + Sync>>,
}

impl Rig {
    fn new(transport: Arc<FakeTransport>) -> Self {
        let root = tempfile::tempdir().expect("root");
        for directory in ["home", "config", "cwd", "tmp"] {
            fs::create_dir_all(root.path().join(directory)).expect("directory");
        }
        Self {
            root,
            transport,
            desktop: FakeDesktop::with_snapshot(two_displays()),
            processor: Arc::new(CountingProcessor::default()),
            draws: Vec::new(),
            clock: None,
            token: CancellationToken::new(),
            observer: None,
        }
    }

    fn standalone() -> Self { Self::new(FakeTransport::absent()) }

    fn home(&self) -> PathBuf { self.root.path().join("home") }

    fn config(&self) -> PathBuf { self.root.path().join("config") }

    fn cwd(&self) -> PathBuf { self.root.path().join("cwd") }

    fn write_config(&self, text: &str) {
        fs::write(self.config().join("wallpapers.toml"), text).expect("config");
    }

    #[allow(clippy::unused_self)]
    fn png(&self, path: &Path) -> PathBuf {
        fs::create_dir_all(path.parent().expect("parent")).expect("parent");
        write_png(path, &solid(8, 8, [10, 20, 30]));
        path.to_path_buf()
    }

    fn env(&self) -> WallpapersCliEnv {
        let desktop: Arc<dyn GuardedDesktopBackend> = Arc::new(self.desktop.clone());
        let token = self.token.clone();
        let scope: CancelScope = Arc::new(move || CancelGuard::new(token.clone()));
        let cwd = self.cwd();
        let mut env = WallpapersCliEnv::new(
            ConfigEnvironment {
                xdg_config_home: None,
                home: Some(self.home()),
            },
            AppEnvironment {
                home: Some(self.root.path().to_path_buf()),
                temporary: Some(self.root.path().join("tmp")),
            },
            desktop,
            Arc::new(|json: &str, ansi: bool| {
                if ansi {
                    format!("<<{json}>>")
                } else {
                    json.to_owned()
                }
            }),
            scope,
        );
        env.current_dir = Arc::new(move || Ok(cwd.clone()));
        env.transport = Arc::<FakeTransport>::clone(&self.transport);
        env.processor = Arc::<CountingProcessor>::clone(&self.processor);
        env.random = Arc::new(ScriptedRandom(Mutex::new(self.draws.clone())));
        env.progress_interval = Duration::from_millis(1);
        if let Some(observer) = &self.observer {
            env.observer = Arc::clone(observer);
        }
        if let Some(clock) = &self.clock {
            env.clock = Arc::<StepClock>::clone(clock);
        }
        env
    }

    fn matches(&self, args: &[&str]) -> Result<clap::ArgMatches, clap::Error> {
        let root = Command::new("stache")
            .arg(
                Arg::new("config")
                    .short('c')
                    .long("config")
                    .global(true)
                    .value_parser(clap::value_parser!(PathBuf)),
            )
            .subcommand(command());
        let config = self.config();
        let mut full = vec![
            "stache".to_owned(),
            "-c".to_owned(),
            config.display().to_string(),
        ];
        full.push("wallpaper".to_owned());
        full.extend(args.iter().map(|arg| (*arg).to_owned()));
        root.try_get_matches_from(full)
    }

    fn run(&self, args: &[&str], ansi: bool) -> Out {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = self.run_into(args, ansi, &mut out, &mut err);
        Out {
            code,
            out: String::from_utf8(out).expect("utf-8 stdout"),
            err: String::from_utf8(err).expect("utf-8 stderr"),
        }
    }

    fn run_into(
        &self,
        args: &[&str],
        ansi: bool,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> ExitCode {
        let matches = self.matches(args).unwrap_or_else(|error| panic!("{args:?}: {error}"));
        let (_, sub) = matches.subcommand().expect("wallpaper subcommand");
        contribution(self.env()).executor().execute(sub, ansi, out, err)
    }

    fn processor_calls(&self) -> usize { self.processor.calls.load(Ordering::SeqCst) }
}

fn strip_ansi(text: &str) -> String {
    let mut stripped = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            stripped.push(c);
        }
    }
    stripped.replace('\r', "")
}

fn success_outcome() -> WallpapersSetOutcome {
    outcome_for(
        &[(1, WallpapersTargetResult::Applied)],
        WallpapersOutcomeStatus::FullSuccess,
        None,
    )
}

fn outcome_for(
    targets: &[(u32, WallpapersTargetResult)],
    status: WallpapersOutcomeStatus,
    cause: Option<WallpapersOutcomeCause>,
) -> WallpapersSetOutcome {
    let pick = |wanted: fn(&WallpapersTargetResult) -> bool| -> Vec<u32> {
        targets.iter().filter(|(_, t)| wanted(t)).map(|(d, _)| *d).collect()
    };
    WallpapersSetOutcome {
        status,
        cause,
        applied: pick(|t| matches!(t, WallpapersTargetResult::Applied)),
        failed: pick(|t| matches!(t, WallpapersTargetResult::Failed { .. })),
        not_attempted: pick(|t| matches!(t, WallpapersTargetResult::NotAttempted)),
        unknown: pick(|t| matches!(t, WallpapersTargetResult::Unknown { .. })),
        targets: targets
            .iter()
            .map(|(display, target)| WallpapersTargetOutcome {
                display: *display,
                target: target.clone(),
            })
            .collect(),
    }
}

#[allow(clippy::unnecessary_wraps)]
fn ok_response(outcome: &WallpapersSetOutcome) -> Reply {
    Ok(Response::Ok {
        data: serde_json::to_value(outcome).expect("outcome json"),
    })
}

const SUCCESS: &str = "Wallpaper set successfully.\n";

// ---------------------------------------------------------------------------
// WP04-01: command tree, help and grammar
// ---------------------------------------------------------------------------

#[test]
fn the_tree_has_exactly_the_legacy_wallpaper_surface() {
    let command = command();
    assert_eq!(command.get_name(), "wallpaper");
    let names: Vec<&str> = command.get_subcommands().map(Command::get_name).collect();
    assert_eq!(names, ["set", "generate-all", "list"]);
    let set = command.find_subcommand("set").expect("set");
    let flags: Vec<(String, Option<char>)> = set
        .get_arguments()
        .map(|arg| (arg.get_id().to_string(), arg.get_short()))
        .collect();
    assert_eq!(flags, [
        ("path".to_owned(), None),
        ("random".to_owned(), Some('r')),
        ("screen".to_owned(), Some('s')),
    ]);
    let screen = set.get_arguments().find(|arg| arg.get_id() == "screen").expect("screen");
    assert_eq!(screen.get_long(), Some("screen"));
    assert_eq!(
        screen
            .get_default_values()
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect::<Vec<_>>(),
        ["all"]
    );
    assert!(command.clone().render_long_help().to_string().contains("generate-all"));
}

#[test]
fn set_help_keeps_the_legacy_text_and_examples() {
    let mut command = command();
    let help = command.find_subcommand_mut("set").expect("set").render_long_help().to_string();
    for needle in [
        "Set the desktop wallpaper.",
        "Set a specific wallpaper by providing a path, or use --random to set",
        "stache wallpaper set /path/to/image.jpg --screen main # Specific wallpaper for main screen",
        "stache wallpaper set --random --screen 2              # Random wallpaper for screen 2",
        "[PATH]",
        "-r, --random",
        "-s, --screen <SCREEN>",
        "Values: all, main, <index> (1-based).",
    ] {
        assert!(help.contains(needle), "missing {needle:?} in\n{help}");
    }
}

#[test]
fn the_screen_value_parser_follows_the_legacy_rules() {
    assert_eq!(parse_screen("all"), Ok(WallpaperScreen::All));
    assert_eq!(parse_screen("ALL"), Ok(WallpaperScreen::All));
    assert_eq!(parse_screen("Main"), Ok(WallpaperScreen::Main));
    assert_eq!(parse_screen("0"), Ok(WallpaperScreen::Index { index: 0 }));
    assert_eq!(parse_screen("1"), Ok(WallpaperScreen::Index { index: 1 }));
    assert_eq!(parse_screen("7"), Ok(WallpaperScreen::Index { index: 7 }));
    for bad in ["", "two", "-1", "1.5", "4294967296", "99999999999999999999"] {
        assert_eq!(
            parse_screen(bad),
            Err(format!(
                "Invalid screen value '{bad}'. Expected 'all', 'main', or a positive integer."
            )),
            "{bad:?}"
        );
    }
    // main, 1 and legacy 0 all address the primary display.
    for primary in [
        WallpaperScreen::Main,
        WallpaperScreen::Index { index: 0 },
        WallpaperScreen::Index { index: 1 },
    ] {
        let snapshot = two_displays();
        let selected = crate::resolve_targets(&snapshot, &primary.as_target()).expect("primary");
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id(), DisplayId::new(1));
    }
}

#[test]
fn a_bad_screen_is_a_grammar_error_before_any_work() {
    let rig = Rig::new(FakeTransport::absent());
    for bad in ["two", "4294967296"] {
        let error = rig.matches(&["set", "--random", "--screen", bad]).expect_err("grammar");
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        assert!(
            error.to_string().contains(&format!(
                "Invalid screen value '{bad}'. Expected 'all', 'main', or a positive integer."
            )),
            "{error}"
        );
        assert_eq!(error.exit_code(), 2);
    }
    assert_eq!(rig.transport.sends(), 0);
    assert_eq!(rig.processor_calls(), 0);
    assert_eq!(rig.desktop.guarded_snapshot_reads(), 0);
}

#[test]
fn no_subcommand_and_unknown_flags_are_grammar_errors() {
    let rig = Rig::new(FakeTransport::absent());
    assert!(rig.matches(&[]).is_err());
    assert!(rig.matches(&["set", "--bogus"]).is_err());
    assert!(rig.matches(&["nope"]).is_err());
    let matches = rig.matches(&["set", "-r", "-s", "main"]).expect("short flags");
    let (_, wallpaper) = matches.subcommand().expect("wallpaper");
    let (_, set) = wallpaper.subcommand().expect("set");
    assert!(set.get_flag("random"));
    assert_eq!(
        set.get_one::<WallpaperScreen>("screen"),
        Some(&WallpaperScreen::Main)
    );
    let defaults = rig.matches(&["set", "--random"]).expect("defaults");
    let (_, wallpaper) = defaults.subcommand().expect("wallpaper");
    let (_, set) = wallpaper.subcommand().expect("set");
    assert_eq!(
        set.get_one::<WallpaperScreen>("screen"),
        Some(&WallpaperScreen::All)
    );
}

#[test]
fn the_global_config_flag_reaches_the_subcommand_matches() {
    let rig = Rig::new(FakeTransport::absent());
    let matches = rig.matches(&["list"]).expect("list");
    let (_, wallpaper) = matches.subcommand().expect("wallpaper");
    let (_, list) = wallpaper.subcommand().expect("list");
    assert_eq!(list.get_one::<PathBuf>("config"), Some(&rig.config()));
}

#[test]
fn both_and_neither_fail_first_with_exact_text() {
    let rig = Rig::new(FakeTransport::absent());
    // Poison every later step: a malformed config, so any config read fails.
    rig.write_config("not toml =");
    let both = rig.run(&["set", "image.png", "--random"], false);
    assert_eq!(both.code, ExitCode::FAILURE);
    assert_eq!(both.out, "");
    assert_eq!(
        both.err,
        "stache: Cannot specify both <path> and --random. Use one or the other.\n"
    );
    let neither = rig.run(&["set"], false);
    assert_eq!(neither.code, ExitCode::FAILURE);
    assert_eq!(neither.out, "");
    assert_eq!(
        neither.err,
        "stache: Either <path> or --random must be specified.\n"
    );
    assert_eq!(rig.transport.sends(), 0, "no socket work");
    assert_eq!(rig.processor_calls(), 0);
    assert_eq!(
        rig.desktop.guarded_snapshot_reads() + rig.desktop.snapshot_reads(),
        0
    );
    assert!(rig.desktop.calls().is_empty());
}

// ---------------------------------------------------------------------------
// WP04-04: token resolution
// ---------------------------------------------------------------------------

#[test]
fn a_bare_existing_file_in_the_cwd_wins_over_a_collection_name() {
    let rig = Rig::new(FakeTransport::replying(ok_response(&success_outcome())));
    let file = rig.png(&rig.cwd().join("wall.png"));
    rig.write_config("list = [\"wall.png\"]\n");
    let run = rig.run(&["set", "wall.png"], false);
    assert_eq!(
        (run.code, run.out.as_str(), run.err.as_str()),
        (ExitCode::SUCCESS, SUCCESS, "")
    );
    assert_eq!(rig.transport.params().selection, WallpaperSelection::File {
        path: file.display().to_string()
    });
}

#[test]
fn a_bare_non_file_becomes_a_name_and_keeps_its_spaces() {
    let rig = Rig::new(FakeTransport::replying(ok_response(&success_outcome())));
    let run = rig.run(&["set", " Sunset "], false);
    assert_eq!(run.code, ExitCode::SUCCESS, "{}", run.err);
    assert_eq!(rig.transport.params().selection, WallpaperSelection::Name {
        name: " Sunset ".to_owned()
    });
}

#[test]
fn resolve_token_precedence_table() {
    let root = tempfile::tempdir().expect("root");
    let cwd = root.path().join("cwd");
    let home = root.path().join("home");
    fs::create_dir_all(cwd.join("sub")).expect("sub");
    fs::create_dir_all(&home).expect("home");
    let png = |path: &Path| write_png(path, &solid(2, 2, [1, 2, 3]));
    png(&cwd.join("a.png"));
    png(&cwd.join(" spaced .png"));
    png(&cwd.join("sub/b.JPG"));
    png(&home.join("c.webp"));
    fs::write(cwd.join("notes.txt"), b"x").expect("text");
    fs::create_dir_all(cwd.join("dir.png")).expect("dir named like an image");

    let resolve = |token: &str| resolve_token(token, &cwd, Some(&home));
    assert_eq!(
        resolve("a.png").unwrap(),
        TokenResolution::File(cwd.join("a.png"))
    );
    // Leading and trailing filename spaces are literal.
    assert_eq!(
        resolve(" spaced .png").unwrap(),
        TokenResolution::File(cwd.join(" spaced .png"))
    );
    assert_eq!(
        resolve("spaced").unwrap(),
        TokenResolution::Name("spaced".to_owned())
    );
    // Relative path-shaped tokens resolve against the invocation cwd.
    assert_eq!(
        resolve("sub/b.JPG").unwrap(),
        TokenResolution::File(cwd.join("sub/b.JPG"))
    );
    assert_eq!(
        resolve("./a.png").unwrap(),
        TokenResolution::File(cwd.join("./a.png"))
    );
    let absolute = cwd.join("a.png");
    assert_eq!(
        resolve(absolute.to_str().unwrap()).unwrap(),
        TokenResolution::File(absolute.clone())
    );
    // Tilde expands from the injected home only.
    assert_eq!(
        resolve("~/c.webp").unwrap(),
        TokenResolution::File(home.join("c.webp"))
    );
    assert!(resolve("~").is_err(), "home is a directory");
    assert_eq!(
        resolve_token("~/c.webp", &cwd, None).unwrap_err().reason(),
        "cannot expand `~`: no home directory is known"
    );
    // `~name` is not home syntax: it is a bare name.
    assert_eq!(resolve("~c").unwrap(), TokenResolution::Name("~c".to_owned()));
    // Path-shaped but unusable is an argument error, never a name fallback.
    for bad in [
        "missing/x.png",
        "/nope/x.png",
        "sub/",
        "sub/notes.txt",
        "./notes.txt",
        "~/missing.png",
    ] {
        let error = resolve(bad).expect_err(bad);
        assert_eq!(error.token(), bad);
    }
    // A bare unsupported file or a directory with an image name is a name.
    assert_eq!(
        resolve("notes.txt").unwrap(),
        TokenResolution::Name("notes.txt".to_owned())
    );
    assert_eq!(
        resolve("dir.png").unwrap(),
        TokenResolution::Name("dir.png".to_owned())
    );
    assert!(resolve("").is_err());
}

#[test]
fn a_missing_path_shaped_argument_is_an_argument_error_with_no_side_effects() {
    let rig = Rig::new(FakeTransport::absent());
    rig.write_config("list = [\"missing\"]\n");
    let run = rig.run(&["set", "./missing.png"], false);
    assert_eq!(run.code, ExitCode::FAILURE);
    assert_eq!(run.out, "");
    assert!(
        run.err
            .starts_with("stache: Wallpaper error: Invalid wallpaper path \"./missing.png\": "),
        "{}",
        run.err
    );
    assert_eq!(rig.transport.sends(), 0, "never sent, never a name fallback");
    assert_eq!(rig.processor_calls(), 0);
}

#[test]
fn a_tilde_argument_expands_from_the_injected_home() {
    let rig = Rig::new(FakeTransport::replying(ok_response(&success_outcome())));
    let file = rig.png(&rig.home().join("pics/h.png"));
    let run = rig.run(&["set", "~/pics/h.png"], false);
    assert_eq!(run.code, ExitCode::SUCCESS, "{}", run.err);
    assert_eq!(rig.transport.params().selection, WallpaperSelection::File {
        path: file.display().to_string()
    });
}

#[test]
fn the_request_carries_the_absolute_config_directory_and_screen() {
    for (flag, expected) in [
        (None, WallpaperScreen::All),
        (Some("main"), WallpaperScreen::Main),
        (Some("MAIN"), WallpaperScreen::Main),
        (Some("0"), WallpaperScreen::Index { index: 0 }),
        (Some("1"), WallpaperScreen::Index { index: 1 }),
        (Some("2"), WallpaperScreen::Index { index: 2 }),
    ] {
        let rig = Rig::new(FakeTransport::replying(ok_response(&success_outcome())));
        let mut args = vec!["set", "--random"];
        if let Some(flag) = flag {
            args.extend(["--screen", flag]);
        }
        let run = rig.run(&args, false);
        assert_eq!(run.code, ExitCode::SUCCESS, "{flag:?}: {}", run.err);
        let params = rig.transport.params();
        assert_eq!(params.screen, expected, "{flag:?}");
        assert_eq!(params.selection, WallpaperSelection::Random);
        assert_eq!(params.config_directory, rig.config().display().to_string());
    }
}

// ---------------------------------------------------------------------------
// WP04-03: routing
// ---------------------------------------------------------------------------

const UNCERTAIN: &str = "stache: Wallpaper error: Wallpaper outcome is unknown after a send or response failure. Check the desktop before retrying; no automatic retry was attempted.\n";

fn assert_uncertain(rig: &Rig, run: &Out) {
    assert_eq!(run.code, ExitCode::FAILURE);
    assert_eq!(run.out, "");
    assert_eq!(run.err, UNCERTAIN);
    assert_eq!(rig.transport.sends(), 1, "never more than one send");
    assert_eq!(rig.processor_calls(), 0, "no local processing");
    assert!(rig.desktop.calls().is_empty(), "no local application");
    assert_eq!(rig.desktop.guarded_snapshot_reads(), 0, "no local snapshot");
}

#[test]
fn every_client_error_except_not_running_is_an_uncertain_outcome() {
    let errors: Vec<Box<dyn Fn() -> ClientError>> = vec![
        Box::new(|| ClientError::Timeout),
        Box::new(|| ClientError::Io(io::Error::from(io::ErrorKind::BrokenPipe))),
        Box::new(|| ClientError::Io(io::Error::from(io::ErrorKind::ConnectionReset))),
        Box::new(|| ClientError::Serialize("late".to_owned())),
        Box::new(|| ClientError::InvalidResponse("incomplete response".to_owned())),
        Box::new(|| {
            ClientError::Remote(WireError {
                kind: WireErrorKind::Handler,
                message: "boom".to_owned(),
            })
        }),
    ];
    for error in errors {
        let rig = Rig::new(FakeTransport::replying(Err(error())));
        rig.png(&rig.cwd().join("w.png"));
        let run = rig.run(&["set", "w.png"], false);
        assert_uncertain(&rig, &run);
    }
}

#[test]
fn a_malformed_ok_payload_is_uncertain() {
    for data in [
        serde_json::json!({ "status": "fullSuccess" }),
        serde_json::json!("ok"),
        serde_json::json!(null),
        serde_json::json!({ "status": "bogus", "applied": [], "failed": [], "notAttempted": [], "unknown": [], "targets": [] }),
    ] {
        let rig = Rig::new(FakeTransport::replying(Ok(Response::Ok { data })));
        let run = rig.run(&["set", "--random"], false);
        assert_uncertain(&rig, &run);
    }
}

#[test]
fn a_handler_wire_error_is_uncertain_and_other_wire_errors_are_pre_native() {
    let rig = Rig::new(FakeTransport::replying(Ok(Response::failure(
        WireErrorKind::Handler,
        "panic",
    ))));
    let run = rig.run(&["set", "--random"], false);
    assert_uncertain(&rig, &run);

    for (kind, tag) in [
        (WireErrorKind::MalformedRequest, "malformedRequest"),
        (WireErrorKind::InvalidParams, "invalidParams"),
        (WireErrorKind::UnknownModule, "unknownModule"),
        (WireErrorKind::UnknownMethod, "unknownMethod"),
        (WireErrorKind::Disabled, "disabled"),
        (WireErrorKind::Unavailable, "unavailable"),
    ] {
        let rig = Rig::new(FakeTransport::replying(Ok(Response::failure(
            kind,
            "bad \"thing\"",
        ))));
        let run = rig.run(&["set", "--random"], false);
        assert_eq!(run.code, ExitCode::FAILURE);
        assert_eq!(
            run.err,
            format!(
                "stache: Wallpaper error: Wallpaper operation failed before native application ({tag}): \"bad \\\"thing\\\"\". No native change was issued.\n"
            )
        );
        assert_eq!(rig.transport.sends(), 1);
        assert!(rig.desktop.calls().is_empty());
        assert_eq!(rig.processor_calls(), 0);
    }
}

#[test]
fn a_gated_host_refusal_prints_the_gate_message_and_applies_nothing_locally() {
    let message = "Stache Next is waiting for Accessibility permission. Grant it in System Settings > Privacy & Security > Accessibility.";
    let rig = Rig::new(FakeTransport::replying(Ok(Response::failure(
        WireErrorKind::Unavailable,
        message,
    ))));
    let run = rig.run(&["set", "--random"], false);
    assert_eq!(run.code, ExitCode::FAILURE);
    assert_eq!(run.out, "");
    assert!(run.err.contains("(unavailable)"), "{}", run.err);
    assert!(run.err.contains(message), "{}", run.err);
    assert!(run.err.contains("No native change was issued."), "{}", run.err);
    assert_eq!(rig.transport.sends(), 1, "no standalone fallback");
    assert_eq!(rig.processor_calls(), 0);
    assert!(rig.desktop.calls().is_empty());
    assert_eq!(rig.desktop.guarded_snapshot_reads(), 0);
}

#[test]
fn only_a_proven_absence_allows_exactly_one_standalone_apply() {
    let rig = Rig::standalone();
    let file = rig.png(&rig.cwd().join("w.png"));
    let run = rig.run(&["set", "w.png"], false);
    assert_eq!(
        (run.code, run.out.as_str(), run.err.as_str()),
        (ExitCode::SUCCESS, SUCCESS, "")
    );
    assert_eq!(
        rig.transport.sends(),
        1,
        "one probe-free send, then one local attempt"
    );
    assert_eq!(rig.processor_calls(), 1);
    let calls = rig.desktop.calls();
    assert_eq!(calls.len(), 2, "one set per display");
    assert_eq!(calls[0].display, DisplayId::new(1));
    assert_eq!(calls[1].display, DisplayId::new(2));
    // The cache entry is per display size and lives under the identity root.
    assert!(
        calls.iter().all(|call| call.image.exists()),
        "prepared cache outputs"
    );
    assert!(file.exists(), "the source is never modified");
}

#[test]
fn a_host_reply_renders_success_and_failure_through_the_same_adapter() {
    let rig = Rig::new(FakeTransport::replying(ok_response(&success_outcome())));
    let run = rig.run(&["set", "--random"], false);
    assert_eq!(
        (run.code, run.out.as_str(), run.err.as_str()),
        (ExitCode::SUCCESS, SUCCESS, "")
    );
    assert_eq!(rig.processor_calls(), 0, "the host does the work");
    assert!(rig.desktop.calls().is_empty());

    let partial = outcome_for(
        &[
            (1, WallpapersTargetResult::Applied),
            (2, WallpapersTargetResult::Failed {
                reason: WallpapersTargetFailure::Native,
                message: "rejected".to_owned(),
            }),
        ],
        WallpapersOutcomeStatus::Partial,
        Some(WallpapersOutcomeCause::Native { message: "x".to_owned() }),
    );
    let rig = Rig::new(FakeTransport::replying(ok_response(&partial)));
    let run = rig.run(&["set", "--random"], false);
    assert_eq!(run.code, ExitCode::FAILURE);
    assert_eq!(run.out, "", "never the success line");
    assert_eq!(
        run.err,
        "stache: Wallpaper error: Wallpaper application partial: applied=[1]; failed=[2]; not-attempted=[]; unknown=[]. Check the desktop before retrying; no automatic retry was attempted.\n"
    );
}

#[test]
fn not_running_with_a_missing_config_uses_defaults_for_a_file_but_not_for_random() {
    let rig = Rig::standalone();
    rig.png(&rig.cwd().join("w.png"));
    assert!(!rig.config().join("wallpapers.toml").exists());
    let run = rig.run(&["set", "w.png"], false);
    assert_eq!(run.code, ExitCode::SUCCESS, "{}", run.err);

    let rig = Rig::standalone();
    let run = rig.run(&["set", "--random"], false);
    assert_eq!(run.code, ExitCode::FAILURE);
    assert_eq!(
        run.err,
        "stache: Wallpaper error: Wallpaper operation failed before native application (source): \"the wallpaper pool is empty\". No native change was issued.\n"
    );
    assert_eq!(rig.processor_calls(), 0);
    assert!(rig.desktop.calls().is_empty());
}

#[test]
fn a_malformed_config_fails_and_never_falls_back_to_defaults() {
    for text in [
        "enabled = ",
        "",
        "interval = \"soon\"\n",
        "path = \"a\\u0000b\"\n",
    ] {
        let rig = Rig::standalone();
        rig.png(&rig.cwd().join("w.png"));
        rig.write_config(text);
        let run = rig.run(&["set", "w.png"], false);
        assert_eq!(run.code, ExitCode::FAILURE, "{text:?}");
        assert!(run.err.starts_with("stache: Wallpaper error: "), "{}", run.err);
        assert!(run.err.contains("wallpapers.toml"), "{}", run.err);
        assert_eq!(rig.processor_calls(), 0);
        assert!(rig.desktop.calls().is_empty());
    }
}

#[test]
fn a_valid_explicit_file_ignores_a_bad_or_empty_pool() {
    for text in [
        "path = \"does-not-exist\"\n",
        "list = []\n",
        "list = [\"missing.png\"]\nenabled = false\n",
    ] {
        let rig = Rig::standalone();
        rig.png(&rig.cwd().join("w.png"));
        rig.write_config(text);
        let run = rig.run(&["set", "w.png"], false);
        assert_eq!(run.code, ExitCode::SUCCESS, "{text}: {}", run.err);
        assert_eq!(rig.desktop.calls().len(), 2);
    }
}

#[test]
fn a_standalone_name_uses_the_current_config_pool_with_first_match_wins() {
    let rig = Rig::standalone();
    rig.png(&rig.config().join("one/Sunset.png"));
    rig.png(&rig.config().join("two/sunset.png"));
    rig.png(&rig.config().join("sunset.png.png"));
    rig.write_config("list = [\"  one/Sunset.png \", \"two/sunset.png\", \"sunset.png.png\"]\n");
    let run = rig.run(&["set", "SUNSET", "--screen", "main"], false);
    assert_eq!(run.code, ExitCode::SUCCESS, "{}", run.err);
    assert_eq!(rig.desktop.calls().len(), 1);
    // The first matching pool entry (config-relative, trimmed) was prepared.
    assert_eq!(*rig.processor.sources.lock().unwrap(), [rig
        .config()
        .join("one/Sunset.png")]);
    assert_eq!(rig.processor_calls(), 1);

    // A name that matches nothing is a typed invalid selection.
    let rig2 = Rig::standalone();
    rig2.write_config("list = []\n");
    let run = rig2.run(&["set", "nothing"], false);
    assert_eq!(run.code, ExitCode::FAILURE);
    assert!(run.err.contains("(invalid-selection)"), "{}", run.err);
    assert_eq!(rig2.processor_calls(), 0);
}

#[test]
fn a_config_list_is_relative_to_the_config_directory_not_the_cwd() {
    let rig = Rig::standalone();
    // Same file name in both places; only the config-relative one is a pool entry.
    rig.png(&rig.config().join("pool.png"));
    rig.write_config("list = [\" pool.png \", \"~/home.png\"]\n");
    rig.png(&rig.home().join("home.png"));
    let run = rig.run(&["list"], false);
    assert_eq!(run.code, ExitCode::SUCCESS, "{}", run.err);
    let listed: Vec<String> = serde_json::from_str(&run.out).expect("json");
    assert_eq!(listed, [
        rig.config().join("pool.png").display().to_string(),
        rig.home().join("home.png").display().to_string(),
    ]);
}

#[test]
fn standalone_screen_range_is_checked_before_any_decode_or_cache_write() {
    for screen in ["3", "99", "4294967295"] {
        let rig = Rig::standalone();
        rig.png(&rig.cwd().join("w.png"));
        let run = rig.run(&["set", "w.png", "--screen", screen], false);
        assert_eq!(run.code, ExitCode::FAILURE, "{screen}");
        assert!(run.err.contains("(invalid-selection)"), "{}", run.err);
        assert!(run.err.contains("out of range"), "{}", run.err);
        assert_eq!(rig.processor_calls(), 0, "zero processor calls");
        assert!(rig.desktop.calls().is_empty());
        let paths = AppPaths::resolve(&AppEnvironment {
            home: Some(rig.root.path().to_path_buf()),
            temporary: Some(rig.root.path().join("tmp")),
        })
        .expect("paths");
        assert!(
            !paths.cache_directory().join("wallpapers").exists(),
            "no cache write"
        );
    }
}

#[test]
fn standalone_random_draws_independently_per_display() {
    let mut rig = Rig::standalone();
    rig.png(&rig.config().join("a.png"));
    rig.png(&rig.config().join("b.png"));
    rig.write_config("list = [\"a.png\", \"b.png\"]\n");
    rig.draws = vec![1, 0];
    let run = rig.run(&["set", "--random"], false);
    assert_eq!(run.code, ExitCode::SUCCESS, "{}", run.err);
    assert_eq!(rig.desktop.calls().len(), 2);
    assert_eq!(*rig.processor.sources.lock().unwrap(), [
        rig.config().join("b.png"),
        rig.config().join("a.png")
    ]);
}

#[test]
fn a_standalone_native_failure_uses_the_application_adapter() {
    let rig = Rig::standalone();
    rig.png(&rig.cwd().join("w.png"));
    rig.desktop.set_outcome(
        DisplayId::new(2),
        Err(DesktopError::Native {
            display: DisplayId::new(2),
            message: "rejected".to_owned(),
        }),
    );
    let run = rig.run(&["set", "w.png"], false);
    assert_eq!(run.code, ExitCode::FAILURE);
    assert_eq!(run.out, "");
    assert_eq!(
        run.err,
        "stache: Wallpaper error: Wallpaper application partial: applied=[1]; failed=[2]; not-attempted=[]; unknown=[]. Check the desktop before retrying; no automatic retry was attempted.\n"
    );
}

#[test]
fn a_cancelled_standalone_set_changes_nothing() {
    let rig = Rig::standalone();
    rig.png(&rig.cwd().join("w.png"));
    rig.token.cancel();
    let run = rig.run(&["set", "w.png"], false);
    assert_eq!(run.code, ExitCode::FAILURE);
    assert!(rig.desktop.calls().is_empty());
    assert_eq!(rig.processor_calls(), 0);
    assert!(run.err.contains("cancelled"), "{}", run.err);
}

#[test]
fn the_standalone_path_builds_no_service_or_host_pieces() {
    // The executor holds only injected dependencies. A standalone set never
    // constructs a `WallpapersService` (its coordinator would wait on a main
    // queue nobody drains), registry, tray, watcher, timer or listener, so the
    // only observable effects are one transport attempt, one processor call
    // and the desktop calls; no control socket is created or bound.
    let rig = Rig::standalone();
    rig.png(&rig.cwd().join("w.png"));
    let run = rig.run(&["set", "w.png"], false);
    assert_eq!(run.code, ExitCode::SUCCESS);
    assert_eq!(rig.transport.sends(), 1);
    assert_eq!(rig.processor_calls(), 1);
    let socket = rig.transport.sockets.lock().unwrap()[0].clone();
    assert!(
        !socket.exists(),
        "{} must not be bound by the CLI",
        socket.display()
    );
}

// ---------------------------------------------------------------------------
// Adapters
// ---------------------------------------------------------------------------

fn body(outcome: &WallpapersSetOutcome) -> String {
    render_outcome(outcome).expect_err("an error body")
}

#[test]
fn full_success_is_the_only_success() {
    assert!(render_outcome(&success_outcome()).is_ok());
    for status in [
        WallpapersOutcomeStatus::Partial,
        WallpapersOutcomeStatus::Failure,
        WallpapersOutcomeStatus::Cancelled,
        WallpapersOutcomeStatus::Uncertain,
    ] {
        let mut outcome = success_outcome();
        outcome.status = status;
        outcome.cause = Some(WallpapersOutcomeCause::Cancelled);
        assert!(render_outcome(&outcome).is_err(), "{status:?}");
    }
}

#[test]
fn the_pre_native_adapters_are_frozen() {
    let refused = |cause| WallpapersSetOutcome::refused(WallpapersOutcomeStatus::Failure, cause);
    assert_eq!(
        body(&refused(WallpapersOutcomeCause::DifferentConfig {
            host: "/a b/host".to_owned(),
            requested: "/q\"uote/\u{1b}[31mred\n".to_owned(),
        })),
        "Running host uses config directory \"/a b/host\"; requested \"/q\\\"uote/\\u001b[31mred\\n\". Use the same config directory or stop that host."
    );
    assert_eq!(
        body(&refused(WallpapersOutcomeCause::NoAcceptedConfig)),
        "No accepted Wallpapers configuration is available. Fix wallpapers.toml and reload, then retry."
    );
    assert_eq!(
        body(&WallpapersSetOutcome::cancelled(
            WallpapersOutcomeCause::QueueFull
        )),
        "Wallpaper queue is full; no native change was issued. Retry after current operations finish."
    );
    assert_eq!(
        body(&WallpapersSetOutcome::cancelled(
            WallpapersOutcomeCause::DeadlineExpired
        )),
        "Wallpaper deadline expired before native application; no native change was issued. Retry when ready."
    );
    assert_eq!(
        body(&WallpapersSetOutcome::cancelled(
            WallpapersOutcomeCause::Cancelled
        )),
        "Wallpaper operation cancelled before native application; no native change was issued."
    );
    for (cause, tag, detail) in [
        (
            WallpapersOutcomeCause::InvalidSelection { message: "m".to_owned() },
            "invalid-selection",
            "m",
        ),
        (
            WallpapersOutcomeCause::Source {
                message: "pool \"x\"".to_owned(),
            },
            "source",
            "pool \\\"x\\\"",
        ),
        (
            WallpapersOutcomeCause::Processing { message: "p".to_owned() },
            "processing",
            "p",
        ),
        (
            WallpapersOutcomeCause::Native { message: "n".to_owned() },
            "native",
            "n",
        ),
        (
            WallpapersOutcomeCause::Unavailable { message: "u\u{1b}".to_owned() },
            "unavailable",
            "u\\u001b",
        ),
    ] {
        assert_eq!(
            body(&refused(cause)),
            format!(
                "Wallpaper operation failed before native application ({tag}): \"{detail}\". No native change was issued."
            )
        );
    }
}

#[test]
fn the_response_uncertainty_body_is_frozen() {
    assert_eq!(
        response_uncertainty_body(),
        "Wallpaper outcome is unknown after a send or response failure. Check the desktop before retrying; no automatic retry was attempted."
    );
}

#[test]
fn the_application_adapter_lists_known_identities_in_target_order() {
    let failed = |reason| WallpapersTargetResult::Failed {
        reason,
        message: "m".to_owned(),
    };
    let unknown = WallpapersTargetResult::Unknown {
        reason: WallpapersTargetFailure::Unknown,
        message: "u".to_owned(),
    };
    let tail = "Check the desktop before retrying; no automatic retry was attempted.";

    let partial = outcome_for(
        &[
            (7, WallpapersTargetResult::Applied),
            (3, failed(WallpapersTargetFailure::Native)),
            (9, WallpapersTargetResult::NotAttempted),
            (5, WallpapersTargetResult::Applied),
        ],
        WallpapersOutcomeStatus::Partial,
        Some(WallpapersOutcomeCause::Native { message: "x".to_owned() }),
    );
    assert_eq!(
        body(&partial),
        format!(
            "Wallpaper application partial: applied=[7,5]; failed=[3]; not-attempted=[9]; unknown=[]. {tail}"
        )
    );

    let uncertain = outcome_for(
        &[
            (2, WallpapersTargetResult::Applied),
            (1, unknown),
            (4, WallpapersTargetResult::NotAttempted),
        ],
        WallpapersOutcomeStatus::Uncertain,
        Some(WallpapersOutcomeCause::Native { message: "x".to_owned() }),
    );
    assert_eq!(
        body(&uncertain),
        format!(
            "Wallpaper application uncertain: applied=[2]; failed=[]; not-attempted=[4]; unknown=[1]. {tail}"
        )
    );

    let all_failed = outcome_for(
        &[
            (1, failed(WallpapersTargetFailure::Native)),
            (2, WallpapersTargetResult::NotAttempted),
        ],
        WallpapersOutcomeStatus::Failure,
        Some(WallpapersOutcomeCause::Native { message: "x".to_owned() }),
    );
    assert_eq!(
        body(&all_failed),
        format!(
            "Wallpaper application failure: applied=[]; failed=[1]; not-attempted=[2]; unknown=[]. {tail}"
        )
    );

    // Cancelled after one display was issued reports the real identities.
    let cancelled = outcome_for(
        &[
            (1, WallpapersTargetResult::Applied),
            (2, WallpapersTargetResult::NotAttempted),
        ],
        WallpapersOutcomeStatus::Cancelled,
        Some(WallpapersOutcomeCause::Cancelled),
    );
    assert_eq!(
        body(&cancelled),
        format!(
            "Wallpaper application cancelled: applied=[1]; failed=[]; not-attempted=[2]; unknown=[]. {tail}"
        )
    );
}

#[test]
fn cancelled_with_only_not_attempted_targets_is_a_pre_native_refusal() {
    let outcome = outcome_for(
        &[(1, WallpapersTargetResult::NotAttempted)],
        WallpapersOutcomeStatus::Cancelled,
        Some(WallpapersOutcomeCause::DeadlineExpired),
    );
    assert_eq!(
        body(&outcome),
        "Wallpaper deadline expired before native application; no native change was issued. Retry when ready."
    );
}

#[test]
fn an_uncertain_outcome_without_identities_uses_the_uncertainty_body() {
    let outcome = WallpapersSetOutcome::refused(
        WallpapersOutcomeStatus::Uncertain,
        WallpapersOutcomeCause::DeadlineExpired,
    );
    assert_eq!(body(&outcome), response_uncertainty_body());
}

#[test]
fn escaped_control_characters_never_reach_stderr_raw() {
    let rig = Rig::new(FakeTransport::replying(ok_response(
        &WallpapersSetOutcome::refused(
            WallpapersOutcomeStatus::Failure,
            WallpapersOutcomeCause::DifferentConfig {
                host: "/h\u{1b}[2J".to_owned(),
                requested: "/r\u{7}".to_owned(),
            },
        ),
    )));
    let run = rig.run(&["set", "--random"], true);
    assert!(
        !run.err.contains('\u{1b}') && !run.err.contains('\u{7}'),
        "{:?}",
        run.err
    );
    assert!(run.err.contains("\\u001b[2J") && run.err.contains("\\u0007"));
    assert!(run.err.ends_with(".\n"));
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

#[test]
fn list_prints_a_pretty_json_array_and_styles_only_a_terminal() {
    let rig = Rig::new(FakeTransport::absent());
    rig.png(&rig.config().join("pics/a.png"));
    rig.png(&rig.config().join("pics/b.JPG"));
    rig.write_config("path = \"pics\"\nenabled = false\n");
    let plain = rig.run(&["list"], false);
    assert_eq!(plain.code, ExitCode::SUCCESS);
    assert_eq!(plain.err, "");
    let expected = serde_json::to_string_pretty(&[
        rig.config().join("pics/a.png").display().to_string(),
        rig.config().join("pics/b.JPG").display().to_string(),
    ])
    .unwrap();
    assert_eq!(plain.out, format!("{expected}\n"));
    let styled = rig.run(&["list"], true);
    assert_eq!(styled.out, format!("<<{expected}>>\n"));
    assert_eq!(strip_ansi(&plain.out), plain.out);
    assert_eq!(rig.transport.sends(), 0, "list never uses the socket");
    assert_eq!(rig.processor_calls(), 0, "metadata only");
    assert_eq!(
        rig.desktop.guarded_snapshot_reads() + rig.desktop.snapshot_reads(),
        0
    );
    assert!(rig.desktop.calls().is_empty());
}

#[test]
fn list_reports_a_valid_empty_pool_but_fails_for_bad_config_or_io() {
    let rig = Rig::new(FakeTransport::absent());
    let missing = rig.run(&["list"], false);
    assert_eq!(
        (missing.code, missing.out.as_str()),
        (ExitCode::SUCCESS, "No wallpapers found.\n")
    );
    rig.write_config("list = [\"gone.png\"]\n");
    let empty = rig.run(&["list"], true);
    assert_eq!(
        (empty.code, empty.out.as_str()),
        (ExitCode::SUCCESS, "No wallpapers found.\n")
    );

    rig.write_config("path = \"missing-dir\"\n");
    let io_failure = rig.run(&["list"], false);
    assert_eq!(io_failure.code, ExitCode::FAILURE);
    assert_eq!(io_failure.out, "");
    assert!(
        io_failure.err.starts_with("stache: Wallpaper error: wallpaper directory "),
        "{}",
        io_failure.err
    );

    rig.write_config("nope =");
    let malformed = rig.run(&["list"], false);
    assert_eq!(malformed.code, ExitCode::FAILURE);
    assert_eq!(malformed.out, "");
    assert!(
        malformed.err.starts_with("stache: Wallpaper error: "),
        "{}",
        malformed.err
    );
}

// ---------------------------------------------------------------------------
// generate-all
// ---------------------------------------------------------------------------

fn pool_rig(names: &[&str]) -> Rig {
    let mut rig = Rig::new(FakeTransport::absent());
    rig.clock = Some(Arc::new(StepClock {
        base: Instant::now(),
        reads: AtomicUsize::new(0),
        cancel_on_first: None,
    }));
    for name in names {
        rig.png(&rig.config().join(name));
    }
    let list = names.iter().map(|n| format!("\"{n}\"")).collect::<Vec<_>>().join(", ");
    rig.write_config(&format!("list = [{list}]\nenabled = false\n"));
    rig
}

#[test]
fn an_empty_generate_all_prints_exactly_and_processes_nothing() {
    for ansi in [false, true] {
        let rig = Rig::new(FakeTransport::absent());
        rig.write_config("list = []\n");
        let run = rig.run(&["generate-all"], ansi);
        assert_eq!(
            (run.code, run.out.as_str(), run.err.as_str()),
            (ExitCode::SUCCESS, "No wallpapers to generate.\n", "")
        );
        assert_eq!(rig.processor_calls(), 0);
        assert_eq!(rig.desktop.guarded_snapshot_reads(), 0);
        assert!(rig.desktop.calls().is_empty());
        assert_eq!(rig.transport.sends(), 0);
    }
}

#[test]
fn generate_all_fails_for_bad_config_or_directory_io() {
    let rig = Rig::new(FakeTransport::absent());
    rig.write_config("nope =");
    let run = rig.run(&["generate-all"], false);
    assert_eq!((run.code, run.out.as_str()), (ExitCode::FAILURE, ""));
    assert!(run.err.starts_with("stache: Wallpaper error: "));
    rig.write_config("path = \"missing\"\n");
    let run = rig.run(&["generate-all"], false);
    assert_eq!((run.code, run.out.as_str()), (ExitCode::FAILURE, ""));
    assert!(run.err.contains("wallpaper directory"));
}

fn hex_name(line: &str) -> bool { line.trim_end_matches(".png").len() == 64 }

#[test]
fn generate_all_reports_generated_then_cached_outputs_and_never_sets() {
    let rig = pool_rig(&["a.png", "b.png"]);
    let first = rig.run(&["generate-all"], false);
    assert_eq!(first.code, ExitCode::SUCCESS, "{}", first.err);
    assert_eq!(first.err, "");
    assert!(!first.out.contains('\u{1b}'), "{:?}", first.out);
    let lines: Vec<&str> = first.out.lines().collect();
    assert_eq!(
        lines[0],
        "Generating wallpapers for 2 screen(s), 2 wallpaper(s) each"
    );
    assert_eq!(lines[1], "Total: 4 images to process");
    assert_eq!(lines[2], "");
    assert_eq!(lines[3], "Screen 0:");
    assert!(
        lines[4].starts_with("  ✓ a.png -> ") && hex_name(&lines[4]["  ✓ a.png -> ".len()..]),
        "{}",
        lines[4]
    );
    assert!(lines[5].starts_with("  ✓ b.png -> "));
    assert_eq!(lines[6], "");
    assert_eq!(lines[7], "Screen 1:");
    assert_eq!(lines[10], "");
    assert_eq!(lines[11], "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    assert_eq!(lines[12], "Done! Generated: 4 (2.00s)");
    assert!(first.out.ends_with('\n'));

    let second = rig.run(&["generate-all"], false);
    assert_eq!(second.code, ExitCode::SUCCESS);
    let lines: Vec<&str> = second.out.lines().collect();
    assert_eq!(lines[4], "  ● a.png (cached)");
    assert_eq!(lines[5], "  ● b.png (cached)");
    assert_eq!(lines[12], "Done! Cached: 4 (2.00s)");

    assert!(rig.desktop.calls().is_empty(), "generate-all never sets");
    assert_eq!(rig.transport.sends(), 0);
    assert_eq!(
        rig.processor_calls(),
        0,
        "the batch pipeline, not the set processor"
    );
    assert_eq!(
        rig.desktop.guarded_snapshot_reads(),
        2,
        "one native snapshot per run"
    );
}

#[test]
fn a_completed_batch_with_a_bad_image_exits_zero_and_reports_every_result() {
    let rig = pool_rig(&["a.png", "c.png"]);
    fs::write(rig.config().join("bad.png"), b"definitely not an image").expect("bad");
    rig.write_config("list = [\"a.png\", \"bad.png\", \"c.png\"]\n");
    let run = rig.run(&["generate-all"], false);
    assert_eq!(run.code, ExitCode::SUCCESS, "{}", run.err);
    assert_eq!(run.err, "");
    let out = &run.out;
    assert_eq!(
        out.matches("✗ bad.png -> ").count(),
        2,
        "one failure per screen: {out}"
    );
    assert_eq!(out.matches("✓ a.png -> ").count(), 2, "{out}");
    assert_eq!(
        out.matches("✓ c.png -> ").count(),
        2,
        "images after the failure still run: {out}"
    );
    assert!(out.contains("Done! Generated: 4 Errors: 2 (2.00s)"), "{out}");
    assert!(rig.desktop.calls().is_empty());
}

#[test]
fn a_terminal_run_hides_and_restores_the_cursor_and_strips_to_the_same_layout() {
    let rig = pool_rig(&["a.png"]);
    let tty = rig.run(&["generate-all"], true);
    assert_eq!(tty.code, ExitCode::SUCCESS, "{}", tty.err);
    assert_eq!(tty.out.matches("\x1b[?25l").count(), 1);
    assert_eq!(tty.out.matches("\x1b[?25h").count(), 1);
    assert!(tty.out.find("\x1b[?25l").unwrap() < tty.out.find("\x1b[?25h").unwrap());
    assert!(tty.out.ends_with('\n'));
    let stripped = strip_ansi(&tty.out);
    assert!(stripped.starts_with("Generating wallpapers for 2 screen(s), 1 wallpaper(s) each\n"));
    assert!(stripped.contains("Screen 0:\n  ✓ a.png -> "));
    assert!(stripped.contains("Done! Generated: 2 (2.00s)\n"));
}

#[test]
fn a_cancelled_generation_exits_nonzero_and_restores_the_terminal() {
    for ansi in [false, true] {
        let mut rig = pool_rig(&["a.png", "b.png"]);
        rig.clock = Some(Arc::new(StepClock {
            base: Instant::now(),
            reads: AtomicUsize::new(0),
            cancel_on_first: Some(rig.token.clone()),
        }));
        let run = rig.run(&["generate-all"], ansi);
        assert_eq!(run.code, ExitCode::FAILURE);
        assert_eq!(
            run.err,
            "stache: Wallpaper error: wallpaper preparation was cancelled\n"
        );
        assert!(!run.out.contains("Done!"), "{}", run.out);
        if ansi {
            assert!(run.out.ends_with("\x1b[2K\r\x1b[?25h"), "{:?}", run.out);
        } else {
            assert!(!run.out.contains('\u{1b}'));
        }
        assert!(rig.desktop.calls().is_empty());
    }
}

struct FailingWriter {
    budget: usize,
    written: Vec<u8>,
}

impl Write for FailingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written.len() + buf.len() > self.budget {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

#[test]
fn an_output_write_failure_exits_nonzero_and_still_restores_the_cursor() {
    for budget in [0, 40, 400] {
        let rig = pool_rig(&["a.png", "b.png"]);
        let mut out = FailingWriter { budget, written: Vec::new() };
        let mut err = Vec::new();
        let code = rig.run_into(&["generate-all"], true, &mut out, &mut err);
        assert_eq!(code, ExitCode::FAILURE, "budget {budget}");
        let err = String::from_utf8(err).unwrap();
        assert!(
            err.starts_with("stache: Wallpaper error: cannot write the generation output: "),
            "{err}"
        );
        assert!(rig.desktop.calls().is_empty());
    }
}

#[test]
fn a_native_enumeration_failure_is_a_top_level_failure() {
    let rig = pool_rig(&["a.png"]);
    rig.desktop.set_snapshot(DisplaySnapshot::new(Vec::new()));
    let run = rig.run(&["generate-all"], true);
    assert_eq!((run.code, run.out.as_str()), (ExitCode::FAILURE, ""));
    assert_eq!(run.err, "stache: Wallpaper error: no displays are available\n");

    let rig = pool_rig(&["a.png"]);
    rig.token.cancel();
    let run = rig.run(&["generate-all"], false);
    assert_eq!(run.code, ExitCode::FAILURE);
    assert_eq!(run.out, "");
}

#[test]
fn list_and_generate_all_work_with_the_module_disabled() {
    let rig = pool_rig(&["a.png"]);
    rig.write_config("enabled = false\nlist = [\"a.png\"]\n");
    assert_eq!(rig.run(&["list"], false).code, ExitCode::SUCCESS);
    assert_eq!(rig.run(&["generate-all"], false).code, ExitCode::SUCCESS);
    assert_eq!(rig.transport.sends(), 0);
}

// ---------------------------------------------------------------------------
// Static and TTY goldens from fixed data
// ---------------------------------------------------------------------------

fn fixed_rows() -> (Vec<Row>, Summary) {
    let rows = vec![
        Row {
            screen: 0,
            name: "a.png".to_owned(),
            outcome: RowOutcome::Generated("aaaa.png".to_owned()),
        },
        Row {
            screen: 0,
            name: "b.png".to_owned(),
            outcome: RowOutcome::Cached,
        },
        Row {
            screen: 0,
            name: "bad.png".to_owned(),
            outcome: RowOutcome::Failed("decode failed".to_owned()),
        },
        Row {
            screen: 1,
            name: "a.png".to_owned(),
            outcome: RowOutcome::Cached,
        },
        Row {
            screen: 1,
            name: "b.png".to_owned(),
            outcome: RowOutcome::Cached,
        },
        Row {
            screen: 1,
            name: "bad.png".to_owned(),
            outcome: RowOutcome::Failed("decode failed".to_owned()),
        },
    ];
    let summary = Summary {
        screens: 2,
        generated: 1,
        cached: 3,
        errors: 2,
        elapsed: Duration::from_millis(1234),
    };
    (rows, summary)
}

#[test]
fn static_golden_matches_the_legacy_layout() {
    let (rows, summary) = fixed_rows();
    assert_eq!(
        header_text(false, 2, 3),
        "Generating wallpapers for 2 screen(s), 3 wallpaper(s) each\nTotal: 6 images to process\n\n"
    );
    assert_eq!(
        results_text(false, &rows, &summary),
        "Screen 0:\n  ✓ a.png -> aaaa.png\n  ● b.png (cached)\n  ✗ bad.png -> decode failed\n\n\
         Screen 1:\n  ● a.png (cached)\n  ● b.png (cached)\n  ✗ bad.png -> decode failed\n\n\
         ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\nDone! Generated: 1 Cached: 3 Errors: 2 (1.23s)\n"
    );
    assert_eq!(progress_text(false, 3, 2, 6), "⠸ Processing... 2/6 (33%)");
}

#[test]
fn tty_golden_uses_the_legacy_escape_sequences_and_strips_to_the_static_text() {
    let (rows, summary) = fixed_rows();
    assert_eq!(
        header_text(true, 2, 3),
        "\x1b[1m\x1b[36mGenerating wallpapers\x1b[0m for 2 screen(s), 3 wallpaper(s) each\n\
         \x1b[2mTotal: 6 images to process\x1b[0m\n\n\x1b[?25l"
    );
    assert_eq!(
        progress_text(true, 0, 1, 6),
        "\x1b[2K\r\x1b[36m⠋ Processing... 1/6 (16%)\x1b[0m"
    );
    let tty = results_text(true, &rows, &summary);
    assert!(tty.starts_with("\x1b[2K\r\x1b[?25h\x1b[1mScreen 0:\x1b[0m\n  \x1b[32m✓\x1b[0m a.png -> aaaa.png\n  \x1b[2m●\x1b[0m b.png \x1b[2m(cached)\x1b[0m\n  \x1b[31m✗\x1b[0m bad.png -> decode failed\n"));
    assert!(tty.ends_with(
        "\x1b[2m━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m\n\x1b[1mDone!\x1b[0m \x1b[32mGenerated: 1\x1b[0m \x1b[33mCached: 3\x1b[0m \x1b[31mErrors: 2\x1b[0m \x1b[2m(1.23s)\x1b[0m\n"
    ));
    assert_eq!(strip_ansi(&tty), results_text(false, &rows, &summary));
    assert_eq!(strip_ansi(&header_text(true, 2, 3)), header_text(false, 2, 3));
}

// ---------------------------------------------------------------------------
// Findings: escaping, transport pin, cancellation and worker panic
// ---------------------------------------------------------------------------

#[test]
fn production_transport_is_pinned_to_the_legacy_policy_and_client_budget() {
    use crate::runtime::{WALLPAPERS_CLIENT_BUDGET, WALLPAPERS_SERVER_BUDGET};
    assert_eq!(
        IpcTransport::production(),
        IpcTransport::with_options(
            stache_sdk::ipc::RetryPolicy::legacy(),
            WALLPAPERS_CLIENT_BUDGET
        )
    );
    assert!(WALLPAPERS_CLIENT_BUDGET > WALLPAPERS_SERVER_BUDGET);
    // `WallpapersCliEnv::new` stores the transport as a trait object, so its
    // concrete type is not observable without real I/O; it is not asserted.
}

#[test]
fn control_characters_in_a_token_and_a_config_path_never_reach_stderr_raw() {
    let rig = Rig::new(FakeTransport::absent());
    rig.write_config("list = []\n");
    let run = rig.run(&["set", "./\u{1b}[2Jx.png"], false);
    assert_eq!(run.code, ExitCode::FAILURE);
    assert!(run.err.contains("\\u001b"), "{:?}", run.err);
    assert!(!run.err.contains('\u{1b}'), "{:?}", run.err);
    assert_eq!(rig.transport.sends(), 0);

    // A config directory path containing ESC that cannot be read as a file.
    let weird = rig.root.path().join("bad\u{1b}[2Jconfig");
    fs::create_dir_all(weird.join("wallpapers.toml")).expect("directory in place of the file");
    let root = Command::new("stache")
        .arg(
            Arg::new("config")
                .short('c')
                .long("config")
                .global(true)
                .value_parser(clap::value_parser!(PathBuf)),
        )
        .subcommand(command());
    let matches = root
        .try_get_matches_from(vec![
            "stache".to_owned(),
            "-c".to_owned(),
            weird.display().to_string(),
            "wallpaper".to_owned(),
            "list".to_owned(),
        ])
        .expect("matches");
    let (_, sub) = matches.subcommand().expect("wallpaper");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = contribution(rig.env()).executor().execute(sub, false, &mut out, &mut err);
    let err = String::from_utf8(err).expect("utf-8");
    assert_eq!(code, ExitCode::FAILURE);
    assert!(err.contains("\\u001b"), "{err:?}");
    assert!(!err.contains('\u{1b}'), "{err:?}");
}

struct CancelOnStart(CancellationToken);

impl crate::batch::BatchObserver for CancelOnStart {
    fn item_started(&self) { self.0.cancel(); }
}

struct PanicOnStart;

impl crate::batch::BatchObserver for PanicOnStart {
    fn item_started(&self) {
        panic!("forced worker panic");
    }
}

#[test]
fn generate_all_cancelled_mid_batch_restores_the_cursor_and_prints_no_summary() {
    let mut rig = pool_rig(&["a.png", "b.png", "c.png"]);
    rig.observer = Some(Arc::new(CancelOnStart(rig.token.clone())));
    let run = rig.run(&["generate-all"], true);
    assert_eq!(run.code, ExitCode::FAILURE, "{run_out:?}", run_out = run.out);
    assert!(run.out.contains("\x1b[?25h"), "cursor restored: {:?}", run.out);
    assert!(!run.out.contains("Done!"), "{:?}", run.out);
    assert!(run.err.contains("cancelled"), "{}", run.err);
    assert_eq!(rig.transport.sends(), 0);
    assert!(rig.desktop.calls().is_empty(), "no setter call");
}

#[test]
fn a_panicking_worker_on_a_terminal_run_returns_with_the_cursor_restored() {
    let (sender, receiver) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut rig = pool_rig(&["a.png", "b.png"]);
        rig.observer = Some(Arc::new(PanicOnStart));
        let run = rig.run(&["generate-all"], true);
        let _ = sender.send((run.code, run.out, run.err));
    });
    let (code, out, err) = receiver
        .recv_timeout(Duration::from_secs(20))
        .expect("the call returns instead of spinning forever");
    assert_eq!(code, ExitCode::FAILURE);
    assert!(out.contains("\x1b[?25h"), "{out:?}");
    assert!(!out.contains("Done!"), "{out:?}");
    assert!(err.starts_with("stache: Wallpaper error: "), "{err:?}");
}
