use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use stache_macos::{
    DesktopBackend, DesktopClock, DesktopError, DisplayEntry, DisplayId, DisplaySnapshot,
    FakeDesktop, GuardedDesktopBackend, SystemDesktopClock,
};
use stache_sdk::{
    AppEnvironment, AppPaths, CacheLease, CacheLeaseError, CancellationToken, LeaseClock, LeaseWait,
};

use super::*;
use crate::batch::{BatchDeps, BatchItem, BatchObserver, prepare_batch_with};
use crate::cache::{CacheError, NoFailures, PublishSeam};
use crate::selection::target_pixels;
use crate::test_support::{
    FailingSeam, FakeClock, TestEnv, coordinates, entry, read_rgb8, snapshot, solid, write_jpeg,
    write_png,
};

fn deps<'a>(env: &'a TestEnv, desktop: &'a FakeDesktop) -> PrepareDeps<'a> {
    PrepareDeps { desktop, paths: env.paths() }
}

fn single_display() -> FakeDesktop {
    FakeDesktop::with_snapshot(snapshot(vec![entry(1, 1280.0, 720.0, 1.0)]))
}

fn token() -> CancellationToken { CancellationToken::new() }

fn cache_files(env: &TestEnv) -> Vec<PathBuf> {
    match fs::read_dir(env.wallpaper_cache()) {
        Ok(entries) => entries.map(|entry| entry.expect("entry").path()).collect(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("cannot list cache: {error}"),
    }
}

#[test]
fn prepare_decodes_every_advertised_format_including_jpeg_content_under_jpg() {
    let env = TestEnv::new();
    let desktop = single_display();
    let image = solid(64, 36, [90, 40, 10]);
    let png = env.png("one.png", &image);
    let jpeg = env.jpeg("two.jpeg", &image);
    let webp = env.webp("three.webp", &image);
    let jpeg_named_jpg = env.path("four.jpg");
    crate::test_support::write_jpeg(&jpeg_named_jpg, &image);
    for source in [png, jpeg, webp, jpeg_named_jpg] {
        let prepared = prepare(
            ScreenTarget::Main,
            &source,
            Effects::default(),
            &deps(&env, &desktop),
            &token(),
        )
        .expect("prepare");
        assert_eq!(read_rgb8(&prepared.targets()[0].image).dimensions(), (1280, 720));
    }
}

#[test]
fn the_cache_lives_under_the_injected_app_paths() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(8, 8, [9, 8, 7]));
    let desktop = single_display();
    let prepared = prepare(
        ScreenTarget::Main,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("prepare");
    let image = &prepared.targets()[0].image;
    assert!(image.starts_with(env.paths().cache_directory().join("wallpapers")));
    assert_eq!(image.extension().and_then(|value| value.to_str()), Some("png"));
    for directory in [
        env.paths().cache_directory(),
        env.wallpaper_cache().as_path(),
    ] {
        assert_eq!(
            fs::metadata(directory).expect("directory").permissions().mode() & 0o777,
            0o700
        );
    }
}

#[test]
fn an_invalid_selector_fails_before_the_cache_exists() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(8, 8, [1, 2, 3]));
    let desktop = single_display();
    let error = prepare(
        ScreenTarget::Index(9),
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect_err("out of range");
    assert!(matches!(
        error,
        PrepareError::Selection(SelectionError::OutOfRange { .. })
    ));
    assert!(!env.paths().cache_directory().exists());
    assert!(desktop.calls().is_empty());
}

#[test]
fn no_displays_fails_before_the_cache_exists() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(8, 8, [1, 2, 3]));
    let desktop = FakeDesktop::new();
    let error = prepare(
        ScreenTarget::All,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect_err("no displays");
    assert!(matches!(
        error,
        PrepareError::Selection(SelectionError::NoDisplays)
    ));
    assert!(!env.paths().cache_directory().exists());
}

#[test]
fn mixed_scales_prepare_distinct_exact_pixel_outputs() {
    let env = TestEnv::new();
    let source = env.png("source.png", coordinates_like(640, 360));
    let desktop = FakeDesktop::with_snapshot(snapshot(vec![
        entry(1, 1280.0, 720.0, 1.0),
        entry(2, 1280.0, 720.0, 2.0),
    ]));
    let prepared = prepare(
        ScreenTarget::All,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("prepare");
    assert_eq!(read_rgb8(&prepared.targets()[0].image).dimensions(), (1280, 720));
    assert_eq!(
        read_rgb8(&prepared.targets()[1].image).dimensions(),
        (2560, 1440)
    );
    assert_ne!(prepared.targets()[0].image, prepared.targets()[1].image);
}

fn coordinates_like(width: u32, height: u32) -> image::RgbImage {
    image::RgbImage::from_fn(width, height, |x, y| {
        #[allow(clippy::cast_possible_truncation)]
        image::Rgb([(x % 251) as u8, (y % 241) as u8, 100])
    })
}

#[test]
fn every_output_exists_before_the_first_setter_call() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(64, 36, [40, 50, 60]));
    let desktop = FakeDesktop::with_snapshot(snapshot(vec![
        entry(1, 1280.0, 720.0, 1.0),
        entry(2, 640.0, 360.0, 2.0),
    ]));
    let prepared = prepare(
        ScreenTarget::All,
        &source,
        Effects { radius: 8, blur: 3 },
        &deps(&env, &desktop),
        &token(),
    )
    .expect("prepare");
    assert!(desktop.calls().is_empty(), "prepare must not call the setter");
    for target in prepared.targets() {
        assert!(target.image.is_file());
        assert_eq!(
            read_rgb8(&target.image).dimensions(),
            (
                target_pixels(&target.entry).expect("pixels").width,
                target_pixels(&target.entry).expect("pixels").height
            )
        );
    }
    let outcome = prepared.apply(&desktop, &token(), &|| true).expect("apply");
    assert!(outcome.is_complete());
    assert_eq!(desktop.calls().len(), 2);
    assert_eq!(desktop.calls()[0].display, prepared.targets()[0].entry.id);
    assert_eq!(desktop.calls()[0].image, prepared.targets()[0].image);
}

#[test]
fn a_processing_failure_makes_zero_setter_calls() {
    let env = TestEnv::new();
    let source = env.path("broken.png");
    fs::write(&source, b"not an image at all").expect("source");
    let desktop = single_display();
    let error = prepare(
        ScreenTarget::Main,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect_err("decode failure");
    assert!(matches!(
        error,
        PrepareError::Process(ProcessError::Decode { .. })
    ));
    assert!(desktop.calls().is_empty());
    assert!(cache_files(&env).iter().all(|path| !path.is_file()));
}

#[test]
fn a_partial_failure_reports_the_true_identities_and_stops() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(48, 27, [1, 1, 1]));
    let desktop = FakeDesktop::with_snapshot(snapshot(vec![
        entry(1, 100.0, 100.0, 1.0),
        entry(2, 100.0, 100.0, 1.0),
        entry(3, 100.0, 100.0, 1.0),
    ]));
    let prepared = prepare(
        ScreenTarget::All,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("prepare");
    let failure = DesktopError::Native {
        display: stache_macos::DisplayId::new(2),
        message: "denied".to_owned(),
    };
    desktop.set_outcome(stache_macos::DisplayId::new(2), Err(failure.clone()));
    let outcome = prepared.apply(&desktop, &token(), &|| true).expect("apply");
    assert_eq!(outcome.applied, vec![stache_macos::DisplayId::new(1)]);
    assert_eq!(outcome.failed, vec![(stache_macos::DisplayId::new(2), failure)]);
    assert_eq!(outcome.not_attempted, vec![stache_macos::DisplayId::new(3)]);
    assert!(outcome.unknown.is_empty());
    assert!(!outcome.is_complete());
    assert_eq!(desktop.calls().len(), 2);
}

#[test]
fn an_unknown_outcome_is_reported_and_stops_further_calls() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(48, 27, [2, 2, 2]));
    let desktop = FakeDesktop::with_snapshot(snapshot(vec![
        entry(1, 100.0, 100.0, 1.0),
        entry(2, 100.0, 100.0, 1.0),
    ]));
    let prepared = prepare(
        ScreenTarget::All,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("prepare");
    let unknown = DesktopError::Unknown { operation: "desktop image set" };
    desktop.set_outcome(stache_macos::DisplayId::new(1), Err(unknown.clone()));
    let outcome = prepared.apply(&desktop, &token(), &|| true).expect("apply");
    assert!(outcome.applied.is_empty());
    assert!(outcome.failed.is_empty());
    assert_eq!(outcome.unknown, vec![(stache_macos::DisplayId::new(1), unknown)]);
    assert_eq!(outcome.not_attempted, vec![stache_macos::DisplayId::new(2)]);
    assert!(!outcome.is_complete());
    assert_eq!(desktop.calls().len(), 1);
}

#[test]
fn hotplug_identity_errors_are_reported_without_retargeting() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(48, 27, [3, 3, 3]));
    let desktop = FakeDesktop::with_snapshot(snapshot(vec![
        entry(1, 100.0, 100.0, 1.0),
        entry(2, 100.0, 100.0, 1.0),
    ]));
    let prepared = prepare(
        ScreenTarget::All,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("prepare");
    desktop.set_outcome(
        stache_macos::DisplayId::new(1),
        Err(DesktopError::DisplayMissing(stache_macos::DisplayId::new(1))),
    );
    desktop.set_outcome(
        stache_macos::DisplayId::new(2),
        Err(DesktopError::DisplayChanged(stache_macos::DisplayId::new(2))),
    );
    let first = prepared.apply(&desktop, &token(), &|| true).expect("apply");
    assert!(matches!(first.failed[0].1, DesktopError::DisplayMissing(_)));
    assert_eq!(first.not_attempted, vec![stache_macos::DisplayId::new(2)]);

    desktop.set_outcome(stache_macos::DisplayId::new(1), Ok(()));
    desktop.set_outcome(
        stache_macos::DisplayId::new(2),
        Err(DesktopError::DisplayChanged(stache_macos::DisplayId::new(2))),
    );
    let second = prepared.apply(&desktop, &token(), &|| true).expect("apply");
    assert_eq!(second.applied, vec![stache_macos::DisplayId::new(1)]);
    assert!(matches!(second.failed[0].1, DesktopError::DisplayChanged(_)));
}

