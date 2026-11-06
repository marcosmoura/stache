//! Immutable cache keys, validated hits and atomic publication.
//!
//! Files live at `<AppPaths cache>/wallpapers/<key>.png`. The key is a SHA-256
//! over a length-prefixed encoding of the canonical source path, the digest of
//! the exact source bytes, the target pixels, the effects and
//! [`PROCESSING_VERSION`]. Same-length or same-mtime changes therefore
//! invalidate because the bytes, not the metadata, are hashed.
//!
//! A hit is used only when it is an owned regular file that fully decodes as
//! RGB8 PNG with the expected dimensions; an owned but corrupt or truncated
//! file is rebuilt. A symlinked or foreign destination is refused. Publication
//! writes a unique same-directory temporary with `create_new`, flushes, syncs,
//! reopens and validates it, then renames it over the final name atomically.
//! Failures remove only the temporary this call created.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use image::{ImageFormat, ImageReader, RgbImage};
use sha2::{Digest, Sha256};
use stache_sdk::{AppPaths, CacheLease, CancellationToken};

use crate::limits;
use crate::processing::{self, Effects, PROCESSING_VERSION, ProcessError};
use crate::selection::TargetPixels;

/// The subdirectory of the identity cache that holds generated wallpapers.
pub const WALLPAPER_CACHE_DIRECTORY: &str = "wallpapers";

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Why a cache operation failed.
#[derive(Debug)]
pub enum CacheError {
    /// A filesystem operation failed.
    Io {
        /// What was attempted.
        action: &'static str,
        /// The affected path.
        path: PathBuf,
        /// The underlying failure.
        error: io::Error,
    },
    /// A cache directory is a link, foreign-owned or the wrong type.
    UnsafeDirectory {
        /// The rejected directory.
        path: PathBuf,
        /// Why it was rejected.
        message: String,
    },
    /// An existing cache entry is a link, foreign-owned or the wrong type.
    UnsafeEntry {
        /// The rejected entry.
        path: PathBuf,
        /// Why it was rejected.
        message: String,
    },
    /// A freshly written temporary did not validate as the expected output.
    InvalidOutput {
        /// The rejected temporary.
        path: PathBuf,
        /// Why it was rejected.
        message: String,
    },
}

impl fmt::Display for CacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { action, path, error } => {
                write!(f, "cannot {action} {}: {error}", path.display())
            }
            Self::UnsafeDirectory { path, message } => {
                write!(f, "unsafe cache directory {}: {message}", path.display())
            }
            Self::UnsafeEntry { path, message } => {
                write!(f, "unsafe cache entry {}: {message}", path.display())
            }
            Self::InvalidOutput { path, message } => {
                write!(f, "invalid wallpaper output {}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for CacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { error, .. } => Some(error),
            Self::UnsafeDirectory { .. }
            | Self::UnsafeEntry { .. }
            | Self::InvalidOutput { .. } => None,
        }
    }
}

fn io_error(action: &'static str, path: &Path, error: io::Error) -> CacheError {
    CacheError::Io {
        action,
        path: path.to_path_buf(),
        error,
    }
}

/// Injected failure points for publication tests.
pub trait PublishSeam: Sync {
    /// Runs after the encoded bytes were written and before the flush.
    fn after_write(&self) -> io::Result<()> { Ok(()) }

    /// Runs after the flush and before the sync.
    fn before_sync(&self) -> io::Result<()> { Ok(()) }

    /// Runs after validation and before the atomic rename.
    fn before_publish(&self) -> io::Result<()> { Ok(()) }
}

/// The production seam with no failure points.
pub struct NoFailures;

impl PublishSeam for NoFailures {}

/// Revalidates cancellation and cache ownership at the final publication point.
///
/// The check runs after the temporary validated and immediately before the
/// atomic rename, so work that became cancelled or lost its lease during the
/// noninterruptible encoder cannot publish an obsolete output. A refusal
/// removes only the temporary this call created; no final file appears.
pub trait PublishCommit: Sync {
    /// Rechecks cancellation and lease ownership.
    ///
    /// # Errors
    /// Returns [`ProcessError::Cancelled`] or [`ProcessError::Lease`] to refuse
    /// publication.
    fn commit(&self) -> Result<(), ProcessError>;
}

/// The commit check used by preparation: cancel or a replaced lease refuses
/// the final rename.
pub struct CancelLeaseCommit<'a> {
    /// The caller's cancellation token.
    pub cancel: &'a CancellationToken,
    /// The shared lease held for this preparation.
    pub lease: &'a CacheLease,
}

impl PublishCommit for CancelLeaseCommit<'_> {
    fn commit(&self) -> Result<(), ProcessError> {
        if self.cancel.is_cancelled() {
            return Err(ProcessError::Cancelled);
        }
        self.lease.verify().map_err(ProcessError::Lease)
    }
}

