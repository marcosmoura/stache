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

fn plan_deps(env: &TestEnv) -> PreparePlanDeps<'_> { PreparePlanDeps { paths: env.paths() } }

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