#[test]
fn cancellation_before_apply_makes_no_calls() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(20, 20, [4, 4, 4]));
    let desktop = FakeDesktop::with_snapshot(snapshot(vec![
        entry(1, 100.0, 100.0, 1.0),
        entry(2, 100.0, 100.0, 1.0),
    ]));
    let prepared = prepare(
        ScreenTarget::All,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("prepare");
    let cancel = token();
    cancel.cancel();
    let outcome = prepared.apply(&desktop, &cancel, &|| true).expect("apply");
    assert!(outcome.applied.is_empty());
    assert_eq!(outcome.not_attempted.len(), 2);
    assert!(desktop.calls().is_empty());
}

#[test]
fn a_stale_closure_can_only_suppress_future_calls() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(20, 20, [5, 5, 5]));
    let desktop = FakeDesktop::with_snapshot(snapshot(vec![
        entry(1, 100.0, 100.0, 1.0),
        entry(2, 100.0, 100.0, 1.0),
        entry(3, 100.0, 100.0, 1.0),
    ]));
    let prepared = prepare(
        ScreenTarget::All,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("prepare");
    let calls = AtomicUsize::new(0);
    let current = || calls.fetch_add(1, Ordering::SeqCst) < 1;
    let outcome = prepared.apply(&desktop, &token(), &current).expect("apply");
    assert_eq!(outcome.applied, vec![stache_macos::DisplayId::new(1)]);
    assert_eq!(outcome.not_attempted.len(), 2);
    assert_eq!(desktop.calls().len(), 1);
}

struct CancelAfterFirst {
    inner: FakeDesktop,
    token: CancellationToken,
    fired: AtomicBool,
}

impl DesktopBackend for CancelAfterFirst {
    fn snapshot(&self) -> Result<DisplaySnapshot, DesktopError> { self.inner.snapshot() }

    fn set_image(
        &self,
        display: stache_macos::DisplayId,
        expected: &stache_macos::DisplayEntry,
        image: &Path,
    ) -> Result<(), DesktopError> {
        let result = self.inner.set_image(display, expected, image);
        if !self.fired.swap(true, Ordering::SeqCst) {
            self.token.cancel();
        }
        result
    }
}

#[test]
fn cancellation_during_apply_stops_after_the_issued_call() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(20, 20, [6, 6, 6]));
    let inner = FakeDesktop::with_snapshot(snapshot(vec![
        entry(1, 100.0, 100.0, 1.0),
        entry(2, 100.0, 100.0, 1.0),
    ]));
    let prepared = prepare(
        ScreenTarget::All,
        &source,
        Effects::default(),
        &deps(&env, &inner),
        &token(),
    )
    .expect("prepare");
    let cancel = token();
    let backend = CancelAfterFirst {
        inner: inner.clone(),
        token: cancel.clone(),
        fired: AtomicBool::new(false),
    };
    let outcome = prepared.apply(&backend, &cancel, &|| true).expect("apply");
    assert_eq!(outcome.applied, vec![stache_macos::DisplayId::new(1)]);
    assert_eq!(outcome.not_attempted, vec![stache_macos::DisplayId::new(2)]);
    assert_eq!(inner.calls().len(), 1);
}

#[test]
fn the_same_basename_in_different_directories_has_different_keys() {
    let env = TestEnv::new();
    fs::create_dir_all(env.path("one")).expect("one");
    fs::create_dir_all(env.path("two")).expect("two");
    let image = solid(32, 18, [70, 80, 90]);
    let first = env.png("one/wallpaper.png", &image);
    let second = env.png("two/wallpaper.png", &image);
    let desktop = single_display();
    let a = prepare(
        ScreenTarget::Main,
        &first,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("first");
    let b = prepare(
        ScreenTarget::Main,
        &second,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("second");
    assert_ne!(a.targets()[0].image, b.targets()[0].image);
}

#[test]
fn changed_bytes_with_unchanged_length_and_mtime_invalidate() {
    let env = TestEnv::new();
    let path = env.path("source.png");
    write_png_bytes(&path, &solid(16, 16, [255, 0, 0]));
    let desktop = single_display();
    let first = prepare(
        ScreenTarget::Main,
        &path,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("first");
    let first_path = first.targets()[0].image.clone();
    let original = fs::metadata(&path).expect("metadata");
    let original_time = original.modified().expect("mtime");

    let replacement = solid(16, 16, [0, 0, 255]);
    write_png_bytes(&path, &replacement);
    let replaced = fs::metadata(&path).expect("metadata");
    assert_eq!(replaced.len(), original.len(), "the test keeps the length equal");
    fs::File::options()
        .write(true)
        .open(&path)
        .expect("open")
        .set_modified(original_time)
        .expect("restore mtime");
    assert_eq!(
        fs::metadata(&path).expect("metadata").modified().expect("mtime"),
        original_time
    );

    let second = prepare(
        ScreenTarget::Main,
        &path,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("second");
    assert_ne!(second.targets()[0].image, first_path);
    assert_eq!(
        read_rgb8(&second.targets()[0].image).get_pixel(0, 0),
        &image::Rgb([0, 0, 255])
    );
}

fn write_png_bytes(path: &Path, image: &image::RgbImage) {
    let file = fs::File::create(path).expect("png file");
    let encoder = image::codecs::png::PngEncoder::new_with_quality(
        file,
        image::codecs::png::CompressionType::Uncompressed,
        image::codecs::png::FilterType::NoFilter,
    );
    image.write_with_encoder(encoder).expect("encode");
}

#[test]
fn an_oversized_corner_radius_is_capped_rather_than_rejected() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(32, 18, [10, 20, 30]));
    let desktop = single_display();
    let prepared = prepare(
        ScreenTarget::Main,
        &source,
        Effects { radius: u32::MAX, blur: 0 },
        &deps(&env, &desktop),
        &token(),
    )
    .expect("the radius is capped to half the smaller edge");
    assert_eq!(prepared.targets().len(), 1);
}

#[test]
fn geometry_and_effect_changes_produce_different_keys() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(32, 18, [10, 20, 30]));
    let desktop = single_display();
    let plain = prepare(
        ScreenTarget::Main,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("plain");
    let blurred = prepare(
        ScreenTarget::Main,
        &source,
        Effects { radius: 0, blur: 4 },
        &deps(&env, &desktop),
        &token(),
    )
    .expect("blurred");
    let rounded = prepare(
        ScreenTarget::Main,
        &source,
        Effects { radius: 6, blur: 0 },
        &deps(&env, &desktop),
        &token(),
    )
    .expect("rounded");
    assert_ne!(plain.targets()[0].image, blurred.targets()[0].image);
    assert_ne!(plain.targets()[0].image, rounded.targets()[0].image);

    desktop.set_snapshot(snapshot(vec![entry(1, 1280.0, 720.0, 2.0)]));
    let larger = prepare(
        ScreenTarget::Main,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("larger");
    assert_ne!(plain.targets()[0].image, larger.targets()[0].image);
}

#[test]
fn a_pre_cancelled_prepare_touches_nothing() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(8, 8, [1, 1, 1]));
    let desktop = single_display();
    let cancel = token();
    cancel.cancel();
    let error = prepare(
        ScreenTarget::All,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &cancel,
    )
    .expect_err("cancelled");
    assert!(matches!(error, PrepareError::Cancelled));
    assert!(!env.paths().cache_directory().exists());
    assert!(desktop.calls().is_empty());
}

#[test]
fn corrupt_and_truncated_cache_outputs_are_rebuilt() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(24, 12, [30, 40, 50]));
    let desktop = single_display();
    let prepared = prepare(
        ScreenTarget::Main,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("prepare");
    let image = prepared.targets()[0].image.clone();

    fs::write(&image, b"garbage").expect("corrupt");
    let rebuilt = prepare(
        ScreenTarget::Main,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("rebuild corrupt");
    assert_eq!(rebuilt.targets()[0].image, image);
    assert_eq!(read_rgb8(&image).get_pixel(0, 0), &image::Rgb([30, 40, 50]));

    let bytes = fs::read(&image).expect("bytes");
    fs::write(&image, &bytes[..10]).expect("truncate");
    let rebuilt = prepare(
        ScreenTarget::Main,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("rebuild truncated");
    assert_eq!(rebuilt.targets()[0].image, image);
    assert_eq!(read_rgb8(&image).dimensions(), (1280, 720));

    // A valid PNG with the wrong dimensions is not a hit.
    write_png(&image, &solid(4, 4, [1, 2, 3]));
    let _ = prepare(
        ScreenTarget::Main,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("rebuild wrong dimensions");
    assert_eq!(read_rgb8(&image).dimensions(), (1280, 720));

    // A valid RGBA PNG of the right size is not an RGB8 output either.
    let rgba = image::RgbaImage::from_pixel(1280, 720, image::Rgba([1, 2, 3, 200]));
    image::DynamicImage::ImageRgba8(rgba).save(&image).expect("rgba png");
    let _ = prepare(
        ScreenTarget::Main,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("rebuild rgba");
    assert_eq!(crate::test_support::color_type(&image), image::ColorType::Rgb8);
    assert_eq!(read_rgb8(&image).dimensions(), (1280, 720));
}

#[test]
fn a_symlinked_cache_entry_is_refused() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(24, 12, [30, 40, 50]));
    let desktop = single_display();
    let prepared = prepare(
        ScreenTarget::Main,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("prepare");
    let image = prepared.targets()[0].image.clone();
    let foreign = env.png("foreign.png", solid(4, 4, [7, 7, 7]));
    fs::remove_file(&image).expect("remove");
    std::os::unix::fs::symlink(&foreign, &image).expect("symlink");
    let error = prepare(
        ScreenTarget::Main,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect_err("symlink");
    assert!(matches!(
        error,
        PrepareError::Process(ProcessError::Cache(CacheError::UnsafeEntry { .. }))
    ));
    assert_eq!(read_rgb8(&foreign).dimensions(), (4, 4));
}

#[test]
fn a_symlinked_cache_root_is_refused() {
    let env = TestEnv::new();
    let parent = env.paths().cache_directory().parent().expect("parent").to_path_buf();
    fs::create_dir_all(&parent).expect("cache parent");
    let elsewhere = env.path("elsewhere");
    fs::create_dir(&elsewhere).expect("elsewhere");
    std::os::unix::fs::symlink(&elsewhere, env.paths().cache_directory()).expect("symlink");
    let source = env.png("source.png", solid(8, 8, [1, 1, 1]));
    let desktop = single_display();
    let error = prepare(
        ScreenTarget::Main,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect_err("symlinked cache root");
    assert!(matches!(
        error,
        PrepareError::Process(ProcessError::Cache(CacheError::UnsafeDirectory { .. }))
    ));
}

#[test]
fn a_symlinked_wallpapers_directory_is_refused() {
    let env = TestEnv::new();
    fs::create_dir_all(env.paths().cache_directory()).expect("cache root");
    let elsewhere = env.path("elsewhere");
    fs::create_dir(&elsewhere).expect("elsewhere");
    std::os::unix::fs::symlink(&elsewhere, env.wallpaper_cache()).expect("symlink");
    let source = env.png("source.png", solid(8, 8, [1, 1, 1]));
    let desktop = single_display();
    let error = prepare(
        ScreenTarget::Main,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect_err("symlinked wallpapers");
    assert!(matches!(
        error,
        PrepareError::Process(ProcessError::Cache(CacheError::UnsafeDirectory { .. }))
    ));
}

#[test]
fn failed_write_flush_and_publish_leave_no_temporary_behind() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(20, 10, [5, 6, 7]));
    let original = fs::read(&source).expect("source bytes");
    let desktop = single_display();
    for point in ["write", "sync", "publish"] {
        let error = prepare_with(
            ScreenTarget::Main,
            &source,
            Effects::default(),
            &deps(&env, &desktop),
            &token(),
            None,
            &FailingSeam { point },
        )
        .expect_err(point);
        assert!(
            matches!(error, PrepareError::Process(ProcessError::Cache(_))),
            "{point}: {error:?}"
        );
        assert!(
            cache_files(&env)
                .iter()
                .all(|path| path.extension().is_none_or(|ext| ext != "tmp")),
            "{point} left a temporary"
        );
        assert!(cache_files(&env).is_empty(), "{point} left an output");
        assert_eq!(fs::read(&source).expect("source bytes"), original);
    }
}

#[test]
fn concurrent_equivalent_producers_reuse_one_valid_output() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(20, 10, [8, 9, 10]));
    let one = single_display();
    let two = single_display();
    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            prepare(
                ScreenTarget::Main,
                &source,
                Effects::default(),
                &deps(&env, &one),
                &token(),
            )
        });
        let second = scope.spawn(|| {
            prepare(
                ScreenTarget::Main,
                &source,
                Effects::default(),
                &deps(&env, &two),
                &token(),
            )
        });
        (
            first.join().expect("first thread"),
            second.join().expect("second thread"),
        )
    });
    let first = first.expect("first producer");
    let second = second.expect("second producer");
    assert_eq!(first.targets()[0].image, second.targets()[0].image);
    assert_eq!(read_rgb8(&first.targets()[0].image).dimensions(), (1280, 720));
    let finals = cache_files(&env)
        .into_iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "png"))
        .count();
    assert_eq!(finals, 1, "the winner is the only final file");
    assert!(
        cache_files(&env)
            .iter()
            .all(|path| path.extension().is_none_or(|ext| ext != "tmp"))
    );
}

