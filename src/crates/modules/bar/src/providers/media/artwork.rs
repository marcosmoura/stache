//! Artwork decoding, the content id and the private on-disk cache.
//!
//! # Pipeline (legacy `media.rs`, tightened)
//!
//! `artworkData` (base64) plus `artworkMimeType` become a 128x128 PNG:
//!
//! 1. a `<`-prefixed value is a textual placeholder and is rejected;
//! 2. the MIME maps to PNG, JPEG, WebP, GIF or BMP (anything else is rejected);
//! 3. the encoded text is capped at 16 MiB and the decoded bytes at 12 MiB; the
//!    decoded size is derived from the text length first, so an oversized value
//!    is rejected before any allocation;
//! 4. the header dimensions are read first: at most 8192 per axis and 16 Mi
//!    pixels, rejected before the pixel decode (decompression bombs);
//!    the decoder also runs under an allocation limit;
//! 5. center-crop to a square, Lanczos3 resize to 128x128, encode PNG.
//!
//! The artwork id is the lowercase hex SHA-256 of the final PNG bytes, so equal
//! artwork shares one cache entry and two tracks with the same title and artist
//! can never collide (the legacy file name was derived from them).
//!
//! # Cache
//!
//! [`ArtworkStore`] keeps `<id>.txt` files holding the PNG as base64 text (the
//! legacy format) in one Next-identity directory that the app injects. Writes
//! are atomic (a temp file in the same directory, then rename), the directory
//! is `0700` and files `0600`, no symlink is followed for a write or a read,
//! reads are size-bounded and verified against the id, the entry count is
//! bounded by pruning the oldest, and a request id must be exactly 64 lowercase
//! hex digits before the filesystem is touched. Nothing here reads the legacy
//! cache.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Cursor, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use image::{ImageFormat, ImageReader, Limits};
use sha2::{Digest, Sha256};

/// The side of the published square artwork.
pub const ARTWORK_SIZE: u32 = 128;
/// The largest base64 text accepted.
pub const MAX_ENCODED_BYTES: usize = 16 * 1024 * 1024;
/// The largest decoded image accepted.
pub const MAX_DECODED_BYTES: usize = 12 * 1024 * 1024;
/// The largest width or height accepted.
pub const MAX_DIMENSION: u32 = 8192;
/// The largest pixel count accepted.
pub const MAX_PIXELS: u64 = 16 * 1024 * 1024;
/// The allocation limit of the pixel decoder (an RGBA image at the pixel cap).
const MAX_DECODE_ALLOC: u64 = 80 * 1024 * 1024;
/// The length of an artwork id: lowercase hex SHA-256.
pub const ID_LEN: usize = 64;
/// The most `.txt` entries kept on disk.
pub const MAX_ENTRIES: usize = 64;
/// The largest cache file read; a 128x128 PNG is far smaller.
pub const MAX_CACHE_FILE_BYTES: u64 = 512 * 1024;
/// The entries kept in memory.
const MEMORY_ENTRIES: usize = 4;

/// Why artwork was not produced. The player supplies the input, so every
/// variant is an expected outcome, never a failure of the Bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtworkError {
    /// The value is a textual placeholder.
    Placeholder,
    /// The MIME type is not a supported image type.
    UnsupportedMime,
    /// The encoded or decoded size is over its cap.
    TooLarge,
    /// The text is not valid base64.
    BadBase64,
    /// The header is unreadable or the dimensions are over the caps.
    Dimensions,
    /// The pixel decode or the PNG encode failed.
    Decode,
}

/// Maps a MIME type to an image format.
#[must_use]
pub fn format_from_mime(mime: &str) -> Option<ImageFormat> {
    match mime.trim().to_ascii_lowercase().as_str() {
        "image/png" | "image/x-png" => Some(ImageFormat::Png),
        "image/jpeg" | "image/jpg" | "image/pjpeg" => Some(ImageFormat::Jpeg),
        "image/webp" => Some(ImageFormat::WebP),
        "image/gif" => Some(ImageFormat::Gif),
        "image/bmp" | "image/x-bmp" | "image/x-ms-bmp" => Some(ImageFormat::Bmp),
        _ => None,
    }
}

