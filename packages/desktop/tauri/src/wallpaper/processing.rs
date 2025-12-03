//! Image processing for wallpapers.
//!
//! Provides functions to apply rounded corners and Gaussian blur effects to images,
//! and resize images to match the primary monitor dimensions.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, GenericImageView, ImageReader, Rgb, RgbImage};
use objc::runtime::{Class, Object};
use objc::{msg_send, sel, sel_impl};

use crate::config::WallpaperConfig;

/// Supported image file extensions.
const SUPPORTED_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png"];

/// Errors that can occur during image processing.
#[derive(Debug)]
#[allow(dead_code)]
pub enum ProcessingError {
    /// Failed to read the source image.
    ImageReadError(String),
    /// Failed to save the processed image.
    ImageSaveError(String),
    /// The specified path is not a valid image file.
    InvalidImagePath(String),
    /// Failed to create the cache directory.
    CacheDirectoryError(String),
}

impl std::fmt::Display for ProcessingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ImageReadError(path) => write!(f, "Failed to read image: {path}"),
            Self::ImageSaveError(path) => write!(f, "Failed to save processed image: {path}"),
            Self::InvalidImagePath(path) => write!(f, "Invalid image path: {path}"),
            Self::CacheDirectoryError(path) => {
                write!(f, "Failed to create cache directory: {path}")
            }
        }
    }
}

impl std::error::Error for ProcessingError {}

/// Objective-C type definitions for `NSScreen` frame.
#[repr(C)]
#[derive(Debug, Copy, Clone)]
struct NSRect {
    origin: NSPoint,
    size: NSSize,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
struct NSPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
struct NSSize {
    width: f64,
    height: f64,
}

/// Screen dimensions.
#[derive(Debug, Clone, Copy)]
pub struct ScreenSize {
    pub width: u32,
    pub height: u32,
}

impl ScreenSize {
    /// Returns a default screen size (4K) if detection fails.
    #[must_use]
    pub const fn default_4k() -> Self { Self { width: 3840, height: 2160 } }
}

/// Gets the primary screen dimensions using macOS APIs.
///
/// Returns a 4K fallback if screen detection fails.
#[must_use]
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
pub fn get_primary_screen_size() -> ScreenSize {
    unsafe {
        let Some(screen_class) = Class::get("NSScreen") else {
            return ScreenSize::default_4k();
        };

        let main_screen: *mut Object = msg_send![screen_class, mainScreen];
        if main_screen.is_null() {
            return ScreenSize::default_4k();
        }

        let frame: NSRect = msg_send![main_screen, frame];

        // Get the backing scale factor for Retina displays
        let scale: f64 = msg_send![main_screen, backingScaleFactor];

        // Calculate actual pixel dimensions
        let width = (frame.size.width * scale) as u32;
        let height = (frame.size.height * scale) as u32;

        if width == 0 || height == 0 {
            return ScreenSize::default_4k();
        }

        ScreenSize { width, height }
    }
}

/// Returns the cache directory for processed wallpapers.
///
/// Uses `~/Library/Caches/{APP_BUNDLE_ID}/wallpapers` on macOS for persistence across reboots.
/// Falls back to `/tmp/{APP_BUNDLE_ID}/wallpapers` if the home directory cannot be determined.
pub fn cache_dir() -> PathBuf {
    use crate::constants::APP_BUNDLE_ID;
    dirs::cache_dir().map_or_else(
        || PathBuf::from(format!("/tmp/{APP_BUNDLE_ID}/wallpapers")),
        |cache| cache.join(format!("{APP_BUNDLE_ID}/wallpapers")),
    )
}

/// Generates a unique cache filename based on the source file, processing parameters, and screen size.
/// Always uses JPEG format for fast saving.
fn cache_filename(source: &Path, config: &WallpaperConfig, screen: ScreenSize) -> String {
    let stem = source.file_stem().and_then(|s| s.to_str()).unwrap_or("wallpaper");
    format!(
        "{stem}_{}x{}_r{}_b{}.jpg",
        screen.width, screen.height, config.radius, config.blur
    )
}

/// Returns the full path to the cached processed image.
pub fn cached_path(source: &Path, config: &WallpaperConfig) -> PathBuf {
    let screen = get_primary_screen_size();
    cache_dir().join(cache_filename(source, config, screen))
}

/// Checks if a cached processed image exists and is valid.
#[allow(dead_code)]
pub fn is_cached(source: &Path, config: &WallpaperConfig) -> bool {
    let cache_path = cached_path(source, config);
    cache_path.exists()
}

/// Ensures the cache directory exists.
pub fn ensure_cache_dir() -> Result<(), ProcessingError> {
    let dir = cache_dir();
    if !dir.exists() {
        fs::create_dir_all(&dir)
            .map_err(|_| ProcessingError::CacheDirectoryError(dir.display().to_string()))?;
    }
    Ok(())
}

/// Checks if a file has a supported image extension.
pub fn is_supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| SUPPORTED_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
}

