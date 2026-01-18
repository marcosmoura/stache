//! Media playback component.
//!
//! Monitors currently playing media using the bundled `media-control` sidecar.
//! Streams media metadata changes and processes artwork for display in the frontend.
//! Artwork is resized to 128x128, cached to disk, and sent as base64-encoded PNG data.

#![allow(unexpected_cfgs)]

use std::collections::hash_map::DefaultHasher;
use std::fs::{File, create_dir_all};
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use image::ImageFormat;
use serde_json::{Map, Value};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use tauri_plugin_shell::ShellExt;
use tauri_plugin_shell::process::CommandEvent;

use crate::cache::get_cache_subdir_str;
use crate::events;
use crate::utils::thread::spawn_named_thread;

/// Resize the provided image to 128x128 and encode it as PNG.
///
/// # Errors
/// Returns an IO error if image processing fails.
fn resize_artwork(img: &image::DynamicImage) -> io::Result<(Vec<u8>, String)> {
    static PNG_BUFFER: OnceLock<std::sync::Mutex<Vec<u8>>> = OnceLock::new();

    const SIZE: u32 = 128;

    // Center-crop to square if the image is not square
    let (width, height) = (img.width(), img.height());
    let cropped = if width == height {
        img.clone()
    } else {
        let min_dim = width.min(height);
        let x_offset = (width - min_dim) / 2;
        let y_offset = (height - min_dim) / 2;
        img.crop_imm(x_offset, y_offset, min_dim, min_dim)
    };

    let resized = cropped.resize_exact(SIZE, SIZE, image::imageops::FilterType::Lanczos3);
    let rgba = resized.to_rgba8();

    let buffer = PNG_BUFFER.get_or_init(|| std::sync::Mutex::new(Vec::with_capacity(4096)));
    let mut buffer = buffer.lock().unwrap_or_else(PoisonError::into_inner);
    buffer.clear();
    let mut cursor = std::io::Cursor::new(&mut *buffer);
    image::DynamicImage::ImageRgba8(rgba)
        .write_to(&mut cursor, image::ImageFormat::Png)
        .map_err(io::Error::other)?;

    let size = usize::try_from(cursor.position())
        .map_err(|_| io::Error::other("Failed to get cursor position"))?;
    let result = buffer[..size].to_vec();
    drop(buffer);
    Ok((result, "png".to_string()))
}

/// Clean a string for safe use as a filename.
///
/// Converts to lowercase, keeps alphanumeric, dash, and underscore characters,
/// and replaces other characters with underscores (without consecutive underscores).
fn cleanup_string_for_filename(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut last_was_underscore = false;

    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() || c == '-' {
            result.push(c);
            last_was_underscore = false;
        } else if c == '_' || !last_was_underscore {
            // Underscore char or first replacement char in a sequence
            result.push('_');
            last_was_underscore = true;
        }
        // Skip consecutive replacement characters (when last_was_underscore && c != '_')
    }

    result.trim_matches('_').to_string()
}

static UNKNOWN: &str = "unknown";

static LAST_MEDIA_PAYLOAD: OnceLock<Mutex<Value>> = OnceLock::new();
static LAST_STATE_HASH: AtomicU64 = AtomicU64::new(0);

fn get_cache_path(state: &Map<String, Value>, extension: &str) -> String {
    let cache_dir = get_cache_dir();

    let artist = state.get("artist").and_then(Value::as_str).unwrap_or(UNKNOWN);
    let title = state.get("title").and_then(Value::as_str).unwrap_or(UNKNOWN);

    let mut cache_name = String::with_capacity(artist.len() + title.len() + 2);
    cache_name.push_str(artist);
    cache_name.push('-');
    cache_name.push_str(title);

    let clean_filename = cleanup_string_for_filename(&cache_name);

    let mut path =
        String::with_capacity(cache_dir.len() + clean_filename.len() + extension.len() + 1);
    path.push_str(cache_dir);
    path.push_str(&clean_filename);
    path.push('.');
    path.push_str(extension);
    path
}

