//! Configuration diagnostics with file, field and parse locations.
//!
//! A diagnostic always names the file it belongs to. Parse errors carry the
//! source line and column reported by the JSONC parser; type errors carry the
//! serde field path. Both are separate from the module lifecycle status.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::error::ConfigError;
use crate::module::ModuleId;

/// One configuration file known to the catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConfigFileId {
    /// The global file, `global.jsonc`.
    Global,
    /// A module file named after its exact identifier.
    Module(ModuleId),
}

impl ConfigFileId {
    /// The global configuration file.
    #[must_use]
    pub const fn global() -> Self { Self::Global }

    /// The file belonging to one module identifier.
    ///
    /// `global` is reserved for [`ConfigFileId::Global`].
    #[must_use]
    pub const fn module(id: ModuleId) -> Self {
        if id.as_str().eq_ignore_ascii_case("global") {
            Self::Global
        } else {
            Self::Module(id)
        }
    }

    /// The module identifier, or `None` for the global file.
    #[must_use]
    pub const fn module_id(self) -> Option<ModuleId> {
        match self {
            Self::Global => None,
            Self::Module(id) => Some(id),
        }
    }

    /// The file name used inside a configuration directory.
    #[must_use]
    pub fn file_name(self) -> String {
        match self {
            Self::Global => "global.jsonc".to_owned(),
            Self::Module(id) => format!("{}.jsonc", id.as_str()),
        }
    }
}

impl fmt::Display for ConfigFileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.file_name()) }
}

/// Why a configuration file was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticKind {
    /// The file could not be read.
    Io,
    /// The file is not valid JSONC under the enabled syntax options.
    Parse,
    /// The file parsed but does not match the registered type.
    Type,
    /// The typed value failed the module's validation.
    Validation,
    /// The file is absent after a previously accepted load.
    Deleted,
    /// The registry rejected the candidate while applying it.
    Application,
}

/// A 1-indexed source position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceLocation {
    /// 1-indexed line number.
    pub line: usize,
    /// 1-indexed column number.
    pub column: usize,
}

/// One rejected configuration file.
///
/// Diagnostics are never used to drive control flow; branch on
/// [`DiagnosticKind`] or the [`ReloadOutcome`](crate::config::ReloadOutcome).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigDiagnostic {
    /// Which catalog file was involved.
    pub file: ConfigFileId,
    /// Absolute or caller-supplied path of the file.
    pub path: PathBuf,
    /// The rejection category.
    pub kind: DiagnosticKind,
    /// Parse position when the parser reported one.
    pub location: Option<SourceLocation>,
    /// serde field path when a type error named one.
    pub field: Option<String>,
    /// Human-readable reason.
    pub message: String,
}

impl ConfigDiagnostic {
    /// An I/O failure while reading the file.
    #[must_use]
    pub fn io(file: ConfigFileId, path: &Path, error: &std::io::Error) -> Self {
        Self::new(file, path, DiagnosticKind::Io, error.to_string())
    }

    /// A JSONC syntax error at a known source position.
    #[must_use]
    pub fn parse(
        file: ConfigFileId,
        path: &Path,
        error: &jsonc_parser::errors::ParseError,
    ) -> Self {
        Self {
            file,
            path: path.to_path_buf(),
            kind: DiagnosticKind::Parse,
            location: Some(SourceLocation {
                line: error.line_display(),
                column: error.column_display(),
            }),
            field: None,
            message: error.kind().to_string(),
        }
    }

    /// A type error at a known serde field path.
    #[must_use]
    pub fn typed(
        file: ConfigFileId,
        path: &Path,
        error: &serde_path_to_error::Error<serde_json::Error>,
    ) -> Self {
        let field = error.path().to_string();
        Self::typed_message(
            file,
            path,
            (!field.is_empty() && field != ".").then_some(field),
            &error.inner().to_string(),
        )
    }

    /// A type error whose field path and message were extracted by the decoder.
    #[must_use]
    pub fn typed_message(
        file: ConfigFileId,
        path: &Path,
        field: Option<String>,
        message: &str,
    ) -> Self {
        Self {
            file,
            path: path.to_path_buf(),
            kind: DiagnosticKind::Type,
            location: None,
            field,
            message: message.to_owned(),
        }
    }

    /// A value rejected by [`crate::ModuleConfig::validate`].
    #[must_use]
    pub fn validation(file: ConfigFileId, path: &Path, error: &ConfigError) -> Self {
        Self::new(
            file,
            path,
            DiagnosticKind::Validation,
            error.message().to_owned(),
        )
    }

    /// A file that disappeared after a previously accepted load.
    #[must_use]
    pub fn deleted(file: ConfigFileId, path: &Path) -> Self {
        Self::new(
            file,
            path,
            DiagnosticKind::Deleted,
            "file was deleted".to_owned(),
        )
    }

    /// A candidate the registry refused to apply.
    #[must_use]
    pub fn application(file: ConfigFileId, path: &Path, message: &str) -> Self {
        Self::new(file, path, DiagnosticKind::Application, message.to_owned())
    }

    fn new(file: ConfigFileId, path: &Path, kind: DiagnosticKind, message: String) -> Self {
        Self {
            file,
            path: path.to_path_buf(),
            kind,
            location: None,
            field: None,
            message,
        }
    }
}

impl fmt::Display for ConfigDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path.display())?;
        if let Some(location) = self.location {
            write!(f, ":{}:{}", location.line, location.column)?;
        }
        write!(f, ": {}", self.message)?;
        if let Some(field) = &self.field {
            write!(f, " (at `{field}`)")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigDiagnostic {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_files_use_the_exact_identifier() {
        let file = ConfigFileId::module(ModuleId::new("proxyAudio"));
        assert_eq!(file.file_name(), "proxyAudio.jsonc");
        assert_eq!(file.module_id(), Some(ModuleId::new("proxyAudio")));
    }

    #[test]
    fn global_is_the_reserved_identifier() {
        assert_eq!(
            ConfigFileId::module(ModuleId::new("global")),
            ConfigFileId::Global
        );
        assert_eq!(ConfigFileId::Global.module_id(), None);
    }

    #[test]
    fn diagnostics_render_path_location_and_field() {
        let diagnostic = ConfigDiagnostic {
            file: ConfigFileId::module(ModuleId::new("proxyAudio")),
            path: PathBuf::from("/tmp/proxyAudio.jsonc"),
            kind: DiagnosticKind::Type,
            location: Some(SourceLocation { line: 4, column: 7 }),
            field: Some("output[0].strategy".to_owned()),
            message: "unknown variant `fuzzy`".to_owned(),
        };
        assert_eq!(
            diagnostic.to_string(),
            "/tmp/proxyAudio.jsonc:4:7: unknown variant `fuzzy` (at `output[0].strategy`)"
        );
    }
}