#[test]
fn preparation_never_modifies_the_source() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(20, 10, [11, 12, 13]));
    let before = fs::read(&source).expect("before");
    let metadata = fs::metadata(&source).expect("metadata");
    let desktop = single_display();
    let _ = prepare(
        ScreenTarget::Main,
        &source,
        Effects { radius: 5, blur: 2 },
        &deps(&env, &desktop),
        &token(),
    )
    .expect("prepare");
    assert_eq!(fs::read(&source).expect("after"), before);
    let after = fs::metadata(&source).expect("metadata");
    assert_eq!(after.len(), metadata.len());
    assert_eq!(after.mtime(), metadata.mtime());
    assert_eq!(after.ino(), metadata.ino());
}

// ---------------------------------------------------------------------------
// Cross-process lease scope
// ---------------------------------------------------------------------------

fn paths(root: &Path) -> AppPaths {
    AppPaths::resolve(&AppEnvironment {
        home: Some(root.to_path_buf()),
        temporary: Some(root.to_path_buf()),
    })
    .expect("paths")
}

fn current_exe() -> PathBuf { std::env::current_exe().expect("test executable") }

fn helper_command(root: &Path, mode: &str) -> Command {
    let mut command = Command::new(current_exe());
    command
        .args(["--exact", "--nocapture", "apply::tests::lease_child_helper"])
        .env("STACHE_WALLPAPER_LEASE_CHILD", mode)
        .env("STACHE_WALLPAPER_LEASE_ROOT", root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    command
}

fn read_marker<R: io::Read>(reader: &mut BufReader<R>) -> String {
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("read marker");
        assert!(read > 0, "the helper exited before printing a marker");
        let marker = line.trim();
        if matches!(marker, "ready" | "acquired" | "busy") {
            return marker.to_owned();
        }
    }
}

fn attempt(root: &Path, mode: &str) -> String {
    let mut child = helper_command(root, mode).spawn().expect("spawn contender");
    let mut reader = BufReader::new(child.stdout.take().expect("stdout"));
    let marker = read_marker(&mut reader);
    assert!(child.wait().expect("wait contender").success());
    marker
}

struct Holder {
    child: Child,
    input: Option<ChildStdin>,
    _output: BufReader<std::process::ChildStdout>,
}

impl Holder {
    fn spawn(root: &Path, mode: &str) -> Self {
        let mut child =
            helper_command(root, mode).stdin(Stdio::piped()).spawn().expect("spawn holder");
        let input = child.stdin.take();
        let mut output = BufReader::new(child.stdout.take().expect("stdout"));
        assert_eq!(read_marker(&mut output), "ready");
        Self { child, input, _output: output }
    }

    fn release(mut self) {
        if let Some(mut input) = self.input.take() {
            let _ = writeln!(input, "release");
        }
        assert!(self.child.wait().expect("wait holder").success());
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn lease_child_helper() {
    let Ok(mode) = std::env::var("STACHE_WALLPAPER_LEASE_CHILD") else {
        return;
    };
    let root = PathBuf::from(std::env::var("STACHE_WALLPAPER_LEASE_ROOT").expect("root"));
    let paths = paths(&root);
    let hold = mode.starts_with("hold");
    let budget = if hold {
        std::time::Duration::from_secs(10)
    } else {
        std::time::Duration::ZERO
    };
    let acquired = match mode.as_str() {
        "hold-shared" | "try-shared" => {
            CacheLease::shared(&paths, &LeaseWait::new(budget, CancellationToken::new()))
        }
        "hold-exclusive" | "try-exclusive" => {
            CacheLease::exclusive(&paths, &LeaseWait::new(budget, CancellationToken::new()))
        }
        other => panic!("unknown helper mode {other}"),
    };
    match acquired {
        Ok(lease) if hold => {
            println!("ready");
            io::stdout().flush().expect("flush");
            let mut line = String::new();
            let _ = io::stdin().read_line(&mut line);
            drop(lease);
        }
        Ok(_lease) => println!("acquired"),
        Err(CacheLeaseError::Busy) if !hold => println!("busy"),
        Err(error) => panic!("helper could not acquire: {error}"),
    }
    io::stdout().flush().expect("flush");
}

#[test]
fn the_shared_lease_is_held_until_apply_finishes() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(16, 8, [20, 21, 22]));
    let desktop = single_display();
    let prepared = prepare(
        ScreenTarget::Main,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("prepare");
    assert_eq!(attempt(env.root(), "try-exclusive"), "busy");
    prepared.apply(&desktop, &token(), &|| true).expect("apply");
    assert_eq!(attempt(env.root(), "try-exclusive"), "busy");
    drop(prepared);
    assert_eq!(attempt(env.root(), "try-exclusive"), "acquired");
}

#[test]
fn an_exclusive_holder_makes_prepare_busy_with_no_cache_write() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(16, 8, [23, 24, 25]));
    let desktop = single_display();
    let holder = Holder::spawn(env.root(), "hold-exclusive");
    let error = prepare_with(
        ScreenTarget::Main,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
        Some(FakeClock::new() as Arc<dyn LeaseClock>),
        &NoFailures,
    )
    .expect_err("busy");
    assert!(matches!(error, PrepareError::Lease(CacheLeaseError::Busy)));
    assert!(!env.paths().cache_directory().exists());
    holder.release();
    let _ = prepare(
        ScreenTarget::Main,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("after release");
}

// ---------------------------------------------------------------------------
// Clear while a prepare is paused inside processing
// ---------------------------------------------------------------------------

/// How long a paused seam or observer waits to be released.
const PAUSE_BUDGET: Duration = Duration::from_secs(10);

/// A publication seam that signals where it reached and then blocks there.
struct PausingSeam {
    point: &'static str,
    reached: Mutex<Sender<()>>,
    release: Mutex<Receiver<()>>,
}

impl PausingSeam {
    fn new(point: &'static str) -> (Arc<Self>, Receiver<()>, Sender<()>) {
        let (reached_sender, reached) = mpsc::channel();
        let (release, release_receiver) = mpsc::channel();
        (
            Arc::new(Self {
                point,
                reached: Mutex::new(reached_sender),
                release: Mutex::new(release_receiver),
            }),
            reached,
            release,
        )
    }

    fn pause(&self) -> io::Result<()> {
        self.reached
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send(())
            .map_err(|_| io::Error::other("the test dropped its seam observer"))?;
        self.release
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .recv_timeout(PAUSE_BUDGET)
            .map_err(|_| io::Error::other("timed out waiting to release the seam"))
    }
}

impl PublishSeam for PausingSeam {
    fn after_write(&self) -> io::Result<()> {
        if self.point == "write" {
            self.pause()
        } else {
            Ok(())
        }
    }

    fn before_sync(&self) -> io::Result<()> {
        if self.point == "sync" {
            self.pause()
        } else {
            Ok(())
        }
    }

    fn before_publish(&self) -> io::Result<()> {
        if self.point == "publish" {
            self.pause()
        } else {
            Ok(())
        }
    }
}

fn cache_has_final(env: &TestEnv) -> bool {
    cache_files(env)
        .iter()
        .any(|path| path.extension().is_some_and(|ext| ext == "png"))
}

fn cache_has_temporary(env: &TestEnv) -> bool {
    cache_files(env)
        .iter()
        .any(|path| path.extension().is_some_and(|ext| ext == "tmp"))
}

