//! Identity inventory checks (`RELEASE-04-01`, `RELEASE-04-06`).
//!
//! The base manifest must carry this stage's release identity and product name,
//! and the other identity's literal must not leak into a tracked source outside
//! a short allow-list. The rename flips the two constants below, not the logic.

use std::path::{Path, PathBuf};
use std::process::Command;

use stache_app::PRODUCT_NAME;
use stache_sdk::{IDENTITY, RELEASE_IDENTITY};

/// The identifier the base `tauri.conf.json` ships with in this stage.
/// Stage 3 changes it to `RELEASE_IDENTITY`.
const BASE_IDENTIFIER: &str = IDENTITY;
/// The identity whose literal must not appear in sources in this stage.
/// Stage 3 changes it to `DEVELOPMENT_IDENTITY`.
const OTHER_IDENTIFIER: &str = RELEASE_IDENTITY;
/// The other product name, as an exact string literal. Stage 3 changes it to
/// `Stache Next`.
const OTHER_PRODUCT_NAME: &str = "Stache";

/// Files allowed to name the other identity: the constants, isolation tests
/// that assert the two never meet, and the measure tool's legacy identifier.
/// Test files (`tests/` directories, `*.test.ts(x)`, `*.spec.ts`) are allowed
/// by [`is_test`]; docs and archives are not sources.
const IDENTIFIER_ALLOWED: &[&str] = &[
    "src/crates/sdk/src/paths.rs",
    "src/crates/sdk/src/instance.rs",
    "src/crates/cli/src/cache.rs",
    "scripts/measure/paths.ts",
    "scripts/measure/process.ts",
];

/// Files allowed to hold the other product name as a literal: the popover's
/// `Stache` heading is the brand mark, not the bundle's product name.
const PRODUCT_NAME_ALLOWED: &[&str] = &["src/ui/src/windows/Popover/Popover.tsx"];

fn root() -> PathBuf { Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..") }

/// Tracked files, minus archives, docs and generated output.
fn tracked_sources() -> Vec<String> {
    let output = Command::new("git")
        .args(["-c", "core.fsmonitor=false", "ls-files", "-z"])
        .current_dir(root())
        .output()
        .expect("git ls-files");
    assert!(output.status.success(), "git ls-files failed");
    String::from_utf8(output.stdout)
        .expect("utf-8 paths")
        .split('\0')
        .filter(|path| !path.is_empty())
        .filter(|path| {
            ![
                "_legacy/",
                "docs/",
                "target/",
                "node_modules/",
                ".worktrees/",
                "src/ui/dist/",
                "coverage/",
            ]
            .iter()
            .any(|prefix| path.starts_with(prefix))
        })
        .filter(|path| {
            !Path::new(path).extension().is_some_and(|extension| {
                ["md", "png", "jpg", "icns", "ico", "lock", "json"]
                    .iter()
                    .any(|skip| extension.eq_ignore_ascii_case(skip))
            }) || path.ends_with("tauri.conf.json")
                || *path == "package.json"
        })
        .filter(|path| !path.contains(".gen.") && !path.contains("/generated/"))
        .map(str::to_owned)
        .collect()
}

fn is_test(path: &str) -> bool {
    path.contains("/tests/")
        || path.ends_with("_tests.rs")
        || path.ends_with("/tests.rs")
        || path.ends_with(".test.ts")
        || path.ends_with(".test.tsx")
        || path.ends_with(".spec.ts")
}

fn read(path: &str) -> Option<String> { std::fs::read_to_string(root().join(path)).ok() }

/// Whether `text` holds `identifier` exactly, not as a prefix of a longer one
/// such as `com.marcosmoura.stache.next` or `com.marcosmoura.stache.helper`.
fn holds_exact(text: &str, identifier: &str) -> bool {
    text.match_indices(identifier).any(|(index, _)| {
        let rest = &text[index + identifier.len()..];
        let mut chars = rest.chars();
        match chars.next() {
            None => true,
            Some('.') => !chars.next().is_some_and(|next| next.is_ascii_alphanumeric()),
            Some(next) => !(next.is_ascii_alphanumeric() || next == '-' || next == '_'),
        }
    })
}

#[test]
fn the_base_manifest_matches_the_identity_constants() {
    let config: serde_json::Value =
        serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri config");
    assert_eq!(config["identifier"], BASE_IDENTIFIER);
    assert_eq!(config["productName"], PRODUCT_NAME);
    assert_ne!(BASE_IDENTIFIER, OTHER_IDENTIFIER);
    assert_ne!(PRODUCT_NAME, OTHER_PRODUCT_NAME);
}

#[test]
fn exact_identifier_matching_ignores_longer_identifiers() {
    assert!(holds_exact(
        "\"com.marcosmoura.stache\"",
        "com.marcosmoura.stache"
    ));
    assert!(holds_exact(
        "Caches/com.marcosmoura.stache/ipc",
        "com.marcosmoura.stache"
    ));
    assert!(holds_exact(
        "is com.marcosmoura.stache.",
        "com.marcosmoura.stache"
    ));
    assert!(!holds_exact(
        "com.marcosmoura.stache.next",
        "com.marcosmoura.stache"
    ));
    assert!(!holds_exact(
        "com.marcosmoura.stache.helper",
        "com.marcosmoura.stache"
    ));
    assert!(!holds_exact(
        "com.marcosmoura.stache-app",
        "com.marcosmoura.stache"
    ));
}

#[test]
fn the_other_identity_appears_only_in_the_allow_list() {
    let quoted: Vec<String> = ['"', '\'', '`']
        .iter()
        .map(|quote| format!("{quote}{OTHER_PRODUCT_NAME}{quote}"))
        .collect();
    let mut leaks = Vec::new();
    for path in tracked_sources() {
        if is_test(&path) || path.ends_with("tests/identity.rs") {
            continue;
        }
        let Some(text) = read(&path) else { continue };
        if holds_exact(&text, OTHER_IDENTIFIER) && !IDENTIFIER_ALLOWED.contains(&path.as_str()) {
            leaks.push(format!("{path}: {OTHER_IDENTIFIER}"));
        }
        let names_product = quoted.iter().any(|literal| text.contains(literal.as_str()))
            || text.contains(&format!(">{OTHER_PRODUCT_NAME}<"));
        if names_product && !PRODUCT_NAME_ALLOWED.contains(&path.as_str()) {
            leaks.push(format!("{path}: {OTHER_PRODUCT_NAME:?}"));
        }
    }
    assert!(
        leaks.is_empty(),
        "other identity outside the allow-list: {leaks:#?}"
    );
}

/// Lines that are code, not comments. A trailing comment on a code line still
/// counts; that only makes the checks below stricter.
fn code_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter(|line| {
        let line = line.trim_start();
        !(line.starts_with("//")
            || line.starts_with('*')
            || line.starts_with("/*")
            || line.starts_with('#'))
    })
}

