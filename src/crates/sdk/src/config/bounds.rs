//! The TOML integer bound for `u64` configuration fields.
//!
//! TOML integers are `i64`. A few fields keep their `u64` Rust type but must
//! reject values the TOML format cannot hold, so a file the application writes
//! always loads back.

use std::fmt;

use serde::de::{Deserialize, Deserializer, Error};

use crate::error::ConfigError;

/// The largest value a TOML integer can hold, `i64::MAX`.
pub const MAX: u64 = i64::MAX as u64;

/// A `deserialize_with` function that rejects values above [`MAX`].
///
/// # Errors
/// Returns the deserializer's error, or a custom one naming the bound.
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let value = u64::deserialize(deserializer)?;
    check(value).map_err(D::Error::custom)?;
    Ok(value)
}

/// Rejects a value above [`MAX`].
///
/// # Errors
/// Returns a [`BoundError`] carrying the offending value.
pub const fn check(value: u64) -> Result<(), BoundError> {
    if value > MAX {
        Err(BoundError(value))
    } else {
        Ok(())
    }
}

/// Rejects a named field above [`MAX`] as a validation failure.
///
/// # Errors
/// Returns a [`ConfigError`] naming the field.
pub fn validate_field(field: &str, value: u64) -> Result<(), ConfigError> {
    check(value).map_err(|error| ConfigError::new(format!("{field}: {error}")))
}

/// A value above the TOML integer maximum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundError(u64);

impl fmt::Display for BoundError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} is above the maximum of {MAX}", self.0)
    }
}

impl std::error::Error for BoundError {}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Deserialize)]
    struct Holder {
        #[serde(deserialize_with = "deserialize")]
        value: u64,
    }

    #[test]
    fn the_maximum_is_accepted_and_one_more_is_not() {
        let ok: Holder = serde_json::from_str(r#"{"value":9223372036854775807}"#).expect("max");
        assert_eq!(ok.value, i64::MAX as u64);
        let error = serde_json::from_str::<Holder>(r#"{"value":9223372036854775808}"#)
            .expect_err("above the bound");
        assert!(error.to_string().contains("9223372036854775808"), "{error}");
        assert!(validate_field("hold_duration", MAX + 1).is_err());
        assert!(validate_field("hold_duration", MAX).is_ok());
    }
}
