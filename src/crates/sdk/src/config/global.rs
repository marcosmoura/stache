//! The global configuration and its startup-command shape.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use specta::Type;

use crate::error::ConfigError;
use crate::logging::LogLevel;
use crate::module::{ModuleConfig, ModuleId};

/// Logging behavior selected by `global.toml`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(default, rename_all = "snake_case")]
pub struct LoggingConfig {
    /// Minimum severity the host writes.
    pub level: LogLevel,
    /// Whether the host includes development-only detail in its records.
    pub debug: bool,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
            debug: false,
        }
    }
}

/// Startup commands, accepted as one string or an ordered list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(untagged)]
pub enum StartupCommands {
    /// One command.
    Single(String),
    /// Commands executed sequentially.
    Multiple(Vec<String>),
}

impl Default for StartupCommands {
    fn default() -> Self { Self::Multiple(Vec::new()) }
}

impl StartupCommands {
    /// The trimmed, non-empty commands in order.
    #[must_use]
    pub fn commands(&self) -> Vec<&str> {
        match self {
            Self::Single(command) => {
                if command.trim().is_empty() {
                    Vec::new()
                } else {
                    vec![command.trim()]
                }
            }
            Self::Multiple(commands) => commands
                .iter()
                .map(String::as_str)
                .map(str::trim)
                .filter(|command| !command.is_empty())
                .collect(),
        }
    }

    /// Whether no command would be executed.
    #[must_use]
    pub fn is_empty(&self) -> bool { self.commands().is_empty() }
}

/// The global configuration shared by every module.
///
/// The registry never hosts this type as a module; it is a catalog entry whose
/// file name is `global.toml`. `enabled` exists only to satisfy
/// [`ModuleConfig`] and is always `true`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(default, rename_all = "snake_case")]
pub struct GlobalConfig {
    /// Logging behavior for the host.
    pub logging: LoggingConfig,
    /// Commands to execute once per desktop launch.
    pub exec_on_startup: StartupCommands,
}

impl GlobalConfig {
    /// The catalog identifier and file stem of the global configuration.
    pub const ID: ModuleId = ModuleId::new("global");

    /// The minimum level selected by this file.
    #[must_use]
    pub const fn log_level(&self) -> LogLevel { self.logging.level }
}

impl ModuleConfig for GlobalConfig {
    fn enabled(&self) -> bool { true }

    fn validate(&self) -> Result<(), ConfigError> { Ok(()) }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::config::diagnostics::ConfigFileId;
    use crate::config::loader;

    #[test]
    fn startup_commands_accept_a_string_or_a_list() {
        let single: GlobalConfig = loader::decode(
            ConfigFileId::Global,
            Path::new("global.toml"),
            "exec_on_startup = \"open -a Terminal\"\n",
        )
        .expect("single");
        assert_eq!(single.exec_on_startup.commands(), ["open -a Terminal"]);
        let list: GlobalConfig = loader::decode(
            ConfigFileId::Global,
            Path::new("global.toml"),
            "exec_on_startup = [\"one\", \" two \", \"\"]\n",
        )
        .expect("list");
        assert_eq!(list.exec_on_startup.commands(), ["one", "two"]);
    }

    #[test]
    fn startup_commands_default_to_an_empty_list() {
        assert!(StartupCommands::default().is_empty());
    }

    #[test]
    fn global_defaults_to_info_logging() {
        let config = GlobalConfig::default();
        assert_eq!(config.log_level(), LogLevel::Info);
        assert!(!config.logging.debug);
        assert!(config.enabled());
    }
}
