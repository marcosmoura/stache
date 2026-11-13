//! The Visual Crossing key reader for `weather.api_keys`.
//!
//! The configured file is read on every weather refresh (no watcher, no cache)
//! and the process environment is never written. The parser is `dotenvy`
//! 0.15.7, the same crate the legacy application used
//! (its `config/env.rs`: `dotenvy::from_path_iter`, bad
//! lines skipped, later duplicates win), so the file format is unchanged.
//!
//! # Legacy audit
//!
//! What `dotenvy::from_read_iter` (and so legacy) does, and what this reader
//! keeps:
//!
//! - Blank lines and `#` comments (whole-line or after an unquoted value) are
//!   skipped.
//! - An optional `export ` prefix is accepted; whitespace around the name and
//!   the `=` is ignored.
//! - Values may be unquoted (trimmed), `'single'` (literal), `"double"` (with
//!   `\"`, `\\`, `\n` style escapes) and may span lines inside quotes.
//! - A line that does not parse (no `=`, an unterminated quote, an invalid
//!   name) is skipped without failing the file. An unterminated quote swallows
//!   the rest of the file, like legacy.
//! - A repeated name: the **last** one wins (legacy collected into a map).
//! - The UTF-8 byte order mark is **not** stripped (`from_path_iter` never
//!   did), so a file that starts with one hides its first entry. Non-UTF-8
//!   lines are skipped.
//! - `$NAME` / `${NAME}` in unquoted and double-quoted values is substituted
//!   from earlier entries of the same file and then from the process
//!   environment (read only), exactly as legacy; the legacy doc comment's
//!   "variable expansion (disabled)" is wrong. A key containing a literal `$`
//!   should be single-quoted.
//!
//! # Differences from legacy
//!
//! - The result is trimmed and an empty value is "no key" (legacy handed the
//!   raw value to the UI, which treated an empty string as no key).
//! - The file is read through a bounded reader ([`MAX_ENV_FILE_BYTES`]) and
//!   must be a regular file (a symlink to one is followed, as legacy did; a
//!   directory, FIFO or device is refused before it is opened). Legacy read
//!   anything `File::open` accepted without a bound.
//! - A failure is a typed, redacted [`ApiKeyDiagnostic`] instead of a
//!   `tracing::warn!` that carried the `dotenvy` error text.
//! - A relative path resolves only against the configuration directory.
//!   Legacy `get_weather_config` fell back to `.` (the process working
//!   directory) when it knew no config path; that fallback is removed.

use std::fs::{self, File};
use std::io::{self, Cursor, Read};
use std::path::Path;

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::config::WeatherConfig;

/// The only name this reader extracts.
pub const KEY_NAME: &str = "VISUAL_CROSSING_API_KEY";

/// The most bytes read from the env file; a larger file is refused.
pub const MAX_ENV_FILE_BYTES: u64 = 64 * 1024;

/// Why no key came out of the configured file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ApiKeyProblem {
    /// `api_keys` starts with `~` but no home directory is known.
    Unresolvable,
    /// The file does not exist.
    Missing,
    /// The path is not a regular file (or a symlink to one).
    NotAFile,
    /// The file exists but could not be read (permissions, I/O error).
    Unreadable,
    /// The file is larger than [`MAX_ENV_FILE_BYTES`].
    TooLarge,
    /// The file was read but has no non-empty `VISUAL_CROSSING_API_KEY`.
    KeyAbsent,
}

/// A redacted, actionable note about the key file. It names the path, never a
/// value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyDiagnostic {
    /// What went wrong.
    pub problem: ApiKeyProblem,
    /// The resolved path, when one could be resolved.
    pub path: Option<String>,
}

/// The result of reading the key file.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKeyRead {
    /// The trimmed key; `None` when missing, empty or unreadable.
    pub key: Option<String>,
    /// Why there is no key (or `None` when there is one or nothing was read).
    pub diagnostic: Option<ApiKeyDiagnostic>,
}

impl std::fmt::Debug for ApiKeyRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKeyRead")
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .field("diagnostic", &self.diagnostic)
            .finish()
    }
}

impl ApiKeyRead {
    const fn none() -> Self { Self { key: None, diagnostic: None } }

    fn problem(problem: ApiKeyProblem, path: Option<&Path>) -> Self {
        Self {
            key: None,
            diagnostic: Some(ApiKeyDiagnostic {
                problem,
                path: path.map(|path| path.display().to_string()),
            }),
        }
    }
}

/// Extracts the trimmed, non-empty `VISUAL_CROSSING_API_KEY` from env-file
/// bytes. Reads the process environment only for `$NAME` substitution (see the
/// module docs) and never writes it.
#[must_use]
pub fn parse_api_key(bytes: &[u8]) -> Option<String> {
    let mut found = None;
    for item in dotenvy::from_read_iter(Cursor::new(bytes)) {
        // A malformed line is skipped, as legacy did.
        if let Ok((name, value)) = item
            && name == KEY_NAME
        {
            found = Some(value);
        }
    }
    found.map(|value| value.trim().to_owned()).filter(|value| !value.is_empty())
}

/// Reads the key for `weather` against the injected `home` and `config_dir`.
///
/// Blocking file I/O: never call it on the main thread. An empty `api_keys`
/// reads nothing.
#[must_use]
pub fn read_api_key(weather: &WeatherConfig, home: Option<&Path>, config_dir: &Path) -> ApiKeyRead {
    let path = match weather.api_keys_path(home, config_dir) {
        Ok(Some(path)) => path,
        Ok(None) => return ApiKeyRead::none(),
        Err(_) => return ApiKeyRead::problem(ApiKeyProblem::Unresolvable, None),
    };
    let bytes = match read_bounded(&path) {
        Ok(bytes) => bytes,
        Err(problem) => return ApiKeyRead::problem(problem, Some(&path)),
    };
    parse_api_key(&bytes).map_or_else(
        || ApiKeyRead::problem(ApiKeyProblem::KeyAbsent, Some(&path)),
        |key| ApiKeyRead {
            key: Some(key),
            diagnostic: None,
        },
    )
}

fn classify(error: &io::Error) -> ApiKeyProblem {
    match error.kind() {
        io::ErrorKind::NotFound => ApiKeyProblem::Missing,
        _ => ApiKeyProblem::Unreadable,
    }
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, ApiKeyProblem> {
    // Checked before opening so a FIFO or device is never opened (an open of a
    // FIFO can block). The handle is checked again after the open.
    let metadata = fs::metadata(path).map_err(|error| classify(&error))?;
    if !metadata.is_file() {
        return Err(ApiKeyProblem::NotAFile);
    }
    let file = File::open(path).map_err(|error| classify(&error))?;
    let opened = file.metadata().map_err(|error| classify(&error))?;
    if !opened.is_file() {
        return Err(ApiKeyProblem::NotAFile);
    }
    if opened.len() > MAX_ENV_FILE_BYTES {
        return Err(ApiKeyProblem::TooLarge);
    }
    let mut bytes = Vec::new();
    file.take(MAX_ENV_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| classify(&error))?;
    if bytes.len() as u64 > MAX_ENV_FILE_BYTES {
        return Err(ApiKeyProblem::TooLarge);
    }
    Ok(bytes)
}
