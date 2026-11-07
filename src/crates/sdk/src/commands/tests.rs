//! Parser, resolver and runner tests over injected processes and fake time.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::*;

// ---------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------

type WaitHook = Box<dyn Fn(usize) + Send + Sync>;

#[derive(Default)]
struct FakeClock {
    now: Mutex<Duration>,
    waits: Mutex<Vec<Duration>>,
    hook: Mutex<Option<WaitHook>>,
}

impl FakeClock {
    fn elapsed(&self) -> Duration { *lock(&self.now) }

    fn wait_count(&self) -> usize { lock(&self.waits).len() }

    fn set_hook(&self, hook: impl Fn(usize) + Send + Sync + 'static) {
        *lock(&self.hook) = Some(Box::new(hook));
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Duration { *lock(&self.now) }

    fn wait(&self, cancel: Option<&CancellationToken>, quantum: Duration) -> bool {
        let count = {
            let mut waits = lock(&self.waits);
            waits.push(quantum);
            waits.len()
        };
        *lock(&self.now) += quantum;
        if let Some(hook) = lock(&self.hook).as_ref() {
            hook(count);
        }
        cancel.is_some_and(CancellationToken::is_cancelled)
    }
}

#[derive(Clone, Default)]
struct Script {
    /// Fake time at which `try_wait` reports an exit.
    exit_at: Option<Duration>,
    exit_code: i32,
    /// Whether `kill` makes the child exit.
    dies_on_kill: bool,
    kill_fails: bool,
    wait_fails: bool,
    /// Cancels this token when the handle is released (the step finished).
    cancel_on_drop: Option<CancellationToken>,
    stdout: Vec<(Duration, Vec<u8>)>,
    stderr: Vec<(Duration, Vec<u8>)>,
}

struct FakeChild {
    script: Script,
    clock: Arc<FakeClock>,
    killed: bool,
    events: Arc<Mutex<Vec<String>>>,
    stdout: VecDeque<(Duration, Vec<u8>)>,
    stderr: VecDeque<(Duration, Vec<u8>)>,
}

impl Drop for FakeChild {
    fn drop(&mut self) {
        if let Some(token) = &self.script.cancel_on_drop {
            token.cancel();
        }
    }
}

impl FakeChild {
    fn exited(&self) -> bool {
        (self.killed && self.script.dies_on_kill)
            || self.script.exit_at.is_some_and(|at| self.clock.now() >= at)
    }

    fn read(
        queue: &mut VecDeque<(Duration, Vec<u8>)>,
        now: Duration,
        exited: bool,
        buffer: &mut [u8],
    ) -> io::Result<usize> {
        match queue.front_mut() {
            Some((at, data)) if *at <= now => {
                let count = data.len().min(buffer.len());
                buffer[..count].copy_from_slice(&data[..count]);
                data.drain(..count);
                if data.is_empty() {
                    queue.pop_front();
                }
                Ok(count)
            }
            None if exited => Ok(0),
            Some(_) | None => Err(io::ErrorKind::WouldBlock.into()),
        }
    }
}

impl ChildProcess for FakeChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.script.wait_fails {
            return Err(io::Error::other("wait broke"));
        }
        if self.exited() {
            let code = if self.killed {
                9
            } else {
                self.script.exit_code << 8
            };
            Ok(Some(ExitStatus::from_raw(code)))
        } else {
            Ok(None)
        }
    }

    fn kill(&mut self) -> io::Result<()> {
        lock(&self.events).push("kill".to_owned());
        self.killed = true;
        if self.script.kill_fails {
            Err(io::Error::other("kill refused"))
        } else {
            Ok(())
        }
    }

    fn read_stdout(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let (now, exited) = (self.clock.now(), self.exited());
        Self::read(&mut self.stdout, now, exited, buffer)
    }

    fn read_stderr(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let (now, exited) = (self.clock.now(), self.exited());
        Self::read(&mut self.stderr, now, exited, buffer)
    }
}

struct FakeFactory {
    clock: Arc<FakeClock>,
    scripts: Mutex<VecDeque<Script>>,
    default: Script,
    specs: Mutex<Vec<SpawnSpec>>,
    events: Arc<Mutex<Vec<String>>>,
    spawn_error: AtomicBool,
}

impl FakeFactory {
    fn new(clock: &Arc<FakeClock>, default: Script) -> Arc<Self> {
        Arc::new(Self {
            clock: Arc::clone(clock),
            scripts: Mutex::new(VecDeque::new()),
            default,
            specs: Mutex::new(Vec::new()),
            events: Arc::new(Mutex::new(Vec::new())),
            spawn_error: AtomicBool::new(false),
        })
    }

    fn push(&self, script: Script) { lock(&self.scripts).push_back(script); }

    fn spawned(&self) -> Vec<SpawnSpec> { lock(&self.specs).clone() }

    fn kills(&self) -> usize { lock(&self.events).iter().filter(|event| *event == "kill").count() }
}

