//! NUL-separated argv encoding, identical to the legacy `encode_mach_args`.

use std::fmt;

/// Largest encoded payload accepted (an engineering bound, far above any
/// borders batch).
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;

/// Why an argv cannot be encoded safely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
    /// Argument `index` contains a NUL, which would split it into two arguments.
    InteriorNul {
        /// Zero-based argument index.
        index: usize,
    },
    /// Argument `index` is empty; an empty argument is indistinguishable from the
    /// terminator for a reader that stops at a double NUL.
    EmptyArgument {
        /// Zero-based argument index.
        index: usize,
    },
    /// The encoded payload would exceed [`MAX_PAYLOAD_BYTES`].
    TooLarge,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InteriorNul { index } => write!(f, "argument {index} contains a NUL byte"),
            Self::EmptyArgument { index } => write!(f, "argument {index} is empty"),
            Self::TooLarge => write!(f, "payload exceeds {MAX_PAYLOAD_BYTES} bytes"),
        }
    }
}

impl std::error::Error for EncodeError {}

/// Encodes `args` as `arg0 NUL arg1 NUL ... NUL` plus one extra terminating NUL.
///
/// Bytes match the legacy encoder for every accepted input. Arguments are
/// opaque bytes: nothing is quoted, split or interpolated, so `["a b","c"]` and
/// `["a","b c"]` encode differently.
///
/// # Errors
/// Rejects interior NULs, empty arguments and oversized payloads.
pub fn encode_mach_args(args: &[String]) -> Result<Vec<u8>, EncodeError> {
    let mut payload = Vec::new();
    for (index, arg) in args.iter().enumerate() {
        if arg.is_empty() {
            return Err(EncodeError::EmptyArgument { index });
        }
        if arg.as_bytes().contains(&0) {
            return Err(EncodeError::InteriorNul { index });
        }
        payload.extend_from_slice(arg.as_bytes());
        payload.push(0);
        if payload.len() >= MAX_PAYLOAD_BYTES {
            return Err(EncodeError::TooLarge);
        }
    }
    payload.push(0);
    Ok(payload)
}
