//! Static release metadata: one version, honest `Info.plist`, referenced icons and the
//! unchanged packaging surface. Never builds a bundle.

use std::path::Path;

const CARGO_TOML: &str = include_str!("../../../../Cargo.toml");
const PACKAGE_JSON: &str = include_str!("../../../../package.json");
const TAURI_CONF: &str = include_str!("../tauri.conf.json");

/// Compares the three manifests against the authoritative `[workspace.package] version`.
fn check_versions(
    cargo_toml: &str,
    package_json: &str,
    tauri_conf: &str,
) -> Result<String, String> {
    let cargo: toml::Table = cargo_toml.parse().map_err(|error| format!("Cargo.toml: {error}"))?;
    let expected = cargo["workspace"]["package"]["version"]
        .as_str()
        .ok_or("Cargo.toml: missing [workspace.package] version")?
        .to_owned();
    for (file, source) in [
        ("package.json", package_json),
        ("tauri.conf.json", tauri_conf),
    ] {
        let json: serde_json::Value =
            serde_json::from_str(source).map_err(|error| format!("{file}: {error}"))?;
        let actual = json["version"].as_str().unwrap_or("<missing>");
        if actual != expected {
            return Err(format!(
                "{file} declares version {actual}, but Cargo.toml declares {expected}"
            ));
        }
    }
    Ok(expected)
}

fn tauri_conf() -> serde_json::Value { serde_json::from_str(TAURI_CONF).expect("tauri config") }

fn plist_dict(source: &str) -> plist::Dictionary {
    plist::from_bytes(source.as_bytes()).expect("plist dictionary")
}

#[test]
fn the_three_manifests_declare_one_version() {
    assert_eq!(
        check_versions(CARGO_TOML, PACKAGE_JSON, TAURI_CONF),
        Ok("0.26.0".to_owned())
    );
}

#[test]
fn a_version_mismatch_names_the_file_and_both_values() {
    let cargo = "[workspace.package]\nversion = \"1.2.3\"\n";
    let good = r#"{"version": "1.2.3"}"#;
    let bad = r#"{"version": "1.2.4"}"#;
    assert_eq!(check_versions(cargo, good, good), Ok("1.2.3".to_owned()));
    let package = check_versions(cargo, bad, good).expect_err("package.json mismatch");
    assert!(package.contains("package.json"), "{package}");
    assert!(
        package.contains("1.2.4") && package.contains("1.2.3"),
        "{package}"
    );
    let tauri = check_versions(cargo, good, bad).expect_err("tauri.conf.json mismatch");
    assert!(tauri.contains("tauri.conf.json"), "{tauri}");
    assert!(tauri.contains("1.2.4") && tauri.contains("1.2.3"), "{tauri}");
}

#[test]
fn info_plist_holds_exactly_the_release_keys() {
    let info = plist_dict(include_str!("../Info.plist"));
    let mut keys: Vec<&str> = info.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, [
        "LSApplicationCategoryType",
        "LSUIElement",
        "NSHighResolutionCapable",
        "NSLocationTemporaryUsageDescriptionDictionary",
        "NSLocationWhenInUseUsageDescription",
    ]);
    assert_eq!(info["LSUIElement"].as_boolean(), Some(true));
    assert_eq!(info["NSHighResolutionCapable"].as_boolean(), Some(true));
    assert_eq!(
        info["LSApplicationCategoryType"].as_string(),
        Some("public.app-category.utilities")
    );
    assert_eq!(
        info["NSLocationWhenInUseUsageDescription"].as_string(),
        Some("Stache uses your location to show local weather in the bar.")
    );
    let temporary = info["NSLocationTemporaryUsageDescriptionDictionary"]
        .as_dictionary()
        .expect("temporary usage dictionary");
    assert_eq!(temporary.len(), 1);
    assert_eq!(
        temporary["StacheWeatherPreciseLocation"].as_string(),
        Some("Stache uses your precise location to show local weather in the bar.")
    );
}

#[test]
fn every_icon_is_referenced_and_present() {
    let app = Path::new(env!("CARGO_MANIFEST_DIR"));
    let config = tauri_conf();
    let referenced: Vec<&str> = config["bundle"]["icon"]
        .as_array()
        .expect("bundle.icon")
        .iter()
        .map(|icon| icon.as_str().expect("icon path"))
        .collect();
    for icon in &referenced {
        assert!(app.join(icon).is_file(), "bundle.icon entry {icon} is missing");
    }
    assert!(app.join("icons/icon.png").is_file(), "icons/icon.png is missing");
    for entry in std::fs::read_dir(app.join("icons")).expect("icons directory") {
        let name = entry.expect("icon entry").file_name().into_string().expect("utf-8 name");
        let path = format!("icons/{name}");
        assert!(
            name == "icon.png" || referenced.contains(&path.as_str()),
            "{path} is not referenced by bundle.icon"
        );
    }
}

#[test]
fn the_packaging_surface_is_unchanged() {
    let config = tauri_conf();
    assert_eq!(
        config["app"]["security"]["capabilities"],
        serde_json::json!(["access-window", "bar-window", "popover-window"])
    );
    assert_eq!(config["bundle"]["targets"], serde_json::json!(["app"]));
    assert_eq!(
        config["bundle"]["resources"],
        serde_json::json!({"resources/media-control/": "media-control/"})
    );
    let entitlements = plist_dict(include_str!("../Entitlements.plist"));
    assert_eq!(entitlements.len(), 1);
    assert_eq!(
        entitlements["com.apple.security.personal-information.location"].as_boolean(),
        Some(true)
    );
}