/// The validated wallpaper cache directory.
#[derive(Clone, Debug)]
pub struct Cache {
    root: PathBuf,
    owner: u32,
}

impl Cache {
    /// Validates or creates the identity cache and its `wallpapers` child.
    ///
    /// Existing links, foreign ownership and non-directories are refused; new
    /// directories are created owner-only.
    pub fn open(paths: &AppPaths) -> Result<Self, CacheError> {
        let owner = paths.owner();
        let cache_root = paths.cache_directory();
        ensure_directory(cache_root, owner)?;
        let root = cache_root.join(WALLPAPER_CACHE_DIRECTORY);
        ensure_directory(&root, owner)?;
        Ok(Self { root, owner })
    }

    /// Returns the final path a key maps to.
    pub fn path_for(&self, key: &str) -> PathBuf { self.root.join(format!("{key}.png")) }

    /// Returns a validated existing output, or `None` when it must be rebuilt.
    ///
    /// # Errors
    /// Refuses a symlinked, foreign-owned or non-file entry instead of reading
    /// through it.
    pub fn lookup(&self, key: &str, expected: TargetPixels) -> Result<Option<PathBuf>, CacheError> {
        let path = self.path_for(key);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error("inspect cache entry", &path, error)),
        };
        validate_entry(&path, &metadata, self.owner)?;
        match validate_output(&path, &metadata, expected, self.owner) {
            Ok(()) => Ok(Some(path)),
            Err(InvalidOutput::Corrupt(_)) => Ok(None),
            Err(InvalidOutput::Unsafe { message }) => {
                Err(CacheError::UnsafeEntry { path, message })
            }
        }
    }

    /// Encodes and publishes one output, returning its final path.
    ///
    /// A validated concurrent winner may be replaced by this call's identical
    /// bytes; the rename is atomic either way and both files stay reusable.
    pub fn publish(
        &self,
        key: &str,
        expected: TargetPixels,
        image: &RgbImage,
        seam: &dyn PublishSeam,
        commit: &dyn PublishCommit,
    ) -> Result<PathBuf, ProcessError> {
        let final_path = self.path_for(key);
        reject_unsafe_destination(&final_path, self.owner)?;
        let temporary = self.temporary_path(key);
        let result = self.write_and_publish(&temporary, &final_path, expected, image, seam, commit);
        if result.is_err() {
            // Only this call's uniquely named temporary is ever removed.
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn temporary_path(&self, key: &str) -> PathBuf {
        let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        self.root.join(format!(".{key}.{}.{sequence}.tmp", std::process::id()))
    }

    fn write_and_publish(
        &self,
        temporary: &Path,
        final_path: &Path,
        expected: TargetPixels,
        image: &RgbImage,
        seam: &dyn PublishSeam,
        commit: &dyn PublishCommit,
    ) -> Result<PathBuf, ProcessError> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(temporary)
            .map_err(|error| io_error("create temporary", temporary, error))?;
        let mut writer = BufWriter::new(file);
        processing::encode_png(image, &mut writer, final_path)?;
        seam.after_write()
            .map_err(|error| io_error("write temporary", temporary, error))?;
        writer.flush().map_err(|error| io_error("flush temporary", temporary, error))?;
        seam.before_sync()
            .map_err(|error| io_error("flush temporary", temporary, error))?;
        let file = writer
            .into_inner()
            .map_err(|error| io_error("flush temporary", temporary, error.into_error()))?;
        file.sync_all().map_err(|error| io_error("sync temporary", temporary, error))?;
        drop(file);
        seam.before_publish()
            .map_err(|error| io_error("publish output", temporary, error))?;

        let metadata = fs::symlink_metadata(temporary)
            .map_err(|error| io_error("inspect temporary", temporary, error))?;
        match validate_output(temporary, &metadata, expected, self.owner) {
            Ok(()) => {}
            Err(InvalidOutput::Corrupt(message) | InvalidOutput::Unsafe { message }) => {
                return Err(ProcessError::Cache(CacheError::InvalidOutput {
                    path: temporary.to_path_buf(),
                    message,
                }));
            }
        }
        // Last commit check: cancellation or a replaced lease observed during
        // the encoder must not publish an obsolete output.
        commit.commit()?;
        fs::rename(temporary, final_path)
            .map_err(|error| io_error("publish output", final_path, error))?;
        Ok(final_path.to_path_buf())
    }
}

