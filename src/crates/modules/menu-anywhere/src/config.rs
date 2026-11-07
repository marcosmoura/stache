//! Authoritative `MenuAnywhere` configuration and the pure modifier mask.

use std::fmt;

use schemars::JsonSchema;
use serde::de::value::MapAccessDeserializer;
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use specta::Type;
use stache_sdk::{ConfigError, ModuleConfig, ModuleId};

/// A keyboard modifier that must be held while clicking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "camelCase")]
pub enum MenuAnywhereModifier {
    /// The Control key.
    Control,
    /// The Option (Alt) key.
    Option,
    /// The Command key.
    Command,
    /// The Shift key.
    Shift,
}

/// The mouse button that summons the menu.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Type,
)]
#[serde(rename_all = "camelCase")]
pub enum MenuAnywhereMouseButton {
    /// The secondary (right) button.
    #[default]
    RightClick,
    /// The middle button, button number 2 only. Other extra buttons such as
    /// back and forward never match.
    MiddleClick,
}

/// A normalized set of the four supported modifiers.
///
/// Bit 0 is Control, bit 1 Option, bit 2 Command and bit 3 Shift. These are
/// private Stache bits, not native event flags: a backend converts its own
/// flags into a mask with [`ModifierMask::from_bits_truncate`], which drops
/// every unrelated flag such as Caps Lock. Duplicates in a configured list OR
/// to one bit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ModifierMask(u8);

impl ModifierMask {
    const ALL: u8 = 0b1111;
    /// Command.
    pub const COMMAND: Self = Self(0b0100);
    /// Control.
    pub const CONTROL: Self = Self(0b0001);
    /// No modifiers.
    pub const NONE: Self = Self(0);
    /// Option.
    pub const OPTION: Self = Self(0b0010);
    /// Shift.
    pub const SHIFT: Self = Self(0b1000);

    /// Builds a mask from raw bits, dropping every bit outside the four
    /// supported modifiers.
    #[must_use]
    pub const fn from_bits_truncate(bits: u8) -> Self { Self(bits & Self::ALL) }

    /// The normalized bits; always within `0..=0b1111`.
    #[must_use]
    pub const fn bits(self) -> u8 { self.0 }

    /// The mask of one modifier.
    #[must_use]
    pub const fn from_modifier(modifier: MenuAnywhereModifier) -> Self {
        match modifier {
            MenuAnywhereModifier::Control => Self::CONTROL,
            MenuAnywhereModifier::Option => Self::OPTION,
            MenuAnywhereModifier::Command => Self::COMMAND,
            MenuAnywhereModifier::Shift => Self::SHIFT,
        }
    }

    /// The union of a configured list; duplicates collapse, `[]` is empty.
    #[must_use]
    pub fn from_modifiers(modifiers: &[MenuAnywhereModifier]) -> Self {
        Self(
            modifiers
                .iter()
                .fold(0, |bits, modifier| bits | Self::from_modifier(*modifier).0),
        )
    }

    /// Whether the held modifiers equal this required set exactly.
    ///
    /// Every constructor normalizes, so unrelated flags never matter, and a
    /// held superset or subset never matches. The empty mask matches only a
    /// click with no supported modifier held.
    #[must_use]
    pub const fn matches(self, held: Self) -> bool { self.0 == held.0 }
}

/// Summon the frontmost application's menu bar at the cursor.
///
/// This configuration is inert: no runtime consumes it yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema, Type)]
#[serde(default, rename_all = "camelCase")]
pub struct MenuAnywhereConfig {
    /// Whether the feature should run.
    pub enabled: bool,
    /// Modifiers that must be held exactly while clicking: any of "control",
    /// "option", "command" and "shift". Duplicates count once.
    ///
    /// CAUTION: `[]` is valid and captures every unmodified matching click
    /// system-wide while enabled.
    pub modifiers: Vec<MenuAnywhereModifier>,
    /// The mouse button that summons the menu: "rightClick" or "middleClick".
    /// "middleClick" means button number 2 only, not back or forward buttons.
    pub mouse_button: MenuAnywhereMouseButton,
}

