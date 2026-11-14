//! Decode, cover-resize, effect and PNG encode primitives.
//!
//! The order matches the legacy processor: decode, cover resize and center
//! crop to the exact target in one step (SIMD Lanczos3 convolution from
//! `fast_image_resize`, reading only the crop window of the source), blur, black rounded corners, then RGB8 PNG
//! with Fast compression and adaptive filtering. Zero effects skip their
//! stages. The compression level changes only the file size, never the
//! decoded pixels, so it is not part of [`PROCESSING_VERSION`]. Source bytes
//! are borrowed and never rewritten.
//!
//! Codec calls are noninterruptible: cancellation is observed between stages,
//! never inside the decode, resize, blur or encode calls. That is the
//! honest boundary of a synchronous codec.

use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use fast_image_resize as fir;
use image::codecs::png::{CompressionType, FilterType as PngFilterType, PngEncoder};
use image::{DynamicImage, GenericImageView, ImageFormat, ImageReader, Rgb, RgbImage, imageops};

use crate::cache::CacheError;
use crate::limits::{self, ResourceError};
use crate::selection::TargetPixels;

/// The processing-version component of every cache key.
///
/// Bump this when decoding, resizing, effect or encoding behavior changes so
/// old cache files stop matching.
pub const PROCESSING_VERSION: u32 = 2;

/// Blur and rounded-corner settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Effects {
    /// Rounded-corner radius in pixels.
    pub radius: u32,
    /// Blur sigma in pixels.
    pub blur: u32,
}

/// Why one output could not be produced.
#[derive(Debug)]
pub enum ProcessError {
    /// The input exceeded an internal resource bound.
    Resource(ResourceError),
    /// A source or output filesystem operation failed.
    Io {
        /// What was attempted.
        action: &'static str,
        /// The affected path.
        path: PathBuf,
        /// The underlying failure.
        error: io::Error,
    },
    /// The source is not a regular file.
    NotRegularFile {
        /// The rejected source.
        path: PathBuf,
    },
    /// The source changed while it was being read.
    SourceChanged {
        /// The replaced source.
        path: PathBuf,
    },
    /// The image codec could not read the source.
    Decode {
        /// The source path.
        path: PathBuf,
        /// The codec's message.
        message: String,
    },
    /// The PNG encoder failed.
    Encode {
        /// The output path.
        path: PathBuf,
        /// The codec's message.
        message: String,
    },
    /// The cover resize failed.
    Resize {
        /// The resizer's message.
        message: String,
    },
    /// Cache validation, lookup or publication failed.
    Cache(CacheError),
    /// The cache lease could not be verified.
    Lease(stache_sdk::CacheLeaseError),
    /// Cancellation was requested before the next stage.
    Cancelled,
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resource(error) => error.fmt(f),
            Self::Io { action, path, error } => {
                write!(f, "cannot {action} {}: {error}", path.display())
            }
            Self::NotRegularFile { path } => {
                write!(f, "source is not a regular file: {}", path.display())
            }
            Self::SourceChanged { path } => {
                write!(f, "source changed while it was being read: {}", path.display())
            }
            Self::Decode { path, message } => {
                write!(f, "cannot decode {}: {message}", path.display())
            }
            Self::Encode { path, message } => {
                write!(f, "cannot encode {}: {message}", path.display())
            }
            Self::Resize { message } => write!(f, "cannot resize the wallpaper: {message}"),
            Self::Cache(error) => error.fmt(f),
            Self::Lease(error) => error.fmt(f),
            Self::Cancelled => f.write_str("wallpaper processing was cancelled"),
        }
    }
}

impl std::error::Error for ProcessError {}

impl From<ResourceError> for ProcessError {
    fn from(error: ResourceError) -> Self { Self::Resource(error) }
}

impl From<CacheError> for ProcessError {
    fn from(error: CacheError) -> Self { Self::Cache(error) }
}

/// Decodes one of the advertised formats from the exact source bytes.
///
/// # Errors
/// Returns a typed resource, decode or format error before any processing.
pub fn decode(source: &Path, bytes: &[u8]) -> Result<DynamicImage, ProcessError> {
    let reader =
        ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|error| ProcessError::Decode {
                path: source.to_path_buf(),
                message: error.to_string(),
            })?;
    let Some(format) = reader.format() else {
        return Err(ProcessError::Decode {
            path: source.to_path_buf(),
            message: "the file is not a recognized image".to_owned(),
        });
    };
    if !matches!(format, ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP) {
        return Err(ProcessError::Decode {
            path: source.to_path_buf(),
            message: format!("{format:?} is not an advertised wallpaper format"),
        });
    }
    let (width, height) = reader.into_dimensions().map_err(|error| ProcessError::Decode {
        path: source.to_path_buf(),
        message: error.to_string(),
    })?;
    limits::check_source_dimensions(source, width, height)?;

    let mut reader =
        ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|error| ProcessError::Decode {
                path: source.to_path_buf(),
                message: error.to_string(),
            })?;
    reader.limits(limits::decoder_limits());
    reader.decode().map_err(|error| ProcessError::Decode {
        path: source.to_path_buf(),
        message: error.to_string(),
    })
}