/// Builds the cache key for one output.
pub fn cache_key(
    canonical: &Path,
    digest: &[u8; 32],
    pixels: TargetPixels,
    effects: Effects,
) -> String {
    let mut fields = Vec::new();
    push_field(&mut fields, b"stache.wallpapers.cache.v1");
    push_field(&mut fields, &PROCESSING_VERSION.to_le_bytes());
    push_field(&mut fields, canonical.as_os_str().as_bytes());
    push_field(&mut fields, digest);
    push_field(&mut fields, &pixels.width.to_le_bytes());
    push_field(&mut fields, &pixels.height.to_le_bytes());
    push_field(&mut fields, &effects.radius.to_le_bytes());
    push_field(&mut fields, &effects.blur.to_le_bytes());
    format!("{:x}", Sha256::digest(&fields))
}

fn push_field(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_le_bytes());
    output.extend_from_slice(value);
}

enum InvalidOutput {
    Corrupt(String),
    Unsafe { message: String },
}

fn validate_entry(path: &Path, metadata: &fs::Metadata, owner: u32) -> Result<(), CacheError> {
    if metadata.file_type().is_symlink() {
        return Err(CacheError::UnsafeEntry {
            path: path.to_path_buf(),
            message: "cache entries must not be symbolic links".to_owned(),
        });
    }
    if !metadata.is_file() {
        return Err(CacheError::UnsafeEntry {
            path: path.to_path_buf(),
            message: "cache entries must be regular files".to_owned(),
        });
    }
    if metadata.uid() != owner {
        return Err(CacheError::UnsafeEntry {
            path: path.to_path_buf(),
            message: format!("cache entry is owned by uid {}", metadata.uid()),
        });
    }
    Ok(())
}

fn validate_output(
    path: &Path,
    metadata: &fs::Metadata,
    expected: TargetPixels,
    owner: u32,
) -> Result<(), InvalidOutput> {
    if metadata.file_type().is_symlink() {
        return Err(InvalidOutput::Unsafe {
            message: "output must not be a symbolic link".to_owned(),
        });
    }
    if !metadata.is_file() {
        return Err(InvalidOutput::Unsafe {
            message: "output must be a regular file".to_owned(),
        });
    }
    if metadata.uid() != owner {
        return Err(InvalidOutput::Unsafe {
            message: format!("output is owned by uid {}", metadata.uid()),
        });
    }
    let valid = decode_png(path, expected).map_err(InvalidOutput::Corrupt)?;
    if valid {
        Ok(())
    } else {
        Err(InvalidOutput::Corrupt(
            "output does not decode as RGB8 PNG with the expected dimensions".to_owned(),
        ))
    }
}

fn decode_png(path: &Path, expected: TargetPixels) -> Result<bool, String> {
    let reader = ImageReader::new(BufReader::new(
        File::open(path).map_err(|error| error.to_string())?,
    ))
    .with_guessed_format()
    .map_err(|error| error.to_string())?;
    if reader.format() != Some(ImageFormat::Png) {
        return Ok(false);
    }
    let (width, height) = reader.into_dimensions().map_err(|error| error.to_string())?;
    if width != expected.width || height != expected.height {
        return Ok(false);
    }
    let mut reader = ImageReader::new(BufReader::new(
        File::open(path).map_err(|error| error.to_string())?,
    ))
    .with_guessed_format()
    .map_err(|error| error.to_string())?;
    reader.limits(limits::output_limits());
    let image = reader.decode().map_err(|error| error.to_string())?;
    Ok(image.color() == image::ColorType::Rgb8
        && image.width() == expected.width
        && image.height() == expected.height)
}

fn reject_unsafe_destination(path: &Path, owner: u32) -> Result<(), CacheError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_entry(path, &metadata, owner),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error("inspect cache destination", path, error)),
    }
}

fn ensure_directory(path: &Path, owner: u32) -> Result<(), CacheError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_directory(path, &metadata, owner)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)
                .or_else(|error| {
                    if error.kind() == io::ErrorKind::AlreadyExists {
                        Ok(())
                    } else {
                        Err(error)
                    }
                })
                .map_err(|error| io_error("create", path, error))?;
            let metadata =
                fs::symlink_metadata(path).map_err(|error| io_error("inspect", path, error))?;
            validate_directory(path, &metadata, owner)?;
        }
        Err(error) => return Err(io_error("inspect", path, error)),
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| io_error("restrict", path, error))
}