/// Lists all supported image files in a directory.
pub fn list_images_in_directory(dir: &Path) -> Vec<PathBuf> {
    if !dir.is_dir() {
        return Vec::new();
    }

    let mut images = Vec::new();

    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && is_supported_image(&path) {
                images.push(path);
            }
        }
    }

    // Sort for consistent ordering in sequential mode
    images.sort();
    images
}

/// Processes an image with the specified rounded corners and blur effects.
///
/// The image is resized to match the primary monitor dimensions, then
/// effects (blur, rounded corners) are applied.
///
/// # Arguments
///
/// * `source` - Path to the source image
/// * `config` - Wallpaper configuration containing radius and blur settings
///
/// # Returns
///
/// The path to the processed image in the cache directory.
pub fn process_image(source: &Path, config: &WallpaperConfig) -> Result<PathBuf, ProcessingError> {
    ensure_cache_dir()?;

    let screen = get_primary_screen_size();
    let cache_path = cached_path(source, config);

    // Return cached version if it exists
    if cache_path.exists() {
        return Ok(cache_path);
    }

    // Load the source image
    let img = ImageReader::open(source)
        .map_err(|_| ProcessingError::ImageReadError(source.display().to_string()))?
        .decode()
        .map_err(|_| ProcessingError::ImageReadError(source.display().to_string()))?;

    // Resize to screen dimensions
    let resized = resize_to_screen(&img, screen);

    // Apply processing (blur, rounded corners)
    let processed = apply_effects(resized, config.radius, config.blur);

    // Save as JPEG with high quality (much faster than PNG)
    let file = File::create(&cache_path)
        .map_err(|_| ProcessingError::ImageSaveError(cache_path.display().to_string()))?;
    let writer = BufWriter::new(file);
    let encoder = JpegEncoder::new_with_quality(writer, 95);
    processed
        .to_rgb8()
        .write_with_encoder(encoder)
        .map_err(|_| ProcessingError::ImageSaveError(cache_path.display().to_string()))?;

    Ok(cache_path)
}

/// Resizes an image to cover the screen dimensions while maintaining aspect ratio.
///
/// Uses "cover" scaling: the image is scaled to fill the entire screen,
/// cropping edges if necessary to avoid letterboxing.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn resize_to_screen(img: &DynamicImage, screen: ScreenSize) -> DynamicImage {
    let (img_width, img_height) = img.dimensions();
    let target_width = screen.width;
    let target_height = screen.height;

    // Calculate scale factors
    let scale_x = f64::from(target_width) / f64::from(img_width);
    let scale_y = f64::from(target_height) / f64::from(img_height);

    // Use "cover" scaling: scale to fill the entire target, cropping if needed
    let scale = scale_x.max(scale_y);

    let scaled_width = (f64::from(img_width) * scale) as u32;
    let scaled_height = (f64::from(img_height) * scale) as u32;

    // Resize the image using CatmullRom (good quality, much faster than Lanczos3)
    let resized = img.resize_exact(
        scaled_width,
        scaled_height,
        image::imageops::FilterType::CatmullRom,
    );

    // Crop to exact target dimensions (center crop)
    let crop_x = (scaled_width.saturating_sub(target_width)) / 2;
    let crop_y = (scaled_height.saturating_sub(target_height)) / 2;

    resized.crop_imm(crop_x, crop_y, target_width, target_height)
}