/// Returns the cache directory for media artwork.
///
/// Uses `~/Library/Caches/{APP_BUNDLE_ID}/media_artwork/` on macOS for persistence.
/// Falls back to `/tmp/{APP_BUNDLE_ID}/media_artwork/` if cache directory unavailable.
fn get_cache_dir() -> &'static str {
    static CACHE_DIR: OnceLock<String> = OnceLock::new();
    CACHE_DIR.get_or_init(|| get_cache_subdir_str("media_artwork"))
}

/// Maps MIME types to image formats.
///
/// Supports common image formats: PNG, JPEG, WebP, GIF, and BMP.
fn image_format_from_mime(mime: &str) -> Option<ImageFormat> {
    // Normalize: lowercase and trim whitespace
    let mime = mime.trim().to_ascii_lowercase();

    match mime.as_str() {
        // PNG variants
        "image/png" | "image/x-png" => Some(ImageFormat::Png),
        // JPEG variants
        "image/jpeg" | "image/jpg" | "image/pjpeg" => Some(ImageFormat::Jpeg),
        // WebP
        "image/webp" => Some(ImageFormat::WebP),
        // GIF
        "image/gif" => Some(ImageFormat::Gif),
        // BMP
        "image/bmp" | "image/x-bmp" | "image/x-ms-bmp" => Some(ImageFormat::Bmp),
        // Unsupported
        _ => None,
    }
}

fn save_artwork(state: &Map<String, Value>) -> io::Result<Option<String>> {
    static CACHE_DIR_CREATED: OnceLock<()> = OnceLock::new();
    static DECODE_BUFFER: OnceLock<std::sync::Mutex<Vec<u8>>> = OnceLock::new();

    let Some(Value::String(art)) = state.get("artworkData") else {
        return Ok(None);
    };
    if art.starts_with('<') {
        return Ok(None);
    }
    let Some(Value::String(mime)) = state.get("artworkMimeType") else {
        return Ok(None);
    };

    let Some(image_format) = image_format_from_mime(mime) else {
        return Ok(None);
    };

    let path = get_cache_path(state, "txt");

    if let Ok(mut existing) = File::open(&path) {
        let mut cached = String::new();
        if existing.read_to_string(&mut cached).is_ok() && !cached.is_empty() {
            return Ok(Some(cached));
        }
    }

    let decode_buffer =
        DECODE_BUFFER.get_or_init(|| std::sync::Mutex::new(Vec::with_capacity(4096)));
    let mut buffer = decode_buffer.lock().unwrap_or_else(PoisonError::into_inner);
    buffer.clear();

    if STANDARD.decode_vec(art, &mut buffer).is_err() {
        return Ok(None);
    }

    let Ok(img) = image::load_from_memory_with_format(&buffer, image_format) else {
        return Ok(None);
    };
    drop(buffer);

    let (enc_bytes, _ext) = resize_artwork(&img)?;
    let base64_encoded = STANDARD.encode(enc_bytes);

    CACHE_DIR_CREATED.get_or_init(|| {
        let _ = create_dir_all(get_cache_dir());
    });

    let mut out = File::create(&path)?;
    out.write_all(base64_encoded.as_bytes())?;
    Ok(Some(base64_encoded))
}

fn calculate_state_hash(state: &Map<String, Value>) -> u64 {
    let mut hasher = DefaultHasher::new();
    state.hash(&mut hasher);
    hasher.finish()
}

fn set_last_media_payload(payload: Option<Value>) {
    let storage = LAST_MEDIA_PAYLOAD.get_or_init(|| Mutex::new(Value::Null));
    let mut guard = storage.lock().unwrap_or_else(PoisonError::into_inner);
    *guard = payload.unwrap_or(Value::Null);
}

fn get_last_media_payload() -> Option<Value> {
    let storage = LAST_MEDIA_PAYLOAD.get_or_init(|| Mutex::new(Value::Null));
    let guard = storage.lock().unwrap_or_else(PoisonError::into_inner);
    if guard.is_null() {
        None
    } else {
        Some(guard.clone())
    }
}

#[tauri::command]
#[must_use]
pub fn get_current_media_info() -> Option<Value> { get_last_media_payload() }

