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
        self.directory.join(format!("{}.toml", id.as_str()))
    }

    /// The per-module JSONC file of an earlier release for one identifier.
    ///
    /// Only `config migrate` reads it; the runtime ignores it.
    #[must_use]
    pub fn module_jsonc_file(&self, id: ModuleId) -> PathBuf {
        self.directory.join(format!("{}.jsonc", id.as_str()))
    }

    /// The names of `<id>.jsonc` files that have no `<id>.toml` next to them.
    ///
    /// Only the given identifiers are considered, in the given order.
    #[must_use]
    pub fn stale_jsonc_files(&self, ids: impl IntoIterator<Item = ModuleId>) -> Vec<String> {
        ids.into_iter()
            .filter(|id| self.module_jsonc_file(*id).is_file() && !self.module_file(*id).exists())
            .map(|id| format!("{}.jsonc", id.as_str()))
            .collect()
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

impl ConfigPaths {
    /// The one legacy-config note, if a legacy source exists and `global.toml`
    /// does not.
    ///
    /// Paths only: the source is never opened, parsed, copied or rewritten.
    /// When registered `<id>.jsonc` files also sit in the directory, plain
    /// `config migrate` would convert those instead, so the note names
    /// `--from <source>`.
    #[must_use]
    pub fn legacy_config_hint(
        &self,
        environment: &ConfigEnvironment,
        ids: impl IntoIterator<Item = ModuleId>,
    ) -> Option<String> {
        if self.global_file().exists() {
            return None;
        }
        let source = self.legacy_sources(environment).into_iter().find(|path| path.exists())?;
        let module_jsonc = ids.into_iter().any(|id| self.module_jsonc_file(id).is_file());
        let command = if module_jsonc {
            format!("stache config migrate --from {}", source.display())
        } else {
            "stache config migrate".to_owned()
        };
        Some(format!(
            "note: {} is a legacy config and is not read; run `{command}`",
            source.display()
        ))
    }
}

/// The hint printed for one stale `<id>.jsonc` file name.
#[must_use]
pub fn stale_jsonc_hint(file_name: &str) -> String {
    format!("note: {file_name} is ignored; run `stache config migrate`")
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
            Path::new("/config/proxyAudio.toml")
        );
        assert_eq!(paths.global_file(), Path::new("/config/global.toml"));
    }

    #[test]
    fn stale_jsonc_files_need_a_jsonc_and_no_toml() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = ConfigPaths::new(root.path());
        let ids = [
            ModuleId::new("tiling"),
            ModuleId::new("bar"),
            ModuleId::new("keepAwake"),
        ];
        std::fs::write(root.path().join("tiling.jsonc"), "{}").expect("tiling");
        std::fs::write(root.path().join("bar.jsonc"), "{}").expect("bar");
        std::fs::write(root.path().join("bar.toml"), "").expect("bar toml");
        assert_eq!(paths.stale_jsonc_files(ids), vec!["tiling.jsonc".to_owned()]);
        assert_eq!(
            stale_jsonc_hint("tiling.jsonc"),
            "note: tiling.jsonc is ignored; run `stache config migrate`"
        );
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
