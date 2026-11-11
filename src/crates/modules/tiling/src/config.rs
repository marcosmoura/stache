//! Authoritative Tiling configuration.

mod borders;
mod color;
mod dimension;
mod floating;
mod gaps;
mod layout;
mod workspaces;

pub use borders::{
    BorderAnimationConfig, BorderAnimationStyle, BorderOrder, BorderStateConfig,
    BorderTransitionConfig, BordersConfig, GradientConfig,
};
pub use dimension::DimensionValue;
pub use floating::{FloatingConfig, FloatingPosition, FloatingPreset};
pub use gaps::{GapValue, GapsConfig, GapsConfigValue, ScreenGapsConfig};
pub use layout::{AnimationConfig, EasingType, LayoutType, MasterConfig, MasterPosition};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use specta::Type;
use stache_sdk::{ConfigError, ModuleConfig, ModuleId};
pub use workspaces::{WindowRule, WorkspaceConfig};

/// Virtual workspaces, layouts, gaps, floating presets and borders.
///
/// Tiling is disabled by default. Setting `enabled: true` (with Accessibility
/// granted) starts window management: Stache moves and resizes windows and
/// hides and unhides applications to implement virtual workspaces.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(default, rename_all = "snake_case")]
pub struct TilingConfig {
    /// Whether the tiling window manager should run.
    pub enabled: bool,
    /// Layout for workspaces that do not specify one.
    pub default_layout: LayoutType,
    /// Workspace definitions. Empty means one default workspace per display.
    pub workspaces: Vec<WorkspaceConfig>,
    /// Windows that are never managed.
    pub ignore: Vec<WindowRule>,
    /// Window transition animations.
    pub animations: AnimationConfig,
    /// Gaps, global or per display.
    pub gaps: GapsConfigValue,
    /// Floating window defaults and presets.
    pub floating: FloatingConfig,
    /// Master layout settings.
    pub master: MasterConfig,
    /// Window borders.
    pub borders: BordersConfig,
}

impl TilingConfig {
    /// The catalog identifier and file stem.
    pub const ID: ModuleId = ModuleId::new("tiling");

    fn check(&self) -> Result<(), String> {
        self.master.validate()?;
        self.gaps.validate()?;
        self.floating.validate()?;
        self.borders.validate()?;
        for (index, workspace) in self.workspaces.iter().enumerate() {
            let at = format!("workspaces[{index}]");
            if self.workspaces[..index].iter().any(|earlier| earlier.name == workspace.name) {
                return Err(format!("{at}: duplicate workspace name `{}`", workspace.name));
            }
            if let Some(preset) = &workspace.preset_on_open
                && !self.floating.has_preset(preset)
            {
                return Err(format!(
                    "{at}.preset_on_open: no floating preset named `{preset}`"
                ));
            }
        }
        Ok(())
    }
}

impl ModuleConfig for TilingConfig {
    fn enabled(&self) -> bool { self.enabled }

    fn validate(&self) -> Result<(), ConfigError> { self.check().map_err(ConfigError::new) }

    /// Keeps every legacy shape and reports the intentional runtime
    /// corrections the migrated file will see once the runtime ships.
    fn migrate_legacy_section(section: &mut Value) -> Vec<String> {
        let mut notes = Vec::new();
        // Borders are always round: the legacy `style` key is dropped rather
        // than carried over as an unknown key.
        if section
            .get_mut("borders")
            .and_then(Value::as_object_mut)
            .and_then(|borders| borders.remove("style"))
            .is_some_and(|style| style.as_str() == Some("square"))
        {
            notes.push(
                "tiling borders.style `square` was removed: borders are always round".to_owned(),
            );
        }
        let Some(object) = section.as_object() else {
            return notes;
        };
        let is_grid = |value: Option<&Value>| value.and_then(Value::as_str) == Some("grid");
        let uses_grid = is_grid(object.get("defaultLayout"))
            || object
                .get("workspaces")
                .and_then(Value::as_array)
                .is_some_and(|all| all.iter().any(|workspace| is_grid(workspace.get("layout"))));
        if uses_grid {
            notes.push(
                "tiling grid layout now tiles at most 10 windows and floats extra windows \
                 (legacy allowed 12 and hid the extras)"
                    .to_owned(),
            );
        }
        if object.get("enabled").and_then(Value::as_bool) == Some(true) {
            let position =
                object.get("floating").and_then(|floating| floating.get("defaultPosition"));
            if position.is_none_or(|value| value.as_str() == Some("center")) {
                notes.push(
                    "tiling floating.default_position `center` will now center newly floating \
                     windows once; use `default` to keep application frames"
                        .to_owned(),
                );
            }
            if !object.contains_key("defaultLayout") {
                notes.push(
                    "tiling default_layout was omitted; dwindle is now the single default"
                        .to_owned(),
                );
            }
        }
        notes
    }
}
