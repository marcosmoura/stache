---
status: draft
---

# 06 — Wallpapers: host memo of validated outputs and source digests

## Scope

A cached switch in the running host stops re-reading the source and re-decoding the cached PNG when neither file changed. Decision D11 of the [overview](00-overview.md). Builds on part 04.

Files expected to change: `src/crates/modules/wallpapers/src/{cache.rs, source.rs, apply.rs, lib.rs, runtime/service.rs}`, their tests, the app tests that construct `RealProcessor`, and `docs/features/wallpapers.md`.

## Requirements

- **Memo.** A new `Send + Sync` type (working name `PrepareMemo`) holds two bounded maps: validated outputs by cache path and source digests by canonical source path. Each holds at most 256 entries and evicts the least recently used. It performs no I/O of its own.
- **Fingerprint.** Device, inode, length, modification time and change time, all from a no-follow `symlink_metadata`.
- **Owner.** `RealProcessor` owns one memo for the life of the host service and passes it through `PreparePlanDeps`. `prepare`, `prepare_plan`, `prepare_batch` and every standalone CLI path pass none and behave exactly as today.
- **Outputs.** After a full validation succeeds (a lookup that decoded, or the validation after a publication) the output's fingerprint is recorded. A later lookup still runs the entry checks (regular file, owner, not a link) on fresh metadata; when the fingerprint matches the record the decode is skipped. Any mismatch runs the full validation and replaces or removes the record.
- **Sources.** After a source is read its fingerprint and digest are recorded. For a later target the source is canonicalized and inspected as `read_source` does (regular file, not a link, within the byte limit); when the fingerprint matches, the recorded digest forms the cache key without opening the file. If the output lookup then misses, the source is read as today, and the digest of the bytes just read is the one used for the key and the record.
- **Never weaker.** The memo cannot make a missing, foreign, linked or non-regular file acceptable, and a file whose fingerprint changed is never served from the record.
- **Counters.** The memo exposes hit and miss counts for outputs and sources, for tests and diagnostics.
- **Docs.** `docs/features/wallpapers.md`: the Read once, Cache key and Validation bullets say what is checked every time, what is checked on first use per process and when a full check runs again.

## Acceptance criteria

- [ ] PF-06-01: Preparing the same plan twice through `RealProcessor` reads no source and decodes no output the second time (the counters show hits; a read hook and a decode hook show zero calls).
- [ ] PF-06-02: A cache file replaced by a corrupt file of the same length whose modification time is set back is not served: it is detected and rebuilt.
- [ ] PF-06-03: A cache file replaced by a link, by a file of another owner or by a directory is refused exactly as without a memo.
- [ ] PF-06-04: A source rewritten with different bytes of the same length and the same modification time produces a different key and a new output.
- [ ] PF-06-05: A source removed after its digest was recorded fails with the same typed error as today.
- [ ] PF-06-06: With 300 distinct outputs each map holds at most 256 entries and the least recently used were evicted.
- [ ] PF-06-07: The standalone `set`, `list` and `generate-all` paths construct no memo and their tests pass unchanged.
- [ ] PF-06-08: `bun run lint`, `bun run format:check` and `bun run test` pass.

## Tests

- Memo unit tests for the fingerprint, the bound and the counters.
- Plan tests for PF-06-01 to PF-06-05 with real files in a temporary directory. Restoring a modification time uses `File::set_modified`; the change time moves on its own, which is what the fingerprint relies on.
- The runtime scenario of part 04 extended: after the warm set, a third set with a rewritten source produces a new file.

## Notes

- Audit numbers: a cache hit costs 1–8 ms for the source read and hash plus 11–25 ms for the PNG decode per display, 25–65 ms for the user's two displays in sequence. After this part it is a few metadata calls.
- The change time cannot be set by a normal write or by `utimes`, so a same-length rewrite with a restored modification time still changes the fingerprint. Writing through the raw device would not; nothing in the previous contract defended against that either once the file is handed to the system.
