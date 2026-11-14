//! One-shot bounded source reads that bind a key to the bytes it describes.
//!
//! A job resolves the canonical source path, checks it without following a
//! final link, reads at most [`MAX_SOURCE_BYTES`] from a read-only descriptor,
//! and hashes exactly the bytes it keeps for decoding. Device, inode, length
//! and modification time are compared before and after the read on both the
//! descriptor and the path, so a replacement during the read is reported
//! instead of producing a key for one file and an output for another. Sources
//! are never written, renamed or removed.
//!
//! [`inspect_source`] runs the same pre-open checks without opening the file.
//! The host uses it with its per-process memo: when the fresh fingerprint
//! (device, inode, length, modification and change time) matches the one
//! recorded after an earlier read, the recorded digest forms the cache key and
//! the file is read only if that output must be rebuilt.

use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::limits::{self, MAX_SOURCE_BYTES};
use crate::memo::FileFingerprint;
use crate::processing::ProcessError;

/// The bytes, digest and canonical identity of one source read.
#[derive(Debug)]
pub struct SourceBytes {
    /// The exact bytes that were hashed and will be decoded.
    pub bytes: Vec<u8>,
    /// SHA-256 of [`Self::bytes`].
    pub digest: [u8; 32],
    /// The canonical source path used in the cache key.
    pub canonical: PathBuf,
    /// The path's no-follow fingerprint, when it was identical before and
    /// after the read, including the change time.
    pub fingerprint: Option<FileFingerprint>,
}

/// Canonicalizes and inspects a source without opening it.
///
/// Runs exactly the checks [`read_source`] runs before it opens the file
/// (resolution, no-follow regular-file check and byte limit) and fails with the
/// same typed errors, returning the canonical path and its fresh metadata.
pub fn inspect_source(path: &Path) -> Result<(PathBuf, fs::Metadata), ProcessError> {
    let canonical = fs::canonicalize(path).map_err(|error| ProcessError::Io {
        action: "resolve source",
        path: path.to_path_buf(),
        error,
    })?;
    let metadata = fs::symlink_metadata(&canonical).map_err(|error| ProcessError::Io {
        action: "inspect source",
        path: canonical.clone(),
        error,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ProcessError::NotRegularFile { path: canonical });
    }
    limits::check_source_len(&canonical, metadata.len())?;
    Ok((canonical, metadata))
}

/// Reads a source with the production byte bound.
pub fn read_source(path: &Path) -> Result<SourceBytes, ProcessError> {
    read_source_with(path, MAX_SOURCE_BYTES, &|_| {})
}

