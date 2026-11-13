//! Static least-privilege and identity checks for the desktop manifest.

#[test]
fn the_desktop_manifest_has_no_fixed_window_and_an_explicit_capability() {
    let config: serde_json::Value =
        serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri config");
    assert_eq!(config["identifier"], stache_sdk::IDENTITY);
    assert_eq!(config["app"]["windows"], serde_json::json!([]));
    assert_eq!(
        config["app"]["security"]["capabilities"],
        serde_json::json!(["access-window", "bar-window", "popover-window"])
    );
    for key in ["csp", "devCsp"] {
        let csp = config["app"]["security"][key].as_str().expect("csp");
        assert!(csp.contains("object-src 'none'"), "{csp}");
        assert!(csp.contains("base-uri 'none'"), "{csp}");
        assert!(csp.contains("http://ipc.localhost"), "{csp}");
        assert!(!csp.contains("unsafe-eval"), "{csp}");
        assert!(!csp.contains("script-src"), "{csp}");
        assert!(!csp.contains('*'), "{csp}");
        assert!(!csp.contains("ipapi.co") && !csp.contains("ipinfo.io"), "{csp}");
        let connect: Vec<&str> = csp
            .split(';')
            .map(str::trim)
            .find_map(|directive| directive.strip_prefix("connect-src "))
            .expect("connect-src")
            .split_whitespace()
            .collect();
        // Every remote host is https and one of the three weather providers.
        let remote: Vec<&str> =
            connect.iter().copied().filter(|source| source.starts_with("https:")).collect();
        assert_eq!(remote, [
            "https://weather.visualcrossing.com",
            "https://api.open-meteo.com",
            "https://geocoding-api.open-meteo.com",
        ]);
        let local: Vec<&str> =
            connect.iter().copied().filter(|source| !source.starts_with("https:")).collect();
        let expected: &[&str] = if key == "csp" {
            &["ipc:", "http://ipc.localhost"]
        } else {
            &[
                "ipc:",
                "http://ipc.localhost",
                "http://localhost:1420",
                "ws://localhost:1420",
            ]
        };
        assert_eq!(local, expected, "{key}");
        let img: Vec<&str> = csp
            .split(';')
            .map(str::trim)
            .find_map(|directive| directive.strip_prefix("img-src "))
            .expect("img-src")
            .split_whitespace()
            .collect();
        assert_eq!(img, ["'self'", "data:"], "{key}");
    }
}

#[test]
fn the_access_window_has_only_the_named_least_privilege_permissions() {
    let capability: serde_json::Value =
        serde_json::from_str(include_str!("../capabilities/access-window.json"))
            .expect("capability");
    assert_eq!(capability["identifier"], "access-window");
    assert_eq!(capability["windows"], serde_json::json!(["stache-access-*"]));
    let permissions: Vec<&str> = capability["permissions"]
        .as_array()
        .expect("permissions")
        .iter()
        .map(|value| value.as_str().expect("permission"))
        .collect();
    assert_eq!(permissions, [
        "core:event:allow-listen",
        "core:event:allow-unlisten",
        "core:window:allow-close",
        "core:window:allow-start-dragging",
        "allow-app-info",
        "allow-host-status",
        "allow-request-accessibility",
        "allow-open-accessibility-settings",
        "allow-quit-app",
    ]);
    let joined = permissions.join(" ");
    for forbidden in [
        "core:default",
        "fs:",
        "shell:",
        "http:",
        "dialog:",
        "opener",
        "plugin",
    ] {
        assert!(!joined.contains(forbidden), "capability grants {forbidden}");
    }
}

#[test]
fn macos_private_api_is_enabled_in_config_and_cargo() {
    let config: serde_json::Value =
        serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri config");
    assert_eq!(config["app"]["macOSPrivateApi"], serde_json::json!(true));
    let manifest = include_str!("../Cargo.toml");
    let tauri_line = manifest
        .lines()
        .find(|line| line.starts_with("tauri = {"))
        .expect("tauri dependency");
    assert!(tauri_line.contains("\"macos-private-api\""), "{tauri_line}");
}

