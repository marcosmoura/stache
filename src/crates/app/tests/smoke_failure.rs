//! Opt-in real-process exit probe for a rejected host setup.
//!
//! Enabled with `--features smoke`; it launches the actual `stache-smoke`
//! binary under a runtime path whose socket exceeds the 103-byte limit while
//! the instance lock remains valid. The host must report failure and the child
//! must exit non-zero even though `run_return` reports zero on macOS.

#![cfg(feature = "smoke")]

use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn rejected_host_setup_exits_non_zero_from_the_real_process() {
    let base = tempfile::tempdir().expect("temporary root");
    let long_root = base.path().join("x".repeat(80));
    std::fs::create_dir_all(&long_root).expect("long root");

    let mut child = Command::new(env!("CARGO_BIN_EXE_stache-smoke"))
        .env("HOME", &long_root)
        .env("TMPDIR", &long_root)
        .env("XDG_CONFIG_HOME", long_root.join("config"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn stache-smoke");

    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait for stache-smoke") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("stache-smoke did not exit after a rejected host setup");
        }
        thread::sleep(Duration::from_millis(50));
    };

    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(
        !status.success(),
        "rejected host setup exited successfully: {status:?}; stderr={stderr}"
    );
    assert!(
        stderr.contains("socket path") && stderr.contains("desktop host exited with code 1"),
        "unexpected failure output: {stderr}"
    );
}
