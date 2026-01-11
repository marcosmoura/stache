//! Image processing for wallpapers.
//!
//! Provides functions to apply rounded corners and Gaussian blur effects to images,
//! and resize images to match the primary monitor dimensions.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, GenericImageView, ImageReader, Rgb, RgbImage};
use natord::compare;
use objc::runtime::{Class, Object};
use objc::{msg_send, sel, sel_impl};

use crate::cache::get_cache_subdir;
use crate::config::WallpaperConfig;

/// Supported image file extensions.
const SUPPORTED_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp"];

/// Errors that can occur during image processing.
#[derive(Debug)]
pub enum ProcessingError {
    /// Failed to read the source image.
    ImageRead(String),
    /// Failed to save the processed image.
    ImageSave(String),
    /// Failed to create the cache directory.
    CacheDirectory(String),
}

impl std::fmt::Display for ProcessingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ImageRead(path) => write!(f, "Failed to read image: {path}"),
            Self::ImageSave(path) => write!(f, "Failed to save processed image: {path}"),
            Self::CacheDirectory(path) => {
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
    /// Returns a default screen size (2K) if detection fails.
    #[must_use]
    pub const fn default_2k() -> Self { Self { width: 2560, height: 1440 } }
}

/// Gets the primary screen dimensions using macOS APIs.
///
/// Returns a 2K fallback if screen detection fails.
#[must_use]
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
pub fn get_primary_screen_size() -> ScreenSize {
    unsafe {
        let Some(screen_class) = Class::get("NSScreen") else {
            return ScreenSize::default_2k();
        };

        let main_screen: *mut Object = msg_send![screen_class, mainScreen];
        if main_screen.is_null() {
            return ScreenSize::default_2k();
        }

        let frame: NSRect = msg_send![main_screen, frame];

        // Get the backing scale factor for Retina displays
        let scale: f64 = msg_send![main_screen, backingScaleFactor];

        // Calculate actual pixel dimensions
        let width = (frame.size.width * scale) as u32;
        let height = (frame.size.height * scale) as u32;

        if width == 0 || height == 0 {
            return ScreenSize::default_2k();
        }

        ScreenSize { width, height }
    }
}

/// Gets the screen dimensions for a specific screen by index.
///
/// Returns a 2K fallback if screen detection fails or the index is invalid.
#[must_use]
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
pub fn get_screen_size(screen_index: usize) -> ScreenSize {
    unsafe {
        let Some(screen_class) = Class::get("NSScreen") else {
            return ScreenSize::default_2k();
        };

        let screens: *mut Object = msg_send![screen_class, screens];
        if screens.is_null() {
            return ScreenSize::default_2k();
        }

        let count: usize = msg_send![screens, count];
        if screen_index >= count {
            return ScreenSize::default_2k();
        }

        let screen: *mut Object = msg_send![screens, objectAtIndex: screen_index];
        if screen.is_null() {
            return ScreenSize::default_2k();
        }

        let frame: NSRect = msg_send![screen, frame];

        // Get the backing scale factor for Retina displays
        let scale: f64 = msg_send![screen, backingScaleFactor];

        // Calculate actual pixel dimensions
        let width = (frame.size.width * scale) as u32;
        let height = (frame.size.height * scale) as u32;

        if width == 0 || height == 0 {
            return ScreenSize::default_2k();
        }

        ScreenSize { width, height }
    }
}

/// Returns the number of available screens.
///
/// Returns 1 as fallback if screen detection fails.
#[must_use]
pub fn get_screen_count() -> usize {
    unsafe {
        let Some(screen_class) = Class::get("NSScreen") else {
            return 1;
        };

        let screens: *mut Object = msg_send![screen_class, screens];
        if screens.is_null() {
            return 1;
        }

        let count: usize = msg_send![screens, count];
        if count == 0 { 1 } else { count }
    }
}

/// Returns the cache directory for processed wallpapers.
///
/// Uses `~/Library/Caches/{APP_BUNDLE_ID}/wallpapers` on macOS for persistence across reboots.
/// Falls back to `/tmp/{APP_BUNDLE_ID}/wallpapers` if the home directory cannot be determined.
pub fn cache_dir() -> PathBuf { get_cache_subdir("wallpapers") }

/// Generates a unique cache filename based on the source file, processing parameters, and screen size.
/// Always uses JPEG format for fast saving.
fn cache_filename(source: &Path, config: &WallpaperConfig, screen: ScreenSize) -> String {
    let stem = source.file_stem().and_then(|s| s.to_str()).unwrap_or("wallpaper");
    format!(
        "{stem}_{}x{}_r{}_b{}.jpg",
        screen.width, screen.height, config.radius, config.blur
    )
}