/// The decoded size a base64 text of `encoded` bytes can produce at most.
const fn decoded_upper_bound(encoded: usize) -> usize { encoded / 4 * 3 + 3 }

/// Turns the player's artwork into the 128x128 PNG bytes.
///
/// # Errors
/// Returns the [`ArtworkError`] that rejected the input.
pub fn process(data: &str, mime: &str) -> Result<Vec<u8>, ArtworkError> {
    if data.starts_with('<') {
        return Err(ArtworkError::Placeholder);
    }
    let format = format_from_mime(mime).ok_or(ArtworkError::UnsupportedMime)?;
    if data.len() > MAX_ENCODED_BYTES || decoded_upper_bound(data.len()) > MAX_DECODED_BYTES + 3 {
        return Err(ArtworkError::TooLarge);
    }
    let bytes = STANDARD.decode(data).map_err(|_| ArtworkError::BadBase64)?;
    if bytes.len() > MAX_DECODED_BYTES {
        return Err(ArtworkError::TooLarge);
    }
    let (width, height) = ImageReader::with_format(Cursor::new(&bytes), format)
        .into_dimensions()
        .map_err(|_| ArtworkError::Dimensions)?;
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
        return Err(ArtworkError::Dimensions);
    }
    let mut reader = ImageReader::with_format(Cursor::new(&bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let image = reader.decode().map_err(|_| ArtworkError::Decode)?;
    drop(bytes);
    resize(&image)
}

/// Center-crops to a square, resizes to 128x128 and encodes PNG.
fn resize(image: &image::DynamicImage) -> Result<Vec<u8>, ArtworkError> {
    let (width, height) = (image.width(), image.height());
    let side = width.min(height);
    let cropped = if width == height {
        image.clone()
    } else {
        image.crop_imm((width - side) / 2, (height - side) / 2, side, side)
    };
    let resized =
        cropped.resize_exact(ARTWORK_SIZE, ARTWORK_SIZE, image::imageops::FilterType::Lanczos3);
    let mut png = Vec::with_capacity(8 * 1024);
    image::DynamicImage::ImageRgba8(resized.to_rgba8())
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .map_err(|_| ArtworkError::Decode)?;
    Ok(png)
}

/// The artwork id of final PNG bytes: lowercase hex SHA-256.
#[must_use]
pub fn artwork_id(png: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(png).iter().fold(String::with_capacity(ID_LEN), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Whether `id` is exactly 64 lowercase hex digits.
#[must_use]
pub fn is_valid_id(id: &str) -> bool {
    id.len() == ID_LEN && id.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// What the store knows about an id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArtworkLookup {
    /// The base64 PNG text.
    Found(String),
    /// Unknown, invalid, unreadable or corrupt.
    Missing,
}

/// Artwork storage: a few entries in memory and a bounded private directory.
pub struct ArtworkStore {
    directory: Option<PathBuf>,
    memory: Mutex<VecDeque<(String, String)>>,
    nonce: AtomicU64,
}

impl std::fmt::Debug for ArtworkStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtworkStore")
            .field("directory", &self.directory)
            .finish_non_exhaustive()
    }
}

impl ArtworkStore {
    /// A store over `directory` (created lazily on the first write); `None`
    /// keeps entries in memory only.
    #[must_use]
    pub const fn new(directory: Option<PathBuf>) -> Self {
        Self {
            directory,
            memory: Mutex::new(VecDeque::new()),
            nonce: AtomicU64::new(0),
        }
    }

    /// The entries held in memory (test introspection).
    #[cfg(test)]
    pub(super) fn memory_len(&self) -> usize {
        self.memory.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// The cache directory, if any.
    #[must_use]
    pub fn directory(&self) -> Option<&Path> { self.directory.as_deref() }

    fn remember(&self, id: &str, text: &str) {
        let mut memory = self.memory.lock().unwrap_or_else(PoisonError::into_inner);
        memory.retain(|(held, _)| held != id);
        memory.push_back((id.to_owned(), text.to_owned()));
        while memory.len() > MEMORY_ENTRIES {
            memory.pop_front();
        }
    }

    /// Stores `png` and returns its id. A disk failure still leaves the entry
    /// in memory and is returned beside the id.
    pub fn put(&self, png: &[u8]) -> (String, Option<io::Error>) {
        let id = artwork_id(png);
        let text = STANDARD.encode(png);
        self.remember(&id, &text);
        let failure = self.directory.as_deref().and_then(|dir| self.write(dir, &id, &text).err());
        (id, failure)
    }

    fn write(&self, dir: &Path, id: &str, text: &str) -> io::Result<()> {
        ensure_private_directory(dir)?;
        let target = dir.join(format!("{id}.txt"));
        if let Ok(metadata) = fs::symlink_metadata(&target)
            && metadata.file_type().is_file()
        {
            return Ok(());
        }
        let nonce = self.nonce.fetch_add(1, Ordering::Relaxed);
        let temp = dir.join(format!(".{id}.{}.{nonce}.tmp", std::process::id()));
        let written = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&temp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            drop(file);
            // A rename replaces a symlink at the target instead of following it.
            fs::rename(&temp, &target)
        })();
        if written.is_err() {
            let _ = fs::remove_file(&temp);
        }
        written?;
        prune(dir, MAX_ENTRIES);
        Ok(())
    }

    /// Looks `id` up: memory, then the directory. An invalid id never touches
    /// the filesystem.
    #[must_use]
    pub fn get(&self, id: &str) -> ArtworkLookup {
        if !is_valid_id(id) {
            return ArtworkLookup::Missing;
        }
        {
            let memory = self.memory.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((_, text)) = memory.iter().find(|(held, _)| held == id) {
                return ArtworkLookup::Found(text.clone());
            }
        }
        let Some(dir) = self.directory.as_deref() else {
            return ArtworkLookup::Missing;
        };
        let Some(text) = read_entry(&dir.join(format!("{id}.txt")), id) else {
            return ArtworkLookup::Missing;
        };
        self.remember(id, &text);
        ArtworkLookup::Found(text)
    }
}

