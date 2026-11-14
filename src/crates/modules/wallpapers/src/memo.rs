//! A per-process memo of validated cache outputs and hashed sources.
//!
//! The host's processor owns one [`PrepareMemo`] for its lifetime; standalone
//! CLI paths have none. The memo performs no I/O: callers inspect a file with a
//! no-follow `symlink_metadata`, run every entry check on that fresh metadata,
//! and only then ask whether its [`FileFingerprint`] matches what was recorded
//! after a full check. A match lets an output skip its PNG decode, or a source
//! skip its read and hash. Any mismatch is a miss and the caller runs the full
//! check again. Each map holds at most [`MEMO_CAPACITY`] entries and evicts the
//! least recently used.

use std::collections::HashMap;
use std::fs::Metadata;
use std::hash::Hash;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

/// How many entries each memo map holds at most.
pub const MEMO_CAPACITY: usize = 256;

/// The identity and state of one file, from a no-follow `symlink_metadata`.
///
/// The change time cannot be set by a normal write or by `utimes`, so a
/// same-length rewrite with a restored modification time still changes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileFingerprint {
    device: u64,
    inode: u64,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl FileFingerprint {
    /// The fingerprint of already-read metadata.
    #[must_use]
    pub fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }

    /// The same inode with the same length and modification time.
    ///
    /// A rename moves the change time, so a published output is compared to
    /// its validated temporary without it.
    #[must_use]
    pub fn same_contents_as(&self, other: &Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.length == other.length
            && self.modified == other.modified
    }
}

/// Hit and miss counts of one memo.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoCounts {
    /// Output lookups that skipped the decode.
    pub output_hits: u64,
    /// Output lookups that ran the full validation.
    pub output_misses: u64,
    /// Source inspections that reused a recorded digest.
    pub source_hits: u64,
    /// Source inspections that found no matching record.
    pub source_misses: u64,
}

/// Validated outputs by cache path and source digests by canonical path.
#[derive(Debug, Default)]
pub struct PrepareMemo {
    outputs: Mutex<Lru<PathBuf, FileFingerprint>>,
    sources: Mutex<Lru<PathBuf, (FileFingerprint, [u8; 32])>>,
    output_hits: AtomicU64,
    output_misses: AtomicU64,
    source_hits: AtomicU64,
    source_misses: AtomicU64,
}

impl PrepareMemo {
    /// An empty memo.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Whether `path` was fully validated with exactly this fingerprint.
    pub fn output_matches(&self, path: &Path, fingerprint: &FileFingerprint) -> bool {
        let hit = lock(&self.outputs).get(path).is_some_and(|recorded| recorded == fingerprint);
        count(if hit {
            &self.output_hits
        } else {
            &self.output_misses
        });
        hit
    }

    /// Records a fully validated output.
    pub fn record_output(&self, path: &Path, fingerprint: FileFingerprint) {
        lock(&self.outputs).insert(path.to_path_buf(), fingerprint);
    }

    /// Forgets an output that is missing, unusable or no longer validates.
    pub fn forget_output(&self, path: &Path) { lock(&self.outputs).remove(path); }

    /// The recorded digest of `canonical` when its fingerprint still matches.
    pub fn source_digest(
        &self,
        canonical: &Path,
        fingerprint: &FileFingerprint,
    ) -> Option<[u8; 32]> {
        let digest = lock(&self.sources)
            .get(canonical)
            .filter(|(recorded, _)| recorded == fingerprint)
            .map(|(_, digest)| *digest);
        count(if digest.is_some() {
            &self.source_hits
        } else {
            &self.source_misses
        });
        digest
    }

    /// Records the digest of the bytes read from `canonical`.
    pub fn record_source(&self, canonical: &Path, fingerprint: FileFingerprint, digest: [u8; 32]) {
        lock(&self.sources).insert(canonical.to_path_buf(), (fingerprint, digest));
    }

    /// The hit and miss counts so far.
    #[must_use]
    pub fn counts(&self) -> MemoCounts {
        MemoCounts {
            output_hits: self.output_hits.load(Ordering::Relaxed),
            output_misses: self.output_misses.load(Ordering::Relaxed),
            source_hits: self.source_hits.load(Ordering::Relaxed),
            source_misses: self.source_misses.load(Ordering::Relaxed),
        }
    }

    #[cfg(test)]
    pub(crate) fn lens(&self) -> (usize, usize) {
        (lock(&self.outputs).len(), lock(&self.sources).len())
    }
}

fn count(counter: &AtomicU64) { counter.fetch_add(1, Ordering::Relaxed); }

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A small least-recently-used map. Eviction scans at most
/// [`MEMO_CAPACITY`] entries, which is cheaper than any file call it saves.
#[derive(Debug)]
struct Lru<K, V> {
    entries: HashMap<K, (V, u64)>,
    tick: u64,
}

impl<K, V> Default for Lru<K, V> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            tick: 0,
        }
    }
}