/// Field-for-field mirror used so decoding accepts only a map root.
///
/// A derived struct decoder also accepts an empty sequence, which would let
/// `[]` pass as the defaults.
#[derive(Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Fields {
    enabled: bool,
    modifiers: Vec<MenuAnywhereModifier>,
    mouse_button: MenuAnywhereMouseButton,
}

impl Default for Fields {
    fn default() -> Self {
        let defaults = MenuAnywhereConfig::default();
        Self {
            enabled: defaults.enabled,
            modifiers: defaults.modifiers,
            mouse_button: defaults.mouse_button,
        }
    }
}

impl<'de> Deserialize<'de> for MenuAnywhereConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RootVisitor;

        impl<'de> Visitor<'de> for RootVisitor {
            type Value = MenuAnywhereConfig;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a MenuAnywhere configuration object")
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                let fields = Fields::deserialize(MapAccessDeserializer::new(map))?;
                Ok(MenuAnywhereConfig {
                    enabled: fields.enabled,
                    modifiers: fields.modifiers,
                    mouse_button: fields.mouse_button,
                })
            }
        }

        deserializer.deserialize_map(RootVisitor)
    }
}

impl Default for MenuAnywhereConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            modifiers: vec![MenuAnywhereModifier::Control, MenuAnywhereModifier::Command],
            mouse_button: MenuAnywhereMouseButton::RightClick,
        }
    }
}

impl MenuAnywhereConfig {
    /// The catalog identifier and file stem.
    pub const ID: ModuleId = ModuleId::new("menuAnywhere");

    /// The normalized required modifier set.
    #[must_use]
    pub fn required_modifiers(&self) -> ModifierMask {
        ModifierMask::from_modifiers(&self.modifiers)
    }
}

impl ModuleConfig for MenuAnywhereConfig {
    fn enabled(&self) -> bool { self.enabled }

    fn validate(&self) -> Result<(), ConfigError> { Ok(()) }
}

#[cfg(test)]
mod tests {
    use MenuAnywhereModifier::{Command, Control, Option, Shift};
    use serde_json::json;
    use stache_sdk::{ConfigCatalog, DiagnosticKind};

    use super::*;

    const ORDER: [MenuAnywhereModifier; 4] = [Control, Option, Command, Shift];

    fn catalog() -> ConfigCatalog {
        let mut catalog = ConfigCatalog::new();
        catalog
            .register::<MenuAnywhereConfig>(MenuAnywhereConfig::ID)
            .expect("register");
        catalog
    }

    fn inspect(text: &str) -> Result<Vec<String>, stache_sdk::config::ConfigDiagnostic> {
        catalog().inspect(
            MenuAnywhereConfig::ID,
            std::path::Path::new("menuAnywhere.jsonc"),
            text,
        )
    }

