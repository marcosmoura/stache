//! Internal, non-configurable resource bounds for the image pipeline.
//!
//! Every limit is chosen so ordinary wallpapers are accepted and absurd inputs
//! are rejected before a large allocation. None of these is a public knob: a
//! caller cannot raise them, and a rejected input receives a typed error
//! instead of a fallback.
//!
//! | Limit | Value | Rationale |
//! | --- | --- | --- |
//! | [`MAX_SOURCE_BYTES`] | 128 MiB | Accepts very large camera exports and panoramas while bounding the one byte buffer a job reads. |
//! | [`MAX_DECODE_DIMENSION`] | 65,536 | The JPEG codec's own maximum edge; also accepts wide panoramas. |
//! | [`MAX_DECODE_PIXELS`] | 100 MP | Above any 8K desktop; a 20,000 × 4,000 panorama (80 MP) still fits. |
//! | [`MAX_TARGET_DIMENSION`] | 16,384 | Above 8K (7,680) with room for future panels on either axis. |
//! | [`MAX_TARGET_PIXELS`] | 100 MP | 8K is 33 MP; 16K × 6K is about 98 MP. |
//! | [`MAX_INTERMEDIATE_PIXELS`] | 100 MP | Bounds the cover-resize canvas before it is allocated. |
//! | [`MAX_INTERMEDIATE_DIMENSION`] | 65,536 | Bounds one edge of that canvas. |
//! | [`MAX_BLUR_RADIUS`] | 4,096 | A blur beyond this cannot be distinguished on any supported panel. Corner radii are capped to half the smaller target edge instead. |

use std::fmt;
use std::path::Path;

/// Maximum number of source bytes read into memory for one source.
pub const MAX_SOURCE_BYTES: u64 = 128 * 1024 * 1024;

/// Maximum decoded width or height of a source.
pub const MAX_DECODE_DIMENSION: u32 = 65_536;

/// Maximum decoded source pixels.
pub const MAX_DECODE_PIXELS: u64 = 100_000_000;

/// Maximum width or height of a target display in pixels.
pub const MAX_TARGET_DIMENSION: u32 = 16_384;

/// Maximum pixels of one target display.
pub const MAX_TARGET_PIXELS: u64 = 100_000_000;

/// Maximum pixels of the cover-resize canvas before the center crop.
pub const MAX_INTERMEDIATE_PIXELS: u64 = 100_000_000;

/// Maximum width or height of the cover-resize canvas.
pub const MAX_INTERMEDIATE_DIMENSION: u32 = 65_536;

/// Maximum accepted blur or rounded-corner radius in pixels.
pub const MAX_BLUR_RADIUS: u32 = 4_096;

/// Maximum number of scoped processing threads for one batch.
pub const MAX_CONCURRENT_WORKERS: usize = 4;

/// An input exceeded an internal resource bound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceError {
    /// The source file is larger than [`MAX_SOURCE_BYTES`].
    SourceBytes {
        /// The rejected source.
        path: std::path::PathBuf,
        /// The observed byte length.
        length: u64,
        /// The accepted maximum.
        limit: u64,
    },
    /// The decoded source is larger than the accepted edge or pixel count.
    SourceDimensions {
        /// The rejected source.
        path: std::path::PathBuf,
        /// The observed width.
        width: u32,
        /// The observed height.
        height: u32,
        /// A human-readable bound description.
        limit: &'static str,
    },
    /// The target display is larger than the accepted edge or pixel count.
    TargetDimensions {
        /// The observed width in pixels.
        width: u32,
        /// The observed height in pixels.
        height: u32,
        /// A human-readable bound description.
        limit: &'static str,
    },
    /// The cover-resize canvas would be larger than the accepted bound.
    IntermediateDimensions {
        /// The canvas width in pixels.
        width: u32,
        /// The canvas height in pixels.
        height: u32,
        /// A human-readable bound description.
        limit: &'static str,
    },
    /// A blur is larger than [`MAX_BLUR_RADIUS`].
    Effect {
        /// The rejected effect.
        effect: &'static str,
        /// The requested value.
        value: u32,
        /// The accepted maximum.
        limit: u32,
    },
}

