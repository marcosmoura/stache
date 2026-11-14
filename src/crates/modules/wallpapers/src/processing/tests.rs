use image::{DynamicImage, GenericImageView, Rgb, RgbImage, imageops};

use super::*;
use crate::test_support::{
    coordinates, oversized_png, read_rgb8, solid, write_jpeg, write_png, write_webp,
};

fn target(width: u32, height: u32) -> TargetPixels { TargetPixels { width, height } }

#[test]
fn every_advertised_format_decodes_from_real_bytes() {
    let root = tempfile::tempdir().expect("root");
    let image = solid(32, 24, [200, 40, 10]);

    let png = root.path().join("a.png");
    write_png(&png, &image);
    let decoded = decode(&png, &std::fs::read(&png).expect("png bytes")).expect("png");
    assert_eq!(decoded.dimensions(), (32, 24));

    let jpeg = root.path().join("a.jpeg");
    write_jpeg(&jpeg, &image);
    let decoded = decode(&jpeg, &std::fs::read(&jpeg).expect("jpeg bytes")).expect("jpeg");
    assert_eq!(decoded.dimensions(), (32, 24));

    // JPEG content under a `.jpg` name: format is sniffed from bytes, not the
    // file extension.
    let jpeg_named_jpg = root.path().join("a.jpg");
    write_jpeg(&jpeg_named_jpg, &image);
    let decoded = decode(
        &jpeg_named_jpg,
        &std::fs::read(&jpeg_named_jpg).expect("jpg bytes"),
    )
    .expect("jpg");
    assert_eq!(decoded.dimensions(), (32, 24));

    let webp = root.path().join("a.webp");
    write_webp(&webp, &image);
    let decoded = decode(&webp, &std::fs::read(&webp).expect("webp bytes")).expect("webp");
    assert_eq!(decoded.dimensions(), (32, 24));
}

#[test]
fn corrupt_bytes_are_a_typed_decode_error() {
    let error = decode(&std::path::PathBuf::from("/tmp/corrupt.png"), b"not an image")
        .expect_err("corrupt");
    assert!(matches!(error, ProcessError::Decode { .. }), "{error:?}");
}

