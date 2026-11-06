//! Shared resolution of configured and explicit wallpaper sources.
//!
//! Configuration strings are trimmed and resolved against the injected home or
//! configuration directory; explicit CLI arguments are literal and resolve
//! against the caller's working directory. No image is decoded here.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::{fmt, fs};

use stache_sdk::{ConfigEnvironment, ConfigPaths};

use crate::WallpapersConfig;

/// File extensions treated as images, compared without ASCII case.
const SUPPORTED_EXTENSIONS: [&str; 4] = ["jpg", "jpeg", "png", "webp"];

/// The injected bases for configured source strings.
#[derive(Clone, Debug)]
pub struct SourceContext {
    config_directory: PathBuf,
    home: Option<PathBuf>,
}

impl SourceContext {
    /// Builds a context from an explicit configuration directory and home.
    #[must_use]
    pub fn new(config_directory: impl Into<PathBuf>, home: Option<PathBuf>) -> Self {
        Self {
            config_directory: config_directory.into(),
            home,
        }
    }

    /// Builds a context from the host's resolved paths and environment.
    #[must_use]
    pub fn from_config(paths: &ConfigPaths, environment: &ConfigEnvironment) -> Self {
        Self::new(paths.directory(), environment.home.clone())
    }

    /// The directory containing `wallpapers.jsonc`.
    #[must_use]
    pub fn config_directory(&self) -> &Path { &self.config_directory }

    /// The injected home directory, if any.
    #[must_use]
    pub fn home(&self) -> Option<&Path> { self.home.as_deref() }
}

/// A resolved, ordered wallpaper pool.
///
/// An empty pool is a successful resolution: the local `list` command can
/// report it, while rotation and random selection call
/// [`SourcePool::require_non_empty`] and fail with [`SourceError::EmptyPool`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourcePool {
    images: Vec<PathBuf>,
}

impl SourcePool {
    /// The resolved image files in order.
    #[must_use]
    pub fn images(&self) -> &[PathBuf] { &self.images }

    /// Consumes the pool and returns its ordered files.
    #[must_use]
    pub fn into_images(self) -> Vec<PathBuf> { self.images }

    /// Whether the pool has no usable image.
    #[must_use]
    pub const fn is_empty(&self) -> bool { self.images.is_empty() }

    /// The number of usable images.
    #[must_use]
    pub const fn len(&self) -> usize { self.images.len() }

    /// Rejects an empty pool where a caller needs at least one image.
    ///
    /// # Errors
    /// Returns [`SourceError::EmptyPool`] when no usable image resolved.
    pub const fn require_non_empty(&self) -> Result<(), SourceError> {
        if self.images.is_empty() {
            Err(SourceError::EmptyPool)
        } else {
            Ok(())
        }
    }

    /// Builds a pool directly for unit tests that do not touch disk.
    #[cfg(test)]
    pub(crate) const fn from_images_for_test(images: Vec<PathBuf>) -> Self { Self { images } }
}

