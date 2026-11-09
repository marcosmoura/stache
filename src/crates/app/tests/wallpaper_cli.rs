//! The composed `stache wallpaper` command tree in the application catalog.
//!
//! These tests use the production catalog (`catalog_for`) with disposable
//! environment roots. They only run commands that never reach the native
//! desktop or the control socket: help, completions, grammar errors,
//! both/neither validation, `list` and an empty `generate-all`.

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use stache_cli::CliEnvironment;
use stache_sdk::{AppEnvironment, ConfigEnvironment};

struct Run {
    code: ExitCode,
    out: String,
    err: String,
}

fn environment(root: &Path) -> CliEnvironment {
    CliEnvironment::new(
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

fn run(root: &Path, args: &[&str], ansi: bool) -> Run {
    let environment = environment(root);
    let commands = stache_app::commands::catalog_for(&environment);
    let configurations = stache_app::catalog::catalog().expect("catalog");
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = stache_cli::run_with_io(
        args.iter().map(OsString::from),
        &configurations,
        &commands,
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

#[test]
#[allow(clippy::redundant_closure_for_method_calls)] // clap is not a dev-dependency here.
fn the_production_catalog_contributes_the_audio_wallpaper_and_tiling_trees() {
    let root = tempfile::tempdir().expect("root");
    let commands = stache_app::commands::catalog_for(&environment(root.path()));
    assert_eq!(commands.names().collect::<Vec<_>>(), [
        "audio",
        "wallpaper",
        "tiling"
    ]);
    let composed = stache_cli::commands::compose(&commands);
    let wallpaper = composed.find_subcommand("wallpaper").expect("wallpaper");
    let names: Vec<&str> = wallpaper.get_subcommands().map(|command| command.get_name()).collect();
    assert_eq!(names, ["set", "generate-all", "list"]);
    let set = wallpaper.find_subcommand("set").expect("set");
    let shorts: Vec<Option<char>> = set.get_arguments().map(|arg| arg.get_short()).collect();
    assert_eq!(shorts, [None, Some('r'), Some('s')]);
    let tiling = composed.find_subcommand("tiling").expect("tiling");
    let names: Vec<&str> = tiling.get_subcommands().map(|command| command.get_name()).collect();
    assert_eq!(names, ["pause", "resume", "query", "window", "workspace"]);
}

#[test]
fn completions_and_help_include_the_wallpaper_surface() {
    let root = tempfile::tempdir().expect("root");
    let completions = run(root.path(), &["stache", "completions", "--shell", "zsh"], false);
    assert_eq!(completions.code, ExitCode::SUCCESS, "{}", completions.err);
    for needle in ["wallpaper", "generate-all", "list", "--random", "--screen"] {
        assert!(completions.out.contains(needle), "{needle}");
    }
    let help = run(root.path(), &["stache", "wallpaper", "--help"], false);
    assert_eq!(help.code, ExitCode::SUCCESS);
    assert!(
        help.out.contains("Wallpaper management commands"),
        "{}",
        help.out
    );
    assert!(run(root.path(), &["stache", "--help"], false).out.contains("wallpaper"));
}

#[test]
fn grammar_errors_exit_two_and_both_neither_exit_one_before_any_work() {
    let root = tempfile::tempdir().expect("root");
    let bad = run(
        root.path(),
        &["stache", "wallpaper", "set", "-r", "--screen", "x"],
        false,
    );
    assert_eq!(bad.code, ExitCode::from(2));
    assert!(bad.err.contains("Invalid screen value 'x'"), "{}", bad.err);
    let overflow = run(
        root.path(),
        &["stache", "wallpaper", "set", "-r", "-s", "99999999999"],
        false,
    );
    assert_eq!(overflow.code, ExitCode::from(2));

    // A malformed config proves no config read happens first.
    let config = root.path().join("cfg");
    fs::create_dir_all(&config).expect("config");
    fs::write(config.join("wallpapers.jsonc"), "{ nope").expect("malformed");
    let config_arg = config.to_str().expect("utf-8");
    let both = run(
        root.path(),
        &[
            "stache",
            "-c",
            config_arg,
            "wallpaper",
            "set",
            "a.png",
            "-r",
        ],
        false,
    );
    assert_eq!(both.code, ExitCode::FAILURE);
    assert_eq!(
        both.err,
        "stache: Cannot specify both <path> and --random. Use one or the other.\n"
    );
    let neither = run(
        root.path(),
        &["stache", "-c", config_arg, "wallpaper", "set"],
        false,
    );
    assert_eq!(neither.code, ExitCode::FAILURE);
    assert_eq!(
        neither.err,
        "stache: Either <path> or --random must be specified.\n"
    );
    assert!(
        !root.path().join("com.marcosmoura.stache.next").exists(),
        "no socket or runtime work"
    );
}

#[test]
fn the_global_config_flag_is_honored_by_the_contributed_list_command() {
    let root = tempfile::tempdir().expect("root");
    let config = root.path().join("cfg");
    fs::create_dir_all(&config).expect("config");
    fs::write(config.join("a.png"), b"not decoded by list").expect("image");
    fs::write(config.join("wallpapers.jsonc"), "{ \"list\": [\"a.png\"] }").expect("config");
    let config_arg = config.to_str().expect("utf-8");

    // Before and after the subcommand, and in `--config=` form.
    for args in [
        vec!["stache", "-c", config_arg, "wallpaper", "list"],
        vec!["stache", "wallpaper", "list", "-c", config_arg],
        vec![
            "stache",
            "wallpaper",
            "list",
            &format!("--config={config_arg}"),
        ],
    ] {
        let listed = run(root.path(), &args, false);
        assert_eq!(listed.code, ExitCode::SUCCESS, "{args:?}: {}", listed.err);
        let paths: Vec<String> = serde_json::from_str(&listed.out).expect("json array");
        assert_eq!(paths, [config.join("a.png").display().to_string()], "{args:?}");
    }
    // Terminal output is styled; piped output is not.
    let styled = run(
        root.path(),
        &["stache", "-c", config_arg, "wallpaper", "list"],
        true,
    );
    assert!(styled.out.contains('\u{1b}'), "{:?}", styled.out);
    let plain = run(
        root.path(),
        &["stache", "-c", config_arg, "wallpaper", "list"],
        false,
    );
    assert!(!plain.out.contains('\u{1b}'));

    // Without that config the default directory has no file: valid empty.
    let empty = run(root.path(), &["stache", "wallpaper", "list"], false);
    assert_eq!(
        (empty.code, empty.out.as_str()),
        (ExitCode::SUCCESS, "No wallpapers found.\n")
    );
    let generate = run(root.path(), &["stache", "wallpaper", "generate-all"], true);
    assert_eq!(
        (generate.code, generate.out.as_str()),
        (ExitCode::SUCCESS, "No wallpapers to generate.\n")
    );
}