#[test]
fn an_oversized_header_fails_before_decoding_and_allocating() {
    // 65,536 × 65,536 is ~4.3 gigapixels: accepted edges, rejected pixels.
    let bytes = oversized_png(65_536, 65_536);
    assert_eq!(&bytes[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    let error = decode(&std::path::PathBuf::from("/tmp/huge.png"), &bytes).expect_err("oversized");
    assert!(
        matches!(
            error,
            ProcessError::Resource(crate::limits::ResourceError::SourceDimensions { .. })
        ),
        "{error:?}"
    );
}

#[test]
fn a_wide_source_center_crops_after_cover_resize() {
    let image = DynamicImage::ImageRgb8(coordinates(400, 100));
    let cropped = cover_crop(&image, target(100, 100)).expect("crop");
    assert_eq!(cropped.dimensions(), (100, 100));
    let source = coordinates(400, 100);
    // 400 × 100 covering 100 × 100 scales by 1 and crops x = 150.
    assert_eq!(cropped.to_rgb8().get_pixel(0, 0), source.get_pixel(150, 0));
    assert_eq!(cropped.to_rgb8().get_pixel(99, 99), source.get_pixel(249, 99));
}

#[test]
fn a_tall_source_center_crops_after_cover_resize() {
    let source = coordinates(100, 400);
    let cropped =
        cover_crop(&DynamicImage::ImageRgb8(source.clone()), target(100, 100)).expect("crop");
    assert_eq!(cropped.dimensions(), (100, 100));
    // 100 × 400 covering 100 × 100 scales by 1 and crops y = 150.
    assert_eq!(cropped.to_rgb8().get_pixel(0, 0), source.get_pixel(0, 150));
    assert_eq!(cropped.to_rgb8().get_pixel(99, 99), source.get_pixel(99, 249));
}

#[test]
fn an_odd_crop_offset_uses_integer_division_like_the_legacy_adapter() {
    let source = coordinates(301, 100);
    let cropped =
        cover_crop(&DynamicImage::ImageRgb8(source.clone()), target(100, 100)).expect("crop");
    // crop_x = (301 - 100) / 2 = 100, not 101.
    assert_eq!(cropped.to_rgb8().get_pixel(0, 0), source.get_pixel(100, 0));
}

#[test]
fn a_small_source_upscales_to_cover_the_target() {
    let image = DynamicImage::ImageRgb8(solid(50, 50, [10, 20, 30]));
    let cropped = cover_crop(&image, target(100, 100)).expect("crop");
    assert_eq!(cropped.dimensions(), (100, 100));
    assert_eq!(*cropped.to_rgb8().get_pixel(50, 50), Rgb::<u8>([10, 20, 30]));
}

#[test]
fn zero_effects_skip_both_stages_and_keep_pixels() {
    let source = coordinates(24, 16);
    let result = apply_effects(DynamicImage::ImageRgb8(source.clone()), Effects::default());
    assert_eq!(result, source);
}

#[test]
fn blur_five_uses_the_gaussian_path_and_blur_six_the_fast_path() {
    let source = coordinates(40, 40);
    let dynamic = DynamicImage::ImageRgb8(source.clone());

    let five = apply_effects(dynamic.clone(), Effects { radius: 0, blur: 5 });
    let gaussian = dynamic.blur(5.0).to_rgb8();
    assert_eq!(five, gaussian);

    let six = apply_effects(dynamic, Effects { radius: 0, blur: 6 });
    let fast = imageops::fast_blur(&source, 6.0);
    assert_eq!(six, fast);
    assert_ne!(five, six, "the two documented paths must differ");
}

#[test]
fn the_corner_radius_is_capped_at_half_the_smaller_dimension() {
    let source = solid(20, 20, [255, 255, 255]);
    let capped = apply_effects(DynamicImage::ImageRgb8(source.clone()), Effects {
        radius: 100,
        blur: 0,
    });
    let explicit = apply_effects(DynamicImage::ImageRgb8(source), Effects { radius: 10, blur: 0 });
    assert_eq!(capped, explicit);
    assert_eq!(capped.get_pixel(0, 0), &Rgb([0, 0, 0]));
}

#[test]
fn corners_are_black_and_antialiased() {
    let source = solid(100, 100, [255, 255, 255]);
    let result = apply_effects(DynamicImage::ImageRgb8(source), Effects { radius: 16, blur: 0 });
    for (x, y) in [(0, 0), (99, 0), (0, 99), (99, 99)] {
        assert_eq!(result.get_pixel(x, y), &Rgb([0, 0, 0]), "corner {x},{y}");
    }
    assert_eq!(result.get_pixel(16, 16), &Rgb([255, 255, 255]));
    let antialiased = (0..16).any(|y| {
        (0..16).any(|x| {
            let pixel = result.get_pixel(x, y);
            pixel[0] > 0 && pixel[0] < 255
        })
    });
    assert!(antialiased, "the rounded edge must have partial coverage pixels");
}

#[test]
fn output_is_lossless_rgb8_png_with_the_expected_dimensions() {
    let source = solid(17, 9, [3, 4, 5]);
    let mut encoded = Vec::new();
    encode_png(&source, &mut encoded, std::path::Path::new("/tmp/out.png")).expect("encode");
    let root = tempfile::tempdir().expect("root");
    let path = root.path().join("out.png");
    std::fs::write(&path, encoded).expect("write");
    assert_eq!(
        image::guess_format(&std::fs::read(&path).expect("bytes")).expect("format"),
        image::ImageFormat::Png
    );
    assert_eq!(crate::test_support::color_type(&path), image::ColorType::Rgb8);
    assert_eq!(read_rgb8(&path), source);
}

/// Fast compression changes only the bytes: the decoded pixels match the same
/// image encoded with Best compression, so the cache key needs no new version.
#[test]
fn fast_output_decodes_to_the_same_pixels_as_best_compression() {
    let source = apply_effects(
        cover_crop(&DynamicImage::ImageRgb8(coordinates(97, 61)), target(80, 45)).expect("cover"),
        Effects { radius: 9, blur: 7 },
    );
    let mut fast = Vec::new();
    encode_png(&source, &mut fast, std::path::Path::new("/tmp/fast.png")).expect("encode");
    let mut best = Vec::new();
    let encoder =
        PngEncoder::new_with_quality(&mut best, CompressionType::Best, PngFilterType::Adaptive);
    source.write_with_encoder(encoder).expect("best encode");

    let decode = |bytes: &[u8]| {
        image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
            .expect("decode")
            .into_rgb8()
    };
    assert_ne!(fast, best, "the two settings must produce different files");
    assert_eq!(decode(&fast), decode(&best));
    assert_eq!(decode(&fast), source);
}

#[test]
fn a_non_positive_target_is_rejected() {
    let error = cover_crop(&DynamicImage::ImageRgb8(solid(4, 4, [0, 0, 0])), target(0, 1))
        .expect_err("zero width");
    assert!(matches!(error, ProcessError::Resource(_)), "{error:?}");
}

#[test]
fn a_cover_canvas_over_the_intermediate_bound_is_rejected_before_resizing() {
    // 1 × 1000 covering 100 × 100 needs a 100 × 100,000 canvas.
    let error = cover_crop(
        &DynamicImage::ImageRgb8(solid(1, 1000, [0, 0, 0])),
        target(100, 100),
    )
    .expect_err("intermediate");
    assert!(
        matches!(
            error,
            ProcessError::Resource(ResourceError::IntermediateDimensions { .. })
        ),
        "{error:?}"
    );
}

#[test]
fn an_empty_source_is_rejected() {
    let error = cover_crop(&DynamicImage::new_rgb8(0, 0), target(10, 10)).expect_err("empty");
    assert!(
        matches!(
            error,
            ProcessError::Resource(ResourceError::SourceDimensions { .. })
        ),
        "{error:?}"
    );
}

#[test]
fn an_oversized_target_is_rejected_before_resizing() {
    let error = cover_crop(
        &DynamicImage::ImageRgb8(solid(4, 4, [0, 0, 0])),
        target(100_000, 1),
    )
    .expect_err("oversized target");
    assert!(
        matches!(
            error,
            ProcessError::Resource(ResourceError::TargetDimensions { .. })
        ),
        "{error:?}"
    );
}

/// The previous cover resize: an exact Lanczos3 resize of the whole source to
/// the cover canvas, then a centred crop. The reference the SIMD path is
/// compared with.
fn reference_cover_crop(image: &DynamicImage, target: TargetPixels) -> RgbImage {
    let (width, height) = image.dimensions();
    let scale = (f64::from(target.width) / f64::from(width))
        .max(f64::from(target.height) / f64::from(height));
    let scaled_width = scaled_edge(width, scale, target.width).expect("width");
    let scaled_height = scaled_edge(height, scale, target.height).expect("height");
    let resized = image.resize_exact(scaled_width, scaled_height, imageops::FilterType::Lanczos3);
    let crop_x = (scaled_width - target.width) / 2;
    let crop_y = (scaled_height - target.height) / 2;
    resized.crop_imm(crop_x, crop_y, target.width, target.height).to_rgb8()
}

/// A deterministic smooth gradient plus per-pixel noise, so the filter's
/// ringing is exercised. The gradient never wraps, so the fixture has no hard
/// full-range edges.
fn textured(width: u32, height: u32, seed: u32) -> RgbImage {
    let mut state = seed.wrapping_mul(0x9E37_79B9) | 1;
    RgbImage::from_fn(width, height, |x, y| {
        let mut channel = |base: u32| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            #[allow(clippy::cast_possible_truncation)]
            let value = (base + state % 56) as u8;
            value
        };
        Rgb([
            channel(x * 200 / width),
            channel(y * 200 / height),
            channel((x + y) * 100 / (width + height) + 50),
        ])
    })
}

/// The textured fixture mapped into 28–228, away from black and white, where
/// Lanczos overshoot never leaves 0–255.
fn mid_range(width: u32, height: u32, seed: u32) -> RgbImage {
    let mut image = textured(width, height, seed);
    for channel in image.iter_mut() {
        #[allow(clippy::cast_possible_truncation)]
        let value = (u32::from(*channel) * 200 / 255 + 28) as u8;
        *channel = value;
    }
    image
}

/// How far a channel may be from 0 or 255 and still count as near black or
/// white in the difference report. An upscale spreads the clipped overshoot
/// over more output pixels, so its margin is wider. The count of larger
/// differences outside these margins is printed, not asserted: a clamped
/// overshoot also changes its neighbours through the second pass.
const DOWNSCALE_CLAMP_MARGIN: u8 = 16;
const UPSCALE_CLAMP_MARGIN: u8 = 48;

struct Difference {
    max: u8,
    over_one: usize,
    channels: usize,
    mean: f64,
    /// Channels that differ by more than 2 with neither value within the
    /// clamp margin of 0 or 255.
    over_two_mid_range: usize,
}

impl fmt::Display for Difference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "max {} mean {:.3} over-1 {}/{} channels, over-2 away from black and white {}",
            self.max, self.mean, self.over_one, self.channels, self.over_two_mid_range
        )
    }
}