impl fmt::Display for ResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceBytes { path, length, limit } => write!(
                f,
                "source {} is {length} bytes, above the {limit}-byte limit",
                path.display()
            ),
            Self::SourceDimensions { path, width, height, limit } => write!(
                f,
                "source {} decodes to {width}x{height}, above {limit}",
                path.display()
            ),
            Self::TargetDimensions { width, height, limit } => {
                write!(f, "target display {width}x{height} is above {limit}")
            }
            Self::IntermediateDimensions { width, height, limit } => {
                write!(f, "cover canvas {width}x{height} is above {limit}")
            }
            Self::Effect { effect, value, limit } => {
                write!(f, "wallpaper {effect} {value} is above the limit of {limit}")
            }
        }
    }
}

impl std::error::Error for ResourceError {}

/// Rejects a source length above [`MAX_SOURCE_BYTES`].
pub fn check_source_len(path: &Path, length: u64) -> Result<(), ResourceError> {
    if length > MAX_SOURCE_BYTES {
        return Err(ResourceError::SourceBytes {
            path: path.to_path_buf(),
            length,
            limit: MAX_SOURCE_BYTES,
        });
    }
    Ok(())
}

/// Rejects a decoded source above the edge or pixel bound.
pub fn check_source_dimensions(path: &Path, width: u32, height: u32) -> Result<(), ResourceError> {
    if width == 0 || height == 0 {
        return Err(ResourceError::SourceDimensions {
            path: path.to_path_buf(),
            width,
            height,
            limit: "a positive size",
        });
    }
    if width > MAX_DECODE_DIMENSION || height > MAX_DECODE_DIMENSION {
        return Err(ResourceError::SourceDimensions {
            path: path.to_path_buf(),
            width,
            height,
            limit: "the decoded edge limit",
        });
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_DECODE_PIXELS {
        return Err(ResourceError::SourceDimensions {
            path: path.to_path_buf(),
            width,
            height,
            limit: "the decoded pixel limit",
        });
    }
    Ok(())
}

/// Rejects a target display above the edge or pixel bound.
pub fn check_target(width: u32, height: u32) -> Result<(), ResourceError> {
    if width == 0 || height == 0 {
        return Err(ResourceError::TargetDimensions {
            width,
            height,
            limit: "a positive size",
        });
    }
    if width > MAX_TARGET_DIMENSION || height > MAX_TARGET_DIMENSION {
        return Err(ResourceError::TargetDimensions {
            width,
            height,
            limit: "the target edge limit",
        });
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_TARGET_PIXELS {
        return Err(ResourceError::TargetDimensions {
            width,
            height,
            limit: "the target pixel limit",
        });
    }
    Ok(())
}

/// Rejects a cover-resize canvas above the edge or pixel bound.
pub fn check_intermediate(width: u32, height: u32) -> Result<(), ResourceError> {
    if width == 0 || height == 0 {
        return Err(ResourceError::IntermediateDimensions {
            width,
            height,
            limit: "a positive size",
        });
    }
    if width > MAX_INTERMEDIATE_DIMENSION || height > MAX_INTERMEDIATE_DIMENSION {
        return Err(ResourceError::IntermediateDimensions {
            width,
            height,
            limit: "the cover-canvas edge limit",
        });
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_INTERMEDIATE_PIXELS {
        return Err(ResourceError::IntermediateDimensions {
            width,
            height,
            limit: "the cover-canvas pixel limit",
        });
    }
    Ok(())
}

/// Rejects a blur above [`MAX_BLUR_RADIUS`].
///
/// A corner radius needs no bound here: it is capped to half the smaller target
/// edge when it is applied, and the target edge is already bounded.
pub const fn check_blur(blur: u32) -> Result<(), ResourceError> {
    if blur > MAX_BLUR_RADIUS {
        return Err(ResourceError::Effect {
            effect: "blur",
            value: blur,
            limit: MAX_BLUR_RADIUS,
        });
    }
    Ok(())
}

/// The `image` decoder limits used for source decoding.
pub fn decoder_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_DIMENSION);
    limits.max_image_height = Some(MAX_DECODE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_PIXELS * 4);
    limits
}

/// The `image` decoder limits used when validating a cache hit.
pub fn output_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_TARGET_DIMENSION);
    limits.max_image_height = Some(MAX_TARGET_DIMENSION);
    limits.max_alloc = Some(MAX_TARGET_PIXELS * 4);
    limits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path() -> &'static Path { Path::new("/tmp/source.png") }

    #[test]
    fn source_byte_limit_accepts_the_boundary_and_rejects_the_next_byte() {
        check_source_len(path(), MAX_SOURCE_BYTES).expect("at the limit");
        let error = check_source_len(path(), MAX_SOURCE_BYTES + 1).expect_err("above");
        assert!(matches!(error, ResourceError::SourceBytes { .. }), "{error:?}");
    }

    #[test]
    fn decoded_dimensions_accept_the_boundary_on_both_axes() {
        check_source_dimensions(path(), MAX_DECODE_DIMENSION, 1).expect("edge");
        check_source_dimensions(path(), 1, MAX_DECODE_DIMENSION).expect("edge");
        assert!(matches!(
            check_source_dimensions(path(), MAX_DECODE_DIMENSION + 1, 1),
            Err(ResourceError::SourceDimensions { .. })
        ));
        assert!(matches!(
            check_source_dimensions(path(), 1, MAX_DECODE_DIMENSION + 1),
            Err(ResourceError::SourceDimensions { .. })
        ));
    }

    #[test]
    fn decoded_pixels_accept_the_boundary_and_reject_the_next_pixel() {
        // 10,000 × 10,000 = 100,000,000 exactly.
        check_source_dimensions(path(), 10_000, 10_000).expect("at the pixel limit");
        assert!(matches!(
            check_source_dimensions(path(), 10_000, 10_001),
            Err(ResourceError::SourceDimensions { .. })
        ));
        // 8K and a wide panorama remain accepted.
        check_source_dimensions(path(), 7_680, 4_320).expect("8K");
        check_source_dimensions(path(), 20_000, 4_000).expect("panorama");
    }

    #[test]
    fn target_dimensions_accept_the_boundary_and_reject_the_next_value() {
        check_target(MAX_TARGET_DIMENSION, 1).expect("edge");
        check_target(1, MAX_TARGET_DIMENSION).expect("edge");
        check_target(7_680, 4_320).expect("8K");
        assert!(matches!(
            check_target(MAX_TARGET_DIMENSION + 1, 1),
            Err(ResourceError::TargetDimensions { .. })
        ));
        assert!(matches!(
            check_target(1, MAX_TARGET_DIMENSION + 1),
            Err(ResourceError::TargetDimensions { .. })
        ));
        // 20,000 × 20,000 is above both the edge and the pixel bound.
        assert!(matches!(
            check_target(20_000, 20_000),
            Err(ResourceError::TargetDimensions { .. })
        ));
    }

    #[test]
    fn target_pixels_accept_the_boundary_and_reject_the_next_pixel() {
        // 8,000 × 12,500 = 100,000,000 exactly.
        check_target(8_000, 12_500).expect("at the pixel limit");
        assert!(matches!(
            check_target(8_000, 12_501),
            Err(ResourceError::TargetDimensions { .. })
        ));
    }

    #[test]
    fn cover_canvas_bounds_accept_the_boundary_and_reject_the_next_value() {
        check_intermediate(MAX_INTERMEDIATE_DIMENSION, 1).expect("edge");
        check_intermediate(10_000, 10_000).expect("at the pixel limit");
        assert!(matches!(
            check_intermediate(MAX_INTERMEDIATE_DIMENSION + 1, 1),
            Err(ResourceError::IntermediateDimensions { .. })
        ));
        assert!(matches!(
            check_intermediate(10_000, 10_001),
            Err(ResourceError::IntermediateDimensions { .. })
        ));
    }

    #[test]
    fn the_blur_bound_accepts_the_boundary_and_reject_the_next_value() {
        check_blur(MAX_BLUR_RADIUS).expect("edge");
        check_blur(0).expect("no blur");
        assert!(matches!(
            check_blur(MAX_BLUR_RADIUS + 1),
            Err(ResourceError::Effect { effect: "blur", .. })
        ));
    }

    #[test]
    fn zero_sized_inputs_are_rejected() {
        assert!(check_source_dimensions(path(), 0, 1).is_err());
        assert!(check_target(1, 0).is_err());
        assert!(check_intermediate(0, 0).is_err());
    }
}
