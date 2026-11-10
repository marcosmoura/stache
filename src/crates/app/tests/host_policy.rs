//! Static least-privilege and identity checks for the desktop manifest.

#[test]
fn the_test_window_has_only_the_named_least_privilege_permissions() {
    let capability: serde_json::Value =
        serde_json::from_str(include_str!("../capabilities/test-window.json")).expect("capability");
    assert_eq!(capability["windows"], serde_json::json!(["stache-test-*"]));
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
    ]);
    let joined = permissions.join(" ");
    for forbidden in [
        "core:default",
        "fs:",
        "shell:",
        "http:",
        "dialog:",
        "media",
        "weather",
        "system",
        "tray",
    ] {
        assert!(!joined.contains(forbidden), "capability grants {forbidden}");
    }
}

#[test]
fn the_desktop_manifest_has_no_fixed_window_and_an_explicit_capability() {
    let config: serde_json::Value =
        serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri config");
    assert_eq!(config["identifier"], stache_sdk::IDENTITY);
    assert_eq!(config["app"]["windows"], serde_json::json!([]));
    assert_eq!(
        config["app"]["security"]["capabilities"],
        serde_json::json!(["test-window", "access-window", "bar-window"])
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
fn the_test_window_does_not_gain_the_guidance_commands() {
    let capability: serde_json::Value =
        serde_json::from_str(include_str!("../capabilities/test-window.json")).expect("capability");
    let joined = capability["permissions"].to_string();
    assert!(!joined.contains("start-dragging"), "test window grants dragging");
    for command in [
        "request-accessibility",
        "open-accessibility-settings",
        "quit-app",
    ] {
        assert!(!joined.contains(command), "test window grants {command}");
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
    // `bar_snapshot`, `bar_action`, `bar_weather`, `bar_media_artwork` and `bar_session_action` are ACL-gated to this
    // capability (and also check the calling window label themselves); the event listener
    // pair is the only rest.
    assert_eq!(permissions, [
        "allow-bar-snapshot",
        "allow-bar-action",
        "allow-bar-weather",
        "allow-bar-media-artwork",
        "allow-bar-session-action",
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
        ("test-window", include_str!("../capabilities/test-window.json")),
        (
            "access-window",
            include_str!("../capabilities/access-window.json"),
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