const fn near_black_or_white(value: u8, margin: u8) -> bool {
    value <= margin || value >= u8::MAX - margin
}

fn difference(actual: &RgbImage, expected: &RgbImage, margin: u8) -> Difference {
    assert_eq!(actual.dimensions(), expected.dimensions());
    let mut result = Difference {
        max: 0,
        over_one: 0,
        channels: 0,
        mean: 0.0,
        over_two_mid_range: 0,
    };
    let mut sum = 0_u64;
    for (a, e) in actual.as_raw().iter().zip(expected.as_raw()) {
        let delta = a.abs_diff(*e);
        result.max = result.max.max(delta);
        result.over_one += usize::from(delta > 1);
        result.over_two_mid_range += usize::from(
            delta > 2 && !near_black_or_white(*a, margin) && !near_black_or_white(*e, margin),
        );
        result.channels += 1;
        sum += u64::from(delta);
    }
    #[allow(clippy::cast_precision_loss)]
    {
        result.mean = sum as f64 / result.channels as f64;
    }
    result
}

/// Resizes with both paths and returns the difference after checking the
/// output is RGB8 at exactly the target size.
fn compare(label: &str, image: &DynamicImage, to: TargetPixels, margin: u8) -> Difference {
    let actual = cover_crop(image, to).expect("cover");
    let DynamicImage::ImageRgb8(actual) = actual else {
        panic!("{label}: output is not RGB8");
    };
    assert_eq!(actual.dimensions(), (to.width, to.height), "{label}");
    difference(&actual, &reference_cover_crop(image, to), margin)
}

