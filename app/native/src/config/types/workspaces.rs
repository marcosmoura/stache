//! Workspace and window rule configuration types.
//!
//! Configuration for workspace definitions and window matching rules.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::tiling::LayoutType;

/// Window matching rule for workspace assignment.
///
/// All specified properties must match (AND logic).
/// At least one property must be specified.
///
/// # Performance
///
/// Call [`WindowRule::prepare()`] after loading rules from config to pre-compute
/// lowercase versions of string fields. This avoids repeated `to_lowercase()` calls
/// during window matching.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct WindowRule {
    /// Match by bundle identifier (e.g., "com.apple.finder").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,

    /// Match by window title (substring match).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// Match by application name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,

    // Cached lowercase versions for fast matching (computed by prepare())
    #[serde(skip)]
    #[schemars(skip)]
    pub(crate) app_id_lower: Option<String>,

    #[serde(skip)]
    #[schemars(skip)]
    pub(crate) title_lower: Option<String>,

    #[serde(skip)]
    #[schemars(skip)]
    pub(crate) app_name_lower: Option<String>,
}

impl WindowRule {
    /// Returns true if the rule has at least one matching criterion.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.app_id.is_some() || self.title.is_some() || self.app_name.is_some()
    }

    /// Pre-computes lowercase versions of string fields for faster matching.
    ///
    /// Call this after loading rules from config. The lowercase values are cached
    /// and reused by window matching logic.
    pub fn prepare(&mut self) {
        self.app_id_lower = self.app_id.as_ref().map(|s| s.to_ascii_lowercase());
        self.title_lower = self.title.as_ref().map(|s| s.to_lowercase());
        self.app_name_lower = self.app_name.as_ref().map(|s| s.to_lowercase());
    }

    /// Returns the cached lowercase `app_id`, or the original if not cached.
    #[must_use]
    pub fn app_id_lowercase(&self) -> Option<&str> {
        self.app_id_lower.as_deref().or(self.app_id.as_deref())
    }

    /// Returns the cached lowercase title, or the original if not cached.
    #[must_use]
    pub fn title_lowercase(&self) -> Option<&str> {
        self.title_lower.as_deref().or(self.title.as_deref())
    }

    /// Returns the cached lowercase `app_name`, or the original if not cached.
    #[must_use]
    pub fn app_name_lowercase(&self) -> Option<&str> {
        self.app_name_lower.as_deref().or(self.app_name.as_deref())
    }
}

/// Helper function for default screen value.
fn default_screen() -> String { "main".to_string() }

/// Workspace configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceConfig {
    /// Unique name for the workspace.
    pub name: String,

    /// Layout mode for this workspace.
    /// If not specified, uses the `defaultLayout` from the tiling config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<LayoutType>,

    /// Screen assignment: "main"/"primary", "secondary", or screen name.
    /// Default: "main"
    #[serde(default = "default_screen")]
    pub screen: String,

    /// Rules for automatically assigning windows to this workspace.
    #[serde(default)]
    pub rules: Vec<WindowRule>,

    /// Floating preset to apply when windows open in this workspace.
    #[serde(
        default,
        rename = "preset-on-open",
        skip_serializing_if = "Option::is_none"
    )]
    pub preset_on_open: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_window_rule_is_valid() {
        let empty = WindowRule::default();
        assert!(!empty.is_valid());

        let with_app_id = WindowRule {
            app_id: Some("com.example.app".to_string()),
            ..Default::default()
        };
        assert!(with_app_id.is_valid());
    }

    #[test]
    fn test_window_rule_prepare() {
        let mut rule = WindowRule {
            app_id: Some("COM.EXAMPLE.App".to_string()),
            title: Some("My Window".to_string()),
            app_name: Some("Example App".to_string()),
            ..Default::default()
        };
        rule.prepare();

        assert_eq!(rule.app_id_lowercase(), Some("com.example.app"));
        assert_eq!(rule.title_lowercase(), Some("my window"));
        assert_eq!(rule.app_name_lowercase(), Some("example app"));
    }
}