/// Creates `dir` (and parents) if needed and requires a real, private
/// directory: a symlink is refused.
fn ensure_private_directory(dir: &Path) -> io::Result<()> {
    if let Some(parent) = dir.parent() {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?;
    }
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(dir)?;
    if !metadata.file_type().is_dir() {
        return Err(io::Error::other(
            "the artwork cache path is not a real directory",
        ));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Reads and verifies one entry; `None` for anything unsafe or wrong.
fn read_entry(path: &Path, id: &str) -> Option<String> {
    let file: File =
        OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_CACHE_FILE_BYTES {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_CACHE_FILE_BYTES + 1).read_to_string(&mut text).ok()?;
    if text.len() as u64 > MAX_CACHE_FILE_BYTES {
        return None;
    }
    let png = STANDARD.decode(text.trim_end()).ok()?;
    (artwork_id(&png) == id).then(|| text.trim_end().to_owned())
}

/// Removes the oldest `.txt` entries beyond `keep` and stale temp files.
fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if !metadata.file_type().is_file() {
            continue;
        }
        let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
        let extension = Path::new(name).extension().and_then(|ext| ext.to_str());
        if extension.is_some_and(|ext| ext.eq_ignore_ascii_case("txt")) {
            files.push((modified, entry.path()));
        } else if name.starts_with('.')
            && extension.is_some_and(|ext| ext.eq_ignore_ascii_case("tmp"))
            && modified.elapsed().is_ok_and(|age| age.as_secs() > 60)
        {
            let _ = fs::remove_file(entry.path());
        }
    }
    if files.len() <= keep {
        return;
    }
    files.sort();
    let excess = files.len() - keep;
    for (_, path) in files.into_iter().take(excess) {
        let _ = fs::remove_file(path);
    }
}