/// `(label, source width, source height, target width, target height, upscale)`.
const COMPARISON_CASES: [(&str, u32, u32, u32, u32, bool); 11] = [
    ("16:9 downscale", 1200, 800, 640, 360, false),
    ("16:10 downscale", 1200, 900, 640, 400, false),
    ("3:2 downscale", 1000, 1000, 600, 400, false),
    ("21:9 downscale", 1200, 800, 840, 360, false),
    ("1:1 downscale", 900, 600, 400, 400, false),
    ("upscale", 160, 90, 640, 400, true),
    ("odd dimensions", 1001, 667, 333, 191, false),
    ("1 px crop margin", 641, 360, 640, 360, false),
    // The far crop edge of these used to round past the source.
    ("edge rounding 16:9", 671, 373, 640, 360, false),
    ("edge rounding 16:10", 656, 411, 640, 400, false),
    ("edge rounding upscale", 165, 93, 640, 360, true),
];

/// Every comparison case plus RGBA8, Luma8 and RGB16 sources, built from
/// `fixture`. Yields `(label, image, target, upscale)`.
fn comparison_inputs(
    fixture: fn(u32, u32, u32) -> RgbImage,
) -> Vec<(String, DynamicImage, TargetPixels, bool)> {
    let mut inputs = Vec::new();
    for (index, (label, width, height, to_width, to_height, upscale)) in
        COMPARISON_CASES.into_iter().enumerate()
    {
        #[allow(clippy::cast_possible_truncation)]
        let image = DynamicImage::ImageRgb8(fixture(width, height, index as u32 + 1));
        inputs.push((label.to_owned(), image, target(to_width, to_height), upscale));
    }
    let rgb = DynamicImage::ImageRgb8(fixture(1001, 667, 7));
    let to = target(400, 225);
    inputs.push((
        "rgba8".to_owned(),
        DynamicImage::ImageRgba8(rgb.to_rgba8()),
        to,
        false,
    ));
    inputs.push((
        "luma8".to_owned(),
        DynamicImage::ImageLuma8(rgb.to_luma8()),
        to,
        false,
    ));
    inputs.push((
        "rgb16".to_owned(),
        DynamicImage::ImageRgb16(rgb.to_rgb16()),
        to,
        false,
    ));
    inputs
}

