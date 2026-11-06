use image::{DynamicImage, GenericImageView, Rgb, imageops};

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

#[test]
fn a_non_positive_target_is_rejected() {
    let error = cover_crop(&DynamicImage::ImageRgb8(solid(4, 4, [0, 0, 0])), target(0, 1))
        .expect_err("zero width");
    assert!(matches!(error, ProcessError::Resource(_)), "{error:?}");
}