/// Pauses a real `prepare_with` at one publication point and proves `cache
/// clear`'s exclusive lease is refused until the prepared value is dropped.
fn a_paused_publication_refuses_clear(point: &'static str) {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(24, 12, [30, 31, 32]));
    let original = fs::read(&source).expect("source bytes");
    let desktop = single_display();
    let (seam, reached, release) = PausingSeam::new(point);

    let prepared = std::thread::scope(|scope| {
        let handle = scope.spawn(|| {
            prepare_with(
                ScreenTarget::Main,
                &source,
                Effects::default(),
                &deps(&env, &desktop),
                &token(),
                None,
                seam.as_ref(),
            )
        });
        reached.recv_timeout(PAUSE_BUDGET).expect("the seam must be reached");
        assert!(
            !cache_has_final(&env),
            "{point}: the final cache path must not exist while paused"
        );
        assert_eq!(
            attempt(env.root(), "try-exclusive"),
            "busy",
            "{point}: clear must be refused while publish is paused"
        );
        release.send(()).expect("release the seam");
        handle.join().expect("prepare thread")
    })
    .unwrap_or_else(|error| panic!("{point}: {error}"));

    assert_eq!(prepared.targets().len(), 1);
    let image = &prepared.targets()[0].image;
    assert_eq!(read_rgb8(image).dimensions(), (1280, 720));
    assert!(!cache_has_temporary(&env), "{point}: a temporary remains");
    assert_eq!(fs::read(&source).expect("source bytes"), original);

    assert_eq!(
        attempt(env.root(), "try-exclusive"),
        "busy",
        "{point}: the lease must still be held by the prepared value"
    );
    drop(prepared);
    assert_eq!(
        attempt(env.root(), "try-exclusive"),
        "acquired",
        "{point}: the lease must be released with the prepared value"
    );
}

#[test]
fn a_pause_after_the_cache_write_refuses_clear() { a_paused_publication_refuses_clear("write"); }

#[test]
fn a_pause_before_the_cache_sync_refuses_clear() { a_paused_publication_refuses_clear("sync"); }

#[test]
fn a_pause_before_the_cache_publish_refuses_clear() {
    a_paused_publication_refuses_clear("publish");
}

#[test]
fn a_cache_hit_read_holds_the_lease_against_clear() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(16, 8, [40, 41, 42]));
    let desktop = single_display();
    let first = prepare(
        ScreenTarget::Main,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("populate the cache");
    drop(first);

    // This prepare takes the read-only hit path and publishes nothing.
    let hit = prepare(
        ScreenTarget::Main,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &token(),
    )
    .expect("cache hit");
    assert!(hit.targets()[0].image.is_file());
    assert_eq!(
        attempt(env.root(), "try-exclusive"),
        "busy",
        "a cache-hit prepare must hold the shared lease"
    );
    drop(hit);
    assert_eq!(attempt(env.root(), "try-exclusive"), "acquired");
}

/// Holds every batch worker inside `item_started` until the test releases it.
struct PausingBatchObserver {
    started: Mutex<Sender<()>>,
    state: Mutex<BatchPauseState>,
    wake: Condvar,
}

struct BatchPauseState {
    started: usize,
    released: bool,
}

impl PausingBatchObserver {
    fn new() -> (Arc<Self>, Receiver<()>) {
        let (started_sender, started) = mpsc::channel();
        (
            Arc::new(Self {
                started: Mutex::new(started_sender),
                state: Mutex::new(BatchPauseState { started: 0, released: false }),
                wake: Condvar::new(),
            }),
            started,
        )
    }

    fn release(&self) {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).released = true;
        self.wake.notify_all();
    }
}

impl BatchObserver for PausingBatchObserver {
    fn item_started(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.started += 1;
        if state.started == 1 {
            self.started
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .send(())
                .expect("the test observer is still alive");
        }
        let deadline = Instant::now() + PAUSE_BUDGET;
        while !state.released {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "the batch pause timed out");
            let (guard, _) =
                self.wake.wait_timeout(state, remaining).unwrap_or_else(PoisonError::into_inner);
            state = guard;
        }
        drop(state);
    }
}

#[test]
fn a_paused_batch_item_refuses_clear() {
    let env = TestEnv::new();
    let first = env.png("first.png", solid(16, 9, [50, 51, 52]));
    let second = env.png("second.png", solid(16, 9, [53, 54, 55]));
    let original = fs::read(&first).expect("source bytes");
    let target = crate::selection::SelectedTarget {
        entry: entry(1, 640.0, 360.0, 1.0),
        pixels: target_pixels(&entry(1, 640.0, 360.0, 1.0)).expect("pixels"),
    };
    let items = vec![
        BatchItem {
            source: first.clone(),
            targets: vec![target],
            effects: Effects::default(),
        },
        BatchItem {
            source: second,
            targets: vec![target],
            effects: Effects::default(),
        },
    ];
    let (observer, started) = PausingBatchObserver::new();

    let report = std::thread::scope(|scope| {
        let handle = scope.spawn(|| {
            prepare_batch_with(
                items,
                &BatchDeps { paths: env.paths() },
                &token(),
                observer.as_ref(),
            )
        });
        started.recv_timeout(PAUSE_BUDGET).expect("a worker must start an item");
        assert!(
            !cache_has_final(&env),
            "no output may exist while the batch is paused"
        );
        assert_eq!(
            attempt(env.root(), "try-exclusive"),
            "busy",
            "clear must be refused while a batch worker is paused"
        );
        observer.release();
        handle.join().expect("batch thread")
    })
    .unwrap_or_else(|error| panic!("{error}"));

    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(report.outputs.len(), 2);
    for output in &report.outputs {
        assert_eq!(read_rgb8(&output.image).dimensions(), (640, 360));
    }
    assert!(!cache_has_temporary(&env), "a temporary remains");
    assert_eq!(fs::read(&first).expect("source bytes"), original);
    assert_eq!(attempt(env.root(), "try-exclusive"), "acquired");
}

// ---------------------------------------------------------------------------
// Caller-resolved mixed-source plans and guarded apply
// ---------------------------------------------------------------------------

fn plan_deps(env: &TestEnv) -> PreparePlanDeps<'_> {
    PreparePlanDeps { paths: env.paths(), memo: None }
}

fn plan_for(desktop: &FakeDesktop, sources: &[PathBuf], effects: Effects) -> Vec<PlannedTarget> {
    let snapshot = desktop.snapshot().expect("caller snapshot");
    let selected = resolve_targets(&snapshot, &ScreenTarget::All).expect("selected");
    selected
        .into_iter()
        .zip(sources)
        .map(|(target, source)| PlannedTarget {
            target,
            source: source.clone(),
            effects,
        })
        .collect()
}

#[test]
fn a_mixed_source_plan_uses_one_caller_snapshot_and_distinct_sizes() {
    let env = TestEnv::new();
    let desktop = FakeDesktop::with_snapshot(snapshot(vec![
        entry(1, 1280.0, 720.0, 1.0),
        entry(2, 640.0, 360.0, 2.0),
    ]));
    let first = env.png("first.png", solid(64, 36, [10, 20, 30]));
    let second = env.jpeg("second.jpeg", solid(40, 40, [40, 50, 60]));
    let plan = plan_for(&desktop, &[first, second], Effects::default());
    assert_eq!(desktop.snapshot_reads(), 1);

    let prepared = prepare_plan(&plan, &plan_deps(&env), &token()).expect("prepare plan");

    assert_eq!(desktop.snapshot_reads(), 1, "preparation reads no snapshot");
    assert_eq!(prepared.targets().len(), 2);
    assert_eq!(read_rgb8(&prepared.targets()[0].image).dimensions(), (1280, 720));
    assert_eq!(read_rgb8(&prepared.targets()[1].image).dimensions(), (1280, 720));
    assert_ne!(prepared.targets()[0].image, prepared.targets()[1].image);
    assert!(desktop.calls().is_empty());
}

#[test]
fn adjacent_targets_with_the_same_source_reuse_one_output() {
    let env = TestEnv::new();
    let desktop = FakeDesktop::with_snapshot(snapshot(vec![
        entry(1, 640.0, 360.0, 1.0),
        entry(2, 640.0, 360.0, 1.0),
    ]));
    let source = env.png("same.png", solid(32, 18, [3, 4, 5]));
    let plan = plan_for(&desktop, &[source.clone(), source], Effects::default());

    let prepared = prepare_plan(&plan, &plan_deps(&env), &token()).expect("prepare plan");

    assert_eq!(prepared.targets()[0].image, prepared.targets()[1].image);
    assert_eq!(
        cache_files(&env)
            .iter()
            .filter(|path| path.extension().is_some_and(|ext| ext == "png"))
            .count(),
        1
    );
}

#[test]
fn a_duplicate_display_is_refused_before_any_read_or_cache_write() {
    let env = TestEnv::new();
    let desktop = single_display();
    let first = env.png("first.png", solid(8, 8, [1, 2, 3]));
    let mut plan = plan_for(&desktop, &[first], Effects::default());
    let duplicate = plan[0].clone();
    plan.push(duplicate);

    let error = prepare_plan(&plan, &plan_deps(&env), &token()).expect_err("duplicate");

    assert!(matches!(
        error,
        PrepareError::DuplicateDisplay(display) if display == stache_macos::DisplayId::new(1)
    ));
    assert!(!env.paths().cache_directory().exists());
}

#[test]
fn an_empty_plan_is_refused_before_the_cache_exists() {
    let env = TestEnv::new();
    let error = prepare_plan(&[], &plan_deps(&env), &token()).expect_err("empty plan");
    assert!(matches!(
        error,
        PrepareError::Selection(SelectionError::NoDisplays)
    ));
    assert!(!env.paths().cache_directory().exists());
}

#[test]
fn a_source_failure_mid_plan_makes_zero_setter_calls() {
    let env = TestEnv::new();
    let desktop = FakeDesktop::with_snapshot(snapshot(vec![
        entry(1, 320.0, 180.0, 1.0),
        entry(2, 320.0, 180.0, 1.0),
    ]));
    let good = env.png("good.png", solid(16, 9, [7, 8, 9]));
    let broken = env.path("broken.png");
    fs::write(&broken, b"not an image").expect("broken source");
    let plan = plan_for(&desktop, &[good, broken], Effects::default());

    let error = prepare_plan(&plan, &plan_deps(&env), &token()).expect_err("decode failure");

    assert!(matches!(
        error,
        PrepareError::Process(ProcessError::Decode { .. })
    ));
    assert!(desktop.calls().is_empty());
    assert!(
        desktop.calls().is_empty(),
        "no setter call may follow a prepare failure"
    );
}

#[test]
fn an_expired_plan_apply_makes_no_native_call() {
    let env = TestEnv::new();
    let desktop = FakeDesktop::with_snapshot(snapshot(vec![
        entry(1, 640.0, 360.0, 1.0),
        entry(2, 640.0, 360.0, 1.0),
    ]));
    let source = env.png("source.png", solid(32, 18, [2, 3, 4]));
    let plan = plan_for(&desktop, &[source.clone(), source], Effects::default());
    let prepared = prepare_plan(&plan, &plan_deps(&env), &token()).expect("prepare plan");
    let past = Instant::now().checked_sub(Duration::from_secs(1)).expect("past instant");
    let clock: Arc<dyn DesktopClock> = Arc::new(SystemDesktopClock);

    let outcome = prepared
        .apply_guarded(&desktop, &token(), Arc::new(|| true), Some(past), clock)
        .expect("guard refusal is not a lease error");

    assert!(outcome.applied.is_empty());
    assert!(outcome.unknown.is_empty());
    assert_eq!(outcome.not_attempted.len(), 2);
    assert!(desktop.calls().is_empty());
}