/// A source-resolution failure with its category preserved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceError {
    /// No usable image resolved; rotation and random selection cannot serve it.
    EmptyPool,
    /// A configured directory is missing, not a directory or unreadable.
    Directory {
        /// The resolved directory.
        path: PathBuf,
        /// The filesystem reason.
        message: String,
    },
    /// A configured `~` value cannot expand without an injected home.
    MissingHome {
        /// The configured source text.
        value: String,
    },
    /// An explicit argument failed before any lookup could succeed.
    InvalidArgument {
        /// The user-facing reason.
        message: String,
    },
    /// An explicit file is missing, not a regular file or unsupported.
    File {
        /// The resolved file path.
        path: PathBuf,
        /// The user-facing reason.
        message: String,
    },
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPool => f.write_str("the wallpaper pool is empty"),
            Self::Directory { path, message } => {
                write!(
                    f,
                    "wallpaper directory {} is unusable: {message}",
                    path.display()
                )
            }
            Self::MissingHome { value } => {
                write!(f, "cannot expand `{value}`: no home directory was supplied")
            }
            Self::InvalidArgument { message } => f.write_str(message),
            Self::File { path, message } => {
                write!(f, "wallpaper file {} is unusable: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for SourceError {}

/// Resolves the configured pool without decoding any image.
///
/// A nonempty `path` replaces `list` completely; an invalid directory is a
/// source failure and never falls back to the list. A list preserves order and
/// duplicates and silently filters entries that are missing, not regular files
/// or have an unsupported extension. Directory enumeration is nonrecursive and
/// naturally ordered with a deterministic path tie-break.
///
/// # Errors
/// Returns a source failure for an unusable directory, a `~` value without an
/// injected home or an unreadable directory.
pub fn resolve_pool(
    config: &WallpapersConfig,
    context: &SourceContext,
) -> Result<SourcePool, SourceError> {
    if !config.path.is_empty() {
        let configured = config.path.trim();
        if configured.is_empty() {
            return Err(SourceError::Directory {
                path: PathBuf::from(&config.path),
                message: "the configured path is blank".to_owned(),
            });
        }
        let directory = configured_path(configured, context)?;
        return list_directory(&directory).map(|images| SourcePool { images });
    }

    let mut images = Vec::new();
    for entry in &config.list {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        // An entry that cannot expand without a home is as unusable as a
        // missing file, so it is filtered like one.
        let Ok(path) = configured_path(entry, context) else {
            continue;
        };
        if is_supported_image(&path) && path.is_file() {
            images.push(path);
        }
    }
    Ok(SourcePool { images })
}

/// Resolves one explicit CLI file independently of the configured pool.
///
/// The argument is literal: it is not trimmed or tilde-expanded. A relative
/// argument resolves against `invocation_cwd`; an absolute one is unchanged.
/// The file must exist, be a regular file and have a supported extension.
///
/// # Errors
/// Returns [`SourceError::InvalidArgument`] for an empty argument and
/// [`SourceError::File`] for a missing, non-regular or unsupported file.
pub fn resolve_explicit_file(
    argument: &Path,
    invocation_cwd: &Path,
) -> Result<PathBuf, SourceError> {
    if argument.as_os_str().is_empty() {
        return Err(SourceError::InvalidArgument {
            message: "the wallpaper file argument is empty".to_owned(),
        });
    }
    let path = if argument.is_absolute() {
        argument.to_path_buf()
    } else {
        invocation_cwd.join(argument)
    };
    let metadata = fs::metadata(&path).map_err(|error| SourceError::File {
        path: path.clone(),
        message: error.to_string(),
    })?;
    if !metadata.is_file() {
        return Err(SourceError::File {
            path,
            message: "not a regular file".to_owned(),
        });
    }
    if !is_supported_image(&path) {
        return Err(SourceError::File {
            path,
            message: "unsupported image extension".to_owned(),
        });
    }
    Ok(path)
}

/// Finds the first pool entry whose file name or file stem equals `token`
/// ignoring case.
///
/// The token is compared untrimmed. Entries are tried in pool order and the
/// first matching entry wins; within one entry the full file name and the stem
/// are alternatives, which is the legacy per-entry precedence. An entry whose
/// name is not valid UTF-8 never matches. Duplicate or ambiguous names are
/// therefore resolved deterministically by pool order.
#[must_use]
pub fn find_pool_entry(pool: &SourcePool, token: &str) -> Option<usize> {
    let token = token.to_lowercase();
    pool.images().iter().position(|path| {
        let name_matches = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.to_lowercase() == token);
        name_matches
            || path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| stem.to_lowercase() == token)
    })
}

/// Whether a path has a supported image extension, ignoring ASCII case.
#[must_use]
pub fn is_supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            SUPPORTED_EXTENSIONS
                .iter()
                .any(|supported| extension.eq_ignore_ascii_case(supported))
        })
}

fn configured_path(value: &str, context: &SourceContext) -> Result<PathBuf, SourceError> {
    if value == "~" {
        return context
            .home
            .clone()
            .ok_or_else(|| SourceError::MissingHome { value: value.to_owned() });
    }
    if let Some(rest) = value.strip_prefix("~/") {
        let home = context
            .home
            .as_ref()
            .ok_or_else(|| SourceError::MissingHome { value: value.to_owned() })?;
        return Ok(home.join(rest));
    }
    let path = Path::new(value);
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(context.config_directory.join(path))
    }
}

