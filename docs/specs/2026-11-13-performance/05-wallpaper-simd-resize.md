---
status: done
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

- [x] PF-05-01: Against the reference, for downscales at 16:9, 16:10, 3:2, 21:9 and 1:1, downscales close to 1:1, upscales, odd dimensions and a 1 px crop margin: on content kept 28 levels away from black and white no channel differs by more than 1 level; on full-range gradient-plus-noise content fewer than 0.1 % of channels differ by more than 1, and none by more than 8 levels on a downscale or 12 on an upscale. Reworded on 2026-11-14 to the measured bounds the user accepted; it first read "no channel differs by more than 2 levels and fewer than 0.1 % of channels differ by more than 1".
- [x] PF-05-02: The output size equals the target for every case of PF-05-01, and the crop is centred to within one source pixel of the reference.
- [x] PF-05-03: RGBA, greyscale and 16-bit sources produce RGB8 output within the same tolerance.
- [x] PF-05-04: The limit errors for target, source and intermediate canvas are returned as before, before any allocation.
- [x] PF-05-05: The cache key for the same inputs differs from the version 1 key.
- [x] PF-05-06: In release, the cover resize of a 6000×4000 RGB8 image to 2560×1440 is at least 3 times faster than the reference, and the single-display harness is faster than after part 04. The numbers are in the feature doc.
- [x] PF-05-07: `bun run lint`, `bun run format:check` and `bun run test` pass.

## Decision (2026-11-14)

PF-05-01 could not be met as first written. On fixtures whose values stay away from black and white the new resizer matches the reference within 1 level in every case, which is what the audit saw on the user's wallpapers. On full-range gradient-plus-noise fixtures fewer than 0.1 % of channels differ by more than 1 (0.015 % at most), but single channels differ by more: up to 3 levels on the ordinary downscales (1 to 3 channels in about 700,000), up to 7 on downscales close to 1:1 (671×373 and 656×411 to 640 wide; 33 to 36 channels above 1 in about 700,000) and up to 9 on the upscale (76 to 106 channels above 1). `fast_image_resize` rounds and clamps to 8 bits between its two passes, where the `image` crate keeps the intermediate in floating point, so Lanczos overshoot that leaves the 0–255 range is cut at a different point and the second pass spreads that to neighbouring pixels. That cannot be configured away for 8-bit pixels.

Measured (2026-11-14, release, same session): the cover resize of 6000×4000 to 2560×1440 takes 66.0 ms against 270.9 ms for the reference (4.10 times faster); the single-display harness went from 431.6 ms to 235.4 ms and the three-display harness from 1,024 ms to 657 ms; the release binary grew by 132,288 bytes (10,598,448 to 10,730,736) with only the RGB8 path linked through `resize_typed` (the dynamic `resize` entry point cost 1,900,816 bytes).

The part was committed as `in-progress` while the rest of the spec continued, and the user was offered three choices: accept the measured bounds, keep the SIMD path for downscales only, or drop the part. **The user accepted the measured bounds on 2026-11-14.** PF-05-01 is reworded to them, and the comparison tests pin them: within 1 level in every case on content kept 28 levels away from black and white; on full-range content fewer than 0.1 % of channels above 1 and none above 8 on a downscale or 12 on an upscale.

## Tests

- A comparison test over the cases of PF-05-01 to PF-05-03 with deterministic textured fixtures (gradient plus noise), so ringing is exercised.
- The limit tests, unchanged.
- The cache key test.
- An ignored release harness for PF-05-06 beside the existing ones.

## Notes

- Found by the review (2026-11-14): the crop window was computed in floating point and could land one rounding step past the source edge, which `fast_image_resize` rejects; about 3 % of source sizes near the targets failed to resize at all. The window is now clamped to the source, a test resizes every source size from 2 below to 120 above each of the user's two targets, and a resize failure has its own `ProcessError::Resize` instead of being reported as a canvas limit. The comparison cases gained three of the previously failing sizes, which is where the 7-level maximum above comes from.
- Audit numbers on the user's wallpapers: 60–83 ms against 254–397 ms at `opt-level = 3`; maximum difference 1 level, mean 0.01–0.14, no channel beyond 1. The tolerance of PF-05-01 leaves room for upscales, which the audit did not measure.
- The manifest names `5.5.0`, the audited version (6.x exists and was not evaluated).
- The crate is MIT or Apache-2.0, about 25,000 lines with SIMD in `unsafe`. It adds `fast_image_resize`, `document-features` and `litrs` to the lockfile.
- The user's cache is rebuilt once after this part, at the new speed.
