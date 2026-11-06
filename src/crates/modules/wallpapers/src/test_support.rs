//! Test fixtures and disposable identity paths.
//!
//! Every helper writes under a `tempfile::TempDir`; no test reads the real home,
//! cache or desktop. Images are encoded with the same `image` crate the
//! pipeline uses, so the bytes are real JPEG/PNG/WebP rather than renamed
//! stand-ins.

use std::fs::File;
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::codecs::webp::WebPEncoder;
use image::{DynamicImage, ExtendedColorType, ImageReader, Rgb, RgbImage};
use stache_macos::{DisplayEntry, DisplayFrame, DisplayId, DisplaySnapshot};
use stache_sdk::{AppEnvironment, AppPaths};

/// A disposable home/temporary root with the identity paths resolved from it.
pub struct TestEnv {
    root: tempfile::TempDir,
    paths: AppPaths,
}

impl TestEnv {
    /// Creates a new isolated environment.
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("temporary root");
        let paths = AppPaths::resolve(&AppEnvironment {
            home: Some(root.path().to_path_buf()),
            temporary: Some(root.path().to_path_buf()),
        })
        .expect("identity paths");
        Self { root, paths }
    }

    /// The resolved identity paths.
    pub fn paths(&self) -> &AppPaths { &self.paths }

    /// The `wallpapers` cache directory (not necessarily created).
    pub fn wallpaper_cache(&self) -> PathBuf { self.paths.cache_directory().join("wallpapers") }

    /// The root of this disposable filesystem.
    pub fn root(&self) -> &Path { self.root.path() }

    /// A unique path under the disposable root.
    pub fn path(&self, name: &str) -> PathBuf { self.root.path().join(name) }

    /// Writes a real PNG and returns its path.
    pub fn png(&self, name: &str, image: impl std::borrow::Borrow<RgbImage>) -> PathBuf {
        let path = self.path(name);
        write_png(&path, image.borrow());
        path
    }

    /// Writes a real JPEG and returns its path.
    pub fn jpeg(&self, name: &str, image: impl std::borrow::Borrow<RgbImage>) -> PathBuf {
        let path = self.path(name);
        write_jpeg(&path, image.borrow());
        path
    }

    /// Writes a real WebP and returns its path.
    pub fn webp(&self, name: &str, image: impl std::borrow::Borrow<RgbImage>) -> PathBuf {
        let path = self.path(name);
        write_webp(&path, image.borrow());
        path
    }
}

/// A solid RGB image.
pub fn solid(width: u32, height: u32, color: [u8; 3]) -> RgbImage {
    RgbImage::from_fn(width, height, |_, _| Rgb(color))
}

/// An image whose pixels encode their coordinates, for crop assertions.
pub fn coordinates(width: u32, height: u32) -> RgbImage {
    RgbImage::from_fn(width, height, |x, y| {
        #[allow(clippy::cast_possible_truncation)]
        Rgb([(x % 256) as u8, (y % 256) as u8, 128])
    })
}

/// Writes a PNG with the production encoder settings.
pub fn write_png(path: &Path, image: &RgbImage) {
    let file = File::create(path).expect("png file");
    let encoder =
        PngEncoder::new_with_quality(file, CompressionType::Default, FilterType::Adaptive);
    image.write_with_encoder(encoder).expect("encode png");
}

/// Writes a real JPEG.
pub fn write_jpeg(path: &Path, image: &RgbImage) {
    let file = File::create(path).expect("jpeg file");
    let mut encoder = JpegEncoder::new_with_quality(file, 92);
    encoder
        .encode_image(&DynamicImage::ImageRgb8(image.clone()))
        .expect("encode jpeg");
}

/// Writes a lossless WebP.
pub fn write_webp(path: &Path, image: &RgbImage) {
    let file = File::create(path).expect("webp file");
    WebPEncoder::new_lossless(file)
        .encode(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgb8,
        )
        .expect("encode webp");
}

/// Decodes any supported file and returns RGB8 pixels.
pub fn read_rgb8(path: &Path) -> RgbImage {
    ImageReader::open(path)
        .expect("open image")
        .with_guessed_format()
        .expect("guess format")
        .decode()
        .expect("decode image")
        .to_rgb8()
}

/// The decoded color type of a file.
pub fn color_type(path: &Path) -> image::ColorType {
    ImageReader::open(path)
        .expect("open image")
        .with_guessed_format()
        .expect("guess format")
        .decode()
        .expect("decode image")
        .color()
}

/// A display entry with a point frame and backing scale.
pub fn entry(id: u32, width: f64, height: f64, scale: f64) -> DisplayEntry {
    DisplayEntry {
        id: DisplayId::new(id),
        frame: DisplayFrame { width, height },
        scale,
    }
}

/// A snapshot in native order.
pub fn snapshot(entries: Vec<DisplayEntry>) -> DisplaySnapshot { DisplaySnapshot::new(entries) }

/// A PNG whose IHDR claims dimensions no decoder may allocate for.
///
/// The file contains a valid signature, IHDR and empty IDAT/IEND chunks; only
/// the header is parsed by `into_dimensions`, which is exactly the pre-decode
/// path the resource check guards.
pub fn oversized_png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    push_chunk(&mut bytes, *b"IHDR", &ihdr);
    push_chunk(&mut bytes, *b"IDAT", &[]);
    push_chunk(&mut bytes, *b"IEND", &[]);
    bytes
}

fn push_chunk(output: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
    output.extend_from_slice(&u32::try_from(data.len()).expect("chunk fits").to_be_bytes());
    output.extend_from_slice(&kind);
    output.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(&kind);
    crc_input.extend_from_slice(data);
    output.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// A publication seam that fails at one named point.
pub struct FailingSeam {
    /// `write`, `sync` or `publish`.
    pub point: &'static str,
}

impl crate::cache::PublishSeam for FailingSeam {
    fn after_write(&self) -> std::io::Result<()> {
        if self.point == "write" {
            Err(std::io::Error::other("injected write failure"))
        } else {
            Ok(())
        }
    }

    fn before_sync(&self) -> std::io::Result<()> {
        if self.point == "sync" {
            Err(std::io::Error::other("injected flush failure"))
        } else {
            Ok(())
        }
    }

    fn before_publish(&self) -> std::io::Result<()> {
        if self.point == "publish" {
            Err(std::io::Error::other("injected publish failure"))
        } else {
            Ok(())
        }
    }
}

/// A clock that advances by the requested slice instead of sleeping.
pub struct FakeClock {
    now: std::sync::Mutex<std::time::Instant>,
}

impl FakeClock {
    /// A clock starting now.
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            now: std::sync::Mutex::new(std::time::Instant::now()),
        })
    }
}

impl stache_sdk::LeaseClock for FakeClock {
    fn now(&self) -> std::time::Instant {
        *self.now.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn sleep(
        &self,
        timeout: std::time::Duration,
        cancellation: &stache_sdk::CancellationToken,
    ) -> bool {
        if cancellation.is_cancelled() {
            return true;
        }
        *self.now.lock().unwrap_or_else(std::sync::PoisonError::into_inner) += timeout;
        false
    }
}