fn list_directory(directory: &Path) -> Result<Vec<PathBuf>, SourceError> {
    let metadata = fs::metadata(directory).map_err(|error| SourceError::Directory {
        path: directory.to_path_buf(),
        message: if error.kind() == std::io::ErrorKind::NotFound {
            "directory does not exist".to_owned()
        } else {
            error.to_string()
        },
    })?;
    if !metadata.is_dir() {
        return Err(SourceError::Directory {
            path: directory.to_path_buf(),
            message: "not a directory".to_owned(),
        });
    }
    let entries = fs::read_dir(directory).map_err(|error| SourceError::Directory {
        path: directory.to_path_buf(),
        message: error.to_string(),
    })?;
    let mut images = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| SourceError::Directory {
            path: directory.to_path_buf(),
            message: error.to_string(),
        })?;
        let path = entry.path();
        if path.is_file() && is_supported_image(&path) {
            images.push(path);
        }
    }
    images.sort_by(|left, right| {
        let left = left.to_string_lossy();
        let right = right.to_string_lossy();
        natural_cmp(&left, &right).then_with(|| left.cmp(&right))
    });
    Ok(images)
}

/// Compares ASCII digit runs numerically and everything else byte by byte.
fn natural_cmp(left: &str, right: &str) -> Ordering {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let (mut i, mut j) = (0, 0);
    while i < left.len() && j < right.len() {
        if left[i].is_ascii_digit() && right[j].is_ascii_digit() {
            let left_start = i;
            while i < left.len() && left[i].is_ascii_digit() {
                i += 1;
            }
            let right_start = j;
            while j < right.len() && right[j].is_ascii_digit() {
                j += 1;
            }
            let ordering = digit_run_cmp(&left[left_start..i], &right[right_start..j]);
            if ordering != Ordering::Equal {
                return ordering;
            }
        } else {
            if left[i] != right[j] {
                return left[i].cmp(&right[j]);
            }
            i += 1;
            j += 1;
        }
    }
    (left.len() - i).cmp(&(right.len() - j))
}