/// Cover-resizes with Lanczos3 and center-crops to the exact target.
///
/// RGB8 sources are borrowed; other colour types are converted to RGB8 first.
/// The result is always RGB8.
///
/// # Errors
/// Returns a resource error when the cover canvas would exceed the internal
/// bounds instead of allocating it, and [`ProcessError::Resize`] when the
/// resizer fails.
pub fn cover_crop(
    image: &DynamicImage,
    target: TargetPixels,
) -> Result<DynamicImage, ProcessError> {
    limits::check_target(target.width, target.height)?;
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return Err(ProcessError::Resource(ResourceError::SourceDimensions {
            path: PathBuf::new(),
            width,
            height,
            limit: "a positive size",
        }));
    }
    let scale = (f64::from(target.width) / f64::from(width))
        .max(f64::from(target.height) / f64::from(height));
    let scaled_width = scaled_edge(width, scale, target.width)?;
    let scaled_height = scaled_edge(height, scale, target.height)?;
    limits::check_intermediate(scaled_width, scaled_height)?;
    // The cover canvas of the old two-step path is never allocated: the
    // centred crop is mapped back to source coordinates and resized straight
    // to the target, with the same per-axis scale as an exact resize to the
    // scaled canvas.
    let (left, top, box_width, box_height) =
        cover_box(width, height, scaled_width, scaled_height, target);
    let converted;
    let rgb = if let DynamicImage::ImageRgb8(rgb) = image {
        rgb
    } else {
        converted = image.to_rgb8();
        &converted
    };
    let resize_failed = |message: String| ProcessError::Resize { message };
    // The typed entry point keeps only the RGB8 convolution reachable, so the
    // other pixel types' code is not linked.
    let source =
        fir::images::TypedImageRef::<fir::pixels::U8x3>::from_buffer(width, height, rgb.as_raw())
            .map_err(|error| resize_failed(error.to_string()))?;
    let mut buffer = vec![0; target.width as usize * target.height as usize * 3];
    let mut output = fir::images::TypedImage::<fir::pixels::U8x3>::from_buffer(
        target.width,
        target.height,
        &mut buffer,
    )
    .map_err(|error| resize_failed(error.to_string()))?;
    let options = fir::ResizeOptions::new()
        .resize_alg(fir::ResizeAlg::Convolution(fir::FilterType::Lanczos3))
        .crop(left, top, box_width, box_height);
    fir::Resizer::new()
        .resize_typed(&source, &mut output, &options)
        .map_err(|error| resize_failed(error.to_string()))?;
    RgbImage::from_raw(target.width, target.height, buffer)
        .map(DynamicImage::ImageRgb8)
        .ok_or_else(|| resize_failed("the output buffer does not match the target".to_owned()))
}

/// The centred crop of the `scaled_width`×`scaled_height` cover canvas, mapped
/// back to source coordinates as `(left, top, width, height)`.
fn cover_box(
    width: u32,
    height: u32,
    scaled_width: u32,
    scaled_height: u32,
    target: TargetPixels,
) -> (f64, f64, f64, f64) {
    let crop_x = (scaled_width - target.width) / 2;
    let crop_y = (scaled_height - target.height) / 2;
    let scale_x = f64::from(width) / f64::from(scaled_width);
    let scale_y = f64::from(height) / f64::from(scaled_height);
    let left = f64::from(crop_x) * scale_x;
    let top = f64::from(crop_y) * scale_y;
    // On the axis that fits exactly the far edge can round one ulp past the
    // source, which the resizer rejects, so both edges are clamped to it.
    let right = (left + f64::from(target.width) * scale_x).min(f64::from(width));
    let bottom = (top + f64::from(target.height) * scale_y).min(f64::from(height));
    (
        left,
        top,
        extent(left, right, width),
        extent(top, bottom, height),
    )
}

/// The positive size from `start` to `end` whose `start + size` the resizer
/// still sees inside `limit`.
// The loop ends: `start` is below `limit` and each step lowers `size`.
#[allow(clippy::while_float)]
fn extent(start: f64, end: f64, limit: u32) -> f64 {
    let mut size = (end - start).max(f64::MIN_POSITIVE);
    while start + size > f64::from(limit) {
        size = size.next_down();
    }
    size
}

