//! Identity inventory checks (`RELEASE-04-01`, `RELEASE-04-06`, `RELEASE-04-12`,
//! `RELEASE-04-15`).
//!
//! The base manifest carries the release identity and product name, the
//! development overlay carries the development ones, and the development
//! identity's literal must not leak into a tracked source outside a short
//! allow-list.

use std::path::{Path, PathBuf};
use std::process::Command;

use stache_app::PRODUCT_NAME;
use stache_sdk::{DEVELOPMENT_IDENTITY, RELEASE_IDENTITY};

/// The identifier the base `tauri.conf.json` ships with.
const BASE_IDENTIFIER: &str = RELEASE_IDENTITY;
/// The identity whose literal must not appear in sources: development.
const OTHER_IDENTIFIER: &str = DEVELOPMENT_IDENTITY;
/// The development product name, as an exact string literal.
const OTHER_PRODUCT_NAME: &str = "Stache Next";

/// Files allowed to name the development identity or product name: the
/// identity constants and the development overlay. Test files (`tests/`
/// directories, `*.test.ts(x)`, `*.spec.ts`) are allowed by [`is_test`]; docs
/// and archives are not sources.
const IDENTIFIER_ALLOWED: &[&str] = &[
    "src/crates/sdk/src/paths.rs",
    "src/crates/app/tauri.dev.conf.json",
];

/// Files allowed to hold the development product name as a literal.
const PRODUCT_NAME_ALLOWED: &[&str] = &["src/crates/app/tauri.dev.conf.json"];

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

fn json(path: &str) -> serde_json::Value {
    serde_json::from_str(&read(path).expect(path)).expect(path)
}

/// `RELEASE-04-12`: the overlay is development-only and the release profile
/// never enables debug assertions, so a package always selects the release
/// identity.
#[test]
fn the_development_overlay_is_dev_only_and_release_builds_select_the_release_identity() {
    let base = json("src/crates/app/tauri.conf.json");
    assert_eq!(base["identifier"], "com.marcosmoura.stache");
    assert_eq!(base["productName"], "Stache");
    assert_eq!(
        base["bundle"]["resources"],
        serde_json::json!({ "resources/media-control/": "media-control/" })
    );

    let overlay = json("src/crates/app/tauri.dev.conf.json");
    assert_eq!(
        overlay,
        serde_json::json!({
            "productName": "Stache Next",
            "identifier": "com.marcosmoura.stache.next",
        })
    );

    let package = json("package.json");
    let scripts = package["scripts"].as_object().expect("scripts");
    let referencing: Vec<&String> = scripts
        .iter()
        .filter(|(_, command)| command.as_str().is_some_and(|c| c.contains("tauri.dev.conf.json")))
        .map(|(name, _)| name)
        .collect();
    assert_eq!(referencing, ["tauri:dev"]);
    let dev = scripts["tauri:dev"].as_str().expect("tauri:dev");
    let base_at = dev.find("--config src/crates/app/tauri.conf.json").expect("base config");
    let overlay_at = dev.find("--config src/crates/app/tauri.dev.conf.json").expect("overlay");
    assert!(base_at < overlay_at, "{dev}");
    let build = scripts["tauri:build"].as_str().expect("tauri:build");
    assert_eq!(build.matches("--config").count(), 1, "{build}");
    assert!(
        build.contains("--config src/crates/app/tauri.conf.json"),
        "{build}"
    );

    for path in tracked_sources() {
        if (path.starts_with("scripts/release") || path.starts_with(".github/"))
            && let Some(text) = read(&path)
        {
            assert!(!text.contains("tauri.dev.conf"), "{path} references the overlay");
        }
    }

    let manifest: toml::Value =
        toml::from_str(&read("Cargo.toml").expect("workspace manifest")).expect("toml");
    let release = &manifest["profile"]["release"];
    assert!(release.get("debug-assertions").is_none(), "{release:?}");
    let flags = release.get("rustflags").map(ToString::to_string).unwrap_or_default();
    assert!(!flags.contains("debug-assertions"), "{flags}");
    for section in ["package", "build-override"] {
        let Some(overrides) = release.get(section) else {
            continue;
        };
        assert!(
            !overrides.to_string().contains("debug-assertions"),
            "{section}: {overrides:?}"
        );
    }
    for path in tracked_sources() {
        if path.ends_with("Cargo.toml") && path != "Cargo.toml" {
            let text = read(&path).expect("manifest");
            assert!(!text.contains("[profile"), "{path} declares a profile");
        }
    }
}

/// `RELEASE-04-15`: the identity is a compile-time constant chosen only by
/// `cfg!(debug_assertions)`; no environment, config key or flag reaches it.
/// A text check over the SDK's path module and the parameterized resolver's
/// callers.
#[test]
fn the_identity_is_selected_only_by_debug_assertions() {
    let paths = read("src/crates/sdk/src/paths.rs").expect("paths.rs");
    let declaration: String = paths
        .split("pub const IDENTITY: &str =")
        .nth(1)
        .expect("IDENTITY declaration")
        .split(';')
        .next()
        .expect("declaration end")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        declaration,
        "if cfg!(debug_assertions) { DEVELOPMENT_IDENTITY } else { RELEASE_IDENTITY }"
    );
    assert_eq!(paths.matches("pub const IDENTITY").count(), 1);
    for forbidden in ["env!(", "option_env!(", "std::env::var(\"STACHE"] {
        assert!(!paths.contains(forbidden), "{forbidden}");
    }
    let environment = paths
        .split("pub struct AppEnvironment {")
        .nth(1)
        .and_then(|rest| rest.split('}').next())
        .expect("AppEnvironment");
    assert!(
        !environment.to_ascii_lowercase().contains("identity"),
        "{environment}"
    );
    assert_eq!(
        stache_sdk::IDENTITY,
        DEVELOPMENT_IDENTITY,
        "tests compile as debug"
    );

    // Production resolves through `AppPaths::resolve`, which passes `IDENTITY`;
    // `resolve_for` appears only in its definition and in test modules.
    for path in tracked_sources() {
        if Path::new(&path).extension().is_none_or(|extension| extension != "rs")
            || is_test(&path)
            || path == "src/crates/sdk/src/paths.rs"
        {
            continue;
        }
        let Some(text) = read(&path) else { continue };
        let production = text.split("#[cfg(test)]").next().unwrap_or_default();
        assert!(!production.contains("resolve_for("), "{path} calls resolve_for");
    }
    let production = paths.split("#[cfg(test)]").next().unwrap_or_default();
    assert_eq!(
        production.matches("resolve_for(").count(),
        2,
        "definition and resolve only"
    );
}