/// Applies rounded corners and blur effects to an image.
#[allow(clippy::cast_precision_loss)]
fn apply_effects(img: DynamicImage, radius: u32, blur: u32) -> DynamicImage {
    let mut result = img;

    // Apply fast box blur if specified (approximates Gaussian blur)
    if blur > 0 {
        result = apply_fast_blur(&result, blur);
    }

    // Apply rounded corners if specified
    if radius > 0 {
        result = apply_rounded_corners(&result, radius * 2);
    }

    result
}

/// Applies a fast box blur approximation to an image.
///
/// Uses multiple passes of box blur to approximate Gaussian blur.
/// Much faster than true Gaussian blur for large blur radii.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn apply_fast_blur(img: &DynamicImage, blur_radius: u32) -> DynamicImage {
    // For small blur values, use the built-in blur (acceptable performance)
    // For larger values, we'd use box blur, but the built-in is fine for typical values
    if blur_radius <= 5 {
        return img.blur(blur_radius as f32);
    }

    // For larger blur values, use a scaled approach:
    // Downscale -> blur at smaller size -> upscale
    let (width, height) = img.dimensions();
    let scale_factor = 4u32; // Downscale by 4x for faster processing

    let small_width = (width / scale_factor).max(1);
    let small_height = (height / scale_factor).max(1);

    // Downscale
    let small = img.resize_exact(small_width, small_height, image::imageops::FilterType::Triangle);

    // Apply blur at smaller size (blur radius also scaled down)
    let blur_at_scale = (blur_radius / scale_factor).max(1);
    let blurred_small = small.blur(blur_at_scale as f32);

    // Upscale back
    blurred_small.resize_exact(width, height, image::imageops::FilterType::Triangle)
}

/// Number of samples per axis for supersampling anti-aliasing.
const AA_SAMPLES: u32 = 4;

/// Applies rounded corners to an image using black fill with high-quality anti-aliasing.
///
/// Uses supersampling for smooth edges. macOS wallpapers don't support transparency,
/// so we fill corners with black.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn apply_rounded_corners(img: &DynamicImage, radius: u32) -> DynamicImage {
    let (width, height) = img.dimensions();
    let mut rgb = img.to_rgb8();

    // Cap radius to half of the smallest dimension
    let max_radius = width.min(height) / 2;
    let radius = radius.min(max_radius);

    if radius == 0 {
        return DynamicImage::ImageRgb8(rgb);
    }

    let radius_f = f64::from(radius);
    let black = Rgb([0u8, 0, 0]);

    // Use 4x4 supersampling for high-quality anti-aliasing
    let sample_step = 1.0 / f64::from(AA_SAMPLES);

    // Process each corner
    for y in 0..radius {
        for x in 0..radius {
            // Calculate coverage using supersampling
            let mut coverage = 0.0_f64;

            for sy in 0..AA_SAMPLES {
                for sx in 0..AA_SAMPLES {
                    let sample_x = (f64::from(sx) + 0.5).mul_add(sample_step, f64::from(x));
                    let sample_y = (f64::from(sy) + 0.5).mul_add(sample_step, f64::from(y));

                    let dx = radius_f - sample_x;
                    let dy = radius_f - sample_y;
                    let distance = dx.hypot(dy);

                    if distance <= radius_f {
                        coverage += 1.0;
                    }
                }
            }

            coverage /= f64::from(AA_SAMPLES * AA_SAMPLES);

            if coverage == 0.0 {
                // Fully outside - fill with black
                set_pixel_black(&mut rgb, x, y, black);
                set_pixel_black(&mut rgb, width - 1 - x, y, black);
                set_pixel_black(&mut rgb, x, height - 1 - y, black);
                set_pixel_black(&mut rgb, width - 1 - x, height - 1 - y, black);
            } else if coverage < 1.0 {
                // Partial coverage - anti-alias
                let blend = coverage as f32;
                blend_pixel_with_black(&mut rgb, x, y, blend);
                blend_pixel_with_black(&mut rgb, width - 1 - x, y, blend);
                blend_pixel_with_black(&mut rgb, x, height - 1 - y, blend);
                blend_pixel_with_black(&mut rgb, width - 1 - x, height - 1 - y, blend);
            }
            // coverage == 1.0: fully inside, keep original pixel
        }
    }

    DynamicImage::ImageRgb8(rgb)
}

/// Sets a pixel to black.
fn set_pixel_black(img: &mut RgbImage, x: u32, y: u32, black: Rgb<u8>) {
    if let Some(pixel) = img.get_pixel_mut_checked(x, y) {
        *pixel = black;
    }
}