#[test]
fn the_bar_window_has_only_the_snapshot_and_event_listener_permissions() {
    let capability: serde_json::Value =
        serde_json::from_str(include_str!("../capabilities/bar.json")).expect("capability");
    assert_eq!(capability["identifier"], "bar-window");
    assert_eq!(capability["windows"], serde_json::json!(["stache-bar-*"]));
    let permissions: Vec<&str> = capability["permissions"]
        .as_array()
        .expect("permissions")
        .iter()
        .map(|value| value.as_str().expect("permission"))
        .collect();
    // `bar_snapshot`, `bar_action`, `bar_weather`, `bar_media_artwork`, `bar_session_action` and
    // `bar_tiling_action` are ACL-gated to this
    // capability (and also check the calling window label themselves); the event listener
    // pair is the only rest.
    assert_eq!(permissions, [
        "allow-bar-snapshot",
        "allow-bar-action",
        "allow-bar-weather",
        "allow-bar-media-artwork",
        "allow-bar-session-action",
        "allow-bar-tiling-action",
        "core:event:allow-listen",
        "core:event:allow-unlisten"
    ]);
    for key in capability.as_object().expect("object").keys() {
        assert!(
            ["identifier", "description", "windows", "permissions"].contains(&key.as_str()),
            "unexpected capability key {key}"
        );
    }
    let joined = permissions.join(" ");
    for forbidden in [
        "core:default",
        "core:window",
        "fs:",
        "shell:",
        "http:",
        "dialog:",
        "opener",
        // The only media permission is the read-only artwork fetch.
        "media:",
        "media-control",
        "allow-media",
        "allow-weather",
        "system",
        "tray",
        "allow-request-accessibility",
        "allow-quit-app",
    ] {
        assert!(!joined.contains(forbidden), "capability grants {forbidden}");
    }
}

#[test]
fn only_the_bar_capability_reaches_bar_windows() {
    for (name, text) in [
        (
            "access-window",
            include_str!("../capabilities/access-window.json"),
        ),
        (
            "popover-window",
            include_str!("../capabilities/popover-window.json"),
        ),
    ] {
        let capability: serde_json::Value = serde_json::from_str(text).expect("capability");
        let windows = capability["windows"].to_string();
        assert!(
            !windows.contains("bar"),
            "{name} must not match Bar windows: {windows}"
        );
    }
}

#[test]
fn the_popover_has_only_its_named_commands_and_the_event_listener() {
    let capability: serde_json::Value =
        serde_json::from_str(include_str!("../capabilities/popover-window.json"))
            .expect("capability");
    assert_eq!(capability["identifier"], "popover-window");
    assert_eq!(capability["windows"], serde_json::json!(["stache-popover"]));
    assert_eq!(
        capability["permissions"],
        serde_json::json!([
            "allow-host-status",
            "allow-hide-popover",
            "allow-set-module-enabled",
            "allow-module-session-action",
            "allow-retry-module",
            "allow-open-accessibility-guide",
            "allow-config-files",
            "allow-config-document",
            "allow-config-edit",
            "allow-open-config-file",
            "allow-request-accessibility",
            "allow-open-accessibility-settings",
            "allow-quit-app",
            "core:event:allow-listen",
            "core:event:allow-unlisten"
        ])
    );
    for (name, text) in [
        (
            "access-window",
            include_str!("../capabilities/access-window.json"),
        ),
        ("bar-window", include_str!("../capabilities/bar.json")),
    ] {
        let capability: serde_json::Value = serde_json::from_str(text).expect("capability");
        let permissions = capability["permissions"].to_string();
        for command in [
            "hide-popover",
            "set-module-enabled",
            "module-session-action",
            "retry-module",
            "open-accessibility-guide",
            "config-files",
            "config-document",
            "config-edit",
            "open-config-file",
        ] {
            assert!(!permissions.contains(command), "{name} must not grant {command}");
        }
        assert!(
            !capability["windows"].to_string().contains("popover"),
            "{name} must not match the popover"
        );
    }
}

/// Location consent needs both halves: the usage string in `Info.plist` and,
/// because the bundle is signed with the hardened runtime, the location
/// entitlement (without it `locationd` never sends the prompt to
/// `CoreLocationAgent`). The entitlements file grants nothing else.
#[test]
fn the_bundle_declares_location_usage_and_only_the_location_entitlement() {
    let config: serde_json::Value =
        serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri config");
    assert_eq!(config["bundle"]["macOS"]["entitlements"], "Entitlements.plist");

    let entitlements = include_str!("../Entitlements.plist");
    let keys: Vec<&str> = entitlements
        .split("<key>")
        .skip(1)
        .filter_map(|rest| rest.split_once("</key>").map(|(key, _)| key.trim()))
        .collect();
    assert_eq!(keys, ["com.apple.security.personal-information.location"]);
    assert!(entitlements.contains("<true/>"), "{entitlements}");

    let info = include_str!("../Info.plist");
    assert!(
        info.contains("<key>NSLocationWhenInUseUsageDescription</key>"),
        "{info}"
    );
    assert!(!info.contains("LSUIElement"), "{info}");
}