#[test]
fn a_stale_plan_apply_makes_no_native_call() {
    let env = TestEnv::new();
    let desktop = single_display();
    let source = env.png("source.png", solid(16, 9, [2, 3, 4]));
    let plan = plan_for(&desktop, &[source], Effects::default());
    let prepared = prepare_plan(&plan, &plan_deps(&env), &token()).expect("prepare plan");
    let clock: Arc<dyn DesktopClock> = Arc::new(SystemDesktopClock);

    let outcome = prepared
        .apply_guarded(&desktop, &token(), Arc::new(|| false), None, clock)
        .expect("stale is not a lease error");

    assert!(outcome.applied.is_empty());
    assert_eq!(outcome.not_attempted.len(), 1);
    assert!(desktop.calls().is_empty());
}

/// A guarded backend that retains a clone of the operation after returning
/// `Unknown`, standing in for an OS call that has already started.
struct RetainingBackend {
    inner: FakeDesktop,
    started: Mutex<Sender<()>>,
    release: Arc<Mutex<Receiver<()>>>,
    held: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl RetainingBackend {
    fn new() -> (Arc<Self>, Receiver<()>, Sender<()>) {
        let (started, started_receiver) = mpsc::channel();
        let (release, release_receiver) = mpsc::channel();
        (
            Arc::new(Self {
                inner: FakeDesktop::new(),
                started: Mutex::new(started),
                release: Arc::new(Mutex::new(release_receiver)),
                held: Mutex::new(None),
            }),
            started_receiver,
            release,
        )
    }

    fn join(&self) {
        let handle = self.held.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(handle) = handle {
            handle.join().expect("retained operation thread");
        }
    }
}

impl DesktopBackend for RetainingBackend {
    fn snapshot(&self) -> Result<DisplaySnapshot, DesktopError> { self.inner.snapshot() }

    fn set_image(
        &self,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &Path,
    ) -> Result<(), DesktopError> {
        self.inner.set_image(display, expected, image)
    }
}

impl GuardedDesktopBackend for RetainingBackend {
    fn snapshot_guarded(
        &self,
        operation: &stache_macos::DesktopOperation,
    ) -> Result<DisplaySnapshot, DesktopError> {
        self.inner.snapshot_guarded(operation)
    }

    fn set_image_guarded(
        &self,
        operation: &stache_macos::DesktopOperation,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &Path,
    ) -> Result<(), DesktopError> {
        operation.check("desktop image set")?;
        let held = operation.clone();
        let started = self.started.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let release = Arc::clone(&self.release);
        let handle = std::thread::spawn(move || {
            let _ = started.send(());
            let _ = release.lock().unwrap_or_else(PoisonError::into_inner).recv();
            drop(held);
        });
        *self.held.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle);
        self.inner.set_image(display, expected, image)?;
        Err(DesktopError::Unknown { operation: "desktop image set" })
    }
}

#[test]
fn the_late_native_closure_keeps_the_prepared_lease_alive() {
    let env = TestEnv::new();
    let desktop = single_display();
    let source = env.png("source.png", solid(16, 9, [4, 5, 6]));
    let plan = plan_for(&desktop, &[source], Effects::default());
    let prepared = prepare_plan(&plan, &plan_deps(&env), &token()).expect("prepare plan");
    let (backend, started, release) = RetainingBackend::new();
    let clock: Arc<dyn DesktopClock> = Arc::new(SystemDesktopClock);

    let outcome = prepared
        .apply_guarded(backend.as_ref(), &token(), Arc::new(|| true), None, clock)
        .expect("apply");
    assert_eq!(outcome.unknown.len(), 1);
    started.recv_timeout(PAUSE_BUDGET).expect("late operation started");

    // The prepared value is gone, but the OS-side call still holds the lease.
    drop(prepared);
    assert_eq!(
        attempt(env.root(), "try-exclusive"),
        "busy",
        "the late closure must retain the cache lease"
    );

    release.send(()).expect("release the late operation");
    backend.join();
    assert_eq!(
        attempt(env.root(), "try-exclusive"),
        "acquired",
        "the lease must be released when the late operation finishes"
    );
}

/// Cancels the token at the last publication seam.
struct CancelAtPublish<'a> {
    cancel: &'a CancellationToken,
}

impl PublishSeam for CancelAtPublish<'_> {
    fn before_publish(&self) -> io::Result<()> {
        self.cancel.cancel();
        Ok(())
    }
}

#[test]
fn cancellation_at_the_final_publication_point_leaves_no_output_or_temporary() {
    let env = TestEnv::new();
    let source = env.png("source.png", solid(24, 12, [8, 9, 10]));
    let desktop = single_display();
    let cancel = token();

    let error = prepare_with(
        ScreenTarget::Main,
        &source,
        Effects::default(),
        &deps(&env, &desktop),
        &cancel,
        None,
        &CancelAtPublish { cancel: &cancel },
    )
    .expect_err("cancelled at publish");

    assert!(matches!(error, PrepareError::Process(ProcessError::Cancelled)));
    assert!(!cache_has_final(&env));
    assert!(!cache_has_temporary(&env));
    assert!(desktop.calls().is_empty());
}

/// Manual cold-processing evidence for `docs/features/wallpapers.md`.
///
/// Not part of the automated gate: it decodes a 6000 × 4000 JPEG and encodes a
/// 2560 × 1440 PNG three times from an empty cache. Run in release with
/// `cargo test -p stache-wallpapers --release measure_cold_processing -- --ignored --nocapture`.
#[test]
#[ignore = "manual cold-processing measurement"]
fn measure_cold_processing_6000x4000_to_2560x1440() {
    let source_env = TestEnv::new();
    let source = source_env.path("large.jpg");
    write_jpeg(&source, &coordinates(6000, 4000));

    let display = snapshot(vec![entry(1, 2560.0, 1440.0, 1.0)]);
    let selected = resolve_targets(&display, &ScreenTarget::All).expect("selected");
    let effects = Effects { radius: 24, blur: 12 };

    let mut runs = Vec::new();
    for _ in 0..3 {
        // A fresh identity root empties the cache for every run.
        let env = TestEnv::new();
        let plan: Vec<PlannedTarget> = selected
            .iter()
            .map(|target| PlannedTarget {
                target: *target,
                source: source.clone(),
                effects,
            })
            .collect();
        let started = Instant::now();
        let prepared = prepare_plan(&plan, &plan_deps(&env), &token()).expect("prepare plan");
        runs.push(started.elapsed());
        assert_eq!(prepared.targets().len(), 1);
        assert_eq!(
            read_rgb8(&prepared.targets()[0].image).dimensions(),
            (2560, 1440)
        );
    }
    runs.sort_unstable();
    let median = runs[runs.len() / 2];
    println!(
        "cold processing 6000x4000 JPEG -> 2560x1440 PNG, radius=24 blur=12: {median:?} (runs {runs:?})"
    );
}

/// A deterministic 6000 × 4000 gradient that differs per `seed`, so each
/// display decodes its own source (the random-all worst case).
fn seeded_gradient(seed: u32) -> image::RgbImage {
    image::RgbImage::from_fn(6000, 4000, |x, y| {
        #[allow(clippy::cast_possible_truncation)]
        image::Rgb([
            ((x + seed * 37) % 256) as u8,
            ((y * (seed + 1)) % 256) as u8,
            ((x / 7 + y / 5 + seed * 91) % 256) as u8,
        ])
    })
}

/// Manual multi-display cold-processing evidence for `docs/features/wallpapers.md`.
///
/// Not part of the automated gate. Three displays (2560 × 1440 @1x, 1512 × 982
/// points @2x = 3024 × 1964 pixels, 2560 × 1440 points @2x = 5120 × 2880
/// pixels) each get a distinct 6000 × 4000 JPEG, `radius = 24`, `blur = 12`,
/// through the shipped `prepare_plan`, from an empty cache and without any
/// setter. Run in release with
/// `cargo test -p stache-wallpapers --release measure_cold_processing_multi_display -- --ignored --nocapture`.
#[test]
#[ignore = "manual cold-processing measurement"]
fn measure_cold_processing_multi_display() {
    let source_env = TestEnv::new();
    let sources: Vec<PathBuf> = (0..3)
        .map(|seed| {
            let path = source_env.path(&format!("large-{seed}.jpg"));
            write_jpeg(&path, &seeded_gradient(seed));
            path
        })
        .collect();

    let displays = snapshot(vec![
        entry(1, 2560.0, 1440.0, 1.0),
        entry(2, 1512.0, 982.0, 2.0),
        entry(3, 2560.0, 1440.0, 2.0),
    ]);
    let selected = resolve_targets(&displays, &ScreenTarget::All).expect("selected");
    let expected = [(2560, 1440), (3024, 1964), (5120, 2880)];
    let effects = Effects { radius: 24, blur: 12 };

    let mut runs = Vec::new();
    for _ in 0..3 {
        // A fresh identity root empties the cache for every run.
        let env = TestEnv::new();
        let plan: Vec<PlannedTarget> = selected
            .iter()
            .zip(&sources)
            .map(|(target, source)| PlannedTarget {
                target: *target,
                source: source.clone(),
                effects,
            })
            .collect();
        let started = Instant::now();
        let prepared = prepare_plan(&plan, &plan_deps(&env), &token()).expect("prepare plan");
        let elapsed = started.elapsed();
        assert_eq!(prepared.targets().len(), 3);
        for (target, size) in prepared.targets().iter().zip(expected) {
            assert_eq!(read_rgb8(&target.image).dimensions(), size);
        }
        println!("run: {} ms", elapsed.as_millis());
        runs.push(elapsed);
    }
    runs.sort_unstable();
    let median = runs[runs.len() / 2];
    println!(
        "multi-display cold processing, 3 distinct 6000x4000 JPEGs -> 2560x1440@1x, 3024x1964 and \
         5120x2880 PNGs, radius=24 blur=12: median {} ms (runs {runs:?})",
        median.as_millis()
    );
}

