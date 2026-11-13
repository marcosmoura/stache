//! The headless generator must check without changing committed source.

#![cfg(feature = "dev-tools")]

use std::path::PathBuf;
use std::process::Command;

fn committed() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("src/ui/src/ipc/bindings.gen.ts")
}

#[test]
fn check_compares_without_modifying_the_committed_file() {
    let path = committed();
    let before = std::fs::read(&path).expect("read committed bindings");
    let status = Command::new(env!("CARGO_BIN_EXE_generate-bindings"))
        .arg("--check")
        .status()
        .expect("run generate-bindings");
    assert!(status.success(), "committed bindings are stale");
    let after = std::fs::read(&path).expect("read committed bindings");
    assert_eq!(before, after, "--check must not rewrite source");
}

#[test]
fn generated_output_ends_with_exactly_one_newline() {
    let path = committed();
    let status = Command::new(env!("CARGO_BIN_EXE_generate-bindings"))
        .arg("--check")
        .status()
        .expect("run generate-bindings");
    assert!(status.success(), "committed bindings are stale");
    let generated = std::fs::read(&path).expect("read committed bindings");
    assert!(
        generated.ends_with(b"\n"),
        "generated output must end in a newline"
    );
    assert!(
        !generated.ends_with(b"\n\n"),
        "generated output must not end in a blank line"
    );
}

#[test]
fn an_unknown_argument_fails_without_touching_source() {
    let path = committed();
    let before = std::fs::read(&path).expect("read committed bindings");
    let status = Command::new(env!("CARGO_BIN_EXE_generate-bindings"))
        .arg("--not-an-option")
        .status()
        .expect("run generate-bindings");
    assert!(!status.success());
    assert_eq!(std::fs::read(&path).expect("read committed bindings"), before);
}