impl ProcessFactory for FakeFactory {
    fn spawn(&self, spec: &SpawnSpec) -> io::Result<Box<dyn ChildProcess>> {
        if self.spawn_error.load(Ordering::SeqCst) {
            return Err(io::Error::other("spawn refused"));
        }
        lock(&self.specs).push(spec.clone());
        let script = lock(&self.scripts).pop_front().unwrap_or_else(|| self.default.clone());
        let base = self.clock.now();
        let shift = |chunks: &[(Duration, Vec<u8>)]| {
            chunks.iter().map(|(at, data)| (base + *at, data.clone())).collect()
        };
        Ok(Box::new(FakeChild {
            clock: Arc::clone(&self.clock),
            killed: false,
            events: Arc::clone(&self.events),
            stdout: shift(&script.stdout),
            stderr: shift(&script.stderr),
            script: Script {
                exit_at: script.exit_at.map(|at| base + at),
                ..script
            },
        }))
    }
}

fn exits_after(milliseconds: u64) -> Script {
    Script {
        exit_at: Some(Duration::from_millis(milliseconds)),
        dies_on_kill: true,
        ..Script::default()
    }
}

fn immortal() -> Script {
    Script {
        dies_on_kill: false,
        ..Script::default()
    }
}

struct Rig {
    clock: Arc<FakeClock>,
    factory: Arc<FakeFactory>,
    executor: CommandExecutor,
    bin: tempfile::TempDir,
}

