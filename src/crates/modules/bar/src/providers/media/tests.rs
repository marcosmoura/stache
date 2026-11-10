//! Media provider tests over fakes: a scripted child, a virtual clock and a
//! temporary cache directory. Nothing here spawns the adapter or perl, kills a
//! process by name or touches the real user cache.

use std::collections::VecDeque;
use std::io::{self, Cursor};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde_json::json;
use stache_sdk::CancellationToken;
use stache_sdk::commands::{
    ChildProcess, Clock, CommandEnvironment, CommandExecutor, ProcessFactory, SpawnSpec,
};

use super::artwork::{
    self, ArtworkError, ArtworkLookup, ArtworkStore, MAX_ENCODED_BYTES, MAX_ENTRIES, is_valid_id,
};
use super::parse::{FIELD_CHARS, Parsed, parse_line};
use super::*;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ------------------------------------------------------------------ images

fn two_color(width: u32, height: u32, vertical_split: bool) -> RgbaImage {
    RgbaImage::from_fn(width, height, |x, y| {
        let first = if vertical_split {
            x < width / 2
        } else {
            y < height / 2
        };
        if first {
            Rgba([255, 0, 0, 255])
        } else {
            Rgba([0, 0, 255, 255])
        }
    })
}

fn encode(image: &RgbaImage, format: ImageFormat) -> Vec<u8> {
    let mut out = Vec::new();
    let dynamic = if format == ImageFormat::Jpeg {
        DynamicImage::ImageRgb8(DynamicImage::ImageRgba8(image.clone()).to_rgb8())
    } else {
        DynamicImage::ImageRgba8(image.clone())
    };
    dynamic.write_to(&mut Cursor::new(&mut out), format).expect("encode");
    out
}

fn b64(bytes: &[u8]) -> String { STANDARD.encode(bytes) }

fn decode_png(png: &[u8]) -> RgbaImage {
    image::load_from_memory_with_format(png, ImageFormat::Png)
        .expect("png")
        .to_rgba8()
}

fn near(pixel: Rgba<u8>, expected: [u8; 3], tolerance: i16) -> bool {
    (0..3).all(|i| (i16::from(pixel[i]) - i16::from(expected[i])).abs() <= tolerance)
}

const RED: [u8; 3] = [255, 0, 0];
const BLUE: [u8; 3] = [0, 0, 255];

// ------------------------------------------------------------------ parser

fn data(payload: &serde_json::Value) -> String {
    json!({"type": "data", "diff": false, "payload": payload}).to_string()
}