// ---------------------------------------------------------------------------
// Parallel plan preparation
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ProbeState {
    started: Vec<usize>,
    finished: Vec<usize>,
    active: usize,
    max_active: usize,
    threads: std::collections::HashSet<std::thread::ThreadId>,
    reads: Vec<PathBuf>,
    decodes: Vec<PathBuf>,
    resident: usize,
    max_resident: usize,
    held: BTreeSet<usize>,
}

/// A plan observer that records events and holds workers at
/// `output_started` until `gate` outputs started and their index is not held.
struct PlanProbe {
    gate: usize,
    state: Mutex<ProbeState>,
    changed: Condvar,
}

impl PlanProbe {
    fn new(gate: usize, held: &[usize]) -> Self {
        Self {
            gate,
            state: Mutex::new(ProbeState {
                held: held.iter().copied().collect(),
                ..ProbeState::default()
            }),
            changed: Condvar::new(),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, ProbeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn update(&self, change: impl FnOnce(&mut ProbeState)) {
        change(&mut self.state());
        self.changed.notify_all();
    }

    /// Blocks until `ready` holds, failing after the pause budget.
    fn wait_for(&self, mut ready: impl FnMut(&ProbeState) -> bool) -> bool {
        let deadline = Instant::now() + PAUSE_BUDGET;
        let mut state = self.state();
        while !ready(&state) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            state =
                self.changed.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0;
        }
        drop(state);
        true
    }

    fn release(&self, plan_index: usize) {
        self.update(|state| {
            state.held.remove(&plan_index);
        });
    }
}

impl PlanObserver for PlanProbe {
    fn output_started(&self, plan_index: usize) {
        self.update(|state| {
            state.started.push(plan_index);
            state.active += 1;
            state.max_active = state.max_active.max(state.active);
            state.threads.insert(std::thread::current().id());
        });
        let gate = self.gate;
        // A missed hold surfaces as a failed assertion in the test.
        let _ =
            self.wait_for(|state| state.started.len() >= gate && !state.held.contains(&plan_index));
    }

    fn output_finished(&self, plan_index: usize) {
        self.update(|state| {
            state.active -= 1;
            state.finished.push(plan_index);
        });
    }

    fn source_read(&self, source: &Path) {
        self.update(|state| {
            state.reads.push(source.to_path_buf());
            state.resident += 1;
            state.max_resident = state.max_resident.max(state.resident);
        });
    }

    fn source_decoded(&self, source: &Path) {
        self.update(|state| state.decodes.push(source.to_path_buf()));
    }

    fn source_released(&self, _source: &Path) { self.update(|state| state.resident -= 1); }
}

fn observed(
    env: &TestEnv,
    plan: &[PlannedTarget],
    cancel: &CancellationToken,
    seam: &dyn PublishSeam,
    probe: &PlanProbe,
) -> Result<Prepared, PrepareError> {
    observed_with(env, plan, cancel, seam, probe, limits::MAX_CONCURRENT_WORKERS)
}

/// Runs with an explicit worker count, so holds never depend on the cores.
fn observed_with(
    env: &TestEnv,
    plan: &[PlannedTarget],
    cancel: &CancellationToken,
    seam: &dyn PublishSeam,
    probe: &PlanProbe,
    workers: usize,
) -> Result<Prepared, PrepareError> {
    prepare_plan_observed(plan, &plan_deps(env), cancel, None, seam, probe, workers)
}

fn final_pngs(env: &TestEnv) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = cache_files(env)
        .into_iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "png"))
        .collect();
    files.sort();
    files
}

fn sorted(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort();
    paths
}

fn three_displays() -> FakeDesktop {
    FakeDesktop::with_snapshot(snapshot(vec![
        entry(1, 64.0, 36.0, 1.0),
        entry(2, 48.0, 30.0, 2.0),
        entry(3, 40.0, 40.0, 1.0),
    ]))
}

#[test]
fn three_sources_on_three_displays_prepare_at_once_in_plan_order() {
    let env = TestEnv::new();
    let desktop = three_displays();
    let colors = [[200, 10, 10], [10, 200, 10], [10, 10, 200]];
    let sources: Vec<PathBuf> = colors
        .iter()
        .enumerate()
        .map(|(index, color)| env.png(&format!("s{index}.png"), solid(32, 18, *color)))
        .collect();
    let plan = plan_for(&desktop, &sources, Effects::default());
    // Every worker waits until all three started: only three concurrent
    // workers can pass this hold.
    let probe = PlanProbe::new(3, &[]);

    let prepared = observed(&env, &plan, &token(), &NoFailures, &probe).expect("prepare");

    let state = probe.state();
    assert_eq!(state.max_active, 3);
    assert_eq!(state.threads.len(), 3);
    assert_eq!(sorted(state.reads.clone()), sorted(sources), "each read once");
    let expected = [(64, 36), (96, 60), (40, 40)];
    for ((target, color), size) in prepared.targets().iter().zip(colors).zip(expected) {
        let image = read_rgb8(&target.image);
        assert_eq!(image.dimensions(), size);
        assert_eq!(image.get_pixel(size.0 / 2, size.1 / 2).0, color);
    }
    let ids: Vec<u32> = prepared.targets().iter().map(|target| target.entry.id.raw()).collect();
    assert_eq!(ids, vec![1, 2, 3]);
    assert_eq!(state.resident, 0, "every source is released before return");
}

#[test]
fn one_source_at_two_sizes_is_read_and_decoded_once_and_resized_in_parallel() {
    let env = TestEnv::new();
    let desktop = three_displays();
    let shared = env.png("shared.png", solid(32, 18, [9, 8, 7]));
    let other = env.png("other.png", solid(32, 18, [1, 2, 3]));
    // The shared source is used by the first and last display, not adjacent.
    let plan = plan_for(
        &desktop,
        &[shared.clone(), other, shared.clone()],
        Effects::default(),
    );
    let probe = PlanProbe::new(3, &[]);

    let prepared = observed(&env, &plan, &token(), &NoFailures, &probe).expect("prepare");

    let state = probe.state();
    assert_eq!(state.reads.iter().filter(|path| **path == shared).count(), 1);
    assert_eq!(state.decodes.iter().filter(|path| **path == shared).count(), 1);
    assert_eq!(
        state.max_active, 3,
        "both sizes of the shared source ran at once"
    );
    assert_eq!(read_rgb8(&prepared.targets()[0].image).dimensions(), (64, 36));
    assert_eq!(read_rgb8(&prepared.targets()[2].image).dimensions(), (40, 40));
    assert_eq!(final_pngs(&env).len(), 3);
}

#[test]
fn one_source_size_and_effect_is_produced_once_for_both_displays() {
    let env = TestEnv::new();
    let desktop = FakeDesktop::with_snapshot(snapshot(vec![
        entry(1, 40.0, 30.0, 1.0),
        entry(2, 80.0, 60.0, 1.0),
        entry(3, 40.0, 30.0, 1.0),
    ]));
    let shared = env.png("shared.png", solid(32, 18, [9, 8, 7]));
    let plan = plan_for(&desktop, &[shared.clone(), shared.clone(), shared], Effects {
        radius: 3,
        blur: 1,
    });
    let probe = PlanProbe::new(0, &[]);

    let prepared = observed(&env, &plan, &token(), &NoFailures, &probe).expect("prepare");

    let state = probe.state();
    assert_eq!(
        state.started,
        vec![0, 1],
        "two distinct outputs for three displays"
    );
    assert_eq!(state.reads.len(), 1);
    assert_eq!(state.decodes.len(), 1);
    assert_eq!(prepared.targets()[0].image, prepared.targets()[2].image);
    assert_ne!(prepared.targets()[0].image, prepared.targets()[1].image);
    assert_eq!(final_pngs(&env).len(), 2);
}

#[test]
fn a_single_output_plan_runs_on_the_calling_thread() {
    let env = TestEnv::new();
    let desktop = FakeDesktop::with_snapshot(snapshot(vec![
        entry(1, 40.0, 30.0, 1.0),
        entry(2, 40.0, 30.0, 1.0),
    ]));
    let shared = env.png("shared.png", solid(16, 9, [4, 4, 4]));
    let plan = plan_for(&desktop, &[shared.clone(), shared], Effects::default());
    let probe = PlanProbe::new(0, &[]);

    let _prepared = observed(&env, &plan, &token(), &NoFailures, &probe).expect("prepare");

    let state = probe.state();
    assert_eq!(state.started.len(), 1);
    assert_eq!(state.threads.iter().copied().collect::<Vec<_>>(), vec![
        std::thread::current().id()
    ]);
}

#[test]
fn a_failure_in_the_second_output_stops_the_third_at_its_next_boundary() {
    let env = TestEnv::new();
    let desktop = three_displays();
    let first = env.png("first.png", solid(16, 9, [1, 1, 1]));
    let broken = env.path("broken.png");
    fs::write(&broken, b"not an image").expect("broken source");
    let third = env.png("third.png", solid(16, 9, [3, 3, 3]));
    let plan = plan_for(&desktop, &[first, broken, third.clone()], Effects::default());
    let probe = PlanProbe::new(3, &[2]);

    let error = std::thread::scope(|scope| {
        let running = scope.spawn(|| observed(&env, &plan, &token(), &NoFailures, &probe));
        assert!(
            probe.wait_for(|state| state.finished.contains(&1)),
            "second output failed"
        );
        probe.release(2);
        running.join().expect("prepare thread").expect_err("decode failure")
    });

    assert!(
        matches!(error, PrepareError::Process(ProcessError::Decode { .. })),
        "{error:?}"
    );
    let state = probe.state();
    assert!(state.finished.contains(&2), "the third worker returned");
    assert!(
        !state.reads.contains(&third),
        "the third output stopped before its read"
    );
    assert!(!cache_has_temporary(&env));
    assert_eq!(state.resident, 0);
}

#[test]
fn with_several_failures_the_lowest_plan_index_error_is_returned() {
    let env = TestEnv::new();
    let desktop = three_displays();
    let sources: Vec<PathBuf> = (0..3)
        .map(|index| env.png(&format!("s{index}.png"), solid(16, 9, [index, 7, 7])))
        .collect();
    let plan = plan_for(&desktop, &sources, Effects::default());
    let first_key = {
        let read = crate::source::read_source(&sources[0]).expect("read");
        let pixels = target_pixels(&plan[0].target.entry).expect("pixels");
        cache_key(&read.canonical, &read.digest, pixels, Effects::default())
    };
    // All three outputs pass every boundary and then fail at publication, so
    // each one records its own error.
    let fail = || {};
    let barrier = PublishBarrier {
        count: 3,
        arrived: Mutex::new(0),
        changed: Condvar::new(),
        act: &fail,
        fail: true,
    };
    let probe = PlanProbe::new(0, &[]);

    let error = observed(&env, &plan, &token(), &barrier, &probe).expect_err("failure");

    let PrepareError::Process(ProcessError::Cache(CacheError::Io { path, .. })) = &error else {
        panic!("unexpected error {error:?}");
    };
    let name = path.file_name().and_then(|name| name.to_str()).expect("temporary name");
    assert!(name.starts_with(&format!(".{first_key}.")), "{name}");
    assert!(!cache_has_temporary(&env));
}