fn scaled_edge(source: u32, scale: f64, target: u32) -> Result<u32, ProcessError> {
    let value = f64::from(source) * scale;
    if !value.is_finite() {
        return Err(ProcessError::Resource(ResourceError::IntermediateDimensions {
            width: u32::MAX,
            height: u32::MAX,
            limit: "a finite cover canvas",
        }));
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let value = value.trunc() as u32;
    // Floating-point truncation can land one pixel below the target; clamping
    // up keeps the crop inside the canvas and never changes a successful
    // cover size by more than that one pixel.
    Ok(value.max(target))
}

/// Applies blur then black rounded corners and returns the RGB8 output.
#[allow(clippy::cast_precision_loss)]
pub fn apply_effects(image: DynamicImage, effects: Effects) -> RgbImage {
    let mut result = image;
    if effects.blur > 0 {
        result = if effects.blur <= 5 {
            // True Gaussian blur, matching the legacy small-radius path.
            result.blur(effects.blur as f32)
        } else {
            // Native-resolution separable box-blur approximation.
            let rgb = result.to_rgb8();
            DynamicImage::ImageRgb8(imageops::fast_blur(&rgb, effects.blur as f32))
        };
    }
    if effects.radius > 0 {
        apply_rounded_corners(&result, effects.radius)
    } else {
        result.to_rgb8()
    }
}

/// Number of samples per axis for corner supersampling.
const AA_SAMPLES: u32 = 8;

/// Black rounded corners with 8×8 supersampling and the legacy radius cap.
fn apply_rounded_corners(image: &DynamicImage, radius: u32) -> RgbImage {
    let (width, height) = image.dimensions();
    let mut rgb = image.to_rgb8();
    let max_radius = width.min(height) / 2;
    let radius = radius.min(max_radius);
    if radius == 0 {
        return rgb;
    }
    let radius_f = f64::from(radius);
    let black = Rgb([0_u8, 0, 0]);
    let sample_step = 1.0 / f64::from(AA_SAMPLES);
    for y in 0..radius {
        for x in 0..radius {
            let mut coverage = 0.0_f64;
            for sy in 0..AA_SAMPLES {
                for sx in 0..AA_SAMPLES {
                    let sample_x = (f64::from(sx) + 0.5).mul_add(sample_step, f64::from(x));
                    let sample_y = (f64::from(sy) + 0.5).mul_add(sample_step, f64::from(y));
                    let dx = radius_f - sample_x;
                    let dy = radius_f - sample_y;
                    if dx.hypot(dy) <= radius_f {
                        coverage += 1.0;
                    }
                }
            }
            coverage /= f64::from(AA_SAMPLES * AA_SAMPLES);
            if coverage == 0.0 {
                set_pixel_black(&mut rgb, x, y, black);
                set_pixel_black(&mut rgb, width - 1 - x, y, black);
                set_pixel_black(&mut rgb, x, height - 1 - y, black);
                set_pixel_black(&mut rgb, width - 1 - x, height - 1 - y, black);
            } else if coverage < 1.0 {
                #[allow(clippy::cast_possible_truncation)]
                let blend = coverage as f32;
                blend_pixel_with_black(&mut rgb, x, y, blend);
                blend_pixel_with_black(&mut rgb, width - 1 - x, y, blend);
                blend_pixel_with_black(&mut rgb, x, height - 1 - y, blend);
                blend_pixel_with_black(&mut rgb, width - 1 - x, height - 1 - y, blend);
            }
        }
    }
    rgb
}

fn set_pixel_black(image: &mut RgbImage, x: u32, y: u32, black: Rgb<u8>) {
    if let Some(pixel) = image.get_pixel_mut_checked(x, y) {
        *pixel = black;
    }
}

fn blend_pixel_with_black(image: &mut RgbImage, x: u32, y: u32, factor: f32) {
    if let Some(pixel) = image.get_pixel_mut_checked(x, y) {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        for channel in 0..3 {
            pixel[channel] = (f32::from(pixel[channel]) * factor) as u8;
        }
    }
}

/// Encodes RGB8 lossless PNG with Fast compression and adaptive filtering.
///
/// # Errors
/// Returns a typed encode error; the writer is left as the codec left it.
pub fn encode_png(
    image: &RgbImage,
    writer: impl Write,
    path: &std::path::Path,
) -> Result<(), ProcessError> {
    let encoder =
        PngEncoder::new_with_quality(writer, CompressionType::Fast, PngFilterType::Adaptive);
    image.write_with_encoder(encoder).map_err(|error| ProcessError::Encode {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests;
