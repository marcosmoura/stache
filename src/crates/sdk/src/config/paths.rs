//! Injected configuration directories.
//!
//! Nothing in this module mutates environment variables or reads the process
//! environment implicitly: callers pass a [`ConfigEnvironment`], which keeps
//! tests independent of the real home directory.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::module::ModuleId;

/// The environment variables used to choose the default directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigEnvironment {
    /// `XDG_CONFIG_HOME`, when set and non-empty.
    pub xdg_config_home: Option<PathBuf>,
    /// `HOME`, when set and non-empty.
    pub home: Option<PathBuf>,
}

impl ConfigEnvironment {
    /// Reads the process environment once.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            xdg_config_home: non_empty_var("XDG_CONFIG_HOME"),
            home: non_empty_var("HOME"),
        }
    }
}

fn non_empty_var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|value| !value.is_empty()).map(PathBuf::from)
}

/// A resolved configuration directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigPaths {
    directory: PathBuf,
}

impl ConfigPaths {
    /// Uses an explicit directory without checking that it exists.
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self { Self { directory: directory.into() } }

    /// Resolves the explicit flag, `XDG_CONFIG_HOME/stache` or
    /// `~/.config/stache`, in that order.
    ///
    /// The directory is not required to exist.
    ///
    /// # Errors
    /// Returns [`PathError::NoHome`] when neither an explicit directory nor a
    /// usable home directory is available.
    pub fn resolve(
        explicit: Option<PathBuf>,
        environment: &ConfigEnvironment,
    ) -> Result<Self, PathError> {
        if let Some(directory) = explicit {
            return Ok(Self::new(directory));
        }
        if let Some(xdg) =
            environment.xdg_config_home.as_ref().filter(|path| !path.as_os_str().is_empty())
        {
            return Ok(Self::new(xdg.join("stache")));
        }
        if let Some(home) = &environment.home {
            return Ok(Self::new(home.join(".config").join("stache")));
        }
        Err(PathError::NoHome)
    }

    /// The directory itself.
    #[must_use]
    pub fn directory(&self) -> &Path { &self.directory }

    /// The global configuration file.
    #[must_use]
    pub fn global_file(&self) -> PathBuf { self.module_file(ModuleId::new("global")) }

    /// The file for one exact module identifier.
    #[must_use]
    pub fn module_file(&self, id: ModuleId) -> PathBuf {
        self.directory.join(format!("{}.jsonc", id.as_str()))
    }

    /// The file for one catalog entry, including the global file.
    #[must_use]
    pub fn file(&self, id: crate::config::ConfigFileId) -> PathBuf {
        self.directory.join(id.file_name())
    }

    /// The ordered legacy migration source pairs.
    ///
    /// Each location contributes `.jsonc` before `.json`; duplicate paths are
    /// removed while preserving order.
    #[must_use]
    pub fn legacy_sources(&self, environment: &ConfigEnvironment) -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        if let Some(xdg) =
            environment.xdg_config_home.as_ref().filter(|path| !path.as_os_str().is_empty())
        {
            push_pair(&mut candidates, &xdg.join("stache"));
        }
        if let Some(home) = &environment.home {
            push_pair(&mut candidates, &home.join(".config").join("stache"));
            push_pair(
                &mut candidates,
                &home.join("Library").join("Application Support").join("stache"),
            );
            push_file_pair(&mut candidates, home, ".stache");
        }
        let mut unique = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            if !unique.contains(&candidate) {
                unique.push(candidate);
            }
        }
        unique
    }
}

fn push_pair(candidates: &mut Vec<PathBuf>, directory: &Path) {
    push_file_pair(candidates, directory, "config");
}

fn push_file_pair(candidates: &mut Vec<PathBuf>, directory: &Path, stem: &str) {
    candidates.push(directory.join(format!("{stem}.jsonc")));
    candidates.push(directory.join(format!("{stem}.json")));
}

/// A missing default directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathError {
    /// Neither an explicit directory nor a non-empty home directory is known.
    NoHome,
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("no configuration directory available: set HOME or pass --config DIR")
    }
}

impl std::error::Error for PathError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(xdg: Option<&str>, home: Option<&str>) -> ConfigEnvironment {
        ConfigEnvironment {
            xdg_config_home: xdg.map(PathBuf::from),
            home: home.map(PathBuf::from),
        }
    }

    #[test]
    fn explicit_directory_wins_over_the_environment() {
        let paths = ConfigPaths::resolve(
            Some(PathBuf::from("/explicit")),
            &environment(Some("/xdg"), Some("/home/me")),
        )
        .expect("resolve");
        assert_eq!(paths.directory(), Path::new("/explicit"));
    }

    #[test]
    fn non_empty_xdg_config_home_wins_over_home() {
        let paths = ConfigPaths::resolve(None, &environment(Some("/xdg"), Some("/home/me")))
            .expect("resolve");
        assert_eq!(paths.directory(), Path::new("/xdg/stache"));
    }

    #[test]
    fn empty_xdg_falls_back_to_the_home_directory() {
        let paths =
            ConfigPaths::resolve(None, &environment(Some(""), Some("/home/me"))).expect("resolve");
        assert_eq!(paths.directory(), Path::new("/home/me/.config/stache"));
    }

    #[test]
    fn a_missing_home_is_reported_instead_of_guessing() {
        assert_eq!(
            ConfigPaths::resolve(None, &environment(None, None)),
            Err(PathError::NoHome)
        );
    }

    #[test]
    fn module_files_use_the_exact_identifier() {
        let paths = ConfigPaths::new("/config");
        assert_eq!(
            paths.module_file(ModuleId::new("proxyAudio")),
            Path::new("/config/proxyAudio.jsonc")
        );
        assert_eq!(paths.global_file(), Path::new("/config/global.jsonc"));
    }

    #[test]
    fn legacy_sources_are_ordered_deduplicated_and_pair_jsonc_first() {
        let paths = ConfigPaths::new("/new");
        let sources =
            paths.legacy_sources(&environment(Some("/home/me/.config"), Some("/home/me")));
        assert_eq!(sources, vec![
            PathBuf::from("/home/me/.config/stache/config.jsonc"),
            PathBuf::from("/home/me/.config/stache/config.json"),
            PathBuf::from("/home/me/Library/Application Support/stache/config.jsonc"),
            PathBuf::from("/home/me/Library/Application Support/stache/config.json"),
            PathBuf::from("/home/me/.stache.jsonc"),
            PathBuf::from("/home/me/.stache.json"),
        ]);
    }

    #[test]
    fn xdg_and_home_config_are_deduplicated() {
        let paths = ConfigPaths::new("/new");
        let sources =
            paths.legacy_sources(&environment(Some("/home/me/.config"), Some("/home/me")));
        assert_eq!(
            sources
                .iter()
                .filter(|path| path.starts_with("/home/me/.config/stache"))
                .count(),
            2
        );
    }
}