impl<K: Eq + Hash + Clone, V> Lru<K, V> {
    const fn next(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    fn get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: Eq + Hash + ?Sized, {
        let tick = self.next();
        self.entries.get_mut(key).map(|(value, used)| {
            *used = tick;
            &*value
        })
    }

    fn insert(&mut self, key: K, value: V) {
        let tick = self.next();
        if !self.entries.contains_key(&key)
            && self.entries.len() >= MEMO_CAPACITY
            && let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(key, _)| key.clone())
        {
            self.entries.remove(&oldest);
        }
        self.entries.insert(key, (value, tick));
    }

    fn remove<Q>(&mut self, key: &Q)
    where
        K: std::borrow::Borrow<Q>,
        Q: Eq + Hash + ?Sized, {
        self.entries.remove(key);
    }

    #[cfg(test)]
    fn len(&self) -> usize { self.entries.len() }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn the_fingerprint_follows_identity_length_and_both_times() {
        let root = tempfile::tempdir().expect("root");
        let path = root.path().join("a.png");
        fs::write(&path, b"first").expect("write");
        let first = FileFingerprint::of(&fs::symlink_metadata(&path).expect("metadata"));
        assert_eq!(
            first,
            FileFingerprint::of(&fs::symlink_metadata(&path).expect("metadata")),
            "an unchanged file keeps its fingerprint"
        );

        // Same length, modification time restored: only the change time moves.
        let modified = fs::metadata(&path).expect("metadata").modified().expect("mtime");
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&path, b"other").expect("rewrite");
        fs::File::options()
            .write(true)
            .open(&path)
            .expect("open")
            .set_modified(modified)
            .expect("restore mtime");
        let second = FileFingerprint::of(&fs::symlink_metadata(&path).expect("metadata"));
        assert_eq!(first.length, second.length);
        assert_eq!(first.modified, second.modified);
        assert_ne!(first, second, "the change time distinguishes the rewrite");

        // A replacement is another inode.
        let other = root.path().join("b.png");
        fs::write(&other, b"other").expect("other");
        fs::rename(&other, &path).expect("replace");
        let third = FileFingerprint::of(&fs::symlink_metadata(&path).expect("metadata"));
        assert_ne!(second.inode, third.inode);
    }

    fn fingerprint(seed: u64) -> FileFingerprint {
        FileFingerprint {
            device: 1,
            inode: seed,
            length: seed,
            modified: (0, 0),
            changed: (0, 0),
        }
    }

    #[test]
    fn each_map_holds_at_most_256_entries_and_evicts_the_least_recently_used() {
        let memo = PrepareMemo::new();
        let path = |index: u64| PathBuf::from(format!("/cache/{index}.png"));
        for index in 0..300 {
            memo.record_output(&path(index), fingerprint(index));
            memo.record_source(&path(index), fingerprint(index), [0; 32]);
            if index == 255 {
                // Using entry 0 last before the bound makes 1..=44 the oldest.
                assert!(memo.output_matches(&path(0), &fingerprint(0)));
                assert!(memo.source_digest(&path(0), &fingerprint(0)).is_some());
            }
        }
        assert_eq!(memo.lens(), (MEMO_CAPACITY, MEMO_CAPACITY));
        // 300 inserts evict 44: entries 1..=44 are the least recently used.
        assert!(memo.output_matches(&path(0), &fingerprint(0)));
        assert!(memo.source_digest(&path(0), &fingerprint(0)).is_some());
        for index in 1..=44 {
            assert!(
                !memo.output_matches(&path(index), &fingerprint(index)),
                "{index}"
            );
            assert!(memo.source_digest(&path(index), &fingerprint(index)).is_none());
        }
        for index in 45..300 {
            assert!(memo.output_matches(&path(index), &fingerprint(index)), "{index}");
            assert!(memo.source_digest(&path(index), &fingerprint(index)).is_some());
        }
    }

    #[test]
    fn counters_report_hits_and_misses_and_a_changed_fingerprint_misses() {
        let memo = PrepareMemo::new();
        let path = Path::new("/cache/a.png");
        assert!(!memo.output_matches(path, &fingerprint(1)));
        memo.record_output(path, fingerprint(1));
        assert!(memo.output_matches(path, &fingerprint(1)));
        assert!(!memo.output_matches(path, &fingerprint(2)));
        memo.forget_output(path);
        assert!(!memo.output_matches(path, &fingerprint(1)));

        assert_eq!(memo.source_digest(path, &fingerprint(1)), None);
        memo.record_source(path, fingerprint(1), [7; 32]);
        assert_eq!(memo.source_digest(path, &fingerprint(1)), Some([7; 32]));
        assert_eq!(memo.source_digest(path, &fingerprint(3)), None);

        assert_eq!(memo.counts(), MemoCounts {
            output_hits: 1,
            output_misses: 3,
            source_hits: 1,
            source_misses: 2,
        });
    }
}