#[test]
fn cancellation_while_workers_run_returns_cancelled_and_leaves_nothing() {
    let env = TestEnv::new();
    let desktop = three_displays();
    let sources: Vec<PathBuf> = (0..3)
        .map(|index| env.png(&format!("s{index}.png"), solid(16, 9, [index, 2, 3])))
        .collect();
    let plan = plan_for(&desktop, &sources, Effects::default());
    let probe = PlanProbe::new(3, &[0, 1, 2]);
    let cancel = token();

    let error = std::thread::scope(|scope| {
        let running = scope.spawn(|| observed(&env, &plan, &cancel, &NoFailures, &probe));
        assert!(
            probe.wait_for(|state| state.started.len() == 3),
            "three workers held"
        );
        cancel.cancel();
        for index in 0..3 {
            probe.release(index);
        }
        running.join().expect("prepare thread").expect_err("cancelled")
    });

    assert!(matches!(error, PrepareError::Cancelled), "{error:?}");
    assert!(final_pngs(&env).is_empty());
    assert!(!cache_has_temporary(&env));
    assert!(probe.state().reads.is_empty());
}

/// Holds every publication at the final point until `count` arrived, then
/// runs `act` once and lets them all commit.
struct PublishBarrier<'a> {
    count: usize,
    arrived: Mutex<usize>,
    changed: Condvar,
    act: &'a (dyn Fn() + Sync),
    fail: bool,
}

impl PublishSeam for PublishBarrier<'_> {
    fn before_publish(&self) -> io::Result<()> {
        let timed_out = {
            let mut arrived = self.arrived.lock().unwrap_or_else(PoisonError::into_inner);
            *arrived += 1;
            if *arrived == self.count {
                (self.act)();
                self.changed.notify_all();
            }
            self.changed
                .wait_timeout_while(arrived, PAUSE_BUDGET, |arrived| *arrived < self.count)
                .unwrap_or_else(PoisonError::into_inner)
                .1
                .timed_out()
        };
        if timed_out {
            return Err(io::Error::other("publication barrier timed out"));
        }
        if self.fail {
            return Err(io::Error::other("injected publication failure"));
        }
        Ok(())
    }
}

fn refused_at_commit(act: &(dyn Fn(&TestEnv, &CancellationToken) + Sync)) -> PrepareError {
    let env = TestEnv::new();
    let desktop = three_displays();
    let sources: Vec<PathBuf> = (0..3)
        .map(|index| env.png(&format!("s{index}.png"), solid(16, 9, [index, 5, 6])))
        .collect();
    let plan = plan_for(&desktop, &sources, Effects::default());
    let cancel = token();
    let run = || act(&env, &cancel);
    let barrier = PublishBarrier {
        count: 3,
        arrived: Mutex::new(0),
        changed: Condvar::new(),
        act: &run,
        fail: false,
    };
    let probe = PlanProbe::new(0, &[]);

    let error = observed(&env, &plan, &cancel, &barrier, &probe).expect_err("refused");

    assert_eq!(
        probe.state().finished.len(),
        3,
        "every worker reached the commit"
    );
    assert!(final_pngs(&env).is_empty(), "no worker published");
    assert!(!cache_has_temporary(&env));
    error
}

#[test]
fn cancellation_at_the_final_commit_publishes_nothing_for_every_worker() {
    let error = refused_at_commit(&|_, cancel| cancel.cancel());
    assert!(
        matches!(error, PrepareError::Process(ProcessError::Cancelled)),
        "{error:?}"
    );
}

#[test]
fn a_replaced_lease_at_the_final_commit_publishes_nothing_for_every_worker() {
    let error = refused_at_commit(&|env, _| {
        let lease = env.paths().cache_lease_path();
        fs::remove_file(&lease).expect("remove lease file");
        fs::write(&lease, b"").expect("replacement lease file");
        fs::set_permissions(&lease, fs::Permissions::from_mode(0o600)).expect("mode");
    });
    assert!(
        matches!(error, PrepareError::Process(ProcessError::Lease(_))),
        "{error:?}"
    );
}

#[test]
fn eight_outputs_never_run_more_than_the_worker_cap_or_hold_more_decodes() {
    let env = TestEnv::new();
    let desktop = FakeDesktop::with_snapshot(snapshot(
        (1..=8).map(|id| entry(id, 32.0 + f64::from(id), 18.0, 1.0)).collect(),
    ));
    let sources: Vec<PathBuf> = (0..8)
        .map(|index| env.png(&format!("s{index}.png"), solid(24, 12, [index, 1, 2])))
        .collect();
    let plan = plan_for(&desktop, &sources, Effects::default());
    // Hold until the cap is running at once, so it is reached rather than
    // trivially met. Asking for more workers than the cap is clamped.
    let probe = PlanProbe::new(limits::MAX_CONCURRENT_WORKERS, &[]);

    let prepared = observed_with(&env, &plan, &token(), &NoFailures, &probe, 64).expect("prepare");

    let state = probe.state();
    assert_eq!(state.max_active, limits::MAX_CONCURRENT_WORKERS);
    assert!(state.threads.len() <= limits::MAX_CONCURRENT_WORKERS);
    assert!(
        state.max_resident <= limits::MAX_CONCURRENT_WORKERS,
        "{}",
        state.max_resident
    );
    assert_eq!(state.reads.len(), 8);
    assert_eq!(state.decodes.len(), 8);
    assert_eq!(state.resident, 0);
    assert_eq!(prepared.targets().len(), 8);
}

#[test]
fn a_cache_file_written_with_best_compression_is_a_hit() {
    let env = TestEnv::new();
    let desktop = single_display();
    let source = env.png("source.png", solid(32, 18, [6, 7, 8]));
    let plan = plan_for(&desktop, std::slice::from_ref(&source), Effects::default());
    let pixels = target_pixels(&plan[0].target.entry).expect("pixels");
    let read = crate::source::read_source(&source).expect("read");
    let key = cache_key(&read.canonical, &read.digest, pixels, Effects::default());
    let cache = Cache::open(env.paths()).expect("cache");
    let path = cache.path_for(&key);
    let image = solid(pixels.width, pixels.height, [6, 7, 8]);
    let mut previous = Vec::new();
    image
        .write_with_encoder(image::codecs::png::PngEncoder::new_with_quality(
            &mut previous,
            image::codecs::png::CompressionType::Best,
            image::codecs::png::FilterType::Adaptive,
        ))
        .expect("encode with the previous settings");
    fs::write(&path, &previous).expect("previous cache file");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("mode");
    let probe = PlanProbe::new(0, &[]);

    let prepared = observed(&env, &plan, &token(), &NoFailures, &probe).expect("prepare");

    assert_eq!(prepared.targets()[0].image, path);
    assert_eq!(
        fs::read(&path).expect("bytes"),
        previous,
        "the hit was not rewritten"
    );
    assert!(probe.state().decodes.is_empty(), "a hit decodes no source");
}

#[test]
fn the_production_worker_count_keeps_every_bound_on_any_machine() {
    let env = TestEnv::new();
    let desktop = FakeDesktop::with_snapshot(snapshot(
        (1..=8).map(|id| entry(id, 32.0 + f64::from(id), 18.0, 1.0)).collect(),
    ));
    let sources: Vec<PathBuf> = (0..8)
        .map(|index| env.png(&format!("s{index}.png"), solid(24, 12, [index, 3, 4])))
        .collect();
    let plan = plan_for(&desktop, &sources, Effects::default());
    let probe = PlanProbe::new(0, &[]);

    // The count `prepare_plan_with` passes in production.
    let prepared = observed_with(
        &env,
        &plan,
        &token(),
        &NoFailures,
        &probe,
        crate::batch::worker_count(),
    )
    .expect("prepare");

    let state = probe.state();
    assert!(state.max_active <= crate::batch::worker_count());
    assert!(state.max_resident <= limits::MAX_CONCURRENT_WORKERS);
    assert_eq!(state.reads.len(), 8);
    assert_eq!(prepared.targets().len(), 8);
}

#[test]
fn a_corrupt_source_shared_by_two_outputs_fails_once_and_leaves_nothing() {
    let env = TestEnv::new();
    let desktop = FakeDesktop::with_snapshot(snapshot(vec![
        entry(1, 40.0, 30.0, 1.0),
        entry(2, 80.0, 60.0, 1.0),
    ]));
    let broken = env.path("broken.png");
    fs::write(&broken, b"not an image").expect("broken source");
    let plan = plan_for(&desktop, &[broken.clone(), broken], Effects::default());
    // Both outputs start before either decodes, so the second waits on the
    // first's failed decode.
    let probe = PlanProbe::new(2, &[]);

    let error = observed_with(&env, &plan, &token(), &NoFailures, &probe, 2).expect_err("decode");

    assert!(
        matches!(error, PrepareError::Process(ProcessError::Decode { .. })),
        "{error:?}"
    );
    let state = probe.state();
    assert_eq!(state.reads.len(), 1);
    assert!(state.decodes.is_empty());
    assert_eq!(state.resident, 0);
    assert!(final_pngs(&env).is_empty());
    assert!(!cache_has_temporary(&env));
}

// ---------------------------------------------------------------------------
// Host memo of validated outputs and source digests
// ---------------------------------------------------------------------------

/// Counts real source reads and full output decodes.
#[derive(Default)]
struct MemoHooks {
    reads: AtomicUsize,
    output_decodes: AtomicUsize,
}

impl MemoHooks {
    fn counts(&self) -> (usize, usize) {
        (
            self.reads.load(Ordering::SeqCst),
            self.output_decodes.load(Ordering::SeqCst),
        )
    }
}

impl PlanObserver for MemoHooks {
    fn source_read(&self, _source: &Path) { self.reads.fetch_add(1, Ordering::SeqCst); }

    fn output_decoded(&self, _output: &Path) { self.output_decodes.fetch_add(1, Ordering::SeqCst); }
}

