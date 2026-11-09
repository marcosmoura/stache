//! Workspace definitions and window rules.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use specta::Type;

use super::LayoutType;

/// A window matching rule; every specified field must match (AND).
///
/// A rule with no field never matches anything.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(default, rename_all = "snake_case")]
pub struct WindowRule {
    /// Bundle identifier, matched case-insensitively and exactly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    /// Window title, matched as a case-insensitive substring.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Application name, matched as a case-insensitive substring.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
}

impl WindowRule {
    /// Whether the rule has at least one criterion.
    #[must_use]
    pub const fn has_criteria(&self) -> bool {
        self.app_id.is_some() || self.title.is_some() || self.app_name.is_some()
    }
}

fn default_screen() -> String { "main".to_owned() }

/// A workspace definition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceConfig {
    /// Unique workspace name.
    pub name: String,
    /// Layout; omitted uses `default_layout`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<LayoutType>,
    /// Display: `main`/`primary`, `secondary` or a display name.
    #[serde(default = "default_screen")]
    pub screen: String,
    /// Rules that assign windows to this workspace.
    #[serde(default)]
    pub rules: Vec<WindowRule>,
    /// Floating preset applied when windows open here. Parsed and validated
    /// but inert: nothing reads it at runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_on_open: Option<String>,
}
