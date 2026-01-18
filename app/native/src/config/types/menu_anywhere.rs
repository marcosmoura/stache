//! `MenuAnywhere` configuration types.
//!
//! Configuration for summoning app menus at cursor position.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Mouse button options for `MenuAnywhere` trigger.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MenuAnywhereMouseButton {
    /// Trigger on right mouse button click.
    #[default]
    RightClick,
    /// Trigger on middle mouse button click.
    MiddleClick,
}

/// Keyboard modifier options for `MenuAnywhere` trigger.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MenuAnywhereModifier {
    /// Control key (^).
    Control,
    /// Option/Alt key (⌥).
    Option,
    /// Command key (⌘).
    Command,
    /// Shift key (⇧).
    Shift,
}

/// Configuration for the `MenuAnywhere` feature.
///
/// `MenuAnywhere` allows you to summon the current application's menu bar
/// at any location on screen using a configurable keyboard + mouse trigger.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct MenuAnywhereConfig {
    /// Whether `MenuAnywhere` is enabled.
    /// Default: false
    pub enabled: bool,

    /// Keyboard modifiers that must be held when clicking.
    /// Default: `["control", "command"]`
    pub modifiers: Vec<MenuAnywhereModifier>,

    /// Mouse button that triggers the menu.
    /// Default: `"rightClick"`
    pub mouse_button: MenuAnywhereMouseButton,
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
    /// Returns whether `MenuAnywhere` functionality is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool { self.enabled }

    /// Returns the required modifier flags as a bitmask for Core Graphics events.
    ///
    /// The returned value uses the macOS `CGEventFlags` constants:
    /// - Control: `0x0004_0000`
    /// - Option:  `0x0008_0000`
    /// - Command: `0x0010_0000`
    /// - Shift:   `0x0002_0000`
    #[must_use]
    pub fn required_modifier_flags(&self) -> u64 {
        let mut flags = 0u64;
        for modifier in &self.modifiers {
            flags |= match modifier {
                MenuAnywhereModifier::Control => 0x0004_0000,
                MenuAnywhereModifier::Option => 0x0008_0000,
                MenuAnywhereModifier::Command => 0x0010_0000,
                MenuAnywhereModifier::Shift => 0x0002_0000,
            };
        }
        flags
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_menu_anywhere_config_default() {
        let config = MenuAnywhereConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.modifiers, vec![
            MenuAnywhereModifier::Control,
            MenuAnywhereModifier::Command
        ]);
        assert_eq!(config.mouse_button, MenuAnywhereMouseButton::RightClick);
    }

    #[test]
    fn test_required_modifier_flags() {
        let config = MenuAnywhereConfig::default();
        // Control (0x40000) + Command (0x100000)
        assert_eq!(config.required_modifier_flags(), 0x0014_0000);
    }
}
