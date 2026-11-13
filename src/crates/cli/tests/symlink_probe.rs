//! The symlink CLI probe (`RELEASE-05-07`, probe half).
//!
//! The question: when an executable inside `<App>.app/Contents/MacOS/` is run
//! through a symlink outside the bundle, does `std::env::current_exe` report
//! the link path, so `is_app_bundle` is false and CLI mode is selected?
//!
//! Safety: this never executes the real `stache` binary. The helper is this
//! test executable itself, copied to `<tmp>/Probe.app/Contents/MacOS/stache`;
//! in child mode it only prints `current_exe` and the verdict of the real
//! `stache_cli::mode::is_app_bundle` (exactly as `main.rs` feeds it: no
//! canonicalization) and exits.

use std::path::Path;
use std::process::Command;

use stache_cli::mode::is_app_bundle;

const CHILD: &str = "STACHE_SYMLINK_PROBE_CHILD";

/// Child mode only: prints the probe line. Inert in a normal test run.
#[test]
fn probe_child() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let exe = std::env::current_exe().unwrap_or_default();
    println!(
        "probe: current_exe={} is_app_bundle={}",
        exe.display(),
        is_app_bundle(&exe)
    );
}

fn probe(program: &Path) -> String { probe_with(Command::new(program)) }

fn probe_with(mut command: Command) -> String {
    let output = command
        .args(["--exact", "probe_child", "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .output()
        .expect("run helper");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let line = stdout
        .lines()
        .find_map(|line| line.find("probe: ").map(|start| &line[start..]))
        .unwrap_or_else(|| panic!("no probe line: {stdout}"))
        .to_owned();
    eprintln!("{line}");
    line
}

#[test]
fn a_link_outside_the_bundle_selects_cli_mode() {
    let root = tempfile::tempdir().expect("root");
    let root = root.path().canonicalize().expect("canonical root");
    let macos = root.join("Probe.app").join("Contents").join("MacOS");
    std::fs::create_dir_all(&macos).expect("bundle");
    let helper = macos.join("stache");
    std::fs::copy(std::env::current_exe().expect("test exe"), &helper).expect("copy helper");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    let link = bin.join("stache");
    std::os::unix::fs::symlink(&helper, &link).expect("link");

    assert_eq!(
        probe(&helper),
        format!("probe: current_exe={} is_app_bundle=true", helper.display())
    );
    assert_eq!(
        probe(&link),
        format!("probe: current_exe={} is_app_bundle=false", link.display())
    );
    // As a shell or Homebrew's linked `bin` reaches it: by name on `PATH`.
    let mut by_name = Command::new("stache");
    by_name.env("PATH", &bin);
    assert_eq!(
        probe_with(by_name),
        format!("probe: current_exe={} is_app_bundle=false", link.display())
    );
}