/// Blends a pixel with black based on a blend factor (0.0 = black, 1.0 = original).
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn blend_pixel_with_black(img: &mut RgbImage, x: u32, y: u32, factor: f32) {
    if let Some(pixel) = img.get_pixel_mut_checked(x, y) {
        pixel[0] = (f32::from(pixel[0]) * factor) as u8;
        pixel[1] = (f32::from(pixel[1]) * factor) as u8;
        pixel[2] = (f32::from(pixel[2]) * factor) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_supported_image() {
        assert!(is_supported_image(Path::new("test.jpg")));
        assert!(is_supported_image(Path::new("test.JPEG")));
        assert!(is_supported_image(Path::new("test.png")));
        assert!(!is_supported_image(Path::new("test.webp")));
        assert!(!is_supported_image(Path::new("test.tiff")));
        assert!(!is_supported_image(Path::new("test.bmp")));
        assert!(!is_supported_image(Path::new("test.txt")));
        assert!(!is_supported_image(Path::new("test.mp4")));
    }

    #[test]
    fn test_cache_filename() {
        let config = WallpaperConfig {
            radius: 10,
            blur: 5,
            ..Default::default()
        };
        let screen = ScreenSize { width: 1920, height: 1080 };

        let filename = cache_filename(Path::new("/path/to/wallpaper.jpg"), &config, screen);
        assert_eq!(filename, "wallpaper_1920x1080_r10_b5.jpg");
    }

    #[test]
    fn test_cached_path() {
        // Note: cached_path uses get_primary_screen_size() internally,
        // so we just verify it returns a valid path format
        let config = WallpaperConfig {
            radius: 10,
            blur: 5,
            ..Default::default()
        };

        let path = cached_path(Path::new("/path/to/wallpaper.png"), &config);
        // The path should contain the screen size, radius, and blur (always .jpg for performance)
        let path_str = path.to_string_lossy();
        assert!(path_str.contains("wallpaper_"));
        assert!(path_str.contains("_r10_b5.jpg"));
    }

    #[test]
    fn test_screen_size_default() {
        let default = ScreenSize::default_4k();
        assert_eq!(default.width, 3840);
        assert_eq!(default.height, 2160);
    }

    #[test]
    fn test_get_primary_screen_size() {
        let screen = get_primary_screen_size();
        // Should return reasonable dimensions (not zero)
        assert!(screen.width > 0);
        assert!(screen.height > 0);
    }

    #[test]
    fn test_rounded_corners_are_applied() {
        // Create a 100x100 white image
        let white_img =
            DynamicImage::ImageRgb8(RgbImage::from_fn(100, 100, |_, _| Rgb([255u8, 255, 255])));

        // Apply rounded corners with radius 16
        let result = apply_rounded_corners(&white_img, 16 * 2);
        let rgb = result.to_rgb8();

        // Top-left corner (0,0) should be black (outside the rounded corner)
        let corner_pixel = rgb.get_pixel(0, 0);
        assert_eq!(
            corner_pixel,
            &Rgb([0u8, 0, 0]),
            "Top-left corner should be black"
        );

        // Pixel at (16, 16) should be white (inside the corner radius)
        let inside_pixel = rgb.get_pixel(16, 16);
        assert_eq!(
            inside_pixel,
            &Rgb([255u8, 255, 255]),
            "Inside corner should remain white"
        );

        // Bottom-right corner should also be black
        let br_corner = rgb.get_pixel(99, 99);
        assert_eq!(
            br_corner,
            &Rgb([0u8, 0, 0]),
            "Bottom-right corner should be black"
        );
    }

    #[test]
    fn test_resize_to_screen() {
        // Create a 200x100 image (2:1 aspect ratio)
        let img =
            DynamicImage::ImageRgb8(RgbImage::from_fn(200, 100, |_, _| Rgb([128u8, 128, 128])));

        // Target screen is 100x100 (1:1 aspect ratio)
        let screen = ScreenSize { width: 100, height: 100 };

        let resized = resize_to_screen(&img, screen);
        let (w, h) = resized.dimensions();

        // Should be exactly screen size
        assert_eq!(w, 100);
        assert_eq!(h, 100);
    }
}