fn save_artwork_and_emit(
    state: &mut Map<String, Value>,
    window: &WebviewWindow,
    skip_hash_check: bool,
) -> io::Result<()> {
    if !skip_hash_check {
        let current_hash = calculate_state_hash(state);
        let last_hash = LAST_STATE_HASH.load(Ordering::Relaxed);
        if current_hash == last_hash && last_hash != 0 {
            return Ok(());
        }
        LAST_STATE_HASH.store(current_hash, Ordering::Relaxed);
    }

    if let Ok(Some(artwork_data)) = save_artwork(state) {
        static ARTWORK_KEY: OnceLock<String> = OnceLock::new();
        let key = ARTWORK_KEY.get_or_init(|| "artwork".to_string());
        state.insert(key.clone(), Value::String(artwork_data));
    }

    state.remove("artworkMimeType");
    state.remove("artworkData");

    let final_payload = Value::Object(state.clone());
    set_last_media_payload(Some(final_payload.clone()));

    // Emit to frontend
    window
        .emit(events::media::PLAYBACK_CHANGED, &final_payload)
        .map_err(io::Error::other)
}

fn parse_json(line: &str) -> Option<Value> {
    static JSON_BUFFER: OnceLock<std::sync::Mutex<Vec<u8>>> = OnceLock::new();
    let buffer = JSON_BUFFER.get_or_init(|| std::sync::Mutex::new(Vec::with_capacity(4096)));
    let mut bb = buffer.lock().unwrap_or_else(PoisonError::into_inner);
    bb.clear();
    bb.extend_from_slice(line.as_bytes());
    let result = serde_json::from_slice::<Value>(&bb).ok();
    drop(bb);
    result
}

fn parse_output(line: &str) -> Option<Value> {
    if line.trim().is_empty() {
        return None;
    }

    let parsed = parse_json(line)?;

    let data_obj = parsed.as_object()?;

    Some(Value::Object(data_obj.clone()))
}

fn process_stream_output(line: &str, state: &mut Map<String, Value>, window: &WebviewWindow) {
    let Some(parsed) = parse_output(line) else {
        return;
    };
    let Some(payload_obj) = parsed.get("payload").and_then(Value::as_object) else {
        return;
    };

    state.clear();
    state.extend(payload_obj.clone());

    if !state.is_empty()
        && let Err(err) = save_artwork_and_emit(state, window, false)
    {
        eprintln!("Failed to emit media update: {err}");
    }
}

#[allow(clippy::needless_pass_by_value)]
fn start_streaming(app: AppHandle, window: WebviewWindow) {
    let args = ["stream", "--no-diff"];
    let sidecar = match app.shell().sidecar("media-control") {
        Ok(cmd) => cmd.args(args),
        Err(err) => {
            eprintln!("Failed to create media-control sidecar: {err}");
            return;
        }
    };
    let spawn_result = sidecar.spawn();
    let Ok((mut rx, child)) = spawn_result else {
        if let Err(err) = spawn_result {
            eprintln!("Failed to spawn media-control sidecar: {err}");
        }
        return;
    };

    let mut state = Map::with_capacity(16);
    tauri::async_runtime::block_on(async {
        while let Some(event) = rx.recv().await {
            match event {
                CommandEvent::Stdout(line) => {
                    if line.is_empty() {
                        continue;
                    }
                    let content = String::from_utf8_lossy(&line);
                    let trimmed = content.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    process_stream_output(trimmed, &mut state, &window);
                }
                CommandEvent::Stderr(line) => {
                    eprintln!("media-control stderr: {}", String::from_utf8_lossy(&line).trim());
                }
                CommandEvent::Error(err) => {
                    eprintln!("media-control stream error: {err}");
                }
                CommandEvent::Terminated(_) => break,
                _ => {}
            }
        }
    });

    let _ = child.kill();
}

/// Initialize the media component.
///
/// Spawns a background thread that streams media control events and processes
/// artwork for efficient frontend display.
pub fn init(window: &WebviewWindow) {
    let w = window.clone();
    let app = window.app_handle().clone();
    spawn_named_thread("media", move || start_streaming(app, w));
}

