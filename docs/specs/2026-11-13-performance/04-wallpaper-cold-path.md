---
status: done
---

# 04 — Wallpapers: Fast PNG, parallel prepare, optimised image stack

## Scope

The three changes to the cold path that need no new dependency and leave every output pixel unchanged. Decisions D7, D8 and D9 of the [overview](00-overview.md).

Files expected to change: `Cargo.toml` (release profile overrides), `src/crates/modules/wallpapers/src/{processing.rs, apply.rs, limits.rs}`, their tests, and `docs/features/wallpapers.md`.

## Requirements

- **Encode.** `encode_png` writes RGB8 PNG with `CompressionType::Fast` and adaptive filtering. `PROCESSING_VERSION` stays 1 and the cache key is unchanged, so a file written before this part is still a hit.
- **Plan preparation.** `prepare_plan_with` keeps its contract and adds concurrency:
  - The whole plan is validated before any read or cache write, the lease is taken once before the first cache read, and the result lists the outputs in plan order. All of that is as today.
  - Each distinct source is read and hashed once and decoded at most once, however many targets use it.
  - Each distinct output (same source bytes, pixels and effects) is produced once and shared by the targets that need it.
  - Work runs on scoped threads, at most `min(available_parallelism, MAX_CONCURRENT_WORKERS)`. A plan with one output runs on the calling thread and spawns nothing. No thread outlives the call.
  - Cancellation is observed at the same stage boundaries in every worker. When one output fails or the call is cancelled, the others stop at their next boundary and remove only their own temporaries.
  - With several failures the error of the lowest plan index is returned.
  - The lease is verified before each publication and the commit check is unchanged.
- **Memory bound.** At most `MAX_CONCURRENT_WORKERS` decoded sources are resident at once. `limits.rs` and the feature doc state this in place of "at most one".
- **Batch path.** `prepare_batch` is unchanged.
- **Optimisation level.** The release profile builds these packages at `opt-level = 3`: `stache-wallpapers` and every crate that decodes, resizes, blurs, encodes or hashes on this path (`image`, `png`, `fdeflate`, `flate2`, `miniz_oxide`, `zune-jpeg`, `zune-core`, `image-webp`, `simd-adler32`, `crc32fast`, `sha2`; confirm the set and the exact versions with `cargo tree -p stache-wallpapers`). Other packages and other profiles are unchanged.
- **Binary size.** Record the size of the release `stache` binary before and after. If it grows by more than 300 KB, stop and report.
- **Measurement.** Run `measure_cold_processing_6000x4000_to_2560x1440` and `measure_cold_processing_multi_display` in release before and after, once each, and add a dated entry to the "Cold-processing measurement" section of `docs/features/wallpapers.md`. Earlier entries stay.
- **Docs.** `docs/features/wallpapers.md`: the Order bullet (Fast compression), the preparation paragraph (parallel, one decode per source, the memory bound), the limits table and the new measurement.

## Acceptance criteria

- [x] PF-04-01: The decoded pixels of an output equal the pixels of the same image encoded with `CompressionType::Best`.
- [x] PF-04-02: A cache file written with the previous encoder settings for a key is returned as a hit.
- [x] PF-04-03: A plan of three displays with three sources prepares with three workers running at once (shown with a hold, not a timer), returns outputs in plan order, and each source is read once.
- [x] PF-04-04: A plan of two displays with the same source and different sizes decodes once; a plan of two displays with the same source, size and effects produces one file and returns it for both.
- [x] PF-04-05: A failure in the second of three outputs returns that error, leaves no temporary file, and the third output's worker stops at its next boundary. Cancellation during the work returns `Cancelled` and leaves no temporary file.
- [x] PF-04-06: A replaced lease or a cancellation observed at the final commit publishes nothing, for every worker.
- [x] PF-04-07: No more than `MAX_CONCURRENT_WORKERS` workers run at once for a plan of eight outputs.
- [x] PF-04-08: The multi-display harness median is at most half of the before value, and the single-display harness is faster than before. Both numbers are in the feature doc.
- [x] PF-04-09: The release binary grew by at most 300 KB.
- [x] PF-04-10: `bun run lint`, `bun run format:check` and `bun run test` pass, including the runtime, CLI and app wallpaper suites.

## Tests

- Encoder tests for PF-04-01 and PF-04-02.
- Plan tests with an injected seam that holds workers at a known point (the batch tests' observer pattern) for PF-04-03 to PF-04-07. No real sleeps.
- The existing `apply`, `batch`, `runtime` and `cli` suites, which already cover lease contention, publication failures and the guarded apply.
- One end-to-end scenario through `WallpapersService` with `FakeDesktop`: a random set on three displays with a cold cache applies three images, a second set with a warm cache applies without producing a file.

## Notes

- Audit numbers on the user's wallpapers (6016×3900 PNG to 2560×1440, blur 10, radius 20): encode 981 ms at `Best` and 18 ms at `Fast` once the calling crate builds at `opt-level = 3`; resize 424 ms at `"s"` and 275 ms at 3; two pipelines at once ran 1.76 times faster than in sequence, four ran 3.47 times faster.
- The harnesses use a synthetic gradient JPEG, which compresses far more easily than a photograph, so they understate the encoder gain. That is why PF-04-08 asks for less than the audit measured.
- Measured on the branch (2026-11-13, Apple M1 Pro, macOS 27.3), once before and once after in the same session with `cargo test -p stache-wallpapers --release measure_cold_processing -- --ignored --nocapture --test-threads=1`: single display, median of three, 701.7 ms to 433.2 ms; three displays, median of three, 4,094 ms to 1,035 ms (25 % of before). Release `stache` binary (`bun run tauri:build -- --no-bundle`): 10,317,440 to 10,598,448 bytes, 281,008 more.
- The unit of parallel work is the distinct output. Outputs are queued source by source and a source's bytes and decode are released when its last output finishes, so every loaded source has an output in flight and at most `MAX_CONCURRENT_WORKERS` decodes are resident.
- The worker count is an input of the observed entry point, so the hold tests fix it and do not depend on the machine's cores.
- For the user to check: `stache wallpaper set --random` on a cold cache, and `stache wallpaper generate-all`.
