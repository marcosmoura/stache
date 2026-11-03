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
        serde_json::json!(["test-window"])
    );
    let csp = config["app"]["security"]["csp"].as_str().expect("csp");
    assert!(csp.contains("object-src 'none'"), "{csp}");
    assert!(csp.contains("base-uri 'none'"), "{csp}");
    assert!(csp.contains("http://ipc.localhost"), "{csp}");
    assert!(!csp.contains("unsafe-eval"), "{csp}");
    assert!(!csp.contains("https:"), "{csp}");
}