/// Reads a source with an injected byte bound and pre-read hook.
pub fn read_source_with(
    path: &Path,
    limit: u64,
    before_read: &dyn Fn(&Path),
) -> Result<SourceBytes, ProcessError> {
    let (canonical, path_before) = inspect_source(path)?;
    let mut file = fs::File::open(&canonical).map_err(|error| ProcessError::Io {
        action: "open source",
        path: canonical.clone(),
        error,
    })?;
    let descriptor_before = file.metadata().map_err(|error| ProcessError::Io {
        action: "inspect source",
        path: canonical.clone(),
        error,
    })?;
    if fingerprint(&descriptor_before) != fingerprint(&path_before) {
        return Err(ProcessError::SourceChanged { path: canonical });
    }

    before_read(&canonical);

    // A file that grew after the metadata check is truncated at one byte past
    // the limit so the oversize read fails without allocating the whole file.
    let capacity = usize::try_from(descriptor_before.len().min(limit)).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    let mut hasher = Sha256::new();
    let mut chunk = vec![0_u8; 64 * 1024];
    {
        let mut reader = (&mut file).take(limit.saturating_add(1));
        loop {
            let read = reader.read(&mut chunk).map_err(|error| ProcessError::Io {
                action: "read source",
                path: canonical.clone(),
                error,
            })?;
            if read == 0 {
                break;
            }
            hasher.update(&chunk[..read]);
            bytes.extend_from_slice(&chunk[..read]);
            if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
                return Err(ProcessError::Resource(limits::ResourceError::SourceBytes {
                    path: canonical,
                    length: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                    limit,
                }));
            }
        }
    }

    let descriptor_after = file.metadata().map_err(|error| ProcessError::Io {
        action: "inspect source",
        path: canonical.clone(),
        error,
    })?;
    let path_after = fs::symlink_metadata(&canonical).map_err(|error| ProcessError::Io {
        action: "inspect source",
        path: canonical.clone(),
        error,
    })?;
    if path_after.file_type().is_symlink() || !path_after.is_file() {
        return Err(ProcessError::SourceChanged { path: canonical });
    }
    if fingerprint(&descriptor_before) != fingerprint(&descriptor_after)
        || fingerprint(&path_before) != fingerprint(&path_after)
    {
        return Err(ProcessError::SourceChanged { path: canonical });
    }

    let digest: [u8; 32] = hasher.finalize().into();
    let before = FileFingerprint::of(&path_before);
    let fingerprint = (before == FileFingerprint::of(&path_after)).then_some(before);
    Ok(SourceBytes {
        bytes,
        digest,
        canonical,
        fingerprint,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Fingerprint {
    device: u64,
    inode: u64,
    length: u64,
    modified: Option<(u64, u32)>,
}

fn fingerprint(metadata: &fs::Metadata) -> Fingerprint {
    Fingerprint {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        modified: metadata.modified().ok().and_then(|time| {
            time.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|duration| (duration.as_secs(), duration.subsec_nanos()))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_is_hashed_from_the_exact_bytes_that_are_kept() {
        let root = tempfile::tempdir().expect("root");
        let path = root.path().join("image.png");
        fs::write(&path, b"first-content").expect("source");
        let first = read_source(&path).expect("read");
        assert_eq!(first.bytes, b"first-content");

        fs::write(&path, b"second-content").expect("rewrite");
        let second = read_source(&path).expect("read again");
        assert_ne!(first.digest, second.digest, "changed bytes change the digest");
        assert_eq!(
            second.digest,
            Sha256::digest(b"second-content").as_slice(),
            "the digest describes exactly the kept bytes"
        );
        assert_eq!(fs::read(&path).expect("source survives"), b"second-content");
    }

    #[test]
    fn the_byte_bound_accepts_the_boundary_and_rejects_the_next_byte() {
        let root = tempfile::tempdir().expect("root");
        let path = root.path().join("image.png");
        fs::write(&path, b"1234").expect("source");
        read_source_with(&path, 4, &|_| {}).expect("at the limit");
        let error = read_source_with(&path, 3, &|_| {}).expect_err("above");
        assert!(
            matches!(
                error,
                ProcessError::Resource(limits::ResourceError::SourceBytes { .. })
            ),
            "{error:?}"
        );
        assert_eq!(fs::read(&path).expect("source survives"), b"1234");
    }

    #[test]
    fn a_replacement_during_the_read_is_reported() {
        let root = tempfile::tempdir().expect("root");
        let path = root.path().join("image.png");
        fs::write(&path, b"original").expect("source");
        let replacement = root.path().join("replacement.png");
        fs::write(&replacement, b"replacement").expect("replacement");
        let hook = move |source: &Path| {
            fs::rename(&replacement, source).expect("replace source");
        };
        let error = read_source_with(&path, MAX_SOURCE_BYTES, &hook).expect_err("replacement");
        assert!(matches!(error, ProcessError::SourceChanged { .. }), "{error:?}");
    }

    #[test]
    fn a_directory_is_not_a_regular_source() {
        let root = tempfile::tempdir().expect("root");
        let error = read_source(root.path()).expect_err("directory");
        assert!(matches!(error, ProcessError::NotRegularFile { .. }), "{error:?}");
    }
}