    #[test]
    fn defaults_are_disabled_control_command_right_click() {
        let config = MenuAnywhereConfig::default();
        assert!(!config.enabled());
        assert_eq!(config.modifiers, [Control, Command]);
        assert_eq!(config.mouse_button, MenuAnywhereMouseButton::RightClick);
        assert_eq!(MenuAnywhereConfig::ID.as_str(), "menuAnywhere");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn empty_objects_fall_back_to_field_defaults() {
        let config: MenuAnywhereConfig = serde_json::from_str("{}").expect("decode");
        assert_eq!(config, MenuAnywhereConfig::default());
        let partial: MenuAnywhereConfig =
            serde_json::from_str(r#"{ "enabled": true }"#).expect("decode");
        assert_eq!(partial.modifiers, [Control, Command]);
    }

    #[test]
    fn the_camel_case_fields_roundtrip() {
        let config = MenuAnywhereConfig {
            enabled: true,
            modifiers: vec![Shift, Option],
            mouse_button: MenuAnywhereMouseButton::MiddleClick,
        };
        let encoded = serde_json::to_value(&config).expect("encode");
        assert_eq!(
            encoded,
            json!({ "enabled": true, "modifiers": ["shift", "option"], "mouseButton": "middleClick" })
        );
        let decoded: MenuAnywhereConfig = serde_json::from_value(encoded).expect("decode");
        assert_eq!(decoded, config);
        assert_eq!(
            serde_json::to_value(MenuAnywhereConfig::default()).expect("encode"),
            json!({ "enabled": false, "modifiers": ["control", "command"], "mouseButton": "rightClick" })
        );
    }

    #[test]
    fn every_enum_spelling_decodes() {
        for (spelling, expected) in [
            ("control", Control),
            ("option", Option),
            ("command", Command),
            ("shift", Shift),
        ] {
            let config: MenuAnywhereConfig =
                serde_json::from_value(json!({ "modifiers": [spelling] })).expect("decode");
            assert_eq!(config.modifiers, [expected]);
        }
        for (spelling, expected) in [
            ("rightClick", MenuAnywhereMouseButton::RightClick),
            ("middleClick", MenuAnywhereMouseButton::MiddleClick),
        ] {
            let config: MenuAnywhereConfig =
                serde_json::from_value(json!({ "mouseButton": spelling })).expect("decode");
            assert_eq!(config.mouse_button, expected);
        }
    }

    #[test]
    fn empty_and_duplicate_modifiers_are_valid_and_preserved() {
        let empty: MenuAnywhereConfig =
            serde_json::from_value(json!({ "modifiers": [] })).expect("decode");
        assert!(empty.modifiers.is_empty());
        assert_eq!(empty.required_modifiers(), ModifierMask::NONE);
        assert_eq!(
            serde_json::to_value(&empty).expect("encode")["modifiers"],
            json!([])
        );

        let duplicated: MenuAnywhereConfig =
            serde_json::from_value(json!({ "modifiers": ["command", "command", "control"] }))
                .expect("decode");
        assert_eq!(duplicated.modifiers, [Command, Command, Control]);
        assert_eq!(
            serde_json::to_value(&duplicated).expect("encode")["modifiers"],
            json!(["command", "command", "control"])
        );
        assert_eq!(
            duplicated.required_modifiers(),
            ModifierMask::from_bits_truncate(0b0101)
        );
        assert!(inspect(r#"{ "modifiers": [] }"#).is_ok());
        assert!(inspect(r#"{ "modifiers": ["shift", "shift"] }"#).is_ok());
    }

    #[test]
    fn invalid_types_and_enums_fail_with_field_paths() {
        for text in [
            r#"{ "modifiers": ["hyper"] }"#,
            r#"{ "modifiers": "command" }"#,
            r#"{ "modifiers": null }"#,
            r#"{ "mouseButton": "leftClick" }"#,
            r#"{ "mouseButton": 2 }"#,
            r#"{ "mouseButton": null }"#,
            r#"{ "enabled": "yes" }"#,
            "[]",
            "null",
            "true",
            r#""command""#,
            "[true]",
        ] {
            let diagnostic = inspect(text).expect_err(text);
            assert_eq!(diagnostic.kind, DiagnosticKind::Type, "{text}");
        }
        for (text, field) in [
            (r#"{ "modifiers": ["hyper"] }"#, "modifiers"),
            (r#"{ "mouseButton": "leftClick" }"#, "mouseButton"),
            (r#"{ "enabled": "yes" }"#, "enabled"),
            (r#"{ "modifiers": [1] }"#, "modifiers"),
        ] {
            let diagnostic = inspect(text).expect_err(text);
            assert!(diagnostic.to_string().contains(field), "{text}: {diagnostic}");
        }
    }

    #[test]
    fn unknown_keys_are_warnings_in_the_catalog() {
        let warnings = inspect(r#"{ "enabled": true, "preset": "wide" }"#).expect("valid");
        assert_eq!(warnings, ["preset"]);
    }

    #[test]
    fn every_subset_maps_to_its_own_four_bit_mask() {
        for bits in 0_u8..16 {
            let subset: Vec<_> = ORDER
                .iter()
                .enumerate()
                .filter(|(index, _)| bits & (1 << index) != 0)
                .map(|(_, modifier)| *modifier)
                .collect();
            let mask = ModifierMask::from_modifiers(&subset);
            assert_eq!(mask.bits(), bits, "{subset:?}");
            let reversed: Vec<_> = subset.iter().rev().copied().collect();
            assert_eq!(
                ModifierMask::from_modifiers(&reversed),
                mask,
                "order is irrelevant"
            );
            let doubled: Vec<_> = subset.iter().chain(subset.iter()).copied().collect();
            assert_eq!(
                ModifierMask::from_modifiers(&doubled),
                mask,
                "duplicates OR to one bit"
            );
        }
    }

    #[test]
    fn matching_is_exact_equality_of_the_four_bits() {
        for required in 0_u8..16 {
            for held in 0_u8..16 {
                assert_eq!(
                    ModifierMask::from_bits_truncate(required)
                        .matches(ModifierMask::from_bits_truncate(held)),
                    required == held,
                    "required {required:#06b} held {held:#06b}"
                );
            }
        }
    }

    #[test]
    fn unrelated_flags_are_ignored_and_never_added() {
        for required in 0_u8..16 {
            for extra in [0x10_u8, 0x20, 0x80, 0xF0] {
                let held = ModifierMask::from_bits_truncate(required | extra);
                assert_eq!(held.bits(), required);
                assert!(ModifierMask::from_bits_truncate(required).matches(held));
            }
        }
        assert_eq!(ModifierMask::from_bits_truncate(0xF0), ModifierMask::NONE);
        // Extra unrelated flags still fail a missing or surplus supported bit.
        assert!(!ModifierMask::CONTROL.matches(ModifierMask::from_bits_truncate(0x10)));
        assert!(!ModifierMask::NONE.matches(ModifierMask::from_bits_truncate(0b0001 | 0x10)));
    }

    #[test]
    fn the_default_requires_exactly_control_and_command() {
        let required = MenuAnywhereConfig::default().required_modifiers();
        assert_eq!(required.bits(), 0b0101);
        assert!(required.matches(ModifierMask::from_bits_truncate(0b0101 | 0x10)));
        assert!(!required.matches(ModifierMask::CONTROL));
        assert!(!required.matches(ModifierMask::from_bits_truncate(0b1101)));
    }

    #[test]
    fn the_empty_mask_matches_only_an_unmodified_click() {
        assert!(ModifierMask::NONE.matches(ModifierMask::NONE));
        assert!(!ModifierMask::NONE.matches(ModifierMask::SHIFT));
    }

    #[test]
    fn the_schema_and_template_carry_the_empty_modifiers_caution() {
        let catalog = catalog();
        let schema = catalog.schema_json(MenuAnywhereConfig::ID).expect("schema").expect("render");
        let value: serde_json::Value = serde_json::from_str(&schema).expect("schema JSON");
        let description = value["properties"]["modifiers"]["description"].as_str().expect("text");
        assert!(
            description.contains("`[]`") && description.contains("system-wide"),
            "{description}"
        );
        let button = value["properties"]["mouseButton"]["description"].as_str().expect("text");
        assert!(button.contains("button number 2 only"), "{button}");

        let template = catalog.template(MenuAnywhereConfig::ID).expect("template");
        assert!(template.contains("system-wide"), "{template}");
        assert!(template.contains("\"enabled\": false"), "{template}");
        assert!(template.contains("\"control\""), "{template}");
        assert!(template.contains("\"rightClick\""), "{template}");
    }
}
