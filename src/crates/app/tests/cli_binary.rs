//! The built `stache` binary selects CLI mode without starting the desktop.
//!
//! Every command in this file exercises the real executable. A command that
//! accidentally initialized Tauri, the tray or a webview would not exit on its
//! own, which is exactly what the bounded wait catches.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};
use std::{fs, thread};

const BINARY: &str = env!("CARGO_BIN_EXE_stache");

struct Workspace {
    root: tempfile::TempDir,
}

impl Workspace {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().expect("temp dir"),
        }
    }

    fn home(&self) -> &Path { self.root.path() }

    fn runtime(&self) -> PathBuf { self.home().join("com.marcosmoura.stache.next") }

    fn cache(&self) -> PathBuf {
        self.home()
            .canonicalize()
            .expect("canonical home")
            .join("Library")
            .join("Caches")
            .join("com.marcosmoura.stache.next")
    }

    fn command(&self) -> Command {
        let mut command = Command::new(BINARY);
        command
            .env("HOME", self.home())
            .env("TMPDIR", self.home())
            .env_remove("XDG_CONFIG_HOME")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn run(&self, arguments: &[&str]) -> Output {
        let mut command = self.command();
        command.args(arguments);
        wait(command)
    }
}

fn wait(mut command: Command) -> Output {
    let mut child = command.spawn().expect("spawn stache");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => return child.wait_with_output().expect("output"),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let output = child.wait_with_output().expect("output after kill");
                panic!(
                    "stache did not exit in 30s (this usually means the desktop started): {}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("utf-8 stdout")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("utf-8 stderr")
}

fn assert_no_desktop(workspace: &Workspace) {
    assert!(
        !workspace.runtime().exists(),
        "standalone commands must not create the runtime directory"
    );
}

#[test]
fn cache_path_and_clear_use_the_injected_identity() {
    let workspace = Workspace::new();
    let output = workspace.run(&["cache", "path"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), format!("{}\n", workspace.cache().display()));
    assert_no_desktop(&workspace);

    fs::create_dir_all(workspace.cache()).expect("cache");
    fs::write(workspace.cache().join("file.bin"), [0_u8; 4]).expect("file");
    let legacy = workspace.home().join("Library").join("Caches").join("com.marcosmoura.stache");
    fs::create_dir_all(&legacy).expect("legacy");
    fs::write(legacy.join("keep"), "keep").expect("legacy file");

    let output = workspace.run(&["cache", "clear"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "Cache cleared successfully. Freed 4 bytes.\n");
    assert!(!workspace.cache().exists());
    assert!(legacy.join("keep").exists());
    assert_no_desktop(&workspace);

    let output = workspace.run(&["cache", "clear"]);
    assert!(output.status.success());
    assert_eq!(
        stdout(&output),
        "Cache directory does not exist. Nothing to clear.\n"
    );
}

#[test]
fn schema_is_piped_as_plain_json() {
    let workspace = Workspace::new();
    let output = workspace.run(&["schema"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = stdout(&output);
    assert!(!body.contains('\u{1b}'), "piped output must not contain ANSI");
    let value: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
    assert!(value["$id"].as_str().is_some_and(|id| id.contains("global.schema.json")));
    assert_no_desktop(&workspace);

    let output = workspace.run(&["schema", "proxyAudio"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("dependsOn"));

    let output = workspace.run(&["schema", "keepAwake"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let value: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("keepAwake schema JSON");
    assert!(
        value["$id"]
            .as_str()
            .is_some_and(|id| id.ends_with("/schemas/keepAwake.schema.json"))
    );
    assert_eq!(value["properties"]["enabled"]["type"], "boolean");
    assert_eq!(value["properties"]["enabled"]["default"], false);

    let output = workspace.run(&["schema", "notunes"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = stdout(&output);
    let value: serde_json::Value = serde_json::from_str(&body).expect("notunes schema JSON");
    let target = &value["properties"]["targetApp"];
    let types = target["type"].as_array().expect("targetApp has explicit types");
    assert!(types.iter().any(|kind| kind == "string"), "{types:?}");
    assert!(value.get("$defs").is_none(), "{body}");
    for preset in ["tidal", "spotify", "feishin", "none"] {
        assert!(!body.contains(preset), "{preset}: {body}");
    }
}

#[test]
fn schema_is_styled_on_a_terminal() {
    let workspace = Workspace::new();
    let mut command = Command::new("/usr/bin/script");
    command
        .arg("-q")
        .arg("/dev/null")
        .arg(BINARY)
        .arg("schema")
        .env("HOME", workspace.home())
        .env("TMPDIR", workspace.home())
        .env_remove("XDG_CONFIG_HOME")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = wait(command);
    assert!(output.status.success(), "{}", stderr(&output));
    let styled = stdout(&output);
    assert!(styled.contains("\u{1b}[36m"), "keys should be styled on a TTY");
    let plain = strip_ansi(&styled).replace('\r', "");
    let start = plain.find('{').expect("JSON body");
    serde_json::from_str::<serde_json::Value>(&plain[start..]).expect("styled output is JSON");
    assert_no_desktop(&workspace);
}

#[test]
fn a_malformed_flag_exits_two_without_starting_the_desktop() {
    let workspace = Workspace::new();
    let output = workspace.run(&["--definitely-not-a-flag"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("unexpected argument"),
        "{}",
        stderr(&output)
    );
    assert_no_desktop(&workspace);
}

#[test]
fn reload_without_a_running_host_exits_one_with_a_prefix() {
    let workspace = Workspace::new();
    let started = Instant::now();
    let output = workspace.run(&["reload"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        "stache: IPC error: Failed to send reload command to Stache app: Stache app is not running\n"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_no_desktop(&workspace);
}

#[test]
fn completions_are_generated_for_supported_shells() {
    let workspace = Workspace::new();
    for shell in ["zsh", "bash", "fish"] {
        let output = workspace.run(&["completions", "--shell", shell]);
        assert!(output.status.success(), "{shell}: {}", stderr(&output));
        assert!(!stdout(&output).is_empty(), "{shell}");
    }
    let output = workspace.run(&["completions", "-s", "zsh"]);
    let script = stdout(&output);
    assert!(script.starts_with("#compdef stache"), "{script}");
    assert!(script.contains("reload"));
    assert!(script.contains("audio"));
    assert!(!script.contains("tiling"));
    assert!(!script.contains("wallpaper"));
    assert_no_desktop(&workspace);
}

#[test]
fn audio_list_is_read_only_and_standalone() {
    let workspace = Workspace::new();
    let output = workspace.run(&["audio", "list"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = stdout(&output);
    assert!(
        body.contains("Name") || body == "No audio devices found.\n",
        "{body}"
    );
    assert!(!body.contains('\u{1b}'), "piped output must not contain ANSI");
    assert_no_desktop(&workspace);
}

#[test]
fn audio_list_json_is_plain_when_piped_and_obeys_direction_flags() {
    let workspace = Workspace::new();
    let output = workspace.run(&["audio", "list", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = stdout(&output);
    assert!(!body.contains('\u{1b}'), "piped output must not contain ANSI");
    let devices: Vec<serde_json::Value> = serde_json::from_str(&body).expect("json array");
    for device in &devices {
        for key in ["name", "type", "input", "output"] {
            assert!(device.get(key).is_some(), "missing {key} in {device}");
        }
    }

    let output = workspace.run(&["audio", "list", "-i", "-j"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let input: Vec<serde_json::Value> = serde_json::from_str(&stdout(&output)).expect("input json");
    assert!(input.iter().all(|device| device["input"].as_bool() == Some(true)));

    let output = workspace.run(&["audio", "list", "-o", "-j"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let output_devices: Vec<serde_json::Value> =
        serde_json::from_str(&stdout(&output)).expect("output json");
    assert!(output_devices.iter().all(|device| device["output"].as_bool() == Some(true)));
    assert_no_desktop(&workspace);
}

#[test]
fn audio_list_json_is_styled_on_a_terminal() {
    let workspace = Workspace::new();
    let mut command = Command::new("/usr/bin/script");
    command
        .arg("-q")
        .arg("/dev/null")
        .arg(BINARY)
        .arg("audio")
        .arg("list")
        .arg("--json")
        .env("HOME", workspace.home())
        .env("TMPDIR", workspace.home())
        .env_remove("XDG_CONFIG_HOME")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = wait(command);
    assert!(output.status.success(), "{}", stderr(&output));
    let styled = stdout(&output);
    assert!(styled.contains('\u{1b}'), "JSON should be styled on a TTY");
    let plain = strip_ansi(&styled).replace('\r', "");
    let start = plain.find(['[', '{']).expect("JSON body");
    serde_json::from_str::<serde_json::Value>(&plain[start..]).expect("styled output is JSON");
    assert_no_desktop(&workspace);
}

#[test]
fn config_path_uses_the_explicit_directory() {
    let workspace = Workspace::new();
    let directory = workspace.home().join("explicit-config");
    let output = workspace.run(&["-c", directory.to_str().expect("utf-8"), "config", "path"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = stdout(&output);
    assert!(body.starts_with(&format!("{}\n", directory.display())), "{body}");
    assert!(body.contains("  global.jsonc\n"), "{body}");
    assert!(body.contains("  keepAwake.jsonc\n"), "{body}");
    assert!(body.contains("  notunes.jsonc\n"), "{body}");
    assert!(body.contains("  proxyAudio.jsonc\n"), "{body}");
    assert_no_desktop(&workspace);
}

#[test]
fn config_init_writes_the_registered_templates() {
    let workspace = Workspace::new();
    let directory = workspace.home().join("init-config");
    let output = workspace.run(&["config", "init", "-p", directory.to_str().expect("utf-8")]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(directory.join("global.jsonc").exists());
    assert!(directory.join("proxyAudio.jsonc").exists());
    let keep_awake = std::fs::read_to_string(directory.join("keepAwake.jsonc")).expect("keepAwake");
    assert!(keep_awake.contains("\"enabled\": false"), "{keep_awake}");
    assert!(!keep_awake.contains("duration"), "{keep_awake}");
    let notunes = std::fs::read_to_string(directory.join("notunes.jsonc")).expect("notunes");
    assert!(notunes.contains("\"enabled\": false"), "{notunes}");
    assert!(!notunes.contains("targetApp"), "{notunes}");
    assert_no_desktop(&workspace);
}

fn strip_ansi(input: &str) -> String {
    let mut output = String::new();
    let mut characters = input.chars();
    while let Some(character) = characters.next() {
        if character == '\u{1b}' {
            for next in characters.by_ref() {
                if next == 'm' {
                    break;
                }
            }
        } else {
            output.push(character);
        }
    }
    output
}
