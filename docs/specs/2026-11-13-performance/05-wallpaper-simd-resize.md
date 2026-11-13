---
status: draft
---

# 05 — Wallpapers: SIMD Lanczos3 cover resize

## Scope

The cover resize uses `fast_image_resize` instead of the `image` crate's Lanczos3. Decision D10 of the [overview](00-overview.md). Builds on part 04.

Files expected to change: `src/crates/modules/wallpapers/Cargo.toml`, `Cargo.lock`, `Cargo.toml` (the `opt-level = 3` override for the new crate), `src/crates/modules/wallpapers/src/processing.rs` and its tests, `src/crates/modules/wallpapers/src/cache.rs` tests if they pin the version, and `docs/features/wallpapers.md`.

## Requirements

- **Dependency.** `fast_image_resize = { version = "5", default-features = false }`. No `rayon` feature: concurrency stays in part 04's workers.
- **Geometry.** `cover_crop` keeps its signature, its limit checks and its geometry: the same cover scale, the same truncation and clamp of the scaled size, the same centred crop. The crop is expressed in source coordinates and resized straight to the target size, so no full-size intermediate canvas is allocated; `check_intermediate` still rejects a canvas that would have exceeded the bound.
- **Pixels.** The filter is Lanczos3 convolution. A source that is already RGB8 is borrowed, not copied; any other colour type is converted to RGB8 first. The result is RGB8 at exactly the target size.
- **Version.** `PROCESSING_VERSION` becomes 2. Entries written under version 1 stop matching and are rebuilt on use.
- **Reference.** The previous implementation stays in the test module as the reference the new one is compared with.
- **Measurement.** Run the two harnesses of part 04 again and add the numbers to the same dated entry.
- **Docs.** `docs/features/wallpapers.md`: the Order bullet names the resizer; the Cache key bullet's version; one sentence on why the dependency exists and what it costs.

## Acceptance criteria

- [ ] PF-05-01: Against the reference, for downscales at 16:9, 16:10, 3:2, 21:9 and 1:1, an upscale, odd dimensions and a 1 px crop margin, no channel differs by more than 2 levels and fewer than 0.1 % of channels differ by more than 1.
- [ ] PF-05-02: The output size equals the target for every case of PF-05-01, and the crop is centred to within one source pixel of the reference.
- [ ] PF-05-03: RGBA, greyscale and 16-bit sources produce RGB8 output within the same tolerance.
- [ ] PF-05-04: The limit errors for target, source and intermediate canvas are returned as before, before any allocation.
- [ ] PF-05-05: The cache key for the same inputs differs from the version 1 key.
- [ ] PF-05-06: In release, the cover resize of a 6000×4000 RGB8 image to 2560×1440 is at least 3 times faster than the reference, and the single-display harness is faster than after part 04. The numbers are in the feature doc.
- [ ] PF-05-07: `bun run lint`, `bun run format:check` and `bun run test` pass.

## Tests

- A comparison test over the cases of PF-05-01 to PF-05-03 with deterministic textured fixtures (gradient plus noise), so ringing is exercised.
- The limit tests, unchanged.
- The cache key test.
- An ignored release harness for PF-05-06 beside the existing ones.

## Notes

- Audit numbers on the user's wallpapers: 60–83 ms against 254–397 ms at `opt-level = 3`; maximum difference 1 level, mean 0.01–0.14, no channel beyond 1. The tolerance of PF-05-01 leaves room for upscales, which the audit did not measure.
- The crate is MIT or Apache-2.0, about 25,000 lines with SIMD in `unsafe`. It adds `fast_image_resize`, `document-features` and `litrs` to the lockfile.
- The user's cache is rebuilt once after this part, at the new speed.
