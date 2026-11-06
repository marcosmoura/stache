//! Client-side resolution of the `set` PATH argument.
//!
//! The token is a literal filesystem argument: it is never trimmed. Only a
//! leading `~` or `~/` expands, from the injected home.
//!
//! - A *path-shaped* token (absolute, `~`-anchored or containing `/`) must name
//!   an existing regular supported file, resolved against the invocation
//!   working directory; otherwise it is an argument error and never a
//!   collection-name fallback.
//! - A *bare* token that names an existing regular supported file in the
//!   working directory is that file; an actual file wins over a collection
//!   entry with the same name. Any other bare token is a collection name.

use std::path::{Path, PathBuf};
use std::{fmt, fs};

use super::adapters::quoted;
use crate::sources::is_supported_image;

/// What the PATH argument resolved to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenResolution {
    /// An absolute, literal, existing supported file.
    File(PathBuf),
    /// A collection name to look up on the accepted pool.
    Name(String),
}

/// Why the PATH argument is unusable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenError {
    token: String,
    reason: String,
}

impl TokenError {
    fn new(token: &str, reason: impl Into<String>) -> Self {
        Self {
            token: token.to_owned(),
            reason: reason.into(),
        }
    }

    /// The offending argument text.
    #[must_use]
    pub fn token(&self) -> &str { &self.token }

    /// The actionable reason.
    #[must_use]
    pub fn reason(&self) -> &str { &self.reason }
}

impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Invalid wallpaper path {}: {}",
            serde_json::Value::String(self.token.clone()),
            self.reason
        )
    }
}

impl std::error::Error for TokenError {}

/// Resolves the PATH argument against the working directory and home.
///
/// # Errors
/// Returns a [`TokenError`] for an empty argument, a `~` argument without a
/// home, or a path-shaped argument that is not an existing regular supported
/// file.
pub fn resolve_token(
    token: &str,
    cwd: &Path,
    home: Option<&Path>,
) -> Result<TokenResolution, TokenError> {
    if token.is_empty() {
        return Err(TokenError::new(token, "the argument is empty"));
    }
    let home_anchored = token == "~" || token.starts_with("~/");
    let path_shaped = home_anchored || token.contains('/');
    if !path_shaped {
        let candidate = cwd.join(token);
        return Ok(if is_supported_file(&candidate) {
            TokenResolution::File(candidate)
        } else {
            TokenResolution::Name(token.to_owned())
        });
    }
    let expanded = if home_anchored {
        let home = home.ok_or_else(|| {
            TokenError::new(token, "cannot expand `~`: no home directory is known")
        })?;
        if token == "~" {
            home.to_path_buf()
        } else {
            home.join(&token[2..])
        }
    } else {
        PathBuf::from(token)
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    };
    let metadata = fs::metadata(&absolute).map_err(|error| {
        TokenError::new(
            token,
            format!(
                "{}: {}",
                quoted(&absolute.to_string_lossy()),
                quoted(&error.to_string())
            ),
        )
    })?;
    if !metadata.is_file() {
        return Err(TokenError::new(
            token,
            format!("{} is not a regular file", quoted(&absolute.to_string_lossy())),
        ));
    }
    if !is_supported_image(&absolute) {
        return Err(TokenError::new(
            token,
            format!(
                "{} is not a jpg, jpeg, png or webp file",
                quoted(&absolute.to_string_lossy())
            ),
        ));
    }
    Ok(TokenResolution::File(absolute))
}

fn is_supported_file(path: &Path) -> bool {
    is_supported_image(path) && fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}