#[cfg(test)]
mod tests {
    use image::ImageFormat;
    use serde_json::{Map, Value, json};

    use super::{
        UNKNOWN, calculate_state_hash, cleanup_string_for_filename, get_cache_dir, get_cache_path,
        get_current_media_info, image_format_from_mime, parse_json, parse_output,
        set_last_media_payload,
    };

    #[test]
    fn test_cleanup_string_for_filename() {
        assert_eq!(cleanup_string_for_filename("Hello World"), "hello_world");
        assert_eq!(cleanup_string_for_filename("Test-File_Name"), "test-file_name");
        assert_eq!(cleanup_string_for_filename("  Spaces  "), "spaces");
        assert_eq!(
            cleanup_string_for_filename("Special!@#$%Characters"),
            "special_characters"
        );
        assert_eq!(cleanup_string_for_filename("Already_clean"), "already_clean");
        // The function doesn't collapse consecutive underscores in the input,
        // only prevents adding multiple consecutive underscores from special chars
        assert_eq!(
            cleanup_string_for_filename("___multiple___underscores___"),
            "multiple___underscores"
        );
    }

    #[test]
    fn test_cleanup_string_empty() {
        assert_eq!(cleanup_string_for_filename(""), "");
        assert_eq!(cleanup_string_for_filename("___"), "");
    }

    #[test]
    fn test_cleanup_string_alphanumeric_only() {
        assert_eq!(cleanup_string_for_filename("abc123"), "abc123");
        assert_eq!(cleanup_string_for_filename("ABC123"), "abc123");
    }

    #[test]
    fn test_image_format_from_mime_png() {
        assert_eq!(image_format_from_mime("image/png"), Some(ImageFormat::Png));
        assert_eq!(image_format_from_mime("IMAGE/PNG"), Some(ImageFormat::Png));
        assert_eq!(image_format_from_mime("image/x-png"), Some(ImageFormat::Png));
        assert_eq!(image_format_from_mime("  image/png  "), Some(ImageFormat::Png));
    }

    #[test]
    fn test_image_format_from_mime_jpeg() {
        assert_eq!(image_format_from_mime("image/jpeg"), Some(ImageFormat::Jpeg));
        assert_eq!(image_format_from_mime("image/jpg"), Some(ImageFormat::Jpeg));
        assert_eq!(image_format_from_mime("image/pjpeg"), Some(ImageFormat::Jpeg));
        assert_eq!(image_format_from_mime("IMAGE/JPEG"), Some(ImageFormat::Jpeg));
    }

    #[test]
    fn test_image_format_from_mime_webp() {
        assert_eq!(image_format_from_mime("image/webp"), Some(ImageFormat::WebP));
        assert_eq!(image_format_from_mime("IMAGE/WEBP"), Some(ImageFormat::WebP));
    }

    #[test]
    fn test_image_format_from_mime_gif() {
        assert_eq!(image_format_from_mime("image/gif"), Some(ImageFormat::Gif));
        assert_eq!(image_format_from_mime("IMAGE/GIF"), Some(ImageFormat::Gif));
    }

    #[test]
    fn test_image_format_from_mime_bmp() {
        assert_eq!(image_format_from_mime("image/bmp"), Some(ImageFormat::Bmp));
        assert_eq!(image_format_from_mime("image/x-bmp"), Some(ImageFormat::Bmp));
        assert_eq!(image_format_from_mime("image/x-ms-bmp"), Some(ImageFormat::Bmp));
    }

    #[test]
    fn test_image_format_from_mime_unknown() {
        assert_eq!(image_format_from_mime("image/tiff"), None);
        assert_eq!(image_format_from_mime("image/svg+xml"), None);
        assert_eq!(image_format_from_mime("text/plain"), None);
        assert_eq!(image_format_from_mime(""), None);
    }