fn state(parsed: Parsed) -> (MediaReading, Option<parse::ArtworkInput>) {
    match parsed {
        Parsed::State { reading, artwork } => (reading, artwork),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_empty_payload_is_the_absent_state() {
    assert_eq!(parse_line(&data(&json!({}))), Parsed::Absent);
}

#[test]
fn a_payload_without_a_title_is_absent_like_the_legacy_hidden_label() {
    assert_eq!(
        parse_line(&data(
            &json!({"artist": "A", "bundleIdentifier": "com.spotify.client"})
        )),
        Parsed::Absent
    );
    assert_eq!(parse_line(&data(&json!({"title": "   "}))), Parsed::Absent);
    assert_eq!(parse_line(&data(&json!({"title": 5}))), Parsed::Absent);
}

#[test]
fn a_full_payload_maps_the_fields_and_ignores_unknown_keys() {
    let (reading, artwork) = state(parse_line(&data(&json!({
        "title": " Song ", "artist": "Band", "album": "LP", "mediaType": "MRMediaRemoteMediaTypeMusic",
        "bundleIdentifier": "com.spotify.client", "playing": true, "playbackRate": 1.0,
        "elapsedTime": 12.5, "totalTrackCount": 9, "somethingNew": {"nested": [1, 2]},
    }))));
    assert_eq!(reading, MediaReading {
        title: "Song".to_owned(),
        artist: "Band".to_owned(),
        album: Some("LP".to_owned()),
        bundle_identifier: "com.spotify.client".to_owned(),
        media_type: Some("MRMediaRemoteMediaTypeMusic".to_owned()),
        playing: true,
        artwork_id: None,
    });
    assert_eq!(artwork, None);
}

#[test]
fn wrong_typed_optional_fields_are_missing_not_errors() {
    let (reading, _) = state(parse_line(&data(&json!({
        "title": "T", "artist": 7, "album": [], "bundleIdentifier": null, "playing": "yes",
        "mediaType": {"a": 1},
    }))));
    assert_eq!(reading.artist, "");
    assert_eq!(reading.album, None);
    assert_eq!(reading.bundle_identifier, "");
    assert_eq!(reading.media_type, None);
    assert!(!reading.playing, "a non-boolean playing is not playing");
}

#[test]
fn text_fields_are_capped_in_characters() {
    let long = "\u{e9}".repeat(FIELD_CHARS * 3);
    let (reading, _) = state(parse_line(&data(
        &json!({"title": long, "artist": long, "album": long, "bundleIdentifier": "b".repeat(1000)}),
    )));
    assert_eq!(reading.title.chars().count(), FIELD_CHARS);
    assert_eq!(reading.artist.chars().count(), FIELD_CHARS);
    assert_eq!(reading.album.expect("album").chars().count(), FIELD_CHARS);
    assert_eq!(
        reading.bundle_identifier.chars().count(),
        parse::IDENTIFIER_CHARS
    );
}

#[test]
fn an_invalid_wrapper_or_type_is_ignored_not_absent() {
    for line in [
        "",
        "   ",
        "not json",
        "[1,2]",
        "\"payload\"",
        r#"{"payload":{"title":"T"}}"#,
        r#"{"type":"error","payload":{"title":"T"}}"#,
        r#"{"type":5,"payload":{"title":"T"}}"#,
        r#"{"type":"data"}"#,
        r#"{"type":"data","payload":[1]}"#,
        r#"{"type":"data","payload":"x"}"#,
        r#"{"type":"data","payload":null}"#,
        "{\"type\":\"data\",\"payload\":{\"title\":\"T\"",
    ] {
        assert_eq!(parse_line(line), Parsed::Ignore, "{line:?}");
    }
}

#[test]
fn deeply_nested_input_is_rejected_by_the_recursion_limit() {
    let nest = "[".repeat(5000) + &"]".repeat(5000);
    let line = format!(r#"{{"type":"data","payload":{{"title":"T","x":{nest}}}}}"#);
    assert_eq!(parse_line(&line), Parsed::Ignore);
}

#[test]
fn artwork_needs_both_data_and_mime() {
    let (_, art) = state(parse_line(&data(
        &json!({"title": "T", "artworkData": "QUJD", "artworkMimeType": "image/png"}),
    )));
    assert_eq!(art.expect("artwork").mime, "image/png");
    for payload in [
        json!({"title": "T", "artworkData": "QUJD"}),
        json!({"title": "T", "artworkMimeType": "image/png"}),
        json!({"title": "T", "artworkData": 5, "artworkMimeType": "image/png"}),
        json!({"title": "T", "artworkData": "", "artworkMimeType": "image/png"}),
    ] {
        let (_, art) = state(parse_line(&data(&payload)));
        assert_eq!(art, None, "{payload}");
    }
}

// ----------------------------------------------------------------- artwork

#[test]
fn every_supported_mime_decodes_to_a_square_png_with_the_center_crop() {
    let cases = [
        (ImageFormat::Png, "image/png", 0),
        (ImageFormat::Png, "image/x-png", 0),
        (ImageFormat::Jpeg, "image/jpeg", 40),
        (ImageFormat::Jpeg, "IMAGE/JPG", 40),
        (ImageFormat::WebP, "image/webp", 2),
        (ImageFormat::Gif, "image/gif", 2),
        (ImageFormat::Bmp, "image/bmp", 0),
        (ImageFormat::Bmp, "image/x-ms-bmp", 0),
    ];
    for (format, mime, tolerance) in cases {
        // 200x100, red left and blue right: the 100x100 center crop keeps half
        // of each.
        let source = encode(&two_color(200, 100, true), format);
        let png = artwork::process(&b64(&source), mime)
            .unwrap_or_else(|error| panic!("{mime}: {error:?}"));
        let out = decode_png(&png);
        assert_eq!(out.dimensions(), (128, 128), "{mime}");
        assert!(
            near(*out.get_pixel(12, 64), RED, tolerance),
            "{mime} left {:?}",
            out.get_pixel(12, 64)
        );
        assert!(
            near(*out.get_pixel(116, 64), BLUE, tolerance),
            "{mime} right {:?}",
            out.get_pixel(116, 64)
        );
    }
}

#[test]
fn a_portrait_image_is_cropped_from_the_middle_rows() {
    // 100x300: red top third... split at 150, so the 100x100 crop (rows 100..200)
    // is half red, half blue.
    let png = artwork::process(
        &b64(&encode(&two_color(100, 300, false), ImageFormat::Png)),
        "image/png",
    )
    .expect("png");
    let out = decode_png(&png);
    assert!(near(*out.get_pixel(64, 12), RED, 0));
    assert!(near(*out.get_pixel(64, 116), BLUE, 0));
}

#[test]
fn a_solid_square_resizes_to_exactly_the_same_pixels() {
    let solid = RgbaImage::from_pixel(300, 300, Rgba([10, 200, 30, 255]));
    let png = artwork::process(&b64(&encode(&solid, ImageFormat::Png)), "image/png").expect("png");
    let out = decode_png(&png);
    assert!(out.pixels().all(|pixel| *pixel == Rgba([10, 200, 30, 255])));
}

#[test]
fn the_same_artwork_gives_the_same_bytes_and_a_stable_content_id() {
    let text = b64(&encode(&two_color(64, 64, true), ImageFormat::Png));
    let first = artwork::process(&text, "image/png").expect("png");
    let second = artwork::process(&text, "image/png").expect("png");
    assert_eq!(first, second);
    let id = artwork::artwork_id(&first);
    assert_eq!(id, artwork::artwork_id(&second));
    assert!(is_valid_id(&id), "{id}");
    let other = artwork::process(
        &b64(&encode(&two_color(64, 64, false), ImageFormat::Png)),
        "image/png",
    )
    .expect("png");
    assert_ne!(id, artwork::artwork_id(&other));
    // SHA-256 of the empty input pins the hash function and the hex form.
    assert_eq!(
        artwork::artwork_id(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn placeholders_bad_mimes_and_bad_base64_are_rejected() {
    let good = b64(&encode(&two_color(8, 8, true), ImageFormat::Png));
    assert_eq!(
        artwork::process("<html>no</html>", "image/png"),
        Err(ArtworkError::Placeholder)
    );
    assert_eq!(
        artwork::process(&good, "image/tiff"),
        Err(ArtworkError::UnsupportedMime)
    );
    assert_eq!(artwork::process(&good, ""), Err(ArtworkError::UnsupportedMime));
    assert_eq!(
        artwork::process("!!!not base64!!!", "image/png"),
        Err(ArtworkError::BadBase64)
    );
    assert_eq!(
        artwork::process(&good[..good.len() - 3], "image/png"),
        Err(ArtworkError::BadBase64)
    );
    // Valid base64 that is not an image of the declared type.
    assert_eq!(
        artwork::process(&b64(b"hello world, not an image"), "image/png"),
        Err(ArtworkError::Dimensions)
    );
    // A JPEG declared as PNG is refused by the header check.
    let jpeg = b64(&encode(&two_color(8, 8, true), ImageFormat::Jpeg));
    assert_eq!(
        artwork::process(&jpeg, "image/png"),
        Err(ArtworkError::Dimensions)
    );
}

#[test]
fn the_encoded_cap_is_enforced_before_any_decoding() {
    let huge = "A".repeat(MAX_ENCODED_BYTES + 1);
    assert_eq!(artwork::process(&huge, "image/png"), Err(ArtworkError::TooLarge));
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// A tiny real PNG whose IHDR claims `width` x `height`: the pixel data is not
/// that large, so a decoder that trusted the header would fail or allocate.
fn png_claiming(width: u32, height: u32) -> Vec<u8> {
    let mut png = encode(&two_color(2, 2, true), ImageFormat::Png);
    png[16..20].copy_from_slice(&width.to_be_bytes());
    png[20..24].copy_from_slice(&height.to_be_bytes());
    let crc = crc32(&png[12..29]);
    png[29..33].copy_from_slice(&crc.to_be_bytes());
    png
}

#[test]
fn dimension_and_pixel_caps_are_rejected_from_the_header_before_decoding() {
    for (width, height) in [(8193, 1), (1, 8193), (8192, 2049), (0, 5)] {
        assert_eq!(
            artwork::process(&b64(&png_claiming(width, height)), "image/png"),
            Err(ArtworkError::Dimensions),
            "{width}x{height}"
        );
    }
    // At the caps the header passes (the pixel decode then fails on the fake
    // data, which is a decode error, not a dimension error).
    assert_eq!(
        artwork::process(&b64(&png_claiming(8192, 2048)), "image/png"),
        Err(ArtworkError::Decode)
    );
}

// ------------------------------------------------------------------- store

fn png_of(index: u32) -> Vec<u8> {
    let color = Rgba([
        u8::try_from(index % 256).unwrap_or(0),
        u8::try_from(index / 256).unwrap_or(0),
        7,
        255,
    ]);
    encode(&RgbaImage::from_pixel(4, 4, color), ImageFormat::Png)
}

fn txt_files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("dir")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_stored_artwork_round_trips_as_base64_text_with_private_modes() {
    let temp = tempfile::tempdir().expect("temp");
    let dir = temp.path().join("cache").join("media-artwork");
    let store = ArtworkStore::new(Some(dir.clone()));
    let png = png_of(1);
    let (id, failure) = store.put(&png);
    assert!(failure.is_none(), "{failure:?}");
    assert_eq!(store.get(&id), ArtworkLookup::Found(b64(&png)));
    let file = dir.join(format!("{id}.txt"));
    assert_eq!(
        std::fs::read_to_string(&file).expect("txt"),
        b64(&png),
        "the legacy text format"
    );
    assert_eq!(
        std::fs::metadata(&file).expect("meta").permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(&dir).expect("meta").permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        txt_files(&dir),
        vec![format!("{id}.txt")],
        "no temp file is left behind"
    );
    // A fresh store (a restart) reads it back from disk.
    assert_eq!(
        ArtworkStore::new(Some(dir)).get(&id),
        ArtworkLookup::Found(b64(&png))
    );
}

#[test]
fn a_memory_only_store_keeps_entries_without_a_directory() {
    let store = ArtworkStore::new(None);
    let (id, failure) = store.put(&png_of(2));
    assert!(failure.is_none());
    assert!(matches!(store.get(&id), ArtworkLookup::Found(_)));
    assert_eq!(store.get(&"0".repeat(64)), ArtworkLookup::Missing);
}

#[test]
fn ids_are_validated_before_the_filesystem_is_touched() {
    let temp = tempfile::tempdir().expect("temp");
    let dir = temp.path().join("cache");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(temp.path().join("secret.txt"), b64(&png_of(3))).expect("secret");
    let store = ArtworkStore::new(Some(dir));
    let valid = "a".repeat(64);
    for id in [
        "",
        "../secret",
        "..%2fsecret",
        "../../etc/passwd",
        "/etc/passwd",
        &"A".repeat(64),
        &"a".repeat(63),
        &"a".repeat(65),
        &format!("{}/", "a".repeat(63)),
        &format!("{}\0", "a".repeat(63)),
        &"g".repeat(64),
        &format!("{}.txt", "a".repeat(60)),
    ] {
        assert_eq!(store.get(id), ArtworkLookup::Missing, "{id:?}");
    }
    assert!(is_valid_id(&valid));
    assert!(!is_valid_id(&valid.to_uppercase()));
}

#[test]
fn a_symlinked_entry_is_neither_read_nor_followed_for_a_write() {
    let temp = tempfile::tempdir().expect("temp");
    let dir = temp.path().join("cache");
    std::fs::create_dir_all(&dir).expect("dir");
    let outside = temp.path().join("outside.txt");
    let png = png_of(4);
    let id = artwork::artwork_id(&png);
    std::fs::write(&outside, b64(&png)).expect("outside");
    symlink(&outside, dir.join(format!("{id}.txt"))).expect("symlink");
    let store = ArtworkStore::new(Some(dir.clone()));
    assert_eq!(store.get(&id), ArtworkLookup::Missing, "a symlink is never read");

    // Writing replaces the link itself; the outside file is untouched.
    std::fs::write(&outside, "original").expect("outside");
    let (_, failure) = store.put(&png);
    assert!(failure.is_none(), "{failure:?}");
    assert_eq!(std::fs::read_to_string(&outside).expect("outside"), "original");
    let entry = dir.join(format!("{id}.txt"));
    assert!(std::fs::symlink_metadata(&entry).expect("meta").file_type().is_file());
    assert_eq!(
        ArtworkStore::new(Some(dir)).get(&id),
        ArtworkLookup::Found(b64(&png))
    );
}

#[test]
fn a_symlinked_cache_directory_is_refused() {
    let temp = tempfile::tempdir().expect("temp");
    let real = temp.path().join("real");
    std::fs::create_dir_all(&real).expect("real");
    let link = temp.path().join("link");
    symlink(&real, &link).expect("symlink");
    let (id, failure) = ArtworkStore::new(Some(link)).put(&png_of(5));
    assert!(failure.is_some(), "the write is refused");
    assert!(
        txt_files(&real).is_empty(),
        "nothing was written through the link"
    );
    assert!(is_valid_id(&id));
}

#[test]
fn corrupt_oversized_and_mismatched_entries_are_missing() {
    let temp = tempfile::tempdir().expect("temp");
    let dir = temp.path().to_path_buf();
    let store = ArtworkStore::new(Some(dir.clone()));
    let png = png_of(6);
    let id = artwork::artwork_id(&png);
    std::fs::write(dir.join(format!("{id}.txt")), "@@@ not base64 @@@").expect("corrupt");
    assert_eq!(store.get(&id), ArtworkLookup::Missing);
    // A different (valid) image under this id fails the content check.
    std::fs::write(dir.join(format!("{id}.txt")), b64(&png_of(7))).expect("other");
    assert_eq!(store.get(&id), ArtworkLookup::Missing);
    // Over the read cap.
    let big = "A".repeat(usize::try_from(artwork::MAX_CACHE_FILE_BYTES).expect("size") + 1);
    std::fs::write(dir.join(format!("{id}.txt")), big).expect("big");
    assert_eq!(store.get(&id), ArtworkLookup::Missing);
    // A directory with that name.
    std::fs::remove_file(dir.join(format!("{id}.txt"))).expect("rm");
    std::fs::create_dir(dir.join(format!("{id}.txt"))).expect("dir");
    assert_eq!(store.get(&id), ArtworkLookup::Missing);
}

#[test]
fn the_directory_is_pruned_to_the_entry_bound_keeping_the_newest() {
    let temp = tempfile::tempdir().expect("temp");
    let dir = temp.path().join("c");
    let store = ArtworkStore::new(Some(dir.clone()));
    let mut last = String::new();
    for index in 0..(u32::try_from(MAX_ENTRIES).expect("count") + 6) {
        last = store.put(&png_of(index)).0;
    }
    let names = txt_files(&dir);
    assert_eq!(names.len(), MAX_ENTRIES, "{names:?}");
    assert!(
        names.contains(&format!("{last}.txt")),
        "the newest entry survives"
    );
}

#[test]
fn the_memory_layer_is_bounded_but_the_directory_still_answers() {
    let temp = tempfile::tempdir().expect("temp");
    let store = ArtworkStore::new(Some(temp.path().to_path_buf()));
    let ids: Vec<String> = (0..10).map(|index| store.put(&png_of(index)).0).collect();
    assert!(store.memory_len() <= 4);
    assert!(matches!(store.get(&ids[0]), ArtworkLookup::Found(_)));
}

// --------------------------------------------------------------- publisher

fn reading(title: &str) -> MediaReading {
    MediaReading {
        title: title.to_owned(),
        artist: "A".to_owned(),
        album: None,
        bundle_identifier: "com.spotify.client".to_owned(),
        media_type: None,
        playing: true,
        artwork_id: None,
    }
}

fn recorder() -> (PublishMedia, Arc<Mutex<Vec<MediaUpdate>>>) {
    let updates = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&updates);
    (Arc::new(move |update| lock(&sink).push(update)), updates)
}

#[test]
fn the_publisher_coalesces_duplicates_and_replaces_the_complete_state() {
    let (publish, updates) = recorder();
    let publisher = Publisher::new(publish);
    publisher.set_state(reading("One"), ArtPlan::None);
    publisher.set_state(reading("One"), ArtPlan::None);
    publisher.set_state(reading("Two"), ArtPlan::None);
    publisher.set_absent();
    publisher.set_absent();
    let shown: Vec<Option<String>> = lock(&updates)
        .iter()
        .map(|u| u.reading.as_ref().map(|r| r.title.clone()))
        .collect();
    assert_eq!(shown, vec![Some("One".to_owned()), Some("Two".to_owned()), None]);
}

#[test]
fn a_late_artwork_only_attaches_to_the_state_still_waiting_for_it() {
    let (publish, updates) = recorder();
    let publisher = Publisher::new(publish);
    publisher.set_state(reading("One"), ArtPlan::Pending(1));
    publisher.set_state(reading("Two"), ArtPlan::Pending(2));
    assert!(
        !publisher.attach(1, &"a".repeat(64)),
        "the superseded decode is dropped"
    );
    assert!(publisher.attach(2, &"b".repeat(64)));
    assert!(
        !publisher.attach(2, &"c".repeat(64)),
        "a state has its artwork once"
    );
    publisher.set_state(reading("Two"), ArtPlan::None);
    assert!(
        !publisher.attach(2, &"b".repeat(64)),
        "a state without artwork expects none"
    );
    let last = lock(&updates).last().cloned().expect("update");
    assert_eq!(last.reading.expect("reading").artwork_id, None);
}

#[test]
fn nothing_is_published_after_close() {
    let (publish, updates) = recorder();
    let publisher = Publisher::new(publish);
    publisher.set_state(reading("One"), ArtPlan::Pending(1));
    publisher.close();
    let sent = lock(&updates).len();
    publisher.set_state(reading("Two"), ArtPlan::None);
    publisher.set_absent();
    publisher.hide(MediaStatus::running());
    assert!(!publisher.attach(1, &"a".repeat(64)));
    assert_eq!(lock(&updates).len(), sent);
    assert_eq!(
        lock(&updates).last().expect("last").reading,
        None,
        "close hides first"
    );
}

// ------------------------------------------------------------- fake child

#[derive(Default)]
struct Script {
    stdout: Vec<Vec<u8>>,
    /// `WouldBlock` reads after the chunks and before the end of stream.
    idle_reads: usize,
    stderr: Vec<u8>,
    exit: i32,
    /// Never ends until signalled.
    stalls: bool,
    term_exits: bool,
    kill_exits: bool,
}

#[derive(Default)]
struct ChildState {
    stdout: VecDeque<Vec<u8>>,
    idle_reads: usize,
    stderr: VecDeque<Vec<u8>>,
    exit: i32,
    stalls: bool,
    term_exits: bool,
    kill_exits: bool,
    ended: Option<i32>,
}

struct FakeChild {
    state: Arc<Mutex<ChildState>>,
    log: Arc<Mutex<Vec<String>>>,
}

fn take(queue: &mut VecDeque<Vec<u8>>, buffer: &mut [u8]) -> Option<usize> {
    let front = queue.front_mut()?;
    let count = front.len().min(buffer.len());
    buffer[..count].copy_from_slice(&front[..count]);
    front.drain(..count);
    if front.is_empty() {
        queue.pop_front();
    }
    Some(count)
}

impl ChildProcess for FakeChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let mut state = lock(&self.state);
        if state.ended.is_none()
            && !state.stalls
            && state.stdout.is_empty()
            && state.idle_reads == 0
        {
            state.ended = Some(state.exit << 8);
        }
        Ok(state.ended.map(ExitStatus::from_raw))
    }

    fn kill(&mut self) -> io::Result<()> {
        lock(&self.log).push("kill".to_owned());
        let mut state = lock(&self.state);
        if state.kill_exits {
            state.ended = Some(9);
        }
        Ok(())
    }

    fn terminate(&mut self) -> io::Result<()> {
        lock(&self.log).push("term".to_owned());
        let mut state = lock(&self.state);
        if state.term_exits {
            state.ended = Some(15);
        }
        Ok(())
    }

    fn read_stdout(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut state = lock(&self.state);
        if state.ended.is_some_and(|code| code == 9 || code == 15) {
            return Ok(0);
        }
        if let Some(count) = take(&mut state.stdout, buffer) {
            return Ok(count);
        }
        if state.idle_reads > 0 {
            state.idle_reads -= 1;
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if state.stalls {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        Ok(0)
    }

    fn read_stderr(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut state = lock(&self.state);
        take(&mut state.stderr, buffer).ok_or_else(|| io::ErrorKind::WouldBlock.into())
    }
}

struct Factory {
    scripts: Mutex<VecDeque<Script>>,
    children: Mutex<Vec<Arc<Mutex<ChildState>>>>,
    spawns: AtomicUsize,
    log: Arc<Mutex<Vec<String>>>,
    specs: Mutex<Vec<SpawnSpec>>,
}

impl ProcessFactory for Factory {
    fn spawn(&self, spec: &SpawnSpec) -> io::Result<Box<dyn ChildProcess>> {
        self.spawns.fetch_add(1, Ordering::SeqCst);
        lock(&self.specs).push(spec.clone());
        lock(&self.log).push("spawn".to_owned());
        let script = lock(&self.scripts)
            .pop_front()
            .unwrap_or_else(|| Script { exit: 1, ..Script::default() });
        let state = Arc::new(Mutex::new(ChildState {
            stdout: script.stdout.into(),
            idle_reads: script.idle_reads,
            stderr: VecDeque::from([script.stderr]),
            exit: script.exit,
            stalls: script.stalls,
            term_exits: script.term_exits,
            kill_exits: script.kill_exits,
            ended: None,
        }));
        lock(&self.children).push(Arc::clone(&state));
        Ok(Box::new(FakeChild {
            state,
            log: Arc::clone(&self.log),
        }))
    }
}

/// Virtual time that also yields a little real time, so a stalled stream does
/// not spin hot. Backoff waits (cancellable, one second or more) are recorded.
#[derive(Default)]
struct TestClock {
    now: Mutex<Duration>,
    backoffs: Mutex<Vec<Duration>>,
    block_backoff: AtomicBool,
    in_backoff: AtomicBool,
}

impl Clock for TestClock {
    fn now(&self) -> Duration { *lock(&self.now) }

    fn wait(&self, cancel: Option<&CancellationToken>, quantum: Duration) -> bool {
        *lock(&self.now) += quantum;
        let Some(cancel) = cancel else {
            std::thread::sleep(Duration::from_micros(200));
            return false;
        };
        if quantum >= Duration::from_secs(1) {
            lock(&self.backoffs).push(quantum);
            if self.block_backoff.load(Ordering::SeqCst) {
                self.in_backoff.store(true, Ordering::SeqCst);
                let cancelled = cancel.wait(Duration::from_secs(30));
                self.in_backoff.store(false, Ordering::SeqCst);
                return cancelled;
            }
        }
        cancel.wait(Duration::from_millis(1))
    }
}

struct Rig {
    parts: MediaParts,
    factory: Arc<Factory>,
    clock: Arc<TestClock>,
    updates: Arc<Mutex<Vec<MediaUpdate>>>,
    publish: PublishMedia,
    stop: CancellationToken,
    _temp: tempfile::TempDir,
}

fn paths() -> MediaPaths {
    MediaPaths {
        script: PathBuf::from("/fake/media-control/mediaremote-adapter.pl"),
        framework: PathBuf::from("/fake/media-control/MediaRemoteAdapter.framework"),
        test_client: PathBuf::from("/fake/media-control/MediaRemoteAdapterTestClient"),
    }
}

fn rig(scripts: Vec<Script>) -> Rig {
    let temp = tempfile::tempdir().expect("temp");
    let log = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(Factory {
        scripts: Mutex::new(scripts.into()),
        children: Mutex::new(Vec::new()),
        spawns: AtomicUsize::new(0),
        log: Arc::clone(&log),
        specs: Mutex::new(Vec::new()),
    });
    let clock = Arc::new(TestClock::default());
    let executor = Arc::new(CommandExecutor::with_parts(
        Arc::clone(&factory) as Arc<dyn ProcessFactory>,
        Arc::clone(&clock) as Arc<dyn Clock>,
        CommandEnvironment::default(),
    ));
    let timing = MediaTiming {
        term_grace: Duration::from_millis(100),
        reap_retries: 3,
        reap_retry_wait: Duration::from_millis(10),
        line_cap: 1024 * 1024,
        ..MediaTiming::default()
    };
    let resolver: MediaResolver = Arc::new(|| Ok(paths()));
    let parts = MediaParts {
        executor,
        clock: Arc::clone(&clock) as Arc<dyn Clock>,
        perl: PathBuf::from("/usr/bin/perl"),
        resolver,
        store: Arc::new(ArtworkStore::new(Some(temp.path().join("media-artwork")))),
        timing,
    };
    let updates = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&updates);
    let log_sink = Arc::clone(&log);
    let publish: PublishMedia = Arc::new(move |update: MediaUpdate| {
        lock(&log_sink).push(format!(
            "publish:{}",
            update.reading.as_ref().map_or("none", |_| "media")
        ));
        lock(&sink).push(update);
    });
    Rig {
        parts,
        factory,
        clock,
        updates,
        publish,
        stop: CancellationToken::new(),
        _temp: temp,
    }
}

impl Rig {
    fn start(&self) -> JoinHandle<()> {
        let parts = self.parts.clone();
        let stop = self.stop.clone();
        let publish = Arc::clone(&self.publish);
        std::thread::spawn(move || owner::run(&parts, &stop, publish))
    }

    fn until(&self, what: &str, condition: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !condition(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn last(&self) -> Option<MediaUpdate> { lock(&self.updates).last().cloned() }

    fn log(&self) -> Vec<String> { lock(&self.factory.log).clone() }

    fn finish(&self, handle: JoinHandle<()>) {
        self.stop.cancel();
        handle.join().expect("worker");
    }
}

fn line(payload: &serde_json::Value) -> Vec<u8> {
    let mut bytes = data(payload).into_bytes();
    bytes.push(b'\n');
    bytes
}

fn spotify(title: &str) -> serde_json::Value {
    json!({"title": title, "artist": "Band", "bundleIdentifier": "com.spotify.client", "playing": true})
}

fn with_art(mut payload: serde_json::Value, image: &[u8], mime: &str) -> serde_json::Value {
    payload["artworkData"] = json!(b64(image));
    payload["artworkMimeType"] = json!(mime);
    payload
}

fn stalling(stdout: Vec<Vec<u8>>) -> Script {
    Script {
        stdout,
        stalls: true,
        term_exits: true,
        kill_exits: true,
        ..Script::default()
    }
}

// ------------------------------------------------------------------- owner

#[test]
fn the_exact_adapter_command_uses_absolute_paths_and_no_diff() {
    let rig = rig(vec![stalling(vec![line(&json!({}))])]);
    let handle = rig.start();
    rig.until("spawn", |rig| rig.factory.spawns.load(Ordering::SeqCst) == 1);
    rig.finish(handle);
    let spec = lock(&rig.factory.specs)[0].clone();
    assert_eq!(spec.program, PathBuf::from("/usr/bin/perl"));
    assert_eq!(spec.arguments, vec![
        "/fake/media-control/mediaremote-adapter.pl",
        "/fake/media-control/MediaRemoteAdapter.framework",
        "/fake/media-control/MediaRemoteAdapterTestClient",
        "stream",
        "--no-diff",
    ]);
}

#[test]
fn a_track_with_artwork_publishes_the_state_then_the_artwork_id() {
    let image = encode(&two_color(200, 100, true), ImageFormat::Png);
    let rig = rig(vec![stalling(vec![
        line(&json!({})),
        line(&spotify("Song")),
        line(&with_art(spotify("Song"), &image, "image/png")),
    ])]);
    let handle = rig.start();
    rig.until("artwork", |rig| {
        rig.last().and_then(|u| u.reading).is_some_and(|r| r.artwork_id.is_some())
    });
    let published = rig.last().expect("update");
    let reading = published.reading.expect("reading");
    assert_eq!(published.status.state, MediaRunState::Running);
    assert_eq!(reading.title, "Song");
    assert_eq!(reading.artist, "Band");
    assert!(reading.playing);
    let id = reading.artwork_id.expect("id");
    let ArtworkLookup::Found(text) = rig.parts.store.get(&id) else {
        panic!("stored")
    };
    let png = STANDARD.decode(text).expect("base64");
    assert_eq!(decode_png(&png).dimensions(), (128, 128));
    assert_eq!(artwork::artwork_id(&png), id);
    // Order: absent, the track without artwork, then the same track with it.
    let shown: Vec<(Option<String>, bool)> = lock(&rig.updates)
        .iter()
        .map(|u| {
            (
                u.reading.as_ref().map(|r| r.title.clone()),
                u.reading.as_ref().is_some_and(|r| r.artwork_id.is_some()),
            )
        })
        .collect();
    assert_eq!(shown.last(), Some(&(Some("Song".to_owned()), true)));
    assert!(shown.contains(&(Some("Song".to_owned()), false)));
    // The artwork bytes are not in anything that was published.
    let all = format!("{:?}", lock(&rig.updates));
    assert!(!all.contains(&b64(&image)));
    rig.finish(handle);
}

#[test]
fn duplicate_snapshots_are_coalesced() {
    let rig = rig(vec![stalling(vec![
        line(&spotify("Song")),
        line(&spotify("Song")),
        line(&spotify("Song")),
        b"{\"type\":\"error\",\"payload\":{\"title\":\"Other\"}}\n".to_vec(),
        b"garbage\n".to_vec(),
        line(&spotify("Song")),
    ])]);
    let handle = rig.start();
    rig.until("media", |rig| rig.last().is_some_and(|u| u.reading.is_some()));
    std::thread::sleep(Duration::from_millis(50));
    let with_media = lock(&rig.updates).iter().filter(|u| u.reading.is_some()).count();
    assert_eq!(with_media, 1);
    rig.finish(handle);
}

#[test]
fn a_new_snapshot_replaces_the_whole_state_and_an_empty_one_hides_it() {
    let rig = rig(vec![stalling(vec![
        line(&spotify("One")),
        line(&json!({"title": "Two", "playing": false})),
        line(&json!({})),
    ])]);
    let handle = rig.start();
    rig.until("hidden", |rig| {
        let updates = lock(&rig.updates);
        updates.iter().any(|u| u.reading.as_ref().is_some_and(|r| r.title == "Two"))
            && updates.last().is_some_and(|u| u.reading.is_none())
    });
    let two = lock(&rig.updates)
        .iter()
        .find_map(|u| u.reading.clone().filter(|r| r.title == "Two"))
        .expect("two");
    assert_eq!(two.artist, "", "nothing of the previous state survives");
    assert_eq!(two.bundle_identifier, "");
    assert!(!two.playing);
    rig.finish(handle);
}

#[test]
fn utf8_and_lines_split_across_chunks_are_reassembled() {
    let bytes = line(
        &json!({"title": "Caf\u{e9} \u{1f3b5}", "artist": "\u{65e5}\u{672c}", "playing": true}),
    );
    let split = bytes.iter().position(|b| *b == 0xC3).expect("multi-byte") + 1;
    let mut chunks = vec![
        bytes[..split].to_vec(),
        bytes[split..bytes.len() - 5].to_vec(),
    ];
    chunks.push(bytes[bytes.len() - 5..].to_vec());
    let mut second = line(&spotify("Next"));
    let mut both = second.split_off(10);
    both.extend(line(&spotify("Third")));
    chunks.extend([second, both]);
    let rig = rig(vec![stalling(chunks)]);
    let handle = rig.start();
    rig.until("third", |rig| {
        rig.last().and_then(|u| u.reading).is_some_and(|r| r.title == "Third")
    });
    let titles: Vec<String> = lock(&rig.updates)
        .iter()
        .filter_map(|u| u.reading.as_ref().map(|r| r.title.clone()))
        .collect();
    assert_eq!(titles, vec!["Caf\u{e9} \u{1f3b5}", "Next", "Third"]);
    rig.finish(handle);
}

#[test]
fn oversized_unterminated_and_invalid_frames_are_dropped_without_changing_the_state() {
    let mut rig = rig(Vec::new());
    rig.parts.timing.line_cap = 400;
    let mut huge = line(&json!({"title": "x".repeat(2000)}));
    huge.splice(0..0, std::iter::empty());
    lock(&rig.factory.scripts).push_back(stalling(vec![
        line(&spotify("Kept")),
        huge,
        b"\xff\xfe\n".to_vec(),
        line(&spotify("Kept")),
    ]));
    let handle = rig.start();
    rig.until("kept", |rig| rig.last().is_some_and(|u| u.reading.is_some()));
    std::thread::sleep(Duration::from_millis(50));
    let titles: Vec<String> = lock(&rig.updates)
        .iter()
        .filter_map(|u| u.reading.as_ref().map(|r| r.title.clone()))
        .collect();
    assert_eq!(titles, vec!["Kept"]);
    rig.finish(handle);
}

#[test]
fn a_superseded_artwork_never_attaches_to_the_newer_track() {
    let first = encode(&two_color(64, 64, true), ImageFormat::Png);
    let second = encode(&two_color(64, 64, false), ImageFormat::Png);
    let expected = artwork::artwork_id(&artwork::process(&b64(&second), "image/png").expect("png"));
    let rig = rig(vec![stalling(vec![
        line(&with_art(spotify("One"), &first, "image/png")),
        line(&with_art(spotify("Two"), &second, "image/png")),
    ])]);
    let handle = rig.start();
    rig.until("two with art", |rig| {
        rig.last()
            .and_then(|u| u.reading)
            .is_some_and(|r| r.title == "Two" && r.artwork_id.is_some())
    });
    for update in lock(&rig.updates).iter() {
        if let Some(reading) = &update.reading {
            if reading.title == "One" {
                assert_eq!(
                    reading.artwork_id, None,
                    "the older track never gets the newer image"
                );
            }
            if let Some(id) = &reading.artwork_id {
                assert_eq!(reading.title, "Two");
                assert_eq!(*id, expected);
            }
        }
    }
    rig.finish(handle);
}

#[test]
fn a_bad_artwork_leaves_the_state_without_an_id() {
    let rig = rig(vec![stalling(vec![
        line(&with_art(
            spotify("Song"),
            b"definitely not an image",
            "image/png",
        )),
        line(
            &json!({"title": "Placeholder", "artworkData": "<html>", "artworkMimeType": "image/png"}),
        ),
    ])]);
    let handle = rig.start();
    rig.until("placeholder", |rig| {
        rig.last().and_then(|u| u.reading).is_some_and(|r| r.title == "Placeholder")
    });
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        lock(&rig.updates)
            .iter()
            .all(|u| u.reading.as_ref().is_none_or(|r| r.artwork_id.is_none()))
    );
    rig.finish(handle);
}

#[test]
fn a_crash_restarts_with_the_exact_backoff_schedule_and_never_spins() {
    let rig = rig(Vec::new());
    let handle = rig.start();
    rig.until("seven restarts", |rig| lock(&rig.clock.backoffs).len() >= 7);
    rig.finish(handle);
    let secs: Vec<u64> = lock(&rig.clock.backoffs).iter().take(7).map(Duration::as_secs).collect();
    assert_eq!(secs, vec![1, 2, 5, 10, 30, 60, 60]);
    // One spawn per run, and every wait was a backoff, not a hot loop.
    assert!(rig.factory.spawns.load(Ordering::SeqCst) <= lock(&rig.clock.backoffs).len() + 1);
}

#[test]
fn the_status_is_restarting_then_unavailable_from_the_third_failure_with_media_hidden() {
    let image_line = line(&spotify("Song"));
    let rig = rig(vec![Script {
        stdout: vec![image_line],
        exit: 2,
        stderr: b"boom: failed\n".to_vec(),
        ..Script::default()
    }]);
    let handle = rig.start();
    rig.until("unavailable", |rig| {
        rig.last().is_some_and(|u| u.status.state == MediaRunState::Unavailable)
    });
    rig.finish(handle);
    let updates = lock(&rig.updates).clone();
    let states: Vec<MediaRunState> = updates.iter().map(|u| u.status.state).collect();
    let first_restart =
        states.iter().position(|s| *s == MediaRunState::Restarting).expect("restarting");
    let first_unavailable = states
        .iter()
        .position(|s| *s == MediaRunState::Unavailable)
        .expect("unavailable");
    assert!(first_restart < first_unavailable);
    assert!(updates[first_unavailable].reading.is_none(), "Media is hidden");
    let restarting = &updates[first_restart];
    assert!(restarting.reading.is_none());
    assert_eq!(
        restarting.status.reason.as_deref(),
        Some("the media adapter exited with code 2")
    );
    // The first run's stderr is reported once the run is over.
    assert!(updates.iter().any(|u| u.status.stderr_tail.as_deref() == Some("boom: failed")));
    let failures = lock(&rig.clock.backoffs).len();
    assert!(failures >= 2, "{failures}");
}

#[test]
fn the_worker_gives_up_after_the_schedule_and_a_new_provider_resumes() {
    // Every spawn fails at once. Unavailable from failure 3, then 5 more cycles.
    let rig = rig(Vec::new());
    let handle = rig.start();
    handle.join().expect("the worker ends by itself after giving up");
    let secs: Vec<u64> = lock(&rig.clock.backoffs).iter().map(Duration::as_secs).collect();
    assert_eq!(
        secs,
        vec![1, 2, 5, 10, 30, 60, 60],
        "no backoff after the last failure"
    );
    let spawns = rig.factory.spawns.load(Ordering::SeqCst);
    assert_eq!(spawns, 8, "3 failures to unavailable plus 5 more cycles");
    let last = rig.last().expect("a final status");
    assert_eq!(last.status.state, MediaRunState::Unavailable);
    let reason = last.status.reason.as_deref().expect("a give-up reason");
    assert!(reason.starts_with(GAVE_UP_REASON), "{reason}");
    assert!(
        reason.contains("(last failure: "),
        "the last failure is kept: {reason}"
    );
    assert!(last.reading.is_none(), "Media stays hidden");
    // Zero spawns afterwards, however long we wait.
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(rig.factory.spawns.load(Ordering::SeqCst), spawns);

    // A Bar restart recreates the provider: a fresh worker spawns again and
    // a run that streams valid data is running with Media shown.
    *lock(&rig.factory.scripts) = VecDeque::from([stalling(vec![line(&spotify("Song"))])]);
    let handle = rig.start();
    rig.until("running again", |rig| {
        rig.last()
            .is_some_and(|u| u.status.state == MediaRunState::Running && u.reading.is_some())
    });
    assert_eq!(rig.factory.spawns.load(Ordering::SeqCst), spawns + 1);
    rig.finish(handle);
}

#[test]
fn a_long_run_resets_the_failure_count() {
    // Two quick failures, then a run that idles for over a minute of virtual
    // time before it ends: that failure counts as the first again.
    let rig = rig(vec![
        Script { exit: 1, ..Script::default() },
        Script { exit: 1, ..Script::default() },
        Script {
            idle_reads: 400,
            exit: 1,
            ..Script::default()
        },
    ]);
    let handle = rig.start();
    rig.until("four backoffs", |rig| lock(&rig.clock.backoffs).len() >= 4);
    rig.finish(handle);
    let secs: Vec<u64> = lock(&rig.clock.backoffs).iter().take(4).map(Duration::as_secs).collect();
    assert_eq!(secs, vec![1, 2, 1, 2], "the long run reset the count");
}

#[test]
fn cancelling_during_a_backoff_wakes_the_worker_at_once_and_spawns_nothing_more() {
    let rig = rig(Vec::new());
    rig.clock.block_backoff.store(true, Ordering::SeqCst);
    let handle = rig.start();
    rig.until("in backoff", |rig| rig.clock.in_backoff.load(Ordering::SeqCst));
    let spawns = rig.factory.spawns.load(Ordering::SeqCst);
    let started = Instant::now();
    rig.stop.cancel();
    handle.join().expect("worker");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(rig.factory.spawns.load(Ordering::SeqCst), spawns);
}

#[test]
fn a_stop_invalidates_the_media_before_the_child_is_signalled_and_reaps_it() {
    let rig = rig(vec![stalling(vec![line(&spotify("Song"))])]);
    let handle = rig.start();
    rig.until("media", |rig| rig.last().is_some_and(|u| u.reading.is_some()));
    rig.finish(handle);
    let log = rig.log();
    let shown = log.iter().position(|e| e == "publish:media").expect("shown");
    let hidden = log.iter().rposition(|e| e == "publish:none").expect("hidden");
    let term = log.iter().position(|e| e == "term").expect("term");
    assert!(shown < hidden && hidden < term, "{log:?}");
    assert!(
        !log[term..].iter().any(|e| e == "publish:media"),
        "nothing is shown after the stop: {log:?}"
    );
    assert!(rig.parts.executor.sweep().is_empty(), "the child was reaped");
    assert_eq!(rig.last().expect("last").reading, None);
}

#[test]
fn a_child_that_ignores_term_is_killed_and_a_child_that_cannot_be_reaped_stays_owned() {
    // SIGTERM ignored, SIGKILL works.
    let rig_a = rig(vec![Script {
        stalls: true,
        kill_exits: true,
        ..Script::default()
    }]);
    let handle = rig_a.start();
    rig_a.until("spawn", |rig| rig.factory.spawns.load(Ordering::SeqCst) == 1);
    rig_a.finish(handle);
    assert_eq!(
        rig_a.log().iter().filter(|e| *e == "term" || *e == "kill").count(),
        2
    );
    assert!(rig_a.parts.executor.sweep().is_empty());

    // Neither signal works: truthfully cleanup-pending, never "reaped".
    let rig_b = rig(vec![Script {
        stalls: true,
        ..Script::default()
    }]);
    let handle = rig_b.start();
    rig_b.until("spawn", |rig| rig.factory.spawns.load(Ordering::SeqCst) == 1);
    rig_b.finish(handle);
    let retained = rig_b.parts.executor.sweep();
    assert_eq!(retained.len(), 1, "the exact child is retained: {retained:?}");
    assert_eq!(
        rig_b.factory.spawns.load(Ordering::SeqCst),
        1,
        "no second child while one is unreaped"
    );
    // Once the child can finally die, a retry reaps it.
    lock(&lock(&rig_b.factory.children)[0]).kill_exits = true;
    let _ = rig_b.parts.executor.sweep();
    assert!(rig_b.parts.executor.sweep().is_empty());
}

#[test]
fn a_late_old_run_cannot_reach_a_successor() {
    // Two runs share a store but not a publisher: closing the first leaves its
    // late decode result and its late lines with nowhere to go.
    let (old_publish, old_updates) = recorder();
    let (new_publish, new_updates) = recorder();
    let old = Publisher::new(old_publish);
    let new = Publisher::new(new_publish);
    old.set_state(reading("Old"), ArtPlan::Pending(9));
    old.close();
    new.set_state(reading("New"), ArtPlan::Pending(9));
    assert!(
        !old.attach(9, &"a".repeat(64)),
        "the old run's decode result is dropped"
    );
    old.set_state(reading("Old again"), ArtPlan::None);
    assert!(
        lock(&new_updates)
            .iter()
            .all(|u| u.reading.as_ref().is_none_or(|r| r.title == "New"))
    );
    assert_eq!(lock(&new_updates).len(), 1);
    assert!(lock(&old_updates).len() <= 2);
}

#[test]
fn a_stderr_flood_is_bounded_and_sanitized() {
    let mut stderr = Vec::new();
    for index in 0..4000 {
        stderr.extend_from_slice(format!("noise line {index}\u{7}\x1b[31m\n").as_bytes());
    }
    stderr.extend_from_slice(&[b'Q'; 5000]);
    stderr.extend_from_slice(b"\nthe real error\n");
    let rig = rig(vec![Script {
        stderr,
        exit: 3,
        ..Script::default()
    }]);
    let handle = rig.start();
    rig.until("restarting", |rig| {
        lock(&rig.updates).iter().any(|u| u.status.stderr_tail.is_some())
    });
    rig.finish(handle);
    let tail = lock(&rig.updates)
        .iter()
        .find_map(|u| u.status.stderr_tail.clone())
        .expect("tail");
    assert!(tail.chars().count() <= STDERR_PUBLISHED_CHARS, "{}", tail.len());
    assert!(tail.ends_with("the real error"), "{tail}");
    assert!(!tail.chars().any(char::is_control));
    assert!(!tail.contains("QQQQ"), "an encoded-looking run is dropped");
}

#[test]
fn unavailable_resources_spawn_nothing_and_do_not_retry() {
    let mut rig = rig(Vec::new());
    rig.parts.resolver = Arc::new(|| Err("the media adapter supports arm64 only".to_owned()));
    let handle = rig.start();
    handle.join().expect("the worker ends by itself");
    assert_eq!(rig.factory.spawns.load(Ordering::SeqCst), 0);
    assert!(lock(&rig.clock.backoffs).is_empty());
    let last = rig.last().expect("a diagnostic");
    assert_eq!(last.status.state, MediaRunState::Unavailable);
    assert_eq!(
        last.status.reason.as_deref(),
        Some("the media adapter supports arm64 only")
    );
    assert_eq!(last.reading, None);
}

#[test]
fn relative_resolver_paths_are_refused_before_any_spawn() {
    let mut rig = rig(Vec::new());
    rig.parts.resolver = Arc::new(|| {
        Ok(MediaPaths {
            script: PathBuf::from("relative.pl"),
            ..paths()
        })
    });
    let handle = rig.start();
    handle.join().expect("worker");
    assert_eq!(rig.factory.spawns.load(Ordering::SeqCst), 0);
    assert_eq!(
        rig.last().expect("update").status.state,
        MediaRunState::Unavailable
    );
}

#[test]
fn a_stop_before_the_first_spawn_starts_no_child() {
    let rig = rig(Vec::new());
    rig.stop.cancel();
    let handle = rig.start();
    handle.join().expect("worker");
    assert_eq!(rig.factory.spawns.load(Ordering::SeqCst), 0);
}

#[test]
fn the_stderr_sanitizer_is_pure() {
    assert_eq!(owner::sanitize_stderr("  \n "), None);
    assert_eq!(owner::sanitize_stderr("a\tb\u{1b}c"), Some("a b c".to_owned()));
}