// ---------------------------------------------------------------------------
// Each surface's UI command calls versus its capability's grants.
//
// Once the app manifest lists commands, Tauri refuses every app command that
// is missing from it, and every listed command a capability does not grant.
// The fake-host UI tests bypass the ACL, so this reads the UI sources instead.
// ---------------------------------------------------------------------------

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn ui_src() -> PathBuf { Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/src") }

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read dir") {
        let path = entry.expect("entry").path();
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
        if path.is_dir() {
            sources(&path, out);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "ts" || extension == "tsx")
            && !name.contains(".test.")
            && !name.contains(".spec.")
            && !name.ends_with(".gen.ts")
        {
            out.push(path);
        }
    }
}

fn identifier(text: &str) -> &str {
    let end = text
        .find(|char: char| !(char.is_ascii_alphanumeric() || char == '_' || char == '$'))
        .unwrap_or(text.len());
    &text[..end]
}

/// Every `commands.<name>` in `text`, as the Rust command name.
fn command_calls(text: &str) -> BTreeSet<String> {
    text.match_indices("commands.")
        .filter(|(index, _)| {
            *index == 0 || !text[..*index].ends_with(|char: char| char.is_ascii_alphanumeric())
        })
        .map(|(index, _)| identifier(&text[index + "commands.".len()..]))
        .filter(|name| !name.is_empty())
        .map(|name| {
            let mut snake = String::new();
            for char in name.chars() {
                if char.is_ascii_uppercase() {
                    snake.push('_');
                    snake.push(char.to_ascii_lowercase());
                } else {
                    snake.push(char);
                }
            }
            snake
        })
        .collect()
}

/// Whether `name` occurs in `text` as a whole identifier.
fn mentions(text: &str, name: &str) -> bool {
    let word = |char: char| char.is_ascii_alphanumeric() || char == '_' || char == '$';
    text.match_indices(name).any(|(index, _)| {
        !text[..index].ends_with(word) && !text[index + name.len()..].starts_with(word)
    })
}

/// Each `src/queries` module's exports and the commands each one reaches. Every
/// top-level declaration is a chunk: code between two declarations belongs to
/// the preceding one, code before the first to all of them (an
/// over-approximation). An export is credited with its own calls plus those of
/// every local declaration it references, transitively. `index` re-exports
/// (`export { a } from './module'`) resolve to the real module.
fn query_exports() -> BTreeMap<(String, String), BTreeSet<String>> {
    let mut files = Vec::new();
    sources(&ui_src().join("queries"), &mut files);
    let mut map = BTreeMap::new();
    let mut reexports: Vec<(String, String, String)> = Vec::new();
    for file in files {
        let module = file.file_stem().and_then(|stem| stem.to_str()).expect("stem").to_owned();
        let text = std::fs::read_to_string(&file).expect("read");
        let mut chunks: Vec<(String, bool, String)> = Vec::new();
        let mut preamble = String::new();
        for line in text.lines() {
            if let Some((names, from)) =
                line.strip_prefix("export {").and_then(|rest| rest.split_once("} from './"))
            {
                let target = &from[..from.find('\'').expect("module")];
                for name in names.split(',') {
                    let name = name.trim().split(" as ").next().unwrap_or_default().trim();
                    if !name.is_empty() {
                        reexports.push((module.clone(), name.to_owned(), target.to_owned()));
                    }
                }
            }
            let (exported, rest) =
                line.strip_prefix("export ").map_or((false, line), |rest| (true, rest));
            let rest = rest.trim_start_matches("async ");
            let name = ["function ", "const ", "class ", "let "]
                .iter()
                .find_map(|keyword| rest.strip_prefix(keyword))
                .map(identifier)
                .unwrap_or_default();
            if exported || !name.is_empty() {
                chunks.push((name.to_owned(), exported, String::new()));
            }
            match chunks.last_mut() {
                Some((_, _, body)) => body.push_str(line),
                None => preamble.push_str(line),
            }
            if let Some((_, _, body)) = chunks.last_mut() {
                body.push('\n');
            }
        }
        let shared = command_calls(&preamble);
        let mut reach: Vec<BTreeSet<String>> =
            chunks.iter().map(|(_, _, body)| command_calls(body)).collect();
        loop {
            let mut changed = false;
            for (index, (_, _, body)) in chunks.iter().enumerate() {
                for (other, (name, _, _)) in chunks.iter().enumerate() {
                    if other != index && !name.is_empty() && mentions(body, name) {
                        let extra: Vec<String> =
                            reach[other].difference(&reach[index]).cloned().collect();
                        changed |= !extra.is_empty();
                        reach[index].extend(extra);
                    }
                }
            }
            if !changed {
                break;
            }
        }
        for ((name, exported, _), mut calls) in chunks.into_iter().zip(reach) {
            if exported {
                calls.extend(shared.iter().cloned());
                map.insert((module.clone(), name), calls);
            }
        }
    }
    for (module, name, target) in reexports {
        let calls = map.get(&(target, name.clone())).cloned().unwrap_or_default();
        map.insert((module, name), calls);
    }
    map
}