fn has_word(line: &str, word: &str) -> bool {
    line.match_indices(word).any(|(index, _)| {
        let before = line[..index].chars().next_back();
        let after = line[index + word.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// `RELEASE-04-06`. What this proves, and its limits:
/// - no `killall`, `pkill` or `pgrep` appears on a non-comment line of a tracked
///   file in `scripts/`, `.github/`, `package.json` or `src/` (the only tools that
///   select processes by name);
/// - no non-comment line compares a quoted `stache` against a name, executable
///   or command value (`==`, `===`, `.contains`, `.includes`, `.ends_with`);
/// - the collision code and its macOS query mention no terminate or signal call.
///
/// It is a text check: a name lookup built indirectly (string concatenation,
/// a value from configuration) would pass. Signals by PID are not covered here;
/// the measure tool signals only its own recorded children.
#[test]
fn no_code_path_selects_or_terminates_a_process_by_name() {
    let mut found = Vec::new();
    for path in tracked_sources() {
        let in_scope = path.starts_with("scripts/")
            || path.starts_with(".github/")
            || path == "package.json"
            || path.starts_with("src/");
        if !in_scope || path.ends_with("tests/identity.rs") {
            continue;
        }
        let Some(text) = read(&path) else { continue };
        for line in code_lines(&text) {
            for tool in ["killall", "pkill", "pgrep"] {
                if has_word(line, tool) {
                    found.push(format!("{path}: {tool}: {}", line.trim()));
                }
            }
            let lower = line.to_ascii_lowercase();
            let quoted_stache = ["\"stache\"", "'stache'", "`stache`"]
                .iter()
                .any(|quoted| lower.contains(quoted));
            let names_a_process = ["name", "executable", "command", "comm"]
                .iter()
                .any(|field| lower.contains(field));
            let compares = [
                "==",
                ".contains(",
                ".includes(",
                ".ends_with(",
                ".endswith(",
            ]
            .iter()
            .any(|operator| lower.contains(operator));
            if quoted_stache && names_a_process && compares {
                found.push(format!("{path}: name comparison: {}", line.trim()));
            }
        }
    }
    for path in [
        "src/crates/app/src/collision.rs",
        "src/crates/macos/src/running_instances.rs",
    ] {
        let text = read(path).expect("collision source");
        for line in code_lines(&text) {
            for call in [
                "terminate",
                "forceTerminate",
                "kill(",
                "signal(",
                "openURL",
                "open(",
            ] {
                if line.contains(call) {
                    found.push(format!("{path}: {call}: {}", line.trim()));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "process selection by name or a signal path: {found:#?}"
    );
}