    #[test]
    fn test_get_cache_dir() {
        use crate::constants::APP_BUNDLE_ID;
        let dir = get_cache_dir();
        // Should use persistent cache location or fallback to /tmp
        assert!(
            dir.contains(&format!("{APP_BUNDLE_ID}/media_artwork"))
                || dir.contains(&format!("{APP_BUNDLE_ID}\\media_artwork"))
        );
        assert!(dir.ends_with('/') || dir.ends_with('\\'));
    }

    #[test]
    fn test_get_cache_path() {
        let mut state = Map::new();
        state.insert("artist".to_string(), Value::String("The Beatles".to_string()));
        state.insert("title".to_string(), Value::String("Hey Jude".to_string()));

        let path = get_cache_path(&state, "txt");
        assert!(path.contains("the_beatles-hey_jude"));
        assert!(
            std::path::Path::new(&path)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
        );
    }

    #[test]
    fn test_get_cache_path_unknown() {
        let state = Map::new();
        let path = get_cache_path(&state, "png");

        assert!(path.contains("unknown-unknown"));
        assert!(
            std::path::Path::new(&path)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
        );
    }

    #[test]
    fn test_get_cache_path_special_chars() {
        let mut state = Map::new();
        state.insert("artist".to_string(), Value::String("AC/DC".to_string()));
        state.insert("title".to_string(), Value::String("Back In Black!".to_string()));

        let path = get_cache_path(&state, "png");
        // Special characters should be cleaned
        assert!(path.contains("ac_dc-back_in_black"));
    }

    #[test]
    fn test_unknown_constant() {
        assert_eq!(UNKNOWN, "unknown");
    }

    #[test]
    fn test_calculate_state_hash_changes_with_data() {
        let mut state_a = Map::new();
        state_a.insert("artist".to_string(), Value::String("Artist".into()));
        state_a.insert("title".to_string(), Value::String("Song".into()));

        let mut state_b = state_a.clone();
        state_b.insert("album".to_string(), Value::String("Album".into()));

        let hash_a = calculate_state_hash(&state_a);
        let hash_b = calculate_state_hash(&state_b);

        assert_ne!(hash_a, hash_b);
    }

    #[test]
    fn test_parse_json_round_trip() {
        let payload = json!({
            "payload": {
                "artist": "Artist",
                "title": "Song"
            }
        });

        let serialized = payload.to_string();
        let parsed = parse_json(&serialized).expect("should parse json");

        assert_eq!(parsed, payload);
    }

    #[test]
    fn test_parse_output_extracts_payload() {
        let payload = json!({
            "payload": {
                "artist": "Artist",
                "title": "Song"
            }
        });
        let serialized = payload.to_string();

        let parsed = parse_output(&serialized).expect("should parse output");

        assert!(parsed.is_object());
        let object = parsed.as_object().unwrap();
        assert_eq!(object.get("payload"), payload.get("payload"));
    }

    #[test]
    fn test_parse_output_ignores_empty_lines() {
        assert!(parse_output("").is_none());
        assert!(parse_output("   ").is_none());
    }

    #[test]
    fn test_get_current_media_info_none_when_unset() {
        set_last_media_payload(None);
        assert!(get_current_media_info().is_none());
    }

    #[test]
    fn test_get_current_media_info_returns_last_state() {
        set_last_media_payload(None);
        let payload = json!({
            "artist": "Artist",
            "title": "Song",
            "playing": true,
            "bundleIdentifier": "test.bundle",
        });

        set_last_media_payload(Some(payload.clone()));

        let result = get_current_media_info().expect("payload should be returned");
        assert_eq!(result, payload);

        set_last_media_payload(None);
    }

    // ========================================================================
    // Additional cleanup_string_for_filename tests
    // ========================================================================

    #[test]
    fn test_cleanup_string_unicode() {
        // Unicode alphanumeric characters are preserved (is_alphanumeric returns true for them)
        assert_eq!(cleanup_string_for_filename("Café"), "café");
        // Japanese characters are alphanumeric in Unicode
        assert_eq!(cleanup_string_for_filename("日本語"), "日本語");
    }

    #[test]
    fn test_cleanup_string_mixed_unicode_ascii() {
        // Unicode alphanumeric characters are preserved
        assert_eq!(cleanup_string_for_filename("Hello世界World"), "hello世界world");
    }