#[test]
fn away_from_black_and_white_the_simd_resize_is_within_one_level_of_the_reference() {
    let mut report = Vec::new();
    let mut failed = false;
    for (label, image, to, _) in comparison_inputs(mid_range) {
        let diff = compare(&label, &image, to, DOWNSCALE_CLAMP_MARGIN);
        failed |= diff.max > 1;
        report.push(format!("{label}: {diff}"));
    }
    assert!(
        !failed,
        "a channel differs by more than 1:\n{}",
        report.join("\n")
    );
}

/// Over the full 0–255 range the bound is wider than 2 levels on purpose.
/// `fast_image_resize` rounds and clamps its intermediate image to 8 bits
/// between the horizontal and vertical passes, where the `image` crate keeps
/// it in floating point, so Lanczos overshoot past black or white is cut at a
/// different point, and the second pass spreads that to neighbouring pixels.
/// On this per-pixel noise fewer than 0.1 % of channels differ by more than one
/// level (at most 0.015 % measured), and none by more than 8 levels on a
/// downscale or 12 on an upscale (7 and 9 measured; the largest are at scales
/// close to 1:1). The same fixture kept away from black and white stays within
/// one level (the test above). The owner accepted this wider bound on
/// 2026-11-14; the performance spec records the decision.
#[test]
fn near_black_and_white_the_simd_resize_differs_only_by_the_8_bit_intermediate_clamp() {
    let mut report = Vec::new();
    let mut failed = false;
    for (label, image, to, upscale) in comparison_inputs(textured) {
        let (cap, margin) = if upscale {
            (12, UPSCALE_CLAMP_MARGIN)
        } else {
            (8, DOWNSCALE_CLAMP_MARGIN)
        };
        let diff = compare(&label, &image, to, margin);
        // Fewer than 0.1 % of channels may differ by more than one level.
        failed |= diff.over_one * 1000 >= diff.channels || diff.max > cap;
        report.push(format!("{label} (cap {cap}): {diff}"));
    }
    assert!(
        !failed,
        "a full-range bound was exceeded:\n{}",
        report.join("\n")
    );
}

/// The brightness-weighted centroid of an output, in output pixels.
fn centroid(image: &RgbImage) -> (f64, f64) {
    let (mut total, mut sum_x, mut sum_y) = (0.0, 0.0, 0.0);
    for (x, y, pixel) in image.enumerate_pixels() {
        let weight = f64::from(pixel[0]);
        total += weight;
        sum_x += weight * f64::from(x);
        sum_y += weight * f64::from(y);
    }
    (sum_x / total, sum_y / total)
}

#[test]
fn the_crop_is_centred_like_the_reference() {
    for (label, width, height, to_width, to_height, _) in COMPARISON_CASES {
        // A bright block off the centre of a dark source.
        let source = RgbImage::from_fn(width, height, |x, y| {
            let inside = x > width * 2 / 5 && x < width * 3 / 5 && y > height / 3 && y < height / 2;
            if inside {
                Rgb([255, 255, 255])
            } else {
                Rgb([0, 0, 0])
            }
        });
        let image = DynamicImage::ImageRgb8(source);
        let to = target(to_width, to_height);
        let actual = cover_crop(&image, to).expect("cover").into_rgb8();
        assert_eq!(actual.dimensions(), (to_width, to_height), "{label}");
        let (ax, ay) = centroid(&actual);
        let (rx, ry) = centroid(&reference_cover_crop(&image, to));
        let scale =
            (f64::from(to_width) / f64::from(width)).max(f64::from(to_height) / f64::from(height));
        // One source pixel is `scale` output pixels.
        assert!((ax - rx).abs() <= scale, "{label}: x {ax} vs {rx}");
        assert!((ay - ry).abs() <= scale, "{label}: y {ay} vs {ry}");
    }
}