fn memo_run(
    env: &TestEnv,
    plan: &[PlannedTarget],
    memo: Option<&PrepareMemo>,
    hooks: &MemoHooks,
) -> Result<Prepared, PrepareError> {
    let deps = PreparePlanDeps { paths: env.paths(), memo };
    prepare_plan_observed(
        plan,
        &deps,
        &token(),
        None,
        &NoFailures,
        hooks,
        limits::MAX_CONCURRENT_WORKERS,
    )
}

fn memo_desktop() -> FakeDesktop {
    FakeDesktop::with_snapshot(snapshot(vec![
        entry(1, 64.0, 48.0, 1.0),
        entry(2, 40.0, 30.0, 1.0),
    ]))
}

fn restore_modified(path: &Path, modified: std::time::SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .expect("open")
        .set_modified(modified)
        .expect("restore mtime");
}

/// PF-06-01.
#[test]
fn a_warm_plan_through_the_host_processor_reads_no_source_and_decodes_no_output() {
    let env = TestEnv::new();
    let a = env.png("a.png", solid(32, 24, [200, 10, 10]));
    let b = env.png("b.png", solid(32, 24, [10, 200, 10]));
    let desktop = memo_desktop();
    let plan = plan_for(&desktop, &[a, b], Effects::default());
    let processor = crate::RealProcessor::host();
    let memo = processor.memo().expect("the host processor owns a memo");

    let cold = crate::Processor::prepare(&processor, &plan, env.paths(), &token()).expect("cold");
    let warm_counts = memo.counts();

    let hooks = MemoHooks::default();
    let warm = memo_run(&env, &plan, Some(memo), &hooks).expect("warm");
    assert_eq!(hooks.counts(), (0, 0), "no source read and no output decode");
    assert_eq!(warm.targets(), cold.targets());
    let counts = memo.counts();
    assert_eq!(counts.source_hits - warm_counts.source_hits, 2);
    assert_eq!(counts.output_hits - warm_counts.output_hits, 2);
    assert_eq!(counts.source_misses, warm_counts.source_misses);
    assert_eq!(counts.output_misses, warm_counts.output_misses);

    // The processor itself takes the same path again.
    let again = crate::Processor::prepare(&processor, &plan, env.paths(), &token()).expect("again");
    assert_eq!(again.targets(), cold.targets());
    assert_eq!(memo.counts().output_hits - counts.output_hits, 2);

    // Without a memo the same warm plan reads and decodes as before.
    let baseline = MemoHooks::default();
    let _ = memo_run(&env, &plan, None, &baseline).expect("plain warm");
    assert_eq!(baseline.counts(), (2, 2));
}

/// PF-06-02.
#[test]
fn a_same_length_corrupt_output_with_its_mtime_restored_is_rebuilt() {
    let env = TestEnv::new();
    let source = env.png("a.png", solid(32, 24, [30, 40, 50]));
    let desktop = memo_desktop();
    let plan = plan_for(&desktop, &[source.clone(), source], Effects::default());
    let memo = PrepareMemo::new();
    let first = memo_run(&env, &plan, Some(&memo), &MemoHooks::default()).expect("cold");
    let image = first.targets()[0].image.clone();
    drop(first);
    let warm = MemoHooks::default();
    let _ = memo_run(&env, &plan, Some(&memo), &warm).expect("warm");
    assert_eq!(warm.counts(), (0, 0));

    let metadata = fs::metadata(&image).expect("metadata");
    let modified = metadata.modified().expect("mtime");
    let length = usize::try_from(metadata.len()).expect("length");
    fs::write(&image, vec![0xA5; length]).expect("corrupt");
    restore_modified(&image, modified);
    let replaced = fs::metadata(&image).expect("metadata");
    assert_eq!(replaced.len(), metadata.len());
    assert_eq!(replaced.modified().expect("mtime"), modified);

    let hooks = MemoHooks::default();
    let rebuilt = memo_run(&env, &plan, Some(&memo), &hooks).expect("rebuild");
    assert_eq!(rebuilt.targets()[0].image, image);
    assert_eq!(hooks.counts(), (1, 1), "decoded, found corrupt, read the source");
    assert_eq!(read_rgb8(&image).get_pixel(0, 0), &image::Rgb([30, 40, 50]));
    assert_eq!(read_rgb8(&image).dimensions(), (64, 48));
}

fn refused_with_and_without_memo(replace: impl Fn(&Path)) {
    let env = TestEnv::new();
    let source = env.png("a.png", solid(32, 24, [30, 40, 50]));
    let desktop = memo_desktop();
    let plan = plan_for(&desktop, &[source.clone(), source], Effects::default());
    let memo = PrepareMemo::new();
    let first = memo_run(&env, &plan, Some(&memo), &MemoHooks::default()).expect("cold");
    let image = first.targets()[0].image.clone();
    drop(first);
    let _ = memo_run(&env, &plan, Some(&memo), &MemoHooks::default()).expect("warm");
    replace(&image);

    let with = memo_run(&env, &plan, Some(&memo), &MemoHooks::default()).expect_err("memo");
    let without = memo_run(&env, &plan, None, &MemoHooks::default()).expect_err("no memo");
    assert!(
        matches!(
            with,
            PrepareError::Process(ProcessError::Cache(CacheError::UnsafeEntry { .. }))
        ),
        "{with:?}"
    );
    assert_eq!(with.to_string(), without.to_string());
}

/// PF-06-03: a link.
#[test]
fn a_memoized_output_replaced_by_a_link_is_refused_as_without_a_memo() {
    refused_with_and_without_memo(|image| {
        let target = image.with_extension("target");
        fs::rename(image, &target).expect("move");
        std::os::unix::fs::symlink(&target, image).expect("symlink");
    });
}

/// PF-06-03: a directory.
#[test]
fn a_memoized_output_replaced_by_a_directory_is_refused_as_without_a_memo() {
    refused_with_and_without_memo(|image| {
        fs::remove_file(image).expect("remove");
        fs::create_dir(image).expect("directory");
    });
}

/// PF-06-04.
#[test]
fn a_memoized_source_rewritten_with_the_same_length_and_mtime_gets_a_new_output() {
    let env = TestEnv::new();
    let source = env.path("a.png");
    write_png_bytes(&source, &solid(32, 24, [255, 0, 0]));
    let desktop = memo_desktop();
    let plan = plan_for(&desktop, &[source.clone(), source.clone()], Effects::default());
    let memo = PrepareMemo::new();
    let first = memo_run(&env, &plan, Some(&memo), &MemoHooks::default()).expect("cold");
    let first_images: Vec<PathBuf> = first.targets().iter().map(|t| t.image.clone()).collect();
    drop(first);

    let original = fs::metadata(&source).expect("metadata");
    write_png_bytes(&source, &solid(32, 24, [0, 0, 255]));
    restore_modified(&source, original.modified().expect("mtime"));
    let replaced = fs::metadata(&source).expect("metadata");
    assert_eq!(replaced.len(), original.len());
    assert_eq!(
        replaced.modified().expect("mtime"),
        original.modified().expect("mtime")
    );

    let hooks = MemoHooks::default();
    let second = memo_run(&env, &plan, Some(&memo), &hooks).expect("second");
    assert_eq!(hooks.counts().0, 1, "the changed source is read again");
    for (target, old) in second.targets().iter().zip(&first_images) {
        assert_ne!(&target.image, old);
        assert_eq!(
            read_rgb8(&target.image).get_pixel(0, 0),
            &image::Rgb([0, 0, 255])
        );
    }
}

/// PF-06-05.
#[test]
fn a_memoized_source_that_was_removed_fails_with_the_same_error() {
    let env = TestEnv::new();
    let source = env.png("a.png", solid(32, 24, [30, 40, 50]));
    let desktop = memo_desktop();
    let plan = plan_for(&desktop, &[source.clone(), source.clone()], Effects::default());
    let memo = PrepareMemo::new();
    let _ = memo_run(&env, &plan, Some(&memo), &MemoHooks::default()).expect("cold");
    fs::remove_file(&source).expect("remove");

    let with = memo_run(&env, &plan, Some(&memo), &MemoHooks::default()).expect_err("memo");
    let without = memo_run(&env, &plan, None, &MemoHooks::default()).expect_err("no memo");
    assert!(
        matches!(
            &with,
            PrepareError::Process(ProcessError::Io { action: "resolve source", error, .. })
                if error.kind() == io::ErrorKind::NotFound
        ),
        "{with:?}"
    );
    assert_eq!(with.to_string(), without.to_string());
}

/// Manual warm-path evidence for the host memo in `docs/features/wallpapers.md`.
///
/// Not part of the automated gate. Prepares a warm two-display plan (one
/// 6000 × 4000 JPEG at 2560 × 1440 and 1920 × 1200, `radius = 24`,
/// `blur = 12`) repeatedly without and with a memo, interleaved, and prints
/// both medians. Run in release with
/// `cargo test -p stache-wallpapers --release measure_warm_plan_with_and_without_memo -- --ignored --nocapture --test-threads=1`.
#[test]
#[ignore = "manual warm-path measurement"]
fn measure_warm_plan_with_and_without_memo() {
    let env = TestEnv::new();
    let source = env.path("large.jpg");
    write_jpeg(&source, &coordinates(6000, 4000));
    let displays = FakeDesktop::with_snapshot(snapshot(vec![
        entry(1, 2560.0, 1440.0, 1.0),
        entry(2, 1920.0, 1200.0, 1.0),
    ]));
    let plan = plan_for(&displays, &[source.clone(), source], Effects {
        radius: 24,
        blur: 12,
    });
    let memo = PrepareMemo::new();
    let deps = PreparePlanDeps {
        paths: env.paths(),
        memo: Some(&memo),
    };
    // Cold run fills the cache; one memo run records both files.
    drop(prepare_plan(&plan, &plan_deps(&env), &token()).expect("cold"));
    drop(prepare_plan_with(&plan, &deps, &token(), None, &NoFailures).expect("record"));

    let (mut without, mut with) = (Vec::new(), Vec::new());
    for _ in 0..15 {
        let started = Instant::now();
        drop(prepare_plan(&plan, &plan_deps(&env), &token()).expect("without"));
        without.push(started.elapsed());
        let started = Instant::now();
        drop(prepare_plan_with(&plan, &deps, &token(), None, &NoFailures).expect("with"));
        with.push(started.elapsed());
    }
    without.sort_unstable();
    with.sort_unstable();
    println!(
        "warm two-display plan, 6000x4000 JPEG -> 2560x1440 and 1920x1200, radius=24 blur=12: \
         without memo median {:?}, with memo median {:?} (15 runs each; counts {:?})",
        without[without.len() / 2],
        with[with.len() / 2],
        memo.counts()
    );
}