fn executable(directory: &Path, name: &str) -> PathBuf {
    let path = directory.join(name);
    std::fs::write(&path, "#!/bin/sh\n").expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

fn rig(default: Script) -> Rig {
    let clock = Arc::new(FakeClock::default());
    let factory = FakeFactory::new(&clock, default);
    let bin = tempfile::tempdir().expect("bin");
    executable(bin.path(), "tool");
    let next = executable(bin.path(), "next-stache");
    let environment = CommandEnvironment {
        path: Some(bin.path().as_os_str().to_owned()),
        next_executable: Some(next),
        ..CommandEnvironment::default()
    };
    let executor = CommandExecutor::with_parts(
        Arc::clone(&factory) as Arc<dyn ProcessFactory>,
        Arc::clone(&clock) as Arc<dyn Clock>,
        environment,
    );
    Rig { clock, factory, executor, bin }
}

fn commands(list: &[&str]) -> Vec<String> { list.iter().map(|c| (*c).to_owned()).collect() }

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

#[test]
fn quoted_arguments_keep_spaces_and_escapes() {
    assert_eq!(
        split_command(r#"open -a "My App" 'two words'"#).expect("split"),
        vec!["open", "-a", "My App", "two words"]
    );
    assert_eq!(split_command(r#"say "a \"quote\"""#).expect("split"), vec![
        "say",
        "a \"quote\""
    ]);
    assert_eq!(split_command("   ").expect("empty"), Vec::<String>::new());
}

#[test]
fn malformed_quotes_are_rejected() {
    assert_eq!(
        split_command("\"open"),
        Err(CommandParseError::UnterminatedQuote('"'))
    );
    assert_eq!(
        split_command("open 'x"),
        Err(CommandParseError::UnterminatedQuote('\''))
    );
    assert_eq!(split_command("open \\"), Err(CommandParseError::TrailingEscape));
    assert_eq!(
        CommandParseError::TrailingEscape.to_string(),
        "trailing escape character"
    );
    assert_eq!(
        CommandParseError::UnterminatedQuote('"').to_string(),
        "unterminated \" quote"
    );
}

#[test]
fn quoting_vectors_match_the_archived_rules() {
    let table: &[(&str, &[&str])] = &[
        ("", &[]),
        ("echo", &["echo"]),
        (r#"a "b 'c' d""#, &["a", "b 'c' d"]),
        (r#"a 'b "c" d'"#, &["a", "b \"c\" d"]),
        (r"a\ b c", &["a b", "c"]),
        (r"'a\b'", &["a\\b"]),
        (r#""a\\b""#, &["a\\b"]),
        (r#""a\nb""#, &["anb"]),
        ("'' x", &["", "x"]),
        (r#"x "" y"#, &["x", "", "y"]),
        (r#"""  tool"#, &["", "tool"]),
        ("a\tb\nc", &["a", "b", "c"]),
        ("a'b'c", &["abc"]),
        // Shell metacharacters are literal arguments; nothing is expanded.
        ("echo | grep > out $HOME ~ * && ;", &[
            "echo", "|", "grep", ">", "out", "$HOME", "~", "*", "&&", ";",
        ]),
        ("echo `x` $(y)", &["echo", "`x`", "$(y)"]),
    ];
    for (input, expected) in table {
        assert_eq!(
            split_command(input).unwrap_or_else(|error| panic!("{input:?}: {error}")),
            *expected,
            "{input:?}"
        );
    }
}

#[test]
fn an_empty_invocation_differs_from_an_empty_quoted_argument() {
    assert_eq!(split_command("").expect("empty"), Vec::<String>::new());
    assert_eq!(split_command("''").expect("empty quoted"), vec![String::new()]);
    let environment = CommandEnvironment::default();
    assert!(matches!(prepare("   ", &environment), Err(StepFailure::Empty)));
    match prepare("''", &environment) {
        Err(StepFailure::Resolve(reason)) => assert_eq!(reason, "binary name cannot be empty"),
        other => panic!("a quoted empty binary is a resolution error: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

#[test]
fn absolute_paths_resolve_directly_and_are_never_searched() {
    let environment = CommandEnvironment::default();
    assert!(resolve_binary("/usr/bin/true", &environment).is_ok());
    assert_eq!(
        resolve_binary("/usr/bin/definitely-missing", &environment),
        Err("/usr/bin/definitely-missing is not executable".to_owned())
    );
    assert_eq!(
        resolve_binary("", &environment),
        Err("binary name cannot be empty".to_owned())
    );
}

#[test]
fn execute_bits_decide_whether_a_candidate_matches() {
    let root = tempfile::tempdir().expect("root");
    let plain = root.path().join("plain");
    std::fs::write(&plain, "x").expect("write");
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    std::fs::create_dir(root.path().join("dir")).expect("dir");
    let environment = CommandEnvironment {
        path: Some(root.path().as_os_str().to_owned()),
        ..CommandEnvironment::default()
    };
    for name in ["plain", "dir"] {
        assert_eq!(
            resolve_binary(name, &environment),
            Err(format!("`{name}` was not found on PATH"))
        );
    }
    assert_eq!(
        resolve_binary("plain", &CommandEnvironment::default()),
        Err("`plain` was not found on PATH".to_owned())
    );
    assert!(resolve_binary(plain.to_str().expect("utf-8"), &environment).is_err());
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    assert_eq!(resolve_binary("plain", &environment), Ok(plain));
}

#[test]
fn search_order_is_extra_paths_path_system_directories_then_home() {
    let environment = CommandEnvironment {
        extra_paths: Some(OsString::from("/extra/one:/extra/two")),
        path: Some(OsString::from("/path/one:/path/two")),
        home: Some(PathBuf::from("/home/me")),
        next_executable: None,
    };
    let expected: Vec<PathBuf> = [
        "/extra/one",
        "/extra/two",
        "/path/one",
        "/path/two",
        "/usr/local/bin",
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
        "/home/me/.cargo/bin",
        "/home/me/.local/bin",
    ]
    .map(PathBuf::from)
    .to_vec();
    assert_eq!(environment.search_directories(), expected);
}

#[test]
fn a_missing_home_omits_only_the_home_directories() {
    let environment = CommandEnvironment {
        path: Some(OsString::from("/path/one")),
        ..CommandEnvironment::default()
    };
    assert_eq!(
        environment.search_directories(),
        [
            "/path/one",
            "/usr/local/bin",
            "/opt/homebrew/bin",
            "/opt/homebrew/sbin"
        ]
        .map(PathBuf::from)
    );
    assert_eq!(CommandEnvironment::default().search_directories().len(), 3);
}

#[test]
fn extra_paths_win_over_path_and_the_first_executable_match_wins() {
    let extra = tempfile::tempdir().expect("extra");
    let path = tempfile::tempdir().expect("path");
    let in_extra = executable(extra.path(), "dual");
    executable(path.path(), "dual");
    executable(path.path(), "only-path");
    let environment = CommandEnvironment {
        extra_paths: Some(extra.path().as_os_str().to_owned()),
        path: Some(path.path().as_os_str().to_owned()),
        ..CommandEnvironment::default()
    };
    assert_eq!(resolve_binary("dual", &environment), Ok(in_extra));
    assert_eq!(
        resolve_binary("only-path", &environment),
        Ok(path.path().join("only-path"))
    );
}

#[test]
fn home_install_directories_are_searched_last() {
    let home = tempfile::tempdir().expect("home");
    std::fs::create_dir_all(home.path().join(".cargo/bin")).expect("cargo");
    std::fs::create_dir_all(home.path().join(".local/bin")).expect("local");
    let local = executable(&home.path().join(".local/bin"), "homed");
    let cargo = executable(&home.path().join(".cargo/bin"), "homed");
    let environment = CommandEnvironment {
        home: Some(home.path().to_path_buf()),
        ..CommandEnvironment::default()
    };
    assert_eq!(resolve_binary("homed", &environment), Ok(cargo));
    std::fs::remove_file(home.path().join(".cargo/bin/homed")).expect("remove");
    assert_eq!(resolve_binary("homed", &environment), Ok(local));
}

#[test]
fn empty_path_components_keep_their_current_cwd_relative_meaning() {
    // The extracted resolver preserves `split_paths` output: an empty component
    // is the empty path, and joining a name onto it is the bare relative name.
    let environment = CommandEnvironment {
        path: Some(OsString::from(":/path/one")),
        ..CommandEnvironment::default()
    };
    let directories = environment.search_directories();
    assert_eq!(directories[0], PathBuf::new());
    assert_eq!(directories[0].join("tool"), PathBuf::from("tool"));
    assert_eq!(directories[1], PathBuf::from("/path/one"));
}

#[test]
fn bare_stache_resolves_only_to_the_injected_running_executable() {
    let bin = tempfile::tempdir().expect("bin");
    let legacy = executable(bin.path(), "stache");
    let next_dir = tempfile::tempdir().expect("next");
    let next = executable(next_dir.path(), "stache-next-binary");
    let environment = CommandEnvironment {
        extra_paths: Some(bin.path().as_os_str().to_owned()),
        path: Some(bin.path().as_os_str().to_owned()),
        next_executable: Some(next.clone()),
        ..CommandEnvironment::default()
    };
    assert_eq!(resolve_binary("stache", &environment), Ok(next));
    assert_ne!(resolve_binary("stache", &environment), Ok(legacy));

    let unavailable = CommandEnvironment {
        next_executable: None,
        ..environment.clone()
    };
    assert_eq!(
        resolve_binary("stache", &unavailable),
        Err("the running Stache executable is unavailable".to_owned()),
        "a competing PATH stache is never a fallback"
    );
    let stale = CommandEnvironment {
        next_executable: Some(next_dir.path().join("gone")),
        ..environment
    };
    assert!(
        resolve_binary("stache", &stale)
            .expect_err("missing")
            .ends_with("is not executable")
    );
}

#[test]
fn only_the_exact_name_stache_is_rewritten_to_the_internal_cli() {
    let rig = rig(exits_after(0));
    let environment = rig.executor.environment();
    let next = environment.next_executable.clone().expect("next");

    let rewritten =
        prepare(r#"stache reload "" 'x y' --stache-internal-cli"#, environment).expect("prepared");
    assert_eq!(rewritten.program, next);
    assert_eq!(rewritten.arguments, [
        INTERNAL_CLI_FLAG,
        "reload",
        "",
        "x y",
        "--stache-internal-cli"
    ]);
    assert_eq!(prepare("stache", environment).expect("bare").arguments, [
        INTERNAL_CLI_FLAG
    ]);
    assert_eq!(
        prepare("stache -- --stache-internal-cli", environment)
            .expect("literal")
            .arguments,
        [INTERNAL_CLI_FLAG, "--", "--stache-internal-cli"],
        "marker text in arguments is neither scanned nor rejected"
    );

    let absolute = format!("{} reload", next.display());
    let prepared = prepare(&absolute, environment).expect("absolute");
    assert_eq!(
        prepared.arguments,
        ["reload"],
        "absolute paths are not rewritten"
    );
    let tool = prepare("tool stache", environment).expect("tool");
    assert_eq!(tool.arguments, ["stache"]);
    let quoted = prepare(r#""stache" a"#, environment).expect("quoted name is the same name");
    assert_eq!(quoted.arguments, [INTERNAL_CLI_FLAG, "a"]);
    assert!(prepare("Stache a", environment).is_err());
    assert!(prepare("./stache a", environment).is_err());
}

#[test]
fn step_failures_render_the_startup_reasons() {
    assert_eq!(StepFailure::Empty.to_string(), "command is empty");
    assert_eq!(
        StepFailure::Spawn("denied".to_owned()).to_string(),
        "could not run: denied"
    );
    assert_eq!(
        StepFailure::Wait("broken".to_owned()).to_string(),
        "could not be waited for: broken"
    );
    assert_eq!(
        StepFailure::Exit(ExitStatus::from_raw(1 << 8)).to_string(),
        "exited with exit status: 1"
    );
}

// ---------------------------------------------------------------------------
// Inherited sequential mode
// ---------------------------------------------------------------------------

#[test]
fn a_sequence_runs_in_order_with_inherited_stdio_and_polls_at_the_quantum() {
    let rig = rig(exits_after(35));
    let cancel = CancellationToken::new();
    let outcome = rig.executor.run_sequence(
        &commands(&["tool one", r#"tool "two words" ''"#]),
        &cancel,
        &cancel,
    );
    assert!(matches!(outcome, SequenceOutcome::Completed), "{outcome:?}");
    let spawned = rig.factory.spawned();
    assert_eq!(spawned.len(), 2);
    assert!(spawned.iter().all(|spec| spec.stdio == StdioMode::Inherit));
    assert_eq!(spawned[0].arguments, ["one"]);
    assert_eq!(spawned[1].arguments, ["two words", ""]);
    assert_eq!(spawned[0].program, rig.bin.path().join("tool"));
    let waits = lock(&rig.clock.waits).clone();
    assert!(!waits.is_empty());
    assert!(
        waits
            .iter()
            .all(|wait| *wait == POLL_QUANTUM && *wait <= Duration::from_millis(10))
    );
    assert_eq!(
        rig.clock.elapsed(),
        POLL_QUANTUM * 8,
        "two steps of 35 ms poll four times each"
    );
}

#[test]
fn the_first_failure_stops_the_sequence_with_its_step_and_status() {
    let rig = rig(exits_after(0));
    rig.factory.push(exits_after(0));
    rig.factory.push(Script { exit_code: 3, ..exits_after(0) });
    let cancel = CancellationToken::new();
    let outcome = rig
        .executor
        .run_sequence(&commands(&["tool", "tool", "tool"]), &cancel, &cancel);
    let SequenceOutcome::Failed { index, total, failure, cleanup } = outcome else {
        panic!("expected a failure: {outcome:?}");
    };
    assert_eq!((index, total), (1, 3));
    assert!(cleanup.is_none());
    assert!(matches!(failure, StepFailure::Exit(status) if status.code() == Some(3)));
    assert_eq!(rig.factory.spawned().len(), 2, "step three never runs");
}

#[test]
fn parse_empty_and_resolution_failures_abort_before_any_spawn() {
    for (list, expected) in [
        (&["\"open"][..], "unterminated \" quote"),
        (&["   "][..], "command is empty"),
        (
            &["definitely-missing-stache-binary"][..],
            "`definitely-missing-stache-binary` was not found on PATH",
        ),
    ] {
        let rig = rig(exits_after(0));
        let cancel = CancellationToken::new();
        let outcome = rig.executor.run_sequence(&commands(list), &cancel, &cancel);
        let SequenceOutcome::Failed { index, failure, .. } = outcome else {
            panic!("{list:?}: {outcome:?}");
        };
        assert_eq!(index, 0);
        assert_eq!(failure.to_string(), expected);
        assert!(rig.factory.spawned().is_empty());
    }
}

#[test]
fn spawn_and_wait_failures_are_typed_and_stop_later_steps() {
    let rig = rig(exits_after(0));
    rig.factory.spawn_error.store(true, Ordering::SeqCst);
    let cancel = CancellationToken::new();
    let outcome = rig.executor.run_sequence(&commands(&["tool", "tool"]), &cancel, &cancel);
    let SequenceOutcome::Failed { failure, .. } = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(failure.to_string(), "could not run: spawn refused");
    assert!(rig.factory.spawned().is_empty());
    assert_eq!(rig.executor.sweep().len(), 0);

    let rig = rig_with_wait_failure();
    let cancel = CancellationToken::new();
    let outcome = rig.executor.run_sequence(&commands(&["tool", "tool"]), &cancel, &cancel);
    let SequenceOutcome::Failed { failure, cleanup, .. } = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(failure.to_string(), "could not be waited for: wait broke");
    assert!(
        matches!(cleanup, Some(Cleanup::Retained(_))),
        "an unwaitable child is retained"
    );
    assert_eq!(rig.factory.spawned().len(), 1);
    assert_eq!(rig.factory.kills(), 1);
}

fn rig_with_wait_failure() -> Rig { rig(Script { wait_fails: true, ..immortal() }) }

#[test]
fn cancellation_before_resolution_spawns_nothing() {
    let rig = rig(exits_after(0));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let outcome =
        rig.executor
            .run_sequence(&commands(&["tool", "definitely-missing"]), &cancel, &cancel);
    assert!(
        matches!(outcome, SequenceOutcome::Cancelled(None)),
        "{outcome:?}"
    );
    assert!(rig.factory.spawned().is_empty());
}

struct StaleOnAdmit {
    cancel: CancellationToken,
    admitted: AtomicUsize,
}

impl SpawnGate for StaleOnAdmit {
    fn admit(&self) -> Option<Box<dyn Send + '_>> {
        self.admitted.fetch_add(1, Ordering::SeqCst);
        // The configuration moved on between resolution and process creation.
        self.cancel.cancel();
        None
    }
}

#[test]
fn a_gate_that_goes_stale_between_resolution_and_spawn_blocks_the_child() {
    let rig = rig(exits_after(0));
    let cancel = CancellationToken::new();
    let gate = StaleOnAdmit {
        cancel: cancel.clone(),
        admitted: AtomicUsize::new(0),
    };
    let outcome = rig.executor.run_sequence(&commands(&["tool", "tool"]), &cancel, &gate);
    assert!(
        matches!(outcome, SequenceOutcome::Cancelled(None)),
        "{outcome:?}"
    );
    assert_eq!(gate.admitted.load(Ordering::SeqCst), 1);
    assert!(rig.factory.spawned().is_empty(), "no stale step is issued");
    assert_eq!(rig.executor.sweep().len(), 0);
    // The slot reserved before the gate was released.
    assert_eq!(lock(&rig.executor.state).used, 0);
}

#[test]
fn cancellation_during_the_wait_kills_only_the_direct_child_and_stops_the_sequence() {
    let rig = rig(Script {
        dies_on_kill: true,
        ..Script::default()
    });
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    rig.clock.set_hook(move |count| {
        if count == 3 {
            trigger.cancel();
        }
    });
    let outcome = rig.executor.run_sequence(&commands(&["tool", "tool"]), &cancel, &cancel);
    assert!(
        matches!(outcome, SequenceOutcome::Cancelled(Some(Cleanup::Reaped))),
        "{outcome:?}"
    );
    assert_eq!(rig.factory.spawned().len(), 1, "the second step never starts");
    assert_eq!(rig.factory.kills(), 1);
    assert_eq!(
        lock(&rig.executor.state).used,
        0,
        "a confirmed reap releases the slot"
    );
}

#[test]
fn cancellation_between_steps_stops_before_the_next_step() {
    let rig = rig(exits_after(10));
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    // The first step's wait ends at its exit poll; cancel on the final wait.
    rig.clock.set_hook(move |count| {
        if count == 1 {
            trigger.cancel();
        }
    });
    let outcome = rig.executor.run_sequence(&commands(&["tool", "tool"]), &cancel, &cancel);
    // Cancelling at the poll that precedes exit is observed at the loop top.
    assert!(
        matches!(outcome, SequenceOutcome::Cancelled(Some(_))),
        "{outcome:?}"
    );
    assert_eq!(rig.factory.spawned().len(), 1);

    // Cancel exactly when the first step's child is released.
    let boundary = self::rig(exits_after(0));
    let cancel = CancellationToken::new();
    boundary.factory.push(Script {
        cancel_on_drop: Some(cancel.clone()),
        ..exits_after(0)
    });
    let outcome =
        boundary
            .executor
            .run_sequence(&commands(&["tool", "tool", "tool"]), &cancel, &cancel);
    assert!(
        matches!(outcome, SequenceOutcome::Cancelled(None)),
        "{outcome:?}"
    );
    assert_eq!(
        boundary.factory.spawned().len(),
        1,
        "no later step starts after the boundary"
    );
}

#[test]
fn cleanup_polls_for_two_seconds_then_transfers_the_exact_handle_to_the_reaper() {
    let rig = rig(immortal());
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    rig.clock.set_hook(move |count| {
        if count == 1 {
            trigger.cancel();
        }
    });
    let diagnostics = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&diagnostics);
    let executor = CommandExecutor::with_parts(
        Arc::clone(&rig.factory) as Arc<dyn ProcessFactory>,
        Arc::clone(&rig.clock) as Arc<dyn Clock>,
        rig.executor.environment().clone(),
    )
    .with_diagnostics(move |line| lock(&sink).push(line.to_owned()));

    let before = rig.clock.elapsed();
    let outcome = executor.run_sequence(&commands(&["tool"]), &cancel, &cancel);
    let SequenceOutcome::Cancelled(Some(Cleanup::Retained(diagnostic))) = outcome else {
        panic!("{outcome:?}");
    };
    assert!(
        diagnostic.contains("not confirmed gone after 2000 ms"),
        "{diagnostic}"
    );
    assert_eq!(lock(&diagnostics).as_slice(), [diagnostic]);
    // One wait before cancel, then the cleanup polls until 2 s have passed.
    assert_eq!(
        rig.clock.elapsed().saturating_sub(before),
        POLL_QUANTUM + CANCEL_CLEANUP
    );
    assert!(lock(&rig.clock.waits).iter().all(|wait| *wait <= POLL_QUANTUM));
    let retained = executor.sweep();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].program, rig.bin.path().join("tool"));
    assert_eq!(
        lock(&executor.state).used,
        1,
        "the retained child keeps its slot"
    );
    assert!(
        rig.factory.kills() >= 2,
        "the sweep signals the retained child again"
    );
}

#[test]
fn a_retained_child_releases_its_slot_when_it_is_finally_reaped() {
    let rig = rig(Script { kill_fails: true, ..immortal() });
    // This child exits long after cleanup gave up on it.
    rig.factory.push(Script {
        exit_at: Some(Duration::from_secs(10)),
        kill_fails: true,
        ..immortal()
    });
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    rig.clock.set_hook(move |count| {
        if count == 1 {
            trigger.cancel();
        }
    });
    let outcome = rig.executor.run_sequence(&commands(&["tool"]), &cancel, &cancel);
    let SequenceOutcome::Cancelled(Some(Cleanup::Retained(diagnostic))) = outcome else {
        panic!("{outcome:?}");
    };
    assert!(diagnostic.contains("kill failed: kill refused"), "{diagnostic}");
    assert_eq!(rig.executor.sweep().len(), 1);
    *lock(&rig.clock.now) += Duration::from_secs(10);
    assert!(rig.executor.sweep().is_empty());
    assert_eq!(lock(&rig.executor.state).used, 0);
}

#[test]
fn exhausting_the_sixty_four_slots_blocks_new_children_without_dropping_any_handle() {
    let rig = rig(immortal());
    let current = Arc::new(Mutex::new(CancellationToken::new()));
    let trigger = Arc::clone(&current);
    rig.clock.set_hook(move |_| lock(&trigger).cancel());
    for expected in 1..=RETAINED_SLOTS {
        let token = CancellationToken::new();
        *lock(&current) = token.clone();
        let outcome = rig.executor.run_sequence(&commands(&["tool"]), &token, &token);
        assert!(matches!(
            outcome,
            SequenceOutcome::Cancelled(Some(Cleanup::Retained(_)))
        ));
        assert_eq!(rig.executor.sweep().len(), expected);
    }
    assert_eq!(rig.factory.spawned().len(), RETAINED_SLOTS);

    let token = CancellationToken::new();
    let outcome = rig.executor.run_sequence(&commands(&["tool", "tool"]), &token, &token);
    let SequenceOutcome::Failed { failure, .. } = outcome else {
        panic!("{outcome:?}")
    };
    assert!(matches!(failure, StepFailure::Capacity));
    assert_eq!(
        rig.factory.spawned().len(),
        RETAINED_SLOTS,
        "exhaustion spawns nothing"
    );
    assert_eq!(
        rig.executor.sweep().len(),
        RETAINED_SLOTS,
        "every handle is still owned"
    );

    // A captured call is blocked by the same slots.
    let limits = CaptureLimits::new(Duration::from_secs(1), 8, 8).expect("limits");
    assert!(matches!(
        rig.executor.run_captured("tool", limits, &token, &token),
        CaptureOutcome::Failed {
            failure: StepFailure::Capacity,
            ..
        }
    ));
}

#[test]
fn retained_slots_belong_to_the_executor_instance_that_spawned_the_child() {
    let first = rig(immortal());
    let second = rig(exits_after(0));
    let token = CancellationToken::new();
    first.clock.set_hook({
        let token = token.clone();
        move |_| token.cancel()
    });
    let outcome = first.executor.run_sequence(&commands(&["tool"]), &token, &token);
    assert!(matches!(
        outcome,
        SequenceOutcome::Cancelled(Some(Cleanup::Retained(_)))
    ));
    assert_eq!(lock(&first.executor.state).used, 1);
    assert_eq!(lock(&second.executor.state).used, 0);
    let fresh = CancellationToken::new();
    assert!(matches!(
        second.executor.run_sequence(&commands(&["tool"]), &fresh, &fresh),
        SequenceOutcome::Completed
    ));
}

// ---------------------------------------------------------------------------
// Captured mode
// ---------------------------------------------------------------------------

fn limits(deadline_ms: u64, out: usize, err: usize) -> CaptureLimits {
    CaptureLimits::new(Duration::from_millis(deadline_ms), out, err).expect("limits")
}

#[test]
fn capture_limits_are_validated_against_the_documented_maximum() {
    assert_eq!(
        CaptureLimits::new(Duration::ZERO, 1, 1),
        Err(LimitError::ZeroDeadline)
    );
    assert_eq!(
        CaptureLimits::new(Duration::from_secs(1), MAX_CAPTURE_BYTES + 1, 1),
        Err(LimitError::CapTooLarge(CaptureStream::Stdout))
    );
    assert_eq!(
        CaptureLimits::new(Duration::from_secs(1), 1, MAX_CAPTURE_BYTES + 1),
        Err(LimitError::CapTooLarge(CaptureStream::Stderr))
    );
    assert!(
        CaptureLimits::new(Duration::from_secs(1), MAX_CAPTURE_BYTES, MAX_CAPTURE_BYTES).is_ok()
    );
    assert_eq!(limits(5, 1, 1).deadline(), Duration::from_millis(5));
}

#[test]
fn captured_output_is_streamed_while_the_child_runs_and_returned_complete() {
    let script = Script {
        exit_at: Some(Duration::from_millis(50)),
        stdout: vec![
            (Duration::from_millis(0), b"one ".to_vec()),
            (Duration::from_millis(20), b"two".to_vec()),
        ],
        stderr: vec![(Duration::from_millis(30), b"warn".to_vec())],
        ..Script::default()
    };
    let rig = rig(script);
    let cancel = CancellationToken::new();
    let outcome = rig.executor.run_captured("tool arg", limits(1000, 16, 16), &cancel, &cancel);
    let CaptureOutcome::Completed(output) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(output.stdout, b"one two");
    assert_eq!(output.stderr, b"warn");
    assert!(output.status.success());
    assert_eq!(rig.factory.spawned()[0].stdio, StdioMode::Capture);
    // Draining ran on the quantum while the child was alive, not at exit only.
    assert!(rig.clock.wait_count() >= 4);
}

#[test]
fn a_nonzero_exit_is_still_a_completed_capture() {
    let rig = rig(Script { exit_code: 4, ..exits_after(0) });
    let cancel = CancellationToken::new();
    let CaptureOutcome::Completed(output) =
        rig.executor.run_captured("tool", limits(100, 4, 4), &cancel, &cancel)
    else {
        panic!("expected completion");
    };
    assert_eq!(output.status.code(), Some(4));
}

#[test]
fn each_stream_has_its_own_cap_and_overflow_is_typed_not_truncated() {
    let rig = rig(Script {
        stdout: vec![(Duration::ZERO, vec![b'x'; 8])],
        stderr: vec![(Duration::ZERO, vec![b'y'; 5])],
        dies_on_kill: true,
        ..Script::default()
    });
    let cancel = CancellationToken::new();
    // Exactly at the cap is fine for stdout; stderr overflows its smaller cap.
    let outcome = rig.executor.run_captured("tool", limits(1000, 8, 4), &cancel, &cancel);
    assert!(
        matches!(outcome, CaptureOutcome::Overflow {
            stream: CaptureStream::Stderr,
            cleanup: Cleanup::Reaped
        }),
        "{outcome:?}"
    );
    assert_eq!(rig.factory.kills(), 1);

    let outcome = rig.executor.run_captured("tool", limits(1000, 7, 8), &cancel, &cancel);
    assert!(
        matches!(outcome, CaptureOutcome::Overflow {
            stream: CaptureStream::Stdout,
            ..
        }),
        "{outcome:?}"
    );
}

#[test]
fn a_child_that_exits_with_output_exactly_at_the_cap_completes() {
    let rig = rig(Script {
        exit_at: Some(Duration::from_millis(10)),
        stdout: vec![(Duration::ZERO, vec![b'x'; 8])],
        ..Script::default()
    });
    let cancel = CancellationToken::new();
    assert!(matches!(
        rig.executor.run_captured("tool", limits(1000, 8, 0), &cancel, &cancel),
        CaptureOutcome::Completed(_)
    ));
}

#[test]
fn the_deadline_terminates_the_child_and_reports_a_timeout() {
    let rig = rig(Script {
        dies_on_kill: true,
        ..Script::default()
    });
    let cancel = CancellationToken::new();
    let outcome = rig.executor.run_captured("tool", limits(100, 8, 8), &cancel, &cancel);
    assert!(
        matches!(outcome, CaptureOutcome::TimedOut { cleanup: Cleanup::Reaped }),
        "{outcome:?}"
    );
    assert_eq!(rig.clock.elapsed(), Duration::from_millis(100));
    assert_eq!(rig.factory.kills(), 1);
}

#[test]
fn a_timeout_with_an_unkillable_child_retains_it_with_an_explicit_outcome() {
    let rig = rig(immortal());
    let cancel = CancellationToken::new();
    let outcome = rig.executor.run_captured("tool", limits(50, 8, 8), &cancel, &cancel);
    let CaptureOutcome::TimedOut {
        cleanup: Cleanup::Retained(diagnostic),
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert!(diagnostic.contains("retained for reaping"));
    assert_eq!(rig.executor.sweep().len(), 1);
    assert_eq!(lock(&rig.executor.state).used, 1);
}

#[test]
fn captured_cancellation_covers_before_spawn_the_gate_and_the_wait() {
    let rig = rig(Script {
        dies_on_kill: true,
        ..Script::default()
    });
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        rig.executor.run_captured("tool", limits(1000, 8, 8), &cancelled, &cancelled),
        CaptureOutcome::Cancelled(None)
    ));
    assert!(rig.factory.spawned().is_empty());

    let cancel = CancellationToken::new();
    let gate = StaleOnAdmit {
        cancel: cancel.clone(),
        admitted: AtomicUsize::new(0),
    };
    assert!(matches!(
        rig.executor.run_captured("tool", limits(1000, 8, 8), &cancel, &gate),
        CaptureOutcome::Cancelled(None)
    ));
    assert!(rig.factory.spawned().is_empty());

    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    rig.clock.set_hook(move |count| {
        if count == 2 {
            trigger.cancel();
        }
    });
    assert!(matches!(
        rig.executor.run_captured("tool", limits(1000, 8, 8), &cancel, &cancel),
        CaptureOutcome::Cancelled(Some(Cleanup::Reaped))
    ));
    assert_eq!(rig.factory.spawned().len(), 1);
}

#[test]
fn captured_resolution_and_wait_failures_are_typed() {
    let rig = rig(Script { wait_fails: true, ..immortal() });
    let cancel = CancellationToken::new();
    assert!(matches!(
        rig.executor
            .run_captured("definitely-missing", limits(100, 8, 8), &cancel, &cancel),
        CaptureOutcome::Failed {
            failure: StepFailure::Resolve(_),
            cleanup: None
        }
    ));
    let outcome = rig.executor.run_captured("tool", limits(100, 8, 8), &cancel, &cancel);
    assert!(
        matches!(outcome, CaptureOutcome::Failed {
            failure: StepFailure::Wait(_),
            cleanup: Some(_)
        }),
        "{outcome:?}"
    );
}

#[test]
fn the_captured_stache_name_uses_the_internal_cli_rewrite_too() {
    let rig = rig(exits_after(0));
    let cancel = CancellationToken::new();
    let _ = rig.executor.run_captured("stache reload", limits(100, 8, 8), &cancel, &cancel);
    let spawned = rig.factory.spawned();
    assert_eq!(spawned[0].arguments, [INTERNAL_CLI_FLAG, "reload"]);
    assert_eq!(
        Some(&spawned[0].program),
        rig.executor.environment().next_executable.as_ref()
    );
}

// ---------------------------------------------------------------------------
// Real processes (harmless fixtures only)
// ---------------------------------------------------------------------------

#[test]
fn real_pipes_are_drained_nonblocking_through_the_os_factory() {
    let executor = CommandExecutor::new(CommandEnvironment::default());
    let cancel = CancellationToken::new();
    let outcome = executor.run_captured(
        "/bin/sh -c 'printf out; printf err >&2; exit 3'",
        limits(10_000, 64, 64),
        &cancel,
        &cancel,
    );
    let CaptureOutcome::Completed(output) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(output.stdout, b"out");
    assert_eq!(output.stderr, b"err");
    assert_eq!(output.status.code(), Some(3));
}

#[test]
fn a_real_child_that_floods_stdout_overflows_and_is_reaped() {
    let executor = CommandExecutor::new(CommandEnvironment::default());
    let cancel = CancellationToken::new();
    let outcome = executor.run_captured(
        "/bin/sh -c 'while :; do printf xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; done'",
        limits(10_000, 256, 64),
        &cancel,
        &cancel,
    );
    assert!(
        matches!(outcome, CaptureOutcome::Overflow {
            stream: CaptureStream::Stdout,
            cleanup: Cleanup::Reaped
        }),
        "{outcome:?}"
    );
    assert!(executor.sweep().is_empty());
}