/// The commands a surface's sources call, directly or through `@/queries/*`.
fn surface_calls(surface: &str) -> BTreeSet<String> {
    let exports = query_exports();
    let mut files = Vec::new();
    sources(&ui_src().join("windows").join(surface), &mut files);
    let mut calls = BTreeSet::new();
    for file in files {
        let text = std::fs::read_to_string(&file).expect("read");
        calls.extend(command_calls(&text));
        let mut rest = text.as_str();
        while let Some(start) = rest.find("import {") {
            let after = &rest[start + "import {".len()..];
            let close = after.find('}').expect("import list");
            let names = &after[..close];
            let tail = after[close..].trim_start_matches('}').trim_start();
            let module = if tail.starts_with("from '@/queries'") {
                Some("index")
            } else {
                tail.strip_prefix("from '@/queries/")
                    .map(|from| &from[..from.find('\'').expect("module")])
            };
            if let Some(module) = module {
                for name in names.split(',') {
                    let name = name.trim().trim_start_matches("type ").trim();
                    let name = name.split(" as ").next().unwrap_or_default().trim();
                    if name.is_empty() {
                        continue;
                    }
                    if let Some(reached) = exports.get(&(module.to_owned(), name.to_owned())) {
                        calls.extend(reached.iter().cloned());
                    }
                }
            }
            rest = &after[close..];
        }
    }
    calls
}

fn granted(capability: &str) -> BTreeSet<String> {
    let capability: serde_json::Value = serde_json::from_str(capability).expect("capability");
    capability["permissions"]
        .as_array()
        .expect("permissions")
        .iter()
        .filter_map(|permission| permission.as_str()?.strip_prefix("allow-"))
        .map(|permission| permission.replace('-', "_"))
        .collect()
}

#[test]
fn every_surface_capability_grants_each_command_its_ui_calls() {
    for (surface, capability) in [
        ("Popover", include_str!("../capabilities/popover-window.json")),
        ("Bar", include_str!("../capabilities/bar.json")),
        (
            "AccessibilityGuide",
            include_str!("../capabilities/access-window.json"),
        ),
    ] {
        let calls = surface_calls(surface);
        assert!(
            !calls.is_empty(),
            "{surface} calls no command; the scan is broken"
        );
        let granted = granted(capability);
        let missing: Vec<&String> = calls.difference(&granted).collect();
        assert!(
            missing.is_empty(),
            "{surface} calls ungranted commands: {missing:?}"
        );
    }
    assert_eq!(surface_calls("UnknownSurface"), BTreeSet::new());
}

#[test]
fn the_scan_sees_the_popovers_reads_through_the_queries() {
    let calls = surface_calls("Popover");
    for command in [
        "host_status",
        "config_files",
        "config_document",
        "config_edit",
    ] {
        assert!(calls.contains(command), "{command}: {calls:?}");
    }
    assert!(!surface_calls("Bar").contains("host_status"));
}

#[test]
fn the_scan_follows_hooks_to_the_commands_they_reach() {
    for (surface, commands) in [
        (
            "Popover",
            &[
                "open_config_file",
                "hide_popover",
                "quit_app",
                "open_accessibility_guide",
                "request_accessibility",
                "open_accessibility_settings",
            ][..],
        ),
        (
            "AccessibilityGuide",
            &[
                "quit_app",
                "request_accessibility",
                "open_accessibility_settings",
            ][..],
        ),
        (
            "Bar",
            &["bar_action", "bar_session_action", "bar_tiling_action"][..],
        ),
    ] {
        let calls = surface_calls(surface);
        for command in commands {
            assert!(calls.contains(*command), "{surface} {command}: {calls:?}");
        }
    }
}

#[test]
fn the_app_manifest_lists_every_exported_command() {
    let build = include_str!("../build.rs");
    let list = &build[build.find(".commands(&[").expect("manifest")..];
    let list = &list[..list.find("])").expect("end")];
    let manifest: BTreeSet<&str> = list.split('"').skip(1).step_by(2).collect();
    let bindings = include_str!("../src/bindings.rs");
    let block = &bindings[bindings.find("collect_commands![").expect("commands")..];
    let block = &block["collect_commands![".len()..block.find(']').expect("end")];
    let exported: BTreeSet<&str> = block
        .split(',')
        .map(|entry| entry.trim().rsplit("::").next().unwrap_or_default())
        .filter(|name| !name.is_empty())
        .collect();
    assert_eq!(manifest, exported);
}