    #[test]
    fn test_cleanup_string_dashes_preserved() {
        assert_eq!(cleanup_string_for_filename("test-file-name"), "test-file-name");
        assert_eq!(cleanup_string_for_filename("a-b-c"), "a-b-c");
    }

    #[test]
    fn test_cleanup_string_numbers() {
        assert_eq!(cleanup_string_for_filename("Track01"), "track01");
        assert_eq!(cleanup_string_for_filename("2024"), "2024");
        assert_eq!(cleanup_string_for_filename("123abc456"), "123abc456");
    }

    #[test]
    fn test_cleanup_string_leading_trailing_special() {
        assert_eq!(cleanup_string_for_filename("!!!hello!!!"), "hello");
        assert_eq!(cleanup_string_for_filename("@#$test@#$"), "test");
    }

    // ========================================================================
    // Additional calculate_state_hash tests
    // ========================================================================

    #[test]
    fn test_calculate_state_hash_empty_map() {
        let state = Map::new();
        let hash = calculate_state_hash(&state);
        // Empty map should have a consistent hash
        assert_eq!(hash, calculate_state_hash(&Map::new()));
    }

    #[test]
    fn test_calculate_state_hash_same_data_same_hash() {
        let mut state1 = Map::new();
        state1.insert("artist".to_string(), Value::String("Test".into()));

        let mut state2 = Map::new();
        state2.insert("artist".to_string(), Value::String("Test".into()));

        assert_eq!(calculate_state_hash(&state1), calculate_state_hash(&state2));
    }

    #[test]
    fn test_calculate_state_hash_different_keys_different_hash() {
        let mut state1 = Map::new();
        state1.insert("artist".to_string(), Value::String("Test".into()));

        let mut state2 = Map::new();
        state2.insert("title".to_string(), Value::String("Test".into()));

        assert_ne!(calculate_state_hash(&state1), calculate_state_hash(&state2));
    }

    #[test]
    fn test_calculate_state_hash_different_values_different_hash() {
        let mut state1 = Map::new();
        state1.insert("artist".to_string(), Value::String("Artist1".into()));

        let mut state2 = Map::new();
        state2.insert("artist".to_string(), Value::String("Artist2".into()));

        assert_ne!(calculate_state_hash(&state1), calculate_state_hash(&state2));
    }

    // ========================================================================
    // Additional parse_json tests
    // ========================================================================

    #[test]
    fn test_parse_json_invalid() {
        assert!(parse_json("not valid json").is_none());
        assert!(parse_json("{incomplete").is_none());
        assert!(parse_json("").is_none());
    }