fn digit_run_cmp(left: &[u8], right: &[u8]) -> Ordering {
    let left = trim_leading_zeros(left);
    let right = trim_leading_zeros(right);
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

fn trim_leading_zeros(value: &[u8]) -> &[u8] {
    let start = value.iter().position(|digit| *digit != b'0').unwrap_or(value.len());
    &value[start..]
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    fn context(root: &Path) -> SourceContext {
        SourceContext::new(root.join("config"), Some(root.join("home")))
    }

    fn write(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(path, b"not decoded").expect("image file");
    }

    fn names(pool: &SourcePool) -> Vec<String> {
        pool.images()
            .iter()
            .map(|path| path.file_name().expect("file name").to_string_lossy().into_owned())
            .collect()
    }

    fn pool_of(names: &[&str]) -> SourcePool {
        SourcePool::from_images_for_test(names.iter().map(PathBuf::from).collect())
    }

    #[test]
    fn find_pool_entry_matches_name_or_stem_ignoring_case_in_pool_order() {
        let pool = pool_of(&["/p/Sunset.PNG", "/q/sunset.jpg", "/p/sunset", "/p/b.png"]);
        assert_eq!(
            find_pool_entry(&pool, "sunset"),
            Some(0),
            "stem of the first entry"
        );
        assert_eq!(find_pool_entry(&pool, "SUNSET.PNG"), Some(0));
        assert_eq!(find_pool_entry(&pool, "sunset.jpg"), Some(1));
        assert_eq!(find_pool_entry(&pool, "B.png"), Some(3));
        assert_eq!(find_pool_entry(&pool, "b"), Some(3));
        assert_eq!(find_pool_entry(&pool, " b"), None, "the token is never trimmed");
        assert_eq!(find_pool_entry(&pool, "png"), None);
        assert_eq!(find_pool_entry(&pool, ""), None);
        assert_eq!(find_pool_entry(&pool_of(&[]), "a"), None);
        // Duplicate occurrences resolve to the first.
        assert_eq!(
            find_pool_entry(&pool_of(&["/a/x.png", "/a/x.png"]), "x"),
            Some(0)
        );
    }

    #[test]
    fn path_overrides_list_and_resolves_under_the_config_directory() {
        let root = tempfile::tempdir().expect("root");
        let context = context(root.path());
        let images = context.config_directory().join("images");
        write(&images.join("wall.jpg"));
        write(&context.config_directory().join("listed.png"));
        let config = WallpapersConfig {
            path: "images".to_owned(),
            list: vec!["listed.png".to_owned()],
            ..WallpapersConfig::default()
        };
        let pool = resolve_pool(&config, &context).expect("pool");
        assert_eq!(names(&pool), ["wall.jpg"]);
    }

    #[test]
    fn an_invalid_path_never_falls_back_to_the_list() {
        let root = tempfile::tempdir().expect("root");
        let context = context(root.path());
        write(&context.config_directory().join("listed.jpg"));
        let config = WallpapersConfig {
            path: "missing".to_owned(),
            list: vec!["listed.jpg".to_owned()],
            ..WallpapersConfig::default()
        };
        let error = resolve_pool(&config, &context).expect_err("invalid directory");
        assert!(matches!(error, SourceError::Directory { .. }), "{error}");
    }

    #[test]
    fn a_whitespace_only_path_is_invalid_and_never_falls_back_to_the_list() {
        let root = tempfile::tempdir().expect("root");
        let context = context(root.path());
        write(&context.config_directory().join("listed.jpg"));
        let config = WallpapersConfig {
            path: "   ".to_owned(),
            list: vec!["listed.jpg".to_owned()],
            ..WallpapersConfig::default()
        };
        let error = resolve_pool(&config, &context).expect_err("blank directory");
        assert!(matches!(error, SourceError::Directory { .. }), "{error}");
    }

    #[test]
    fn list_preserves_order_and_duplicates_while_filtering_unusable_entries() {
        let root = tempfile::tempdir().expect("root");
        let context = context(root.path());
        let directory = context.config_directory();
        write(&directory.join("b.jpg"));
        write(&directory.join("a.png"));
        fs::create_dir_all(directory.join("fake.jpg")).expect("directory");
        write(&directory.join("notes.txt"));
        write(&directory.join("animation.gif"));
        let config = WallpapersConfig {
            list: vec![
                "b.jpg".to_owned(),
                "missing.jpg".to_owned(),
                "a.png".to_owned(),
                "b.jpg".to_owned(),
                "fake.jpg".to_owned(),
                "notes.txt".to_owned(),
                "animation.gif".to_owned(),
                "  ".to_owned(),
            ],
            ..WallpapersConfig::default()
        };
        let pool = resolve_pool(&config, &context).expect("pool");
        assert_eq!(names(&pool), ["b.jpg", "a.png", "b.jpg"]);
    }

    #[test]
    fn an_all_invalid_list_is_an_empty_pool_rather_than_a_source_failure() {
        let root = tempfile::tempdir().expect("root");
        let context = context(root.path());
        let config = WallpapersConfig {
            list: vec!["missing.jpg".to_owned(), "animation.gif".to_owned()],
            ..WallpapersConfig::default()
        };
        let pool = resolve_pool(&config, &context).expect("empty is successful");
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
        assert_eq!(pool.require_non_empty(), Err(SourceError::EmptyPool));
    }

    #[test]
    fn a_default_config_resolves_an_empty_pool() {
        let root = tempfile::tempdir().expect("root");
        let pool =
            resolve_pool(&WallpapersConfig::default(), &context(root.path())).expect("empty pool");
        assert!(pool.is_empty());
        assert!(pool.into_images().is_empty());
    }

    #[test]
    fn natural_order_handles_two_and_ten_with_a_deterministic_tie_break() {
        let root = tempfile::tempdir().expect("root");
        let context = context(root.path());
        let directory = context.config_directory().join("images");
        for name in ["image10.jpg", "image2.jpg", "image02.jpg"] {
            write(&directory.join(name));
        }
        let config = WallpapersConfig {
            path: "images".to_owned(),
            ..WallpapersConfig::default()
        };
        let first = resolve_pool(&config, &context).expect("pool");
        let second = resolve_pool(&config, &context).expect("pool");
        assert_eq!(names(&first), ["image02.jpg", "image2.jpg", "image10.jpg"]);
        assert_eq!(first, second);
    }

    #[test]
    fn enumeration_is_nonrecursive_and_skips_unsupported_entries() {
        let root = tempfile::tempdir().expect("root");
        let context = context(root.path());
        let directory = context.config_directory().join("images");
        write(&directory.join("real.jpg"));
        write(&directory.join("nested/deep.png"));
        fs::create_dir_all(directory.join("looks-like.jpg")).expect("directory");
        write(&directory.join("clip.gif"));
        let config = WallpapersConfig {
            path: "images".to_owned(),
            ..WallpapersConfig::default()
        };
        let pool = resolve_pool(&config, &context).expect("pool");
        assert_eq!(names(&pool), ["real.jpg"]);
    }

    #[test]
    fn mixed_case_image_extensions_are_supported() {
        let root = tempfile::tempdir().expect("root");
        let context = context(root.path());
        let directory = context.config_directory().join("images");
        for name in ["a.JPG", "b.JpEg", "c.PnG", "d.WEBP", "e.gif", "f.txt"] {
            write(&directory.join(name));
        }
        let config = WallpapersConfig {
            path: "images".to_owned(),
            ..WallpapersConfig::default()
        };
        let pool = resolve_pool(&config, &context).expect("pool");
        assert_eq!(names(&pool), ["a.JPG", "b.JpEg", "c.PnG", "d.WEBP"]);
        assert!(is_supported_image(Path::new("x.WeBp")));
        assert!(!is_supported_image(Path::new("x")));
    }

    #[test]
    fn config_strings_are_trimmed_before_tilde_expansion() {
        let root = tempfile::tempdir().expect("root");
        let context = context(root.path());
        write(&context.home().expect("home").join("a.jpg"));
        let config = WallpapersConfig {
            list: vec!["  ~/a.jpg  ".to_owned()],
            ..WallpapersConfig::default()
        };
        let pool = resolve_pool(&config, &context).expect("pool");
        assert_eq!(names(&pool), ["a.jpg"]);

        let config = WallpapersConfig {
            path: "  ~  ".to_owned(),
            ..WallpapersConfig::default()
        };
        let pool = resolve_pool(&config, &context).expect("home directory");
        assert_eq!(names(&pool), ["a.jpg"]);
    }

    #[test]
    fn tilde_absolute_and_config_relative_sources_resolve() {
        let root = tempfile::tempdir().expect("root");
        let context = context(root.path());
        let home = context.home().expect("home").to_path_buf();
        let absolute = root.path().join("absolute");
        write(&home.join("pics/a.jpg"));
        write(&absolute.join("b.png"));
        write(&context.config_directory().join("c.webp"));

        for (configured, expected) in [
            ("~/pics".to_owned(), home.join("pics")),
            (absolute.to_string_lossy().into_owned(), absolute.clone()),
        ] {
            let config = WallpapersConfig {
                path: configured,
                ..WallpapersConfig::default()
            };
            let pool = resolve_pool(&config, &context).expect("pool");
            assert!(
                pool.images().iter().all(|path| path.starts_with(&expected)),
                "{pool:?}"
            );
        }

        let config = WallpapersConfig {
            list: vec!["c.webp".to_owned()],
            ..WallpapersConfig::default()
        };
        let pool = resolve_pool(&config, &context).expect("pool");
        assert_eq!(names(&pool), ["c.webp"]);
    }

    #[test]
    fn a_tilde_source_without_an_injected_home_is_a_source_failure() {
        let root = tempfile::tempdir().expect("root");
        let context = SourceContext::new(root.path().join("config"), None);
        let config = WallpapersConfig {
            path: "~/pics".to_owned(),
            ..WallpapersConfig::default()
        };
        let error = resolve_pool(&config, &context).expect_err("no home");
        assert!(matches!(error, SourceError::MissingHome { .. }), "{error}");
        assert!(error.to_string().contains("no home directory"));
    }

    #[test]
    fn a_tilde_list_entry_without_an_injected_home_is_filtered() {
        let root = tempfile::tempdir().expect("root");
        let context = SourceContext::new(root.path().join("config"), None);
        write(&context.config_directory().join("listed.jpg"));
        let config = WallpapersConfig {
            list: vec!["~/gone.jpg".to_owned(), "listed.jpg".to_owned()],
            ..WallpapersConfig::default()
        };
        let pool = resolve_pool(&config, &context).expect("pool");
        assert_eq!(names(&pool), ["listed.jpg"]);
    }

    #[test]
    fn a_missing_directory_or_a_regular_file_is_a_source_failure() {
        let root = tempfile::tempdir().expect("root");
        let context = context(root.path());
        let missing = WallpapersConfig {
            path: "missing".to_owned(),
            ..WallpapersConfig::default()
        };
        let error = resolve_pool(&missing, &context).expect_err("missing");
        let SourceError::Directory { message, .. } = error else {
            panic!("expected a directory failure");
        };
        assert_eq!(message, "directory does not exist");

        let file = context.config_directory().join("image.jpg");
        write(&file);
        let not_a_directory = WallpapersConfig {
            path: "image.jpg".to_owned(),
            ..WallpapersConfig::default()
        };
        let error = resolve_pool(&not_a_directory, &context).expect_err("file");
        let SourceError::Directory { message, .. } = error else {
            panic!("expected a directory failure");
        };
        assert_eq!(message, "not a directory");
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_directory_is_a_source_failure() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("root");
        let context = context(root.path());
        let directory = context.config_directory().join("locked");
        write(&directory.join("a.jpg"));
        let original = fs::metadata(&directory).expect("metadata").permissions();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o000)).expect("lock");
        let config = WallpapersConfig {
            path: "locked".to_owned(),
            ..WallpapersConfig::default()
        };
        let result = resolve_pool(&config, &context);
        fs::set_permissions(&directory, original).expect("restore");
        let error = result.expect_err("the directory cannot be read");
        assert!(matches!(error, SourceError::Directory { .. }), "{error}");
    }

    #[test]
    fn explicit_files_ignore_the_configured_pool() {
        let root = tempfile::tempdir().expect("root");
        let context = context(root.path());
        let cwd = root.path().join("cwd");
        write(&cwd.join("chosen.jpg"));
        let config = WallpapersConfig {
            path: "missing".to_owned(),
            ..WallpapersConfig::default()
        };
        assert!(resolve_pool(&config, &context).is_err());
        let chosen = resolve_explicit_file(Path::new("chosen.jpg"), &cwd).expect("explicit file");
        assert_eq!(chosen, cwd.join("chosen.jpg"));
    }

    #[test]
    fn explicit_files_keep_literal_spaces_and_are_not_trimmed() {
        let root = tempfile::tempdir().expect("root");
        let cwd = root.path().join("cwd");
        let spaced = cwd.join("  spaced.jpg");
        write(&spaced);
        let resolved = resolve_explicit_file(Path::new("  spaced.jpg"), &cwd)
            .expect("literal spaces are preserved");
        assert_eq!(resolved, spaced);
        assert!(
            resolve_explicit_file(Path::new("spaced.jpg"), &cwd).is_err(),
            "trimming would have found the wrong file"
        );
    }

    #[test]
    fn explicit_relative_files_use_the_invocation_cwd_and_absolute_files_stay_absolute() {
        let root = tempfile::tempdir().expect("root");
        let cwd = root.path().join("cwd");
        let other = root.path().join("other");
        write(&cwd.join("local.png"));
        write(&other.join("absolute.png"));
        assert_eq!(
            resolve_explicit_file(Path::new("local.png"), &cwd).expect("relative"),
            cwd.join("local.png")
        );
        let absolute = other.join("absolute.png");
        assert_eq!(
            resolve_explicit_file(&absolute, &cwd).expect("absolute"),
            absolute
        );
    }

    #[test]
    fn an_empty_explicit_argument_is_an_argument_failure() {
        let root = tempfile::tempdir().expect("root");
        let error = resolve_explicit_file(Path::new(""), root.path()).expect_err("empty");
        assert!(matches!(error, SourceError::InvalidArgument { .. }), "{error}");
    }

    #[test]
    fn explicit_files_must_exist_be_regular_and_have_an_image_extension() {
        let root = tempfile::tempdir().expect("root");
        let cwd = root.path().join("cwd");
        let missing = resolve_explicit_file(Path::new("missing.jpg"), &cwd).expect_err("missing");
        assert!(matches!(missing, SourceError::File { .. }), "{missing}");

        fs::create_dir_all(cwd.join("directory.jpg")).expect("directory");
        let directory =
            resolve_explicit_file(Path::new("directory.jpg"), &cwd).expect_err("directory");
        assert!(matches!(directory, SourceError::File { .. }), "{directory}");

        write(&cwd.join("notes.txt"));
        let unsupported =
            resolve_explicit_file(Path::new("notes.txt"), &cwd).expect_err("unsupported");
        assert!(unsupported.to_string().contains("unsupported image extension"));
    }

    #[test]
    fn natural_comparison_is_numeric_and_stable() {
        assert_eq!(natural_cmp("image2.jpg", "image10.jpg"), Ordering::Less);
        assert_eq!(natural_cmp("image10.jpg", "image2.jpg"), Ordering::Greater);
        assert_eq!(natural_cmp("image02.jpg", "image2.jpg"), Ordering::Equal);
        assert_eq!(natural_cmp("a.jpg", "a.jpg"), Ordering::Equal);
        assert_eq!(natural_cmp("a.jpg", "a.jpg.bak"), Ordering::Less);
        assert_eq!(digit_run_cmp(b"000", b"0"), Ordering::Equal);
    }
}