/// Manual PF evidence for `docs/features/wallpapers.md`: the cover resize of
/// a 6000 × 4000 RGB8 image to 2560 × 1440, new against the reference. Run in
/// release with
/// `cargo test -p stache-wallpapers --release measure_cover_resize -- --ignored --nocapture`.
#[test]
#[ignore = "manual cover-resize measurement"]
fn measure_cover_resize_6000x4000_to_2560x1440() {
    use std::time::{Duration, Instant};

    let image = DynamicImage::ImageRgb8(textured(6000, 4000, 3));
    let to = target(2560, 1440);
    let median = |run: &dyn Fn()| {
        let mut runs: Vec<Duration> = (0..5)
            .map(|_| {
                let started = Instant::now();
                run();
                started.elapsed()
            })
            .collect();
        runs.sort_unstable();
        runs[runs.len() / 2]
    };
    let simd = median(&|| {
        std::hint::black_box(cover_crop(&image, to).expect("cover"));
    });
    let reference = median(&|| {
        std::hint::black_box(reference_cover_crop(&image, to));
    });
    let speedup = reference.as_secs_f64() / simd.as_secs_f64();
    println!(
        "cover resize 6000x4000 RGB8 -> 2560x1440: fast_image_resize {simd:?}, reference {reference:?}, {speedup:.2}x"
    );
    assert!(speedup >= 3.0, "speed-up {speedup:.2}x");
}

/// Asserts the cover resize of a solid `width`×`height` source succeeds at
/// exactly the target size.
fn assert_covers(width: u32, height: u32, to: TargetPixels) {
    let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(width, height, Rgb([90, 140, 200])));
    let output = cover_crop(&image, to)
        .unwrap_or_else(|error| panic!("{width}x{height} -> {}x{}: {error}", to.width, to.height));
    assert_eq!(output.dimensions(), (to.width, to.height), "{width}x{height}");
}

#[test]
fn sizes_whose_crop_edge_rounds_past_the_source_still_resize() {
    for (width, height) in [(2651, 1492), (2562, 1441), (1561, 878)] {
        assert_covers(width, height, target(2560, 1440));
    }
}

/// The crop box of every source in a band around the targets, plus upscales
/// and aspect ratios on both sides of the target's, is accepted by
/// `fast_image_resize`. A full Lanczos resize costs about 1.5 s per size in a
/// debug build, so the band checks the box with a nearest-neighbour resize to
/// one pixel: the library validates the box before and independently of the
/// algorithm and output size.
#[test]
fn every_source_size_near_the_target_has_a_crop_box_inside_the_source() {
    let mut sizes = Vec::new();
    for (to_width, to_height) in [(2560, 1440), (1920, 1200)] {
        for width in to_width - 2..=to_width + 120 {
            for height in to_height - 2..=to_height + 120 {
                sizes.push((width, height, to_width, to_height));
            }
        }
        for divisor in 2..=4 {
            for delta in 0..40 {
                let (width, height) = (to_width / divisor, to_height / divisor);
                sizes.push((width + delta, height + delta * 2, to_width, to_height));
                sizes.push((width + delta * 2, height + delta, to_width, to_height));
            }
        }
    }
    let pixels = vec![0_u8; (2680 * 1560 * 3) as usize];
    let mut failures = Vec::new();
    for (width, height, to_width, to_height) in sizes {
        let to = target(to_width, to_height);
        let scale =
            (f64::from(to_width) / f64::from(width)).max(f64::from(to_height) / f64::from(height));
        let scaled_width = scaled_edge(width, scale, to_width).expect("width");
        let scaled_height = scaled_edge(height, scale, to_height).expect("height");
        let (left, top, box_width, box_height) =
            cover_box(width, height, scaled_width, scaled_height, to);
        assert!(
            box_width > 0.0 && box_height > 0.0,
            "{width}x{height}: empty box"
        );
        let source = fir::images::TypedImageRef::<fir::pixels::U8x3>::from_buffer(
            width,
            height,
            &pixels[..(width * height * 3) as usize],
        )
        .expect("source");
        let mut one = [0_u8; 3];
        let mut output = fir::images::TypedImage::<fir::pixels::U8x3>::from_buffer(1, 1, &mut one)
            .expect("output");
        let options = fir::ResizeOptions::new()
            .resize_alg(fir::ResizeAlg::Nearest)
            .crop(left, top, box_width, box_height);
        if let Err(error) = fir::Resizer::new().resize_typed(&source, &mut output, &options) {
            failures.push(format!("{width}x{height} -> {to_width}x{to_height}: {error}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} sizes failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
