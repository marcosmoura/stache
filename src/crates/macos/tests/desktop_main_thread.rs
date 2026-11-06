//! Standalone main-thread compile/open proof for the desktop adapter.
//!
//! This binary deliberately uses `harness = false` so `main` runs on the process
//! main thread, exactly like the standalone CLI. It only reads `NSScreen` data
//! through [`NativeDesktop::snapshot`]; it never calls the setter, opens no
//! window, creates no `NSApplication`, tray or dispatch pump, and needs no
//! Tauri.
//!
//! On a headless machine zero displays are allowed. When displays are present,
//! identities are non-zero and the reported order matches `NSScreen.screens`
//! element by element.
//!
//! With the `testing` feature the same run also exercises the real
//! resolve-and-recheck half of the setter through
//! [`NativeDesktop::check_target_for_test`]: an absent id, the real id with an
//! unchanged entry, and the real id with a width, height or scale mismatch. The
//! mismatch is simulated; a physically connected or disconnected display is not
//! exercised. The entry point has no path to the native setter.
//!
//! The argument handling implements the small `--list --format terse` protocol
//! nextest requires from custom harnesses. `--list --format terse --ignored`
//! prints nothing because this harness ignores no test.

use std::sync::Arc;

use objc2::MainThreadMarker;
use objc2_app_kit::NSScreen;
use stache_macos::{DesktopBackend, MainThread, NativeDesktop, is_main_thread};

const TEST_NAME: &str = "standalone_main_thread_snapshot";
#[cfg(feature = "testing")]
const RECHECK_TEST_NAME: &str = "standalone_main_thread_target_recheck";

fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    if arguments.iter().any(|argument| argument == "--list") {
        if !arguments.iter().any(|argument| argument == "--ignored") {
            println!("{TEST_NAME}: test");
            #[cfg(feature = "testing")]
            println!("{RECHECK_TEST_NAME}: test");
        }
        return;
    }
    run();
}

fn run() {
    let marker = MainThreadMarker::new().expect("the harness main runs on the process main thread");
    assert!(
        is_main_thread(),
        "the standalone proof must run on the main thread"
    );

    let backend = NativeDesktop::new(Arc::new(MainThread));
    let snapshot = backend.snapshot().expect("native snapshot runs inline on the main thread");

    let native_order: Vec<u32> = NSScreen::screens(marker)
        .to_vec()
        .iter()
        .map(|screen| screen.CGDirectDisplayID())
        .collect();
    let reported: Vec<u32> = snapshot.entries().iter().map(|entry| entry.id.raw()).collect();
    assert_eq!(
        reported, native_order,
        "snapshot order must match NSScreen.screens"
    );

    let repeated = backend.snapshot().expect("second native snapshot");
    let repeated_order: Vec<u32> = repeated.entries().iter().map(|entry| entry.id.raw()).collect();
    assert_eq!(
        reported, repeated_order,
        "index order must stay stable between calls"
    );

    for (index, entry) in snapshot.entries().iter().enumerate() {
        assert_ne!(entry.id.raw(), 0, "display {index} has no CGDirectDisplayID");
        assert!(
            entry.scale.is_finite() && entry.scale > 0.0,
            "display {index} scale"
        );
        assert!(
            entry.frame.width.is_finite() && entry.frame.height.is_finite(),
            "display {index} frame"
        );
    }

    #[cfg(feature = "testing")]
    run_recheck(&backend);
}

#[cfg(feature = "testing")]
fn run_recheck(backend: &NativeDesktop) {
    use stache_macos::{DesktopError, DisplayEntry, DisplayId};

    let snapshot = backend.snapshot().expect("native snapshot before the target recheck");
    if snapshot.entries().is_empty() {
        println!(
            "skipped: the target recheck cases need at least one real display; \
             zero displays are allowed on a headless machine"
        );
        return;
    }

    let entry = *snapshot.get(0).expect("checked non-empty above");

    let absent = DisplayId::new(absent_id(&snapshot));
    let probe = DisplayEntry { id: absent, ..entry };
    assert_eq!(
        backend.check_target_for_test(absent, &probe),
        Err(DesktopError::DisplayMissing(absent)),
        "an id absent from NSScreen.screens must be reported as missing"
    );

    backend
        .check_target_for_test(entry.id, &entry)
        .expect("an unchanged entry must pass the recheck");

    let mut width_changed = entry;
    width_changed.frame.width += 1.0;
    assert_eq!(
        backend.check_target_for_test(entry.id, &width_changed),
        Err(DesktopError::DisplayChanged(entry.id)),
        "a width change must be reported as changed"
    );

    let mut height_changed = entry;
    height_changed.frame.height += 1.0;
    assert_eq!(
        backend.check_target_for_test(entry.id, &height_changed),
        Err(DesktopError::DisplayChanged(entry.id)),
        "a height change must be reported as changed"
    );

    let mut scale_changed = entry;
    scale_changed.scale += 1.0;
    assert_eq!(
        backend.check_target_for_test(entry.id, &scale_changed),
        Err(DesktopError::DisplayChanged(entry.id)),
        "a scale change must be reported as changed"
    );
}

#[cfg(feature = "testing")]
fn absent_id(snapshot: &stache_macos::DisplaySnapshot) -> u32 {
    let present: Vec<u32> = snapshot.entries().iter().map(|entry| entry.id.raw()).collect();
    let mut candidate = 1_u32;
    loop {
        if !present.contains(&candidate) {
            return candidate;
        }
        candidate = candidate.checked_add(1).expect("finite display list");
    }
}