    #[test]
    fn test_parse_json_array() {
        let result = parse_json("[1, 2, 3]");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), json!([1, 2, 3]));
    }

    #[test]
    fn test_parse_json_nested() {
        let result = parse_json(r#"{"outer": {"inner": "value"}}"#);
        assert!(result.is_some());
    }

    #[test]
    fn test_parse_json_with_numbers() {
        let result = parse_json(r#"{"int": 42, "float": 3.14}"#);
        assert!(result.is_some());
    }

    // ========================================================================
    // Additional parse_output tests
    // ========================================================================

    #[test]
    fn test_parse_output_without_payload() {
        let result = parse_output(r#"{"data": "value"}"#);
        assert!(result.is_some());
        // Should still return the object even without payload field
        let obj = result.unwrap();
        assert!(obj.is_object());
    }

    #[test]
    fn test_parse_output_null_payload() {
        let result = parse_output(r#"{"payload": null}"#);
        // payload is null, so it won't match as_object()
        assert!(result.is_some());
    }

    #[test]
    fn test_parse_output_array_payload() {
        let result = parse_output(r#"{"payload": []}"#);
        // payload is array, not object
        assert!(result.is_some());
    }

    #[test]
    fn test_parse_output_whitespace_only() {
        assert!(parse_output("   \n\t   ").is_none());
    }

    #[test]
    fn test_parse_output_newlines() {
        assert!(parse_output("\n\n\n").is_none());
    }

    // ========================================================================
    // Additional image_format_from_mime tests
    // ========================================================================

    #[test]
    fn test_image_format_from_mime_whitespace() {
        assert_eq!(image_format_from_mime("  image/png  "), Some(ImageFormat::Png));
        assert_eq!(image_format_from_mime("\timage/jpeg\n"), Some(ImageFormat::Jpeg));
    }

    #[test]
    fn test_image_format_from_mime_application_type() {
        // Application types should not match
        assert_eq!(image_format_from_mime("application/png"), None);
        assert_eq!(image_format_from_mime("application/octet-stream"), None);
    }

    #[test]
    fn test_image_format_from_mime_video_type() {
        // Video types should not match
        assert_eq!(image_format_from_mime("video/mp4"), None);
    }

    #[test]
    fn test_image_format_from_mime_audio_type() {
        // Audio types should not match
        assert_eq!(image_format_from_mime("audio/mpeg"), None);
    }

    // ========================================================================
    // Additional get_cache_path tests
    // ========================================================================

    #[test]
    fn test_get_cache_path_long_names() {
        let mut state = Map::new();
        state.insert(
            "artist".to_string(),
            Value::String("A Very Long Artist Name That Goes On And On".to_string()),
        );
        state.insert(
            "title".to_string(),
            Value::String("An Equally Long Song Title".to_string()),
        );

        let path = get_cache_path(&state, "txt");
        assert!(path.contains("a_very_long_artist_name"));
        assert!(path.ends_with(".txt"));
    }

    #[test]
    fn test_get_cache_path_different_extensions() {
        let state = Map::new();

        let txt_path = get_cache_path(&state, "txt");
        let png_path = get_cache_path(&state, "png");
        let jpg_path = get_cache_path(&state, "jpg");

        assert!(txt_path.ends_with(".txt"));
        assert!(png_path.ends_with(".png"));
        assert!(jpg_path.ends_with(".jpg"));
    }

    #[test]
    fn test_get_cache_path_only_artist() {
        let mut state = Map::new();
        state.insert("artist".to_string(), Value::String("Solo Artist".to_string()));

        let path = get_cache_path(&state, "txt");
        assert!(path.contains("solo_artist-unknown"));
    }

    #[test]
    fn test_get_cache_path_only_title() {
        let mut state = Map::new();
        state.insert("title".to_string(), Value::String("Untitled Song".to_string()));

        let path = get_cache_path(&state, "txt");
        assert!(path.contains("unknown-untitled_song"));
    }

    // ========================================================================
    // set_last_media_payload and get_last_media_payload tests
    // ========================================================================

    #[test]
    fn test_set_and_get_media_payload_complex() {
        set_last_media_payload(None);

        let payload = json!({
            "artist": "Complex Artist",
            "title": "Complex Song",
            "album": "Album Name",
            "playing": true,
            "duration": 240.5,
            "position": 120.0,
            "artwork": "base64data"
        });

        set_last_media_payload(Some(payload.clone()));

        let result = get_current_media_info().unwrap();
        assert_eq!(result["artist"], "Complex Artist");
        assert_eq!(result["duration"], 240.5);

        set_last_media_payload(None);
    }

    #[test]
    fn test_set_media_payload_overwrite() {
        set_last_media_payload(None);

        let payload1 = json!({"artist": "First"});
        let payload2 = json!({"artist": "Second"});

        set_last_media_payload(Some(payload1));
        set_last_media_payload(Some(payload2.clone()));

        let result = get_current_media_info().unwrap();
        assert_eq!(result, payload2);

        set_last_media_payload(None);
    }

    // ========================================================================
    // get_cache_dir consistency test
    // ========================================================================

    #[test]
    fn test_get_cache_dir_consistent() {
        let dir1 = get_cache_dir();
        let dir2 = get_cache_dir();
        assert_eq!(dir1, dir2);
    }

    #[test]
    fn test_get_cache_dir_ends_with_separator() {
        let dir = get_cache_dir();
        // Should end with path separator for easy concatenation
        assert!(dir.ends_with('/') || dir.ends_with('\\'));
    }
}