/// Generates a unique cache filename for a specific screen.
fn cache_filename_for_screen(
    source: &Path,
    config: &WallpaperConfig,
    screen: ScreenSize,
    screen_index: usize,
) -> String {
    let stem = source.file_stem().and_then(|s| s.to_str()).unwrap_or("wallpaper");
    format!(
        "{stem}_s{screen_index}_{}x{}_r{}_b{}.jpg",
        screen.width, screen.height, config.radius, config.blur
    )
}

/// Returns the full path to the cached processed image.
pub fn cached_path(source: &Path, config: &WallpaperConfig) -> PathBuf {
    let screen = get_primary_screen_size();
    cache_dir().join(cache_filename(source, config, screen))
}

/// Returns the full path to the cached processed image for a specific screen.
pub fn cached_path_for_screen(
    source: &Path,
    config: &WallpaperConfig,
    screen_index: usize,
) -> PathBuf {
    let screen = get_screen_size(screen_index);
    cache_dir().join(cache_filename_for_screen(source, config, screen, screen_index))
}

/// Ensures the cache directory exists.
pub fn ensure_cache_dir() -> Result<(), ProcessingError> {
    let dir = cache_dir();
    if !dir.exists() {
        fs::create_dir_all(&dir)
            .map_err(|_| ProcessingError::CacheDirectory(dir.display().to_string()))?;
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

    // Sort for consistent ordering in sequential mode using natural/human sorting
    images.sort_by(|a, b| compare(a.to_string_lossy().as_ref(), b.to_string_lossy().as_ref()));
    images
}

/// Internal image processing implementation.
///
/// Loads, resizes, applies effects, and saves the processed image to the cache.
/// This is the common logic shared by `process_image` and `process_image_for_screen`.
fn process_image_internal(
    source: &Path,
    config: &WallpaperConfig,
    cache_path: PathBuf,
    screen: ScreenSize,
) -> Result<PathBuf, ProcessingError> {
    // Return cached version if it exists
    if cache_path.exists() {
        return Ok(cache_path);
    }

    // Load the source image
    let img = ImageReader::open(source)
        .map_err(|_| ProcessingError::ImageRead(source.display().to_string()))?
        .decode()
        .map_err(|_| ProcessingError::ImageRead(source.display().to_string()))?;

    // Resize to screen dimensions
    let resized = resize_to_screen(&img, screen);

    // Apply processing (blur, rounded corners)
    let processed = apply_effects(resized, config.radius, config.blur);

    // Save as JPEG with high quality (much faster than PNG)
    let file = File::create(&cache_path)
        .map_err(|_| ProcessingError::ImageSave(cache_path.display().to_string()))?;
    let writer = BufWriter::new(file);
    let encoder = JpegEncoder::new_with_quality(writer, 95);
    processed
        .to_rgb8()
        .write_with_encoder(encoder)
        .map_err(|_| ProcessingError::ImageSave(cache_path.display().to_string()))?;

    Ok(cache_path)
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

    process_image_internal(source, config, cache_path, screen)
}

/// Processes an image for a specific screen.
///
/// The image is resized to match the specified screen's dimensions, then
/// effects (blur, rounded corners) are applied.
///
/// # Arguments
///
/// * `source` - Path to the source image
/// * `config` - Wallpaper configuration containing radius and blur settings
/// * `screen_index` - The 0-based index of the target screen
///
/// # Returns
///
/// The path to the processed image in the cache directory.
pub fn process_image_for_screen(
    source: &Path,
    config: &WallpaperConfig,
    screen_index: usize,
) -> Result<PathBuf, ProcessingError> {
    ensure_cache_dir()?;

    let screen = get_screen_size(screen_index);
    let cache_path = cached_path_for_screen(source, config, screen_index);

    process_image_internal(source, config, cache_path, screen)
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
        result = apply_rounded_corners(&result, radius);
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

    // Downscale using CatmullRom for better quality
    let small = img.resize_exact(
        small_width,
        small_height,
        image::imageops::FilterType::CatmullRom,
    );

    // Apply blur at smaller size (blur radius also scaled down)
    let blur_at_scale = (blur_radius / scale_factor).max(1);
    let blurred_small = small.blur(blur_at_scale as f32);

    // Upscale back using CatmullRom for smoother result
    blurred_small.resize_exact(width, height, image::imageops::FilterType::CatmullRom)
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
        assert!(is_supported_image(Path::new("test.webp")));
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
        let default = ScreenSize::default_2k();
        assert_eq!(default.width, 2560);
        assert_eq!(default.height, 1440);
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
        let result = apply_rounded_corners(&white_img, 16);
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

    // ========================================================================
    // ScreenSize tests
    // ========================================================================

    #[test]
    fn test_screen_size_copy_and_clone() {
        let original = ScreenSize { width: 1920, height: 1080 };
        let copied = original; // Copy
        let cloned = original.clone();

        assert_eq!(original.width, copied.width);
        assert_eq!(original.height, copied.height);
        assert_eq!(original.width, cloned.width);
        assert_eq!(original.height, cloned.height);
    }

    // ========================================================================
    // ProcessingError tests
    // ========================================================================

    #[test]
    fn test_processing_error_display_image_read() {
        let err = ProcessingError::ImageRead("/path/to/image.jpg".to_string());
        let display = err.to_string();
        assert!(display.contains("Failed to read image"));
        assert!(display.contains("/path/to/image.jpg"));
    }

    #[test]
    fn test_processing_error_display_image_save() {
        let err = ProcessingError::ImageSave("/cache/output.jpg".to_string());
        let display = err.to_string();
        assert!(display.contains("Failed to save processed image"));
        assert!(display.contains("/cache/output.jpg"));
    }

    #[test]
    fn test_processing_error_display_cache_directory() {
        let err = ProcessingError::CacheDirectory("permission denied".to_string());
        let display = err.to_string();
        assert!(display.contains("Failed to create cache directory"));
        assert!(display.contains("permission denied"));
    }

    #[test]
    fn test_processing_error_is_debug() {
        let err = ProcessingError::ImageRead("test.jpg".to_string());
        let debug = format!("{:?}", err);
        assert!(debug.contains("ImageRead"));
    }

    // ========================================================================
    // Cache filename tests
    // ========================================================================

    #[test]
    fn test_cache_filename_different_configs_produce_different_names() {
        let screen = ScreenSize { width: 1920, height: 1080 };
        let path = Path::new("/test/image.jpg");

        let config1 = WallpaperConfig {
            radius: 10,
            blur: 5,
            ..Default::default()
        };
        let config2 = WallpaperConfig {
            radius: 20,
            blur: 5,
            ..Default::default()
        };
        let config3 = WallpaperConfig {
            radius: 10,
            blur: 10,
            ..Default::default()
        };

        let name1 = cache_filename(path, &config1, screen);
        let name2 = cache_filename(path, &config2, screen);
        let name3 = cache_filename(path, &config3, screen);

        assert_ne!(name1, name2);
        assert_ne!(name1, name3);
        assert_ne!(name2, name3);
    }

    #[test]
    fn test_cache_filename_different_screens_produce_different_names() {
        let config = WallpaperConfig {
            radius: 10,
            blur: 5,
            ..Default::default()
        };
        let path = Path::new("/test/image.jpg");

        let screen1 = ScreenSize { width: 1920, height: 1080 };
        let screen2 = ScreenSize { width: 2560, height: 1440 };

        let name1 = cache_filename(path, &config, screen1);
        let name2 = cache_filename(path, &config, screen2);

        assert_ne!(name1, name2);
    }

    #[test]
    fn test_cache_filename_for_screen_includes_screen_index() {
        let config = WallpaperConfig {
            radius: 10,
            blur: 5,
            ..Default::default()
        };
        let path = Path::new("/test/image.jpg");

        let name0 =
            cache_filename_for_screen(path, &config, ScreenSize { width: 1920, height: 1080 }, 0);
        let name1 =
            cache_filename_for_screen(path, &config, ScreenSize { width: 1920, height: 1080 }, 1);

        assert!(name0.contains("_s0_"));
        assert!(name1.contains("_s1_"));
        assert_ne!(name0, name1);
    }

    #[test]
    fn test_cache_filename_handles_no_extension() {
        let config = WallpaperConfig::default();
        let screen = ScreenSize { width: 1920, height: 1080 };
        let path = Path::new("/test/imagefile");

        let name = cache_filename(path, &config, screen);
        // Should use the stem (filename without extension) or full filename
        assert!(name.contains("imagefile"));
        assert!(name.ends_with(".jpg"));
    }

    // ========================================================================
    // is_supported_image edge cases
    // ========================================================================

    #[test]
    fn test_is_supported_image_mixed_case() {
        assert!(is_supported_image(Path::new("test.JpG")));
        assert!(is_supported_image(Path::new("test.PNG")));
        assert!(is_supported_image(Path::new("test.WeBp")));
        assert!(is_supported_image(Path::new("test.JPEG")));
    }

    #[test]
    fn test_is_supported_image_no_extension() {
        assert!(!is_supported_image(Path::new("imagefile")));
        assert!(!is_supported_image(Path::new(".")));
        assert!(!is_supported_image(Path::new("..")));
    }

    #[test]
    fn test_is_supported_image_hidden_file() {
        assert!(is_supported_image(Path::new(".hidden.jpg")));
        assert!(!is_supported_image(Path::new(".hidden")));
    }

    #[test]
    fn test_is_supported_image_double_extension() {
        // Should only check last extension
        assert!(is_supported_image(Path::new("test.tar.jpg")));
        assert!(!is_supported_image(Path::new("test.jpg.tar")));
    }

    // ========================================================================
    // resize_to_screen edge cases
    // ========================================================================

    #[test]
    fn test_resize_to_screen_tall_image() {
        // Create a 100x200 image (1:2 aspect ratio, taller than wide)
        let img =
            DynamicImage::ImageRgb8(RgbImage::from_fn(100, 200, |_, _| Rgb([128u8, 128, 128])));

        // Target screen is 100x100 (1:1 aspect ratio)
        let screen = ScreenSize { width: 100, height: 100 };

        let resized = resize_to_screen(&img, screen);
        let (w, h) = resized.dimensions();

        // Should be exactly screen size (cover scaling)
        assert_eq!(w, 100);
        assert_eq!(h, 100);
    }

    #[test]
    fn test_resize_to_screen_same_aspect_ratio() {
        // Create a small 16:9 image (same aspect ratio as target)
        let img = DynamicImage::ImageRgb8(RgbImage::from_fn(160, 90, |_, _| Rgb([64u8, 64, 64])));

        // Target screen is also 16:9 but larger
        let screen = ScreenSize { width: 320, height: 180 };

        let resized = resize_to_screen(&img, screen);
        let (w, h) = resized.dimensions();

        assert_eq!(w, 320);
        assert_eq!(h, 180);
    }

    #[test]
    fn test_resize_to_screen_square_image() {
        // Create a small square image
        let img = DynamicImage::ImageRgb8(RgbImage::from_fn(50, 50, |_, _| Rgb([200u8, 200, 200])));

        // Target is 16:9 aspect ratio
        let screen = ScreenSize { width: 160, height: 90 };

        let resized = resize_to_screen(&img, screen);
        let (w, h) = resized.dimensions();

        assert_eq!(w, 160);
        assert_eq!(h, 90);
    }

    // ========================================================================
    // apply_fast_blur tests
    // ========================================================================

    #[test]
    fn test_apply_fast_blur_small_radius() {
        let img =
            DynamicImage::ImageRgb8(RgbImage::from_fn(100, 100, |_, _| Rgb([128u8, 128, 128])));

        let blurred = apply_fast_blur(&img, 3);
        let (w, h) = blurred.dimensions();

        // Dimensions should be preserved
        assert_eq!(w, 100);
        assert_eq!(h, 100);
    }

    #[test]
    fn test_apply_fast_blur_large_radius_uses_scale() {
        let img =
            DynamicImage::ImageRgb8(RgbImage::from_fn(200, 200, |_, _| Rgb([100u8, 100, 100])));

        let blurred = apply_fast_blur(&img, 20);
        let (w, h) = blurred.dimensions();

        // Dimensions should be preserved after scale down and up
        assert_eq!(w, 200);
        assert_eq!(h, 200);
    }

    #[test]
    fn test_apply_fast_blur_zero_radius() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_fn(50, 50, |x, y| {
            Rgb([(x as u8), (y as u8), 128])
        }));

        // Zero radius should be handled (no blur applied due to condition)
        // But the function doesn't guard against 0, so it would call blur(0.0)
        // which is essentially a no-op
        let result = apply_fast_blur(&img, 0);
        assert_eq!(result.dimensions(), (50, 50));
    }

    // ========================================================================
    // apply_rounded_corners edge cases
    // ========================================================================

    #[test]
    fn test_apply_rounded_corners_zero_radius() {
        let img =
            DynamicImage::ImageRgb8(RgbImage::from_fn(100, 100, |_, _| Rgb([255u8, 255, 255])));

        let result = apply_rounded_corners(&img, 0);
        let rgb = result.to_rgb8();

        // With zero radius, corners should remain unchanged (white)
        let corner = rgb.get_pixel(0, 0);
        assert_eq!(corner, &Rgb([255u8, 255, 255]));
    }

    #[test]
    fn test_apply_rounded_corners_max_radius_cap() {
        // Create a small 20x20 image
        let img = DynamicImage::ImageRgb8(RgbImage::from_fn(20, 20, |_, _| Rgb([255u8, 255, 255])));

        // Request radius larger than half the image size
        let result = apply_rounded_corners(&img, 100);

        // Should not panic and should handle the cap correctly
        let (w, h) = result.dimensions();
        assert_eq!(w, 20);
        assert_eq!(h, 20);
    }

    #[test]
    fn test_apply_rounded_corners_all_corners_affected() {
        let img =
            DynamicImage::ImageRgb8(RgbImage::from_fn(100, 100, |_, _| Rgb([255u8, 255, 255])));

        let result = apply_rounded_corners(&img, 16);
        let rgb = result.to_rgb8();

        // All four corners should be black
        assert_eq!(rgb.get_pixel(0, 0), &Rgb([0u8, 0, 0]), "Top-left");
        assert_eq!(rgb.get_pixel(99, 0), &Rgb([0u8, 0, 0]), "Top-right");
        assert_eq!(rgb.get_pixel(0, 99), &Rgb([0u8, 0, 0]), "Bottom-left");
        assert_eq!(rgb.get_pixel(99, 99), &Rgb([0u8, 0, 0]), "Bottom-right");
    }

    // ========================================================================
    // apply_effects tests
    // ========================================================================

    #[test]
    fn test_apply_effects_no_effects() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_fn(50, 50, |_, _| Rgb([128u8, 128, 128])));

        let result = apply_effects(img.clone(), 0, 0);

        // Should be essentially unchanged
        assert_eq!(result.dimensions(), (50, 50));
    }

    #[test]
    fn test_apply_effects_blur_only() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_fn(50, 50, |_, _| Rgb([128u8, 128, 128])));

        let result = apply_effects(img, 0, 5);
        assert_eq!(result.dimensions(), (50, 50));
    }

    #[test]
    fn test_apply_effects_radius_only() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_fn(50, 50, |_, _| Rgb([255u8, 255, 255])));

        let result = apply_effects(img, 10, 0);
        let rgb = result.to_rgb8();

        // Corner should be black due to rounded corners
        assert_eq!(rgb.get_pixel(0, 0), &Rgb([0u8, 0, 0]));
    }

    #[test]
    fn test_apply_effects_both() {
        let img =
            DynamicImage::ImageRgb8(RgbImage::from_fn(100, 100, |_, _| Rgb([200u8, 200, 200])));

        let result = apply_effects(img, 10, 5);
        let rgb = result.to_rgb8();

        // Dimensions preserved
        assert_eq!(result.dimensions(), (100, 100));
        // Corner should be black
        assert_eq!(rgb.get_pixel(0, 0), &Rgb([0u8, 0, 0]));
    }

    // ========================================================================
    // get_screen_size tests
    // ========================================================================

    #[test]
    fn test_get_screen_size_zero_index() {
        let screen = get_screen_size(0);
        // Should return valid dimensions
        assert!(screen.width > 0);
        assert!(screen.height > 0);
    }

    #[test]
    fn test_get_screen_size_high_index_fallback() {
        // Very high index should fall back to default
        let screen = get_screen_size(999);
        // Should still return valid dimensions (fallback)
        assert!(screen.width > 0);
        assert!(screen.height > 0);
    }

    // ========================================================================
    // get_screen_count tests
    // ========================================================================

    #[test]
    fn test_get_screen_count_returns_at_least_one() {
        let count = get_screen_count();
        assert!(count >= 1);
    }

    // ========================================================================
    // Constants tests
    // ========================================================================

    #[test]
    fn test_supported_extensions_not_empty() {
        assert!(!SUPPORTED_EXTENSIONS.is_empty());
    }

    #[test]
    fn test_supported_extensions_are_lowercase() {
        for ext in SUPPORTED_EXTENSIONS {
            assert_eq!(
                *ext,
                ext.to_lowercase(),
                "Extension should be lowercase: {}",
                ext
            );
        }
    }

    #[test]
    fn test_aa_samples_is_reasonable() {
        assert!(AA_SAMPLES >= 2);
        assert!(AA_SAMPLES <= 16);
    }
}
