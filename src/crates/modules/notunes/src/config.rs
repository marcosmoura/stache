//! Authoritative noTunes configuration and replacement target.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use specta::Type;
use stache_sdk::{ConfigError, ModuleConfig, ModuleId};

/// The accepted `target_app` forms, appended to every parse failure.
const ACCEPTED_FORMS: &str =
    "expected an absolute .app path or a bundle identifier such as com.example.player";

/// Where a blocked application's replacement is opened from.
///
/// The value is user data: the module never falls back to a built-in player.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplacementTarget {
    /// An absolute path to an application bundle.
    Path(PathBuf),
    /// A bundle identifier resolved through the native workspace.
    BundleId(String),
}

impl ReplacementTarget {
    /// Parses one configured `target_app` value.
    ///
    /// A value containing a path separator or ending in `.app` is a path and
    /// must be absolute; anything else must be a bundle identifier.
    ///
    /// # Errors
    /// Returns a user-facing reason for an empty value, a relative path or a
    /// malformed bundle identifier.
    pub fn parse(value: &str) -> Result<Self, String> {
        let value = value.trim();
        if value.is_empty() {
            return Err(ACCEPTED_FORMS.to_owned());
        }
        if value.contains('/') || ends_with_app(value) {
            return if value.starts_with('/') {
                Ok(Self::Path(PathBuf::from(value)))
            } else {
                Err(format!("`{value}` is not an absolute path; {ACCEPTED_FORMS}"))
            };
        }
        if is_bundle_identifier(value) {
            Ok(Self::BundleId(value.to_owned()))
        } else {
            Err(format!("`{value}` is not a bundle identifier; {ACCEPTED_FORMS}"))
        }
    }
}

/// Automatic Apple Music and iTunes blocking.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(default, rename_all = "snake_case")]
pub struct NoTunesConfig {
    /// Whether the blocker should run.
    pub enabled: bool,
    /// The replacement opened after a blocked application: an absolute `.app`
    /// path or a bundle identifier. Omitting it blocks without launching
    /// anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_app: Option<String>,
}

impl NoTunesConfig {
    /// The catalog identifier and file stem.
    pub const ID: ModuleId = ModuleId::new("notunes");

    /// The configured replacement, or `None` for block-only.
    ///
    /// # Errors
    /// Returns the parse reason for an invalid `target_app` value.
    pub fn target(&self) -> Result<Option<ReplacementTarget>, String> {
        self.target_app.as_deref().map(ReplacementTarget::parse).transpose()
    }
}

impl ModuleConfig for NoTunesConfig {
    fn enabled(&self) -> bool { self.enabled }

    fn validate(&self) -> Result<(), ConfigError> {
        self.target()
            .map(|_| ())
            .map_err(|error| ConfigError::new(format!("target_app is invalid: {error}")))
    }

    /// Translates the legacy preset spellings that `config migrate` may meet.
    ///
    /// The running configuration accepts only a path or a bundle identifier, so
    /// the preset names live here and nowhere else. A legacy section that was
    /// enabled and relied on the old implicit Spotify default now blocks without
    /// launching anything, which the returned note makes visible.
    fn migrate_legacy_section(section: &mut serde_json::Value) -> Vec<String> {
        let Some(object) = section.as_object_mut() else {
            return Vec::new();
        };
        let Some(spelling) =
            object.get("targetApp").and_then(serde_json::Value::as_str).map(str::to_owned)
        else {
            let relied_on_default = !object.contains_key("targetApp")
                && object.get("enabled").and_then(serde_json::Value::as_bool) == Some(true);
            return if relied_on_default {
                vec![
                    "notunes had no target_app; the migrated file blocks without launching a replacement"
                        .to_owned(),
                ]
            } else {
                Vec::new()
            };
        };
        let bundle_id = match spelling.as_str() {
            "tidal" => "com.tidal.desktop",
            "spotify" => "com.spotify.client",
            "feishin" => "org.jeffvli.feishin",
            "none" => {
                object.remove("targetApp");
                return Vec::new();
            }
            _ => return Vec::new(),
        };
        object.insert(
            "targetApp".to_owned(),
            serde_json::Value::String(bundle_id.to_owned()),
        );
        Vec::new()
    }
}

/// Whether a value ends in `.app`, ignoring ASCII case.
fn ends_with_app(value: &str) -> bool {
    value
        .get(value.len().saturating_sub(4)..)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".app"))
}

