//! The adapter's stream lines.
//!
//! Each line is `{"type":"data","diff":false,"payload":{...}}`. With `--no-diff`
//! every `data` line carries the complete state, so a line **replaces** the
//! state. An empty payload means nothing is playing.
//!
//! The parse is lenient about player-supplied values and strict about shape:
//! the wrapper and `payload` must be JSON objects, unknown keys and other
//! `type`s are ignored, a wrong-typed optional field is treated as missing, and
//! text fields are trimmed and capped at [`FIELD_CHARS`] characters.
//! `serde_json`'s recursion limit (128) bounds nesting, and the stream's line
//! cap bounds the total size before this runs.

use serde::Deserialize;
use serde_json::Value;

use super::MediaReading;

/// The most characters kept of a title, artist or album.
pub const FIELD_CHARS: usize = 1024;
/// The longest accepted bundle identifier or media type, in characters.
pub const IDENTIFIER_CHARS: usize = 256;

/// The artwork a line carried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtworkInput {
    /// The base64 text.
    pub data: String,
    /// The MIME type.
    pub mime: String,
}

/// What one line means.
#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    /// Not a usable data line: the state is unchanged.
    Ignore,
    /// Nothing is playing (empty payload or no title): the legacy absent state.
    Absent,
    /// The complete state.
    State {
        /// The reading without artwork.
        reading: MediaReading,
        /// The artwork bytes, when the line carried usable-looking input.
        artwork: Option<ArtworkInput>,
    },
}

#[derive(Deserialize)]
struct Wrapper {
    #[serde(rename = "type")]
    kind: Option<Value>,
    payload: Option<Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Payload {
    title: Option<Value>,
    artist: Option<Value>,
    album: Option<Value>,
    #[serde(rename = "bundleIdentifier")]
    bundle_identifier: Option<Value>,
    #[serde(rename = "mediaType")]
    media_type: Option<Value>,
    playing: Option<Value>,
    #[serde(rename = "artworkData")]
    artwork_data: Option<Value>,
    #[serde(rename = "artworkMimeType")]
    artwork_mime_type: Option<Value>,
}

fn text(value: Option<Value>, limit: usize) -> Option<String> {
    let Some(Value::String(raw)) = value else { return None };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(limit).collect::<String>().trim_end().to_owned())
}

/// Parses one stream line.
#[must_use]
pub fn parse_line(line: &str) -> Parsed {
    if line.trim().is_empty() {
        return Parsed::Ignore;
    }
    let Ok(wrapper) = serde_json::from_str::<Wrapper>(line) else {
        return Parsed::Ignore;
    };
    if !matches!(&wrapper.kind, Some(Value::String(kind)) if kind == "data") {
        return Parsed::Ignore;
    }
    let Some(Value::Object(map)) = wrapper.payload else {
        return Parsed::Ignore;
    };
    if map.is_empty() {
        return Parsed::Absent;
    }
    let Ok(payload) = serde_json::from_value::<Payload>(Value::Object(map)) else {
        return Parsed::Ignore;
    };
    // Legacy hid the component without a label, and the label is the title.
    let Some(title) = text(payload.title, FIELD_CHARS) else {
        return Parsed::Absent;
    };
    let artwork = match (payload.artwork_data, payload.artwork_mime_type) {
        (Some(Value::String(data)), Some(Value::String(mime))) if !data.is_empty() => {
            Some(ArtworkInput { data, mime })
        }
        _ => None,
    };
    Parsed::State {
        reading: MediaReading {
            title,
            artist: text(payload.artist, FIELD_CHARS).unwrap_or_default(),
            album: text(payload.album, FIELD_CHARS),
            bundle_identifier: text(payload.bundle_identifier, IDENTIFIER_CHARS)
                .unwrap_or_default(),
            media_type: text(payload.media_type, IDENTIFIER_CHARS),
            playing: matches!(payload.playing, Some(Value::Bool(true))),
            artwork_id: None,
        },
        artwork,
    }
}