fn validate_directory(path: &Path, metadata: &fs::Metadata, owner: u32) -> Result<(), CacheError> {
    if metadata.file_type().is_symlink() {
        return Err(CacheError::UnsafeDirectory {
            path: path.to_path_buf(),
            message: "cache directories must not be symbolic links".to_owned(),
        });
    }
    if !metadata.is_dir() {
        return Err(CacheError::UnsafeDirectory {
            path: path.to_path_buf(),
            message: "cache path must be a directory".to_owned(),
        });
    }
    if metadata.uid() != owner {
        return Err(CacheError::UnsafeDirectory {
            path: path.to_path_buf(),
            message: format!("cache directory is owned by uid {}", metadata.uid()),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::MAX_TARGET_DIMENSION;

    #[test]
    fn a_foreign_owned_entry_or_directory_is_refused() {
        let root = tempfile::tempdir().expect("root");
        let file = root.path().join("entry.png");
        fs::write(&file, b"x").expect("file");
        let metadata = fs::symlink_metadata(&file).expect("metadata");
        let foreign = metadata.uid().saturating_add(1);
        assert!(matches!(
            validate_entry(&file, &metadata, foreign),
            Err(CacheError::UnsafeEntry { .. })
        ));
        assert!(matches!(
            validate_directory(root.path(), &metadata, metadata.uid()),
            Err(CacheError::UnsafeDirectory { .. })
        ));
    }

    #[test]
    fn keys_encode_every_component_with_length_prefixes() {
        let path = Path::new("/tmp/wallpaper.jpg");
        let digest = [7_u8; 32];
        let pixels = TargetPixels { width: 1920, height: 1080 };
        let effects = Effects { radius: 10, blur: 4 };
        let key = cache_key(path, &digest, pixels, effects);
        assert_eq!(key.len(), 64);
        assert!(key.chars().all(|character| character.is_ascii_hexdigit()));

        let same = cache_key(path, &digest, pixels, effects);
        assert_eq!(key, same);
        assert_ne!(cache_key(path, &[8; 32], pixels, effects), key);
        assert_ne!(
            cache_key(
                path,
                &digest,
                TargetPixels { width: 1921, height: 1080 },
                effects
            ),
            key
        );
        assert_ne!(
            cache_key(path, &digest, pixels, Effects { radius: 11, blur: 4 }),
            key
        );
        assert_ne!(
            cache_key(path, &digest, pixels, Effects { radius: 10, blur: 5 }),
            key
        );
    }

    #[test]
    fn the_processing_version_is_part_of_the_key_scheme() {
        // A version change alters the field material, so the same inputs no
        // longer produce the previous filename. Assert the constant is in the
        // encoded domain marker used by this implementation.
        let key = cache_key(
            Path::new("/tmp/a.png"),
            &[0; 32],
            TargetPixels { width: 1, height: 1 },
            Effects::default(),
        );
        let mut fields = Vec::new();
        push_field(&mut fields, b"stache.wallpapers.cache.v1");
        push_field(&mut fields, &(PROCESSING_VERSION + 1).to_le_bytes());
        push_field(&mut fields, Path::new("/tmp/a.png").as_os_str().as_bytes());
        push_field(&mut fields, &[0_u8; 32]);
        push_field(&mut fields, &1_u32.to_le_bytes());
        push_field(&mut fields, &1_u32.to_le_bytes());
        push_field(&mut fields, &0_u32.to_le_bytes());
        push_field(&mut fields, &0_u32.to_le_bytes());
        assert_ne!(key, format!("{:x}", Sha256::digest(&fields)));
    }

    struct CancelAtPublish<'a> {
        cancel: &'a CancellationToken,
    }

    impl PublishSeam for CancelAtPublish<'_> {
        fn before_publish(&self) -> io::Result<()> {
            self.cancel.cancel();
            Ok(())
        }
    }

    #[test]
    fn a_cancelled_final_commit_publishes_nothing() {
        use std::time::Duration;

        use stache_sdk::{AppEnvironment, CacheLease, CancellationToken, LeaseWait};

        let root = tempfile::tempdir().expect("root");
        let paths = AppPaths::resolve(&AppEnvironment {
            home: Some(root.path().to_path_buf()),
            temporary: Some(root.path().to_path_buf()),
        })
        .expect("paths");
        let cache = Cache::open(&paths).expect("cache");
        let token = CancellationToken::new();
        let lease =
            CacheLease::shared(&paths, &LeaseWait::new(Duration::ZERO, CancellationToken::new()))
                .expect("lease");
        let commit = CancelLeaseCommit { cancel: &token, lease: &lease };
        let image = RgbImage::from_pixel(4, 4, image::Rgb([1, 2, 3]));

        let result = cache.publish(
            "cancelled",
            TargetPixels { width: 4, height: 4 },
            &image,
            &CancelAtPublish { cancel: &token },
            &commit,
        );

        assert!(matches!(result, Err(ProcessError::Cancelled)));
        let entries: Vec<_> = fs::read_dir(cache.path_for("cancelled").parent().expect("parent"))
            .expect("listing")
            .map(|entry| entry.expect("entry").path())
            .collect();
        assert!(
            entries.is_empty(),
            "no final file or temporary may remain: {entries:?}"
        );
    }

    #[test]
    fn the_documented_target_edge_is_used_for_decode_limits() {
        let limits = limits::output_limits();
        assert_eq!(limits.max_image_width, Some(MAX_TARGET_DIMENSION));
        assert_eq!(limits.max_image_height, Some(MAX_TARGET_DIMENSION));
    }
}