/// Whether a value is a plausible bundle identifier.
fn is_bundle_identifier(value: &str) -> bool {
    value.contains('.')
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use stache_sdk::ConfigCatalog;

    use super::*;

    #[test]
    fn defaults_are_disabled_and_block_only() {
        let config = NoTunesConfig::default();
        assert!(!config.enabled());
        assert_eq!(config.target_app, None);
        assert_eq!(config.target(), Ok(None));
    }

    #[test]
    fn an_absent_target_serializes_only_the_enabled_flag() {
        assert_eq!(
            serde_json::to_value(NoTunesConfig::default()).expect("encode"),
            json!({ "enabled": false })
        );
    }

    #[test]
    fn empty_objects_fall_back_to_field_defaults() {
        let config: NoTunesConfig = serde_json::from_str("{}").expect("decode");
        assert_eq!(config, NoTunesConfig::default());
    }

    #[test]
    fn migration_translates_only_the_legacy_preset_names() {
        for (spelling, bundle_id) in [
            ("tidal", "com.tidal.desktop"),
            ("spotify", "com.spotify.client"),
            ("feishin", "org.jeffvli.feishin"),
        ] {
            let mut section = json!({ "enabled": true, "targetApp": spelling });
            assert!(NoTunesConfig::migrate_legacy_section(&mut section).is_empty());
            assert_eq!(section, json!({ "enabled": true, "targetApp": bundle_id }));
        }

        let mut block_only = json!({ "enabled": true, "targetApp": "none" });
        assert!(NoTunesConfig::migrate_legacy_section(&mut block_only).is_empty());
        assert_eq!(block_only, json!({ "enabled": true }));

        for untouched in [
            json!({ "enabled": true }),
            json!({ "targetApp": "/Applications/Spotify.app" }),
            json!({ "targetApp": "com.example.player" }),
        ] {
            let mut section = untouched.clone();
            let notes = NoTunesConfig::migrate_legacy_section(&mut section);
            assert_eq!(section, untouched);
            let relied_on_default = untouched.get("enabled") == Some(&json!(true))
                && untouched.get("targetApp").is_none();
            assert_eq!(!notes.is_empty(), relied_on_default, "{untouched}: {notes:?}");
        }

        let mut disabled = json!({ "enabled": false });
        assert!(NoTunesConfig::migrate_legacy_section(&mut disabled).is_empty());
    }

    #[test]
    fn a_path_and_a_bundle_identifier_classify() {
        assert_eq!(
            ReplacementTarget::parse("/Applications/My Player.app"),
            Ok(ReplacementTarget::Path(PathBuf::from(
                "/Applications/My Player.app"
            )))
        );
        assert_eq!(
            ReplacementTarget::parse("  com.example.player  "),
            Ok(ReplacementTarget::BundleId("com.example.player".to_owned()))
        );
        assert_eq!(
            ReplacementTarget::parse("/Applications/Player.app2"),
            Ok(ReplacementTarget::Path(PathBuf::from(
                "/Applications/Player.app2"
            )))
        );
    }

    #[test]
    fn a_relative_path_is_rejected() {
        for value in ["Applications/Player.app", "Player.app"] {
            let error = ReplacementTarget::parse(value).expect_err("relative path");
            assert!(error.contains("absolute"), "{error}");
        }
    }

    #[test]
    fn empty_and_malformed_values_are_rejected() {
        for value in [
            "",
            "   ",
            "player",
            "com.example player",
            "com..example/",
            "Playér",
        ] {
            let error = ReplacementTarget::parse(value).expect_err("invalid target");
            assert!(error.contains("expected an absolute .app path"), "{error}");
        }
    }

    #[test]
    fn legacy_preset_spellings_are_rejected() {
        for value in ["spotify", "tidal", "feishin", "none"] {
            let error = ReplacementTarget::parse(value).expect_err("legacy spelling");
            assert!(error.contains("not a bundle identifier"), "{error}");
        }
        for value in ["spotify", "tidal", "feishin", "none"] {
            let config = NoTunesConfig {
                enabled: true,
                target_app: Some(value.to_owned()),
            };
            let error = config.validate().expect_err("legacy spelling");
            assert!(error.message().contains("target_app"), "{error}");
        }
    }

    #[test]
    fn the_camel_case_fields_roundtrip() {
        let config = NoTunesConfig {
            enabled: true,
            target_app: Some("/Applications/My Player.app".to_owned()),
        };
        let encoded = serde_json::to_value(&config).expect("encode");
        assert_eq!(
            encoded,
            json!({ "enabled": true, "target_app": "/Applications/My Player.app" })
        );
        let decoded: NoTunesConfig = serde_json::from_value(encoded).expect("decode");
        assert_eq!(decoded, config);
    }

    #[test]
    fn unknown_keys_are_warnings_in_the_catalog() {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<NoTunesConfig>(NoTunesConfig::ID).expect("register");
        let warnings = catalog
            .inspect(
                NoTunesConfig::ID,
                std::path::Path::new("notunes.toml"),
                "enabled = true\ntarget_app = \"com.example.player\"\ntarget = \"tidal\"\n",
            )
            .expect("the file stays valid");
        assert_eq!(warnings, ["target"]);
    }

    #[test]
    fn the_template_contains_only_the_enabled_flag() {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<NoTunesConfig>(NoTunesConfig::ID).expect("register");
        let template = catalog.template(NoTunesConfig::ID).expect("template");
        assert!(template.contains("\nenabled = false\n"), "{template}");
        assert!(!template.contains("target_app"), "{template}");
    }

    #[test]
    fn an_invalid_target_is_a_catalog_validation_diagnostic() {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<NoTunesConfig>(NoTunesConfig::ID).expect("register");
        let diagnostic = catalog
            .inspect(
                NoTunesConfig::ID,
                std::path::Path::new("notunes.toml"),
                "target_app = \"spotify\"\n",
            )
            .expect_err("legacy spelling");
        assert_eq!(diagnostic.kind, stache_sdk::DiagnosticKind::Validation);
        assert!(diagnostic.message.contains("target_app"), "{diagnostic}");
    }
}
